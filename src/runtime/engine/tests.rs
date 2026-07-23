#[cfg(windows)]
use super::configured_package_search_directory_exists;
#[cfg(windows)]
use super::format_vulcan_fs_list_non_utf8_file_name_error;
use super::host_result::{
    host_result_capability_to_json_value, normalize_change_set_payload,
    resolve_host_result_capability, validate_change_set_payload,
};
use super::lease::RuntimeSessionManager;
use super::runlua::{ExecShellLauncher, lock_runlua_print_capture, runlua_cwd_guard};
#[cfg(windows)]
use super::vulcan_process_candidate_paths;
#[cfg(windows)]
use super::windows_wide_null_path;
use super::{
    LoadedSkill, LuaEngine, LuaVm, LuaVmPool, LuaVmPoolConfig, LuaVmPoolState,
    LuaVmRequestScopeGuard, ManagedRuntimeServices, ManagedRuntimeWorkerKey,
    ManagedRuntimeWorkerService, NativeLibrarySearchGuard, ResolvedEntryTarget,
    RunLuaVmBuildContext, SkillApplyLifecycleAction, SkillPackageConfigService,
    VulcanInternalExecutionContext, build_lua_call_dispatch_entries,
    copy_managed_node_package_import_root, default_runlua_vm_pool_config,
    find_vulcan_process_candidate, format_lifecycle_recovery_error, get_vulcan_context_table,
    get_vulcan_deps_table, get_vulcan_runtime_internal_table, get_vulcan_table,
    invoke_managed_runtime_worker, json_to_lua_table, lua_value_to_json,
    managed_runtime_status_from_plan, managed_runtime_worker_result_to_json,
    parse_runtime_request_context_json, populate_vulcan_dependency_context,
    populate_vulcan_file_context, populate_vulcan_internal_execution_context,
    prepare_managed_node_import_root, read_lua_help_payload_source, read_skill_text_file,
    render_host_visible_path, render_lua_help_payload_text, render_lua_print_argument,
    resolve_vulcan_fs_copy_effective_destination_path, spawn_managed_runtime_worker,
    system_time_to_unix_millis_i64, vulcan_fs_target_exists, vulcan_fs_target_is_dir,
};
use crate::host::callbacks::runtime_model_callback_test_guard;
use crate::host::database::RuntimeDatabaseProviderCallbacks;
use crate::lua_skill::SkillMeta;
use crate::runtime::encoding::{
    RuntimeTextEncoding, default_runtime_text_encoding, encode_runtime_text,
};
use crate::runtime::managed_package::{
    ManagedRuntimePackageContext, replace_lua_managed_package_context,
};
#[cfg(windows)]
use crate::runtime::managed_runtime::managed_env_dir;
use crate::runtime::managed_runtime::{
    MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION, ManagedRuntimeEnvMarker, ManagedRuntimeEnvPlan,
    ManagedRuntimeKind, ManagedRuntimeRootSource, ManagedRuntimeRoots,
    current_managed_runtime_platform_key, managed_env_marker_path, resolve_managed_runtime_install,
    resolve_node_env_plan, resolve_python_env_plan, sha256_file,
    validate_managed_node_runtime_version,
};
use crate::runtime::managed_session_events::{
    ManagedSessionEventCenter, RuntimeManagedSessionEvent, RuntimeManagedSessionEventKind,
};
use crate::runtime_options::LuaRuntimeRunLuaPoolConfig;
use crate::skill::dependencies::PackageDependencyManifest;
use crate::{
    LuaEngineOptions, LuaRuntimeCapabilityOptions, LuaRuntimeHostOptions,
    LuaRuntimeManagedRuntimeConfig, RuntimeClientInfo, RuntimeHostToolAction,
    RuntimeModelEmbedRequest, RuntimeModelEmbedResponse, RuntimeModelError, RuntimeModelErrorCode,
    RuntimeModelLlmRequest, RuntimeModelLlmResponse, RuntimeModelUsage, RuntimeRequestContext,
    RuntimeSkillRoot, SkillInstallRequest, SkillInstallSourceType, SkillManagementAuthority,
    SkillUninstallOptions, set_host_tool_callback, set_model_embed_callback,
    set_model_llm_callback,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use mlua::{Lua, Table, Value as LuaValue};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink as create_unix_symlink};
#[cfg(windows)]
use std::os::windows::fs::{
    symlink_dir as create_windows_dir_symlink, symlink_file as create_windows_file_symlink,
};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Barrier, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};

use crate::runtime::test_support::{TestEnvRestoreGuard, process_env_test_guard};

/// Verify the internal apply action subset admits only install and update lifecycle actions.
/// 验证内部 apply 动作子集只接受安装与更新生命周期动作。
#[test]
fn skill_apply_lifecycle_action_only_accepts_install_and_update() {
    // Accepted install action narrowed from the public lifecycle enum.
    // 从公开生命周期枚举收窄得到的可接受安装动作。
    let install_action = SkillApplyLifecycleAction::from_lifecycle_action(
        crate::skill::manager::SkillLifecycleAction::Install,
    );
    // Accepted update action narrowed from the public lifecycle enum.
    // 从公开生命周期枚举收窄得到的可接受更新动作。
    let update_action = SkillApplyLifecycleAction::from_lifecycle_action(
        crate::skill::manager::SkillLifecycleAction::Update,
    );
    assert!(matches!(
        install_action,
        Ok(SkillApplyLifecycleAction::Install)
    ));
    assert!(matches!(
        update_action,
        Ok(SkillApplyLifecycleAction::Update)
    ));

    // Unsupported lifecycle actions that must stay out of the install/update apply pipeline.
    // 必须排除在安装/更新 apply 流程之外的不支持生命周期动作。
    let unsupported_actions = [
        crate::skill::manager::SkillLifecycleAction::Reload,
        crate::skill::manager::SkillLifecycleAction::Uninstall,
        crate::skill::manager::SkillLifecycleAction::Enable,
        crate::skill::manager::SkillLifecycleAction::Disable,
    ];
    for action in unsupported_actions {
        // Explicit conversion error returned before the apply pipeline can perform target-root matching.
        // 在 apply 流程执行目标根匹配前返回的显式转换错误。
        let error = SkillApplyLifecycleAction::from_lifecycle_action(action)
            .expect_err("non-apply lifecycle action should be rejected");
        assert_eq!(error, format!("unsupported apply action {:?}", action));
    }
}

/// Verify lifecycle recovery errors keep the primary failure unchanged when recovery succeeds.
/// 验证恢复成功时生命周期恢复错误会保持主失败信息不变。
#[test]
fn lifecycle_recovery_error_keeps_base_message_when_recovery_succeeds() {
    // Formatted lifecycle failure when rollback and runtime restore both succeed.
    // 回滚与运行时恢复都成功时格式化得到的生命周期失败信息。
    let message = format_lifecycle_recovery_error(
        "Failed to finalize uninstall: commit failed".to_string(),
        Ok::<(), String>(()),
        Ok::<(), String>(()),
    );

    assert_eq!(message, "Failed to finalize uninstall: commit failed");
}

/// Verify lifecycle recovery errors append only real rollback and restore failures.
/// 验证生命周期恢复错误只追加真实发生的回滚与恢复失败。
#[test]
fn lifecycle_recovery_error_appends_failed_recovery_steps() {
    // Formatted lifecycle failure when both recovery steps fail.
    // 两个恢复步骤都失败时格式化得到的生命周期失败信息。
    let message = format_lifecycle_recovery_error(
        "Failed to reload LuaSkills after install: reload failed".to_string(),
        Err("rollback failed because backup was locked".to_string()),
        Err("runtime restore failed because manifest is invalid".to_string()),
    );

    assert_eq!(
        message,
        "Failed to reload LuaSkills after install: reload failed. rollback failed: rollback failed because backup was locked. runtime restore failed: runtime restore failed because manifest is invalid"
    );
}

/// Verify runtime request context parsing rejects malformed non-empty context objects.
/// 验证运行时请求上下文解析会拒绝格式错误的非空上下文对象。
#[test]
fn parse_runtime_request_context_json_rejects_malformed_non_empty_context() {
    // Empty request object that intentionally represents the absence of host request context.
    // 有意表示缺少宿主请求上下文的空 request 对象。
    let empty_context = parse_runtime_request_context_json(json!({}), "test.request")
        .expect("empty request object should parse");
    // Empty array produced by an empty Lua table when it carries no string keys.
    // 空 Lua 表没有字符串键时转换得到的空数组。
    let empty_lua_table_context = parse_runtime_request_context_json(json!([]), "test.request")
        .expect("empty Lua table request context should parse");
    // Valid request object preserving request identity.
    // 保留请求身份的合法 request 对象。
    let valid_context = parse_runtime_request_context_json(
        json!({
            "request_id": "req-1",
            "client_name": "Codex"
        }),
        "test.request",
    )
    .expect("valid request context should parse")
    .expect("valid request context should be present");
    // Malformed request object with a typed field that cannot deserialize into RuntimeRequestContext.
    // 带有无法反序列化为 RuntimeRequestContext 的类型字段的格式错误 request 对象。
    let malformed_context_error =
        parse_runtime_request_context_json(json!({"request_id": 42}), "test.request")
            .expect_err("malformed request context should fail");

    assert!(empty_context.is_none());
    assert!(empty_lua_table_context.is_none());
    assert_eq!(valid_context.request_id.as_deref(), Some("req-1"));
    assert!(
        malformed_context_error.contains("test.request is not a valid runtime request context")
    );
}

/// Build one invocation context whose client capabilities contain one host_result block.
/// 构造一份客户端能力中包含单个 host_result 块的调用上下文。
///
/// The host_result parameter is the raw host_result capability value under client_capabilities.
/// host_result 参数是 client_capabilities 下的原始 host_result 能力值。
///
/// Returns one invocation context that can be passed into host-result capability resolution.
/// 返回一份可传入 host-result 能力解析逻辑的调用上下文。
fn host_result_capability_test_context(
    host_result: Value,
) -> crate::runtime_options::LuaInvocationContext {
    // Request context carrying only the host-result client capability under test.
    // 仅携带本次测试关注的 host-result 客户端能力的请求上下文。
    let request_context = RuntimeRequestContext {
        client_capabilities: json!({ "host_result": host_result }),
        ..RuntimeRequestContext::default()
    };
    crate::runtime_options::LuaInvocationContext::new(Some(request_context), json!({}), json!({}))
}

/// Verify host_result enabled without allowed_kinds preserves the documented unrestricted kind list.
/// 验证 host_result 启用但缺少 allowed_kinds 时会保留约定的不限制类型列表。
#[test]
fn resolve_host_result_capability_allows_missing_allowed_kinds() {
    // Invocation context with host_result enabled and no kind restriction.
    // 启用 host_result 且不携带类型限制的调用上下文。
    let invocation_context = host_result_capability_test_context(json!({
        "enabled": true,
        "max_payload_bytes": 1024
    }));
    // Resolved host-result capability converted to JSON for stable field assertions.
    // 解析后的 host-result 能力，转换为 JSON 后进行稳定字段断言。
    let capability_json = host_result_capability_to_json_value(
        &resolve_host_result_capability(Some(&invocation_context))
            .expect("host_result capability should resolve"),
    );

    assert_eq!(capability_json["enabled"], true);
    assert_eq!(capability_json["allowed_kinds"], json!([]));
    assert_eq!(capability_json["max_payload_bytes"], json!(1024));
}

/// Verify malformed allowed_kinds is rejected instead of becoming an unrestricted kind list.
/// 验证格式错误的 allowed_kinds 会被拒绝，而不是变成不限制类型列表。
#[test]
fn resolve_host_result_capability_rejects_malformed_allowed_kinds() {
    // Invocation context with a malformed host_result allowed_kinds field.
    // 携带格式错误 host_result allowed_kinds 字段的调用上下文。
    let invocation_context = host_result_capability_test_context(json!({
        "enabled": true,
        "allowed_kinds": "change_set"
    }));
    // Capability resolution error produced by the malformed allowed_kinds field.
    // 由格式错误 allowed_kinds 字段产生的能力解析错误。
    let error = resolve_host_result_capability(Some(&invocation_context))
        .expect_err("malformed allowed_kinds should be rejected");

    assert!(error.contains("host_result.allowed_kinds"));
}

/// Verify Lua-to-JSON conversion rejects invalid UTF-8 strings instead of replacing them.
/// 验证 Lua 到 JSON 的转换会拒绝非法 UTF-8 字符串，而不是将其替换掉。
#[test]
fn lua_value_to_json_rejects_invalid_utf8_string() {
    // Lua string that intentionally contains bytes that are not valid UTF-8.
    // 有意包含非法 UTF-8 字节的 Lua 字符串。
    let lua = Lua::new();
    let invalid_string = lua
        .create_string([0xff])
        .expect("invalid UTF-8 Lua string should be constructible");
    // Conversion error returned by the shared Lua-to-JSON boundary.
    // 共享 Lua 到 JSON 边界返回的转换错误。
    let error = lua_value_to_json(&LuaValue::String(invalid_string))
        .expect_err("invalid UTF-8 Lua string should fail JSON conversion");

    assert!(error.contains("Cannot convert Lua string to JSON: invalid UTF-8"));
}

/// Verify JSON objects, arrays, nested empty containers, and null values survive a direct round trip.
/// 验证 JSON 对象、数组、嵌套空容器与 null 值可在直接往返中保持不变。
#[test]
fn json_value_round_trip_preserves_container_kinds_and_null() {
    // Lua state that owns the shared protected JSON container metatables.
    // 持有共享受保护 JSON 容器元表的 Lua 状态。
    let lua = Lua::new();
    // Independent payloads covering both top-level container kinds and recursively nested values.
    // 覆盖两种顶层容器类型及递归嵌套值的独立载荷。
    let payloads = [
        json!({}),
        json!([]),
        json!({
            "object": {},
            "array": [],
            "nested": {
                "object": {},
                "array": []
            },
            "nullable": null,
            "nullableArray": [null]
        }),
    ];

    for expected in payloads {
        // Recursively marked Lua table produced by the production JSON decoder boundary.
        // 由生产 JSON 解码边界产生的递归标记 Lua 表。
        let table = json_to_lua_table(&lua, &expected).expect("convert JSON container to Lua");
        // JSON value reconstructed by the production Lua result encoder boundary.
        // 由生产 Lua 结果编码边界重建的 JSON 值。
        let actual = lua_value_to_json(&LuaValue::Table(table))
            .expect("convert marked Lua container back to JSON");
        assert_eq!(actual, expected);
    }
}

/// Verify print argument rendering surfaces invalid UTF-8 strings in log text.
/// 验证 print 参数渲染会在日志文本中显式暴露非法 UTF-8 字符串。
#[test]
fn render_lua_print_argument_marks_invalid_utf8_string() {
    // Lua string that intentionally contains bytes that cannot be rendered as UTF-8 text.
    // 有意包含无法渲染为 UTF-8 文本字节的 Lua 字符串。
    let lua = Lua::new();
    let invalid_string = lua
        .create_string([0xff])
        .expect("invalid UTF-8 Lua print string should be constructible");
    // Rendered log argument returned by the runtime print formatter.
    // 运行时 print 格式化器返回的日志参数文本。
    let rendered = render_lua_print_argument(LuaValue::String(invalid_string));

    assert!(rendered.contains("invalid UTF-8 Lua string"));
    assert!(!rendered.is_empty());
}

/// Guard one process-wide host-tool callback test and clear global callback state on drop.
/// 保护单个进程级宿主工具回调测试，并在释放时清理全局回调状态。
struct HostToolCallbackTestGuard {
    /// Hold the process-wide mutex guard until the current test finishes.
    /// 持有进程级互斥锁直到当前测试结束。
    _guard: MutexGuard<'static, ()>,
}

impl Drop for HostToolCallbackTestGuard {
    /// Clear the global host-tool callback when one guarded test finishes.
    /// 当受保护测试结束时清理全局宿主工具回调。
    fn drop(&mut self) {
        set_host_tool_callback(None);
    }
}

/// Acquire the process-wide host-tool callback test guard.
/// 获取进程级宿主工具回调测试保护锁。
fn host_tool_callback_test_guard() -> HostToolCallbackTestGuard {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("lock host tool callback test guard");
    set_host_tool_callback(None);
    HostToolCallbackTestGuard { _guard: guard }
}

/// Acquire the process-wide environment mutation guard used by PATH-sensitive tests.
/// 获取供依赖 PATH 的测试使用的进程级环境变量修改保护锁。
/// Mark one test program file as executable on Unix-like platforms.
/// 在类 Unix 平台上将单个测试程序文件标记为可执行。
#[cfg(unix)]
fn mark_test_program_executable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("read test program metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable bit on test program");
}

/// Mark one test program file as executable on Unix-like platforms.
/// 在类 Unix 平台上将单个测试程序文件标记为可执行。
#[cfg(not(unix))]
fn mark_test_program_executable(_path: &Path) {}

/// Verify Windows PATHEXT candidates are appended without replacing the base path.
/// 验证 Windows PATHEXT 候选会追加到基础路径后，而不是替换基础路径。
#[cfg(windows)]
#[test]
fn vulcan_process_candidate_paths_appends_windows_pathexts() {
    // Process-wide environment guard that serializes PATHEXT mutation in this test.
    // 串行化本测试中 PATHEXT 修改的进程级环境保护锁。
    let _env_guard = process_env_test_guard();
    // Environment restore guard that restores the original PATHEXT after the test.
    // 测试结束后恢复原始 PATHEXT 的环境变量恢复保护器。
    let _restore_guard = TestEnvRestoreGuard::capture("PATHEXT");
    unsafe {
        std::env::set_var("PATHEXT", ".CMD;.EXE");
    }

    // Base executable path without an extension.
    // 未携带扩展名的基础可执行文件路径。
    let base = PathBuf::from(r"C:\Tools\demo-runner");
    // Candidate paths expanded from the explicit PATHEXT value.
    // 根据显式 PATHEXT 值展开得到的候选路径列表。
    let candidates = vulcan_process_candidate_paths(&base).expect("expand PATHEXT candidates");

    assert_eq!(candidates[0], base);
    assert_eq!(candidates[1], PathBuf::from(r"C:\Tools\demo-runner.cmd"));
    assert_eq!(candidates[2], PathBuf::from(r"C:\Tools\demo-runner.exe"));
}

/// Verify an explicitly empty Windows PATHEXT is respected instead of falling back to defaults.
/// 验证显式为空的 Windows PATHEXT 会被尊重，而不是退回默认扩展列表。
#[cfg(windows)]
#[test]
fn vulcan_process_candidate_paths_respects_empty_windows_pathext() {
    // Process-wide environment guard that serializes PATHEXT mutation in this test.
    // 串行化本测试中 PATHEXT 修改的进程级环境保护锁。
    let _env_guard = process_env_test_guard();
    // Environment restore guard that restores the original PATHEXT after the test.
    // 测试结束后恢复原始 PATHEXT 的环境变量恢复保护器。
    let _restore_guard = TestEnvRestoreGuard::capture("PATHEXT");
    unsafe {
        std::env::set_var("PATHEXT", " ; ; ");
    }

    // Base executable path without an extension.
    // 未携带扩展名的基础可执行文件路径。
    let base = PathBuf::from(r"C:\Tools\empty-pathext-runner");
    // Candidate paths expanded from an explicitly empty PATHEXT value.
    // 根据显式为空的 PATHEXT 值展开得到的候选路径列表。
    let candidates =
        vulcan_process_candidate_paths(&base).expect("expand empty PATHEXT candidates");

    assert_eq!(candidates, vec![base]);
}

/// Verify a missing Windows PATHEXT uses the runtime's documented default executable extensions.
/// 验证缺失的 Windows PATHEXT 会使用运行时记录的默认可执行扩展列表。
#[cfg(windows)]
#[test]
fn vulcan_process_candidate_paths_uses_default_windows_pathext_when_missing() {
    // Process-wide environment guard that serializes PATHEXT mutation in this test.
    // 串行化本测试中 PATHEXT 修改的进程级环境保护锁。
    let _env_guard = process_env_test_guard();
    // Environment restore guard that restores the original PATHEXT after the test.
    // 测试结束后恢复原始 PATHEXT 的环境变量恢复保护器。
    let _restore_guard = TestEnvRestoreGuard::capture("PATHEXT");
    unsafe {
        std::env::remove_var("PATHEXT");
    }

    // Base executable path without an extension.
    // 未携带扩展名的基础可执行文件路径。
    let base = PathBuf::from(r"C:\Tools\missing-pathext-runner");
    // Candidate paths expanded from the runtime default PATHEXT list.
    // 根据运行时默认 PATHEXT 列表展开得到的候选路径列表。
    let candidates =
        vulcan_process_candidate_paths(&base).expect("expand missing PATHEXT candidates");

    assert_eq!(candidates[0], base);
    assert_eq!(
        candidates[1],
        PathBuf::from(r"C:\Tools\missing-pathext-runner.com")
    );
    assert_eq!(
        candidates[2],
        PathBuf::from(r"C:\Tools\missing-pathext-runner.exe")
    );
    assert_eq!(
        candidates[3],
        PathBuf::from(r"C:\Tools\missing-pathext-runner.bat")
    );
    assert_eq!(
        candidates[4],
        PathBuf::from(r"C:\Tools\missing-pathext-runner.cmd")
    );
}

/// Verify Windows DLL directory conversion errors render paths through the host-visible formatter.
/// 验证 Windows DLL 目录转换错误会通过宿主可见路径渲染器输出路径。
#[cfg(windows)]
#[test]
fn windows_wide_null_path_error_uses_host_visible_path() {
    // Windows path containing one embedded NUL before the wide-string terminator is appended.
    // 在追加宽字符串终止符之前已经包含嵌入 NUL 的 Windows 路径。
    let path = PathBuf::from("C:\\luaskills\0ffi");
    // Error returned by the real Windows wide-path conversion helper.
    // 真实 Windows 宽路径转换辅助函数返回的错误。
    let error =
        windows_wide_null_path(&path).expect_err("embedded NUL path should fail conversion");
    // Expected diagnostic text rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断文本。
    let expected = format!(
        "Windows DLL directory contains an embedded NUL: {}",
        render_host_visible_path(&path)
    );

    assert_eq!(error, expected);
}

/// Verify native library search setup rejects host-provided FFI root probe errors.
/// 验证原生库搜索设置会拒绝宿主 FFI 根目录探测错误。
#[cfg(windows)]
#[test]
fn native_library_search_guard_rejects_host_ffi_root_probe_errors() {
    // Host-provided FFI root containing one embedded NUL that filesystem metadata cannot inspect.
    // 包含内嵌 NUL 的宿主 FFI 根目录，文件系统元数据无法探测该路径。
    let invalid_ffi_root = PathBuf::from("C:\\luaskills\0ffi");
    // Host options that route native library search setup through the invalid FFI root.
    // 通过非法 FFI 根目录触发原生库搜索设置的宿主选项。
    let host_options = LuaRuntimeHostOptions {
        host_provided_ffi_root: Some(invalid_ffi_root),
        ..Default::default()
    };
    // Error returned before the invalid root can behave like a missing or non-directory path.
    // 在非法根目录表现得像缺失或非目录路径之前返回的错误。
    let error = NativeLibrarySearchGuard::new(&host_options)
        .expect_err("invalid host_provided_ffi_root metadata probe should fail");

    assert!(
        error.contains("failed to inspect host_provided_ffi_root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("luaskills"), "unexpected error: {}", error);
}

/// Create one test file symlink that points at the requested target path.
/// 创建一个指向指定目标路径的测试文件符号链接。
#[cfg(unix)]
fn create_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
    create_unix_symlink(target_path, link_path).expect("create test file symlink");
    true
}

/// Return whether one Windows symlink-dependent test should be skipped because the host lacks symlink privileges.
/// 返回当前 Windows 符号链接相关测试是否应因宿主缺少符号链接权限而跳过。
#[cfg(windows)]
fn should_skip_windows_symlink_test(error: &std::io::Error) -> bool {
    /// Windows privilege error returned when symlink creation requires elevation or Developer Mode.
    /// 当符号链接创建需要管理员权限或开发者模式时 Windows 返回的权限错误码。
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

    error.kind() == std::io::ErrorKind::PermissionDenied
        || error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
}

/// Create one test file symlink that points at the requested target path.
/// 创建一个指向指定目标路径的测试文件符号链接。
#[cfg(windows)]
fn create_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
    match create_windows_file_symlink(target_path, link_path) {
        Ok(()) => true,
        Err(error) if should_skip_windows_symlink_test(&error) => {
            eprintln!(
                "skip symlink-dependent test because Windows symlink privileges are unavailable: {error}"
            );
            false
        }
        Err(error) => panic!("create test file symlink: {error}"),
    }
}

/// Create one test directory symlink that points at the requested target path.
/// 创建一个指向指定目标路径的测试目录符号链接。
#[cfg(unix)]
fn create_test_dir_symlink(link_path: &Path, target_path: &Path) -> bool {
    create_unix_symlink(target_path, link_path).expect("create test directory symlink");
    true
}

/// Create one test directory symlink that points at the requested target path.
/// 创建一个指向指定目标路径的测试目录符号链接。
#[cfg(windows)]
fn create_test_dir_symlink(link_path: &Path, target_path: &Path) -> bool {
    match create_windows_dir_symlink(target_path, link_path) {
        Ok(()) => true,
        Err(error) if should_skip_windows_symlink_test(&error) => {
            eprintln!(
                "skip symlink-dependent test because Windows symlink privileges are unavailable: {error}"
            );
            false
        }
        Err(error) => panic!("create test directory symlink: {error}"),
    }
}

/// Build one minimal loaded skill for collision-index tests.
/// 为冲突编号测试构造一个最小已加载 skill。
fn make_loaded_skill(
    directory_name: &str,
    skill_id: &str,
    local_entry_name: &str,
    lua_module: &str,
) -> LoadedSkill {
    let mut meta: SkillMeta = serde_yaml::from_str(&format!("name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nentries:\n  - name: {local_entry_name}\n    lua_entry: runtime/test.lua\n    lua_module: {lua_module}\n"))
            .expect("deserialize minimal skill meta");
    meta.bind_directory_skill_id(skill_id.to_string());
    // Real temporary runtime root required by the trusted package constructor.
    // 可信包构造器所需的真实临时运行时根。
    let runtime_root = make_temp_runtime_root(&format!("loaded-{directory_name}"));
    // Real temporary Skill directory used by the package identity.
    // 包身份使用的真实临时 Skill 目录。
    let skill_dir = runtime_root.join("skills").join(directory_name);
    fs::create_dir_all(&skill_dir).expect("create loaded Skill package root");
    // Trusted package context attached to the synthetic LoadedSkill fixture.
    // 附加到合成 LoadedSkill 夹具的可信包上下文。
    let managed_package =
        ManagedRuntimePackageContext::for_skill(skill_id, &skill_dir, &runtime_root, None)
            .expect("create loaded Skill managed package");
    LoadedSkill {
        meta,
        dir: skill_dir,
        root_name: "ROOT".to_string(),
        managed_package,
        lancedb_binding: None,
        sqlite_binding: None,
        resolved_entry_names: HashMap::new(),
    }
}

/// Verify host-visible path normalization strips the Windows drive-letter verbatim prefix.
/// 验证对宿主可见的路径归一化会去掉 Windows 盘符 verbatim 前缀。
#[cfg(windows)]
#[test]
fn normalize_host_visible_path_text_strips_windows_drive_verbatim_prefix() {
    assert_eq!(
        crate::runtime::path::normalize_host_visible_path_text(
            r"\\?\C:\runtime-test-root\skill.lua",
        ),
        r"C:\runtime-test-root\skill.lua"
    );
}

/// Verify the `vulcan.fs.list` non-UTF-8 filename diagnostic renders one host-visible directory path.
/// 验证 `vulcan.fs.list` 非 UTF-8 文件名诊断会渲染宿主可见的目录路径。
#[cfg(windows)]
#[test]
fn vulcan_fs_list_non_utf8_file_name_error_uses_host_visible_path() {
    let directory = PathBuf::from(r"\\?\C:\runtime-test-root\skills");
    let error = format_vulcan_fs_list_non_utf8_file_name_error(
        &directory,
        std::ffi::OsStr::new("invalid-entry"),
    );

    assert!(error.contains(r"C:\runtime-test-root\skills"));
    assert!(error.contains("invalid-entry"));
    assert!(!error.contains(r"\\?\"));
}

/// Verify host-visible path normalization strips the Windows UNC verbatim prefix.
/// 验证对宿主可见的路径归一化会去掉 Windows UNC verbatim 前缀。
#[cfg(windows)]
#[test]
fn normalize_host_visible_path_text_strips_windows_unc_verbatim_prefix() {
    assert_eq!(
        crate::runtime::path::normalize_host_visible_path_text(r"\\?\UNC\server\share\skill.lua",),
        r"\\server\share\skill.lua"
    );
}

/// Verify host-visible path normalization preserves ordinary POSIX paths on Unix-like platforms.
/// 验证对宿主可见的路径归一化会在类 Unix 平台保留普通 POSIX 路径。
#[cfg(not(windows))]
#[test]
fn normalize_host_visible_path_text_preserves_posix_path() {
    assert_eq!(
        crate::runtime::path::normalize_host_visible_path_text("/tmp/runtime-test-root/skill.lua"),
        "/tmp/runtime-test-root/skill.lua"
    );
}

/// Build one minimal engine instance used only for registry tests.
/// 构造仅用于入口注册表测试的最小引擎实例。
fn make_test_engine(skills: HashMap<String, LoadedSkill>) -> LuaEngine {
    let skill_config_service = Arc::new(
        SkillPackageConfigService::new(None, Duration::from_secs(5), Duration::from_millis(200))
            .expect("create runtime test skill config service"),
    );
    skill_config_service.replace_registry(
        skills
            .values()
            .map(|skill| (&skill.meta, skill.root_name.as_str(), skill.dir.as_path())),
    );
    LuaEngine {
        skills,
        entry_registry: Default::default(),
        runtime_skill_roots: Vec::new(),
        pool: Arc::new(LuaVmPool {
            config: LuaVmPoolConfig {
                min_size: 1,
                max_size: 1,
                idle_ttl_secs: 60,
            },
            state: Mutex::new(LuaVmPoolState {
                available: Vec::new(),
                total_count: 0,
            }),
            condvar: Condvar::new(),
        }),
        runlua_pool: Arc::new(LuaVmPool::new(default_runlua_vm_pool_config())),
        public_runtime_sessions: Arc::new(RuntimeSessionManager::new()),
        system_runtime_sessions: Arc::new(RuntimeSessionManager::new()),
        managed_runtime_services: ManagedRuntimeServices::new()
            .expect("create managed runtime test services"),
        managed_runtime_workers: ManagedRuntimeWorkerService::new(),
        managed_runtime_roots: None,
        skill_config_service,
        lancedb_host: None,
        sqlite_host: None,
        database_provider_callbacks: Arc::new(RuntimeDatabaseProviderCallbacks::default()),
        native_library_search_guard: NativeLibrarySearchGuard::default(),
        host_options: Arc::new(LuaRuntimeHostOptions::default()),
    }
}

/// Build one minimal runtime engine that can execute pooled-VM isolation tests.
/// 构造一个可用于池化虚拟机隔离测试的最小运行时引擎。
fn make_runtime_test_engine() -> LuaEngine {
    make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
}

/// Return the single-VM pool configuration shared by ordinary runtime engine tests.
/// 返回普通运行时引擎测试共享的单虚拟机池配置。
fn runtime_test_single_vm_pool_config() -> LuaVmPoolConfig {
    LuaVmPoolConfig {
        min_size: 1,
        max_size: 1,
        idle_ttl_secs: 60,
    }
}

/// Build engine options with the ordinary single-VM test pool and explicit host options.
/// 使用普通单虚拟机测试池和显式宿主选项构造引擎选项。
fn runtime_test_engine_options(host_options: LuaRuntimeHostOptions) -> LuaEngineOptions {
    LuaEngineOptions {
        host_options,
        pool_config: runtime_test_single_vm_pool_config(),
    }
}

/// Try to build one minimal runtime engine with explicit host options.
/// 尝试使用显式宿主选项构造一个最小运行时引擎。
fn try_make_runtime_test_engine_with_host_options(
    host_options: LuaRuntimeHostOptions,
) -> Result<LuaEngine, Box<dyn std::error::Error>> {
    LuaEngine::new(runtime_test_engine_options(host_options))
}

/// Build one minimal runtime engine with explicit host options.
/// 使用显式宿主选项构造一个最小运行时引擎。
fn make_runtime_test_engine_with_host_options(host_options: LuaRuntimeHostOptions) -> LuaEngine {
    try_make_runtime_test_engine_with_host_options(host_options)
        .expect("create runtime test engine")
}

/// Verify two engines retain independent host-selected distribution and environment authorities.
/// 验证两个引擎会保留彼此独立的宿主选定发行与环境授权。
#[test]
fn engines_with_distinct_managed_roots_do_not_share_root_authority() {
    // Root isolates both engine layouts while allowing direct path-identity comparisons.
    // Root 隔离两个引擎布局，同时允许直接比较路径身份。
    let root = std::env::temp_dir().join(format!(
        "luaskills-distinct-engine-managed-roots-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    // RuntimeRootA is the first engine's LuaSkills data authority.
    // RuntimeRootA 是第一个引擎的 LuaSkills 数据授权。
    let runtime_root_a = root.join("engine-a").join("runtime data");
    // DistributionRootA is the first engine's read-only runtime authority.
    // DistributionRootA 是第一个引擎的只读运行时授权。
    let distribution_root_a = root.join("engine-a").join("application runtimes");
    // EnvironmentRootA is the first engine's writable environment authority.
    // EnvironmentRootA 是第一个引擎的可写环境授权。
    let environment_root_a = root.join("engine-a").join("managed environments");
    // RuntimeRootB is the second engine's independent LuaSkills data authority.
    // RuntimeRootB 是第二个引擎的独立 LuaSkills 数据授权。
    let runtime_root_b = root.join("engine-b").join("runtime data");
    // DistributionRootB is the second engine's distinct read-only runtime authority.
    // DistributionRootB 是第二个引擎的独立只读运行时授权。
    let distribution_root_b = root.join("engine-b").join("application runtimes");
    // EnvironmentRootB is the second engine's distinct writable environment authority.
    // EnvironmentRootB 是第二个引擎的独立可写环境授权。
    let environment_root_b = root.join("engine-b").join("managed environments");
    fs::create_dir_all(&runtime_root_a).expect("create first engine runtime root");
    fs::create_dir_all(&distribution_root_a).expect("create first engine distribution root");
    fs::create_dir_all(&runtime_root_b).expect("create second engine runtime root");
    fs::create_dir_all(&distribution_root_b).expect("create second engine distribution root");

    // HostOptionsA binds only the first engine's three selected filesystem boundaries.
    // HostOptionsA 仅绑定第一个引擎选定的三个文件系统边界。
    let mut host_options_a = LuaRuntimeHostOptions::with_runtime_root(&runtime_root_a);
    host_options_a.managed_runtime_distribution_root = Some(distribution_root_a.clone());
    host_options_a.managed_runtime_environment_root = Some(environment_root_a.clone());
    // HostOptionsB binds only the second engine's three selected filesystem boundaries.
    // HostOptionsB 仅绑定第二个引擎选定的三个文件系统边界。
    let mut host_options_b = LuaRuntimeHostOptions::with_runtime_root(&runtime_root_b);
    host_options_b.managed_runtime_distribution_root = Some(distribution_root_b.clone());
    host_options_b.managed_runtime_environment_root = Some(environment_root_b.clone());
    // EngineA owns a root-set object that must never be reused by EngineB.
    // EngineA 拥有绝不能被 EngineB 复用的根集合对象。
    let engine_a = make_runtime_test_engine_with_host_options(host_options_a);
    // EngineB owns a separately allocated and separately pinned root-set object.
    // EngineB 拥有独立分配且独立固定的根集合对象。
    let engine_b = make_runtime_test_engine_with_host_options(host_options_b);
    // RootsA exposes the first engine's immutable selected authorities.
    // RootsA 暴露第一个引擎不可变的选定授权。
    let roots_a = engine_a
        .managed_runtime_roots
        .as_ref()
        .expect("first engine managed roots");
    // RootsB exposes the second engine's immutable selected authorities.
    // RootsB 暴露第二个引擎不可变的选定授权。
    let roots_b = engine_b
        .managed_runtime_roots
        .as_ref()
        .expect("second engine managed roots");

    assert!(!Arc::ptr_eq(roots_a, roots_b));
    assert_ne!(roots_a.distribution_root(), roots_b.distribution_root());
    assert_ne!(roots_a.environment_root(), roots_b.environment_root());
    assert_eq!(
        roots_a.distribution_root(),
        fs::canonicalize(&distribution_root_a).expect("canonical first distribution root")
    );
    assert_eq!(
        roots_b.distribution_root(),
        fs::canonicalize(&distribution_root_b).expect("canonical second distribution root")
    );
    assert_eq!(
        roots_a.environment_root(),
        fs::canonicalize(&environment_root_a).expect("canonical first environment root")
    );
    assert_eq!(
        roots_b.environment_root(),
        fs::canonicalize(&environment_root_b).expect("canonical second environment root")
    );
    drop(engine_a);
    drop(engine_b);
    let _ = fs::remove_dir_all(root);
}

/// Verify managed Python and Node bridge tables are present in the Lua-facing runtime module.
/// 验证面向 Lua 的运行时模块中已经注册受管 Python 与 Node 桥接表。
#[test]
fn vulcan_runtime_registers_managed_child_runtime_bridges() {
    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
return {
  python_table = type(vulcan.runtime.python),
  python_status = type(vulcan.runtime.python.status),
  python_invoke = type(vulcan.runtime.python.invoke),
  python_session = type(vulcan.runtime.python.session),
  python_session_open = type(vulcan.runtime.python.session.open),
  node_table = type(vulcan.runtime.node),
  node_status = type(vulcan.runtime.node.status),
  node_invoke = type(vulcan.runtime.node.invoke),
  node_session = type(vulcan.runtime.node.session),
  node_session_open = type(vulcan.runtime.node.session.open),
}
"#,
            &json!({}),
            None,
        )
        .expect("managed runtime bridge tables should be registered");

    assert_eq!(result["python_table"], "table");
    assert_eq!(result["python_status"], "function");
    assert_eq!(result["python_invoke"], "function");
    assert_eq!(result["python_session"], "table");
    assert_eq!(result["python_session_open"], "function");
    assert_eq!(result["node_table"], "table");
    assert_eq!(result["node_status"], "function");
    assert_eq!(result["node_invoke"], "function");
    assert_eq!(result["node_session"], "table");
    assert_eq!(result["node_session_open"], "function");
}

/// Verify ordinary Skill packages cannot open persistent Python or Node managed sessions.
/// 验证普通 Skill 包不能打开持久 Python 或 Node 受管会话。
#[test]
fn ordinary_skill_package_cannot_open_persistent_managed_sessions() {
    let runtime_root = make_temp_runtime_root("ordinary-skill-persistent-session");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create ordinary Skill session runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "ordinary-session-skill");
    let lua = Lua::new();
    replace_lua_managed_package_context(&lua, Some(package));
    let services = ManagedRuntimeServices::new().expect("create managed session test services");
    lua.set_app_data(services);
    let encoding = default_runtime_text_encoding();

    let python_error = super::open_managed_python_session(&lua, LuaValue::Nil, encoding)
        .expect_err("ordinary Skill Python persistent session must be rejected")
        .to_string();
    let node_error = super::open_managed_node_session(&lua, LuaValue::Nil, encoding)
        .expect_err("ordinary Skill Node persistent session must be rejected")
        .to_string();

    assert!(python_error.contains("available only inside a System Plugin lease"));
    assert!(node_error.contains("available only inside a System Plugin lease"));
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Build one temporary runtime root path for one isolated skill-config test case.
/// 为单个隔离技能配置测试用例构造一条临时运行时根目录路径。
fn make_temp_runtime_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "luaskills_{}_{}_{}",
        label,
        std::process::id(),
        label.len()
    ))
}

/// Verify package path setup reports lua package directory metadata probe errors.
/// 验证 package 路径初始化会报告 lua 包目录元数据探测错误。
///
/// This test has no parameters and fails through assertions when invalid package roots are hidden.
/// 本测试不接收参数；当非法包根目录被隐藏时会通过断言失败。
///
/// Return unit after validating the setup helper returns a lua_packages_dir inspection diagnostic.
/// 校验初始化辅助函数返回 lua_packages_dir 探测诊断后返回 unit。
#[test]
fn setup_package_paths_reports_invalid_lua_packages_dir_probe_errors() {
    // Lua state passed to the same package path setup helper used by pooled runtime VMs.
    // 传给池化运行时虚拟机同一 package 路径初始化辅助函数的 Lua 状态。
    let lua = Lua::new();
    // Host package root containing an embedded NUL that filesystem metadata cannot inspect.
    // 包含内嵌 NUL 且文件系统元数据无法探测的宿主包根目录。
    let invalid_lua_packages_dir = PathBuf::from("invalid\0lua-packages");
    // Host options that force setup_package_paths to inspect the invalid lua package root.
    // 强制 setup_package_paths 探测非法 lua 包根目录的宿主选项。
    let host_options = LuaRuntimeHostOptions {
        lua_packages_dir: Some(invalid_lua_packages_dir),
        ..Default::default()
    };

    // Error text returned before the invalid path can be treated like a missing package root.
    // 在非法路径被当作包根目录缺失之前返回的错误文本。
    let error_text = LuaEngine::setup_package_paths(&lua, &host_options)
        .expect_err("invalid lua_packages_dir metadata probe should fail")
        .to_string();

    assert!(
        error_text.contains("failed to inspect configured lua_packages_dir"),
        "unexpected error text: {}",
        error_text
    );
}

/// Verify package path setup reports host-provided FFI root metadata probe errors.
/// 验证 package 路径初始化会报告宿主提供 FFI 根目录的元数据探测错误。
///
/// This test has no parameters and fails through assertions when invalid FFI roots are omitted.
/// 本测试不接收参数；当非法 FFI 根目录被省略时会通过断言失败。
///
/// Return unit after validating the setup helper returns a host_provided_ffi_root inspection diagnostic.
/// 校验初始化辅助函数返回 host_provided_ffi_root 探测诊断后返回 unit。
#[test]
fn setup_package_paths_reports_invalid_host_provided_ffi_root_probe_errors() {
    // Temporary runtime root used to provide an existing lua package directory.
    // 用于提供已存在 lua 包目录的临时运行时根目录。
    let runtime_root = make_temp_runtime_root("package-path-invalid-ffi-root");
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = fs::remove_dir_all(&runtime_root);
    // Existing lua package root required before the helper inspects the optional FFI root.
    // 辅助函数探测可选 FFI 根目录前所需的已存在 lua 包根目录。
    let lua_packages_dir = runtime_root.join("lua_packages");
    fs::create_dir_all(&lua_packages_dir).expect("create lua packages dir");
    // Host FFI root containing an embedded NUL that filesystem metadata cannot inspect.
    // 包含内嵌 NUL 且文件系统元数据无法探测的宿主 FFI 根目录。
    let invalid_ffi_root = runtime_root.join("libs\0invalid");
    // Lua state passed to the same package path setup helper used by pooled runtime VMs.
    // 传给池化运行时虚拟机同一 package 路径初始化辅助函数的 Lua 状态。
    let lua = Lua::new();
    // Host options that route FFI search path setup through the invalid FFI root.
    // 通过非法 FFI 根目录触发 FFI 搜索路径初始化的宿主选项。
    let host_options = LuaRuntimeHostOptions {
        lua_packages_dir: Some(lua_packages_dir),
        host_provided_ffi_root: Some(invalid_ffi_root),
        ..Default::default()
    };

    // Error text returned before the invalid FFI root can be silently omitted from cpath.
    // 在非法 FFI 根目录被静默排除出 cpath 之前返回的错误文本。
    let error_text = LuaEngine::setup_package_paths(&lua, &host_options)
        .expect_err("invalid host_provided_ffi_root metadata probe should fail")
        .to_string();

    assert!(
        error_text.contains("failed to inspect configured host_provided_ffi_root"),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify package path setup rejects a configured lua package root that is not a directory.
/// 验证 package 路径初始化会拒绝不是目录的已配置 lua 包根路径。
///
/// This test has no parameters and fails through assertions when file roots are inserted into package paths.
/// 本测试不接收参数；当文件根路径被插入 package 路径时会通过断言失败。
///
/// Return unit after validating the setup helper reports a non-directory lua package root.
/// 校验初始化辅助函数报告非目录 lua 包根路径后返回 unit。
#[test]
fn setup_package_paths_rejects_non_directory_lua_packages_dir() {
    // Temporary runtime root used to isolate the non-directory package root fixture.
    // 用于隔离非目录包根路径夹具的临时运行时根目录。
    let runtime_root = make_temp_runtime_root("package-path-file-root");
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    // File path deliberately configured where setup_package_paths requires a directory root.
    // 故意配置在 setup_package_paths 需要目录根的位置上的文件路径。
    let lua_packages_file = runtime_root.join("lua_packages_file");
    fs::write(&lua_packages_file, "not a directory").expect("write package root file");
    // Lua state passed to the same package path setup helper used by pooled runtime VMs.
    // 传给池化运行时虚拟机同一 package 路径初始化辅助函数的 Lua 状态。
    let lua = Lua::new();
    // Host options that force setup_package_paths to inspect the non-directory lua package root.
    // 强制 setup_package_paths 探测非目录 lua 包根路径的宿主选项。
    let host_options = LuaRuntimeHostOptions {
        lua_packages_dir: Some(lua_packages_file),
        ..Default::default()
    };

    // Error text returned before a file path can be inserted into package.path or package.cpath.
    // 在文件路径被插入 package.path 或 package.cpath 之前返回的错误文本。
    let error_text = LuaEngine::setup_package_paths(&lua, &host_options)
        .expect_err("non-directory lua_packages_dir should fail")
        .to_string();

    assert!(
        error_text.contains("configured lua_packages_dir is not a directory"),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify configured Lua search roots reject Windows namespaces without ordinary equivalents.
/// 验证已配置 Lua 搜索根会拒绝不存在普通等价形式的 Windows 命名空间。
#[cfg(windows)]
#[test]
fn configured_package_search_root_rejects_unsupported_windows_verbatim_namespace() {
    // Volume GUID path checked before metadata lookup, so no platform-specific fixture is required.
    // 在元数据查询前检查的卷 GUID 路径，因此无需平台专属夹具。
    let path = Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\lua_packages");
    // Stable configuration error proving the namespace never reaches package.path or package.cpath.
    // 稳定配置错误用于证明该命名空间绝不会进入 package.path 或 package.cpath。
    let error = configured_package_search_directory_exists(path, "lua_packages_dir")
        .expect_err("unsupported package search namespace must fail");
    assert!(
        error
            .to_string()
            .contains("unsupported Windows verbatim path namespace"),
        "unexpected package search error: {error}"
    );
}

/// Verify ordinary VM package search paths and Lua path arguments strip Windows verbatim prefixes.
/// 验证普通虚拟机包搜索路径与 Lua 路径参数会去除 Windows verbatim 前缀。
#[cfg(windows)]
#[test]
fn ordinary_lua_path_boundaries_strip_windows_verbatim_prefixes() {
    // Temporary runtime root canonicalized to the Windows verbatim spelling used by Rust.
    // 规范化为 Rust 所用 Windows verbatim 写法的临时运行时根。
    let runtime_root = make_temp_runtime_root("ordinary-lua-verbatim-filter");
    let _ = fs::remove_dir_all(&runtime_root);
    // Lua package and native-module roots required by the production setup helper.
    // 生产初始化辅助函数所需的 Lua 包根与原生模块根。
    let lua_packages_dir = runtime_root.join("lua_packages");
    let ffi_root = runtime_root.join("ffi");
    fs::create_dir_all(&lua_packages_dir).expect("create Lua package root");
    fs::create_dir_all(&ffi_root).expect("create FFI root");
    // Canonical roots deliberately retain the verbatim prefix internally on Windows.
    // 在 Windows 内部有意保留 verbatim 前缀的规范根。
    let canonical_lua_packages =
        fs::canonicalize(&lua_packages_dir).expect("canonicalize Lua package root");
    let canonical_ffi_root = fs::canonicalize(&ffi_root).expect("canonicalize FFI root");
    assert!(
        canonical_lua_packages
            .to_string_lossy()
            .starts_with(r"\\?\"),
        "expected a verbatim canonical path: {}",
        canonical_lua_packages.to_string_lossy()
    );
    // Lua state receiving the same package path setup as production pooled VMs.
    // 接收与生产池化虚拟机相同包路径初始化的 Lua 状态。
    let lua = Lua::new();
    // Host options injecting canonical roots across the Rust-to-Lua boundary.
    // 跨 Rust 到 Lua 边界注入规范根的宿主选项。
    let host_options = LuaRuntimeHostOptions {
        lua_packages_dir: Some(canonical_lua_packages.clone()),
        host_provided_ffi_root: Some(canonical_ffi_root),
        ..Default::default()
    };
    LuaEngine::setup_package_paths(&lua, &host_options)
        .expect("configure ordinary Lua package paths");
    // Actual Lua package search strings after production initialization.
    // 生产初始化后的真实 Lua 包搜索字符串。
    let package: Table = lua.globals().get("package").expect("get package table");
    let package_path: String = package.get("path").expect("get package.path");
    let package_cpath: String = package.get("cpath").expect("get package.cpath");
    assert!(!package_path.contains(r"\\?\"), "{package_path}");
    assert!(!package_cpath.contains(r"\\?\"), "{package_cpath}");
    assert!(package_path.contains(&render_host_visible_path(&canonical_lua_packages)));

    // Full runtime API check proving a host-injected verbatim path is normalized before Lua use.
    // 完整运行时 API 检查，证明宿主注入的 verbatim 路径会在 Lua 使用前被规范化。
    let engine = make_runtime_test_engine();
    let normalized = engine
        .run_lua(
            "return vulcan.path.normalize(args.path)",
            &json!({"path": canonical_lua_packages}),
            None,
        )
        .expect("normalize verbatim Lua path argument");
    assert_eq!(
        normalized,
        json!(render_host_visible_path(&canonical_lua_packages))
    );
    // Reserved PWD field must be normalized as soon as host JSON enters Lua arguments.
    // 保留的 PWD 字段必须在宿主 JSON 进入 Lua 参数时立即完成归一化。
    // Verbatim text retained as a control value for non-path and nested JSON fields.
    // 作为非路径字段与嵌套 JSON 字段对照值保留的 verbatim 文本。
    let verbatim_text = canonical_lua_packages.to_string_lossy().into_owned();
    // Injected argument result proving only the declared top-level PWD field is rewritten.
    // 注入参数结果，用于证明仅声明的顶层 PWD 字段会被改写。
    let injected_paths = engine
        .run_lua(
            "return { PWD = args.PWD, raw = args.raw, nested = args.nested.PWD }",
            &json!({
                "PWD": canonical_lua_packages,
                "raw": verbatim_text,
                "nested": {"PWD": verbatim_text}
            }),
            None,
        )
        .expect("read normalized host-injected PWD");
    assert_eq!(
        injected_paths["PWD"],
        json!(render_host_visible_path(&canonical_lua_packages))
    );
    assert_eq!(injected_paths["raw"], json!(verbatim_text));
    assert_eq!(injected_paths["nested"], json!(verbatim_text));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify Lua and host argument paths reject Windows verbatim namespaces without safe equivalents.
/// 验证 Lua 与宿主参数路径会拒绝不存在安全等价形式的 Windows verbatim 命名空间。
#[cfg(windows)]
#[test]
fn ordinary_lua_path_boundaries_reject_unsupported_windows_verbatim_namespaces() {
    // Runtime engine exercising both Lua path APIs and the reserved host-injected PWD field.
    // 同时覆盖 Lua 路径 API 与宿主注入保留 PWD 字段的运行时引擎。
    let engine = make_runtime_test_engine();
    // Volume GUID namespace deliberately lacking an ordinary DOS or UNC spelling.
    // 有意使用不存在普通 DOS 或 UNC 写法的卷 GUID 命名空间。
    let volume_path = r"\\?\Volume{00000000-0000-0000-0000-000000000000}\module.lua";

    // Lua path parser failure returned before any filesystem lookup occurs.
    // 在任何文件系统寻址发生前返回的 Lua 路径解析失败。
    let path_error = engine
        .run_lua(
            "return vulcan.path.normalize(args.path)",
            &json!({"path": volume_path}),
            None,
        )
        .expect_err("unsupported path namespace must fail Lua path parsing");
    assert!(
        path_error.contains("unsupported Windows verbatim path namespace"),
        "unexpected path error: {path_error}"
    );

    // Reserved PWD rejection proving host injection cannot reinterpret the namespace as relative.
    // 保留 PWD 拒绝结果，用于证明宿主注入不会把该命名空间重新解释为相对路径。
    let pwd_error = engine
        .run_lua("return args.PWD", &json!({"PWD": volume_path}), None)
        .expect_err("unsupported PWD namespace must fail host argument conversion");
    assert!(
        pwd_error.contains("unsupported Windows verbatim path namespace"),
        "unexpected PWD error: {pwd_error}"
    );

    // Process lookup failure proving executable search never receives a rewritten relative name.
    // 进程查找失败结果，用于证明可执行文件搜索不会收到被改写的相对名称。
    let which_error = engine
        .run_lua(
            "return vulcan.process.which(args.path)",
            &json!({"path": volume_path}),
            None,
        )
        .expect_err("unsupported process path namespace must fail lookup parsing");
    assert!(
        which_error.contains("unsupported Windows verbatim path namespace"),
        "unexpected process.which error: {which_error}"
    );
}

/// Build one minimal managed Node environment plan for import-root tests.
/// 为 import-root 测试构造一个最小受管 Node 环境计划。
///
/// The env_dir parameter is the environment directory that owns `.luaskills-packages`.
/// env_dir 参数是拥有 `.luaskills-packages` 的环境目录。
///
/// Return a plan with stable dummy dependency metadata and the requested environment directory.
/// 返回带有稳定假依赖元数据和指定环境目录的计划。
fn make_test_managed_node_env_plan(env_dir: PathBuf) -> ManagedRuntimeEnvPlan {
    // ManagedRuntimeRoots supplies a real stable authority while this helper controls env_dir separately.
    // ManagedRuntimeRoots 提供真实稳定授权，同时本辅助函数单独控制 env_dir。
    let authority_root = std::env::temp_dir();
    let managed_runtime_roots = Arc::new(
        ManagedRuntimeRoots::new(
            &authority_root,
            Some(&authority_root),
            Some(&authority_root),
        )
        .expect("create synthetic Node plan roots"),
    );
    ManagedRuntimeEnvPlan {
        runtime: ManagedRuntimeKind::Node,
        platform: "windows-x64".to_string(),
        runtime_root: managed_runtime_roots.runtime_root().to_path_buf(),
        distribution_root: managed_runtime_roots.distribution_root().to_path_buf(),
        environment_root: managed_runtime_roots.environment_root().to_path_buf(),
        distribution_source: ManagedRuntimeRootSource::HostConfigured,
        environment_source: ManagedRuntimeRootSource::HostConfigured,
        runtime_install_root: authority_root.join("node-install"),
        runtime_version: "20.0.0".to_string(),
        runtime_executable: env_dir.join("node.exe"),
        runtime_install_manifest_hash: "node-manifest".to_string(),
        runtime_executable_hash: "node-executable".to_string(),
        package_manager: "pnpm".to_string(),
        package_manager_version: "9.0.0".to_string(),
        package_manager_executable: env_dir.join("pnpm.cmd"),
        package_manager_install_root: authority_root.join("pnpm-install"),
        package_manager_install_manifest_hash: "pnpm-manifest".to_string(),
        package_manager_executable_hash: "pnpm-executable".to_string(),
        managed_runtime_roots,
        package_manifest_path: None,
        lockfile_path: env_dir.join("pnpm-lock.yaml"),
        lock_hash: "lock-hash".to_string(),
        package_manifest_hash: None,
        env_hash: "env-hash".to_string(),
        env_dir: env_dir.clone(),
        expected_marker: ManagedRuntimeEnvMarker {
            schema_version: MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION,
            runtime: "node".to_string(),
            runtime_version: "20.0.0".to_string(),
            package_manager: "pnpm".to_string(),
            package_manager_version: "9.0.0".to_string(),
            platform: "windows-x64".to_string(),
            lock_hash: "lock-hash".to_string(),
            package_manifest_hash: None,
            runtime_install_manifest_hash: "node-manifest".to_string(),
            runtime_executable_hash: "node-executable".to_string(),
            package_manager_install_manifest_hash: "pnpm-manifest".to_string(),
            package_manager_executable_hash: "pnpm-executable".to_string(),
            env_hash: "env-hash".to_string(),
        },
    }
}

/// Write one minimal ordinary-file managed installation for identity-validation tests.
/// 为身份校验测试写入一个最小普通文件受管安装。
///
/// `install_dir` is the fixed asset directory; identity fields become the strict manifest, and the
/// returned executable is canonical and strictly contained by that directory.
/// `install_dir` 是固定资产目录；身份字段会写入严格清单，返回的可执行文件为规范路径且严格
/// 包含在该目录内。
///
/// Returns the canonical executable path after all fixture bytes are materialized.
/// 全部夹具字节落盘后返回规范可执行文件路径。
fn write_test_managed_runtime_install(
    install_dir: &Path,
    runtime: &str,
    version: &str,
    platform: &str,
) -> PathBuf {
    // ExecutableName keeps the synthetic ordinary file recognizable on each native platform.
    // ExecutableName 使合成普通文件在每个原生平台上都保持可识别。
    let executable_name = if cfg!(windows) {
        format!("{runtime}.exe")
    } else {
        runtime.to_string()
    };
    // RelativeExecutable obeys the same normal-component contract as production manifests.
    // RelativeExecutable 遵守与生产清单相同的普通路径组件契约。
    let relative_executable = PathBuf::from("bin").join(executable_name);
    // Executable is the concrete contained file whose bytes form the asset identity.
    // Executable 是具体受包含文件，其字节构成资产身份。
    let executable = install_dir.join(&relative_executable);
    fs::create_dir_all(executable.parent().expect("synthetic executable parent"))
        .expect("create synthetic managed install");
    fs::write(&executable, format!("{runtime}-{version}-{platform}"))
        .expect("write synthetic managed executable");
    // Manifest is the exact strict identity document consumed by the production resolver.
    // Manifest 是生产解析器消费的精确严格身份文档。
    let manifest = json!({
        "schema_version": 1,
        "runtime": runtime,
        "version": version,
        "platform": platform,
        "executable": relative_executable,
    });
    fs::write(
        install_dir.join("runtime-manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("encode synthetic managed manifest"),
    )
    .expect("write synthetic managed manifest");
    fs::canonicalize(executable).expect("canonicalize synthetic managed executable")
}

/// Build one identity-valid managed Node plan for Worker snapshot lifecycle tests.
/// 为 Worker 快照生命周期测试构造一个身份有效的受管 Node 计划。
///
/// `env_dir` must be an existing canonical directory whose parent owns all disposable fixture data.
/// `env_dir` 必须是现有规范目录，其父目录拥有全部可丢弃夹具数据。
///
/// Returns a plan whose roots, manifests, executables, and hashes pass production revalidation.
/// 返回根、清单、可执行文件与哈希均可通过生产重新校验的计划。
fn make_validated_test_managed_node_env_plan(env_dir: PathBuf) -> ManagedRuntimeEnvPlan {
    // RuntimeRoot owns both the explicit environment authority and disposable distribution fixture.
    // RuntimeRoot 同时拥有显式环境授权与可丢弃发行夹具。
    let runtime_root = env_dir
        .parent()
        .expect("validated environment parent")
        .to_path_buf();
    // Platform is the exact supported key for the native test runner.
    // Platform 是原生测试运行器对应的精确受支持键。
    let platform = current_managed_runtime_platform_key().expect("supported test platform");
    // DistributionRoot is separate from env_dir while remaining under the disposable test root.
    // DistributionRoot 与 env_dir 分离，同时仍位于可丢弃测试根下。
    let distribution_root = runtime_root.join("managed-plan-distributions");
    fs::create_dir_all(&distribution_root).expect("create synthetic distribution root");
    // NodeInstallDir follows the fixed public Node asset naming contract.
    // NodeInstallDir 遵守固定公开 Node 资产命名契约。
    let node_install_dir = distribution_root
        .join("node")
        .join(format!("node-20.0.0-{platform}"));
    write_test_managed_runtime_install(&node_install_dir, "node", "20.0.0", &platform);
    // PnpmInstallDir follows the platform-neutral package-manager naming contract.
    // PnpmInstallDir 遵守平台中立的包管理器命名契约。
    let pnpm_install_dir = distribution_root.join("node").join("pnpm-9.0.0");
    // PnpmExecutable is retained directly because the public resolver intentionally exposes runtimes only.
    // PnpmExecutable 会被直接保留，因为公共解析器有意只暴露运行时。
    let pnpm_executable =
        write_test_managed_runtime_install(&pnpm_install_dir, "pnpm", "9.0.0", "any");
    // Roots pins the same native objects that the plan must revalidate before snapshot creation.
    // Roots 固定计划在创建快照前必须重新校验的同一批原生对象。
    let roots = Arc::new(
        ManagedRuntimeRoots::new(&runtime_root, Some(&distribution_root), Some(&runtime_root))
            .expect("create validated synthetic roots"),
    );
    // NodeDescriptor comes from the production read-only resolver and supplies exact asset hashes.
    // NodeDescriptor 来自生产只读解析器，并提供精确资产哈希。
    let node_descriptor = resolve_managed_runtime_install(
        roots.distribution_root(),
        ManagedRuntimeKind::Node,
        "20.0.0",
        &platform,
    )
    .expect("resolve synthetic Node installation");
    // PnpmInstallRoot is the canonical platform-neutral package-manager installation object.
    // PnpmInstallRoot 是规范的平台中立包管理器安装对象。
    let pnpm_install_root =
        fs::canonicalize(&pnpm_install_dir).expect("canonicalize synthetic pnpm installation");
    // PnpmManifestHash identifies the exact manifest bytes consumed during plan validation.
    // PnpmManifestHash 标识计划校验期间消费的精确清单字节。
    let pnpm_manifest_hash = sha256_file(&pnpm_install_root.join("runtime-manifest.json"))
        .expect("hash synthetic pnpm manifest");
    // PnpmExecutableHash identifies the exact package-manager executable bytes.
    // PnpmExecutableHash 标识精确包管理器可执行文件字节。
    let pnpm_executable_hash =
        sha256_file(&pnpm_executable).expect("hash synthetic pnpm executable");
    // ExpectedMarker mirrors every identity field retained by the synthetic plan.
    // ExpectedMarker 镜像合成计划保留的每个身份字段。
    let expected_marker = ManagedRuntimeEnvMarker {
        schema_version: MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION,
        runtime: "node".to_string(),
        runtime_version: "20.0.0".to_string(),
        package_manager: "pnpm".to_string(),
        package_manager_version: "9.0.0".to_string(),
        platform: platform.clone(),
        lock_hash: "lock-hash".to_string(),
        package_manifest_hash: None,
        runtime_install_manifest_hash: node_descriptor.manifest_hash.clone(),
        runtime_executable_hash: node_descriptor.executable_hash.clone(),
        package_manager_install_manifest_hash: pnpm_manifest_hash.clone(),
        package_manager_executable_hash: pnpm_executable_hash.clone(),
        env_hash: "env-hash".to_string(),
    };
    ManagedRuntimeEnvPlan {
        runtime: ManagedRuntimeKind::Node,
        platform,
        runtime_root: roots.runtime_root().to_path_buf(),
        distribution_root: roots.distribution_root().to_path_buf(),
        environment_root: roots.environment_root().to_path_buf(),
        distribution_source: ManagedRuntimeRootSource::HostConfigured,
        environment_source: ManagedRuntimeRootSource::HostConfigured,
        runtime_install_root: node_descriptor.install_root,
        runtime_version: "20.0.0".to_string(),
        runtime_executable: node_descriptor.executable,
        runtime_install_manifest_hash: node_descriptor.manifest_hash,
        runtime_executable_hash: node_descriptor.executable_hash,
        package_manager: "pnpm".to_string(),
        package_manager_version: "9.0.0".to_string(),
        package_manager_executable: pnpm_executable,
        package_manager_install_root: pnpm_install_root,
        package_manager_install_manifest_hash: pnpm_manifest_hash,
        package_manager_executable_hash: pnpm_executable_hash,
        managed_runtime_roots: roots,
        package_manifest_path: None,
        lockfile_path: env_dir.join("pnpm-lock.yaml"),
        lock_hash: "lock-hash".to_string(),
        package_manifest_hash: None,
        env_hash: "env-hash".to_string(),
        env_dir,
        expected_marker,
    }
}

/// Publish the exact expected marker for one synthetic managed-environment plan.
/// 为一个合成受管环境计划发布精确预期 marker。
///
/// `plan` supplies both the marker payload and its environment-owned destination path.
/// `plan` 同时提供 marker 载荷及其环境所有的目标路径。
///
/// Return unit after a complete test-fixture write, panicking only on fixture construction failure.
/// 在完整测试夹具写入后返回空值，仅在夹具构造失败时 panic。
fn write_test_managed_env_marker(plan: &ManagedRuntimeEnvPlan) {
    let marker = serde_json::to_vec_pretty(&plan.expected_marker)
        .expect("encode synthetic managed environment marker");
    fs::write(managed_env_marker_path(&plan.env_dir), marker)
        .expect("write synthetic managed environment marker");
}

/// Build one real trusted Skill package context for managed-runtime tests.
/// 为受管运行时测试构造一个真实可信的 Skill 包上下文。
///
/// The runtime_root parameter is the existing runtime root that owns the test package.
/// runtime_root 参数是拥有测试包的现有运行时根目录。
///
/// The package_id parameter is the validated package identifier and directory name.
/// package_id 参数是经过校验的包标识及目录名称。
///
/// Return the canonical package context consumed by production managed-runtime helpers.
/// 返回生产受管运行时辅助函数所消费的规范包上下文。
fn make_test_managed_runtime_package(
    runtime_root: &Path,
    package_id: &str,
) -> Arc<ManagedRuntimePackageContext> {
    // Real package directory required by the trusted package-context constructor.
    // 可信包上下文构造器所需的真实包目录。
    let package_root = runtime_root.join("skills").join(package_id);
    fs::create_dir_all(&package_root).expect("create managed runtime test package root");
    ManagedRuntimePackageContext::for_skill(package_id, &package_root, runtime_root, None)
        .expect("create managed runtime test package context")
}

/// Verify managed runtime status exposes marker parse failures instead of hiding them as not-ready.
/// 验证受管运行时状态会暴露标记解析失败，而不是将其隐藏为未就绪状态。
///
/// This test has no parameters and fails through assertions when marker errors are omitted.
/// 本测试不接收参数；当标记错误被省略时会通过断言失败。
///
/// Return unit after validating the structured status error and cleaning the temporary runtime root.
/// 校验结构化状态错误并清理临时运行时根目录后返回 unit。
#[test]
fn managed_runtime_status_reports_invalid_env_marker_error() {
    // Temporary runtime root used to isolate the damaged marker fixture.
    // 用于隔离损坏标记夹具的临时运行时根目录。
    let runtime_root = make_temp_runtime_root("managed-runtime-invalid-marker");
    // Best-effort cleanup for stale state left by an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = fs::remove_dir_all(&runtime_root);
    // Managed environment directory that owns the marker checked by status.
    // 拥有 status 所检查标记文件的受管环境目录。
    let env_dir = runtime_root.join("dependencies/envs/node/node-20.0.0/env-hash");
    fs::create_dir_all(&env_dir).expect("create managed env dir");
    // Managed runtime plan whose marker path points at the damaged fixture.
    // 标记路径指向损坏夹具的受管运行时计划。
    let plan = make_test_managed_node_env_plan(env_dir.clone());
    // Marker path used by the same readiness helper that production status calls.
    // 生产 status 调用的同一就绪辅助函数使用的标记路径。
    let marker_path = managed_env_marker_path(&env_dir);
    fs::write(&marker_path, "{not-json").expect("write invalid managed env marker");

    // Status object returned by the managed runtime status builder.
    // 受管运行时状态构造器返回的状态对象。
    let status = managed_runtime_status_from_plan(&plan);

    assert_eq!(status["available"], true);
    assert_eq!(status["configured"], true);
    assert_eq!(status["ready"], false);
    assert_eq!(status["runtime"], "node");
    assert_eq!(status["persistent_session"]["supported"], true);
    assert_eq!(
        status["persistent_session"]["target_os"],
        std::env::consts::OS
    );
    assert_eq!(
        status["persistent_session"]["target_arch"],
        std::env::consts::ARCH
    );
    assert_eq!(
        status["message"],
        "managed runtime environment status check failed"
    );
    // Error text preserved from the marker reader for host-visible diagnostics.
    // 从标记读取器保留下来的错误文本，用于宿主可见诊断。
    let error = status["error"]
        .as_str()
        .expect("status should include readiness error");
    assert!(error.contains("Failed to parse"));
    assert!(error.contains(&render_host_visible_path(&marker_path)));

    // Best-effort cleanup for the isolated runtime root created by this test.
    // 清理由本测试创建的隔离运行时根目录。
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Build one stable absolute file path string for payload-validation tests.
/// 为载荷校验测试构造一条稳定绝对文件路径字符串。
fn make_change_set_test_path(file_name: &str) -> String {
    render_host_visible_path(&std::env::temp_dir().join(file_name))
}

/// Build deterministic multi-line delete content for `change_set` lifecycle tests.
/// 为 `change_set` 生命周期测试构造确定性的多行删除内容。
fn make_change_set_delete_content(line_count: usize) -> String {
    (1..=line_count)
        .map(|line_number| format!("deleted line {line_number}"))
        .collect::<Vec<String>>()
        .join("\n")
}

/// Create one minimal runtime directory layout used by skill-config tests.
/// 创建技能配置测试使用的最小运行时目录结构。
fn create_runtime_test_layout(runtime_root: &Path) {
    for relative_path in [
        "skills",
        "temp",
        "resources",
        "lua_packages",
        "bin",
        "libs",
        "system_lua_lib",
    ] {
        fs::create_dir_all(runtime_root.join(relative_path))
            .expect("create runtime test layout path");
    }
}

/// Write one minimal packaged-runtime luaskills-packages metadata tree for runtime validation tests.
/// 为运行时校验测试写入一个最小打包运行时 luaskills-packages 元数据目录树。
fn write_runtime_packages_test_metadata(runtime_root: &Path) {
    let resources_dir = runtime_root.join("resources");
    let packages_root = resources_dir.join("luaskills-packages");
    let help_packages_dir = packages_root.join("help").join("packages");
    let help_modules_dir = packages_root.join("help").join("modules");
    let packages_licenses_dir = runtime_root.join("licenses").join("luaskills-packages");
    fs::create_dir_all(&help_packages_dir).expect("create package help test dir");
    fs::create_dir_all(&help_modules_dir).expect("create module help test dir");
    fs::create_dir_all(&packages_licenses_dir).expect("create package license test dir");

    fs::write(
        resources_dir.join("lua-runtime-manifest.json"),
        "{\n  \"schema_version\": 1,\n  \"layout\": \"luaskills-runtime-v1\"\n}\n",
    )
    .expect("write runtime manifest test file");
    fs::write(
        packages_root.join("lua_packages.txt"),
        "pkg demo-package 0.1.0\n",
    )
    .expect("write package compatibility file");
    fs::write(
        packages_root.join("install-manifest.json"),
        "{\n  \"schema_version\": 1,\n  \"packages\": []\n}\n",
    )
    .expect("write package install manifest");
    fs::write(
            packages_root.join("platform-support.json"),
            "{\n  \"schema_version\": 1,\n  \"supported_targets\": [\"windows-x64\", \"linux-x64\", \"linux-arm64\", \"macos-x64\", \"macos-arm64\"]\n}\n",
        )
        .expect("write package platform support");
    fs::write(
        packages_root.join("THIRD_PARTY_LICENSES.json"),
        "{\n  \"schema_version\": 1,\n  \"luarocks_packages\": []\n}\n",
    )
    .expect("write package third-party licenses");
    fs::write(
        packages_root.join("THIRD_PARTY_NOTICES.md"),
        "# Third-Party Notices\n",
    )
    .expect("write package third-party notices");
    fs::write(
        packages_root.join("help").join("index.json"),
        "{\n  \"schema_version\": 1,\n  \"packages\": [],\n  \"modules\": []\n}\n",
    )
    .expect("write package help index");
    fs::write(
        help_packages_dir.join("demo-package.json"),
        "{\n  \"schema_version\": 1,\n  \"package_name\": \"demo-package\"\n}\n",
    )
    .expect("write package help document");
    fs::write(
        packages_licenses_dir.join("index.json"),
        "{\n  \"schema_version\": 1,\n  \"luarocks_packages\": []\n}\n",
    )
    .expect("write package license index");
    fs::write(
            resources_dir.join("luaskills-packages-manifest.json"),
            "{\n  \"schema_version\": 1,\n  \"layout\": \"luaskills-packages-runtime-v1\",\n  \"paths\": {\n    \"install_manifest\": \"resources/luaskills-packages/install-manifest.json\",\n    \"compat_lua_packages_txt\": \"resources/luaskills-packages/lua_packages.txt\",\n    \"platform_support\": \"resources/luaskills-packages/platform-support.json\",\n    \"third_party_licenses\": \"resources/luaskills-packages/THIRD_PARTY_LICENSES.json\",\n    \"third_party_notices\": \"resources/luaskills-packages/THIRD_PARTY_NOTICES.md\",\n    \"help_index\": \"resources/luaskills-packages/help/index.json\",\n    \"package_help_root\": \"resources/luaskills-packages/help/packages\",\n    \"module_help_root\": \"resources/luaskills-packages/help/modules\",\n    \"license_index\": \"licenses/luaskills-packages/index.json\"\n  }\n}\n",
        )
        .expect("write runtime packages manifest");
}

/// Write one minimal skill fixture that reads one value from `vulcan.config`.
/// 写入一个最小技能夹具，用于从 `vulcan.config` 读取单个值。
fn write_skill_config_test_skill(runtime_root: &Path, skill_id: &str) -> PathBuf {
    let skill_dir = runtime_root.join("skills").join(skill_id);
    fs::create_dir_all(skill_dir.join("runtime")).expect("create config test runtime dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        format!(
            "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nconfig:\n  - key: api_token\n    type: string\n    required: true\n    sensitive: true\n    description: Service access token.\n    constraints:\n      min_length: 1\n      max_length: 4096\nentries:\n  - name: ping\n    description: Config ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
        ),
    )
    .expect("write config test skill yaml");
    fs::write(
        skill_dir.join("runtime").join("ping.lua"),
        "return function(args)\n  if args.action == \"describe\" then\n    return vulcan.json.encode(vulcan.config.describe())\n  end\n  if args.action == \"status\" then\n    return vulcan.json.encode(vulcan.config.status())\n  end\n  if args.action == \"set_undeclared\" then\n    return vulcan.config.set(\"undeclared\", \"blocked\")\n  end\n  if args.action == \"tamper_then_set\" then\n    vulcan.runtime.internal.skill_name = args.target_skill\n    vulcan.config.set(\"api_token\", args.value)\n  end\n  local value = vulcan.config.get(\"api_token\")\n  if value == nil then\n    return \"missing\"\n  end\n  return value\nend\n",
    )
    .expect("write config test runtime entry");
    skill_dir
}

/// Write one config-aware skill fixture that calls another package and then rereads its own config.
/// 写入一个配置感知技能夹具，它会调用另一个技能包并在返回后重新读取自身配置。
///
/// `runtime_root` owns the common test skill root.
/// `runtime_root` 持有公共测试技能根目录。
///
/// `skill_id` identifies the caller package and `target_tool` identifies the nested entry.
/// `skill_id` 标识调用方技能包，`target_tool` 标识嵌套入口。
///
/// Returns the created caller package directory.
/// 返回已创建的调用方技能包目录。
fn write_skill_config_caller_test_skill(
    runtime_root: &Path,
    skill_id: &str,
    target_tool: &str,
) -> PathBuf {
    let skill_dir = runtime_root.join("skills").join(skill_id);
    fs::create_dir_all(skill_dir.join("runtime")).expect("create config caller runtime dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        format!(
        "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nconfig:\n  - key: api_token\n    type: string\n    required: true\n    sensitive: true\n    description: Service access token.\nentries:\n  - name: ping\n    description: Config caller entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
        ),
    )
    .expect("write config caller skill yaml");
    fs::write(
        skill_dir.join("runtime").join("ping.lua"),
        format!(
            "return function(args)\n  local nested = vulcan.call(\"{target_tool}\", {{}})\n  local own = vulcan.config.get(\"api_token\")\n  return nested .. \":\" .. own\nend\n"
        ),
    )
    .expect("write config caller runtime entry");
    skill_dir
}

/// Write one minimal enabled skill fixture into a specific skills root.
/// 将一个最小启用技能夹具写入指定 skills 根目录。
fn write_minimal_skill_to_root(skill_root: &Path, skill_id: &str) -> PathBuf {
    write_minimal_skill_to_root_with_response(skill_root, skill_id, "ok")
}

/// Write one minimal enabled skill fixture with a deterministic response into a specific skills root.
/// 将带有确定响应的最小启用技能夹具写入指定 skills 根目录。
fn write_minimal_skill_to_root_with_response(
    skill_root: &Path,
    skill_id: &str,
    response: &str,
) -> PathBuf {
    let skill_dir = skill_root.join(skill_id);
    fs::create_dir_all(skill_dir.join("runtime")).expect("create minimal skill runtime dir");
    fs::write(
            skill_dir.join("skill.yaml"),
            format!(
                "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nentries:\n  - name: ping\n    description: Minimal ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
            ),
        )
        .expect("write minimal skill yaml");
    fs::write(
        skill_dir.join("runtime").join("ping.lua"),
        format!("return function(args)\n  return '{response}'\nend\n"),
    )
    .expect("write minimal skill runtime entry");
    skill_dir
}

/// Write one model-capability test skill with caller-provided Lua source.
/// 写入一个使用调用方提供 Lua 源码的模型能力测试 skill。
fn write_model_test_skill_to_root(skill_root: &Path, skill_id: &str, lua_source: &str) -> PathBuf {
    let skill_dir = skill_root.join(skill_id);
    fs::create_dir_all(skill_dir.join("runtime")).expect("create model test skill runtime dir");
    fs::write(
            skill_dir.join("skill.yaml"),
            format!(
                "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nentries:\n  - name: ping\n    description: Model test entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
            ),
        )
        .expect("write model test skill yaml");
    fs::write(skill_dir.join("runtime").join("ping.lua"), lua_source)
        .expect("write model test runtime entry");
    skill_dir
}

/// Write one skill fixture whose final AI-facing input schema comes from one external JSON file.
/// 写入一个最终面向 AI 输入 schema 来自外部 JSON 文件的技能夹具。
fn write_schema_file_skill_to_root(skill_root: &Path, skill_id: &str) -> PathBuf {
    let skill_dir = skill_root.join(skill_id);
    fs::create_dir_all(skill_dir.join("runtime")).expect("create schema skill runtime dir");
    fs::create_dir_all(skill_dir.join("schemas")).expect("create schema skill schema dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        format!(
            "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nentries:\n  - name: inspect\n    description: Schema file entry.\n    lua_entry: runtime/inspect.lua\n    lua_module: {skill_id}.inspect\n    input_schema_file: schemas/inspect.input.schema.json\n"
        ),
    )
    .expect("write schema skill yaml");
    fs::write(
        skill_dir.join("schemas").join("inspect.input.schema.json"),
        serde_json::to_string_pretty(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "nodes": {
                    "type": "array",
                    "description": "Node selector list.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "file": { "type": "string" },
                            "structural_path": { "type": "string" }
                        },
                        "required": ["file", "structural_path"]
                    }
                },
                "strict": {
                    "type": "boolean",
                    "description": "Enable strict validation."
                }
            },
            "required": ["nodes"]
        }))
        .expect("serialize schema skill input schema"),
    )
    .expect("write schema skill input schema");
    fs::write(
        skill_dir.join("runtime").join("inspect.lua"),
        "return function(args)\n  return 'schema-ok'\nend\n",
    )
    .expect("write schema skill runtime entry");
    skill_dir
}

/// Verify runtime entry export carries the resolved external JSON input schema and derived parameters.
/// 验证运行时入口导出会携带已解析的外部 JSON 输入 schema 与推导出的参数列表。
#[test]
fn list_entries_exposes_resolved_entry_input_schema() {
    let runtime_root = make_temp_runtime_root("entry-input-schema-export");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_schema_file_skill_to_root(&runtime_root.join("skills"), "demo-schema-skill");

    let mut engine = LuaEngine::new(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        LuaRuntimeHostOptions {
            runtime_root: None,
            temp_dir: Some(runtime_root.join("temp")),
            resources_dir: Some(runtime_root.join("resources")),
            lua_packages_dir: Some(runtime_root.join("lua_packages")),
            host_provided_tool_root: Some(runtime_root.join("bin").join("tools")),
            host_provided_lua_root: Some(runtime_root.join("lua_packages")),
            host_provided_ffi_root: Some(runtime_root.join("libs")),
            download_cache_root: Some(runtime_root.join("temp").join("downloads")),
            ..LuaRuntimeHostOptions::default()
        },
    ))
    .expect("create engine for schema export test");
    engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load schema export test root");

    // Runtime entries exported through the public listing API after schema resolution.
    // 在 schema 解析后通过公开列表 API 导出的运行时入口。
    let entries = engine.list_entries().expect("list schema export entries");
    let entry = entries
        .iter()
        .find(|item| item.local_name == "inspect")
        .expect("inspect entry");
    assert_eq!(entry.input_schema["type"], "object");
    assert_eq!(entry.input_schema["required"], json!(["nodes"]));
    assert_eq!(entry.input_schema["properties"]["nodes"]["type"], "array");
    assert_eq!(
        entry.input_schema["properties"]["nodes"]["items"]["properties"]["file"]["type"],
        "string"
    );
    assert_eq!(entry.parameters.len(), 2);
    assert_eq!(entry.parameters[0].name, "nodes");
    assert_eq!(entry.parameters[0].param_type, "array");
    assert!(entry.parameters[0].required);
    assert_eq!(entry.parameters[1].name, "strict");
    assert_eq!(entry.parameters[1].param_type, "boolean");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the canonical `change_set` validator accepts explicit AI-oriented modify hunks and file lifecycle records.
/// 验证 canonical `change_set` 校验器会接受面向 AI 的显式 modify hunk 与文件生命周期记录。
#[test]
fn validate_change_set_payload_accepts_hunks_and_file_lifecycle_changes() {
    let modify_path = make_change_set_test_path("luaskills_change_set_modify.lua");
    let create_path = make_change_set_test_path("luaskills_change_set_create.lua");
    let delete_path = make_change_set_test_path("luaskills_change_set_delete.lua");
    let rename_old_path = make_change_set_test_path("luaskills_change_set_old.lua");
    let rename_new_path = make_change_set_test_path("luaskills_change_set_new.lua");
    let payload = json!({
        "mode": "applied",
        "summary": "Updated one file and lifecycle metadata.",
        "files": [
            {
                "change": "modify",
                "path": modify_path,
                "hunks": [
                    {
                        "before": "local a = 1\nlocal b = 2",
                        "delete": [
                            { "line": 10, "content": "local x = 1" },
                            { "line": 11, "content": "return x" }
                        ],
                        "insert": [
                            { "line": 10, "content": "local x = 2" },
                            { "line": 11, "content": "local y = 3" },
                            { "line": 12, "content": "return x + y" }
                        ],
                        "after": "end\nreturn M"
                    }
                ]
            },
            {
                "change": "create",
                "path": create_path,
                "content": "local M = {}\nreturn M\n"
            },
            {
                "change": "delete",
                "path": delete_path,
                "content": "return legacy\n"
            },
            {
                "change": "rename",
                "old_path": rename_old_path,
                "new_path": rename_new_path
            }
        ]
    });

    validate_change_set_payload("demo.skill", &payload)
        .expect("change_set payload should be accepted");
}

/// Verify legacy delete records are normalized into explicit full mode with one computed total line count.
/// 验证旧版 delete 记录会被归一化为显式全文模式并补齐总行数。
#[test]
fn normalize_change_set_payload_expands_delete_full_mode_and_total_line_count() {
    let delete_path = make_change_set_test_path("luaskills_change_set_delete_full.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "delete",
                "path": delete_path,
                "content": "alpha\nbeta\ngamma\n"
            }
        ]
    });

    let normalized = normalize_change_set_payload(payload);
    assert_eq!(
        normalized["files"][0]["content_mode"],
        Value::String("full".to_string())
    );
    assert_eq!(
        normalized["files"][0]["total_line_count"],
        Value::Number(serde_json::Number::from(3_u64))
    );
    assert_eq!(normalized["files"][0]["content"], "alpha\nbeta\ngamma\n");
}

/// Verify oversized delete records are forcibly converted into truncated mode with head and tail snippets.
/// 验证超大 delete 记录会被强制转换为截断模式，并输出前后片段。
#[test]
fn normalize_change_set_payload_truncates_large_delete_content() {
    let delete_path = make_change_set_test_path("luaskills_change_set_delete_large.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "delete",
                "path": delete_path,
                "content": make_change_set_delete_content(520)
            }
        ]
    });

    let normalized = normalize_change_set_payload(payload);
    assert_eq!(
        normalized["files"][0]["content_mode"],
        Value::String("truncated".to_string())
    );
    assert_eq!(
        normalized["files"][0]["total_line_count"],
        Value::Number(serde_json::Number::from(520_u64))
    );
    assert!(normalized["files"][0].get("content").is_none());
    assert_eq!(
        normalized["files"][0]["content_head"],
        Value::String(make_change_set_delete_content(50))
    );
    assert_eq!(
        normalized["files"][0]["content_tail"],
        Value::String(
            (471..=520)
                .map(|line_number| format!("deleted line {line_number}"))
                .collect::<Vec<String>>()
                .join("\n")
        )
    );
}

/// Verify canonical validation accepts explicit truncated delete records when they carry line-count metadata and both snippets.
/// 验证 canonical 校验会接受带总行数与前后片段的显式截断 delete 记录。
#[test]
fn validate_change_set_payload_accepts_truncated_delete_records() {
    let delete_path = make_change_set_test_path("luaskills_change_set_delete_truncated.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "delete",
                "path": delete_path,
                "content_mode": "truncated",
                "total_line_count": 520,
                "content_head": make_change_set_delete_content(50),
                "content_tail": (471..=520)
                    .map(|line_number| format!("deleted line {line_number}"))
                    .collect::<Vec<String>>()
                    .join("\n")
            }
        ]
    });

    validate_change_set_payload("demo.skill", &payload)
        .expect("truncated delete payload should be accepted");
}

/// Verify explicit truncated delete records must expose the total deleted line count when full content is omitted.
/// 验证显式截断 delete 记录在省略全文时必须暴露删除总行数。
#[test]
fn validate_change_set_payload_rejects_truncated_delete_without_total_line_count() {
    let delete_path =
        make_change_set_test_path("luaskills_change_set_delete_truncated_missing_total.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "delete",
                "path": delete_path,
                "content_mode": "truncated",
                "content_head": "line 1\nline 2",
                "content_tail": "line 519\nline 520"
            }
        ]
    });

    let error = validate_change_set_payload("demo.skill", &payload)
        .expect_err("truncated delete payload should require total_line_count");
    assert!(error.contains("change_set.files[0].total_line_count"));
}

/// Verify modify file records must carry at least one non-empty hunk list.
/// 验证 modify 文件记录必须携带至少一个非空 hunk 列表。
#[test]
fn validate_change_set_payload_rejects_modify_without_hunks() {
    let modify_path = make_change_set_test_path("luaskills_change_set_modify_missing_hunks.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "modify",
                "path": modify_path
            }
        ]
    });

    let error = validate_change_set_payload("demo.skill", &payload)
        .expect_err("modify file record should require hunks");
    assert!(error.contains("change_set.files[0].hunks"));
}

/// Verify modify hunks must carry at least one deleted or inserted line block.
/// 验证 modify hunk 必须至少携带一组删除或插入行块。
#[test]
fn validate_change_set_payload_rejects_empty_modify_hunk() {
    let modify_path = make_change_set_test_path("luaskills_change_set_modify_empty_hunk.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "modify",
                "path": modify_path,
                "hunks": [
                    {
                        "before": "",
                        "delete": [],
                        "insert": [],
                        "after": ""
                    }
                ]
            }
        ]
    });

    let error = validate_change_set_payload("demo.skill", &payload)
        .expect_err("modify hunk should require deleted or inserted lines");
    assert!(error.contains("must include at least one deleted or inserted line"));
}

/// Verify rename records must expose both old and new absolute file paths.
/// 验证 rename 记录必须同时暴露旧绝对路径与新绝对路径。
#[test]
fn validate_change_set_payload_rejects_rename_without_both_paths() {
    let rename_old_path = make_change_set_test_path("luaskills_change_set_old_only.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "rename",
                "old_path": rename_old_path
            }
        ]
    });

    let error = validate_change_set_payload("demo.skill", &payload)
        .expect_err("rename record should require both old_path and new_path");
    assert!(error.contains("change_set.files[0].new_path"));
}

/// Verify modify line blocks must keep ascending line numbers so hosts and models can replay them deterministically.
/// 验证 modify 行块必须保持递增行号，确保宿主与模型可以确定性回放。
#[test]
fn validate_change_set_payload_rejects_out_of_order_hunk_lines() {
    let modify_path = make_change_set_test_path("luaskills_change_set_modify_unordered_lines.lua");
    let payload = json!({
        "mode": "applied",
        "files": [
            {
                "change": "modify",
                "path": modify_path,
                "hunks": [
                    {
                        "before": "local a = 1",
                        "delete": [
                            { "line": 11, "content": "return x" },
                            { "line": 10, "content": "local x = 1" }
                        ],
                        "insert": [],
                        "after": "return M"
                    }
                ]
            }
        ]
    });

    let error = validate_change_set_payload("demo.skill", &payload)
        .expect_err("modify hunk line numbers should be strictly increasing");
    assert!(error.contains("line numbers must be strictly increasing"));
}

/// Verify ROOT keeps priority over PROJECT and USER for identical skill ids.
/// 验证 ROOT 对同名 skill 始终高于 PROJECT 与 USER。
#[test]
fn load_from_roots_keeps_root_priority_over_project_and_user() {
    let runtime_root = make_temp_runtime_root("formal-root-load-priority");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let project_root = RuntimeSkillRoot {
        name: "PROJECT".to_string(),
        skills_dir: runtime_root.join("project_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    write_minimal_skill_to_root_with_response(&root_root.skills_dir, "vulcan-codekit", "root");
    write_minimal_skill_to_root_with_response(
        &project_root.skills_dir,
        "vulcan-codekit",
        "project",
    );
    write_minimal_skill_to_root_with_response(&user_root.skills_dir, "vulcan-codekit", "user");
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[root_root, project_root, user_root])
        .expect("formal root chain should load");

    let result = engine
        .call_skill("vulcan-codekit-ping", &json!({}), None)
        .expect("call root-priority skill");
    assert_eq!(result.content, "root");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify `load_from_roots` reports skill-root probe errors instead of treating them as empty roots.
/// 验证 `load_from_roots` 会报告技能根探测错误，而不是把它们当作空根目录。
#[test]
fn load_from_roots_rejects_skill_root_probe_errors() {
    // Valid host resource root used so packaged-runtime marker probing does not mask the skill-root failure.
    // 使用有效的宿主 resources 根目录，避免打包运行时标记探测掩盖技能根失败。
    let runtime_root = make_temp_runtime_root("skill-root-probe-error");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    // Engine whose packaged-runtime resource probe resolves against a valid missing path.
    // 打包运行时 resources 探测会落在有效缺失路径上的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    // Skill root containing one embedded NUL that filesystem metadata cannot inspect.
    // 包含内嵌 NUL 的技能根路径，文件系统元数据无法探测该路径。
    let invalid_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: PathBuf::from("invalid\0skills"),
    };

    // Error returned before the invalid root can behave like a confirmed missing root.
    // 在非法根表现得像确认缺失根之前返回的错误。
    let error_text = engine
        .load_from_roots(&[invalid_root])
        .expect_err("skill-root metadata probe error should fail")
        .to_string();

    assert!(
        error_text.contains("failed to inspect skill root 'ROOT'"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains("invalid"),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify `load_from_roots` rejects configured skill roots that exist as non-directory files.
/// 验证 `load_from_roots` 会拒绝以非目录文件形式存在的已配置技能根。
#[test]
fn load_from_roots_rejects_file_skill_root() {
    // Runtime root that isolates the file skill-root fixture.
    // 隔离文件型技能根夹具的运行时根目录。
    let runtime_root = make_temp_runtime_root("skill-root-file");
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    // File deliberately occupying the configured ROOT skill-root path.
    // 故意占用已配置 ROOT 技能根路径的文件。
    let file_root = runtime_root.join("skills");
    fs::write(&file_root, "not a directory\n").expect("write file skill root");
    // Engine whose packaged-runtime resource probe resolves against a valid missing path.
    // 打包运行时 resources 探测会落在有效缺失路径上的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });

    // Error returned before the file root can fall through to skill instance collection.
    // 在文件型根继续进入 skill 实例收集之前返回的错误。
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: file_root.clone(),
        }])
        .expect_err("file skill root should fail")
        .to_string();

    assert!(
        error_text.contains("skill root 'ROOT' is not a directory"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains(&render_host_visible_path(&file_root)),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify one packaged runtime loads successfully when the embedded luaskills-packages metadata tree is complete.
/// 验证在内嵌 luaskills-packages 元数据目录树完整时，一个打包运行时能够成功加载。
#[test]
fn load_from_roots_accepts_packaged_runtime_with_packages_metadata() {
    let runtime_root = make_temp_runtime_root("packaged-runtime-packages-ok");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_runtime_packages_test_metadata(&runtime_root);
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-packaged-skill");

    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("packaged runtime with package metadata should load");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify one packaged runtime fails with a clear error when the top-level luaskills-packages manifest is missing.
/// 验证当顶层 luaskills-packages 清单缺失时，一个打包运行时会给出清晰错误并加载失败。
#[test]
fn load_from_roots_rejects_packaged_runtime_without_packages_manifest() {
    let runtime_root = make_temp_runtime_root("packaged-runtime-missing-manifest");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    fs::write(
        runtime_root
            .join("resources")
            .join("lua-runtime-manifest.json"),
        "{\n  \"schema_version\": 1,\n  \"layout\": \"luaskills-runtime-v1\"\n}\n",
    )
    .expect("write runtime manifest trigger file");
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-missing-manifest");

    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime without package manifest should fail")
        .to_string();
    assert!(
        error_text.contains("luaskills-packages-manifest.json"),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify packaged runtime marker probe errors fail instead of disabling package validation.
/// 验证打包运行时标记探测错误会失败，而不是关闭包校验。
///
/// This test has no parameters and fails through assertions when marker path errors are hidden.
/// 本测试不接收参数；当标记路径错误被隐藏时会通过断言失败。
///
/// Return unit after validating the load path reports a packaged-runtime inspection diagnostic.
/// 校验加载路径会报告打包运行时探测诊断后返回 unit。
#[test]
fn load_from_roots_rejects_packaged_runtime_marker_probe_errors() {
    // Runtime root that contains a valid skill so only the resources path is invalid.
    // 包含有效 skill 的运行时根目录，确保只有 resources 路径非法。
    let runtime_root = make_temp_runtime_root("packaged-runtime-marker-probe-error");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-marker-probe-error");

    // Host-provided resources path containing an embedded NUL that cannot be inspected by filesystem metadata.
    // 包含内嵌 NUL 的宿主 resources 路径，文件系统元数据无法探测该路径。
    let invalid_resources_dir = runtime_root.join("resources\0invalid");
    // Engine that validates the invalid packaged-runtime resources path during root loading.
    // 在根目录加载期间校验非法打包运行时 resources 路径的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(invalid_resources_dir),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    // Error returned before the marker probe failure can look like an absent packaged-runtime marker.
    // 在标记探测失败表现得像打包运行时标记缺失之前返回的错误。
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime marker probe error should fail")
        .to_string();
    assert!(
        error_text.contains("failed to inspect lua-runtime-manifest"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains("lua-runtime-manifest.json"),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify a packaged runtime marker path must be a file instead of any existing filesystem entry.
/// 验证打包运行时标记路径必须是文件，而不是任意已存在的文件系统条目。
#[test]
fn load_from_roots_rejects_packaged_runtime_directory_marker_file() {
    // Runtime root that isolates the directory marker fixture.
    // 隔离目录标记夹具的运行时根目录。
    let runtime_root = make_temp_runtime_root("packaged-runtime-directory-marker");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    // Directory occupying the runtime marker file path.
    // 占用运行时标记文件路径的目录。
    let marker_dir = runtime_root
        .join("resources")
        .join("lua-runtime-manifest.json");
    fs::create_dir_all(&marker_dir).expect("create directory runtime marker");
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-directory-marker");

    // Engine that validates packaged-runtime resources during root loading.
    // 在根目录加载期间校验打包运行时资源的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    // Error returned before a directory marker can trigger package layout validation.
    // 在目录标记触发布局校验之前返回的错误。
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime directory marker should fail")
        .to_string();
    assert!(
        error_text.contains("lua-runtime-manifest is not a file"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains(&render_host_visible_path(&marker_dir)),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify a packaged runtime packages manifest path must be a file before JSON reading.
/// 验证打包运行时 packages 清单路径必须在 JSON 读取前就是文件。
#[test]
fn load_from_roots_rejects_packaged_runtime_directory_packages_manifest() {
    // Runtime root that isolates the directory packages manifest fixture.
    // 隔离目录 packages 清单夹具的运行时根目录。
    let runtime_root = make_temp_runtime_root("packaged-runtime-directory-manifest");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    fs::write(
        runtime_root
            .join("resources")
            .join("lua-runtime-manifest.json"),
        "{\n  \"schema_version\": 1,\n  \"layout\": \"luaskills-runtime-v1\"\n}\n",
    )
    .expect("write runtime manifest trigger file");
    // Directory occupying the required packages manifest file path.
    // 占用必需 packages 清单文件路径的目录。
    let packages_manifest_dir = runtime_root
        .join("resources")
        .join("luaskills-packages-manifest.json");
    fs::create_dir_all(&packages_manifest_dir).expect("create directory packages manifest");
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-directory-manifest");

    // Engine that validates packaged-runtime resources during root loading.
    // 在根目录加载期间校验打包运行时资源的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    // Error returned before a directory manifest can fall through to JSON reading.
    // 在目录清单继续进入 JSON 读取之前返回的错误。
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime directory manifest should fail")
        .to_string();
    assert!(
        error_text.contains("luaskills-packages-manifest is not a file"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains(&render_host_visible_path(&packages_manifest_dir)),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify one packaged runtime fails with a clear error when one manifest-declared packages file is missing.
/// 验证当清单声明的某个 packages 文件缺失时，一个打包运行时会给出清晰错误并加载失败。
#[test]
fn load_from_roots_rejects_packaged_runtime_when_declared_packages_file_is_missing() {
    let runtime_root = make_temp_runtime_root("packaged-runtime-missing-help-index");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_runtime_packages_test_metadata(&runtime_root);
    fs::remove_file(
        runtime_root
            .join("resources")
            .join("luaskills-packages")
            .join("help")
            .join("index.json"),
    )
    .expect("remove package help index");
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-missing-help-index");

    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime with missing declared file should fail")
        .to_string();
    assert!(
        error_text.contains("luaskills-packages\\help\\index.json")
            || error_text.contains("luaskills-packages/help/index.json"),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify a manifest-declared packaged-runtime file target cannot be satisfied by a directory.
/// 验证清单声明的打包运行时文件目标不能由目录满足。
#[test]
fn load_from_roots_rejects_packaged_runtime_declared_file_as_directory() {
    // Runtime root that isolates the declared file-as-directory fixture.
    // 隔离声明文件被目录占位夹具的运行时根目录。
    let runtime_root = make_temp_runtime_root("packaged-runtime-file-as-directory");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_runtime_packages_test_metadata(&runtime_root);
    // Manifest-declared help index file path intentionally replaced by a directory.
    // 清单声明的帮助索引文件路径被有意替换为目录。
    let help_index_dir = runtime_root.join("resources/luaskills-packages/help/index.json");
    fs::remove_file(&help_index_dir).expect("remove help index file");
    fs::create_dir_all(&help_index_dir).expect("create directory help index");
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-file-as-directory");

    // Engine that validates packaged-runtime resources during root loading.
    // 在根目录加载期间校验打包运行时资源的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    // Error returned before the directory can satisfy a file contract.
    // 在目录满足文件契约之前返回的错误。
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime declared file directory should fail")
        .to_string();
    assert!(
        error_text.contains("help_index is not a file"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains(&render_host_visible_path(&help_index_dir)),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify a manifest-declared packaged-runtime directory target cannot be satisfied by a file.
/// 验证清单声明的打包运行时目录目标不能由文件满足。
#[test]
fn load_from_roots_rejects_packaged_runtime_declared_directory_as_file() {
    // Runtime root that isolates the declared directory-as-file fixture.
    // 隔离声明目录被文件占位夹具的运行时根目录。
    let runtime_root = make_temp_runtime_root("packaged-runtime-directory-as-file");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_runtime_packages_test_metadata(&runtime_root);
    // Manifest-declared package help root intentionally replaced by a file.
    // 清单声明的包帮助根目录被有意替换为文件。
    let package_help_root_file = runtime_root.join("resources/luaskills-packages/help/packages");
    fs::remove_dir_all(&package_help_root_file).expect("remove package help root directory");
    fs::write(&package_help_root_file, "not a directory\n").expect("write package help root file");
    write_minimal_skill_to_root(&runtime_root.join("skills"), "demo-directory-as-file");

    // Engine that validates packaged-runtime resources during root loading.
    // 在根目录加载期间校验打包运行时资源的引擎。
    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        resources_dir: Some(runtime_root.join("resources")),
        lua_packages_dir: Some(runtime_root.join("lua_packages")),
        host_provided_lua_root: Some(runtime_root.join("lua_packages")),
        ..Default::default()
    });
    // Error returned before the file can satisfy a directory contract.
    // 在文件满足目录契约之前返回的错误。
    let error_text = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect_err("packaged runtime declared directory file should fail")
        .to_string();
    assert!(
        error_text.contains("package_help_root is not a directory"),
        "unexpected error text: {}",
        error_text
    );
    assert!(
        error_text.contains(&render_host_visible_path(&package_help_root_file)),
        "unexpected error text: {}",
        error_text
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify delegated query helpers hide ROOT-owned metadata while runtime calls still use active skills.
/// 验证委托查询辅助函数会隐藏 ROOT 元数据，同时运行时调用仍使用已激活技能。
#[test]
fn delegated_authority_query_helpers_hide_root_skills() {
    let runtime_root = make_temp_runtime_root("delegated-query-hides-root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: " root ".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    write_minimal_skill_to_root(&root_root.skills_dir, "vulcan-root-skill");
    write_minimal_skill_to_root(&user_root.skills_dir, "vulcan-user-skill");
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[root_root, user_root])
        .expect("root and user runtime should load");

    // System-visible entries that should include ROOT-owned runtime entries.
    // 系统权限可见的入口，应包含 ROOT 拥有的运行时入口。
    let system_entries = engine
        .list_entries_for_authority(SkillManagementAuthority::System)
        .expect("list system-visible entries");
    // Delegated-visible entries that should hide ROOT-owned runtime entries.
    // 委托权限可见的入口，应隐藏 ROOT 拥有的运行时入口。
    let delegated_entries = engine
        .list_entries_for_authority(SkillManagementAuthority::DelegatedTool)
        .expect("list delegated-visible entries");
    assert!(
        system_entries
            .iter()
            .any(|entry| entry.root_name == " root ")
    );
    assert!(
        delegated_entries
            .iter()
            .all(|entry| !entry.root_name.trim().eq_ignore_ascii_case("ROOT"))
    );

    // System-visible help trees that should include ROOT-owned runtime help.
    // 系统权限可见的帮助树，应包含 ROOT 拥有的运行时帮助。
    let system_help = engine
        .list_skill_help_for_authority(SkillManagementAuthority::System)
        .expect("list system-visible help");
    // Delegated-visible help trees that should hide ROOT-owned runtime help.
    // 委托权限可见的帮助树，应隐藏 ROOT 拥有的运行时帮助。
    let delegated_help = engine
        .list_skill_help_for_authority(SkillManagementAuthority::DelegatedTool)
        .expect("list delegated-visible help");
    assert!(system_help.iter().any(|help| help.root_name == " root "));
    assert!(
        delegated_help
            .iter()
            .all(|help| !help.root_name.trim().eq_ignore_ascii_case("ROOT"))
    );

    let delegated_detail = engine
        .render_skill_help_detail_for_authority(
            SkillManagementAuthority::DelegatedTool,
            "vulcan-root-skill",
            "main",
            None,
        )
        .expect("delegated detail should be filtered");
    assert!(delegated_detail.is_none());

    let root_call = engine
        .call_skill("vulcan-root-skill-ping", &json!({}), None)
        .expect("runtime call should reach any active skill");
    assert_eq!(root_call.content, "ok");

    let root_run_lua = engine
        .run_lua(
            "return vulcan.call('vulcan-root-skill-ping', {})",
            &json!({}),
            None,
        )
        .expect("runtime Lua execution should use the active runtime view");
    assert_eq!(root_run_lua, json!("ok"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify authority-scoped entry queries reject registry targets whose owning skill disappeared.
/// 验证带权限作用域的入口查询会拒绝所属 skill 已消失的注册表目标。
///
/// This test has no parameters and fails through assertions when stale targets are hidden as misses.
/// 本测试不接收参数；当失效目标被隐藏成未命中时会通过断言失败。
///
/// Return unit after validating missing-tool and stale-target query behavior.
/// 校验未注册工具与失效目标查询行为后返回 unit。
#[test]
fn authority_entry_queries_reject_stale_registry_targets() {
    // Minimal engine with no loaded skills and an initially empty entry registry.
    // 没有已加载 skill 且入口注册表初始为空的最小引擎。
    let mut engine = make_test_engine(HashMap::new());

    assert!(
        !engine
            .is_skill_for_authority(SkillManagementAuthority::DelegatedTool, "missing-tool")
            .expect("missing tool should query successfully")
    );
    assert_eq!(
        engine
            .skill_name_for_tool_for_authority(
                SkillManagementAuthority::DelegatedTool,
                "missing-tool",
            )
            .expect("missing tool skill name query should succeed"),
        None
    );

    // Registry target whose storage key does not exist in `engine.skills`.
    // 存储键不存在于 `engine.skills` 中的注册表目标。
    let stale_target = ResolvedEntryTarget {
        canonical_name: "ghost-skill-ping".to_string(),
        skill_storage_key: "missing-storage-key".to_string(),
        skill_id: "ghost-skill".to_string(),
        local_name: "ping".to_string(),
    };
    engine
        .entry_registry
        .insert(stale_target.canonical_name.clone(), stale_target);

    // Error returned by the visibility query instead of silently hiding the stale target.
    // 可见性查询返回的错误，而不是静默隐藏失效目标。
    let is_skill_error = engine
        .is_skill_for_authority(SkillManagementAuthority::DelegatedTool, "ghost-skill-ping")
        .expect_err("stale registry target should fail is_skill query");
    assert!(is_skill_error.contains("ghost-skill-ping"));
    assert!(is_skill_error.contains("missing-storage-key"));

    // Error returned by the owner-name query for the same stale target.
    // 针对同一失效目标的所属名称查询返回的错误。
    let skill_name_error = engine
        .skill_name_for_tool_for_authority(
            SkillManagementAuthority::DelegatedTool,
            "ghost-skill-ping",
        )
        .expect_err("stale registry target should fail skill-name query");
    assert!(skill_name_error.contains("ghost-skill-ping"));
    assert!(skill_name_error.contains("missing-storage-key"));
}

/// Verify runtime entry listing rejects stale registry targets instead of skipping descriptors.
/// 验证运行时入口列表会拒绝失效注册表目标，而不是跳过描述符。
///
/// This test has no parameters and fails through assertions when stale targets are hidden from listings.
/// 本测试不接收参数；当失效目标从列表中被隐藏时会通过断言失败。
///
/// Return unit after validating missing-skill and missing-local-entry diagnostics.
/// 校验缺失 skill 与缺失局部入口诊断后返回 unit。
#[test]
fn list_entries_rejects_stale_registry_targets() {
    // Minimal engine with no loaded skills and an initially empty entry registry.
    // 没有已加载 skill 且入口注册表初始为空的最小引擎。
    let mut missing_skill_engine = make_test_engine(HashMap::new());
    // Registry target whose storage key does not exist in the engine skill map.
    // 存储键不存在于引擎 skill 映射中的注册表目标。
    let missing_skill_target = ResolvedEntryTarget {
        canonical_name: "ghost-skill-ping".to_string(),
        skill_storage_key: "missing-storage-key".to_string(),
        skill_id: "ghost-skill".to_string(),
        local_name: "ping".to_string(),
    };
    missing_skill_engine.entry_registry.insert(
        missing_skill_target.canonical_name.clone(),
        missing_skill_target,
    );

    // Error returned before a missing-skill target can disappear from the entry listing.
    // 在缺失 skill 的目标从入口列表中消失前返回的错误。
    let missing_skill_error = missing_skill_engine
        .list_entries()
        .expect_err("missing skill target should fail entry listing");
    assert!(missing_skill_error.contains("ghost-skill-ping"));
    assert!(missing_skill_error.contains("missing-storage-key"));

    // Skill map containing one loaded skill whose manifest exposes only the `ping` entry.
    // 包含一个已加载 skill 的映射，该 skill 的 manifest 只暴露 `ping` 入口。
    let mut skills = HashMap::new();
    skills.insert(
        "alpha-storage".to_string(),
        make_loaded_skill("alpha", "alpha-skill", "ping", "alpha_module"),
    );
    // Engine whose registry target points at a missing local entry on the loaded skill.
    // 注册表目标指向已加载 skill 上缺失局部入口的引擎。
    let mut missing_entry_engine = make_test_engine(skills);
    // Registry target whose owning skill exists but whose local entry no longer exists.
    // 所属 skill 存在但局部入口已不存在的注册表目标。
    let missing_entry_target = ResolvedEntryTarget {
        canonical_name: "alpha-skill-missing".to_string(),
        skill_storage_key: "alpha-storage".to_string(),
        skill_id: "alpha-skill".to_string(),
        local_name: "missing".to_string(),
    };
    missing_entry_engine.entry_registry.insert(
        missing_entry_target.canonical_name.clone(),
        missing_entry_target,
    );

    // Error returned before a missing-local-entry target can disappear from the entry listing.
    // 在缺失局部入口的目标从入口列表中消失前返回的错误。
    let missing_entry_error = missing_entry_engine
        .list_entries()
        .expect_err("missing local entry target should fail entry listing");
    assert!(missing_entry_error.contains("alpha-skill-missing"));
    assert!(missing_entry_error.contains("missing"));
    assert!(missing_entry_error.contains("alpha-skill"));
}

/// Verify help listing rejects entries that lost their resolved canonical names.
/// 验证帮助列表会拒绝丢失已解析 canonical 名称的入口。
///
/// This test has no parameters and fails through assertions when related entries are silently omitted.
/// 本测试不接收参数；当关联入口被静默省略时会通过断言失败。
///
/// Return unit after validating the unresolved related-entry diagnostic.
/// 校验未解析关联入口诊断后返回 unit。
#[test]
fn list_skill_help_rejects_unresolved_related_entries() {
    // Skill map containing one loaded skill whose resolved-entry mapping is intentionally empty.
    // 包含一个已加载 skill 的映射，其已解析入口映射被故意保持为空。
    let mut skills = HashMap::new();
    skills.insert(
        "alpha-storage".to_string(),
        make_loaded_skill("alpha", "alpha-skill", "ping", "alpha_module"),
    );
    // Engine whose help list must detect the missing canonical entry mapping.
    // 帮助列表必须检测缺失 canonical 入口映射的引擎。
    let engine = make_test_engine(skills);

    // Error returned before the help related-entry list can silently drop `ping`.
    // 在帮助关联入口列表静默丢弃 `ping` 前返回的错误。
    let error = engine
        .list_skill_help()
        .expect_err("unresolved related entry should fail help listing");
    assert!(error.contains("main"));
    assert!(error.contains("alpha-skill"));
    assert!(error.contains("ping"));
}

/// Verify `vulcan.call` dispatch building rejects stale registry targets instead of skipping them.
/// 验证 `vulcan.call` 分发构建会拒绝失效注册表目标，而不是跳过它们。
///
/// This test has no parameters and fails through assertions when dispatch entries silently drop stale targets.
/// 本测试不接收参数；当分发入口静默丢弃失效目标时会通过断言失败。
///
/// Return unit after validating missing-skill and missing-local-entry diagnostics.
/// 校验缺失 skill 与缺失局部入口诊断后返回 unit。
#[test]
fn vulcan_call_dispatch_build_rejects_stale_registry_targets() {
    // Skill map with one loaded skill used by the valid dispatch entry.
    // 包含一个已加载 skill 的技能映射，用于构造有效分发入口。
    let mut skills = HashMap::new();
    skills.insert(
        "alpha-storage".to_string(),
        make_loaded_skill("alpha", "alpha-skill", "ping", "alpha_module"),
    );

    // Registry containing one target whose owning skill storage key is absent.
    // 包含一个所属 skill 存储键缺失目标的注册表。
    let mut missing_skill_registry = BTreeMap::new();
    missing_skill_registry.insert(
        "ghost-skill-ping".to_string(),
        ResolvedEntryTarget {
            canonical_name: "ghost-skill-ping".to_string(),
            skill_storage_key: "missing-storage-key".to_string(),
            skill_id: "ghost-skill".to_string(),
            local_name: "ping".to_string(),
        },
    );

    // Error returned before a stale missing-skill target can disappear from `vulcan.call`.
    // 在缺失 skill 的失效目标从 `vulcan.call` 中消失前返回的错误。
    let missing_skill_error = build_lua_call_dispatch_entries(&skills, &missing_skill_registry)
        .err()
        .expect("missing skill target should fail dispatch build");
    assert!(missing_skill_error.contains("ghost-skill-ping"));
    assert!(missing_skill_error.contains("missing-storage-key"));

    // Registry containing one target whose local entry no longer exists in the loaded skill.
    // 包含一个局部入口已不在已加载 skill 中存在的目标注册表。
    let mut missing_entry_registry = BTreeMap::new();
    missing_entry_registry.insert(
        "alpha-skill-missing".to_string(),
        ResolvedEntryTarget {
            canonical_name: "alpha-skill-missing".to_string(),
            skill_storage_key: "alpha-storage".to_string(),
            skill_id: "alpha-skill".to_string(),
            local_name: "missing".to_string(),
        },
    );

    // Error returned before a stale missing-entry target can disappear from `vulcan.call`.
    // 在缺失局部入口的失效目标从 `vulcan.call` 中消失前返回的错误。
    let missing_entry_error = build_lua_call_dispatch_entries(&skills, &missing_entry_registry)
        .err()
        .expect("missing local entry target should fail dispatch build");
    assert!(missing_entry_error.contains("alpha-skill-missing"));
    assert!(missing_entry_error.contains("missing"));
    assert!(missing_entry_error.contains("alpha-skill"));

    // Registry containing one valid target that should still build one dispatch entry.
    // 包含一个有效目标的注册表，应仍然构造出一个分发入口。
    let mut valid_registry = BTreeMap::new();
    valid_registry.insert(
        "alpha-skill-ping".to_string(),
        ResolvedEntryTarget {
            canonical_name: "alpha-skill-ping".to_string(),
            skill_storage_key: "alpha-storage".to_string(),
            skill_id: "alpha-skill".to_string(),
            local_name: "ping".to_string(),
        },
    );

    // Dispatch entries produced for the valid registry.
    // 针对有效注册表生成的分发入口。
    let dispatch_entries = build_lua_call_dispatch_entries(&skills, &valid_registry)
        .expect("valid dispatch target should build");
    assert_eq!(dispatch_entries.len(), 1);
    assert_eq!(dispatch_entries[0].display_name, "alpha-skill-ping");
}

/// Verify formal root chains reject unknown labels and reversed priority order.
/// 验证正式根链会拒绝未知标签和反向优先级顺序。
#[test]
fn load_from_roots_rejects_unknown_or_reversed_formal_layers() {
    let runtime_root = make_temp_runtime_root("formal-root-chain-validation");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let mut engine = make_runtime_test_engine();
    let reversed_error = engine
        .load_from_roots(&[
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: runtime_root.join("user_skills"),
            },
            RuntimeSkillRoot {
                name: "ROOT".to_string(),
                skills_dir: runtime_root.join("root_skills"),
            },
        ])
        .expect_err("reversed formal root order should fail");
    assert!(
        reversed_error
            .to_string()
            .contains("ROOT -> PROJECT -> USER")
    );

    let unknown_error = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "WORKSPACE".to_string(),
            skills_dir: runtime_root.join("workspace_skills"),
        }])
        .expect_err("unknown formal root label should fail");
    assert!(
        unknown_error
            .to_string()
            .contains("unsupported skill root label")
    );

    let missing_root_error = engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: runtime_root.join("user_skills"),
        }])
        .expect_err("missing ROOT layer should fail");
    assert!(
        missing_root_error
            .to_string()
            .contains("ROOT skill root is required")
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify ordinary skills installs do not fall back to the system-controlled ROOT layer.
/// 验证普通 skills 安装不会回落到系统控制的 ROOT 层。
#[test]
fn install_skill_rejects_root_only_runtime() {
    let runtime_root = make_temp_runtime_root("ordinary-install-root-only");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    let mut engine = make_runtime_test_engine();

    let error = engine
        .install_skill(
            &[root_root],
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("ordinary install must reject root-only runtime");
    assert!(error.to_string().contains("ROOT is system-controlled"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify system installs do not fall back to ordinary layers when ROOT is absent.
/// 验证 system 安装在缺少 ROOT 时不会回退到普通层。
#[test]
fn system_install_skill_rejects_runtime_without_root() {
    let runtime_root = make_temp_runtime_root("system-install-without-root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    fs::create_dir_all(&user_root.skills_dir).expect("create user skills root");
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_install_skill(
            &[user_root],
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("system install without ROOT should fail");
    assert!(error.to_string().contains("ROOT skill root is required"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the Lua-visible ordinary skill-management layer list excludes ROOT.
/// 验证 Lua 可见的普通技能管理层级列表不包含 ROOT。
#[test]
fn runtime_skills_layers_excludes_root() {
    let runtime_root = make_temp_runtime_root("runtime-skills-layers-root-only");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        capabilities: LuaRuntimeCapabilityOptions {
            enable_skill_management_bridge: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("create root-only layer test engine");
    engine
        .load_from_roots(&[root_root])
        .expect("root-only runtime should load");
    let result = engine
        .run_lua("return vulcan.runtime.skills.layers()", &json!({}), None)
        .expect("layers function should run");

    assert_eq!(result["labels"], json!([]));
    assert_eq!(result["layers"], json!([]));
    assert_eq!(result["writable"], json!(false));
    assert!(result["default"].is_null());

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify layers reflects loaded PROJECT and USER roots and the bridge writable policy.
/// 验证 layers 会反映已加载 PROJECT/USER 根以及桥接写入策略。
#[test]
fn runtime_skills_layers_reflects_loaded_roots_and_bridge_policy() {
    let runtime_root = make_temp_runtime_root("runtime-skills-layers-dynamic");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[root_root.clone(), user_root])
        .expect("root and user runtime should load");
    let disabled_result = engine
        .run_lua("return vulcan.runtime.skills.layers()", &json!({}), None)
        .expect("layers function should run when bridge is disabled");
    assert_eq!(disabled_result["default"], json!("USER"));
    assert_eq!(disabled_result["labels"], json!(["USER"]));
    assert_eq!(disabled_result["writable"], json!(false));
    assert_eq!(disabled_result["layers"][0]["writable"], json!(false));

    let mut enabled_engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
            capabilities: LuaRuntimeCapabilityOptions {
                enable_skill_management_bridge: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("create enabled layer test engine");
    let project_root = RuntimeSkillRoot {
        name: "PROJECT".to_string(),
        skills_dir: runtime_root.join("project_skills"),
    };
    enabled_engine
        .load_from_roots(&[
            root_root,
            project_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: runtime_root.join("enabled_user_skills"),
            },
        ])
        .expect("root, project, user runtime should load");
    let enabled_result = enabled_engine
        .run_lua("return vulcan.runtime.skills.layers()", &json!({}), None)
        .expect("layers function should run when bridge is enabled");
    assert_eq!(enabled_result["default"], json!("USER"));
    assert_eq!(enabled_result["labels"], json!(["PROJECT", "USER"]));
    assert_eq!(enabled_result["writable"], json!(true));
    assert_eq!(enabled_result["layers"][0]["writable"], json!(true));
    assert!(
        enabled_result["labels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value != "ROOT")
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the ordinary Lua bridge rejects ROOT targets before dispatching to the host callback.
/// 验证普通 Lua 桥接会在分发到宿主回调前拒绝 ROOT 目标。
#[test]
fn runtime_skills_bridge_rejects_root_payload_before_callback() {
    let engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        capabilities: LuaRuntimeCapabilityOptions {
            enable_skill_management_bridge: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("create bridge test engine");

    let error = engine
        .run_lua(
            "return vulcan.runtime.skills.install({ layer = 'ROOT', skill_id = 'vulcan-codekit' })",
            &json!({}),
            None,
        )
        .expect_err("root target should be rejected by bridge");
    assert!(error.contains("cannot target the system-controlled ROOT layer"));
    assert!(!error.contains("no host callback"));

    let object_error = engine
            .run_lua(
                "return vulcan.runtime.skills.install({ target_root = { name = 'ROOT', skills_dir = 'C:/tmp/root-skills' }, skill_id = 'vulcan-codekit' })",
                &json!({}),
                None,
            )
            .expect_err("root target object should be rejected by bridge");
    assert!(object_error.contains("cannot target the system-controlled ROOT layer"));
    assert!(!object_error.contains("no host callback"));
}

/// Verify ordinary explicit-root APIs reject ROOT write targets before lifecycle work starts.
/// 验证普通显式根 API 会在生命周期工作开始前拒绝 ROOT 写入目标。
#[test]
fn ordinary_explicit_root_apis_reject_root_target() {
    let runtime_root = make_temp_runtime_root("ordinary-explicit-root-rejects-root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    fs::create_dir_all(&user_root.skills_dir).expect("create user skills root");
    let skill_roots = vec![root_root.clone(), user_root];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .install_skill_in_root(
            &skill_roots,
            &root_root,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("ordinary explicit root install should reject ROOT");
    assert!(error.to_string().contains("ordinary skills plane cannot"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify ROOT-owned skill ids cannot be installed or updated in ordinary layers by any authority.
/// 验证 ROOT 拥有的 skill id 不能被任何权限安装或更新到普通层。
#[test]
fn root_owned_skill_id_blocks_project_user_install_update_for_all_authorities() {
    let runtime_root = make_temp_runtime_root("root-owned-skill-id-blocks-ordinary");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let project_root = RuntimeSkillRoot {
        name: "PROJECT".to_string(),
        skills_dir: runtime_root.join("project_skills"),
    };
    let root_skill_dir = write_minimal_skill_to_root(&root_root.skills_dir, "vulcan-codekit");
    write_minimal_skill_to_root(&project_root.skills_dir, "vulcan-codekit");
    let skill_roots = vec![root_root, project_root.clone()];
    let mut engine = make_runtime_test_engine();

    let ordinary_install_error = engine
        .install_skill_in_root(
            &skill_roots,
            &project_root,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("ordinary install must reject ROOT-owned skill id");
    let ordinary_install_rendered = ordinary_install_error.to_string();
    assert!(ordinary_install_rendered.contains("ROOT system layer"));
    assert!(ordinary_install_rendered.contains(&render_host_visible_path(&root_skill_dir)));

    let system_install_error = engine
        .system_install_skill_in_root(
            &skill_roots,
            &project_root,
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("system install must reject ROOT-owned skill id in PROJECT");
    assert!(
        system_install_error
            .to_string()
            .contains("ROOT system layer")
    );

    let system_update_error = engine
        .system_update_skill_in_root(
            &skill_roots,
            &project_root,
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("system update must also reject ROOT-owned skill id in PROJECT");
    assert!(
        system_update_error
            .to_string()
            .contains("ROOT system layer")
    );

    let delegated_update_error = engine
        .system_update_skill_in_root(
            &skill_roots,
            &project_root,
            SkillManagementAuthority::DelegatedTool,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("delegated update must reject ROOT-owned skill id in PROJECT");
    assert!(
        delegated_update_error
            .to_string()
            .contains("ROOT system layer")
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify ordinary explicit-root uninstall may clean a USER residual shadowed by ROOT.
/// 验证普通显式根卸载可以清理被 ROOT 遮蔽的 USER 残留。
#[test]
fn ordinary_uninstall_in_root_cleans_user_residual_when_root_owns_same_skill_id() {
    let runtime_root = make_temp_runtime_root("ordinary-uninstall-cleans-root-shadow");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    let root_skill_dir = write_minimal_skill_to_root(&root_root.skills_dir, "vulcan-codekit");
    let user_skill_dir = write_minimal_skill_to_root(&user_root.skills_dir, "vulcan-codekit");
    let skill_roots = vec![root_root, user_root.clone()];
    let mut engine = make_runtime_test_engine();

    let result = engine
        .uninstall_skill_in_root(
            &skill_roots,
            &user_root,
            "vulcan-codekit",
            &SkillUninstallOptions::default(),
        )
        .expect("ordinary uninstall should clean USER residual");
    assert_eq!(result.skill_id, "vulcan-codekit");
    assert!(!user_skill_dir.exists());
    assert!(root_skill_dir.exists());

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify delegated authority cannot use a system explicit-root API to write ROOT.
/// 验证委托权限不能借助 system 显式根 API 写入 ROOT。
#[test]
fn delegated_authority_rejects_system_root_write() {
    let runtime_root = make_temp_runtime_root("delegated-system-root-write-reject");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    let skill_roots = vec![root_root.clone()];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_install_skill_in_root(
            &skill_roots,
            &root_root,
            SkillManagementAuthority::DelegatedTool,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("delegated authority must reject ROOT writes");
    assert!(error.to_string().contains("DelegatedTool authority"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify explicit-root system updates fail instead of returning a successful missing-skill result.
/// 验证显式根 system 更新在缺少目标技能时会失败，而不是返回成功的 missing-skill 结果。
#[test]
fn system_update_skill_in_root_missing_target_returns_error() {
    let runtime_root = make_temp_runtime_root("system-update-target-missing");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user").join("skills"),
    };
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root").join("skills"),
    };
    fs::create_dir_all(&user_root.skills_dir).expect("create user skills root");
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    let skill_roots = vec![root_root, user_root.clone()];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_update_skill_in_root(
            &skill_roots,
            &user_root,
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("missing explicit-root update target should fail");
    let rendered = error.to_string();

    assert!(rendered.contains("not installed in target root 'USER'"));
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify explicit-root apply rejects PROJECT changes when ROOT owns the same skill id.
/// 验证明确定根应用会在 ROOT 拥有同名 skill 时拒绝 PROJECT 变更。
#[test]
fn system_update_skill_in_root_rejects_shadowed_fallback_target() {
    let runtime_root = make_temp_runtime_root("system-update-shadowed-root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let project_root = RuntimeSkillRoot {
        name: "PROJECT".to_string(),
        skills_dir: runtime_root.join("project_skills"),
    };
    write_minimal_skill_to_root(&root_root.skills_dir, "vulcan-codekit");
    write_minimal_skill_to_root(&project_root.skills_dir, "vulcan-codekit");
    let skill_roots = vec![root_root, project_root.clone()];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_update_skill_in_root(
            &skill_roots,
            &project_root,
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("shadowed fallback target should fail before update");
    let rendered = error.to_string();

    assert!(rendered.contains("ROOT system layer"));
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify explicit-root install derives skill ids with the same GitHub locator rules as the manager.
/// 验证明确定根安装使用与管理器一致的 GitHub 定位规则推导技能标识。
#[test]
fn system_install_skill_in_root_accepts_trailing_slash_github_url_for_shadow_check() {
    let runtime_root = make_temp_runtime_root("system-install-trailing-slash-source");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    write_minimal_skill_to_root(&user_root.skills_dir, "vulcan-codekit");
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    let skill_roots = vec![root_root.clone(), user_root];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_install_skill_in_root(
            &skill_roots,
            &root_root,
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: None,
                source: Some("https://github.com/LuaSkills/vulcan-codekit/".to_string()),
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("root install should derive source skill id before managed download");
    let rendered = error.to_string();

    assert!(!rendered.contains("shadowed by higher-priority root"));
    assert!(!rendered.contains("requires skill_id"));
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify explicit-root system updates reject unlisted targets before probing target contents.
/// 验证明确定根 system 更新会在探测目标内容前拒绝链外目标。
#[test]
fn system_update_skill_in_root_rejects_unlisted_target_before_missing_target() {
    let runtime_root = make_temp_runtime_root("system-update-unlisted-root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let rogue_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("rogue_skills"),
    };
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    fs::create_dir_all(&user_root.skills_dir).expect("create user skills root");
    let skill_roots = vec![root_root, user_root];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_update_skill_in_root(
            &skill_roots,
            &rogue_root,
            SkillManagementAuthority::System,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("unlisted explicit update target root should be rejected");
    let rendered = error.to_string();

    assert!(rendered.contains("not part of the full runtime root chain"));
    assert!(rendered.contains(&render_host_visible_path(&rogue_root.skills_dir)));
    assert!(!rendered.contains("not installed in target root"));
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify explicit-root uninstall rejects target roots outside the active runtime chain.
/// 验证明确定根卸载会拒绝当前运行时根链之外的目标根。
#[test]
fn system_uninstall_skill_in_root_rejects_unlisted_target_root() {
    let runtime_root = make_temp_runtime_root("system-uninstall-unlisted-root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user").join("skills"),
    };
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root").join("skills"),
    };
    let rogue_root = RuntimeSkillRoot {
        name: "ROGUE".to_string(),
        skills_dir: runtime_root.join("rogue").join("skills"),
    };
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    fs::create_dir_all(&user_root.skills_dir).expect("create user skills root");
    let rogue_skill_dir = write_minimal_skill_to_root(&rogue_root.skills_dir, "vulcan-codekit");
    let skill_roots = vec![root_root, user_root];
    let mut engine = make_runtime_test_engine();

    let error = engine
        .system_uninstall_skill_in_root(
            &skill_roots,
            &rogue_root,
            SkillManagementAuthority::System,
            "vulcan-codekit",
            &SkillUninstallOptions::default(),
        )
        .expect_err("unlisted explicit target root should be rejected");
    let rendered = error.to_string();

    assert!(rendered.contains("not part of the full runtime root chain"));
    assert!(rendered.contains(&render_host_visible_path(&rogue_root.skills_dir)));
    assert!(
        rogue_skill_dir.exists(),
        "unlisted target skill directory should not be removed"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify skill database cleanup errors render paths through the host-visible formatter.
/// 验证技能数据库清理错误会通过宿主可见路径渲染器输出路径。
#[test]
fn skill_database_cleanup_error_uses_host_visible_path() {
    // Runtime engine used to call the real database cleanup helper.
    // 用于调用真实数据库清理辅助函数的运行时引擎。
    let engine = make_runtime_test_engine();
    // Temporary database root that isolates the cleanup failure fixture.
    // 隔离清理失败夹具的临时数据库根目录。
    let database_root = make_temp_runtime_root("database-cleanup-path");
    let _ = fs::remove_dir_all(&database_root);
    fs::create_dir_all(database_root.join("sqlite")).expect("create sqlite database root");
    // Database path pre-created as a file so remove_dir_all fails deterministically.
    // 预先创建为文件的数据库路径，使 remove_dir_all 稳定失败。
    let database_dir = database_root.join("sqlite").join("demo-skill");
    fs::write(&database_dir, "not a directory").expect("write conflicting database file");
    // Error returned by the real skill database cleanup helper.
    // 真实技能数据库清理辅助函数返回的错误。
    let error = engine
        .remove_skill_database_dir(&database_root, "demo-skill", true, "sqlite")
        .expect_err("database cleanup should fail when target is a file");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "failed to remove sqlite directory {}:",
        render_host_visible_path(&database_dir)
    );

    assert!(
        error.to_string().starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&database_root);
}

/// Verify invalid skill database cleanup targets are reported instead of treated as absent.
/// 验证非法技能数据库清理目标会被报告，而不是被当作不存在。
#[test]
fn skill_database_cleanup_invalid_target_path_is_reported() {
    // Runtime engine used to call the real database cleanup helper.
    // 用于调用真实数据库清理辅助函数的运行时引擎。
    let engine = make_runtime_test_engine();
    // Database root containing an embedded NUL that remove_dir_all cannot inspect.
    // 包含内嵌 NUL 且 remove_dir_all 无法探测的数据库根目录。
    let database_root = PathBuf::from("invalid\0database-root");

    // Error returned before the invalid cleanup target can behave like a missing directory.
    // 在非法清理目标表现得像缺失目录之前返回的错误。
    let error = engine
        .remove_skill_database_dir(&database_root, "demo-skill", true, "sqlite")
        .expect_err("invalid database cleanup target should fail");
    let rendered = error.to_string();

    assert!(
        rendered.contains("failed to remove sqlite directory"),
        "unexpected error: {}",
        rendered
    );
    assert!(
        rendered.contains("invalid"),
        "unexpected error: {}",
        rendered
    );
}

/// Verify the isolated runlua pool uses the documented default sizing when the host does not override it.
/// 验证宿主未覆盖时隔离 runlua 池会使用文档声明的默认容量配置。
#[test]
fn runlua_pool_uses_default_config_when_host_does_not_override() {
    let engine = make_runtime_test_engine();
    assert_eq!(engine.runlua_pool.config.min_size, 1);
    assert_eq!(engine.runlua_pool.config.max_size, 4);
    assert_eq!(engine.runlua_pool.config.idle_ttl_secs, 60);
}

/// Verify managed runtime root derivation errors render paths through the host-visible formatter.
/// 验证受管运行时根目录推导错误会通过宿主可见路径渲染器输出路径。
#[test]
fn managed_runtime_root_error_uses_host_visible_path() {
    // Real package directory paired with an intentionally missing runtime root.
    // 与故意缺失运行时根配对的真实包目录。
    let fixture_root = make_temp_runtime_root("managed-package-missing-runtime-root");
    let _ = fs::remove_dir_all(&fixture_root);
    let package_root = fixture_root.join("package");
    fs::create_dir_all(&package_root).expect("create package root");
    // Missing authoritative runtime root rejected during context construction.
    // 在上下文构造期间被拒绝的缺失权威运行时根。
    let missing_runtime_root = fixture_root.join("missing-runtime");
    let error = ManagedRuntimePackageContext::for_skill(
        "missing-runtime",
        &package_root,
        &missing_runtime_root,
        None,
    )
    .expect_err("missing runtime root must fail");
    assert!(
        error.contains("failed to canonicalize runtime_root"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&fixture_root);
}

/// Verify managed runtime source-file errors render paths through the host-visible formatter.
/// 验证受管运行时源文件错误会通过宿主可见路径渲染器输出路径。
#[test]
fn managed_runtime_skill_file_error_uses_host_visible_path() {
    // Real trusted package used by the canonical package-file resolver.
    // 规范包文件解析器使用的真实可信包。
    let runtime_root = make_temp_runtime_root("managed-package-missing-file");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "missing-file");
    // Missing canonical candidate expected in the stable diagnostic.
    // 稳定诊断中预期出现的缺失规范候选路径。
    let missing_file = package.package_root().join("handlers/missing.py");
    let error = package
        .resolve_existing_file("handlers/missing.py", "file")
        .expect_err("missing package file must fail");
    assert!(
        error.contains(&render_host_visible_path(&missing_file)),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify managed runtime source-file probe errors are not reported as missing files.
/// 验证受管运行时源文件探测错误不会被报告为文件缺失。
///
/// This test has no parameters and fails through assertions when invalid paths are folded into not-found errors.
/// 本测试不接收参数；当非法路径被折叠为 not-found 错误时会通过断言失败。
///
/// Return unit after validating the managed runtime file resolver emits an inspection diagnostic.
/// 校验受管运行时文件解析器输出探测诊断后返回 unit。
#[test]
fn managed_runtime_skill_file_reports_probe_errors() {
    // Real trusted package used before introducing an invalid child path.
    // 引入非法子路径前使用的真实可信包。
    let runtime_root = make_temp_runtime_root("managed-package-probe-error");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "probe-error");
    // Embedded-NUL child path that cannot be canonicalized by the filesystem.
    // 文件系统无法规范化的内嵌 NUL 子路径。
    let error = package
        .resolve_existing_file("handlers/invalid\0main.py", "file")
        .expect_err("invalid package file probe must fail");
    assert!(
        error.contains("failed to canonicalize file"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify managed runtime source-file resolution rejects directory placeholders.
/// 验证受管运行时源文件解析会拒绝目录占位。
///
/// This test has no parameters and fails through assertions when directories are treated as missing files.
/// 本测试不接收参数；当目录被当作缺失文件时会通过断言失败。
///
/// Return unit after validating the managed runtime file resolver emits a non-file diagnostic.
/// 校验受管运行时文件解析器输出非文件诊断后返回 unit。
#[test]
fn managed_runtime_skill_file_rejects_directory_source_path() {
    // Real trusted package containing a directory where a source file is required.
    // 在需要源文件的位置包含目录的真实可信包。
    let runtime_root = make_temp_runtime_root("managed-package-directory-file");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "directory-file");
    let directory_source = package.package_root().join("handlers/main.py");
    fs::create_dir_all(&directory_source).expect("create directory source fixture");
    // Explicit regular-file diagnostic returned for the directory target.
    // 针对目录目标返回的显式普通文件诊断。
    let error = package
        .resolve_existing_file("handlers/main.py", "file")
        .expect_err("directory package source must fail");
    assert!(
        error.contains("file is not a file"),
        "unexpected error: {error}"
    );
    assert!(
        error.contains(&render_host_visible_path(&directory_source)),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify managed package snapshot-root creation errors use the host-visible path formatter.
/// 验证受管包快照根创建错误会使用宿主可见路径渲染器。
#[test]
fn managed_package_snapshot_root_creation_error_uses_host_visible_path() {
    // Temporary root that isolates the managed Node import-root cleanup fixture.
    // 隔离受管 Node import-root 清理夹具的临时根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-cleanup-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Managed environment directory that owns the import root.
    // 拥有 import root 的受管环境目录。
    let env_dir = temp_root.join("env");
    fs::create_dir_all(&env_dir).expect("create managed node env dir");
    // Real trusted package context supplied to the import-root preparation helper.
    // 传给 import-root 准备辅助函数的真实可信包上下文。
    let package = make_test_managed_runtime_package(&temp_root, "demo");
    // Minimal managed Node env plan consumed by the real preparation helper.
    // 真实准备辅助函数消费的最小受管 Node 环境计划。
    // CanonicalEnvDir matches the native environment-root spelling retained by the plan authority.
    // CanonicalEnvDir 与计划授权保留的原生环境根路径形式保持一致。
    let canonical_env_dir =
        fs::canonicalize(&env_dir).expect("canonicalize managed Node environment");
    let plan = make_validated_test_managed_node_env_plan(canonical_env_dir);
    // Namespace path pre-created as a file so environment-local root creation fails deterministically.
    // 预先创建为文件的命名空间路径，使环境内根目录创建稳定失败。
    let snapshot_namespace = plan.env_dir.join(".ls-t");
    fs::write(&snapshot_namespace, "conflicting snapshot namespace")
        .expect("write conflicting snapshot namespace file");
    // Error returned by the real managed Node import-root preparation helper.
    // 真实受管 Node import-root 准备辅助函数返回的错误。
    let error = prepare_managed_node_import_root(&plan, package.as_ref())
        .expect_err("file snapshot namespace should fail root creation");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "failed to create managed package snapshot root {}:",
        render_host_visible_path(&snapshot_namespace)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify managed Node import-root parent creation errors are reported before copy starts.
/// 验证受管 Node import-root 父目录创建错误会在复制开始前被报告。
#[test]
fn managed_node_import_root_invalid_parent_path_is_reported() {
    // Temporary skill root that isolates the managed Node import-root probe fixture.
    // 隔离受管 Node import-root 探测夹具的临时 skill 根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-invalid-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Real trusted package context supplied to the import-root preparation helper.
    // 传给 import-root 准备辅助函数的真实可信包上下文。
    let package = make_test_managed_runtime_package(&temp_root, "demo");
    // Managed environment path containing an embedded NUL that symlink_metadata cannot inspect.
    // 包含内嵌 NUL 且 symlink_metadata 无法探测的受管环境路径。
    let env_dir = PathBuf::from("invalid\0managed-node-env");
    // Minimal managed Node env plan consumed by the real preparation helper.
    // 真实准备辅助函数消费的最小受管 Node 环境计划。
    let plan = make_test_managed_node_env_plan(env_dir);

    // Error returned before the invalid package parent can be created or copied into.
    // 在非法包父目录可被创建或复制前返回的错误。
    let error = prepare_managed_node_import_root(&plan, package.as_ref())
        .expect_err("invalid import root parent should fail before copy");

    assert!(
        error.starts_with("failed to create managed package snapshot root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify managed Node import-root copy errors render source and destination paths consistently.
/// 验证受管 Node import-root 复制错误会一致渲染源路径与目标路径。
#[test]
fn managed_node_import_root_copy_error_uses_host_visible_path() {
    // Temporary root that isolates the managed Node import-root copy fixture.
    // 隔离受管 Node import-root 复制夹具的临时根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-copy-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Source skill directory consumed by the recursive copy helper.
    // 递归复制辅助函数消费的源 skill 目录。
    let source_dir = temp_root.join("source");
    // Destination import root consumed by the recursive copy helper.
    // 递归复制辅助函数消费的目标 import root。
    let destination_dir = temp_root.join("destination");
    fs::create_dir_all(&source_dir).expect("create managed node source dir");
    // Source file that the real copy helper will attempt to copy.
    // 真实复制辅助函数将尝试复制的源文件。
    let source_file = source_dir.join("handler.js");
    fs::write(&source_file, "export default function handler() {}")
        .expect("write managed node source file");
    // Destination path pre-created as a directory so fs::copy fails on the real target path.
    // 预先创建为目录的目标路径，使 fs::copy 在真实目标路径上失败。
    let destination_file = destination_dir.join("handler.js");
    fs::create_dir_all(&destination_file).expect("create conflicting destination directory");
    // Error returned by the real recursive import-root copy helper.
    // 真实递归 import-root 复制辅助函数返回的错误。
    let error = copy_managed_node_package_import_root(&source_dir, &destination_dir)
        .expect_err("copying a file onto a directory should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "failed to copy {} to {}:",
        render_host_visible_path(&source_file),
        render_host_visible_path(&destination_file)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify managed Node import-root copy rejects unsupported Unix file types explicitly.
/// 验证受管 Node import-root 复制会显式拒绝不支持的 Unix 文件类型。
#[cfg(unix)]
#[test]
fn managed_node_import_root_copy_rejects_unsupported_unix_file_type() {
    use std::os::unix::fs::FileTypeExt;

    // Temporary root that isolates the unsupported-file-type copy fixture.
    // 隔离不支持文件类型复制夹具的临时根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-unsupported-type");
    let _ = fs::remove_dir_all(&temp_root);
    // Source skill directory consumed by the recursive copy helper.
    // 递归复制辅助函数消费的源 skill 目录。
    let source_dir = temp_root.join("source");
    // Destination import root consumed by the recursive copy helper.
    // 递归复制辅助函数消费的目标 import root。
    let destination_dir = temp_root.join("destination");
    fs::create_dir_all(&source_dir).expect("create managed node source dir");
    // FIFO path that is neither a regular file nor a directory.
    // 既不是普通文件也不是目录的 FIFO 路径。
    let fifo_path = source_dir.join("events.pipe");
    let status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo should create FIFO fixture");
    assert!(
        fs::metadata(&fifo_path)
            .expect("read FIFO metadata")
            .file_type()
            .is_fifo(),
        "fixture should be FIFO"
    );

    // Error returned before the unsupported source entry can be silently skipped.
    // 在不支持的源目录项被静默跳过之前返回的错误。
    let error = copy_managed_node_package_import_root(&source_dir, &destination_dir)
        .expect_err("unsupported FIFO entry should fail import-root copy");

    assert!(
        error.contains("unsupported file type"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&fifo_path)),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify managed Node import-root copy rejects symlink entries explicitly.
/// 验证受管 Node import-root 复制会显式拒绝符号链接目录项。
#[test]
fn managed_node_import_root_copy_rejects_symlink_entry() {
    // Temporary root that isolates the symlink copy fixture.
    // 隔离符号链接复制夹具的临时根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-symlink-entry");
    let _ = fs::remove_dir_all(&temp_root);
    // Source skill directory consumed by the recursive copy helper.
    // 递归复制辅助函数消费的源 skill 目录。
    let source_dir = temp_root.join("source");
    // Destination import root consumed by the recursive copy helper.
    // 递归复制辅助函数消费的目标 import root。
    let destination_dir = temp_root.join("destination");
    fs::create_dir_all(&source_dir).expect("create managed node source dir");
    // Real file target used only to create the symlink fixture.
    // 仅用于创建符号链接夹具的真实文件目标。
    let real_file_path = temp_root.join("real-handler.js");
    fs::write(&real_file_path, "export default function handler() {}")
        .expect("write managed node real file");
    // Symlink entry inside the source skill directory that should not be followed during import copy.
    // 源 skill 目录内的符号链接目录项，import 复制期间不应跟随它。
    let symlink_path = source_dir.join("handler-link.js");
    if !create_test_file_symlink(&symlink_path, &real_file_path) {
        let _ = fs::remove_dir_all(&temp_root);
        return;
    }

    // Error returned before the symlink source entry can be silently followed or skipped.
    // 在符号链接源目录项被静默跟随或跳过之前返回的错误。
    let error = copy_managed_node_package_import_root(&source_dir, &destination_dir)
        .expect_err("symlink entry should fail import-root copy");

    assert!(
        error.contains("unsupported file type"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&symlink_path)),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify skill text-file read errors render paths through the host-visible formatter.
/// 验证 skill 文本文件读取错误会通过宿主可见路径渲染器输出路径。
#[test]
fn skill_text_file_read_error_uses_host_visible_path() {
    // Temporary skill directory used by the real text-file reader helper.
    // 真实文本文件读取辅助函数使用的临时 skill 目录。
    let skill_dir = make_temp_runtime_root("skill-text-read-path").join("skills/demo");
    let _ = fs::remove_dir_all(&skill_dir);
    fs::create_dir_all(&skill_dir).expect("create skill text fixture dir");
    // Missing text file path resolved by the production helper.
    // 生产辅助函数解析出的缺失文本文件路径。
    let missing_file = skill_dir.join("docs/missing.md");
    // Error returned by the real skill text-file reader.
    // 真实 skill 文本文件读取器返回的错误。
    let error = read_skill_text_file(&skill_dir, "docs/missing.md", "help")
        .expect_err("missing skill text file should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to read help file {}:",
        render_host_visible_path(&missing_file)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(
        skill_dir
            .parent()
            .and_then(Path::parent)
            .expect("test skill dir should have runtime root"),
    );
}

/// Verify Lua help-source read errors render paths through the host-visible formatter.
/// 验证 Lua 帮助源码读取错误会通过宿主可见路径渲染器输出路径。
#[test]
fn lua_help_source_read_error_uses_host_visible_path() {
    // Temporary skill directory used by the real Lua help-source reader helper.
    // 真实 Lua 帮助源码读取辅助函数使用的临时 skill 目录。
    let skill_dir = make_temp_runtime_root("lua-help-source-read-path").join("skills/demo");
    let _ = fs::remove_dir_all(&skill_dir);
    fs::create_dir_all(&skill_dir).expect("create lua help fixture dir");
    // Missing Lua help file path resolved by the production helper.
    // 生产辅助函数解析出的缺失 Lua 帮助文件路径。
    let missing_file = skill_dir.join("help/missing.lua");
    // Error returned by the real Lua help-source reader.
    // 真实 Lua 帮助源码读取器返回的错误。
    let error = match read_lua_help_payload_source(&skill_dir, "help/missing.lua") {
        Ok(_) => panic!("missing Lua help file should fail"),
        Err(error) => error,
    };
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to read help file {}:",
        render_host_visible_path(&missing_file)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(
        skill_dir
            .parent()
            .and_then(Path::parent)
            .expect("test skill dir should have runtime root"),
    );
}

/// Verify Lua help payload runtime errors render paths through the host-visible formatter.
/// 验证 Lua 帮助载荷运行错误会通过宿主可见路径渲染器输出路径。
#[test]
fn lua_help_payload_runtime_error_uses_host_visible_path() {
    // Lua VM used by the real help payload renderer.
    // 真实帮助载荷渲染器使用的 Lua 虚拟机。
    let lua = Lua::new();
    // Help file path used only for payload diagnostics.
    // 仅用于载荷诊断信息的帮助文件路径。
    let helper_path = make_temp_runtime_root("lua-help-runtime-path").join("help/broken.lua");
    // Lua help source that compiles and initializes, then fails during returned function execution.
    // 可编译并初始化，但在返回函数执行时失败的 Lua 帮助源码。
    let helper_source = "return function() error('help boom') end";
    // Error returned by the real Lua help payload renderer.
    // 真实 Lua 帮助载荷渲染器返回的错误。
    let error = render_lua_help_payload_text(&lua, &helper_path, helper_source, "@broken-help")
        .expect_err("runtime-failing help payload should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Help runtime error for {}:",
        render_host_visible_path(&helper_path)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
}

/// Verify the host can override the isolated runlua pool sizing with the same shape as the main VM pool.
/// 验证宿主可以使用与主虚拟机池相同的参数形状覆盖隔离 runlua 池容量。
#[test]
fn runlua_pool_honors_host_override_config() {
    let engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        runlua_pool_config: Some(LuaRuntimeRunLuaPoolConfig {
            min_size: 2,
            max_size: 5,
            idle_ttl_secs: 90,
        }),
        ..Default::default()
    })
    .expect("create runtime test engine with custom runlua pool");
    assert_eq!(engine.runlua_pool.config.min_size, 2);
    assert_eq!(engine.runlua_pool.config.max_size, 5);
    assert_eq!(engine.runlua_pool.config.idle_ttl_secs, 90);
}

/// Verify one host-selected managed-runtime policy reaches every engine-owned consumer unchanged.
/// 验证一份宿主选择的受管运行时策略会原样到达每个引擎所有消费者。
#[test]
fn managed_runtime_config_reaches_worker_and_session_services() {
    // Config assigns nondefault B3-B7 values so accidental constant use remains observable.
    // Config 为 B3-B7 分配非默认值，使意外使用常量的行为保持可观察。
    let config = LuaRuntimeManagedRuntimeConfig {
        worker_pool_max_size_per_environment: 7,
        worker_idle_ttl_secs: 11,
        persistent_session_limit_per_engine: 13,
        persistent_session_default_buffer_limit_bytes_per_stream: 17_000,
        invoke_default_timeout_ms: Some(19_000),
    };
    // Engine is the production constructor path that distributes the validated policy.
    // Engine 是分发已校验策略的生产构造路径。
    let engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        managed_runtime_config: config,
        ..Default::default()
    })
    .expect("create engine with managed runtime policy");
    // WorkerPool exposes the exact capacity and idle values retained by the service.
    // WorkerPool 暴露服务保留的精确容量与空闲值。
    let worker_pool = engine.managed_runtime_workers.lock_pool();

    assert_eq!(worker_pool.max_size_per_environment, 7);
    assert_eq!(worker_pool.idle_ttl_secs, 11);
    drop(worker_pool);
    assert_eq!(
        engine
            .managed_runtime_services
            .persistent_session_limit_per_engine(),
        13
    );
    assert_eq!(
        engine
            .managed_runtime_services
            .persistent_session_default_buffer_limit_bytes_per_stream(),
        17_000
    );
    assert_eq!(
        engine.managed_runtime_workers.invoke_default_timeout_ms(),
        Some(19_000)
    );
}

/// Verify invalid managed-runtime policy fails before an explicit environment root is created.
/// 验证非法受管运行时策略会在创建显式环境根之前失败。
#[test]
fn managed_runtime_config_validation_precedes_filesystem_allocation() {
    // FixtureRoot owns the existing runtime data root and the intentionally absent environment root.
    // FixtureRoot 拥有现有运行时数据根与故意缺失的环境根。
    let fixture_root = make_temp_runtime_root("managed-runtime-config-validation");
    let _ = fs::remove_dir_all(&fixture_root);
    // RuntimeRoot must exist so only policy validation determines the result.
    // RuntimeRoot 必须存在，从而仅由策略校验决定结果。
    let runtime_root = fixture_root.join("runtime");
    fs::create_dir_all(&runtime_root).expect("create config validation runtime root");
    // EnvironmentRoot must remain absent when validation rejects the zero Worker capacity.
    // EnvironmentRoot 在零 Worker 容量被校验拒绝时必须保持缺失。
    let environment_root = fixture_root.join("must-not-be-created");
    // HostOptions carries the invalid policy through the real engine constructor.
    // HostOptions 通过真实引擎构造器携带非法策略。
    let mut host_options = LuaRuntimeHostOptions::with_runtime_root(&runtime_root);
    host_options.managed_runtime_environment_root = Some(environment_root.clone());
    host_options
        .managed_runtime_config
        .worker_pool_max_size_per_environment = 0;
    // Error is returned before ManagedRuntimeRoots can create the configured writable directory.
    // Error 会在 ManagedRuntimeRoots 创建已配置可写目录之前返回。
    let error = LuaEngine::new(runtime_test_engine_options(host_options))
        .err()
        .expect("zero Worker capacity must reject engine creation")
        .to_string();

    assert!(
        error.contains("worker_pool_max_size_per_environment"),
        "unexpected error: {error}"
    );
    assert!(!environment_root.exists());
    let _ = fs::remove_dir_all(&fixture_root);
}

/// Verify session and invoke request defaults apply only when the per-call field is omitted.
/// 验证会话与 invoke 请求默认值仅在单次调用字段省略时生效。
#[test]
fn managed_runtime_request_defaults_preserve_per_call_priority() {
    // Lua owns the request tables consumed by the production strict parsers.
    // Lua 拥有生产严格解析器消费的请求表。
    let lua = Lua::new();
    // SessionDefaultSpec omits buffer_limit_bytes and therefore receives the host value.
    // SessionDefaultSpec 省略 buffer_limit_bytes，因此接收宿主值。
    let session_default_spec = lua.create_table().expect("create default session spec");
    session_default_spec
        .set("file", "sidecar.js")
        .expect("set default session file");
    // SessionDefault is parsed with a deliberately nondefault 2048-byte host buffer.
    // SessionDefault 使用故意设置为非默认的 2048 字节宿主缓冲进行解析。
    let session_default = super::parse_managed_runtime_session_open_request(
        LuaValue::Table(session_default_spec),
        "vulcan.runtime.node.session.open",
        default_runtime_text_encoding(),
        2_048,
    )
    .expect("parse default session request");
    assert_eq!(session_default.buffer_limit_bytes, 2_048);

    // SessionOverrideSpec provides a per-call buffer that must win over the host default.
    // SessionOverrideSpec 提供必须优先于宿主默认值的单次缓冲。
    let session_override_spec = lua.create_table().expect("create override session spec");
    session_override_spec
        .set("file", "sidecar.js")
        .expect("set override session file");
    session_override_spec
        .set("buffer_limit_bytes", 4_096_u64)
        .expect("set override session buffer");
    // SessionOverride is the parsed request proving per-call priority.
    // SessionOverride 是证明单次调用优先级的已解析请求。
    let session_override = super::parse_managed_runtime_session_open_request(
        LuaValue::Table(session_override_spec),
        "vulcan.runtime.node.session.open",
        default_runtime_text_encoding(),
        2_048,
    )
    .expect("parse override session request");
    assert_eq!(session_override.buffer_limit_bytes, 4_096);

    // InvokeDefaultSpec omits timeout_ms and therefore receives the configured host timeout.
    // InvokeDefaultSpec 省略 timeout_ms，因此接收已配置宿主超时。
    let invoke_default_spec = lua.create_table().expect("create default invoke spec");
    invoke_default_spec
        .set("file", "handler.js")
        .expect("set default invoke file");
    // InvokeDefault is the parsed request carrying the host-level timeout.
    // InvokeDefault 是携带宿主级超时的已解析请求。
    let invoke_default = super::parse_managed_runtime_invoke_request(
        LuaValue::Table(invoke_default_spec),
        "vulcan.runtime.node.invoke",
        "default",
        Some(3_000),
    )
    .expect("parse default invoke request");
    assert_eq!(invoke_default.timeout_ms, Some(3_000));

    // InvokeOverrideSpec provides the exact per-call timeout that must remain authoritative.
    // InvokeOverrideSpec 提供必须保持权威的精确单次调用超时。
    let invoke_override_spec = lua.create_table().expect("create override invoke spec");
    invoke_override_spec
        .set("file", "handler.js")
        .expect("set override invoke file");
    invoke_override_spec
        .set("timeout_ms", 5_000_u64)
        .expect("set override invoke timeout");
    // InvokeOverride is the parsed request proving per-call timeout priority.
    // InvokeOverride 是证明单次调用超时优先级的已解析请求。
    let invoke_override = super::parse_managed_runtime_invoke_request(
        LuaValue::Table(invoke_override_spec),
        "vulcan.runtime.node.invoke",
        "default",
        Some(3_000),
    )
    .expect("parse override invoke request");
    assert_eq!(invoke_override.timeout_ms, Some(5_000));
}

/// Verify the engine host API persists string skill config values into one explicit config file.
/// 验证引擎宿主 API 会把字符串技能配置值持久化到显式配置文件中。
#[test]
fn skill_config_engine_api_persists_values_into_explicit_file() {
    let runtime_root = make_temp_runtime_root("skill_config_explicit_path");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_skill_config_test_skill(&runtime_root, "demo-skill");
    let config_root = runtime_root.join("custom");
    let config_file = config_root.join("system-skills").join("config.json");

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_root: Some(config_root),
        ..Default::default()
    })
    .expect("create skill config test engine");
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load declared config test skill");

    engine
        .set_skill_config_value("demo-skill", "api_token", "sk-explicit")
        .expect("set explicit skill config");
    assert_eq!(
        engine
            .get_skill_config_value("demo-skill", "api_token")
            .expect("read explicit skill config"),
        Some("sk-explicit".to_string())
    );
    let entries = engine
        .list_skill_config_entries(Some("demo-skill"))
        .expect("list explicit skill config");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].skill_id, "demo-skill");
    assert_eq!(entries[0].key, "api_token");
    assert_eq!(entries[0].value, "sk-explicit");
    assert!(config_file.exists());

    let deleted = engine
        .delete_skill_config_value("demo-skill", "api_token", None)
        .expect("delete explicit skill config");
    assert!(deleted.deleted);
    assert_eq!(
        engine
            .get_skill_config_value("demo-skill", "api_token")
            .expect("read deleted explicit skill config"),
        None
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify invalid reload requests fail before clearing the active runtime view.
/// 验证无效重载请求会在清空当前运行时视图前失败。
#[test]
fn reload_from_roots_rejects_invalid_chain_before_resetting_runtime_state() {
    let runtime_root = make_temp_runtime_root("reload-invalid-chain-preserves-state");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    fs::create_dir_all(&root_root.skills_dir).expect("create root skills root");
    write_minimal_skill_to_root_with_response(&user_root.skills_dir, "vulcan-codekit", "user");
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[root_root, user_root.clone()])
        .expect("initial root and user runtime should load");

    let invalid_reload_error = engine
        .reload_from_roots(&[user_root])
        .expect_err("missing ROOT reload should fail");
    assert!(
        invalid_reload_error
            .to_string()
            .contains("ROOT skill root is required")
    );

    let result = engine
        .call_skill("vulcan-codekit-ping", &json!({}), None)
        .expect("old entry should remain callable after failed reload");
    assert_eq!(result.content, "user");

    let layers = engine
        .run_lua("return vulcan.runtime.skills.layers()", &json!({}), None)
        .expect("layers should still use the previously loaded root chain");
    assert_eq!(layers["labels"], json!(["USER"]));
    assert_eq!(layers["default"], json!("USER"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify one loaded skill can read its own namespaced config through `vulcan.config.get`.
/// 验证单个已加载技能可以通过 `vulcan.config.get` 读取自己的命名空间配置。
#[test]
fn call_skill_reads_own_skill_config_namespace() {
    let runtime_root = make_temp_runtime_root("skill_config_call_skill");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_skill_config_test_skill(&runtime_root, "demo-skill");

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_root: Some(runtime_root.join("user-config")),
        ..Default::default()
    })
    .expect("create call_skill config test engine");
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load config test skill");
    engine
        .set_skill_config_value("demo-skill", "api_token", "sk-from-config")
        .expect("seed skill config value");

    let result = engine
        .call_skill("demo-skill-ping", &json!({}), None)
        .expect("call skill with config");
    assert_eq!(result.content, "sk-from-config");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify Lua package configuration structure and status APIs expose declared metadata only.
/// 验证 Lua 技能包配置结构与状态 API 只暴露已声明元数据。
#[test]
fn lua_package_config_describe_status_and_declaration_guards() {
    let runtime_root = make_temp_runtime_root("skill_config_lua_structure");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_skill_config_test_skill(&runtime_root, "demo-skill");

    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_root: Some(runtime_root.join("user-config")),
        ..Default::default()
    });
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load Lua config structure test skill");

    let status = engine
        .call_skill("demo-skill-ping", &json!({"action": "status"}), None)
        .expect("query incomplete Lua config status");
    let status_json: Value =
        serde_json::from_str(&status.content).expect("Lua config status should be JSON");
    assert!(!status_json["complete"].as_bool().unwrap());
    assert_eq!(status_json["missing"][0]["key"], "api_token");

    let description = engine
        .call_skill("demo-skill-ping", &json!({"action": "describe"}), None)
        .expect("query Lua config structure");
    let description_json: Value =
        serde_json::from_str(&description.content).expect("Lua config descriptor should be JSON");
    assert_eq!(description_json["skill_id"], "demo-skill");
    assert_eq!(description_json["items"][0]["type"], "string");
    assert_eq!(description_json["items"][0]["sensitive"], true);
    assert!(description_json["items"][0].get("value").is_none());

    let undeclared_error = engine
        .call_skill(
            "demo-skill-ping",
            &json!({"action": "set_undeclared"}),
            None,
        )
        .expect_err("Lua must reject writes to undeclared package config keys");
    assert!(undeclared_error.contains("is not declared by skill package"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify Lua-visible runtime markers cannot redirect package-configuration writes across packages.
/// 验证 Lua 可见运行时标记无法将技能包配置写入重定向到其他技能包。
#[test]
fn lua_visible_skill_marker_cannot_bypass_package_config_isolation() {
    let runtime_root = make_temp_runtime_root("skill_config_visible_marker_isolation");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_skill_config_test_skill(&runtime_root, "package-a");
    write_skill_config_test_skill(&runtime_root, "package-b");

    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_root: Some(runtime_root.join("user-config")),
        ..Default::default()
    });
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load visible marker isolation skills");
    engine
        .set_skill_config_value("package-a", "api_token", "package-a-before")
        .expect("seed package A config");
    engine
        .set_skill_config_value("package-b", "api_token", "package-b-before")
        .expect("seed package B config");

    let result = engine
        .call_skill(
            "package-a-ping",
            &json!({
                "action": "tamper_then_set",
                "target_skill": "package-b",
                "value": "package-a-after"
            }),
            None,
        )
        .expect("call package A after tampering with the Lua-visible marker");
    assert_eq!(result.content, "package-a-after");
    assert_eq!(
        engine
            .get_skill_config_value("package-a", "api_token")
            .expect("read package A config")
            .as_deref(),
        Some("package-a-after")
    );
    assert_eq!(
        engine
            .get_skill_config_value("package-b", "api_token")
            .expect("read package B config")
            .as_deref(),
        Some("package-b-before")
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify nested package calls switch configuration ownership and restore the caller afterward.
/// 验证嵌套技能包调用会切换配置归属，并在返回后恢复调用方归属。
#[test]
fn nested_skill_call_isolates_and_restores_package_config_context() {
    let runtime_root = make_temp_runtime_root("skill_config_nested_isolation");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    write_skill_config_test_skill(&runtime_root, "package-b");
    write_skill_config_caller_test_skill(&runtime_root, "package-a", "package-b-ping");

    let mut engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_root: Some(runtime_root.join("user-config")),
        ..Default::default()
    });
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load nested config isolation skills");
    engine
        .set_skill_config_value("package-a", "api_token", "package-a-value")
        .expect("set caller package config");
    engine
        .set_skill_config_value("package-b", "api_token", "package-b-value")
        .expect("set target package config");

    let result = engine
        .call_skill("package-a-ping", &json!({}), None)
        .expect("call nested package config entry");
    assert_eq!(result.content, "package-b-value:package-a-value");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the checked-in package configuration example loads and exercises its documented flow.
/// 验证仓库内技能包配置示例可以加载并执行其文档化流程。
#[test]
fn package_config_example_skill_smoke_test() {
    let runtime_root = make_temp_runtime_root("skill_config_example_smoke");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    fs::create_dir_all(&runtime_root).expect("create example smoke runtime root");
    let config_root = runtime_root.join("config");
    let example_skills_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("skill-package-config");

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_root: Some(config_root),
        ..Default::default()
    })
    .expect("create example package config engine");
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: example_skills_root,
        }])
        .expect("load checked-in config example");

    let status_result = engine
        .call_skill("example-config-skill-status", &json!({}), None)
        .expect("call config example status");
    let status_json: Value =
        serde_json::from_str(&status_result.content).expect("example status should be JSON");
    assert_eq!(
        status_json["declaration"]["items"].as_array().map(Vec::len),
        Some(5)
    );
    assert_eq!(status_json["status"]["complete"], false);
    assert_eq!(status_json["status"]["missing"][0]["key"], "api_token");

    engine
        .set_skill_config_value("example-config-skill", "api_token", "example-token")
        .expect("configure example token");
    let query_result = engine
        .call_skill("example-config-skill-query", &json!({}), None)
        .expect("call configured example query");
    let query_json: Value =
        serde_json::from_str(&query_result.content).expect("example query should be JSON");
    assert_eq!(query_json["retry_count"], 3);
    assert_eq!(query_json["temperature"], 0.7);
    assert_eq!(query_json["provider"], "openai");
    assert_eq!(query_json["telemetry_enabled"], false);

    let invalid_error = engine
        .call_skill(
            "example-config-skill-query",
            &json!({"action": "set_invalid_demo"}),
            None,
        )
        .expect_err("example invalid write should fail");
    assert!(invalid_error.contains("exceeds maximum"));
    engine
        .call_skill(
            "example-config-skill-query",
            &json!({"action": "set_valid_demo"}),
            None,
        )
        .expect("example valid write should succeed");
    assert_eq!(
        engine
            .get_skill_config_value("example-config-skill", "retry_count")
            .expect("read normalized example retry count"),
        Some("5".to_string())
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify `vulcan.config.*` rejects calls that execute without one active skill context.
/// 验证 `vulcan.config.*` 会拒绝在没有活动技能上下文时执行的调用。
#[test]
fn run_lua_config_api_requires_active_skill_context() {
    let engine = make_runtime_test_engine();
    let error = engine
        .run_lua("return vulcan.config.get('api_token')", &json!({}), None)
        .expect_err("run_lua config access should require active skill context");
    assert!(error.contains("vulcan.config.get requires one active skill context"));
}

/// Verify `vulcan.models.*` reports disabled capabilities and structured unavailable errors by default.
/// 验证 `vulcan.models.*` 默认报告能力未开启，并返回结构化不可用错误。
#[test]
fn vulcan_models_defaults_without_callbacks() {
    let _guard = runtime_model_callback_test_guard();
    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
local status = vulcan.models.status()
local embed = vulcan.models.embed("x")
local llm = vulcan.models.llm("s", "u")
return {
  status_ok = status.ok,
  embed_capability = status.capabilities.embed,
  llm_capability = status.capabilities.llm,
  has_embed = vulcan.models.has("embed"),
  has_llm = vulcan.models.has("llm"),
  has_unknown = vulcan.models.has("rerank"),
  embed_ok = embed.ok,
  embed_code = embed.error.code,
  llm_ok = llm.ok,
  llm_code = llm.error.code,
}
"#,
            &json!({}),
            None,
        )
        .expect("run model defaults lua");

    assert_eq!(result["status_ok"], true);
    assert_eq!(result["embed_capability"], false);
    assert_eq!(result["llm_capability"], false);
    assert_eq!(result["has_embed"], false);
    assert_eq!(result["has_llm"], false);
    assert_eq!(result["has_unknown"], false);
    assert_eq!(result["embed_ok"], false);
    assert_eq!(result["embed_code"], "model_unavailable");
    assert_eq!(result["llm_ok"], false);
    assert_eq!(result["llm_code"], "model_unavailable");
}

/// Verify model APIs return structured invalid-argument errors instead of throwing to Lua.
/// 验证模型 API 会返回结构化非法参数错误，而不是向 Lua 抛出异常。
#[test]
fn vulcan_models_validate_arguments() {
    let _guard = runtime_model_callback_test_guard();
    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
local embed_empty = vulcan.models.embed("")
local embed_table = vulcan.models.embed({ "a", "b" })
local embed_extra = vulcan.models.embed("x", "extra")
local llm_empty_system = vulcan.models.llm("", "u")
local llm_empty_user = vulcan.models.llm("s", "")
local llm_extra = vulcan.models.llm("s", "u", "extra")
return {
  embed_empty = embed_empty.error.code,
  embed_table = embed_table.error.code,
  embed_extra = embed_extra.error.code,
  llm_empty_system = llm_empty_system.error.code,
  llm_empty_user = llm_empty_user.error.code,
  llm_extra = llm_extra.error.code,
}
"#,
            &json!({}),
            None,
        )
        .expect("run model argument validation lua");

    assert_eq!(result["embed_empty"], "invalid_argument");
    assert_eq!(result["embed_table"], "invalid_argument");
    assert_eq!(result["embed_extra"], "invalid_argument");
    assert_eq!(result["llm_empty_system"], "invalid_argument");
    assert_eq!(result["llm_empty_user"], "invalid_argument");
    assert_eq!(result["llm_extra"], "invalid_argument");
}

/// Verify registered embedding callbacks receive text and full caller context.
/// 验证已注册的 embedding 回调会收到文本和完整调用方上下文。
#[test]
fn vulcan_models_embed_dispatches_registered_callback_with_context() {
    let _guard = runtime_model_callback_test_guard();
    let captured_request: Arc<Mutex<Option<RuntimeModelEmbedRequest>>> = Arc::new(Mutex::new(None));
    let captured_request_for_callback = captured_request.clone();
    set_model_embed_callback(Some(Arc::new(move |request| {
        *captured_request_for_callback
            .lock()
            .expect("lock captured embed request") = Some(request.clone());
        Ok(RuntimeModelEmbedResponse {
            vector: vec![0.25, 0.5, 0.75],
            dimensions: 3,
            usage: Some(RuntimeModelUsage {
                input_tokens: Some(2),
                output_tokens: None,
            }),
        })
    })));

    let engine = make_runtime_test_engine();
    let has_embed = engine
        .run_lua("return vulcan.models.has('embed')", &json!({}), None)
        .expect("run has embed lua");
    assert_eq!(has_embed, json!(true));

    let runtime_root = make_temp_runtime_root("model-embed-context");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    let skill_dir = write_model_test_skill_to_root(
        &runtime_root.join("skills"),
        "model-skill",
        "return function(args)\n  local result = vulcan.models.embed(\"hello\")\n  return vulcan.json.encode(result)\nend\n",
    );
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load model embed test skill");
    let invocation_context = crate::runtime_options::LuaInvocationContext::new(
        Some(RuntimeRequestContext {
            request_id: Some("req-embed-1".to_string()),
            client_name: Some("Codex Desktop".to_string()),
            transport_name: Some("mcp".to_string()),
            session_id: Some("session-embed".to_string()),
            client_info: Some(RuntimeClientInfo {
                kind: Some("desktop".to_string()),
                name: Some("Codex Desktop".to_string()),
                version: Some("test".to_string()),
            }),
            client_capabilities: json!({"models": true}),
        }),
        json!({"budget": "test"}),
        json!({"tool": "config"}),
    );
    let result = engine
        .call_skill("model-skill-ping", &json!({}), Some(&invocation_context))
        .expect("call model embed skill");
    let result_json: Value =
        serde_json::from_str(&result.content).expect("parse embed result json");
    let captured = captured_request
        .lock()
        .expect("lock captured embed request")
        .clone()
        .expect("embed request captured");

    assert_eq!(result_json["ok"], true);
    assert_eq!(result_json["vector"], json!([0.25, 0.5, 0.75]));
    assert_eq!(result_json["dimensions"], 3);
    assert_eq!(result_json["usage"]["input_tokens"], 2);
    assert_eq!(captured.text, "hello");
    assert_eq!(captured.caller.skill_id.as_deref(), Some("model-skill"));
    assert_eq!(captured.caller.entry_name.as_deref(), Some("ping"));
    assert_eq!(
        captured.caller.canonical_tool_name.as_deref(),
        Some("model-skill-ping")
    );
    assert_eq!(captured.caller.root_name.as_deref(), Some("ROOT"));
    assert_eq!(
        captured.caller.skill_dir.as_deref(),
        Some(render_host_visible_path(&skill_dir).as_str())
    );
    assert_eq!(
        captured.caller.client_name.as_deref(),
        Some("Codex Desktop")
    );
    assert_eq!(captured.caller.request_id.as_deref(), Some("req-embed-1"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify malformed Lua request context is reported instead of silently dropping caller context.
/// 验证格式错误的 Lua request context 会被报告，而不是静默丢弃调用方上下文。
#[test]
fn vulcan_models_embed_rejects_malformed_request_context() {
    let _guard = runtime_model_callback_test_guard();
    // Whether the host embedding callback was reached after caller-context validation.
    // 调用方上下文校验后是否触达宿主 embedding 回调。
    let callback_called = Arc::new(Mutex::new(false));
    let callback_called_for_callback = Arc::clone(&callback_called);
    set_model_embed_callback(Some(Arc::new(move |_request| {
        *callback_called_for_callback
            .lock()
            .expect("lock malformed context callback flag") = true;
        Ok(RuntimeModelEmbedResponse {
            vector: vec![1.0],
            dimensions: 1,
            usage: None,
        })
    })));

    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            "vulcan.context.request = { request_id = 42 }\nreturn vulcan.models.embed(\"hello\")",
            &json!({}),
            None,
        )
        .expect("run malformed request context embed lua");

    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "internal_error");
    assert!(
        result["error"]["message"]
            .as_str()
            .expect("model error message should be text")
            .contains("vulcan.context.request is not a valid runtime request context")
    );
    assert!(
        !*callback_called
            .lock()
            .expect("read malformed context callback flag")
    );
}

/// Verify registered LLM callbacks receive prompts and full caller context.
/// 验证已注册的 LLM 回调会收到提示词和完整调用方上下文。
#[test]
fn vulcan_models_llm_dispatches_registered_callback_with_context() {
    let _guard = runtime_model_callback_test_guard();
    let captured_request: Arc<Mutex<Option<RuntimeModelLlmRequest>>> = Arc::new(Mutex::new(None));
    let captured_request_for_callback = captured_request.clone();
    set_model_llm_callback(Some(Arc::new(move |request| {
        *captured_request_for_callback
            .lock()
            .expect("lock captured llm request") = Some(request.clone());
        Ok(RuntimeModelLlmResponse {
            assistant: "assistant text".to_string(),
            usage: Some(RuntimeModelUsage {
                input_tokens: Some(5),
                output_tokens: Some(7),
            }),
        })
    })));

    let engine = make_runtime_test_engine();
    let has_llm = engine
        .run_lua("return vulcan.models.has('llm')", &json!({}), None)
        .expect("run has llm lua");
    assert_eq!(has_llm, json!(true));

    let runtime_root = make_temp_runtime_root("model-llm-context");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    let skill_dir = write_model_test_skill_to_root(
        &runtime_root.join("skills"),
        "llm-skill",
        "return function(args)\n  local result = vulcan.models.llm(\"system\", \"user\")\n  return vulcan.json.encode(result)\nend\n",
    );
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load model llm test skill");
    let result = engine
        .call_skill("llm-skill-ping", &json!({}), None)
        .expect("call model llm skill");
    let result_json: Value = serde_json::from_str(&result.content).expect("parse llm result json");
    let captured = captured_request
        .lock()
        .expect("lock captured llm request")
        .clone()
        .expect("llm request captured");

    assert_eq!(result_json["ok"], true);
    assert_eq!(result_json["assistant"], "assistant text");
    assert_eq!(result_json["usage"]["input_tokens"], 5);
    assert_eq!(result_json["usage"]["output_tokens"], 7);
    assert_eq!(captured.system, "system");
    assert_eq!(captured.user, "user");
    assert_eq!(captured.caller.skill_id.as_deref(), Some("llm-skill"));
    assert_eq!(captured.caller.entry_name.as_deref(), Some("ping"));
    assert_eq!(
        captured.caller.canonical_tool_name.as_deref(),
        Some("llm-skill-ping")
    );
    assert_eq!(captured.caller.root_name.as_deref(), Some("ROOT"));
    assert_eq!(
        captured.caller.skill_dir.as_deref(),
        Some(render_host_visible_path(&skill_dir).as_str())
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify callback errors preserve standard codes and provider raw fields.
/// 验证回调错误会保留标准错误码和 provider 原始字段。
#[test]
fn vulcan_models_wrap_callback_errors_and_provider_fields() {
    let _guard = runtime_model_callback_test_guard();
    set_model_embed_callback(Some(Arc::new(|_| {
        Err(RuntimeModelError {
            code: RuntimeModelErrorCode::ProviderError,
            message: "provider failed".to_string(),
            provider_message: Some("raw provider message".to_string()),
            provider_code: Some("model_not_found".to_string()),
            provider_status: Some(400),
        })
    })));
    set_model_llm_callback(Some(Arc::new(|_| {
        Err(RuntimeModelError {
            code: RuntimeModelErrorCode::Timeout,
            message: "llm timed out".to_string(),
            provider_message: None,
            provider_code: None,
            provider_status: None,
        })
    })));

    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
local embed = vulcan.models.embed("hello")
local llm = vulcan.models.llm("system", "user")
return {
  embed_ok = embed.ok,
  embed_code = embed.error.code,
  embed_message = embed.error.message,
  provider_message = embed.error.provider_message,
  provider_code = embed.error.provider_code,
  provider_status = embed.error.provider_status,
  llm_ok = llm.ok,
  llm_code = llm.error.code,
  llm_message = llm.error.message,
}
"#,
            &json!({}),
            None,
        )
        .expect("run model error wrapping lua");

    assert_eq!(result["embed_ok"], false);
    assert_eq!(result["embed_code"], "provider_error");
    assert_eq!(result["embed_message"], "provider failed");
    assert_eq!(result["provider_message"], "raw provider message");
    assert_eq!(result["provider_code"], "model_not_found");
    assert_eq!(result["provider_status"], 400);
    assert_eq!(result["llm_ok"], false);
    assert_eq!(result["llm_code"], "timeout");
    assert_eq!(result["llm_message"], "llm timed out");
}

/// Verify `vulcan.host.*` returns safe defaults when no host callback is registered.
/// 验证未注册宿主回调时 `vulcan.host.*` 会返回安全默认值。
#[test]
fn vulcan_host_bridge_defaults_without_callback() {
    let _guard = host_tool_callback_test_guard();
    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
local tools = vulcan.host.list()
local called = vulcan.host.call("model.embed", {})
return {
  list_len = #tools,
  has = vulcan.host.has("model.embed"),
  has_tool = vulcan.host.has_tool("model.embed"),
  call_ok = called.ok,
  call_code = called.error.code,
}
"#,
            &json!({}),
            None,
        )
        .expect("run host bridge default lua");

    assert_eq!(result["list_len"], 0);
    assert_eq!(result["has"], false);
    assert_eq!(result["has_tool"], false);
    assert_eq!(result["call_ok"], false);
    assert_eq!(result["call_code"], "host_tool_callback_missing");
}

/// Verify `vulcan.host.*` dispatches list, has, and call requests through the host callback.
/// 验证 `vulcan.host.*` 会通过宿主回调分发 list、has 与 call 请求。
#[test]
fn vulcan_host_bridge_dispatches_registered_callback() {
    let _guard = host_tool_callback_test_guard();
    set_host_tool_callback(Some(Arc::new(|request| match request.action {
        RuntimeHostToolAction::List => Ok(json!([
            {
                "name": "model.echo",
                "description": "Echo test host tool",
                "input_schema": {
                    "type": "object",
                },
            }
        ])),
        RuntimeHostToolAction::Has => Ok(json!(request.tool_name.as_deref() == Some("model.echo"))),
        RuntimeHostToolAction::Call => {
            let tool_name = request.tool_name.as_deref().unwrap_or_default();
            if tool_name != "model.echo" {
                return Err(format!("host tool not found: {}", tool_name));
            }
            Ok(json!({
                "ok": true,
                "value": {
                    "echo": request.args["text"].clone(),
                },
                "meta": {
                    "tool": tool_name,
                },
            }))
        }
    })));

    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
local tools = vulcan.host.list()
local called = vulcan.host.call("model.echo", { text = "hello" })
return {
  first = tools[1].name,
  has = vulcan.host.has("model.echo"),
  missing = vulcan.host.has_tool("missing.tool"),
  ok = called.ok,
  echo = called.value.echo,
  tool = called.meta.tool,
}
"#,
            &json!({}),
            None,
        )
        .expect("run host bridge callback lua");

    assert_eq!(result["first"], "model.echo");
    assert_eq!(result["has"], true);
    assert_eq!(result["missing"], false);
    assert_eq!(result["ok"], true);
    assert_eq!(result["echo"], "hello");
    assert_eq!(result["tool"], "model.echo");
}

/// Verify host-tool arguments and callback results preserve empty objects, arrays, and null values.
/// 验证宿主工具参数与回调结果会保留空对象、数组及 null 值。
#[test]
fn vulcan_host_bridge_preserves_json_container_types_and_null() {
    // Process-wide callback guard serializing this host-tool callback test.
    // 串行化当前宿主工具回调测试的进程级保护器。
    let _guard = host_tool_callback_test_guard();
    set_host_tool_callback(Some(Arc::new(|request| match request.action {
        RuntimeHostToolAction::List => Ok(json!([])),
        RuntimeHostToolAction::Has => Ok(json!(true)),
        RuntimeHostToolAction::Call => {
            // Explicit object markers must bypass the historical empty host-args compatibility rule.
            // 显式对象标记必须绕过历史空宿主参数兼容规则。
            if request.tool_name.as_deref() == Some("json.empty-object") {
                assert_eq!(request.args, json!({}));
                return Ok(json!({"ok": true}));
            }
            // Explicit array markers must also retain their container identity at the callback boundary.
            // 显式数组标记在回调边界也必须保留其容器身份。
            if request.tool_name.as_deref() == Some("json.empty-array") {
                assert_eq!(request.args, json!([]));
                return Ok(json!({"ok": true}));
            }
            // Canonical protocol shape expected at the Rust callback boundary.
            // Rust 回调边界预期的规范协议结构。
            let expected = json!({
                "environment": {},
                "binary_path_requests": [],
                "contributions": [],
                "optional": null,
                "nullable_items": [null]
            });
            assert_eq!(request.args, expected);
            Ok(json!({
                "ok": true,
                "value": expected
            }))
        }
    })));

    // Runtime engine exercising both Lua-to-host arguments and host-to-Lua callback results.
    // 同时覆盖 Lua 到宿主参数与宿主到 Lua 回调结果的运行时引擎。
    let engine = make_runtime_test_engine();
    // Result returned through the shared RunLua JSON boundary.
    // 通过共享 RunLua JSON 边界返回的结果。
    let result = engine
        .run_lua(
            r#"
local called = vulcan.host.call("json.protocol", {
  environment = vulcan.json.object(),
  binary_path_requests = vulcan.json.array(),
  contributions = vulcan.json.array(),
  optional = vulcan.json.null,
  nullable_items = vulcan.json.array({ vulcan.json.null }),
})
vulcan.host.call("json.empty-object", vulcan.json.object())
vulcan.host.call("json.empty-array", vulcan.json.array())
return called.value
"#,
            &json!({}),
            None,
        )
        .expect("run host JSON fidelity bridge");

    assert_eq!(
        result,
        json!({
            "environment": {},
            "binary_path_requests": [],
            "contributions": [],
            "optional": null,
            "nullable_items": [null]
        })
    );
}

/// Verify `vulcan.host.call` converts callback failures into table error envelopes.
/// 验证 `vulcan.host.call` 会把回调失败转换为 table 错误包络。
#[test]
fn vulcan_host_call_wraps_callback_errors() {
    let _guard = host_tool_callback_test_guard();
    set_host_tool_callback(Some(Arc::new(|request| match request.action {
        RuntimeHostToolAction::List => Ok(json!([])),
        RuntimeHostToolAction::Has => Ok(json!(true)),
        RuntimeHostToolAction::Call => {
            assert!(request.args.as_object().is_some());
            assert!(request.args.as_object().unwrap().is_empty());
            Err("model provider failed".to_string())
        }
    })));

    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua(
            r#"
local called = vulcan.host.call("model.fail", {})
return {
  ok = called.ok,
  code = called.error.code,
  message = called.error.message,
}
"#,
            &json!({}),
            None,
        )
        .expect("run host bridge callback error lua");

    assert_eq!(result["ok"], false);
    assert_eq!(result["code"], "host_tool_callback_error");
    assert_eq!(result["message"], "model provider failed");
}

/// Assert that one pooled Lua VM has returned to the neutral request baseline.
/// 断言单个池化 Lua 虚拟机已经回到中性的请求基线状态。
fn assert_vm_scope_is_clean(lua: &mlua::Lua) {
    let context = get_vulcan_context_table(lua).expect("get vulcan.context");
    let request: Table = context.get("request").expect("get request table");
    assert_eq!(request.raw_len(), 0);
    assert_eq!(request.pairs::<String, LuaValue>().count(), 0);
    assert!(matches!(
        context
            .get::<LuaValue>("client_info")
            .expect("get client_info"),
        LuaValue::Nil
    ));
    assert!(matches!(
        context
            .get::<LuaValue>("client_capabilities")
            .expect("get client_capabilities"),
        LuaValue::Table(_)
    ));
    assert!(matches!(
        context
            .get::<LuaValue>("client_budget")
            .expect("get client_budget"),
        LuaValue::Table(_)
    ));
    assert!(matches!(
        context
            .get::<LuaValue>("tool_config")
            .expect("get tool_config"),
        LuaValue::Table(_)
    ));
    assert!(matches!(
        context.get::<LuaValue>("skill_dir").expect("get skill_dir"),
        LuaValue::Nil
    ));
    assert!(matches!(
        context.get::<LuaValue>("entry_dir").expect("get entry_dir"),
        LuaValue::Nil
    ));
    assert!(matches!(
        context
            .get::<LuaValue>("entry_file")
            .expect("get entry_file"),
        LuaValue::Nil
    ));

    let deps = get_vulcan_deps_table(lua).expect("get vulcan.deps");
    assert!(matches!(
        deps.get::<LuaValue>("tools_path").expect("get tools_path"),
        LuaValue::Nil
    ));
    assert!(matches!(
        deps.get::<LuaValue>("lua_path").expect("get lua_path"),
        LuaValue::Nil
    ));
    assert!(matches!(
        deps.get::<LuaValue>("ffi_path").expect("get ffi_path"),
        LuaValue::Nil
    ));

    let internal = get_vulcan_runtime_internal_table(lua).expect("get runtime internal");
    assert!(matches!(
        internal
            .get::<LuaValue>("tool_name")
            .expect("get tool_name"),
        LuaValue::Nil
    ));
    assert!(matches!(
        internal
            .get::<LuaValue>("skill_name")
            .expect("get skill_name"),
        LuaValue::Nil
    ));
    assert!(matches!(
        internal
            .get::<LuaValue>("entry_name")
            .expect("get entry_name"),
        LuaValue::Nil
    ));
    assert!(matches!(
        internal
            .get::<LuaValue>("root_name")
            .expect("get root_name"),
        LuaValue::Nil
    ));
    assert!(
        !internal
            .get::<bool>("luaexec_active")
            .expect("get luaexec_active")
    );
    assert!(matches!(
        internal
            .get::<LuaValue>("luaexec_caller_tool_name")
            .expect("get luaexec_caller_tool_name"),
        LuaValue::Nil
    ));

    let vulcan = get_vulcan_table(lua).expect("get vulcan");
    let lancedb: Table = vulcan.get("lancedb").expect("get lancedb");
    assert!(!lancedb.get::<bool>("enabled").expect("get lancedb enabled"));
    let sqlite: Table = vulcan.get("sqlite").expect("get sqlite");
    assert!(!sqlite.get::<bool>("enabled").expect("get sqlite enabled"));
    assert!(matches!(
        lua.globals()
            .get::<LuaValue>("__runlua_args")
            .expect("get __runlua_args"),
        LuaValue::Nil
    ));
}

/// Verify that skill manifests must not declare skill_id explicitly.
/// 验证 skill 清单不允许再显式声明 skill_id 字段。
#[test]
fn load_from_roots_rejects_explicit_skill_id_field() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_reject_skill_id_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    let skill_dir = skill_root.join("vulcan-codekit");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create runtime dir");
    fs::write(
            skill_dir.join("skill.yaml"),
            "name: vulcan-codekit\nversion: 0.1.0\nskill_id: vulcan-codekit\nentries:\n  - name: ast-tree\n    lua_entry: runtime/test.lua\n    lua_module: vulcan-codekit.ast-tree\n",
        )
        .expect("write skill yaml");
    fs::write(skill_dir.join("runtime").join("test.lua"), "return 'ok'\n")
        .expect("write runtime entry");

    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");

    let error = engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: skill_root,
        }])
        .expect_err("explicit skill_id should be rejected");
    let rendered = error.to_string();
    assert!(rendered.contains("must not declare skill_id"));
    assert!(rendered.contains(&render_host_visible_path(&skill_dir)));

    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify optional dependency manifest probing reports filesystem errors.
/// 验证可选依赖清单探测会报告文件系统错误。
#[test]
fn load_skill_dependency_manifest_reports_probe_errors() {
    // Runtime engine used by the real optional dependency manifest loader.
    // 真实可选依赖清单加载器使用的运行时引擎。
    let engine = make_runtime_test_engine();
    // Skill directory containing one embedded NUL that makes dependencies.yaml impossible to inspect.
    // 包含内嵌 NUL 的 skill 目录，使 dependencies.yaml 无法被探测。
    let invalid_skill_dir = PathBuf::from("invalid\0skill");

    // Error returned before the invalid dependency manifest can behave like a missing manifest.
    // 在非法依赖清单表现得像缺失清单之前返回的错误。
    let error = engine
        .load_skill_dependency_manifest(&invalid_skill_dir)
        .expect_err("invalid dependency manifest probe should fail");

    assert!(
        error.contains("failed to inspect dependency manifest"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("dependencies.yaml"),
        "unexpected error: {}",
        error
    );
}

/// Verify required managed-runtime manifest loading reports filesystem probe errors.
/// 验证必需受管运行时清单加载会报告文件系统探测错误。
///
/// This test has no parameters and fails through assertions when invalid manifest paths are folded into missing manifests.
/// 本测试不接收参数；当非法清单路径被折叠为清单缺失时会通过断言失败。
///
/// Return unit after validating the required manifest loader emits an inspection diagnostic.
/// 校验必需清单加载器输出探测诊断后返回 unit。
#[test]
fn load_current_managed_runtime_manifest_reports_probe_errors() {
    // Real package context used to verify package-relative manifest containment.
    // 用于验证包相对清单包含关系的真实包上下文。
    let runtime_root = make_temp_runtime_root("managed-package-parent-manifest");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "parent-manifest");
    // Parent traversal rejected lexically before any filesystem probe.
    // 在任何文件系统探测前按词法拒绝的父目录穿越。
    let error = package
        .resolve_existing_file("../dependencies.yaml", "dependency manifest")
        .expect_err("parent manifest path must fail");
    assert!(
        error.contains("must be a non-empty safe path"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify optional managed-runtime manifest loading reports filesystem probe errors.
/// 验证可选受管运行时清单加载会报告文件系统探测错误。
///
/// This test has no parameters and fails through assertions when invalid manifest paths are folded into absent manifests.
/// 本测试不接收参数；当非法清单路径被折叠为清单不存在时会通过断言失败。
///
/// Return unit after validating the optional manifest loader emits an inspection diagnostic.
/// 校验可选清单加载器输出探测诊断后返回 unit。
#[test]
fn load_optional_current_managed_runtime_manifest_reports_probe_errors() {
    // Real package context used to verify absolute manifest rejection.
    // 用于验证绝对清单路径拒绝逻辑的真实包上下文。
    let runtime_root = make_temp_runtime_root("managed-package-absolute-manifest");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "absolute-manifest");
    let absolute_manifest = package.package_root().join("dependencies.yaml");
    fs::write(&absolute_manifest, "dependencies: {}\n").expect("write manifest fixture");
    // UTF-8 absolute fixture path supplied to the strict relative-path resolver.
    // 提供给严格相对路径解析器的 UTF-8 绝对夹具路径。
    let absolute_manifest_text = absolute_manifest
        .to_str()
        .expect("test path should be UTF-8");
    let error = package
        .resolve_existing_file(absolute_manifest_text, "dependency manifest")
        .expect_err("absolute manifest path must fail");
    assert!(
        error.contains("must be a non-empty safe path"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify dependency manifest loading rejects directory placeholders.
/// 验证依赖清单加载会拒绝目录占位。
///
/// This test has no parameters and fails through assertions when a directory manifest is treated as a real manifest.
/// 本测试不接收参数；当目录型清单被当作真实清单时会通过断言失败。
///
/// Return unit after validating the optional dependency manifest loader emits a non-file diagnostic.
/// 校验可选依赖清单加载器输出非文件诊断后返回 unit。
#[test]
fn load_skill_dependency_manifest_rejects_directory_manifest_path() {
    // Runtime engine used by the real optional dependency manifest loader.
    // 真实可选依赖清单加载器使用的运行时引擎。
    let engine = make_runtime_test_engine();
    // Temporary skill directory used to isolate the directory manifest fixture.
    // 用于隔离目录型清单夹具的临时 skill 目录。
    let skill_dir = make_temp_runtime_root("dependency-manifest-directory").join("skills/demo");
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = fs::remove_dir_all(&skill_dir);
    // Directory deliberately placed where dependencies.yaml must be one manifest file.
    // 故意放在 dependencies.yaml 必须是清单文件位置上的目录。
    let manifest_dir = skill_dir.join("dependencies.yaml");
    fs::create_dir_all(&manifest_dir).expect("create directory dependency manifest fixture");

    // Error returned before the directory manifest can be parsed or treated as absent.
    // 在目录型清单被解析或被当作不存在之前返回的错误。
    let error = engine
        .load_skill_dependency_manifest(&skill_dir)
        .expect_err("directory dependency manifest should fail");

    assert!(
        error.contains("dependency manifest is not a file"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&manifest_dir)),
        "unexpected error: {}",
        error
    );

    let _ = fs::remove_dir_all(
        skill_dir
            .parent()
            .and_then(Path::parent)
            .expect("test skill dir should have runtime root"),
    );
}

/// Verify dependency preparation reports dependency-manifest probe errors before loading a skill.
/// 验证依赖准备会在加载 skill 前报告依赖清单探测错误。
#[test]
fn ensure_skill_dependencies_reports_manifest_probe_errors() {
    // Runtime engine used by the real dependency preparation helper.
    // 真实依赖准备辅助函数使用的运行时引擎。
    let engine = make_runtime_test_engine();
    // Configured ROOT skill root used only to satisfy the dependency helper signature.
    // 仅用于满足依赖辅助函数签名的 ROOT 技能根配置。
    let skill_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: PathBuf::from("skills"),
    };
    // Skill directory containing one embedded NUL that makes dependencies.yaml impossible to inspect.
    // 包含内嵌 NUL 的 skill 目录，使 dependencies.yaml 无法被探测。
    let invalid_skill_dir = PathBuf::from("invalid\0skill");

    // Error returned before dependency preparation can treat the manifest as absent.
    // 在依赖准备把清单当作不存在之前返回的错误。
    let error = engine
        .ensure_skill_dependencies(&skill_root, &invalid_skill_dir)
        .expect_err("invalid dependency manifest probe should fail during preparation");

    assert!(
        error.contains("failed to inspect dependency manifest"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("dependencies.yaml"),
        "unexpected error: {}",
        error
    );
}

/// Verify `load_single_skill` reports skill manifest probe errors instead of treating them as missing files.
/// 验证 `load_single_skill` 会报告 skill 清单探测错误，而不是把它们当作缺失文件。
#[test]
fn load_single_skill_reports_skill_yaml_probe_errors() {
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine = make_runtime_test_engine();
    // Skill directory containing one embedded NUL that makes skill.yaml impossible to inspect.
    // 包含内嵌 NUL 的 skill 目录，使 skill.yaml 无法被探测。
    let invalid_skill_dir = PathBuf::from("invalid\0skill");

    // Error returned before the invalid manifest can behave like a missing skill.yaml file.
    // 在非法清单表现得像缺失 skill.yaml 文件之前返回的错误。
    let error = engine
        .load_single_skill(&invalid_skill_dir, "ROOT", None)
        .expect_err("invalid skill.yaml probe should fail");

    assert!(
        error.to_string().contains("failed to inspect skill.yaml"),
        "unexpected error: {}",
        error
    );
}

/// Verify `load_single_skill` reports Lua entry probe errors instead of treating them as missing entries.
/// 验证 `load_single_skill` 会报告 Lua 入口探测错误，而不是把它们当作缺失入口。
#[test]
fn load_single_skill_reports_lua_entry_probe_errors() {
    // Temporary runtime root that isolates the invalid Lua entry fixture.
    // 隔离非法 Lua 入口夹具的临时运行时根目录。
    let temp_root = make_temp_runtime_root("lua-entry-probe-error");
    let _ = fs::remove_dir_all(&temp_root);
    // Skill directory whose manifest points at a Lua entry path containing an embedded NUL.
    // 清单指向包含内嵌 NUL 的 Lua 入口路径的 skill 目录。
    let skill_dir = temp_root.join("skills").join("demo-skill");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create invalid Lua entry fixture dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        "name: demo-skill\nversion: 0.1.0\nentries:\n  - name: run\n    lua_entry: \"runtime/run\\0.lua\"\n    lua_module: demo_skill.run\n",
    )
    .expect("write invalid Lua entry manifest");
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine = make_runtime_test_engine();

    // Error returned before the invalid Lua entry can behave like a missing file.
    // 在非法 Lua 入口表现得像缺失文件之前返回的错误。
    let error = engine
        .load_single_skill(&skill_dir, "ROOT", None)
        .expect_err("invalid Lua entry probe should fail");

    assert!(
        error.to_string().contains("failed to inspect Lua entry"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.to_string().contains("demo-skill"),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `load_single_skill` rejects a directory at `skill.yaml` before YAML reading.
/// 验证 `load_single_skill` 会在读取 YAML 前拒绝位于 `skill.yaml` 的目录。
#[test]
fn load_single_skill_rejects_directory_skill_yaml() {
    // Temporary runtime root that isolates the directory manifest fixture.
    // 隔离目录清单夹具的临时运行时根目录。
    let temp_root = make_temp_runtime_root("directory-skill-yaml");
    // Best-effort cleanup for any stale fixture from an interrupted test run.
    // 清理上次中断测试可能留下的旧夹具。
    let _ = fs::remove_dir_all(&temp_root);
    // Skill directory whose `skill.yaml` path is deliberately a directory.
    // `skill.yaml` 路径被有意创建为目录的 skill 目录。
    let skill_dir = temp_root.join("skills").join("demo-skill");
    // Directory occupying the required manifest file path.
    // 占用必需清单文件路径的目录。
    let skill_yaml_dir = skill_dir.join("skill.yaml");
    fs::create_dir_all(&skill_yaml_dir).expect("create directory skill.yaml fixture");
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");

    // Error returned before the directory manifest can fall through to YAML reading.
    // 在目录清单继续进入 YAML 读取之前返回的错误。
    let error = engine
        .load_single_skill(&skill_dir, "ROOT", None)
        .expect_err("directory skill.yaml should fail");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!(
        "skill.yaml is not a file for skill {}: {}",
        render_host_visible_path(&skill_dir),
        render_host_visible_path(&skill_yaml_dir)
    );

    assert!(
        error.to_string().contains(&expected),
        "unexpected error: {}",
        error
    );
    // Best-effort cleanup for the directory manifest fixture.
    // 清理目录清单测试夹具。
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `load_single_skill` rejects a directory at a declared Lua entry before compilation.
/// 验证 `load_single_skill` 会在编译前拒绝声明为 Lua 入口的目录。
#[test]
fn load_single_skill_rejects_directory_lua_entry() {
    // Temporary runtime root that isolates the directory Lua entry fixture.
    // 隔离目录 Lua 入口夹具的临时运行时根目录。
    let temp_root = make_temp_runtime_root("directory-lua-entry");
    // Best-effort cleanup for any stale fixture from an interrupted test run.
    // 清理上次中断测试可能留下的旧夹具。
    let _ = fs::remove_dir_all(&temp_root);
    // Skill directory whose manifest points at a directory instead of a Lua file.
    // 清单指向目录而不是 Lua 文件的 skill 目录。
    let skill_dir = temp_root.join("skills").join("demo-skill");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create directory entry runtime dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        "name: demo-skill\nversion: 0.1.0\nentries:\n  - name: run\n    lua_entry: runtime/run.lua\n    lua_module: demo_skill.run\n",
    )
    .expect("write directory Lua entry manifest");
    // Directory occupying the declared Lua entry file path.
    // 占用已声明 Lua 入口文件路径的目录。
    let lua_entry_dir = skill_dir.join("runtime/run.lua");
    fs::create_dir_all(&lua_entry_dir).expect("create directory Lua entry fixture");
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");

    // Error returned before the directory Lua entry can fall through to compilation.
    // 在目录 Lua 入口继续进入编译之前返回的错误。
    let error = engine
        .load_single_skill(&skill_dir, "ROOT", None)
        .expect_err("directory Lua entry should fail");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!(
        "Lua entry runtime/run.lua is not a file for skill {}: {}",
        render_host_visible_path(&skill_dir),
        render_host_visible_path(&lua_entry_dir)
    );

    assert!(
        error.to_string().contains(&expected),
        "unexpected error: {}",
        error
    );
    // Best-effort cleanup for the directory Lua entry fixture.
    // 清理目录 Lua 入口测试夹具。
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify missing skill manifests render the skill directory through the host-visible formatter.
/// 验证缺失 skill 清单错误会通过宿主可见路径渲染器输出 skill 目录。
#[test]
fn load_single_skill_missing_skill_yaml_error_uses_host_visible_path() {
    // Temporary runtime root that isolates the missing manifest fixture.
    // 隔离缺失清单夹具的临时运行时根目录。
    let temp_root = make_temp_runtime_root("missing-skill-yaml-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Skill root that owns the target skill directory.
    // 拥有目标 skill 目录的 skill 根目录。
    let skill_root = temp_root.join("skills");
    // Skill directory intentionally missing skill.yaml.
    // 有意缺失 skill.yaml 的 skill 目录。
    let skill_dir = skill_root.join("demo-skill");
    fs::create_dir_all(&skill_dir).expect("create missing manifest skill dir");
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");

    // Error returned by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数返回的错误。
    let error = engine
        .load_single_skill(&skill_dir, "ROOT", None)
        .expect_err("missing skill.yaml should fail");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!(
        "skill.yaml not found in {}",
        render_host_visible_path(&skill_dir)
    );

    assert!(
        error.to_string().contains(&expected),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify invalid skill directory names render paths through the host-visible formatter.
/// 验证非法 skill 目录名错误会通过宿主可见路径渲染器输出路径。
#[test]
fn load_single_skill_invalid_skill_directory_error_uses_host_visible_path() {
    // Temporary runtime root that isolates the invalid directory-name fixture.
    // 隔离非法目录名夹具的临时运行时根目录。
    let temp_root = make_temp_runtime_root("invalid-skill-dir-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Skill root that owns the target skill directory.
    // 拥有目标 skill 目录的 skill 根目录。
    let skill_root = temp_root.join("skills");
    // Skill directory whose name fails LuaSkills identifier validation.
    // 目录名无法通过 LuaSkills 标识符校验的 skill 目录。
    let skill_dir = skill_root.join("bad skill");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create invalid-name skill dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        "name: bad skill\nversion: 0.1.0\nentries:\n  - name: run\n    lua_entry: runtime/run.lua\n    lua_module: bad_skill.run\n",
    )
    .expect("write invalid-name manifest");
    fs::write(skill_dir.join("runtime/run.lua"), "return function() end\n")
        .expect("write invalid-name runtime entry");
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");

    // Error returned by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数返回的错误。
    let error = engine
        .load_single_skill(&skill_dir, "ROOT", None)
        .expect_err("invalid skill directory name should fail");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!("skill {}:", render_host_visible_path(&skill_dir));

    assert!(
        error.to_string().contains(&expected),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify missing Lua entry files render the skill directory through the host-visible formatter.
/// 验证缺失 Lua 入口文件错误会通过宿主可见路径渲染器输出 skill 目录。
#[test]
fn load_single_skill_missing_lua_entry_error_uses_host_visible_path() {
    // Temporary runtime root that isolates the missing Lua entry fixture.
    // 隔离缺失 Lua 入口夹具的临时运行时根目录。
    let temp_root = make_temp_runtime_root("missing-lua-entry-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Skill root that owns the target skill directory.
    // 拥有目标 skill 目录的 skill 根目录。
    let skill_root = temp_root.join("skills");
    // Skill directory whose manifest points at a missing Lua entry file.
    // 清单指向缺失 Lua 入口文件的 skill 目录。
    let skill_dir = skill_root.join("demo-skill");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create missing entry skill dir");
    fs::write(
        skill_dir.join("skill.yaml"),
        "name: demo-skill\nversion: 0.1.0\nentries:\n  - name: run\n    lua_entry: runtime/missing.lua\n    lua_module: demo_skill.run\n",
    )
    .expect("write missing entry manifest");
    // Runtime engine used by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数使用的运行时引擎。
    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");

    // Error returned by the real single-skill loading helper.
    // 真实单个 skill 加载辅助函数返回的错误。
    let error = engine
        .load_single_skill(&skill_dir, "ROOT", None)
        .expect_err("missing Lua entry should fail");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!(
        "Lua entry runtime/missing.lua not found in {}",
        render_host_visible_path(&skill_dir)
    );

    assert!(
        error.to_string().contains(&expected),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify Lua tool source read errors render paths through the host-visible formatter.
/// 验证 Lua 工具源码读取错误会通过宿主可见路径渲染器输出路径。
#[test]
fn compile_skill_into_lua_read_error_uses_host_visible_path() {
    // Temporary skill directory used by the real Lua compilation helper.
    // 真实 Lua 编译辅助函数使用的临时 skill 目录。
    let skill_dir = make_temp_runtime_root("compile-skill-read-path").join("skills/demo-skill");
    let _ = fs::remove_dir_all(&skill_dir);
    fs::create_dir_all(&skill_dir).expect("create compile skill fixture dir");
    // Loaded skill metadata whose entry points at runtime/test.lua.
    // 入口指向 runtime/test.lua 的已加载 skill 元数据。
    let mut skill = make_loaded_skill("demo-skill", "demo-skill", "run", "demo_skill.run");
    skill.dir = skill_dir.clone();
    // Tool entry consumed by the real compile helper.
    // 真实编译辅助函数消费的工具入口。
    let tool = skill
        .meta
        .entries()
        .next()
        .expect("test skill should have one entry")
        .clone();
    // Missing Lua source path resolved by the production helper.
    // 生产辅助函数解析出的缺失 Lua 源码路径。
    let lua_path = skill_dir.join("runtime/test.lua");
    // Lua VM passed to the real compile helper.
    // 传给真实编译辅助函数的 Lua 虚拟机。
    let lua = Lua::new();

    // Error returned by the real Lua compilation helper.
    // 真实 Lua 编译辅助函数返回的错误。
    let error = LuaEngine::compile_skill_into_lua(&lua, &skill, &tool, false)
        .expect_err("missing Lua source file should fail compilation");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!("Failed to read {}:", render_host_visible_path(&lua_path));

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(
        skill_dir
            .parent()
            .and_then(Path::parent)
            .expect("test skill dir should have runtime root"),
    );
}

/// Verify that host-ignored skills are skipped before dependency, database, or entry setup.
/// 验证宿主忽略的 skill 会在依赖、数据库与入口初始化之前被跳过。
#[test]
fn load_from_roots_skips_host_ignored_skill_before_resource_setup() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_ignored_skill_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    let skill_dir = skill_root.join("grpc-memory");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create runtime dir");
    fs::write(
            skill_dir.join("skill.yaml"),
            "name: grpc-memory\nversion: 0.1.0\nenable: true\ndebug: false\nsqlite:\n  enable: true\nlancedb:\n  enable: true\nentries:\n  - name: remember\n    lua_entry: runtime/remember.lua\n    lua_module: grpc-memory.remember\n",
        )
        .expect("write skill yaml");
    fs::write(
        skill_dir.join("runtime").join("remember.lua"),
        "return function(args)\n  return 'unexpected-load'\nend\n",
    )
    .expect("write runtime entry");

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        dependency_dir_name: "dependencies".to_string(),
        state_dir_name: "state".to_string(),
        database_dir_name: "databases".to_string(),
        ignored_skill_ids: vec!["grpc-memory".to_string()],
        ..Default::default()
    })
    .expect("create engine");

    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: skill_root,
        }])
        .expect("ignored skill should not fail loading");

    assert!(engine.skills.is_empty());
    assert!(engine.entry_registry.is_empty());
    assert!(!temp_root.join("dependencies").exists());
    assert!(!temp_root.join("state").exists());
    assert!(!temp_root.join("databases").exists());

    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify that colliding `skill-entry` names receive deterministic numeric suffixes.
/// 验证发生冲突的 `skill-entry` 名称会收到稳定且可预测的数字后缀。
#[test]
fn rebuild_entry_registry_appends_numeric_suffixes_for_collisions() {
    let mut skills = HashMap::new();
    skills.insert(
        "alpha".to_string(),
        make_loaded_skill("alpha", "foo-bar", "baz", "alpha_module"),
    );
    skills.insert(
        "beta".to_string(),
        make_loaded_skill("beta", "foo", "bar-baz", "beta_module"),
    );
    skills.insert(
        "gamma".to_string(),
        make_loaded_skill("gamma", "foo-bar", "baz", "gamma_module"),
    );

    let mut engine = make_test_engine(skills);
    engine
        .rebuild_entry_registry()
        .expect("entry registry should rebuild successfully");

    assert!(engine.entry_registry.contains_key("foo-bar-baz"));
    assert!(engine.entry_registry.contains_key("foo-bar-baz-2"));
    assert!(engine.entry_registry.contains_key("foo-bar-baz-3"));

    let alpha_skill = engine
        .skills
        .get("alpha")
        .expect("alpha skill should exist");
    let beta_skill = engine.skills.get("beta").expect("beta skill should exist");
    let gamma_skill = engine
        .skills
        .get("gamma")
        .expect("gamma skill should exist");

    assert_eq!(alpha_skill.resolved_tool_name("baz"), Some("foo-bar-baz"));
    assert_eq!(
        beta_skill.resolved_tool_name("bar-baz"),
        Some("foo-bar-baz-2")
    );
    assert_eq!(gamma_skill.resolved_tool_name("baz"), Some("foo-bar-baz-3"));
}

/// Verify entry-registry rebuild rejects loaded skills whose directory basename is unavailable.
/// 验证入口注册表重建会拒绝目录基名不可用的已加载 skill。
#[test]
fn rebuild_entry_registry_rejects_invalid_loaded_skill_directory_name() {
    let mut skill = make_loaded_skill("alpha", "foo-bar", "baz", "alpha_module");
    // Invalid directory path injected after loading to exercise the registry invariant guard.
    // 加载后注入的非法目录路径，用于覆盖注册表不变量保护。
    skill.dir = PathBuf::new();
    let mut skills = HashMap::new();
    skills.insert("alpha".to_string(), skill);

    let mut engine = make_test_engine(skills);
    // Error returned before canonical entry names are derived from corrupted directory metadata.
    // 在使用损坏目录元数据派生 canonical 入口名前返回的错误。
    let error = engine
        .rebuild_entry_registry()
        .expect_err("invalid loaded skill directory name should fail");

    assert!(
        error.contains("loaded skill 'foo-bar' has invalid directory name"),
        "unexpected error: {}",
        error
    );
}

/// Verify that host-reserved public tool names are treated as occupied during canonical-name generation.
/// 验证宿主保留的公开工具名称会在 canonical 名称生成阶段被视为已占用名称。
#[test]
fn rebuild_entry_registry_skips_host_reserved_names() {
    let mut skills = HashMap::new();
    skills.insert(
        "alpha".to_string(),
        make_loaded_skill("alpha", "vulcan", "help-list", "alpha_module"),
    );

    let mut engine = make_test_engine(skills);
    Arc::get_mut(&mut engine.host_options)
        .expect("host options should be uniquely owned in test")
        .reserved_entry_names = vec!["vulcan-help-list".to_string()];

    engine
        .rebuild_entry_registry()
        .expect("entry registry should rebuild successfully");

    assert!(!engine.entry_registry.contains_key("vulcan-help-list"));
    assert!(engine.entry_registry.contains_key("vulcan-help-list-2"));

    let alpha_skill = engine
        .skills
        .get("alpha")
        .expect("alpha skill should exist");
    assert_eq!(
        alpha_skill.resolved_tool_name("help-list"),
        Some("vulcan-help-list-2")
    );
}

/// Verify that the pooled VM scope guard clears request state even when setup exits early.
/// 验证池化虚拟机作用域守卫即使在安装阶段提前退出也会清理请求状态。
#[test]
fn pooled_vm_scope_guard_cleans_state_after_early_exit() {
    let engine = make_runtime_test_engine();
    let scope_result: Result<(), String> = (|| {
        let mut lease = engine.acquire_vm()?;
        let _scope_guard = LuaVmRequestScopeGuard::new(&mut lease, engine.host_options.as_ref())?;
        let lua = _scope_guard.lua()?;
        LuaEngine::populate_vulcan_request_context(
            lua,
            Some(&crate::runtime_options::LuaInvocationContext::new(
                None,
                json!({"budget":"test"}),
                json!({"tool":"config"}),
            )),
        )?;
        populate_vulcan_internal_execution_context(
            lua,
            &VulcanInternalExecutionContext {
                tool_name: Some("test-tool".to_string()),
                skill_name: Some("test-skill".to_string()),
                entry_name: Some("test".to_string()),
                root_name: Some("ROOT".to_string()),
                luaexec_active: false,
                luaexec_caller_tool_name: None,
            },
        )?;
        let skill_dir = Path::new("D:/runtime-test-root/skills/test-skill");
        let entry_file = Path::new("D:/runtime-test-root/skills/test-skill/runtime/test.lua");
        populate_vulcan_file_context(lua, Some(skill_dir), Some(entry_file))?;
        populate_vulcan_dependency_context(
            lua,
            engine.host_options.as_ref(),
            Some(skill_dir),
            Some("test-skill"),
        )?;
        lua.globals()
            .set(
                "__runlua_args",
                json_to_lua_table(lua, &json!({"stale":"value"})).expect("build runlua args table"),
            )
            .expect("set stale runlua args");
        Err("simulated setup failure".to_string())
    })();
    assert_eq!(
        scope_result.expect_err("scope should fail"),
        "simulated setup failure"
    );

    let lease = engine.acquire_vm().expect("reacquire pooled vm");
    assert_vm_scope_is_clean(lease.lua().expect("lease should own Lua VM"));
}

/// Verify that a pooled VM with broken core tables is discarded before it can be reused.
/// 验证当池化虚拟机的核心表被破坏时，该实例会在复用前被直接丢弃。
#[test]
fn pooled_vm_scope_guard_discards_vm_when_entry_reset_fails() {
    let engine = make_runtime_test_engine();
    {
        let lease = engine.acquire_vm().expect("borrow pooled vm");
        let vulcan =
            get_vulcan_table(lease.lua().expect("lease should own Lua VM")).expect("get vulcan");
        vulcan
            .set("context", LuaValue::Nil)
            .expect("break vulcan.context");
    }

    let mut broken_lease = engine.acquire_vm().expect("reacquire broken pooled vm");
    let error = match LuaVmRequestScopeGuard::new(&mut broken_lease, engine.host_options.as_ref()) {
        Ok(_) => panic!("broken pooled vm should fail normalization"),
        Err(error) => error,
    };
    assert!(error.contains("vulcan.context"));

    let mut fresh_lease = engine.acquire_vm().expect("borrow fresh pooled vm");
    let fresh_scope = LuaVmRequestScopeGuard::new(&mut fresh_lease, engine.host_options.as_ref())
        .expect("normalize fresh pooled vm");
    assert_vm_scope_is_clean(fresh_scope.lua().expect("scope guard should own Lua VM"));
}

/// Verify that cleanup failures retire the current pooled VM instead of returning dirty state.
/// 验证当清理阶段失败时，当前池化虚拟机会被退役，而不是带着脏状态返回池中。
#[test]
fn pooled_vm_scope_guard_discards_vm_when_exit_reset_fails() {
    let engine = make_runtime_test_engine();
    let mut lease = engine.acquire_vm().expect("borrow pooled vm");
    let scope_guard = LuaVmRequestScopeGuard::new(&mut lease, engine.host_options.as_ref())
        .expect("normalize pooled vm");
    let vulcan = get_vulcan_table(scope_guard.lua().expect("scope guard should own Lua VM"))
        .expect("get vulcan");
    vulcan
        .set("context", LuaValue::Nil)
        .expect("break vulcan.context");
    let error = scope_guard
        .finish()
        .expect_err("cleanup should fail after context corruption");
    assert!(error.contains("vulcan.context"));

    let mut fresh_lease = engine.acquire_vm().expect("borrow fresh pooled vm");
    let fresh_scope = LuaVmRequestScopeGuard::new(&mut fresh_lease, engine.host_options.as_ref())
        .expect("normalize fresh pooled vm");
    assert_vm_scope_is_clean(fresh_scope.lua().expect("scope guard should own Lua VM"));
}

/// Verify that run_lua clears transient args after one successful execution.
/// 验证 run_lua 在成功执行后会清理临时参数状态。
#[test]
fn run_lua_clears_args_after_success() {
    let engine = make_runtime_test_engine();
    let result = engine
        .run_lua("return args.value", &json!({"value":"hello"}), None)
        .expect("run_lua should succeed");
    assert_eq!(result, json!("hello"));

    let lease = engine.acquire_vm().expect("reacquire pooled vm");
    assert_vm_scope_is_clean(lease.lua().expect("lease should own Lua VM"));
}

/// Verify `vulcan.json.encode` reports non-JSON Lua values instead of returning empty text.
/// 验证 `vulcan.json.encode` 会报告非 JSON Lua 值，而不是返回空文本。
#[test]
fn run_lua_json_encode_rejects_function_value() {
    let engine = make_runtime_test_engine();
    let error = engine
        .run_lua(
            "return vulcan.json.encode(function() end)",
            &json!({}),
            None,
        )
        .expect_err("json.encode of a function should fail");

    assert!(error.contains("json.encode: Cannot convert Lua function to JSON"));
}

/// Verify `vulcan.json` decode, encode, constructors, null, protection, and copy semantics together.
/// 验证 `vulcan.json` 解码、编码、构造器、null、保护及拷贝语义可协同工作。
#[test]
fn run_lua_json_api_preserves_types_and_uses_protected_shallow_copies() {
    // Runtime engine exposing the production vulcan.json module.
    // 暴露生产 vulcan.json 模块的运行时引擎。
    let engine = make_runtime_test_engine();
    // JSON result combining decoded data and explicitly constructed containers.
    // 组合解码数据与显式构造容器的 JSON 结果。
    let result = engine
        .run_lua(
            r#"
local decoded = vulcan.json.decode([[{
  "object": {},
  "array": [],
  "nested": {"object": {}, "array": []},
  "nullable": null,
  "nullableArray": [null]
}]])
local object_source = { name = "before" }
local array_source = { "one", "two" }
local copied_object = vulcan.json.object(object_source)
local copied_array = vulcan.json.array(array_source)
object_source.name = "after"
array_source[1] = "changed"
return {
  decoded = vulcan.json.decode(vulcan.json.encode(decoded)),
  empty_object = vulcan.json.object(),
  empty_array = vulcan.json.array(),
  copied_object = copied_object,
  copied_array = copied_array,
  object_source = object_source,
  array_source = array_source,
  explicit_null = vulcan.json.null,
  protected_object = getmetatable(copied_object) == false,
  protected_array = getmetatable(copied_array) == false,
}
"#,
            &json!({}),
            None,
        )
        .expect("run JSON fidelity API Lua");

    assert_eq!(
        result["decoded"],
        json!({
            "object": {},
            "array": [],
            "nested": {"object": {}, "array": []},
            "nullable": null,
            "nullableArray": [null]
        })
    );
    assert_eq!(result["empty_object"], json!({}));
    assert_eq!(result["empty_array"], json!([]));
    assert_eq!(result["copied_object"], json!({"name": "before"}));
    assert_eq!(result["copied_array"], json!(["one", "two"]));
    assert_eq!(result["object_source"], json!({"name": "after"}));
    assert_eq!(result["array_source"], json!(["changed", "two"]));
    assert!(result["explicit_null"].is_null());
    assert_eq!(result["protected_object"], true);
    assert_eq!(result["protected_array"], true);
}

/// Verify an ordinary unmarked empty Lua table keeps the historical empty-array encoding.
/// 验证普通无标记 Lua 空表继续保持历史空数组编码行为。
#[test]
fn run_lua_unmarked_empty_table_keeps_legacy_array_semantics() {
    // Runtime engine exercising the compatibility branch of the shared converter.
    // 覆盖共享转换器兼容分支的运行时引擎。
    let engine = make_runtime_test_engine();
    // Unmarked empty table result produced by ordinary Lua syntax.
    // 由普通 Lua 语法产生的无标记空表结果。
    let result = engine
        .run_lua("return {}", &json!({}), None)
        .expect("convert ordinary empty Lua table");

    assert_eq!(result, json!([]));
}

/// Verify marked and inferred JSON containers reject mixed keys, holes, and invalid constructors.
/// 验证已标记及推断 JSON 容器会拒绝混合键、空洞与非法构造器参数。
#[test]
fn run_lua_json_containers_reject_invalid_structures() {
    // Runtime engine reused across independent invalid-container cases.
    // 在多个独立非法容器用例间复用的运行时引擎。
    let engine = make_runtime_test_engine();
    // Lua snippets paired with the stable diagnostic fragment each one must produce.
    // Lua 片段及每个片段必须产生的稳定诊断片段。
    let cases = [
        (
            "return vulcan.json.array({ [1] = 'one', name = 'invalid' })",
            "JSON array table cannot contain string keys",
        ),
        (
            "return vulcan.json.object({ [1] = 'invalid' })",
            "JSON object table cannot contain numeric keys",
        ),
        (
            "return vulcan.json.array({ [1] = 'one', [3] = 'three' })",
            "JSON array table must be contiguous from index 1; missing index 2",
        ),
        (
            "return vulcan.json.object('invalid')",
            "vulcan.json.object expects no argument or exactly one table",
        ),
        (
            "return vulcan.json.array(42)",
            "vulcan.json.array expects no argument or exactly one table",
        ),
        (
            "return vulcan.json.object(nil)",
            "vulcan.json.object expects no argument or exactly one table",
        ),
        (
            "return vulcan.json.array({}, {})",
            "vulcan.json.array expects no argument or exactly one table",
        ),
        (
            "local value = vulcan.json.array(); value.name = 'invalid'; return value",
            "JSON array table cannot contain string keys",
        ),
        (
            "local value = vulcan.json.object(); value[1] = 'invalid'; return value",
            "JSON object table cannot contain numeric keys",
        ),
        (
            "return { [1] = 'one', name = 'invalid' }",
            "JSON array table cannot contain string keys",
        ),
    ];

    for (code, expected_error) in cases {
        // Explicit conversion failure returned by the production RunLua boundary.
        // 由生产 RunLua 边界返回的显式转换失败。
        let error = engine
            .run_lua(code, &json!({}), None)
            .expect_err("invalid JSON container must fail");
        assert!(
            error.contains(expected_error),
            "expected `{expected_error}` in `{error}`"
        );
    }
}

/// Verify the Lua VM pool recovers state access and condition-variable wakeups after lock poisoning.
/// 验证 Lua 虚拟机池在锁 poison 后仍能恢复状态访问和条件变量唤醒。
#[test]
fn lua_vm_pool_recovers_after_poisoned_state_lock_and_wait() {
    // Pool configured with one reserved slot so acquire must wait until a VM is returned.
    // 配置为单个已占用槽位的池，使 acquire 必须等待虚拟机归还。
    let pool = Arc::new(LuaVmPool::new(LuaVmPoolConfig {
        min_size: 1,
        max_size: 1,
        idle_ttl_secs: 60,
    }));

    // Captured panic result from a writer that poisons the pool state while reserving the only slot.
    // 写入者在保留唯一槽位时制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to create the poisoned pool-state scenario for this recovery test.
        // 仅用于为本恢复测试制造池状态 poison 场景的保护对象。
        let mut state = pool.state.lock().expect("initial lua vm pool state lock");
        state.total_count = 1;
        panic!("poison lua vm pool state for recovery test");
    }));

    assert!(poison_result.is_err());

    // Pool clone moved into the notifier that makes the waiting acquire path progress.
    // 移入通知线程的池副本，用于推进等待中的 acquire 路径。
    let notifier_pool = pool.clone();
    // Notifier thread that returns one VM to the poisoned pool state and wakes the waiter.
    // 通知线程会向已 poison 的池状态归还一个虚拟机并唤醒等待方。
    let notifier = thread::spawn(move || {
        thread::sleep(Duration::from_millis(25));
        // Recovered pool state guard used to publish one available VM.
        // 恢复后的池状态保护对象，用于发布一个可用虚拟机。
        let mut state = notifier_pool.lock_state();
        state.available.push(LuaVm {
            lua: unsafe { mlua::Lua::unsafe_new() },
            last_used_at: Instant::now(),
        });
        notifier_pool.condvar.notify_one();
    });

    // Lease acquired through the poisoned wait path; factory must not run because capacity is already reserved.
    // 通过已 poison 的等待路径获取的租约；容量已被保留，因此工厂不应执行。
    let lease = pool
        .acquire(|| Err("factory should not run while one VM is reserved".to_string()))
        .expect("pool should recover and acquire returned VM");

    assert_eq!(pool.total_count(), 1);
    assert!(
        lease
            .lua()
            .expect("lease should own Lua VM")
            .globals()
            .set("__pool_recovered", true)
            .is_ok()
    );
    drop(lease);
    notifier
        .join()
        .expect("poison recovery notifier should finish");
}

/// Verify a retired Lua VM lease reports an explicit error instead of panicking on later Lua access.
/// 验证已退役的 Lua VM 租约在后续 Lua 访问时返回显式错误，而不是触发 panic。
#[test]
fn lua_vm_lease_lua_returns_error_after_discard() {
    let engine = make_runtime_test_engine();
    let mut lease = engine.acquire_vm().expect("borrow pooled vm");

    lease.discard();
    let error = match lease.lua() {
        Ok(_) => panic!("discarded lease should not expose a Lua VM"),
        Err(error) => error,
    };

    assert!(error.contains("pooled Lua VM lease has already been retired"));
}

/// Verify isolated `vulcan.runtime.lua.exec` calls reuse the dedicated runlua VM pool.
/// 验证隔离 `vulcan.runtime.lua.exec` 调用会复用独立的 runlua 虚拟机池。
#[test]
fn execute_runlua_request_inline_reuses_dedicated_pool() {
    let engine = make_runtime_test_engine();
    assert_eq!(engine.runlua_pool.total_count(), 0);

    let first = engine
        .execute_runlua_request_json_inline(r#"{"code":"return 1"}"#)
        .expect("first inline runlua should succeed");
    assert!(!first.trim().is_empty());
    assert_eq!(engine.runlua_pool.total_count(), 1);

    let second = engine
        .execute_runlua_request_json_inline(r#"{"code":"return 2"}"#)
        .expect("second inline runlua should succeed");
    assert!(!second.trim().is_empty());
    assert_eq!(engine.runlua_pool.total_count(), 1);
}

/// Verify runlua return rendering marks invalid UTF-8 strings instead of dropping their content.
/// 验证 runlua 返回值渲染会标记非法 UTF-8 字符串，而不是丢弃内容。
#[test]
fn execute_runlua_request_inline_marks_invalid_utf8_return_string() {
    // Runtime engine used to execute an inline runlua request returning invalid UTF-8 bytes.
    // 用于执行返回非法 UTF-8 字节的内联 runlua 请求的运行时引擎。
    let engine = make_runtime_test_engine();
    // Rendered runlua result produced from one Lua byte string that is not valid UTF-8.
    // 由一个非法 UTF-8 Lua 字节字符串产生的已渲染 runlua 结果。
    let result = engine
        .execute_runlua_request_json_inline(r#"{"code":"return string.char(255)"}"#)
        .expect("inline runlua should render invalid UTF-8 return strings");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("invalid UTF-8 Lua string"));
}

/// Verify file-based luaexec keeps working after the process-wide cwd guard is poisoned.
/// 验证进程级 cwd guard 锁 poison 后，基于文件的 luaexec 仍可继续执行。
#[test]
fn execute_runlua_request_inline_recovers_after_poisoned_cwd_guard() {
    // Captured panic result from a holder that poisons the process-wide runlua cwd guard.
    // 进程级 runlua cwd guard 持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the process-wide runlua cwd lock.
        // 仅用于制造进程级 runlua cwd 锁 poison 的保护对象。
        let _guard = runlua_cwd_guard().lock().expect("initial runlua cwd guard");
        panic!("poison runlua cwd guard for recovery test");
    }));

    assert!(poison_result.is_err());

    // Runtime engine used to execute a file-backed luaexec request after cwd guard recovery.
    // 用于在 cwd guard 恢复后执行文件型 luaexec 请求的运行时引擎。
    let engine = make_runtime_test_engine();
    // Temporary runlua script root used by the file-backed request.
    // 文件型请求使用的临时 runlua 脚本根目录。
    let temp_root = make_temp_runtime_root("runlua-poisoned-cwd-guard");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&temp_root).expect("create poisoned cwd runlua dir");
    // Lua file executed through the production file-backed luaexec path.
    // 通过生产文件型 luaexec 路径执行的 Lua 文件。
    let script_path = temp_root.join("script.lua");
    fs::write(
        &script_path,
        "print('cwd-guard-recovered'); return 'file-ok'\n",
    )
    .expect("write poisoned cwd runlua script");
    // File-backed luaexec JSON request that exercises the cwd guard path.
    // 用于触发 cwd guard 路径的文件型 luaexec JSON 请求。
    let request = json!({
        "file": render_host_visible_path(&script_path)
    });

    // Rendered runlua result produced after the poisoned cwd guard is recovered.
    // 已 poison 的 cwd guard 恢复后产生的 runlua 渲染结果。
    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("file-backed runlua should recover poisoned cwd guard");
    assert!(result.contains("SUCCESS"));
    assert!(result.contains("cwd-guard-recovered"));
    assert!(result.contains("file-ok"));

    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify file-based luaexec read errors use host-visible path rendering once.
/// 验证文件型 luaexec 读取错误会使用宿主可见路径渲染且只输出一次底层错误。
#[test]
fn execute_runlua_request_inline_file_read_error_uses_host_visible_path() {
    // Runtime engine used to execute the missing file-backed luaexec request.
    // 用于执行缺失文件型 luaexec 请求的运行时引擎。
    let engine = make_runtime_test_engine();
    // Temporary root that isolates the missing luaexec file path.
    // 隔离缺失 luaexec 文件路径的临时根目录。
    let temp_root = make_temp_runtime_root("runlua-missing-file-path");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&temp_root).expect("create missing file runlua dir");
    // Missing Lua file path consumed by the production file-backed luaexec resolver.
    // 生产文件型 luaexec 解析器消费的缺失 Lua 文件路径。
    let missing_path = temp_root.join("missing.lua");
    let _ = fs::remove_file(&missing_path);
    // File-backed luaexec JSON request that exercises the read_to_string error branch.
    // 用于触发 read_to_string 错误分支的文件型 luaexec JSON 请求。
    let request = json!({
        "file": render_host_visible_path(&missing_path)
    });

    // Error returned by the real inline runlua request entrypoint.
    // 真实进程内 runlua 请求入口返回的错误。
    let error = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect_err("missing luaexec file should fail before execution");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to read luaexec file {}:",
        render_host_visible_path(&missing_path)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    assert_eq!(
        error.matches("os error").count(),
        1,
        "unexpected duplicated OS error text: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify runlua print capture remains writable and readable after its lock is poisoned.
/// 验证 runlua print 捕获锁 poison 后仍可继续写入和读取。
#[test]
fn runlua_print_capture_recovers_after_poisoned_lock() {
    // Shared print-capture buffer used to mimic one isolated runlua execution.
    // 用于模拟单次隔离 runlua 执行的共享 print 捕获缓冲区。
    let captured_output = Arc::new(Mutex::new(Vec::<String>::new()));
    // Captured panic result from a holder that poisons only the print-capture buffer.
    // 单个 print 捕获缓冲区锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the runlua print-capture lock.
        // 仅用于制造 runlua print 捕获锁 poison 的保护对象。
        let _guard = captured_output
            .lock()
            .expect("initial runlua print capture lock");
        panic!("poison runlua print capture for recovery test");
    }));

    assert!(poison_result.is_err());

    {
        // Recovered print-capture guard used to append one captured print line.
        // 用于追加单条 print 捕获行的已恢复 print 捕获保护对象。
        let mut recovered_capture = lock_runlua_print_capture(&captured_output);
        recovered_capture.push("after-poison".to_string());
    }

    // Captured output cloned back through the same recovery helper.
    // 通过同一个恢复辅助函数回读克隆出的捕获输出。
    let captured = lock_runlua_print_capture(&captured_output).clone();
    assert_eq!(captured, vec!["after-poison".to_string()]);
}

/// Verify isolated runlua redirects Lua `io.open` to the Rust-backed managed IO table.
/// 验证隔离 runlua 会把 Lua `io.open` 重定向到 Rust 托管 IO 表。
#[test]
fn execute_runlua_request_inline_uses_managed_io_open() {
    let engine = make_runtime_test_engine();
    let path = std::env::temp_dir().join(format!(
        "luaskills_runlua_managed_io_{}.txt",
        std::process::id()
    ));
    fs::write(&path, "managed-io-ok").expect("write managed io test file");
    let request = json!({
        "code": "local f = io.open(args.path, 'r'); local value = f:read('*a'); f:close(); return value",
        "args": {
            "path": render_host_visible_path(&path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should read through managed io");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("managed-io-ok"));
    let _ = fs::remove_file(path);
}

/// Verify isolated runlua supports default managed `io.input` and `io.read`.
/// 验证隔离 runlua 支持托管默认 `io.input` 与 `io.read`。
#[test]
fn execute_runlua_request_inline_uses_managed_io_default_input() {
    let engine = make_runtime_test_engine();
    let path = std::env::temp_dir().join(format!(
        "luaskills_runlua_managed_io_input_{}.txt",
        std::process::id()
    ));
    fs::write(&path, "managed-default-input").expect("write managed input test file");
    let request = json!({
        "code": "io.input(args.path); return io.read('*a')",
        "args": {
            "path": render_host_visible_path(&path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should read through managed default input");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("managed-default-input"));
    let _ = fs::remove_file(path);
}

/// Verify isolated runlua supports default managed `io.output` and `io.write`.
/// 验证隔离 runlua 支持托管默认 `io.output` 与 `io.write`。
#[test]
fn execute_runlua_request_inline_uses_managed_io_default_output() {
    let engine = make_runtime_test_engine();
    let path = std::env::temp_dir().join(format!(
        "luaskills_runlua_managed_io_output_{}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    let request = json!({
        "code": "io.output(args.path); io.write('managed', '-', 'default-output'); io.close(); return vulcan.io.read_text(args.path, { encoding = 'utf-8' })",
        "args": {
            "path": render_host_visible_path(&path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should write through managed default output");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("managed-default-output"));
    let _ = fs::remove_file(path);
}

/// Verify `vulcan.fs.list` reports non-UTF-8 directory entries with one host-visible directory path.
/// 验证 `vulcan.fs.list` 会用宿主可见目录路径报告非 UTF-8 目录项。
#[cfg(target_os = "linux")]
#[test]
fn execute_runlua_request_inline_fs_list_non_utf8_entry_error_uses_host_visible_path() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-list-non-utf8-entry");
    let invalid_name = std::ffi::OsString::from_vec(vec![0xff, b'.', b'l', b'u', b'a']);
    let invalid_path = temp_root.join(&invalid_name);
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&temp_root).expect("create non-UTF-8 list test directory");
    fs::write(&invalid_path, "non-utf8-name").expect("write non-UTF-8 list test file");
    let expected_dir = render_host_visible_path(&temp_root);
    let request = json!({
        "code": "return vulcan.json.encode(vulcan.fs.list(args.path))",
        "args": {
            "path": expected_dir
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should surface non-UTF-8 vulcan.fs.list entry errors");

    assert!(result.contains("FAILED"));
    assert!(result.contains("fs.list: non-UTF-8 file name under"));
    assert!(result.contains(&expected_dir));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.rename` supports Unicode paths without depending on native `os.rename`.
/// 验证 `vulcan.fs.rename` 支持 Unicode 路径，并且不依赖原生 `os.rename`。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_rename_with_unicode_paths() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-rename-unicode");
    let source_dir = temp_root.join("中文目录");
    let source_path = source_dir.join("旧名字.lua");
    let target_path = source_dir.join("新名字.lua");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_dir).expect("create unicode rename test dir");
    fs::write(&source_path, "rename-unicode-ok").expect("write unicode rename source file");
    let request = json!({
        "code": "local renamed = vulcan.fs.rename(args.old_path, args.new_path); return tostring(renamed) .. '|' .. tostring(vulcan.fs.exists(args.old_path)) .. '|' .. tostring(vulcan.fs.exists(args.new_path))",
        "args": {
            "old_path": render_host_visible_path(&source_path),
            "new_path": render_host_visible_path(&target_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should rename unicode path through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true|false|true"));
    assert!(!source_path.exists());
    assert_eq!(
        fs::read_to_string(&target_path).expect("read renamed unicode target file"),
        "rename-unicode-ok"
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.mkdir` can create nested Unicode directories recursively.
/// 验证 `vulcan.fs.mkdir` 能够递归创建嵌套 Unicode 目录。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_mkdir_recursive_with_unicode_paths() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-mkdir-unicode");
    let target_path = temp_root.join("一级中文目录").join("二级中文目录");
    let _ = fs::remove_dir_all(&temp_root);
    let request = json!({
        "code": "local created = vulcan.fs.mkdir(args.path, { recursive = true }); return tostring(created) .. '|' .. tostring(vulcan.fs.is_dir(args.path))",
        "args": {
            "path": render_host_visible_path(&target_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should create unicode directories through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true|true"));
    assert!(target_path.is_dir());
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.mkdir` target probing reports invalid paths before creation.
/// 验证 `vulcan.fs.mkdir` 目标探测会在创建前报告非法路径。
#[test]
fn vulcan_fs_mkdir_target_status_reports_invalid_target_probe() {
    // Invalid mkdir target path that the filesystem metadata API cannot inspect.
    // 文件系统元数据 API 无法探测的非法 mkdir 目标路径。
    let invalid_path = PathBuf::from("invalid\0mkdir-target");

    // Error returned before the invalid target can behave like a missing directory.
    // 在非法目标表现得像缺失目录之前返回的错误。
    let error = super::vulcan_fs_mkdir_target_status(&invalid_path)
        .expect_err("invalid mkdir target probe should fail");

    assert!(
        error.contains("fs.mkdir: failed to inspect"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
}

/// Verify `vulcan.fs.remove` can delete Unicode directory trees recursively.
/// 验证 `vulcan.fs.remove` 能够递归删除 Unicode 目录树。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_remove_recursive_with_unicode_paths() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-remove-unicode");
    let target_path = temp_root.join("待删除中文目录");
    let nested_path = target_path.join("子目录");
    let nested_file = nested_path.join("内容.lua");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&nested_path).expect("create unicode remove nested dir");
    fs::write(&nested_file, "remove-unicode-ok").expect("write unicode remove nested file");
    let request = json!({
        "code": "local removed = vulcan.fs.remove(args.path, { recursive = true }); return tostring(removed) .. '|' .. tostring(vulcan.fs.exists(args.path))",
        "args": {
            "path": render_host_visible_path(&target_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should remove unicode directory through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true|false"));
    assert!(!target_path.exists());
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.remove` deletes one symlink entry itself instead of treating it as missing after the target disappears.
/// 验证 `vulcan.fs.remove` 会删除符号链接条目本身，而不是在目标消失后把它误判为缺失。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_remove_for_dangling_symlink_entries() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-remove-dangling-symlink");
    let target_dir = temp_root.join("符号链接目录");
    let target_path = target_dir.join("目标文件.txt");
    let link_path = target_dir.join("悬空链接.txt");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create dangling symlink test dir");
    fs::write(&target_path, "dangling-symlink-ok").expect("write dangling symlink target file");
    if !create_test_file_symlink(&link_path, &target_path) {
        let _ = fs::remove_dir_all(&temp_root);
        return;
    }
    fs::remove_file(&target_path).expect("remove symlink target file");
    let request = json!({
        "code": "local removed = vulcan.fs.remove(args.path); return tostring(removed) .. '|' .. tostring(vulcan.fs.exists(args.path))",
        "args": {
            "path": render_host_visible_path(&link_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should remove dangling symlink entries through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true|false"));
    assert!(!link_path.exists());
    let link_metadata =
        fs::symlink_metadata(&link_path).expect_err("dangling symlink path should be gone");
    assert_eq!(link_metadata.kind(), std::io::ErrorKind::NotFound);
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify Unix millisecond conversion keeps normal post-epoch metadata timestamps.
/// 验证 Unix 毫秒转换会保留正常的 epoch 之后元数据时间戳。
#[test]
fn system_time_to_unix_millis_i64_accepts_post_epoch_time() {
    // Timestamp one millisecond after the Unix epoch.
    // Unix epoch 之后一毫秒的时间戳。
    let timestamp = std::time::UNIX_EPOCH + Duration::from_millis(1);

    assert_eq!(
        system_time_to_unix_millis_i64(timestamp, "test modified time")
            .expect("post-epoch timestamp should convert"),
        1
    );
}

/// Verify Unix millisecond conversion rejects pre-epoch metadata timestamps.
/// 验证 Unix 毫秒转换会拒绝早于 epoch 的元数据时间戳。
#[test]
fn system_time_to_unix_millis_i64_rejects_pre_epoch_time() {
    // Timestamp one millisecond before the Unix epoch.
    // Unix epoch 之前一毫秒的时间戳。
    let timestamp = std::time::UNIX_EPOCH - Duration::from_millis(1);

    // Error returned for a pre-epoch timestamp conversion attempt.
    // 早于 epoch 的时间戳转换尝试返回的错误。
    let error = system_time_to_unix_millis_i64(timestamp, "test modified time")
        .expect_err("pre-epoch timestamp should fail");

    assert!(
        error.starts_with(
            "test modified time is before Unix epoch and cannot be represented as modified_unix_ms:"
        ),
        "unexpected error: {}",
        error
    );
}

/// Verify `vulcan.fs.stat` returns structured metadata for Unicode file paths.
/// 验证 `vulcan.fs.stat` 会为 Unicode 文件路径返回结构化元数据。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_stat_with_unicode_paths() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-stat-unicode");
    let target_dir = temp_root.join("中文信息目录");
    let target_path = target_dir.join("信息.lua");
    let file_content = "stat-file-size";
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create unicode stat dir");
    fs::write(&target_path, file_content).expect("write unicode stat file");
    let request = json!({
        "code": "return vulcan.json.encode(vulcan.fs.stat(args.path))",
        "args": {
            "path": render_host_visible_path(&target_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should stat unicode file through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("\"kind\":\"file\""));
    assert!(result.contains("\"is_file\":true"));
    assert!(result.contains("\"is_dir\":false"));
    assert!(result.contains("\"is_symlink\":false"));
    assert!(result.contains("\"readonly\":false"));
    assert!(result.contains(&format!("\"size\":{}", file_content.len())));
    assert!(result.contains("\"modified_unix_ms\":"));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.exists` and `vulcan.fs.is_dir` probe errors do not become false.
/// 验证 `vulcan.fs.exists` 与 `vulcan.fs.is_dir` 的探测错误不会变成 false。
#[test]
fn vulcan_fs_target_boolean_helpers_report_probe_errors() {
    // Target path containing one embedded NUL that filesystem metadata cannot inspect.
    // 包含一个内嵌 NUL 的目标路径，文件系统元数据无法探测该路径。
    let invalid_path = PathBuf::from("invalid\0path");
    // Existence error returned before the invalid path can behave like a missing target.
    // 在非法路径表现得像目标缺失之前返回的存在性错误。
    let exists_error = vulcan_fs_target_exists(&invalid_path, "fs.exists")
        .expect_err("invalid fs.exists metadata probe should fail");
    // Directory classification error returned before the invalid path can behave like a non-directory.
    // 在非法路径表现得像非目录之前返回的目录分类错误。
    let is_dir_error = vulcan_fs_target_is_dir(&invalid_path, "fs.is_dir")
        .expect_err("invalid fs.is_dir metadata probe should fail");

    assert!(exists_error.contains("fs.exists: failed to inspect"));
    assert!(exists_error.contains("invalid"));
    assert!(is_dir_error.contains("fs.is_dir: failed to inspect"));
    assert!(is_dir_error.contains("invalid"));
}

/// Verify `vulcan.fs.copy` honors the explicit overwrite option on Unicode file paths.
/// 验证 `vulcan.fs.copy` 会在 Unicode 文件路径上遵循显式 overwrite 选项。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_copy_with_overwrite_control() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-copy-unicode");
    let source_dir = temp_root.join("复制目录");
    let source_path = source_dir.join("源文件.lua");
    let target_path = source_dir.join("目标文件.lua");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_dir).expect("create unicode copy dir");
    fs::write(&source_path, "copy-source-content").expect("write unicode copy source");
    let request = json!({
        "code": "local first = vulcan.fs.copy(args.src_path, args.dst_path); local second = vulcan.fs.copy(args.src_path, args.dst_path); local third = vulcan.fs.copy(args.src_path, args.dst_path, { overwrite = true }); return tostring(first) .. '|' .. tostring(second) .. '|' .. tostring(third)",
        "args": {
            "src_path": render_host_visible_path(&source_path),
            "dst_path": render_host_visible_path(&target_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should copy unicode file through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true|false|true"));
    assert_eq!(
        fs::read_to_string(&target_path).expect("read copied unicode target file"),
        "copy-source-content"
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.copy` treats one dangling destination symlink as an existing path entry for overwrite checks.
/// 验证 `vulcan.fs.copy` 在 overwrite 校验中会把悬空目标符号链接当作已存在的路径条目处理。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_copy_with_dangling_symlink_destination() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-copy-dangling-symlink-target");
    let source_dir = temp_root.join("复制目录");
    let source_path = source_dir.join("源文件.lua");
    let missing_target_path = source_dir.join("缺失目标.lua");
    let dangling_link_path = source_dir.join("悬空目标链接.lua");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_dir).expect("create dangling symlink copy dir");
    fs::write(&source_path, "copy-dangling-link-content")
        .expect("write dangling symlink copy source");
    fs::write(&missing_target_path, "stale-target").expect("write dangling symlink real target");
    if !create_test_file_symlink(&dangling_link_path, &missing_target_path) {
        let _ = fs::remove_dir_all(&temp_root);
        return;
    }
    fs::remove_file(&missing_target_path).expect("remove dangling symlink real target");
    let request = json!({
        "code": "local first = vulcan.fs.copy(args.src_path, args.dst_path); local second = vulcan.fs.copy(args.src_path, args.dst_path, { overwrite = true }); return tostring(first) .. '|' .. tostring(second)",
        "args": {
            "src_path": render_host_visible_path(&source_path),
            "dst_path": render_host_visible_path(&dangling_link_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should honor overwrite checks for dangling symlink destinations");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("false|true"));
    assert!(!missing_target_path.exists());
    let target_metadata =
        fs::symlink_metadata(&dangling_link_path).expect("read copied dangling target metadata");
    assert!(!target_metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_to_string(&dangling_link_path).expect("read replaced dangling target file"),
        "copy-dangling-link-content"
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.copy` destination ancestor probe errors do not behave like missing parents.
/// 验证 `vulcan.fs.copy` 目标祖先探测错误不会表现得像父级缺失。
#[test]
fn vulcan_fs_copy_effective_destination_reports_ancestor_probe_errors() {
    // Destination path whose parent contains one embedded NUL that filesystem metadata cannot inspect.
    // 父级包含内嵌 NUL 的目标路径，文件系统元数据无法探测该父级。
    let target_path = PathBuf::from("invalid\0parent").join("child.txt");
    // Error returned before the invalid parent can be treated as a merely missing ancestor.
    // 在非法父级被当作单纯缺失祖先之前返回的错误。
    let error = resolve_vulcan_fs_copy_effective_destination_path(&target_path, true)
        .expect_err("invalid destination ancestor probe should fail");

    assert!(error.contains("fs.copy: failed to inspect destination ancestor"));
    assert!(error.contains("invalid"));
}

/// Verify `vulcan.fs.copy` can recursively copy Unicode directory trees and replace the destination tree on overwrite.
/// 验证 `vulcan.fs.copy` 能递归复制 Unicode 目录树，并在 overwrite 时整体替换目标目录树。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_copy_directory_tree_with_overwrite_control() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-copy-tree-unicode");
    let source_dir = temp_root.join("源目录");
    let source_nested_dir = source_dir.join("一级子目录").join("二级子目录");
    let target_dir = temp_root.join("目标目录");
    let target_extra_file = target_dir.join("待替换.txt");
    let source_root_file = source_dir.join("根文件.txt");
    let source_nested_file = source_nested_dir.join("深层文件.lua");
    let target_nested_file = target_dir
        .join("一级子目录")
        .join("二级子目录")
        .join("深层文件.lua");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_nested_dir).expect("create unicode source tree");
    fs::write(&source_root_file, "root-v1").expect("write unicode source root file");
    fs::write(&source_nested_file, "nested-v1").expect("write unicode source nested file");
    let request = json!({
        "code": "local first = vulcan.fs.copy(args.src_path, args.dst_path); vulcan.fs.write(vulcan.path.join(args.dst_path, '待替换.txt'), 'stale-target'); vulcan.fs.write(vulcan.path.join(args.src_path, '根文件.txt'), 'root-v2'); vulcan.fs.write(vulcan.path.join(args.src_path, '一级子目录', '二级子目录', '深层文件.lua'), 'nested-v2'); vulcan.fs.write(vulcan.path.join(args.src_path, '新增文件.txt'), 'new-file'); local second = vulcan.fs.copy(args.src_path, args.dst_path); local third = vulcan.fs.copy(args.src_path, args.dst_path, { overwrite = true }); return tostring(first) .. '|' .. tostring(second) .. '|' .. tostring(third)",
        "args": {
            "src_path": render_host_visible_path(&source_dir),
            "dst_path": render_host_visible_path(&target_dir)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should recursively copy unicode directory tree through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true|false|true"));
    assert_eq!(
        fs::read_to_string(target_dir.join("根文件.txt")).expect("read copied target root file"),
        "root-v2"
    );
    assert_eq!(
        fs::read_to_string(&target_nested_file).expect("read copied target nested file"),
        "nested-v2"
    );
    assert_eq!(
        fs::read_to_string(target_dir.join("新增文件.txt")).expect("read copied target new file"),
        "new-file"
    );
    assert!(!target_extra_file.exists());
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.copy` rejects directory targets nested under the source tree.
/// 验证 `vulcan.fs.copy` 会拒绝把目录目标放到源目录树内部。
#[test]
fn execute_runlua_request_inline_rejects_vulcan_fs_copy_directory_into_own_child() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-copy-tree-nested-target");
    let source_dir = temp_root.join("源目录");
    let source_nested_dir = source_dir.join("子目录");
    let target_dir = source_dir.join("复制目标");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_nested_dir).expect("create unicode nested source tree");
    fs::write(source_nested_dir.join("内容.lua"), "nested-target-guard")
        .expect("write unicode nested source file");
    let request = json!({
        "code": "return tostring(vulcan.fs.copy(args.src_path, args.dst_path, { overwrite = true }))",
        "args": {
            "src_path": render_host_visible_path(&source_dir),
            "dst_path": render_host_visible_path(&target_dir)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect(
            "inline runlua should render one failed result for nested vulcan.fs.copy destination",
        );

    assert!(result.contains("FAILED"));
    assert!(result.contains("destination directory must not be inside source directory"));
    assert!(!target_dir.exists());
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.copy` rejects one real directory source when the destination resolves back into that tree through one symlinked parent.
/// 验证当目标通过父级符号链接回落到真实源目录树内部时，`vulcan.fs.copy` 会拒绝复制。
#[test]
fn execute_runlua_request_inline_rejects_vulcan_fs_copy_directory_via_symlinked_target_parent() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-copy-tree-symlink-parent");
    let source_dir = temp_root.join("真实源目录");
    let source_nested_dir = source_dir.join("子目录");
    let alias_dir = temp_root.join("源目录别名");
    let effective_target_dir = source_dir.join("复制目标");
    let requested_target_dir = alias_dir.join("复制目标");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_nested_dir).expect("create symlinked parent source tree");
    fs::write(source_nested_dir.join("内容.lua"), "symlink-parent-guard")
        .expect("write symlinked parent source file");
    if !create_test_dir_symlink(&alias_dir, &source_dir) {
        let _ = fs::remove_dir_all(&temp_root);
        return;
    }
    let request = json!({
        "code": "return tostring(vulcan.fs.copy(args.src_path, args.dst_path, { overwrite = true }))",
        "args": {
            "src_path": render_host_visible_path(&source_dir),
            "dst_path": render_host_visible_path(&requested_target_dir)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should reject symlinked-parent vulcan.fs.copy destination");

    assert!(result.contains("FAILED"));
    assert!(result.contains("destination directory must not be inside source directory"));
    assert!(!effective_target_dir.exists());
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.fs.copy` rejects one symlinked source directory when the effective destination is nested under the real tree.
/// 验证当符号链接源目录解析后真实目标落在同一目录树内部时，`vulcan.fs.copy` 会拒绝复制。
#[test]
fn execute_runlua_request_inline_rejects_vulcan_fs_copy_directory_via_symlinked_source_alias() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-copy-tree-symlink-source");
    let source_dir = temp_root.join("真实源目录");
    let source_nested_dir = source_dir.join("子目录");
    let source_alias_dir = temp_root.join("源目录别名");
    let target_dir = source_dir.join("复制目标");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&source_nested_dir).expect("create symlinked source tree");
    fs::write(source_nested_dir.join("内容.lua"), "symlink-source-guard")
        .expect("write symlinked source file");
    if !create_test_dir_symlink(&source_alias_dir, &source_dir) {
        let _ = fs::remove_dir_all(&temp_root);
        return;
    }
    let request = json!({
        "code": "return tostring(vulcan.fs.copy(args.src_path, args.dst_path, { overwrite = true }))",
        "args": {
            "src_path": render_host_visible_path(&source_alias_dir),
            "dst_path": render_host_visible_path(&target_dir)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should reject symlinked-source vulcan.fs.copy destination");

    assert!(result.contains("FAILED"));
    assert!(result.contains("destination directory must not be inside source directory"));
    assert!(!target_dir.exists());
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify missing `vulcan.fs.stat` targets return `nil` instead of a runtime error.
/// 验证缺失的 `vulcan.fs.stat` 目标会返回 `nil`，而不是运行时错误。
#[test]
fn execute_runlua_request_inline_returns_nil_for_missing_vulcan_fs_stat() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-stat-missing");
    let missing_path = temp_root.join("不存在目录").join("不存在.lua");
    let request = json!({
        "code": "return tostring(vulcan.fs.stat(args.path) == nil)",
        "args": {
            "path": render_host_visible_path(&missing_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should return nil for missing vulcan.fs.stat target");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("true"));
}

/// Verify `vulcan.fs.write_bytes` and `vulcan.fs.read_bytes` round-trip Base64 payloads on Unicode paths.
/// 验证 `vulcan.fs.write_bytes` 与 `vulcan.fs.read_bytes` 能在 Unicode 路径上往返 Base64 载荷。
#[test]
fn execute_runlua_request_inline_supports_vulcan_fs_byte_roundtrip() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-fs-bytes-unicode");
    let target_dir = temp_root.join("二进制目录");
    let target_path = target_dir.join("原始数据.bin");
    let payload = vec![0_u8, 1_u8, 2_u8, 0xff_u8, 0x80_u8, b'A'];
    let payload_base64 = BASE64_STANDARD.encode(&payload);
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create unicode bytes dir");
    let request = json!({
        "code": "local wrote = vulcan.fs.write_bytes(args.path, args.base64); local echoed = vulcan.fs.read_bytes(args.path); return tostring(wrote) .. '|' .. echoed",
        "args": {
            "path": render_host_visible_path(&target_path),
            "base64": payload_base64
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should roundtrip base64 bytes through vulcan.fs");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains(&format!("true|{}", payload_base64)));
    assert_eq!(
        fs::read(&target_path).expect("read written unicode bytes file"),
        payload
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.path.*` helpers expose stable basename, stem, extension, dirname, normalize, and absolute-path behavior.
/// 验证 `vulcan.path.*` 辅助函数会暴露稳定的 basename、stem、extension、dirname、normalize 与绝对路径判断行为。
#[test]
fn execute_runlua_request_inline_supports_vulcan_path_helpers() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-path-helpers");
    let target_dir = temp_root.join("中文目录");
    let file_path = target_dir.join("example.test.lua");
    let messy_path = target_dir
        .join("子目录")
        .join("..")
        .join("example.test.lua");
    let request = json!({
        "code": "return vulcan.json.encode({ dirname = vulcan.path.dirname(args.file_path), basename = vulcan.path.basename(args.file_path), stem = vulcan.path.stem(args.file_path), extname = vulcan.path.extname(args.file_path), normalized = vulcan.path.normalize(args.messy_path), is_abs = vulcan.path.is_abs(args.file_path) })",
        "args": {
            "file_path": render_host_visible_path(&file_path),
            "messy_path": render_host_visible_path(&messy_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should expose vulcan.path helpers");

    let expected_dirname =
        serde_json::to_string(&render_host_visible_path(&target_dir)).expect("json dirname");
    let expected_normalized =
        serde_json::to_string(&render_host_visible_path(&file_path)).expect("json normalized");
    assert!(result.contains("SUCCESS"));
    assert!(result.contains(&format!("\"dirname\":{}", expected_dirname)));
    assert!(result.contains("\"basename\":\"example.test.lua\""));
    assert!(result.contains("\"stem\":\"example.test\""));
    assert!(result.contains("\"extname\":\".lua\""));
    assert!(result.contains(&format!("\"normalized\":{}", expected_normalized)));
    assert!(result.contains("\"is_abs\":true"));
}

/// Verify `vulcan.process.launchers` reports one default shell and one shell-name list that includes it.
/// 验证 `vulcan.process.launchers` 会返回一个默认 shell，以及包含该默认值的 shell 名称列表。
#[test]
fn execute_runlua_request_inline_reports_vulcan_process_launchers_with_default_shell() {
    let engine = make_runtime_test_engine();
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"return vulcan.json.encode(vulcan.process.launchers())"}"#,
        )
        .expect("inline runlua should expose vulcan.process.launchers");

    assert!(result.contains("SUCCESS"));
    #[cfg(windows)]
    {
        assert!(result.contains("\"default\":\"cmd\""));
        assert!(result.contains("\"shells\":[\"cmd\""));
    }
    #[cfg(not(windows))]
    {
        assert!(result.contains("\"default\":\"sh\""));
        assert!(result.contains("\"shells\":[\"sh\""));
    }
}

/// Verify `vulcan.process.launchers` discovers PATH-provided Unix-like shell launchers such as `bash` and `zsh`.
/// 验证 `vulcan.process.launchers` 会发现通过 PATH 提供的类 Unix shell 启动器，例如 `bash` 与 `zsh`。
#[test]
fn execute_runlua_request_inline_detects_vulcan_process_launchers_from_path() {
    let _env_guard = process_env_test_guard();
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-process-launchers-path");
    let target_dir = temp_root.join("path-bin");
    #[cfg(windows)]
    let bash_launcher_path = target_dir.join("bash.cmd");
    #[cfg(not(windows))]
    let bash_launcher_path = target_dir.join("bash");
    #[cfg(windows)]
    let zsh_launcher_path = target_dir.join("zsh.cmd");
    #[cfg(not(windows))]
    let zsh_launcher_path = target_dir.join("zsh");
    let _restore_guard = {
        #[cfg(windows)]
        {
            TestEnvRestoreGuard::capture("PATH").and_capture("PATHEXT")
        }
        #[cfg(not(windows))]
        {
            TestEnvRestoreGuard::capture("PATH")
        }
    };
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create launcher discovery path dir");
    #[cfg(windows)]
    fs::write(&bash_launcher_path, "@echo off\r\necho fake-bash\r\n")
        .expect("write fake bash launcher");
    #[cfg(not(windows))]
    fs::write(&bash_launcher_path, "#!/bin/sh\nprintf fake-bash\n")
        .expect("write fake bash launcher");
    #[cfg(windows)]
    fs::write(&zsh_launcher_path, "@echo off\r\necho fake-zsh\r\n")
        .expect("write fake zsh launcher");
    #[cfg(not(windows))]
    fs::write(&zsh_launcher_path, "#!/bin/sh\nprintf fake-zsh\n").expect("write fake zsh launcher");
    mark_test_program_executable(&bash_launcher_path);
    mark_test_program_executable(&zsh_launcher_path);
    unsafe { std::env::set_var("PATH", target_dir.as_os_str()) };
    #[cfg(windows)]
    unsafe {
        std::env::set_var("PATHEXT", ".CMD;.EXE;.BAT;.COM");
    }
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"return vulcan.json.encode(vulcan.process.launchers())"}"#,
        )
        .expect("inline runlua should discover PATH-provided process launchers");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("\"bash\""));
    assert!(result.contains("\"zsh\""));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify shell launchers build stable command-carrier argument sequences for command mode.
/// 验证各类 shell 启动器会为命令模式构造稳定的命令承载参数序列。
#[test]
fn process_exec_shell_launchers_build_expected_command_args() {
    let command_text = "printf launcher-check";
    assert_eq!(
        ExecShellLauncher::Cmd.command_args(command_text),
        vec![String::from("/C"), command_text.to_string()]
    );
    assert_eq!(
        ExecShellLauncher::Pwsh.command_args(command_text),
        vec![
            String::from("-NoProfile"),
            String::from("-Command"),
            command_text.to_string(),
        ]
    );
    assert_eq!(
        ExecShellLauncher::Powershell.command_args(command_text),
        vec![
            String::from("-NoProfile"),
            String::from("-Command"),
            command_text.to_string(),
        ]
    );
    assert_eq!(
        ExecShellLauncher::Bash.command_args(command_text),
        vec![String::from("-lc"), command_text.to_string()]
    );
    assert_eq!(
        ExecShellLauncher::Zsh.command_args(command_text),
        vec![String::from("-lc"), command_text.to_string()]
    );
    assert_eq!(
        ExecShellLauncher::Sh.command_args(command_text),
        vec![String::from("-c"), command_text.to_string()]
    );
}

/// Verify `vulcan.process.exec` accepts one shell name taken directly from `vulcan.process.launchers().default`.
/// 验证 `vulcan.process.exec` 接受直接来自 `vulcan.process.launchers().default` 的 shell 名称。
#[test]
fn execute_runlua_request_inline_supports_vulcan_process_exec_with_explicit_shell_name() {
    let engine = make_runtime_test_engine();
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"local launchers = vulcan.process.launchers(); local command; if launchers.default == 'cmd' then command = 'echo explicit-shell-ok' else command = 'printf explicit-shell-ok' end; local executed = vulcan.process.exec({ command = command, shell = launchers.default, encoding = 'utf-8' }); return vulcan.json.encode({ shell = launchers.default, success = executed.success, stdout = executed.stdout })"}"#,
        )
        .expect("inline runlua should execute process.exec with one explicit shell name");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("\"success\":true"));
    assert!(result.contains("explicit-shell-ok"));
}

/// Verify `vulcan.process.exec` timeout diagnostics report the explicit requested timeout.
/// 验证 `vulcan.process.exec` 超时诊断会报告显式请求的超时时长。
#[test]
fn execute_runlua_request_inline_reports_vulcan_process_exec_timeout_ms() {
    // Process-wide environment guard because the shell command may resolve helper executables through PATH.
    // 进程级环境保护锁，因为 shell 命令可能会通过 PATH 解析辅助可执行文件。
    let _env_guard = process_env_test_guard();
    // Runtime engine used to execute one process command that should exceed the timeout.
    // 用于执行一个预期超过超时时长的进程命令的运行时引擎。
    let engine = make_runtime_test_engine();
    // Rendered runlua result carrying the structured process timeout envelope.
    // 携带结构化进程超时结果包络的已渲染 runlua 结果。
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"local info = vulcan.os.info(); local command; if info.os == 'windows' then command = 'ping -n 3 127.0.0.1 >NUL' else command = 'sleep 1' end; local executed = vulcan.process.exec({ command = command, timeout_ms = 50, encoding = 'utf-8' }); return vulcan.json.encode({ timed_out = executed.timed_out, success = executed.success, error = executed.error })"}"#,
        )
        .expect("inline runlua should return one process timeout result");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("\"timed_out\":true"));
    assert!(result.contains("\"success\":false"));
    assert!(result.contains("process execution timed out after 50 ms"));
}

/// Verify `vulcan.process.exec` can spawn one PATH-discovered Windows shell launcher using its resolved executable path.
/// 验证 `vulcan.process.exec` 能通过解析后的实际可执行路径启动一个由 PATH 发现的 Windows shell 启动器。
#[cfg(windows)]
#[test]
fn execute_runlua_request_inline_supports_vulcan_process_exec_with_path_resolved_shell_launcher() {
    let _env_guard = process_env_test_guard();
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-process-exec-shell-path");
    let target_dir = temp_root.join("path-bin");
    let bash_launcher_path = target_dir.join("bash.cmd");
    let _restore_guard = TestEnvRestoreGuard::capture("PATH").and_capture("PATHEXT");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create shell launcher path dir");
    fs::write(
        &bash_launcher_path,
        "@echo off\r\necho resolved-shell-ok\r\n",
    )
    .expect("write path-resolved bash launcher");
    unsafe { std::env::set_var("PATH", target_dir.as_os_str()) };
    unsafe {
        std::env::set_var("PATHEXT", ".CMD;.EXE;.BAT;.COM");
    }
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"local executed = vulcan.process.exec({ command = 'echo ignored-command-text', shell = 'bash', encoding = 'utf-8' }); return vulcan.json.encode({ success = executed.success, stdout = executed.stdout })"}"#,
        )
        .expect("inline runlua should execute process.exec through one PATH-resolved shell launcher");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("\"success\":true"));
    assert!(result.contains("resolved-shell-ok"));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify default Windows shell execution keeps the native `cmd.exe` launch semantics instead of preferring one PATH-shadowed copy.
/// 验证 Windows 默认 shell 执行会保留原生 `cmd.exe` 启动语义，而不是优先使用 PATH 中的同名影子副本。
#[cfg(windows)]
#[test]
fn execute_runlua_request_inline_keeps_default_shell_outside_path_shadowing() {
    let _env_guard = process_env_test_guard();
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-process-exec-default-shell-shadow");
    let target_dir = temp_root.join("path-bin");
    let shadow_cmd_path = target_dir.join("cmd.exe");
    let _restore_guard = TestEnvRestoreGuard::capture("PATH");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create default shell shadow path dir");
    fs::write(&shadow_cmd_path, "@echo off\r\necho fake-shadow-cmd\r\n")
        .expect("write shadow cmd launcher");
    unsafe { std::env::set_var("PATH", target_dir.as_os_str()) };
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"local executed = vulcan.process.exec({ command = 'echo default-shell-ok', encoding = 'utf-8' }); return vulcan.json.encode({ success = executed.success, stdout = executed.stdout })"}"#,
        )
        .expect("inline runlua should keep native default shell execution semantics");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("\"success\":true"));
    assert!(result.contains("default-shell-ok"));
    assert!(!result.contains("fake-shadow-cmd"));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify `vulcan.process.exec` rejects shell-name selection when Lua tries to use `program` mode.
/// 验证当 Lua 试图使用 `program` 模式时，`vulcan.process.exec` 会拒绝 shell 名称选择。
#[test]
fn execute_runlua_request_inline_rejects_vulcan_process_exec_shell_name_in_program_mode() {
    let engine = make_runtime_test_engine();
    let result = engine
        .execute_runlua_request_json_inline(
            r#"{"code":"local launchers = vulcan.process.launchers(); local ok, err = pcall(function() return vulcan.process.exec({ program = 'demo-shell-mode-program', shell = launchers.default }) end); return tostring(ok), tostring(err)"}"#,
        )
        .expect("inline runlua should surface one program-mode shell-name validation error");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("false"));
    assert!(result.contains("requires command mode"));
}

/// Verify `vulcan.process.which` resolves one explicit Unicode path without shelling out.
/// 验证 `vulcan.process.which` 能在不借助 shell 的情况下解析单个显式 Unicode 路径。
#[test]
fn execute_runlua_request_inline_supports_vulcan_process_which_for_explicit_path() {
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-process-which-explicit");
    let target_dir = temp_root.join("查找目录");
    #[cfg(windows)]
    let program_path = target_dir.join("测试工具.cmd");
    #[cfg(not(windows))]
    let program_path = target_dir.join("测试工具");
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create process which explicit dir");
    fs::write(&program_path, "echo explicit-process-which")
        .expect("write process which explicit program");
    mark_test_program_executable(&program_path);
    // ProgramArgument carries the native canonical spelling on Windows to exercise prefix filtering.
    // ProgramArgument 在 Windows 上携带原生规范写法，用于覆盖前缀过滤。
    #[cfg(windows)]
    let program_argument =
        fs::canonicalize(&program_path).expect("canonical explicit process.which program");
    #[cfg(not(windows))]
    let program_argument = program_path.clone();
    let request = json!({
        "code": "return vulcan.json.encode({ found = vulcan.process.which(args.program) })",
        "args": {
            "program": program_argument
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should resolve explicit process.which path");

    let expected_found = serde_json::to_string(&render_host_visible_path(&program_argument))
        .expect("json explicit found");
    assert!(result.contains("SUCCESS"));
    assert!(result.contains(&format!("\"found\":{}", expected_found)));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify process candidate lookup reports metadata errors instead of treating them as misses.
/// 验证进程候选查找会报告元数据错误，而不是将其视为未命中。
#[test]
fn vulcan_process_candidate_lookup_reports_metadata_probe_errors() {
    // Candidate path containing one embedded NUL that filesystem metadata cannot inspect.
    // 包含一个内嵌 NUL 的候选路径，文件系统元数据无法探测该路径。
    let invalid_candidate = PathBuf::from("invalid\0program");
    // Error returned before the invalid candidate can be treated as a normal lookup miss.
    // 在非法候选被视为普通查找未命中之前返回的错误。
    let error = find_vulcan_process_candidate(&invalid_candidate)
        .expect_err("invalid executable candidate metadata probe should fail");

    assert!(error.contains("process.which: failed to inspect executable candidate"));
    assert!(error.contains("invalid"));
}

/// Verify `vulcan.process.which` searches PATH and honors PATHEXT-style resolution on the host.
/// 验证 `vulcan.process.which` 会搜索 PATH，并在宿主上遵循 PATHEXT 风格的解析规则。
#[test]
fn execute_runlua_request_inline_supports_vulcan_process_which_via_path_search() {
    let _env_guard = process_env_test_guard();
    let engine = make_runtime_test_engine();
    let temp_root = make_temp_runtime_root("vulcan-process-which-path");
    let target_dir = temp_root.join("path-bin");
    #[cfg(windows)]
    let program_name = "demo-which-tool";
    #[cfg(not(windows))]
    let program_name = "demo-which-tool";
    #[cfg(windows)]
    let program_path = target_dir.join("demo-which-tool.cmd");
    #[cfg(not(windows))]
    let program_path = target_dir.join("demo-which-tool");
    let _restore_guard = {
        #[cfg(windows)]
        {
            TestEnvRestoreGuard::capture("PATH").and_capture("PATHEXT")
        }
        #[cfg(not(windows))]
        {
            TestEnvRestoreGuard::capture("PATH")
        }
    };
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&target_dir).expect("create process which path dir");
    fs::write(&program_path, "echo path-process-which").expect("write process which path program");
    mark_test_program_executable(&program_path);
    unsafe { std::env::set_var("PATH", target_dir.as_os_str()) };
    #[cfg(windows)]
    unsafe {
        std::env::set_var("PATHEXT", ".CMD;.EXE;.BAT;.COM");
    }
    let request = json!({
        "code": "return vulcan.json.encode({ found = vulcan.process.which(args.program) })",
        "args": {
            "program": program_name
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should resolve process.which through PATH search");

    let expected_found =
        serde_json::to_string(&render_host_visible_path(&program_path)).expect("json path found");
    assert!(result.contains("SUCCESS"));
    assert!(result.contains(&format!("\"found\":{}", expected_found)));
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify isolated runlua redirects Lua `io.popen` to the Rust-backed read implementation.
/// 验证隔离 runlua 会把 Lua `io.popen` 重定向到 Rust 托管读取实现。
#[test]
fn execute_runlua_request_inline_uses_managed_io_popen() {
    let engine = make_runtime_test_engine();
    let result = engine
            .execute_runlua_request_json_inline(
                r#"{"code":"local f = io.popen('echo managed-popen-ok', 'r'); local value = f:read('*a'); local ok = f:close(); return value, ok"}"#,
            )
            .expect("inline runlua should read through managed io.popen");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("managed-popen-ok"));
    assert!(result.contains("true"));
}

/// Verify isolated runlua rejects the unsupported managed `io.popen` write mode.
/// 验证隔离 runlua 会拒绝暂不支持的托管 `io.popen` 写入模式。
#[test]
fn execute_runlua_request_inline_rejects_io_popen_write_mode() {
    let engine = make_runtime_test_engine();
    let result = engine
        .execute_runlua_request_json_inline(r#"{"code":"return io.popen('echo hello', 'w')"}"#)
        .expect("inline runlua should render the managed io.popen mode error");

    assert!(result.contains("FAILED"));
    assert!(result.contains("write mode is not implemented yet"));
}

/// Verify host default text encoding is used by managed IO when Lua omits encoding options.
/// 验证 Lua 省略编码选项时托管 IO 会使用宿主默认文本编码。
#[test]
fn execute_runlua_request_inline_uses_host_default_text_encoding() {
    let engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        default_text_encoding: Some("gb18030".to_string()),
        ..Default::default()
    });
    let path = std::env::temp_dir().join(format!(
        "luaskills_runlua_default_encoding_{}.txt",
        std::process::id()
    ));
    let bytes = encode_runtime_text("宿主默认编码", RuntimeTextEncoding::Gb18030)
        .expect("encode host default gb18030 test file");
    fs::write(&path, bytes).expect("write host default encoding file");
    let request = json!({
        "code": "return vulcan.io.read_text(args.path)",
        "args": {
            "path": render_host_visible_path(&path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should read through host default encoding");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("宿主默认编码"));
    let _ = fs::remove_file(path);
}

/// Verify hosts can disable the managed global `io` compatibility layer for luaexec.
/// 验证宿主可以为 luaexec 关闭托管全局 `io` 兼容层。
#[test]
fn execute_runlua_request_inline_can_disable_managed_io_compat() {
    let engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        capabilities: LuaRuntimeCapabilityOptions {
            enable_managed_io_compat: false,
            ..Default::default()
        },
        ..Default::default()
    });
    let result = engine
            .execute_runlua_request_json_inline(
                r#"{"code":"local preload = package and package.preload and package.preload.io; return type(preload) == 'function' and 'managed' or 'native'"}"#,
            )
            .expect("inline runlua should keep native io when managed compat is disabled");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("native"));
}

/// Verify `vulcan.process.exec` exposes explicit encoding metadata after byte-based capture.
/// 验证 `vulcan.process.exec` 在按字节捕获后会暴露明确的编码元数据。
#[test]
fn execute_runlua_request_inline_reports_process_exec_encoding_metadata() {
    let engine = make_runtime_test_engine();
    let result = engine
            .execute_runlua_request_json_inline(
                r#"{"code":"local info = vulcan.os.info(); local spec; if info.os == 'windows' then spec = { program = 'cmd', args = { '/C', 'echo exec-encoding-ok' }, encoding = 'utf-8' } else spec = { program = 'sh', args = { '-c', 'printf exec-encoding-ok' }, encoding = 'utf-8' } end; local result = vulcan.process.exec(spec); return result.stdout, result.stdout_encoding, result.stdout_lossy"}"#,
            )
            .expect("inline runlua should execute process.exec");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("exec-encoding-ok"));
    assert!(result.contains("utf-8"));
    assert!(result.contains("false"));
}

/// Verify `vulcan.process.session` can write to stdin and read captured stdout.
/// 验证 `vulcan.process.session` 可以写入 stdin 并读取捕获的 stdout。
#[test]
fn execute_runlua_request_inline_uses_process_session_write_read() {
    let engine = make_runtime_test_engine();
    let result = engine
            .execute_runlua_request_json_inline(
                r#"{"code":"local info = vulcan.os.info(); local spec; if info.os == 'windows' then spec = { program = 'cmd', args = { '/V:ON', '/C', 'set /P line=&echo session:!line!' }, encoding = 'utf-8' } else spec = { program = '/bin/sh', args = { '-c', 'read line; echo session:$line' }, encoding = 'utf-8' } end; local session = vulcan.process.session.open(spec); session:write('ok\\n'); local status = session:close({ timeout_ms = 3000 }); local output = session:read({ timeout_ms = 3000 }); return output.stdout, status.exited,status.success"}"#,
            )
            .expect("inline runlua should exercise process session");

    assert!(result.contains("SUCCESS"), "unexpected result: {result}");
    assert!(result.contains("session:ok"), "unexpected result: {result}");
    assert!(result.contains("true"), "unexpected result: {result}");
}

/// Verify persistent runtime sessions keep Lua VM globals across eval calls.
/// 验证持久运行时会话会在多次 eval 调用之间保留 Lua VM 全局状态。
#[test]
fn runtime_session_eval_preserves_vm_state_across_calls() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"stateful-test","ttl_sec":60}"#)
            .expect("create runtime session"),
    )
    .expect("create response json");
    assert_eq!(created["ok"], true);
    let lease_id = created["lease_id"]
        .as_str()
        .expect("lease id should be present")
        .to_string();

    let first_request = json!({
        "lease_id": lease_id,
        "code": "counter = (counter or 0) + 1; return counter"
    });
    let first: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&first_request.to_string())
            .expect("first runtime session eval"),
    )
    .expect("first eval response json");
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"], json!(1));

    let second_request = json!({
        "lease_id": lease_id,
        "code": "counter = (counter or 0) + 1; return counter"
    });
    let second: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&second_request.to_string())
            .expect("second runtime session eval"),
    )
    .expect("second eval response json");
    assert_eq!(second["ok"], true);
    assert_eq!(second["result"], json!(2));
}

/// Python sidecar source embedded from the repository integration fixture.
/// 从仓库集成夹具嵌入的 Python sidecar 源码。
const MANAGED_SESSION_PYTHON_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/managed_sessions/python/session.py"
));
/// Python sibling module embedded from the repository integration fixture.
/// 从仓库集成夹具嵌入的 Python 同级模块。
const MANAGED_SESSION_PYTHON_HELPER_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/managed_sessions/python/helper.py"
));
/// Python invoke handler embedded from the repository integration fixture.
/// 从仓库集成夹具嵌入的 Python invoke 处理器。
const MANAGED_SESSION_PYTHON_INVOKE_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/managed_sessions/python/invoke.py"
));
/// Node sidecar source embedded from the repository integration fixture.
/// 从仓库集成夹具嵌入的 Node sidecar 源码。
const MANAGED_SESSION_NODE_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/managed_sessions/node/session.mjs"
));
/// Node sibling module embedded from the repository integration fixture.
/// 从仓库集成夹具嵌入的 Node 同级模块。
const MANAGED_SESSION_NODE_HELPER_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/managed_sessions/node/helper.mjs"
));
/// Node invoke handler embedded from the repository integration fixture.
/// 从仓库集成夹具嵌入的 Node invoke 处理器。
const MANAGED_SESSION_NODE_INVOKE_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/managed_sessions/node/invoke.mjs"
));
/// Synthetic package-manager version used only by prebuilt integration environments.
/// 仅由预构建集成环境使用的合成包管理器版本。
const MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION: &str = "0.0.0-test";

/// Language runtime exercised by one real managed-session integration fixture.
/// 单个真实受管会话集成夹具所验证的语言运行时。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSessionTestRuntime {
    /// Host CPython runtime.
    /// 宿主 CPython 运行时。
    Python,
    /// Host Node.js runtime.
    /// 宿主 Node.js 运行时。
    Node,
}

impl ManagedSessionTestRuntime {
    /// Return the stable lowercase test label for this runtime.
    /// 返回当前运行时的稳定小写测试标签。
    fn label(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Node => "node",
        }
    }

    /// Return the Lua session-open function path for this runtime.
    /// 返回当前运行时对应的 Lua session-open 函数路径。
    fn lua_open_api(self) -> &'static str {
        match self {
            Self::Python => "vulcan.runtime.python.session.open",
            Self::Node => "vulcan.runtime.node.session.open",
        }
    }

    /// Return the Lua managed-runtime status function path for this runtime.
    /// 返回当前运行时对应的 Lua 受管运行时状态函数路径。
    fn lua_status_api(self) -> &'static str {
        match self {
            Self::Python => "vulcan.runtime.python.status",
            Self::Node => "vulcan.runtime.node.status",
        }
    }

    /// Return the Lua managed-runtime invoke function path for this runtime.
    /// 返回当前运行时对应的 Lua 受管运行时调用函数路径。
    fn lua_invoke_api(self) -> &'static str {
        match self {
            Self::Python => "vulcan.runtime.python.invoke",
            Self::Node => "vulcan.runtime.node.invoke",
        }
    }

    /// Return the package-relative sidecar file for this runtime.
    /// 返回当前运行时对应的包相对 sidecar 文件。
    fn sidecar_file(self) -> &'static str {
        match self {
            Self::Python => "runtime/session.py",
            Self::Node => "runtime/session.mjs",
        }
    }

    /// Return the package-relative invoke handler file for this runtime.
    /// 返回当前运行时对应的包相对 invoke 处理器文件。
    fn invoke_file(self) -> &'static str {
        match self {
            Self::Python => "runtime/invoke.py",
            Self::Node => "runtime/invoke.mjs",
        }
    }

    /// Return the marker proving one sibling runtime module was imported.
    /// 返回用于证明已导入同级运行时模块的标记。
    fn import_marker(self) -> &'static str {
        match self {
            Self::Python => "python-relative-import-ok",
            Self::Node => "node-relative-import-ok",
        }
    }
}

/// One discovered real host interpreter used to seed a managed-runtime layout.
/// 一个已发现的真实宿主解释器，用于构造受管运行时布局。
#[derive(Clone, Debug)]
pub(crate) struct HostManagedRuntime {
    /// Canonical interpreter executable path.
    /// 规范解释器可执行文件路径。
    executable: PathBuf,
    /// Exact runtime version reported by the interpreter.
    /// 解释器报告的精确运行时版本。
    version: String,
}

/// Discover one real Python or Node executable and its exact version.
/// 发现一个真实 Python 或 Node 可执行文件及其精确版本。
///
/// `runtime` selects the interpreter probe; unavailable runtimes produce one explicit skip message.
/// `runtime` 选择解释器探测；不可用运行时会产生一条明确跳过消息。
///
/// Return a canonical executable and version when usable, otherwise `None` for an explicit skip.
/// 可用时返回规范可执行文件与版本；明确跳过时返回 `None`。
pub(crate) fn discover_host_managed_runtime(
    runtime: ManagedSessionTestRuntime,
) -> Option<HostManagedRuntime> {
    // Candidate program names searched without mutating process-wide PATH.
    // 在不修改进程级 PATH 的情况下搜索的候选程序名。
    let candidates: &[&str] = match runtime {
        ManagedSessionTestRuntime::Python if cfg!(windows) => &["python"],
        ManagedSessionTestRuntime::Python => &["python3", "python"],
        ManagedSessionTestRuntime::Node => &["node"],
    };
    for candidate in candidates {
        // Probe script prints the authoritative executable path and version on separate lines.
        // 探测脚本会在独立行打印权威可执行文件路径与版本。
        let probe_argument = match runtime {
            ManagedSessionTestRuntime::Python => {
                "import sys; print(sys.executable); print('.'.join(map(str, sys.version_info[:3])))"
            }
            ManagedSessionTestRuntime::Node => {
                "console.log(process.execPath); console.log(process.versions.node)"
            }
        };
        // Interpreter-specific inline-evaluation flag; Node uses `-e` while Python uses `-c`.
        // 解释器专属的内联求值参数；Node 使用 `-e`，Python 使用 `-c`。
        let probe_flag = match runtime {
            ManagedSessionTestRuntime::Python => "-c",
            ManagedSessionTestRuntime::Node => "-e",
        };
        // ProbeResult distinguishes an unavailable command from a malformed runtime response.
        // ProbeResult 区分不可用命令与格式错误的运行时响应。
        let probe_result = Command::new(candidate)
            .args([probe_flag, probe_argument])
            .output();
        let Ok(output) = probe_result else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        // Stdout must contain exactly the two authoritative values needed by the fixture.
        // Stdout 必须包含夹具所需的两个权威值。
        let Ok(stdout) = String::from_utf8(output.stdout) else {
            continue;
        };
        // Lines are trimmed because command output uses platform-native line endings.
        // Lines 会被去除首尾空白，因为命令输出使用平台原生换行。
        let mut lines = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let (Some(executable), Some(version)) = (lines.next(), lines.next()) else {
            continue;
        };
        // Real Node integration runs only on the same supported ESM baseline enforced by plan
        // resolution; an older host runtime is an explicit unavailable fixture, not a crash probe.
        // 真实 Node 集成仅在环境计划强制的同一 ESM 支持基线上运行；较旧宿主运行时属于明确不可用
        // 夹具，而不是崩溃探针。
        if runtime == ManagedSessionTestRuntime::Node
            && let Err(error) = validate_managed_node_runtime_version(version)
        {
            eprintln!("skip real managed Node integration candidate {executable}: {error}");
            continue;
        }
        // CanonicalExecutable rejects launcher aliases that do not resolve to a real file.
        // CanonicalExecutable 拒绝无法解析为真实文件的启动器别名。
        let Ok(canonical_executable) = fs::canonicalize(executable) else {
            continue;
        };
        return Some(HostManagedRuntime {
            executable: canonical_executable,
            version: version.to_string(),
        });
    }
    eprintln!(
        "skip real managed {} session integration test: no usable host {} runtime was found on PATH",
        runtime.label(),
        runtime.label()
    );
    // Native acceptance CI converts missing real runtimes into a hard failure.
    // 原生验收 CI 会把缺少真实运行时转换为硬失败。
    assert_ne!(
        std::env::var("LUASKILLS_REQUIRE_MANAGED_SESSION_RUNTIMES").as_deref(),
        Ok("1"),
        "required real managed {} runtime was not found on PATH",
        runtime.label()
    );
    None
}

/// Install one real executable inside a managed installation and write its relative manifest.
/// 在受管安装目录内安装一个真实可执行文件，并写入其相对路径清单。
///
/// `install_dir`, runtime identity fields, and `source_executable` define one synthetic trusted
/// installation whose manifest obeys the same containment contract as production assets.
/// `install_dir`、运行时身份字段与 `source_executable` 定义一个合成可信安装，
/// 其清单遵循与生产资产相同的包含关系契约。
///
/// Returns after the contained executable and strict manifest are durably materialized.
/// 在受包含约束的可执行文件与严格清单完成落盘后返回。
fn write_managed_session_install_manifest(
    install_dir: &Path,
    runtime: &str,
    version: &str,
    platform: &str,
    source_executable: &Path,
) {
    // Platform-native contained executable name used by the relative manifest.
    // 相对路径清单使用的平台原生受包含可执行文件名。
    let executable_name = if cfg!(windows) {
        format!("{runtime}.exe")
    } else {
        runtime.to_string()
    };
    // Relative path with only normal components accepted by production resolution.
    // 仅包含生产解析所接受普通组件的相对路径。
    let relative_executable = PathBuf::from("bin").join(executable_name);
    // Concrete destination strictly inside the installation directory.
    // 严格位于安装目录内的具体目标路径。
    let installed_executable = install_dir.join(&relative_executable);
    fs::create_dir_all(
        installed_executable
            .parent()
            .expect("contained executable parent"),
    )
    .expect("create managed session install executable directory");
    // Hard links preserve exact runtime bytes without a second copy; cross-volume fixtures fall back safely.
    // 硬链接可在不二次复制的情况下保留精确运行时字节；跨卷夹具会安全降级为复制。
    if fs::hard_link(source_executable, &installed_executable).is_err() {
        fs::copy(source_executable, &installed_executable)
            .expect("copy real executable into managed installation");
    }
    #[cfg(windows)]
    if matches!(runtime, "python" | "uv") {
        // PythonFixtureExecutables cover both the interpreter and the lightweight uv stand-in.
        // PythonFixtureExecutables 同时覆盖解释器与轻量 uv 占位程序。
        install_managed_session_windows_python_companions(
            source_executable,
            installed_executable
                .parent()
                .expect("contained executable parent"),
        );
    }
    #[cfg(target_os = "macos")]
    if matches!(runtime, "python" | "uv") {
        // PythonFixtureExecutables cover both the interpreter and the lightweight uv stand-in.
        // PythonFixtureExecutables 同时覆盖解释器与轻量 uv 占位程序。
        install_managed_session_macos_python_companions(source_executable, install_dir);
    }
    // Manifest mirrors the exact runtime asset contract consumed by plan resolution.
    // Manifest 模拟计划解析所消费的精确运行时资产契约。
    let manifest = json!({
        "schema_version": 1,
        "runtime": runtime,
        "version": version,
        "platform": platform,
        "executable": relative_executable,
    });
    fs::write(
        install_dir.join("runtime-manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("encode managed session install manifest"),
    )
    .expect("write managed session install manifest");
}

/// Install the macOS CPython dynamic library beside a synthetic managed test prefix.
/// 在合成受管测试前缀中安装 macOS CPython 动态库。
///
/// `source_executable` identifies the uv-managed interpreter whose native `@rpath` resolves
/// through `../lib`; `destination_root` is the synthetic installation prefix containing `bin`.
/// `source_executable` 标识原生 `@rpath` 通过 `../lib` 解析的 uv 受管解释器；
/// `destination_root` 是包含 `bin` 的合成安装前缀。
///
/// Returns after every direct `libpython*.dylib` bootstrap image is materialized with its source
/// filename, matching the prefix relationship provided by production uv assets.
/// 按源文件名落盘全部直接的 `libpython*.dylib` 启动映像后返回，并保持生产 uv 资产提供的前缀关系。
#[cfg(target_os = "macos")]
fn install_managed_session_macos_python_companions(
    source_executable: &Path,
    destination_root: &Path,
) {
    // SourcePrefix is the real CPython prefix whose executable is located under `bin`.
    // SourcePrefix 是真实 CPython 前缀，其可执行文件位于 `bin` 下。
    let source_prefix = source_executable
        .parent()
        .and_then(Path::parent)
        .expect("managed macOS Python installation prefix");
    // SourceLib is the exact directory encoded by the interpreter's `@rpath` load command.
    // SourceLib 是解释器 `@rpath` 加载命令编码的精确目录。
    let source_lib = source_prefix.join("lib");
    // DestinationLib restores the same `bin/../lib` relationship in the synthetic fixture.
    // DestinationLib 在合成夹具中恢复相同的 `bin/../lib` 关系。
    let destination_lib = destination_root.join("lib");
    fs::create_dir_all(&destination_lib).expect("create managed macOS Python companion directory");
    // InstalledCount makes a malformed or changed upstream Python layout fail explicitly.
    // InstalledCount 使格式错误或已变化的上游 Python 布局显式失败。
    let mut installed_count = 0_usize;
    for entry in fs::read_dir(&source_lib).expect("read managed macOS Python library directory") {
        // SourceEntry is one direct child of the trusted uv-managed Python library directory.
        // SourceEntry 是可信 uv 受管 Python 库目录的一个直接子项。
        let entry = entry.expect("read managed macOS Python library entry");
        // FileName preserves the install name expected by the interpreter image.
        // FileName 保留解释器映像所期望的安装名称。
        let file_name = entry.file_name();
        let normalized_name = file_name.to_string_lossy();
        if !normalized_name.starts_with("libpython") || !normalized_name.ends_with(".dylib") {
            continue;
        }
        // CanonicalSource follows an upstream alias without reproducing an escaping symlink.
        // CanonicalSource 跟随上游别名，但不会复制可逃逸的符号链接。
        let canonical_source = fs::canonicalize(entry.path())
            .expect("canonicalize managed macOS Python companion library");
        assert!(
            canonical_source.is_file(),
            "managed macOS Python companion must be a regular file: {}",
            canonical_source.display()
        );
        // Destination is a contained regular file visible to the executable's native loader.
        // Destination 是原生加载器可见的受包含约束普通文件。
        let destination = destination_lib.join(file_name);
        if fs::hard_link(&canonical_source, &destination).is_err() {
            fs::copy(&canonical_source, &destination)
                .expect("copy managed macOS Python companion library");
        }
        installed_count += 1;
    }
    assert!(
        installed_count > 0,
        "managed macOS Python installation contains no libpython dynamic library: {}",
        source_lib.display()
    );
}

/// Copy minimal Windows CPython bootstrap DLLs beside a contained Python test executable.
/// 将最小 Windows CPython 启动 DLL 复制到受包含约束的 Python 测试可执行文件旁。
///
/// `source_executable` identifies the discovered host runtime and `destination_dir` is the
/// contained managed runtime directory already present on the controlled child PATH.
/// `source_executable` 标识已发现的宿主运行时，`destination_dir` 是已经位于受控子进程 PATH 上的
/// 受包含约束受管运行时目录。
///
/// Returns after copying CPython, zlib, and VC runtime bootstrap DLLs, or immediately for a
/// non-Python executable.
/// 复制 CPython、zlib 与 VC runtime 启动 DLL 后返回；对于非 Python 可执行文件则立即返回。
#[cfg(windows)]
fn install_managed_session_windows_python_companions(
    source_executable: &Path,
    destination_dir: &Path,
) {
    // Lowercase source filename used only to identify the Python integration runtime.
    // 仅用于识别 Python 集成运行时的小写源文件名。
    let source_name = source_executable
        .file_name()
        .expect("managed session source executable name")
        .to_string_lossy()
        .to_ascii_lowercase();
    if source_name != "python.exe" {
        return;
    }
    // Host runtime directory containing CPython's stable and versioned DLLs.
    // 包含 CPython 稳定及版本化 DLL 的宿主运行时目录。
    let source_dir = source_executable
        .parent()
        .expect("managed session source executable parent");
    for entry in fs::read_dir(source_dir).expect("read host Python runtime directory") {
        // Real sibling file considered for the minimal fixture runtime bundle.
        // 纳入最小夹具运行时包考虑范围的真实同级文件。
        let entry = entry.expect("read host Python runtime entry");
        // Lowercase sibling filename used for the explicit minimal bootstrap-DLL filter.
        // 用于显式最小启动 DLL 过滤的小写同级文件名。
        let file_name = entry.file_name();
        let normalized_name = file_name.to_string_lossy().to_ascii_lowercase();
        // Only direct CPython image dependencies are copied; unrelated Conda libraries stay out.
        // 仅复制 CPython 映像直接依赖项；无关 Conda 库不会进入夹具。
        let is_bootstrap_dll = normalized_name.ends_with(".dll")
            && (normalized_name.starts_with("python")
                || normalized_name == "zlib.dll"
                || normalized_name.starts_with("vcruntime"));
        if !is_bootstrap_dll {
            continue;
        }
        if !entry
            .file_type()
            .expect("inspect host Python runtime entry")
            .is_file()
        {
            continue;
        }
        // Contained companion path visible to the Windows DLL loader through controlled PATH.
        // Windows DLL 加载器可通过受控 PATH 看到的受包含约束配套路径。
        let destination = destination_dir.join(&file_name);
        if fs::hard_link(entry.path(), &destination).is_err() {
            fs::copy(entry.path(), &destination)
                .expect("copy Python companion DLL into managed installation");
        }
    }
}

/// Install one Python executable at the platform-native managed virtual-environment path.
/// 在平台原生受管虚拟环境路径安装一个 Python 可执行文件。
#[cfg(unix)]
fn install_managed_session_python_executable(source: &Path, destination: &Path) {
    fs::create_dir_all(destination.parent().expect("Python venv executable parent"))
        .expect("create Python venv bin directory");
    create_unix_symlink(source, destination).expect("link real Python into managed test venv");
}

/// Install one Python executable at the platform-native managed virtual-environment path.
/// 在平台原生受管虚拟环境路径安装一个 Python 可执行文件。
#[cfg(windows)]
fn install_managed_session_python_executable(source: &Path, destination: &Path) {
    fs::create_dir_all(destination.parent().expect("Python venv executable parent"))
        .expect("create Python venv Scripts directory");
    if fs::hard_link(source, destination).is_err() {
        fs::copy(source, destination).expect("copy real Python into managed test venv");
    }
    // CPython DLLs must be beside the copied executable because initial image loading precedes child PATH use.
    // CPython DLL 必须位于复制后的可执行文件旁，因为初始映像加载早于子进程 PATH 的使用。
    install_managed_session_windows_python_companions(
        source,
        destination.parent().expect("Python venv executable parent"),
    );
}

/// Return the site-packages directory for one lightweight managed Python environment.
/// 返回单个轻量受管 Python 环境的 site-packages 目录。
///
/// `env_dir` is the resolved managed environment root and `version` is the exact CPython version.
/// `env_dir` 是已解析受管环境根，`version` 是精确 CPython 版本。
///
/// Return the platform-native directory searched by the managed virtual-environment interpreter.
/// 返回受管虚拟环境解释器搜索的平台原生目录。
fn managed_session_python_site_packages(env_dir: &Path, version: &str) -> PathBuf {
    #[cfg(windows)]
    {
        // Exact version is encoded by pyvenv.cfg; Windows keeps site-packages version-independent.
        // 精确版本由 pyvenv.cfg 编码；Windows 的 site-packages 与版本无关。
        let _ = version;
        // Windows virtual environments use one version-independent Lib/site-packages path.
        // Windows 虚拟环境使用一个与版本无关的 Lib/site-packages 路径。
        env_dir.join(".venv/Lib/site-packages")
    }
    #[cfg(not(windows))]
    {
        // MajorMinor selects the standard Unix virtual-environment library directory.
        // MajorMinor 选择标准 Unix 虚拟环境库目录。
        let mut version_parts = version.split('.');
        let major = version_parts.next().expect("managed Python major version");
        let minor = version_parts.next().expect("managed Python minor version");
        env_dir
            .join(".venv/lib")
            .join(format!("python{major}.{minor}"))
            .join("site-packages")
    }
}

/// Install one same-named dependency carrying a package-specific marker.
/// 安装一个携带包专属标记的同名依赖。
///
/// `env_dir`, `runtime`, and `version` select the exact managed dependency tree.
/// `env_dir`、`runtime` 与 `version` 选择精确受管依赖树。
///
/// `marker` is returned by the real invoke fixture to prove dependency isolation.
/// `marker` 会由真实 invoke 夹具返回，用于证明依赖隔离。
fn install_managed_session_fixture_dependency(
    env_dir: &Path,
    runtime: ManagedSessionTestRuntime,
    version: &str,
    marker: &str,
) {
    // EncodedMarker is safe source text for both Python and JavaScript string literals.
    // EncodedMarker 是可安全用于 Python 与 JavaScript 字符串字面量的源码文本。
    let encoded_marker = serde_json::to_string(marker).expect("encode fixture dependency marker");
    match runtime {
        ManagedSessionTestRuntime::Python => {
            // SitePackages is the only dependency location available after PYTHONPATH removal.
            // SitePackages 是移除 PYTHONPATH 后唯一可用的依赖位置。
            let site_packages = managed_session_python_site_packages(env_dir, version);
            fs::create_dir_all(&site_packages)
                .expect("create managed Python fixture site-packages");
            fs::write(
                site_packages.join("managed_session_dependency.py"),
                format!(
                    "# Package-specific dependency marker used by managed integration tests.\n# 受管集成测试使用的包专属依赖标记。\nDEPENDENCY_MARKER = {encoded_marker}\n"
                ),
            )
            .expect("write managed Python fixture dependency");
        }
        ManagedSessionTestRuntime::Node => {
            // DependencyRoot is resolved as a bare ESM dependency from the package snapshot.
            // DependencyRoot 会作为裸 ESM 依赖从包快照中解析。
            let dependency_root = env_dir.join("node_modules/managed-session-dependency");
            fs::create_dir_all(&dependency_root)
                .expect("create managed Node fixture dependency root");
            fs::write(
                dependency_root.join("package.json"),
                r#"{"name":"managed-session-dependency","version":"1.0.0","type":"module","exports":"./index.mjs"}"#,
            )
            .expect("write managed Node fixture dependency manifest");
            fs::write(
                dependency_root.join("index.mjs"),
                format!(
                    "// Package-specific dependency marker used by managed integration tests.\n// 受管集成测试使用的包专属依赖标记。\nexport const DEPENDENCY_MARKER = {encoded_marker};\n"
                ),
            )
            .expect("write managed Node fixture dependency module");
        }
    }
}

/// Materialize one ready managed environment around a discovered host runtime.
/// 围绕一个已发现宿主运行时构造就绪的受管环境。
///
/// `plan` is the production-resolved environment identity and `host` supplies the real executable.
/// `plan` 是生产逻辑解析的环境身份，`host` 提供真实可执行文件。
///
/// `dependency_marker` is installed into only this exact environment.
/// `dependency_marker` 只会安装到当前精确环境中。
fn prepare_ready_managed_session_environment(
    plan: &ManagedRuntimeEnvPlan,
    runtime: ManagedSessionTestRuntime,
    host: &HostManagedRuntime,
    dependency_marker: &str,
) {
    fs::create_dir_all(&plan.env_dir).expect("create ready managed session environment");
    fs::write(
        managed_env_marker_path(&plan.env_dir),
        serde_json::to_vec_pretty(&plan.expected_marker)
            .expect("encode managed session environment marker"),
    )
    .expect("write managed session environment marker");
    match runtime {
        ManagedSessionTestRuntime::Python => {
            // VenvExecutable is the exact executable path required by Python session launch.
            // VenvExecutable 是 Python 会话启动要求的精确可执行文件路径。
            #[cfg(windows)]
            let venv_executable = plan.env_dir.join(".venv/Scripts/python.exe");
            #[cfg(not(windows))]
            let venv_executable = plan.env_dir.join(".venv/bin/python");
            install_managed_session_python_executable(&host.executable, &venv_executable);
            // PyvenvConfig points the lightweight test venv back to the real base interpreter.
            // PyvenvConfig 将轻量测试 venv 指回真实基础解释器。
            let pyvenv_config = format!(
                "home = {}\ninclude-system-site-packages = false\nversion = {}\nexecutable = {}\n",
                render_host_visible_path(
                    host.executable
                        .parent()
                        .expect("host Python executable parent")
                ),
                host.version,
                render_host_visible_path(&host.executable)
            );
            fs::write(plan.env_dir.join(".venv/pyvenv.cfg"), pyvenv_config)
                .expect("write managed test pyvenv config");
            install_managed_session_fixture_dependency(
                &plan.env_dir,
                runtime,
                &host.version,
                dependency_marker,
            );
            // Probe confirms the exact managed interpreter and isolated dependency can start.
            // Probe 确认精确受管解释器及隔离依赖均可启动。
            let probe = Command::new(&venv_executable)
                .args([
                    "-c",
                    "import managed_session_dependency; print(managed_session_dependency.DEPENDENCY_MARKER)",
                ])
                .output()
                .expect("run managed test Python executable");
            assert!(
                probe.status.success(),
                "managed test Python executable failed: status={} stdout={} stderr={}",
                probe.status,
                String::from_utf8_lossy(&probe.stdout),
                String::from_utf8_lossy(&probe.stderr)
            );
            assert_eq!(
                String::from_utf8(probe.stdout)
                    .expect("managed Python dependency probe output")
                    .trim(),
                dependency_marker
            );
            // IsolatedProbe uses the production env_clear policy before any session/worker test.
            // IsolatedProbe 会在任何会话/Worker 测试之前使用生产 env_clear 策略。
            let mut isolated_probe = Command::new(&venv_executable);
            isolated_probe.args([
                "-c",
                "import managed_session_dependency; print(managed_session_dependency.DEPENDENCY_MARKER)",
            ]);
            crate::runtime::managed_runtime_session::configure_managed_python_command_environment(
                &mut isolated_probe,
                plan,
            )
            .expect("configure isolated managed Python probe");
            // IsolatedOutput proves the real interpreter and dependency remain executable without host env.
            // IsolatedOutput 证明真实解释器与依赖在没有宿主环境时仍可执行。
            let isolated_output = isolated_probe
                .output()
                .expect("run isolated managed test Python executable");
            assert!(
                isolated_output.status.success(),
                "isolated managed test Python executable failed: status={} stdout={} stderr={}",
                isolated_output.status,
                String::from_utf8_lossy(&isolated_output.stdout),
                String::from_utf8_lossy(&isolated_output.stderr)
            );
            assert_eq!(
                String::from_utf8(isolated_output.stdout)
                    .expect("isolated managed Python dependency probe output")
                    .trim(),
                dependency_marker
            );
        }
        ManagedSessionTestRuntime::Node => {
            fs::create_dir_all(plan.env_dir.join("node_modules"))
                .expect("create ready Node dependency directory");
            install_managed_session_fixture_dependency(
                &plan.env_dir,
                runtime,
                &host.version,
                dependency_marker,
            );
            // IsolatedProbe confirms the exact contained Node executable survives relocation and
            // the production environment-clearing policy before Worker launch is exercised.
            // IsolatedProbe 确认精确受包含 Node 可执行文件可在重定位后及生产环境清空策略下存活，
            // 然后才验证 Worker 启动。
            let mut isolated_probe = Command::new(&plan.runtime_executable);
            isolated_probe.args(["-e", "process.stdout.write(process.versions.node)"]);
            crate::runtime::managed_runtime_session::configure_managed_node_command_environment(
                &mut isolated_probe,
                plan,
            )
            .expect("configure isolated managed Node probe");
            let isolated_output = isolated_probe
                .output()
                .expect("run isolated managed test Node executable");
            assert!(
                isolated_output.status.success(),
                "isolated managed test Node executable failed: status={} stdout={} stderr={}",
                isolated_output.status,
                String::from_utf8_lossy(&isolated_output.stdout),
                String::from_utf8_lossy(&isolated_output.stderr)
            );
            assert_eq!(
                String::from_utf8(isolated_output.stdout)
                    .expect("isolated managed Node executable probe output"),
                host.version
            );
        }
    }
}

/// One isolated System Plugin package backed by a prevalidated real host runtime.
/// 一个由已验证真实宿主运行时支撑的隔离 System Plugin 包。
pub(crate) struct ManagedSessionSystemLayout {
    /// Base strict System Plugin filesystem layout.
    /// 基础严格 System Plugin 文件系统布局。
    base: SystemRuntimeTestLayout,
    /// Language runtime configured in dependencies.yaml.
    /// 在 dependencies.yaml 中配置的语言运行时。
    runtime: ManagedSessionTestRuntime,
    /// Declared runtime version used for installation and environment path identity.
    /// 用于安装与环境路径身份的已声明运行时版本。
    runtime_version: String,
    /// Ready managed environment directory used by cleanup assertions.
    /// 清理断言使用的就绪受管环境目录。
    env_dir: PathBuf,
    /// Canonical managed distribution root selected for this fixture.
    /// 当前夹具选定的规范受管发行根。
    distribution_root: PathBuf,
    /// Canonical managed environment root selected for this fixture.
    /// 当前夹具选定的规范受管环境根。
    environment_root: PathBuf,
    /// Whether both managed roots are injected explicitly through host options.
    /// 两个受管根是否通过宿主选项显式注入。
    host_configured_roots: bool,
    /// Marker installed into this package's exact managed dependency tree.
    /// 安装到当前包精确受管依赖树中的标记。
    dependency_marker: String,
}

impl ManagedSessionSystemLayout {
    /// Create a real managed runtime package without invoking network package managers.
    /// 创建一个真实受管运行时包，同时不调用网络包管理器。
    ///
    /// `label`, `runtime`, and `host` select an isolated root and authoritative interpreter.
    /// `label`、`runtime` 与 `host` 选择隔离根目录及权威解释器。
    ///
    /// Return a strict System layout whose environment marker and executable are already ready.
    /// 返回一个环境标记与可执行文件均已就绪的严格 System 布局。
    pub(crate) fn new(
        label: &str,
        runtime: ManagedSessionTestRuntime,
        host: &HostManagedRuntime,
    ) -> Self {
        Self::new_with_runtime_version_and_root_mode(label, None, runtime, host, false)
    }

    /// Create a real managed runtime package with roots outside the LuaSkills data root.
    /// 使用位于 LuaSkills 数据根之外的根目录创建真实受管运行时包。
    ///
    /// `label`, `runtime`, and `host` select the isolated fixture and authoritative interpreter;
    /// the returned layout injects distinct distribution and environment roots through host options.
    /// `label`、`runtime` 与 `host` 选择隔离夹具及权威解释器；返回布局会通过宿主选项注入
    /// 独立的发行根与环境根。
    ///
    /// Returns a ready strict System layout that exercises the host-configured root path end to end.
    /// 返回一个可端到端验证宿主配置根路径的就绪严格 System 布局。
    fn new_with_host_configured_roots(
        label: &str,
        runtime: ManagedSessionTestRuntime,
        host: &HostManagedRuntime,
    ) -> Self {
        Self::new_with_runtime_version_and_root_mode(label, None, runtime, host, true)
    }

    /// Create a real managed runtime package with an optional path-padding runtime version.
    /// 使用可选路径填充运行时版本创建一个真实受管运行时包。
    ///
    /// `label` identifies the package, `runtime_version` can lengthen only the managed environment
    /// path, and `runtime` plus `host` select the exact executable fixture.
    /// `label` 标识包，`runtime_version` 可仅加长受管环境路径，`runtime` 与 `host` 选择精确可执行
    /// 文件夹具。
    ///
    /// Returns a ready strict System layout using the declared version for reproducible identity.
    /// 返回一个使用已声明版本形成可复现身份的就绪严格 System 布局。
    fn new_with_runtime_version(
        label: &str,
        runtime_version: Option<&str>,
        runtime: ManagedSessionTestRuntime,
        host: &HostManagedRuntime,
    ) -> Self {
        Self::new_with_runtime_version_and_root_mode(label, runtime_version, runtime, host, false)
    }

    /// Build one fixture with an optional version override and explicit root-selection mode.
    /// 使用可选版本覆盖与显式根选择模式构造单个夹具。
    ///
    /// `label` partitions temporary data, `runtime_version` may override only the declared version,
    /// `runtime` and `host` select the real interpreter, and `host_configured_roots` controls whether
    /// the distribution/environment authorities are external host options or legacy derived paths.
    /// `label` 隔离临时数据，`runtime_version` 只能覆盖声明版本，`runtime` 与 `host` 选择真实
    /// 解释器，`host_configured_roots` 控制发行/环境授权使用外部宿主选项还是兼容派生路径。
    ///
    /// Returns a fully prepared package, exact environment plan, and ready dependency tree.
    /// 返回完整准备的包、精确环境计划与就绪依赖树。
    fn new_with_runtime_version_and_root_mode(
        label: &str,
        runtime_version: Option<&str>,
        runtime: ManagedSessionTestRuntime,
        host: &HostManagedRuntime,
        host_configured_roots: bool,
    ) -> Self {
        // Base creates the strict trust-root and canonical package paths.
        // Base 创建严格信任根与规范包路径。
        let base = SystemRuntimeTestLayout::new(label);
        // HostRootsParent is a deterministic sibling of runtime_root and therefore outside its boundary.
        // HostRootsParent 是 runtime_root 的确定性同级目录，因此位于其边界之外。
        let host_roots_parent = base.runtime_root.with_extension("host-managed-roots");
        if host_configured_roots {
            let _ = fs::remove_dir_all(&host_roots_parent);
        }
        // DistributionRoot follows either the explicit host layout or the legacy compatible layout.
        // DistributionRoot 使用显式宿主布局或旧有兼容布局。
        let distribution_root = if host_configured_roots {
            host_roots_parent.join("应用 资产").join("managed runtimes")
        } else {
            base.runtime_root.join("dependencies/runtimes")
        };
        // EnvironmentRoot follows either the explicit writable host layout or the legacy env layout.
        // EnvironmentRoot 使用显式可写宿主布局或旧有环境布局。
        let environment_root = if host_configured_roots {
            host_roots_parent
                .join("用户 数据")
                .join("managed runtime envs")
        } else {
            base.runtime_root.join("dependencies/envs")
        };
        let runtime_version = runtime_version.unwrap_or(&host.version).to_string();
        // DependencyMarker identifies the package-specific managed dependency tree.
        // DependencyMarker 标识包专属的受管依赖树。
        let dependency_marker = format!("{}-{label}-dependency", runtime.label());
        // Platform must match the runtime installation manifests exactly.
        // Platform 必须与运行时安装清单精确一致。
        let platform =
            current_managed_runtime_platform_key().expect("resolve managed session test platform");
        // RuntimeDir follows the production shared-runtime installation layout.
        // RuntimeDir 遵循生产共享运行时安装布局。
        let runtime_dir = match runtime {
            ManagedSessionTestRuntime::Python => distribution_root
                .join("python")
                .join(format!("cpython-{runtime_version}-{platform}")),
            ManagedSessionTestRuntime::Node => distribution_root
                .join("node")
                .join(format!("node-{runtime_version}-{platform}")),
        };
        // PackageManagerDir satisfies plan resolution while a ready marker prevents installation.
        // PackageManagerDir 满足计划解析；就绪标记会阻止实际安装。
        let package_manager_dir = match runtime {
            ManagedSessionTestRuntime::Python => distribution_root.join("python").join(format!(
                "uv-{MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION}-{platform}"
            )),
            ManagedSessionTestRuntime::Node => distribution_root.join("node").join(format!(
                "pnpm-{MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION}"
            )),
        };
        write_managed_session_install_manifest(
            &runtime_dir,
            runtime.label(),
            &runtime_version,
            &platform,
            &host.executable,
        );
        let (package_manager_name, package_manager_platform) = match runtime {
            ManagedSessionTestRuntime::Python => ("uv", platform.as_str()),
            ManagedSessionTestRuntime::Node => ("pnpm", "any"),
        };
        write_managed_session_install_manifest(
            &package_manager_dir,
            package_manager_name,
            MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION,
            package_manager_platform,
            &host.executable,
        );
        // RuntimeSourceDir contains the real sidecar and sibling import fixture.
        // RuntimeSourceDir 包含真实 sidecar 与同级导入夹具。
        let runtime_source_dir = base.package_root.join("runtime");
        fs::create_dir_all(&runtime_source_dir)
            .expect("create managed session package runtime directory");
        match runtime {
            ManagedSessionTestRuntime::Python => {
                fs::write(
                    runtime_source_dir.join("session.py"),
                    MANAGED_SESSION_PYTHON_SOURCE,
                )
                .expect("write Python managed session fixture");
                fs::write(
                    runtime_source_dir.join("helper.py"),
                    MANAGED_SESSION_PYTHON_HELPER_SOURCE,
                )
                .expect("write Python managed session helper");
                fs::write(
                    runtime_source_dir.join("invoke.py"),
                    MANAGED_SESSION_PYTHON_INVOKE_SOURCE,
                )
                .expect("write Python managed invoke fixture");
                fs::create_dir_all(base.package_root.join("python"))
                    .expect("create Python lockfile directory");
                fs::write(base.package_root.join("python/requirements.lock"), "")
                    .expect("write Python managed session lockfile");
                fs::write(
                    &base.dependencies_file,
                    format!(
                        "python_runtime:\n  version: \"{}\"\n  package_manager: uv\n  package_manager_version: \"{}\"\n  lockfile: python/requirements.lock\n",
                        runtime_version, MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION
                    ),
                )
                .expect("write Python managed session dependencies");
            }
            ManagedSessionTestRuntime::Node => {
                fs::write(
                    runtime_source_dir.join("session.mjs"),
                    MANAGED_SESSION_NODE_SOURCE,
                )
                .expect("write Node managed session fixture");
                fs::write(
                    runtime_source_dir.join("helper.mjs"),
                    MANAGED_SESSION_NODE_HELPER_SOURCE,
                )
                .expect("write Node managed session helper");
                fs::write(
                    runtime_source_dir.join("invoke.mjs"),
                    MANAGED_SESSION_NODE_INVOKE_SOURCE,
                )
                .expect("write Node managed invoke fixture");
                fs::create_dir_all(base.package_root.join("node"))
                    .expect("create Node manifest directory");
                fs::write(
                    base.package_root.join("node/package.json"),
                    r#"{"name":"managed-session-fixture","version":"1.0.0","private":true}"#,
                )
                .expect("write Node managed session package manifest");
                fs::write(
                    base.package_root.join("node/pnpm-lock.yaml"),
                    "lockfileVersion: '9.0'\n",
                )
                .expect("write Node managed session lockfile");
                fs::write(
                    &base.dependencies_file,
                    format!(
                        "node_runtime:\n  version: \"{}\"\n  package_manager: pnpm\n  package_manager_version: \"{}\"\n  package_json: node/package.json\n  lockfile: node/pnpm-lock.yaml\n",
                        runtime_version, MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION
                    ),
                )
                .expect("write Node managed session dependencies");
            }
        }
        // Lua module proves the package root is present on package.path inside the System VM.
        // Lua 模块证明 System VM 内的 package.path 包含包根目录。
        fs::write(
            base.package_root.join("fixture_module.lua"),
            "-- Stable package.path marker returned by integration tests.\n-- 集成测试返回的稳定 package.path 标记。\nreturn { marker = 'system-package-module-ok' }\n",
        )
        .expect("write System package Lua module fixture");
        // Manifest is parsed through the same strict loader used by System lease creation.
        // Manifest 通过 System 租约创建使用的同一严格加载器解析。
        let manifest = PackageDependencyManifest::load_from_path(&base.dependencies_file)
            .expect("load managed session dependency manifest");
        // Package context is used only to derive the exact ready environment identity.
        // Package context 仅用于派生精确的就绪环境身份。
        // ManagedRoots mirrors the exact engine authority before deriving the ready fixture plan.
        // ManagedRoots 在派生就绪夹具计划前镜像精确的引擎授权。
        let managed_roots = Arc::new(
            ManagedRuntimeRoots::new(
                &base.runtime_root,
                host_configured_roots.then_some(distribution_root.as_path()),
                host_configured_roots.then_some(environment_root.as_path()),
            )
            .expect("build managed session fixture root authority"),
        );
        let package = ManagedRuntimePackageContext::for_skill_with_roots(
            &base.package_id,
            &base.package_root,
            managed_roots,
            Some(manifest),
        )
        .expect("build managed session fixture package context");
        // Plan is the authoritative environment path consumed later by the System context.
        // Plan 是后续 System 上下文消费的权威环境路径。
        let plan = match runtime {
            ManagedSessionTestRuntime::Python => resolve_python_env_plan(
                package.as_ref(),
                package
                    .dependency_manifest()
                    .and_then(|manifest| manifest.python_runtime.as_ref())
                    .expect("Python runtime declaration"),
            ),
            ManagedSessionTestRuntime::Node => resolve_node_env_plan(
                package.as_ref(),
                package
                    .dependency_manifest()
                    .and_then(|manifest| manifest.node_runtime.as_ref())
                    .expect("Node runtime declaration"),
            ),
        }
        .expect("resolve managed session environment plan");
        prepare_ready_managed_session_environment(&plan, runtime, host, &dependency_marker);
        Self {
            base,
            runtime,
            runtime_version,
            env_dir: plan.env_dir,
            distribution_root: plan.distribution_root,
            environment_root: plan.environment_root,
            host_configured_roots,
            dependency_marker,
        }
    }

    /// Return host options for the strict System Plugin runtime roots.
    /// 返回严格 System Plugin 运行时根对应的宿主选项。
    pub(crate) fn host_options(&self) -> LuaRuntimeHostOptions {
        // HostOptions starts with the strict System package trust boundary.
        // HostOptions 从严格 System 包信任边界开始构造。
        let mut host_options = self.base.host_options();
        if self.host_configured_roots {
            host_options.managed_runtime_distribution_root = Some(self.distribution_root.clone());
            host_options.managed_runtime_environment_root = Some(self.environment_root.clone());
        }
        host_options
    }

    /// Return one strict System lease request, optionally replacing the same SID generation.
    /// 返回一个严格 System 租约请求，并可选择替换同 SID 代际。
    pub(crate) fn create_request(&self, sid: &str, replace: bool) -> Value {
        // Request uses the infinite System lifetime required by long-running plugin sessions.
        // Request 使用长期插件会话要求的无限 System 生命周期。
        let mut request = self.base.create_request(sid);
        request["ttl_sec"] = json!(0);
        request["replace"] = json!(replace);
        request
    }

    /// Return the package-relative sidecar file for this layout.
    /// 返回当前布局对应的包相对 sidecar 文件。
    fn sidecar_file(&self) -> &'static str {
        self.runtime.sidecar_file()
    }

    /// Return the Lua session-open API for this layout.
    /// 返回当前布局对应的 Lua session-open API。
    fn lua_open_api(&self) -> &'static str {
        self.runtime.lua_open_api()
    }

    /// Return the expected package-relative import marker.
    /// 返回预期的包相对导入标记。
    fn import_marker(&self) -> &'static str {
        self.runtime.import_marker()
    }

    /// Return the package-relative invoke handler file for this layout.
    /// 返回当前布局对应的包相对 invoke 处理器文件。
    fn invoke_file(&self) -> &'static str {
        self.runtime.invoke_file()
    }

    /// Return the Lua managed-runtime status function path for this layout.
    /// 返回当前布局对应的 Lua 受管运行时状态函数路径。
    fn lua_status_api(&self) -> &'static str {
        self.runtime.lua_status_api()
    }

    /// Return the Lua managed-runtime invoke function path for this layout.
    /// 返回当前布局对应的 Lua 受管运行时调用函数路径。
    fn lua_invoke_api(&self) -> &'static str {
        self.runtime.lua_invoke_api()
    }

    /// Return the package runtime directory used as an authorized relative cwd.
    /// 返回作为授权相对 cwd 使用的包 runtime 目录。
    fn runtime_source_dir(&self) -> PathBuf {
        self.base.package_root.join("runtime")
    }
}

/// A second System Plugin package sharing one engine runtime root but owning its dependencies.
/// 共享同一引擎运行时根、但独立拥有依赖的第二个 System Plugin 包。
struct ManagedSessionSiblingSystemPackage {
    /// Stable package identifier trusted by strict System lease creation.
    /// 严格 System 租约创建所信任的稳定包标识。
    package_id: String,
    /// Canonical package root under the configured System trust root.
    /// 配置的 System 信任根下的规范包根。
    package_root: PathBuf,
    /// Canonical dependency manifest path.
    /// 规范依赖清单路径。
    dependencies_file: PathBuf,
    /// Runtime implemented by the sibling package fixture.
    /// 同级包夹具实现的运行时。
    runtime: ManagedSessionTestRuntime,
    /// Ready package-specific managed environment.
    /// 包专属的就绪受管环境。
    env_dir: PathBuf,
    /// Marker installed in only the sibling dependency tree.
    /// 只安装在同级包依赖树中的标记。
    dependency_marker: String,
    /// Marker exported by the sibling's same-named Lua module.
    /// 同级包同名 Lua 模块导出的标记。
    lua_module_marker: String,
}

impl ManagedSessionSiblingSystemPackage {
    /// Return one strict infinite-lifetime System lease request for this package.
    /// 返回当前包的严格无限生命周期 System 租约请求。
    ///
    /// `sid` supplies the stable host identity for the independent lease.
    /// `sid` 为独立租约提供稳定宿主身份。
    ///
    /// Return canonical package paths without any cross-package search roots.
    /// 返回规范包路径，且不包含任何跨包搜索根。
    fn create_request(&self, sid: &str) -> Value {
        json!({
            "sid": sid,
            "ttl_sec": 0,
            "system_package": {
                "id": self.package_id.as_str(),
                "root": render_host_visible_path(&self.package_root),
                "dependencies_file": "dependencies.yaml",
            }
        })
    }
}

/// Create one sibling System Plugin with a distinct lock identity and dependency tree.
/// 创建一个具有独立锁身份与依赖树的同级 System Plugin。
///
/// `layout` supplies the already-installed shared runtime assets and System trust root.
/// `layout` 提供已安装的共享运行时资产与 System 信任根。
///
/// `package_id` and `dependency_marker` identify the isolated package and dependency evidence.
/// `package_id` 与 `dependency_marker` 标识隔离包及依赖证据。
///
/// Return a canonical sibling package ready for real status and invoke calls.
/// 返回可执行真实 status 与 invoke 调用的规范同级包。
fn create_managed_session_sibling_system_package(
    layout: &ManagedSessionSystemLayout,
    host: &HostManagedRuntime,
    package_id: &str,
    dependency_marker: &str,
) -> ManagedSessionSiblingSystemPackage {
    // PackageRoot remains strictly inside the same configured System Lua trust root.
    // PackageRoot 严格位于同一已配置 System Lua 信任根内部。
    let package_root = layout.base.system_lua_lib_dir.join(package_id);
    let dependencies_file = package_root.join("dependencies.yaml");
    let runtime_source_dir = package_root.join("runtime");
    fs::create_dir_all(&runtime_source_dir)
        .expect("create sibling managed runtime source directory");
    match layout.runtime {
        ManagedSessionTestRuntime::Python => {
            fs::write(
                runtime_source_dir.join("invoke.py"),
                MANAGED_SESSION_PYTHON_INVOKE_SOURCE,
            )
            .expect("write sibling Python invoke fixture");
            fs::write(
                runtime_source_dir.join("helper.py"),
                MANAGED_SESSION_PYTHON_HELPER_SOURCE,
            )
            .expect("write sibling Python relative-import helper fixture");
            fs::create_dir_all(package_root.join("python"))
                .expect("create sibling Python lock directory");
            fs::write(
                package_root.join("python/requirements.lock"),
                format!("# isolated System package lock identity: {package_id}\n"),
            )
            .expect("write sibling Python lockfile");
            fs::write(
                &dependencies_file,
                format!(
                    "python_runtime:\n  version: \"{}\"\n  package_manager: uv\n  package_manager_version: \"{}\"\n  lockfile: python/requirements.lock\n",
                    host.version, MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION
                ),
            )
            .expect("write sibling Python dependencies");
        }
        ManagedSessionTestRuntime::Node => {
            fs::write(
                runtime_source_dir.join("invoke.mjs"),
                MANAGED_SESSION_NODE_INVOKE_SOURCE,
            )
            .expect("write sibling Node invoke fixture");
            fs::write(
                runtime_source_dir.join("helper.mjs"),
                MANAGED_SESSION_NODE_HELPER_SOURCE,
            )
            .expect("write sibling Node relative-import helper fixture");
            fs::create_dir_all(package_root.join("node"))
                .expect("create sibling Node manifest directory");
            fs::write(
                package_root.join("node/package.json"),
                format!(
                    r#"{{"name":"{package_id}","version":"1.0.0","private":true,"type":"module"}}"#
                ),
            )
            .expect("write sibling Node package manifest");
            fs::write(
                package_root.join("node/pnpm-lock.yaml"),
                format!(
                    "lockfileVersion: '9.0'\n# isolated System package lock identity: {package_id}\n"
                ),
            )
            .expect("write sibling Node lockfile");
            fs::write(
                &dependencies_file,
                format!(
                    "node_runtime:\n  version: \"{}\"\n  package_manager: pnpm\n  package_manager_version: \"{}\"\n  package_json: node/package.json\n  lockfile: node/pnpm-lock.yaml\n",
                    host.version, MANAGED_SESSION_TEST_PACKAGE_MANAGER_VERSION
                ),
            )
            .expect("write sibling Node dependencies");
        }
    }
    // LuaModuleMarker distinguishes the same module name loaded by two dedicated System VMs.
    // LuaModuleMarker 区分两个专用 System VM 加载的同名模块。
    let lua_module_marker = format!("{package_id}-lua-module");
    fs::write(
        package_root.join("fixture_module.lua"),
        format!(
            "-- Package-specific module marker returned by isolation tests.\n-- 隔离测试返回的包专属模块标记。\nreturn {{ marker = {} }}\n",
            serde_json::to_string(&lua_module_marker).expect("quote sibling Lua module marker")
        ),
    )
    .expect("write sibling System Lua module fixture");
    // Manifest follows the same strict parser used during System lease creation.
    // Manifest 使用与 System 租约创建相同的严格解析器。
    let manifest = PackageDependencyManifest::load_from_path(&dependencies_file)
        .expect("load sibling managed dependency manifest");
    // PackageContext derives the exact package-specific environment identity.
    // PackageContext 派生精确的包专属环境身份。
    let package = ManagedRuntimePackageContext::for_skill(
        package_id,
        &package_root,
        &layout.base.runtime_root,
        Some(manifest),
    )
    .expect("build sibling managed package context");
    // Plan proves lock contents participate in environment isolation.
    // Plan 证明锁文件内容参与环境隔离。
    let plan = match layout.runtime {
        ManagedSessionTestRuntime::Python => resolve_python_env_plan(
            package.as_ref(),
            package
                .dependency_manifest()
                .and_then(|manifest| manifest.python_runtime.as_ref())
                .expect("sibling Python runtime declaration"),
        ),
        ManagedSessionTestRuntime::Node => resolve_node_env_plan(
            package.as_ref(),
            package
                .dependency_manifest()
                .and_then(|manifest| manifest.node_runtime.as_ref())
                .expect("sibling Node runtime declaration"),
        ),
    }
    .expect("resolve sibling managed environment plan");
    prepare_ready_managed_session_environment(&plan, layout.runtime, host, dependency_marker);
    ManagedSessionSiblingSystemPackage {
        package_id: package_id.to_string(),
        package_root: fs::canonicalize(&package_root)
            .expect("canonicalize sibling System package root"),
        dependencies_file: fs::canonicalize(&dependencies_file)
            .expect("canonicalize sibling dependency manifest"),
        runtime: layout.runtime,
        env_dir: plan.env_dir,
        dependency_marker: dependency_marker.to_string(),
        lua_module_marker,
    }
}

/// Stable identity returned by one strict System lease creation.
/// 单次严格 System 租约创建返回的稳定身份。
#[derive(Clone, Debug)]
struct ManagedSessionLeaseHandle {
    /// Opaque manager-issued lease identifier.
    /// 管理器签发的不透明租约标识。
    lease_id: String,
    /// SID-local lease generation.
    /// SID 内部租约代际。
    generation: u64,
}

/// Create one strict System lease and assert its canonical success response.
/// 创建一个严格 System 租约并断言其规范成功响应。
fn create_managed_session_test_lease(
    engine: &LuaEngine,
    layout: &ManagedSessionSystemLayout,
    sid: &str,
    replace: bool,
) -> ManagedSessionLeaseHandle {
    create_managed_session_test_lease_from_request(engine, &layout.create_request(sid, replace))
}

/// Create one strict System lease from an already-built canonical request.
/// 从一个已构造的规范请求创建严格 System 租约。
///
/// `engine` owns the dedicated VM and `request` identifies its exact trusted package.
/// `engine` 拥有专用 VM，`request` 标识其精确可信包。
///
/// Return the opaque lease identity and generation after asserting successful creation.
/// 在断言创建成功后返回不透明租约身份与代际。
fn create_managed_session_test_lease_from_request(
    engine: &LuaEngine,
    request: &Value,
) -> ManagedSessionLeaseHandle {
    // Created is decoded from the same JSON surface used by production hosts.
    // Created 从生产宿主使用的同一 JSON 接口解码。
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&request.to_string())
            .expect("create managed session System lease"),
    )
    .expect("decode managed session System lease response");
    assert_eq!(created["ok"], true, "unexpected create response: {created}");
    ManagedSessionLeaseHandle {
        lease_id: created["lease_id"]
            .as_str()
            .expect("managed session lease id")
            .to_string(),
        generation: created["generation"]
            .as_u64()
            .expect("managed session lease generation"),
    }
}

/// Evaluate Lua inside one exact System lease generation and decode the stable JSON response.
/// 在一个精确 System 租约代际中执行 Lua，并解码稳定 JSON 响应。
fn eval_managed_session_test_lease(
    engine: &LuaEngine,
    lease: &ManagedSessionLeaseHandle,
    code: &str,
) -> Value {
    // Request binds evaluation to both the opaque lease id and generation.
    // Request 将执行同时绑定到不透明租约 id 与代际。
    let request = json!({
        "lease_id": lease.lease_id,
        "generation": lease.generation,
        "code": code,
    });
    serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(&request.to_string())
            .expect("evaluate managed session System lease"),
    )
    .expect("decode managed session System eval response")
}

/// Close one exact System lease generation and assert a successful first close.
/// 关闭一个精确 System 租约代际并断言首次关闭成功。
fn close_managed_session_test_lease(engine: &LuaEngine, lease: &ManagedSessionLeaseHandle) {
    // Closed response proves the VM and its userdata were synchronously retired.
    // Closed 响应证明 VM 及其 userdata 已同步退役。
    let closed: Value = serde_json::from_str(
        &engine
            .close_system_runtime_lease_json(
                &json!({
                    "lease_id": lease.lease_id,
                    "generation": lease.generation,
                })
                .to_string(),
            )
            .expect("close managed session System lease"),
    )
    .expect("decode managed session close response");
    assert_eq!(closed["ok"], true, "unexpected close response: {closed}");
    assert_eq!(closed["closed"], true);
}

/// Build Lua source that stores one managed session in a persistent global.
/// 构造把一个受管会话保存到持久全局变量的 Lua 源码。
pub(crate) fn managed_session_open_lua(
    layout: &ManagedSessionSystemLayout,
    global_name: &str,
    buffer_limit_bytes: usize,
    args: &[String],
) -> String {
    // Args contains JSON-quoted strings, which are valid Lua string literals for fixture paths.
    // Args 包含 JSON 引号字符串；这些字符串对夹具路径也是合法 Lua 字面量。
    let args = args
        .iter()
        .map(|value| serde_json::to_string(value).expect("quote managed session Lua argument"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "local fixture_module = require('fixture_module')\n{global_name} = {}({{ file = '{}', args = {{{args}}}, cwd = 'runtime', stdout_encoding = 'utf-8', stderr_encoding = 'utf-8', stdin_encoding = 'utf-8', buffer_limit_bytes = {buffer_limit_bytes} }})\nreturn {{ module_marker = fixture_module.marker, status = {global_name}:status() }}",
        layout.lua_open_api(),
        layout.sidecar_file(),
    )
}

/// Direct and descendant process identifiers emitted by one real sidecar.
/// 一个真实 sidecar 输出的直接与后代进程标识。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedSessionProcessTree {
    /// Direct managed sidecar process identifier.
    /// 直接受管 sidecar 进程标识。
    root_pid: u32,
    /// Sleeping descendant process identifier.
    /// 休眠后代进程标识。
    child_pid: u32,
}

/// Parse the startup record from one Lua session read result.
/// 从一次 Lua 会话读取结果中解析启动记录。
fn parse_managed_session_started(read_result: &Value) -> (ManagedSessionProcessTree, Value) {
    // Stdout is the exact decoded stream returned by shared process userdata.
    // Stdout 是共享进程 userdata 返回的精确解码流。
    let stdout = read_result["stdout"]
        .as_str()
        .expect("managed session read stdout");
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        // Record ignores non-JSON diagnostics while requiring one authoritative started event.
        // Record 会忽略非 JSON 诊断，同时要求存在一条权威 started 事件。
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["event"] != "started" {
            continue;
        }
        // RootPid and ChildPid are bounded to the platform process-id width used by Child.
        // RootPid 与 ChildPid 被限制到 Child 使用的平台进程 id 宽度。
        let root_pid = u32::try_from(record["root_pid"].as_u64().expect("started root_pid"))
            .expect("root_pid fits u32");
        let child_pid = u32::try_from(record["child_pid"].as_u64().expect("started child_pid"))
            .expect("child_pid fits u32");
        return (
            ManagedSessionProcessTree {
                root_pid,
                child_pid,
            },
            record,
        );
    }
    panic!(
        "managed session startup record missing from stdout: {stdout:?}; read result: {read_result}"
    );
}

/// Return whether one process is still alive without converting probe failures into exit.
/// 返回某个进程是否仍存活，且不会把探测失败转换为退出。
fn managed_session_process_exists(pid: u32) -> Result<bool, String> {
    #[cfg(windows)]
    {
        /// Synchronize access required by WaitForSingleObject on one process handle.
        /// 在进程句柄上调用 WaitForSingleObject 所需的同步访问权限。
        const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
        // Handle pins the selected process identity while its signaled state is inspected.
        // Handle 在检查信号状态期间固定所选进程身份。
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
                0,
                pid,
            )
        };
        if handle.is_null() {
            // Error distinguishes an absent PID from a real process-probe failure.
            // Error 用于区分 PID 不存在与真实进程探测失败。
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(false);
            }
            return Err(format!("OpenProcess({pid}): {error}"));
        }
        // HandleOwner closes the native handle on every branch.
        // HandleOwner 会在每个分支关闭原生句柄。
        let handle_owner = unsafe { OwnedHandle::from_raw_handle(handle as _) };
        // WaitResult reports whether the process has reached a terminal state.
        // WaitResult 报告进程是否已经进入终态。
        let wait_result = unsafe { WaitForSingleObject(handle_owner.as_raw_handle() as HANDLE, 0) };
        match wait_result {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            WAIT_FAILED => Err(format!(
                "WaitForSingleObject({pid}): {}",
                std::io::Error::last_os_error()
            )),
            other => Err(format!(
                "WaitForSingleObject({pid}) returned unexpected status {other}"
            )),
        }
    }
    #[cfg(unix)]
    {
        // Result is the signal-free POSIX process-existence probe.
        // Result 是不发送信号的 POSIX 进程存在性探测。
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result == 0 {
            return Ok(true);
        }
        // Error classifies absent, permission-restricted, and failed probes explicitly.
        // Error 会显式分类进程不存在、权限受限与探测失败。
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(format!("kill({pid}, 0): {error}")),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!(
            "managed session process probing is unsupported on {}",
            std::env::consts::OS
        ))
    }
}

/// Assert that both processes in one managed sidecar tree exit before a deadline.
/// 断言一个受管 sidecar 进程树中的两个进程都会在截止时间前退出。
fn assert_managed_session_process_tree_exits(
    process_tree: ManagedSessionProcessTree,
    timeout: Duration,
) {
    // Deadline bounds platform scheduling and asynchronous Job/process-group teardown.
    // Deadline 为平台调度与异步 Job/进程组清理设置上限。
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // RootAlive and ChildAlive preserve probe failures as test failures.
        // RootAlive 与 ChildAlive 会把探测失败保留为测试失败。
        let root_alive = managed_session_process_exists(process_tree.root_pid)
            .expect("probe managed session root process");
        let child_alive = managed_session_process_exists(process_tree.child_pid)
            .expect("probe managed session descendant process");
        if !root_alive && !child_alive {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("managed session process tree did not exit before timeout: {process_tree:?}");
}

/// Drain every currently queued managed-session event without waiting.
/// 以非等待方式取出当前所有排队的受管会话事件。
fn drain_managed_session_events(center: &ManagedSessionEventCenter) {
    loop {
        // Batch removes up to all logical slots supported by the service limit.
        // Batch 最多取出服务上限支持的全部逻辑槽。
        let batch = center.poll(256).expect("drain managed session events");
        if batch.events.is_empty() && batch.remaining == 0 {
            return;
        }
    }
}

/// Wait until all requested event kinds have been observed from real background activity.
/// 等待从真实后台活动中观察到全部请求的事件类型。
fn wait_for_managed_session_event_kinds(
    center: &ManagedSessionEventCenter,
    expected: &[RuntimeManagedSessionEventKind],
    timeout: Duration,
) -> Vec<RuntimeManagedSessionEvent> {
    // Deadline bounds the durable event wait loop.
    // Deadline 为持久事件等待循环设置上限。
    let deadline = Instant::now() + timeout;
    // Events retains sequence order across multiple bounded batches.
    // Events 跨多个有界批次保留序号顺序。
    let mut events: Vec<RuntimeManagedSessionEvent> = Vec::new();
    loop {
        if expected
            .iter()
            .all(|kind| events.iter().any(|event| event.kind == *kind))
        {
            return events;
        }
        // RemainingMillis is recomputed without allowing a zero wait before the deadline.
        // RemainingMillis 会重新计算，并避免在截止前出现零等待。
        let remaining_millis = deadline
            .checked_duration_since(Instant::now())
            .map(|remaining| remaining.as_millis().max(1) as u64)
            .unwrap_or(0);
        assert!(
            remaining_millis > 0,
            "managed session events timed out: {events:?}"
        );
        // Batch waits for at least one new durable event or reports timeout explicitly.
        // Batch 等待至少一个新的持久事件，或显式报告超时。
        let batch = center
            .wait(256, remaining_millis)
            .expect("wait for managed session events");
        assert!(
            !batch.timed_out,
            "managed session events timed out: {events:?}"
        );
        events.extend(batch.events);
    }
}

/// One real strict System Plugin layout used by direct engine tests.
/// 直接引擎测试使用的真实严格 System Plugin 布局。
struct SystemRuntimeTestLayout {
    /// Canonical runtime root configured on the engine.
    /// 配置到引擎上的规范运行时根目录。
    runtime_root: PathBuf,
    /// Canonical System Lua trust root.
    /// 规范 System Lua 信任根目录。
    system_lua_lib_dir: PathBuf,
    /// Stable System Plugin package identifier.
    /// 稳定的 System Plugin 包标识符。
    package_id: String,
    /// Canonical System Plugin package root.
    /// 规范 System Plugin 包根目录。
    package_root: PathBuf,
    /// Canonical dependency manifest file.
    /// 规范依赖清单文件。
    dependencies_file: PathBuf,
}

impl SystemRuntimeTestLayout {
    /// Create one isolated real System Plugin package layout.
    /// 创建一个隔离的真实 System Plugin 包布局。
    ///
    /// The label parameter partitions temporary layouts created by independent tests.
    /// label 参数用于隔离不同测试创建的临时布局。
    ///
    /// Return canonical paths ready for strict System lease creation.
    /// 返回可直接用于严格 System 租约创建的规范路径。
    fn new(label: &str) -> Self {
        let runtime_root = make_temp_runtime_root(label);
        let _ = fs::remove_dir_all(&runtime_root);
        let system_lua_lib_dir = runtime_root.join("system_lua_lib");
        let package_id = "vulcan-debug".to_string();
        let package_root = system_lua_lib_dir.join(&package_id);
        let dependencies_file = package_root.join("dependencies.yaml");
        fs::create_dir_all(&package_root).expect("create System runtime test package root");
        fs::write(&dependencies_file, "{}\n")
            .expect("write System runtime test dependency manifest");
        Self {
            runtime_root: fs::canonicalize(&runtime_root)
                .expect("canonicalize System runtime test root"),
            system_lua_lib_dir: fs::canonicalize(&system_lua_lib_dir)
                .expect("canonicalize System runtime trust root"),
            package_id,
            package_root: fs::canonicalize(&package_root)
                .expect("canonicalize System runtime package root"),
            dependencies_file: fs::canonicalize(&dependencies_file)
                .expect("canonicalize System runtime dependency manifest"),
        }
    }

    /// Return host options bound to the real runtime and System trust roots.
    /// 返回绑定真实运行时根与 System 信任根的宿主选项。
    fn host_options(&self) -> LuaRuntimeHostOptions {
        LuaRuntimeHostOptions {
            runtime_root: Some(self.runtime_root.clone()),
            system_lua_lib_dir: Some(self.system_lua_lib_dir.clone()),
            ..Default::default()
        }
    }

    /// Return one strict direct-engine System lease create request.
    /// 返回一个严格的直接引擎 System 租约创建请求。
    ///
    /// The sid parameter is the stable host-provided lease identity.
    /// sid 参数是宿主提供的稳定租约身份。
    ///
    /// Return a request without high-level FFI-only authority or public module roots.
    /// 返回不含高层 FFI 专属 authority 或公开模块根的请求。
    fn create_request(&self, sid: &str) -> Value {
        json!({
            "sid": sid,
            "ttl_sec": 60,
            "system_package": {
                "id": self.package_id.as_str(),
                "root": render_host_visible_path(&self.package_root),
                "dependencies_file": "dependencies.yaml"
            }
        })
    }

    /// Return the canonical package descriptor expected in lease responses.
    /// 返回租约响应中预期的规范包描述符。
    fn expected_package_json(&self) -> Value {
        json!({
            "id": self.package_id.as_str(),
            "root": render_host_visible_path(&self.package_root),
            "dependencies_file": render_host_visible_path(&self.dependencies_file)
        })
    }
}

impl Drop for SystemRuntimeTestLayout {
    /// Remove the isolated runtime layout after the owning engine has been dropped.
    /// 在所属引擎释放后删除隔离运行时布局。
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.runtime_root);
    }
}

/// Verify System lease replacement and close retire each exact package owner from the worker service.
/// 验证 System 租约替换与关闭会分别从 Worker 服务退役对应的精确包所有者。
#[test]
fn system_lease_replace_and_close_retire_worker_owners() {
    let layout = SystemRuntimeTestLayout::new("system-worker-owner-retirement");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    // First active System lease whose package owner remains reusable until replacement.
    // 其包所有者在替换前保持可复用的首个活动 System 租约。
    let first: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &layout.create_request("system-worker-owner").to_string(),
            )
            .expect("create first System worker-owner lease"),
    )
    .expect("decode first System worker-owner lease");
    assert_eq!(first["ok"], true);
    assert!(
        engine
            .managed_runtime_workers
            .lock_pool()
            .buckets
            .is_empty()
    );

    // Replacement request committing a new package lifetime under the same SID.
    // 在同一 SID 下提交新包生命周期的替换请求。
    let mut replacement_request = layout.create_request("system-worker-owner");
    replacement_request["replace"] = json!(true);
    let replacement: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&replacement_request.to_string())
            .expect("replace System worker-owner lease"),
    )
    .expect("decode replacement System worker-owner lease");
    assert_eq!(replacement["ok"], true);
    assert!(
        engine
            .managed_runtime_workers
            .lock_pool()
            .buckets
            .is_empty()
    );

    // Close request retiring the replacement generation's distinct package owner.
    // 退役替换代际独立包所有者的关闭请求。
    let close_request = json!({
        "lease_id": replacement["lease_id"],
        "sid": replacement["sid"],
        "generation": replacement["generation"],
    });
    let closed: Value = serde_json::from_str(
        &engine
            .close_system_runtime_lease_json(&close_request.to_string())
            .expect("close replacement System worker-owner lease"),
    )
    .expect("decode closed System worker-owner lease");
    assert_eq!(closed["ok"], true);
    assert!(
        engine
            .managed_runtime_workers
            .lock_pool()
            .buckets
            .is_empty()
    );
}

/// Verify one prune pass retires every expired System package owner, not only one lease.
/// 验证单次清理会退役全部过期 System 包所有者，而不只是一个租约。
#[test]
fn system_lease_prune_retires_all_expired_worker_owners() {
    let layout = SystemRuntimeTestLayout::new("system-worker-owner-prune");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    for sid in ["expired-worker-owner-a", "expired-worker-owner-b"] {
        // Finite request producing a distinct package owner for each SID.
        // 为每个 SID 生成独立包所有者的有限期请求。
        let mut request = layout.create_request(sid);
        request["ttl_sec"] = json!(1);
        let created: Value = serde_json::from_str(
            &engine
                .create_system_runtime_lease_json(&request.to_string())
                .expect("create expiring System worker-owner lease"),
        )
        .expect("decode expiring System worker-owner lease");
        assert_eq!(created["ok"], true);
    }
    thread::sleep(Duration::from_millis(1_200));
    // Listing performs one manager prune that must enqueue both expired owners.
    // 列表操作执行一次管理器清理，且必须将两个过期所有者全部入队。
    let listed: Value = serde_json::from_str(
        &engine
            .list_system_runtime_leases_json("{}")
            .expect("list after System worker-owner expiry"),
    )
    .expect("decode System worker-owner list");
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["leases"].as_array().map(Vec::len), Some(0));
    assert!(
        engine
            .managed_runtime_workers
            .lock_pool()
            .buckets
            .is_empty()
    );
}

/// Verify System leases preserve explicit cwd and echo one canonical package across create, status, and list.
/// 验证 System 租约会保留显式 cwd，并在 create、status 与 list 中回显同一规范包。
#[test]
fn system_runtime_lease_preserves_explicit_cwd_and_package_identity() {
    let layout = SystemRuntimeTestLayout::new("system-runtime-lease-cwd");
    let explicit_cwd = layout.runtime_root.join("host-cwd");
    fs::create_dir_all(&explicit_cwd).expect("create explicit host cwd");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let mut create_request = layout.create_request("system-cwd-test");
    create_request["workspace_root"] = json!(render_host_visible_path(&layout.runtime_root));
    create_request["cwd"] = json!(render_host_visible_path(&explicit_cwd));

    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&create_request.to_string())
            .expect("create system runtime lease"),
    )
    .expect("system runtime lease create response json");
    assert_eq!(created["ok"], true);
    assert_eq!(
        created["cwd"],
        json!(render_host_visible_path(&explicit_cwd))
    );
    assert_eq!(
        created["system_lua_lib"],
        json!(render_host_visible_path(&layout.system_lua_lib_dir))
    );
    assert_eq!(created["system_package"], layout.expected_package_json());

    let lease_id = created["lease_id"]
        .as_str()
        .expect("lease id should be present")
        .to_string();
    let generation = created["generation"]
        .as_u64()
        .expect("generation should be present");
    let status_request = json!({
        "lease_id": lease_id,
        "generation": generation
    });
    let status: Value = serde_json::from_str(
        &engine
            .system_runtime_lease_status_json(&status_request.to_string())
            .expect("status system runtime lease"),
    )
    .expect("system runtime lease status response json");
    assert_eq!(status["ok"], true);
    assert_eq!(
        status["cwd"],
        json!(render_host_visible_path(&explicit_cwd))
    );
    assert_eq!(
        status["system_lua_lib"],
        json!(render_host_visible_path(&layout.system_lua_lib_dir))
    );
    assert_eq!(status["system_package"], layout.expected_package_json());

    let listed: Value = serde_json::from_str(
        &engine
            .list_system_runtime_leases_json("{}")
            .expect("list system runtime leases"),
    )
    .expect("system runtime lease list response json");
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["leases"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        listed["leases"][0]["system_package"],
        layout.expected_package_json()
    );

    let eval_request = json!({
        "lease_id": lease_id,
        "generation": generation,
        "code": "return { cwd = vulcan.runtime.cwd() }"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(&eval_request.to_string())
            .expect("eval system runtime lease"),
    )
    .expect("system runtime lease eval response json");
    assert_eq!(eval["ok"], true);
    assert_eq!(eval["cwd"], json!(render_host_visible_path(&explicit_cwd)));
    assert_eq!(
        eval["system_lua_lib"],
        json!(render_host_visible_path(&layout.system_lua_lib_dir))
    );
    assert_eq!(eval["system_package"], layout.expected_package_json());
    assert_eq!(
        eval["result"]["cwd"],
        json!(render_host_visible_path(&explicit_cwd))
    );
}

/// Verify System Lease eval preserves protocol container types and exposes Lua-compatible search paths.
/// 验证 System Lease eval 会保留协议容器类型并暴露 Lua 兼容搜索路径。
#[test]
fn system_runtime_lease_preserves_json_protocol_types_and_search_path_spelling() {
    // Strict System package layout whose canonical Windows paths may carry verbatim prefixes internally.
    // 严格 System 包布局；其 Windows 规范路径在内部可能带有 verbatim 前缀。
    let layout = SystemRuntimeTestLayout::new("system-json-path-fidelity");
    // Engine configured with the real System trust root and package identity.
    // 配置了真实 System 信任根与包身份的引擎。
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    // Created lease response from the production JSON create surface.
    // 由生产 JSON 创建表面返回的租约响应。
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &layout
                    .create_request("system-json-path-fidelity")
                    .to_string(),
            )
            .expect("create System JSON fidelity lease"),
    )
    .expect("decode System JSON fidelity create response");
    assert_eq!(created["ok"], true);

    // Eval request exercising decode, encode, lease result conversion, package.path, and package.cpath.
    // 覆盖解码、编码、租约结果转换、package.path 与 package.cpath 的执行请求。
    let eval_request = json!({
        "lease_id": created["lease_id"],
        "generation": created["generation"],
        "code": r#"
local protocol = vulcan.json.decode([[{
  "environment": {},
  "binary_path_requests": [],
  "contributions": [],
  "optional": null,
  "nullable_items": [null]
}]])
return {
  protocol = protocol,
  encoded = vulcan.json.encode(protocol),
  package_path = package.path,
  package_cpath = package.cpath,
}
"#
    });
    // Eval response decoded from the stable production lease JSON envelope.
    // 从稳定生产租约 JSON 包络解码的执行响应。
    let evaluated: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(&eval_request.to_string())
            .expect("eval System JSON fidelity lease"),
    )
    .expect("decode System JSON fidelity eval response");
    assert_eq!(evaluated["ok"], true, "unexpected eval: {evaluated}");
    // Canonical protocol shape required by System Plugin Shell Hook consumers.
    // System Plugin Shell Hook 消费方要求的规范协议结构。
    let expected_protocol = json!({
        "environment": {},
        "binary_path_requests": [],
        "contributions": [],
        "optional": null,
        "nullable_items": [null]
    });
    assert_eq!(evaluated["result"]["protocol"], expected_protocol);
    // Encoded JSON text parsed independently to avoid depending on object key order.
    // 独立解析编码 JSON 文本，避免依赖对象键顺序。
    let encoded_protocol: Value = serde_json::from_str(
        evaluated["result"]["encoded"]
            .as_str()
            .expect("encoded protocol text"),
    )
    .expect("parse encoded protocol text");
    assert_eq!(encoded_protocol, expected_protocol);

    #[cfg(windows)]
    {
        // Lua module search strings must never retain the Windows verbatim prefix.
        // Lua 模块搜索字符串绝不能保留 Windows verbatim 前缀。
        let package_path = evaluated["result"]["package_path"]
            .as_str()
            .expect("System package.path text");
        let package_cpath = evaluated["result"]["package_cpath"]
            .as_str()
            .expect("System package.cpath text");
        assert!(!package_path.contains(r"\\?\"), "{package_path}");
        assert!(!package_cpath.contains(r"\\?\"), "{package_cpath}");
    }

    // Close request proving the dedicated VM can be retired after the regression check.
    // 证明回归检查后可退役专用虚拟机的关闭请求。
    let closed: Value = serde_json::from_str(
        &engine
            .close_system_runtime_lease_json(
                &json!({
                    "lease_id": created["lease_id"],
                    "generation": created["generation"]
                })
                .to_string(),
            )
            .expect("close System JSON fidelity lease"),
    )
    .expect("decode System JSON fidelity close response");
    assert_eq!(closed["ok"], true);
}

/// Verify a Windows child process inherits the authorized package cwd without verbatim syntax.
/// 验证 Windows 子进程继承不含 verbatim 语法的已授权包 cwd。
#[cfg(windows)]
#[test]
fn system_runtime_lease_process_session_inherits_non_verbatim_package_cwd() {
    let layout = SystemRuntimeTestLayout::new("system-runtime-process-cwd");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &layout.create_request("system-process-cwd-test").to_string(),
            )
            .expect("create System process-cwd lease"),
    )
    .expect("decode System process-cwd lease");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("System process-cwd lease id")
        .to_string();
    let generation = created["generation"]
        .as_u64()
        .expect("System process-cwd lease generation");
    // EvalRequest launches cmd without an explicit cwd so it must inherit the lease package root.
    // EvalRequest 启动未显式指定 cwd 的 cmd，因此它必须继承租约包根。
    let eval_request = json!({
        "lease_id": lease_id,
        "generation": generation,
        "code": r#"
local session = vulcan.process.session.open({
  program = "cmd",
  args = { "/D", "/S", "/C", "cd" },
  encoding = "utf-8",
})
local status = session:close({ timeout_ms = 3000 })
local output = session:read({ timeout_ms = 3000, max_bytes = 8192 })
return {
  stdout = output.stdout,
  stderr = output.stderr,
  exited = status.exited,
  success = status.success,
}
"#
    });
    let evaluated: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(&eval_request.to_string())
            .expect("eval System process-cwd lease"),
    )
    .expect("decode System process-cwd eval");
    assert_eq!(evaluated["ok"], true);
    assert_eq!(evaluated["result"]["exited"], true);
    assert_eq!(evaluated["result"]["success"], true);
    assert_eq!(
        evaluated["result"]["stdout"]
            .as_str()
            .expect("cmd cwd stdout")
            .trim(),
        render_host_visible_path(&layout.package_root)
    );
    assert!(
        evaluated["result"]["stderr"]
            .as_str()
            .expect("cmd cwd stderr")
            .trim()
            .is_empty(),
        "cmd must not reject the lease cwd as a UNC path: {evaluated}"
    );
}

/// Verify System workspace and cwd inputs are canonical authorization boundaries, not display data.
/// 验证 System 工作区与 cwd 输入属于规范授权边界，而非仅用于显示的数据。
#[test]
fn system_runtime_lease_rejects_relative_workspace_and_unauthorized_cwd() {
    let layout = SystemRuntimeTestLayout::new("system-runtime-path-authorization");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    // Relative workspace roots depend on ambient process cwd and must never become authorization roots.
    // 相对工作区根依赖进程环境 cwd，绝不能成为授权根。
    let mut relative_workspace = layout.create_request("system-relative-workspace");
    relative_workspace["workspace_root"] = json!("relative-workspace");
    let workspace_error = engine
        .create_system_runtime_lease_json(&relative_workspace.to_string())
        .expect_err("relative System workspace root must be rejected");
    assert!(workspace_error.contains("workspace_root must be an absolute path"));

    // Existing cwd outside both the package and an absent workspace must also be rejected.
    // 位于包外且没有工作区授权的既有 cwd 也必须被拒绝。
    let outside_cwd = layout.runtime_root.join("unauthorized-cwd");
    fs::create_dir_all(&outside_cwd).expect("create unauthorized System cwd");
    let mut unauthorized_cwd = layout.create_request("system-unauthorized-cwd");
    unauthorized_cwd["cwd"] = json!(render_host_visible_path(&outside_cwd));
    let cwd_error = engine
        .create_system_runtime_lease_json(&unauthorized_cwd.to_string())
        .expect_err("unauthorized System cwd must be rejected");
    assert!(cwd_error.contains("outside the System package root"));
}

/// Verify System workspace and cwd authorization remains bound to activation-time directory objects.
/// 验证 System 工作区与 cwd 授权始终绑定到激活时的目录对象。
#[test]
fn system_runtime_lease_rejects_replaced_workspace_and_cwd_objects() {
    // Layout and engine provide one strict System package shared by two independent leases.
    // Layout 与引擎提供由两个独立租约共享的严格 System 包。
    let layout = SystemRuntimeTestLayout::new("system-runtime-path-identity");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    // CwdWorkspace remains stable while only its selected cwd object is replaced at the same name.
    // CwdWorkspace 保持稳定，仅其选定 cwd 对象在同名位置被替换。
    let cwd_workspace = layout.runtime_root.join("cwd-workspace");
    let authorized_cwd = cwd_workspace.join("selected-cwd");
    fs::create_dir_all(&authorized_cwd).expect("create identity-bound System cwd");
    let mut cwd_request = layout.create_request("system-cwd-object-identity");
    cwd_request["workspace_root"] = json!(render_host_visible_path(&cwd_workspace));
    cwd_request["cwd"] = json!(render_host_visible_path(&authorized_cwd));
    let cwd_lease: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&cwd_request.to_string())
            .expect("create cwd-identity System lease"),
    )
    .expect("decode cwd-identity System lease");
    assert_eq!(cwd_lease["ok"], true);
    let retired_cwd = cwd_workspace.join("retired-cwd");
    fs::rename(&authorized_cwd, &retired_cwd).expect("retire original System cwd object");
    fs::create_dir(&authorized_cwd).expect("replace System cwd at the authorized name");
    let cwd_eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": cwd_lease["lease_id"],
                    "sid": cwd_lease["sid"],
                    "generation": cwd_lease["generation"],
                    "code": "return true",
                })
                .to_string(),
            )
            .expect("encode replaced-cwd evaluation"),
    )
    .expect("decode replaced-cwd evaluation");
    assert_eq!(cwd_eval["ok"], false);
    assert!(
        cwd_eval["message"]
            .as_str()
            .is_some_and(|message| message.contains("cwd filesystem object changed")),
        "unexpected replaced-cwd response: {cwd_eval}"
    );

    // WorkspaceOnly is independently captured even when the effective cwd defaults to the package.
    // WorkspaceOnly 即使在有效 cwd 默认采用包根时也会被独立捕获。
    let workspace_only = layout.runtime_root.join("workspace-only");
    fs::create_dir(&workspace_only).expect("create identity-bound System workspace");
    let mut workspace_request = layout.create_request("system-workspace-object-identity");
    workspace_request["workspace_root"] = json!(render_host_visible_path(&workspace_only));
    let workspace_lease: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&workspace_request.to_string())
            .expect("create workspace-identity System lease"),
    )
    .expect("decode workspace-identity System lease");
    assert_eq!(workspace_lease["ok"], true);
    let retired_workspace = layout.runtime_root.join("retired-workspace");
    fs::rename(&workspace_only, &retired_workspace)
        .expect("retire original System workspace object");
    fs::create_dir(&workspace_only).expect("replace System workspace at the authorized name");
    let workspace_eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": workspace_lease["lease_id"],
                    "sid": workspace_lease["sid"],
                    "generation": workspace_lease["generation"],
                    "code": "return true",
                })
                .to_string(),
            )
            .expect("encode replaced-workspace evaluation"),
    )
    .expect("decode replaced-workspace evaluation");
    assert_eq!(workspace_eval["ok"], false);
    assert!(
        workspace_eval["message"]
            .as_str()
            .is_some_and(|message| message.contains("workspace_root filesystem object changed")),
        "unexpected replaced-workspace response: {workspace_eval}"
    );
}

/// Verify a persistent System VM refuses evaluation after its activated package root disappears.
/// 验证持久 System VM 在已激活包根消失后会拒绝执行。
#[test]
fn system_runtime_lease_revalidates_package_root_before_each_eval() {
    let layout = SystemRuntimeTestLayout::new("system-runtime-live-root-revalidation");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &layout
                    .create_request("system-live-root-revalidation")
                    .to_string(),
            )
            .expect("create System live-root lease"),
    )
    .expect("decode System live-root lease");
    assert_eq!(created["ok"], true);
    fs::remove_dir_all(&layout.package_root).expect("remove activated System package root");
    let evaluated: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": created["lease_id"],
                    "sid": created["sid"],
                    "generation": created["generation"],
                    "code": "return true",
                })
                .to_string(),
            )
            .expect("encode failed live-root evaluation"),
    )
    .expect("decode failed live-root evaluation");

    assert_eq!(evaluated["ok"], false);
    assert_eq!(evaluated["error_code"], "eval_failed");
    assert!(
        evaluated["message"]
            .as_str()
            .is_some_and(|message| message.contains("failed to revalidate package root"))
    );
}

/// Verify System Plugin context containers reject normal and raw mutation while remaining iterable and serializable.
/// 验证 System Plugin 上下文容器会拒绝普通及 raw 修改，同时保持可迭代与可序列化。
#[test]
fn system_runtime_plugin_context_is_deeply_read_only() {
    // Strict package layout whose mounts include nested object and array containers.
    // 挂载信息包含嵌套对象与数组容器的严格包布局。
    let layout = SystemRuntimeTestLayout::new("system-runtime-readonly-context");
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let mut create_request = layout.create_request("system-readonly-context");
    create_request["mounts"] = json!({
        "nested": { "value": "original" },
        "array": [2, 3, 5]
    });
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(&create_request.to_string())
            .expect("create read-only System context lease"),
    )
    .expect("decode read-only System context lease");
    assert_eq!(created["ok"], true);

    // Lua probes direct assignment, rawset bypass attempts, pairs/ipairs, length, and host serialization.
    // Lua 探测直接赋值、rawset 绕过尝试、pairs/ipairs、长度及宿主序列化。
    let eval_request = json!({
        "lease_id": created["lease_id"],
        "sid": created["sid"],
        "generation": created["generation"],
        "code": r#"
local plugin = vulcan.runtime.system_plugin
local mounts = vulcan.runtime.mounts
local direct_ok = pcall(function() plugin.id = 'mutated' end)
local raw_ok = pcall(function() rawset(plugin, 'id', 'mutated') end)
local nested_ok = pcall(function() mounts.nested.value = 'mutated' end)
local runtime_direct_ok = pcall(function() vulcan.runtime.system_plugin = { id = 'mutated' } end)
local runtime_setmetatable_ok = pcall(function() setmetatable(vulcan.runtime, {}) end)
local require_debug_ok = pcall(function() return require('debug') end)
local pair_count = 0
for _key, _value in pairs(plugin) do pair_count = pair_count + 1 end
local array_sum = 0
for _index, value in ipairs(mounts.array) do array_sum = array_sum + value end
return {
    direct_ok = direct_ok,
    raw_ok = raw_ok,
    nested_ok = nested_ok,
    runtime_direct_ok = runtime_direct_ok,
    runtime_setmetatable_ok = runtime_setmetatable_ok,
    rawset_type = type(rawset),
    debug_type = type(debug),
    require_debug_ok = require_debug_ok,
    pair_count = pair_count,
    array_len = #mounts.array,
    array_sum = array_sum,
    plugin = plugin,
    mounts = mounts,
}
"#
    });
    let evaluated: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(&eval_request.to_string())
            .expect("evaluate read-only System context"),
    )
    .expect("decode read-only System context evaluation");
    assert_eq!(evaluated["ok"], true, "evaluation failed: {evaluated}");
    let result = &evaluated["result"];
    assert_eq!(result["direct_ok"], false);
    assert_eq!(result["raw_ok"], false);
    assert_eq!(result["nested_ok"], false);
    assert_eq!(result["runtime_direct_ok"], false);
    assert_eq!(result["runtime_setmetatable_ok"], false);
    assert_eq!(result["rawset_type"], "nil");
    assert_eq!(result["debug_type"], "nil");
    assert_eq!(result["require_debug_ok"], false);
    assert_eq!(result["pair_count"], 5);
    assert_eq!(result["array_len"], 3);
    assert_eq!(result["array_sum"], 10);
    assert_eq!(result["plugin"]["id"], layout.package_id);
    assert_eq!(result["plugin"]["sid"], "system-readonly-context");
    assert_eq!(result["mounts"], create_request["mounts"]);
}

/// Verify an active System lease and its VM state survive an ordinary Skill reload.
/// 验证活动 System 租约及其 VM 状态会在普通 Skill 重载后继续存活。
#[test]
fn system_runtime_lease_survives_ordinary_skill_reload() {
    let layout = SystemRuntimeTestLayout::new("system-runtime-lease-reload");
    let skill_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: layout.runtime_root.join("skills"),
    };
    write_minimal_skill_to_root_with_response(
        &skill_root.skills_dir,
        "ordinary-reload-skill",
        "before-reload",
    );
    let mut engine = make_runtime_test_engine_with_host_options(layout.host_options());
    engine
        .load_from_roots(std::slice::from_ref(&skill_root))
        .expect("load ordinary Skill before System lease creation");
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &layout.create_request("system-reload-survival").to_string(),
            )
            .expect("create System lease before ordinary reload"),
    )
    .expect("decode System reload-survival create response");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("System reload-survival lease id")
        .to_string();
    let generation = created["generation"]
        .as_u64()
        .expect("System reload-survival generation");
    let first_eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": lease_id,
                    "generation": generation,
                    "code": "persistent_counter = 1; return persistent_counter"
                })
                .to_string(),
            )
            .expect("evaluate System lease before ordinary reload"),
    )
    .expect("decode pre-reload System eval response");
    assert_eq!(first_eval["ok"], true);
    assert_eq!(first_eval["result"], json!(1));

    write_minimal_skill_to_root_with_response(
        &skill_root.skills_dir,
        "ordinary-reload-skill",
        "after-reload",
    );
    engine
        .reload_from_roots(std::slice::from_ref(&skill_root))
        .expect("reload ordinary Skills without retiring System leases");
    let ordinary_result = engine
        .call_skill("ordinary-reload-skill-ping", &json!({}), None)
        .expect("call reloaded ordinary Skill");
    assert_eq!(ordinary_result.content, "after-reload");

    let second_eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": lease_id,
                    "generation": generation,
                    "code": "persistent_counter = persistent_counter + 1; return persistent_counter"
                })
                .to_string(),
            )
            .expect("evaluate preserved System lease after ordinary reload"),
    )
    .expect("decode post-reload System eval response");
    assert_eq!(second_eval["ok"], true);
    assert_eq!(second_eval["result"], json!(2));
    assert_eq!(
        second_eval["system_package"],
        layout.expected_package_json()
    );
}

/// Verify the dedicated System VM cannot dispatch ordinary loaded Skill entries through vulcan.call.
/// 验证专用 System VM 无法通过 vulcan.call 分发普通已加载 Skill 入口。
#[test]
fn system_runtime_lease_dedicated_vm_cannot_dispatch_ordinary_skill() {
    let layout = SystemRuntimeTestLayout::new("system-runtime-lease-isolation");
    let skill_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: layout.runtime_root.join("skills"),
    };
    write_minimal_skill_to_root_with_response(
        &skill_root.skills_dir,
        "ordinary-isolation-skill",
        "ordinary-visible",
    );
    let mut engine = make_runtime_test_engine_with_host_options(layout.host_options());
    engine
        .load_from_roots(std::slice::from_ref(&skill_root))
        .expect("load ordinary Skill before isolated System VM creation");
    let ordinary_result = engine
        .call_skill("ordinary-isolation-skill-ping", &json!({}), None)
        .expect("ordinary host dispatch should remain available");
    assert_eq!(ordinary_result.content, "ordinary-visible");
    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &layout
                    .create_request("system-dispatch-isolation")
                    .to_string(),
            )
            .expect("create isolated System runtime lease"),
    )
    .expect("decode isolated System lease create response");
    let eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "lease_id": created["lease_id"],
                    "generation": created["generation"],
                    "code": "return vulcan.call('ordinary-isolation-skill-ping', {})"
                })
                .to_string(),
            )
            .expect("evaluate isolated System Skill dispatch attempt"),
    )
    .expect("decode isolated System dispatch response");
    assert_eq!(eval["ok"], false);
    assert_eq!(eval["error_code"], json!("eval_failed"));
    let message = eval["message"]
        .as_str()
        .expect("isolated System dispatch failure message");
    assert!(
        message.contains("Skill 'ordinary-isolation-skill-ping' not found"),
        "unexpected isolation error: {message}"
    );
}

/// Open one session and synchronously read its authoritative startup record.
/// 打开一个会话并同步读取其权威启动记录。
fn open_and_capture_managed_session_tree(
    engine: &LuaEngine,
    layout: &ManagedSessionSystemLayout,
    lease: &ManagedSessionLeaseHandle,
    global_name: &str,
    args: &[String],
) -> ManagedSessionProcessTree {
    // OpenResponse proves the userdata was stored in the persistent lease VM.
    // OpenResponse 证明 userdata 已保存到持久租约 VM 中。
    let open_response = eval_managed_session_test_lease(
        engine,
        lease,
        &managed_session_open_lua(layout, global_name, 4096, args),
    );
    assert_eq!(open_response["ok"], true, "open failed: {open_response}");
    // ReadResponse waits for sidecar initialization and returns the startup JSON line.
    // ReadResponse 等待 sidecar 初始化并返回启动 JSON 行。
    let read_response = eval_managed_session_test_lease(
        engine,
        lease,
        &format!(
            "return {global_name}:read({{ timeout_ms = 5000, max_bytes = 4096, until_text = 'started' }})"
        ),
    );
    assert_eq!(
        read_response["ok"], true,
        "startup read failed: {read_response}"
    );
    parse_managed_session_started(&read_response["result"]).0
}

/// Assert that no live Node session snapshot remains under the ready environment.
/// 断言就绪环境下不再保留任何活动 Node 会话快照。
fn assert_no_active_node_session_snapshots(layout: &ManagedSessionSystemLayout) {
    // SnapshotsRoot may remain as an empty private namespace after deterministic cleanup.
    // SnapshotsRoot 在确定性清理后可以保留为空的私有命名空间。
    let snapshots_root = layout.env_dir.join(".ls-s");
    let Ok(mut snapshots) = fs::read_dir(&snapshots_root) else {
        return;
    };
    assert!(
        snapshots.next().is_none(),
        "active Node session snapshot leaked under {}",
        render_host_visible_path(&snapshots_root)
    );
}

/// Exercise real managed status and repeated invoke calls inside one strict System lease.
/// 在一个严格 System 租约内验证真实受管 status 与重复 invoke 调用。
///
/// `runtime` selects the production Lua surface, `layout_label` partitions its filesystem,
/// `expect_long_source` requires Windows long-path evidence, and `host_configured_roots` selects
/// independent external distribution and environment authorities.
/// `runtime` 选择生产 Lua 接口，`layout_label` 隔离其文件系统，`expect_long_source` 要求返回
/// Windows 长路径证据，`host_configured_roots` 选择独立外部发行与环境授权。
///
/// Returns unit after status, cold invoke, warm reuse, isolation, and optional long-path assertions.
/// 完成 status、冷调用、热复用、隔离及可选长路径断言后返回空值。
fn run_managed_runtime_status_invoke_integration(
    runtime: ManagedSessionTestRuntime,
    layout_label: &str,
    expect_long_source: bool,
    host_configured_roots: bool,
) {
    // Host is absent only when the requested real interpreter is unavailable.
    // Host 仅在请求的真实解释器不可用时为空。
    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    let _env_guard = process_env_test_guard();
    // Restore guard paired with a representative host secret injected before worker launch.
    // 与 Worker 启动前注入的代表性宿主密钥配对的恢复保护器。
    let _secret_restore_guard = TestEnvRestoreGuard::capture("LUASKILLS_TEST_HOST_SECRET");
    unsafe {
        std::env::set_var(
            "LUASKILLS_TEST_HOST_SECRET",
            "must-not-cross-managed-worker-boundary",
        );
    }
    // Layout contains the real handler and one package-specific ready dependency tree.
    // Layout 包含真实处理器与一个包专属就绪依赖树。
    let layout = if expect_long_source {
        assert!(
            !host_configured_roots,
            "long-path and host-root fixture modes must remain independently attributable"
        );
        // RuntimeVersion lengthens only the managed environment group; the System package cwd stays short.
        // RuntimeVersion 仅加长受管环境分组；System 包 cwd 保持较短。
        let runtime_version = managed_session_long_path_runtime_version(runtime, &host.version);
        ManagedSessionSystemLayout::new_with_runtime_version(
            layout_label,
            Some(&runtime_version),
            runtime,
            &host,
        )
    } else if host_configured_roots {
        ManagedSessionSystemLayout::new_with_host_configured_roots(layout_label, runtime, &host)
    } else {
        ManagedSessionSystemLayout::new(layout_label, runtime, &host)
    };
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-status-invoke-{}", runtime.label()),
        false,
    );
    // Eval performs status and two sequential invokes through the actual managed worker protocol.
    // Eval 通过真实受管 Worker 协议执行 status 与两次顺序 invoke。
    let eval = eval_managed_session_test_lease(
        &engine,
        &lease,
        &format!(
            "local status = {}()\nlocal first = {}({{ file = '{}', args = {{ value = 'first' }}, timeout_ms = 5000 }})\nlocal second = {}({{ file = '{}', args = {{ value = 'second' }}, timeout_ms = 5000 }})\nreturn {{ status = status, first = first, second = second }}",
            layout.lua_status_api(),
            layout.lua_invoke_api(),
            layout.invoke_file(),
            layout.lua_invoke_api(),
            layout.invoke_file(),
        ),
    );
    assert_eq!(
        eval["ok"], true,
        "managed status/invoke eval failed: {eval}"
    );
    // Status proves the System package declaration resolves to the prevalidated real runtime.
    // Status 证明 System 包声明解析到预验证的真实运行时。
    let status = &eval["result"]["status"];
    assert_eq!(status["available"], true);
    assert_eq!(status["configured"], true);
    assert_eq!(status["ready"], true);
    assert_eq!(status["runtime"], runtime.label());
    assert_eq!(status["runtime_version"], layout.runtime_version);
    assert_eq!(status["env_dir"], render_host_visible_path(&layout.env_dir));
    // ExpectedRootSource distinguishes explicit host authority from the compatible derived layout.
    // ExpectedRootSource 区分显式宿主授权与兼容派生布局。
    let expected_root_source = if host_configured_roots {
        "host_configured"
    } else {
        "runtime_root_default"
    };
    assert_eq!(status["distribution_source"], expected_root_source);
    assert_eq!(status["environment_source"], expected_root_source);
    assert_eq!(
        status["distribution_root"],
        render_host_visible_path(&layout.distribution_root)
    );
    assert_eq!(
        status["environment_root"],
        render_host_visible_path(&layout.environment_root)
    );
    // FirstResult proves arguments, dependency resolution, trusted context, and output capture.
    // FirstResult 证明参数、依赖解析、可信上下文与输出捕获均有效。
    let first = &eval["result"]["first"];
    assert_eq!(first["ok"], true, "first managed invoke failed: {first}");
    assert_eq!(first["status"], 0);
    assert_eq!(first["timed_out"], false);
    assert_eq!(first["worker_reused"], false);
    assert_eq!(first["value"]["counter"], 1);
    assert_eq!(
        first["value"]["dependency_marker"],
        layout.dependency_marker
    );
    assert_eq!(first["value"]["relative_marker"], layout.import_marker());
    assert_eq!(first["value"]["package_id"], layout.base.package_id);
    assert_eq!(first["value"]["value"], "first");
    assert_eq!(first["value"]["host_secret_visible"], false);
    assert!(
        !first["value"]["path"]
            .as_str()
            .expect("first managed invoke controlled PATH")
            .is_empty()
    );
    // SourcePath and WorkerCwd prove execution came from the private snapshot without inheriting
    // an uncontrolled Windows host cwd.
    // SourcePath 与 WorkerCwd 证明执行来自私有快照，且不会继承未受控的 Windows 宿主 cwd。
    let source_path = first["value"]["source_path"]
        .as_str()
        .expect("first managed invoke snapshot source path");
    let worker_cwd = first["value"]["cwd"]
        .as_str()
        .expect("first managed invoke worker cwd");
    assert!(source_path.contains(".ls-w"), "{source_path}");
    assert!(!worker_cwd.is_empty());
    #[cfg(windows)]
    if expect_long_source {
        use std::os::windows::ffi::OsStrExt;

        assert!(
            Path::new(source_path).as_os_str().encode_wide().count() >= 260,
            "expected long Windows snapshot source, got {source_path}"
        );
        assert!(
            Path::new(worker_cwd)
                .as_os_str()
                .encode_wide()
                .count()
                .saturating_add(1)
                < 260,
            "expected short Windows worker cwd, got {worker_cwd}"
        );
        assert_ne!(
            PathBuf::from(worker_cwd),
            std::env::current_dir().expect("resolve host cwd for worker isolation assertion")
        );
    }
    #[cfg(not(windows))]
    assert!(!expect_long_source, "long source assertion is Windows-only");
    assert!(
        first["stdout"]
            .as_str()
            .expect("first managed invoke stdout")
            .contains(&format!("{}-invoke", runtime.label()))
    );
    assert_eq!(first["env_hash"], status["env_hash"]);
    assert_eq!(first["env_dir"], status["env_dir"]);
    // SecondResult proves the warm worker retained only this package module's state.
    // SecondResult 证明热 Worker 只保留当前包模块的状态。
    let second = &eval["result"]["second"];
    assert_eq!(second["ok"], true, "second managed invoke failed: {second}");
    assert_eq!(second["worker_reused"], true);
    assert_eq!(second["value"]["counter"], 2);
    assert_eq!(second["value"]["value"], "second");
    assert_eq!(
        second["value"]["dependency_marker"],
        layout.dependency_marker
    );
    if host_configured_roots {
        // SessionOpen proves the persistent process path consumes the same explicit root plan.
        // SessionOpen 证明持久进程路径消费相同的显式根计划。
        let session_open = eval_managed_session_test_lease(
            &engine,
            &lease,
            &managed_session_open_lua(&layout, "host_root_session", 4096, &[]),
        );
        assert_eq!(
            session_open["ok"], true,
            "host-root session open failed: {session_open}"
        );
        assert_eq!(session_open["result"]["status"]["running"], true);
        // SessionWrite crosses a later eval through the saved userdata and real sidecar stdin.
        // SessionWrite 通过已保存 userdata 与真实 sidecar stdin 跨越后续 eval。
        let session_write = eval_managed_session_test_lease(
            &engine,
            &lease,
            r#"host_root_session:write('{"action":"echo","value":"host-root-session"}\n'); return true"#,
        );
        assert_eq!(
            session_write["ok"], true,
            "host-root session write failed: {session_write}"
        );
        // SessionRead proves stdout is produced by that same persistent host-selected runtime.
        // SessionRead 证明 stdout 来自同一个持久宿主选定运行时。
        let session_read = eval_managed_session_test_lease(
            &engine,
            &lease,
            "return host_root_session:read({ timeout_ms = 5000, max_bytes = 8192, until_text = 'host-root-session' })",
        );
        assert_eq!(
            session_read["ok"], true,
            "host-root session read failed: {session_read}"
        );
        assert!(
            session_read["result"]["stdout"]
                .as_str()
                .expect("host-root session stdout")
                .contains("host-root-session")
        );
        // SessionClose exercises deterministic process-tree cleanup before the enclosing lease closes.
        // SessionClose 在外层租约关闭前验证确定性的进程树清理。
        let session_close = eval_managed_session_test_lease(
            &engine,
            &lease,
            "return host_root_session:close({ timeout_ms = 0 })",
        );
        assert_eq!(
            session_close["ok"], true,
            "host-root session close failed: {session_close}"
        );
    }
    close_managed_session_test_lease(&engine, &lease);
}

/// Return a valid declared runtime version padded enough to force Windows snapshot paths past MAX_PATH.
/// 返回一个有效的已声明运行时版本，并填充到足以使 Windows 快照路径超过 MAX_PATH。
///
/// `runtime` selects the version grammar and `host_version` is the exact discovered interpreter version.
/// `runtime` 选择版本语法，`host_version` 是探测得到的精确解释器版本。
///
/// Returns a runtime-specific valid version used only by long-path integration fixtures.
/// 返回一个仅供长路径集成夹具使用、且符合对应运行时语法的版本。
fn managed_session_long_path_runtime_version(
    runtime: ManagedSessionTestRuntime,
    host_version: &str,
) -> String {
    match runtime {
        ManagedSessionTestRuntime::Python => format!("{}-{}", host_version, "p".repeat(160)),
        ManagedSessionTestRuntime::Node => format!("{}+{}", host_version, "n".repeat(160)),
    }
}

/// Return a Windows runtime version that keeps Python itself short while forcing its snapshot source long.
/// 返回一个 Windows 运行时版本，使 Python 自身路径保持较短、同时强制快照源码成为长路径。
///
/// `layout_label` and `host_version` reproduce the exact Python fixture path grammar before creation.
/// `layout_label` 与 `host_version` 会在创建前复现精确的 Python 夹具路径语法。
///
/// Returns a padded valid version whose virtual-environment interpreter is exactly 245 UTF-16 units.
/// 返回一个经过填充的有效版本，使虚拟环境解释器路径恰为 245 个 UTF-16 代码单元。
#[cfg(windows)]
fn managed_python_session_persistence_long_path_runtime_version(
    layout_label: &str,
    host_version: &str,
) -> String {
    use std::os::windows::ffi::OsStrExt;

    // TargetInterpreterLength leaves explicit room below MAX_PATH while the longer snapshot suffix crosses it.
    // TargetInterpreterLength 在 MAX_PATH 以下留下明确余量，同时让更长的快照后缀越过该边界。
    const TARGET_INTERPRETER_LENGTH: usize = 245;
    // PlaceholderHash has the exact fixed width of every SHA-256 environment identity.
    // PlaceholderHash 与每个 SHA-256 环境身份具有完全相同的固定宽度。
    let placeholder_hash = "0".repeat(64);
    // UnpaddedVersion includes the Python prerelease separator that remains before added padding.
    // UnpaddedVersion 包含添加填充前仍会保留的 Python 预发布分隔符。
    let unpadded_version = format!("{host_version}-");
    // RuntimeRoot is materialized once so Windows expands any 8.3 temporary-directory alias exactly
    // as SystemRuntimeTestLayout will when it canonicalizes the real fixture root.
    // RuntimeRoot 会先实际落盘一次，使 Windows 按 SystemRuntimeTestLayout 规范化真实夹具根时的
    // 相同方式精确展开临时目录中的任何 8.3 别名。
    let runtime_root = make_temp_runtime_root(layout_label);
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create long-source sizing runtime root");
    // CanonicalRuntimeRoot is the exact visible identity used later by managed plan resolution.
    // CanonicalRuntimeRoot 是后续受管计划解析实际使用的精确可见身份。
    let canonical_runtime_root =
        fs::canonicalize(&runtime_root).expect("canonicalize long-source sizing runtime root");
    fs::remove_dir_all(&canonical_runtime_root).expect("remove long-source sizing runtime root");
    // EnvironmentRoot mirrors SystemRuntimeTestLayout and ManagedSessionSystemLayout exactly.
    // EnvironmentRoot 精确镜像 SystemRuntimeTestLayout 与 ManagedSessionSystemLayout。
    let environment_root = canonical_runtime_root.join("dependencies").join("envs");
    // UnpaddedEnvDir uses production layout logic with an equal-width identity placeholder.
    // UnpaddedEnvDir 使用生产布局逻辑及等宽身份占位符。
    let unpadded_env_dir = managed_env_dir(
        &environment_root,
        ManagedRuntimeKind::Python,
        &unpadded_version,
        &placeholder_hash,
    );
    // UnpaddedInterpreter matches the executable suffix used by persistent Python sessions.
    // UnpaddedInterpreter 与持久 Python 会话使用的可执行文件后缀一致。
    let unpadded_interpreter = render_host_visible_path(
        &unpadded_env_dir
            .join(".venv")
            .join("Scripts")
            .join("python.exe"),
    );
    // UnpaddedLength is measured from the same canonical, non-verbatim spelling asserted by the test.
    // UnpaddedLength 使用测试断言采用的同一规范非 verbatim 写法精确计量。
    let unpadded_length = Path::new(&unpadded_interpreter)
        .as_os_str()
        .encode_wide()
        .count();
    // PaddingLength is exact; failure means this runner's temporary root leaves no valid test window.
    // PaddingLength 是精确值；失败表示当前 runner 的临时根没有留下有效测试窗口。
    let padding_length = TARGET_INTERPRETER_LENGTH
        .checked_sub(unpadded_length)
        .filter(|length| *length > 0)
        .unwrap_or_else(|| {
            panic!(
                "persistent-session test root leaves no long-source window: unpadded interpreter length={unpadded_length}"
            )
        });
    format!("{}-{}", host_version, "p".repeat(padding_length))
}

/// Exercise two System Plugin packages with same-named modules and dependencies in one engine.
/// 在一个引擎内验证两个具有同名模块与依赖的 System Plugin 包。
///
/// `runtime` selects the Python or Node dependency and invoke implementation.
/// `runtime` 选择 Python 或 Node 依赖及 invoke 实现。
fn run_managed_system_plugin_dependency_module_isolation_integration(
    runtime: ManagedSessionTestRuntime,
) {
    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    let _env_guard = process_env_test_guard();
    // Primary and Sibling share runtime assets but have different lock inputs and package roots.
    // Primary 与 Sibling 共享运行时资产，但拥有不同锁输入与包根。
    let layout = ManagedSessionSystemLayout::new(
        &format!("managed-plugin-isolation-{}", runtime.label()),
        runtime,
        &host,
    );
    let sibling_dependency_marker = format!("{}-sibling-dependency", runtime.label());
    let sibling = create_managed_session_sibling_system_package(
        &layout,
        &host,
        "vulcan-debug-sibling",
        &sibling_dependency_marker,
    );
    assert_ne!(layout.env_dir, sibling.env_dir);
    assert_eq!(sibling.runtime, runtime);
    assert!(sibling.dependencies_file.is_file());
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let primary_lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-plugin-primary-{}", runtime.label()),
        false,
    );
    let sibling_lease = create_managed_session_test_lease_from_request(
        &engine,
        &sibling.create_request(&format!("system-plugin-sibling-{}", runtime.label())),
    );
    // PrimaryEval loads the shared Lua module name and advances only the primary worker module.
    // PrimaryEval 加载共享 Lua 模块名，并只推进主包 Worker 模块。
    let primary_eval = eval_managed_session_test_lease(
        &engine,
        &primary_lease,
        &format!(
            "local module = require('fixture_module')\nlocal status = {}()\nlocal first = {}({{ file = '{}', args = {{ value = 'primary-first' }}, timeout_ms = 5000 }})\nlocal second = {}({{ file = '{}', args = {{ value = 'primary-second' }}, timeout_ms = 5000 }})\nlocal spam = {}({{ file = '{}', args = {{ value = 'primary-spam', spam = true }}, timeout_ms = 5000 }})\nreturn {{ module_marker = module.marker, status = status, first = first, second = second, spam = spam }}",
            runtime.lua_status_api(),
            runtime.lua_invoke_api(),
            runtime.invoke_file(),
            runtime.lua_invoke_api(),
            runtime.invoke_file(),
            runtime.lua_invoke_api(),
            runtime.invoke_file(),
        ),
    );
    assert_eq!(
        primary_eval["ok"], true,
        "primary package invoke failed: {primary_eval}"
    );
    // SiblingEval uses the same public names while resolving only sibling package artifacts.
    // SiblingEval 使用相同公开名称，同时只解析同级包制品。
    let sibling_eval = eval_managed_session_test_lease(
        &engine,
        &sibling_lease,
        &format!(
            "local module = require('fixture_module')\nlocal status = {}()\nlocal first = {}({{ file = '{}', args = {{ value = 'sibling-first' }}, timeout_ms = 5000 }})\nreturn {{ module_marker = module.marker, status = status, first = first }}",
            runtime.lua_status_api(),
            runtime.lua_invoke_api(),
            runtime.invoke_file(),
        ),
    );
    assert_eq!(
        sibling_eval["ok"], true,
        "sibling package invoke failed: {sibling_eval}"
    );
    // Lua module markers prove package.path/module caches never cross dedicated System VMs.
    // Lua 模块标记证明 package.path/模块缓存不会跨越专用 System VM。
    assert_eq!(
        primary_eval["result"]["module_marker"],
        "system-package-module-ok"
    );
    assert_eq!(
        sibling_eval["result"]["module_marker"],
        sibling.lua_module_marker
    );
    // Environment identities and same-named dependency values prove dependency-tree isolation.
    // 环境身份与同名依赖值证明依赖树隔离。
    let primary_status = &primary_eval["result"]["status"];
    let sibling_status = &sibling_eval["result"]["status"];
    assert_eq!(primary_status["ready"], true);
    assert_eq!(sibling_status["ready"], true);
    assert_ne!(primary_status["env_hash"], sibling_status["env_hash"]);
    assert_eq!(
        primary_status["env_dir"],
        render_host_visible_path(&layout.env_dir)
    );
    assert_eq!(
        sibling_status["env_dir"],
        render_host_visible_path(&sibling.env_dir)
    );
    let primary_first = &primary_eval["result"]["first"];
    let sibling_first = &sibling_eval["result"]["first"];
    assert_eq!(
        primary_first["ok"], true,
        "primary managed result failed: {primary_first}"
    );
    assert_eq!(
        sibling_first["ok"], true,
        "sibling managed result failed: {sibling_first}"
    );
    assert_eq!(
        primary_first["value"]["dependency_marker"],
        layout.dependency_marker
    );
    assert_eq!(
        sibling_first["value"]["dependency_marker"],
        sibling.dependency_marker
    );
    assert_eq!(
        primary_first["value"]["relative_marker"],
        runtime.import_marker()
    );
    assert_eq!(
        sibling_first["value"]["relative_marker"],
        runtime.import_marker()
    );
    assert_eq!(primary_first["value"]["package_id"], layout.base.package_id);
    assert_eq!(sibling_first["value"]["package_id"], sibling.package_id);
    assert_eq!(primary_first["value"]["counter"], 1);
    assert_eq!(primary_eval["result"]["second"]["value"]["counter"], 2);
    let primary_spam = &primary_eval["result"]["spam"];
    assert_eq!(primary_spam["ok"], true);
    assert_eq!(primary_spam["value"]["counter"], 3);
    assert!(
        primary_spam["stdout_dropped_bytes"]
            .as_u64()
            .is_some_and(|dropped| dropped > 0)
    );
    assert!(
        primary_spam["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.len() <= 256 * 1024)
    );
    assert_eq!(sibling_first["value"]["counter"], 1);
    close_managed_session_test_lease(&engine, &primary_lease);
    close_managed_session_test_lease(&engine, &sibling_lease);
}

/// Exercise persistent state, events, security boundaries, and bounded output for one runtime.
/// 针对一个运行时验证持久状态、事件、安全边界与有界输出。
///
/// `runtime` selects the real interpreter and `expect_long_source` requests a Windows path beyond MAX_PATH.
/// `runtime` 选择真实解释器，`expect_long_source` 请求一条超过 MAX_PATH 的 Windows 路径。
///
/// Returns unit after the complete persistent-session and cleanup contract succeeds.
/// 完整持久会话与清理契约成功后返回空值。
fn run_managed_session_persistence_integration(
    runtime: ManagedSessionTestRuntime,
    expect_long_source: bool,
) {
    // Host is optional only when the requested real interpreter is unavailable.
    // Host 仅在请求的真实解释器不可用时才为空。
    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    // Process-wide guard serializes PATH-sensitive child runtime launches.
    // 进程级保护锁串行化依赖 PATH 的子运行时启动。
    let _env_guard = process_env_test_guard();
    // Restore guard paired with a representative host secret injected before session launch.
    // 与会话启动前注入的代表性宿主密钥配对的恢复保护器。
    let _secret_restore_guard = TestEnvRestoreGuard::capture("LUASKILLS_TEST_HOST_SECRET");
    unsafe {
        std::env::set_var(
            "LUASKILLS_TEST_HOST_SECRET",
            "must-not-cross-managed-session-boundary",
        );
    }
    // Layout contains one strict System package and one prevalidated ready environment.
    // Layout 包含一个严格 System 包与一个已预验证的就绪环境。
    let layout_label = if expect_long_source {
        format!("managed-session-long-persistence-{}", runtime.label())
    } else {
        format!("managed-session-persistence-{}", runtime.label())
    };
    // Layout uses a grammar-valid padded version only when the test must prove native long-path launch.
    // 仅当测试必须证明原生长路径启动时，Layout 才使用符合语法的填充版本。
    let layout = if expect_long_source {
        #[cfg(windows)]
        {
            // Long-source layout is intentionally Python-specific because its path window is
            // calculated from the virtual-environment interpreter suffix.
            // 长源码布局有意仅用于 Python，因为其路径窗口按虚拟环境解释器后缀计算。
            assert_eq!(runtime, ManagedSessionTestRuntime::Python);
            let runtime_version = managed_python_session_persistence_long_path_runtime_version(
                &layout_label,
                &host.version,
            );
            ManagedSessionSystemLayout::new_with_runtime_version(
                &layout_label,
                Some(&runtime_version),
                runtime,
                &host,
            )
        }
        #[cfg(not(windows))]
        {
            panic!("long persistent-session source fixture is Windows-only")
        }
    } else {
        ManagedSessionSystemLayout::new(&layout_label, runtime, &host)
    };
    #[cfg(windows)]
    if expect_long_source {
        use std::os::windows::ffi::OsStrExt;

        // MinimumSource uses the shortest possible formatted snapshot identity, so crossing
        // MAX_PATH here proves every real source remains long after prefix filtering.
        // MinimumSource 使用格式允许的最短快照身份，因此这里在过滤前缀后仍超过 MAX_PATH，
        // 足以证明每条真实源码路径也都保持为长路径。
        let minimum_source = render_host_visible_path(
            &layout
                .env_dir
                .join(".ls-s")
                .join("0-0-0-00000000")
                .join(layout.sidecar_file()),
        );
        // InterpreterPath stays below MAX_PATH so this test isolates source-argument handling from
        // CPython's independent `sys.executable` descendant-launch behavior.
        // InterpreterPath 保持低于 MAX_PATH，使本测试只验证源码参数处理，而不混入 CPython
        // 通过 `sys.executable` 启动后代进程的独立行为。
        let interpreter_path = render_host_visible_path(
            &layout
                .env_dir
                .join(".venv")
                .join("Scripts")
                .join("python.exe"),
        );
        // MinimumSourceLength and InterpreterLength are measured in Windows UTF-16 code units.
        // MinimumSourceLength 与 InterpreterLength 均以 Windows UTF-16 代码单元计量。
        let minimum_source_length = Path::new(&minimum_source).as_os_str().encode_wide().count();
        let interpreter_length = Path::new(&interpreter_path)
            .as_os_str()
            .encode_wide()
            .count();
        assert!(
            minimum_source_length >= 260,
            "expected persistent-session snapshot source past MAX_PATH, length={minimum_source_length}, path={minimum_source}"
        );
        assert!(
            interpreter_length < 260,
            "expected persistent-session interpreter below MAX_PATH, length={interpreter_length}, path={interpreter_path}"
        );
    }
    #[cfg(not(windows))]
    assert!(
        !expect_long_source,
        "long persistent-session source assertion is Windows-only"
    );
    // Engine is the production JSON surface under test.
    // Engine 是被测的生产 JSON 接口。
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    // EventCenter receives notifications from the real pipe readers and exit watcher.
    // EventCenter 接收真实管道读取器与退出监视器的通知。
    let event_center = engine.managed_runtime_services.event_center();
    // WakeCount proves actual background output wakes the host edge callback.
    // WakeCount 证明真实后台输出会唤醒宿主边沿回调。
    let wake_count = Arc::new(AtomicUsize::new(0));
    let wake_count_for_callback = Arc::clone(&wake_count);
    event_center
        .set_wake_callback(Some(Arc::new(move || {
            wake_count_for_callback.fetch_add(1, AtomicOrdering::AcqRel);
        })))
        .expect("install managed session wake callback");
    // Lease uses the required infinite System profile lifetime.
    // Lease 使用所要求的无限 System profile 生命周期。
    let lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-persistence-{}", runtime.label()),
        false,
    );

    // FirstEval creates and stores the session in a persistent Lua global.
    // FirstEval 创建会话并把它保存到持久 Lua 全局变量。
    let first_eval = eval_managed_session_test_lease(
        &engine,
        &lease,
        &managed_session_open_lua(&layout, "persistent_session", 1024, &[]),
    );
    assert_eq!(first_eval["ok"], true, "first eval failed: {first_eval}");
    assert_eq!(
        first_eval["result"]["module_marker"],
        "system-package-module-ok"
    );
    assert_eq!(first_eval["result"]["status"]["running"], true);
    // Lua-visible id correlates this userdata with the engine-level event stream.
    // Lua 可见标识会把当前 userdata 与引擎级事件流关联起来。
    let lua_managed_session_id = first_eval["result"]["status"]["managed_session_id"]
        .as_u64()
        .expect("Lua-visible managed session id");

    // SecondEval writes one stateful command through the same saved userdata.
    // SecondEval 通过同一个已保存 userdata 写入一条有状态命令。
    let second_eval = eval_managed_session_test_lease(
        &engine,
        &lease,
        r#"persistent_session:write('{"action":"echo","value":"alpha"}\n'); return true"#,
    );
    assert_eq!(second_eval["ok"], true, "second eval failed: {second_eval}");

    // ThirdEval reads startup, stderr, and echo output from that exact process.
    // ThirdEval 从该精确进程读取启动、stderr 与 echo 输出。
    let third_eval = eval_managed_session_test_lease(
        &engine,
        &lease,
        "return persistent_session:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'alpha' })",
    );
    assert_eq!(third_eval["ok"], true, "third eval failed: {third_eval}");
    // ReadResult is the shared process-session contract returned through JSON.
    // ReadResult 是通过 JSON 返回的共享进程会话契约。
    let read_result = &third_eval["result"];
    // ProcessTree identifies the real root and descendant used for later cleanup assertions.
    // ProcessTree 标识用于后续清理断言的真实根进程与后代进程。
    let (process_tree, started) = parse_managed_session_started(read_result);
    assert_eq!(started["marker"], layout.import_marker());
    if runtime == ManagedSessionTestRuntime::Node {
        assert_eq!(started["dependency_marker"], layout.dependency_marker);
    }
    assert_eq!(started["host_secret_visible"], false);
    assert_eq!(started["managed_context_present"], true);
    assert!(
        !started["path"]
            .as_str()
            .expect("managed session controlled PATH")
            .is_empty()
    );
    assert_eq!(
        started["cwd"],
        render_host_visible_path(
            &fs::canonicalize(layout.runtime_source_dir())
                .expect("canonicalize managed session runtime cwd")
        )
    );
    assert!(
        read_result["stdout"]
            .as_str()
            .expect("persistent session stdout")
            .contains(r#""value":"alpha","counter":1"#)
    );
    // StderrText includes any diagnostic bytes already drained with the alpha response.
    // StderrText 包含随 alpha 响应一同取出的全部已就绪诊断字节。
    let mut stderr_text = read_result["stderr"]
        .as_str()
        .expect("persistent session stderr")
        .to_string();
    if !stderr_text.contains("stderr-started") {
        // StderrRead handles the valid scheduling race where the stderr reader appends slightly later.
        // StderrRead 处理 stderr 读取器稍晚追加这一合法调度竞态。
        let stderr_read = eval_managed_session_test_lease(
            &engine,
            &lease,
            "return persistent_session:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'stderr-started' })",
        );
        assert_eq!(stderr_read["ok"], true, "stderr read failed: {stderr_read}");
        stderr_text.push_str(
            stderr_read["result"]["stderr"]
                .as_str()
                .expect("delayed persistent session stderr"),
        );
    }
    assert!(stderr_text.contains("stderr-started"));
    assert!(
        managed_session_process_exists(process_tree.root_pid)
            .expect("probe persistent sidecar root")
    );
    assert!(
        managed_session_process_exists(process_tree.child_pid)
            .expect("probe persistent sidecar descendant")
    );

    // InitialEvents must include both readable streams from real background readers.
    // InitialEvents 必须包含真实后台读取器产生的两个可读流事件。
    let initial_events = wait_for_managed_session_event_kinds(
        &event_center,
        &[
            RuntimeManagedSessionEventKind::StdoutReadable,
            RuntimeManagedSessionEventKind::StderrReadable,
        ],
        Duration::from_secs(5),
    );
    // ManagedSessionId is the engine-local identity shared by all events for this sidecar.
    // ManagedSessionId 是当前 sidecar 全部事件共享的引擎本地身份。
    let managed_session_id = initial_events[0].managed_session_id;
    assert_eq!(managed_session_id, lua_managed_session_id);
    assert!(initial_events.iter().all(|event| {
        event.system_lease_id == lease.lease_id
            && event.generation == lease.generation
            && event.managed_session_id == managed_session_id
    }));
    assert!(
        initial_events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(wake_count.load(AtomicOrdering::Acquire) >= 1);
    drain_managed_session_events(&event_center);

    // ZeroRead proves the integrated Lua surface preserves true nonblocking timeout semantics.
    // ZeroRead 证明集成 Lua 接口保留真正非阻塞超时语义。
    let zero_read = eval_managed_session_test_lease(
        &engine,
        &lease,
        "return persistent_session:read({ timeout_ms = 0, max_bytes = 4096 })",
    );
    assert_eq!(zero_read["ok"], true);
    assert_eq!(zero_read["result"]["timed_out"], true);
    assert_eq!(zero_read["result"]["stdout"], "");
    // ZeroEventWait proves host polling is also truly nonblocking when the queue is empty.
    // ZeroEventWait 证明事件队列为空时宿主轮询也是真正非阻塞。
    let zero_event_wait = event_center
        .wait(256, 0)
        .expect("nonblocking managed event wait");
    assert!(zero_event_wait.events.is_empty());
    assert!(zero_event_wait.timed_out);

    // SpamWrite requests enough flushed output to overflow the configured 1024-byte ring.
    // SpamWrite 请求足够多的刷新输出，以溢出配置的 1024 字节环形缓冲。
    let spam_write = eval_managed_session_test_lease(
        &engine,
        &lease,
        r#"persistent_session:write('{"action":"spam"}\n'); return true"#,
    );
    assert_eq!(spam_write["ok"], true);
    // SpamRead waits for the final marker while retaining only the newest bounded window.
    // SpamRead 等待最终标记，同时只保留最新的有界窗口。
    let spam_read = eval_managed_session_test_lease(
        &engine,
        &lease,
        "return persistent_session:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'spam_end' })",
    );
    assert_eq!(spam_read["ok"], true, "spam read failed: {spam_read}");
    assert!(
        spam_read["result"]["stdout"]
            .as_str()
            .expect("spam stdout")
            .contains("spam_end")
    );
    assert!(
        spam_read["result"]["stdout_dropped_bytes"]
            .as_u64()
            .expect("stdout dropped byte count")
            > 0
    );
    assert!(
        spam_read["result"]["stdout_total_bytes"]
            .as_u64()
            .expect("stdout total byte count")
            > 1024
    );
    // SpamEvents contains one coalesced stdout slot despite many flushed sidecar writes.
    // SpamEvents 尽管 sidecar 多次刷新写入，仍只包含一个合并后的 stdout 槽。
    let spam_events = event_center.poll(256).expect("poll coalesced spam events");
    assert_eq!(
        spam_events
            .events
            .iter()
            .filter(|event| {
                event.managed_session_id == managed_session_id
                    && event.kind == RuntimeManagedSessionEventKind::StdoutReadable
            })
            .count(),
        1
    );
    drain_managed_session_events(&event_center);

    // ExitWrite asks only the root sidecar to exit, leaving its descendant for tree cleanup.
    // ExitWrite 只要求根 sidecar 退出，并把后代留给进程树清理。
    let exit_write = eval_managed_session_test_lease(
        &engine,
        &lease,
        r#"persistent_session:write('{"action":"exit"}\n'); return true"#,
    );
    assert_eq!(exit_write["ok"], true);
    // ExitEvents proves the independent direct-child watcher reliably publishes terminal state.
    // ExitEvents 证明独立直接子进程监视器会可靠发布终态。
    let exit_events = wait_for_managed_session_event_kinds(
        &event_center,
        &[RuntimeManagedSessionEventKind::Exited],
        Duration::from_secs(5),
    );
    assert!(exit_events.iter().any(|event| {
        event.managed_session_id == managed_session_id
            && event.kind == RuntimeManagedSessionEventKind::Exited
    }));
    assert!(
        managed_session_process_exists(process_tree.child_pid)
            .expect("probe descendant after root exit"),
        "descendant must remain alive until explicit tree cleanup"
    );
    // CloseResult terminates and waits for the entire tree after the root already exited.
    // CloseResult 在根进程已经退出后终止并等待完整进程树。
    let close_result = eval_managed_session_test_lease(
        &engine,
        &lease,
        "return persistent_session:close({ timeout_ms = 0 })",
    );
    assert_eq!(
        close_result["ok"], true,
        "session close failed: {close_result}"
    );
    assert_managed_session_process_tree_exits(process_tree, Duration::from_secs(5));
    drain_managed_session_events(&event_center);
    thread::sleep(Duration::from_millis(50));
    // StaleEvents remains empty because close permanently shuts the observer gate.
    // StaleEvents 保持为空，因为 close 会永久关闭观察器通知门。
    let stale_events = event_center
        .wait(256, 0)
        .expect("poll events after managed session close");
    assert!(stale_events.events.is_empty());
    assert!(stale_events.timed_out);

    // ParentTraversal attempts to leave the package root lexically and must fail before launch.
    // ParentTraversal 尝试以词法方式离开包根，且必须在启动前失败。
    let parent_traversal = eval_managed_session_test_lease(
        &engine,
        &lease,
        &format!(
            "return {}({{ file = '../outside-sidecar', buffer_limit_bytes = 1024 }})",
            layout.lua_open_api()
        ),
    );
    assert_eq!(parent_traversal["ok"], false);
    assert!(
        parent_traversal["message"]
            .as_str()
            .expect("parent traversal error")
            .contains("must be a non-empty safe path")
    );
    // OutsideSource is the real target used to create one escaping symbolic link.
    // OutsideSource 是用于创建逃逸符号链接的真实目标。
    let outside_source = layout
        .base
        .runtime_root
        .join(format!("outside-session-{}", runtime.label()));
    fs::write(&outside_source, "outside managed session fixture")
        .expect("write outside managed session target");
    // EscapeLink lives inside the package but canonicalizes outside its trust boundary.
    // EscapeLink 位于包内，但规范化后会越出其信任边界。
    let escape_link = layout.runtime_source_dir().join(match runtime {
        ManagedSessionTestRuntime::Python => "escape.py",
        ManagedSessionTestRuntime::Node => "escape.mjs",
    });
    if create_test_file_symlink(&escape_link, &outside_source) {
        // SymlinkEscape must fail without starting another managed process.
        // SymlinkEscape 必须失败，且不得启动另一个受管进程。
        let symlink_escape = eval_managed_session_test_lease(
            &engine,
            &lease,
            &format!(
                "return {}({{ file = 'runtime/{}', buffer_limit_bytes = 1024 }})",
                layout.lua_open_api(),
                escape_link
                    .file_name()
                    .expect("escape link file name")
                    .to_string_lossy()
            ),
        );
        assert_eq!(symlink_escape["ok"], false);
        assert!(
            symlink_escape["message"]
                .as_str()
                .expect("symlink escape error")
                .contains("escapes package root")
                || symlink_escape["message"]
                    .as_str()
                    .expect("Node symlink copy error")
                    .contains("unsupported package entry type")
        );
    }

    event_center
        .set_wake_callback(None)
        .expect("clear managed session wake callback");
    close_managed_session_test_lease(&engine, &lease);
    assert_no_active_node_session_snapshots(&layout);
}

/// Exercise two simultaneous process identities and independent state for one runtime.
/// 针对一个运行时验证两个并行进程身份与独立状态。
fn run_managed_session_isolation_integration(runtime: ManagedSessionTestRuntime) {
    // Host is absent only when the real requested interpreter is unavailable.
    // Host 仅在请求的真实解释器不可用时为空。
    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    let _env_guard = process_env_test_guard();
    // Layout and engine isolate both sessions from every other test.
    // Layout 与 Engine 把两个会话同其他测试完全隔离。
    let layout = ManagedSessionSystemLayout::new(
        &format!("managed-session-isolation-{}", runtime.label()),
        runtime,
        &host,
    );
    let engine = make_runtime_test_engine_with_host_options(layout.host_options());
    let lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-isolation-{}", runtime.label()),
        false,
    );
    // OpenBoth creates two userdata instances and waits for both sidecars before the first write.
    // OpenBoth 创建两个 userdata 实例，并在首次写入前等待两个 sidecar 就绪。
    let open_both = eval_managed_session_test_lease(
        &engine,
        &lease,
        &format!(
            "session_a = {}({{ file = '{}', cwd = 'runtime', buffer_limit_bytes = 4096 }})\nsession_b = {}({{ file = '{}', cwd = 'runtime', buffer_limit_bytes = 4096 }})\nlocal a = session_a:read({{ timeout_ms = 5000, max_bytes = 4096, until_text = 'started' }})\nif not string.find(a.stderr, 'stderr-started', 1, true) then local extra_a = session_a:read({{ timeout_ms = 5000, max_bytes = 4096, until_text = 'stderr-started' }}); a.stderr = a.stderr .. extra_a.stderr end\nlocal b = session_b:read({{ timeout_ms = 5000, max_bytes = 4096, until_text = 'started' }})\nif not string.find(b.stderr, 'stderr-started', 1, true) then local extra_b = session_b:read({{ timeout_ms = 5000, max_bytes = 4096, until_text = 'stderr-started' }}); b.stderr = b.stderr .. extra_b.stderr end\nreturn {{ a = a, b = b }}",
            layout.lua_open_api(),
            layout.sidecar_file(),
            layout.lua_open_api(),
            layout.sidecar_file(),
        ),
    );
    assert_eq!(
        open_both["ok"], true,
        "open two sessions failed: {open_both}"
    );
    // StartupTrees prove both processes are distinct before any request data is sent.
    // StartupTrees 在发送任何请求数据前证明两个进程彼此独立。
    let (tree_a, _) = parse_managed_session_started(&open_both["result"]["a"]);
    let (tree_b, _) = parse_managed_session_started(&open_both["result"]["b"]);
    assert_ne!(tree_a.root_pid, tree_b.root_pid);
    assert_ne!(tree_a.child_pid, tree_b.child_pid);
    // WriteBoth sends different values without relying on worker-pool affinity.
    // WriteBoth 在不依赖 Worker Pool 亲和性的情况下发送不同值。
    let write_both = eval_managed_session_test_lease(
        &engine,
        &lease,
        r#"session_a:write('{"action":"echo","value":"isolation-a"}\n'); session_b:write('{"action":"echo","value":"isolation-b"}\n'); return true"#,
    );
    assert_eq!(write_both["ok"], true);
    // ReadBoth returns each stream separately after its own echo marker appears.
    // ReadBoth 在各自 echo 标记出现后分别返回两个流。
    let read_both = eval_managed_session_test_lease(
        &engine,
        &lease,
        "local a = session_a:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'isolation-a' }); local b = session_b:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'isolation-b' }); return { a = a, b = b }",
    );
    assert_eq!(
        read_both["ok"], true,
        "read two sessions failed: {read_both}"
    );
    // StdoutA and StdoutB prove data never crosses session buffers.
    // StdoutA 与 StdoutB 证明数据不会跨越会话缓冲区。
    let stdout_a = read_both["result"]["a"]["stdout"]
        .as_str()
        .expect("session A stdout");
    let stdout_b = read_both["result"]["b"]["stdout"]
        .as_str()
        .expect("session B stdout");
    assert!(stdout_a.contains("isolation-a"));
    assert!(!stdout_a.contains("isolation-b"));
    assert!(stdout_b.contains("isolation-b"));
    assert!(!stdout_b.contains("isolation-a"));
    assert!(stdout_a.contains(r#""counter":1"#));
    assert!(stdout_b.contains(r#""counter":1"#));

    // SecondA increments only session A's local counter.
    // SecondA 只递增 session A 的本地计数器。
    let second_a = eval_managed_session_test_lease(
        &engine,
        &lease,
        r#"session_a:write('{"action":"echo","value":"isolation-a-2"}\n'); return session_a:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'isolation-a-2' })"#,
    );
    assert_eq!(second_a["ok"], true);
    assert!(
        second_a["result"]["stdout"]
            .as_str()
            .expect("second session A stdout")
            .contains(r#""counter":2"#)
    );
    // IdleB proves session B remains live and has no unread output from session A.
    // IdleB 证明 session B 仍存活，且没有来自 session A 的未读输出。
    let idle_b = eval_managed_session_test_lease(
        &engine,
        &lease,
        "local read = session_b:read({ timeout_ms = 0, max_bytes = 4096 }); local status = session_b:status(); return { read = read, status = status }",
    );
    assert_eq!(idle_b["ok"], true);
    assert_eq!(idle_b["result"]["read"]["timed_out"], true);
    assert_eq!(idle_b["result"]["read"]["stdout"], "");
    assert_eq!(idle_b["result"]["status"]["running"], true);

    close_managed_session_test_lease(&engine, &lease);
    assert_managed_session_process_tree_exits(tree_a, Duration::from_secs(5));
    assert_managed_session_process_tree_exits(tree_b, Duration::from_secs(5));
    assert_no_active_node_session_snapshots(&layout);
}

/// Exercise rollback, replace, reload survival, and engine-drop cleanup for one runtime.
/// 针对一个运行时验证回滚、替换、重载存活与 Engine Drop 清理。
fn run_managed_session_lifecycle_integration(runtime: ManagedSessionTestRuntime) {
    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    let _env_guard = process_env_test_guard();
    // Layout is reused because its ready environment is immutable across lifecycle cases.
    // Layout 会被复用，因为其就绪环境在各生命周期场景间保持不可变。
    let layout = ManagedSessionSystemLayout::new(
        &format!("managed-session-lifecycle-{}", runtime.label()),
        runtime,
        &host,
    );
    // Engine remains mutable so ordinary Skill reload can be exercised in place.
    // Engine 保持可变，以便原地验证普通 Skill 重载。
    let mut engine = make_runtime_test_engine_with_host_options(layout.host_options());

    // RollbackLease verifies an eval error kills only the session created by that transaction.
    // RollbackLease 验证 eval 错误只会杀死该事务创建的会话。
    let rollback_lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-rollback-{}", runtime.label()),
        false,
    );
    // PidFile receives startup identity before the forced Lua error triggers rollback.
    // PidFile 会在强制 Lua 错误触发回滚前接收启动身份。
    let pid_file = layout
        .runtime_source_dir()
        .join(format!("rollback-{}.json", runtime.label()));
    let pid_file_lua = serde_json::to_string(&render_host_visible_path(&pid_file))
        .expect("quote rollback pid file for Lua");
    // RollbackEval waits for startup, then fails the transaction deliberately.
    // RollbackEval 等待启动完成，随后有意让事务失败。
    let rollback_eval = eval_managed_session_test_lease(
        &engine,
        &rollback_lease,
        &format!(
            "rollback_session = {}({{ file = '{}', args = {{{pid_file_lua}}}, cwd = 'runtime', buffer_limit_bytes = 4096 }})\nlocal ready = rollback_session:read({{ timeout_ms = 5000, max_bytes = 4096, until_text = 'started' }})\nerror('forced managed session rollback after ' .. ready.stdout)",
            layout.lua_open_api(),
            layout.sidecar_file(),
        ),
    );
    assert_eq!(rollback_eval["ok"], false);
    assert_eq!(rollback_eval["error_code"], "eval_failed");
    // RollbackRecord is written by the real sidecar before rollback termination begins.
    // RollbackRecord 由真实 sidecar 在回滚终止开始前写入。
    let rollback_record: Value = serde_json::from_slice(
        &fs::read(&pid_file).expect("read rollback managed session pid file"),
    )
    .expect("decode rollback managed session pid file");
    let rollback_tree = ManagedSessionProcessTree {
        root_pid: u32::try_from(
            rollback_record["root_pid"]
                .as_u64()
                .expect("rollback root_pid"),
        )
        .expect("rollback root_pid fits u32"),
        child_pid: u32::try_from(
            rollback_record["child_pid"]
                .as_u64()
                .expect("rollback child_pid"),
        )
        .expect("rollback child_pid fits u32"),
    };
    assert_managed_session_process_tree_exits(rollback_tree, Duration::from_secs(5));
    // RollbackCleanup proves package snapshots disappear immediately while the failed lease VM remains alive.
    // RollbackCleanup 证明失败租约 VM 仍存活时，包快照已经由回滚立即删除。
    assert_no_active_node_session_snapshots(&layout);
    // RolledBackStatus remains safely callable through the Lua global and reports closed state.
    // RolledBackStatus 仍可通过 Lua 全局安全调用，并报告已关闭状态。
    let rolled_back_status = eval_managed_session_test_lease(
        &engine,
        &rollback_lease,
        "return rollback_session:status()",
    );
    assert_eq!(rolled_back_status["ok"], true);
    assert_eq!(rolled_back_status["result"]["closed"], true);
    close_managed_session_test_lease(&engine, &rollback_lease);
    assert_no_active_node_session_snapshots(&layout);

    // ExpiringLease proves manager pruning destroys both the persistent VM and full process tree.
    // ExpiringLease 证明管理器过期清理会销毁持久 VM 与完整进程树。
    let expiring_sid = format!("system-managed-expiry-{}", runtime.label());
    let mut expiring_request = layout.create_request(&expiring_sid, false);
    expiring_request["ttl_sec"] = json!(1);
    let expiring_lease = create_managed_session_test_lease_from_request(&engine, &expiring_request);
    let expiring_tree = open_and_capture_managed_session_tree(
        &engine,
        &layout,
        &expiring_lease,
        "expiring_session",
        &[],
    );
    thread::sleep(Duration::from_millis(1_200));
    // Listing triggers the same complete prune pass used by production lease maintenance.
    // 列表操作触发生产租约维护使用的同一完整过期清理流程。
    let listed_after_expiry: Value = serde_json::from_str(
        &engine
            .list_system_runtime_leases_json("{}")
            .expect("list System leases after managed session expiry"),
    )
    .expect("decode System lease list after managed session expiry");
    assert!(
        listed_after_expiry["leases"]
            .as_array()
            .expect("System lease list array")
            .iter()
            .all(|lease| lease["lease_id"] != expiring_lease.lease_id)
    );
    assert_managed_session_process_tree_exits(expiring_tree, Duration::from_secs(5));
    // ExpiredEval proves the stale handle returns its stable terminal error without panic.
    // ExpiredEval 证明陈旧句柄会返回稳定过期错误且不会 panic。
    let expired_eval = eval_managed_session_test_lease(&engine, &expiring_lease, "return 1");
    assert_eq!(expired_eval["ok"], false);
    assert_eq!(expired_eval["error_code"], "lease_expired");
    assert_no_active_node_session_snapshots(&layout);

    // ReplacedLease owns a live process tree that replacement must synchronously retire.
    // ReplacedLease 拥有一棵活动进程树，替换操作必须同步将其退役。
    let replaced_lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-replace-{}", runtime.label()),
        false,
    );
    let replaced_tree = open_and_capture_managed_session_tree(
        &engine,
        &layout,
        &replaced_lease,
        "replace_session",
        &[],
    );
    let replacement = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-replace-{}", runtime.label()),
        true,
    );
    assert!(replacement.generation > replaced_lease.generation);
    assert_managed_session_process_tree_exits(replaced_tree, Duration::from_secs(5));
    // StaleEval proves the old generation returns its stable terminal error instead of panicking.
    // StaleEval 证明旧代际返回稳定终态错误，而不会 panic。
    let stale_eval = eval_managed_session_test_lease(&engine, &replaced_lease, "return 1");
    assert_eq!(stale_eval["ok"], false);
    assert_eq!(stale_eval["error_code"], "lease_replaced");
    close_managed_session_test_lease(&engine, &replacement);
    assert_no_active_node_session_snapshots(&layout);

    // ReloadLease proves ordinary Skill reload preserves the dedicated System VM and session.
    // ReloadLease 证明普通 Skill 重载会保留专用 System VM 与会话。
    let reload_lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-reload-{}", runtime.label()),
        false,
    );
    let reload_tree = open_and_capture_managed_session_tree(
        &engine,
        &layout,
        &reload_lease,
        "reload_session",
        &[],
    );
    // EmptySkillRoot is a valid formal root used only to rebuild ordinary runtime state.
    // EmptySkillRoot 是仅用于重建普通运行时状态的合法正式根。
    let empty_skill_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: layout.base.runtime_root.join("reload-skills"),
    };
    fs::create_dir_all(&empty_skill_root.skills_dir).expect("create empty reload Skill root");
    engine
        .reload_from_roots(std::slice::from_ref(&empty_skill_root))
        .expect("reload ordinary Skills while managed System session is active");
    // ReloadEcho proves the exact sidecar state remains reachable after reload.
    // ReloadEcho 证明重载后仍可访问精确 sidecar 状态。
    let reload_echo = eval_managed_session_test_lease(
        &engine,
        &reload_lease,
        r#"reload_session:write('{"action":"echo","value":"after-reload"}\n'); return reload_session:read({ timeout_ms = 5000, max_bytes = 4096, until_text = 'after-reload' })"#,
    );
    assert_eq!(reload_echo["ok"], true, "reload echo failed: {reload_echo}");
    assert!(
        reload_echo["result"]["stdout"]
            .as_str()
            .expect("reload session stdout")
            .contains("after-reload")
    );
    close_managed_session_test_lease(&engine, &reload_lease);
    assert_managed_session_process_tree_exits(reload_tree, Duration::from_secs(5));
    assert_no_active_node_session_snapshots(&layout);

    // EngineDropLease is intentionally left active until the owning engine is dropped.
    // EngineDropLease 会被有意保持活动，直到所属 Engine 被释放。
    let engine_drop_lease = create_managed_session_test_lease(
        &engine,
        &layout,
        &format!("system-managed-engine-drop-{}", runtime.label()),
        false,
    );
    let engine_drop_tree = open_and_capture_managed_session_tree(
        &engine,
        &layout,
        &engine_drop_lease,
        "engine_drop_session",
        &[],
    );
    drop(engine);
    assert_managed_session_process_tree_exits(engine_drop_tree, Duration::from_secs(5));
    assert_no_active_node_session_snapshots(&layout);
}

/// Verify a strict System lease executes real managed Python status and invoke APIs.
/// 验证严格 System 租约会执行真实受管 Python status 与 invoke API。
#[test]
fn system_runtime_python_status_and_invoke_execute_real_runtime() {
    run_managed_runtime_status_invoke_integration(
        ManagedSessionTestRuntime::Python,
        "managed-status-invoke-python",
        false,
        false,
    );
}

/// Verify a strict System lease executes real managed Node status and invoke APIs.
/// 验证严格 System 租约会执行真实受管 Node status 与 invoke API。
#[test]
fn system_runtime_node_status_and_invoke_execute_real_runtime() {
    run_managed_runtime_status_invoke_integration(
        ManagedSessionTestRuntime::Node,
        "managed-status-invoke-node",
        false,
        false,
    );
}

/// Verify explicit external host roots drive real Python status, invoke, and persistent sessions.
/// 验证显式外部宿主根会驱动真实 Python status、invoke 与持久会话。
#[test]
fn system_runtime_python_host_configured_roots_execute_all_runtime_paths() {
    run_managed_runtime_status_invoke_integration(
        ManagedSessionTestRuntime::Python,
        "managed-host-roots-python",
        false,
        true,
    );
}

/// Verify explicit external host roots drive real Node status, invoke, and persistent sessions.
/// 验证显式外部宿主根会驱动真实 Node status、invoke 与持久会话。
#[test]
fn system_runtime_node_host_configured_roots_execute_all_runtime_paths() {
    run_managed_runtime_status_invoke_integration(
        ManagedSessionTestRuntime::Node,
        "managed-host-roots-node",
        false,
        true,
    );
}

/// Verify pooled Python invocation remains reusable when its snapshot source exceeds MAX_PATH.
/// 验证池化 Python 调用在快照源码超过 MAX_PATH 时仍可复用。
#[cfg(windows)]
#[test]
fn system_runtime_python_worker_supports_long_snapshot_source_path() {
    run_managed_runtime_status_invoke_integration(
        ManagedSessionTestRuntime::Python,
        "managed-python-long-worker",
        true,
        false,
    );
}

/// Verify pooled Node invocation remains reusable when its snapshot source exceeds MAX_PATH.
/// 验证池化 Node 调用在快照源码超过 MAX_PATH 时仍可复用。
#[cfg(windows)]
#[test]
fn system_runtime_node_worker_supports_long_snapshot_source_path() {
    run_managed_runtime_status_invoke_integration(
        ManagedSessionTestRuntime::Node,
        "managed-node-long-worker",
        true,
        false,
    );
}

/// Verify two Python System Plugins isolate same-named dependencies and modules.
/// 验证两个 Python System Plugin 会隔离同名依赖与模块。
#[test]
fn system_runtime_python_plugins_isolate_dependencies_and_modules() {
    run_managed_system_plugin_dependency_module_isolation_integration(
        ManagedSessionTestRuntime::Python,
    );
}

/// Verify two Node System Plugins isolate same-named dependencies and modules.
/// 验证两个 Node System Plugin 会隔离同名依赖与模块。
#[test]
fn system_runtime_node_plugins_isolate_dependencies_and_modules() {
    run_managed_system_plugin_dependency_module_isolation_integration(
        ManagedSessionTestRuntime::Node,
    );
}

/// Verify a real managed Python session persists across evals and emits bounded events.
/// 验证真实受管 Python 会话会跨 eval 持续存在并发出有界事件。
#[test]
fn system_runtime_python_session_persists_across_evals_and_emits_events() {
    run_managed_session_persistence_integration(ManagedSessionTestRuntime::Python, false);
}

/// Verify a real managed Node session persists across evals and emits bounded events.
/// 验证真实受管 Node 会话会跨 eval 持续存在并发出有界事件。
#[test]
fn system_runtime_node_session_persists_across_evals_and_emits_events() {
    run_managed_session_persistence_integration(ManagedSessionTestRuntime::Node, false);
}

/// Verify a real Python persistent session launches from a snapshot path beyond Windows MAX_PATH.
/// 验证真实 Python 持久会话可从超过 Windows MAX_PATH 的快照路径启动。
#[cfg(windows)]
#[test]
fn system_runtime_python_session_supports_long_snapshot_source_path() {
    run_managed_session_persistence_integration(ManagedSessionTestRuntime::Python, true);
}

/// Verify two real managed Python sessions remain process- and state-isolated.
/// 验证两个真实受管 Python 会话保持进程与状态隔离。
#[test]
fn system_runtime_python_sessions_are_isolated() {
    run_managed_session_isolation_integration(ManagedSessionTestRuntime::Python);
}

/// Verify two real managed Node sessions remain process- and state-isolated.
/// 验证两个真实受管 Node 会话保持进程与状态隔离。
#[test]
fn system_runtime_node_sessions_are_isolated() {
    run_managed_session_isolation_integration(ManagedSessionTestRuntime::Node);
}

/// Verify Python session resources follow rollback, replace, reload, and engine lifecycles.
/// 验证 Python 会话资源遵循回滚、替换、重载与 Engine 生命周期。
#[test]
fn system_runtime_python_session_lifecycle_cleanup_is_complete() {
    run_managed_session_lifecycle_integration(ManagedSessionTestRuntime::Python);
}

/// Verify Node session resources follow rollback, replace, reload, and engine lifecycles.
/// 验证 Node 会话资源遵循回滚、替换、重载与 Engine 生命周期。
#[test]
fn system_runtime_node_session_lifecycle_cleanup_is_complete() {
    run_managed_session_lifecycle_integration(ManagedSessionTestRuntime::Node);
}

/// Verify closed runtime sessions return a stable lease_closed error.
/// 验证已关闭的运行时会话会返回稳定的 lease_closed 错误。
#[test]
fn runtime_session_eval_reports_closed_lease() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"closed-test","ttl_sec":60}"#)
            .expect("create runtime session"),
    )
    .expect("create response json");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("lease id should be present")
        .to_string();
    let close_request = json!({ "lease_id": lease_id });
    let closed: Value = serde_json::from_str(
        &engine
            .close_runtime_lease_json(&close_request.to_string())
            .expect("close runtime session"),
    )
    .expect("close response json");
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["closed"], true);

    let eval_request = json!({
        "lease_id": lease_id,
        "code": "return 1"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&eval_request.to_string())
            .expect("eval closed runtime session"),
    )
    .expect("eval response json");
    assert_eq!(eval["ok"], false);
    assert_eq!(eval["error_code"], "lease_closed");
}

/// Verify closed runtime sessions return a stable lease_closed error from status.
/// 验证已关闭的运行时会话在 status 中会返回稳定的 lease_closed 错误。
#[test]
fn runtime_session_status_reports_closed_lease() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"closed-status-test","ttl_sec":60}"#)
            .expect("create runtime session"),
    )
    .expect("create response json");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("lease id should be present")
        .to_string();
    let close_request = json!({ "lease_id": lease_id.clone() });
    let closed: Value = serde_json::from_str(
        &engine
            .close_runtime_lease_json(&close_request.to_string())
            .expect("close runtime session"),
    )
    .expect("close response json");
    assert_eq!(closed["ok"], true);

    let status_request = json!({ "lease_id": lease_id });
    let status: Value = serde_json::from_str(
        &engine
            .runtime_lease_status_json(&status_request.to_string())
            .expect("status closed runtime session"),
    )
    .expect("status response json");
    assert_eq!(status["ok"], false);
    assert_eq!(status["error_code"], "lease_closed");
}

/// Verify replaced runtime sessions keep a stable lease_replaced terminal error.
/// 验证被替换的运行时会话会保留稳定的 lease_replaced 终态错误。
#[test]
fn runtime_session_eval_reports_replaced_lease() {
    let engine = make_runtime_test_engine();
    let first_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"replace-test","ttl_sec":60}"#)
            .expect("create first runtime session"),
    )
    .expect("first create response json");
    let first_lease_id = first_created["lease_id"]
        .as_str()
        .expect("first lease id should be present")
        .to_string();

    let second_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"replace-test","ttl_sec":60,"replace":true}"#)
            .expect("create second runtime session"),
    )
    .expect("second create response json");
    assert_eq!(second_created["ok"], true);
    assert_ne!(second_created["lease_id"], first_created["lease_id"]);

    let eval_request = json!({
        "lease_id": first_lease_id,
        "code": "return 1"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&eval_request.to_string())
            .expect("eval replaced runtime session"),
    )
    .expect("replaced eval response json");
    assert_eq!(eval["ok"], false);
    assert_eq!(eval["error_code"], "lease_replaced");
}

/// Verify replaced lease tombstones use typed identity instead of cached display snapshots.
/// 验证被替换租约的墓碑使用 typed 身份，而不是缓存展示快照。
#[test]
fn runtime_session_replaced_tombstone_ignores_corrupted_snapshot_identity() {
    let engine = make_runtime_test_engine();
    let first_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"replace-snapshot-test","ttl_sec":60}"#)
            .expect("create first runtime session"),
    )
    .expect("first create response json");
    // Lease id returned by the real runtime-session create path.
    // 真实运行时会话创建路径返回的租约 id。
    let first_lease_id = first_created["lease_id"]
        .as_str()
        .expect("first lease id should be present")
        .to_string();
    // Generation returned by the real runtime-session create path.
    // 真实运行时会话创建路径返回的 generation。
    let first_generation = first_created["generation"]
        .as_u64()
        .expect("first generation should be present");

    engine
        .public_runtime_sessions
        .replace_active_snapshot_for_test(
            &first_lease_id,
            json!({
                "ok": true,
                "sid": "corrupted-sid",
                "lease_id": "corrupted-lease",
                "generation": 999_u64,
                "profile": "system_lua_lib"
            }),
        );

    let second_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(
                r#"{"sid":"replace-snapshot-test","ttl_sec":60,"replace":true}"#,
            )
            .expect("create replacement runtime session"),
    )
    .expect("replacement create response json");
    assert_eq!(second_created["ok"], true);

    let eval_request = json!({
        "lease_id": first_lease_id,
        "sid": "replace-snapshot-test",
        "generation": first_generation,
        "code": "return 1"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&eval_request.to_string())
            .expect("eval replaced runtime session with echoed identity"),
    )
    .expect("replaced eval response json");

    assert_eq!(eval["ok"], false);
    assert_eq!(eval["error_code"], "lease_replaced");
    assert!(
        eval["message"]
            .as_str()
            .expect("lease error should be text")
            .contains("sid `replace-snapshot-test`, generation 1")
    );
}

/// Verify active lease listing uses typed identity instead of cached display snapshots.
/// 验证活跃租约列表使用 typed 身份，而不是缓存展示快照。
#[test]
fn runtime_session_list_uses_typed_identity_when_snapshot_is_corrupted() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"list-snapshot-test","ttl_sec":60}"#)
            .expect("create runtime session for list snapshot test"),
    )
    .expect("list snapshot create response json");
    // Lease id returned by the real runtime-session create path.
    // 真实运行时会话创建路径返回的租约 id。
    let lease_id = created["lease_id"]
        .as_str()
        .expect("lease id should be present")
        .to_string();
    // Generation returned by the real runtime-session create path.
    // 真实运行时会话创建路径返回的 generation。
    let generation = created["generation"]
        .as_u64()
        .expect("generation should be present");

    engine
        .public_runtime_sessions
        .replace_active_snapshot_for_test(
            &lease_id,
            json!({
                "ok": true,
                "sid": "corrupted-sid",
                "lease_id": "corrupted-lease",
                "generation": 999_u64,
                "profile": "system_lua_lib",
                "lifetime": "finite"
            }),
        );

    let listed: Value = serde_json::from_str(
        &engine
            .list_runtime_leases_json(r#"{"sid":"list-snapshot-test"}"#)
            .expect("list runtime sessions with typed sid"),
    )
    .expect("list snapshot response json");
    let leases = listed["leases"]
        .as_array()
        .expect("leases should be an array");

    assert_eq!(listed["ok"], true);
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0]["sid"], "list-snapshot-test");
    assert_eq!(leases[0]["lease_id"], lease_id);
    assert_eq!(leases[0]["generation"], generation);
    assert_eq!(leases[0]["profile"], "public");
}

/// Verify replaced runtime sessions return a stable lease_replaced error from status.
/// 验证被替换的运行时会话在 status 中会返回稳定的 lease_replaced 错误。
#[test]
fn runtime_session_status_reports_replaced_lease() {
    let engine = make_runtime_test_engine();
    let first_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"replace-status-test","ttl_sec":60}"#)
            .expect("create first runtime session"),
    )
    .expect("first create response json");
    let first_lease_id = first_created["lease_id"]
        .as_str()
        .expect("first lease id should be present")
        .to_string();

    let second_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(
                r#"{"sid":"replace-status-test","ttl_sec":60,"replace":true}"#,
            )
            .expect("create second runtime session"),
    )
    .expect("second create response json");
    assert_eq!(second_created["ok"], true);

    let status_request = json!({ "lease_id": first_lease_id });
    let status: Value = serde_json::from_str(
        &engine
            .runtime_lease_status_json(&status_request.to_string())
            .expect("status replaced runtime session"),
    )
    .expect("status response json");
    assert_eq!(status["ok"], false);
    assert_eq!(status["error_code"], "lease_replaced");
}

/// Verify a stale runtime-session handle observes lease_replaced after another caller replaces the SID lease.
/// 验证陈旧运行时会话句柄会在另一个调用方替换同 SID 租约后观察到 lease_replaced。
#[test]
fn runtime_session_stale_handle_reports_replaced_after_manager_get() {
    let engine = make_runtime_test_engine();
    let first_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"replace-race-test","ttl_sec":60}"#)
            .expect("create first runtime session"),
    )
    .expect("first create response json");
    let first_lease_id = first_created["lease_id"]
        .as_str()
        .expect("first lease id should be present")
        .to_string();
    let stale_session = engine
        .public_runtime_sessions
        .get(&first_lease_id, None, None, None)
        .expect("capture stale runtime session handle");

    let replaced: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"replace-race-test","ttl_sec":60,"replace":true}"#)
            .expect("replace runtime session"),
    )
    .expect("replace response json");
    assert_eq!(replaced["ok"], true);

    let mut stale_session = stale_session.lock().expect("lock stale runtime session");
    let error = LuaEngine::ensure_runtime_session_active(&mut stale_session)
        .expect_err("stale handle should fail");
    assert_eq!(error.code, "lease_replaced");
}

/// Verify replace=true rejects one busy lease before creating a second VM for the same SID.
/// 验证 replace=true 会在同一 SID 的旧租约忙碌时拒绝替换，而不会创建第二个虚拟机。
#[test]
fn runtime_session_replace_rejects_busy_lease() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"busy-replace-test","ttl_sec":60}"#)
            .expect("create busy replace runtime session"),
    )
    .expect("busy replace create response json");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("busy replace lease id should be present")
        .to_string();

    let session = engine
        .public_runtime_sessions
        .get(&lease_id, None, None, None)
        .expect("get busy replace runtime session");
    let guard = session.lock().expect("lock busy replace runtime session");

    let blocked_replace: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"busy-replace-test","ttl_sec":60,"replace":true}"#)
            .expect("replace busy runtime session"),
    )
    .expect("busy replace response json");
    assert_eq!(blocked_replace["ok"], false);
    assert_eq!(blocked_replace["error_code"], "lease_busy");
    assert!(
        blocked_replace["message"]
            .as_str()
            .expect("busy replace message should be present")
            .contains("cannot replace busy lease")
    );

    let listed: Value = serde_json::from_str(
        &engine
            .list_runtime_leases_json(r#"{"sid":"busy-replace-test"}"#)
            .expect("list busy replace runtime sessions"),
    )
    .expect("busy replace list response json");
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["leases"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed["leases"][0]["lease_id"], lease_id);

    drop(guard);

    let replaced: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"busy-replace-test","ttl_sec":60,"replace":true}"#)
            .expect("replace idle runtime session"),
    )
    .expect("idle replace response json");
    assert_eq!(replaced["ok"], true);
    assert_ne!(replaced["lease_id"], created["lease_id"]);
}

/// Verify poisoned runtime session locks recover for status, eval, and close operations.
/// 验证运行时会话锁 poison 后，status、eval 与 close 操作仍可恢复执行。
#[test]
fn runtime_session_operations_recover_poisoned_session_lock() {
    // Runtime engine used to create one persistent lease and poison its session lock.
    // 用于创建单个持久租约并制造其会话锁 poison 的运行时引擎。
    let engine = make_runtime_test_engine();
    // Created lease payload whose session lock will be poisoned before operations.
    // 会在操作前制造会话锁 poison 的已创建租约载荷。
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"poison-session-ops","ttl_sec":60}"#)
            .expect("create poisoned ops runtime session"),
    )
    .expect("poisoned ops create response json");
    // Opaque lease id used for all follow-up runtime session operations.
    // 用于所有后续运行时会话操作的不透明租约标识。
    let lease_id = created["lease_id"]
        .as_str()
        .expect("poisoned ops lease id should be present")
        .to_string();
    // Shared runtime session handle cloned from the manager before poisoning.
    // 在制造 poison 前从管理器克隆出的共享运行时会话句柄。
    let session = engine
        .public_runtime_sessions
        .get(&lease_id, None, None, None)
        .expect("get poisoned ops runtime session");

    // Captured panic result from a holder that poisons only the runtime session lock.
    // 单个运行时会话锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the runtime session lock.
        // 仅用于制造运行时会话锁 poison 的保护对象。
        let _guard = session.lock().expect("initial runtime session lock");
        panic!("poison runtime session lock for operations recovery test");
    }));
    assert!(poison_result.is_err());

    // Status response read through the recovered session lock.
    // 通过已恢复会话锁读取到的状态响应。
    let status_request = json!({ "lease_id": lease_id });
    let status: Value = serde_json::from_str(
        &engine
            .runtime_lease_status_json(&status_request.to_string())
            .expect("status through poisoned runtime session"),
    )
    .expect("poisoned status response json");
    assert_eq!(status["ok"], true);

    // Eval response executed through the recovered session lock.
    // 通过已恢复会话锁执行得到的 eval 响应。
    let eval_request = json!({
        "lease_id": lease_id,
        "code": "return 42"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&eval_request.to_string())
            .expect("eval through poisoned runtime session"),
    )
    .expect("poisoned eval response json");
    assert_eq!(eval["ok"], true);
    assert_eq!(eval["result"], json!(42));

    // Close response produced through the recovered session lock.
    // 通过已恢复会话锁产生的关闭响应。
    let close: Value = serde_json::from_str(
        &engine
            .close_runtime_lease_json(&status_request.to_string())
            .expect("close through poisoned runtime session"),
    )
    .expect("poisoned close response json");
    assert_eq!(close["ok"], true);
}

/// Verify replace=true recovers a poisoned existing session lock instead of treating it as busy.
/// 验证 replace=true 会恢复已 poison 的旧会话锁，而不是将其误判为忙碌。
#[test]
fn runtime_session_replace_recovers_poisoned_existing_session_lock() {
    // Runtime engine used to create and replace one poisoned SID-local lease.
    // 用于创建并替换单个已 poison 的 SID 局部租约的运行时引擎。
    let engine = make_runtime_test_engine();
    // Created lease payload whose session lock will be poisoned before replacement.
    // 替换前会制造会话锁 poison 的已创建租约载荷。
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"poison-replace-test","ttl_sec":60}"#)
            .expect("create poisoned replace runtime session"),
    )
    .expect("poisoned replace create response json");
    // Original lease id expected to be retired when replace=true succeeds.
    // replace=true 成功时预期会被退役的原始租约标识。
    let original_lease_id = created["lease_id"]
        .as_str()
        .expect("poisoned replace lease id should be present")
        .to_string();
    // Shared runtime session handle cloned from the manager before poisoning.
    // 在制造 poison 前从管理器克隆出的共享运行时会话句柄。
    let session = engine
        .public_runtime_sessions
        .get(&original_lease_id, None, None, None)
        .expect("get poisoned replace runtime session");

    // Captured panic result from a holder that poisons only the existing session lock.
    // 单个旧会话锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the existing runtime session lock.
        // 仅用于制造旧运行时会话锁 poison 的保护对象。
        let _guard = session
            .lock()
            .expect("initial poisoned replace runtime session lock");
        panic!("poison runtime session lock for replace recovery test");
    }));
    assert!(poison_result.is_err());

    // Replacement response proving Poisoned is recovered while WouldBlock remains the only busy case.
    // 替换响应，用于证明 Poisoned 会恢复，而 WouldBlock 才是真正忙碌场景。
    let replaced: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(
                r#"{"sid":"poison-replace-test","ttl_sec":60,"replace":true}"#,
            )
            .expect("replace poisoned runtime session"),
    )
    .expect("poisoned replace response json");
    assert_eq!(replaced["ok"], true);
    assert_ne!(replaced["lease_id"], original_lease_id);

    // Status response for the retired original lease should report replacement, not busy.
    // 已退役原始租约的状态响应应报告已替换，而不是忙碌。
    let original_status: Value = serde_json::from_str(
        &engine
            .runtime_lease_status_json(&json!({ "lease_id": original_lease_id }).to_string())
            .expect("status original poisoned replaced runtime session"),
    )
    .expect("poisoned original status response json");
    assert_eq!(original_status["ok"], false);
    assert_eq!(original_status["error_code"], "lease_replaced");
}

/// Verify runtime sessions reject a mismatched echoed SID before executing code.
/// 验证运行时会话会在执行前拒绝不匹配的回传 SID。
#[test]
fn runtime_session_eval_rejects_sid_mismatch() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"identity-test","ttl_sec":60}"#)
            .expect("create identity runtime session"),
    )
    .expect("identity create response json");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("identity lease id should be present")
        .to_string();

    let eval_request = json!({
        "lease_id": lease_id,
        "sid": "wrong-sid",
        "code": "return 1"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&eval_request.to_string())
            .expect("eval runtime session with wrong sid"),
    )
    .expect("wrong sid eval response json");
    assert_eq!(eval["ok"], false);
    assert_eq!(eval["error_code"], "lease_sid_mismatch");
}

/// Verify runtime sessions reject a mismatched echoed generation before executing code.
/// 验证运行时会话会在执行前拒绝不匹配的回传 generation。
#[test]
fn runtime_session_eval_rejects_generation_mismatch() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"generation-test","ttl_sec":60}"#)
            .expect("create generation runtime session"),
    )
    .expect("generation create response json");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("generation lease id should be present")
        .to_string();
    let sid = created["sid"]
        .as_str()
        .expect("generation sid should be present")
        .to_string();

    let eval_request = json!({
        "lease_id": lease_id,
        "sid": sid,
        "generation": 999_u64,
        "code": "return 1"
    });
    let eval: Value = serde_json::from_str(
        &engine
            .eval_runtime_lease_json(&eval_request.to_string())
            .expect("eval runtime session with wrong generation"),
    )
    .expect("wrong generation eval response json");
    assert_eq!(eval["ok"], false);
    assert_eq!(eval["error_code"], "lease_generation_mismatch");
}

/// Verify runtime-session list only returns active leases and supports SID filtering.
/// 验证运行时会话列表仅返回活跃租约并支持 SID 过滤。
#[test]
fn runtime_session_list_returns_only_active_leases() {
    let engine = make_runtime_test_engine();
    let alpha_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"alpha-test","ttl_sec":60}"#)
            .expect("create alpha runtime session"),
    )
    .expect("alpha create response json");
    let beta_created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"beta-test","ttl_sec":60}"#)
            .expect("create beta runtime session"),
    )
    .expect("beta create response json");
    let beta_lease_id = beta_created["lease_id"]
        .as_str()
        .expect("beta lease id should be present")
        .to_string();

    let all_list: Value = serde_json::from_str(
        &engine
            .list_runtime_leases_json(r#"{}"#)
            .expect("list runtime sessions"),
    )
    .expect("list response json");
    assert_eq!(all_list["ok"], true);
    assert_eq!(all_list["leases"].as_array().map(Vec::len), Some(2),);

    let alpha_only: Value = serde_json::from_str(
        &engine
            .list_runtime_leases_json(r#"{"sid":"alpha-test"}"#)
            .expect("list alpha runtime sessions"),
    )
    .expect("alpha list response json");
    assert_eq!(alpha_only["ok"], true);
    assert_eq!(alpha_only["leases"].as_array().map(Vec::len), Some(1),);
    assert_eq!(alpha_only["leases"][0]["sid"], alpha_created["sid"]);

    let beta_close_request = json!({ "lease_id": beta_lease_id });
    let beta_closed: Value = serde_json::from_str(
        &engine
            .close_runtime_lease_json(&beta_close_request.to_string())
            .expect("close beta runtime session"),
    )
    .expect("beta close response json");
    assert_eq!(beta_closed["ok"], true);

    let remaining: Value = serde_json::from_str(
        &engine
            .list_runtime_leases_json(r#"{}"#)
            .expect("list remaining runtime sessions"),
    )
    .expect("remaining list response json");
    assert_eq!(remaining["ok"], true);
    assert_eq!(remaining["leases"].as_array().map(Vec::len), Some(1),);
    assert_eq!(remaining["leases"][0]["sid"], alpha_created["sid"]);
}

/// Verify list requests still return busy active leases while a caller is holding the session lock.
/// 验证当调用方持有会话锁时列表请求仍然会返回忙碌但活跃的租约。
#[test]
fn runtime_session_list_keeps_busy_active_leases_visible() {
    let engine = make_runtime_test_engine();
    let created: Value = serde_json::from_str(
        &engine
            .create_runtime_lease_json(r#"{"sid":"busy-list-test","ttl_sec":60}"#)
            .expect("create busy runtime session"),
    )
    .expect("busy create response json");
    let lease_id = created["lease_id"]
        .as_str()
        .expect("busy lease id should be present")
        .to_string();
    let session = engine
        .public_runtime_sessions
        .get(&lease_id, None, None, None)
        .expect("get busy runtime session");
    let _guard = session.lock().expect("lock busy runtime session");

    let listed: Value = serde_json::from_str(
        &engine
            .list_runtime_leases_json(r#"{"sid":"busy-list-test"}"#)
            .expect("list busy runtime sessions"),
    )
    .expect("busy list response json");
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["leases"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed["leases"][0]["lease_id"], lease_id);
}

/// Verify that run_lua clears transient args after one failed execution.
/// 验证 run_lua 在失败执行后同样会清理临时参数状态。
#[test]
fn run_lua_clears_args_after_failure() {
    let engine = make_runtime_test_engine();
    let error = engine
        .run_lua("error('boom')", &json!({"value":"hello"}), None)
        .expect_err("run_lua should fail");
    assert!(error.contains("Lua run_lua error"));

    let lease = engine.acquire_vm().expect("reacquire pooled vm");
    assert_vm_scope_is_clean(lease.lua().expect("lease should own Lua VM"));
}

/// Verify that `vulcan.call` restores the outer execution context even when the nested skill corrupts it.
/// 验证当嵌套技能破坏上下文时，`vulcan.call` 仍会恢复外层执行上下文。
#[test]
fn vulcan_call_restores_outer_context_after_nested_failure() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_nested_call_restore_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    let skill_dir = skill_root.join("test-skill");
    fs::create_dir_all(skill_dir.join("runtime")).expect("create runtime dir");
    fs::write(
            skill_dir.join("skill.yaml"),
            "name: test-skill\nversion: 0.1.0\nenable: true\ndebug: false\nentries:\n  - name: outer\n    lua_entry: runtime/outer.lua\n    lua_module: test-skill.outer\n  - name: nested\n    lua_entry: runtime/nested.lua\n    lua_module: test-skill.nested\n",
        )
        .expect("write skill yaml");
    fs::write(
        skill_dir.join("runtime").join("outer.lua"),
        r#"return function(args)
  vulcan.context.entry_dir = "custom-entry-dir-before-nested"
  local ok, err = pcall(vulcan.call, "test-skill-nested", {})
  if ok then
    return "nested-call-unexpected-success"
  end
  local tool_name = (vulcan.runtime and vulcan.runtime.internal and vulcan.runtime.internal.tool_name) or "tool-nil"
  local entry_file = (vulcan.context and vulcan.context.entry_file) or "entry-nil"
  local entry_dir = (vulcan.context and vulcan.context.entry_dir) or "entry-dir-nil"
  local deps_path = (vulcan.deps and vulcan.deps.lua_path) or "deps-nil"
  return tool_name .. "|" .. entry_file .. "|" .. entry_dir .. "|" .. deps_path
end
"#,
    )
    .expect("write outer runtime entry");
    fs::write(
            skill_dir.join("runtime").join("nested.lua"),
            "return function(args)\n  vulcan.runtime = nil\n  vulcan.context = nil\n  vulcan.deps = nil\n  error(\"boom\")\nend\n",
        )
        .expect("write nested runtime entry");

    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create engine");
    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: skill_root.clone(),
        }])
        .expect("load nested-call test skill");

    let result = engine
        .call_skill("test-skill-outer", &json!({}), None)
        .expect("outer skill should succeed after nested failure");
    assert!(result.content.starts_with("test-skill-outer|"));
    assert!(result.content.contains("outer.lua"));
    assert!(!result.content.contains("|entry-nil|"));
    assert!(!result.content.contains("|entry-dir-nil|"));
    assert!(result.content.contains("|custom-entry-dir-before-nested|"));
    assert!(!result.content.ends_with("|deps-nil"));
    assert!(result.content.contains("test-skill"));

    let _ = fs::remove_dir_all(&temp_root);
}

/// Build one exact worker key backed by a real package and canonical environment directory.
/// 使用真实包与规范环境目录构造一个精确 Worker 键。
///
/// `runtime_root` owns the fixture and `label` partitions package and runtime identities.
/// `runtime_root` 拥有夹具，`label` 用于隔离包及运行时身份。
///
/// Return a key suitable for the production engine-owned worker service.
/// 返回适用于生产引擎所有 Worker 服务的键。
fn make_test_managed_runtime_worker_key(
    runtime_root: &Path,
    label: &str,
) -> (ManagedRuntimeWorkerKey, Arc<ManagedRuntimePackageContext>) {
    // Trusted package whose exact owner token participates in worker lifetime isolation.
    // 其精确所有者令牌参与 Worker 生命周期隔离的可信包。
    let package = make_test_managed_runtime_package(runtime_root, label);
    // Canonical environment directory participating in the complete worker key.
    // 参与完整 Worker 键的规范环境目录。
    let env_dir = runtime_root.join(format!("env-{label}"));
    fs::create_dir_all(&env_dir).expect("create worker key environment directory");
    let env_dir = fs::canonicalize(&env_dir).expect("canonicalize worker key environment");
    (
        ManagedRuntimeWorkerKey {
            runtime: label.to_string(),
            env_hash: format!("hash-{label}"),
            env_dir,
            package_identity: package.identity().clone(),
            owner_token: package.owner_token(),
            owner_state: package.owner_state(),
        },
        package,
    )
}

/// Verify the host-selected per-environment Worker maximum is enforced before process creation.
/// 验证宿主选择的每环境 Worker 上限会在进程创建前强制执行。
#[test]
fn managed_runtime_worker_pool_enforces_configured_capacity() {
    // RuntimeRoot owns the real package identity required by the production Worker key.
    // RuntimeRoot 拥有生产 Worker 键所需的真实包身份。
    let runtime_root = make_temp_runtime_root("managed-worker-configured-capacity");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create configured Worker runtime root");
    // Key and Package retain one exact environment and its live package owner.
    // Key 与 Package 保留一个精确环境及其活动包所有者。
    let (key, package) = make_test_managed_runtime_worker_key(&runtime_root, "capacity");
    // Config permits exactly one live Worker slot for the key.
    // Config 为该键精确允许一个活动 Worker 槽。
    let config = LuaRuntimeManagedRuntimeConfig {
        worker_pool_max_size_per_environment: 1,
        ..LuaRuntimeManagedRuntimeConfig::default()
    };
    // Pool is the production reservation state configured without spawning a child.
    // Pool 是未启动子进程而完成配置的生产预留状态。
    let mut pool = super::ManagedRuntimeWorkerPool::new(config);
    // FirstCheckout reserves the sole slot and therefore requires a future spawn.
    // FirstCheckout 预留唯一槽，因此要求后续启动进程。
    let (first_checkout, retired) = pool.reserve(&key).expect("reserve first Worker slot");
    assert!(matches!(
        first_checkout,
        super::ManagedRuntimeWorkerCheckout::SpawnReserved
    ));
    assert!(retired.is_empty());
    // Error proves the second reservation observes the host-selected maximum.
    // Error 证明第二次预留会遵守宿主选择的最大值。
    let error = pool
        .reserve(&key)
        .err()
        .expect("second Worker slot must exceed configured capacity");
    assert_eq!(
        error,
        "managed runtime worker pool is exhausted for this environment; max_size=1"
    );
    pool.discard(&key);
    drop(package);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the host-selected Worker idle lifetime retires an expired real Worker before reuse.
/// 验证宿主选择的 Worker 空闲时长会在复用前回收已过期的真实 Worker。
#[test]
fn managed_runtime_worker_pool_applies_configured_idle_ttl() {
    // EnvGuard serializes process-environment access while production Worker commands are spawned.
    // EnvGuard 在启动生产 Worker 命令期间串行化进程环境访问。
    let _env_guard = process_env_test_guard();
    // RuntimeRoot owns the real package, environment, and Worker processes used by this test.
    // RuntimeRoot 拥有本测试使用的真实包、环境与 Worker 进程。
    let runtime_root = make_temp_runtime_root("managed-worker-configured-idle-ttl");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create configured idle Worker runtime root");
    // Key and Package retain one exact active owner for both the expired and replacement Workers.
    // Key 与 Package 为过期 Worker 和替代 Worker 保留同一个精确活动所有者。
    let (key, _package) = make_test_managed_runtime_worker_key(&runtime_root, "idle-ttl");
    // Config selects a one-second idle lifetime so the test can mark a Worker deterministically old.
    // Config 选择一秒空闲时长，使测试能够确定性地把 Worker 标记为过期。
    let config = LuaRuntimeManagedRuntimeConfig {
        worker_pool_max_size_per_environment: 1,
        worker_idle_ttl_secs: 1,
        ..LuaRuntimeManagedRuntimeConfig::default()
    };
    // Service is the production engine-owned pool path configured with the nondefault lifetime.
    // Service 是使用非默认时长配置的生产引擎所有池路径。
    let service = ManagedRuntimeWorkerService::new_with_config(config)
        .expect("create configured idle Worker service");
    // FirstWorker is a real child returned to the configured idle pool.
    // FirstWorker 是归还到已配置空闲池的真实子进程。
    let (first_worker, first_reused) = service
        .acquire(key.clone(), || {
            let mut command = managed_runtime_echo_worker_command();
            spawn_managed_runtime_worker(&mut command)
        })
        .expect("spawn first configured idle Worker");
    assert!(!first_reused);
    service.release(key.clone(), first_worker);
    {
        // Pool exposes the returned Worker's monotonic timestamp for a sleep-free expiry fixture.
        // Pool 暴露已归还 Worker 的单调时间戳，用于构造无需 sleep 的过期夹具。
        let mut pool = service.lock_pool();
        let bucket = pool
            .buckets
            .get_mut(&key)
            .expect("configured idle Worker bucket");
        bucket.available[0].last_used_at = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("construct expired Worker timestamp");
    }
    // SpawnCount distinguishes a fresh replacement from accidental reuse of the expired Worker.
    // SpawnCount 用于区分新替代 Worker 与意外复用过期 Worker。
    let mut spawn_count = 0usize;
    let (replacement_worker, replacement_reused) = service
        .acquire(key.clone(), || {
            spawn_count += 1;
            let mut command = managed_runtime_echo_worker_command();
            spawn_managed_runtime_worker(&mut command)
        })
        .expect("replace expired configured idle Worker");

    assert!(!replacement_reused);
    assert_eq!(spawn_count, 1);
    service.discard(&key, replacement_worker);
    drop(service);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify one engine-owned worker service recovers after its isolated pool lock is poisoned.
/// 验证单个引擎所有 Worker 服务在隔离池锁中毒后仍可恢复。
#[test]
fn managed_runtime_worker_service_recovers_after_poisoned_engine_lock() {
    // Isolated service whose poison state cannot affect any other engine.
    // 其中毒状态不会影响任何其他引擎的隔离服务。
    let service = ManagedRuntimeWorkerService::new();
    // Captured panic result from a holder that poisons only this service's pool lock.
    // 仅使当前服务池锁中毒的持有者所产生的 panic 捕获结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the current engine-owned worker service.
        // 仅用于使当前引擎所有 Worker 服务中毒的保护对象。
        let _guard = service
            .pool
            .lock()
            .expect("initial managed runtime worker pool lock");
        panic!("poison managed runtime worker pool for recovery test");
    }));

    assert!(poison_result.is_err());

    // Real runtime root and package identity used by the migrated worker key.
    // 迁移后 Worker 键使用的真实运行时根与包身份。
    let runtime_root = make_temp_runtime_root("managed-worker-poison-key");
    let _ = fs::remove_dir_all(&runtime_root);
    let package = make_test_managed_runtime_package(&runtime_root, "poison-test");
    // Canonical environment directory participating in worker isolation.
    // 参与 Worker 隔离的规范环境目录。
    let env_dir = runtime_root.join("env");
    fs::create_dir_all(&env_dir).expect("create poison worker environment directory");
    let env_dir = fs::canonicalize(&env_dir).expect("canonicalize poison worker environment");
    // Unique worker key used for a harmless mutation against the recovered pool.
    // 用于对已恢复池执行无害修改的唯一 worker 键。
    let key = super::ManagedRuntimeWorkerKey {
        runtime: "poison-test".to_string(),
        env_hash: "hash".to_string(),
        env_dir,
        package_identity: package.identity().clone(),
        owner_token: package.owner_token(),
        owner_state: package.owner_state(),
    };
    // Recovered engine-local pool guard proving later access remains available.
    // 证明后续访问仍然可用的已恢复引擎局部池保护对象。
    let mut pool = service.lock_pool();
    pool.discard(&key);

    assert!(!pool.buckets.contains_key(&key));
    drop(pool);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify a missing-worker factory runs outside the service pool lock and may retire its owner.
/// 验证缺失 Worker 工厂在服务池锁之外运行，并且可以退役其所有者。
///
/// This regression test bounds completion so a future lock-order regression fails deterministically.
/// 此回归测试限制完成时间，使未来的锁顺序回退能够确定性失败。
#[test]
fn managed_runtime_worker_factory_runs_outside_pool_lock() {
    // Isolated runtime fixture and exact owner key used by the reentrant factory.
    // 重入工厂所使用的隔离运行时夹具与精确所有者键。
    let runtime_root = make_temp_runtime_root("managed-worker-unlocked-factory");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create unlocked factory runtime root");
    let (key, _package) = make_test_managed_runtime_worker_key(&runtime_root, "unlocked-factory");
    let owner_token = key.owner_token;
    let service = ManagedRuntimeWorkerService::new();
    let thread_service = Arc::clone(&service);
    let factory_service = Arc::clone(&service);
    // Bounded result channel proving the reentrant retirement cannot deadlock on the pool lock.
    // 有界结果通道，用于证明重入退役不会在池锁上死锁。
    let (result_sender, result_receiver) = std::sync::mpsc::sync_channel(1);
    let worker_thread = thread::spawn(move || {
        let outcome = thread_service.acquire(key, move || {
            // Owner retirement intentionally re-enters the same service during unlocked startup.
            // 所有者退役会在无锁启动期间有意重入同一个服务。
            factory_service.retire_owner(owner_token);
            Err("factory stopped after owner retirement".to_string())
        });
        let message = match outcome {
            Ok(_) => "unexpected worker acquisition success".to_string(),
            Err(error) => error,
        };
        result_sender
            .send(message)
            .expect("send unlocked factory outcome");
    });

    let error = result_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("worker factory must complete without holding the pool lock");
    assert_eq!(error, "factory stopped after owner retirement");
    worker_thread
        .join()
        .expect("join unlocked factory worker thread");
    drop(service);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify concurrent worker requests share one immutable owner snapshot and retire it safely.
/// 验证并发 Worker 请求共享同一个不可变所有者快照，并可安全退役该快照。
#[test]
fn managed_worker_package_snapshot_is_singleton_per_owner_and_retirement_safe() {
    let runtime_root = make_temp_runtime_root("managed-node-worker-snapshot-singleton");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create Node snapshot runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "snapshot-singleton");
    fs::write(
        package.package_root().join("handler.js"),
        "export default 1;\n",
    )
    .expect("write Node snapshot package source");
    let env_dir = runtime_root.join("env");
    fs::create_dir_all(&env_dir).expect("create Node snapshot environment");
    // CanonicalEnvDir uses the same native spelling retained by the root authority on Windows.
    // CanonicalEnvDir 使用与 Windows 根授权保留值相同的原生规范路径形式。
    let canonical_env_dir =
        fs::canonicalize(&env_dir).expect("canonicalize Node snapshot environment");
    let plan = make_validated_test_managed_node_env_plan(canonical_env_dir);
    write_test_managed_env_marker(&plan);
    let key = super::managed_runtime_worker_key(&plan, package.as_ref());
    let service = ManagedRuntimeWorkerService::new();
    // Barrier forces all callers to contend on the same lazy initialization cell.
    // Barrier 强制全部调用方在同一个延迟初始化单元上竞争。
    let barrier = Arc::new(Barrier::new(8));
    let mut threads = Vec::new();
    for _index in 0..8 {
        let thread_service = Arc::clone(&service);
        let thread_package = Arc::clone(&package);
        let thread_plan = plan.clone();
        let thread_key = key.clone();
        let thread_barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            thread_barrier.wait();
            thread_service.package_snapshot(&thread_key, &thread_plan, thread_package.as_ref())
        }));
    }
    let snapshots = threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .expect("join concurrent Node snapshot thread")
                .expect("prepare concurrent Node snapshot")
        })
        .collect::<Vec<_>>();
    let first_snapshot = snapshots
        .first()
        .expect("at least one Node snapshot should exist");
    let snapshot_root = first_snapshot.root().to_path_buf();
    assert!(
        snapshots
            .iter()
            .all(|snapshot| Arc::ptr_eq(first_snapshot, snapshot))
    );
    assert!(snapshot_root.join("handler.js").is_file());
    assert_eq!(service.lock_package_snapshots().len(), 1);

    service.retire_owner(package.owner_token());
    assert!(service.lock_package_snapshots().is_empty());
    assert!(
        snapshot_root.exists(),
        "active snapshot owners must retain files"
    );
    drop(snapshots);
    assert!(
        !snapshot_root.exists(),
        "snapshot must disappear after the final active owner is released"
    );
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify a transient package snapshot filesystem failure leaves the lazy cache and is retried.
/// 验证瞬态包快照文件系统失败会从延迟缓存移除并得到重试。
#[test]
fn managed_worker_package_snapshot_retries_after_transient_failure() {
    let runtime_root = make_temp_runtime_root("managed-node-worker-snapshot-retry");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create Node snapshot retry runtime root");
    let package = make_test_managed_runtime_package(&runtime_root, "snapshot-retry");
    fs::write(
        package.package_root().join("handler.js"),
        "export default 1;\n",
    )
    .expect("write retry Node package source");
    // A file at the private snapshot namespace forces the first namespace creation to fail while
    // the enclosing environment remains valid under its lifecycle lease.
    // 私有快照命名空间上的文件会强制首次命名空间创建失败，同时外围环境在生命周期租约下仍然有效。
    let env_dir = runtime_root.join("env");
    fs::create_dir_all(&env_dir).expect("create retry managed environment");
    // CanonicalEnvDir keeps the synthetic environment inside its canonical authority root.
    // CanonicalEnvDir 确保合成环境位于其规范授权根之内。
    let canonical_env_dir =
        fs::canonicalize(&env_dir).expect("canonicalize retry managed environment");
    let plan = make_validated_test_managed_node_env_plan(canonical_env_dir);
    write_test_managed_env_marker(&plan);
    let snapshot_namespace = plan.env_dir.join(".ls-w");
    fs::write(&snapshot_namespace, "temporary collision")
        .expect("write temporary snapshot namespace collision");
    let key = super::managed_runtime_worker_key(&plan, package.as_ref());
    let service = ManagedRuntimeWorkerService::new();
    let first_error = service
        .package_snapshot(&key, &plan, package.as_ref())
        .expect_err("temporary environment collision must fail snapshot creation");
    assert!(first_error.contains("failed to create managed package snapshot root"));
    assert!(service.lock_package_snapshots().is_empty());

    fs::remove_file(&snapshot_namespace).expect("remove temporary snapshot namespace collision");
    let snapshot = service
        .package_snapshot(&key, &plan, package.as_ref())
        .expect("retry Node snapshot after filesystem recovery");
    let snapshot_root = snapshot.root().to_path_buf();
    assert!(snapshot_root.join("handler.js").is_file());
    service.retire_owner(package.owner_token());
    drop(snapshot);
    assert!(!snapshot_root.exists());
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the managed runtime worker pool reuses one warm line-oriented worker.
/// 验证受管运行时 worker 池会复用一个热的逐行协议 worker。
#[test]
fn managed_runtime_worker_pool_reuses_warm_worker() {
    // Hold the shared PATH guard while the test spawns a named shell executable.
    // 在测试按名称启动 shell 可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Engine-owned service whose pool must retain one healthy worker between calls.
    // 其池必须在调用间保留一个健康 Worker 的引擎所有服务。
    let service = ManagedRuntimeWorkerService::new();
    // Real runtime root and package identity used by the migrated worker key.
    // 迁移后 Worker 键使用的真实运行时根与包身份。
    let runtime_root = make_temp_runtime_root("managed-worker-reuse-key");
    let _ = fs::remove_dir_all(&runtime_root);
    let package = make_test_managed_runtime_package(&runtime_root, "pool-test");
    // Canonical environment directory participating in worker isolation.
    // 参与 Worker 隔离的规范环境目录。
    let env_dir = runtime_root.join("env");
    fs::create_dir_all(&env_dir).expect("create reused worker environment directory");
    let env_dir = fs::canonicalize(&env_dir).expect("canonicalize reused worker environment");
    let key = super::ManagedRuntimeWorkerKey {
        runtime: "test".to_string(),
        env_hash: "hash".to_string(),
        env_dir,
        package_identity: package.identity().clone(),
        owner_token: package.owner_token(),
        owner_state: package.owner_state(),
    };
    let mut spawn_count = 0usize;
    let mut factory = || {
        spawn_count += 1;
        let mut command = managed_runtime_echo_worker_command();
        spawn_managed_runtime_worker(&mut command)
    };

    let (worker, reused) = service
        .acquire(key.clone(), &mut factory)
        .expect("first worker should spawn");
    assert!(!reused);
    let (worker, first) =
        invoke_managed_runtime_worker(worker, &json!({"value": 1}), Some(3_000), reused);
    assert!(!first.worker_reused);
    assert_eq!(first.envelope["ok"], true);
    assert_eq!(first.envelope["value"], 1);
    assert!(!first.discard_worker);
    service.release(key.clone(), worker);

    let (worker, reused) = service
        .acquire(key.clone(), &mut factory)
        .expect("second worker should reuse");
    assert!(reused);
    let (worker, second) =
        invoke_managed_runtime_worker(worker, &json!({"value": 2}), Some(3_000), reused);
    assert!(second.worker_reused);
    assert_eq!(second.envelope["ok"], true);
    assert_eq!(second.envelope["value"], 2);
    assert!(!second.discard_worker);
    service.release(key, worker);
    assert_eq!(spawn_count, 1);
    drop(service);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify every VM kind in one engine shares one worker service while separate engines remain isolated.
/// 验证单个引擎内的每种 VM 共享同一 Worker 服务，同时不同引擎保持隔离。
#[test]
fn managed_runtime_worker_service_is_engine_local_and_shared_by_all_vm_kinds() {
    // Independent engines whose short-worker services must never alias.
    // 其短期 Worker 服务绝不能互为别名的独立引擎。
    let first_engine = make_runtime_test_engine();
    let second_engine = make_runtime_test_engine();
    assert!(!Arc::ptr_eq(
        &first_engine.managed_runtime_workers,
        &second_engine.managed_runtime_workers
    ));

    // Ordinary pooled VM created through the primary engine path.
    // 通过主引擎路径创建的普通池化 VM。
    let ordinary_vm = first_engine.create_vm().expect("create ordinary worker VM");
    // Isolated runlua VM created with the exact engine-owned service Arc.
    // 使用精确引擎所有服务 Arc 创建的隔离 runlua VM。
    let runlua_vm = LuaEngine::create_runlua_vm(RunLuaVmBuildContext::from_engine(
        &first_engine,
        &first_engine.skills,
        &first_engine.entry_registry,
    ))
    .expect("create isolated runlua worker VM");
    // Dedicated System VM that must share services without ordinary Skill dispatch state.
    // 必须共享服务但不包含普通 Skill 分发状态的专用 System VM。
    let system_vm = first_engine
        .create_system_runtime_vm()
        .expect("create System worker VM");

    for vm in [&ordinary_vm, &runlua_vm, &system_vm] {
        // Installed service cloned after releasing the Lua app-data borrow.
        // 释放 Lua 应用数据借用后克隆的已安装服务。
        let installed_service = vm
            .lua
            .app_data_ref::<Arc<ManagedRuntimeWorkerService>>()
            .map(|service| Arc::clone(&service))
            .expect("worker service must be installed");
        assert!(Arc::ptr_eq(
            &installed_service,
            &first_engine.managed_runtime_workers
        ));
    }
}

/// Verify owner retirement removes idle workers and destroys active workers when they return.
/// 验证所有者退役会移除空闲 Worker，并在活动 Worker 归还时将其销毁。
#[test]
fn managed_runtime_worker_owner_retirement_covers_idle_and_active_workers() {
    // Hold the shared PATH guard while production worker commands are spawned.
    // 在启动生产 Worker 命令期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Isolated runtime fixture and exact owner-partitioned worker key.
    // 隔离运行时夹具与按精确所有者分区的 Worker 键。
    let runtime_root = make_temp_runtime_root("managed-worker-owner-retire");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create owner retirement runtime root");
    let (key, _package) = make_test_managed_runtime_worker_key(&runtime_root, "owner-retire");
    let service = ManagedRuntimeWorkerService::new();
    // Factory count proving two simultaneous checkouts reserve distinct workers.
    // 证明两个同时借出会预留不同 Worker 的工厂计数。
    let mut spawn_count = 0usize;
    let mut factory = || {
        spawn_count += 1;
        let mut command = managed_runtime_echo_worker_command();
        spawn_managed_runtime_worker(&mut command)
    };
    // Active worker retained across retirement and idle worker returned before retirement.
    // 跨退役保持活动的 Worker 与退役前已归还的空闲 Worker。
    let (active_worker, _) = service
        .acquire(key.clone(), &mut factory)
        .expect("acquire active owner worker");
    let (idle_worker, _) = service
        .acquire(key.clone(), &mut factory)
        .expect("acquire idle owner worker");
    service.release(key.clone(), idle_worker);
    {
        let pool = service.lock_pool();
        let bucket = pool.buckets.get(&key).expect("owner bucket before retire");
        assert_eq!(bucket.available.len(), 1);
        assert_eq!(bucket.total_count, 2);
    }

    service.retire_owner(key.owner_token);
    {
        let pool = service.lock_pool();
        assert!(!pool.buckets.contains_key(&key));
    }
    assert!(
        key.owner_state
            .upgrade()
            .expect("test owner state should remain live")
            .is_retired()
    );
    // Returning the previously active worker cannot recreate its retired bucket.
    // 归还此前活动的 Worker 不能重新创建其已退役桶。
    service.release(key.clone(), active_worker);
    assert!(!service.lock_pool().buckets.contains_key(&key));
    let retired_error = service
        .acquire(key.clone(), || panic!("retired owner factory must not run"))
        .err()
        .expect("retired owner acquisition must fail");
    assert!(retired_error.contains("is retired"));
    assert_eq!(spawn_count, 2);
    drop(service);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify dropping an engine releases its worker service and every idle worker owned by that service.
/// 验证释放引擎会释放其 Worker 服务以及该服务拥有的全部空闲 Worker。
#[test]
fn lua_engine_drop_releases_managed_runtime_worker_service() {
    // Hold the shared PATH guard while one real idle worker is installed into the engine service.
    // 将一个真实空闲 Worker 安装到引擎服务期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let runtime_root = make_temp_runtime_root("managed-worker-engine-drop");
    let _ = fs::remove_dir_all(&runtime_root);
    fs::create_dir_all(&runtime_root).expect("create engine drop runtime root");
    let (key, _package) = make_test_managed_runtime_worker_key(&runtime_root, "engine-drop");
    let engine = make_runtime_test_engine();
    // Weak service reference proving no VM or pool retains the service after engine destruction.
    // 证明引擎销毁后没有 VM 或池保留服务的弱引用。
    let weak_service = Arc::downgrade(&engine.managed_runtime_workers);
    let (worker, _) = engine
        .managed_runtime_workers
        .acquire(key.clone(), || {
            let mut command = managed_runtime_echo_worker_command();
            spawn_managed_runtime_worker(&mut command)
        })
        .expect("spawn engine-owned idle worker");
    engine.managed_runtime_workers.release(key, worker);
    drop(engine);
    assert!(weak_service.upgrade().is_none());
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify ordinary Skill reload retires only old loaded-Skill owners and preserves unrelated System owners.
/// 验证普通 Skill 重载只退役旧已加载 Skill 所有者，并保留无关 System 所有者。
#[test]
fn ordinary_skill_reload_retires_only_loaded_skill_worker_owners() {
    // Synthetic old Skill whose exact package lifetime must retire after replacement state is ready.
    // 其精确包生命周期必须在替换状态就绪后退役的合成旧 Skill。
    let old_skill = make_loaded_skill("worker-reload-old", "worker-reload", "main", "main");
    let old_owner_state = old_skill
        .managed_package
        .owner_state()
        .upgrade()
        .expect("old Skill owner state should be live");
    let runtime_root = old_skill.managed_package.runtime_root().to_path_buf();
    // Independent package owner representing a System lease outside the ordinary Skill table.
    // 表示普通 Skill 表之外 System 租约的独立包所有者。
    let system_package = make_test_managed_runtime_package(&runtime_root, "system-preserved");
    let system_owner_state = system_package
        .owner_state()
        .upgrade()
        .expect("System owner state should be live");
    let mut skills = HashMap::new();
    skills.insert("worker-reload".to_string(), old_skill);
    let mut engine = make_test_engine(skills);
    // Empty replacement ROOT proving the old Skill disappears without introducing a new owner.
    // 证明旧 Skill 消失且不引入新所有者的空替换 ROOT。
    let replacement_skills = runtime_root.join("replacement-skills");
    fs::create_dir_all(&replacement_skills).expect("create empty replacement Skill root");
    engine
        .reload_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: replacement_skills,
        }])
        .expect("reload empty ordinary Skill root");
    assert!(old_owner_state.is_retired());
    assert!(!system_owner_state.is_retired());
    drop(engine);
    drop(system_package);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify an oversized worker protocol line is fully drained without consuming the next record.
/// 验证超大 Worker 协议行会被完整排空，且不会消费下一条记录。
#[test]
fn managed_runtime_worker_line_limit_preserves_next_protocol_record() {
    // Input contains one over-limit record followed by one valid JSON envelope.
    // 输入包含一条超限记录及其后的一条有效 JSON 信封。
    let mut input = vec![b'x'; super::MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES + 64];
    input.extend_from_slice(b"\n{\"ok\":true}\n");
    let mut reader = std::io::Cursor::new(input);
    let oversized = super::read_managed_runtime_worker_line(&mut reader)
        .expect("read oversized worker line")
        .expect("oversized worker line should produce one record")
        .expect_err("oversized worker line must be rejected");
    assert!(oversized.contains("protocol line exceeded"));

    let next = super::read_managed_runtime_worker_line(&mut reader)
        .expect("read protocol record after oversized line")
        .expect("valid worker line should remain available")
        .expect("valid worker line should decode");
    assert_eq!(next, r#"{"ok":true}"#);
    assert!(
        super::read_managed_runtime_worker_line(&mut reader)
            .expect("read clean worker EOF")
            .is_none()
    );
}

/// Verify malformed managed runtime worker envelopes become explicit protocol errors.
/// 验证格式错误的受管运行时 worker 信封会变成显式协议错误。
///
/// This test has no parameters and fails through assertions when malformed envelopes are defaulted.
/// 本测试不接收参数；当格式错误的信封被默认值掩盖时会通过断言失败。
///
/// Return unit after validating worker discard state and the Lua-facing error payload.
/// 校验 worker 丢弃状态与面向 Lua 的错误载荷后返回 unit。
#[test]
fn managed_runtime_worker_rejects_malformed_json_envelope() {
    // Hold the shared PATH guard while the test spawns a named shell executable.
    // 在测试按名称启动 shell 可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Worker command that emits a JSON object missing the required `ok` protocol field.
    // 发出缺少必填 `ok` 协议字段 JSON 对象的 worker 命令。
    let mut command = managed_runtime_malformed_envelope_worker_command();
    // Managed runtime worker spawned through the production worker launcher.
    // 通过生产 worker 启动器创建的受管运行时 worker。
    let worker =
        spawn_managed_runtime_worker(&mut command).expect("malformed envelope worker should spawn");

    // Invocation result returned after the malformed envelope is read from stdout.
    // 从 stdout 读取格式错误信封后返回的调用结果。
    let (worker, result) =
        invoke_managed_runtime_worker(worker, &json!({"value": 1}), Some(3_000), false);

    assert!(result.discard_worker);
    assert_eq!(result.envelope["ok"], false);
    // Protocol error captured in the normalized worker envelope.
    // 归一化 worker 信封中捕获到的协议错误。
    let envelope_error = result.envelope["error"]
        .as_str()
        .expect("normalized envelope should include protocol error");
    assert!(envelope_error.contains("malformed JSON envelope"));
    assert!(envelope_error.contains("field `ok`"));

    // Test plan used only to render the final Lua-facing payload metadata.
    // 仅用于渲染最终面向 Lua 载荷元数据的测试计划。
    let plan = make_test_managed_node_env_plan(PathBuf::from("D:/malformed-envelope-env"));
    // Lua-facing payload converted from the normalized worker invocation result.
    // 从已归一化 worker 调用结果转换得到的面向 Lua 载荷。
    let payload = managed_runtime_worker_result_to_json(result, &plan);

    assert_eq!(payload["ok"], false);
    assert_eq!(payload["value"], Value::Null);
    assert_eq!(payload["stdout"], "");
    assert_eq!(payload["stderr"], "");
    assert_eq!(payload["timed_out"], false);
    assert_eq!(payload["worker_reused"], false);
    // Protocol error preserved in the final Lua-facing payload.
    // 最终面向 Lua 载荷中保留的协议错误。
    let payload_error = payload["error"]
        .as_str()
        .expect("payload should include protocol error");
    assert!(payload_error.contains("malformed JSON envelope"));
    assert!(payload_error.contains("field `ok`"));
    drop(worker);
}

/// Build a tiny cross-platform JSON line echo worker command for pool tests.
/// 为池测试构造一个极小的跨平台 JSON 行回显 worker 命令。
fn managed_runtime_echo_worker_command() -> Command {
    #[cfg(windows)]
    {
        let script = "$OutputEncoding=[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; while (($line=[Console]::In.ReadLine()) -ne $null) { $request = $line | ConvertFrom-Json; $response = @{ ok = $true; value = $request.value; stdout = ''; stderr = '' } | ConvertTo-Json -Compress; [Console]::Out.WriteLine($response); [Console]::Out.Flush() }";
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", script]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "while IFS= read -r line; do value=$(printf '%s' \"$line\" | sed -n 's/.*\"value\":\\([0-9][0-9]*\\).*/\\1/p'); printf '{\"ok\":true,\"value\":%s,\"stdout\":\"\",\"stderr\":\"\"}\\n' \"$value\"; done",
        ]);
        command
    }
}

/// Build a tiny cross-platform worker command that emits a malformed JSON envelope.
/// 构造一个发出格式错误 JSON 信封的极小跨平台 worker 命令。
///
/// Return a command that responds to each request with JSON missing the required `ok` field.
/// 返回一个对每个请求响应缺少必填 `ok` 字段 JSON 的命令。
fn managed_runtime_malformed_envelope_worker_command() -> Command {
    #[cfg(windows)]
    {
        // PowerShell script that returns one malformed managed runtime envelope per input line.
        // 每行输入返回一个格式错误受管运行时信封的 PowerShell 脚本。
        let script = r#"$OutputEncoding=[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; while (($line=[Console]::In.ReadLine()) -ne $null) { [Console]::Out.WriteLine('{"value":1,"stdout":"","stderr":""}'); [Console]::Out.Flush() }"#;
        // PowerShell command used as the malformed envelope worker process.
        // 作为格式错误信封 worker 进程使用的 PowerShell 命令。
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", script]);
        command
    }
    #[cfg(not(windows))]
    {
        // POSIX shell script that returns one malformed managed runtime envelope per input line.
        // 每行输入返回一个格式错误受管运行时信封的 POSIX shell 脚本。
        let script = r#"while IFS= read -r line; do printf '%s\n' '{"value":1,"stdout":"","stderr":""}'; done"#;
        // Shell command used as the malformed envelope worker process.
        // 作为格式错误信封 worker 进程使用的 shell 命令。
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }
}
