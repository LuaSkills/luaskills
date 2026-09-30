//! Real LuaJIT regression coverage for the embedded integer and float value contract.
//! 嵌入式整数与浮点值契约的真实 LuaJIT 回归覆盖。

use super::capabilities::{binding, descriptor};
use super::pools::{drained, pool_manager, pool_policy};
use super::*;
use crate::runtime::embedded::capabilities::*;
use crate::runtime::embedded::{
    EffectState, HostEffectPhase, InstanceReuse, OperationPhase, OperationRegistry,
};

/// Reject unsafe JSON integer `arguments` before business code, including recursive values.
/// 在业务代码前拒绝不安全 JSON 整数 `arguments`，包含递归值。
#[test]
fn embedded_integer_arguments_reject_unsafe_values_before_business_execution() {
    // The file is written only if the actual captured Lua export executes.
    // 仅当真实捕获 Lua 导出执行时才写入文件。
    let layout = SystemRuntimeTestLayout::new("embedded integer rejection");
    // All rejected calls share a valid initialized instance to isolate argument admission.
    // 所有拒绝调用共享有效已初始化实例，以隔离参数入场。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Safe bounds derive from the single production integer-domain constant.
    // 安全边界从唯一生产整数域常量派生。
    let maximum = LuaEngine::EMBEDDED_MAX_SAFE_INTEGER;
    // The export side effect distinguishes refusal from silent numerical rounding.
    // 导出副作用区分拒绝与静默数值舍入。
    let mut module = engine.create_embedded_module(definition(&layout,
        "return {call=function(a) vulcan.fs.write('unsafe-executed.txt','bad'); return a end}"),
        "integer-rejection", control()).unwrap();
    for arguments in [
        json!(maximum + 1),
        json!(-(maximum + 1)),
        json!(maximum + 2),
        json!(maximum + 3),
        json!(i64::MAX),
        json!(i64::MIN),
        json!(u64::MAX),
        json!({"nested":[null,{"secret/key":maximum + 2}]}),
        // Arbitrary object-key length must not expand the pre-admission error frame.
        // 任意对象键长度不得膨胀入场前错误帧。
        Value::Object(serde_json::Map::from_iter([(
            "/".repeat(800),
            json!(u64::MAX),
        )])),
    ] {
        // Inspect the structured error without accepting an approximate result.
        // 检查结构化错误，不接受近似结果。
        let error = module
            .invoke(ModuleInvocation {
                operation_id: "integer-input",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap_err();
        assert_eq!(error.code, EmbeddedErrorCode::InvalidArgument);
        assert!(!error.message.contains(&(maximum + 2).to_string()));
        assert!(!error.message.contains('/'));
        assert!(!layout.package_root.join("unsafe-executed.txt").exists());
        assert!(module.is_reusable());
    }
}

/// Safe endpoint integers and explicit IEEE-754 floats survive actual Lua execution.
/// 安全端点整数及显式 IEEE-754 浮点数通过真实 Lua 执行。
#[test]
fn embedded_integer_safe_endpoints_and_explicit_floats_preserve_values() {
    // One VM exercises both ingress validation and normalized export serialization.
    // 单个 VM 同时验证入口校验及规范化导出序列化。
    let layout = SystemRuntimeTestLayout::new("embedded integer float roundtrip");
    // Exact echo preserves the existing null, protected-container and embedded-NUL mapping.
    // 精确回显保留既有空值、受保护容器及嵌入零字节映射。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Reuse tests also ensure representation checks do not poison successful instances.
    // 复用测试同时确保表示检查不会污染成功实例。
    let mut module = engine
        .create_embedded_module(
            definition(&layout, "return {call=function(a) return a end}"),
            "integer-roundtrip",
            control(),
        )
        .unwrap();
    // The complete continuous domain endpoints must remain integer JSON values.
    // 完整连续域端点必须保持 JSON 整数值。
    let maximum = LuaEngine::EMBEDDED_MAX_SAFE_INTEGER;
    // Structured safe values catch container or null regressions from the checked converter.
    // 结构化安全值检测已校验转换器造成的容器或空值回归。
    let arguments =
        json!({"min":-maximum,"max":maximum,"null":null,"array":[],"object":{},"nul":"中\u{0}文"});
    assert_eq!(
        module
            .invoke(ModuleInvocation {
                operation_id: "integer-safe",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &LuaInvocationContext::default(),
                control: control()
            })
            .unwrap(),
        arguments
    );
    for float in [
        (maximum + 1) as f64,
        (maximum + 3) as f64,
        i64::MAX as f64,
        u64::MAX as f64,
        -((maximum + 1) as f64),
        i64::MIN as f64,
    ] {
        // Explicit float tagging is observable in serde_json even when mlua later infers an integer.
        // 显式浮点标签在 serde_json 可观察，即使 mlua 随后推断为整数。
        let arguments = json!({"nested":[float]});
        // Outside-domain Lua numbers must be encoded as floats, not lossless-looking JSON integers.
        // 域外 Lua 数字必须编码为浮点数，不能伪装为无损 JSON 整数。
        let result = module
            .invoke(ModuleInvocation {
                operation_id: "float-safe",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap();
        assert!(result["nested"][0].is_f64());
        assert_eq!(result["nested"][0].as_f64(), Some(float));
    }
}

/// Host callbacks receive normalized floats and reject unsafe host integer results without hiding commits.
/// 宿主回调接收规范化浮点值，并拒绝不安全宿主整数结果且不隐藏提交。
#[test]
fn embedded_integer_host_result_rejection_preserves_effect_ledger() {
    // The operation ledger is shared with the registry, so it records the actual native commit.
    // 操作账本与注册表共享，因此记录真实原生提交。
    let layout = SystemRuntimeTestLayout::new("embedded integer host committed");
    // Real pool resources preserve registry-to-Lua lifecycle ordering.
    // 真实池资源保留注册表到 Lua 的生命周期顺序。
    let manager = pool_manager(&layout);
    // Namespaces match so evidence cannot be detached from the admitted operation.
    // 命名空间一致，确保证据不能脱离已接纳操作。
    let registry =
        CapabilityRegistry::new("integer-effects".into(), manager.config().clone()).unwrap();
    // Retain native observations to prove the handler executed and received an explicit float.
    // 保留原生观测，证明处理器已执行并收到显式浮点数。
    let observed = Arc::new(Mutex::new(Vec::new()));
    // Callback owns its observation storage for the duration of the immutable registration.
    // 回调在不可变注册期间持有自身观测存储。
    let captured = Arc::clone(&observed);
    // Mutation classification forces real effect evidence rather than read-only default state.
    // 变更分类强制记录真实副作用证据，而非只读默认状态。
    let mut contract = descriptor("test.integer", CapabilityExecution::Native);
    contract.effects = CapabilityEffects::Mutating;
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: Some(Arc::new(move |invocation| {
                captured.lock().unwrap().push(invocation.arguments.clone());
                CapabilityOutcome {
                    result: Ok(json!({"nested":[u64::MAX]})),
                    effects: EffectState::Committed,
                }
            })),
        }])
        .unwrap();
    // A bound module consumes the error envelope rather than an unsafe rounded host value.
    // 已绑定模块消费错误信封，而非不安全舍入宿主值。
    let (_, capabilities) = binding(&registry);
    // Actual Lua literal bypasses JSON integer ingress while exercising Lua-to-host normalization.
    // 真实 Lua 字面量绕过 JSON 整数入口，同时验证 Lua 到宿主规范化。
    let pool = manager.create_pool_with_capabilities("integer-effects".into(), definition(&layout,
        "return {call=function() return vulcan.host.call('test.integer',{number=2^53}) end}"),
        pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    // The owner supplies original control so native ledger recording uses this exact operation.
    // 所有者提供原始控制，原生账本记录使用此精确操作。
    let operations = OperationRegistry::new("integer-effects".into(), manager.config()).unwrap();
    // Admission creates the identity used by the native effect ledger.
    // 入场创建原生副作用账本使用的身份。
    let (handle, mut owner) = operations.admit(control()).unwrap();
    // Stable identity captured before actual VM initialization.
    // 在真实 VM 初始化前捕获稳定身份。
    let id = handle.snapshot().unwrap().operation_id;
    owner.advance(OperationPhase::Initializing).unwrap();
    // This lease owns the real initialized LuaJIT module.
    // 此租借持有真实已初始化 LuaJIT 模块。
    let mut lease = pool.acquire(owner.control()).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    // The host result is rejected inside its envelope after the commit has already been recorded.
    // 宿主结果在提交已记录后于其信封内部被拒绝。
    let result = lease
        .invoke(ModuleInvocation {
            operation_id: &id,
            session_id: None,
            export: "call",
            arguments: &Value::Null,
            context: &LuaInvocationContext::default(),
            control: owner.control(),
        })
        .unwrap();
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert!(observed.lock().unwrap().first().unwrap()["number"].is_f64());
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"]["code"], json!("invalid_argument"));
    assert_eq!(result["effects"], json!("committed"));
    assert!(result.get("value").is_none());
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(result), EffectState::NotApplicable)
        .unwrap();
    // The completed top-level operation retains host-confirmed mutation evidence.
    // 已完成顶层操作保留宿主确认的变更证据。
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.effects, EffectState::Committed);
    assert!(
        snapshot
            .host_effects
            .iter()
            .any(|effect| effect.effects == EffectState::Committed
                && effect.phase == HostEffectPhase::Completed)
    );
    pool.close().unwrap();
    drop(lease);
    drained(&pool);
}

/// Mount metadata and request context reject unsafe integers before source or export execution.
/// 挂载元数据及请求上下文在源码或导出执行前拒绝不安全整数。
#[test]
fn embedded_integer_mounts_and_context_reject_unsafe_values() {
    // Both possible execution points write files only if rejection happens too late.
    // 两个可能执行点仅在拒绝过晚时写入文件。
    let layout = SystemRuntimeTestLayout::new("embedded integer context rejection");
    // Reuse the exact host package fixture for both declaration and invocation rejection.
    // 对声明及调用拒绝复用精确宿主包夹具。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Mount values reach Lua through readonly userdata and still need early integer checks.
    // 挂载值通过只读 userdata 到达 Lua，仍需提前检查整数。
    let mut declaration = definition(
        &layout,
        "vulcan.fs.write('source-executed.txt','bad'); return {call=function() return true end}",
    );
    declaration.mounts = json!({"nested":[u64::MAX]});
    assert_eq!(
        engine
            .create_embedded_module(declaration, "integer-mounts", control())
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert!(!layout.package_root.join("source-executed.txt").exists());
    // An initialized module proves request context validation precedes business code.
    // 已初始化模块证明请求上下文校验先于业务代码。
    let mut module = engine.create_embedded_module(definition(&layout,
        "return {call=function() vulcan.fs.write('context-executed.txt','bad'); return true end}"), "integer-context", control()).unwrap();
    // The authoritative context field is passed directly to the production request projector.
    // 权威上下文字段直接传递给生产请求投影器。
    let context = LuaInvocationContext::new(
        None,
        json!({}),
        Value::Object(serde_json::Map::from_iter([(
            "/".repeat(800),
            json!({"nested":[u64::MAX]}),
        )])),
    );
    // Actual context rejection retains only the fixed host root, regardless of application key width.
    // 真实上下文拒绝只保留固定宿主根，不受应用键宽度影响。
    let error = module
        .invoke(ModuleInvocation {
            operation_id: "integer-context",
            session_id: None,
            export: "call",
            arguments: &Value::Null,
            context: &context,
            control: control(),
        })
        .unwrap_err();
    assert_eq!(error.code, EmbeddedErrorCode::InvalidArgument);
    assert!(error.message.contains("context/tool_config"));
    assert!(!error.message.contains(&"/".repeat(800)));
    assert!(!layout.package_root.join("context-executed.txt").exists());
}

/// Closing application arguments are checked at registration while Schema numbers remain declarations.
/// 关闭应用参数在注册时校验，同时 Schema 数字保持声明语义。
#[test]
fn embedded_integer_finalizer_validation_does_not_reject_schema_numbers() {
    // Valid module paths isolate the application-value rule from package authorization.
    // 有效模块路径将应用值规则与包授权隔离。
    let layout = SystemRuntimeTestLayout::new("embedded integer finalizer declaration");
    // The real engine compiles the Schema before capturing a callable export.
    // 真实引擎在捕获可调用导出前编译 Schema。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Schema maximum is intentionally outside Lua's application integer domain.
    // Schema 最大值刻意超出 Lua 应用整数域。
    let mut declaration = definition(&layout, "return {call=function(a) return a end}");
    declaration
        .exports
        .iter_mut()
        .find(|export| export.name == "call")
        .unwrap()
        .input_schema = json!({"type":"integer","maximum":u64::MAX});
    // Safe actual arguments remain valid even when their Schema contains a larger bound.
    // 即使 Schema 包含更大边界，安全实际参数仍然有效。
    let mut module = engine
        .create_embedded_module(declaration.clone(), "integer-schema", control())
        .unwrap();
    // This application value is the positive continuous-domain endpoint.
    // 此应用值为连续域正端点。
    let arguments = json!(LuaEngine::EMBEDDED_MAX_SAFE_INTEGER);
    assert_eq!(
        module
            .invoke(ModuleInvocation {
                operation_id: "integer-schema",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &LuaInvocationContext::default(),
                control: control()
            })
            .unwrap(),
        arguments
    );
    declaration.finalizer = Some(crate::runtime::embedded::ModuleFinalizer {
        export: "call".into(),
        arguments: json!(LuaEngine::EMBEDDED_MAX_SAFE_INTEGER + 1),
        timeout_ms: 1000,
    });
    assert_eq!(
        engine
            .create_embedded_module(declaration, "integer-finalizer", control())
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
}
