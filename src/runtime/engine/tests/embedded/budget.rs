//! Embedded export deadlines that protected calls and the JIT cannot get around.
//! 保护调用与 JIT 都无法绕开的嵌入式导出截止时间。

use super::*;
use std::time::Instant;

/// Invoke `source`'s `call` export under a 200 ms deadline on a worker thread.
/// 在工作线程上以 200 毫秒截止时间调用 `source` 的 `call` 导出。
///
/// Return the error code and how long the call took, or fail when it outlives a 30 s watchdog.
/// 返回错误码与调用耗时；若超过 30 秒看门狗仍未返回则判定失败。
fn invoke_under_short_deadline(
    label: &'static str,
    source: &'static str,
) -> (Option<EmbeddedErrorCode>, Duration) {
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let layout = SystemRuntimeTestLayout::new(label);
        let engine = Arc::new(make_runtime_test_engine_with_host_options(
            layout.host_options(),
        ));
        let mut module = engine
            .create_embedded_module(definition(&layout, source), "budget-instance", control())
            .unwrap();
        let context = LuaInvocationContext::default();
        let started = Instant::now();
        let outcome = module.invoke(ModuleInvocation {
            operation_id: "budget-operation",
            session_id: None,
            export: "call",
            arguments: &json!({}),
            context: &context,
            control: Arc::new(CallControl::new(Duration::from_millis(200)).unwrap()),
        });
        let _ = result_tx.send((outcome.err().map(|error| error.code), started.elapsed()));
    });
    result_rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("embedded case '{label}' outlived its deadline"))
}

/// Retry loops around protected calls, and code that tries to re-enable the JIT, still stop.
/// 包在保护调用外的重试循环，以及试图重新开启 JIT 的代码，仍会被终止。
#[test]
fn embedded_deadline_cannot_be_caught_or_outrun() {
    for (label, source) in [
        (
            "embedded budget pcall",
            "local function spin() while true do end end return {call=function(a) while true do pcall(spin) end end}",
        ),
        (
            "embedded budget xpcall",
            "local function spin() while true do end end return {call=function(a) while true do xpcall(spin, function(e) return e end) end end}",
        ),
        (
            "embedded budget coroutine",
            "local function spin() while true do end end return {call=function(a) while true do coroutine.resume(coroutine.create(spin)) end end}",
        ),
        (
            "embedded budget jit on",
            "return {call=function(a) if jit.on then jit.on() end local x = 0 while true do x = x + 1 end end}",
        ),
        (
            "embedded budget jit require",
            "return {call=function(a) local ok, j = pcall(require, 'jit') if ok and j.on then j.on() end local x = 0 while true do x = x + 1 end end}",
        ),
    ] {
        let (code, elapsed) = invoke_under_short_deadline(label, source);
        assert_eq!(code, Some(EmbeddedErrorCode::DeadlineExceeded), "{label}");
        assert!(elapsed < Duration::from_secs(10), "{label}: {elapsed:?}");
    }
}
