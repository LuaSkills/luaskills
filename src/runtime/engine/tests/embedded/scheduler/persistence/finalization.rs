//! A real Lua VM survives lost SQLite closing checkpoints without replaying either stage.
//! 真实 Lua VM 跨 SQLite 关闭检查点确认丢失存活，且不重放任何阶段。

use super::super::finalization::closing_definition;
use super::*;

/// Lose both closing acknowledgements with real commits and rollbacks, then retry only storage.
/// 对真实提交和回滚均丢失两个关闭确认，然后仅重试存储。
#[test]
fn embedded_scheduler_finalization_recovery_retains_vm_and_never_replays() {
    for committed in [false, true] {
        let layout = SystemRuntimeTestLayout::new("automatic closing durable recovery");
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(16, 1024 * 1024));
        // Release both real Lua gates on assertion failure so fixture cleanup cannot strand a worker.
        // 断言失败时释放两个真实 Lua 门禁，避免夹具清理遗留工作线程。
        let business_release = FinalizerRelease(layout.package_root.join("business-release"));
        let closing_release = FinalizerRelease(layout.package_root.join("closing-release"));
        let source = r#"
            -- File gates expose actual execution without invoking another durable host checkpoint.
            -- 文件门禁暴露真实执行，且不引入其他持久宿主检查点。
            local open, clock = io.open, os.clock
            local count = 0
            -- Wait for the named host-owned release and return after bounded fixture coordination.
            -- 等待指定的宿主释放，在有界夹具协调后返回。
            local function gate(name)
                local entered=assert(open(name..'-entered','w')); entered:write('yes'); entered:close()
                local deadline=clock()+8
                repeat
                    local ok, released=pcall(open,name..'-release','r')
                    if ok and released then released:close(); return end
                until clock() >= deadline
                error('fixture gate expired')
            end
            return {
                -- Record exactly one business entry and preserve VM-local state for shutdown.
                -- 精确记录一次业务进入，并为关闭保留 VM 局部状态。
                call=function()
                    count=count+1
                    local f=assert(open('business-count','a')); f:write('x'); f:close()
                    gate('business')
                    return count
                end,
                -- Closing uses the same count and records its independent observable invocation.
                -- 关闭使用同一计数，并记录自身独立可观测调用。
                shutdown=function()
                    assert(count==1)
                    local f=assert(open('closing-count','a')); f:write('x'); f:close()
                    gate('closing')
                    return 'closed'
                end
            }
        "#;
        let pool = runtime
            .register_pool(
                closing_definition(&layout, source, OBSERVE.as_millis() as u64),
                pool_policy(InstanceReuse::SingleCall),
                permissions(),
                "r1".into(),
            )
            .unwrap();
        let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        until(
            || layout.package_root.join("business-entered").exists(),
            "business never reached its real gate",
        );
        journal.lose_next_confirmation_for_test(committed);
        drop(business_release);
        let failed = failure(&runtime, &operation);
        assert_eq!(failed.phase, OperationPhase::Cleaning);
        assert!(operation.snapshot().unwrap().finalization.is_none());
        assert!(!layout.package_root.join("closing-count").exists());
        assert_eq!(runtime.usage().unwrap().cleaning_operations, 1);
        runtime.request_close().unwrap();
        assert!(!runtime.poll_closed().unwrap());
        assert!(journal.recover_storage().unwrap());
        assert_eq!(
            runtime
                .persistence_failure(operation.id())
                .unwrap()
                .unwrap()
                .retry,
            CheckpointRetryState::Waiting
        );
        assert!(runtime.retry_checkpoint(operation.id()).unwrap());
        until(
            || layout.package_root.join("closing-entered").exists(),
            "closing never resumed on the original VM",
        );
        let intent = journal.get(runtime.id(), operation.id()).unwrap().unwrap();
        let stages = intent.snapshot.finalization.unwrap();
        assert_eq!(stages.business.result().unwrap(), json!(1));
        assert!(stages.outcome.is_none());
        journal.lose_next_confirmation_for_test(committed);
        drop(closing_release);
        assert_eq!(
            failure(&runtime, &operation).phase,
            OperationPhase::Cleaning
        );
        assert!(
            operation
                .snapshot()
                .unwrap()
                .finalization
                .unwrap()
                .outcome
                .is_none()
        );
        assert!(!runtime.poll_closed().unwrap());
        assert!(journal.recover_storage().unwrap());
        assert_eq!(
            runtime
                .persistence_failure(operation.id())
                .unwrap()
                .unwrap()
                .retry,
            CheckpointRetryState::Waiting
        );
        assert!(runtime.retry_checkpoint(operation.id()).unwrap());
        let terminal = operation.wait(OBSERVE).unwrap();
        assert_eq!(terminal.phase, OperationPhase::Succeeded);
        assert_eq!(terminal.value, Some(json!(1)));
        assert!(terminal.cancellation_requested);
        assert_eq!(
            terminal
                .finalization
                .unwrap()
                .outcome
                .unwrap()
                .result()
                .unwrap(),
            json!("closed")
        );
        for name in ["business-count", "closing-count"] {
            assert_eq!(
                fs::read_to_string(layout.package_root.join(name)).unwrap(),
                "x"
            );
        }
        shutdown_durable(&runtime, &writer);
    }
}
