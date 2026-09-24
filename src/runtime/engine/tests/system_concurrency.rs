//! Deterministic System lease concurrency and relative-path regression coverage.
//! 系统租约确定性并发及相对路径回归覆盖。

use super::*;
use crate::runtime::engine::runlua;

/// Builds a lease with a distinct authorized directory below the same System package.
/// 在同一系统包内构建具有独立授权目录的租约。
fn lease(engine: &LuaEngine, layout: &SystemRuntimeTestLayout, name: &str) -> Value {
    let cwd = layout.package_root.join(name);
    fs::create_dir_all(&cwd).unwrap();
    fs::write(cwd.join("identity.txt"), name).unwrap();
    fs::write(cwd.join("identity.lua"), format!("return '{name}'")).unwrap();
    let mut request = layout.create_request(name);
    request["cwd"] = json!(render_host_visible_path(&cwd));
    let response: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&request.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    response
}

/// Executes exact source against an existing lease and returns its structured response.
/// 对既有租约执行精确源码并返回结构化响应。
fn eval(engine: &LuaEngine, lease: &Value, code: &str) -> Value {
    serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": lease["lease_id"], "generation": lease["generation"], "code": code,
                })
                .to_string(),
            )
            .unwrap(),
    )
    .unwrap()
}

/// A slow host callback must not prevent another System lease from finishing before release.
/// 慢宿主回调不得阻止另一个系统租约在其放行前完成。
/// Channels prove ordering; the timeout only bounds a failed regression.
/// 通道证明顺序；超时仅为回归失败提供时间边界。
#[test]
fn system_runtime_lease_host_wait_does_not_block_another_lease() {
    let _guard = host_tool_callback_test_guard();
    let layout = SystemRuntimeTestLayout::new("system-concurrent 中文 空格");
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    let slow = lease(&engine, &layout, "slow");
    let fast = lease(&engine, &layout, "fast");
    let original_cwd = std::env::current_dir().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    set_host_tool_callback(Some(Arc::new(move |_| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| error.to_string())?;
        Ok(json!(true))
    })));
    let slow_engine = engine.clone();
    let slow_thread = thread::spawn(move || {
        eval(
            &slow_engine,
            &slow,
            "vulcan.host.call('pause', {}) return vulcan.fs.read('identity.txt')",
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (finished_tx, finished_rx) = mpsc::channel();
    let fast_engine = engine.clone();
    let fast_thread = thread::spawn(move || {
        let response = eval(
            &fast_engine,
            &fast,
            "local f=io.open('identity.txt','r'); local text=f:read('*a'); f:close(); return {fs=vulcan.fs.read('identity.txt'), io=text, module=dofile('identity.lua'), imported=require('identity'), cwd=vulcan.runtime.cwd()}",
        );
        finished_tx.send(response).unwrap();
    });
    let finished = finished_rx.recv_timeout(Duration::from_secs(2));
    let observed_cwd = std::env::current_dir().unwrap();
    release_tx.send(()).unwrap();
    let slow_result = slow_thread.join().unwrap();
    fast_thread.join().unwrap();
    let result = finished.expect("fast lease must finish while slow lease is still blocked");
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["fs"], "fast");
    assert_eq!(result["result"]["io"], "fast");
    assert_eq!(result["result"]["module"], "fast");
    assert_eq!(result["result"]["imported"], "fast");
    assert_eq!(slow_result["result"], "slow");
    assert_eq!(observed_cwd, original_cwd);
}

/// System relative reads remain anchored even while ordinary file execution owns the process cwd lock.
/// 即使普通文件执行持有进程目录锁，系统相对读取仍绑定自身目录。
#[test]
fn system_runtime_lease_ignores_process_cwd_guard() {
    let layout = SystemRuntimeTestLayout::new("system-independent 中文 cwd");
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    let created = lease(&engine, &layout, "isolated");
    let guard = runlua::lock_runlua_cwd_guard();
    // Simulate ordinary runLua while its process cwd points at a different package.
    // 模拟普通 runLua 执行时进程目录指向另一个包。
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&layout.runtime_root).unwrap();
    let (tx, rx) = mpsc::channel();
    let command = if cfg!(windows) {
        "type identity.txt"
    } else {
        "cat identity.txt"
    };
    let code = format!(
        "vulcan.io.write_text('written.txt', 'local'); local child=vulcan.process.exec({{ command={command:?}, timeout_ms=3000 }}); local pipe=io.popen({command:?}, 'r'); local output=pipe:read('*a'); pipe:close(); return {{ text=vulcan.fs.read('written.txt'), module=require('identity'), child=child.stdout, popen=output }}"
    );
    let task = thread::spawn(move || tx.send(eval(&engine, &created, &code)).unwrap());
    let result = rx.recv_timeout(Duration::from_secs(2));
    std::env::set_current_dir(original_cwd).unwrap();
    drop(guard);
    task.join().unwrap();
    let result = result.expect("System eval must not acquire the process cwd guard");
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["text"], "local");
    assert_eq!(result["result"]["module"], "isolated");
    assert_eq!(
        result["result"]["child"].as_str().unwrap().trim(),
        "isolated"
    );
    assert_eq!(
        result["result"]["popen"].as_str().unwrap().trim(),
        "isolated"
    );
}
