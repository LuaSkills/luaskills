//! Actual Lua stage restrictions retain live permission and queue ownership semantics.
//! 真实 Lua 阶段限制保留实时权限及队列归属语义。

use super::*;

/// Frozen initialization names constrain cached facades and permission-free callbacks without changing exports.
/// 冻结初始化名称限制已缓存外观及无权限回调，且不改变导出权限。
#[test]
fn embedded_initialization_policy_narrows_all_lua_facades_and_preserves_business() {
    // Both explicit deny-all and one-name policies execute the same real module source.
    // 显式全部拒绝及单名称策略执行相同真实模块源码。
    for names in [BTreeSet::new(), BTreeSet::from(["test.init".into()])] {
        let layout = SystemRuntimeTestLayout::new("initialization capability narrowing");
        let manager = pool_manager(&layout);
        let registry =
            CapabilityRegistry::new("initialization-policy".into(), manager.config().clone())
                .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        for name in ["test.init", "test.business", "test.public"] {
            // Descriptor permissions deliberately cannot substitute for the exact-name initialization restriction.
            // 描述符权限刻意不能替代精确名称初始化限制。
            let mut declaration = descriptor(name, CapabilityExecution::Native);
            if name == "test.public" {
                declaration.permissions.clear();
            }
            let observed = Arc::clone(&calls);
            registry
                .register(vec![CapabilityRegistrationRequest {
                    descriptor: declaration,
                    native: Some(Arc::new(move |invocation| {
                        observed.lock().unwrap().push(name);
                        CapabilityOutcome {
                            result: Ok(invocation.arguments.clone()),
                            effects: EffectState::NotApplicable,
                        }
                    })),
                }])
                .unwrap();
        }
        registry
            .register(vec![CapabilityRegistrationRequest {
                descriptor: descriptor("test.queue", CapabilityExecution::Queued),
                native: None,
            }])
            .unwrap();
        let (_, original) = binding(&registry);
        let capabilities = original
            .clone()
            .with_initialization_capabilities(names.clone())
            .unwrap();
        assert!(original.initialization_capabilities().is_none());
        assert_eq!(capabilities.initialization_capabilities(), Some(&names));
        let pool = manager.create_pool_with_capabilities("restricted".into(), definition(&layout, r#"
local call, has, list = vulcan.host.call, vulcan.host.has, vulcan.host.list
local initialization = {
  allowed = call('test.init', nil), business = call('test.business', {phase='export'}),
  public = call('test.public', nil), queued = call('test.queue', nil),
  init_visible = has('test.init'), business_visible = has('test.business'), count = #list()
}
return {call=function(a)
  return {initialization=initialization, visible=has(a.name), count=#list(), response=call(a.name,a)}
end}
"#), pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
        let mut lease = pool.acquire(control()).unwrap();
        assert_eq!(calls.lock().unwrap().len(), names.len());
        assert!(registry.host_requests().take(1).unwrap().is_empty());
        // Cached facade closures must use the current host stage, regardless of source-provided phase hints.
        // 已缓存外观闭包必须使用当前宿主阶段，与源码提供的阶段提示无关。
        let arguments = json!({"name":"test.business","phase":"initialization"});
        let result = lease
            .invoke(ModuleInvocation {
                operation_id: "business-after-initialization",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap();
        assert_eq!(result["initialization"]["allowed"]["ok"], !names.is_empty());
        assert_eq!(result["initialization"]["init_visible"], !names.is_empty());
        assert_eq!(result["initialization"]["business_visible"], false);
        assert_eq!(result["initialization"]["count"], names.len());
        for name in ["business", "public", "queued"] {
            assert_eq!(
                result["initialization"][name]["error"]["code"],
                "permission_denied"
            );
            assert_eq!(result["initialization"][name]["effects"], "not_started");
        }
        assert_eq!(result["visible"], true);
        assert_eq!(result["count"], 4);
        assert_eq!(result["response"]["value"], arguments);
        assert_eq!(calls.lock().unwrap().last(), Some(&"test.business"));
        pool.close().unwrap();
        drop(lease);
        drained(&pool);
    }
}

/// A frozen initialization declaration never restores a grant revoked before the actual VM starts.
/// 冻结初始化声明绝不恢复真实 VM 启动前已撤回的授权。
#[test]
fn embedded_initialization_policy_keeps_live_revocation_and_rejects_unavailable_names() {
    let layout = SystemRuntimeTestLayout::new("initialization live revocation");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("initialization-revoked".into(), manager.config().clone()).unwrap();
    let calls = Arc::new(Mutex::new(0));
    let observed = Arc::clone(&calls);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.init", CapabilityExecution::Native),
            native: Some(Arc::new(move |_| {
                *observed.lock().unwrap() += 1;
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    let (permissions, original) = binding(&registry);
    assert_eq!(
        original
            .clone()
            .with_initialization_capabilities(BTreeSet::from(["missing".into()]))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::PermissionDenied
    );
    let capabilities = original
        .clone()
        .with_initialization_capabilities(BTreeSet::from(["test.init".into()]))
        .unwrap();
    permissions.revoke("test.host").unwrap();
    assert_eq!(
        original
            .with_initialization_capabilities(BTreeSet::from(["test.init".into()]))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::PermissionDenied
    );
    let pool = manager.create_pool_with_capabilities("revoked".into(), definition(&layout, r#"
assert(not vulcan.host.has('test.init'))
assert(#vulcan.host.list() == 0)
local result = vulcan.host.call('test.init',nil)
assert(not result.ok and result.error.code == 'permission_denied' and result.effects == 'not_started')
return {call=function() return true end}
"#), pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    let lease = pool.acquire(control()).unwrap();
    assert_eq!(*calls.lock().unwrap(), 0);
    pool.close().unwrap();
    drop(lease);
    drained(&pool);
}

/// An allowed queued initializer retains actual VM ownership after cancellation until its exact handler acknowledges.
/// 已允许排队初始化器在取消后保留真实 VM 归属，直到精确处理器确认。
#[test]
fn embedded_initialization_policy_preserves_queued_callback_cancellation_ownership() {
    let layout = SystemRuntimeTestLayout::new("initialization queued ownership");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("initialization-queued".into(), manager.config().clone()).unwrap();
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.queue", CapabilityExecution::Queued),
            native: None,
        }])
        .unwrap();
    let (_, original) = binding(&registry);
    let capabilities = original
        .with_initialization_capabilities(BTreeSet::from(["test.queue".into()]))
        .unwrap();
    let pool = manager.create_pool_with_capabilities("queued".into(), definition(&layout,
        "assert(vulcan.host.call('test.queue',nil).ok); return {call=function() return true end}"),
        pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    let cancellation = control();
    let executing = Arc::clone(&pool);
    let execution_control = Arc::clone(&cancellation);
    let worker = std::thread::spawn(move || executing.acquire(execution_control).is_err());
    let broker = registry.host_requests();
    let deadline = Instant::now() + Duration::from_secs(2);
    let request = loop {
        if let Some(request) = broker.take(1).unwrap().pop() {
            break request;
        }
        assert!(
            Instant::now() < deadline,
            "allowed initializer did not dispatch"
        );
        std::thread::yield_now();
    };
    cancellation.cancel();
    assert!(!worker.is_finished());
    assert_eq!(manager.usage().unwrap().resident, 1);
    broker
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    assert!(worker.join().unwrap());
    pool.close().unwrap();
    drained(&pool);
    assert!(broker.is_drained().unwrap());
}
