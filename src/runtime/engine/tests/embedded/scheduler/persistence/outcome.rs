//! Real returned-handler ownership through durable confirmation and explicit recovery.
//! 跨持久确认及显式恢复保留真实已返回处理器所有权。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Register one explicit queued fixture capability and return its real pool identity.
/// 注册一项显式队列夹具能力并返回其真实池身份。
fn queued_pool(runtime: &EmbeddedRuntime, layout: &SystemRuntimeTestLayout) -> String {
    // Use the authoritative integration fixture's permission and schema shape.
    // 使用权威集成夹具的权限及 Schema 形状。
    let mut descriptor = super::super::super::capabilities::descriptor(
        "test.queued_outcome",
        CapabilityExecution::Queued,
    );
    descriptor.effects = CapabilityEffects::Mutating;
    descriptor.max_call_ms = OBSERVE.as_millis() as u64;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: None,
        }])
        .unwrap();
    runtime
        .register_pool(
            definition(
                layout,
                "return {call=function() return vulcan.host.call('test.queued_outcome',{}) end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap()
}

/// SDK completion returns acceptance while disk is blocked, preserving one original result and registration owner.
/// 磁盘阻塞期间 SDK 完成返回接纳结果，保留一个原始结果及注册所有者。
#[test]
fn embedded_effect_outcome_queued_disk_wait_keeps_control_available() {
    // Cover both ordinary success and runtime shutdown after the SDK has handed over its result.
    // 覆盖普通成功及 SDK 交出结果之后运行时关闭。
    for closing in [false, true] {
        // Each case drives a real Lua request through the bounded host queue.
        // 每个场景通过有界宿主队列推进真实 Lua 请求。
        let layout = SystemRuntimeTestLayout::new("embedded queued outcome disk wait");
        // The shared writer owns the actual blocked filesystem work.
        // 共享写入者拥有真实被阻塞文件系统工作。
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(16, 128 * 1024));
        // The real pool binds the exact capability snapshot before invocation.
        // 真实池在调用前绑定精确能力快照。
        let pool = queued_pool(&runtime, &layout);
        // Keep the original operation handle throughout accepted, confirmed and terminal states.
        // 在已接纳、已确认及终态期间保留原始操作句柄。
        let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        // Dispatch already proves that the original start intent has reached the real database.
        // 分发已证明原始开始意图到达真实数据库。
        let request = host_request(&runtime);
        // The SDK control surface remains available independently of the operation execution thread.
        // SDK 控制入口独立于操作执行线程继续可用。
        let broker = runtime.capabilities().host_requests();
        // Delay only confirmation, after the exact request was actually delivered.
        // 精确请求实际交付后，仅延迟确认。
        let gate = journal.block_for_test();
        // Observe the control result before releasing disk, with a bounded independent control thread.
        // 使用有界独立控制线程，在释放磁盘前观测控制结果。
        let (sent, received) = std::sync::mpsc::channel();
        // Clone only the real broker and delivered identity, never create another request.
        // 仅克隆真实代理及已交付身份，绝不创建另一请求。
        let control_broker = Arc::clone(&broker);
        // This identifier remains stable across completion acceptance and durable acknowledgement.
        // 此标识符在完成接纳及持久确认之间保持稳定。
        let request_id = request.request_id.clone();
        // The exact SDK outcome must survive storage waiting without another submission.
        // 精确 SDK 结果必须在存储等待期间存活，不需要再次提交。
        let control = std::thread::spawn(move || {
            sent.send(control_broker.complete(
                &request_id,
                CapabilityOutcome {
                    result: Ok(json!({"original": true})),
                    effects: EffectState::Committed,
                },
            ))
            .unwrap()
        });
        // Capture acceptance without making a blocking write look responsive after the gate was released.
        // 在门禁释放前捕获接纳，避免将阻塞写入误判为响应及时。
        let accepted = received.recv_timeout(OBSERVE);
        if accepted.is_err() {
            drop(gate);
            control.join().unwrap();
            panic!("SDK completion waited for disk instead of accepting the original result");
        }
        accepted.unwrap().unwrap();
        control.join().unwrap();
        assert_eq!(
            broker.status(&request.request_id).unwrap().phase,
            HostRequestPhase::Completing
        );
        assert_eq!(
            runtime
                .capabilities()
                .status(&request.registration_id)
                .unwrap()
                .in_flight,
            1
        );
        assert!(operation.snapshot().unwrap().value.is_none());
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
        if closing {
            runtime.request_close().unwrap();
            assert!(!runtime.poll_closed().unwrap());
        }
        drop(gate);
        // Completion and registration drainage depend on the actual acknowledged write, not request cancellation.
        // 完成及注册排空依赖真实已确认写入，不依赖请求取消。
        let completed = operation.wait(OBSERVE).unwrap();
        if closing {
            assert_eq!(
                completed.phase,
                OperationPhase::Cancelled,
                "{:?}",
                completed.error
            );
        } else {
            assert_eq!(
                completed.phase,
                OperationPhase::Succeeded,
                "{:?}",
                completed.error
            );
            assert_eq!(
                completed.value.as_ref().unwrap()["value"],
                json!({"original": true})
            );
        }
        assert!(
            completed
                .host_effects
                .iter()
                .any(|record| record.effects == EffectState::Committed
                    && record.phase == HostEffectPhase::Completed)
        );
        assert!(
            journal
                .get(runtime.id(), operation.id())
                .unwrap()
                .unwrap()
                .snapshot
                .host_effects
                .iter()
                .all(|record| record.effects != EffectState::RolledBack)
        );
        shutdown_durable(&runtime, &writer);
    }
}

/// A failed accepted SDK completion retains its original value and requires explicit storage recovery only.
/// 已接纳 SDK 完成失败时保留原始值，且仅要求显式存储恢复。
#[test]
fn embedded_effect_outcome_queued_failure_never_replaces_or_replays_result() {
    // Hold actual completed receipts after dispatch so only confirmation admission fails.
    // 分发后持有真实已完成回执，使仅确认入场失败。
    let layout = SystemRuntimeTestLayout::new("embedded queued outcome capacity");
    // Independent database capacity accommodates every retained receipt filler.
    // 独立数据库容量容纳每个保留回执填充项。
    let (journal, writer, runtime) =
        durable_runtime(&layout, pool_config(), journal_config(64, 256 * 1024));
    // The callback queue is the only execution transport declared by this pool.
    // 回调队列是此池声明的唯一执行传输。
    let pool = queued_pool(&runtime, &layout);
    // Keep one original operation and one actually delivered host request.
    // 保留一个原始操作及一个真实已交付宿主请求。
    let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
    // Actual delivery happens before any storage admission exhaustion.
    // 任何存储入场耗尽之前先发生真实交付。
    let request = host_request(&runtime);
    // The broker owns accepted completion evidence even if the SDK discards its temporary return value.
    // 即使 SDK 丢弃临时返回值，代理仍拥有已接纳完成证据。
    let broker = runtime.capabilities().host_requests();
    // Fill the writer's exact quota through its own admission result.
    // 通过写入者自身入场结果填满其精确配额。
    let mut held = Vec::new();
    loop {
        // Every filler has a distinct namespace-local record identity.
        // 每个填充项拥有不同的命名空间内记录身份。
        let mut row = filler(0);
        row.operation_id = format!("queued-held-{}", held.len());
        // Known completed receipts still count until the host releases their actual ownership.
        // 在宿主释放真实所有权前，已知完成回执仍计入预算。
        let receipt = match writer.submit("queued-outcome-fill", None, Arc::new(row)) {
            Ok(receipt) => receipt,
            Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => break,
            Err(error) => panic!("unexpected writer admission error: {error:?}"),
        };
        // Confirm that only receipt retention, not an unrelated database failure, fills capacity.
        // 确认仅回执保留填满容量，不是无关数据库故障。
        let acknowledged = receipt.wait(OBSERVE).unwrap();
        assert_eq!(acknowledged.phase, JournalWritePhase::Completed);
        assert!(acknowledged.error.is_none());
        held.push(receipt);
    }
    broker
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!({"retained": 23})),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    // A separate storage failure does not mean the SDK should resubmit its already accepted result.
    // 独立存储故障不代表 SDK 应重新提交已被接纳的结果。
    let failed = failure(&runtime, &operation);
    assert_eq!(failed.error.code, EmbeddedErrorCode::CapacityExceeded);
    assert_eq!(
        broker.status(&request.request_id).unwrap().phase,
        HostRequestPhase::Completing
    );
    assert_eq!(
        broker
            .complete(
                &request.request_id,
                CapabilityOutcome {
                    result: Ok(json!("changed")),
                    effects: EffectState::RolledBack
                }
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::AlreadyCompleted
    );
    drop(held);
    for _ in 0..3 {
        assert!(broker.take(1).unwrap().is_empty());
        assert_eq!(
            broker.status(&request.request_id).unwrap().phase,
            HostRequestPhase::Completing
        );
    }
    assert_eq!(
        failure(&runtime, &operation).retry,
        CheckpointRetryState::Waiting
    );
    assert!(retry(&runtime, &operation));
    // The original accepted value flows into Lua after recovery, with no second queue delivery.
    // 恢复后原始已接纳值流入 Lua，不发生第二次队列交付。
    let completed = operation.wait(OBSERVE).unwrap();
    assert_eq!(
        completed.phase,
        OperationPhase::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(
        completed.value.as_ref().unwrap()["value"],
        json!({"retained": 23})
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
    assert!(broker.take(1).unwrap().is_empty());
    assert!(
        completed
            .host_effects
            .iter()
            .all(|record| record.effects == EffectState::Committed)
    );
    shutdown_durable(&runtime, &writer);
}

/// Request recovery after releasing a test's retained receipts; retry only observations rejected before mutation.
/// 释放测试保留回执后请求恢复；仅重试在变更前被拒绝的观测。
fn retry(runtime: &EmbeddedRuntime, operation: &OperationHandle) -> bool {
    // Store the one authoritative retry response instead of issuing a second mutation to inspect it.
    // 保存单个权威重试响应，不为检查它而发出第二次变更。
    let mut accepted = None;
    until(
        || match runtime.retry_checkpoint(operation.id()) {
            Ok(value) => {
                accepted = Some(value);
                true
            }
            Err(error) if error.code == EmbeddedErrorCode::Busy => false,
            Err(error) => panic!("unexpected checkpoint recovery error: {error:?}"),
        },
        "native confirmation never accepted checkpoint recovery",
    );
    accepted.expect("retry response was captured")
}

/// Preserve a returned native value, committed evidence and actual registration ownership until confirmation succeeds.
/// 在确认成功前保留已返回原生值、已提交证据及真实注册所有权。
#[test]
fn embedded_effect_outcome_native_failure_retains_result_and_shutdown_ownership() {
    // Recovery must work during normal operation and after runtime closure has requested cancellation.
    // 恢复必须在正常运行及运行时关闭已请求取消后均可工作。
    for closing in [false, true] {
        // Each case owns a real VM, database and independent runtime namespace.
        // 每个场景拥有真实 VM、数据库及独立运行时命名空间。
        let layout = SystemRuntimeTestLayout::new("embedded durable native outcome");
        // Database room exceeds writer receipt capacity so only the intended admission gate is exhausted.
        // 数据库空间超过写入者回执容量，使仅预期入场门耗尽。
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(64, 256 * 1024));
        // Completed receipts are deliberately retained by a separate host-side owner.
        // 已完成回执由独立宿主侧所有者有意保留。
        let held = Arc::new(std::sync::Mutex::new(Vec::<JournalWriteReceipt>::new()));
        // One atomic entry count proves recovery never invokes the handler again.
        // 单个原子进入计数证明恢复绝不再次调用处理器。
        let count = Arc::new(AtomicUsize::new(0));
        // The callback shares exact receipt ownership and the original bounded writer.
        // 回调共享精确回执所有权及原始有界写入者。
        let handler_held = Arc::clone(&held);
        // Retain the original writer until native registration really drains.
        // 在原生注册真实排空前保留原始写入者。
        let handler_writer = Arc::clone(&writer);
        // Count actual business entries, independently from persistence attempts.
        // 独立于持久尝试统计真实业务进入。
        let called = Arc::clone(&count);
        // Use the same explicit native schema and permissions as other real Lua integration fixtures.
        // 使用与其他真实 Lua 集成夹具相同的显式原生 Schema 及权限。
        let mut descriptor = super::super::super::capabilities::descriptor(
            "test.persisted_outcome",
            CapabilityExecution::Native,
        );
        descriptor.effects = CapabilityEffects::Mutating;
        descriptor.max_call_ms = OBSERVE.as_millis() as u64;
        // Retain the exact returned registration identity for lifetime queries.
        // 保留精确返回的注册身份以查询生命周期。
        let registration = runtime
            .capabilities()
            .register(vec![CapabilityRegistrationRequest {
                descriptor,
                native: Some(Arc::new(move |_| {
                    called.fetch_add(1, Ordering::SeqCst);
                    // Intention is already durable; fill the remaining real queue quota before returning committed evidence.
                    // 意图已持久化；返回已提交证据前填满剩余真实队列配额。
                    let mut receipts = handler_held.lock().unwrap();
                    loop {
                        // Unrelated reconciled rows consume exact retained writer receipts.
                        // 无关且已对账行消耗精确保留写入者回执。
                        let mut row = filler(0);
                        row.operation_id = format!("outcome-held-{}", receipts.len());
                        // Derive saturation from real admission rather than copying the configured count limit.
                        // 从真实入场派生饱和，不复制配置数量上限。
                        let receipt =
                            match handler_writer.submit("outcome-fill", None, Arc::new(row)) {
                                Ok(receipt) => receipt,
                                Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => {
                                    break;
                                }
                                Err(error) => {
                                    panic!("unexpected receipt admission failure: {error:?}")
                                }
                            };
                        // Verify actual acknowledgement; a timed-out observation is not a completed receipt.
                        // 校验真实确认；超时观测不是已完成回执。
                        let stored = receipt.wait(OBSERVE).unwrap();
                        assert_eq!(stored.phase, JournalWritePhase::Completed);
                        assert!(stored.error.is_none());
                        assert!(stored.revision.is_some());
                        receipts.push(receipt);
                    }
                    CapabilityOutcome {
                        result: Ok(json!({"original": "returned native value"})),
                        effects: EffectState::Committed,
                    }
                })),
            }])
            .unwrap()
            .into_iter()
            .next()
            .expect("single fixture registration returns one identity");
        // Return the original host value through the actual Lua bridge after confirmation unblocks.
        // 确认解除阻塞后，通过真实 Lua 桥返回原始宿主值。
        let pool = runtime.register_pool(
            definition(&layout, "return {call=function() local response=vulcan.capabilities.call('test.persisted_outcome',{}); assert(response.ok); return response.value end}"),
            pool_policy(InstanceReuse::Reusable), permissions(), "r1".into(),
        ).unwrap();
        // The actual operation must remain live while its returned host value awaits confirmation.
        // 已返回宿主值等待确认期间，真实操作必须保持活动。
        let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        // A persistence error is separately observable without converting the known host commit into an execution error.
        // 持久错误独立可观测，不将已知宿主提交转为执行错误。
        let failed = failure(&runtime, &operation);
        assert_eq!(failed.phase, OperationPhase::Running);
        assert_eq!(failed.error.code, EmbeddedErrorCode::CapacityExceeded);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime
                .submit(call(&pool, Value::Null), OBSERVE)
                .err()
                .map(|error| error.code),
            Some(EmbeddedErrorCode::Busy)
        );
        // Capture live and disk evidence before repairing capacity; disk still has the conservative start intent.
        // 修复容量前捕获实时及磁盘证据；磁盘仍保存保守开始意图。
        let live = operation.snapshot().unwrap();
        assert_eq!(live.phase, OperationPhase::WaitingForHost);
        assert!(live.value.is_none());
        assert!(
            live.host_effects
                .iter()
                .any(|record| record.capability_name == "test.persisted_outcome"
                    && record.effects == EffectState::Committed
                    && record.phase == HostEffectPhase::Running)
        );
        assert!(
            journal
                .get(runtime.id(), operation.id())
                .unwrap()
                .unwrap()
                .snapshot
                .host_effects
                .iter()
                .any(|record| record.effects == EffectState::Unknown)
        );
        // Unregistering closes new dispatch but cannot release a returned callback's outstanding confirmation owner.
        // 注销关闭新分发，但不能释放已返回回调尚在等待确认的所有者。
        let retiring = runtime.capabilities().unregister(&registration).unwrap();
        assert_eq!(retiring.in_flight, 1);
        assert!(!retiring.drained);
        if closing {
            runtime.request_close().unwrap();
            assert!(!runtime.poll_closed().unwrap());
        }
        held.lock().unwrap().clear();
        // Keep the retry physically in flight while verifying duplicate requests cannot enqueue another write.
        // 核对重复请求不能排入另一写入时，使重试保持物理在途。
        let gate = journal.block_for_test();
        assert!(retry(&runtime, &operation));
        assert!(!retry(&runtime, &operation));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(
            !runtime
                .capabilities()
                .status(&registration)
                .unwrap()
                .drained
        );
        drop(gate);
        // Cancellation changes only the returned operation result; the original committed host evidence survives.
        // 取消仅改变返回操作结果；原始已提交宿主证据继续保留。
        let completed = operation.wait(OBSERVE).unwrap();
        if closing {
            assert_eq!(
                completed.phase,
                OperationPhase::Cancelled,
                "{:?}",
                completed.error
            );
        } else {
            assert_eq!(
                completed.phase,
                OperationPhase::Succeeded,
                "{:?}",
                completed.error
            );
            assert_eq!(
                completed.value,
                Some(json!({"original": "returned native value"}))
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(
            completed
                .host_effects
                .iter()
                .any(|record| record.effects == EffectState::Committed
                    && record.phase == HostEffectPhase::Completed)
        );
        assert!(
            runtime
                .capabilities()
                .status(&registration)
                .unwrap()
                .drained
        );
        assert!(
            journal
                .get(runtime.id(), operation.id())
                .unwrap()
                .unwrap()
                .snapshot
                .host_effects
                .iter()
                .any(|record| record.effects == EffectState::Committed
                    && record.phase == HostEffectPhase::Completed)
        );
        shutdown_durable(&runtime, &writer);
    }
}
