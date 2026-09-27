use super::*;
use crate::runtime::embedded::{HostEffectPhase, OperationPhase, OperationRegistry};

/// Real queued commits survive cancellation of the containing Lua invocation and top-level publication.
/// 真实队列提交在包含它的 Lua 调用取消及顶层发布后继续保留。
#[test]
fn embedded_operation_lua_cancel_after_host_commit_preserves_queryable_evidence() {
    // All identities originate from the same runtime registry and admitted operation.
    // 全部身份来自相同运行时注册表与已接纳操作。
    let layout = SystemRuntimeTestLayout::new("embedded operation committed cancellation");
    let manager = pool_manager(&layout);
    let capabilities =
        CapabilityRegistry::new("effect-runtime".into(), manager.config().clone()).unwrap();
    let operations = OperationRegistry::new("effect-runtime".into(), manager.config()).unwrap();
    let mut contract = descriptor("test.write", CapabilityExecution::Queued);
    contract.effects = CapabilityEffects::Mutating;
    capabilities
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: None,
        }])
        .unwrap();
    let (_, binding) = binding(&capabilities);
    let pool = manager
        .create_pool_with_capabilities(
            "effects".into(),
            definition(
                &layout,
                "return {call=function() return vulcan.capabilities.call('test.write',{}) end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            binding,
        )
        .unwrap();
    let (handle, mut owner) = operations.admit(control()).unwrap();
    let id = handle.snapshot().unwrap().operation_id;
    owner.advance(OperationPhase::Initializing).unwrap();
    let mut lease = pool.acquire(owner.control()).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    let invocation_control = owner.control();
    let invocation_id = id.clone();
    let worker = std::thread::spawn(move || {
        lease.invoke(ModuleInvocation {
            operation_id: &invocation_id,
            session_id: None,
            export: "call",
            arguments: &Value::Null,
            context: &LuaInvocationContext::default(),
            control: invocation_control,
        })
    });
    let broker = capabilities.host_requests();
    let deadline = Instant::now() + Duration::from_secs(2);
    let request = loop {
        if let Some(request) = broker.take(1).unwrap().pop() {
            break request;
        }
        assert!(Instant::now() < deadline, "host write was not dispatched");
        std::thread::yield_now();
    };
    assert!(request.effect_id.is_some());
    assert_eq!(request.caller.operation_id, id);
    assert!(handle.cancel().unwrap());
    assert!(!worker.is_finished());
    let live = handle.snapshot().unwrap();
    assert!(
        live.host_effects
            .iter()
            .any(
                |effect| effect.request_id.as_deref() == Some(&request.request_id)
                    && effect.phase == HostEffectPhase::Running
                    && effect.effects == EffectState::Unknown
            )
    );
    broker
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!({"written":true})),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let result = worker.join().unwrap();
    assert_eq!(
        result.as_ref().unwrap_err().code,
        EmbeddedErrorCode::Cancelled
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    // This owner has no separate direct mutation; it must not erase the recorded host commit.
    // 此所有者没有独立直接变更；不得抹去已记录宿主提交。
    owner.complete(result, EffectState::NotApplicable).unwrap();
    let final_state = operations.get(&id).unwrap().snapshot().unwrap();
    assert_eq!(final_state.phase, OperationPhase::Cancelled);
    assert_eq!(final_state.effects, EffectState::Committed);
    assert!(
        final_state
            .host_effects
            .iter()
            .any(
                |effect| effect.effect_id == request.effect_id.as_deref().unwrap()
                    && effect.request_id.as_deref() == Some(&request.request_id)
                    && effect.effects == EffectState::Committed
                    && effect.phase == HostEffectPhase::Completed
            )
    );
    pool.close().unwrap();
    drained(&pool);
}

/// Lua errors and invalid output contracts cannot discard a native host's confirmed commit.
/// Lua 错误与无效输出契约不能丢弃原生宿主确认的提交。
#[test]
fn embedded_operation_lua_failure_and_schema_error_preserve_native_commit() {
    for lua_error in [false, true] {
        // Each case owns a real pool and fresh operation identity.
        // 每个场景拥有真实池与新的操作身份。
        let layout = SystemRuntimeTestLayout::new("embedded operation native committed failure");
        let manager = pool_manager(&layout);
        let capabilities =
            CapabilityRegistry::new("native-effect-runtime".into(), manager.config().clone())
                .unwrap();
        let operations =
            OperationRegistry::new("native-effect-runtime".into(), manager.config()).unwrap();
        let mut contract = descriptor("test.write", CapabilityExecution::Native);
        contract.effects = CapabilityEffects::Mutating;
        capabilities
            .register(vec![CapabilityRegistrationRequest {
                descriptor: contract,
                native: Some(Arc::new(|_| CapabilityOutcome {
                    result: Ok(json!(true)),
                    effects: EffectState::Committed,
                })),
            }])
            .unwrap();
        let (_, binding) = binding(&capabilities);
        let source = if lua_error {
            "return {call=function() vulcan.capabilities.call('test.write',{}); error('after commit') end}"
        } else {
            "return {call=function() vulcan.capabilities.call('test.write',{}); return 'wrong type' end}"
        };
        let mut declaration = definition(&layout, source);
        declaration
            .exports
            .iter_mut()
            .find(|export| export.name == "call")
            .unwrap()
            .output_schema = json!({"type":"integer"});
        let pool = manager
            .create_pool_with_capabilities(
                "native-effects".into(),
                declaration,
                pool_policy(InstanceReuse::Reusable),
                binding,
            )
            .unwrap();
        let (handle, mut owner) = operations.admit(control()).unwrap();
        let id = handle.snapshot().unwrap().operation_id;
        owner.advance(OperationPhase::Initializing).unwrap();
        let mut lease = pool.acquire(owner.control()).unwrap();
        owner.advance(OperationPhase::Running).unwrap();
        let result = lease.invoke(ModuleInvocation {
            operation_id: &id,
            session_id: None,
            export: "call",
            arguments: &Value::Null,
            context: &LuaInvocationContext::default(),
            control: owner.control(),
        });
        assert!(result.is_err());
        drop(lease);
        drained(&pool);
        owner.advance(OperationPhase::Cleaning).unwrap();
        owner.complete(result, EffectState::RolledBack).unwrap();
        let snapshot = handle.snapshot().unwrap();
        assert_eq!(snapshot.phase, OperationPhase::Failed);
        assert_eq!(
            snapshot.effects,
            EffectState::Committed,
            "a later independent rollback must not erase a host commit"
        );
        assert!(
            snapshot
                .host_effects
                .iter()
                .any(|effect| effect.effects == EffectState::Committed
                    && effect.phase == HostEffectPhase::Completed)
        );
        pool.close().unwrap();
    }
}
