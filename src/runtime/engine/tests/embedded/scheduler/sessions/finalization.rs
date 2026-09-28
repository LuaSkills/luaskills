//! Closing a pinned VM owns an independent operation and reserved retention capacity.
//! 关闭固定 VM 拥有独立操作及预留保留容量。

use super::super::finalization::closing_definition;
use super::*;

/// Idle generation, plugin and runtime closure all retain executors until session finalizers actually finish.
/// 空闲代次、插件和运行时关闭均保留执行器，直到会话关闭回调实际结束。
#[test]
fn embedded_session_finalization_idle_generation_and_runtime_closure() {
    for scope in ["pool", "plugin", "runtime"] {
        let layout = SystemRuntimeTestLayout::new("idle session lifecycle closure");
        let runtime = runtime(&layout, pool_config());
        let pool = runtime.register_pool(
            closing_definition(&layout, "return {call=function() return 0 end, shutdown=function() return 'idle closed' end}", 1000),
            pool_policy(InstanceReuse::Session), permissions(), "r1".into(),
        ).unwrap();
        let session = opened(&runtime, &pool);
        match scope {
            "pool" => runtime.close_pool(&pool).unwrap(),
            "plugin" => runtime.close_plugin(&layout.package_id).unwrap(),
            "runtime" => runtime.request_close().unwrap(),
            _ => unreachable!(),
        }
        let closing = closed_operation(&runtime, &session);
        assert_eq!(closing.phase, OperationPhase::Succeeded);
        assert_eq!(
            closing
                .finalization
                .unwrap()
                .outcome
                .unwrap()
                .result()
                .unwrap(),
            json!("idle closed")
        );
        shutdown(&runtime);
    }
}

/// Closing context must fit its ledger before initialization, rather than discovering impossible cleanup admission later.
/// 关闭上下文必须在初始化前满足账本容量，不能事后才发现清理无法入场。
#[test]
fn embedded_session_finalization_context_capacity_is_validated_before_opening() {
    let layout = SystemRuntimeTestLayout::new("session closing context capacity");
    let mut config = pool_config();
    config.max_effect_bytes_per_operation = 1;
    let runtime = runtime(&layout, config);
    let pool = runtime
        .register_pool(
            closing_definition(&layout, "error('must not initialize')", 1000),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .open_session(&pool, Duration::from_secs(1))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let usage = runtime.plugin(&layout.package_id).unwrap();
    assert_eq!(
        (
            usage.retained_operations,
            usage.reserved_operations,
            usage.retained_sessions
        ),
        (0, 0, 0)
    );
    shutdown(&runtime);
}

/// Read the final closing result after the session's actual ownership has drained.
/// 在会话实际所有权排空后读取最终关闭结果。
fn closed_operation(runtime: &EmbeddedRuntime, session: &str) -> OperationSnapshot {
    let closed = closed_session(runtime, session);
    runtime
        .operation(
            closed
                .finalization_operation
                .as_ref()
                .expect("closing operation"),
        )
        .unwrap()
        .snapshot()
        .unwrap()
}

/// Multiple business results stay immutable while closing observes the original VM's accumulated state.
/// 多个业务结果保持不可变，而关闭观察原 VM 的累计状态。
#[test]
fn embedded_session_finalization_uses_independent_identity_and_same_vm() {
    let layout = SystemRuntimeTestLayout::new("session finalization state");
    let runtime = runtime(&layout, pool_config());
    let pool = runtime.register_pool(
        closing_definition(&layout, "local n=0; return {call=function() n=n+1; return n end, shutdown=function() return n end}", 1000),
        pool_policy(InstanceReuse::Session), permissions(), "r1".into(),
    ).unwrap();
    let session = opened(&runtime, &pool);
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .reserved_operations,
        1
    );
    let mut business = Vec::new();
    for expected in 1..=3 {
        let operation = session_call(&runtime, &session, Value::Null);
        let snapshot = operation.wait(Duration::from_secs(3)).unwrap();
        assert_eq!(snapshot.value, Some(json!(expected)));
        assert!(snapshot.finalization.is_none());
        business.push((operation, serde_json::to_value(snapshot).unwrap()));
    }
    runtime.close_session(&session).unwrap();
    let closing = closed_operation(&runtime, &session);
    assert_eq!(closing.phase, OperationPhase::Succeeded);
    assert_eq!(closing.value, Some(Value::Null));
    let stages = closing.finalization.unwrap();
    assert_eq!(stages.business.result().unwrap(), Value::Null);
    assert_eq!(stages.outcome.unwrap().result().unwrap(), json!(3));
    assert_eq!(stages.business_effect_count, 0);
    for (operation, previous) in business {
        assert_ne!(operation.id(), closing.operation_id);
        assert_eq!(
            serde_json::to_value(operation.snapshot().unwrap()).unwrap(),
            previous
        );
    }
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .reserved_operations,
        0
    );
    runtime.close_session(&session).unwrap();
    assert_eq!(
        closed_operation(&runtime, &session).operation_id,
        closing.operation_id
    );
    shutdown(&runtime);
}

/// Closing remains admitted when either global or plugin operation retention is otherwise exhausted.
/// 全局或插件操作保留容量已耗尽时，关闭仍然可以入场。
#[test]
fn embedded_session_finalization_reserved_capacity_survives_full_retention() {
    for global in [true, false] {
        let layout = SystemRuntimeTestLayout::new("session finalization full retention");
        let mut config = pool_config();
        config.max_queued_calls = 2;
        if global {
            config.max_operations = 3;
        }
        let mut plugin = plugin_policy(&config);
        plugin.max_operations = 3;
        let runtime = runtime_with_plugin(&layout, config, plugin);
        let mut policy = pool_policy(InstanceReuse::Session);
        policy.max_queued_calls = 2;
        let pool = runtime.register_pool(
            closing_definition(&layout, "return {call=function() return 7 end, shutdown=function() return 'closed' end}", 1000),
            policy, permissions(), "r1".into(),
        ).unwrap();
        let session = opened(&runtime, &pool);
        assert_eq!(
            session_call(&runtime, &session, Value::Null)
                .wait(Duration::from_secs(3))
                .unwrap()
                .value,
            Some(json!(7))
        );
        assert_eq!(
            runtime
                .submit_session(
                    &session,
                    "call".into(),
                    Value::Null,
                    LuaInvocationContext::default(),
                    Duration::from_secs(1)
                )
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::CapacityExceeded
        );
        let usage = runtime.plugin(&layout.package_id).unwrap();
        assert_eq!(
            (usage.retained_operations, usage.reserved_operations),
            (2, 1)
        );
        runtime.close_session(&session).unwrap();
        assert_eq!(
            closed_operation(&runtime, &session).phase,
            OperationPhase::Succeeded
        );
        let usage = runtime.plugin(&layout.package_id).unwrap();
        assert_eq!(
            (usage.retained_operations, usage.reserved_operations),
            (3, 0)
        );
        shutdown(&runtime);
    }
}

/// Rejected opening releases both reservation budgets before any source code can execute.
/// 被拒绝的开启在任何源码可以执行前释放两个预留预算。
#[test]
fn embedded_session_finalization_open_rejection_releases_reserved_capacity() {
    let layout = SystemRuntimeTestLayout::new("session finalization admission rollback");
    let mut config = pool_config();
    config.max_queued_calls = 1;
    config.max_running_calls = 1;
    config.max_operations = 1;
    let runtime = runtime(&layout, config);
    let mut policy = pool_policy(InstanceReuse::Session);
    policy.max_queued_calls = 1;
    policy.max_running_calls = 1;
    let pool = runtime
        .register_pool(
            closing_definition(&layout, "error('must not initialize')", 1000),
            policy,
            permissions(),
            "r1".into(),
        )
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            runtime
                .open_session(&pool, Duration::from_secs(1))
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::CapacityExceeded
        );
        let usage = runtime.plugin(&layout.package_id).unwrap();
        assert_eq!(
            (
                usage.retained_operations,
                usage.reserved_operations,
                usage.retained_sessions
            ),
            (0, 0, 0)
        );
        assert_eq!(usage.resources.resident, 0);
    }
    shutdown(&runtime);
}

/// Error, deadline and use exhaustion close the retained VM once and preserve original business evidence.
/// 错误、截止及使用额度耗尽均只关闭保留 VM 一次，并保留原业务证据。
#[test]
fn embedded_session_finalization_automatic_failure_and_use_exhaustion() {
    for (business, expected_error, max_uses) in [
        (
            "error('business failed')",
            Some(EmbeddedErrorCode::ExecutionFailed),
            None,
        ),
        (
            "while true do end",
            Some(EmbeddedErrorCode::DeadlineExceeded),
            None,
        ),
        ("return n", None, Some(1)),
    ] {
        for (closing, closing_error) in [
            ("return n", None),
            (
                "error('closing failed')",
                Some(EmbeddedErrorCode::ExecutionFailed),
            ),
            (
                "while true do end",
                Some(EmbeddedErrorCode::DeadlineExceeded),
            ),
        ] {
            let layout = SystemRuntimeTestLayout::new("session automatic closing outcomes");
            let runtime = runtime(&layout, pool_config());
            let source = format!(
                "local n=0; return {{call=function() n=n+1; {business} end, shutdown=function() assert(n==1); {closing} end}}"
            );
            let mut policy = pool_policy(InstanceReuse::Session);
            policy.max_uses = max_uses;
            let pool = runtime
                .register_pool(
                    closing_definition(&layout, &source, 50),
                    policy,
                    permissions(),
                    "r1".into(),
                )
                .unwrap();
            let session = opened(&runtime, &pool);
            let business = runtime
                .submit_session(
                    &session,
                    "call".into(),
                    Value::Null,
                    LuaInvocationContext::default(),
                    Duration::from_millis(100),
                )
                .unwrap();
            let result = business.wait(Duration::from_secs(3)).unwrap();
            assert_eq!(
                result.error.as_ref().map(|error| error.code),
                expected_error
            );
            let closing = closed_operation(&runtime, &session);
            assert_eq!(
                closing.error.as_ref().map(|error| error.code),
                closing_error
            );
            assert_eq!(
                runtime
                    .session(&session)
                    .unwrap()
                    .error
                    .as_ref()
                    .map(|error| error.code),
                expected_error.or(closing_error)
            );
            assert_eq!(
                serde_json::to_value(business.snapshot().unwrap()).unwrap(),
                serde_json::to_value(result).unwrap()
            );
            shutdown(&runtime);
        }
    }
}

/// Runtime shutdown keeps callback admission alive for a new closing identity with the original session authority.
/// 运行时关闭为新关闭身份保留回调入场，并使用原会话权威。
#[test]
fn embedded_session_finalization_runtime_close_waits_for_actual_host_ack() {
    let layout = SystemRuntimeTestLayout::new("session finalization shutdown callbacks");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::super::capabilities::descriptor(
                "test.close",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let pool = runtime.register_pool(
        closing_definition(&layout, "local host=vulcan.host.call; return {call=function() local r=host('test.close','business'); assert(r.ok); return r.value end, shutdown=function() local r=host('test.close','closing'); assert(r.ok); return r.value end}", 5000),
        pool_policy(InstanceReuse::Session), permissions(), "r1".into(),
    ).unwrap();
    let session = opened(&runtime, &pool);
    let operation = session_call(&runtime, &session, Value::Null);
    let business = host_request(&runtime);
    runtime.request_close().unwrap();
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &business.request_id,
            CapabilityOutcome {
                result: Ok(json!("ack")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let closing = host_request(&runtime);
    assert_eq!(closing.arguments, json!("closing"));
    assert_ne!(closing.caller.operation_id, business.caller.operation_id);
    assert_eq!(closing.caller.session_id, Some(session.clone()));
    assert_eq!(
        runtime
            .session(&session)
            .unwrap()
            .finalization_operation
            .as_deref(),
        Some(closing.caller.operation_id.as_str())
    );
    assert!(!runtime.poll_closed().unwrap());
    assert_eq!(
        operation.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
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
    let result = closed_operation(&runtime, &session);
    assert_eq!(result.phase, OperationPhase::Succeeded);
    assert_eq!(result.host_effects.len(), 1);
    assert_eq!(
        result
            .finalization
            .unwrap()
            .outcome
            .unwrap()
            .result()
            .unwrap(),
        json!("closed")
    );
    shutdown(&runtime);
}

/// Failed initialization never invokes a closing export and releases unused lifecycle capacity.
/// 失败初始化绝不调用关闭导出，并释放未使用的生命周期容量。
#[test]
fn embedded_session_finalization_initialization_failure_releases_reservation() {
    let layout = SystemRuntimeTestLayout::new("session closing initialization failure");
    let runtime = runtime(&layout, pool_config());
    let pool = runtime
        .register_pool(
            closing_definition(&layout, "error('initialization failed')", 1000),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let opening = runtime.open_session(&pool, Duration::from_secs(1)).unwrap();
    assert_eq!(
        opening
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Failed
    );
    assert!(
        closed_session(&runtime, &opening.session_id)
            .finalization_operation
            .is_none()
    );
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .reserved_operations,
        0
    );
    runtime.forget_operation(opening.operation.id()).unwrap();
    runtime.forget_session(&opening.session_id).unwrap();
    shutdown(&runtime);
}
