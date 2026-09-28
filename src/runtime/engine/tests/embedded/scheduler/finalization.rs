//! Automatic closing uses the original real Lua VM and independent durable stage evidence.
//! 自动关闭使用原始真实 Lua VM 及独立持久阶段证据。

use super::*;

/// Declare `source` with one explicit shutdown export and finite `timeout_ms`.
/// 为 `source` 声明一个显式关闭导出及有限的 `timeout_ms`。
pub(super) fn closing_definition(
    layout: &SystemRuntimeTestLayout,
    source: &str,
    timeout_ms: u64,
) -> ModuleDefinition {
    let mut module = definition(layout, source);
    module.exports.push(ModuleExport {
        name: "shutdown".into(),
        input_schema: json!({"type":"null"}),
        output_schema: json!(true),
    });
    module.finalizer = Some(ModuleFinalizer {
        export: "shutdown".into(),
        arguments: Value::Null,
        timeout_ms,
    });
    module
}

/// Preserve both stages on the same VM, including business errors and independently timed closing failures.
/// 在同一 VM 上保留两个阶段，包括业务错误及独立计时的关闭失败。
#[test]
fn embedded_scheduler_finalization_preserves_both_stage_outcomes() {
    for (business, business_error) in [
        ("return count", None),
        (
            "error('business failed')",
            Some(EmbeddedErrorCode::ExecutionFailed),
        ),
        (
            "while true do end",
            Some(EmbeddedErrorCode::DeadlineExceeded),
        ),
    ] {
        for (closing, closing_error) in [
            ("return count", None),
            (
                "error('shutdown failed')",
                Some(EmbeddedErrorCode::ExecutionFailed),
            ),
            (
                "while true do end",
                Some(EmbeddedErrorCode::DeadlineExceeded),
            ),
        ] {
            let layout = SystemRuntimeTestLayout::new("automatic closing stage outcomes");
            let runtime = runtime(&layout, pool_config());
            let source = format!(
                "local count=0; return {{call=function() count=count+1; {business} end, shutdown=function() assert(count==1); {closing} end}}"
            );
            let pool = runtime
                .register_pool(
                    closing_definition(&layout, &source, 50),
                    pool_policy(InstanceReuse::SingleCall),
                    permissions(),
                    "r1".into(),
                )
                .unwrap();
            let operation = runtime
                .submit(call(&pool, Value::Null), Duration::from_millis(500))
                .unwrap();
            let snapshot = operation.wait(Duration::from_secs(3)).unwrap();
            let stages = snapshot
                .finalization
                .as_ref()
                .expect("initialized VM must close");
            let expected = business_error.or(closing_error);
            assert_eq!(snapshot.error.as_ref().map(|error| error.code), expected);
            assert_eq!(
                stages
                    .business
                    .result()
                    .as_ref()
                    .err()
                    .map(|error| error.code),
                business_error
            );
            assert_eq!(
                stages
                    .outcome
                    .as_ref()
                    .unwrap()
                    .result()
                    .as_ref()
                    .err()
                    .map(|error| error.code),
                closing_error
            );
            if expected.is_none() {
                assert_eq!(snapshot.value, Some(json!(1)));
                assert_eq!(stages.outcome.as_ref().unwrap().result().unwrap(), json!(1));
            }
            assert_eq!(runtime.usage().unwrap().cleaning_operations, 0);
            shutdown(&runtime);
        }
    }
}

/// Closing after runtime cancellation still dispatches queued host cleanup with original operation identity.
/// 运行时取消后仍使用原操作身份分发排队宿主清理。
#[test]
fn embedded_scheduler_finalization_survives_runtime_close_and_waits_for_host_ack() {
    let layout = SystemRuntimeTestLayout::new("automatic closing shutdown host request");
    let runtime = runtime(&layout, pool_config());
    let mut descriptor =
        super::super::capabilities::descriptor("test.close", CapabilityExecution::Queued);
    descriptor.effects = CapabilityEffects::Mutating;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: None,
        }])
        .unwrap();
    let pool = runtime.register_pool(
        closing_definition(&layout, "local host=vulcan.host.call; return {call=function() local r=host('test.close','business'); assert(r.ok); return r.value end, shutdown=function() local r=host('test.close','closing'); assert(r.ok,r.error and r.error.message); return r.value end}", 5000),
        pool_policy(InstanceReuse::SingleCall), permissions(), "r1".into(),
    ).unwrap();
    let operation = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(5))
        .unwrap();
    let business = host_request(&runtime);
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &business.request_id,
            CapabilityOutcome {
                result: Ok(json!("business acknowledged")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let closing = loop {
        if let Some(request) = runtime
            .capabilities()
            .host_requests()
            .take(1)
            .unwrap()
            .pop()
        {
            break request;
        }
        let observed = operation.snapshot().unwrap();
        assert!(
            !observed.phase.is_terminal() && Instant::now() < deadline,
            "closing request missing: {observed:?}; usage: {:?}",
            runtime.usage()
        );
        std::thread::yield_now();
    };
    assert_eq!(closing.arguments, json!("closing"));
    assert_eq!(closing.caller.operation_id, operation.id());
    assert_eq!(closing.caller, business.caller);
    assert!(!runtime.poll_closed().unwrap());
    assert_eq!(
        operation.snapshot().unwrap().phase,
        OperationPhase::Cleaning
    );
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &closing.request_id,
            CapabilityOutcome {
                result: Ok(json!("closed")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let snapshot = operation.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Cancelled);
    let stages = snapshot.finalization.unwrap();
    assert_eq!(stages.business_effect_count, 1);
    assert_eq!(stages.outcome.unwrap().result().unwrap(), json!("closed"));
    assert_eq!(snapshot.host_effects.len(), 2);
    assert!(
        snapshot
            .host_effects
            .iter()
            .all(|effect| effect.effects == EffectState::Committed)
    );
    shutdown(&runtime);
}

/// Invalid contracts and unsupported reuse are rejected before any plugin initialization.
/// 非法契约及不支持的复用模式在任何插件初始化前被拒绝。
#[test]
fn embedded_scheduler_finalization_validates_registration_and_initialization() {
    let layout = SystemRuntimeTestLayout::new("automatic closing admission validation");
    let runtime = runtime(&layout, pool_config());
    for reuse in [InstanceReuse::Reusable, InstanceReuse::Session] {
        let error = runtime
            .register_pool(
                closing_definition(&layout, "error('must not initialize')", 100),
                pool_policy(reuse),
                permissions(),
                "r1".into(),
            )
            .unwrap_err();
        assert_eq!(error.code, EmbeddedErrorCode::Unsupported);
    }
    for invalid in ["export", "arguments", "timeout"] {
        let mut module = closing_definition(&layout, "error('must not initialize')", 100);
        let closing = module.finalizer.as_mut().unwrap();
        match invalid {
            "export" => closing.export = "missing".into(),
            "arguments" => closing.arguments = json!(1),
            "timeout" => closing.timeout_ms = 0,
            _ => unreachable!(),
        }
        assert!(
            runtime
                .register_pool(
                    module,
                    pool_policy(InstanceReuse::SingleCall),
                    permissions(),
                    "r1".into()
                )
                .is_err()
        );
    }
    let pool = runtime
        .register_pool(
            closing_definition(&layout, "error('initialization failed')", 100),
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let snapshot = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(3))
        .unwrap()
        .wait(Duration::from_secs(3))
        .unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Failed);
    assert!(snapshot.finalization.is_none());
    shutdown(&runtime);
}

/// Revoking the exact capability remains authoritative even during a registered closing stage.
/// 即使处于已注册关闭阶段，撤销精确能力仍保持权威。
#[test]
fn embedded_scheduler_finalization_respects_revocation() {
    let layout = SystemRuntimeTestLayout::new("automatic closing revoked capability");
    let runtime = runtime(&layout, pool_config());
    let registration = runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.close",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap()
        .remove(0);
    let pool = runtime.register_pool(
        closing_definition(&layout, "local host=vulcan.host.call; return {call=function() local r=host('test.close','business'); assert(r.ok); return r.value end, shutdown=function() local r=host('test.close','closing'); assert(r.ok,r.error and r.error.message); return r.value end}", 5000),
        pool_policy(InstanceReuse::SingleCall), permissions(), "r1".into(),
    ).unwrap();
    let operation = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(5))
        .unwrap();
    let business = host_request(&runtime);
    runtime.capabilities().unregister(&registration).unwrap();
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &business.request_id,
            CapabilityOutcome {
                result: Ok(json!(1)),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    let snapshot = operation.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Failed);
    let stages = snapshot.finalization.unwrap();
    assert!(stages.outcome.unwrap().result().is_err());
    assert!(
        runtime
            .capabilities()
            .host_requests()
            .take(1)
            .unwrap()
            .is_empty()
    );
    shutdown(&runtime);
}
