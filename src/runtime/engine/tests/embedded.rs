//! Behavioral coverage for typed plugin loading and direct export invocation.
//! 类型化插件加载与导出直接调用的行为覆盖。

use super::*;
use crate::LuaInvocationContext;
use crate::runtime::embedded::{
    CallControl, EmbeddedErrorCode, ModuleDefinition, ModuleExport, ModuleInvocation,
};

mod capabilities;
mod ffi;
mod pools;
mod scheduler;

/// Build a declaration using the established System fixture and exact `source`.
/// 使用既有 System 测试夹具与精确 `source` 构造声明。
/// The returned module exposes only the `call` export.
/// 返回的模块只暴露 `call` 导出。
fn definition(layout: &SystemRuntimeTestLayout, source: &str) -> ModuleDefinition {
    ModuleDefinition {
        plugin_id: layout.package_id.clone(),
        generation: "generation-one".to_owned(),
        package_root: render_host_visible_path(&layout.package_root),
        dependencies_file: "dependencies.yaml".to_owned(),
        workspace_root: None,
        cwd: None,
        mounts: json!({}),
        security_partition: "workspace-a".to_owned(),
        source: source.to_owned(),
        exports: vec![ModuleExport {
            name: "call".to_owned(),
            input_schema: json!(true),
            output_schema: json!(true),
        }],
    }
}

/// Return a generous finite control so functional tests do not depend on timing noise.
/// 返回较宽裕的有限控制，避免功能测试依赖计时噪声。
fn control() -> Arc<CallControl> {
    Arc::new(CallControl::new(Duration::from_secs(10)).unwrap())
}

/// Module state persists while the captured export resists table redirection.
/// 模块状态持续存在，同时捕获的导出不会被表重定向。
#[test]
fn embedded_module_invokes_captured_functions_without_reloading_source() {
    // Existing canonical fixture includes the package dependency boundary.
    // 既有规范夹具包含包依赖边界。
    let layout = SystemRuntimeTestLayout::new("embedded exports 中文");
    // The instance retains the last engine reference during Lua finalization.
    // 实例在 Lua 终结期间保留最后一个引擎引用。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // A mutable public Lua table must not change the host's validated function identity.
    // 可变公开 Lua 表不得改变宿主已校验的函数身份。
    let source = "local n=0; local m={}; m.call=function(a) n=n+1; m.call=function() return 'replaced' end; return {count=n,arg=a} end; return m";
    // Directly loaded instance under one fixed activation identity.
    // 在固定激活身份下直接加载的实例。
    let mut module = engine
        .create_embedded_module(definition(&layout, source), "direct-instance", control())
        .unwrap();
    drop(engine);
    // Explicit arguments exercise serde's null, empty container, Unicode, and embedded-NUL mapping.
    // 显式参数验证 serde 的空值、空容器、Unicode 与嵌入零字节映射。
    let arguments = json!({"null":null,"object":{},"array":[],"text":"中文\u{0}value"});
    // Trusted request context is installed anew on every invocation.
    // 每次调用重新安装的可信请求上下文。
    let context = LuaInvocationContext::default();
    for expected in [1, 2] {
        // Direct call result must preserve both persistent state and structured input.
        // 直接调用结果必须同时保留持久状态与结构化输入。
        let result = module
            .invoke(ModuleInvocation {
                operation_id: "test-operation",
                session_id: None,
                export: "call",
                arguments: &arguments,
                context: &context,
                control: control(),
            })
            .unwrap();
        assert_eq!(result, json!({"count":expected,"arg":arguments}));
    }
    module.close().unwrap();
    assert!(!module.is_reusable());
}

/// Independent module capacity must not borrow the legacy manager's eight slots.
/// 独立模块容量不得借用旧管理器的八个槽位。
#[test]
fn embedded_modules_are_independent_of_legacy_lease_limit() {
    // More modules than the authoritative old limit prove manager independence.
    // 创建超过权威旧上限的模块以证明管理器独立。
    let layout = SystemRuntimeTestLayout::new("embedded capacity");
    // All instances deliberately share one immutable engine and package.
    // 所有实例刻意共享同一个不可变引擎与包。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Keep all modules alive together so sequential teardown cannot mask the limit.
    // 同时保留全部模块存活，防止依次销毁掩盖上限问题。
    let modules = (0..MAX_RUNTIME_SESSION_LEASES_PER_MANAGER + 1)
        .map(|index| {
            engine
                .create_embedded_module(
                    definition(&layout, "return {call=function(a) return a end}"),
                    &format!("instance-{index}"),
                    control(),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(modules.len(), MAX_RUNTIME_SESSION_LEASES_PER_MANAGER + 1);
}

/// Failed invocation disables reuse, and pre-cancellation prevents Lua side effects.
/// 失败调用禁止复用，预先取消阻止 Lua 副作用。
#[test]
fn embedded_cancellation_prevents_execution_and_reuse() {
    // The would-be side effect is inspected through the filesystem afterward.
    // 随后通过文件系统检查本应发生的副作用。
    let layout = SystemRuntimeTestLayout::new("embedded cancel");
    // Reuse the established logical-directory and file-service implementation.
    // 复用既有逻辑目录及文件服务实现。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // A write makes accidental post-cancellation execution observable.
    // 写入使取消后的意外执行可观察。
    let mut module = engine
        .create_embedded_module(
            definition(
                &layout,
                "return {call=function() vulcan.fs.write('unexpected.txt','bad'); return true end}",
            ),
            "cancel-instance",
            control(),
        )
        .unwrap();
    // Cancellation requested before admission to the actual Lua call.
    // 在进入实际 Lua 调用前请求取消。
    let cancelled = control();
    assert!(cancelled.cancel());
    assert!(!cancelled.cancel());
    // Exact invocation result, with no string parsing of the failure.
    // 精确调用结果，不对错误进行字符串解析。
    let result = module.invoke(ModuleInvocation {
        operation_id: "test-operation",
        session_id: None,
        export: "call",
        arguments: &json!({}),
        context: &LuaInvocationContext::default(),
        control: cancelled,
    });
    assert_eq!(result.unwrap_err().code, EmbeddedErrorCode::Cancelled);
    assert!(!layout.package_root.join("unexpected.txt").exists());
    assert!(!module.is_reusable());
}

/// Every call revalidates package identity and rejects a same-name replacement.
/// 每次调用重新校验包身份，并拒绝同名替换。
#[test]
fn embedded_module_revalidates_package_identity() {
    // The fixture package is moved within its already known temporary parent.
    // 夹具包在其已知临时父目录内移动。
    let layout = SystemRuntimeTestLayout::new("embedded identity");
    // Loaded module holds the original directory identity.
    // 已加载模块持有原目录身份。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // A trivial export isolates validation from plugin behavior.
    // 简单导出将校验与插件行为隔离。
    let mut module = engine
        .create_embedded_module(
            definition(&layout, "return {call=function() return true end}"),
            "identity-instance",
            control(),
        )
        .unwrap();
    fs::rename(
        &layout.package_root,
        layout.package_root.with_extension("old"),
    )
    .unwrap();
    fs::create_dir(&layout.package_root).unwrap();
    fs::write(layout.package_root.join("dependencies.yaml"), "{}\n").unwrap();
    assert!(
        module
            .invoke(ModuleInvocation {
                operation_id: "test-operation",
                session_id: None,
                export: "call",
                arguments: &json!({}),
                context: &LuaInvocationContext::default(),
                control: control()
            })
            .is_err()
    );
    assert!(!module.is_reusable());
}

/// Loading fails immediately if a declared function is absent or not callable.
/// 声明函数缺失或不可调用时，加载立即失败。
#[test]
fn embedded_module_rejects_invalid_exports_before_activation() {
    // Valid package paths ensure failure is caused by the export contract.
    // 有效包路径确保失败由导出契约引起。
    let layout = SystemRuntimeTestLayout::new("embedded bad export");
    // One engine verifies both incompatible module shapes.
    // 一个引擎验证两种不兼容模块形状。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    for source in ["return {call=12}", "return {}"] {
        assert!(
            engine
                .create_embedded_module(definition(&layout, source), "bad-instance", control())
                .is_err()
        );
    }
}

/// Input contracts reject bad requests before side effects; bad outputs disable reuse.
/// 输入契约在副作用前拒绝错误请求；错误输出禁止复用。
#[test]
fn embedded_module_enforces_input_and_output_contracts() {
    // File creation is an observable side effect of actual function execution.
    // 文件创建是实际函数执行的可观察副作用。
    let layout = SystemRuntimeTestLayout::new("embedded schemas");
    // Keep validation and VM behavior inside the real native engine.
    // 将校验与 VM 行为保留在真实原生引擎内。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // The declared result is intentionally incompatible with the implementation.
    // 声明的结果刻意与实现不兼容。
    let mut declaration = definition(
        &layout,
        "return {call=function(a) vulcan.fs.write('effect.txt','done'); return 'wrong-result' end}",
    );
    declaration.exports = vec![ModuleExport {
        name: "call".to_owned(),
        input_schema: json!({"type":"integer","minimum":1}),
        output_schema: json!({"type":"integer"}),
    }];
    // Loading succeeds because the exact function exists and both schemas are valid.
    // 精确函数存在且两种 Schema 均有效，因此加载成功。
    let mut module = engine
        .create_embedded_module(declaration, "schema-instance", control())
        .unwrap();
    assert!(
        module
            .invoke(ModuleInvocation {
                operation_id: "test-operation",
                session_id: None,
                export: "call",
                arguments: &json!("rejected-secret"),
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .is_err()
    );
    assert!(!layout.package_root.join("effect.txt").exists());
    assert!(module.is_reusable());
    assert!(
        module
            .invoke(ModuleInvocation {
                operation_id: "test-operation",
                session_id: None,
                export: "call",
                arguments: &json!(1),
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .is_err()
    );
    assert!(layout.package_root.join("effect.txt").exists());
    assert!(!module.is_reusable());
}
