//! Blocking host calls stay inside the evaluation deadline, and Worker stderr never backs up.
//! 阻塞宿主调用不超出执行截止时间，Worker stderr 也不会积压。

use super::*;
use crate::runtime::engine::logical_cwd;

/// Run `code` as runLua with `timeout_ms` and return the rendered result with its wall time.
/// 以 `timeout_ms` 将 `code` 作为 runLua 执行，返回渲染结果及其耗时。
fn timed_runlua(code: &'static str, timeout_ms: u64) -> (String, Duration) {
    let rendered = run_engine_operation_with_watchdog(move |engine| {
        let request = json!({ "code": code, "timeout_ms": timeout_ms });
        let started_at = Instant::now();
        let result = match engine.execute_runlua_request_json_inline(&request.to_string()) {
            Ok(text) | Err(text) => text,
        };
        format!("{}\u{1}{result}", started_at.elapsed().as_millis())
    });
    let (elapsed_ms, result) = rendered.split_once('\u{1}').expect("timed result");
    (
        result.to_owned(),
        Duration::from_millis(elapsed_ms.parse().expect("elapsed milliseconds")),
    )
}

/// `cap_timeout_ms` keeps the smaller bound, fills an absent one, and rejects an expired deadline.
/// `cap_timeout_ms` 保留较小的上限、补全缺失的上限，并拒绝已过期的截止时间。
#[test]
fn evaluation_deadline_caps_host_timeouts() {
    let lua = Lua::new();
    assert_eq!(logical_cwd::cap_timeout_ms(&lua, Some(5)).unwrap(), Some(5));
    assert_eq!(logical_cwd::cap_timeout_ms(&lua, None).unwrap(), None);

    lua.set_app_data(logical_cwd::EvaluationDeadline(
        Instant::now() + Duration::from_secs(60),
    ));
    assert_eq!(logical_cwd::cap_timeout_ms(&lua, Some(5)).unwrap(), Some(5));
    let filled = logical_cwd::cap_timeout_ms(&lua, None).unwrap().unwrap();
    assert!(filled > 50_000 && filled <= 60_000, "filled {filled}");
    let capped = logical_cwd::cap_timeout_ms(&lua, Some(u64::MAX))
        .unwrap()
        .unwrap();
    assert!(capped <= 60_000, "capped {capped}");

    lua.set_app_data(logical_cwd::EvaluationDeadline(Instant::now()));
    let error = logical_cwd::cap_timeout_ms(&lua, Some(5)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(logical_cwd::EVALUATION_DEADLINE_EXCEEDED),
        "{error}"
    );
}

/// A plain runLua timeout caps `process.exec`, not only System leases.
/// 普通 runLua 的超时同样约束 `process.exec`，而不只是系统租约。
#[test]
fn runlua_deadline_caps_process_exec() {
    let (result, elapsed) = timed_runlua(
        r#"local spec
if vulcan.os.info().os == 'windows' then
  spec = { program = 'cmd', args = { '/c', 'ping -n 30 127.0.0.1 >nul' } }
else
  spec = { program = '/bin/sh', args = { '-c', 'sleep 30' } }
end
local result = vulcan.process.exec(spec)
return 'exec returned timed_out=' .. tostring(result.timed_out)"#,
        500,
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "process.exec outlived the runLua timeout: {elapsed:?}; {result}"
    );
}

/// A runLua timeout caps `process.session` reads.
/// runLua 超时约束 `process.session` 的读取。
#[test]
fn runlua_deadline_caps_process_session_read() {
    let (result, elapsed) = timed_runlua(
        r#"local spec
if vulcan.os.info().os == 'windows' then
  spec = { program = 'cmd', args = { '/c', 'pause>nul' } }
else
  spec = { program = '/bin/sh', args = { '-c', 'read line' } }
end
local session = vulcan.process.session.open(spec)
local read = session:read({ timeout_ms = 20000 })
session:kill()
return 'read returned timed_out=' .. tostring(read.timed_out)"#,
        500,
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "process.session.read outlived the runLua timeout: {elapsed:?}; {result}"
    );
}

/// Build a worker that writes far more raw stderr than a pipe buffer holds (about 125 KB) before each response.
/// 构造一个在每次响应前写出远超管道缓冲区容量原始 stderr 的 worker。
fn managed_runtime_noisy_stderr_worker_command() -> Command {
    #[cfg(windows)]
    {
        let script = r#"$OutputEncoding=[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; while (($line=[Console]::In.ReadLine()) -ne $null) { for ($i = 0; $i -lt 5000; $i++) { [Console]::Error.WriteLine("raw stderr noise line $i") }; [Console]::Error.Flush(); [Console]::Out.WriteLine('{"ok":true,"value":1,"stdout":"","stderr":""}'); [Console]::Out.Flush() }"#;
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", script]);
        command
    }
    #[cfg(not(windows))]
    {
        let script = r#"while IFS= read -r line; do i=0; while [ "$i" -lt 5000 ]; do echo "raw stderr noise line $i" >&2; i=$((i + 1)); done; printf '%s\n' '{"ok":true,"value":1,"stdout":"","stderr":""}'; done"#;
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }
}

/// Raw Worker stderr is drained while the handler runs, so a noisy handler still answers in time.
/// Worker 原始 stderr 在处理函数运行期间即被读取，因此输出很多的处理函数仍能及时应答。
#[test]
fn managed_runtime_worker_drains_raw_stderr_while_handler_runs() {
    let _env_guard = process_env_test_guard();
    let mut command = managed_runtime_noisy_stderr_worker_command();
    let worker = spawn_managed_runtime_worker(&mut command).expect("noisy stderr worker spawns");
    let started_at = Instant::now();
    let (worker, result) =
        invoke_managed_runtime_worker(worker, &json!({"value": 1}), Some(30_000), false);
    let elapsed = started_at.elapsed();
    assert!(!result.timed_out, "worker stalled on stderr: {elapsed:?}");
    // The response may overtake the tail of the stderr stream, but the noise has started arriving.
    // 响应可能先于 stderr 流的末尾到达，但噪声已经开始到达。
    let envelope = result.envelope.to_string();
    assert!(envelope.contains("raw stderr noise line"), "{envelope}");
    // Raw stderr outside the envelope makes the Worker non-reusable.
    // 信封之外的原始 stderr 使该 Worker 不可复用。
    assert!(result.discard_worker);
    drop(worker);
}
