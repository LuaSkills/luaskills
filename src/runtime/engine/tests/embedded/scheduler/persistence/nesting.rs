//! Deeply nested Lua results are rejected before they reach durable history, which stays readable.
//! 嵌套过深的 Lua 结果在进入持久历史之前就被拒绝，持久历史保持可读。

use super::*;

/// Run one durable call whose result nests `depth + 1` arrays; return its terminal snapshot after
/// confirming the journal can still list, read, forget and reopen that operation's history.
/// 运行一次结果嵌套 `depth + 1` 层数组的持久调用；在确认日志仍能列出、读取、遗忘并重新打开该操作历史后，
/// 返回其终态快照。
fn durable_nested_result(depth: usize) -> OperationSnapshot {
    let layout = SystemRuntimeTestLayout::new("embedded durable nested result");
    let limits = journal_config(16, 128 * 1024);
    let (journal, writer, runtime) = durable_runtime(&layout, pool_config(), limits);
    let source = format!(
        "return {{call=function(a) local t={{1}} for i=1,{depth} do t={{t}} end return t end}}"
    );
    let pool = runtime
        .register_pool(
            definition(&layout, &source),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let operation = runtime.submit(call(&pool, json!(null)), OBSERVE).unwrap();
    let result = operation.wait(OBSERVE).unwrap();
    let id = operation.id().to_owned();
    until(
        || matches!(journal.get(runtime.id(), &id), Ok(Some(record)) if record.snapshot.phase.is_terminal()),
        "terminal checkpoint of the nested result",
    );
    journal.next(None).expect("history stays listable");
    runtime.forget_operation(&id).unwrap();
    drop(operation);
    shutdown_durable(&runtime, &writer);
    drop(runtime);
    drop(writer);
    drop(journal);
    let deadline = Instant::now() + OBSERVE;
    loop {
        match OperationJournal::open(&layout.runtime_root.join("operations.db"), limits) {
            Err(error) if error.code == EmbeddedErrorCode::Busy && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            reopened => {
                reopened.expect("history reopens");
                break;
            }
        }
    }
    result
}

/// A result at the 100-level limit succeeds; a 201-level result fails as an ordinary operation
/// error, and in both cases the journal stays readable after a restart.
/// 处于 100 层上限的结果成功；201 层的结果以普通操作错误失败，两种情况下日志在重启后都保持可读。
#[test]
fn embedded_scheduler_persistence_rejects_results_nested_beyond_limit() {
    let at_limit = durable_nested_result(99);
    assert_eq!(at_limit.phase, OperationPhase::Succeeded);

    let too_deep = durable_nested_result(200);
    assert_ne!(too_deep.phase, OperationPhase::Succeeded);
    let error = too_deep.error.expect("deep result carries an error");
    assert!(
        error.message.contains("nesting exceeds 100 levels"),
        "unexpected error: {error:?}"
    );
}
