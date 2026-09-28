//! Real capability admission across separately budgeted operation finalization.
//! 跨独立预算操作关闭阶段的真实能力入场。

use super::*;
use crate::runtime::embedded::{EmbeddedError, OperationPhase, OperationRegistry};

/// Keep both outcomes and original effect authority while issuing a closing budget only once.
/// 仅签发一次关闭预算，同时保留两个结果和原副作用权威。
#[test]
fn embedded_operation_finalization_preserves_results_identity_and_stage_admission() {
    for business in [
        Ok(Value::Null),
        Err(EmbeddedError::new(
            EmbeddedErrorCode::Cancelled,
            "business cancelled",
        )),
    ] {
        for closing in [
            Ok(json!("closed")),
            Err(EmbeddedError::new(
                EmbeddedErrorCode::ExecutionFailed,
                "closing failed",
            )),
        ] {
            // Each matrix case owns its capability registry and operation namespace.
            // 每个矩阵用例拥有自身能力注册表与操作命名空间。
            let limits = config();
            let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
            let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = Arc::clone(&calls);
            register(
                &capabilities,
                "test.stage",
                Arc::new(move |_| {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    value(Value::Null)
                }),
            );
            let (handle, mut owner) = operations.admit(control()).unwrap();
            let mut identity = caller();
            identity.operation_id = handle.id().to_owned();
            let snapshot = capabilities.snapshot().unwrap();
            let permissions = grants();
            owner.advance(OperationPhase::Running).unwrap();
            snapshot
                .invoke_native(
                    "test.stage",
                    identity.clone(),
                    Arc::clone(&permissions),
                    Value::Null,
                    owner.control(),
                )
                .unwrap()
                .result
                .unwrap();
            owner
                .prepare_finalization("shutdown".into(), business.clone(), Duration::from_secs(10))
                .unwrap();
            assert!(owner.poll_finalization().unwrap());
            assert_eq!(
                owner
                    .prepare_finalization(
                        "other".into(),
                        Ok(json!("replacement")),
                        Duration::from_secs(10)
                    )
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::Busy
            );
            assert_eq!(
                snapshot
                    .invoke_native(
                        "test.stage",
                        identity.clone(),
                        Arc::clone(&permissions),
                        Value::Null,
                        owner.control()
                    )
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::Closed
            );
            let closing_control = owner.take_finalization_control().unwrap();
            assert_eq!(
                closing_control.operation_id().unwrap().as_deref(),
                Some(handle.id())
            );
            assert_eq!(
                owner.take_finalization_control().unwrap_err().code,
                EmbeddedErrorCode::Closed
            );
            assert_eq!(
                owner
                    .complete(business.clone(), EffectState::NotApplicable)
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::Busy
            );
            handle.cancel().unwrap();
            closing_control.check().unwrap();
            snapshot
                .invoke_native(
                    "test.stage",
                    identity.clone(),
                    Arc::clone(&permissions),
                    Value::Null,
                    Arc::clone(&closing_control),
                )
                .unwrap()
                .result
                .unwrap();
            owner.prepare_finalization_outcome(closing.clone()).unwrap();
            assert!(owner.poll_finalization().unwrap());
            assert_eq!(
                owner
                    .prepare_finalization_outcome(Ok(json!("replacement")))
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::Busy
            );
            assert_eq!(
                snapshot
                    .invoke_native(
                        "test.stage",
                        identity,
                        permissions,
                        Value::Null,
                        closing_control
                    )
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::Closed
            );
            assert_eq!(
                owner
                    .complete(Ok(json!("different business")), EffectState::NotApplicable)
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::InvalidArgument
            );
            owner
                .complete(business.clone(), EffectState::NotApplicable)
                .unwrap();
            let terminal = handle.snapshot().unwrap();
            let evidence = terminal.finalization.as_ref().unwrap();
            assert_eq!(evidence.business.result(), business);
            assert_eq!(evidence.outcome.as_ref().unwrap().result(), closing);
            assert_eq!(evidence.business_effect_count, 1);
            assert_eq!(terminal.host_effects.len(), 2);
            assert!(
                terminal
                    .host_effects
                    .iter()
                    .all(|effect| effect.caller.operation_id == handle.id())
            );
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
            terminal.validate_finalization().unwrap();
            match business
                .clone()
                .and_then(|value| closing.clone().map(|_| value))
            {
                Ok(value) => assert_eq!(terminal.value, Some(value)),
                Err(error) => assert_eq!(terminal.error, Some(error)),
            }
        }
    }
}

/// Closing admission and outcome sealing wait for actual queued-handler release.
/// 关闭入场及结果封存等待真实队列处理器释放。
#[test]
fn embedded_operation_finalization_waits_for_live_handlers() {
    // One queued implementation allows exact observation of both lifecycle barriers.
    // 一个队列实现允许精确观察两个生命周期屏障。
    let limits = config();
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let capabilities = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let mut contract = descriptor("test.stage");
    contract.execution = CapabilityExecution::Queued;
    capabilities
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: None,
        }])
        .unwrap();
    let (handle, mut owner) = operations.admit(control()).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    let mut identity = caller();
    identity.operation_id = handle.id().to_owned();
    let snapshot = capabilities.snapshot().unwrap();
    let broker = capabilities.host_requests();
    let business_request = snapshot
        .submit_queued(
            "test.stage",
            identity.clone(),
            grants(),
            Value::Null,
            owner.control(),
        )
        .unwrap();
    broker.take(1).unwrap();
    assert_eq!(
        owner
            .prepare_finalization("shutdown".into(), Ok(Value::Null), Duration::from_secs(10))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert!(handle.snapshot().unwrap().finalization.is_none());
    broker
        .complete(business_request.id(), value(Value::Null))
        .unwrap();
    owner
        .prepare_finalization("shutdown".into(), Ok(Value::Null), Duration::from_secs(10))
        .unwrap();
    let closing_control = owner.take_finalization_control().unwrap();
    let closing_request = snapshot
        .submit_queued(
            "test.stage",
            identity,
            grants(),
            Value::Null,
            closing_control,
        )
        .unwrap();
    broker.take(1).unwrap();
    assert_eq!(
        owner
            .prepare_finalization_outcome(Ok(Value::Null))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert!(
        handle
            .snapshot()
            .unwrap()
            .finalization
            .unwrap()
            .outcome
            .is_none()
    );
    broker
        .complete(closing_request.id(), value(Value::Null))
        .unwrap();
    owner.prepare_finalization_outcome(Ok(Value::Null)).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotApplicable)
        .unwrap();
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Succeeded);
}

/// Each retained stage result uses the original value bound without losing an explicit null.
/// 每个保留的阶段结果使用原值上限，且不丢失显式空值。
#[test]
fn embedded_operation_finalization_bounds_both_stage_results() {
    // Preserve one authoritative byte limit for both stages.
    // 两个阶段共用一个权威字节上限。
    let limits = config();
    let oversize = json!("x".repeat(limits.max_value_bytes + 1));
    let operations = OperationRegistry::new("runtime-a".into(), &limits).unwrap();
    let (handle, mut owner) = operations.admit(control()).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    owner
        .prepare_finalization(
            "shutdown".into(),
            Ok(oversize.clone()),
            Duration::from_secs(10),
        )
        .unwrap();
    owner.take_finalization_control().unwrap();
    owner
        .prepare_finalization_outcome(Ok(oversize.clone()))
        .unwrap();
    owner.complete(Ok(oversize), EffectState::Unknown).unwrap();
    let terminal = handle.snapshot().unwrap();
    let evidence = terminal.finalization.as_ref().unwrap();
    assert_eq!(
        evidence.business.result().unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        evidence
            .outcome
            .as_ref()
            .unwrap()
            .result()
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    terminal.validate_finalization().unwrap();
}
