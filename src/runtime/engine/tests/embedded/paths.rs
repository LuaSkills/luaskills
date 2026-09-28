//! Exercise exact host package roots through real embedded execution and legacy rejection.
//! 通过真实嵌入式执行及旧入口拒绝验证宿主精确包根。

use super::*;
use crate::runtime::managed_package::ManagedRuntimeLeaseBinding;

/// System and embedded declarations of identical bytes must retain distinct worker identities.
/// 相同字节的 System 与嵌入式声明必须保持不同 Worker 身份。
#[test]
fn embedded_module_package_kind_separates_legacy_worker_namespace() {
    // This package is intentionally inside System so both constructors accept the same path.
    // 此包刻意位于 System 内，让两个构造器都接受同一路径。
    let layout = SystemRuntimeTestLayout::new("embedded package namespace");
    // Obtain the actual engine's resolved interpreter and environment roots.
    // 获取真实引擎已解析的解释器及环境根。
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    // Both contexts share runtime roots; only package authority kind differs.
    // 两个上下文共享运行时根，仅包权威类型不同。
    let roots = engine
        .managed_runtime_roots_for(&layout.runtime_root)
        .expect("resolve runtime roots");
    // Bind identical context metadata to isolate the package-kind distinction.
    // 绑定相同上下文元数据，独立验证包类型差异。
    let binding = Arc::new(ManagedRuntimeLeaseBinding::new(
        "same-instance".into(),
        None,
        json!({}),
    ));
    binding
        .bind("same-instance".into(), 1)
        .expect("bind context");
    // The legacy namespace remains unchanged.
    // 旧命名空间保持不变。
    let legacy = ManagedRuntimePackageContext::for_system_plugin_with_roots(
        &layout.package_id,
        &layout.package_root,
        Arc::clone(&roots),
        &layout.system_lua_lib_dir,
        "dependencies.yaml",
        Arc::clone(&binding),
    )
    .expect("legacy package");
    // The formal module uses the same exact package under a different ownership kind.
    // 正式模块在不同所有权类型下使用同一精确包。
    let embedded = ManagedRuntimePackageContext::for_embedded_plugin_with_roots(
        &layout.package_id,
        &layout.package_root,
        roots,
        "dependencies.yaml",
        binding,
    )
    .expect("embedded package");
    assert_eq!(legacy.identity().kind().as_str(), "system_plugin");
    assert_eq!(
        embedded.worker_context_json()["package"]["kind"],
        "embedded_plugin"
    );
    assert_ne!(
        legacy.identity().stable_hash(),
        embedded.identity().stable_hash()
    );
}

/// Move the fixture package into an immutable-generation layout outside its System root.
/// 将夹具包移入 System 根之外的不可变代次布局。
/// Return the same cleanup owner with canonical package and manifest paths updated.
/// 返回同一个清理所有者，并更新规范包及清单路径。
pub(super) fn external_layout(label: &str) -> SystemRuntimeTestLayout {
    // The established fixture owns every directory touched by this helper.
    // 既有夹具拥有本辅助方法触及的全部目录。
    let mut layout = SystemRuntimeTestLayout::new(label);
    // Preserve the plugin ID as the leaf while introducing a separate immutable generation.
    // 保留插件 ID 为叶目录，同时引入独立不可变代次。
    let generation = layout
        .runtime_root
        .join("user-plugins/.vulcan-generations/packages/physical-one");
    fs::create_dir_all(&generation).expect("create external generation");
    // This destination remains inside the known temporary fixture, outside its System subtree.
    // 此目标仍位于已知临时夹具内，并处于其 System 子树外。
    let package = generation.join(&layout.package_id);
    assert!(package.starts_with(&layout.runtime_root));
    assert!(!package.starts_with(&layout.system_lua_lib_dir));
    fs::rename(&layout.package_root, &package).expect("move exact package outside System root");
    layout.package_root = fs::canonicalize(package).expect("canonicalize external package");
    layout.dependencies_file = layout.package_root.join("dependencies.yaml");
    layout
}

/// Formal modules load external package-local files even when the legacy System directory is absent.
/// 即使旧 System 目录不存在，正式模块仍加载外部包内文件。
#[test]
fn embedded_module_authorizes_exact_external_package_without_system_root() {
    // Use a physical generation layout and remove only its now-empty legacy directory.
    // 使用物理代次布局，只移除其现已为空的旧目录。
    let layout = external_layout("embedded external package");
    fs::remove_dir(&layout.system_lua_lib_dir).expect("remove empty System directory");
    fs::write(
        layout.package_root.join("local_entry.lua"),
        "return 'package-local'",
    )
    .expect("write package-local module");
    // The actual engine configuration still names the absent legacy directory.
    // 真实引擎配置仍指向不存在的旧目录。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Require resolves the declared physical package while state survives direct calls.
    // require 解析已声明物理包，同时状态跨直接调用保留。
    let mut module = engine.create_embedded_module(
        definition(&layout, "local n=0; return {call=function() n=n+1; return {value=require('local_entry'),root=vulcan.runtime.system_plugin.root,count=n} end}"),
        "external-module", control(),
    ).expect("authorize exact external package");
    for count in [1, 2] {
        // The returned root is the actual authorized package, never a relocated System copy.
        // 返回根为实际授权包，绝非重新放置的 System 副本。
        let value = module
            .invoke(ModuleInvocation {
                operation_id: "external-call",
                session_id: None,
                export: "call",
                arguments: &Value::Null,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .expect("invoke external package");
        assert_eq!(
            value,
            json!({"value":"package-local", "root":render_host_visible_path(&layout.package_root), "count":count})
        );
    }
    module.close().expect("retire external module");
    assert!(
        !layout.system_lua_lib_dir.exists(),
        "formal module must not recreate System roots"
    );
}

/// Legacy System leases still reject the exact external package accepted by formal modules.
/// 旧 System 租约仍拒绝正式模块接受的同一精确外部包。
#[test]
fn embedded_module_external_authorization_preserves_legacy_system_boundary() {
    // Keep the old trust root present so rejection proves containment, not a missing directory.
    // 保持旧信任根存在，确保拒绝证明包含关系，而非目录缺失。
    let layout = external_layout("embedded legacy boundary");
    // Both public APIs run against the very same configured engine.
    // 两个公开 API 使用同一个已配置引擎。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Direct legacy create must fail before any VM is published.
    // 直接旧创建必须在发布任何 VM 前失败。
    let error = engine
        .create_system_runtime_lease_json(&layout.create_request("legacy-external").to_string())
        .expect_err("legacy System containment must remain enforced");
    assert!(error.contains("strict descendant"), "{error}");
    // Explicit native authorization is available solely through the formal module declaration.
    // 显式原生授权仅通过正式模块声明提供。
    let mut module = engine
        .create_embedded_module(
            definition(&layout, "return {call=function() return true end}"),
            "formal-external",
            control(),
        )
        .expect("formal external package");
    module.close().expect("close formal module");
}

/// Exact authorization preserves dependency containment and cwd boundaries before source execution.
/// 精确授权在源码执行前保留依赖包含关系及 cwd 边界。
#[test]
fn embedded_module_external_paths_reject_escapes_before_initialization() {
    // The marker makes accidental initialization observable for every invalid declaration.
    // 标记使每个无效声明的意外初始化都可观察。
    let layout = external_layout("embedded external rejection");
    fs::write(
        layout
            .package_root
            .parent()
            .expect("generation parent")
            .join("outside.yaml"),
        "{}\n",
    )
    .expect("write outside manifest");
    // Every attempt uses the same engine and exact generation.
    // 每次尝试使用同一引擎及精确代次。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    for case in [
        "relative-root",
        "parent-manifest",
        "absolute-manifest",
        "missing-manifest",
        "outside-cwd",
        "invalid-mounts",
    ] {
        // Clone the authoritative shape and change one forbidden declaration per case.
        // 克隆权威形状，并在每种情况仅更改一个禁止的声明。
        let mut declared = definition(
            &layout,
            "vulcan.fs.write('executed.txt','bad'); return {call=function() return true end}",
        );
        match case {
            "relative-root" => declared.package_root = "relative-package".into(),
            "parent-manifest" => declared.dependencies_file = "../outside.yaml".into(),
            "absolute-manifest" => {
                declared.dependencies_file = render_host_visible_path(&layout.dependencies_file)
            }
            "missing-manifest" => declared.dependencies_file = "missing.yaml".into(),
            "outside-cwd" => declared.cwd = Some(render_host_visible_path(&layout.runtime_root)),
            "invalid-mounts" => declared.mounts = json!([]),
            _ => unreachable!("all fixture cases enumerated"),
        }
        assert!(
            engine
                .create_embedded_module(declared, case, control())
                .is_err(),
            "{case}"
        );
        assert!(
            !layout.package_root.join("executed.txt").exists(),
            "{case} executed source"
        );
    }
}

/// External package, manifest, workspace and cwd objects cannot be replaced at the same path.
/// 外部包、清单、工作区及 cwd 对象不能在同路径下替换。
#[test]
fn embedded_module_external_paths_revalidate_native_object_identities() {
    for case in ["package", "manifest", "workspace", "cwd"] {
        // A fresh actual module isolates each replaced native object.
        // 每个新真实模块独立验证一个被替换原生对象。
        let layout = external_layout(&format!("embedded replacement {case}"));
        // Authorize a workspace and a child cwd, retaining separate object identities.
        // 授权工作区及子 cwd，保留各自对象身份。
        let workspace = layout.runtime_root.join("workspace");
        // Replacing this child tests cwd independently of its workspace parent.
        // 替换此子目录，独立于工作区父目录验证 cwd。
        let cwd = workspace.join("work");
        fs::create_dir_all(&cwd).expect("create authorized workspace");
        // One immutable engine remains alive through replacement and actual invocation.
        // 不可变引擎在替换及实际调用期间持续存活。
        let engine = Arc::new(make_runtime_test_engine_with_host_options(
            layout.host_options(),
        ));
        // The export would write a marker only if object validation incorrectly succeeds.
        // 只有对象校验错误地通过，导出才会写入标记。
        let mut declared = definition(
            &layout,
            "return {call=function() vulcan.fs.write('executed.txt','bad'); return true end}",
        );
        declared.workspace_root = Some(render_host_visible_path(&workspace));
        declared.cwd = Some(render_host_visible_path(&cwd));
        // Load before replacing the exact filesystem object.
        // 在替换精确文件系统对象前加载。
        let mut module = engine
            .create_embedded_module(declared, case, control())
            .expect("load original objects");
        // Every path is inside the fixture, and the old object remains alive under a new name.
        // 每条路径都位于夹具内，旧对象在新名称下保持存在。
        let replaced = match case {
            "package" => &layout.package_root,
            "manifest" => &layout.dependencies_file,
            "workspace" => &workspace,
            "cwd" => &cwd,
            _ => unreachable!("all replacement cases enumerated"),
        };
        fs::rename(replaced, replaced.with_extension("original")).expect("retain original object");
        if case == "manifest" {
            fs::write(replaced, "{}\n").expect("replace manifest with identical bytes");
        } else {
            fs::create_dir_all(replaced).expect("replace directory");
            if case == "workspace" {
                fs::create_dir_all(&cwd).expect("recreate child cwd");
            }
            if case == "package" {
                fs::write(&layout.dependencies_file, "{}\n").expect("recreate manifest");
            }
        }
        assert!(
            module
                .invoke(ModuleInvocation {
                    operation_id: "after-replacement",
                    session_id: None,
                    export: "call",
                    arguments: &Value::Null,
                    context: &LuaInvocationContext::default(),
                    control: control(),
                })
                .is_err(),
            "{case} replacement must reject"
        );
        assert!(!module.is_reusable());
        assert!(!cwd.join("executed.txt").exists(), "{case} executed source");
        module.close().expect("retire rejected module");
    }
}
