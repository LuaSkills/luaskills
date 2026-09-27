use super::*;
use crate::runtime::embedded::{HostEffectPhase, OperationPhase, OperationRegistry};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Retention exhaustion stops new callbacks before execution and never discards earlier commit evidence.
/// 保留耗尽在执行前阻止新回调，且绝不丢弃之前的提交证据。
#[test]
fn embedded_effect_retention_bound_and_sealing_prevent_invisible_execution() {
    // One retained record makes both count exhaustion and later terminal sealing observable.
    // 单个保留记录使数量耗尽与后续终态封存均可观察。
    let mut limits = config();
    limits.max_effect_records_per_operation = 1;
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    register(
        &capabilities,
        "test.commit",
        Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            }
        }),
    );
    let (handle, mut owner) = operations.admit(control()).unwrap();
    let mut identity = caller();
    identity.operation_id = handle.snapshot().unwrap().operation_id;
    owner.advance(OperationPhase::Running).unwrap();
    let snapshot = capabilities.snapshot().unwrap();
    snapshot
        .invoke_native(
            "test.commit",
            identity.clone(),
            grants(),
            Value::Null,
            owner.control(),
        )
        .unwrap();
    assert_eq!(
        snapshot
            .invoke_native(
                "test.commit",
                identity.clone(),
                grants(),
                Value::Null,
                owner.control()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::RolledBack)
        .unwrap();
    assert_eq!(handle.snapshot().unwrap().effects, EffectState::Committed);
    assert_eq!(
        snapshot
            .invoke_native(
                "test.commit",
                identity,
                grants(),
                Value::Null,
                owner.control()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Tiny metadata budgets reject the handler before any business implementation runs.
/// 极小元数据预算在任何业务实现运行前拒绝处理器。
#[test]
fn embedded_effect_byte_retention_rejects_before_host_execution() {
    let mut limits = config();
    limits.max_effect_bytes_per_operation = 1;
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    register(
        &capabilities,
        "test.never",
        Arc::new(|_| panic!("retention refusal ran host code")),
    );
    let (handle, owner) = operations.admit(control()).unwrap();
    let mut identity = caller();
    identity.operation_id = handle.snapshot().unwrap().operation_id;
    assert_eq!(
        capabilities
            .snapshot()
            .unwrap()
            .invoke_native(
                "test.never",
                identity,
                grants(),
                Value::Null,
                owner.control()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert!(handle.snapshot().unwrap().host_effects.is_empty());
}

/// Neither owner mistakes nor cancellation may complete an operation while a queued handler still owns execution.
/// 所有者误操作或取消均不能在队列处理器仍拥有执行权时完成操作。
#[test]
fn embedded_effect_live_handler_blocks_terminal_operation_publication() {
    let limits = config();
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let mut contract = descriptor("test.queue");
    contract.execution = CapabilityExecution::Queued;
    capabilities
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: None,
        }])
        .unwrap();
    let (handle, mut owner) = operations.admit(control()).unwrap();
    let mut identity = caller();
    identity.operation_id = handle.snapshot().unwrap().operation_id;
    owner.advance(OperationPhase::Running).unwrap();
    let request = capabilities
        .snapshot()
        .unwrap()
        .submit_queued(
            "test.queue",
            identity,
            grants(),
            Value::Null,
            owner.control(),
        )
        .unwrap();
    let broker = capabilities.host_requests();
    broker.take(1).unwrap();
    handle.cancel().unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    assert_eq!(
        owner
            .complete(Ok(Value::Null), EffectState::NotApplicable)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert!(!handle.snapshot().unwrap().phase.is_terminal());
    broker
        .complete(
            request.id(),
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    owner
        .complete(
            Err(crate::runtime::embedded::EmbeddedError::new(
                EmbeddedErrorCode::Cancelled,
                "cancelled",
            )),
            EffectState::NotApplicable,
        )
        .unwrap();
    let terminal = handle.snapshot().unwrap();
    assert_eq!(terminal.phase, OperationPhase::Cancelled);
    assert_eq!(terminal.effects, EffectState::Committed);
    assert!(
        terminal
            .host_effects
            .iter()
            .all(|effect| effect.phase == HostEffectPhase::Completed)
    );
}

/// A control cannot be rebound to another journal or retroactively hide an untracked callback.
/// 控制对象不能重新绑定其他日志，也不能追溯隐藏未跟踪回调。
#[test]
fn embedded_effect_control_ownership_is_frozen_before_callback_execution() {
    let limits = config();
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    register(&capabilities, "test.call", Arc::new(|_| value(Value::Null)));
    let untracked = control();
    capabilities
        .snapshot()
        .unwrap()
        .invoke_native(
            "test.call",
            caller(),
            grants(),
            Value::Null,
            Arc::clone(&untracked),
        )
        .unwrap();
    assert!(untracked.host_effects().unwrap().is_none());
    assert!(
        matches!(operations.admit(untracked), Err(error) if error.code == EmbeddedErrorCode::Busy)
    );
    let registered = control();
    let (handle, owner) = operations.admit(Arc::clone(&registered)).unwrap();
    assert!(
        matches!(operations.admit(registered), Err(error) if error.code == EmbeddedErrorCode::Busy)
    );
    assert_eq!(
        owner.control().operation_id().unwrap().as_deref(),
        Some(handle.snapshot().unwrap().operation_id.as_str())
    );
    // A valid registry caller still cannot attach an unrelated operation identity to this control.
    // 有效注册表调用方仍不能将无关操作身份附加到此控制对象。
    assert_eq!(
        capabilities
            .snapshot()
            .unwrap()
            .invoke_native(
                "test.call",
                caller(),
                grants(),
                Value::Null,
                owner.control()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert!(handle.snapshot().unwrap().host_effects.is_empty());
}

/// Retained evidence stays in actual admission order and preserves independent mixed outcomes.
/// 保留证据维持真实入场顺序，并保留独立混合结果。
#[test]
fn embedded_effect_records_preserve_order_and_independent_outcomes() {
    let limits = config();
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let observed_ids = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&observed_ids);
    register(
        &capabilities,
        "test.effects",
        Arc::new(move |call| {
            observed
                .lock()
                .unwrap()
                .push(call.effect_id.clone().unwrap());
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: match call.arguments.as_str().unwrap() {
                    "commit" => EffectState::Committed,
                    "rollback" => EffectState::RolledBack,
                    "unknown" => EffectState::Unknown,
                    _ => panic!("unexpected fixture effect"),
                },
            }
        }),
    );
    let (handle, mut owner) = operations.admit(control()).unwrap();
    let mut identity = caller();
    identity.operation_id = handle.snapshot().unwrap().operation_id;
    owner.advance(OperationPhase::Running).unwrap();
    // Crossing a decimal boundary catches accidental lexical ordering of opaque string IDs.
    // 跨越十进制位数边界，捕获不透明字符串 ID 意外按字典顺序排序。
    for effect in ["commit", "rollback", "unknown"]
        .into_iter()
        .cycle()
        .take(12)
    {
        capabilities
            .snapshot()
            .unwrap()
            .invoke_native(
                "test.effects",
                identity.clone(),
                grants(),
                json!(effect),
                owner.control(),
            )
            .unwrap();
    }
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotApplicable)
        .unwrap();
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(
        snapshot
            .host_effects
            .iter()
            .map(|effect| effect.effect_id.clone())
            .collect::<Vec<_>>(),
        *observed_ids.lock().unwrap()
    );
    assert_eq!(snapshot.effects, EffectState::Unknown);
    assert!(
        snapshot
            .host_effects
            .iter()
            .any(|effect| effect.effects == EffectState::Committed)
    );
    assert!(
        snapshot
            .host_effects
            .iter()
            .any(|effect| effect.effects == EffectState::RolledBack)
    );
}
