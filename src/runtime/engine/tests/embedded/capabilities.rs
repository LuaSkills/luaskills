use super::pools::{drained, pool_manager, pool_policy};
use super::*;
use crate::runtime::embedded::capabilities::*;
use crate::runtime::embedded::{EffectState, InstanceReuse};
use std::collections::BTreeSet;

mod effects;
mod initialization;

/// Forward structured module input through the real native boundary without losing empty container kinds.
/// 经真实原生边界转发结构化模块输入，且不丢失空容器类型。
#[test]
fn embedded_capability_forwarded_arguments_preserve_json_container_types() {
    // The actual module and native callback share one immutable registry binding.
    // 真实模块与原生回调共享一个不可变注册表绑定。
    let layout = SystemRuntimeTestLayout::new("embedded forwarded containers");
    // Reuse the existing explicit pool budgets and actual VM owner.
    // 复用现有显式池预算与真实 VM 所有者。
    let manager = pool_manager(&layout);
    // Independent runtime namespace prevents global bridge fallback from masking conversion errors.
    // 独立运行时命名空间避免全局桥接回退掩盖转换错误。
    let registry =
        CapabilityRegistry::new("container-runtime".into(), manager.config().clone()).unwrap();
    // Strict object input reproduces the host's real configuration and storage contracts.
    // 严格对象输入复现宿主真实配置及存储契约。
    let mut contract = descriptor("test.forward", CapabilityExecution::Native);
    contract.input_schema = json!({"type":"object"});
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: Some(Arc::new(|invocation| CapabilityOutcome {
                result: Ok(invocation.arguments.clone()),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap();
    // The native result travels back through the typed module result serializer.
    // 原生结果再经类型化模块结果序列化器返回。
    let (_, capabilities) = binding(&registry);
    // The module directly forwards the supplied object, including an empty root object.
    // 模块直接转发所提供对象，包括空根对象。
    let pool = manager
        .create_pool_with_capabilities(
            "containers".into(),
            definition(
                &layout,
                "return {call=function(a) return vulcan.host.call('test.forward',a) end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            capabilities,
        )
        .unwrap();
    // Retain one VM so subsequent conversions also exercise cached metatables.
    // 保留一个 VM，使后续转换同时验证缓存元表。
    let mut lease = pool.acquire(control()).unwrap();
    for arguments in [
        json!({}),
        json!({"object":{},"array":[],"nested":[{},[],null],"text":"中文\u{0}","fraction":1.25}),
        // Safe endpoints and explicit large floats traverse both real callback value boundaries.
        // 安全端点及显式大浮点数经过两个真实回调值边界。
        json!({"min":-LuaEngine::EMBEDDED_MAX_SAFE_INTEGER,"max":LuaEngine::EMBEDDED_MAX_SAFE_INTEGER,
            "float":(LuaEngine::EMBEDDED_MAX_SAFE_INTEGER + 1) as f64}),
    ] {
        // Assert on the complete envelope so a schema rejection is visible as a failed round trip.
        // 对完整信封断言，使 Schema 拒绝作为往返失败可见。
        let result = lease
            .invoke(ModuleInvocation {
                operation_id: "container-forward",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap();
        assert_eq!(
            result,
            json!({"ok":true,"value":arguments,"effects":"not_applicable"})
        );
    }
    pool.close().unwrap();
    drop(lease);
    drained(&pool);
}

/// Return a complete capability contract for actual Lua-to-host integration tests.
/// 返回真实 Lua 到宿主集成测试所用的完整能力契约。
pub(super) fn descriptor(name: &str, execution: CapabilityExecution) -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: name.into(),
        version: "1.0.0".into(),
        description: "Embedded integration probe".into(),
        input_schema: json!(true),
        output_schema: json!(true),
        execution,
        permissions: BTreeSet::from(["test.host".into()]),
        scope: CapabilityScope::Invocation,
        max_concurrent: 2,
        max_call_ms: 5000,
        max_input_bytes: 1024,
        max_output_bytes: 1024,
        effects: CapabilityEffects::ReadOnly,
        idempotency: CapabilityIdempotency::None,
    }
}

/// Return live test permissions and an immutable binding for the registry's current snapshot.
/// 返回实时测试权限与注册表当前快照的不可变绑定。
pub(super) fn binding(
    registry: &CapabilityRegistry,
) -> (Arc<CapabilityPermissions>, ModuleCapabilities) {
    // A distinct permission owner lets revocation affect an already reused VM.
    // 独立权限所有者使撤权可以影响已经复用的 VM。
    let permissions = CapabilityPermissions::new(BTreeSet::from(["test.host".into()])).unwrap();
    let binding = ModuleCapabilities::new(
        registry.snapshot().unwrap(),
        Arc::clone(&permissions),
        "execution-v1".into(),
    )
    .unwrap();
    (permissions, binding)
}

/// Initialization and reused calls obtain trusted operation identities and observe live revocation.
/// 初始化与复用调用获得可信操作身份，并观察实时撤权。
#[test]
fn embedded_capability_lua_binds_authority_before_initialization_and_refreshes_calls() {
    // Capture actual native invocation authority from real Lua callbacks.
    // 从真实 Lua 回调捕获实际原生调用权威。
    let layout = SystemRuntimeTestLayout::new("embedded capabilities identity");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("runtime-identity".into(), manager.config().clone()).unwrap();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&observations);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.echo", CapabilityExecution::Native),
            native: Some(Arc::new(move |invocation| {
                captured.lock().unwrap().push(invocation.caller.clone());
                CapabilityOutcome {
                    result: Ok(invocation.arguments.clone()),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    // Module initialization calls the same bound capability before its export is captured.
    // 模块初始化在导出被捕获前调用同一个已绑定能力。
    let (permissions, capabilities) = binding(&registry);
    let pool = manager.create_pool_with_capabilities("identity".into(), definition(&layout,
        "local init=vulcan.capabilities.call('test.echo',{phase='init'}); assert(init.ok); return {call=function(a) return {visible=vulcan.capabilities.has('test.echo'),response=vulcan.host.call('test.echo',a)} end}"),
        pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    let forged = json!({"plugin_id":"forged","runtime_id":"forged","operation_id":"forged","text":"中文\u{0}"});
    for operation in ["operation-one", "operation-two"] {
        let result = lease
            .invoke(ModuleInvocation {
                operation_id: operation,
                session_id: None,
                export: "call",
                arguments: &forged,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap();
        assert_eq!(result["response"]["value"], forged);
        assert_eq!(result["visible"], json!(true));
    }
    // The native handler never observes the forged identity fields as caller authority.
    // 原生处理器绝不将伪造身份字段视作调用方权威。
    let calls = observations.lock().unwrap().clone();
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|caller| caller.plugin_id == layout.package_id
                && caller.runtime_id == "runtime-identity"
                && caller.execution_revision == "execution-v1")
    );
    assert!(
        calls
            .iter()
            .any(|caller| caller.operation_id.ends_with(":initialize"))
    );
    assert!(
        calls
            .iter()
            .any(|caller| caller.operation_id == "operation-one")
    );
    assert!(
        calls
            .iter()
            .any(|caller| caller.operation_id == "operation-two")
    );
    permissions.revoke("test.host").unwrap();
    let result = lease
        .invoke(ModuleInvocation {
            operation_id: "operation-denied",
            session_id: None,
            export: "call",
            arguments: &Value::Null,
            context: &LuaInvocationContext::default(),
            control: control(),
        })
        .unwrap();
    assert_eq!(result["visible"], json!(false));
    assert_eq!(
        result["response"]["error"]["code"],
        json!("permission_denied")
    );
    assert_eq!(observations.lock().unwrap().len(), 3);
    pool.close().unwrap();
    drop(lease);
    drained(&pool);
}

/// Real Lua execution blocks on an independent SDK queue while retaining its VM execution permit.
/// 真实 Lua 执行在独立 SDK 队列等待，同时保留 VM 执行许可。
#[test]
fn embedded_capability_lua_waits_for_queued_handler_without_blocking_control_channel() {
    // The main thread acts as the SDK pump while one existing worker owns the VM.
    // 主线程充当 SDK 事件泵，一个已有工作线程拥有 VM。
    let layout = SystemRuntimeTestLayout::new("embedded capabilities queued");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("runtime-queue".into(), manager.config().clone()).unwrap();
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.queue", CapabilityExecution::Queued),
            native: None,
        }])
        .unwrap();
    let (_, capabilities) = binding(&registry);
    let pool = manager
        .create_pool_with_capabilities(
            "queue".into(),
            definition(
                &layout,
                "return {call=function(a) return vulcan.capabilities.call('test.queue',a) end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            capabilities,
        )
        .unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    let worker = std::thread::spawn(move || {
        lease.invoke(ModuleInvocation {
            operation_id: "queued-operation",
            session_id: None,
            export: "call",
            arguments: &json!({"message":"hello"}),
            context: &LuaInvocationContext::default(),
            control: control(),
        })
    });
    let broker = registry.host_requests();
    let deadline = Instant::now() + Duration::from_secs(2);
    let request = loop {
        if let Some(request) = broker.take(1).unwrap().pop() {
            break request;
        }
        assert!(
            Instant::now() < deadline,
            "Lua did not dispatch its host request"
        );
        std::thread::yield_now();
    };
    assert_eq!(request.caller.operation_id, "queued-operation");
    assert_eq!(request.arguments, json!({"message":"hello"}));
    assert_eq!(manager.usage().unwrap().running, 1);
    assert!(!worker.is_finished());
    broker
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!({"reply":"world"})),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap(),
        json!({"ok":true,"value":{"reply":"world"},"effects":"not_applicable"})
    );
    pool.close().unwrap();
    drained(&pool);
    assert!(broker.is_drained().unwrap());
}

/// New unbound modules must not inherit a process-global host callback registered for legacy skills.
/// 新未绑定模块不得继承为旧技能注册的进程全局宿主回调。
#[test]
fn embedded_capability_unbound_lua_cannot_reach_legacy_callbacks() {
    // Legacy callback mutation uses the existing serial test guard.
    // 旧回调变更使用既有串行测试保护。
    let _guard = host_tool_callback_test_guard();
    set_host_tool_callback(Some(Arc::new(|_| {
        panic!("embedded module reached legacy callback")
    })));
    let layout = SystemRuntimeTestLayout::new("embedded capabilities no ambient authority");
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    let mut module = engine.create_embedded_module(definition(&layout,
        "return {call=function() return {response=vulcan.host.call('legacy',{}),exists=vulcan.host.has('legacy'),model=vulcan.models.llm~=nil,management=vulcan.runtime.skills.enabled} end}"), "unbound-instance", control()).unwrap();
    let result = module
        .invoke(ModuleInvocation {
            operation_id: "unbound-operation",
            session_id: None,
            export: "call",
            arguments: &Value::Null,
            context: &LuaInvocationContext::default(),
            control: control(),
        })
        .unwrap();
    assert_eq!(
        result["response"]["error"]["code"],
        json!("permission_denied")
    );
    assert_eq!(result["exists"], json!(false));
    assert_eq!(result["model"], json!(false));
    assert_eq!(result["management"], json!(false));
}

/// A resident VM cannot be redirected by replacing a capability under its original name.
/// 常驻 VM 不能被原名称下的能力替换重定向。
#[test]
fn embedded_capability_lua_snapshot_replacement_requires_new_pool() {
    // Two activated generations deliberately share a public capability name.
    // 两个激活代次刻意共享公开能力名称。
    let layout = SystemRuntimeTestLayout::new("embedded capability snapshot replacement");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("runtime-replacement".into(), manager.config().clone()).unwrap();
    let old_id = registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.version", CapabilityExecution::Native),
            native: Some(Arc::new(|_| CapabilityOutcome {
                result: Ok(json!("old")),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap()
        .remove(0);
    let (_, old_binding) = binding(&registry);
    let source = "return {call=function() return vulcan.capabilities.call('test.version',{}) end}";
    let old_pool = manager
        .create_pool_with_capabilities(
            "old".into(),
            definition(&layout, source),
            pool_policy(InstanceReuse::Reusable),
            old_binding,
        )
        .unwrap();
    let mut old_lease = old_pool.acquire(control()).unwrap();
    registry.unregister(&old_id).unwrap();
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.version", CapabilityExecution::Native),
            native: Some(Arc::new(|_| CapabilityOutcome {
                result: Ok(json!("new")),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap();
    let (_, new_binding) = binding(&registry);
    let new_pool = manager
        .create_pool_with_capabilities(
            "new".into(),
            definition(&layout, source),
            pool_policy(InstanceReuse::Reusable),
            new_binding,
        )
        .unwrap();
    let mut new_lease = new_pool.acquire(control()).unwrap();
    // Identical application calls retain the exact registration identity frozen by each pool.
    // 相同应用调用保留各池冻结的精确注册身份。
    let invoke = |lease: &mut crate::runtime::embedded::ModuleLease| {
        lease
            .invoke(ModuleInvocation {
                operation_id: "replacement-operation",
                session_id: None,
                export: "call",
                arguments: &Value::Null,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap()
    };
    assert_eq!(invoke(&mut old_lease)["error"]["code"], json!("closed"));
    assert_eq!(invoke(&mut new_lease)["value"], json!("new"));
    old_pool.close().unwrap();
    new_pool.close().unwrap();
    drop((old_lease, new_lease));
    drained(&old_pool);
    drained(&new_pool);
}

/// Trusted session authority is refreshed for each call and cannot be forged through arguments.
/// 每次调用刷新可信会话权威，且不能通过参数伪造。
#[test]
fn embedded_capability_lua_session_scope_requires_host_identity() {
    // The same VM first receives no session and then an explicit host-bound session.
    // 同一 VM 先接收无会话调用，再接收显式宿主绑定会话。
    let layout = SystemRuntimeTestLayout::new("embedded capability session authority");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("runtime-session".into(), manager.config().clone()).unwrap();
    let mut contract = descriptor("test.session", CapabilityExecution::Native);
    contract.scope = CapabilityScope::Session;
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: Some(Arc::new(|call| CapabilityOutcome {
                result: Ok(json!(call.caller.session_id)),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap();
    let (_, capabilities) = binding(&registry);
    let pool = manager
        .create_pool_with_capabilities(
            "session".into(),
            definition(
                &layout,
                "return {call=function(a) return vulcan.capabilities.call('test.session',a) end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            capabilities,
        )
        .unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    for session in [None, Some("trusted-session"), None] {
        let result = lease
            .invoke(ModuleInvocation {
                operation_id: "session-operation",
                session_id: session,
                export: "call",
                arguments: &json!({"session_id":"forged-session"}),
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap();
        if let Some(session) = session {
            assert_eq!(result["value"], json!(session));
        } else {
            assert_eq!(result["error"]["code"], json!("permission_denied"));
        }
    }
    pool.close().unwrap();
    drop(lease);
    drained(&pool);
}
