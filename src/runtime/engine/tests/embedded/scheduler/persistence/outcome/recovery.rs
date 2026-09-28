//! Original native and SDK outcomes survive uncertain SQLite confirmations without business replay.
//! 原始原生及 SDK 结果跨不确定 SQLite 确认保留，不重放业务。

use super::*;

/// Recover the original `operation` after its completion confirmation was committed or rolled back as `committed` says.
/// 按 `committed` 表示的完成确认已提交或回滚结果，恢复原始 `operation`。
/// `registration` must keep its real permit until confirmation; `closing` also tests the runtime shutdown barrier.
/// `registration` 必须在确认前保留真实许可；`closing` 还检验运行时关闭屏障。
/// The supplied journal and runtime remain their original owners; return the exact published terminal snapshot.
/// 所提供日志及运行时继续为原所有者；返回精确发布的终态快照。
fn recover_confirmation(
    journal: &OperationJournal,
    runtime: &EmbeddedRuntime,
    operation: &OperationHandle,
    registration: &str,
    committed: bool,
    closing: bool,
) -> OperationSnapshot {
    // The failure concerns persistence of a known host outcome, not another callback execution.
    // 故障涉及已知宿主结果的持久化，不涉及再次执行回调。
    let failed = failure(runtime, operation);
    assert_eq!(failed.phase, OperationPhase::Running);
    assert_eq!(failed.error.code, EmbeddedErrorCode::Internal);
    assert_eq!(
        journal.get(runtime.id(), operation.id()).unwrap_err(),
        failed.error
    );
    // Retain authority and the original incomplete public result before recovery starts.
    // 恢复开始前保留权威及原始未完成公开结果。
    let before = operation.snapshot().unwrap();
    assert!(before.value.is_none());
    assert!(
        before
            .host_effects
            .iter()
            .any(|effect| effect.registration_id == registration
                && effect.effects == EffectState::Committed
                && effect.phase == HostEffectPhase::Running)
    );
    // Closing registration cannot pretend that retained completion ownership has drained.
    // 关闭注册不能假装保留中的完成所有权已经排空。
    let retiring = runtime.capabilities().unregister(registration).unwrap();
    assert_eq!(retiring.in_flight, 1);
    assert!(!retiring.drained);
    if closing {
        runtime.request_close().unwrap();
        assert!(!runtime.poll_closed().unwrap());
    }
    assert!(journal.recover_storage().unwrap());
    // Recovered disk distinguishes the two real outcomes while live ownership remains unchanged.
    // 恢复磁盘区分两种真实结果，同时实时所有权保持不变。
    let restored = journal.get(runtime.id(), operation.id()).unwrap().unwrap();
    // Find by stable registration identity rather than the effect's evolving position.
    // 按稳定注册身份查找，不依赖副作用不断演进的位置。
    let effect = restored
        .snapshot
        .host_effects
        .iter()
        .find(|effect| effect.registration_id == registration)
        .unwrap();
    assert_eq!(
        effect.effects,
        if committed {
            EffectState::Committed
        } else {
            EffectState::Unknown
        }
    );
    assert_eq!(
        runtime
            .capabilities()
            .status(registration)
            .unwrap()
            .in_flight,
        1
    );
    assert_eq!(
        failure(runtime, operation).retry,
        CheckpointRetryState::Waiting
    );
    assert!(retry(runtime, operation));
    // Explicit retry acknowledges the original candidate, allowing the actual retained owner to exit.
    // 显式重试确认原始候选，允许真实保留所有者退出。
    let completed = operation.wait(OBSERVE).unwrap();
    assert_eq!(completed.context, before.context);
    assert_eq!(
        completed.phase,
        if closing {
            OperationPhase::Cancelled
        } else {
            OperationPhase::Succeeded
        },
        "{:?}",
        completed.error
    );
    assert!(
        completed
            .host_effects
            .iter()
            .any(|effect| effect.registration_id == registration
                && effect.effects == EffectState::Committed
                && effect.phase == HostEffectPhase::Completed)
    );
    assert!(runtime.capabilities().status(registration).unwrap().drained);
    assert_eq!(
        journal
            .get(runtime.id(), operation.id())
            .unwrap()
            .unwrap()
            .snapshot
            .phase,
        completed.phase
    );
    completed
}

/// SDK-owned completion remains exactly once across both uncertain disk outcomes and shutdown cancellation.
/// SDK 所有的完成跨两种不确定磁盘结果及关闭取消仍保持一次执行。
#[test]
fn embedded_storage_recovery_preserves_queued_completion() {
    for committed in [false, true] {
        for closing in [false, true] {
            // This real Lua invocation uses the formal host-request queue and SQLite writer.
            // 此真实 Lua 调用使用正式宿主请求队列及 SQLite 写入者。
            let layout = SystemRuntimeTestLayout::new("embedded queued uncertain confirmation");
            // Storage has ample room so the injected loss only concerns transaction confirmation.
            // 存储空间充足，因此注入丢失仅涉及事务确认。
            let (journal, writer, runtime) =
                durable_runtime(&layout, pool_config(), journal_config(16, 128 * 1024));
            // Existing integration authority supplies the exact queued descriptor and module binding.
            // 既有集成权威提供精确队列描述及模块绑定。
            let pool = queued_pool(&runtime, &layout);
            // Preserve one admitted operation while its original response is reconciled.
            // 原始响应对账期间保留单个入场操作。
            let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
            // Extraction proves pre-execution intent was committed before the fault is armed.
            // 提取证明故障启用前，执行前意图已经提交。
            let request = host_request(&runtime);
            // Retain the same broker and completion identity throughout recovery.
            // 恢复全程保留同一代理及完成身份。
            let broker = runtime.capabilities().host_requests();
            journal.lose_next_confirmation_for_test(committed);
            broker
                .complete(
                    &request.request_id,
                    CapabilityOutcome {
                        result: Ok(json!({"original": true})),
                        effects: EffectState::Committed,
                    },
                )
                .unwrap();
            // This fixture's Lua export returns the capability envelope without unwrapping its value.
            // 此夹具的 Lua 导出直接返回能力信封，不展开其值。
            let completed = recover_confirmation(
                &journal,
                &runtime,
                &operation,
                &request.registration_id,
                committed,
                closing,
            );
            if !closing {
                assert_eq!(
                    completed.value,
                    Some(json!({"ok": true, "effects": "committed", "value": {"original": true}}))
                );
            }
            assert_eq!(
                broker
                    .complete(
                        &request.request_id,
                        CapabilityOutcome {
                            result: Ok(json!("replacement")),
                            effects: EffectState::RolledBack
                        }
                    )
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::AlreadyCompleted
            );
            shutdown_durable(&runtime, &writer);
        }
    }
}

/// Native callback stacks and permits stay owned until the original outcome is durably reconciled.
/// 原生回调栈及许可在原始结果持久对账前始终保持所有权。
#[test]
fn embedded_storage_recovery_preserves_native_completion() {
    for committed in [false, true] {
        for closing in [false, true] {
            // A real VM invokes the native handler once and then waits on storage confirmation.
            // 真实 VM 调用原生处理器一次，随后等待存储确认。
            let layout = SystemRuntimeTestLayout::new("embedded native uncertain confirmation");
            // Explicit journal ownership allows infrastructure recovery without destroying the runtime.
            // 显式日志所有权允许恢复基础设施，而不销毁运行时。
            let (journal, writer, runtime) =
                durable_runtime(&layout, pool_config(), journal_config(16, 128 * 1024));
            // Count actual native entries independently from all storage attempts.
            // 独立于所有存储尝试计数真实原生进入。
            let count = Arc::new(AtomicUsize::new(0));
            // Share only the entry counter with the real callback.
            // 与真实回调共享进入计数器。
            let called = Arc::clone(&count);
            // Arm the fault only after the handler has actually entered and its start intent is durable.
            // 仅在处理器实际进入且开始意图已持久后启用故障。
            let handler_journal = Arc::clone(&journal);
            // Reuse the integration descriptor's explicit permissions and schema.
            // 复用集成描述的明确权限及 Schema。
            let mut descriptor = super::super::super::super::capabilities::descriptor(
                "test.recover_native",
                CapabilityExecution::Native,
            );
            descriptor.effects = CapabilityEffects::Mutating;
            descriptor.max_call_ms = OBSERVE.as_millis() as u64;
            // The exact registration identity proves retained native ownership during recovery.
            // 精确注册身份用于证明恢复期间保留的原生所有权。
            let registration = runtime
                .capabilities()
                .register(vec![CapabilityRegistrationRequest {
                    descriptor,
                    native: Some(Arc::new(move |_| {
                        called.fetch_add(1, Ordering::SeqCst);
                        handler_journal.lose_next_confirmation_for_test(committed);
                        CapabilityOutcome {
                            result: Ok(json!({"original": true})),
                            effects: EffectState::Committed,
                        }
                    })),
                }])
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
            // The Lua bridge returns this original native value only after durable completion confirmation.
            // Lua 桥仅在持久完成确认后返回此原始原生值。
            let pool = runtime.register_pool(definition(&layout, "return {call=function() local response=vulcan.capabilities.call('test.recover_native',{}); assert(response.ok); return response.value end}"), pool_policy(InstanceReuse::Reusable), permissions(), "r1".into()).unwrap();
            // Keep one operation and its original module context throughout recovery and optional closure.
            // 在恢复及可选关闭期间保留单个操作及其原始模块上下文。
            let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
            // This Lua export explicitly unwraps the original native response value.
            // 此 Lua 导出显式展开原始原生响应值。
            let completed = recover_confirmation(
                &journal,
                &runtime,
                &operation,
                &registration,
                committed,
                closing,
            );
            if !closing {
                assert_eq!(completed.value, Some(json!({"original": true})));
            }
            assert_eq!(count.load(Ordering::SeqCst), 1);
            shutdown_durable(&runtime, &writer);
        }
    }
}
