//! Durable prewarm ownership survives ambiguous writes without exposing an unconfirmed VM to business work.
//! 持久预热归属跨不确定写入保留，未确认 VM 不暴露给业务执行。

use super::*;

/// Both rolled-back and committed-but-unacknowledged initialization checkpoints retain exact VM ownership.
/// 回滚及已提交但未确认的初始化检查点均保留精确 VM 归属。
#[test]
fn embedded_prewarm_persistence_failure_blocks_reuse_until_explicit_recovery() {
    for committed in [false, true] {
        // One physical slot forces the later business call to borrow this confirmed instance.
        // 单个物理槽迫使后续业务调用借用此已确认实例。
        let layout = SystemRuntimeTestLayout::new("prewarm durable reuse ownership");
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(16, 1024 * 1024));
        let mut policy = pool_policy(InstanceReuse::Reusable);
        policy.max_resident_vms = 1;
        policy.max_running_calls = 1;
        // The file gate is actual Lua initialization; release it even if a test assertion unwinds.
        // 文件门禁属于真实 Lua 初始化；即使测试断言展开也释放它。
        let release = FinalizerRelease(layout.package_root.join("prewarm-release"));
        let source = r#"
            -- File markers independently prove initialization and business entry counts.
            -- 文件标记独立证明初始化及业务进入次数。
            local open, clock = io.open, os.clock
            local initialized = assert(open('prewarm-count','a'))
            initialized:write('x'); initialized:close()
            -- The observation budget is injected from the single Rust fixture constant.
            -- 观测预算从唯一 Rust 夹具常量注入。
            local deadline = clock() + __OBSERVE_SECS__
            local released = false
            repeat
                local ok, gate = pcall(open, 'prewarm-release', 'r')
                if ok and gate then gate:close(); released=true; break end
            until clock() >= deadline
            assert(released, 'prewarm fixture gate expired')
            return {call=function()
                -- Business begins only after confirmed prewarm publication permits reuse.
                -- 仅已确认预热发布允许复用后业务才开始。
                local count = assert(open('business-count','a'))
                count:write('x'); count:close()
                return true
            end}
        "#
        .replace("__OBSERVE_SECS__", &OBSERVE.as_secs().to_string());
        let pool = runtime
            .register_pool(
                definition(&layout, &source),
                policy,
                permissions(),
                "durable-prewarm".into(),
            )
            .unwrap();
        let operation = runtime
            .prewarm_instance(
                EmbeddedPrewarm {
                    pool_id: pool.clone(),
                    context: LuaInvocationContext::default(),
                },
                OBSERVE,
            )
            .unwrap();
        until(
            || layout.package_root.join("prewarm-count").exists(),
            "actual prewarm initialization never entered",
        );
        // Admit the follower before storage failure; failed storage correctly fences all new admission.
        // 在存储失败前接纳后继；失败存储会正确封锁全部新入场。
        let business = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        journal.lose_next_confirmation_for_test(committed);
        drop(release);
        assert_eq!(
            failure(&runtime, &operation).phase,
            OperationPhase::Cleaning
        );
        // Public phase remains the last acknowledged checkpoint, distinct from the failed Cleaning candidate.
        // 公开阶段保留最后已确认检查点，区别于失败的清理候选。
        assert_eq!(
            operation.snapshot().unwrap().phase,
            OperationPhase::Initializing
        );
        assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
        // Physical residency cannot substitute for confirmed scheduler readiness after a failed checkpoint.
        // 检查点失败后，物理常驻不能替代已确认调度器就绪。
        let readiness = runtime.reusable_pool_status(&pool).unwrap();
        assert_eq!(readiness.ready, 0);
        assert_eq!(readiness.unavailable, 1);
        assert!(readiness.admission_blocked);
        // A later real business call cannot take ownership while the original checkpoint is unresolved.
        // 原检查点未解决时，后续真实业务调用不能取得归属。
        assert_eq!(
            runtime
                .submit(call(&pool, Value::Null), OBSERVE)
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::Busy
        );
        assert_eq!(
            business.wait(Duration::from_millis(25)).unwrap().phase,
            OperationPhase::Queued
        );
        assert!(!layout.package_root.join("business-count").exists());
        assert!(journal.recover_storage().unwrap());
        assert!(runtime.retry_checkpoint(operation.id()).unwrap());
        let result = operation.wait(OBSERVE).unwrap();
        assert_eq!(
            result.phase,
            OperationPhase::Succeeded,
            "{:?}",
            result.error
        );
        assert!(
            result.value.as_ref().unwrap()["instance_id"]
                .as_str()
                .is_some()
        );
        assert_eq!(business.wait(OBSERVE).unwrap().value, Some(json!(true)));
        until(
            || runtime.reusable_pool_status(&pool).unwrap().ready == 1,
            "confirmed original instance did not become borrowable",
        );
        assert!(
            !runtime
                .reusable_pool_status(&pool)
                .unwrap()
                .admission_blocked
        );
        assert_eq!(
            fs::read_to_string(layout.package_root.join("prewarm-count")).unwrap(),
            "x"
        );
        assert_eq!(
            fs::read_to_string(layout.package_root.join("business-count")).unwrap(),
            "x"
        );
        assert_eq!(
            journal
                .get(runtime.id(), operation.id())
                .unwrap()
                .unwrap()
                .snapshot
                .context,
            result.context
        );
        shutdown_durable(&runtime, &writer);
    }
}
