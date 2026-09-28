//! Real Lua-to-native execution intent and scheduler recovery evidence.
//! 真实 Lua 到原生执行意图及调度器恢复证据。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Initialization and exported calls must persist each native dispatch before entering its implementation.
/// 初始化及导出调用必须在进入原生实现前持久化每次分发。
#[test]
fn embedded_effect_intent_lua_initialization_and_call_are_durable() {
    // Run a real trusted package through the formal scheduler and SQLite worker.
    // 通过正式调度器及 SQLite 写入者运行真实可信包。
    let layout = SystemRuntimeTestLayout::new("embedded durable native intent");
    // Every handler reads the same actual journal that acknowledged its dispatch.
    // 每个处理器读取确认其分发的同一真实日志。
    let (journal, writer, runtime) =
        durable_runtime(&layout, pool_config(), journal_config(16, 128 * 1024));
    // Count actual handler entries rather than callback requests.
    // 统计真实处理器进入次数，不统计回调请求。
    let count = Arc::new(AtomicUsize::new(0));
    // Share actual execution evidence with the callback implementation.
    // 与回调实现共享真实执行证据。
    let called = Arc::clone(&count);
    // Keep journal ownership available for callback-side inspection.
    // 为回调侧检查保持日志所有权可用。
    let database = Arc::clone(&journal);
    // Derive the fixture contract from the existing real Lua capability test contract.
    // 从既有真实 Lua 能力测试契约派生夹具契约。
    let mut descriptor = super::super::super::capabilities::descriptor(
        "test.persisted_write",
        CapabilityExecution::Native,
    );
    descriptor.effects = CapabilityEffects::Mutating;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: Some(Arc::new(move |call| {
                // Identity originates from the VM binding even during module initialization.
                // 即使模块初始化期间，身份也来自 VM 绑定。
                let stored = database
                    .get(&call.caller.runtime_id, &call.caller.operation_id)
                    .unwrap()
                    .unwrap();
                // Application stage text only selects the expected phase; it never supplies lookup authority.
                // 应用阶段文本仅选择预期阶段，绝不提供查询权威。
                let expected = match call.arguments.as_str().unwrap() {
                    "initialize" => OperationPhase::Initializing,
                    "invoke" => OperationPhase::Running,
                    stage => panic!("unexpected fixture stage: {stage}"),
                };
                assert_eq!(stored.snapshot.phase, expected);
                // Compare the durable caller before handler execution, not a later reconstructed module identity.
                // 在处理器执行前比较持久调用方，不比较稍后重建的模块身份。
                assert_eq!(
                    stored
                        .snapshot
                        .host_effects
                        .iter()
                        .find(|record| Some(record.effect_id.as_str()) == call.effect_id.as_deref())
                        .unwrap()
                        .caller,
                    call.caller,
                );
                assert!(stored.snapshot.host_effects.iter().any(|record| Some(
                    record.effect_id.as_str()
                )
                    == call.effect_id.as_deref()
                    && record.phase == HostEffectPhase::Running
                    && record.effects == EffectState::Unknown));
                called.fetch_add(1, Ordering::SeqCst);
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::Committed,
                }
            })),
        }])
        .unwrap();
    // Initialization and invocation use the same operation but distinct effect identities.
    // 初始化及调用使用同一操作，但副作用身份不同。
    let pool = runtime.register_pool(
        definition(&layout, "vulcan.capabilities.call('test.persisted_write','initialize'); return {call=function() vulcan.capabilities.call('test.persisted_write','invoke'); return 7 end}"),
        pool_policy(InstanceReuse::Reusable), permissions(), "r1".into(),
    ).unwrap();
    // Public terminal success requires all retained stage checkpoints to acknowledge.
    // 公开终态成功要求全部保留阶段检查点确认。
    let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
    // The returned Lua value and two actual host entries must survive the complete pipeline.
    // 返回 Lua 值及两次真实宿主进入必须通过完整调用链保留。
    let completed = operation.wait(OBSERVE).unwrap();
    assert_eq!(
        completed.phase,
        OperationPhase::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(completed.value, Some(json!(7)));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    // Two stage starts, two intent/outcome pairs, cleaning and terminal publication produce eight exact revisions.
    // 两次阶段开始、两对意图与结果、清理及终态发布产生八次精确修订。
    let stored = journal.get(runtime.id(), operation.id()).unwrap().unwrap();
    assert_eq!(stored.revision, 8);
    assert_eq!(stored.snapshot.host_effects.len(), 2);
    assert!(
        stored
            .snapshot
            .host_effects
            .iter()
            .all(|record| record.phase == HostEffectPhase::Completed
                && record.effects == EffectState::Committed)
    );
    shutdown_durable(&runtime, &writer);
}

/// A caught Lua callback rejection still retains the failed intent until explicit scheduler recovery.
/// Lua 捕获回调拒绝后，失败意图仍保留至显式调度恢复。
#[test]
fn embedded_effect_intent_lua_failure_recovers_storage_without_execution() {
    // Files synchronize actual Lua execution before filling the writer's retained receipt quota.
    // 文件同步真实 Lua 执行，然后填满写入者保留回执配额。
    let layout = SystemRuntimeTestLayout::new("embedded durable intent rejection");
    // Independent row capacity prevents unrelated journal record exhaustion.
    // 独立行容量防止无关的日志记录耗尽。
    let (journal, writer, runtime) =
        durable_runtime(&layout, pool_config(), journal_config(64, 256 * 1024));
    // Record any incorrectly dispatched mutation.
    // 记录任何错误分发的变更。
    let count = Arc::new(AtomicUsize::new(0));
    // The native callback shares only its actual invocation counter.
    // 原生回调仅共享真实调用计数。
    let called = Arc::clone(&count);
    // The existing fixture schema explicitly declares a native mutating operation.
    // 既有夹具 Schema 显式声明原生可变更操作。
    let mut descriptor = super::super::super::capabilities::descriptor(
        "test.persisted_write",
        CapabilityExecution::Native,
    );
    descriptor.effects = CapabilityEffects::Mutating;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: Some(Arc::new(move |_| {
                called.fetch_add(1, Ordering::SeqCst);
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::Committed,
                }
            })),
        }])
        .unwrap();
    // The explicit release guard also unblocks Lua if a fixture assertion fails.
    // 若夹具断言失败，显式释放守卫也会解除 Lua 阻塞。
    let release = FinalizerRelease(layout.package_root.join("release-intent"));
    // Derive Lua's fixture timeout from the sole observation budget.
    // 从唯一观测预算派生 Lua 夹具超时。
    let source = r#"return {call=function()
        -- Expose actual Running execution before the host fills storage receipts.
        -- 宿主填满存储回执前暴露真实执行阶段。
        local marker=assert(io.open('intent-entered','w')); marker:write('ready'); marker:close()
        -- Bound fixture coordination independently from production cancellation.
        -- 独立于生产取消限制夹具协调时间。
        local deadline=os.clock()+__OBSERVE_SECONDS__
        while true do
            -- Release only after the test has retained all available writer receipts.
            -- 仅在测试保留全部可用写入回执之后释放。
            local opened,ready=pcall(io.open,'release-intent','r')
            if opened and ready then ready:close(); break end
            assert(os.clock()<deadline,'intent fixture release timed out')
        end
        pcall(function() vulcan.capabilities.call('test.persisted_write',{}) end)
        return 11
    end}"#
        .replace("__OBSERVE_SECONDS__", &OBSERVE.as_secs().to_string());
    // The native storage error is caught by Lua, so preserving its original success result is observable.
    // 原生存储错误被 Lua 捕获，因此可以观测原始成功结果的保留。
    let pool = runtime
        .register_pool(
            definition(&layout, &source),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    // The real operation owns all subsequent failed and repaired checkpoints.
    // 真实操作拥有全部后续失败及修复检查点。
    let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
    until(
        || layout.package_root.join("intent-entered").exists(),
        "Lua did not reach the intended storage gate",
    );
    // Saturate from the writer's real admission response instead of duplicating its configured limit.
    // 根据写入者真实入场响应填满，不重复定义其配置上限。
    let mut held = Vec::new();
    loop {
        // Each unrelated reconciled row consumes one retained receipt.
        // 每条无关且已对账行消耗一个保留回执。
        let mut row = filler(0);
        row.operation_id = format!("held-{}", held.len());
        // A completed receipt remains budgeted until its owner releases it.
        // 已完成回执在所有者释放前继续计入预算。
        let receipt = match writer.submit("intent-fill", None, Arc::new(row)) {
            Ok(receipt) => receipt,
            Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => break,
            Err(error) => panic!("unexpected writer admission error: {error:?}"),
        };
        // Only an actual committed revision proves that this retained receipt has completed.
        // 只有真实已提交修订才证明此保留回执已经完成。
        let acknowledged = receipt.wait(OBSERVE).unwrap();
        assert_eq!(acknowledged.phase, JournalWritePhase::Completed);
        assert!(acknowledged.error.is_none());
        assert!(acknowledged.revision.is_some());
        held.push(receipt);
    }
    drop(release);
    // Even though Lua caught the error, its failed original intent prevents terminal publication.
    // 即使 Lua 捕获错误，其失败原始意图仍阻止终态发布。
    let failed = {
        // Report actual operation and writer evidence if the expected retained fault never appears.
        // 若预期保留故障始终未出现，报告真实操作及写入者证据。
        let deadline = Instant::now() + OBSERVE;
        loop {
            match runtime.persistence_failure(operation.id()) {
                Ok(Some(failed)) => break failed,
                Ok(None) => {}
                Err(error) if error.code == EmbeddedErrorCode::Busy => {}
                Err(error) => panic!("unexpected checkpoint observation error: {error:?}"),
            }
            assert!(
                Instant::now() < deadline,
                "intent fault missing: operation={:?}, writer={:?}, entries={}",
                operation.snapshot(),
                writer.status(),
                count.load(Ordering::SeqCst)
            );
            std::thread::yield_now();
        }
    };
    assert_eq!(failed.phase, OperationPhase::Running);
    assert_eq!(failed.error.code, EmbeddedErrorCode::CapacityExceeded);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    drop(held);
    assert_eq!(
        runtime
            .persistence_failure(operation.id())
            .unwrap()
            .unwrap()
            .retry,
        CheckpointRetryState::Waiting
    );
    assert!(runtime.retry_checkpoint(operation.id()).unwrap());
    // Storage recovery publishes the frozen Lua value and never dispatches its cancelled native attempt.
    // 存储恢复发布冻结 Lua 值，绝不分发其已取消原生尝试。
    let completed = operation.wait(OBSERVE).unwrap();
    assert_eq!(
        completed.phase,
        OperationPhase::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(completed.value, Some(json!(11)));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(
        completed
            .host_effects
            .iter()
            .all(|record| record.phase == HostEffectPhase::Completed
                && record.effects == EffectState::NotStarted)
    );
    assert_eq!(
        journal
            .get(runtime.id(), operation.id())
            .unwrap()
            .unwrap()
            .snapshot
            .value,
        completed.value
    );
    shutdown_durable(&runtime, &writer);
}
