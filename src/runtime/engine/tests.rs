use super::host_result::{
    host_result_capability_to_json_value, normalize_change_set_payload,
    resolve_host_result_capability, validate_change_set_payload,
};
use super::lease::RuntimeSessionManager;
use super::runlua::{
    ExecShellLauncher, lock_runlua_cwd_guard, lock_runlua_print_capture, runlua_cwd_guard,
};
#[cfg(windows)]
use super::vulcan_process_candidate_paths;
#[cfg(windows)]
use super::windows_wide_null_path;
use super::{
    LoadedSkill, LuaEngine, LuaVm, LuaVmPool, LuaVmPoolConfig, LuaVmPoolState,
    LuaVmRequestScopeGuard, ManagedRuntimeWorkerPool, NativeLibrarySearchGuard,
    ResolvedEntryTarget, SkillApplyLifecycleAction, SkillConfigStore,
    VulcanInternalExecutionContext, build_lua_call_dispatch_entries,
    copy_managed_node_skill_import_root, default_runlua_vm_pool_config,
    find_vulcan_process_candidate, format_lifecycle_recovery_error,
    format_vulcan_fs_list_non_utf8_file_name_error, get_vulcan_context_table,
    get_vulcan_deps_table, get_vulcan_runtime_internal_table, get_vulcan_table,
    invoke_managed_runtime_worker, json_to_lua_table, lock_managed_runtime_worker_pool,
    lua_value_to_json, managed_runtime_status_from_plan, managed_runtime_worker_pool,
    managed_runtime_worker_result_to_json, parse_runtime_request_context_json,
    populate_vulcan_dependency_context, populate_vulcan_file_context,
    populate_vulcan_internal_execution_context, prepare_managed_node_import_root,
    read_lua_help_payload_source, read_skill_text_file, render_host_visible_path,
    render_lua_help_payload_text, render_lua_print_argument, resolve_managed_runtime_skill_file,
    resolve_vulcan_fs_copy_effective_destination_path, runtime_root_from_skill_dir,
    spawn_managed_runtime_worker, system_time_to_unix_millis_i64, vulcan_fs_target_exists,
    vulcan_fs_target_is_dir,
};
use crate::host::callbacks::runtime_model_callback_test_guard;
use crate::host::database::RuntimeDatabaseProviderCallbacks;
use crate::lua_skill::SkillMeta;
use crate::runtime::encoding::{RuntimeTextEncoding, encode_runtime_text};
use crate::runtime::managed_runtime::{
    ManagedRuntimeEnvMarker, ManagedRuntimeEnvPlan, ManagedRuntimeKind, managed_env_marker_path,
};
use crate::runtime_options::LuaRuntimeRunLuaPoolConfig;
use crate::{
    LuaEngineOptions, LuaRuntimeCapabilityOptions, LuaRuntimeHostOptions, RuntimeClientInfo,
    RuntimeHostToolAction, RuntimeModelEmbedRequest, RuntimeModelEmbedResponse, RuntimeModelError,
    RuntimeModelErrorCode, RuntimeModelLlmRequest, RuntimeModelLlmResponse, RuntimeModelUsage,
    RuntimeRequestContext, RuntimeSkillRoot, SkillInstallRequest, SkillInstallSourceType,
    SkillManagementAuthority, SkillUninstallOptions, set_host_tool_callback,
    set_model_embed_callback, set_model_llm_callback,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use mlua::{Lua, Table, Value as LuaValue};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink as create_unix_symlink};
#[cfg(windows)]
use std::os::windows::fs::{
    symlink_dir as create_windows_dir_symlink, symlink_file as create_windows_file_symlink,
};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

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
    LoadedSkill {
        meta,
        dir: PathBuf::from(format!("D:/tests/{directory_name}")),
        root_name: "ROOT".to_string(),
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
        runtime_sessions: Arc::new(RuntimeSessionManager::new()),
        skill_config_store: Arc::new(
            SkillConfigStore::new(None).expect("create runtime test skill config store"),
        ),
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
  node_table = type(vulcan.runtime.node),
  node_status = type(vulcan.runtime.node.status),
  node_invoke = type(vulcan.runtime.node.invoke),
}
"#,
            &json!({}),
            None,
        )
        .expect("managed runtime bridge tables should be registered");

    assert_eq!(result["python_table"], "table");
    assert_eq!(result["python_status"], "function");
    assert_eq!(result["python_invoke"], "function");
    assert_eq!(result["node_table"], "table");
    assert_eq!(result["node_status"], "function");
    assert_eq!(result["node_invoke"], "function");
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

/// Build one minimal managed Node environment plan for import-root tests.
/// 为 import-root 测试构造一个最小受管 Node 环境计划。
///
/// The env_dir parameter is the environment directory that owns `.luaskills-skill`.
/// env_dir 参数是拥有 `.luaskills-skill` 的环境目录。
///
/// Return a plan with stable dummy dependency metadata and the requested environment directory.
/// 返回带有稳定假依赖元数据和指定环境目录的计划。
fn make_test_managed_node_env_plan(env_dir: PathBuf) -> ManagedRuntimeEnvPlan {
    ManagedRuntimeEnvPlan {
        runtime: ManagedRuntimeKind::Node,
        platform: "windows-x64".to_string(),
        runtime_root: env_dir.join("runtime-root"),
        runtime_version: "20.0.0".to_string(),
        runtime_executable: env_dir.join("node.exe"),
        package_manager: "pnpm".to_string(),
        package_manager_version: "9.0.0".to_string(),
        package_manager_executable: env_dir.join("pnpm.cmd"),
        package_manifest_path: None,
        lockfile_path: env_dir.join("pnpm-lock.yaml"),
        lock_hash: "lock-hash".to_string(),
        package_manifest_hash: None,
        env_hash: "env-hash".to_string(),
        env_dir: env_dir.clone(),
        expected_marker: ManagedRuntimeEnvMarker {
            schema_version: 1,
            runtime: "node".to_string(),
            runtime_version: "20.0.0".to_string(),
            package_manager: "pnpm".to_string(),
            package_manager_version: "9.0.0".to_string(),
            platform: "windows-x64".to_string(),
            lock_hash: "lock-hash".to_string(),
            package_manifest_hash: None,
            env_hash: "env-hash".to_string(),
        },
    }
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
                "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nentries:\n  - name: ping\n    description: Config ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
            ),
        )
        .expect("write config test skill yaml");
    fs::write(
            skill_dir.join("runtime").join("ping.lua"),
            "return function(args)\n  local value = vulcan.config.get(\"api_token\")\n  if value == nil then\n    return \"missing\"\n  end\n  return value\nend\n",
        )
        .expect("write config test runtime entry");
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
    // Skill directory shape that cannot provide the expected runtime root ancestor.
    // 无法提供预期 runtime root 祖先目录的 skill 目录形态。
    let skill_dir = PathBuf::from("orphan-skill");
    // Error returned by the real managed runtime root derivation helper.
    // 真实受管运行时根目录推导辅助函数返回的错误。
    let error = runtime_root_from_skill_dir(&skill_dir, "vulcan.runtime.python.status")
        .expect_err("orphan skill dir should not derive runtime root");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!(
        "vulcan.runtime.python.status: failed to derive runtime root from skill_dir {}",
        render_host_visible_path(&skill_dir)
    );

    assert!(
        error.to_string().contains(&expected),
        "unexpected error: {}",
        error
    );
}

/// Verify managed runtime source-file errors render paths through the host-visible formatter.
/// 验证受管运行时源文件错误会通过宿主可见路径渲染器输出路径。
#[test]
fn managed_runtime_skill_file_error_uses_host_visible_path() {
    // Temporary skill directory used by the real managed runtime file resolver.
    // 真实受管运行时文件解析器使用的临时 skill 目录。
    let skill_dir = make_temp_runtime_root("managed-runtime-missing-file").join("skills/demo");
    let _ = fs::remove_dir_all(&skill_dir);
    fs::create_dir_all(&skill_dir).expect("create managed runtime skill dir");
    // Missing source file path expected after safe skill-relative resolution.
    // 安全 skill 相对解析后预期得到的缺失源文件路径。
    let missing_file = skill_dir.join("handlers/missing.py");
    // Error returned by the real managed runtime skill-file resolver.
    // 真实受管运行时 skill 文件解析器返回的错误。
    let error = resolve_managed_runtime_skill_file(
        &skill_dir,
        "handlers/missing.py",
        "vulcan.runtime.python.invoke",
        "file",
    )
    .expect_err("missing managed runtime source file should fail");
    // Expected diagnostic fragment rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断片段。
    let expected = format!(
        "vulcan.runtime.python.invoke: file not found: {}",
        render_host_visible_path(&missing_file)
    );

    assert!(
        error.to_string().contains(&expected),
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
    // Skill directory containing one embedded NUL that makes the resolved source file impossible to inspect.
    // 包含内嵌 NUL 的 skill 目录，使解析后的源文件无法被探测。
    let invalid_skill_dir = PathBuf::from("invalid\0skill");

    // Error returned before the invalid source file can behave like a missing file.
    // 在非法源文件表现得像缺失文件之前返回的错误。
    let error = resolve_managed_runtime_skill_file(
        &invalid_skill_dir,
        "handlers/main.py",
        "vulcan.runtime.python.invoke",
        "file",
    )
    .expect_err("invalid managed runtime source file probe should fail")
    .to_string();

    assert!(
        error.contains("vulcan.runtime.python.invoke: failed to inspect file"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("handlers"), "unexpected error: {}", error);
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
    // Temporary skill directory used to isolate the directory source-file fixture.
    // 用于隔离目录型源文件夹具的临时 skill 目录。
    let skill_dir = make_temp_runtime_root("managed-runtime-directory-file").join("skills/demo");
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = fs::remove_dir_all(&skill_dir);
    // Directory deliberately placed where the managed runtime request requires one source file.
    // 故意放在受管运行时请求需要源文件位置上的目录。
    let directory_source = skill_dir.join("handlers/main.py");
    fs::create_dir_all(&directory_source).expect("create directory source fixture");

    // Error returned before the directory source can be reported as a missing file.
    // 在目录型源文件被报告为缺失文件之前返回的错误。
    let error = resolve_managed_runtime_skill_file(
        &skill_dir,
        "handlers/main.py",
        "vulcan.runtime.node.invoke",
        "file",
    )
    .expect_err("directory managed runtime source path should fail")
    .to_string();

    assert!(
        error.contains("vulcan.runtime.node.invoke: file is not a file"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&directory_source)),
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

/// Verify managed Node import-root cleanup errors render paths through the host-visible formatter.
/// 验证受管 Node import-root 清理错误会通过宿主可见路径渲染器输出路径。
#[test]
fn managed_node_import_root_cleanup_error_uses_host_visible_path() {
    // Temporary root that isolates the managed Node import-root cleanup fixture.
    // 隔离受管 Node import-root 清理夹具的临时根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-cleanup-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Managed environment directory that owns the import root.
    // 拥有 import root 的受管环境目录。
    let env_dir = temp_root.join("env");
    // Skill directory supplied to the real import-root preparation helper.
    // 传给真实 import-root 准备辅助函数的 skill 目录。
    let skill_dir = temp_root.join("skills/demo");
    fs::create_dir_all(&env_dir).expect("create managed node env dir");
    fs::create_dir_all(&skill_dir).expect("create managed node skill dir");
    // Existing import-root file that makes remove_dir_all fail deterministically.
    // 让 remove_dir_all 稳定失败的既有 import-root 文件。
    let import_root = env_dir.join(".luaskills-skill");
    fs::write(&import_root, "stale import root file").expect("write stale import root file");
    // Minimal managed Node env plan consumed by the real preparation helper.
    // 真实准备辅助函数消费的最小受管 Node 环境计划。
    let plan = make_test_managed_node_env_plan(env_dir);
    // Error returned by the real managed Node import-root preparation helper.
    // 真实受管 Node import-root 准备辅助函数返回的错误。
    let error = prepare_managed_node_import_root(&plan, &skill_dir)
        .expect_err("file import root should fail directory removal");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to remove {}:",
        render_host_visible_path(&import_root)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    let _ = fs::remove_dir_all(&temp_root);
}

/// Verify managed Node import-root probe errors are reported before copy starts.
/// 验证受管 Node import-root 探测错误会在复制开始前被报告。
#[test]
fn managed_node_import_root_invalid_existing_path_is_reported() {
    // Temporary skill root that isolates the managed Node import-root probe fixture.
    // 隔离受管 Node import-root 探测夹具的临时 skill 根目录。
    let temp_root = make_temp_runtime_root("managed-node-import-invalid-path");
    let _ = fs::remove_dir_all(&temp_root);
    // Skill directory supplied to the real import-root preparation helper.
    // 传给真实 import-root 准备辅助函数的 skill 目录。
    let skill_dir = temp_root.join("skills/demo");
    fs::create_dir_all(&skill_dir).expect("create managed node skill dir");
    // Managed environment path containing an embedded NUL that symlink_metadata cannot inspect.
    // 包含内嵌 NUL 且 symlink_metadata 无法探测的受管环境路径。
    let env_dir = PathBuf::from("invalid\0managed-node-env");
    // Minimal managed Node env plan consumed by the real preparation helper.
    // 真实准备辅助函数消费的最小受管 Node 环境计划。
    let plan = make_test_managed_node_env_plan(env_dir);

    // Error returned before the invalid import root can be treated as absent and recreated.
    // 在非法 import root 被当作不存在并重建之前返回的错误。
    let error = prepare_managed_node_import_root(&plan, &skill_dir)
        .expect_err("invalid import root probe should fail before copy");

    assert!(
        error.starts_with("Failed to inspect"),
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
    let error = copy_managed_node_skill_import_root(&source_dir, &destination_dir)
        .expect_err("copying a file onto a directory should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to copy {} to {}:",
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
    let error = copy_managed_node_skill_import_root(&source_dir, &destination_dir)
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
    let error = copy_managed_node_skill_import_root(&source_dir, &destination_dir)
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

/// Verify the engine host API persists string skill config values into one explicit config file.
/// 验证引擎宿主 API 会把字符串技能配置值持久化到显式配置文件中。
#[test]
fn skill_config_engine_api_persists_values_into_explicit_file() {
    let runtime_root = make_temp_runtime_root("skill_config_explicit_path");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);
    let config_file = runtime_root.join("custom").join("skill_config.json");

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_file_path: Some(config_file.clone()),
        ..Default::default()
    })
    .expect("create skill config test engine");

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
        .delete_skill_config_value("demo-skill", "api_token")
        .expect("delete explicit skill config");
    assert!(deleted);
    assert_eq!(
        engine
            .get_skill_config_value("demo-skill", "api_token")
            .expect("read deleted explicit skill config"),
        None
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the unified skill config store falls back to `<runtime_root>/config/skill_config.json` after roots load.
/// 验证统一技能配置存储会在加载根目录后回退到 `<runtime_root>/config/skill_config.json`。
#[test]
fn skill_config_store_uses_default_runtime_config_file_after_load() {
    let runtime_root = make_temp_runtime_root("skill_config_default_path");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);

    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create default skill config test engine");

    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        }])
        .expect("load empty roots for default skill config path");

    let expected_path = runtime_root.join("config").join("skill_config.json");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve default skill config file path"),
        expected_path
    );

    engine
        .set_skill_config_value("demo-skill", "endpoint", "https://example.test")
        .expect("write default skill config");
    assert!(expected_path.exists());

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify the unified skill config store resolves the default config path even before the skills directory exists.
/// 验证统一技能配置存储会在技能目录尚未创建前解析默认配置路径。
#[test]
fn skill_config_store_initializes_default_path_before_skills_dir_exists() {
    let runtime_root = make_temp_runtime_root("skill_config_without_skills_dir");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    fs::create_dir_all(&runtime_root).expect("create runtime root without skills dir");

    let missing_skills_dir = runtime_root.join("skills");
    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create config path initialization test engine");

    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: missing_skills_dir,
        }])
        .expect("load roots without an existing skills directory");

    let expected_path = runtime_root.join("config").join("skill_config.json");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve config path without skills directory"),
        expected_path
    );

    engine
        .set_skill_config_value("demo-skill", "api_token", "sk-before-install")
        .expect("write config before any skills directory exists");
    assert!(expected_path.exists());

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

/// Verify reload failures after formal validation still preserve the active runtime view.
/// 验证 formal 校验之后发生的重载失败仍会保留当前活动运行时视图。
#[test]
fn reload_from_roots_preserves_state_after_ambiguous_config_root_error() {
    let runtime_root = make_temp_runtime_root("reload-ambiguous-preserves-state");
    let first_ambiguous_root = make_temp_runtime_root("reload-ambiguous-first");
    let second_ambiguous_root = make_temp_runtime_root("reload-ambiguous-second");
    for path in [&runtime_root, &first_ambiguous_root, &second_ambiguous_root] {
        if path.exists() {
            let _ = fs::remove_dir_all(path);
        }
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
        .load_from_roots(&[root_root, user_root])
        .expect("initial root and user runtime should load");
    let previous_config_path = engine
        .skill_config_store
        .file_path()
        .expect("resolve previous skill config path");

    let ambiguous_reload_error = engine
        .reload_from_roots(&[
            RuntimeSkillRoot {
                name: "ROOT".to_string(),
                skills_dir: first_ambiguous_root.join("skills"),
            },
            RuntimeSkillRoot {
                name: "PROJECT".to_string(),
                skills_dir: second_ambiguous_root.join("skills"),
            },
        ])
        .expect_err("ambiguous config root reload should fail");
    assert!(
        ambiguous_reload_error
            .to_string()
            .contains("multiple runtime roots map to different parents")
    );

    let result = engine
        .call_skill("vulcan-codekit-ping", &json!({}), None)
        .expect("old entry should remain callable after ambiguous reload failure");
    assert_eq!(result.content, "user");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve config path after failed reload"),
        previous_config_path
    );

    let layers = engine
        .run_lua("return vulcan.runtime.skills.layers()", &json!({}), None)
        .expect("layers should still use the previous root chain");
    assert_eq!(layers["labels"], json!(["USER"]));
    assert_eq!(layers["default"], json!("USER"));

    let _ = fs::remove_dir_all(&runtime_root);
    let _ = fs::remove_dir_all(&first_ambiguous_root);
    let _ = fs::remove_dir_all(&second_ambiguous_root);
}

/// Verify reloading a different runtime root updates the default unified skill-config path.
/// 验证重新加载另一套运行时根目录时会同步更新默认统一技能配置路径。
#[test]
fn reload_from_roots_updates_default_skill_config_path() {
    let first_runtime_root = make_temp_runtime_root("skill_config_reload_first");
    let second_runtime_root = make_temp_runtime_root("skill_config_reload_second");
    if first_runtime_root.exists() {
        let _ = fs::remove_dir_all(&first_runtime_root);
    }
    if second_runtime_root.exists() {
        let _ = fs::remove_dir_all(&second_runtime_root);
    }
    create_runtime_test_layout(&first_runtime_root);
    create_runtime_test_layout(&second_runtime_root);

    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create reload skill config test engine");

    engine
        .load_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: first_runtime_root.join("skills"),
        }])
        .expect("load first runtime root");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve first config path"),
        first_runtime_root.join("config").join("skill_config.json")
    );

    engine
        .reload_from_roots(&[crate::host::options::RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: second_runtime_root.join("skills"),
        }])
        .expect("reload second runtime root");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve second config path"),
        second_runtime_root.join("config").join("skill_config.json")
    );

    let _ = fs::remove_dir_all(&first_runtime_root);
    let _ = fs::remove_dir_all(&second_runtime_root);
}

/// Verify reload keeps the initially resolved explicit relative skill-config path.
/// 验证重载会保持初始解析后的显式相对技能配置路径。
#[test]
fn reload_from_roots_keeps_frozen_relative_explicit_skill_config_path() {
    let _cwd_guard = lock_runlua_cwd_guard();
    let original_cwd = std::env::current_dir().expect("resolve original cwd");
    /// Restore the process current directory when the test exits.
    /// 在测试退出时恢复进程当前工作目录。
    struct CwdRestoreGuard(PathBuf);
    impl Drop for CwdRestoreGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _cwd_restore = CwdRestoreGuard(original_cwd);
    let first_cwd = make_temp_runtime_root("skill_config_reload_relative_cwd_first");
    let second_cwd = make_temp_runtime_root("skill_config_reload_relative_cwd_second");
    let runtime_root = make_temp_runtime_root("skill_config_reload_relative_runtime");
    for path in [&first_cwd, &second_cwd, &runtime_root] {
        if path.exists() {
            let _ = fs::remove_dir_all(path);
        }
        fs::create_dir_all(path).expect("create explicit config reload test directory");
    }
    let relative_config_path = PathBuf::from("config").join("skill_config.json");
    std::env::set_current_dir(&first_cwd).expect("switch to first cwd");
    let expected_config_path = first_cwd.join(&relative_config_path);

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_file_path: Some(relative_config_path),
        ..Default::default()
    })
    .expect("create explicit relative config reload test engine");
    engine
        .load_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("root_skills"),
        }])
        .expect("load initial root for explicit relative config reload test");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve explicit config path before reload"),
        expected_config_path
    );

    std::env::set_current_dir(&second_cwd).expect("switch to second cwd before reload");
    engine
        .reload_from_roots(&[RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("other_root_skills"),
        }])
        .expect("reload should preserve frozen explicit config path");
    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve explicit config path after reload"),
        expected_config_path
    );

    let _ = fs::remove_dir_all(&first_cwd);
    let _ = fs::remove_dir_all(&second_cwd);
    let _ = fs::remove_dir_all(&runtime_root);
}

/// Verify explicit unified config file paths bypass ambiguous runtime-root inference.
/// 验证显式统一配置文件路径会绕过歧义运行时根目录推导。
#[test]
fn load_from_roots_accepts_explicit_skill_config_path_for_ambiguous_runtime_roots() {
    let first_runtime_root = make_temp_runtime_root("skill_config_explicit_ambiguous_first");
    let second_runtime_root = make_temp_runtime_root("skill_config_explicit_ambiguous_second");
    if first_runtime_root.exists() {
        let _ = fs::remove_dir_all(&first_runtime_root);
    }
    if second_runtime_root.exists() {
        let _ = fs::remove_dir_all(&second_runtime_root);
    }
    fs::create_dir_all(&first_runtime_root).expect("create first explicit ambiguous runtime root");
    fs::create_dir_all(&second_runtime_root)
        .expect("create second explicit ambiguous runtime root");
    let explicit_config_file = first_runtime_root.join("custom").join("skill_config.json");

    let mut engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        skill_config_file_path: Some(explicit_config_file.clone()),
        ..Default::default()
    })
    .expect("create explicit ambiguous root test engine");

    engine
        .load_from_roots(&[
            crate::host::options::RuntimeSkillRoot {
                name: "ROOT".to_string(),
                skills_dir: first_runtime_root.join("skills"),
            },
            crate::host::options::RuntimeSkillRoot {
                name: "PROJECT".to_string(),
                skills_dir: second_runtime_root.join("skills"),
            },
        ])
        .expect("explicit config path should bypass ambiguous runtime roots");

    assert_eq!(
        engine
            .skill_config_store
            .file_path()
            .expect("resolve explicit config path"),
        explicit_config_file
    );

    let _ = fs::remove_dir_all(&first_runtime_root);
    let _ = fs::remove_dir_all(&second_runtime_root);
}

/// Verify divergent runtime roots require one explicit unified skill config file path.
/// 验证运行时根目录分叉时必须显式提供统一技能配置文件路径。
#[test]
fn load_from_roots_rejects_ambiguous_default_skill_config_runtime_root() {
    let first_runtime_root = make_temp_runtime_root("skill_config_ambiguous_first");
    let second_runtime_root = make_temp_runtime_root("skill_config_ambiguous_second");
    if first_runtime_root.exists() {
        let _ = fs::remove_dir_all(&first_runtime_root);
    }
    if second_runtime_root.exists() {
        let _ = fs::remove_dir_all(&second_runtime_root);
    }
    fs::create_dir_all(&first_runtime_root).expect("create first ambiguous runtime root");
    fs::create_dir_all(&second_runtime_root).expect("create second ambiguous runtime root");

    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
            .expect("create ambiguous root test engine");

    let error = engine
        .load_from_roots(&[
            crate::host::options::RuntimeSkillRoot {
                name: "ROOT".to_string(),
                skills_dir: first_runtime_root.join("skills"),
            },
            crate::host::options::RuntimeSkillRoot {
                name: "PROJECT".to_string(),
                skills_dir: second_runtime_root.join("skills"),
            },
        ])
        .expect_err("ambiguous runtime roots should require an explicit config file path");
    assert!(
        error
            .to_string()
            .contains("set host_options.skill_config_file_path explicitly"),
        "unexpected ambiguous root error: {error}"
    );

    let _ = fs::remove_dir_all(&first_runtime_root);
    let _ = fs::remove_dir_all(&second_runtime_root);
}

/// Verify lexically equivalent runtime roots do not get misclassified as ambiguous.
/// 验证词法等价的运行时根目录不会被误判为歧义根目录。
#[test]
fn canonical_skill_config_runtime_root_normalizes_equivalent_runtime_roots() {
    let runtime_root = make_temp_runtime_root("skill_config_equivalent_runtime_root");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    create_runtime_test_layout(&runtime_root);

    let engine = try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
        .expect("create equivalent runtime root test engine");

    let equivalent_root = runtime_root.join("nested").join("..").join("skills");
    let resolved_runtime_root = engine
        .canonical_skill_config_runtime_root(&[
            crate::host::options::RuntimeSkillRoot {
                name: "ROOT".to_string(),
                skills_dir: runtime_root.join("skills"),
            },
            crate::host::options::RuntimeSkillRoot {
                name: "PROJECT".to_string(),
                skills_dir: equivalent_root,
            },
        ])
        .expect("equivalent runtime roots should resolve to one canonical root");

    assert_eq!(resolved_runtime_root, runtime_root);

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

    let mut engine =
        try_make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions::default())
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
    // Skill directory containing one embedded NUL that makes dependencies.yaml impossible to inspect.
    // 包含内嵌 NUL 的 skill 目录，使 dependencies.yaml 无法被探测。
    let invalid_skill_dir = PathBuf::from("invalid\0managed-skill");

    // Error returned before the invalid manifest can behave like a missing required manifest.
    // 在非法清单表现得像必需清单缺失之前返回的错误。
    let error = super::load_current_managed_runtime_manifest(
        &invalid_skill_dir,
        "vulcan.runtime.node.invoke",
    )
    .expect_err("invalid required managed runtime manifest probe should fail")
    .to_string();

    assert!(
        error.contains("vulcan.runtime.node.invoke: failed to inspect dependency manifest"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("dependencies.yaml"),
        "unexpected error: {}",
        error
    );
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
    // Skill directory containing one embedded NUL that makes dependencies.yaml impossible to inspect.
    // 包含内嵌 NUL 的 skill 目录，使 dependencies.yaml 无法被探测。
    let invalid_skill_dir = PathBuf::from("invalid\0managed-status-skill");

    // Error returned before the invalid manifest can behave like an absent optional manifest.
    // 在非法清单表现得像可选清单不存在之前返回的错误。
    let error = super::load_optional_current_managed_runtime_manifest(
        &invalid_skill_dir,
        "vulcan.runtime.node.status",
    )
    .expect_err("invalid optional managed runtime manifest probe should fail")
    .to_string();

    assert!(
        error.contains("vulcan.runtime.node.status: failed to inspect dependency manifest"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("dependencies.yaml"),
        "unexpected error: {}",
        error
    );
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
        .load_single_skill(&invalid_skill_dir, "ROOT")
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
        .load_single_skill(&skill_dir, "ROOT")
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
        .load_single_skill(&skill_dir, "ROOT")
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
        .load_single_skill(&skill_dir, "ROOT")
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
        .load_single_skill(&skill_dir, "ROOT")
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
        .load_single_skill(&skill_dir, "ROOT")
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
        .load_single_skill(&skill_dir, "ROOT")
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
#[cfg(unix)]
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
    let request = json!({
        "code": "return vulcan.json.encode({ found = vulcan.process.which(args.program) })",
        "args": {
            "program": render_host_visible_path(&program_path)
        }
    });

    let result = engine
        .execute_runlua_request_json_inline(&request.to_string())
        .expect("inline runlua should resolve explicit process.which path");

    let expected_found = serde_json::to_string(&render_host_visible_path(&program_path))
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
                r#"{"code":"local info = vulcan.os.info(); local spec; if info.os == 'windows' then spec = { program = 'cmd', args = { '/V:ON', '/C', 'set /P line=&echo session:!line!' }, encoding = 'utf-8' } else spec = { program = 'sh', args = { '-c', 'read line; echo session:$line' }, encoding = 'utf-8' } end; local session = vulcan.process.session.open(spec); session:write('ok\\n'); local status = session:close({ timeout_ms = 3000 }); local output = session:read({ timeout_ms = 3000 }); return output.stdout, status.exited, status.success"}"#,
            )
            .expect("inline runlua should exercise process session");

    assert!(result.contains("SUCCESS"));
    assert!(result.contains("session:ok"));
    assert!(result.contains("true"));
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

/// Verify system runtime leases preserve one explicit host-owned cwd while still exposing the fixed system_lua_lib directory.
/// 验证 system 运行时租约会保留宿主显式传入的 cwd，同时继续暴露固定的 system_lua_lib 目录。
#[test]
fn system_runtime_lease_preserves_explicit_cwd_override() {
    let runtime_root = make_temp_runtime_root("system-runtime-lease-cwd");
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let explicit_cwd = runtime_root.join("host-cwd");
    let fixed_system_dir = runtime_root.join("fixed-system-lua-lib");
    fs::create_dir_all(&explicit_cwd).expect("create explicit host cwd");

    let engine = make_runtime_test_engine_with_host_options(LuaRuntimeHostOptions {
        system_lua_lib_dir: Some(fixed_system_dir.clone()),
        ..Default::default()
    });

    let created: Value = serde_json::from_str(
        &engine
            .create_system_runtime_lease_json(
                &json!({
                    "authority": "system",
                    "sid": "system-cwd-test",
                    "ttl_sec": 60,
                    "cwd": render_host_visible_path(&explicit_cwd)
                })
                .to_string(),
            )
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
        json!(render_host_visible_path(&fixed_system_dir))
    );

    let lease_id = created["lease_id"]
        .as_str()
        .expect("lease id should be present")
        .to_string();
    let generation = created["generation"]
        .as_u64()
        .expect("generation should be present");

    let status: Value = serde_json::from_str(
        &engine
            .system_runtime_lease_status_json(
                &json!({
                    "authority": "system",
                    "lease_id": lease_id,
                    "generation": generation
                })
                .to_string(),
            )
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
        json!(render_host_visible_path(&fixed_system_dir))
    );

    let eval: Value = serde_json::from_str(
        &engine
            .eval_system_runtime_lease_json(
                &json!({
                    "authority": "system",
                    "lease_id": lease_id,
                    "generation": generation,
                    "code": "return { cwd = vulcan.runtime.cwd() }"
                })
                .to_string(),
            )
            .expect("eval system runtime lease"),
    )
    .expect("system runtime lease eval response json");
    assert_eq!(eval["ok"], true);
    assert_eq!(eval["cwd"], json!(render_host_visible_path(&explicit_cwd)));
    assert_eq!(
        eval["system_lua_lib"],
        json!(render_host_visible_path(&fixed_system_dir))
    );
    assert_eq!(
        eval["result"]["cwd"],
        json!(render_host_visible_path(&explicit_cwd))
    );

    let _ = fs::remove_dir_all(&runtime_root);
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

    engine.runtime_sessions.replace_active_snapshot_for_test(
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

    engine.runtime_sessions.replace_active_snapshot_for_test(
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
        .runtime_sessions
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
        .runtime_sessions
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
        .runtime_sessions
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
        .runtime_sessions
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
        .runtime_sessions
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

/// Verify the managed runtime worker pool recovers after its process-wide lock is poisoned.
/// 验证进程级受管运行时 worker 池锁 poison 后仍可恢复。
#[test]
fn managed_runtime_worker_pool_recovers_after_poisoned_global_lock() {
    // Captured panic result from a holder that poisons the global managed runtime worker pool lock.
    // 全局受管运行时 worker 池锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the process-wide managed runtime worker pool.
        // 仅用于制造进程级受管运行时 worker 池 poison 的保护对象。
        let _guard = managed_runtime_worker_pool()
            .lock()
            .expect("initial managed runtime worker pool lock");
        panic!("poison managed runtime worker pool for recovery test");
    }));

    assert!(poison_result.is_err());

    // Unique worker key used for a harmless mutation against the recovered pool.
    // 用于对已恢复池执行无害修改的唯一 worker 键。
    let key = super::ManagedRuntimeWorkerKey {
        runtime: "poison-test".to_string(),
        env_hash: "hash".to_string(),
        skill_dir: PathBuf::from("D:/poison-test-skill"),
    };
    // Recovered global worker pool guard used to prove later pool access does not fail.
    // 已恢复的全局 worker 池保护对象，用于证明后续池访问不会失败。
    let mut pool = lock_managed_runtime_worker_pool();
    pool.discard(&key);

    assert!(!pool.buckets.contains_key(&key));
}

/// Verify the managed runtime worker pool reuses one warm line-oriented worker.
/// 验证受管运行时 worker 池会复用一个热的逐行协议 worker。
#[test]
fn managed_runtime_worker_pool_reuses_warm_worker() {
    // Hold the shared PATH guard while the test spawns a named shell executable.
    // 在测试按名称启动 shell 可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let mut pool = ManagedRuntimeWorkerPool::new();
    let key = super::ManagedRuntimeWorkerKey {
        runtime: "test".to_string(),
        env_hash: "hash".to_string(),
        skill_dir: PathBuf::from("D:/test-skill"),
    };
    let mut spawn_count = 0usize;
    let mut factory = || {
        spawn_count += 1;
        let mut command = managed_runtime_echo_worker_command();
        spawn_managed_runtime_worker(&mut command)
    };

    let (worker, reused) = pool
        .acquire(key.clone(), &mut factory)
        .expect("first worker should spawn");
    assert!(!reused);
    let (worker, first) =
        invoke_managed_runtime_worker(worker, &json!({"value": 1}), Some(3_000), reused);
    assert!(!first.worker_reused);
    assert_eq!(first.envelope["ok"], true);
    assert_eq!(first.envelope["value"], 1);
    assert!(!first.discard_worker);
    pool.release(key.clone(), worker);

    let (worker, reused) = pool
        .acquire(key.clone(), &mut factory)
        .expect("second worker should reuse");
    assert!(reused);
    let (worker, second) =
        invoke_managed_runtime_worker(worker, &json!({"value": 2}), Some(3_000), reused);
    assert!(second.worker_reused);
    assert_eq!(second.envelope["ok"], true);
    assert_eq!(second.envelope["value"], 2);
    assert!(!second.discard_worker);
    pool.release(key, worker);
    assert_eq!(spawn_count, 1);
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
