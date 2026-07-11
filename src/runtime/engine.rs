use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use mlua::{
    AnyUserData, Function, HookTriggers, Lua, MultiValue, Table, Value as LuaValue, VmState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt::Display;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::{
    AddDllDirectory, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_USER_DIRS,
    RemoveDllDirectory, SetDefaultDllDirectories,
};

use crate::dependency::manager::{DependencyManager, DependencyManagerConfig, ensure_directory};
use crate::entry_descriptor::{RuntimeEntryDescriptor, RuntimeEntryParameterDescriptor};
use crate::host::callbacks::{
    RuntimeEntryRegistryDelta, RuntimeHostToolAction, RuntimeHostToolRequest, RuntimeModelCaller,
    RuntimeModelEmbedRequest, RuntimeModelEmbedResponse, RuntimeModelError, RuntimeModelErrorCode,
    RuntimeModelLlmRequest, RuntimeModelLlmResponse, RuntimeModelUsage, RuntimeSkillLifecycleEvent,
    RuntimeSkillManagementAction, RuntimeSkillManagementRequest,
    RuntimeSkillOperationProgressEmitter, dispatch_host_tool_request, dispatch_model_embed_request,
    dispatch_model_llm_request, dispatch_skill_management_request, try_has_host_tool_callback,
    try_has_model_embed_callback, try_has_model_llm_callback, try_has_skill_management_callback,
};
use crate::host::database::RuntimeDatabaseProviderCallbacks;
use crate::lancedb_host::{LanceDbSkillBinding, LanceDbSkillHost, disabled_skill_status_json};
use crate::lua_skill::{SkillMeta, validate_luaskills_identifier, validate_luaskills_version};
use crate::runtime::config::{SkillConfigEntry, SkillConfigStore};
use crate::runtime::encoding::{
    RuntimeTextEncoding, decode_runtime_text, default_runtime_text_encoding, encode_runtime_text,
};
use crate::runtime::managed_io::{create_vulcan_io_table, install_managed_io_compat};
use crate::runtime::managed_package::{
    ManagedRuntimeOwnerState, ManagedRuntimePackageContext, ManagedRuntimePackageIdentity,
    current_lua_managed_package_context, optional_lua_managed_package_context,
    replace_lua_managed_package_context, retire_managed_runtime_owner_state,
};
use crate::runtime::managed_runtime::{
    ManagedRuntimeEnvPlan, current_managed_runtime_persistent_session_capability,
    ensure_managed_env, managed_env_is_ready, resolve_node_env_plan, resolve_python_env_plan,
};
use crate::runtime::managed_runtime_services::{
    ManagedRuntimeServices, ManagedRuntimeSessionEventIdentity, ManagedRuntimeTransactionContext,
};
#[cfg(test)]
use crate::runtime::managed_runtime_session::prepare_unleased_managed_package_snapshot;
use crate::runtime::managed_runtime_session::{
    ManagedPackageSnapshot, ManagedRuntimeSessionOpenRequest,
    configure_managed_node_command_environment, configure_managed_python_command_environment,
    ensure_persistent_managed_runtime_session_platform_supported, launch_managed_node_session,
    launch_managed_python_session, prepare_managed_package_snapshot,
};
use crate::runtime::managed_session_events::{
    ManagedSessionEventCenter, RuntimeManagedSessionEventBatch, RuntimeManagedSessionWakeCallback,
};
use crate::runtime::path::render_host_visible_path;
use crate::runtime::process_session::{
    DetachedChildReaperPermit, ManagedChildProcessTree, create_managed_process_session_userdata,
    create_process_session_table,
};
use crate::runtime_context::{RuntimeClientInfo, RuntimeRequestContext};
use crate::runtime_help::{
    RuntimeHelpDetail, RuntimeHelpNodeDescriptor, RuntimeSkillHelpDescriptor,
};
use crate::runtime_logging::{error as log_error, info as log_info, warn as log_warn};
use crate::runtime_options::{LuaInvocationContext, LuaRuntimeHostOptions, RuntimeSkillRoot};
use crate::runtime_result::RuntimeInvocationResult;
use crate::skill::dependencies::PackageDependencyManifest;
use crate::skill::manager::{
    PreparedSkillApply, ResolvedSkillInstance, SkillApplyResult, SkillInstallRequest,
    SkillManagementAuthority, SkillManager, SkillManagerConfig, SkillOperationPlane,
    SkillUninstallOptions, SkillUninstallResult, collect_effective_skill_instances_from_roots,
    resolve_declared_skill_instance_from_roots, resolve_effective_skill_instance_from_roots,
    resolve_requested_skill_id,
};
use crate::sqlite_host::{
    SqliteSkillBinding, SqliteSkillHost,
    disabled_skill_status_json as disabled_sqlite_skill_status_json,
};
use crate::tool_cache::{ToolCacheConfig, configure_global_tool_cache, global_tool_cache};

mod bridge;
mod host_result;
mod lease;
mod runlua;

use self::bridge::{
    create_host_tool_call_fn, create_host_tool_has_fn, create_host_tool_list_fn,
    create_model_embed_fn, create_model_has_fn, create_model_llm_fn, create_model_status_fn,
    create_runtime_skill_layers_fn, create_runtime_skill_management_bridge_fn,
};
use self::host_result::{
    host_result_capability_to_json_value, parse_tool_call_output, resolve_host_result_capability,
};
use self::lease::RuntimeSessionManager;
use self::runlua::{
    RunLuaRuntimeContext, default_exec_shell_name, exec_result_to_lua_table, execute_exec_request,
    optional_u64_arg, parse_exec_request, require_path_arg, require_string_arg, require_table_arg,
    resolve_host_default_text_encoding, supported_exec_shell_names,
};

// ============================================================
// Loaded skill (compiled Lua function + metadata)
// ============================================================

#[derive(Clone)]
struct LoadedSkill {
    meta: SkillMeta,
    dir: std::path::PathBuf,
    root_name: String,
    /// Trusted managed-runtime package context built by the skill loader.
    /// 由 Skill 加载器构造的可信受管运行时包上下文。
    managed_package: Arc<ManagedRuntimePackageContext>,
    lancedb_binding: Option<Arc<LanceDbSkillBinding>>,
    sqlite_binding: Option<Arc<SqliteSkillBinding>>,
    resolved_entry_names: HashMap<String, String>,
}

/// Render one filesystem path for user-facing runtime logs without Windows verbatim prefixes.
/// 为面向用户的运行时日志渲染文件系统路径，并去掉 Windows verbatim 前缀。
fn render_log_friendly_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Normalize one runtime-root path with stable lexical component folding.
/// 使用稳定的词法组件折叠规则规范化单个运行时根目录路径。
fn normalize_runtime_root_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut can_pop_normal = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
                can_pop_normal = false;
            }
            Component::RootDir => {
                normalized.push(component.as_os_str());
                can_pop_normal = false;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if can_pop_normal && normalized.pop() {
                    can_pop_normal = !matches!(
                        normalized.components().next_back(),
                        Some(Component::Prefix(_)) | Some(Component::RootDir) | None
                    );
                } else if !path.is_absolute() {
                    normalized.push(component.as_os_str());
                    can_pop_normal = false;
                }
            }
            Component::Normal(part) => {
                normalized.push(part);
                can_pop_normal = true;
            }
        }
    }
    normalized
}

/// Structured path table stored in `resources/luaskills-packages-manifest.json`.
/// 存储在 `resources/luaskills-packages-manifest.json` 中的结构化路径表。
#[derive(Debug, Deserialize)]
struct RuntimePackagesManifestPaths {
    install_manifest: String,
    compat_lua_packages_txt: String,
    platform_support: String,
    third_party_licenses: String,
    third_party_notices: String,
    help_index: String,
    package_help_root: String,
    module_help_root: String,
    license_index: String,
}

/// Runtime-facing luaskills-packages manifest embedded inside one packaged runtime.
/// 嵌入在一个打包运行时中的面向运行时的 luaskills-packages 清单。
#[derive(Debug, Deserialize)]
struct RuntimePackagesManifest {
    schema_version: u32,
    layout: String,
    paths: RuntimePackagesManifestPaths,
}

/// Expected filesystem kind for one packaged-runtime manifest target.
/// 单个打包运行时清单目标期望的文件系统类型。
#[derive(Debug, Clone, Copy)]
enum PackagedRuntimeTargetKind {
    /// Target must resolve to a regular file.
    /// 目标必须解析为普通文件。
    File,
    /// Target must resolve to a directory.
    /// 目标必须解析为目录。
    Directory,
}

impl PackagedRuntimeTargetKind {
    /// Return whether filesystem metadata satisfies this packaged-runtime target kind.
    /// 返回文件系统元数据是否满足当前打包运行时目标类型。
    ///
    /// The metadata parameter is the resolved filesystem metadata for the manifest target.
    /// metadata 参数是清单目标对应的已解析文件系统元数据。
    ///
    /// Return true when the metadata matches the expected target kind.
    /// 当元数据匹配期望目标类型时返回 true。
    fn matches_metadata(self, metadata: &fs::Metadata) -> bool {
        match self {
            Self::File => metadata.is_file(),
            Self::Directory => metadata.is_dir(),
        }
    }

    /// Return the noun used when rendering diagnostics for this packaged-runtime target kind.
    /// 返回渲染当前打包运行时目标类型诊断时使用的名词。
    ///
    /// Return a stable English noun for user-facing packaged-runtime layout errors.
    /// 返回用于面向用户的打包运行时布局错误的稳定英文名词。
    fn diagnostic_noun(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

/// Require one runtime-relative path string and reject absolute or traversal-only payloads.
/// 要求一个运行时相对路径字符串，并拒绝绝对路径或纯穿越型载荷。
fn validate_runtime_relative_manifest_path(label: &str, relative_path: &str) -> Result<(), String> {
    let candidate = Path::new(relative_path);
    if candidate.is_absolute() {
        return Err(format!(
            "packaged runtime is invalid: {} must be runtime-relative, got '{}'",
            label, relative_path
        ));
    }
    if candidate.components().next().is_none() {
        return Err(format!("packaged runtime is invalid: {} is empty", label));
    }
    Ok(())
}

/// Validate one required manifest target path inside a packaged runtime root.
/// 校验打包运行时根目录中的一个必需清单目标路径。
///
/// The runtime_root parameter is the packaged runtime root used to resolve manifest-relative paths.
/// runtime_root 参数是用于解析清单相对路径的打包运行时根目录。
///
/// The label parameter names the manifest field being validated.
/// label 参数命名正在校验的清单字段。
///
/// The relative_path parameter is the runtime-relative path declared by the manifest.
/// relative_path 参数是清单声明的运行时相对路径。
///
/// The expected_kind parameter is the required filesystem kind for the declared target.
/// expected_kind 参数是声明目标必须满足的文件系统类型。
///
/// Return unit when the manifest target exists with the expected kind.
/// 当清单目标以期望类型存在时返回 unit。
fn validate_packaged_runtime_target(
    runtime_root: &Path,
    label: &str,
    relative_path: &str,
    expected_kind: PackagedRuntimeTargetKind,
) -> Result<(), String> {
    validate_runtime_relative_manifest_path(label, relative_path)?;
    let candidate = runtime_root.join(relative_path);
    if !packaged_runtime_target_exists(&candidate, label, expected_kind)? {
        return Err(format!(
            "packaged runtime is invalid: missing {}",
            render_log_friendly_path(&candidate)
        ));
    }
    Ok(())
}

/// Inspect one packaged-runtime target without hiding filesystem metadata errors or type mismatches.
/// 检查单个打包运行时目标，同时不隐藏文件系统元数据错误或类型不匹配。
///
/// The path parameter is the concrete packaged-runtime path being inspected.
/// path 参数是正在检查的具体打包运行时路径。
///
/// The label parameter names the manifest field or marker path used in diagnostics.
/// label 参数命名用于诊断的清单字段或标记路径。
///
/// The expected_kind parameter is the required filesystem kind for the target.
/// expected_kind 参数是目标必须满足的文件系统类型。
///
/// Return true for a matching target, false for a confirmed missing path, or an explicit probe/type error.
/// 匹配目标返回 true，确认缺失路径返回 false；探测或类型失败时返回显式错误。
fn packaged_runtime_target_exists(
    path: &Path,
    label: &str,
    expected_kind: PackagedRuntimeTargetKind,
) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if expected_kind.matches_metadata(&metadata) => Ok(true),
        Ok(_) => Err(format!(
            "packaged runtime is invalid: {} is not a {}: {}",
            label,
            expected_kind.diagnostic_noun(),
            render_log_friendly_path(path)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "packaged runtime is invalid: failed to inspect {} '{}': {}",
            label,
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Validate the luaskills-packages metadata layout embedded under one packaged runtime resources directory.
/// 校验一个打包运行时 resources 目录下嵌入的 luaskills-packages 元数据布局。
fn validate_packaged_runtime_packages_layout(resources_dir: &Path) -> Result<(), String> {
    let runtime_manifest_path = resources_dir.join("lua-runtime-manifest.json");
    if !packaged_runtime_target_exists(
        &runtime_manifest_path,
        "lua-runtime-manifest",
        PackagedRuntimeTargetKind::File,
    )? {
        return Ok(());
    }

    let runtime_root = resources_dir.parent().ok_or_else(|| {
        format!(
            "packaged runtime is invalid: resources directory has no parent: {}",
            render_log_friendly_path(resources_dir)
        )
    })?;
    let packages_manifest_path = resources_dir.join("luaskills-packages-manifest.json");
    if !packaged_runtime_target_exists(
        &packages_manifest_path,
        "luaskills-packages-manifest",
        PackagedRuntimeTargetKind::File,
    )? {
        return Err(format!(
            "packaged runtime is incomplete: missing {}",
            render_log_friendly_path(&packages_manifest_path)
        ));
    }

    let manifest_text = fs::read_to_string(&packages_manifest_path).map_err(|error| {
        format!(
            "packaged runtime is invalid: failed to read {}: {}",
            render_log_friendly_path(&packages_manifest_path),
            error
        )
    })?;
    let manifest: RuntimePackagesManifest =
        serde_json::from_str(&manifest_text).map_err(|error| {
            format!(
                "packaged runtime is invalid: failed to parse {}: {}",
                render_log_friendly_path(&packages_manifest_path),
                error
            )
        })?;

    if manifest.schema_version != 1 {
        return Err(format!(
            "packaged runtime is invalid: unsupported luaskills-packages manifest schema_version {}",
            manifest.schema_version
        ));
    }
    if manifest.layout != "luaskills-packages-runtime-v1" {
        return Err(format!(
            "packaged runtime is invalid: unsupported luaskills-packages layout '{}'",
            manifest.layout
        ));
    }

    validate_packaged_runtime_target(
        runtime_root,
        "install_manifest",
        &manifest.paths.install_manifest,
        PackagedRuntimeTargetKind::File,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "compat_lua_packages_txt",
        &manifest.paths.compat_lua_packages_txt,
        PackagedRuntimeTargetKind::File,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "platform_support",
        &manifest.paths.platform_support,
        PackagedRuntimeTargetKind::File,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "third_party_licenses",
        &manifest.paths.third_party_licenses,
        PackagedRuntimeTargetKind::File,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "third_party_notices",
        &manifest.paths.third_party_notices,
        PackagedRuntimeTargetKind::File,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "help_index",
        &manifest.paths.help_index,
        PackagedRuntimeTargetKind::File,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "package_help_root",
        &manifest.paths.package_help_root,
        PackagedRuntimeTargetKind::Directory,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "module_help_root",
        &manifest.paths.module_help_root,
        PackagedRuntimeTargetKind::Directory,
    )?;
    validate_packaged_runtime_target(
        runtime_root,
        "license_index",
        &manifest.paths.license_index,
        PackagedRuntimeTargetKind::File,
    )?;
    Ok(())
}

/// Pool sizing configuration for Lua virtual machines.
/// Lua 虚拟机池的容量配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LuaVmPoolConfig {
    /// Minimum number of VMs that should stay warm.
    /// 需要常驻保温的最小虚拟机数量。
    pub min_size: usize,
    /// Maximum number of VMs allowed in the pool.
    /// 池内允许存在的最大虚拟机数量。
    pub max_size: usize,
    /// Idle TTL in seconds before an excess VM can be retired.
    /// 多余虚拟机在空闲多少秒后允许回收。
    pub idle_ttl_secs: u64,
}

impl LuaVmPoolConfig {
    /// Return a normalized pool config with safe bounds.
    /// 返回经过安全边界归一化后的池配置。
    fn normalized(self) -> Self {
        let min_size = self.min_size.max(1);
        let max_size = self.max_size.max(min_size);
        let idle_ttl_secs = self.idle_ttl_secs.max(1);
        Self {
            min_size,
            max_size,
            idle_ttl_secs,
        }
    }
}

/// Return the default dedicated pool config used by isolated runlua execution.
/// 返回隔离 runlua 执行使用的默认独立池配置。
fn default_runlua_vm_pool_config() -> LuaVmPoolConfig {
    LuaVmPoolConfig {
        min_size: 1,
        max_size: 4,
        idle_ttl_secs: 60,
    }
}

/// Runtime state of a single Lua VM instance.
/// 单个 Lua 虚拟机实例的运行时状态。
struct LuaVm {
    lua: Lua,
    last_used_at: Instant,
}

/// Complete dependency context required to construct one isolated runlua-compatible VM.
/// 构造单个隔离且兼容 runlua 的 VM 所需的完整依赖上下文。
struct RunLuaVmBuildContext<'a> {
    /// Loaded Skill registry visible to the new VM.
    /// 新 VM 可见的已加载 Skill 注册表。
    skills: &'a HashMap<String, LoadedSkill>,
    /// Canonical entry registry visible to nested calls.
    /// 嵌套调用可见的规范入口注册表。
    entry_registry: &'a BTreeMap<String, ResolvedEntryTarget>,
    /// Normalized host options shared with runtime bridges.
    /// 与运行时桥接共享的规范化宿主选项。
    host_options: Arc<LuaRuntimeHostOptions>,
    /// Unified Skill configuration store installed into the runtime module.
    /// 安装到运行时模块的统一 Skill 配置存储。
    skill_config_store: Arc<SkillConfigStore>,
    /// Runtime Skill roots used for dependency context resolution.
    /// 用于依赖上下文解析的运行时 Skill 根。
    runtime_skill_roots: Vec<RuntimeSkillRoot>,
    /// Optional LanceDB host bridge visible to nested calls.
    /// 嵌套调用可见的可选 LanceDB 宿主桥接。
    lancedb_host: Option<Arc<LanceDbSkillHost>>,
    /// Optional SQLite host bridge visible to nested calls.
    /// 嵌套调用可见的可选 SQLite 宿主桥接。
    sqlite_host: Option<Arc<SqliteSkillHost>>,
    /// Engine-owned long-session, transaction, and event service.
    /// 引擎所有的长期会话、事务与事件服务。
    managed_runtime_services: Arc<ManagedRuntimeServices>,
    /// Engine-owned short-lived worker service.
    /// 引擎所有的短期 Worker 服务。
    managed_runtime_workers: Arc<ManagedRuntimeWorkerService>,
}

impl<'a> RunLuaVmBuildContext<'a> {
    /// Capture one VM build context from an engine and explicit visible registries.
    /// 从引擎及显式可见注册表捕获一个 VM 构建上下文。
    ///
    /// `engine` supplies shared services while `skills` and `entry_registry` define VM visibility.
    /// `engine` 提供共享服务，`skills` 与 `entry_registry` 定义 VM 可见性。
    ///
    /// Return one complete immutable construction context.
    /// 返回一个完整不可变构造上下文。
    fn from_engine(
        engine: &LuaEngine,
        skills: &'a HashMap<String, LoadedSkill>,
        entry_registry: &'a BTreeMap<String, ResolvedEntryTarget>,
    ) -> Self {
        Self {
            skills,
            entry_registry,
            host_options: Arc::clone(&engine.host_options),
            skill_config_store: Arc::clone(&engine.skill_config_store),
            runtime_skill_roots: engine.runtime_skill_roots.clone(),
            lancedb_host: engine.lancedb_host.clone(),
            sqlite_host: engine.sqlite_host.clone(),
            managed_runtime_services: Arc::clone(&engine.managed_runtime_services),
            managed_runtime_workers: Arc::clone(&engine.managed_runtime_workers),
        }
    }
}

/// Shared mutable state for the Lua VM pool.
/// Lua 虚拟机池的共享可变状态。
struct LuaVmPoolState {
    available: Vec<LuaVm>,
    total_count: usize,
}

/// Pool of Lua VM instances with opportunistic scaling.
/// 支持按需扩缩容的 Lua 虚拟机池。
struct LuaVmPool {
    config: LuaVmPoolConfig,
    state: Mutex<LuaVmPoolState>,
    condvar: Condvar,
}

/// Process-level native library search guard owned by one runtime engine.
/// 由单个运行时引擎持有的进程级原生库搜索保护句柄。
#[derive(Debug, Default)]
struct NativeLibrarySearchGuard {
    /// Registered native library directories kept alive for the lifetime of this engine.
    /// 在该引擎生命周期内保持有效的已注册原生库目录集合。
    #[cfg(windows)]
    directories: Vec<NativeLibraryDirectoryCookie>,
}

/// Windows DLL directory cookie returned by `AddDllDirectory`.
/// `AddDllDirectory` 返回的 Windows DLL 目录句柄。
#[cfg(windows)]
#[derive(Debug)]
struct NativeLibraryDirectoryCookie(*mut core::ffi::c_void);

#[cfg(windows)]
impl NativeLibraryDirectoryCookie {
    /// Return whether the cookie points at one registered DLL directory.
    /// 返回该句柄是否指向一个已注册的 DLL 目录。
    fn is_valid(&self) -> bool {
        !self.0.is_null()
    }
}

#[cfg(windows)]
unsafe impl Send for NativeLibraryDirectoryCookie {}

#[cfg(windows)]
unsafe impl Sync for NativeLibraryDirectoryCookie {}

#[cfg(windows)]
impl Drop for NativeLibrarySearchGuard {
    /// Drop registered DLL directory cookies before the guard is released.
    /// 在保护句柄释放前丢弃已注册的 DLL 目录句柄。
    fn drop(&mut self) {
        self.directories.clear();
    }
}

#[cfg(windows)]
impl Drop for NativeLibraryDirectoryCookie {
    /// Remove the registered Windows DLL directory when the owning engine is dropped.
    /// 当所属引擎释放时移除已注册的 Windows DLL 目录。
    fn drop(&mut self) {
        if self.is_valid() {
            unsafe {
                RemoveDllDirectory(self.0);
            }
        }
    }
}

impl NativeLibrarySearchGuard {
    /// Register the host-provided FFI/native library root for this process.
    /// 为当前进程注册宿主提供的 FFI/原生库根目录。
    fn new(host_options: &LuaRuntimeHostOptions) -> Result<Self, String> {
        #[cfg(windows)]
        {
            Self::new_windows(host_options)
        }
        #[cfg(not(windows))]
        {
            let _ = host_options;
            Ok(Self::default())
        }
    }

    /// Register Windows DLL search directories without mutating the global PATH variable.
    /// 在不修改全局 PATH 变量的前提下注册 Windows DLL 搜索目录。
    #[cfg(windows)]
    fn new_windows(host_options: &LuaRuntimeHostOptions) -> Result<Self, String> {
        let mut directories = Vec::new();
        let Some(host_provided_ffi_root) = host_options.host_provided_ffi_root.as_ref() else {
            return Ok(Self { directories });
        };
        // Host-provided FFI root status inspected before any process-wide DLL search mutation.
        // 在修改进程级 DLL 搜索设置前探测宿主提供的 FFI 根目录状态。
        let ffi_root_is_directory = host_provided_ffi_root_is_directory(host_provided_ffi_root)?;
        if !ffi_root_is_directory {
            return Ok(Self { directories });
        }

        // DefaultDllDirectories enables the USER_DIRS search bucket used by AddDllDirectory.
        // DefaultDllDirectories 启用 AddDllDirectory 所依赖的 USER_DIRS 搜索桶。
        let default_directory_result = unsafe {
            SetDefaultDllDirectories(
                LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS,
            )
        };
        if default_directory_result == 0 {
            return Err(format!(
                "failed to enable Windows DLL directory search for host_provided_ffi_root: {}",
                std::io::Error::last_os_error()
            ));
        }

        let wide_path = windows_wide_null_path(host_provided_ffi_root)?;
        let cookie = NativeLibraryDirectoryCookie(unsafe { AddDllDirectory(wide_path.as_ptr()) });
        if !cookie.is_valid() {
            return Err(format!(
                "failed to add Windows DLL directory {}: {}",
                render_log_friendly_path(host_provided_ffi_root),
                std::io::Error::last_os_error()
            ));
        }
        directories.push(cookie);
        Ok(Self { directories })
    }
}

/// Inspect whether the host-provided FFI root is a real directory without hiding metadata probe errors.
/// 检查宿主提供的 FFI 根目录是否为真实目录，同时不隐藏元数据探测错误。
///
/// The path parameter is the host-provided native-library directory configured by the host.
/// path 参数是宿主配置的原生库目录。
///
/// Return true for an existing directory, false for a missing or non-directory path, or an explicit probe error.
/// 已存在目录返回 true，缺失或非目录路径返回 false；探测失败时返回显式错误。
#[cfg(windows)]
fn host_provided_ffi_root_is_directory(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect host_provided_ffi_root {}: {}",
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Check whether one configured Lua package search directory is usable without hiding probe errors.
/// 检查单个已配置的 Lua 包搜索目录是否可用，同时不隐藏探测错误。
///
/// The path parameter is the host-provided directory that will be inserted into package search paths.
/// path 参数是即将插入 package 搜索路径的宿主提供目录。
///
/// The option_name parameter identifies the owning host option in diagnostics.
/// option_name 参数用于在诊断中标识所属宿主选项。
///
/// Return true for an existing directory, false for a missing directory, or an explicit configuration/probe error.
/// 已存在目录返回 true，缺失目录返回 false；配置无效或探测失败时返回显式错误。
fn configured_package_search_directory_exists(
    path: &Path,
    option_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "configured {option_name} is not a directory: {}",
            render_log_friendly_path(path)
        )
        .into()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect configured {option_name} {}: {}",
            render_log_friendly_path(path),
            error
        )
        .into()),
    }
}

/// Convert one Windows filesystem path into a null-terminated wide string.
/// 将单个 Windows 文件系统路径转换为以空字符结尾的宽字符串。
#[cfg(windows)]
fn windows_wide_null_path(path: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<u16>>();
    if wide_path.contains(&0) {
        return Err(format!(
            "Windows DLL directory contains an embedded NUL: {}",
            render_log_friendly_path(path)
        ));
    }
    wide_path.push(0);
    Ok(wide_path)
}

// ============================================================
// LuaEngine — LuaJIT VM wrapper
// ============================================================

pub struct LuaEngine {
    skills: HashMap<String, LoadedSkill>,
    entry_registry: BTreeMap<String, ResolvedEntryTarget>,
    runtime_skill_roots: Vec<RuntimeSkillRoot>,
    pool: Arc<LuaVmPool>,
    runlua_pool: Arc<LuaVmPool>,
    /// Public runtime-session manager replaced together with ordinary Skill state.
    /// 随普通 Skill 状态一同替换的公开运行时会话管理器。
    public_runtime_sessions: Arc<RuntimeSessionManager>,
    /// Dedicated System runtime-session manager preserved across ordinary Skill reloads.
    /// 在普通 Skill 重载期间保持不变的专用 System 运行时会话管理器。
    system_runtime_sessions: Arc<RuntimeSessionManager>,
    /// Engine-owned managed process, transaction, and event lifecycle service.
    /// 引擎拥有的受管进程、事务与事件生命周期服务。
    managed_runtime_services: Arc<ManagedRuntimeServices>,
    /// Engine-owned short-lived managed runtime worker service shared by every engine VM.
    /// 由每个引擎 VM 共享的引擎所有短期受管运行时 Worker 服务。
    managed_runtime_workers: Arc<ManagedRuntimeWorkerService>,
    skill_config_store: Arc<SkillConfigStore>,
    lancedb_host: Option<Arc<LanceDbSkillHost>>,
    sqlite_host: Option<Arc<SqliteSkillHost>>,
    database_provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    native_library_search_guard: NativeLibrarySearchGuard,
    host_options: Arc<LuaRuntimeHostOptions>,
}

/// Resolved runtime entry target produced after canonical-name collision indexing.
/// 经过 canonical 名称冲突编号后得到的运行时入口目标。
#[derive(Debug, Clone)]
struct ResolvedEntryTarget {
    /// Final canonical tool name exposed to hosts and Lua dispatch.
    /// 暴露给宿主和 Lua 分发器的最终 canonical 工具名。
    canonical_name: String,
    /// Internal storage key of the owning loaded skill.
    /// 所属已加载 skill 的内部存储键。
    skill_storage_key: String,
    /// Owning stable skill identifier declared in skill metadata.
    /// 在 skill 元数据中声明的所属稳定 skill 标识符。
    skill_id: String,
    /// Stable local entry name declared by the owning skill.
    /// 所属 skill 声明的稳定局部入口名称。
    local_name: String,
}

/// Construction options used by the host to create one LuaSkills runtime engine.
/// 宿主创建单个 LuaSkills 运行时引擎时使用的构造选项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LuaEngineOptions {
    /// Pool sizing configuration for reusable Lua virtual machines.
    /// 可复用 Lua 虚拟机池的容量配置。
    pub pool_config: LuaVmPoolConfig,
    /// Host-owned runtime paths and external library locations.
    /// 宿主拥有的运行时路径与外部动态库位置配置。
    pub host_options: LuaRuntimeHostOptions,
}

impl LuaEngineOptions {
    /// Build engine options from one pool config and one host option object.
    /// 基于一份虚拟机池配置和一份宿主选项对象构造引擎选项。
    pub fn new(pool_config: LuaVmPoolConfig, host_options: LuaRuntimeHostOptions) -> Self {
        Self {
            pool_config,
            host_options,
        }
    }
}

impl LoadedSkill {
    /// Return the resolved canonical entry name for one local entry name.
    /// 返回某个局部入口名称对应的已解析 canonical 名称。
    fn resolved_tool_name(&self, local_name: &str) -> Option<&str> {
        self.resolved_entry_names
            .get(local_name)
            .map(String::as_str)
    }
}

/// Return a stable human-readable Lua value type name.
/// 返回稳定且可读的 Lua 值类型名称。
fn lua_value_type_name(value: &LuaValue) -> &'static str {
    match value {
        LuaValue::Nil => "nil",
        LuaValue::Boolean(_) => "boolean",
        LuaValue::LightUserData(_) => "lightuserdata",
        LuaValue::Integer(_) => "integer",
        LuaValue::Number(_) => "number",
        LuaValue::String(_) => "string",
        LuaValue::Table(_) => "table",
        LuaValue::Function(_) => "function",
        LuaValue::Thread(_) => "thread",
        LuaValue::UserData(_) => "userdata",
        LuaValue::Error(_) => "error",
        LuaValue::Other(_) => "other",
    }
}

/// Render one Lua value as a log-safe `print` argument.
/// 将单个 Lua 值渲染为适合日志输出的 `print` 参数。
///
/// The value parameter is one argument passed to the runtime-provided global `print` function.
/// value 参数是传给运行时提供的全局 `print` 函数的单个参数。
///
/// Return a printable string, using an explicit diagnostic for invalid UTF-8 Lua strings.
/// 返回可打印字符串；对于非法 UTF-8 Lua 字符串返回显式诊断文本。
fn render_lua_print_argument(value: LuaValue) -> String {
    match value {
        LuaValue::String(value) => match value.to_str() {
            Ok(text) => text.to_string(),
            Err(error) => format!("<invalid UTF-8 Lua string: {error}>"),
        },
        LuaValue::Integer(value) => value.to_string(),
        LuaValue::Number(value) => value.to_string(),
        LuaValue::Boolean(value) => value.to_string(),
        LuaValue::Nil => "nil".to_string(),
        other => format!("{:?}", other),
    }
}

/// Read one optional `recursive` flag from one `vulcan.fs.*` options value.
/// 从单个 `vulcan.fs.*` 选项值中读取可选的 `recursive` 标志。
fn parse_vulcan_fs_recursive_option(value: LuaValue, fn_name: &str) -> mlua::Result<bool> {
    match value {
        LuaValue::Nil => Ok(false),
        other => {
            let options = require_table_arg(other, fn_name, "options")?;
            let recursive_value: LuaValue = options.get("recursive")?;
            match recursive_value {
                LuaValue::Nil => Ok(false),
                LuaValue::Boolean(flag) => Ok(flag),
                other => Err(mlua::Error::runtime(format!(
                    "{fn_name}: options.recursive must be a boolean when provided: {}",
                    lua_value_type_name(&other)
                ))),
            }
        }
    }
}

/// Read one optional `overwrite` flag from one `vulcan.fs.copy` options value.
/// 从单个 `vulcan.fs.copy` 选项值中读取可选的 `overwrite` 标志。
fn parse_vulcan_fs_overwrite_option(value: LuaValue, fn_name: &str) -> mlua::Result<bool> {
    match value {
        LuaValue::Nil => Ok(false),
        other => {
            let options = require_table_arg(other, fn_name, "options")?;
            let overwrite_value: LuaValue = options.get("overwrite")?;
            match overwrite_value {
                LuaValue::Nil => Ok(false),
                LuaValue::Boolean(flag) => Ok(flag),
                other => Err(mlua::Error::runtime(format!(
                    "{fn_name}: options.overwrite must be a boolean when provided: {}",
                    lua_value_type_name(&other)
                ))),
            }
        }
    }
}

/// Resolve one `vulcan.fs.copy` path into a normalized absolute path for relationship checks.
/// 将单个 `vulcan.fs.copy` 路径解析为归一化绝对路径，以便做关系校验。
fn resolve_vulcan_fs_copy_absolute_path(path: &Path) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| format!("fs.copy: {}", error))?;
    Ok(if path.is_absolute() {
        normalize_runtime_root_path(path)
    } else {
        normalize_runtime_root_path(&cwd.join(path))
    })
}

/// Resolve one `vulcan.fs.copy` destination into the effective absolute location created after existing parent links are followed.
/// 将单个 `vulcan.fs.copy` 目标解析为跟随现有父级链接后最终会创建到的实际绝对位置。
fn resolve_vulcan_fs_copy_effective_destination_path(
    target: &Path,
    recreate_leaf: bool,
) -> Result<PathBuf, String> {
    let absolute_target = resolve_vulcan_fs_copy_absolute_path(target)?;
    let mut suffix = Vec::<PathBuf>::new();
    let mut cursor = if recreate_leaf {
        let leaf_name = absolute_target.file_name().ok_or_else(|| {
            format!(
                "fs.copy: destination path must not be one filesystem root: {}",
                render_log_friendly_path(&absolute_target)
            )
        })?;
        suffix.push(PathBuf::from(leaf_name));
        absolute_target.parent().ok_or_else(|| {
            format!(
                "fs.copy: destination path must have one parent directory: {}",
                render_log_friendly_path(&absolute_target)
            )
        })?
    } else {
        absolute_target.as_path()
    };
    loop {
        // Existing ancestor probe that treats missing paths as traversal input and preserves other filesystem errors.
        // 已存在祖先探测会把缺失路径作为上溯输入，同时保留其它文件系统错误。
        let ancestor_exists = path_entry_exists(
            cursor,
            &format!(
                "fs.copy: failed to inspect destination ancestor {}",
                render_log_friendly_path(cursor)
            ),
        )?;
        if ancestor_exists {
            break;
        }
        let missing_name = cursor.file_name().ok_or_else(|| {
            format!(
                "fs.copy: destination path could not resolve one existing ancestor: {}",
                render_log_friendly_path(&absolute_target)
            )
        })?;
        suffix.push(PathBuf::from(missing_name));
        cursor = cursor.parent().ok_or_else(|| {
            format!(
                "fs.copy: destination path must stay under one existing filesystem root: {}",
                render_log_friendly_path(&absolute_target)
            )
        })?;
    }
    let mut resolved = fs::canonicalize(cursor).map_err(|error| format!("fs.copy: {}", error))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(normalize_runtime_root_path(&resolved))
}

/// Validate that one directory-copy destination is neither equal to nor nested under the source directory.
/// 校验单个目录复制目标既不等于源目录，也不位于源目录内部。
fn validate_vulcan_fs_copy_directory_target(
    source: &Path,
    target: &Path,
    recreate_target_leaf: bool,
) -> Result<(), String> {
    let resolved_source =
        fs::canonicalize(source).map_err(|error| format!("fs.copy: {}", error))?;
    let resolved_target =
        resolve_vulcan_fs_copy_effective_destination_path(target, recreate_target_leaf)?;
    if resolved_source == resolved_target {
        return Err(format!(
            "fs.copy: source and destination must differ: {}",
            render_log_friendly_path(&resolved_source)
        ));
    }
    if resolved_target.starts_with(&resolved_source) {
        return Err(format!(
            "fs.copy: destination directory must not be inside source directory: {}",
            render_log_friendly_path(&resolved_target)
        ));
    }
    Ok(())
}

/// Remove one existing `vulcan.fs.copy` destination so overwrite mode can replace it atomically.
/// 删除单个已存在的 `vulcan.fs.copy` 目标，以便 overwrite 模式能够整体替换它。
fn remove_vulcan_fs_copy_target(target: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(target).map_err(|error| format!("fs.copy: {}", error))?;
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        fs::remove_dir_all(target).map_err(|error| format!("fs.copy: {}", error))?;
    } else {
        fs::remove_file(target).map_err(|error| format!("fs.copy: {}", error))?;
    }
    Ok(())
}

/// Detect whether one filesystem entry exists at the path itself even when the target behind one symlink is missing.
/// 判断单个路径位置本身是否存在文件系统条目，即使其背后的符号链接目标已经缺失。
fn path_entry_exists(path: &Path, error_prefix: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{error_prefix}: {}", error)),
    }
}

/// Inspect whether one optional `dependencies.yaml` manifest is a file without hiding filesystem probe errors.
/// 检查单个可选 `dependencies.yaml` 清单是否为文件，同时不隐藏文件系统探测错误。
///
/// The dependencies_path parameter is the concrete dependency manifest path derived from one skill directory.
/// dependencies_path 参数是从单个 skill 目录派生出的具体依赖清单路径。
///
/// Return true for an existing manifest file, false for a confirmed missing manifest, or an explicit probe/configuration error.
/// 已存在清单文件返回 true，确认缺失清单返回 false；探测失败或配置无效时返回显式错误。
fn skill_dependency_manifest_path_exists(dependencies_path: &Path) -> Result<bool, String> {
    match fs::metadata(dependencies_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "dependency manifest is not a file: {}",
            render_log_friendly_path(dependencies_path)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect dependency manifest {}: {}",
            render_log_friendly_path(dependencies_path),
            error
        )),
    }
}

/// Inspect whether one required skill file exists without hiding filesystem probe errors.
/// 检查单个必需 skill 文件是否存在，同时不隐藏文件系统探测错误。
///
/// The path parameter is the concrete required file path derived from one skill directory.
/// path 参数是从单个 skill 目录派生出的具体必需文件路径。
///
/// The file_label parameter names the required file in user-facing diagnostics.
/// file_label 参数用于在面向用户的诊断中命名该必需文件。
///
/// The skill_dir parameter is the owning skill directory used to keep diagnostics actionable.
/// skill_dir 参数是所属 skill 目录，用于保持诊断可定位。
///
/// Return true for an existing regular file, false for a confirmed missing path, or an explicit probe/type error.
/// 已存在普通文件返回 true，确认缺失路径返回 false；探测或类型失败时返回显式错误。
fn required_skill_file_path_exists(
    path: &Path,
    file_label: &str,
    skill_dir: &Path,
) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "{} is not a file for skill {}: {}",
            file_label,
            render_log_friendly_path(skill_dir),
            render_log_friendly_path(path)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect {} {} for skill {}: {}",
            file_label,
            render_log_friendly_path(path),
            render_log_friendly_path(skill_dir),
            error
        )),
    }
}

/// Check whether one filesystem target exists without hiding metadata probe errors.
/// 检查单个文件系统目标是否存在，同时不隐藏元数据探测错误。
///
/// The path parameter is the target path whose resolved metadata should be inspected.
/// path 参数是需要检查已解析元数据的目标路径。
///
/// The api_name parameter identifies the Lua-facing filesystem API used in diagnostics.
/// api_name 参数用于在诊断中标识面向 Lua 的文件系统 API。
///
/// Return true when the resolved target exists, false when it is missing, or an explicit probe error.
/// 已解析目标存在时返回 true，目标缺失时返回 false；探测失败时返回显式错误。
fn vulcan_fs_target_exists(path: &Path, api_name: &str) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "{api_name}: failed to inspect {}: {}",
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Check whether one filesystem target is a directory without hiding metadata probe errors.
/// 检查单个文件系统目标是否为目录，同时不隐藏元数据探测错误。
///
/// The path parameter is the target path whose resolved metadata should be inspected.
/// path 参数是需要检查已解析元数据的目标路径。
///
/// The api_name parameter identifies the Lua-facing filesystem API used in diagnostics.
/// api_name 参数用于在诊断中标识面向 Lua 的文件系统 API。
///
/// Return true for a directory target, false for a missing or non-directory target, or an explicit probe error.
/// 目标为目录时返回 true，目标缺失或不是目录时返回 false；探测失败时返回显式错误。
fn vulcan_fs_target_is_dir(path: &Path, api_name: &str) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "{api_name}: failed to inspect {}: {}",
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Existing target state used before `vulcan.fs.mkdir` creates one directory.
/// `vulcan.fs.mkdir` 创建目录前使用的既有目标状态。
#[derive(Debug)]
enum VulcanFsMkdirTargetStatus {
    /// No filesystem entry exists at the requested mkdir target.
    /// 请求的 mkdir 目标位置不存在文件系统条目。
    Missing,
    /// The requested mkdir target already resolves to one directory.
    /// 请求的 mkdir 目标已经解析为一个目录。
    ExistingDirectory,
    /// The requested mkdir target exists but does not resolve to one directory.
    /// 请求的 mkdir 目标存在，但没有解析为目录。
    ExistingNonDirectory,
}

/// Inspect one `vulcan.fs.mkdir` target without hiding metadata probe errors.
/// 检查单个 `vulcan.fs.mkdir` 目标，同时不隐藏元数据探测错误。
///
/// The path parameter is the caller-supplied directory path that may already exist.
/// path 参数是调用方传入且可能已经存在的目录路径。
///
/// Return a precise target status, or an explicit probe error before any creation attempt.
/// 返回精确的目标状态；创建前探测失败时返回显式错误。
fn vulcan_fs_mkdir_target_status(path: &Path) -> Result<VulcanFsMkdirTargetStatus, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(VulcanFsMkdirTargetStatus::Missing);
        }
        Err(error) => {
            return Err(format!(
                "fs.mkdir: failed to inspect {}: {}",
                render_log_friendly_path(path),
                error
            ));
        }
    };

    if metadata.file_type().is_symlink() {
        return match fs::metadata(path) {
            Ok(target_metadata) if target_metadata.is_dir() => {
                Ok(VulcanFsMkdirTargetStatus::ExistingDirectory)
            }
            Ok(_) => Ok(VulcanFsMkdirTargetStatus::ExistingNonDirectory),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Ok(VulcanFsMkdirTargetStatus::ExistingNonDirectory)
            }
            Err(error) => Err(format!(
                "fs.mkdir: failed to inspect {}: {}",
                render_log_friendly_path(path),
                error
            )),
        };
    }

    if metadata.is_dir() {
        Ok(VulcanFsMkdirTargetStatus::ExistingDirectory)
    } else {
        Ok(VulcanFsMkdirTargetStatus::ExistingNonDirectory)
    }
}

/// Recursively copy one directory tree for `vulcan.fs.copy` while rejecting symbolic links for predictable behavior.
/// 为 `vulcan.fs.copy` 递归复制单个目录树，并拒绝符号链接以保证行为可预测。
fn copy_vulcan_fs_directory_recursive(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|error| format!("fs.copy: {}", error))?;
    for entry in fs::read_dir(source).map_err(|error| format!("fs.copy: {}", error))? {
        let entry = entry.map_err(|error| format!("fs.copy: {}", error))?;
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("fs.copy: {}", error))?;
        let destination = target.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(format!(
                "fs.copy: symbolic-link entries are not supported inside directory trees: {}",
                render_log_friendly_path(&entry_path)
            ));
        }
        if file_type.is_dir() {
            copy_vulcan_fs_directory_recursive(&entry_path, &destination)?;
        } else if file_type.is_file() {
            fs::copy(&entry_path, &destination).map_err(|error| format!("fs.copy: {}", error))?;
        } else {
            return Err(format!(
                "fs.copy: unsupported entry type inside directory tree: {}",
                render_log_friendly_path(&entry_path)
            ));
        }
    }
    Ok(())
}

/// Classify one filesystem type into the stable `vulcan.fs.stat` kind strings.
/// 将单个文件系统类型归类为稳定的 `vulcan.fs.stat` kind 字符串。
fn classify_vulcan_fs_kind(file_type: &fs::FileType) -> &'static str {
    if file_type.is_file() {
        "file"
    } else if file_type.is_dir() {
        "dir"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

/// Convert one system time into a Lua-safe Unix millisecond timestamp.
/// 将单个系统时间转换为 Lua 可安全表示的 Unix 毫秒时间戳。
///
/// The time parameter is the filesystem timestamp that must be represented.
/// time 参数是需要表示的文件系统时间戳。
///
/// The context parameter names the caller and data source for error diagnostics.
/// context 参数命名调用方与数据来源，用于错误诊断。
///
/// Returns the Unix millisecond timestamp, saturated to i64::MAX when it exceeds Lua's integer-safe range.
/// 返回 Unix 毫秒时间戳；当超过 Lua 整数安全范围时饱和为 i64::MAX。
fn system_time_to_unix_millis_i64(time: SystemTime, context: &str) -> Result<i64, String> {
    // Duration measured from the Unix epoch for a representable timestamp.
    // 可表示时间戳相对于 Unix epoch 的持续时间。
    let duration = time.duration_since(UNIX_EPOCH).map_err(|error| {
        format!(
            "{} is before Unix epoch and cannot be represented as modified_unix_ms: {}",
            context, error
        )
    })?;
    // Millisecond value before clamping into the Lua-facing integer type.
    // 转换为面向 Lua 的整数类型之前的毫秒值。
    let millis = duration.as_millis();
    Ok(if millis > i64::MAX as u128 {
        i64::MAX
    } else {
        millis as i64
    })
}

/// Convert one metadata modified time into Unix milliseconds with explicit failure context.
/// 将单个元数据修改时间转换为 Unix 毫秒，并提供显式失败上下文。
///
/// The metadata parameter is the filesystem metadata returned by `fs.stat`.
/// metadata 参数是 `fs.stat` 返回的文件系统元数据。
///
/// The path parameter is the caller-supplied stat target used in diagnostics.
/// path 参数是调用方传入的 stat 目标路径，用于诊断。
///
/// Returns the modified timestamp in Unix milliseconds.
/// 返回 Unix 毫秒形式的修改时间戳。
fn metadata_modified_unix_ms(metadata: &fs::Metadata, path: &Path) -> Result<i64, String> {
    // Rendered stat target path used to keep filesystem errors actionable.
    // 渲染后的 stat 目标路径，用于保持文件系统错误可定位。
    let path_label = render_log_friendly_path(path);
    // Raw modified timestamp read from the filesystem metadata.
    // 从文件系统元数据读取到的原始修改时间戳。
    let modified = metadata.modified().map_err(|error| {
        format!(
            "fs.stat: failed to read modified time for {}: {}",
            path_label, error
        )
    })?;
    system_time_to_unix_millis_i64(
        modified,
        &format!("fs.stat modified time for {}", path_label),
    )
}

/// Build one Lua table for the current `vulcan.fs.stat` metadata snapshot.
/// 为当前 `vulcan.fs.stat` 元数据快照构造一个 Lua table。
///
/// The lua parameter is the Lua VM used to allocate the result table.
/// lua 参数是用于分配结果 table 的 Lua 虚拟机。
///
/// The metadata parameter is the filesystem metadata snapshot to expose.
/// metadata 参数是需要暴露的文件系统元数据快照。
///
/// The path parameter is the original stat target used for timestamp diagnostics.
/// path 参数是原始 stat 目标路径，用于时间戳诊断。
///
/// Returns a Lua table containing the structured stat fields.
/// 返回包含结构化 stat 字段的 Lua table。
fn create_vulcan_fs_stat_table(
    lua: &Lua,
    metadata: &fs::Metadata,
    path: &Path,
) -> mlua::Result<Table> {
    let file_type = metadata.file_type();
    let stat = lua.create_table()?;
    stat.set("kind", classify_vulcan_fs_kind(&file_type))?;
    stat.set("is_file", file_type.is_file())?;
    stat.set("is_dir", file_type.is_dir())?;
    stat.set("is_symlink", file_type.is_symlink())?;
    stat.set("readonly", metadata.permissions().readonly())?;
    if file_type.is_file() {
        stat.set("size", metadata.len())?;
    }
    // Required modified timestamp exposed by the public `vulcan.fs.stat` table.
    // 公开 `vulcan.fs.stat` 表必须暴露的修改时间戳。
    let modified_unix_ms =
        metadata_modified_unix_ms(metadata, path).map_err(mlua::Error::runtime)?;
    stat.set("modified_unix_ms", modified_unix_ms)?;
    Ok(stat)
}

/// Format one `vulcan.fs.list` error for a directory entry whose file name is not UTF-8.
/// 为文件名不是 UTF-8 的单个目录项格式化 `vulcan.fs.list` 错误。
///
/// Parameters: `dir` is the directory being listed, and `name` is the original platform file name.
/// 参数：`dir` 是正在列举的目录，`name` 是原始平台文件名。
///
/// Returns: a Lua-facing diagnostic string with the directory rendered through the host-visible path formatter.
/// 返回：一条面向 Lua 的诊断字符串，其中目录会通过宿主可见路径渲染器输出。
fn format_vulcan_fs_list_non_utf8_file_name_error(dir: &Path, name: &OsStr) -> String {
    format!(
        "fs.list: non-UTF-8 file name under {}: {:?}",
        render_log_friendly_path(dir),
        name
    )
}

/// Render one `vulcan.path.dirname` result with script-friendly fallback semantics.
/// 以适合脚本使用的兜底语义渲染单个 `vulcan.path.dirname` 结果。
fn render_vulcan_path_dirname(path: &Path) -> String {
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => ".".to_string(),
        Some(parent) => render_host_visible_path(parent),
        None if path.is_absolute() => render_host_visible_path(path),
        None => ".".to_string(),
    }
}

/// Render one normalized path string for `vulcan.path.normalize`.
/// 为 `vulcan.path.normalize` 渲染单个规范化后的路径字符串。
fn render_vulcan_normalized_path(path: &Path) -> String {
    let normalized = normalize_runtime_root_path(path);
    if normalized.as_os_str().is_empty() {
        ".".to_string()
    } else {
        render_host_visible_path(&normalized)
    }
}

/// Render one optional UTF-8 path component for Lua-facing `vulcan.path` helpers.
/// 为面向 Lua 的 `vulcan.path` 辅助函数渲染单个可选 UTF-8 路径片段。
///
/// The component parameter is the OS path component returned by std::path APIs.
/// component 参数是 std::path API 返回的 OS 路径片段。
///
/// The api_name parameter identifies the Lua helper used in runtime errors.
/// api_name 参数用于在运行时错误中标识 Lua 辅助函数。
///
/// Return the component text, an empty string for missing components, or an explicit UTF-8 error.
/// 返回片段文本；片段不存在时返回空字符串；无法表示为 UTF-8 时返回显式错误。
fn render_vulcan_path_component(component: Option<&OsStr>, api_name: &str) -> mlua::Result<String> {
    let Some(component) = component else {
        return Ok(String::new());
    };
    component.to_str().map(str::to_string).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "{api_name}: path component cannot be represented as UTF-8"
        ))
    })
}

/// Detect whether one `vulcan.process.which` input should be treated as an explicit path.
/// 判断单个 `vulcan.process.which` 输入是否应按显式路径处理。
fn is_vulcan_process_explicit_path(program: &str) -> bool {
    Path::new(program).is_absolute() || program.contains('/') || program.contains('\\')
}

/// Resolve one possibly relative process-search path against the current working directory.
/// 将单个可能为相对路径的进程搜索路径相对于当前工作目录解析出来。
fn resolve_vulcan_process_search_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_runtime_root_path(path)
    } else {
        normalize_runtime_root_path(&cwd.join(path))
    }
}

/// Return the default ordered PATHEXT list used when Windows PATHEXT is absent.
/// 返回 Windows PATHEXT 缺失时使用的默认有序扩展列表。
///
/// Returns the default executable extension list for Windows process lookup.
/// 返回 Windows 进程查找使用的默认可执行扩展名列表。
#[cfg(windows)]
fn default_vulcan_process_windows_pathexts() -> Vec<String> {
    vec![
        ".com".to_string(),
        ".exe".to_string(),
        ".bat".to_string(),
        ".cmd".to_string(),
    ]
}

/// Parse one PATHEXT environment value into normalized Windows executable extensions.
/// 将单个 PATHEXT 环境变量值解析为规范化后的 Windows 可执行扩展名列表。
///
/// The value parameter is the concrete PATHEXT value read from the process environment.
/// value 参数是从进程环境读取到的具体 PATHEXT 值。
///
/// Returns the normalized extension list represented by the provided environment value.
/// 返回由给定环境变量值表示的规范化扩展名列表。
#[cfg(windows)]
fn parse_vulcan_process_windows_pathexts(value: &str) -> Vec<String> {
    value
        .split(';')
        .filter_map(|entry| {
            // PATHEXT entry after trimming user-provided separators and surrounding whitespace.
            // 去除用户提供的分隔符与外围空白后的 PATHEXT 条目。
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.starts_with('.') {
                Some(trimmed.to_ascii_lowercase())
            } else {
                Some(format!(".{}", trimmed).to_ascii_lowercase())
            }
        })
        .collect::<Vec<_>>()
}

/// Return the ordered PATHEXT list used by Windows process lookup.
/// 返回 Windows 进程查找使用的有序 PATHEXT 列表。
///
/// Returns the parsed PATHEXT list, or the Windows default list when PATHEXT is absent.
/// 返回解析后的 PATHEXT 列表；当 PATHEXT 不存在时返回 Windows 默认列表。
///
/// Returns an error when PATHEXT exists but cannot be represented as UTF-8.
/// 当 PATHEXT 存在但无法表示为 UTF-8 时返回错误。
#[cfg(windows)]
fn vulcan_process_windows_pathexts() -> Result<Vec<String>, String> {
    match std::env::var("PATHEXT") {
        Ok(value) => Ok(parse_vulcan_process_windows_pathexts(&value)),
        Err(std::env::VarError::NotPresent) => Ok(default_vulcan_process_windows_pathexts()),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("process.which: PATHEXT cannot be represented as UTF-8".to_string())
        }
    }
}

/// Expand one process-search base path into platform-specific executable candidates.
/// 将单个进程搜索基路径展开为平台相关的可执行候选路径列表。
///
/// The base parameter is the executable path before PATHEXT expansion.
/// base 参数是执行 PATHEXT 扩展之前的可执行文件路径。
///
/// Returns the ordered candidate path list for executable probing.
/// 返回用于可执行文件探测的有序候选路径列表。
///
/// Returns an error when the process executable environment cannot be parsed.
/// 当进程可执行文件环境无法解析时返回错误。
#[cfg(windows)]
fn vulcan_process_candidate_paths(base: &Path) -> Result<Vec<PathBuf>, String> {
    // Candidate path list that always preserves the original base path before PATHEXT expansion.
    // 候选路径列表，在 PATHEXT 扩展前始终保留原始基础路径。
    let mut candidates = vec![base.to_path_buf()];
    if base.extension().is_some() {
        return Ok(candidates);
    }
    for ext in vulcan_process_windows_pathexts()? {
        // Append PATHEXT as an OS string so non-UTF paths remain intact during lookup.
        // 以 OS 字符串追加 PATHEXT，避免在查找过程中破坏非 UTF 路径。
        let mut candidate = base.as_os_str().to_os_string();
        candidate.push(ext);
        candidates.push(PathBuf::from(candidate));
    }
    Ok(candidates)
}

/// Expand one process-search base path into platform-specific executable candidates.
/// 将单个进程搜索基路径展开为平台相关的可执行候选路径列表。
///
/// The base parameter is the executable path used directly on non-Windows hosts.
/// base 参数是在非 Windows 宿主上直接使用的可执行文件路径。
///
/// Returns the single candidate path used for executable probing.
/// 返回用于可执行文件探测的唯一候选路径。
#[cfg(not(windows))]
fn vulcan_process_candidate_paths(base: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(vec![base.to_path_buf()])
}

/// Check whether one candidate path is executable on the current platform.
/// 检查单个候选路径在当前平台上是否可执行。
#[cfg(unix)]
fn is_vulcan_process_executable(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && (metadata.permissions().mode() & 0o111) != 0),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "process.which: failed to inspect executable candidate {}: {}",
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Check whether one candidate path is executable on the current platform.
/// 检查单个候选路径在当前平台上是否可执行。
#[cfg(windows)]
fn is_vulcan_process_executable(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "process.which: failed to inspect executable candidate {}: {}",
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Check whether one candidate path is executable on the current platform.
/// 检查单个候选路径在当前平台上是否可执行。
#[cfg(not(any(unix, windows)))]
fn is_vulcan_process_executable(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "process.which: failed to inspect executable candidate {}: {}",
            render_log_friendly_path(path),
            error
        )),
    }
}

/// Find the first executable candidate derived from one base process path.
/// 从单个基础进程路径派生的候选项中找出第一个可执行目标。
///
/// The base parameter is the executable path before platform candidate expansion.
/// base 参数是执行平台候选扩展之前的可执行文件路径。
///
/// Returns the first executable candidate when one exists.
/// 当存在可执行候选路径时返回第一个命中的候选路径。
///
/// Returns an error when the process executable environment cannot be parsed.
/// 当进程可执行文件环境无法解析时返回错误。
fn find_vulcan_process_candidate(base: &Path) -> Result<Option<PathBuf>, String> {
    for candidate in vulcan_process_candidate_paths(base)? {
        // Candidate metadata probe that keeps missing files as lookup misses while surfacing other filesystem errors.
        // 候选元数据探测会把缺失文件保留为查找未命中，同时暴露其它文件系统错误。
        if is_vulcan_process_executable(&candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// Resolve one program name into a host-visible executable path using PATH-like lookup semantics.
/// 使用类 PATH 的查找语义，将单个程序名解析为宿主可见的可执行路径。
///
/// The program parameter is the command name or explicit path requested by `vulcan.process.which`.
/// program 参数是 `vulcan.process.which` 请求的命令名或显式路径。
///
/// Returns the first host-visible executable path that matches the request.
/// 返回第一个匹配请求的宿主可见可执行路径。
///
/// Returns an error when the current directory or executable lookup environment cannot be read.
/// 当当前目录或可执行查找环境无法读取时返回错误。
fn resolve_vulcan_process_which(program: &str) -> Result<Option<PathBuf>, String> {
    // Current working directory used to resolve relative explicit paths and relative PATH entries.
    // 用于解析相对显式路径与相对 PATH 条目的当前工作目录。
    let cwd = std::env::current_dir().map_err(|error| format!("process.which: {}", error))?;
    if is_vulcan_process_explicit_path(program) {
        // Explicit program path resolved against the current working directory when needed.
        // 必要时基于当前工作目录解析得到的显式程序路径。
        let explicit_path = resolve_vulcan_process_search_path(Path::new(program), &cwd);
        return find_vulcan_process_candidate(&explicit_path);
    }
    // Host PATH environment used for command-name lookup.
    // 用于命令名查找的宿主 PATH 环境变量。
    let Some(path_env) = std::env::var_os("PATH") else {
        return Ok(None);
    };
    // Search each PATH entry in order so the first executable match preserves host lookup precedence.
    // 按顺序搜索每个 PATH 条目，确保第一个可执行命中保留宿主查找优先级。
    for search_dir in std::env::split_paths(&path_env) {
        // Search directory resolved against the current working directory when the PATH entry is relative.
        // 当 PATH 条目为相对路径时，基于当前工作目录解析得到的搜索目录。
        let resolved_dir = resolve_vulcan_process_search_path(&search_dir, &cwd);
        // Candidate base path before platform-specific executable suffix expansion.
        // 执行平台相关可执行后缀扩展之前的候选基础路径。
        let base = resolved_dir.join(program);
        if let Some(found) = find_vulcan_process_candidate(&base)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Validate one relative metadata path against a fixed prefix and reject traversal.
/// 按固定目录前缀校验单个 skill 元数据相对路径，并拒绝路径穿越。
fn validate_skill_relative_path(
    relative_path: &str,
    expected_prefix: &str,
    field_label: &str,
) -> Result<(), String> {
    let trimmed = relative_path.trim();
    if trimmed.is_empty() {
        return Err(format!("{field_label} must not be empty"));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "{field_label} must be a relative path under {expected_prefix}"
        ));
    }

    let normalized = trimmed.replace('\\', "/");
    let required_prefix = format!("{expected_prefix}/");
    if !normalized.starts_with(&required_prefix) {
        return Err(format!("{field_label} must start with {required_prefix}"));
    }

    for component in path.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(format!("{field_label} must not contain parent"));
        }
    }

    Ok(())
}

/// Validate one discovered skill directory name against the strict LuaSkills rule.
/// 按严格 LuaSkills 规则校验一个被发现的 skill 目录名。
/// Build the absolute Lua entry file path for a tool.
/// 构建工具 Lua 入口文件的绝对路径。
fn tool_entry_path(skill_dir: &Path, tool: &crate::lua_skill::SkillToolMeta) -> PathBuf {
    skill_dir.join(&tool.lua_entry)
}

/// Build the private Lua global name used to store one compiled skill handler.
/// 构建用于存储单个已编译 skill handler 的私有 Lua 全局名称。
///
/// The module_name parameter is the manifest-declared Lua module name for the entry.
/// module_name 参数是 manifest 中声明的入口 Lua 模块名。
///
/// Return the shared global key used by skill registration and invocation.
/// 返回 skill 注册与调用共同使用的全局键。
fn lua_skill_handler_global_name(module_name: &str) -> String {
    format!("__skill_{}", module_name)
}

/// Resolve one compiled skill handler from Lua globals by module name.
/// 按模块名从 Lua globals 中解析一个已编译的 skill handler。
///
/// The lua parameter is the VM that owns the compiled skill handlers.
/// lua 参数是持有已编译 skill handler 的虚拟机。
///
/// The module_name parameter is the manifest-declared Lua module name for the entry.
/// module_name 参数是 manifest 中声明的入口 Lua 模块名。
///
/// Return the registered handler function, or the underlying Lua lookup error.
/// 返回已注册的 handler 函数，或底层 Lua 查找错误。
fn resolve_lua_skill_handler(lua: &Lua, module_name: &str) -> mlua::Result<Function> {
    lua.globals()
        .get(lua_skill_handler_global_name(module_name))
}

/// Lua-side invocation inputs prepared for one public `call_skill` request.
/// 为一次公开 `call_skill` 请求准备好的 Lua 侧调用输入。
struct CallSkillLuaInvocationInput {
    /// Compiled Lua handler resolved from the target module name.
    /// 通过目标模块名解析出的已编译 Lua handler。
    handler: Function,

    /// Lua table converted from the caller JSON arguments.
    /// 由调用方 JSON 参数转换得到的 Lua 表。
    args_table: Table,
}

/// Prepare the Lua handler and Lua argument table for one public `call_skill` request.
/// 为一次公开 `call_skill` 请求准备 Lua handler 与 Lua 参数表。
///
/// The lua parameter is the scoped Lua VM that owns compiled handlers and conversion state.
/// lua 参数是持有已编译 handler 与转换状态的作用域内 Lua VM。
///
/// The module_name parameter is the manifest-declared Lua module name for the target entry.
/// module_name 参数是目标入口在 manifest 中声明的 Lua 模块名。
///
/// The args parameter contains the caller-provided JSON arguments.
/// args 参数包含调用方提供的 JSON 参数。
///
/// Return the resolved handler and converted argument table for direct invocation.
/// 返回可直接调用的已解析 handler 与已转换参数表。
fn prepare_call_skill_lua_invocation_input(
    lua: &Lua,
    module_name: &str,
    args: &Value,
) -> Result<CallSkillLuaInvocationInput, String> {
    let handler = resolve_lua_skill_handler(lua, module_name)
        .map_err(|error| format!("Skill function '{}' not found: {}", module_name, error))?;
    let args_table = json_to_lua_table(lua, args)?;

    Ok(CallSkillLuaInvocationInput {
        handler,
        args_table,
    })
}

/// Invoke one loaded Lua skill handler and parse its host-visible output.
/// 调用一个已加载的 Lua skill handler，并解析其宿主可见输出。
///
/// The handler parameter is the compiled Lua function registered for the target skill entry.
/// handler 参数是为目标 skill 入口注册的已编译 Lua 函数。
///
/// The args_table parameter is the Lua table converted from the caller JSON arguments.
/// args_table 参数是由调用方 JSON 参数转换得到的 Lua 表。
///
/// The display_tool_name parameter is the canonical tool name used in user-facing diagnostics.
/// display_tool_name 参数是用于面向用户诊断信息的 canonical 工具名。
///
/// The invocation_context parameter is the optional request context used for output parsing.
/// invocation_context 参数是用于输出解析的可选请求上下文。
///
/// Return the parsed runtime invocation result after the Lua handler succeeds.
/// 在 Lua handler 成功后返回已解析的运行时调用结果。
fn invoke_loaded_lua_skill_handler(
    handler: Function,
    args_table: Table,
    display_tool_name: &str,
    invocation_context: Option<&LuaInvocationContext>,
) -> Result<RuntimeInvocationResult, String> {
    let result: MultiValue = handler.call(args_table).map_err(|error| {
        let message = format!("Lua skill '{}' error: {}", display_tool_name, error);
        log_error(format!("[LuaSkill:error] {}", message));
        message
    })?;

    parse_tool_call_output(result, display_tool_name, invocation_context).map_err(|error| {
        log_error(format!("[LuaSkill:error] {}", error));
        error
    })
}

/// Resolved dispatcher metadata for one strict LuaSkills entry.
/// 单个严格 LuaSkills 入口的已解析分发元数据。
#[derive(Clone)]
struct LuaCallDispatchEntry {
    /// Canonical display name used as the active tool name.
    /// 作为当前活动工具名使用的 canonical 显示名称。
    display_name: String,
    /// Lua module name registered in the VM globals.
    /// 注册到虚拟机全局表中的 Lua 模块名。
    module_name: String,
    /// Owning skill id of the current entry.
    /// 当前入口所属的 skill id。
    owner_skill_id: String,
    /// Stable local entry name declared by the owning skill.
    /// 所属 skill 声明的稳定局部入口名称。
    local_name: String,
    /// Runtime root name that owns the current entry.
    /// 拥有当前入口的运行时 root 名称。
    root_name: String,
    /// Owning skill directory used to restore file context.
    /// 用于恢复文件上下文的所属 skill 目录。
    owner_skill_dir: PathBuf,
    /// Trusted managed package context owned by the target Skill.
    /// 目标 Skill 拥有的可信受管包上下文。
    owner_managed_package: Arc<ManagedRuntimePackageContext>,
    /// Absolute entry file path used to restore file context.
    /// 用于恢复文件上下文的绝对入口文件路径。
    entry_path: PathBuf,
}

/// Provider bindings resolved for one nested `vulcan.call` target.
/// 为单个嵌套 `vulcan.call` 目标解析出的 provider binding。
struct LuaCallProviderBindings {
    /// LanceDB binding resolved for the nested target owner skill.
    /// 为嵌套目标所属 skill 解析出的 LanceDB binding。
    lancedb: Option<Arc<LanceDbSkillBinding>>,

    /// SQLite binding resolved for the nested target owner skill.
    /// 为嵌套目标所属 skill 解析出的 SQLite binding。
    sqlite: Option<Arc<SqliteSkillBinding>>,
}

impl LuaCallDispatchEntry {
    /// Reject nested `vulcan.call` targets that are unsafe while luaexec is active.
    /// 拒绝 luaexec 激活期间不安全的嵌套 `vulcan.call` 目标。
    ///
    /// The outer_internal_context parameter is the execution context captured before entering the nested target.
    /// outer_internal_context 参数是进入嵌套目标前捕获的外层执行上下文。
    ///
    /// Return `Ok(())` when this entry is allowed for the current outer context.
    /// 当前外层上下文允许调用该入口时返回 `Ok(())`。
    fn reject_forbidden_luaexec_call(
        &self,
        outer_internal_context: &VulcanInternalExecutionContext,
    ) -> mlua::Result<()> {
        if !outer_internal_context.luaexec_active {
            return Ok(());
        }
        if outer_internal_context.luaexec_caller_tool_name.as_deref()
            == Some(self.display_name.as_str())
        {
            return Err(mlua::Error::runtime(format!(
                "vulcan.call cannot call the current luaexec caller tool '{}'",
                self.display_name
            )));
        }
        if self.owner_skill_id == "vulcan-runtime"
            && (self.local_name == "lua-exec" || self.local_name == "lua-file")
        {
            return Err(mlua::Error::runtime(format!(
                "vulcan.call cannot invoke '{}' inside luaexec",
                self.display_name
            )));
        }
        Ok(())
    }
}

/// Resolve provider bindings for one nested `vulcan.call` target.
/// 为单个嵌套 `vulcan.call` 目标解析 provider binding。
///
/// The owner_skill_name parameter is the skill id that owns the nested target.
/// owner_skill_name 参数是拥有嵌套目标的 skill id。
///
/// The lancedb_host parameter is the optional shared LanceDB host.
/// lancedb_host 参数是可选的共享 LanceDB 宿主。
///
/// The sqlite_host parameter is the optional shared SQLite host.
/// sqlite_host 参数是可选的共享 SQLite 宿主。
///
/// Return both provider bindings that should be installed before entering the nested target.
/// 返回进入嵌套目标前应安装的两个 provider binding。
fn resolve_lua_call_provider_bindings(
    owner_skill_name: &str,
    lancedb_host: Option<&Arc<LanceDbSkillHost>>,
    sqlite_host: Option<&Arc<SqliteSkillHost>>,
) -> Result<LuaCallProviderBindings, String> {
    let lancedb = match lancedb_host {
        Some(host) => host.binding_for_skill(owner_skill_name)?,
        None => None,
    };
    let sqlite = match sqlite_host {
        Some(host) => host.binding_for_skill(owner_skill_name)?,
        None => None,
    };

    Ok(LuaCallProviderBindings { lancedb, sqlite })
}

/// Build the immutable `vulcan.call` dispatcher entries from the loaded runtime registry.
/// 根据已加载运行时注册表构建不可变的 `vulcan.call` 分发入口。
///
/// The skills_map parameter contains loaded skill metadata keyed by runtime storage id.
/// skills_map 参数包含按运行时存储 id 索引的已加载 skill 元数据。
///
/// The entry_registry parameter contains canonical entry targets exposed to callers.
/// entry_registry 参数包含对调用方暴露的 canonical 入口目标。
///
/// Return dispatcher entries that can be moved into the Lua closure, or an explicit registry consistency error.
/// 返回可移动到 Lua 闭包中的分发入口；当注册表不一致时返回显式错误。
fn build_lua_call_dispatch_entries(
    skills_map: &HashMap<String, LoadedSkill>,
    entry_registry: &BTreeMap<String, ResolvedEntryTarget>,
) -> Result<Vec<LuaCallDispatchEntry>, String> {
    // Dispatcher entries collected in registry order so nested calls preserve canonical name ordering.
    // 按注册表顺序收集的分发入口，确保嵌套调用保持 canonical 名称顺序。
    let mut dispatch_entries = Vec::with_capacity(entry_registry.len());
    for target in entry_registry.values() {
        // Loaded skill that must still own the resolved registry target.
        // 必须仍然拥有已解析注册表目标的已加载 skill。
        let skill = skills_map.get(&target.skill_storage_key).ok_or_else(|| {
            format!(
                "vulcan.call registry target '{}' references missing loaded skill storage key '{}'",
                target.canonical_name, target.skill_storage_key
            )
        })?;
        // Local tool entry that must still exist on the owning skill metadata.
        // 必须仍然存在于所属 skill 元数据中的局部工具入口。
        let tool = skill
            .meta
            .find_tool_by_local_name(&target.local_name)
            .ok_or_else(|| {
                format!(
                    "vulcan.call registry target '{}' references missing local entry '{}' in skill '{}'",
                    target.canonical_name, target.local_name, target.skill_id
                )
            })?;
        // Resolved Lua entry path used when entering nested call context.
        // 进入嵌套调用上下文时使用的已解析 Lua 入口路径。
        let entry_path = tool_entry_path(&skill.dir, tool);
        dispatch_entries.push(LuaCallDispatchEntry {
            display_name: target.canonical_name.clone(),
            module_name: tool.lua_module.clone(),
            owner_skill_id: target.skill_id.clone(),
            local_name: target.local_name.clone(),
            root_name: skill.root_name.clone(),
            owner_skill_dir: skill.dir.clone(),
            owner_managed_package: skill.managed_package.clone(),
            entry_path,
        });
    }
    Ok(dispatch_entries)
}

/// Resolve one `vulcan.call` dispatch entry by its canonical display name.
/// 通过 canonical 展示名称解析一个 `vulcan.call` 分发入口。
///
/// The dispatch_entries parameter contains immutable entries built from the runtime registry.
/// dispatch_entries 参数包含从运行时 registry 构建出的不可变入口。
///
/// The name parameter is the Lua caller provided skill name.
/// name 参数是 Lua 调用方提供的 skill 名称。
///
/// Return the matching dispatch entry or the runtime error reported to Lua callers.
/// 返回匹配的分发入口，或返回报告给 Lua 调用方的运行时错误。
fn resolve_lua_call_dispatch_entry<'a>(
    dispatch_entries: &'a [LuaCallDispatchEntry],
    name: &str,
) -> mlua::Result<&'a LuaCallDispatchEntry> {
    dispatch_entries
        .iter()
        .find(|entry| entry.display_name == name)
        .ok_or_else(|| mlua::Error::runtime(format!("Skill '{}' not found", name)))
}

/// Resolve the Lua handler used by one nested `vulcan.call` dispatch entry.
/// 解析单个嵌套 `vulcan.call` 分发入口使用的 Lua handler。
///
/// The lua parameter is the VM that owns compiled skill handlers.
/// lua 参数是持有已编译 skill handler 的虚拟机。
///
/// The dispatch_entry parameter is the already resolved `vulcan.call` target metadata.
/// dispatch_entry 参数是已解析的 `vulcan.call` 目标元数据。
///
/// Return the compiled handler or the Lua runtime error reported to nested callers.
/// 返回已编译 handler，或返回报告给嵌套调用方的 Lua runtime error。
fn resolve_lua_call_dispatch_handler(
    lua: &Lua,
    dispatch_entry: &LuaCallDispatchEntry,
) -> mlua::Result<Function> {
    let module = &dispatch_entry.module_name;
    resolve_lua_skill_handler(lua, module)
        .map_err(|_| mlua::Error::runtime(format!("Skill function '{}' not found", module)))
}

/// Internal per-VM Vulcan execution markers used for tool dispatch guards.
/// 用于工具分发保护的每个虚拟机内部 Vulcan 执行标记。
#[derive(Debug, Clone, Default)]
struct VulcanInternalExecutionContext {
    /// Current tool name executing inside this Lua VM.
    /// 当前 Lua 虚拟机内正在执行的工具名称。
    tool_name: Option<String>,
    /// Current owner skill name executing inside this Lua VM.
    /// 当前 Lua 虚拟机内正在执行的所属 skill 名称。
    skill_name: Option<String>,
    /// Current local entry name executing inside this Lua VM.
    /// 当前 Lua 虚拟机内正在执行的局部入口名称。
    entry_name: Option<String>,
    /// Current runtime root name that owns the executing skill.
    /// 拥有当前执行 skill 的运行时根名称。
    root_name: Option<String>,
    /// Whether the current Lua VM is the isolated luaexec runtime environment.
    /// 当前 Lua 虚拟机是否处于隔离的 luaexec 运行环境。
    luaexec_active: bool,
    /// Original tool name that launched the current luaexec request.
    /// 发起当前 luaexec 请求的原始工具名称。
    luaexec_caller_tool_name: Option<String>,
}

/// Parse one JSON value as an optional runtime request context.
/// 将单个 JSON 值解析为可选运行时请求上下文。
///
/// The value parameter is the JSON representation captured from `vulcan.context.request`.
/// value 参数是从 `vulcan.context.request` 捕获到的 JSON 表示。
///
/// The source_name parameter identifies the context source in runtime diagnostics.
/// source_name 参数用于在运行时诊断信息中标识上下文来源。
///
/// Return `None` for the documented empty-context sentinels, or the parsed request context.
/// 对约定的空上下文哨兵返回 `None`；否则返回已解析的请求上下文。
fn parse_runtime_request_context_json(
    value: Value,
    source_name: &str,
) -> Result<Option<RuntimeRequestContext>, String> {
    match &value {
        Value::Object(object) if object.is_empty() => Ok(None),
        Value::Array(array) if array.is_empty() => Ok(None),
        _ => serde_json::from_value::<RuntimeRequestContext>(value)
            .map(Some)
            .map_err(|error| {
                format!("{source_name} is not a valid runtime request context: {error}")
            }),
    }
}

/// Captured `vulcan.context` fields restored after a nested call exits.
/// 嵌套调用退出后需要恢复的 `vulcan.context` 字段。
struct VulcanContextSnapshot {
    /// Captured `vulcan.context.request` value.
    /// 捕获的 `vulcan.context.request` 值。
    request: LuaValue,
    /// Captured `vulcan.context.client_info` value.
    /// 捕获的 `vulcan.context.client_info` 值。
    client_info: LuaValue,
    /// Captured `vulcan.context.client_capabilities` value.
    /// 捕获的 `vulcan.context.client_capabilities` 值。
    client_capabilities: LuaValue,
    /// Captured `vulcan.context.client_budget` value.
    /// 捕获的 `vulcan.context.client_budget` 值。
    client_budget: LuaValue,
    /// Captured `vulcan.context.tool_config` value.
    /// 捕获的 `vulcan.context.tool_config` 值。
    tool_config: LuaValue,
    /// Captured `vulcan.context.host_result` value.
    /// 捕获的 `vulcan.context.host_result` 值。
    host_result: LuaValue,
}

/// Captured Lua file-context path with both exact Lua text and host path semantics.
/// 同时保存精确 Lua 文本与宿主路径语义的文件上下文路径快照。
struct VulcanFileContextPath {
    /// Exact string captured from `vulcan.context`.
    /// 从 `vulcan.context` 捕获到的精确字符串。
    lua_text: String,
    /// Host path derived from the exact Lua string for internal path consumers.
    /// 从精确 Lua 字符串派生、供内部路径消费者使用的宿主路径。
    path: PathBuf,
}

impl VulcanFileContextPath {
    /// Build one captured file-context path from an exact Lua-visible string.
    /// 根据一个 Lua 可见的精确字符串构建文件上下文路径快照。
    ///
    /// The `lua_text` parameter is the string read from one `vulcan.context` file field.
    /// `lua_text` 参数是从一个 `vulcan.context` 文件字段读取到的字符串。
    ///
    /// Return a value that preserves the Lua string and exposes a matching host path.
    /// 返回同时保留 Lua 字符串并暴露对应宿主路径的值。
    fn from_lua_text(lua_text: String) -> Self {
        // Derive the internal path once so dependency restore does not reinterpret raw strings later.
        // 只派生一次内部路径，避免 dependency restore 后续再次解释原始字符串。
        let path = PathBuf::from(&lua_text);
        Self { lua_text, path }
    }

    /// Return the exact Lua-visible string captured for this file context path.
    /// 返回为该文件上下文路径捕获到的精确 Lua 可见字符串。
    ///
    /// The self parameter owns the captured Lua text.
    /// self 参数持有已捕获的 Lua 文本。
    ///
    /// Return the raw string that should be restored into `vulcan.context`.
    /// 返回应恢复到 `vulcan.context` 的原始字符串。
    fn lua_text(&self) -> &str {
        &self.lua_text
    }

    /// Return the host path derived from the captured Lua-visible string.
    /// 返回从已捕获 Lua 可见字符串派生出的宿主路径。
    ///
    /// The self parameter owns the derived host path.
    /// self 参数持有已派生的宿主路径。
    ///
    /// Return the path reference used by internal dependency restoration.
    /// 返回内部依赖恢复使用的路径引用。
    fn path(&self) -> &Path {
        &self.path
    }
}

/// Captured Vulcan file context fields restored after a nested call exits.
/// 嵌套调用退出后需要恢复的 Vulcan 文件上下文字段。
struct VulcanFileContextSnapshot {
    /// Captured `vulcan.context.skill_dir` path.
    /// 捕获的 `vulcan.context.skill_dir` 路径。
    skill_dir: Option<VulcanFileContextPath>,
    /// Captured `vulcan.context.entry_dir` path.
    /// 捕获的 `vulcan.context.entry_dir` 路径。
    entry_dir: Option<VulcanFileContextPath>,
    /// Captured `vulcan.context.entry_file` path.
    /// 捕获的 `vulcan.context.entry_file` 路径。
    entry_file: Option<VulcanFileContextPath>,
}

/// Lua context metadata required to enter one loaded skill execution.
/// 进入一次已加载 skill 执行所需的 Lua 上下文元数据。
struct LoadedSkillLuaContext<'a> {
    /// Display tool name stored in `vulcan.runtime.internal.tool_name`.
    /// 存入 `vulcan.runtime.internal.tool_name` 的展示工具名称。
    display_tool_name: &'a str,
    /// Entry name stored in `vulcan.runtime.internal.entry_name`.
    /// 存入 `vulcan.runtime.internal.entry_name` 的入口名称。
    entry_name: &'a str,
    /// Absolute entry path exposed through `vulcan.context.entry_file`.
    /// 通过 `vulcan.context.entry_file` 暴露的绝对入口路径。
    entry_path: &'a Path,
    /// Optional invocation context exposed through `vulcan.context`.
    /// 通过 `vulcan.context` 暴露的可选调用上下文。
    invocation_context: Option<&'a LuaInvocationContext>,
}

/// Resolved metadata required to invoke one public `call_skill` request.
/// 调用一次公开 `call_skill` 请求所需的已解析元数据。
struct CallSkillInvocationTarget<'a> {
    /// Loaded skill that owns the resolved tool entry.
    /// 拥有已解析工具入口的已加载 skill。
    skill: &'a LoadedSkill,

    /// Tool metadata resolved from the target local entry name.
    /// 通过目标局部入口名解析出的工具元数据。
    tool: &'a crate::lua_skill::SkillToolMeta,

    /// Canonical tool name exposed to hosts and diagnostics.
    /// 暴露给宿主与诊断信息的 canonical 工具名。
    display_tool_name: &'a str,

    /// Stable local entry name declared by the owning skill.
    /// 所属 skill 声明的稳定局部入口名称。
    local_entry_name: &'a str,
}

/// Dependency context behavior used while entering anonymous Lua execution.
/// 进入匿名 Lua 执行时使用的依赖上下文行为。
enum AnonymousLuaDependencyContext<'a> {
    /// Clear dependency paths by installing an empty dependency context with host defaults.
    /// 通过宿主默认值安装空依赖上下文，从而清空依赖路径。
    ClearWithHostOptions(&'a LuaRuntimeHostOptions),
    /// Preserve the dependency context already installed by the surrounding VM setup.
    /// 保留外围虚拟机 setup 已经安装好的依赖上下文。
    PreserveCurrent,
}

/// Managed package-context behavior used while entering anonymous Lua execution.
/// 进入匿名 Lua 执行时使用的受管包上下文行为。
enum AnonymousLuaManagedPackageContext<'a> {
    /// Clear any package inherited from a pooled or previous execution scope.
    /// 清除从池化或此前执行作用域继承的任何包。
    Clear,
    /// Install one explicit trusted package for the anonymous execution scope.
    /// 为匿名执行作用域安装一个显式可信包。
    Set(&'a Arc<ManagedRuntimePackageContext>),
    /// Preserve the package already installed by the surrounding execution scope.
    /// 保留外围执行作用域已经安装的包。
    PreserveCurrent,
}

/// Lua context metadata required before executing anonymous Lua code.
/// 执行匿名 Lua 代码前所需的 Lua 上下文元数据。
struct AnonymousLuaExecutionContext<'a> {
    /// Optional invocation context exposed through `vulcan.context`.
    /// 通过 `vulcan.context` 暴露的可选调用上下文。
    invocation_context: Option<&'a LuaInvocationContext>,
    /// Internal runtime metadata exposed through `vulcan.runtime.internal`.
    /// 通过 `vulcan.runtime.internal` 暴露的内部运行时元数据。
    internal_context: VulcanInternalExecutionContext,
    /// Optional entry file exposed through `vulcan.context.entry_file`.
    /// 通过 `vulcan.context.entry_file` 暴露的可选入口文件。
    entry_file: Option<&'a Path>,
    /// Dependency context behavior for this anonymous execution mode.
    /// 当前匿名执行模式的依赖上下文行为。
    dependency_context: AnonymousLuaDependencyContext<'a>,
    /// Managed package behavior for this anonymous execution mode.
    /// 当前匿名执行模式的受管包行为。
    managed_package_context: AnonymousLuaManagedPackageContext<'a>,
}

/// Resolved nested skill call target used to enter one `vulcan.call`.
/// 用于进入一次 `vulcan.call` 的已解析嵌套 skill 调用目标。
struct LuaNestedCallTarget<'a> {
    /// Canonical tool name exposed to the nested Lua runtime context.
    /// 暴露给嵌套 Lua 运行时上下文的 canonical 工具名称。
    display_name: &'a str,
    /// Owning skill id used for dependency and database context binding.
    /// 用于依赖与数据库上下文绑定的所属 skill 标识符。
    owner_skill_name: &'a str,
    /// Local entry name declared by the owning skill.
    /// 所属 skill 声明的局部入口名称。
    owner_local_name: &'a str,
    /// Runtime root name that owns the nested target.
    /// 拥有嵌套目标的运行时根名称。
    owner_root_name: &'a str,
    /// Absolute owning skill directory used for file and dependency context.
    /// 用于文件与依赖上下文的所属 skill 绝对目录。
    owner_skill_dir: &'a Path,
    /// Trusted managed package context owned by the nested target.
    /// 嵌套目标拥有的可信受管包上下文。
    owner_managed_package: &'a Arc<ManagedRuntimePackageContext>,
    /// Absolute Lua entry file path used for the nested file context.
    /// 用于嵌套文件上下文的 Lua 入口绝对路径。
    entry_path: &'a Path,
    /// Inherited invocation context passed into the nested skill.
    /// 传入嵌套 skill 的继承调用上下文。
    invocation_context: &'a LuaInvocationContext,
    /// LanceDB binding resolved for the owning skill before entering the call.
    /// 进入调用前为所属 skill 解析出的 LanceDB 绑定。
    lancedb_binding: Option<Arc<LanceDbSkillBinding>>,
    /// SQLite binding resolved for the owning skill before entering the call.
    /// 进入调用前为所属 skill 解析出的 SQLite 绑定。
    sqlite_binding: Option<Arc<SqliteSkillBinding>>,
}

/// Build the resolved nested call target passed into `enter_nested_call`.
/// 构建传入 `enter_nested_call` 的已解析嵌套调用目标。
///
/// The `dispatch_entry` parameter contains the immutable registry metadata for the nested target.
/// `dispatch_entry` 参数包含嵌套目标的不可变注册表元数据。
///
/// The `invocation_context` parameter is the context inherited from the outer `vulcan.context`.
/// `invocation_context` 参数是从外层 `vulcan.context` 继承得到的上下文。
///
/// The `provider_bindings` parameter contains database bindings resolved for the owner skill.
/// `provider_bindings` 参数包含为所属 skill 解析出的数据库绑定。
///
/// Return the complete target payload required to enter the nested call.
/// 返回进入嵌套调用所需的完整目标载荷。
fn build_lua_nested_call_target<'a>(
    dispatch_entry: &'a LuaCallDispatchEntry,
    invocation_context: &'a LuaInvocationContext,
    provider_bindings: LuaCallProviderBindings,
) -> LuaNestedCallTarget<'a> {
    LuaNestedCallTarget {
        display_name: &dispatch_entry.display_name,
        owner_skill_name: &dispatch_entry.owner_skill_id,
        owner_local_name: &dispatch_entry.local_name,
        owner_root_name: &dispatch_entry.root_name,
        owner_skill_dir: &dispatch_entry.owner_skill_dir,
        owner_managed_package: &dispatch_entry.owner_managed_package,
        entry_path: &dispatch_entry.entry_path,
        invocation_context,
        lancedb_binding: provider_bindings.lancedb,
        sqlite_binding: provider_bindings.sqlite,
    }
}

/// Build the internal execution context installed for one nested `vulcan.call` target.
/// 构建为单个嵌套 `vulcan.call` 目标安装的内部执行上下文。
///
/// The `target` parameter supplies the nested target identity fields.
/// `target` 参数提供嵌套目标的身份字段。
///
/// The `outer_internal_context` parameter supplies luaexec markers captured before entering the nested target.
/// `outer_internal_context` 参数提供进入嵌套目标前捕获的 luaexec 标记。
///
/// Return the internal execution context that should be written into `vulcan.runtime.internal`.
/// 返回应写入 `vulcan.runtime.internal` 的内部执行上下文。
fn build_lua_nested_internal_execution_context(
    target: &LuaNestedCallTarget<'_>,
    outer_internal_context: &VulcanInternalExecutionContext,
) -> VulcanInternalExecutionContext {
    VulcanInternalExecutionContext {
        tool_name: Some(target.display_name.to_string()),
        skill_name: Some(target.owner_skill_name.to_string()),
        entry_name: Some(target.owner_local_name.to_string()),
        root_name: Some(target.owner_root_name.to_string()),
        luaexec_active: outer_internal_context.luaexec_active,
        luaexec_caller_tool_name: outer_internal_context.luaexec_caller_tool_name.clone(),
    }
}

/// Populate file, dependency, and provider contexts for one nested `vulcan.call`.
/// 为单次嵌套 `vulcan.call` 填充文件、依赖与 provider 上下文。
///
/// The `lua` parameter is the VM being switched into the nested target context.
/// `lua` 参数是正在切换到嵌套目标上下文的 VM。
///
/// The `host_options` parameter supplies dependency search roots for the nested owner skill.
/// `host_options` 参数提供嵌套目标所属 skill 的依赖搜索根。
///
/// The `target` parameter supplies file paths, owner identity, and provider bindings for the nested target.
/// `target` 参数提供嵌套目标的文件路径、所属身份与 provider binding。
///
/// Return `Ok(())` after all resource contexts are installed.
/// 所有资源上下文安装完成后返回 `Ok(())`。
fn populate_lua_nested_resource_contexts(
    lua: &Lua,
    host_options: &LuaRuntimeHostOptions,
    target: LuaNestedCallTarget<'_>,
) -> Result<(), String> {
    // Target package context installed before any nested managed-runtime API can execute.
    // 在任何嵌套受管运行时 API 执行前安装的目标包上下文。
    replace_lua_managed_package_context(lua, Some(target.owner_managed_package.clone()));
    populate_vulcan_file_context(lua, Some(target.owner_skill_dir), Some(target.entry_path))?;
    populate_vulcan_dependency_context(
        lua,
        host_options,
        Some(target.owner_skill_dir),
        Some(target.owner_skill_name),
    )?;
    LuaEngine::populate_vulcan_lancedb_context(
        lua,
        target.lancedb_binding,
        Some(target.owner_skill_name),
    )?;
    LuaEngine::populate_vulcan_sqlite_context(
        lua,
        target.sqlite_binding,
        Some(target.owner_skill_name),
    )?;
    Ok(())
}

/// Host-visible skill lifecycle event draft assembled inside the runtime engine.
/// 运行时引擎内部组装的宿主可见技能生命周期事件草稿。
struct SkillLifecycleEventDraft<'a> {
    /// Operation plane that evaluated the lifecycle action.
    /// 评估生命周期动作的操作平面。
    plane: SkillOperationPlane,
    /// Lifecycle action represented by this event.
    /// 当前事件表示的生命周期动作。
    action: crate::skill::manager::SkillLifecycleAction,
    /// Skill identifier targeted by the lifecycle operation.
    /// 生命周期操作所针对的技能标识符。
    skill_id: &'a str,
    /// Optional runtime root name that owns the target skill instance.
    /// 拥有目标技能实例的可选运行时根名称。
    root_name: Option<String>,
    /// Optional physical skill directory for the target skill instance.
    /// 目标技能实例的可选物理技能目录。
    skill_dir: Option<String>,
    /// High-level lifecycle outcome status emitted to the host.
    /// 发送给宿主的高层生命周期结果状态。
    status: &'a str,
    /// Optional human-readable lifecycle outcome message.
    /// 可选的人类可读生命周期结果说明。
    message: Option<String>,
}

/// Format one lifecycle failure message together with rollback and runtime-restore diagnostics.
/// 将单个生命周期失败消息与回滚、运行时恢复诊断一起格式化。
///
/// The base_message parameter is the primary failure text that started recovery handling.
/// base_message 参数是触发恢复处理的主失败文本。
///
/// The rollback_result parameter is the result of restoring staged skill filesystem or state changes.
/// rollback_result 参数是恢复暂存技能文件系统或状态变更的结果。
///
/// The restore_result parameter is the result of reloading the runtime after recovery.
/// restore_result 参数是恢复后重新加载运行时的结果。
///
/// Returns one complete human-readable failure message.
/// 返回一条完整的人类可读失败消息。
fn format_lifecycle_recovery_error<R, S>(
    base_message: String,
    rollback_result: Result<(), R>,
    restore_result: Result<(), S>,
) -> String
where
    R: Display,
    S: Display,
{
    // Mutable failure message that receives only the recovery diagnostics that actually failed.
    // 可变失败消息，仅追加真实失败的恢复诊断。
    let mut message = base_message;
    if let Err(error) = rollback_result {
        message.push_str(&format!(". rollback failed: {}", error));
    }
    if let Err(error) = restore_result {
        message.push_str(&format!(". runtime restore failed: {}", error));
    }
    message
}

/// Internal lifecycle action subset accepted by the install/update apply pipeline.
/// 安装/更新 apply 流程接受的内部生命周期动作子集。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkillApplyLifecycleAction {
    /// Install one skill into a resolved runtime root.
    /// 将单个技能安装到已解析的运行时根。
    Install,
    /// Update one already installed skill in a resolved runtime root.
    /// 更新已安装在运行时根中的单个技能。
    Update,
}

impl SkillApplyLifecycleAction {
    /// Convert one manager lifecycle action into the narrower install/update apply action set.
    /// 将单个管理器生命周期动作转换为更窄的安装/更新 apply 动作集合。
    ///
    /// The action parameter is the public manager lifecycle action supplied by runtime entrypoints.
    /// action 参数是运行时入口传入的公开管理器生命周期动作。
    ///
    /// Return the narrowed action for install/update, or an explicit unsupported-action error.
    /// 对安装/更新返回收窄后的动作；其他动作返回显式不支持错误。
    fn from_lifecycle_action(
        action: crate::skill::manager::SkillLifecycleAction,
    ) -> Result<Self, String> {
        match action {
            crate::skill::manager::SkillLifecycleAction::Install => Ok(Self::Install),
            crate::skill::manager::SkillLifecycleAction::Update => Ok(Self::Update),
            unsupported => Err(format!("unsupported apply action {:?}", unsupported)),
        }
    }
}

/// Capture one `vulcan.context` field into a nested-call snapshot.
/// 将单个 `vulcan.context` 字段捕获到嵌套调用快照中。
///
/// The `context` parameter is the `vulcan.context` table being captured.
/// `context` 参数是正在捕获的 `vulcan.context` 表。
///
/// The `field_name` parameter is the exact context field key to read.
/// `field_name` 参数是需要读取的精确上下文字段键。
///
/// Return the captured Lua value for the requested field.
/// 返回指定字段捕获到的 Lua 值。
fn capture_vulcan_context_snapshot_field(
    context: &Table,
    field_name: &str,
) -> Result<LuaValue, String> {
    context
        .get(field_name)
        .map_err(|error| format!("Failed to read vulcan.context.{}: {}", field_name, error))
}

/// Capture the current request/client/tool/host result fields stored on `vulcan.context`.
/// 捕获当前存放在 `vulcan.context` 上的请求、客户端、工具与宿主结果字段。
///
/// The `lua` parameter is the VM whose `vulcan.context` table should be captured.
/// `lua` 参数是需要捕获 `vulcan.context` 表的 VM。
///
/// Return the complete context snapshot required to restore the outer nested-call state.
/// 返回恢复外层嵌套调用状态所需的完整上下文快照。
fn capture_vulcan_context_snapshot(lua: &Lua) -> Result<VulcanContextSnapshot, String> {
    let context = get_vulcan_context_table(lua)?;
    Ok(VulcanContextSnapshot {
        request: capture_vulcan_context_snapshot_field(&context, "request")?,
        client_info: capture_vulcan_context_snapshot_field(&context, "client_info")?,
        client_capabilities: capture_vulcan_context_snapshot_field(
            &context,
            "client_capabilities",
        )?,
        client_budget: capture_vulcan_context_snapshot_field(&context, "client_budget")?,
        tool_config: capture_vulcan_context_snapshot_field(&context, "tool_config")?,
        host_result: capture_vulcan_context_snapshot_field(&context, "host_result")?,
    })
}

/// Restore one captured `vulcan.context` field from a nested-call snapshot.
/// 从嵌套调用快照恢复单个已捕获的 `vulcan.context` 字段。
///
/// The `context` parameter is the `vulcan.context` table being restored.
/// `context` 参数是正在恢复的 `vulcan.context` 表。
///
/// The `field_name` parameter is the exact context field key to write.
/// `field_name` 参数是需要写入的精确上下文字段键。
///
/// The `value` parameter is the captured Lua value to restore.
/// `value` 参数是需要恢复的已捕获 Lua 值。
///
/// Return `Ok(())` after the field is restored.
/// 字段恢复完成后返回 `Ok(())`。
fn restore_vulcan_context_snapshot_field(
    context: &Table,
    field_name: &str,
    value: &LuaValue,
) -> Result<(), String> {
    context
        .set(field_name, value.clone())
        .map_err(|error| format!("Failed to restore vulcan.context.{}: {}", field_name, error))
}

/// Restore the request/client/tool/host result snapshot captured before a nested call.
/// 恢复嵌套调用前捕获的请求、客户端、工具与宿主结果快照。
///
/// The `lua` parameter is the VM whose `vulcan.context` table should be restored.
/// `lua` 参数是需要恢复 `vulcan.context` 表的 VM。
///
/// The `snapshot` parameter contains the captured outer context values.
/// `snapshot` 参数包含已捕获的外层上下文值。
///
/// Return `Ok(())` after all captured fields are restored.
/// 所有已捕获字段恢复后返回 `Ok(())`。
fn restore_vulcan_context_snapshot(
    lua: &Lua,
    snapshot: &VulcanContextSnapshot,
) -> Result<(), String> {
    let context = get_vulcan_context_table(lua)?;
    restore_vulcan_context_snapshot_field(&context, "request", &snapshot.request)?;
    restore_vulcan_context_snapshot_field(&context, "client_info", &snapshot.client_info)?;
    restore_vulcan_context_snapshot_field(
        &context,
        "client_capabilities",
        &snapshot.client_capabilities,
    )?;
    restore_vulcan_context_snapshot_field(&context, "client_budget", &snapshot.client_budget)?;
    restore_vulcan_context_snapshot_field(&context, "tool_config", &snapshot.tool_config)?;
    restore_vulcan_context_snapshot_field(&context, "host_result", &snapshot.host_result)?;
    Ok(())
}

/// Capture the current Lua entry context stored on `vulcan`.
/// 捕获当前存放在 `vulcan` 上的 Lua 入口文件上下文。
fn capture_vulcan_file_context(lua: &Lua) -> Result<VulcanFileContextSnapshot, String> {
    let context = get_vulcan_context_table(lua)?;
    Ok(VulcanFileContextSnapshot {
        skill_dir: capture_vulcan_file_context_path(&context, "skill_dir")?,
        entry_dir: capture_vulcan_file_context_path(&context, "entry_dir")?,
        entry_file: capture_vulcan_file_context_path(&context, "entry_file")?,
    })
}

/// Capture one optional path-like file context field from `vulcan.context`.
/// 从 `vulcan.context` 捕获单个可选的类路径文件上下文字段。
///
/// The `context` parameter is the existing `vulcan.context` table to read.
/// `context` 参数是需要读取的现有 `vulcan.context` 表。
///
/// The `field_name` parameter is the exact field key to capture.
/// `field_name` 参数是需要捕获的精确字段键。
///
/// Return the captured path wrapper, or `None` when the Lua field is nil.
/// 返回捕获到的路径包装值；当 Lua 字段为 nil 时返回 `None`。
fn capture_vulcan_file_context_path(
    context: &Table,
    field_name: &str,
) -> Result<Option<VulcanFileContextPath>, String> {
    // Preserve the existing Lua contract: only string or nil values are valid file context fields.
    // 保留现有 Lua 约定：文件上下文字段只能是字符串或 nil。
    let lua_text: Option<String> = context
        .get(field_name)
        .map_err(|error| format!("Failed to read vulcan.context.{}: {}", field_name, error))?;
    Ok(lua_text.map(VulcanFileContextPath::from_lua_text))
}

/// Restore one optional path field on `vulcan.context` from a captured file context snapshot.
/// 从捕获的文件上下文快照恢复 `vulcan.context` 上的单个可选路径字段。
///
/// The `context` parameter is the existing `vulcan.context` table to update.
/// `context` 参数是需要更新的现有 `vulcan.context` 表。
///
/// The `field_name` parameter is the exact field key to restore.
/// `field_name` 参数是需要恢复的精确字段键。
///
/// The `value` parameter is the captured optional file context path for that field.
/// `value` 参数是该字段捕获到的可选文件上下文路径。
///
/// Return `Ok(())` after the field is restored or cleared to nil.
/// 字段恢复或清空为 nil 后返回 `Ok(())`。
fn restore_vulcan_file_context_field(
    context: &Table,
    field_name: &str,
    value: Option<&VulcanFileContextPath>,
) -> Result<(), String> {
    match value {
        Some(value) => context
            .set(field_name, value.lua_text())
            .map_err(|error| format!("Failed to restore vulcan.context.{}: {}", field_name, error)),
        None => context
            .set(field_name, LuaValue::Nil)
            .map_err(|error| format!("Failed to restore vulcan.context.{}: {}", field_name, error)),
    }
}

/// Restore the exact Vulcan file context snapshot captured before a nested call.
/// 恢复嵌套调用前捕获的精确 Vulcan 文件上下文快照。
///
/// The `lua` parameter is the VM whose `vulcan.context` table should be restored.
/// `lua` 参数是需要恢复 `vulcan.context` 表的 VM。
///
/// The `snapshot` parameter contains the captured skill directory, entry directory, and entry file values.
/// `snapshot` 参数包含捕获到的 skill 目录、入口目录与入口文件值。
///
/// Return `Ok(())` after all file context fields are restored.
/// 所有文件上下文字段恢复后返回 `Ok(())`。
fn restore_vulcan_file_context_snapshot(
    lua: &Lua,
    snapshot: &VulcanFileContextSnapshot,
) -> Result<(), String> {
    // Restore from the captured table values instead of deriving entry_dir from entry_file.
    // 从捕获到的表值恢复，而不是从 entry_file 重新推导 entry_dir。
    let context = get_vulcan_context_table(lua)?;
    restore_vulcan_file_context_field(&context, "skill_dir", snapshot.skill_dir.as_ref())?;
    restore_vulcan_file_context_field(&context, "entry_dir", snapshot.entry_dir.as_ref())?;
    restore_vulcan_file_context_field(&context, "entry_file", snapshot.entry_file.as_ref())?;
    Ok(())
}

/// Restore dependency paths from the captured outer file and internal contexts.
/// 根据已捕获的外层文件上下文与内部上下文恢复依赖路径。
///
/// The `lua` parameter is the VM whose `vulcan.deps` table should be restored.
/// `lua` 参数是需要恢复 `vulcan.deps` 表的 VM。
///
/// The `host_options` parameter supplies dependency directory naming rules.
/// `host_options` 参数提供依赖目录命名规则。
///
/// The `file_context` parameter contains the captured outer file context paths.
/// `file_context` 参数包含已捕获的外层文件上下文路径。
///
/// The `internal_context` parameter contains the captured outer active skill name.
/// `internal_context` 参数包含已捕获的外层 active skill 名称。
///
/// Return `Ok(())` after dependency paths are restored or cleared.
/// 依赖路径恢复或清空后返回 `Ok(())`。
fn restore_lua_nested_dependency_context(
    lua: &Lua,
    host_options: &LuaRuntimeHostOptions,
    file_context: &VulcanFileContextSnapshot,
    internal_context: &VulcanInternalExecutionContext,
) -> Result<(), String> {
    populate_vulcan_dependency_context(
        lua,
        host_options,
        file_context
            .skill_dir
            .as_ref()
            .map(VulcanFileContextPath::path),
        internal_context.skill_name.as_deref(),
    )
}

/// Captured outer state owned by one nested `vulcan.call` guard.
/// 单个嵌套 `vulcan.call` 守卫持有的外层状态快照。
struct LuaNestedOuterStateSnapshot {
    /// Captured core `vulcan` table topology.
    /// 已捕获的核心 `vulcan` 表拓扑。
    core_state: VulcanCoreModuleState,
    /// Captured request/client/tool/host result context.
    /// 已捕获的请求、客户端、工具与宿主结果上下文。
    context: VulcanContextSnapshot,
    /// Captured LanceDB provider skill marker.
    /// 已捕获的 LanceDB provider skill 标记。
    lancedb_skill_name: String,
    /// Captured SQLite provider skill marker.
    /// 已捕获的 SQLite provider skill 标记。
    sqlite_skill_name: String,
    /// Captured internal execution context.
    /// 已捕获的内部执行上下文。
    internal_context: VulcanInternalExecutionContext,
    /// Captured file context.
    /// 已捕获的文件上下文。
    file_context: VulcanFileContextSnapshot,
    /// Captured host-private managed package context.
    /// 已捕获的宿主私有受管包上下文。
    managed_package: Option<Arc<ManagedRuntimePackageContext>>,
}

/// Capture the complete outer state required to restore after one nested `vulcan.call`.
/// 捕获单次嵌套 `vulcan.call` 结束后恢复外层所需的完整状态。
///
/// The `lua` parameter is the VM whose current `vulcan` state should be captured.
/// `lua` 参数是需要捕获当前 `vulcan` 状态的 VM。
///
/// Return the complete outer state snapshot used by the nested-call guard.
/// 返回嵌套调用守卫使用的完整外层状态快照。
fn capture_lua_nested_outer_state(lua: &Lua) -> Result<LuaNestedOuterStateSnapshot, String> {
    let vulcan = get_vulcan_table(lua)?;
    Ok(LuaNestedOuterStateSnapshot {
        core_state: VulcanCoreModuleState::capture(lua)?,
        context: capture_vulcan_context_snapshot(lua)?,
        lancedb_skill_name: capture_provider_skill_marker(&vulcan, "__lancedb_skill_name")?,
        sqlite_skill_name: capture_provider_skill_marker(&vulcan, "__sqlite_skill_name")?,
        internal_context: capture_vulcan_internal_execution_context(lua)?,
        file_context: capture_vulcan_file_context(lua)?,
        managed_package: optional_lua_managed_package_context(lua),
    })
}

/// Populate the current skill directory, entry directory, and entry file onto `vulcan`.
/// 将当前 skill 目录、入口目录与入口文件路径注入到 `vulcan` 模块。
fn populate_vulcan_file_context(
    lua: &Lua,
    skill_dir: Option<&Path>,
    entry_file: Option<&Path>,
) -> Result<(), String> {
    let context = get_vulcan_context_table(lua)?;

    match skill_dir {
        Some(path) => context
            .set("skill_dir", render_host_visible_path(path))
            .map_err(|error| format!("Failed to set vulcan.context.skill_dir: {}", error))?,
        None => context
            .set("skill_dir", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.context.skill_dir: {}", error))?,
    }

    match entry_file {
        Some(path) => {
            let entry_dir = path.parent().unwrap_or(path);
            context
                .set("entry_dir", render_host_visible_path(entry_dir))
                .map_err(|error| format!("Failed to set vulcan.context.entry_dir: {}", error))?;
            context
                .set("entry_file", render_host_visible_path(path))
                .map_err(|error| format!("Failed to set vulcan.context.entry_file: {}", error))?;
        }
        None => {
            context
                .set("entry_dir", LuaValue::Nil)
                .map_err(|error| format!("Failed to clear vulcan.context.entry_dir: {}", error))?;
            context
                .set("entry_file", LuaValue::Nil)
                .map_err(|error| format!("Failed to clear vulcan.context.entry_file: {}", error))?;
        }
    }

    Ok(())
}

/// Populate the current skill dependency roots onto `vulcan.deps`.
/// 将当前技能依赖根路径注入到 `vulcan.deps` 中。
fn populate_vulcan_dependency_context(
    lua: &Lua,
    host_options: &LuaRuntimeHostOptions,
    skill_dir: Option<&Path>,
    skill_id: Option<&str>,
) -> Result<(), String> {
    let deps = get_vulcan_deps_table(lua)?;

    let clear_paths = || -> Result<(), String> {
        deps.set("tools_path", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.deps.tools_path: {}", error))?;
        deps.set("lua_path", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.deps.lua_path: {}", error))?;
        deps.set("ffi_path", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.deps.ffi_path: {}", error))?;
        Ok(())
    };

    let Some(skill_dir) = skill_dir else {
        return clear_paths();
    };
    let Some(skill_id) = skill_id.filter(|value| !value.trim().is_empty()) else {
        return clear_paths();
    };

    let skills_root = skill_dir.parent().ok_or_else(|| {
        format!(
            "Failed to derive skills root from skill directory {}",
            render_log_friendly_path(skill_dir)
        )
    })?;
    let runtime_root = skills_root.parent().ok_or_else(|| {
        format!(
            "Failed to derive runtime root from skill directory {}",
            render_log_friendly_path(skill_dir)
        )
    })?;
    let dependency_root = runtime_root.join(host_options.dependency_dir_name.as_str());

    deps.set(
        "tools_path",
        render_host_visible_path(&dependency_root.join("tools").join(skill_id)),
    )
    .map_err(|error| format!("Failed to set vulcan.deps.tools_path: {}", error))?;
    deps.set(
        "lua_path",
        render_host_visible_path(&dependency_root.join("lua").join(skill_id)),
    )
    .map_err(|error| format!("Failed to set vulcan.deps.lua_path: {}", error))?;
    deps.set(
        "ffi_path",
        render_host_visible_path(&dependency_root.join("ffi").join(skill_id)),
    )
    .map_err(|error| format!("Failed to set vulcan.deps.ffi_path: {}", error))?;
    Ok(())
}

/// Capture the internal execution markers currently stored on `vulcan`.
/// 捕获当前存放在 `vulcan` 上的内部执行标记。
fn capture_vulcan_internal_execution_context(
    lua: &Lua,
) -> Result<VulcanInternalExecutionContext, String> {
    let internal = get_vulcan_runtime_internal_table(lua)?;
    let tool_name: Option<String> = internal.get("tool_name").map_err(|error| {
        format!(
            "Failed to read vulcan.runtime.internal.tool_name: {}",
            error
        )
    })?;
    let skill_name: Option<String> = internal.get("skill_name").map_err(|error| {
        format!(
            "Failed to read vulcan.runtime.internal.skill_name: {}",
            error
        )
    })?;
    let entry_name: Option<String> = internal.get("entry_name").map_err(|error| {
        format!(
            "Failed to read vulcan.runtime.internal.entry_name: {}",
            error
        )
    })?;
    let root_name: Option<String> = internal.get("root_name").map_err(|error| {
        format!(
            "Failed to read vulcan.runtime.internal.root_name: {}",
            error
        )
    })?;
    let luaexec_active: bool = internal.get("luaexec_active").map_err(|error| {
        format!(
            "Failed to read vulcan.runtime.internal.luaexec_active: {}",
            error
        )
    })?;
    let luaexec_caller_tool_name: Option<String> =
        internal.get("luaexec_caller_tool_name").map_err(|error| {
            format!(
                "Failed to read vulcan.runtime.internal.luaexec_caller_tool_name: {}",
                error
            )
        })?;
    Ok(VulcanInternalExecutionContext {
        tool_name,
        skill_name,
        entry_name,
        root_name,
        luaexec_active,
        luaexec_caller_tool_name,
    })
}

/// Populate the internal execution markers stored on `vulcan`.
/// 填充存放在 `vulcan` 上的内部执行标记。
fn populate_vulcan_internal_execution_context(
    lua: &Lua,
    context: &VulcanInternalExecutionContext,
) -> Result<(), String> {
    let internal = get_vulcan_runtime_internal_table(lua)?;

    match context.tool_name.as_deref() {
        Some(tool_name) => internal.set("tool_name", tool_name).map_err(|error| {
            format!("Failed to set vulcan.runtime.internal.tool_name: {}", error)
        })?,
        None => internal.set("tool_name", LuaValue::Nil).map_err(|error| {
            format!(
                "Failed to clear vulcan.runtime.internal.tool_name: {}",
                error
            )
        })?,
    }

    match context.skill_name.as_deref() {
        Some(skill_name) => internal.set("skill_name", skill_name).map_err(|error| {
            format!(
                "Failed to set vulcan.runtime.internal.skill_name: {}",
                error
            )
        })?,
        None => internal.set("skill_name", LuaValue::Nil).map_err(|error| {
            format!(
                "Failed to clear vulcan.runtime.internal.skill_name: {}",
                error
            )
        })?,
    }

    match context.entry_name.as_deref() {
        Some(entry_name) => internal.set("entry_name", entry_name).map_err(|error| {
            format!(
                "Failed to set vulcan.runtime.internal.entry_name: {}",
                error
            )
        })?,
        None => internal.set("entry_name", LuaValue::Nil).map_err(|error| {
            format!(
                "Failed to clear vulcan.runtime.internal.entry_name: {}",
                error
            )
        })?,
    }

    match context.root_name.as_deref() {
        Some(root_name) => internal.set("root_name", root_name).map_err(|error| {
            format!("Failed to set vulcan.runtime.internal.root_name: {}", error)
        })?,
        None => internal.set("root_name", LuaValue::Nil).map_err(|error| {
            format!(
                "Failed to clear vulcan.runtime.internal.root_name: {}",
                error
            )
        })?,
    }

    internal
        .set("luaexec_active", context.luaexec_active)
        .map_err(|error| {
            format!(
                "Failed to set vulcan.runtime.internal.luaexec_active: {}",
                error
            )
        })?;

    match context.luaexec_caller_tool_name.as_deref() {
        Some(tool_name) => internal
            .set("luaexec_caller_tool_name", tool_name)
            .map_err(|error| {
                format!(
                    "Failed to set vulcan.runtime.internal.luaexec_caller_tool_name: {}",
                    error
                )
            })?,
        None => internal
            .set("luaexec_caller_tool_name", LuaValue::Nil)
            .map_err(|error| {
                format!(
                    "Failed to clear vulcan.runtime.internal.luaexec_caller_tool_name: {}",
                    error
                )
            })?,
    }

    Ok(())
}

/// Resolve the active skill identifier currently stored in the internal `vulcan` execution context.
/// 解析当前存储在内部 `vulcan` 执行上下文中的活动技能标识符。
fn current_vulcan_config_skill_id(lua: &Lua, api_name: &str) -> Result<String, mlua::Error> {
    let internal = get_vulcan_runtime_internal_table(lua)
        .map_err(|error| mlua::Error::runtime(format!("{}: {}", api_name, error)))?;
    let skill_name: Option<String> = internal
        .get("skill_name")
        .map_err(|error| mlua::Error::runtime(format!("{}: {}", api_name, error)))?;
    skill_name
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            mlua::Error::runtime(format!("{} requires one active skill context", api_name))
        })
}

/// Read one optional string field from a managed runtime invoke table.
/// 从受管运行时 invoke 表读取一个可选字符串字段。
fn managed_runtime_optional_string_field(
    table: &Table,
    api_name: &str,
    field_name: &str,
) -> Result<Option<String>, mlua::Error> {
    let value: LuaValue = table
        .get(field_name)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(value) => Ok(Some(
            value
                .to_str()
                .map_err(|error| {
                    mlua::Error::runtime(format!("{api_name}: {field_name} is not UTF-8: {error}"))
                })?
                .to_string(),
        )),
        other => Err(mlua::Error::runtime(format!(
            "{api_name}: {field_name} must be a string or nil, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Read one optional timeout field from a managed runtime invoke table.
/// 从受管运行时 invoke 表读取一个可选超时字段。
fn managed_runtime_optional_timeout_ms(
    table: &Table,
    api_name: &str,
) -> Result<Option<u64>, mlua::Error> {
    let value: LuaValue = table
        .get("timeout_ms")
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    optional_u64_arg(value, api_name, "timeout_ms")
}

/// Host-private Lua app-data slot containing the active resource transaction.
/// 包含活动资源事务的宿主私有 Lua 应用数据槽。
#[derive(Clone, Copy, Default)]
struct ManagedRuntimeTransactionContextSlot {
    /// Active evaluation transaction; absent outside a transactional lease eval.
    /// 活动执行事务；事务型租约执行之外不存在。
    context: Option<ManagedRuntimeTransactionContext>,
}

/// RAII Lua transaction scope restoring the previous app-data context on every exit path.
/// 在每条退出路径上恢复此前应用数据上下文的 RAII Lua 事务作用域。
struct LuaManagedRuntimeTransactionScopeGuard {
    /// Lua VM whose host-private transaction slot is temporarily replaced.
    /// 宿主私有事务槽被临时替换的 Lua VM。
    lua: Lua,
    /// Exact transaction context active before this scope.
    /// 当前作用域前活动的精确事务上下文。
    previous: Option<ManagedRuntimeTransactionContext>,
}

/// Return the engine-owned managed runtime services installed in one Lua VM.
/// 返回安装在单个 Lua VM 中的引擎所有受管运行时服务。
fn current_lua_managed_runtime_services(
    lua: &Lua,
    api_name: &str,
) -> Result<Arc<ManagedRuntimeServices>, mlua::Error> {
    // Cloned service reference released from the app-data borrow before any lock or launch.
    // 在任何加锁或启动前解除应用数据借用的服务克隆。
    let services = lua
        .app_data_ref::<Arc<ManagedRuntimeServices>>()
        .map(|services| Arc::clone(&services));
    services.ok_or_else(|| {
        mlua::Error::runtime(format!(
            "{api_name}: managed runtime services are not installed"
        ))
    })
}

/// Return the engine-owned worker service installed in one Lua VM.
/// 返回安装在单个 Lua VM 中的引擎所有 Worker 服务。
///
/// `lua` owns the app-data registry and `api_name` prefixes a stable missing-service error.
/// `lua` 拥有应用数据注册表，`api_name` 为稳定的服务缺失错误添加前缀。
///
/// Return the exact service shared by every VM created from the same engine.
/// 返回同一引擎创建的全部 VM 共享的精确服务。
fn current_lua_managed_runtime_worker_service(
    lua: &Lua,
    api_name: &str,
) -> Result<Arc<ManagedRuntimeWorkerService>, mlua::Error> {
    // Cloned worker service released from the app-data borrow before pool access or process launch.
    // 在访问池或启动进程前解除应用数据借用的 Worker 服务克隆。
    let service = lua
        .app_data_ref::<Arc<ManagedRuntimeWorkerService>>()
        .map(|service| Arc::clone(&service));
    service.ok_or_else(|| {
        mlua::Error::runtime(format!(
            "{api_name}: managed runtime worker service is not installed"
        ))
    })
}

/// Return the active evaluation transaction when one was installed.
/// 返回已安装的活动执行事务。
fn optional_lua_managed_runtime_transaction(lua: &Lua) -> Option<ManagedRuntimeTransactionContext> {
    lua.app_data_ref::<ManagedRuntimeTransactionContextSlot>()
        .and_then(|slot| slot.context)
}

/// Replace the active Lua resource transaction and return the previous context.
/// 替换活动 Lua 资源事务并返回此前上下文。
fn replace_lua_managed_runtime_transaction(
    lua: &Lua,
    context: Option<ManagedRuntimeTransactionContext>,
) -> Option<ManagedRuntimeTransactionContext> {
    lua.set_app_data(ManagedRuntimeTransactionContextSlot { context })
        .and_then(|slot| slot.context)
}

impl LuaManagedRuntimeTransactionScopeGuard {
    /// Install one optional transaction context until this guard is dropped.
    /// 安装一个可选事务上下文，直到当前保护对象被释放。
    fn install(lua: &Lua, context: Option<ManagedRuntimeTransactionContext>) -> Self {
        // Exact previous context captured for nested or repeated execution safety.
        // 为嵌套或重复执行安全捕获的精确此前上下文。
        let previous = replace_lua_managed_runtime_transaction(lua, context);
        Self {
            lua: lua.clone(),
            previous,
        }
    }
}

impl Drop for LuaManagedRuntimeTransactionScopeGuard {
    /// Restore the exact previous transaction context without running Lua code.
    /// 在不执行 Lua 代码的情况下恢复精确此前事务上下文。
    fn drop(&mut self) {
        replace_lua_managed_runtime_transaction(&self.lua, self.previous);
    }
}

/// Default retained byte limit for each managed runtime session output stream.
/// 每个受管运行时会话输出流默认保留的字节上限。
const MANAGED_RUNTIME_SESSION_DEFAULT_BUFFER_LIMIT_BYTES: usize = 1024 * 1024;

/// Parse one strict managed Python or Node persistent-session request.
/// 解析一个严格的受管 Python 或 Node 持久会话请求。
fn parse_managed_runtime_session_open_request(
    spec: LuaValue,
    api_name: &str,
    default_encoding: RuntimeTextEncoding,
) -> Result<ManagedRuntimeSessionOpenRequest, mlua::Error> {
    // Required Lua request table.
    // 必需的 Lua 请求表。
    let table = match spec {
        LuaValue::Table(table) => table,
        other => {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: spec must be a table, got {}",
                lua_value_type_name(&other)
            )));
        }
    };
    validate_managed_runtime_session_open_fields(&table, api_name)?;
    // Required package-relative source file.
    // 必需的包相对源文件。
    let file = managed_runtime_optional_string_field(&table, api_name, "file")?
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| mlua::Error::runtime(format!("{api_name}: file is required")))?;
    // Optional direct process argument array.
    // 可选的直接进程参数数组。
    let args = parse_managed_runtime_session_args(&table, api_name)?;
    // Optional cwd resolved and authorized by the pure Rust launch layer.
    // 由纯 Rust 启动层解析并授权的可选 cwd。
    let cwd = managed_runtime_optional_string_field(&table, api_name, "cwd")?;
    // Explicit or host-default stream encodings.
    // 显式或宿主默认的流编码。
    let stdout_encoding = managed_runtime_session_encoding_field(
        &table,
        "stdout_encoding",
        api_name,
        default_encoding,
    )?;
    let stderr_encoding = managed_runtime_session_encoding_field(
        &table,
        "stderr_encoding",
        api_name,
        default_encoding,
    )?;
    let stdin_encoding = managed_runtime_session_encoding_field(
        &table,
        "stdin_encoding",
        api_name,
        default_encoding,
    )?;
    // Positive per-stream byte limit converted without truncation.
    // 无截断转换得到的正数每流字节上限。
    let buffer_limit_bytes = match optional_u64_arg(
        table
            .get("buffer_limit_bytes")
            .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?,
        api_name,
        "buffer_limit_bytes",
    )? {
        Some(0) => {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: buffer_limit_bytes must be greater than zero"
            )));
        }
        Some(value) => usize::try_from(value).map_err(|_| {
            mlua::Error::runtime(format!(
                "{api_name}: buffer_limit_bytes exceeds the platform usize range"
            ))
        })?,
        None => MANAGED_RUNTIME_SESSION_DEFAULT_BUFFER_LIMIT_BYTES,
    };
    Ok(ManagedRuntimeSessionOpenRequest {
        file,
        args,
        cwd,
        stdout_encoding,
        stderr_encoding,
        stdin_encoding,
        buffer_limit_bytes,
    })
}

/// Reject every unknown field in a managed runtime session open request.
/// 拒绝受管运行时会话打开请求中的每个未知字段。
fn validate_managed_runtime_session_open_fields(
    table: &Table,
    api_name: &str,
) -> Result<(), mlua::Error> {
    for pair in table.clone().pairs::<LuaValue, LuaValue>() {
        // Outer request key decoded as UTF-8 text.
        // 解码为 UTF-8 文本的外层请求键。
        let (key, _value) =
            pair.map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
        let LuaValue::String(key) = key else {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: request keys must be strings"
            )));
        };
        // Stable field name checked against the complete session contract.
        // 针对完整会话契约检查的稳定字段名称。
        let key = key.to_str().map_err(|error| {
            mlua::Error::runtime(format!("{api_name}: key is not UTF-8: {error}"))
        })?;
        if !matches!(
            key.as_ref(),
            "file"
                | "args"
                | "cwd"
                | "stdout_encoding"
                | "stderr_encoding"
                | "stdin_encoding"
                | "buffer_limit_bytes"
        ) {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: unknown field `{key}`"
            )));
        }
    }
    Ok(())
}

/// Parse the optional dense string array passed as child-process arguments.
/// 解析作为子进程参数传入的可选稠密字符串数组。
fn parse_managed_runtime_session_args(
    table: &Table,
    api_name: &str,
) -> Result<Vec<String>, mlua::Error> {
    // Raw args value distinguishing absence from a table contract.
    // 区分缺失与表契约的原始 args 值。
    let value: LuaValue = table
        .get("args")
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    let args_table = match value {
        LuaValue::Nil => return Ok(Vec::new()),
        LuaValue::Table(table) => table,
        other => {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: args must be an array table or nil, got {}",
                lua_value_type_name(&other)
            )));
        }
    };
    // Lua raw array length used to require a dense one-based sequence.
    // 用于要求稠密一基序列的 Lua 原始数组长度。
    let length = args_table.raw_len();
    // Parsed argument strings in stable index order.
    // 按稳定索引顺序解析的参数字符串。
    let mut args = Vec::with_capacity(length);
    for index in 1..=length {
        // Required UTF-8 argument at the current dense array index.
        // 当前稠密数组索引处必需的 UTF-8 参数。
        let argument: mlua::String = args_table.get(index).map_err(|error| {
            mlua::Error::runtime(format!(
                "{api_name}: args[{index}] must be a string: {error}"
            ))
        })?;
        args.push(
            argument
                .to_str()
                .map_err(|error| {
                    mlua::Error::runtime(format!("{api_name}: args[{index}] is not UTF-8: {error}"))
                })?
                .to_string(),
        );
    }
    for pair in args_table.pairs::<LuaValue, LuaValue>() {
        // Actual argument-table key verified against the dense index range.
        // 针对稠密索引范围验证的实际参数表键。
        let (key, _value) =
            pair.map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
        let LuaValue::Integer(index) = key else {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: args must contain only integer array keys"
            )));
        };
        if index < 1
            || usize::try_from(index)
                .ok()
                .is_none_or(|index| index > length)
        {
            return Err(mlua::Error::runtime(format!(
                "{api_name}: args must be a dense one-based array"
            )));
        }
    }
    Ok(args)
}

/// Parse one optional session encoding label with a trusted host default.
/// 使用可信宿主默认值解析一个可选会话编码标签。
fn managed_runtime_session_encoding_field(
    table: &Table,
    field_name: &str,
    api_name: &str,
    default_encoding: RuntimeTextEncoding,
) -> Result<RuntimeTextEncoding, mlua::Error> {
    // Optional explicit encoding label supplied by the package code.
    // 包代码提供的可选显式编码标签。
    let label = managed_runtime_optional_string_field(table, api_name, field_name)?;
    match label {
        Some(label) => RuntimeTextEncoding::parse(&label)
            .map_err(|error| mlua::Error::runtime(format!("{api_name}: {field_name}: {error}"))),
        None => Ok(default_encoding),
    }
}

/// Require one bound System lease and build its managed-session event routing identity.
/// 要求一个已绑定的 System 租约，并构造其受管会话事件路由身份。
fn system_managed_runtime_session_event_identity(
    package: &ManagedRuntimePackageContext,
    api_name: &str,
) -> Result<ManagedRuntimeSessionEventIdentity, mlua::Error> {
    // System-only binding is mandatory because persistent sessions require lease ownership.
    // System 专属绑定是必需的，因为长期会话必须拥有租约所有权。
    let binding = package.lease_binding().ok_or_else(|| {
        mlua::Error::runtime(format!(
            "{api_name}: persistent managed sessions are available only inside a System Plugin lease"
        ))
    })?;
    // Manager-issued lease identity guaranteed before System package evaluation begins.
    // 在 System 包执行开始前保证存在的管理器签发租约身份。
    let identity = binding.identity().ok_or_else(|| {
        mlua::Error::runtime(format!("{api_name}: system lease identity is not bound"))
    })?;
    Ok(ManagedRuntimeSessionEventIdentity {
        lease_id: identity.lease_id().to_string(),
        sid: binding.sid().to_string(),
        generation: identity.generation(),
    })
}

/// Open one managed Python persistent session through the shared process userdata.
/// 通过共享进程 userdata 打开一个受管 Python 持久会话。
fn open_managed_python_session(
    lua: &Lua,
    spec: LuaValue,
    default_encoding: RuntimeTextEncoding,
) -> Result<AnyUserData, mlua::Error> {
    // Stable API name used by validation and launch diagnostics.
    // 校验与启动诊断使用的稳定 API 名称。
    let api_name = "vulcan.runtime.python.session.open";
    ensure_persistent_managed_runtime_session_platform_supported()
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Trusted System Plugin package whose lease owns every persistent session.
    // 其租约拥有每个长期会话的可信 System Plugin 包。
    let package = current_lua_managed_package_context(lua, api_name)?;
    // Engine-owned resource and event lifecycle service.
    // 引擎拥有的资源与事件生命周期服务。
    let services = current_lua_managed_runtime_services(lua, api_name)?;
    // Bound System identity is checked before transaction lookup for one stable package-kind error.
    // 在查询事务前检查已绑定 System 身份，以获得稳定的包类型错误。
    let event_identity = system_managed_runtime_session_event_identity(package.as_ref(), api_name)?;
    // Active evaluation transaction required for deterministic rollback on any Lua failure.
    // 为确保任何 Lua 失败都能确定性回滚而要求的活动执行事务。
    let transaction = optional_lua_managed_runtime_transaction(lua).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "{api_name}: persistent managed sessions require an active System lease evaluation"
        ))
    })?;
    // Pre-launch reservation registered before any child output can occur.
    // 在任何子进程输出发生前注册的启动前预留。
    let reservation = services
        .reserve_session(
            package.owner_token(),
            Some(transaction),
            Some(event_identity),
        )
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Optional package-agnostic observer publishing durable System events.
    // 发布持久 System 事件的可选包无关观察器。
    let observer = reservation.observer();
    // Strict session launch request parsed from Lua.
    // 从 Lua 解析得到的严格会话启动请求。
    let request = parse_managed_runtime_session_open_request(spec, api_name, default_encoding)?;
    // Pure Rust launch result owning the process tree and optional package snapshot cleanup.
    // 拥有进程树与可选包快照清理的纯 Rust 启动结果。
    let launch = launch_managed_python_session(package.as_ref(), request, observer)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Live session id activated only after the process core is fully constructed.
    // 仅在进程核心完整构造后激活的活动会话 id。
    let (session_id, cleanup) = reservation
        .activate(&launch.core, launch.cleanup)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    create_managed_process_session_userdata(lua, launch.core, Some(cleanup), Some(session_id))
}

/// Open one managed Node persistent session through the shared process userdata.
/// 通过共享进程 userdata 打开一个受管 Node 持久会话。
fn open_managed_node_session(
    lua: &Lua,
    spec: LuaValue,
    default_encoding: RuntimeTextEncoding,
) -> Result<AnyUserData, mlua::Error> {
    // Stable API name used by validation and launch diagnostics.
    // 校验与启动诊断使用的稳定 API 名称。
    let api_name = "vulcan.runtime.node.session.open";
    ensure_persistent_managed_runtime_session_platform_supported()
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Trusted System Plugin package whose lease owns every persistent session.
    // 其租约拥有每个长期会话的可信 System Plugin 包。
    let package = current_lua_managed_package_context(lua, api_name)?;
    // Engine-owned resource and event lifecycle service.
    // 引擎拥有的资源与事件生命周期服务。
    let services = current_lua_managed_runtime_services(lua, api_name)?;
    // Bound System identity is checked before transaction lookup for one stable package-kind error.
    // 在查询事务前检查已绑定 System 身份，以获得稳定的包类型错误。
    let event_identity = system_managed_runtime_session_event_identity(package.as_ref(), api_name)?;
    // Active evaluation transaction required for deterministic rollback on any Lua failure.
    // 为确保任何 Lua 失败都能确定性回滚而要求的活动执行事务。
    let transaction = optional_lua_managed_runtime_transaction(lua).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "{api_name}: persistent managed sessions require an active System lease evaluation"
        ))
    })?;
    // Pre-launch reservation registered before any child output can occur.
    // 在任何子进程输出发生前注册的启动前预留。
    let reservation = services
        .reserve_session(
            package.owner_token(),
            Some(transaction),
            Some(event_identity),
        )
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Optional package-agnostic observer publishing durable System events.
    // 发布持久 System 事件的可选包无关观察器。
    let observer = reservation.observer();
    // Strict session launch request parsed from Lua.
    // 从 Lua 解析得到的严格会话启动请求。
    let request = parse_managed_runtime_session_open_request(spec, api_name, default_encoding)?;
    // Pure Rust launch result owning the process tree and unique Node snapshot cleanup.
    // 拥有进程树与唯一 Node 快照清理的纯 Rust 启动结果。
    let launch = launch_managed_node_session(package.as_ref(), request, observer)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Live session id activated only after the process core is fully constructed.
    // 仅在进程核心完整构造后激活的活动会话 id。
    let (session_id, cleanup) = reservation
        .activate(&launch.core, launch.cleanup)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    create_managed_process_session_userdata(lua, launch.core, Some(cleanup), Some(session_id))
}

/// Default maximum number of warm workers kept for one managed runtime pool key.
/// 单个受管运行时池键默认保留的最大热 worker 数量。
const MANAGED_RUNTIME_WORKER_POOL_MAX_SIZE: usize = 4;

/// Default idle time before one excess managed runtime worker can be retired.
/// 多余受管运行时 worker 默认可回收的空闲时间。
const MANAGED_RUNTIME_WORKER_IDLE_TTL_SECS: u64 = 60;
/// Maximum best-effort wait for one worker child or its pipe readers during teardown.
/// Worker 清理期间等待单个子进程或其管道读取器的最大尽力时长。
const MANAGED_RUNTIME_WORKER_TEARDOWN_TIMEOUT_SECS: u64 = 2;
/// Maximum UTF-8 bytes accepted for one line-delimited Worker protocol record.
/// 单条逐行 Worker 协议记录允许的最大 UTF-8 字节数。
const MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum queued protocol lines per Worker stream before reader backpressure is applied.
/// 应用读取器背压前每个 Worker 流最多排队的协议行数。
const MANAGED_RUNTIME_WORKER_CHANNEL_CAPACITY: usize = 4;

/// Resolved invocation request for one managed child runtime.
/// 单个受管子运行时的已解析调用请求。
struct ManagedRuntimeInvokeRequest {
    /// Skill-relative source file requested by Lua.
    /// Lua 请求的 skill 相对源文件。
    file: String,
    /// Exported handler name inside the source file.
    /// 源文件内部导出的处理函数名称。
    handler: String,
    /// JSON arguments converted from the Lua request table.
    /// 从 Lua 请求表转换得到的 JSON 参数。
    args: Value,
    /// Optional timeout in milliseconds for the child process.
    /// 子进程可选超时时间，单位为毫秒。
    timeout_ms: Option<u64>,
}

/// Stable key that partitions managed runtime workers by runtime, environment, and package.
/// 按运行时、环境与包隔离受管运行时 Worker 的稳定键。
#[derive(Debug, Clone)]
struct ManagedRuntimeWorkerKey {
    /// Runtime kind name such as `python` or `node`.
    /// 运行时类型名称，例如 `python` 或 `node`。
    runtime: String,
    /// Environment identity hash produced from dependency inputs.
    /// 基于依赖输入生成的环境身份哈希。
    env_hash: String,
    /// Canonical environment directory preventing cross-runtime-root hash collisions.
    /// 防止跨运行时根哈希碰撞的规范环境目录。
    env_dir: PathBuf,
    /// Stable trusted package identity isolating module globals across packages.
    /// 在不同包之间隔离模块全局状态的稳定可信包身份。
    package_identity: ManagedRuntimePackageIdentity,
    /// Exact package-lifetime token preventing reuse after reload, close, or replace.
    /// 防止在重载、关闭或替换后复用的精确包生命周期令牌。
    owner_token: u64,
    /// Weak retirement state that rejects stale in-flight VM calls without retaining the package.
    /// 在不保留包的前提下拒绝陈旧执行中 VM 调用的弱退役状态。
    owner_state: Weak<ManagedRuntimeOwnerState>,
}

impl ManagedRuntimeWorkerKey {
    /// Verify that the exact package owner is still live and has not crossed retirement.
    /// 验证精确包所有者仍然存活且尚未跨越退役边界。
    ///
    /// The function has no parameters and returns a stable lifecycle error for stale requests.
    /// 此函数不接收参数，并针对陈旧请求返回稳定生命周期错误。
    fn ensure_owner_active(&self) -> Result<(), String> {
        let owner_state = self.owner_state.upgrade().ok_or_else(|| {
            format!(
                "managed runtime worker owner `{}` is no longer active",
                self.owner_token
            )
        })?;
        if owner_state.is_retired() {
            return Err(format!(
                "managed runtime worker owner `{}` is retired",
                self.owner_token
            ));
        }
        Ok(())
    }
}

impl PartialEq for ManagedRuntimeWorkerKey {
    /// Compare stable partition fields while intentionally excluding the weak lifecycle handle.
    /// 比较稳定分区字段，同时有意排除弱生命周期句柄。
    ///
    /// `other` is another worker key; the result is true only for the same exact pool partition.
    /// `other` 是另一个 Worker 键；仅精确属于同一池分区时结果为 true。
    fn eq(&self, other: &Self) -> bool {
        self.runtime == other.runtime
            && self.env_hash == other.env_hash
            && self.env_dir == other.env_dir
            && self.package_identity == other.package_identity
            && self.owner_token == other.owner_token
    }
}

impl Eq for ManagedRuntimeWorkerKey {}

impl Hash for ManagedRuntimeWorkerKey {
    /// Hash the same stable fields used by equality and exclude the non-identity weak handle.
    /// 对与相等比较一致的稳定字段求哈希，并排除不属于身份的弱句柄。
    ///
    /// `state` receives deterministic partition bytes; this function returns no value.
    /// `state` 接收确定性的分区字节；此函数不返回值。
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.runtime.hash(state);
        self.env_hash.hash(state);
        self.env_dir.hash(state);
        self.package_identity.hash(state);
        self.owner_token.hash(state);
    }
}

/// Result returned by one managed runtime worker invocation.
/// 单个受管运行时 worker 调用返回的结果。
struct ManagedRuntimeWorkerInvokeResult {
    /// JSON envelope emitted by the worker.
    /// worker 发出的 JSON 信封。
    envelope: Value,
    /// Whether the process was killed because it exceeded the timeout.
    /// 进程是否因为超过超时限制而被终止。
    timed_out: bool,
    /// Whether the worker process was reused from a previous invocation.
    /// worker 进程是否复用了之前的调用实例。
    worker_reused: bool,
    /// Whether the worker should be discarded instead of returned to the pool.
    /// worker 是否应被丢弃而不是归还给池。
    discard_worker: bool,
}

/// Inseparable direct-child ownership and its pre-reserved final-reaper capacity.
/// 不可分割的直接子进程所有权及其预留最终回收容量。
struct ManagedRuntimeWorkerChildOwnership {
    /// Direct child retained until definitive reap or static-reaper handoff.
    /// 保留到确定回收或静态回收器交接为止的直接子进程。
    child: Child,
    /// Capacity guaranteeing that final handoff cannot fail after the process exists.
    /// 保证进程存在后最终交接不会失败的容量许可。
    reaper_permit: DetachedChildReaperPermit,
}

/// Worker process kept warm for repeated managed runtime invocations.
/// 为重复受管运行时调用保持热状态的 worker 进程。
struct ManagedRuntimeWorker {
    /// Atomic child/permit ownership retained through final teardown.
    /// 贯穿最终清理阶段保留的原子子进程与许可所有权。
    child_ownership: Option<ManagedRuntimeWorkerChildOwnership>,
    /// Full process-tree controller retained until the worker is destroyed.
    /// 保留到 Worker 销毁为止的完整进程树控制器。
    process_tree: ManagedChildProcessTree,
    /// Optional immutable package snapshot retained for the complete worker lifetime.
    /// 在完整 Worker 生命周期内保留的可选不可变包快照。
    package_snapshot: Option<Arc<ManagedPackageSnapshot>>,
    /// Writable stdin pipe used to send line-delimited JSON requests.
    /// 用于发送按行分隔 JSON 请求的可写标准输入管道。
    stdin: ChildStdin,
    /// Receiver for line-delimited stdout envelopes.
    /// 接收按行分隔 stdout 信封的通道。
    stdout_rx: mpsc::Receiver<Result<String, String>>,
    /// Receiver for line-delimited stderr messages.
    /// 接收按行分隔 stderr 消息的通道。
    stderr_rx: mpsc::Receiver<Result<String, String>>,
    /// Background stdout reader handle.
    /// stdout 后台读取线程句柄。
    stdout_reader: Option<thread::JoinHandle<()>>,
    /// Background stderr reader handle.
    /// stderr 后台读取线程句柄。
    stderr_reader: Option<thread::JoinHandle<()>>,
    /// Last time this worker was used.
    /// 当前 worker 最近一次被使用的时间。
    last_used_at: Instant,
    /// Whether this worker was freshly spawned and has not yet served an invocation.
    /// 当前 worker 是否为新启动且尚未服务过调用。
    fresh: bool,
}

/// Mutable bucket for one managed runtime worker pool key.
/// 单个受管运行时 worker 池键对应的可变桶。
struct ManagedRuntimeWorkerBucket {
    /// Available workers that can be reused immediately.
    /// 可立即复用的空闲 worker。
    available: Vec<ManagedRuntimeWorker>,
    /// Total workers currently assigned to this key.
    /// 当前分配给该键的 worker 总数。
    total_count: usize,
}

/// Lock-scoped worker checkout decision completed before any process creation.
/// 在任何进程创建前于锁作用域内完成的 Worker 借出决策。
enum ManagedRuntimeWorkerCheckout {
    /// Existing idle worker detached for immediate reuse.
    /// 为立即复用而分离的既有空闲 Worker。
    Reused(Box<ManagedRuntimeWorker>),
    /// Capacity slot reserved for one worker that must be spawned outside the pool lock.
    /// 为必须在池锁外启动的 Worker 预留的容量槽。
    SpawnReserved,
}

/// Mutable worker pool owned by one managed runtime worker service.
/// 由单个受管运行时 Worker 服务拥有的可变 Worker 池。
struct ManagedRuntimeWorkerPool {
    /// Worker buckets partitioned by runtime, environment, package, and owner lifetime.
    /// 按运行时、环境、包及所有者生命周期分区的 Worker 桶。
    buckets: HashMap<ManagedRuntimeWorkerKey, ManagedRuntimeWorkerBucket>,
}

/// One shared lazy package snapshot initialization cell.
/// 单个共享的延迟包快照初始化单元。
type ManagedPackageSnapshotCell = Arc<OnceLock<Result<Arc<ManagedPackageSnapshot>, String>>>;

/// Owner-partitioned immutable package snapshots retained by one worker service.
/// 由单个 Worker 服务保留并按所有者分区的不可变包快照。
type ManagedPackageSnapshotMap = HashMap<ManagedRuntimeWorkerKey, ManagedPackageSnapshotCell>;

impl ManagedRuntimeWorkerPool {
    /// Create an empty managed runtime worker pool.
    /// 创建一个空的受管运行时 worker 池。
    fn new() -> Self {
        Self {
            buckets: HashMap::new(),
        }
    }

    /// Reap idle workers beyond the minimum warm count for one key.
    /// 回收单个键下超过最小保温数量的空闲 worker。
    fn reap_idle_locked(bucket: &mut ManagedRuntimeWorkerBucket) -> Vec<ManagedRuntimeWorker> {
        let idle_limit = Duration::from_secs(MANAGED_RUNTIME_WORKER_IDLE_TTL_SECS);
        let now = Instant::now();
        let mut index = 0usize;
        // Detached workers returned to the service for process teardown after unlocking.
        // 返回给服务以便解锁后执行进程清理的分离 Worker。
        let mut retired_workers = Vec::new();
        while index < bucket.available.len() {
            let should_remove = now
                .checked_duration_since(bucket.available[index].last_used_at)
                .map(|idle| idle >= idle_limit)
                .unwrap_or(false);
            if should_remove {
                retired_workers.push(bucket.available.swap_remove(index));
                bucket.total_count = bucket.total_count.saturating_sub(1);
            } else {
                index += 1;
            }
        }
        retired_workers
    }

    /// Reserve one checkout decision without spawning or destroying a process under the pool lock.
    /// 在池锁内预留一次借出决策，但不启动或销毁任何进程。
    ///
    /// `key` identifies the exact environment and package owner requesting a worker.
    /// `key` 标识请求 Worker 的精确环境与包所有者。
    ///
    /// Returns a reuse/spawn decision plus idle workers that must be destroyed after unlocking.
    /// 返回复用或启动决策，以及必须在解锁后销毁的空闲 Worker。
    fn reserve(
        &mut self,
        key: &ManagedRuntimeWorkerKey,
    ) -> Result<(ManagedRuntimeWorkerCheckout, Vec<ManagedRuntimeWorker>), String> {
        key.ensure_owner_active()?;
        let bucket =
            self.buckets
                .entry(key.clone())
                .or_insert_with(|| ManagedRuntimeWorkerBucket {
                    available: Vec::new(),
                    total_count: 0,
                });
        let retired_workers = Self::reap_idle_locked(bucket);
        if let Some(mut worker) = bucket.available.pop() {
            worker.last_used_at = Instant::now();
            return Ok((
                ManagedRuntimeWorkerCheckout::Reused(Box::new(worker)),
                retired_workers,
            ));
        }
        if bucket.total_count >= MANAGED_RUNTIME_WORKER_POOL_MAX_SIZE {
            return Err(format!(
                "managed runtime worker pool is exhausted for this environment; max_size={MANAGED_RUNTIME_WORKER_POOL_MAX_SIZE}"
            ));
        }
        bucket.total_count = bucket
            .total_count
            .checked_add(1)
            .ok_or_else(|| "managed runtime worker pool count exhausted".to_string())?;
        Ok((ManagedRuntimeWorkerCheckout::SpawnReserved, retired_workers))
    }

    /// Return one healthy worker or detach it when its exact owner has retired.
    /// 归还一个健康 Worker；其精确所有者已退役时则将其分离。
    ///
    /// `key` identifies the original checkout bucket and `worker` is the completed worker.
    /// `key` 标识原始借出桶，`worker` 是已完成调用的 Worker。
    ///
    /// Return detached workers that must be destroyed outside the pool lock.
    /// 返回需要在池锁外销毁的分离 Worker 集合。
    fn release(
        &mut self,
        key: ManagedRuntimeWorkerKey,
        mut worker: ManagedRuntimeWorker,
    ) -> Vec<ManagedRuntimeWorker> {
        if key.ensure_owner_active().is_err() {
            return vec![worker];
        }
        worker.last_used_at = Instant::now();
        worker.fresh = false;
        let bucket = self
            .buckets
            .entry(key)
            .or_insert_with(|| ManagedRuntimeWorkerBucket {
                available: Vec::new(),
                total_count: 1,
            });
        if bucket.total_count == 0 {
            bucket.total_count = 1;
        }
        bucket.available.push(worker);
        Self::reap_idle_locked(bucket)
    }

    /// Discard one worker slot for a key after the worker exits or becomes invalid.
    /// 在 worker 退出或失效后丢弃单个键下的 worker 名额。
    fn discard(&mut self, key: &ManagedRuntimeWorkerKey) {
        if let Some(bucket) = self.buckets.get_mut(key) {
            bucket.total_count = bucket.total_count.saturating_sub(1);
            if bucket.total_count == 0 && bucket.available.is_empty() {
                self.buckets.remove(key);
            }
        }
    }

    /// Retire one exact owner and detach every idle worker owned by it.
    /// 退役一个精确所有者，并分离其拥有的全部空闲 Worker。
    ///
    /// `owner_token` is the immutable package-lifetime token being retired.
    /// `owner_token` 是正在退役的不可变包生命周期令牌。
    ///
    /// Return idle workers that must be destroyed after releasing the pool lock.
    /// 返回必须在释放池锁后销毁的空闲 Worker。
    fn retire_owner(&mut self, owner_token: u64) -> Vec<ManagedRuntimeWorker> {
        // Exact bucket keys selected before mutation to keep owner matching deterministic.
        // 在修改前选出的精确桶键，用于保持所有者匹配的确定性。
        let retired_keys: Vec<_> = self
            .buckets
            .keys()
            .filter(|key| key.owner_token == owner_token)
            .cloned()
            .collect();
        // Idle workers detached without running process teardown under the pool lock.
        // 在不于池锁内执行进程清理的前提下分离的空闲 Worker。
        let mut retired_workers = Vec::new();
        for key in retired_keys {
            if let Some(bucket) = self.buckets.remove(&key) {
                retired_workers.extend(bucket.available);
            }
        }
        retired_workers
    }
}

/// Engine-owned service providing one isolated short-lived managed runtime worker pool.
/// 提供单个隔离短期受管运行时 Worker 池的引擎所有服务。
struct ManagedRuntimeWorkerService {
    /// Mutable pool state isolated to the owning engine.
    /// 隔离到所属引擎的可变池状态。
    pool: Mutex<ManagedRuntimeWorkerPool>,
    /// Lazily initialized immutable package snapshots keyed by exact worker ownership.
    /// 按精确 Worker 所有权键控并延迟初始化的不可变包快照。
    package_snapshots: Mutex<ManagedPackageSnapshotMap>,
}

impl ManagedRuntimeWorkerService {
    /// Create one empty engine-owned worker service.
    /// 创建一个空的引擎所有 Worker 服务。
    ///
    /// Return a shared service suitable for injection into every VM of one engine.
    /// 返回适合注入单个引擎全部 VM 的共享服务。
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pool: Mutex::new(ManagedRuntimeWorkerPool::new()),
            package_snapshots: Mutex::new(HashMap::new()),
        })
    }

    /// Return one immutable owner-scoped package snapshot, creating it outside service locks.
    /// 返回一个不可变的所有者作用域包快照，并在服务锁外创建它。
    ///
    /// `key` is the exact Worker key; `plan` and `package` are its authoritative source inputs.
    /// `key` 是精确 Worker 键；`plan` 与 `package` 是其权威源输入。
    ///
    /// Returns the shared snapshot retained by both the service and every spawned Worker.
    /// 返回由服务及每个已启动 Worker 共同保留的共享快照。
    fn package_snapshot(
        &self,
        key: &ManagedRuntimeWorkerKey,
        plan: &ManagedRuntimeEnvPlan,
        package: &ManagedRuntimePackageContext,
    ) -> Result<Arc<ManagedPackageSnapshot>, String> {
        key.ensure_owner_active()?;
        // Once cell selected under lock; recursive package copying happens only after unlock.
        // 在锁内选择 Once 单元；递归包复制仅在解锁后发生。
        let snapshot_cell = {
            let mut snapshots = self.lock_package_snapshots();
            Arc::clone(
                snapshots
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(OnceLock::new())),
            )
        };
        let result = snapshot_cell
            .get_or_init(|| prepare_managed_package_snapshot(plan, package, ".ls-w").map(Arc::new))
            .clone();
        // A failed initialization is not cached for the owner lifetime because filesystem failures
        // can be transient; pointer identity prevents an older waiter from deleting a newer retry cell.
        // 初始化失败不会在整个所有者生命周期内缓存，因为文件系统故障可能是瞬态；指针身份可防止
        // 旧等待方删除更新的重试单元。
        if result.is_err() {
            let mut snapshots = self.lock_package_snapshots();
            if snapshots
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &snapshot_cell))
            {
                snapshots.remove(key);
            }
            return result;
        }
        // Retirement can race snapshot construction while no service lock is held.
        // 退役可能在未持有服务锁的快照构造期间发生竞态。
        if let Err(error) = key.ensure_owner_active() {
            let mut snapshots = self.lock_package_snapshots();
            if snapshots
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &snapshot_cell))
            {
                snapshots.remove(key);
            }
            return Err(error);
        }
        result
    }

    /// Acquire one worker from this engine's isolated pool.
    /// 从当前引擎的隔离池中获取一个 Worker。
    ///
    /// `key` identifies the exact environment and owner; `factory` starts a missing worker.
    /// `key` 标识精确环境与所有者，`factory` 在缺少 Worker 时启动新实例。
    ///
    /// Return the worker together with whether it was reused.
    /// 返回 Worker 及其是否被复用。
    fn acquire<F>(
        &self,
        key: ManagedRuntimeWorkerKey,
        mut factory: F,
    ) -> Result<(ManagedRuntimeWorker, bool), String>
    where
        F: FnMut() -> Result<ManagedRuntimeWorker, String>,
    {
        // Checkout decision and idle detachment finish before any blocking process work.
        // 借出决策与空闲分离会在任何阻塞进程操作前完成。
        let (checkout, retired_workers) = self.lock_pool().reserve(&key)?;
        drop(retired_workers);
        match checkout {
            ManagedRuntimeWorkerCheckout::Reused(worker) => Ok((*worker, true)),
            ManagedRuntimeWorkerCheckout::SpawnReserved => match factory() {
                Ok(mut worker) => {
                    worker.last_used_at = Instant::now();
                    // Retirement may race the unlocked spawn and must invalidate the new worker.
                    // 退役可能与解锁后的启动竞态，因此必须使新 Worker 失效。
                    let owner_retired = {
                        let mut pool = self.lock_pool();
                        if key.ensure_owner_active().is_err() {
                            pool.discard(&key);
                            true
                        } else {
                            false
                        }
                    };
                    if owner_retired {
                        drop(worker);
                        Err(format!(
                            "managed runtime worker owner `{}` retired during worker startup",
                            key.owner_token
                        ))
                    } else {
                        Ok((worker, false))
                    }
                }
                Err(error) => {
                    self.lock_pool().discard(&key);
                    Err(error)
                }
            },
        }
    }

    /// Return one healthy worker, destroying it when its owner retired while it was active.
    /// 归还一个健康 Worker；其所有者在活动期间退役时销毁该 Worker。
    ///
    /// `key` is the original checkout key and `worker` is the completed worker.
    /// `key` 是原始借出键，`worker` 是已完成调用的 Worker。
    fn release(&self, key: ManagedRuntimeWorkerKey, worker: ManagedRuntimeWorker) {
        // Retired worker detached while holding the pool lock and dropped only after unlock.
        // 持有池锁时分离、仅在解锁后释放的已退役 Worker。
        let retired_workers = self.lock_pool().release(key, worker);
        drop(retired_workers);
    }

    /// Discard one invalid worker and release its reserved pool slot.
    /// 丢弃一个无效 Worker 并释放其预留池槽位。
    ///
    /// `key` identifies the checkout accounting entry and `worker` is always destroyed.
    /// `key` 标识借出记账条目，`worker` 始终会被销毁。
    fn discard(&self, key: &ManagedRuntimeWorkerKey, worker: ManagedRuntimeWorker) {
        {
            let mut pool = self.lock_pool();
            pool.discard(key);
        }
        drop(worker);
    }

    /// Retire every worker belonging to one exact package lifetime.
    /// 退役属于一个精确包生命周期的全部 Worker。
    ///
    /// `owner_token` irreversibly marks the live package state before pooled resources are detached.
    /// `owner_token` 在分离池资源前不可逆地标记活动包状态。
    fn retire_owner(&self, owner_token: u64) {
        // Atomic retirement publication closes the race before either service lock is acquired.
        // 原子退役发布会在获取任一服务锁前关闭竞态窗口。
        let _ = retire_managed_runtime_owner_state(owner_token);
        // Idle workers detached atomically before their potentially blocking process teardown.
        // 在可能阻塞的进程清理前以原子方式分离的空闲 Worker。
        let retired_workers = self.lock_pool().retire_owner(owner_token);
        drop(retired_workers);
        // Service ownership is removed after idle workers so active workers alone retain snapshots.
        // 在空闲 Worker 之后移除服务所有权，使活动 Worker 成为快照的唯一保留方。
        let retired_snapshots = {
            let mut snapshots = self.lock_package_snapshots();
            let retired_keys = snapshots
                .keys()
                .filter(|key| key.owner_token == owner_token)
                .cloned()
                .collect::<Vec<_>>();
            retired_keys
                .into_iter()
                .filter_map(|key| snapshots.remove(&key))
                .collect::<Vec<_>>()
        };
        drop(retired_snapshots);
    }

    /// Acquire the isolated pool lock and recover any poisoned state.
    /// 获取隔离池锁，并恢复任何中毒状态。
    ///
    /// Return the mutable pool guard scoped to this service.
    /// 返回作用域限定于当前服务的可变池保护对象。
    fn lock_pool(&self) -> MutexGuard<'_, ManagedRuntimeWorkerPool> {
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquire the package snapshot registry lock and recover any poisoned state.
    /// 获取包快照注册表锁，并恢复任何中毒状态。
    fn lock_package_snapshots(&self) -> MutexGuard<'_, ManagedPackageSnapshotMap> {
        self.package_snapshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for ManagedRuntimeWorkerService {
    /// Destroy every remaining idle worker when the owning engine service is released.
    /// 当所属引擎服务被释放时销毁全部剩余空闲 Worker。
    fn drop(&mut self) {
        // Exclusive pool access available because the final Arc owner is being destroyed.
        // 因最终 Arc 所有者正在销毁而可用的池独占访问。
        let pool = self
            .pool
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.buckets.clear();
        self.package_snapshots
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl Drop for ManagedRuntimeWorker {
    /// Terminate the full worker process tree and bound every teardown wait.
    /// 终止完整 Worker 进程树，并限制每次清理等待。
    fn drop(&mut self) {
        if let Some(ManagedRuntimeWorkerChildOwnership {
            mut child,
            reaper_permit,
        }) = self.child_ownership.take()
        {
            let (errors, reaped) =
                terminate_managed_runtime_worker_child_bounded(&mut child, &self.process_tree);
            for error in errors {
                log_warn(format!(
                    "[LuaSkill:warn] managed runtime worker teardown failed: {error}"
                ));
            }
            if reaped {
                self.process_tree.clear_detached_guard();
                drop(reaper_permit);
            } else {
                let keepalive = self
                    .package_snapshot
                    .take()
                    .map(|snapshot| Box::new(move || drop(snapshot)) as Box<dyn FnOnce() + Send>);
                self.process_tree
                    .handoff_to_reaper(reaper_permit, child, keepalive);
            }
        }
        let reader_deadline = Instant::now()
            .checked_add(Duration::from_secs(
                MANAGED_RUNTIME_WORKER_TEARDOWN_TIMEOUT_SECS,
            ))
            .unwrap_or_else(Instant::now);
        if let Some(handle) = self.stdout_reader.take() {
            join_managed_runtime_reader_bounded(handle, "stdout", reader_deadline);
        }
        if let Some(handle) = self.stderr_reader.take() {
            join_managed_runtime_reader_bounded(handle, "stderr", reader_deadline);
        }
    }
}

/// Terminate and reap one Worker process tree without any unbounded wait.
/// 在不进行任何无界等待的前提下终止并回收一棵 Worker 进程树。
///
/// `child` is the direct process handle and `process_tree` owns every platform descendant strategy.
/// `child` 是直接进程句柄，`process_tree` 拥有每个平台的后代进程策略。
///
/// Returns every teardown diagnostic while still attempting all fallback operations.
/// 在仍尝试全部后备操作的同时返回每条清理诊断。
fn terminate_managed_runtime_worker_child_bounded(
    child: &mut Child,
    process_tree: &ManagedChildProcessTree,
) -> (Vec<String>, bool) {
    let mut errors = Vec::new();
    if let Err(error) = process_tree.terminate(child) {
        errors.push(format!("process-tree termination: {error}"));
    }
    // Direct-child kill remains an independent fallback for a partially failed tree strategy.
    // 直接子进程 kill 仍是进程树策略部分失败时的独立后备路径。
    let direct_child_already_reaped = match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => {
            if let Err(error) = child.kill() {
                errors.push(format!("direct-child kill: {error}"));
            }
            false
        }
        Err(error) => {
            errors.push(format!("initial direct-child status probe: {error}"));
            false
        }
    };
    let direct_child_reaped = if direct_child_already_reaped {
        true
    } else {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(
                MANAGED_RUNTIME_WORKER_TEARDOWN_TIMEOUT_SECS,
            ))
            .unwrap_or_else(Instant::now);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    errors.push("direct child did not exit before teardown deadline".to_string());
                    break false;
                }
                Err(error) => {
                    errors.push(format!("direct-child status probe: {error}"));
                    break false;
                }
            }
        }
    };
    // Snapshot ownership may be released only after both the direct Child and authoritative Job
    // accounting converge. A failed tree probe deliberately forces static-reaper handoff.
    // 仅当直接 Child 与权威 Job 记账均收敛后才可释放快照所有权；进程树探测失败会刻意强制
    // 交接静态回收器。
    let descendant_tree_empty = match process_tree.detached_tree_is_empty() {
        Ok(empty) => empty,
        Err(error) => {
            errors.push(format!("process-tree convergence probe: {error}"));
            false
        }
    };
    (errors, direct_child_reaped && descendant_tree_empty)
}

/// Tear down an unpublished worker child and preserve ownership after a bounded timeout.
/// 清理一个尚未发布的 Worker 子进程，并在有界超时后继续保留其所有权。
///
/// `child`, `process_tree`, and `permit` are the inseparable spawn result. Returns teardown
/// diagnostics after either definitive reap or static-reaper handoff.
/// `child`、`process_tree` 与 `permit` 是不可分割的启动结果。确定回收或交接静态回收器后，
/// 返回清理诊断。
fn teardown_unpublished_managed_runtime_worker(
    mut child: Child,
    process_tree: &ManagedChildProcessTree,
    permit: DetachedChildReaperPermit,
    keepalive: Option<Box<dyn FnOnce() + Send>>,
) -> Vec<String> {
    let (errors, reaped) = terminate_managed_runtime_worker_child_bounded(&mut child, process_tree);
    if reaped {
        process_tree.clear_detached_guard();
        drop(permit);
    } else {
        process_tree.handoff_to_reaper(permit, child, keepalive);
    }
    errors
}

/// Join one worker pipe reader only within the shared teardown deadline.
/// 仅在共享清理截止时间内等待一个 Worker 管道读取器。
///
/// `handle` owns the reader thread, `stream_name` labels diagnostics, and `deadline` bounds waiting.
/// `handle` 拥有读取线程，`stream_name` 标识诊断，`deadline` 限制等待。
fn join_managed_runtime_reader_bounded(
    handle: thread::JoinHandle<()>,
    stream_name: &str,
    deadline: Instant,
) {
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        if handle.join().is_err() {
            log_warn(format!(
                "[LuaSkill:warn] managed runtime worker {stream_name} reader panicked"
            ));
        }
    } else {
        // Dropping a still-running handle detaches it so engine teardown can never block forever.
        // 释放仍在运行的句柄会将其分离，使引擎清理永远不会无限阻塞。
        log_warn(format!(
            "[LuaSkill:warn] managed runtime worker {stream_name} reader exceeded teardown deadline and was detached"
        ));
        drop(handle);
    }
}

/// Spawn one bounded line reader that forwards protocol records into a finite channel.
/// 启动一个有界逐行读取器，并将协议记录转发到有限通道。
///
/// `reader` is one child pipe, `sender` applies bounded backpressure, and `stream_name` labels errors.
/// `reader` 是一个子进程管道，`sender` 应用有界背压，`stream_name` 标识错误。
///
/// Returns a join handle or a thread-creation error without panicking.
/// 返回线程 join 句柄，或返回线程创建错误且不触发 panic。
fn spawn_managed_runtime_line_reader<R>(
    reader: R,
    sender: mpsc::SyncSender<Result<String, String>>,
    stream_name: &'static str,
) -> Result<thread::JoinHandle<()>, String>
where
    R: std::io::Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("managed-runtime-worker-{stream_name}"))
        .spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let record = match read_managed_runtime_worker_line(&mut reader) {
                    Ok(Some(record)) => record,
                    Ok(None) => break,
                    Err(error) => Err(format!(
                        "managed runtime worker {stream_name} read failed: {error}"
                    )),
                };
                let terminal_error = record.is_err();
                if sender.send(record).is_err() || terminal_error {
                    break;
                }
            }
        })
        .map_err(|error| format!("spawn managed runtime worker {stream_name} reader: {error}"))
}

/// Read one newline-delimited Worker record while discarding bytes beyond the hard limit.
/// 读取一条换行分隔 Worker 记录，同时丢弃超过硬上限的字节。
///
/// `reader` remains positioned immediately after the consumed newline or EOF.
/// `reader` 在返回时定位于已消费换行符之后或 EOF。
///
/// Returns `None` only for clean EOF before any byte, otherwise one UTF-8 record or bounded error.
/// 仅在读取任何字节前遇到干净 EOF 时返回 `None`，否则返回一条 UTF-8 记录或有界错误。
fn read_managed_runtime_worker_line<R>(
    reader: &mut R,
) -> Result<Option<Result<String, String>>, std::io::Error>
where
    R: BufRead,
{
    // Retained prefix never grows beyond the protocol limit even for a missing newline.
    // 即使缺少换行符，保留前缀也永远不会超过协议上限。
    let mut retained = Vec::new();
    let mut saw_input = false;
    let mut overflowed = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !saw_input {
                return Ok(None);
            }
            break;
        }
        saw_input = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let content_len = newline.unwrap_or(consumed);
        if !overflowed {
            let remaining =
                MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES.saturating_sub(retained.len());
            let copied = content_len.min(remaining);
            retained.extend_from_slice(&available[..copied]);
            overflowed = copied < content_len;
        }
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if retained.last() == Some(&b'\r') {
        retained.pop();
    }
    if overflowed {
        return Ok(Some(Err(format!(
            "managed runtime worker protocol line exceeded {MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES} bytes"
        ))));
    }
    match String::from_utf8(retained) {
        Ok(line) => Ok(Some(Ok(line))),
        Err(error) => Ok(Some(Err(format!(
            "managed runtime worker protocol line is not valid UTF-8: {error}"
        )))),
    }
}

/// Build one worker-pool key from a managed runtime plan and trusted package identity.
/// 根据受管运行时计划与可信包身份构造 Worker 池键。
fn managed_runtime_worker_key(
    plan: &ManagedRuntimeEnvPlan,
    package: &ManagedRuntimePackageContext,
) -> ManagedRuntimeWorkerKey {
    ManagedRuntimeWorkerKey {
        runtime: plan.runtime.as_str().to_string(),
        env_hash: plan.env_hash.clone(),
        env_dir: normalize_runtime_root_path(&plan.env_dir),
        package_identity: package.identity().clone(),
        owner_token: package.owner_token(),
        owner_state: package.owner_state(),
    }
}

/// Spawn a new managed runtime worker from a prepared command.
/// 从已准备好的命令启动一个新的受管运行时 worker。
#[cfg(test)]
fn spawn_managed_runtime_worker(command: &mut Command) -> Result<ManagedRuntimeWorker, String> {
    spawn_managed_runtime_worker_with_snapshot(command, None)
}

/// Spawn one Worker while retaining its immutable package snapshot through partial rollback.
/// 启动一个 Worker，并让其不可变包快照跨越部分回滚继续保留。
fn spawn_managed_runtime_worker_with_snapshot(
    command: &mut Command,
    package_snapshot: Option<Arc<ManagedPackageSnapshot>>,
) -> Result<ManagedRuntimeWorker, String> {
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let attach_keepalive = package_snapshot.as_ref().map(|snapshot| {
        let snapshot = Arc::clone(snapshot);
        Box::new(move || drop(snapshot)) as Box<dyn FnOnce() + Send>
    });
    let (mut child, process_tree, reaper_permit) = ManagedChildProcessTree::spawn_with_keepalive(
        command,
        "managed runtime worker",
        attach_keepalive,
    )?;
    let (stdin, stdout, stderr) =
        match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
            _ => {
                let cleanup_errors = teardown_unpublished_managed_runtime_worker(
                    child,
                    &process_tree,
                    reaper_permit,
                    package_snapshot.as_ref().map(|snapshot| {
                        let snapshot = Arc::clone(snapshot);
                        Box::new(move || drop(snapshot)) as Box<dyn FnOnce() + Send>
                    }),
                );
                return Err(if cleanup_errors.is_empty() {
                    "managed runtime worker pipe is unavailable".to_string()
                } else {
                    format!(
                        "managed runtime worker pipe is unavailable; cleanup also failed: {}",
                        cleanup_errors.join("; ")
                    )
                });
            }
        };
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(MANAGED_RUNTIME_WORKER_CHANNEL_CAPACITY);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(MANAGED_RUNTIME_WORKER_CHANNEL_CAPACITY);
    // Reader creation is fallible so an OS thread limit cannot strand the already spawned tree.
    // 读取器创建是可失败的，因此操作系统线程限制不会遗留已经启动的进程树。
    let stdout_reader = match spawn_managed_runtime_line_reader(stdout, stdout_tx, "stdout") {
        Ok(handle) => handle,
        Err(error) => {
            let cleanup_errors = teardown_unpublished_managed_runtime_worker(
                child,
                &process_tree,
                reaper_permit,
                package_snapshot.as_ref().map(|snapshot| {
                    let snapshot = Arc::clone(snapshot);
                    Box::new(move || drop(snapshot)) as Box<dyn FnOnce() + Send>
                }),
            );
            return Err(if cleanup_errors.is_empty() {
                error
            } else {
                format!(
                    "{error}; cleanup also failed: {}",
                    cleanup_errors.join("; ")
                )
            });
        }
    };
    let stderr_reader = match spawn_managed_runtime_line_reader(stderr, stderr_tx, "stderr") {
        Ok(handle) => handle,
        Err(error) => {
            let cleanup_errors = teardown_unpublished_managed_runtime_worker(
                child,
                &process_tree,
                reaper_permit,
                package_snapshot.as_ref().map(|snapshot| {
                    let snapshot = Arc::clone(snapshot);
                    Box::new(move || drop(snapshot)) as Box<dyn FnOnce() + Send>
                }),
            );
            let reader_deadline = Instant::now()
                .checked_add(Duration::from_secs(
                    MANAGED_RUNTIME_WORKER_TEARDOWN_TIMEOUT_SECS,
                ))
                .unwrap_or_else(Instant::now);
            join_managed_runtime_reader_bounded(stdout_reader, "stdout", reader_deadline);
            return Err(if cleanup_errors.is_empty() {
                error
            } else {
                format!(
                    "{error}; cleanup also failed: {}",
                    cleanup_errors.join("; ")
                )
            });
        }
    };
    Ok(ManagedRuntimeWorker {
        child_ownership: Some(ManagedRuntimeWorkerChildOwnership {
            child,
            reaper_permit,
        }),
        process_tree,
        package_snapshot,
        stdin,
        stdout_rx,
        stderr_rx,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
        last_used_at: Instant::now(),
        fresh: true,
    })
}

/// Drain any currently buffered stderr lines from one managed runtime worker.
/// 排空一个受管运行时 worker 当前已缓冲的 stderr 行。
fn drain_managed_runtime_worker_stderr(worker: &ManagedRuntimeWorker) -> String {
    let mut output = String::new();
    let mut dropped_bytes = 0usize;
    while let Ok(record) = worker.stderr_rx.try_recv() {
        let line = match record {
            Ok(line) => line,
            Err(error) => format!("[worker stderr reader error: {error}]"),
        };
        if !output.is_empty() {
            append_bounded_worker_diagnostic(&mut output, "\n", &mut dropped_bytes);
        }
        append_bounded_worker_diagnostic(&mut output, &line, &mut dropped_bytes);
    }
    if dropped_bytes > 0 {
        let marker = format!("\n[worker stderr truncated; dropped_bytes={dropped_bytes}]");
        // Marker replaces the tail if necessary so diagnostics remain within the same hard bound.
        // 必要时标记会替换尾部，使诊断仍保持在同一硬上限内。
        while output.len().saturating_add(marker.len())
            > MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES
        {
            output.pop();
        }
        output.push_str(&marker);
    }
    output
}

/// Append UTF-8 diagnostic text without allowing the destination to exceed the protocol bound.
/// 追加 UTF-8 诊断文本，同时不允许目标超过协议上限。
///
/// `output` is retained text, `text` is the incoming fragment, and `dropped_bytes` accumulates loss.
/// `output` 是保留文本，`text` 是传入片段，`dropped_bytes` 累计丢弃量。
fn append_bounded_worker_diagnostic(output: &mut String, text: &str, dropped_bytes: &mut usize) {
    let remaining = MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES.saturating_sub(output.len());
    if text.len() <= remaining {
        output.push_str(text);
        return;
    }
    let mut retained_bytes = remaining.min(text.len());
    while retained_bytes > 0 && !text.is_char_boundary(retained_bytes) {
        retained_bytes -= 1;
    }
    output.push_str(&text[..retained_bytes]);
    *dropped_bytes = dropped_bytes.saturating_add(text.len().saturating_sub(retained_bytes));
}

/// Build one explicit diagnostic after the managed worker stdout protocol closes unexpectedly.
/// 在受管 Worker stdout 协议意外关闭后构造一条显式诊断。
///
/// `worker` retains the authoritative direct-child handle. Returns a message including its
/// nonblocking exit status probe without converting an observation failure into a panic.
/// `worker` 保留权威直接子进程句柄；返回包含非阻塞退出状态探测的消息，且不会把观察失败
/// 转换为 panic。
fn managed_runtime_worker_stdout_closed_error(worker: &mut ManagedRuntimeWorker) -> String {
    let status = match worker.child_ownership.as_mut() {
        Some(ownership) => match ownership.child.try_wait() {
            Ok(Some(status)) => format!("exited with {status}"),
            Ok(None) => "is still running".to_string(),
            Err(error) => format!("status probe failed: {error}"),
        },
        None => "has no direct-child ownership".to_string(),
    };
    format!("managed runtime worker stdout closed; child {status}")
}

/// Invoke one already leased managed runtime worker with one JSON request.
/// 使用一个已租出的受管运行时 worker 发起一次 JSON 请求。
fn invoke_managed_runtime_worker(
    mut worker: ManagedRuntimeWorker,
    payload: &Value,
    timeout_ms: Option<u64>,
    worker_reused: bool,
) -> (ManagedRuntimeWorker, ManagedRuntimeWorkerInvokeResult) {
    let mut discard_worker = false;
    let mut timed_out = false;
    // Stale raw stderr proves a prior protocol cycle was not clean and prevents unsafe reuse.
    // 陈旧原始 stderr 证明上一协议周期不干净，因此阻止不安全复用。
    let stale_stderr = drain_managed_runtime_worker_stderr(&worker);
    if !stale_stderr.is_empty() {
        let envelope = json!({
            "ok": false,
            "value": null,
            "stdout": "",
            "stderr": stale_stderr,
            "error": "managed runtime worker produced raw stderr outside its response envelope",
        });
        return (
            worker,
            ManagedRuntimeWorkerInvokeResult {
                envelope,
                timed_out,
                worker_reused,
                discard_worker: true,
            },
        );
    }
    let payload_text = match serde_json::to_string(payload) {
        Ok(payload_text) => payload_text,
        Err(error) => {
            discard_worker = true;
            let envelope = json!({
                "ok": false,
                "value": null,
                "stdout": "",
                "stderr": "",
                "error": format!("failed to serialize managed runtime request: {error}"),
            });
            return (
                worker,
                ManagedRuntimeWorkerInvokeResult {
                    envelope,
                    timed_out,
                    worker_reused,
                    discard_worker,
                },
            );
        }
    };
    if let Err(error) = writeln!(worker.stdin, "{payload_text}") {
        discard_worker = true;
        let envelope = json!({
            "ok": false,
            "value": null,
            "stdout": "",
            "stderr": drain_managed_runtime_worker_stderr(&worker),
            "error": format!("failed to write managed runtime worker stdin: {error}"),
        });
        return (
            worker,
            ManagedRuntimeWorkerInvokeResult {
                envelope,
                timed_out,
                worker_reused,
                discard_worker,
            },
        );
    }
    if let Err(error) = worker.stdin.flush() {
        discard_worker = true;
        let envelope = json!({
            "ok": false,
            "value": null,
            "stdout": "",
            "stderr": drain_managed_runtime_worker_stderr(&worker),
            "error": format!("failed to flush managed runtime worker stdin: {error}"),
        });
        return (
            worker,
            ManagedRuntimeWorkerInvokeResult {
                envelope,
                timed_out,
                worker_reused,
                discard_worker,
            },
        );
    }
    let line_result = match timeout_ms {
        Some(timeout_ms) => match worker
            .stdout_rx
            .recv_timeout(Duration::from_millis(timeout_ms))
        {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                discard_worker = true;
                if let Some(child_ownership) = worker.child_ownership.as_mut() {
                    let _ = child_ownership.child.kill();
                }
                Err("managed runtime worker timed out".to_string())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                discard_worker = true;
                Err(managed_runtime_worker_stdout_closed_error(&mut worker))
            }
        },
        None => match worker.stdout_rx.recv() {
            Ok(line) => line,
            Err(_) => {
                discard_worker = true;
                Err(managed_runtime_worker_stdout_closed_error(&mut worker))
            }
        },
    };
    let mut envelope = match line_result {
        Ok(line) => match serde_json::from_str::<Value>(&line) {
            // Worker envelope parsed from the line-delimited stdout protocol.
            // 从按行分隔 stdout 协议解析得到的 worker 信封。
            Ok(envelope) => match validate_managed_runtime_worker_envelope(&envelope) {
                Ok(()) => envelope,
                Err(error) => {
                    discard_worker = true;
                    managed_runtime_worker_protocol_error_envelope(format!(
                        "managed runtime worker returned malformed JSON envelope: {error}"
                    ))
                }
            },
            Err(error) => {
                discard_worker = true;
                json!({
                    "ok": false,
                    "value": null,
                    "stdout": line,
                    "stderr": drain_managed_runtime_worker_stderr(&worker),
                    "error": format!("managed runtime worker returned invalid JSON envelope: {error}"),
                })
            }
        },
        Err(error) => json!({
            "ok": false,
            "value": null,
            "stdout": "",
            "stderr": drain_managed_runtime_worker_stderr(&worker),
            "error": error,
        }),
    };
    // Raw child stderr is surfaced on every cycle and makes the Worker non-reusable.
    // 每个协议周期都会暴露子进程原始 stderr，并使该 Worker 不可复用。
    let raw_stderr = drain_managed_runtime_worker_stderr(&worker);
    if !raw_stderr.is_empty() {
        discard_worker = true;
        merge_managed_runtime_worker_raw_stderr(&mut envelope, &raw_stderr);
    }
    (
        worker,
        ManagedRuntimeWorkerInvokeResult {
            envelope,
            timed_out,
            worker_reused,
            discard_worker,
        },
    )
}

/// Merge bounded raw child stderr into one Worker envelope without growing it without limit.
/// 将有界子进程原始 stderr 合并到一个 Worker 信封中，且不允许其无限增长。
///
/// `envelope` is the current protocol object and `raw_stderr` is already host-side bounded text.
/// `envelope` 是当前协议对象，`raw_stderr` 是已在宿主侧限制大小的文本。
fn merge_managed_runtime_worker_raw_stderr(envelope: &mut Value, raw_stderr: &str) {
    let Some(object) = envelope.as_object_mut() else {
        return;
    };
    let existing = object
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut merged = String::new();
    let mut dropped_bytes = 0usize;
    append_bounded_worker_diagnostic(&mut merged, existing, &mut dropped_bytes);
    if !merged.is_empty() {
        append_bounded_worker_diagnostic(&mut merged, "\n", &mut dropped_bytes);
    }
    append_bounded_worker_diagnostic(&mut merged, raw_stderr, &mut dropped_bytes);
    if dropped_bytes > 0 {
        let marker = format!("\n[combined stderr truncated; dropped_bytes={dropped_bytes}]");
        while merged.len().saturating_add(marker.len())
            > MANAGED_RUNTIME_WORKER_PROTOCOL_LINE_LIMIT_BYTES
        {
            merged.pop();
        }
        merged.push_str(&marker);
    }
    object.insert("stderr".to_string(), Value::String(merged));
}

/// Build one explicit managed runtime worker protocol error envelope.
/// 构建一个显式的受管运行时 worker 协议错误信封。
///
/// The message parameter is the protocol validation error that should be exposed to Lua callers.
/// message 参数是需要暴露给 Lua 调用方的协议校验错误。
///
/// Return a well-formed worker envelope that carries the protocol error without pretending success.
/// 返回一个携带协议错误且不会伪装成功的格式正确 worker 信封。
fn managed_runtime_worker_protocol_error_envelope(message: impl Into<String>) -> Value {
    // Protocol error text normalized before embedding it into the worker envelope.
    // 嵌入 worker 信封之前归一化后的协议错误文本。
    let error = message.into();
    json!({
        "ok": false,
        "value": null,
        "stdout": "",
        "stderr": "",
        "error": error,
    })
}

/// Borrow the JSON object backing one managed runtime worker envelope.
/// 借用单个受管运行时 worker 信封背后的 JSON 对象。
///
/// The envelope parameter is the raw JSON value emitted by the worker process.
/// envelope 参数是 worker 进程发出的原始 JSON 值。
///
/// Return the object map, or an explicit protocol error when the envelope is not an object.
/// 返回对象映射；当信封不是对象时返回显式协议错误。
fn managed_runtime_worker_envelope_object(
    envelope: &Value,
) -> Result<&serde_json::Map<String, Value>, String> {
    envelope
        .as_object()
        .ok_or_else(|| "managed runtime worker envelope must be a JSON object".to_string())
}

/// Read one required field from a managed runtime worker envelope object.
/// 从受管运行时 worker 信封对象中读取一个必填字段。
///
/// The object parameter is the validated envelope object map.
/// object 参数是已校验的信封对象映射。
///
/// The field_name parameter is the required protocol field name.
/// field_name 参数是必填协议字段名。
///
/// Return the field value, or an explicit protocol error when it is absent.
/// 返回字段值；当字段缺失时返回显式协议错误。
fn managed_runtime_worker_required_envelope_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<&'a Value, String> {
    object
        .get(field_name)
        .ok_or_else(|| format!("managed runtime worker envelope field `{field_name}` is required"))
}

/// Read one required boolean field from a managed runtime worker envelope object.
/// 从受管运行时 worker 信封对象中读取一个必填布尔字段。
///
/// The object parameter is the validated envelope object map.
/// object 参数是已校验的信封对象映射。
///
/// The field_name parameter is the required boolean protocol field name.
/// field_name 参数是必填布尔协议字段名。
///
/// Return the boolean field value, or an explicit protocol error when it has the wrong type.
/// 返回布尔字段值；当字段类型错误时返回显式协议错误。
fn managed_runtime_worker_required_bool_envelope_field(
    object: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<bool, String> {
    managed_runtime_worker_required_envelope_field(object, field_name)?
        .as_bool()
        .ok_or_else(|| {
            format!("managed runtime worker envelope field `{field_name}` must be a boolean")
        })
}

/// Read one required string field from a managed runtime worker envelope object.
/// 从受管运行时 worker 信封对象中读取一个必填字符串字段。
///
/// The object parameter is the validated envelope object map.
/// object 参数是已校验的信封对象映射。
///
/// The field_name parameter is the required string protocol field name.
/// field_name 参数是必填字符串协议字段名。
///
/// Return the string slice, or an explicit protocol error when it has the wrong type.
/// 返回字符串切片；当字段类型错误时返回显式协议错误。
fn managed_runtime_worker_required_string_envelope_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<&'a str, String> {
    managed_runtime_worker_required_envelope_field(object, field_name)?
        .as_str()
        .ok_or_else(|| {
            format!("managed runtime worker envelope field `{field_name}` must be a string")
        })
}

/// Validate one optional nullable string field inside a managed runtime worker envelope.
/// 校验受管运行时 worker 信封中的一个可选可空字符串字段。
///
/// The object parameter is the validated envelope object map.
/// object 参数是已校验的信封对象映射。
///
/// The field_name parameter is the optional nullable string protocol field name.
/// field_name 参数是可选可空字符串协议字段名。
///
/// Return unit when the field is absent, null, or a string; otherwise return an explicit protocol error.
/// 当字段缺失、为空或为字符串时返回 unit；否则返回显式协议错误。
fn validate_managed_runtime_worker_optional_nullable_string_field(
    object: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<(), String> {
    match object.get(field_name) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(_)) => Ok(()),
        Some(_) => Err(format!(
            "managed runtime worker envelope field `{field_name}` must be a string or null"
        )),
    }
}

/// Read one optional non-negative byte counter from a Worker envelope, defaulting to zero.
/// 从 Worker 信封读取一个可选非负字节计数器，缺失时默认为零。
///
/// `object` is the validated envelope map and `field_name` identifies the counter.
/// `object` 是已校验信封映射，`field_name` 标识计数器。
fn managed_runtime_worker_optional_u64_envelope_field(
    object: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<u64, String> {
    match object.get(field_name) {
        None => Ok(0),
        Some(value) => value.as_u64().ok_or_else(|| {
            format!("managed runtime worker envelope field `{field_name}` must be a non-negative integer")
        }),
    }
}

/// Validate one managed runtime worker JSON envelope before pooling decisions are made.
/// 在作出池化决策之前校验单个受管运行时 worker JSON 信封。
///
/// The envelope parameter is the raw JSON value emitted by the worker process.
/// envelope 参数是 worker 进程发出的原始 JSON 值。
///
/// Return unit when the envelope satisfies the worker protocol, or an explicit protocol error.
/// 当信封满足 worker 协议时返回 unit；否则返回显式协议错误。
fn validate_managed_runtime_worker_envelope(envelope: &Value) -> Result<(), String> {
    // Worker envelope object that owns the protocol fields.
    // 拥有协议字段的 worker 信封对象。
    let object = managed_runtime_worker_envelope_object(envelope)?;
    // Worker success flag that decides whether an error field is required.
    // 决定是否需要错误字段的 worker 成功标志。
    let ok = managed_runtime_worker_required_bool_envelope_field(object, "ok")?;

    managed_runtime_worker_required_envelope_field(object, "value")?;
    managed_runtime_worker_required_string_envelope_field(object, "stdout")?;
    managed_runtime_worker_required_string_envelope_field(object, "stderr")?;
    managed_runtime_worker_optional_u64_envelope_field(object, "stdout_dropped_bytes")?;
    managed_runtime_worker_optional_u64_envelope_field(object, "stderr_dropped_bytes")?;
    validate_managed_runtime_worker_optional_nullable_string_field(object, "trace")?;
    if !ok {
        // Error text required when the worker reports a failed handler invocation.
        // worker 报告 handler 调用失败时必须提供的错误文本。
        let error = managed_runtime_worker_required_string_envelope_field(object, "error")?;
        if error.trim().is_empty() {
            return Err(
                "managed runtime worker envelope field `error` must be a non-empty string when `ok` is false"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Convert one validated worker invocation result into the Lua-facing result payload.
/// 将一个已校验的 worker 调用结果转换为面向 Lua 的返回载荷。
///
/// The result parameter is the worker invocation result after stdout envelope validation.
/// result 参数是 stdout 信封校验后的 worker 调用结果。
///
/// The plan parameter is the managed runtime environment plan that supplied the worker.
/// plan 参数是提供该 worker 的受管运行时环境计划。
///
/// Return the Lua-facing result payload, or an explicit protocol error for malformed envelopes.
/// 返回面向 Lua 的结果载荷；当信封格式错误时返回显式协议错误。
fn managed_runtime_worker_validated_result_to_json(
    result: &ManagedRuntimeWorkerInvokeResult,
    plan: &ManagedRuntimeEnvPlan,
) -> Result<Value, String> {
    validate_managed_runtime_worker_envelope(&result.envelope)?;
    // Worker envelope object after protocol validation has completed.
    // 完成协议校验后的 worker 信封对象。
    let object = managed_runtime_worker_envelope_object(&result.envelope)?;
    // Effective success flag, forced to false when the Rust side timed out.
    // 有效成功标志；Rust 侧发生超时时强制为 false。
    let ok =
        managed_runtime_worker_required_bool_envelope_field(object, "ok")? && !result.timed_out;
    // Worker result value copied from the required protocol field.
    // 从必填协议字段复制得到的 worker 结果值。
    let value = managed_runtime_worker_required_envelope_field(object, "value")?.clone();
    // Worker stdout text copied from the required protocol field.
    // 从必填协议字段复制得到的 worker stdout 文本。
    let stdout = Value::String(
        managed_runtime_worker_required_string_envelope_field(object, "stdout")?.to_string(),
    );
    // Worker stderr text copied from the required protocol field.
    // 从必填协议字段复制得到的 worker stderr 文本。
    let stderr = Value::String(
        managed_runtime_worker_required_string_envelope_field(object, "stderr")?.to_string(),
    );
    // Bounded capture diagnostics report bytes omitted inside the language wrapper.
    // 有界捕获诊断报告语言包装器内部省略的字节数。
    let stdout_dropped_bytes =
        managed_runtime_worker_optional_u64_envelope_field(object, "stdout_dropped_bytes")?;
    let stderr_dropped_bytes =
        managed_runtime_worker_optional_u64_envelope_field(object, "stderr_dropped_bytes")?;
    // Optional worker error value exposed only when the envelope included it.
    // 仅在信封包含时暴露的可选 worker 错误值。
    let error = match object.get("error") {
        Some(value) => value.clone(),
        None => Value::Null,
    };
    // Optional worker trace value exposed only when the envelope included it.
    // 仅在信封包含时暴露的可选 worker trace 值。
    let trace = match object.get("trace") {
        Some(value) => value.clone(),
        None => Value::Null,
    };

    Ok(json!({
        "ok": ok,
        "value": value,
        "stdout": stdout,
        "stderr": stderr,
        "stdout_dropped_bytes": stdout_dropped_bytes,
        "stderr_dropped_bytes": stderr_dropped_bytes,
        "error": error,
        "trace": trace,
        "status": if result.timed_out { Value::Null } else { json!(0) },
        "timed_out": result.timed_out,
        "worker_reused": result.worker_reused,
        "env_hash": plan.env_hash,
        "env_dir": render_host_visible_path(&plan.env_dir),
    }))
}

/// Convert one worker protocol error into the Lua-facing managed runtime result payload.
/// 将单个 worker 协议错误转换为面向 Lua 的受管运行时结果载荷。
///
/// The error parameter is the validation error that explains the malformed worker envelope.
/// error 参数是解释 worker 信封格式错误的校验错误。
///
/// The result parameter carries timeout and reuse metadata from the original invocation.
/// result 参数携带原始调用中的超时与复用元数据。
///
/// The plan parameter is the managed runtime environment plan that supplied the worker.
/// plan 参数是提供该 worker 的受管运行时环境计划。
///
/// Return a structured failure payload that preserves worker metadata and exposes the protocol error.
/// 返回保留 worker 元数据并暴露协议错误的结构化失败载荷。
fn managed_runtime_worker_protocol_result_json(
    error: String,
    result: &ManagedRuntimeWorkerInvokeResult,
    plan: &ManagedRuntimeEnvPlan,
) -> Value {
    json!({
        "ok": false,
        "value": null,
        "stdout": "",
        "stderr": "",
        "stdout_dropped_bytes": 0,
        "stderr_dropped_bytes": 0,
        "error": error,
        "trace": Value::Null,
        "status": if result.timed_out { Value::Null } else { json!(0) },
        "timed_out": result.timed_out,
        "worker_reused": result.worker_reused,
        "env_hash": plan.env_hash,
        "env_dir": render_host_visible_path(&plan.env_dir),
    })
}

/// Convert one worker invocation result into the Lua-facing result payload.
/// 将 worker 调用结果转换为面向 Lua 的返回载荷。
///
/// The result parameter is the normalized worker invocation result produced by the line protocol.
/// result 参数是逐行协议产出的已归一化 worker 调用结果。
///
/// The plan parameter is the managed runtime environment plan that supplied the worker.
/// plan 参数是提供该 worker 的受管运行时环境计划。
///
/// Return the Lua-facing payload, using an explicit protocol-error payload for malformed envelopes.
/// 返回面向 Lua 的载荷；当信封格式错误时使用显式协议错误载荷。
fn managed_runtime_worker_result_to_json(
    result: ManagedRuntimeWorkerInvokeResult,
    plan: &ManagedRuntimeEnvPlan,
) -> Value {
    match managed_runtime_worker_validated_result_to_json(&result, plan) {
        Ok(value) => value,
        Err(error) => managed_runtime_worker_protocol_result_json(error, &result, plan),
    }
}

/// Return the Python executable inside a managed virtual environment.
/// 返回受管虚拟环境内部的 Python 可执行文件。
fn managed_python_venv_executable(plan: &ManagedRuntimeEnvPlan) -> PathBuf {
    if cfg!(windows) {
        plan.env_dir
            .join(".venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        plan.env_dir.join(".venv").join("bin").join("python")
    }
}

/// Parse a Lua invocation table for `vulcan.runtime.python/node.invoke`.
/// 解析 `vulcan.runtime.python/node.invoke` 使用的 Lua 调用表。
fn parse_managed_runtime_invoke_request(
    value: LuaValue,
    api_name: &str,
    default_handler: &str,
) -> Result<ManagedRuntimeInvokeRequest, mlua::Error> {
    let table = require_table_arg(value, api_name, "spec")?;
    let file = managed_runtime_optional_string_field(&table, api_name, "file")?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| mlua::Error::runtime(format!("{api_name}: file is required")))?;
    let handler = managed_runtime_optional_string_field(&table, api_name, "handler")?
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_handler.to_string());
    let args_value: LuaValue = table
        .get("args")
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    let args = match args_value {
        LuaValue::Nil => Value::Object(serde_json::Map::new()),
        other => lua_value_to_json(&other)
            .map_err(|error| mlua::Error::runtime(format!("{api_name}: args: {error}")))?,
    };
    let timeout_ms = managed_runtime_optional_timeout_ms(&table, api_name)?;
    Ok(ManagedRuntimeInvokeRequest {
        file,
        handler,
        args,
        timeout_ms,
    })
}

/// Build a status JSON value for one resolved managed runtime plan.
/// 为一个已解析受管运行时计划构造状态 JSON 值。
///
/// The plan parameter is the resolved managed runtime environment plan to inspect.
/// plan 参数是需要检查的已解析受管运行时环境计划。
///
/// Return a status object that preserves readiness-check errors instead of hiding them as not-ready state.
/// 返回一个保留就绪检查错误的状态对象，而不是把错误隐藏成未就绪状态。
fn managed_runtime_status_from_plan(plan: &ManagedRuntimeEnvPlan) -> Value {
    // Side-effect-free target capability included regardless of environment readiness.
    // 无论环境是否就绪都包含的无副作用目标能力。
    let persistent_session = current_managed_runtime_persistent_session_capability();
    // Readiness check result returned by inspecting the managed environment marker.
    // 通过检查受管环境标记得到的就绪检测结果。
    let readiness_result = managed_env_is_ready(plan);
    match readiness_result {
        Ok(ready) => json!({
            "available": true,
            "configured": true,
            "ready": ready,
            "runtime": plan.runtime.as_str(),
            "runtime_version": plan.runtime_version,
            "runtime_executable": render_host_visible_path(&plan.runtime_executable),
            "package_manager": plan.package_manager,
            "package_manager_version": plan.package_manager_version,
            "package_manager_executable": render_host_visible_path(&plan.package_manager_executable),
            "env_hash": plan.env_hash,
            "env_dir": render_host_visible_path(&plan.env_dir),
            "persistent_session": persistent_session,
            "message": if ready {
                "managed runtime environment is ready"
            } else {
                "managed runtime environment is configured but not yet created"
            },
        }),
        Err(error) => json!({
            "available": true,
            "configured": true,
            "ready": false,
            "runtime": plan.runtime.as_str(),
            "runtime_version": plan.runtime_version,
            "runtime_executable": render_host_visible_path(&plan.runtime_executable),
            "package_manager": plan.package_manager,
            "package_manager_version": plan.package_manager_version,
            "package_manager_executable": render_host_visible_path(&plan.package_manager_executable),
            "env_hash": plan.env_hash,
            "env_dir": render_host_visible_path(&plan.env_dir),
            "persistent_session": persistent_session,
            "message": "managed runtime environment status check failed",
            "error": error,
        }),
    }
}

/// Return the Python worker code that isolates stdout/stderr and emits line-delimited JSON envelopes.
/// 返回隔离标准输出/标准错误并发出按行分隔 JSON 信封的 Python worker 代码。
fn managed_python_worker_source() -> &'static str {
    r#"
import contextlib
import importlib.util
import io
import json
import sys
import traceback

MODULE_CACHE = {}
ACTIVE_SNAPSHOT_ROOT = None
MAX_CAPTURE_BYTES = 256 * 1024
MAX_ENVELOPE_BYTES = 2 * 1024 * 1024

class BoundedTextCapture(io.TextIOBase):
    """Retain a bounded UTF-8 prefix while accounting for dropped bytes.
    保留有界 UTF-8 前缀并统计丢弃字节。"""

    def __init__(self):
        """Initialize one empty bounded capture.
        初始化一个空的有界捕获器。"""
        self._bytes = bytearray()
        self.dropped_bytes = 0

    def write(self, value):
        """Append text without allowing retained memory to exceed the hard limit.
        追加文本且不允许保留内存超过硬上限。"""
        text = str(value)
        encoded = text.encode("utf-8", errors="replace")
        remaining = max(0, MAX_CAPTURE_BYTES - len(self._bytes))
        retained = min(remaining, len(encoded))
        self._bytes.extend(encoded[:retained])
        self.dropped_bytes += len(encoded) - retained
        return len(text)

    def flush(self):
        """Provide the TextIO flush contract without external buffering.
        提供 TextIO flush 契约且不使用外部缓冲。"""
        return None

    def getvalue(self):
        """Decode the retained UTF-8 prefix for the response envelope.
        为响应信封解码保留的 UTF-8 前缀。"""
        return bytes(self._bytes).decode("utf-8", errors="replace")

def encode_envelope(envelope, stdout_buffer, stderr_buffer):
    """Serialize one bounded response and replace oversized or invalid values with an error.
    序列化一个有界响应，并用错误替换过大或无效的值。"""
    try:
        encoded = json.dumps(envelope, ensure_ascii=False, separators=(",", ":"))
        if len(encoded.encode("utf-8")) <= MAX_ENVELOPE_BYTES:
            return encoded
        error = f"managed runtime worker envelope exceeded {MAX_ENVELOPE_BYTES} bytes"
    except Exception as exc:
        error = f"handler result is not JSON serializable: {exc}"
    fallback = {
        "ok": False,
        "value": None,
        "stdout": stdout_buffer.getvalue(),
        "stderr": stderr_buffer.getvalue(),
        "stdout_dropped_bytes": stdout_buffer.dropped_bytes,
        "stderr_dropped_bytes": stderr_buffer.dropped_bytes,
        "error": error,
        "trace": traceback.format_exc(),
    }
    return json.dumps(fallback, ensure_ascii=False, separators=(",", ":"))

def configure_snapshot_root(request):
    """Install exactly one immutable package import root for this pooled worker.
    为当前池化 Worker 安装唯一不可变包导入根。"""
    global ACTIVE_SNAPSHOT_ROOT
    snapshot_root = request.get("snapshot_root")
    if not isinstance(snapshot_root, str) or not snapshot_root:
        raise RuntimeError("managed Python worker snapshot_root must be a non-empty string")
    if ACTIVE_SNAPSHOT_ROOT is None:
        sys.path[:] = [entry for entry in sys.path if entry not in ("", snapshot_root)]
        sys.path.insert(0, snapshot_root)
        ACTIVE_SNAPSHOT_ROOT = snapshot_root
    elif ACTIVE_SNAPSHOT_ROOT != snapshot_root:
        raise RuntimeError("managed Python worker snapshot_root changed within one worker")

def handle(request):
    stdout_buffer = BoundedTextCapture()
    stderr_buffer = BoundedTextCapture()
    try:
        configure_snapshot_root(request)
        file_path = request["file"]
        module = MODULE_CACHE.get(file_path)
        if module is None:
            spec = importlib.util.spec_from_file_location("_luaskills_python_entry_" + str(len(MODULE_CACHE)), file_path)
            if spec is None or spec.loader is None:
                raise RuntimeError(f"failed to load Python module: {file_path}")
            module = importlib.util.module_from_spec(spec)
            with contextlib.redirect_stdout(stdout_buffer), contextlib.redirect_stderr(stderr_buffer):
                spec.loader.exec_module(module)
            MODULE_CACHE[file_path] = module
        handler = getattr(module, request.get("handler") or "main")
        with contextlib.redirect_stdout(stdout_buffer), contextlib.redirect_stderr(stderr_buffer):
            value = handler(request.get("args") or {}, request.get("ctx") or {})
        envelope = {
            "ok": True,
            "value": value,
            "stdout": stdout_buffer.getvalue(),
            "stderr": stderr_buffer.getvalue(),
            "stdout_dropped_bytes": stdout_buffer.dropped_bytes,
            "stderr_dropped_bytes": stderr_buffer.dropped_bytes,
        }
    except Exception as exc:
        envelope = {
            "ok": False,
            "value": None,
            "stdout": stdout_buffer.getvalue(),
            "stderr": stderr_buffer.getvalue(),
            "stdout_dropped_bytes": stdout_buffer.dropped_bytes,
            "stderr_dropped_bytes": stderr_buffer.dropped_bytes,
            "error": str(exc),
            "trace": traceback.format_exc(),
        }
    return encode_envelope(envelope, stdout_buffer, stderr_buffer)

for line in sys.stdin:
    try:
        request = json.loads(line)
        encoded = handle(request)
    except Exception as exc:
        encoded = json.dumps({
            "ok": False,
            "value": None,
            "stdout": "",
            "stderr": "",
            "stdout_dropped_bytes": 0,
            "stderr_dropped_bytes": 0,
            "error": str(exc),
            "trace": traceback.format_exc(),
        }, ensure_ascii=False, separators=(",", ":"))
    sys.stdout.write(encoded + "\n")
    sys.stdout.flush()
"#
}

/// Return the Node.js worker code that isolates console output and emits line-delimited JSON envelopes.
/// 返回隔离 console 输出并发出按行分隔 JSON 信封的 Node.js worker 代码。
fn managed_node_worker_source() -> &'static str {
    r#"
const readline = require("readline");
const { pathToFileURL } = require("url");

const moduleCache = new Map();
const MAX_CAPTURE_BYTES = 256 * 1024;
const MAX_ENVELOPE_BYTES = 2 * 1024 * 1024;

// Create one bounded UTF-8 capture with explicit dropped-byte accounting.
// 创建一个带显式丢弃字节统计的有界 UTF-8 捕获器。
function createBoundedCapture() {
  const chunks = [];
  let retainedBytes = 0;
  let droppedBytes = 0;
  let lineCount = 0;
  return {
    write(value) {
      const encoded = Buffer.from(String(value), "utf8");
      const remaining = Math.max(0, MAX_CAPTURE_BYTES - retainedBytes);
      const retained = Math.min(remaining, encoded.length);
      if (retained > 0) {
        chunks.push(encoded.subarray(0, retained));
        retainedBytes += retained;
      }
      droppedBytes += encoded.length - retained;
    },
    writeLine(value) {
      if (lineCount > 0) {
        this.write("\n");
      }
      this.write(value);
      lineCount += 1;
    },
    text() {
      return Buffer.concat(chunks, retainedBytes).toString("utf8");
    },
    droppedBytes() {
      return droppedBytes;
    },
  };
}

// Serialize one bounded response and replace oversized or invalid values with an error.
// 序列化一个有界响应，并用错误替换过大或无效的值。
function encodeEnvelope(envelope, stdout, stderr) {
  try {
    const encoded = JSON.stringify(envelope);
    if (Buffer.byteLength(encoded, "utf8") <= MAX_ENVELOPE_BYTES) {
      return encoded;
    }
    throw new Error(`managed runtime worker envelope exceeded ${MAX_ENVELOPE_BYTES} bytes`);
  } catch (error) {
    return JSON.stringify({
      ok: false,
      value: null,
      stdout: stdout.text(),
      stderr: stderr.text(),
      stdout_dropped_bytes: stdout.droppedBytes(),
      stderr_dropped_bytes: stderr.droppedBytes(),
      error: error && error.message ? error.message : String(error),
      trace: error && error.stack ? error.stack : null,
    });
  }
}

async function handle(request) {
  const stdout = createBoundedCapture();
  const stderr = createBoundedCapture();
  // Preserve only the four methods replaced below. Copying and assigning the complete native
  // Console object also rewrites its internal stream state and can crash Node 18.
  // 仅保留下方会替换的四个方法。复制并回写完整原生 Console 对象还会改写其内部流状态，
  // 并可能导致 Node 18 崩溃。
  const originalConsole = {
    log: console.log,
    info: console.info,
    warn: console.warn,
    error: console.error,
  };
  console.log = (...args) => stdout.writeLine(args.map(String).join(" "));
  console.info = (...args) => stdout.writeLine(args.map(String).join(" "));
  console.warn = (...args) => stderr.writeLine(args.map(String).join(" "));
  console.error = (...args) => stderr.writeLine(args.map(String).join(" "));
  try {
    let module = moduleCache.get(request.file);
    if (!module) {
      module = await import(pathToFileURL(request.file).href);
      moduleCache.set(request.file, module);
    }
    const handlerName = request.handler || "default";
    const handler = handlerName === "default" ? (module.default || module.main) : module[handlerName];
    if (typeof handler !== "function") {
      throw new Error(`handler not found or not callable: ${handlerName}`);
    }
    const value = await handler(request.args || {}, request.ctx || {});
    // JSON.stringify omits undefined object properties, so normalize absent handler results to null.
    // JSON.stringify 会省略 undefined 对象属性，因此将 handler 的空返回归一化为 null。
    const normalizedValue = value === undefined ? null : value;
    return encodeEnvelope({
      ok: true,
      value: normalizedValue,
      stdout: stdout.text(),
      stderr: stderr.text(),
      stdout_dropped_bytes: stdout.droppedBytes(),
      stderr_dropped_bytes: stderr.droppedBytes(),
    }, stdout, stderr);
  } catch (error) {
    return encodeEnvelope({
      ok: false,
      value: null,
      stdout: stdout.text(),
      stderr: stderr.text(),
      stdout_dropped_bytes: stdout.droppedBytes(),
      stderr_dropped_bytes: stderr.droppedBytes(),
      error: error && error.message ? error.message : String(error),
      trace: error && error.stack ? error.stack : null,
    }, stdout, stderr);
  } finally {
    console.log = originalConsole.log;
    console.info = originalConsole.info;
    console.warn = originalConsole.warn;
    console.error = originalConsole.error;
  }
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on("line", async (line) => {
  try {
    const request = JSON.parse(line || "{}");
    process.stdout.write(await handle(request) + "\n");
  } catch (error) {
    process.stdout.write(JSON.stringify({
      ok: false,
      value: null,
      stdout: "",
      stderr: "",
      stdout_dropped_bytes: 0,
      stderr_dropped_bytes: 0,
      error: error && error.message ? error.message : String(error),
      trace: error && error.stack ? error.stack : null,
    }) + "\n");
  }
});
"#
}

/// Prepare one test-only Node package snapshot through the production RAII implementation.
/// 通过生产 RAII 实现准备一个仅用于测试的 Node 包快照。
#[cfg(test)]
fn prepare_managed_node_import_root(
    plan: &ManagedRuntimeEnvPlan,
    package: &ManagedRuntimePackageContext,
) -> Result<ManagedPackageSnapshot, String> {
    prepare_unleased_managed_package_snapshot(plan, package, ".ls-t")
}

/// Recursively copy one package directory into a managed Node.js import root.
/// 将单个包目录递归复制到受管 Node.js import 根目录。
#[cfg(test)]
fn copy_managed_node_package_import_root(source: &Path, destination: &Path) -> Result<(), String> {
    crate::runtime::managed_runtime_session::copy_managed_package_tree(source, destination)
}

/// Invoke one managed runtime payload through a pooled worker.
/// 通过池化 worker 调用一个受管运行时载荷。
fn invoke_pooled_managed_runtime<F>(
    worker_service: &ManagedRuntimeWorkerService,
    key: ManagedRuntimeWorkerKey,
    plan: &ManagedRuntimeEnvPlan,
    payload: &Value,
    timeout_ms: Option<u64>,
    mut factory: F,
) -> Result<Value, String>
where
    F: FnMut() -> Result<ManagedRuntimeWorker, String>,
{
    let (worker, worker_reused) = worker_service.acquire(key.clone(), &mut factory)?;
    let (worker, result) =
        invoke_managed_runtime_worker(worker, payload, timeout_ms, worker_reused);
    let discard_worker = result.discard_worker;
    let result_json = managed_runtime_worker_result_to_json(result, plan);
    if discard_worker {
        worker_service.discard(&key, worker);
    } else {
        worker_service.release(key, worker);
    }
    Ok(result_json)
}

/// Invoke one Python handler through the managed Python environment.
/// 通过受管 Python 环境调用一个 Python 处理函数。
fn invoke_managed_python(lua: &Lua, spec: LuaValue) -> Result<LuaValue, mlua::Error> {
    // Stable API name used by every validation and runtime diagnostic.
    // 所有校验与运行时诊断使用的稳定 API 名称。
    let api_name = "vulcan.runtime.python.invoke";
    // Strict invocation request decoded from the Lua value.
    // 从 Lua 值严格解码得到的调用请求。
    let request = parse_managed_runtime_invoke_request(spec, api_name, "main")?;
    // Trusted package context installed by the engine before entering package code.
    // 引擎在进入包代码前安装的可信包上下文。
    let package = current_lua_managed_package_context(lua, api_name)?;
    // Engine-owned worker service installed in every VM created by this engine.
    // 安装在当前引擎所创建全部 VM 中的引擎所有 Worker 服务。
    let worker_service = current_lua_managed_runtime_worker_service(lua, api_name)?;
    // Authoritative package manifest captured when the package was loaded.
    // 加载包时捕获的权威包清单。
    let manifest = package.dependency_manifest().ok_or_else(|| {
        mlua::Error::runtime(format!("{api_name}: dependencies.yaml is not present"))
    })?;
    // Declared Python runtime specification for this exact package.
    // 当前精确包声明的 Python 运行时规范。
    let runtime_spec = manifest.python_runtime.as_ref().ok_or_else(|| {
        mlua::Error::runtime(format!("{api_name}: python_runtime is not declared"))
    })?;
    // Environment plan resolved only from the trusted package context.
    // 仅从可信包上下文解析得到的环境计划。
    let plan = resolve_python_env_plan(package.as_ref(), runtime_spec)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    ensure_managed_env(&plan)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Canonical source file proven to remain inside the package root.
    // 已证明仍位于包根目录内的规范源码文件。
    let _source_file = package
        .resolve_existing_file(&request.file, "file")
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    let key = managed_runtime_worker_key(&plan, package.as_ref());
    let package_snapshot = worker_service
        .package_snapshot(&key, &plan, package.as_ref())
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    let import_file = package_snapshot
        .worker_source_path(&request.file)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    let snapshot_root = package_snapshot
        .python_worker_import_root()
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Controlled worker payload containing package metadata but no host secrets.
    // 包含包元数据但不包含宿主密钥的受控 Worker 载荷。
    let payload = json!({
        "file": import_file,
        "snapshot_root": snapshot_root,
        "handler": request.handler,
        "args": request.args,
        "ctx": package.worker_context_json(),
    });
    // Package-partitioned worker-pool key.
    // 按包隔离的 Worker 池键。
    // JSON result returned by the managed Python worker.
    // 受管 Python Worker 返回的 JSON 结果。
    let result = invoke_pooled_managed_runtime(
        worker_service.as_ref(),
        key,
        &plan,
        &payload,
        request.timeout_ms,
        || {
            // Command pinned to the environment-specific Python executable.
            // 固定使用环境专属 Python 可执行文件的命令。
            let mut command = Command::new(managed_python_venv_executable(&plan));
            command.arg("-c").arg(managed_python_worker_source());
            package_snapshot.configure_worker_command(&mut command)?;
            configure_managed_python_command_environment(&mut command, &plan)?;
            spawn_managed_runtime_worker_with_snapshot(
                &mut command,
                Some(Arc::clone(&package_snapshot)),
            )
        },
    )
    .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    json_value_to_lua(lua, &result)
}

/// Invoke one Node.js handler through the managed Node.js environment.
/// 通过受管 Node.js 环境调用一个 Node.js 处理函数。
fn invoke_managed_node(lua: &Lua, spec: LuaValue) -> Result<LuaValue, mlua::Error> {
    // Stable API name used by every validation and runtime diagnostic.
    // 所有校验与运行时诊断使用的稳定 API 名称。
    let api_name = "vulcan.runtime.node.invoke";
    // Strict invocation request decoded from the Lua value.
    // 从 Lua 值严格解码得到的调用请求。
    let request = parse_managed_runtime_invoke_request(spec, api_name, "default")?;
    // Trusted package context installed by the engine before entering package code.
    // 引擎在进入包代码前安装的可信包上下文。
    let package = current_lua_managed_package_context(lua, api_name)?;
    // Engine-owned worker service installed in every VM created by this engine.
    // 安装在当前引擎所创建全部 VM 中的引擎所有 Worker 服务。
    let worker_service = current_lua_managed_runtime_worker_service(lua, api_name)?;
    // Authoritative package manifest captured when the package was loaded.
    // 加载包时捕获的权威包清单。
    let manifest = package.dependency_manifest().ok_or_else(|| {
        mlua::Error::runtime(format!("{api_name}: dependencies.yaml is not present"))
    })?;
    // Declared Node runtime specification for this exact package.
    // 当前精确包声明的 Node 运行时规范。
    let runtime_spec = manifest
        .node_runtime
        .as_ref()
        .ok_or_else(|| mlua::Error::runtime(format!("{api_name}: node_runtime is not declared")))?;
    // Environment plan resolved only from the trusted package context.
    // 仅从可信包上下文解析得到的环境计划。
    let plan = resolve_node_env_plan(package.as_ref(), runtime_spec)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    ensure_managed_env(&plan)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Canonical source file validation performed before creating the import snapshot.
    // 创建导入快照前执行的规范源码文件校验。
    let _source_file = package
        .resolve_existing_file(&request.file, "file")
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Exact owner-partitioned worker key also owns the immutable package snapshot identity.
    // 同时拥有不可变包快照身份的精确所有者隔离 Worker 键。
    let key = managed_runtime_worker_key(&plan, package.as_ref());
    // Immutable package snapshot retained by the service and every worker using it.
    // 由服务及每个使用它的 Worker 共同保留的不可变包快照。
    let package_snapshot = worker_service
        .package_snapshot(&key, &plan, package.as_ref())
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Source path inside the validated package snapshot.
    // 已校验包快照内的源码路径。
    let import_file = package_snapshot
        .node_worker_source_path(&request.file)
        .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    // Controlled worker payload containing package metadata but no host secrets.
    // 包含包元数据但不包含宿主密钥的受控 Worker 载荷。
    let payload = json!({
        // Unix uses the validated relative name after object-pinned `fchdir`; Windows uses the
        // validated absolute share-locked source independently from its neutral short cwd.
        // Unix 会在对象固定的 `fchdir` 后使用已校验相对名称；Windows 则独立于中立短 cwd 使用
        // 已校验且受共享锁保护的绝对源码。
        "file": import_file,
        "env_dir": plan.env_dir,
        "handler": request.handler,
        "args": request.args,
        "ctx": package.worker_context_json(),
    });
    // JSON result returned by the managed Node worker.
    // 受管 Node Worker 返回的 JSON 结果。
    let result = invoke_pooled_managed_runtime(
        worker_service.as_ref(),
        key,
        &plan,
        &payload,
        request.timeout_ms,
        || {
            // Command pinned to the declared managed Node executable.
            // 固定使用已声明受管 Node 可执行文件的命令。
            let mut command = Command::new(&plan.runtime_executable);
            command.arg("-e").arg(managed_node_worker_source());
            package_snapshot.configure_worker_command(&mut command)?;
            configure_managed_node_command_environment(&mut command, &plan)?;
            spawn_managed_runtime_worker_with_snapshot(
                &mut command,
                Some(Arc::clone(&package_snapshot)),
            )
        },
    )
    .map_err(|error| mlua::Error::runtime(format!("{api_name}: {error}")))?;
    json_value_to_lua(lua, &result)
}

/// Return status for the active skill's managed Python declaration.
/// 返回当前 skill 的受管 Python 声明状态。
fn managed_python_status(lua: &Lua) -> Result<LuaValue, mlua::Error> {
    // Stable API name used by every validation diagnostic.
    // 所有校验诊断使用的稳定 API 名称。
    let api_name = "vulcan.runtime.python.status";
    // Trusted package context installed by the engine before entering package code.
    // 引擎在进入包代码前安装的可信包上下文。
    let package = current_lua_managed_package_context(lua, api_name)?;
    // Optional package manifest captured at package-load time.
    // 加载包时捕获的可选包清单。
    let Some(manifest) = package.dependency_manifest() else {
        return json_value_to_lua(
            lua,
            &json!({
                "available": false,
                "configured": false,
                "ready": false,
                "runtime": "python",
                "persistent_session": current_managed_runtime_persistent_session_capability(),
                "message": "dependencies.yaml is not present",
            }),
        );
    };
    // Optional Python runtime declaration from the authoritative manifest.
    // 权威清单中的可选 Python 运行时声明。
    let Some(spec) = manifest.python_runtime.as_ref() else {
        return json_value_to_lua(
            lua,
            &json!({
                "available": false,
                "configured": false,
                "ready": false,
                "runtime": "python",
                "persistent_session": current_managed_runtime_persistent_session_capability(),
                "message": "python_runtime is not declared",
            }),
        );
    };
    match resolve_python_env_plan(package.as_ref(), spec) {
        Ok(plan) => json_value_to_lua(lua, &managed_runtime_status_from_plan(&plan)),
        Err(error) => json_value_to_lua(
            lua,
            &json!({
                "available": false,
                "configured": true,
                "ready": false,
                "runtime": "python",
                "persistent_session": current_managed_runtime_persistent_session_capability(),
                "error": error,
            }),
        ),
    }
}

/// Return status for the active skill's managed Node.js declaration.
/// 返回当前 skill 的受管 Node.js 声明状态。
fn managed_node_status(lua: &Lua) -> Result<LuaValue, mlua::Error> {
    // Stable API name used by every validation diagnostic.
    // 所有校验诊断使用的稳定 API 名称。
    let api_name = "vulcan.runtime.node.status";
    // Trusted package context installed by the engine before entering package code.
    // 引擎在进入包代码前安装的可信包上下文。
    let package = current_lua_managed_package_context(lua, api_name)?;
    // Optional package manifest captured at package-load time.
    // 加载包时捕获的可选包清单。
    let Some(manifest) = package.dependency_manifest() else {
        return json_value_to_lua(
            lua,
            &json!({
                "available": false,
                "configured": false,
                "ready": false,
                "runtime": "node",
                "persistent_session": current_managed_runtime_persistent_session_capability(),
                "message": "dependencies.yaml is not present",
            }),
        );
    };
    // Optional Node runtime declaration from the authoritative manifest.
    // 权威清单中的可选 Node 运行时声明。
    let Some(spec) = manifest.node_runtime.as_ref() else {
        return json_value_to_lua(
            lua,
            &json!({
                "available": false,
                "configured": false,
                "ready": false,
                "runtime": "node",
                "persistent_session": current_managed_runtime_persistent_session_capability(),
                "message": "node_runtime is not declared",
            }),
        );
    };
    match resolve_node_env_plan(package.as_ref(), spec) {
        Ok(plan) => json_value_to_lua(lua, &managed_runtime_status_from_plan(&plan)),
        Err(error) => json_value_to_lua(
            lua,
            &json!({
                "available": false,
                "configured": true,
                "ready": false,
                "runtime": "node",
                "persistent_session": current_managed_runtime_persistent_session_capability(),
                "error": error,
            }),
        ),
    }
}

/// Return whether one help payload should be executed as Lua instead of read as plain text.
/// 判断某个帮助载荷是否应按 Lua 执行，而不是按纯文本读取。
fn is_lua_help_file(relative_path: &str) -> bool {
    Path::new(relative_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("lua"))
        .unwrap_or(false)
}

/// Read one UTF-8 text file relative to the skill directory.
/// 读取相对于 skill 目录的一份 UTF-8 文本文件。
fn read_skill_text_file(
    skill_dir: &Path,
    relative_path: &str,
    label: &str,
) -> Result<String, String> {
    let file_path = skill_dir.join(relative_path);
    std::fs::read_to_string(&file_path).map_err(|error| {
        format!(
            "Failed to read {label} file {}: {}",
            render_log_friendly_path(&file_path),
            error
        )
    })
}

/// Source file data required to execute one Lua help payload.
/// 执行单个 Lua 帮助载荷所需的源码文件数据。
struct LuaHelpPayloadSource {
    /// Resolved help file path used for file context and diagnostics.
    /// 用于文件上下文与诊断信息的已解析帮助文件路径。
    helper_path: PathBuf,

    /// UTF-8 Lua source loaded from the help file.
    /// 从帮助文件加载得到的 UTF-8 Lua 源码。
    source: String,
}

/// Read one Lua help payload source file relative to a loaded skill directory.
/// 读取相对于已加载 skill 目录的单个 Lua 帮助载荷源码文件。
///
/// The skill_dir parameter is the loaded skill directory that owns the help file.
/// skill_dir 参数是拥有帮助文件的已加载 skill 目录。
///
/// The relative_path parameter is the manifest-declared help file path.
/// relative_path 参数是 manifest 中声明的帮助文件路径。
///
/// Return the resolved help path together with the loaded Lua source.
/// 返回已解析帮助路径以及已加载的 Lua 源码。
fn read_lua_help_payload_source(
    skill_dir: &Path,
    relative_path: &str,
) -> Result<LuaHelpPayloadSource, String> {
    let helper_path = skill_dir.join(relative_path);
    let source = std::fs::read_to_string(&helper_path).map_err(|error| {
        format!(
            "Failed to read help file {}: {}",
            render_log_friendly_path(&helper_path),
            error
        )
    })?;

    Ok(LuaHelpPayloadSource {
        helper_path,
        source,
    })
}

/// Execute one Lua help payload and render it as plain UTF-8 text.
/// 执行一个 Lua 帮助载荷，并将其渲染为普通 UTF-8 文本。
///
/// The lua parameter is the scoped Lua VM with the help context already installed.
/// lua 参数是已经安装帮助上下文的作用域内 Lua VM。
///
/// The helper_path parameter is the absolute help file path used in diagnostics.
/// helper_path 参数是用于诊断信息的帮助文件绝对路径。
///
/// The helper_source parameter contains the Lua help file source code.
/// helper_source 参数包含 Lua 帮助文件源码。
///
/// The chunk_name parameter is the diagnostic chunk name assigned to the Lua source.
/// chunk_name 参数是分配给 Lua 源码的诊断 chunk 名称。
///
/// Return the plain string produced by the Lua help payload.
/// 返回 Lua 帮助载荷产生的普通字符串。
fn render_lua_help_payload_text(
    lua: &Lua,
    helper_path: &Path,
    helper_source: &str,
    chunk_name: &str,
) -> Result<String, String> {
    let chunk = lua.load(helper_source).set_name(chunk_name);
    let exported: LuaValue = chunk
        .into_function()
        .map_err(|error| {
            format!(
                "Help compile error for {}: {}",
                render_log_friendly_path(helper_path),
                error
            )
        })?
        .call(())
        .map_err(|error| {
            format!(
                "Help init error for {}: {}",
                render_log_friendly_path(helper_path),
                error
            )
        })?;

    let rendered_value = match exported {
        LuaValue::Function(function) => function.call(()).map_err(|error| {
            format!(
                "Help runtime error for {}: {}",
                render_log_friendly_path(helper_path),
                error
            )
        })?,
        other => other,
    };

    match rendered_value {
        LuaValue::String(text) => text
            .to_str()
            .map(|value| value.to_string())
            .map_err(|error| {
                format!(
                    "Help {} returned invalid UTF-8 text: {}",
                    render_log_friendly_path(helper_path),
                    error
                )
            }),
        other => Err(format!(
            "Help {} must return a plain string, actual_type='{}'",
            render_log_friendly_path(helper_path),
            lua_value_type_name(&other)
        )),
    }
}

/// Return the root `vulcan` Lua table.
/// 返回根级 `vulcan` Lua 表。
fn get_vulcan_table(lua: &Lua) -> Result<Table, String> {
    lua.globals()
        .get("vulcan")
        .map_err(|error| format!("Failed to get vulcan module: {}", error))
}

/// Return the nested `vulcan.context` Lua table.
/// 返回嵌套的 `vulcan.context` Lua 表。
fn get_vulcan_context_table(lua: &Lua) -> Result<Table, String> {
    let vulcan = get_vulcan_table(lua)?;
    vulcan
        .get("context")
        .map_err(|error| format!("Failed to get vulcan.context: {}", error))
}

/// Return the nested `vulcan.deps` Lua table.
/// 返回嵌套的 `vulcan.deps` Lua 表。
fn get_vulcan_deps_table(lua: &Lua) -> Result<Table, String> {
    let vulcan = get_vulcan_table(lua)?;
    vulcan
        .get("deps")
        .map_err(|error| format!("Failed to get vulcan.deps: {}", error))
}

/// Return the nested `vulcan.runtime` Lua table.
/// 返回嵌套的 `vulcan.runtime` Lua 表。
fn get_vulcan_runtime_table(lua: &Lua) -> Result<Table, String> {
    let vulcan = get_vulcan_table(lua)?;
    vulcan
        .get("runtime")
        .map_err(|error| format!("Failed to get vulcan.runtime: {}", error))
}

/// Return the nested `vulcan.runtime.internal` Lua table.
/// 返回嵌套的 `vulcan.runtime.internal` Lua 表。
fn get_vulcan_runtime_internal_table(lua: &Lua) -> Result<Table, String> {
    let runtime = get_vulcan_runtime_table(lua)?;
    runtime
        .get("internal")
        .map_err(|error| format!("Failed to get vulcan.runtime.internal: {}", error))
}

/// Return the nested `vulcan.runtime.lua` Lua table.
/// 返回嵌套的 `vulcan.runtime.lua` Lua 表。
fn get_vulcan_runtime_lua_table(lua: &Lua) -> Result<Table, String> {
    let runtime = get_vulcan_runtime_table(lua)?;
    runtime
        .get("lua")
        .map_err(|error| format!("Failed to get vulcan.runtime.lua: {}", error))
}

/// Snapshot of the mutable core `vulcan` tables that must survive nested-call failures.
/// 会在嵌套调用失败后恢复的 `vulcan` 可变核心表快照。
#[derive(Clone)]
struct VulcanCoreModuleState {
    vulcan: Table,
    call: Function,
    runtime: Table,
    runtime_skills: Table,
    runtime_internal: Table,
    runtime_lua: Table,
    runtime_python: Table,
    runtime_node: Table,
    fs: Table,
    io: Table,
    path: Table,
    process: Table,
    os: Table,
    json: Table,
    cache: Table,
    context: Table,
    deps: Table,
    models: Table,
}

impl VulcanCoreModuleState {
    /// Capture the currently installed `vulcan` root tables before one nested skill call mutates them.
    /// 在一次嵌套技能调用可能修改它们之前，捕获当前安装好的 `vulcan` 根表结构。
    fn capture(lua: &Lua) -> Result<Self, String> {
        let vulcan = get_vulcan_table(lua)?;
        let runtime = get_vulcan_runtime_table(lua)?;
        Ok(Self {
            call: vulcan
                .get("call")
                .map_err(|error| format!("Failed to get vulcan.call: {}", error))?,
            runtime_skills: runtime
                .get("skills")
                .map_err(|error| format!("Failed to get vulcan.runtime.skills: {}", error))?,
            runtime_internal: runtime
                .get("internal")
                .map_err(|error| format!("Failed to get vulcan.runtime.internal: {}", error))?,
            runtime_lua: runtime
                .get("lua")
                .map_err(|error| format!("Failed to get vulcan.runtime.lua: {}", error))?,
            runtime_python: runtime
                .get("python")
                .map_err(|error| format!("Failed to get vulcan.runtime.python: {}", error))?,
            runtime_node: runtime
                .get("node")
                .map_err(|error| format!("Failed to get vulcan.runtime.node: {}", error))?,
            fs: vulcan
                .get("fs")
                .map_err(|error| format!("Failed to get vulcan.fs: {}", error))?,
            io: vulcan
                .get("io")
                .map_err(|error| format!("Failed to get vulcan.io: {}", error))?,
            path: vulcan
                .get("path")
                .map_err(|error| format!("Failed to get vulcan.path: {}", error))?,
            process: vulcan
                .get("process")
                .map_err(|error| format!("Failed to get vulcan.process: {}", error))?,
            os: vulcan
                .get("os")
                .map_err(|error| format!("Failed to get vulcan.os: {}", error))?,
            json: vulcan
                .get("json")
                .map_err(|error| format!("Failed to get vulcan.json: {}", error))?,
            cache: vulcan
                .get("cache")
                .map_err(|error| format!("Failed to get vulcan.cache: {}", error))?,
            models: vulcan
                .get("models")
                .map_err(|error| format!("Failed to get vulcan.models: {}", error))?,
            context: vulcan
                .get("context")
                .map_err(|error| format!("Failed to get vulcan.context: {}", error))?,
            deps: vulcan
                .get("deps")
                .map_err(|error| format!("Failed to get vulcan.deps: {}", error))?,
            vulcan,
            runtime,
        })
    }

    /// Reinstall the captured `vulcan` core table topology after one nested call corrupts it.
    /// 在嵌套调用破坏表结构后，重新安装捕获到的 `vulcan` 核心表拓扑。
    fn restore(&self, lua: &Lua) -> Result<(), String> {
        self.runtime
            .set("skills", self.runtime_skills.clone())
            .map_err(|error| format!("Failed to restore vulcan.runtime.skills: {}", error))?;
        self.runtime
            .set("internal", self.runtime_internal.clone())
            .map_err(|error| format!("Failed to restore vulcan.runtime.internal: {}", error))?;
        self.runtime
            .set("lua", self.runtime_lua.clone())
            .map_err(|error| format!("Failed to restore vulcan.runtime.lua: {}", error))?;
        self.runtime
            .set("python", self.runtime_python.clone())
            .map_err(|error| format!("Failed to restore vulcan.runtime.python: {}", error))?;
        self.runtime
            .set("node", self.runtime_node.clone())
            .map_err(|error| format!("Failed to restore vulcan.runtime.node: {}", error))?;
        self.vulcan
            .set("call", self.call.clone())
            .map_err(|error| format!("Failed to restore vulcan.call: {}", error))?;
        self.vulcan
            .set("runtime", self.runtime.clone())
            .map_err(|error| format!("Failed to restore vulcan.runtime: {}", error))?;
        self.vulcan
            .set("fs", self.fs.clone())
            .map_err(|error| format!("Failed to restore vulcan.fs: {}", error))?;
        self.vulcan
            .set("io", self.io.clone())
            .map_err(|error| format!("Failed to restore vulcan.io: {}", error))?;
        self.vulcan
            .set("path", self.path.clone())
            .map_err(|error| format!("Failed to restore vulcan.path: {}", error))?;
        self.vulcan
            .set("process", self.process.clone())
            .map_err(|error| format!("Failed to restore vulcan.process: {}", error))?;
        self.vulcan
            .set("os", self.os.clone())
            .map_err(|error| format!("Failed to restore vulcan.os: {}", error))?;
        self.vulcan
            .set("json", self.json.clone())
            .map_err(|error| format!("Failed to restore vulcan.json: {}", error))?;
        self.vulcan
            .set("cache", self.cache.clone())
            .map_err(|error| format!("Failed to restore vulcan.cache: {}", error))?;
        self.vulcan
            .set("models", self.models.clone())
            .map_err(|error| format!("Failed to restore vulcan.models: {}", error))?;
        self.vulcan
            .set("context", self.context.clone())
            .map_err(|error| format!("Failed to restore vulcan.context: {}", error))?;
        self.vulcan
            .set("deps", self.deps.clone())
            .map_err(|error| format!("Failed to restore vulcan.deps: {}", error))?;
        lua.globals()
            .set("vulcan", self.vulcan.clone())
            .map_err(|error| format!("Failed to restore global vulcan module: {}", error))?;
        Ok(())
    }
}

/// Return the non-empty skill identifier string when the captured value is usable.
/// 当捕获到的技能标识可用时，返回其非空字符串引用。
fn non_empty_skill_name(value: &str) -> Option<&str> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Capture one provider skill marker from the shared `vulcan` table.
/// 从共享 `vulcan` 表捕获单个 provider skill 标记。
///
/// The `vulcan` parameter is the root runtime table that owns provider markers.
/// `vulcan` 参数是持有 provider 标记的运行时根表。
///
/// The `marker_key` parameter is the exact private marker key to read.
/// `marker_key` 参数是需要读取的精确私有标记键。
///
/// Return the captured marker string, including the empty-string marker for no active skill.
/// 返回捕获到的标记字符串，包括表示无 active skill 的空字符串标记。
fn capture_provider_skill_marker(vulcan: &Table, marker_key: &str) -> Result<String, String> {
    vulcan
        .get(marker_key)
        .map_err(|error| format!("Failed to read vulcan.{}: {}", marker_key, error))
}

/// Provider contexts resolved from captured outer provider skill markers during nested restore.
/// 嵌套恢复期间根据已捕获的外层 provider skill 标记解析出的 provider 上下文。
struct RestoredLuaProviderContexts<'a> {
    /// LanceDB binding restored for the captured outer skill marker.
    /// 为已捕获外层 skill 标记恢复的 LanceDB binding。
    lancedb_binding: Option<Arc<LanceDbSkillBinding>>,
    /// LanceDB skill name restored into the provider marker.
    /// 恢复到 provider 标记中的 LanceDB skill 名称。
    lancedb_skill_name: Option<&'a str>,
    /// SQLite binding restored for the captured outer skill marker.
    /// 为已捕获外层 skill 标记恢复的 SQLite binding。
    sqlite_binding: Option<Arc<SqliteSkillBinding>>,
    /// SQLite skill name restored into the provider marker.
    /// 恢复到 provider 标记中的 SQLite skill 名称。
    sqlite_skill_name: Option<&'a str>,
}

/// Resolve provider bindings and skill names needed to restore outer provider contexts.
/// 解析恢复外层 provider 上下文所需的 provider binding 与 skill 名称。
///
/// The `lancedb_host` parameter is the optional shared LanceDB host captured by the guard.
/// `lancedb_host` 参数是 guard 捕获的可选共享 LanceDB 宿主。
///
/// The `sqlite_host` parameter is the optional shared SQLite host captured by the guard.
/// `sqlite_host` 参数是 guard 捕获的可选共享 SQLite 宿主。
///
/// The `lancedb_skill_marker` parameter is the captured LanceDB provider skill marker.
/// `lancedb_skill_marker` 参数是捕获到的 LanceDB provider skill 标记。
///
/// The `sqlite_skill_marker` parameter is the captured SQLite provider skill marker.
/// `sqlite_skill_marker` 参数是捕获到的 SQLite provider skill 标记。
///
/// Return the provider bindings and marker skill names that should be restored together.
/// 返回应一起恢复的 provider binding 与标记 skill 名称。
fn resolve_restored_lua_provider_contexts<'a>(
    lancedb_host: Option<&Arc<LanceDbSkillHost>>,
    sqlite_host: Option<&Arc<SqliteSkillHost>>,
    lancedb_skill_marker: &'a str,
    sqlite_skill_marker: &'a str,
) -> Result<RestoredLuaProviderContexts<'a>, String> {
    // Normalize the LanceDB marker once so lookup and marker restore share one identity.
    // 只归一化一次 LanceDB 标记，确保查找与标记恢复共享同一身份。
    let lancedb_skill_name = non_empty_skill_name(lancedb_skill_marker);
    // Normalize the SQLite marker once so lookup and marker restore share one identity.
    // 只归一化一次 SQLite 标记，确保查找与标记恢复共享同一身份。
    let sqlite_skill_name = non_empty_skill_name(sqlite_skill_marker);
    // Resolve the LanceDB binding only when both the host and captured skill marker exist.
    // 仅当宿主与已捕获 skill 标记同时存在时解析 LanceDB binding。
    let lancedb_binding = match (lancedb_host, lancedb_skill_name) {
        (Some(host), Some(skill_name)) => host.binding_for_skill(skill_name)?,
        _ => None,
    };
    // Resolve the SQLite binding only when both the host and captured skill marker exist.
    // 仅当宿主与已捕获 skill 标记同时存在时解析 SQLite binding。
    let sqlite_binding = match (sqlite_host, sqlite_skill_name) {
        (Some(host), Some(skill_name)) => host.binding_for_skill(skill_name)?,
        _ => None,
    };

    Ok(RestoredLuaProviderContexts {
        lancedb_binding,
        lancedb_skill_name,
        sqlite_binding,
        sqlite_skill_name,
    })
}

/// Restore provider tables from the provider skill markers captured before one nested call.
/// 根据单次嵌套调用前捕获的 provider skill 标记恢复 provider 表。
///
/// The `lua` parameter is the VM whose provider tables should be restored.
/// `lua` 参数是需要恢复 provider 表的 VM。
///
/// The `lancedb_host` parameter is the optional shared LanceDB host captured by the guard.
/// `lancedb_host` 参数是 guard 捕获的可选共享 LanceDB 宿主。
///
/// The `sqlite_host` parameter is the optional shared SQLite host captured by the guard.
/// `sqlite_host` 参数是 guard 捕获的可选共享 SQLite 宿主。
///
/// The `lancedb_skill_marker` parameter is the captured LanceDB provider skill marker.
/// `lancedb_skill_marker` 参数是捕获到的 LanceDB provider skill 标记。
///
/// The `sqlite_skill_marker` parameter is the captured SQLite provider skill marker.
/// `sqlite_skill_marker` 参数是捕获到的 SQLite provider skill 标记。
///
/// Return `Ok(())` after both provider tables and skill markers are restored.
/// 两个 provider 表与 skill 标记均恢复后返回 `Ok(())`。
fn restore_lua_nested_provider_contexts(
    lua: &Lua,
    lancedb_host: Option<&Arc<LanceDbSkillHost>>,
    sqlite_host: Option<&Arc<SqliteSkillHost>>,
    lancedb_skill_marker: &str,
    sqlite_skill_marker: &str,
) -> Result<(), String> {
    let RestoredLuaProviderContexts {
        lancedb_binding,
        lancedb_skill_name,
        sqlite_binding,
        sqlite_skill_name,
    } = resolve_restored_lua_provider_contexts(
        lancedb_host,
        sqlite_host,
        lancedb_skill_marker,
        sqlite_skill_marker,
    )?;
    LuaEngine::populate_vulcan_lancedb_context(lua, lancedb_binding, lancedb_skill_name)?;
    LuaEngine::populate_vulcan_sqlite_context(lua, sqlite_binding, sqlite_skill_name)?;
    Ok(())
}

/// Clear the transient `__runlua_args` global used by pooled VM requests.
/// 清理池化虚拟机请求期间使用的临时 `__runlua_args` 全局变量。
fn clear_runlua_args_global(lua: &Lua) -> Result<(), String> {
    lua.globals()
        .set("__runlua_args", LuaValue::Nil)
        .map_err(|error| format!("Failed to clear __runlua_args: {}", error))
}

/// Reset one pooled Lua VM back to the neutral per-request baseline.
/// 将单个池化 Lua 虚拟机重置回中性的单次请求基线状态。
fn reset_pooled_vm_request_scope(
    lua: &Lua,
    host_options: &LuaRuntimeHostOptions,
) -> Result<(), String> {
    LuaEngine::populate_anonymous_lua_context(
        lua,
        AnonymousLuaExecutionContext {
            invocation_context: None,
            internal_context: VulcanInternalExecutionContext::default(),
            entry_file: None,
            dependency_context: AnonymousLuaDependencyContext::ClearWithHostOptions(host_options),
            managed_package_context: AnonymousLuaManagedPackageContext::Clear,
        },
    )?;
    clear_runlua_args_global(lua)?;
    Ok(())
}

/// One RAII guard that keeps pooled Lua VM request-scoped state isolated.
/// 一个用于保持池化 Lua 虚拟机请求级状态隔离的 RAII 守卫。
struct LuaVmRequestScopeGuard<'a> {
    lease: &'a mut LuaVmLease,
    host_options: &'a LuaRuntimeHostOptions,
    active: bool,
}

impl<'a> LuaVmRequestScopeGuard<'a> {
    /// Normalize one pooled VM before use and arm cleanup for all exit paths.
    /// 在使用前归一化单个池化虚拟机，并为全部退出路径启用清理保护。
    fn new(
        lease: &'a mut LuaVmLease,
        host_options: &'a LuaRuntimeHostOptions,
    ) -> Result<Self, String> {
        let mut guard = Self {
            lease,
            host_options,
            active: true,
        };
        if let Err(error) = guard
            .lua()
            .and_then(|lua| reset_pooled_vm_request_scope(lua, host_options))
        {
            guard.lease.discard();
            guard.active = false;
            return Err(error);
        }
        Ok(guard)
    }

    /// Borrow the guarded Lua VM while the request scope is active.
    /// 在请求作用域激活期间借用受守卫保护的 Lua 虚拟机。
    ///
    /// Returns the borrowed Lua VM, or an explicit error if the lease has already retired its VM.
    /// 返回借用的 Lua 虚拟机；如果租约已经淘汰其虚拟机，则返回显式错误。
    fn lua(&self) -> Result<&Lua, String> {
        self.lease.lua()
    }

    /// Explicitly finish the request scope and surface cleanup errors to the caller.
    /// 显式结束当前请求作用域，并将清理错误返回给调用方。
    fn finish(mut self) -> Result<(), String> {
        let cleanup_result = self
            .lua()
            .and_then(|lua| reset_pooled_vm_request_scope(lua, self.host_options));
        if let Err(error) = cleanup_result {
            self.lease.discard();
            self.active = false;
            return Err(error);
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for LuaVmRequestScopeGuard<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Err(error) = self
            .lua()
            .and_then(|lua| reset_pooled_vm_request_scope(lua, self.host_options))
        {
            log_error(format!(
                "[LuaSkill:error] Failed to reset pooled Lua VM request scope: {}",
                error
            ));
            self.lease.discard();
        }
    }
}

/// Finish a pooled Lua VM request scope and merge the main result with cleanup errors.
/// 结束池化 Lua VM 请求作用域，并合并主流程结果与清理错误。
///
/// The main_result parameter is the result produced while the request scope was active.
/// main_result 参数是在请求作用域激活期间产生的主流程结果。
///
/// The scope_guard parameter owns the request scope that must be explicitly finished.
/// scope_guard 参数持有必须显式结束的请求作用域。
///
/// The cleanup_error_label parameter names the cleanup phase in combined error messages.
/// cleanup_error_label 参数用于在组合错误消息中命名清理阶段。
///
/// Return the main success value only when cleanup also succeeds.
/// 仅当清理也成功时返回主流程成功值。
fn finish_pooled_vm_request_scope<T>(
    main_result: Result<T, String>,
    scope_guard: LuaVmRequestScopeGuard<'_>,
    cleanup_error_label: &str,
) -> Result<T, String> {
    let cleanup_result = scope_guard.finish();
    match (main_result, cleanup_result) {
        (Ok(result), Ok(())) => Ok(result),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(main_error), Ok(())) => Err(main_error),
        (Err(main_error), Err(cleanup_error)) => Err(format!(
            "{}; {}: {}",
            main_error, cleanup_error_label, cleanup_error
        )),
    }
}

/// RAII guard that restores the outer `vulcan` execution context after one nested `vulcan.call`.
/// 在一次嵌套 `vulcan.call` 之后恢复外层 `vulcan` 执行上下文的 RAII 守卫。
struct LuaNestedCallScopeGuard {
    lua: Lua,
    host_options: Arc<LuaRuntimeHostOptions>,
    lancedb_host: Option<Arc<LanceDbSkillHost>>,
    sqlite_host: Option<Arc<SqliteSkillHost>>,
    /// Captured outer state restored after the nested call finishes.
    /// 嵌套调用结束后需要恢复的外层状态快照。
    previous_state: LuaNestedOuterStateSnapshot,
    active: bool,
}

/// Prepared nested call scope and inherited invocation context for one `vulcan.call`.
/// 单次 `vulcan.call` 已准备好的嵌套调用作用域与继承调用上下文。
struct PreparedLuaNestedCallScope {
    /// Guard that owns the captured outer `vulcan` state and restores it after the nested call.
    /// 持有已捕获外层 `vulcan` 状态并在嵌套调用后恢复它的守卫。
    guard: LuaNestedCallScopeGuard,
    /// Invocation context inherited from the outer `vulcan.context` snapshot.
    /// 从外层 `vulcan.context` 快照继承得到的调用上下文。
    invocation_context: LuaInvocationContext,
}

impl LuaNestedCallScopeGuard {
    /// Capture the current outer `vulcan` execution state before entering one nested skill.
    /// 在进入一次嵌套技能调用之前捕获当前外层 `vulcan` 执行状态。
    fn new(
        lua: &Lua,
        host_options: Arc<LuaRuntimeHostOptions>,
        lancedb_host: Option<Arc<LanceDbSkillHost>>,
        sqlite_host: Option<Arc<SqliteSkillHost>>,
    ) -> Result<Self, String> {
        Ok(Self {
            lua: lua.clone(),
            host_options,
            lancedb_host,
            sqlite_host,
            previous_state: capture_lua_nested_outer_state(lua)?,
            active: true,
        })
    }

    /// Return the internal execution context captured before this nested call began.
    /// 返回本次嵌套调用开始前捕获的内部执行上下文。
    ///
    /// The self parameter owns the outer-state snapshot for one nested call scope.
    /// self 参数持有单次嵌套调用作用域的外层状态快照。
    ///
    /// Return the captured internal context used by luaexec checks and nested context derivation.
    /// 返回 luaexec 校验与嵌套上下文派生所使用的已捕获内部上下文。
    fn previous_internal_context(&self) -> &VulcanInternalExecutionContext {
        &self.previous_state.internal_context
    }

    /// Build the nested invocation context from the outer `vulcan.context` snapshot.
    /// 从外层 `vulcan.context` 快照构造嵌套调用的 invocation context。
    ///
    /// The self parameter owns the previously captured request, budget, and tool config values.
    /// self 参数持有此前捕获的 request、budget 与 tool config 值。
    ///
    /// Return the invocation context that should be inherited by one nested `vulcan.call` target.
    /// 返回一个嵌套 `vulcan.call` 目标应继承的 invocation context。
    fn previous_invocation_context(&self) -> mlua::Result<LuaInvocationContext> {
        let request_context_json = lua_value_to_json(&self.previous_state.context.request)
            .map_err(mlua::Error::runtime)?;
        let request_context =
            parse_runtime_request_context_json(request_context_json, "vulcan.context.request")
                .map_err(mlua::Error::runtime)?;
        let client_budget = lua_value_to_json(&self.previous_state.context.client_budget)
            .map_err(mlua::Error::runtime)?;
        let tool_config = lua_value_to_json(&self.previous_state.context.tool_config)
            .map_err(mlua::Error::runtime)?;
        Ok(LuaInvocationContext::new(
            request_context,
            client_budget,
            tool_config,
        ))
    }

    /// Switch the current Lua VM into one resolved nested skill target before invocation.
    /// 在调用前把当前 Lua 虚拟机切换到一个已解析的嵌套 skill 目标。
    ///
    /// Parameters: `target` contains the resolved entry metadata, inherited invocation context, and database bindings.
    /// 参数：`target` 包含已解析入口元数据、继承调用上下文与数据库绑定。
    ///
    /// Returns: `Ok(())` after all `vulcan` subcontexts are populated, or an error string for the failed step.
    /// 返回：所有 `vulcan` 子上下文填充成功后返回 `Ok(())`，否则返回失败步骤的错误字符串。
    fn enter_nested_call(&self, target: LuaNestedCallTarget<'_>) -> Result<(), String> {
        LuaEngine::populate_vulcan_request_context(&self.lua, Some(target.invocation_context))?;
        // Build the internal context from target identity while preserving outer luaexec markers.
        // 使用目标身份构建内部上下文，同时保留外层 luaexec 标记。
        let nested_internal_context =
            build_lua_nested_internal_execution_context(&target, self.previous_internal_context());
        populate_vulcan_internal_execution_context(&self.lua, &nested_internal_context)?;
        populate_lua_nested_resource_contexts(&self.lua, self.host_options.as_ref(), target)?;
        Ok(())
    }

    /// Restore the outer `vulcan` execution state captured before the nested call began.
    /// 恢复嵌套调用开始前捕获到的外层 `vulcan` 执行状态。
    fn restore_previous_state(&self) -> Result<(), String> {
        // Exact outer package restored before any fallible Lua-state restoration step.
        // 在任何可能失败的 Lua 状态恢复步骤前恢复精确外层包。
        replace_lua_managed_package_context(&self.lua, self.previous_state.managed_package.clone());
        self.previous_state.core_state.restore(&self.lua)?;
        restore_lua_nested_provider_contexts(
            &self.lua,
            self.lancedb_host.as_ref(),
            self.sqlite_host.as_ref(),
            &self.previous_state.lancedb_skill_name,
            &self.previous_state.sqlite_skill_name,
        )?;
        restore_vulcan_context_snapshot(&self.lua, &self.previous_state.context)?;
        populate_vulcan_internal_execution_context(&self.lua, self.previous_internal_context())?;
        restore_vulcan_file_context_snapshot(&self.lua, &self.previous_state.file_context)?;
        restore_lua_nested_dependency_context(
            &self.lua,
            self.host_options.as_ref(),
            &self.previous_state.file_context,
            self.previous_internal_context(),
        )?;
        Ok(())
    }

    /// Explicitly finish the nested-call scope and surface any restore failure to the caller.
    /// 显式结束嵌套调用作用域，并把恢复失败信息返回给调用方。
    fn finish(mut self) -> Result<(), String> {
        let restore_result = self.restore_previous_state();
        self.active = false;
        restore_result
    }

    /// Finish one nested call scope and merge the target call result with restore errors.
    /// 结束一次嵌套调用作用域，并合并目标调用结果与恢复错误。
    ///
    /// The call_result parameter is the Lua result returned by the nested target function.
    /// call_result 参数是嵌套目标函数返回的 Lua 结果。
    ///
    /// Return the target result only when restoring the outer context also succeeds.
    /// 仅当外层上下文恢复也成功时返回目标结果。
    fn finish_nested_call<T>(self, call_result: mlua::Result<T>) -> mlua::Result<T> {
        let restore_result = self.finish().map_err(mlua::Error::runtime);
        match (call_result, restore_result) {
            (Ok(result), Ok(())) => Ok(result),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(call_error), Ok(())) => Err(call_error),
            (Err(call_error), Err(restore_error)) => Err(mlua::Error::runtime(format!(
                "{}; nested vulcan.call restore failed: {}",
                call_error, restore_error
            ))),
        }
    }
}

/// Prepare the nested call guard and inherited invocation context for one `vulcan.call`.
/// 为单次 `vulcan.call` 准备嵌套调用守卫与继承调用上下文。
///
/// The `lua` parameter is the VM executing the nested dispatch.
/// `lua` 参数是执行嵌套分发的 VM。
///
/// The `host_options` parameter is the shared runtime host options captured by the dispatcher.
/// `host_options` 参数是 dispatcher 捕获的共享运行时宿主选项。
///
/// The `lancedb_host` parameter is the optional shared LanceDB host captured by the dispatcher.
/// `lancedb_host` 参数是 dispatcher 捕获的可选共享 LanceDB 宿主。
///
/// The `sqlite_host` parameter is the optional shared SQLite host captured by the dispatcher.
/// `sqlite_host` 参数是 dispatcher 捕获的可选共享 SQLite 宿主。
///
/// Return the guard plus the invocation context inherited from the captured outer state.
/// 返回守卫以及从已捕获外层状态继承得到的调用上下文。
fn prepare_lua_nested_call_scope(
    lua: &Lua,
    host_options: Arc<LuaRuntimeHostOptions>,
    lancedb_host: Option<Arc<LanceDbSkillHost>>,
    sqlite_host: Option<Arc<SqliteSkillHost>>,
) -> mlua::Result<PreparedLuaNestedCallScope> {
    // Capture the outer state before deriving any nested invocation context from it.
    // 在从外层状态推导任何嵌套调用上下文之前先捕获外层状态。
    let guard = LuaNestedCallScopeGuard::new(lua, host_options, lancedb_host, sqlite_host)
        .map_err(mlua::Error::runtime)?;
    // Derive the inherited invocation context from the captured outer request data.
    // 从已捕获的外层请求数据推导继承调用上下文。
    let invocation_context = guard.previous_invocation_context()?;
    Ok(PreparedLuaNestedCallScope {
        guard,
        invocation_context,
    })
}

impl Drop for LuaNestedCallScopeGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Err(error) = self.restore_previous_state() {
            log_error(format!(
                "[LuaSkill:error] Failed to restore nested vulcan.call context: {}",
                error
            ));
        }
    }
}

/// Checked-out VM guard that returns the VM back into the pool on drop.
/// 已借出的虚拟机守卫，在释放时会自动归还到池中。
struct LuaVmLease {
    pool: Arc<LuaVmPool>,
    vm: Option<LuaVm>,
}

impl LuaVmLease {
    /// Borrow the underlying Lua VM immutably for the duration of the lease.
    /// 在租约生命周期内以只读方式借用底层 Lua 虚拟机。
    ///
    /// Returns the borrowed Lua VM, or an explicit error if the lease has already retired its VM.
    /// 返回借用的 Lua 虚拟机；如果租约已经淘汰其虚拟机，则返回显式错误。
    fn lua(&self) -> Result<&Lua, String> {
        self.vm
            .as_ref()
            .map(|vm| &vm.lua)
            .ok_or_else(|| "pooled Lua VM lease has already been retired".to_string())
    }

    /// Permanently retire the currently leased VM instead of returning it to the pool.
    /// 永久淘汰当前租出的虚拟机，而不是把它放回池中。
    fn discard(&mut self) {
        if let Some(vm) = self.vm.take() {
            self.pool.discard(vm);
        }
    }
}

impl Drop for LuaVmLease {
    fn drop(&mut self) {
        if let Some(mut vm) = self.vm.take() {
            vm.last_used_at = Instant::now();
            self.pool.release(vm);
        }
    }
}

impl LuaVmPool {
    /// Create a new empty Lua VM pool.
    /// 创建一个新的空 Lua 虚拟机池。
    fn new(config: LuaVmPoolConfig) -> Self {
        Self {
            config: config.normalized(),
            state: Mutex::new(LuaVmPoolState {
                available: Vec::new(),
                total_count: 0,
            }),
            condvar: Condvar::new(),
        }
    }

    /// Prewarm the pool to the configured minimum size.
    /// 预热到配置要求的最小虚拟机数量。
    fn prewarm<F>(&self, mut factory: F) -> Result<(), String>
    where
        F: FnMut() -> Result<LuaVm, String>,
    {
        while self.total_count() < self.config.min_size {
            {
                let mut state = self.lock_state();
                state.total_count += 1;
            }
            match factory() {
                Ok(vm) => self.release(vm),
                Err(error) => {
                    let mut state = self.lock_state();
                    state.total_count = state.total_count.saturating_sub(1);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Acquire a VM from the pool, growing on demand up to the configured limit.
    /// 从池中获取虚拟机，并在未达上限时按需扩容。
    fn acquire<F>(self: &Arc<Self>, mut factory: F) -> Result<LuaVmLease, String>
    where
        F: FnMut() -> Result<LuaVm, String>,
    {
        loop {
            let mut state = self.lock_state();
            self.reap_idle_locked(&mut state);

            if let Some(mut vm) = state.available.pop() {
                vm.last_used_at = Instant::now();
                return Ok(LuaVmLease {
                    pool: self.clone(),
                    vm: Some(vm),
                });
            }

            if state.total_count < self.config.max_size {
                state.total_count += 1;
                drop(state);
                match factory() {
                    Ok(vm) => {
                        return Ok(LuaVmLease {
                            pool: self.clone(),
                            vm: Some(vm),
                        });
                    }
                    Err(error) => {
                        let mut state = self.lock_state();
                        state.total_count = state.total_count.saturating_sub(1);
                        self.condvar.notify_one();
                        return Err(error);
                    }
                }
            }

            drop(self.wait_state(state));
        }
    }

    /// Return a VM back into the pool.
    /// 将虚拟机归还到池中。
    fn release(&self, vm: LuaVm) {
        let mut state = self.lock_state();
        state.available.push(vm);
        self.reap_idle_locked(&mut state);
        self.condvar.notify_one();
    }

    /// Retire one broken VM so later borrowers receive a fresh instance instead of stale state.
    /// 退役一个已损坏的虚拟机，确保后续借用方拿到的是新实例而不是陈旧状态。
    fn discard(&self, _vm: LuaVm) {
        let mut state = self.lock_state();
        if state.total_count > 0 {
            state.total_count -= 1;
        }
        self.condvar.notify_one();
    }

    /// Return the current total number of VMs in the pool.
    /// 返回当前池中的虚拟机总数。
    fn total_count(&self) -> usize {
        self.lock_state().total_count
    }

    /// Acquire the pool state lock and return its guard, recovering after another pool operation panics while holding it.
    /// 获取并返回虚拟机池状态锁；如果其它池操作持锁 panic，则恢复状态继续使用。
    fn lock_state(&self) -> MutexGuard<'_, LuaVmPoolState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait on the pool condition variable and return the reacquired state guard, recovering poisoned state after wakeup.
    /// 等待虚拟机池条件变量并返回重新获取的状态锁；唤醒后如果状态已 poison，则恢复继续使用。
    fn wait_state<'a>(
        &self,
        state: MutexGuard<'a, LuaVmPoolState>,
    ) -> MutexGuard<'a, LuaVmPoolState> {
        self.condvar
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reap idle available VMs while respecting the minimum pool size.
    /// 在保证最小池规模的前提下回收空闲虚拟机。
    fn reap_idle_locked(&self, state: &mut LuaVmPoolState) {
        if state.total_count <= self.config.min_size {
            return;
        }

        let idle_limit = Duration::from_secs(self.config.idle_ttl_secs);
        let now = Instant::now();
        let mut index = 0usize;
        while index < state.available.len() && state.total_count > self.config.min_size {
            let should_remove = now
                .checked_duration_since(state.available[index].last_used_at)
                .map(|idle| idle >= idle_limit)
                .unwrap_or(false);
            if should_remove {
                state.available.swap_remove(index);
                state.total_count = state.total_count.saturating_sub(1);
            } else {
                index += 1;
            }
        }
    }
}

impl LuaEngine {
    /// Poll a bounded batch of managed-session events without waiting.
    /// 以非等待方式轮询一批有界受管会话事件。
    ///
    /// `max_events` must be positive and limits the number of destructively drained events.
    /// `max_events` 必须为正数，并限制破坏性取出的事件数量。
    ///
    /// Returns sequence-ordered events, the remaining queue size, and a false timeout flag.
    /// 返回按序号排序的事件、剩余队列大小以及 false 超时标记。
    pub fn poll_managed_session_events(
        &self,
        max_events: usize,
    ) -> Result<RuntimeManagedSessionEventBatch, String> {
        self.managed_runtime_services
            .event_center()
            .poll(max_events)
    }

    /// Wait for and destructively drain a bounded batch of managed-session events.
    /// 等待并破坏性取出一批有界受管会话事件。
    ///
    /// `max_events` must be positive; `timeout` is rounded up to the next whole millisecond.
    /// `max_events` 必须为正数；`timeout` 会向上取整到下一个整毫秒。
    ///
    /// Returns immediately for zero timeout, on an event, or when the event center closes.
    /// 在零超时、事件到达或事件中心关闭时立即返回。
    pub fn wait_managed_session_events(
        &self,
        max_events: usize,
        timeout: Duration,
    ) -> Result<RuntimeManagedSessionEventBatch, String> {
        // Millisecond transport precision rounded upward so a positive duration never becomes polling.
        // 毫秒传输精度向上取整，确保正时长不会退化为轮询。
        let timeout_ms = timeout.as_nanos().div_ceil(1_000_000);
        let timeout_ms = u64::try_from(timeout_ms)
            .map_err(|_| "managed session event timeout is too large".to_string())?;
        self.managed_runtime_services
            .event_center()
            .wait(max_events, timeout_ms)
    }

    /// Replace or clear the host wake callback for managed-session event queue edges.
    /// 替换或清除受管会话事件队列边沿的宿主唤醒回调。
    ///
    /// `callback` runs outside event-center locks and must only schedule host work, never enter Lua.
    /// `callback` 在事件中心锁外运行，并且只能调度宿主工作，禁止进入 Lua。
    ///
    /// Returns after every callback generation retired by this operation has quiesced.
    /// 在当前操作退役的全部回调代际收敛后返回。
    pub fn set_managed_session_wake_callback(
        &self,
        callback: Option<RuntimeManagedSessionWakeCallback>,
    ) -> Result<(), String> {
        self.managed_runtime_services
            .event_center()
            .set_wake_callback(callback)
    }

    /// Clone the engine event center for FFI waits performed outside the registry mutex.
    /// 克隆引擎事件中心，供 FFI 在注册表互斥锁外执行等待。
    pub(crate) fn managed_session_event_center(&self) -> Arc<ManagedSessionEventCenter> {
        self.managed_runtime_services.event_center()
    }

    /// Retire long-lived sessions and short-lived workers for one exact package owner.
    /// 退役一个精确包所有者的长期会话与短期 Worker。
    ///
    /// `owner_token` is the immutable lifetime token shared by both managed runtime services.
    /// `owner_token` 是两个受管运行时服务共享的不可变生命周期令牌。
    ///
    /// Return any long-lived session teardown failure after always retiring worker reuse.
    /// 始终退役 Worker 复用后，返回任何长期会话清理失败。
    fn retire_managed_runtime_owner(&self, owner_token: u64) -> Result<(), String> {
        // Session teardown result retained while worker retirement is guaranteed to run.
        // 在保证执行 Worker 退役的同时保留的会话清理结果。
        let session_result = self.managed_runtime_services.retire_owner(owner_token);
        self.managed_runtime_workers.retire_owner(owner_token);
        session_result
    }

    /// Drain manager retirements and apply them to both engine-owned managed runtime services.
    /// 排空管理器退役项，并将其应用到两个引擎所有受管运行时服务。
    ///
    /// `manager` owns the committed retirement queue and `operation` labels cleanup diagnostics.
    /// `manager` 拥有已提交退役队列，`operation` 标记清理诊断。
    fn retire_runtime_session_manager_owners(
        &self,
        manager: &RuntimeSessionManager,
        operation: &str,
    ) {
        // Exact owner tokens detached by pruning, replacement, or explicit close.
        // 因清理、替换或显式关闭而分离的精确所有者令牌。
        let owner_tokens = match manager.take_retired_owner_tokens() {
            Ok(owner_tokens) => owner_tokens,
            Err(error) => {
                log_error(format!(
                    "[LuaSkill] Failed to drain managed runtime owner retirements after {operation}: {}",
                    error.message
                ));
                return;
            }
        };
        for owner_token in owner_tokens {
            if let Err(error) = self.retire_managed_runtime_owner(owner_token) {
                log_error(format!(
                    "[LuaSkill] Failed to retire managed runtime owner {owner_token} after {operation}: {error}"
                ));
            }
        }
    }

    /// Return the normalized formal label for one raw runtime skill root name.
    /// 返回单个原始运行时技能根名称的规范化正式标签。
    fn normalized_skill_root_name(root_name: &str) -> String {
        root_name.trim().to_ascii_uppercase()
    }

    /// Return the normalized formal label for one runtime skill root.
    /// 返回单个运行时技能根的规范化正式标签。
    fn normalized_skill_root_label(root: &RuntimeSkillRoot) -> String {
        Self::normalized_skill_root_name(&root.name)
    }

    /// Return the fixed load-priority rank for one formal root label.
    /// 返回单个正式根标签的固定加载优先级排序值。
    fn formal_skill_root_rank(label: &str) -> Option<usize> {
        match label {
            "ROOT" => Some(0),
            "PROJECT" => Some(1),
            "USER" => Some(2),
            _ => None,
        }
    }

    /// Return whether one runtime skill root is the system-controlled ROOT layer.
    /// 返回单个运行时技能根是否为系统控制的 ROOT 层。
    fn is_root_skill_root(root: &RuntimeSkillRoot) -> bool {
        Self::normalized_skill_root_label(root) == "ROOT"
    }

    /// Return whether one runtime skill root is writable through the ordinary skills plane.
    /// 返回单个运行时技能根是否可通过普通 skills 平面写入。
    fn is_user_mutable_skill_root(root: &RuntimeSkillRoot) -> bool {
        matches!(
            Self::normalized_skill_root_label(root).as_str(),
            "PROJECT" | "USER"
        )
    }

    /// Validate that the configured skill root chain uses only ROOT, PROJECT, and USER in fixed order.
    /// 校验已配置技能根链仅使用 ROOT、PROJECT、USER 且顺序固定。
    fn validate_formal_skill_root_chain(skill_roots: &[RuntimeSkillRoot]) -> Result<(), String> {
        if skill_roots.is_empty() {
            return Err(
                "ROOT skill root is required; pass a ROOT layer before starting LuaSkills"
                    .to_string(),
            );
        }
        let mut previous_rank = None;
        let mut seen_labels = BTreeSet::new();
        for root in skill_roots {
            let label = Self::normalized_skill_root_label(root);
            let rank = Self::formal_skill_root_rank(&label).ok_or_else(|| {
                format!(
                    "unsupported skill root label '{}'; expected one of ROOT, PROJECT, USER",
                    root.name
                )
            })?;
            if !seen_labels.insert(label.clone()) {
                return Err(format!(
                    "duplicate skill root label '{}'; only one ROOT, PROJECT, and USER root is supported",
                    label
                ));
            }
            if previous_rank
                .map(|previous_rank| rank < previous_rank)
                .unwrap_or(false)
            {
                return Err(
                    "skill roots must be ordered by fixed priority ROOT -> PROJECT -> USER"
                        .to_string(),
                );
            }
            previous_rank = Some(rank);
        }
        if !seen_labels.contains("ROOT") {
            return Err(
                "ROOT skill root is required; pass a ROOT layer before starting LuaSkills"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Inspect whether one configured runtime skill root path is a directory without hiding filesystem probe errors.
    /// 检查单个已配置运行时技能根路径是否为目录，同时不隐藏文件系统探测错误。
    ///
    /// The root parameter is the configured runtime skill root whose skills_dir should be inspected.
    /// root 参数是需要检查 skills_dir 的已配置运行时技能根。
    ///
    /// Return true for an existing directory, false for a confirmed missing path, or an explicit probe/type error.
    /// 已存在目录返回 true，确认缺失路径返回 false；探测或类型失败时返回显式错误。
    fn runtime_skill_root_dir_is_directory(root: &RuntimeSkillRoot) -> Result<bool, String> {
        match fs::metadata(&root.skills_dir) {
            Ok(metadata) if metadata.is_dir() => Ok(true),
            Ok(_) => Err(format!(
                "skill root '{}' is not a directory: {}",
                root.name,
                render_log_friendly_path(&root.skills_dir)
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!(
                "failed to inspect skill root '{}' at {}: {}",
                root.name,
                render_log_friendly_path(&root.skills_dir),
                error
            )),
        }
    }

    /// Return whether at least one configured runtime skill root directory exists after checked probing.
    /// 在经过可失败探测后返回是否至少存在一个已配置运行时技能根目录。
    ///
    /// The skill_roots parameter is the ordered runtime skill root chain provided by the host.
    /// skill_roots 参数是宿主提供的有序运行时技能根链。
    ///
    /// Return true when any root directory exists, false when all roots are confirmed missing, or an explicit probe/type error.
    /// 任一根目录存在时返回 true，全部根路径确认缺失时返回 false；探测或类型失败时返回显式错误。
    fn any_runtime_skill_root_dir_exists(skill_roots: &[RuntimeSkillRoot]) -> Result<bool, String> {
        for root in skill_roots {
            if Self::runtime_skill_root_dir_is_directory(root)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Find one configured skill root by its formal label.
    /// 按正式标签查找单个已配置技能根。
    fn find_skill_root_by_label<'a>(
        skill_roots: &'a [RuntimeSkillRoot],
        label: &str,
    ) -> Option<&'a RuntimeSkillRoot> {
        skill_roots
            .iter()
            .find(|root| Self::normalized_skill_root_label(root) == label)
    }

    /// Resolve the default install target for one operation plane.
    /// 解析单个操作平面的默认安装目标根。
    fn default_install_skill_root<'a>(
        &self,
        plane: SkillOperationPlane,
        skill_roots: &'a [RuntimeSkillRoot],
    ) -> Result<&'a RuntimeSkillRoot, String> {
        match plane {
            SkillOperationPlane::Skills => Self::find_skill_root_by_label(skill_roots, "USER")
                .or_else(|| Self::find_skill_root_by_label(skill_roots, "PROJECT"))
                .filter(|root| Self::is_user_mutable_skill_root(root))
                .ok_or_else(|| {
                    "ordinary skills plane requires a PROJECT or USER skill root; ROOT is system-controlled"
                        .to_string()
                }),
            SkillOperationPlane::System => {
                if let Some(root) = Self::find_skill_root_by_label(skill_roots, "ROOT") {
                    Ok(root)
                } else {
                    Err(
                        "system install requires a configured ROOT skill root; ordinary PROJECT/USER layers must be managed through the skills plane"
                            .to_string(),
                    )
                }
            }
        }
    }

    /// Convert one host-injected authority into the lifecycle operation plane it may use.
    /// 将单个宿主注入权限转换为可使用的生命周期操作平面。
    fn operation_plane_for_authority(authority: SkillManagementAuthority) -> SkillOperationPlane {
        match authority {
            SkillManagementAuthority::System => SkillOperationPlane::System,
            SkillManagementAuthority::DelegatedTool => SkillOperationPlane::Skills,
        }
    }

    /// Resolve the canonical runtime root used by the unified skill-config file.
    /// 解析统一技能配置文件所使用的规范运行时根目录。
    fn canonical_skill_config_runtime_root(
        &self,
        skill_roots: &[RuntimeSkillRoot],
    ) -> Result<PathBuf, String> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for skill_root in skill_roots {
            let candidate = normalize_runtime_root_path(&self.runtime_root_for(skill_root));
            if !candidates.iter().any(|existing| existing == &candidate) {
                candidates.push(candidate);
            }
        }
        match candidates.len() {
            0 => Err("at least one skill root is required to resolve the unified skill config path".to_string()),
            1 => Ok(candidates.remove(0)),
            _ => Err(
                "multiple runtime roots map to different parents; set host_options.skill_config_file_path explicitly".to_string()
            ),
        }
    }

    /// Create a new LuaEngine with LuaJIT VM and registered globals.
    pub fn new(options: LuaEngineOptions) -> Result<Self, Box<dyn std::error::Error>> {
        let LuaEngineOptions {
            pool_config,
            host_options,
        } = options;
        let host_options = host_options.normalized();
        let _default_text_encoding =
            resolve_host_default_text_encoding(&host_options).map_err(std::io::Error::other)?;
        let runlua_pool_config = host_options
            .runlua_pool_config
            .map(|config| LuaVmPoolConfig {
                min_size: config.min_size,
                max_size: config.max_size,
                idle_ttl_secs: config.idle_ttl_secs,
            })
            .unwrap_or_else(default_runlua_vm_pool_config);
        configure_global_tool_cache(
            host_options
                .cache_config
                .clone()
                .unwrap_or_else(ToolCacheConfig::default),
        );
        let native_library_search_guard =
            NativeLibrarySearchGuard::new(&host_options).map_err(std::io::Error::other)?;
        let database_provider_callbacks =
            Arc::new(RuntimeDatabaseProviderCallbacks::capture_process_defaults());
        // Engine-owned lifecycle service shared by every VM created from this engine.
        // 由当前引擎创建的每个 VM 共享的引擎所有生命周期服务。
        let managed_runtime_services =
            ManagedRuntimeServices::new().map_err(std::io::Error::other)?;
        // Engine-owned worker service isolated from every other LuaEngine instance.
        // 与其他所有 LuaEngine 实例隔离的引擎所有 Worker 服务。
        let managed_runtime_workers = ManagedRuntimeWorkerService::new();
        Ok(Self {
            skills: HashMap::new(),
            entry_registry: BTreeMap::new(),
            runtime_skill_roots: Vec::new(),
            pool: Arc::new(LuaVmPool::new(pool_config)),
            runlua_pool: Arc::new(LuaVmPool::new(runlua_pool_config)),
            public_runtime_sessions: Arc::new(RuntimeSessionManager::new()),
            system_runtime_sessions: Arc::new(RuntimeSessionManager::new()),
            managed_runtime_services,
            managed_runtime_workers,
            skill_config_store: Arc::new(
                SkillConfigStore::new(host_options.skill_config_file_path.clone())
                    .map_err(std::io::Error::other)?,
            ),
            lancedb_host: None,
            sqlite_host: None,
            database_provider_callbacks,
            native_library_search_guard,
            host_options: Arc::new(host_options),
        })
    }

    /// Build the shared runtime root used by host-managed sibling directories.
    /// 构造宿主管理同级目录所使用的共享运行时根目录。
    fn runtime_root_for(&self, skill_root: &RuntimeSkillRoot) -> PathBuf {
        skill_root
            .skills_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| skill_root.skills_dir.clone())
    }

    /// Collect the resources directories that may represent packaged runtime layouts for the active root chain.
    /// 收集当前根目录链中可能代表打包运行时布局的 resources 目录。
    fn packaged_runtime_resources_dirs(&self, skill_roots: &[RuntimeSkillRoot]) -> Vec<PathBuf> {
        let mut deduped = BTreeSet::new();
        if let Some(resources_dir) = self.host_options.resources_dir.as_ref() {
            deduped.insert(normalize_runtime_root_path(resources_dir));
        } else {
            for skill_root in skill_roots {
                deduped.insert(normalize_runtime_root_path(
                    &self.runtime_root_for(skill_root).join("resources"),
                ));
            }
        }
        deduped.into_iter().collect()
    }

    /// Validate the embedded luaskills-packages metadata whenever one packaged runtime layout is detected.
    /// 在检测到打包运行时布局时校验其内嵌的 luaskills-packages 元数据。
    fn validate_packaged_runtime_resources(
        &self,
        skill_roots: &[RuntimeSkillRoot],
    ) -> Result<(), String> {
        for resources_dir in self.packaged_runtime_resources_dirs(skill_roots) {
            validate_packaged_runtime_packages_layout(&resources_dir)?;
        }
        Ok(())
    }

    /// Build the sibling state root for one named skill root.
    /// 为单个命名技能根构造同级状态根目录。
    fn state_root_for(&self, skill_root: &RuntimeSkillRoot) -> PathBuf {
        self.runtime_root_for(skill_root)
            .join(self.host_options.state_dir_name.as_str())
    }

    /// Build the sibling dependency root for one named skill root.
    /// 为单个命名技能根构造同级依赖根目录。
    fn dependency_root_for(&self, skill_root: &RuntimeSkillRoot) -> PathBuf {
        self.runtime_root_for(skill_root)
            .join(self.host_options.dependency_dir_name.as_str())
    }

    /// Return whether the host policy forces one skill identifier to be ignored.
    /// 返回宿主策略是否强制忽略指定技能标识符。
    fn is_host_ignored_skill(&self, skill_id: &str) -> bool {
        self.host_options
            .ignored_skill_ids
            .iter()
            .any(|ignored| ignored.trim() == skill_id)
    }

    /// Build the sibling database root for one named skill root.
    /// 为单个命名技能根构造同级数据库根目录。
    fn database_root_for(&self, skill_root: &RuntimeSkillRoot) -> PathBuf {
        self.runtime_root_for(skill_root)
            .join(self.host_options.database_dir_name.as_str())
    }

    /// Capture the shared runtime root used by the unified skill config file.
    /// 记录统一技能配置文件所使用的共享运行时根目录。
    fn refresh_skill_config_runtime_root(
        &self,
        skill_roots: &[RuntimeSkillRoot],
    ) -> Result<(), String> {
        if self.skill_config_store.has_explicit_file_path() {
            return Ok(());
        }
        let runtime_root = self.canonical_skill_config_runtime_root(skill_roots)?;
        self.skill_config_store
            .set_default_runtime_root(&runtime_root)
    }

    /// Create an empty reload candidate that preserves immutable host policy and callback snapshots.
    /// 创建一个空的重载候选引擎，并保留不可变宿主策略与回调快照。
    fn empty_reload_candidate(&self) -> Result<Self, Box<dyn std::error::Error>> {
        let explicit_skill_config_file_path = if self.skill_config_store.has_explicit_file_path() {
            Some(
                self.skill_config_store
                    .file_path()
                    .map_err(std::io::Error::other)?,
            )
        } else {
            None
        };
        Ok(Self {
            skills: HashMap::new(),
            entry_registry: BTreeMap::new(),
            runtime_skill_roots: Vec::new(),
            pool: Arc::new(LuaVmPool::new(self.pool.config)),
            runlua_pool: Arc::new(LuaVmPool::new(self.runlua_pool.config)),
            public_runtime_sessions: Arc::new(RuntimeSessionManager::new()),
            system_runtime_sessions: Arc::clone(&self.system_runtime_sessions),
            managed_runtime_services: Arc::clone(&self.managed_runtime_services),
            managed_runtime_workers: Arc::clone(&self.managed_runtime_workers),
            skill_config_store: Arc::new(
                SkillConfigStore::new(explicit_skill_config_file_path)
                    .map_err(std::io::Error::other)?,
            ),
            lancedb_host: None,
            sqlite_host: None,
            database_provider_callbacks: self.database_provider_callbacks.clone(),
            native_library_search_guard: NativeLibrarySearchGuard::new(&self.host_options)
                .map_err(std::io::Error::other)?,
            host_options: self.host_options.clone(),
        })
    }

    /// Replace the active runtime state with one fully loaded reload candidate.
    /// 使用一个已完整加载的重载候选引擎替换当前活动运行时状态。
    fn replace_runtime_state_from(&mut self, next: LuaEngine) {
        self.skills = next.skills;
        self.entry_registry = next.entry_registry;
        self.runtime_skill_roots = next.runtime_skill_roots;
        self.pool = next.pool;
        self.runlua_pool = next.runlua_pool;
        self.public_runtime_sessions = next.public_runtime_sessions;
        self.skill_config_store = next.skill_config_store;
        self.lancedb_host = next.lancedb_host;
        self.sqlite_host = next.sqlite_host;
        self.database_provider_callbacks = next.database_provider_callbacks;
        self.native_library_search_guard = next.native_library_search_guard;
        self.host_options = next.host_options;
    }

    /// Build the dependency-manager configuration for one named skill root.
    /// 为单个命名技能根构造依赖管理器配置。
    fn dependency_manager_config_for(
        &self,
        skill_root: &RuntimeSkillRoot,
    ) -> Result<DependencyManagerConfig, String> {
        let runtime_root = skill_root
            .skills_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| skill_root.skills_dir.clone());
        let dependency_root = self.dependency_root_for(skill_root);
        let tool_root = dependency_root.join("tools");
        let host_tool_root = self
            .host_options
            .host_provided_tool_root
            .clone()
            .unwrap_or_else(|| runtime_root.join("bin"));
        let lua_root = dependency_root.join("lua");
        let host_lua_root = self
            .host_options
            .host_provided_lua_root
            .clone()
            .or_else(|| self.host_options.lua_packages_dir.clone())
            .unwrap_or_else(|| runtime_root.join("lua_packages"));
        let ffi_root = dependency_root.join("ffi");
        let host_ffi_root = self
            .host_options
            .host_provided_ffi_root
            .clone()
            .or_else(|| {
                self.host_options
                    .lancedb_library_path
                    .as_ref()
                    .and_then(|path| path.parent().map(Path::to_path_buf))
            })
            .or_else(|| {
                self.host_options
                    .sqlite_library_path
                    .as_ref()
                    .and_then(|path| path.parent().map(Path::to_path_buf))
            })
            .unwrap_or_else(|| runtime_root.join("libs"));
        let download_cache_root = self
            .host_options
            .download_cache_root
            .clone()
            .unwrap_or_else(|| runtime_root.join("temp").join("downloads"));

        ensure_directory(&tool_root)?;
        ensure_directory(&host_tool_root)?;
        ensure_directory(&lua_root)?;
        ensure_directory(&host_lua_root)?;
        ensure_directory(&ffi_root)?;
        ensure_directory(&host_ffi_root)?;
        ensure_directory(&download_cache_root)?;

        Ok(DependencyManagerConfig {
            tool_root,
            host_tool_root,
            lua_root,
            host_lua_root,
            ffi_root,
            host_ffi_root,
            download_cache_root,
            allow_network_download: self.host_options.allow_network_download,
            github_base_url: self.host_options.github_base_url.clone(),
            github_api_base_url: self.host_options.github_api_base_url.clone(),
        })
    }

    /// Build the skill-manager configuration for one named skill root.
    /// 为单个命名技能根构造技能管理器配置。
    fn skill_manager_for(&self, skill_root: &RuntimeSkillRoot) -> Result<SkillManager, String> {
        let state_root = self.state_root_for(skill_root);
        let dependency_config = self.dependency_manager_config_for(skill_root)?;
        ensure_directory(&state_root)?;
        Ok(SkillManager::new(SkillManagerConfig {
            skill_root: skill_root.clone(),
            lifecycle_root: state_root,
            download_cache_root: dependency_config.download_cache_root,
            allow_network_download: dependency_config.allow_network_download,
            github_base_url: dependency_config.github_base_url,
            github_api_base_url: dependency_config.github_api_base_url,
            official_skill_hub_base_url: self.host_options.official_skill_hub_base_url.clone(),
            enable_private_url_skill_install: self.host_options.enable_private_url_skill_install,
            private_skill_source_allowlist: self
                .host_options
                .private_skill_source_allowlist
                .clone(),
        }))
    }

    /// Build the skill-manager configuration for one operation with progress reporting enabled.
    /// 为启用进度回馈的单次操作构造技能管理器配置。
    fn skill_manager_for_with_progress(
        &self,
        skill_root: &RuntimeSkillRoot,
        progress: RuntimeSkillOperationProgressEmitter,
    ) -> Result<SkillManager, String> {
        let state_root = self.state_root_for(skill_root);
        let dependency_config = self.dependency_manager_config_for(skill_root)?;
        ensure_directory(&state_root)?;
        Ok(SkillManager::new_with_progress(
            SkillManagerConfig {
                skill_root: skill_root.clone(),
                lifecycle_root: state_root,
                download_cache_root: dependency_config.download_cache_root,
                allow_network_download: dependency_config.allow_network_download,
                github_base_url: dependency_config.github_base_url,
                github_api_base_url: dependency_config.github_api_base_url,
                official_skill_hub_base_url: self.host_options.official_skill_hub_base_url.clone(),
                enable_private_url_skill_install: self
                    .host_options
                    .enable_private_url_skill_install,
                private_skill_source_allowlist: self
                    .host_options
                    .private_skill_source_allowlist
                    .clone(),
            },
            Some(progress),
        ))
    }

    /// Ensure dependencies declared by one skill directory are installed before the skill is loaded.
    /// 在真正加载 skill 前确保该目录声明的依赖已经安装完成。
    fn ensure_skill_dependencies(
        &self,
        skill_root: &RuntimeSkillRoot,
        skill_dir: &Path,
    ) -> Result<Option<PackageDependencyManifest>, String> {
        let Some(manifest) = self.load_skill_dependency_manifest(skill_dir)? else {
            return Ok(None);
        };
        if manifest.is_empty() {
            return Ok(Some(manifest));
        }

        let skill_name = skill_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown-skill");
        let manager = DependencyManager::new(self.dependency_manager_config_for(skill_root)?);
        manager.ensure_skill_dependencies(skill_name, &manifest)?;
        Ok(Some(manifest))
    }

    /// Load one optional dependency manifest from one skill directory when the file exists.
    /// 当依赖文件存在时，从单个技能目录加载可选依赖清单。
    fn load_skill_dependency_manifest(
        &self,
        skill_dir: &Path,
    ) -> Result<Option<PackageDependencyManifest>, String> {
        let dependencies_path = skill_dir.join("dependencies.yaml");
        if !skill_dependency_manifest_path_exists(&dependencies_path)? {
            return Ok(None);
        }
        PackageDependencyManifest::load_from_path(&dependencies_path).map(Some)
    }

    /// Return whether two runtime skill roots refer to the same configured root entry.
    /// 返回两个运行时技能根是否指向同一个已配置根条目。
    fn runtime_skill_roots_match(left: &RuntimeSkillRoot, right: &RuntimeSkillRoot) -> bool {
        left.name == right.name && left.skills_dir == right.skills_dir
    }

    /// Return the ordered-chain index of one configured root.
    /// 返回单个已配置根在有序根链中的索引。
    fn runtime_skill_root_index(
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
    ) -> Result<usize, String> {
        skill_roots
            .iter()
            .position(|root| Self::runtime_skill_roots_match(root, target_root))
            .ok_or_else(|| {
                format!(
                    "target root '{}' at {} is not part of the full runtime root chain",
                    target_root.name,
                    render_log_friendly_path(&target_root.skills_dir)
                )
            })
    }

    /// Ensure one delegated ordinary target is a configured PROJECT or USER root.
    /// 确保单个委托普通目标是已配置的 PROJECT 或 USER 根。
    fn validate_ordinary_target_root(
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        action: crate::skill::manager::SkillLifecycleAction,
    ) -> Result<(), String> {
        Self::runtime_skill_root_index(skill_roots, target_root)?;
        if Self::is_root_skill_root(target_root) {
            return Err(format!(
                "ordinary skills plane cannot {:?} the system-controlled ROOT skill root",
                action
            ));
        }
        if !Self::is_user_mutable_skill_root(target_root) {
            return Err(format!(
                "ordinary skills plane can only {:?} PROJECT or USER skill roots; got '{}'",
                action, target_root.name
            ));
        }
        Ok(())
    }

    /// Ensure one authority may write the requested target root.
    /// 确保单个权限等级可以写入请求的目标根。
    fn validate_authority_for_target_root(
        authority: SkillManagementAuthority,
        target_root: &RuntimeSkillRoot,
        action: crate::skill::manager::SkillLifecycleAction,
    ) -> Result<(), String> {
        if authority == SkillManagementAuthority::DelegatedTool
            && Self::is_root_skill_root(target_root)
        {
            return Err(format!(
                "DelegatedTool authority cannot {:?} the system-controlled ROOT skill root",
                action
            ));
        }
        Ok(())
    }

    /// Return the ROOT-owned declaration for one skill id when the root exists.
    /// 当 ROOT 存在时返回单个 skill id 的 ROOT 层声明。
    fn resolve_root_declared_skill_instance(
        skill_roots: &[RuntimeSkillRoot],
        skill_id: &str,
    ) -> Result<Option<ResolvedSkillInstance>, String> {
        let Some(root) = Self::find_skill_root_by_label(skill_roots, "ROOT") else {
            return Ok(None);
        };
        resolve_declared_skill_instance_from_roots(std::slice::from_ref(root), skill_id)
    }

    /// Reject PROJECT or USER install/update when ROOT already owns the same skill id.
    /// 当 ROOT 已拥有同名 skill id 时拒绝 PROJECT 或 USER 的安装与更新。
    fn ensure_root_skill_id_is_not_system_occupied(
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        skill_id: &str,
        action: crate::skill::manager::SkillLifecycleAction,
    ) -> Result<(), String> {
        if !matches!(
            action,
            crate::skill::manager::SkillLifecycleAction::Install
                | crate::skill::manager::SkillLifecycleAction::Update
        ) || !Self::is_user_mutable_skill_root(target_root)
        {
            return Ok(());
        }
        if let Some(root_instance) =
            Self::resolve_root_declared_skill_instance(skill_roots, skill_id)?
        {
            return Err(format!(
                "skill '{}' is managed by the ROOT system layer at {}; {:?} in '{}' is not allowed until the ROOT skill is removed",
                skill_id,
                render_log_friendly_path(&root_instance.actual_dir),
                action,
                target_root.name
            ));
        }
        Ok(())
    }

    /// Ensure one explicit target root will be effective after the staged apply operation.
    /// 确保单个显式目标根在暂存应用操作完成后会成为生效根。
    fn ensure_explicit_apply_target_will_be_effective(
        skill_roots: &[RuntimeSkillRoot],
        target_root: Option<&RuntimeSkillRoot>,
        skill_id: &str,
    ) -> Result<(), String> {
        let Some(target_root) = target_root else {
            return Ok(());
        };
        let target_index = Self::runtime_skill_root_index(skill_roots, target_root)?;
        let Some(effective_instance) =
            resolve_declared_skill_instance_from_roots(skill_roots, skill_id)?
        else {
            return Ok(());
        };
        let effective_root = RuntimeSkillRoot {
            name: effective_instance.root_name.clone(),
            skills_dir: effective_instance.skills_root.clone(),
        };
        let effective_index = Self::runtime_skill_root_index(skill_roots, &effective_root)?;
        if effective_index < target_index {
            return Err(format!(
                "skill '{}' in target root '{}' is shadowed by higher-priority root '{}'; update the higher-priority layer or remove that override before changing this fallback root",
                skill_id, target_root.name, effective_instance.root_name
            ));
        }
        Ok(())
    }

    /// Resolve final canonical entry names for all loaded skills with stable collision indexing.
    /// 为全部已加载 skill 解析最终 canonical 入口名，并以稳定顺序处理冲突编号。
    fn rebuild_entry_registry(&mut self) -> Result<(), String> {
        /// One unresolved entry candidate collected before collision indexing.
        /// 冲突编号前收集到的单个未解析入口候选项。
        #[derive(Clone)]
        struct EntrySeed {
            /// Internal storage key of the owning loaded skill.
            /// 所属已加载 skill 的内部存储键。
            skill_storage_key: String,
            /// Stable skill identifier declared in metadata.
            /// 元数据中声明的稳定 skill 标识符。
            skill_id: String,
            /// Stable local entry name declared by the skill.
            /// skill 声明的稳定局部入口名称。
            local_name: String,
            /// Unresolved `skill-entry` base name before numeric suffixing.
            /// 添加数字后缀前的未解析 `skill-entry` 基础名称。
            base_name: String,
            /// Deterministic tie-breaker based on directory basename.
            /// 基于目录基名的确定性并列打破键。
            directory_name: String,
            /// Module name used as the final low-level tie-breaker.
            /// 作为最终并列打破条件的模块名称。
            module_name: String,
        }

        let mut seeds = Vec::new();
        for (skill_storage_key, skill) in &self.skills {
            for tool in skill.meta.entries() {
                let local_name = tool.name.trim().to_string();
                if seeds.iter().any(|seed: &EntrySeed| {
                    seed.skill_storage_key == *skill_storage_key && seed.local_name == local_name
                }) {
                    return Err(format!(
                        "skill '{}' declares duplicate local entry name '{}'",
                        skill.meta.effective_skill_id(),
                        local_name
                    ));
                }

                let directory_name = skill
                    .dir
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        format!(
                            "loaded skill '{}' has invalid directory name: {}",
                            skill.meta.effective_skill_id(),
                            render_log_friendly_path(&skill.dir)
                        )
                    })?
                    .to_string();
                seeds.push(EntrySeed {
                    skill_storage_key: skill_storage_key.clone(),
                    skill_id: skill.meta.effective_skill_id().to_string(),
                    local_name: local_name.clone(),
                    base_name: skill.meta.tool_base_name(tool),
                    directory_name,
                    module_name: tool.lua_module.clone(),
                });
            }
        }

        seeds.sort_by(|left, right| {
            (
                left.base_name.as_str(),
                left.directory_name.as_str(),
                left.skill_id.as_str(),
                left.local_name.as_str(),
                left.module_name.as_str(),
            )
                .cmp(&(
                    right.base_name.as_str(),
                    right.directory_name.as_str(),
                    right.skill_id.as_str(),
                    right.local_name.as_str(),
                    right.module_name.as_str(),
                ))
        });

        for skill in self.skills.values_mut() {
            skill.resolved_entry_names.clear();
        }

        let mut registry = BTreeMap::new();
        let mut base_name_counters = HashMap::<String, usize>::new();
        let mut occupied_names = self
            .host_options
            .reserved_entry_names
            .iter()
            .cloned()
            .collect::<HashSet<String>>();
        for seed in seeds {
            let mut duplicate_index = *base_name_counters.get(&seed.base_name).unwrap_or(&0usize);
            let canonical_name = loop {
                duplicate_index += 1;
                let candidate_name = if duplicate_index == 1 {
                    seed.base_name.clone()
                } else {
                    format!("{}-{}", seed.base_name, duplicate_index)
                };
                if !occupied_names.contains(&candidate_name) {
                    break candidate_name;
                }
            };
            base_name_counters.insert(seed.base_name.clone(), duplicate_index);
            occupied_names.insert(canonical_name.clone());

            let resolved_target = ResolvedEntryTarget {
                canonical_name: canonical_name.clone(),
                skill_storage_key: seed.skill_storage_key.clone(),
                skill_id: seed.skill_id.clone(),
                local_name: seed.local_name.clone(),
            };
            registry.insert(canonical_name.clone(), resolved_target);

            let skill = self
                .skills
                .get_mut(&seed.skill_storage_key)
                .ok_or_else(|| {
                    format!(
                        "internal error: missing loaded skill '{}' while building entry registry",
                        seed.skill_storage_key
                    )
                })?;
            skill
                .resolved_entry_names
                .insert(seed.local_name.clone(), canonical_name);
        }

        self.entry_registry = registry;
        Ok(())
    }

    /// Load skills from an ordered root chain where earlier roots override later roots.
    /// 从有序根目录覆盖链加载技能，前面的根目录会覆盖后面的同名技能。
    pub fn load_from_roots(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
    ) -> Result<(), Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        self.runtime_skill_roots = skill_roots.to_vec();
        if !skill_roots.is_empty() {
            self.refresh_skill_config_runtime_root(skill_roots)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            self.validate_packaged_runtime_resources(skill_roots)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        }
        if !Self::any_runtime_skill_root_dir_exists(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
        {
            return Ok(());
        }

        for resolved_instance in collect_effective_skill_instances_from_roots(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
        {
            let skill_name = resolved_instance.skill_id;
            if self.is_host_ignored_skill(&skill_name) {
                log_info(format!(
                    "[LuaSkill] Skipped host-ignored skill '{}'",
                    skill_name
                ));
                continue;
            }
            let resolved_root = RuntimeSkillRoot {
                name: resolved_instance.root_name.clone(),
                skills_dir: resolved_instance.skills_root.clone(),
            };
            let resolved_skill_manager = self
                .skill_manager_for(&resolved_root)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            if !resolved_skill_manager.is_skill_enabled(&skill_name)? {
                log_warn(format!(
                    "[LuaSkill] Skipped disabled skill '{}'",
                    skill_name
                ));
                continue;
            }
            let actual_dir = resolved_instance.actual_dir;
            log_info(format!(
                "[LuaSkill] Loaded '{}' from root '{}'",
                skill_name, resolved_instance.root_name
            ));

            // Parsed manifest returned by dependency preparation and retained without a second read.
            // 由依赖准备返回并在不二次读取的情况下保留的已解析清单。
            let dependency_manifest =
                match self.ensure_skill_dependencies(&resolved_root, &actual_dir) {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        log_error(format!(
                            "[LuaSkill] Failed to prepare dependencies for {}: {}",
                            skill_name, error
                        ));
                        continue;
                    }
                };

            if let Err(e) = self.load_single_skill(
                &actual_dir,
                &resolved_instance.root_name,
                dependency_manifest,
            ) {
                log_error(format!("[LuaSkill] Failed to load {}: {}", skill_name, e));
            }
        }

        self.rebuild_entry_registry()
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;

        self.pool
            .prewarm(|| self.create_vm())
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        self.runlua_pool
            .prewarm(|| {
                Self::create_runlua_vm(RunLuaVmBuildContext::from_engine(
                    self,
                    &self.skills,
                    &self.entry_registry,
                ))
            })
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;

        log_info(format!("[LuaSkill] {} skills loaded", self.skills.len()));
        Ok(())
    }

    /// Reload all skills from one ordered root chain and rebuild runtime state from scratch.
    /// 从一条有序根目录覆盖链中重载全部技能，并从零重建运行时状态。
    pub fn reload_from_roots(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
    ) -> Result<(), Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let previous_entries = self
            .list_entries()
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        // Exact ordinary Skill owners retired only after the replacement state is ready.
        // 仅在替换状态准备完成后退役的精确普通 Skill 所有者。
        let retired_skill_owner_tokens: Vec<_> = self
            .skills
            .values()
            .map(|skill| skill.managed_package.owner_token())
            .collect();
        let mut next = self.empty_reload_candidate()?;
        next.load_from_roots(skill_roots)?;
        self.replace_runtime_state_from(next);
        for owner_token in retired_skill_owner_tokens {
            if let Err(error) = self.retire_managed_runtime_owner(owner_token) {
                log_error(format!(
                    "[LuaSkill] Failed to retire managed runtime owner {owner_token} after Skill reload: {error}"
                ));
            }
        }
        self.emit_entry_registry_delta(previous_entries)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Ok(())
    }

    /// Execute one mutating skill lifecycle action in the requested operation plane and then reload the runtime view.
    /// 在指定操作平面执行一次会改变状态的技能生命周期动作，并在完成后立即重载运行时视图。
    fn mutate_skill_state_and_reload(
        &mut self,
        plane: SkillOperationPlane,
        action: crate::skill::manager::SkillLifecycleAction,
        skill_roots: &[RuntimeSkillRoot],
        skill_id: &str,
        reason: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        validate_luaskills_identifier(skill_id, "skill_id")
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let resolved_instance = resolve_declared_skill_instance_from_roots(skill_roots, skill_id)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("declared skill instance '{}' not found", skill_id).into()
            })?;
        let resolved_root = RuntimeSkillRoot {
            name: resolved_instance.root_name.clone(),
            skills_dir: resolved_instance.skills_root.clone(),
        };
        let manager = self
            .skill_manager_for(&resolved_root)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let removed_dependency_manifest =
            if action == crate::skill::manager::SkillLifecycleAction::Uninstall {
                self.load_skill_dependency_manifest(&resolved_instance.actual_dir)
                    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
            } else {
                None
            };
        if let Err(error) = manager.guard_operation(plane, action, skill_id) {
            self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
                plane,
                action,
                skill_id,
                root_name: Some(resolved_instance.root_name.clone()),
                skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
                status: "blocked",
                message: Some(error.clone()),
            });
            return Err(error.into());
        }
        let action_result = match action {
            crate::skill::manager::SkillLifecycleAction::Disable => manager
                .disable_skill_in_plane(plane, skill_id, reason)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() }),
            crate::skill::manager::SkillLifecycleAction::Enable => manager
                .enable_skill_in_plane(plane, skill_id)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() }),
            crate::skill::manager::SkillLifecycleAction::Uninstall => manager
                .uninstall_skill_at_path_in_plane(plane, skill_id, &resolved_instance.actual_dir)
                .map(|_| ())
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() }),
            _ => {
                return Err(format!("unsupported state mutation action {:?}", action).into());
            }
        };
        if let Err(error) = action_result {
            let message = error.to_string();
            self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
                plane,
                action,
                skill_id,
                root_name: Some(resolved_instance.root_name.clone()),
                skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
                status: "failed",
                message: Some(message),
            });
            return Err(error);
        }
        if action == crate::skill::manager::SkillLifecycleAction::Uninstall {
            let dependency_manager = DependencyManager::new(
                self.dependency_manager_config_for(&resolved_root)
                    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?,
            );
            dependency_manager
                .cleanup_uninstalled_skill_dependencies_from_roots(
                    skill_roots,
                    skill_id,
                    removed_dependency_manifest.as_ref(),
                )
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        }
        self.reload_from_roots(skill_roots)?;
        self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
            plane,
            action,
            skill_id,
            root_name: Some(resolved_instance.root_name.clone()),
            skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
            status: "completed",
            message: None,
        });
        Ok(())
    }

    /// Remove one optional skill-owned database directory when the caller explicitly requests it.
    /// 调用方显式请求时删除单个技能拥有的可选数据库目录。
    fn remove_skill_database_dir(
        &self,
        database_root: &Path,
        skill_id: &str,
        remove_requested: bool,
        database_label: &str,
    ) -> Result<(bool, bool), Box<dyn std::error::Error>> {
        if !remove_requested {
            return Ok((false, true));
        }
        let database_dir = database_root.join(database_label).join(skill_id);
        match fs::remove_dir_all(&database_dir) {
            Ok(()) => Ok((true, false)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok((false, false)),
            Err(error) => Err(format!(
                "failed to remove {database_label} directory {}: {}",
                render_log_friendly_path(&database_dir),
                error
            )
            .into()),
        }
    }

    /// Execute one uninstall action with explicit database-retention semantics and then reload the runtime view.
    /// 以显式数据库保留语义执行一次卸载动作，并在完成后重载运行时视图。
    fn uninstall_skill_and_reload(
        &mut self,
        plane: SkillOperationPlane,
        skill_roots: &[RuntimeSkillRoot],
        skill_id: &str,
        options: &SkillUninstallOptions,
    ) -> Result<SkillUninstallResult, Box<dyn std::error::Error>> {
        self.uninstall_skill_and_reload_in_root(plane, skill_roots, None, skill_id, options)
    }

    /// Execute one uninstall action against an optional explicit target root and then reload the full runtime view.
    /// 针对可选的显式目标根执行一次卸载动作，并随后重载完整运行时视图。
    fn uninstall_skill_and_reload_in_root(
        &mut self,
        plane: SkillOperationPlane,
        skill_roots: &[RuntimeSkillRoot],
        target_root: Option<&RuntimeSkillRoot>,
        skill_id: &str,
        options: &SkillUninstallOptions,
    ) -> Result<SkillUninstallResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        validate_luaskills_identifier(skill_id, "skill_id")
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        if let Some(target_root) = target_root {
            Self::runtime_skill_root_index(skill_roots, target_root)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        }
        let target_roots = target_root.map(|root| vec![root.clone()]);
        let resolution_roots = target_roots.as_deref().unwrap_or(skill_roots);
        let resolved_instance = if target_root.is_some() {
            resolve_declared_skill_instance_from_roots(resolution_roots, skill_id)
        } else {
            resolve_effective_skill_instance_from_roots(resolution_roots, skill_id)
        }
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
        .ok_or_else(|| -> Box<dyn std::error::Error> {
            match target_root {
                Some(root) => format!(
                    "skill instance '{}' not found in target root '{}'",
                    skill_id, root.name
                )
                .into(),
                None => format!("effective skill instance '{}' not found", skill_id).into(),
            }
        })?;
        let resolved_root = RuntimeSkillRoot {
            name: resolved_instance.root_name.clone(),
            skills_dir: resolved_instance.skills_root.clone(),
        };
        let manager = self
            .skill_manager_for(&resolved_root)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let removed_dependency_manifest = self
            .load_skill_dependency_manifest(&resolved_instance.actual_dir)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        if let Err(error) = manager.guard_operation(
            plane,
            crate::skill::manager::SkillLifecycleAction::Uninstall,
            skill_id,
        ) {
            self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
                plane,
                action: crate::skill::manager::SkillLifecycleAction::Uninstall,
                skill_id,
                root_name: Some(resolved_instance.root_name.clone()),
                skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
                status: "blocked",
                message: Some(error.clone()),
            });
            return Err(error.into());
        }
        let prepared_uninstall = match manager.prepare_uninstall_skill_at_path_in_plane(
            plane,
            skill_id,
            &resolved_instance.actual_dir,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                let message = error.to_string();
                self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
                    plane,
                    action: crate::skill::manager::SkillLifecycleAction::Uninstall,
                    skill_id,
                    root_name: Some(resolved_instance.root_name.clone()),
                    skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
                    status: "failed",
                    message: Some(message),
                });
                return Err(error.into());
            }
        };
        if let Err(reload_error) = self.reload_from_roots(skill_roots) {
            let rollback_error = manager.rollback_prepared_skill_uninstall(&prepared_uninstall);
            let restore_error = self.reload_from_roots(skill_roots);
            let message = format_lifecycle_recovery_error(
                format!(
                    "Failed to reload LuaSkills after uninstall: {}",
                    reload_error
                ),
                rollback_error,
                restore_error,
            );
            self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
                plane,
                action: crate::skill::manager::SkillLifecycleAction::Uninstall,
                skill_id,
                root_name: Some(resolved_instance.root_name.clone()),
                skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
                status: "failed",
                message: Some(message.clone()),
            });
            return Err(message.into());
        }
        let mut result = match manager.commit_prepared_skill_uninstall(&prepared_uninstall) {
            Ok(result) => result,
            Err(error) => {
                let rollback_error = manager.rollback_prepared_skill_uninstall(&prepared_uninstall);
                let restore_error = self.reload_from_roots(skill_roots);
                let message = format_lifecycle_recovery_error(
                    format!("Failed to finalize uninstall: {}", error),
                    rollback_error,
                    restore_error,
                );
                self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
                    plane,
                    action: crate::skill::manager::SkillLifecycleAction::Uninstall,
                    skill_id,
                    root_name: Some(resolved_instance.root_name.clone()),
                    skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
                    status: "failed",
                    message: Some(message.clone()),
                });
                return Err(message.into());
            }
        };
        let dependency_manager = DependencyManager::new(
            self.dependency_manager_config_for(&resolved_root)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?,
        );
        if let Err(error) = dependency_manager.cleanup_uninstalled_skill_dependencies_from_roots(
            skill_roots,
            skill_id,
            removed_dependency_manifest.as_ref(),
        ) {
            log_warn(format!(
                "[LuaSkills:uninstall] Stale dependency cleanup warning for skill '{}': {}",
                skill_id, error
            ));
            result.message = format!(
                "{} (warning: stale dependency cleanup failed: {})",
                result.message, error
            );
        }
        let (sqlite_removed, sqlite_retained) = match self.remove_skill_database_dir(
            &self.database_root_for(&resolved_root),
            skill_id,
            options.remove_sqlite,
            "sqlite",
        ) {
            Ok(result) => result,
            Err(error) => {
                log_warn(format!(
                    "[LuaSkills:uninstall] SQLite cleanup warning for skill '{}': {}",
                    skill_id, error
                ));
                result.message = format!(
                    "{} (warning: sqlite cleanup failed: {})",
                    result.message, error
                );
                (false, false)
            }
        };
        let (lancedb_removed, lancedb_retained) = match self.remove_skill_database_dir(
            &self.database_root_for(&resolved_root),
            skill_id,
            options.remove_lancedb,
            "lancedb",
        ) {
            Ok(result) => result,
            Err(error) => {
                log_warn(format!(
                    "[LuaSkills:uninstall] LanceDB cleanup warning for skill '{}': {}",
                    skill_id, error
                ));
                result.message = format!(
                    "{} (warning: lancedb cleanup failed: {})",
                    result.message, error
                );
                (false, false)
            }
        };
        result.sqlite_removed = sqlite_removed;
        result.sqlite_retained = sqlite_retained;
        result.lancedb_removed = lancedb_removed;
        result.lancedb_retained = lancedb_retained;
        let summary = format!(
            "skill package removed={} sqlite_removed={} sqlite_retained={} lancedb_removed={} lancedb_retained={}",
            result.skill_removed,
            result.sqlite_removed,
            result.sqlite_retained,
            result.lancedb_removed,
            result.lancedb_retained
        );
        result.message = if result.message.is_empty() {
            summary
        } else {
            format!("{}; {}", summary, result.message)
        };
        self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
            plane,
            action: crate::skill::manager::SkillLifecycleAction::Uninstall,
            skill_id,
            root_name: Some(resolved_instance.root_name.clone()),
            skill_dir: Some(render_host_visible_path(&resolved_instance.actual_dir)),
            status: "completed",
            message: Some(result.message.clone()),
        });
        Ok(result)
    }

    /// Execute one install or update preflight request in the requested operation plane.
    /// 在指定操作平面执行一次安装或更新预检查请求。
    fn apply_skill_request(
        &mut self,
        plane: SkillOperationPlane,
        action: crate::skill::manager::SkillLifecycleAction,
        skill_roots: &[RuntimeSkillRoot],
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        self.apply_skill_request_in_root(plane, action, skill_roots, None, request)
    }

    /// Execute one install or update request against an optional explicit target root.
    /// 针对可选的显式目标根执行一次安装或更新请求。
    fn apply_skill_request_in_root(
        &mut self,
        plane: SkillOperationPlane,
        action: crate::skill::manager::SkillLifecycleAction,
        skill_roots: &[RuntimeSkillRoot],
        target_root: Option<&RuntimeSkillRoot>,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        let apply_action = SkillApplyLifecycleAction::from_lifecycle_action(action)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let explicit_target_root = target_root;
        if let Some(target_root) = explicit_target_root {
            Self::runtime_skill_root_index(skill_roots, target_root)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        }
        let requested_skill_id = resolve_requested_skill_id(request)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let explicit_target_roots = explicit_target_root.map(|root| vec![root.clone()]);
        let update_resolution_roots = explicit_target_roots.as_deref().unwrap_or(skill_roots);
        let target_root = match apply_action {
            SkillApplyLifecycleAction::Install => {
                if let Some(target_root) = explicit_target_root {
                    target_root.clone()
                } else {
                    self.default_install_skill_root(plane, skill_roots)?.clone()
                }
            }
            SkillApplyLifecycleAction::Update => {
                let resolved_instance = resolve_declared_skill_instance_from_roots(
                    update_resolution_roots,
                    &requested_skill_id,
                )
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
                .ok_or_else(|| -> Box<dyn std::error::Error> {
                    match explicit_target_root {
                        Some(root) => format!(
                            "skill '{}' is not installed in target root '{}'",
                            requested_skill_id, root.name
                        )
                        .into(),
                        None => format!("skill '{}' is not installed", requested_skill_id).into(),
                    }
                })?;
                RuntimeSkillRoot {
                    name: resolved_instance.root_name,
                    skills_dir: resolved_instance.skills_root,
                }
            }
        };
        Self::ensure_root_skill_id_is_not_system_occupied(
            skill_roots,
            &target_root,
            &requested_skill_id,
            action,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        if explicit_target_root.is_some() {
            Self::ensure_explicit_apply_target_will_be_effective(
                skill_roots,
                explicit_target_root,
                &requested_skill_id,
            )
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        }
        let operation_roots_owned = if let Some(target_root) = explicit_target_root {
            Some(vec![target_root.clone()])
        } else if apply_action == SkillApplyLifecycleAction::Install
            && plane == SkillOperationPlane::System
            && Self::is_root_skill_root(&target_root)
        {
            Some(vec![target_root.clone()])
        } else {
            None
        };
        let operation_roots = operation_roots_owned.as_deref().unwrap_or(skill_roots);
        let previous_dependency_manifest = if apply_action == SkillApplyLifecycleAction::Update {
            resolve_declared_skill_instance_from_roots(operation_roots, &requested_skill_id)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
                .and_then(|resolved| {
                    self.load_skill_dependency_manifest(&resolved.actual_dir)
                        .transpose()
                })
                .transpose()
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
        } else {
            None
        };
        let progress = RuntimeSkillOperationProgressEmitter::new(
            plane,
            action,
            Some(target_root.name.clone()),
            Some(requested_skill_id.clone()),
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        progress.emit(
            "validating_request",
            "completed",
            Some("skill lifecycle request accepted".to_string()),
        );
        let manager = match self.skill_manager_for_with_progress(&target_root, progress.clone()) {
            Ok(manager) => manager,
            Err(error) => {
                progress.emit("failed", "failed", Some(error.clone()));
                return Err(error.into());
            }
        };
        let prepared = match apply_action {
            SkillApplyLifecycleAction::Install => {
                match manager.prepare_install_skill(plane, operation_roots, request) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        progress.emit("failed", "failed", Some(error.clone()));
                        return Err(error.into());
                    }
                }
            }
            SkillApplyLifecycleAction::Update => {
                match manager.prepare_update_skill(plane, operation_roots, request) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        progress.emit("failed", "failed", Some(error.clone()));
                        return Err(error.into());
                    }
                }
            }
        };
        let mut result = match &prepared {
            PreparedSkillApply::Immediate(result) => result.clone(),
            PreparedSkillApply::Install(_) | PreparedSkillApply::Update(_) => {
                progress.emit(
                    "reloading_runtime",
                    "started",
                    Some("reloading LuaSkills runtime after staged change".to_string()),
                );
                if let Err(reload_error) = self.reload_from_roots(skill_roots) {
                    let rollback_error = manager.rollback_prepared_skill_apply(&prepared);
                    let restore_error = self.reload_from_roots(skill_roots);
                    let message = format_lifecycle_recovery_error(
                        format!(
                            "Failed to reload LuaSkills after {:?}: {}",
                            action, reload_error
                        ),
                        rollback_error,
                        restore_error,
                    );
                    progress.emit("failed", "failed", Some(message.clone()));
                    return Err(message.into());
                }

                progress.emit(
                    "committing",
                    "started",
                    Some("committing staged skill lifecycle change".to_string()),
                );
                let committed = manager.commit_prepared_skill_apply(&prepared).map_err(
                    |error| -> Box<dyn std::error::Error> {
                        let rollback_error = manager.rollback_prepared_skill_apply(&prepared);
                        let restore_error = self.reload_from_roots(skill_roots);
                        let message = format_lifecycle_recovery_error(
                            format!("Failed to finalize {:?}: {}", action, error),
                            rollback_error,
                            restore_error,
                        );
                        progress.emit("failed", "failed", Some(message.clone()));
                        message.into()
                    },
                )?;
                progress.emit(
                    "committing",
                    "completed",
                    Some("skill lifecycle change committed".to_string()),
                );
                committed
            }
        };
        if action == crate::skill::manager::SkillLifecycleAction::Update
            && result.status == "updated"
        {
            let current_dependency_manifest =
                resolve_declared_skill_instance_from_roots(operation_roots, &result.skill_id)
                    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?
                    .and_then(|resolved| {
                        self.load_skill_dependency_manifest(&resolved.actual_dir)
                            .transpose()
                    })
                    .transpose()
                    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            let dependency_manager = DependencyManager::new(
                self.dependency_manager_config_for(&target_root)
                    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?,
            );
            if let Err(error) = dependency_manager.cleanup_updated_skill_dependencies(
                &result.skill_id,
                previous_dependency_manifest.as_ref(),
                current_dependency_manifest.as_ref(),
            ) {
                log_warn(format!(
                    "[LuaSkills:update] Stale dependency cleanup warning for skill '{}': {}",
                    result.skill_id, error
                ));
                result.message = format!(
                    "{} (warning: stale dependency cleanup failed: {})",
                    result.message, error
                );
            }
        }
        let resolved_instance =
            resolve_declared_skill_instance_from_roots(operation_roots, &result.skill_id)
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        self.emit_skill_lifecycle_event(SkillLifecycleEventDraft {
            plane,
            action,
            skill_id: &result.skill_id,
            root_name: resolved_instance
                .as_ref()
                .map(|instance| instance.root_name.clone()),
            skill_dir: resolved_instance
                .as_ref()
                .map(|instance| render_host_visible_path(&instance.actual_dir)),
            status: &result.status,
            message: Some(result.message.clone()),
        });
        progress.emit("completed", "completed", Some(result.message.clone()));
        Ok(result)
    }

    /// Mark one skill disabled through the ordinary skills plane using an ordered root chain and immediately reload the runtime view.
    /// 通过普通 skills 平面使用有序根目录链将单个技能标记为停用，并立即重载运行时视图。
    pub fn disable_skill_in_roots(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        skill_id: &str,
        reason: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.mutate_skill_state_and_reload(
            SkillOperationPlane::Skills,
            crate::skill::manager::SkillLifecycleAction::Disable,
            skill_roots,
            skill_id,
            reason,
        )
    }

    /// Mark one skill disabled through the host-controlled system plane using an ordered root chain and immediately reload the runtime view.
    /// 通过宿主控制的 system 平面使用有序根目录链将单个技能标记为停用，并立即重载运行时视图。
    pub fn system_disable_skill_in_roots(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        authority: SkillManagementAuthority,
        skill_id: &str,
        reason: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.mutate_skill_state_and_reload(
            Self::operation_plane_for_authority(authority),
            crate::skill::manager::SkillLifecycleAction::Disable,
            skill_roots,
            skill_id,
            reason,
        )
    }

    /// Remove the disabled marker of one skill through the ordinary skills plane and immediately reload the runtime view.
    /// 通过普通 skills 平面移除单个技能的停用标记，并立即重载运行时视图。
    pub fn enable_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        skill_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.mutate_skill_state_and_reload(
            SkillOperationPlane::Skills,
            crate::skill::manager::SkillLifecycleAction::Enable,
            skill_roots,
            skill_id,
            None,
        )
    }

    /// Remove the disabled marker of one skill through the host-controlled system plane and immediately reload the runtime view.
    /// 通过宿主控制的 system 平面移除单个技能的停用标记，并立即重载运行时视图。
    pub fn system_enable_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        authority: SkillManagementAuthority,
        skill_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.mutate_skill_state_and_reload(
            Self::operation_plane_for_authority(authority),
            crate::skill::manager::SkillLifecycleAction::Enable,
            skill_roots,
            skill_id,
            None,
        )
    }

    /// Uninstall one skill directory through the ordinary skills plane and immediately reload the runtime view.
    /// 通过普通 skills 平面卸载单个技能目录，并立即重载运行时视图。
    pub fn uninstall_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        skill_id: &str,
        options: &SkillUninstallOptions,
    ) -> Result<SkillUninstallResult, Box<dyn std::error::Error>> {
        self.uninstall_skill_and_reload(SkillOperationPlane::Skills, skill_roots, skill_id, options)
    }

    /// Uninstall one skill through the ordinary skills plane from an explicit PROJECT or USER root.
    /// 通过普通 skills 平面从显式 PROJECT 或 USER 根卸载单个技能。
    pub fn uninstall_skill_in_root(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        skill_id: &str,
        options: &SkillUninstallOptions,
    ) -> Result<SkillUninstallResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_ordinary_target_root(
            skill_roots,
            target_root,
            crate::skill::manager::SkillLifecycleAction::Uninstall,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        self.uninstall_skill_and_reload_in_root(
            SkillOperationPlane::Skills,
            skill_roots,
            Some(target_root),
            skill_id,
            options,
        )
    }

    /// Uninstall one skill directory through the host-controlled system plane and immediately reload the runtime view.
    /// 通过宿主控制的 system 平面卸载单个技能目录，并立即重载运行时视图。
    pub fn system_uninstall_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        authority: SkillManagementAuthority,
        skill_id: &str,
        options: &SkillUninstallOptions,
    ) -> Result<SkillUninstallResult, Box<dyn std::error::Error>> {
        self.uninstall_skill_and_reload(
            Self::operation_plane_for_authority(authority),
            skill_roots,
            skill_id,
            options,
        )
    }

    /// Uninstall one skill through the host-controlled system plane from an explicit target root.
    /// 通过宿主控制的 system 平面从显式目标根卸载单个技能。
    pub fn system_uninstall_skill_in_root(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        authority: SkillManagementAuthority,
        skill_id: &str,
        options: &SkillUninstallOptions,
    ) -> Result<SkillUninstallResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::runtime_skill_root_index(skill_roots, target_root)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_authority_for_target_root(
            authority,
            target_root,
            crate::skill::manager::SkillLifecycleAction::Uninstall,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let plane = Self::operation_plane_for_authority(authority);
        self.uninstall_skill_and_reload_in_root(
            plane,
            skill_roots,
            Some(target_root),
            skill_id,
            options,
        )
    }

    /// Preflight one install request through the ordinary skills plane and return a structured result.
    /// 通过普通 skills 平面对一次安装请求执行预检查，并返回结构化结果。
    pub fn install_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        self.apply_skill_request(
            SkillOperationPlane::Skills,
            crate::skill::manager::SkillLifecycleAction::Install,
            skill_roots,
            request,
        )
    }

    /// Preflight one install request through the ordinary skills plane into an explicit PROJECT or USER root.
    /// 通过普通 skills 平面将一次安装请求预检查并写入显式 PROJECT 或 USER 根。
    pub fn install_skill_in_root(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_ordinary_target_root(
            skill_roots,
            target_root,
            crate::skill::manager::SkillLifecycleAction::Install,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        self.apply_skill_request_in_root(
            SkillOperationPlane::Skills,
            crate::skill::manager::SkillLifecycleAction::Install,
            skill_roots,
            Some(target_root),
            request,
        )
    }

    /// Preflight one install request through the host-controlled system plane and return a structured result.
    /// 通过宿主控制的 system 平面对一次安装请求执行预检查，并返回结构化结果。
    pub fn system_install_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        authority: SkillManagementAuthority,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        self.apply_skill_request(
            Self::operation_plane_for_authority(authority),
            crate::skill::manager::SkillLifecycleAction::Install,
            skill_roots,
            request,
        )
    }

    /// Preflight one install request through the host-controlled system plane into an explicit target root.
    /// 通过宿主控制的 system 平面将一次安装请求预检查并写入显式目标根。
    pub fn system_install_skill_in_root(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        authority: SkillManagementAuthority,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::runtime_skill_root_index(skill_roots, target_root)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_authority_for_target_root(
            authority,
            target_root,
            crate::skill::manager::SkillLifecycleAction::Install,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let plane = Self::operation_plane_for_authority(authority);
        self.apply_skill_request_in_root(
            plane,
            crate::skill::manager::SkillLifecycleAction::Install,
            skill_roots,
            Some(target_root),
            request,
        )
    }

    /// Preflight one update request through the ordinary skills plane and return a structured result.
    /// 通过普通 skills 平面对一次更新请求执行预检查，并返回结构化结果。
    pub fn update_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        self.apply_skill_request(
            SkillOperationPlane::Skills,
            crate::skill::manager::SkillLifecycleAction::Update,
            skill_roots,
            request,
        )
    }

    /// Preflight one update request through the ordinary skills plane against an explicit PROJECT or USER root.
    /// 通过普通 skills 平面对显式 PROJECT 或 USER 根执行一次更新预检查。
    pub fn update_skill_in_root(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_ordinary_target_root(
            skill_roots,
            target_root,
            crate::skill::manager::SkillLifecycleAction::Update,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        self.apply_skill_request_in_root(
            SkillOperationPlane::Skills,
            crate::skill::manager::SkillLifecycleAction::Update,
            skill_roots,
            Some(target_root),
            request,
        )
    }

    /// Preflight one update request through the host-controlled system plane and return a structured result.
    /// 通过宿主控制的 system 平面对一次更新请求执行预检查，并返回结构化结果。
    pub fn system_update_skill(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        authority: SkillManagementAuthority,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        self.apply_skill_request(
            Self::operation_plane_for_authority(authority),
            crate::skill::manager::SkillLifecycleAction::Update,
            skill_roots,
            request,
        )
    }

    /// Preflight one update request through the host-controlled system plane against an explicit target root.
    /// 通过宿主控制的 system 平面对显式目标根执行一次更新预检查。
    pub fn system_update_skill_in_root(
        &mut self,
        skill_roots: &[RuntimeSkillRoot],
        target_root: &RuntimeSkillRoot,
        authority: SkillManagementAuthority,
        request: &SkillInstallRequest,
    ) -> Result<SkillApplyResult, Box<dyn std::error::Error>> {
        Self::validate_formal_skill_root_chain(skill_roots)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::runtime_skill_root_index(skill_roots, target_root)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        Self::validate_authority_for_target_root(
            authority,
            target_root,
            crate::skill::manager::SkillLifecycleAction::Update,
        )
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        let plane = Self::operation_plane_for_authority(authority);
        self.apply_skill_request_in_root(
            plane,
            crate::skill::manager::SkillLifecycleAction::Update,
            skill_roots,
            Some(target_root),
            request,
        )
    }

    /// Emit one structured lifecycle event through the host callback bridge.
    /// 通过宿主回调桥发送一条结构化生命周期事件。
    ///
    /// Parameters: `event` carries the complete lifecycle payload before conversion to the public callback type.
    /// 参数：`event` 携带转换为公开回调类型之前的完整生命周期载荷。
    ///
    /// Returns: nothing; the registered host callback is invoked only when present.
    /// 返回：无返回值；仅在宿主已注册回调时调用该回调。
    fn emit_skill_lifecycle_event(&self, event: SkillLifecycleEventDraft<'_>) {
        crate::host::callbacks::emit_skill_lifecycle_event(&RuntimeSkillLifecycleEvent {
            plane: event.plane,
            action: event.action,
            skill_id: event.skill_id.to_string(),
            root_name: event.root_name,
            skill_dir: event.skill_dir,
            status: event.status.to_string(),
            message: event.message,
        });
    }

    /// Compare pre-reload and post-reload entry snapshots and emit one host-visible registry delta.
    /// 对比重载前后入口快照并发出一条宿主可见的注册表差异事件。
    fn emit_entry_registry_delta(
        &self,
        previous_entries: Vec<RuntimeEntryDescriptor>,
    ) -> Result<(), String> {
        let current_entries = self.list_entries()?;
        let previous_map = previous_entries
            .into_iter()
            .map(|entry| (entry.canonical_name.clone(), entry))
            .collect::<BTreeMap<String, RuntimeEntryDescriptor>>();
        let current_map = current_entries
            .into_iter()
            .map(|entry| (entry.canonical_name.clone(), entry))
            .collect::<BTreeMap<String, RuntimeEntryDescriptor>>();

        let mut added_entries = Vec::new();
        let mut updated_entries = Vec::new();
        let mut removed_entry_names = Vec::new();

        for (canonical_name, current_entry) in &current_map {
            match previous_map.get(canonical_name) {
                None => added_entries.push(current_entry.clone()),
                Some(previous_entry) if previous_entry != current_entry => {
                    updated_entries.push(current_entry.clone());
                }
                Some(_) => {}
            }
        }

        for canonical_name in previous_map.keys() {
            if !current_map.contains_key(canonical_name) {
                removed_entry_names.push(canonical_name.clone());
            }
        }

        if added_entries.is_empty() && updated_entries.is_empty() && removed_entry_names.is_empty()
        {
            return Ok(());
        }

        crate::host::callbacks::emit_entry_registry_delta(&RuntimeEntryRegistryDelta {
            added_entries,
            removed_entry_names,
            updated_entries,
        });
        Ok(())
    }

    /// Load a single skill from its directory.
    fn load_single_skill(
        &mut self,
        dir: &Path,
        root_name: &str,
        dependency_manifest: Option<PackageDependencyManifest>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let skill_yaml = dir.join("skill.yaml");
        // Skill manifest existence probe that preserves filesystem errors before reading YAML.
        // 读取 YAML 前保留文件系统错误的 skill 清单存在性探测。
        let skill_yaml_exists = required_skill_file_path_exists(&skill_yaml, "skill.yaml", dir)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        if !skill_yaml_exists {
            return Err(
                format!("skill.yaml not found in {}", render_log_friendly_path(dir)).into(),
            );
        }

        let yaml_str = std::fs::read_to_string(&skill_yaml)?;
        let yaml_value: serde_yaml::Value = serde_yaml::from_str(&yaml_str)?;
        if yaml_value.as_mapping().is_some_and(|mapping| {
            mapping.contains_key(serde_yaml::Value::String("skill_id".to_string()))
        }) {
            return Err(format!(
                "skill {} must not declare skill_id in skill.yaml; directory name is the only skill_id",
                render_log_friendly_path(dir)
            )
            .into());
        }
        let mut meta: SkillMeta = serde_yaml::from_value(yaml_value)?;
        let directory_skill_id = dir
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                format!(
                    "invalid skill directory name: {}",
                    render_log_friendly_path(dir)
                )
            })?
            .trim()
            .to_string();
        validate_luaskills_identifier(&directory_skill_id, "skill directory name")
            .map_err(|error| format!("skill {}: {}", render_log_friendly_path(dir), error))?;
        meta.bind_directory_skill_id(directory_skill_id.clone());

        if !meta.is_enabled() {
            log_info(format!(
                "[LuaSkill] Skip disabled skill '{}'",
                meta.effective_skill_id()
            ));
            return Ok(());
        }
        meta.resolve_entry_input_schemas(dir)
            .map_err(|error| format!("skill {}: {}", meta.name, error))?;
        validate_luaskills_identifier(meta.effective_skill_id(), "skill_id")
            .map_err(|error| format!("skill {}: {}", meta.name, error))?;
        validate_luaskills_version(meta.version(), "version")
            .map_err(|error| format!("skill {}: {}", meta.effective_skill_id(), error))?;

        if meta.entries.is_empty() {
            return Err(format!("skill {} must declare at least one entry", meta.name).into());
        }

        for tool in meta.entries() {
            validate_luaskills_identifier(tool.name.trim(), "entry.name").map_err(|error| {
                format!("skill {} entry {}: {}", meta.name, tool.name.trim(), error)
            })?;
            if tool.lua_entry.trim().is_empty() || tool.lua_module.trim().is_empty() {
                return Err(format!(
                    "skill {} declares entry {} but lua_entry/lua_module is missing",
                    meta.name, tool.name
                )
                .into());
            }

            validate_skill_relative_path(&tool.lua_entry, "runtime", "entry.lua_entry")
                .map_err(|error| format!("skill {} entry {}: {}", meta.name, tool.name, error))?;

            let lua_path = tool_entry_path(dir, tool);
            // Lua entry existence probe that preserves filesystem errors before runtime compilation.
            // 运行时编译前保留文件系统错误的 Lua 入口存在性探测。
            let lua_entry_label = format!("Lua entry {}", tool.lua_entry);
            let lua_entry_exists =
                required_skill_file_path_exists(&lua_path, &lua_entry_label, dir)
                    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            if !lua_entry_exists {
                return Err(format!(
                    "Lua entry {} not found in {}",
                    tool.lua_entry,
                    render_log_friendly_path(dir)
                )
                .into());
            }
        }

        if !meta.help.main.file.trim().is_empty() {
            validate_skill_relative_path(&meta.help.main.file, "help", "help.main.file")
                .map_err(|error| format!("skill {} help main: {}", meta.name, error))?;
        }
        for topic in &meta.help.topics {
            validate_skill_relative_path(&topic.file, "help", "help.topic.file").map_err(
                |error| {
                    format!(
                        "skill {} help topic {}: {}",
                        meta.name,
                        topic.name.trim(),
                        error
                    )
                },
            )?;
        }

        let effective_lancedb = meta.effective_lancedb();
        let lancedb_binding = if effective_lancedb.enable {
            if self.lancedb_host.is_none() {
                self.lancedb_host = Some(Arc::new(
                    LanceDbSkillHost::new(
                        self.host_options.as_ref().clone(),
                        self.database_provider_callbacks.clone(),
                    )
                    .map_err(|error| {
                        format!("Failed to initialize LanceDB skill host: {}", error)
                    })?,
                ));
            }

            let host = self
                .lancedb_host
                .as_ref()
                .ok_or("LanceDB skill host missing after initialization")?
                .clone();

            Some(
                host.register_skill(root_name, meta.effective_skill_id(), dir, effective_lancedb)
                    .map_err(|error| {
                        format!(
                            "Failed to register LanceDB for skill {}: {}",
                            meta.effective_skill_id(),
                            error
                        )
                    })?,
            )
        } else {
            None
        };

        let effective_sqlite = meta.effective_sqlite();
        let sqlite_binding = if effective_sqlite.enable {
            if self.sqlite_host.is_none() {
                self.sqlite_host = Some(Arc::new(
                    SqliteSkillHost::new(
                        self.host_options.as_ref().clone(),
                        self.database_provider_callbacks.clone(),
                    )
                    .map_err(|error| {
                        format!("Failed to initialize SQLite skill host: {}", error)
                    })?,
                ));
            }

            let host = self
                .sqlite_host
                .as_ref()
                .ok_or("SQLite skill host missing after initialization")?
                .clone();

            Some(
                host.register_skill(root_name, meta.effective_skill_id(), dir, effective_sqlite)
                    .map_err(|error| {
                        format!(
                            "Failed to register SQLite for skill {}: {}",
                            meta.effective_skill_id(),
                            error
                        )
                    })?,
            )
        } else {
            None
        };

        // Authoritative named root selected by the loader before package construction.
        // 包构造前由加载器选择的权威命名根。
        let skill_root = self
            .runtime_skill_roots
            .iter()
            .find(|skill_root| skill_root.name == root_name)
            .ok_or_else(|| format!("runtime skill root `{root_name}` is not registered"))?;
        // Runtime root derived from the selected host-provided RuntimeSkillRoot.
        // 从选定宿主提供 RuntimeSkillRoot 派生的运行时根。
        let runtime_root = self.runtime_root_for(skill_root);
        // Canonical managed package context shared by every entry of this Skill.
        // 由当前 Skill 所有入口共享的规范受管包上下文。
        let managed_package = ManagedRuntimePackageContext::for_skill(
            meta.effective_skill_id(),
            dir,
            &runtime_root,
            dependency_manifest,
        )?;

        self.skills.insert(
            meta.effective_skill_id().to_string(),
            LoadedSkill {
                meta,
                dir: dir.to_path_buf(),
                root_name: root_name.to_string(),
                managed_package,
                lancedb_binding,
                sqlite_binding,
                resolved_entry_names: HashMap::new(),
            },
        );

        Ok(())
    }

    /// Build a fresh Lua VM instance from one explicit runtime state snapshot.
    /// 基于一份显式运行时状态快照创建全新的 Lua 虚拟机实例。
    fn create_vm_with_runtime_state(
        &self,
        skills: HashMap<String, LoadedSkill>,
        entry_registry: BTreeMap<String, ResolvedEntryTarget>,
    ) -> Result<LuaVm, String> {
        let skills = Arc::new(skills);
        let entry_registry = Arc::new(entry_registry);
        let lua = unsafe { Lua::unsafe_new() };
        lua.set_app_data(Arc::clone(&self.managed_runtime_services));
        lua.set_app_data(Arc::clone(&self.managed_runtime_workers));
        Self::setup_package_paths(&lua, self.host_options.as_ref())
            .map_err(|error| error.to_string())?;
        Self::register_vulcan_module(
            &lua,
            self.host_options.as_ref(),
            self.skill_config_store.clone(),
            &self.runtime_skill_roots,
        )
        .map_err(|error| error.to_string())?;
        Self::populate_vulcan_luaexec_bridge(
            &lua,
            RunLuaRuntimeContext::from_engine(self, skills.clone(), entry_registry.clone()),
        )?;
        Self::register_skill_functions(&lua, skills.as_ref())?;
        Self::populate_vulcan_call_for_lua(
            &lua,
            skills.as_ref(),
            entry_registry.as_ref(),
            self.host_options.clone(),
            self.lancedb_host.clone(),
            self.sqlite_host.clone(),
        )?;
        Ok(LuaVm {
            lua,
            last_used_at: Instant::now(),
        })
    }

    /// Build a fresh Lua VM instance with all loaded skills registered.
    /// 创建一个全新的 Lua 虚拟机实例，并注册当前已加载的全部技能。
    fn create_vm(&self) -> Result<LuaVm, String> {
        self.create_vm_with_runtime_state(self.skills.clone(), self.entry_registry.clone())
    }

    /// Borrow a Lua VM from the pool for one operation.
    /// 从虚拟机池借出一个 Lua 实例执行一次操作。
    fn acquire_vm(&self) -> Result<LuaVmLease, String> {
        self.pool.acquire(|| self.create_vm())
    }

    /// Build a fresh isolated runlua VM instance with current runtime state registered.
    /// 创建一个带有当前运行时状态注册信息的全新隔离 runlua 虚拟机实例。
    fn create_runlua_vm(context: RunLuaVmBuildContext<'_>) -> Result<LuaVm, String> {
        // Complete engine snapshot destructured once for deterministic VM initialization.
        // 为确定性 VM 初始化仅解构一次的完整引擎快照。
        let RunLuaVmBuildContext {
            skills,
            entry_registry,
            host_options,
            skill_config_store,
            runtime_skill_roots,
            lancedb_host,
            sqlite_host,
            managed_runtime_services,
            managed_runtime_workers,
        } = context;
        let lua = unsafe { Lua::unsafe_new() };
        lua.set_app_data(managed_runtime_services);
        lua.set_app_data(managed_runtime_workers);
        Self::setup_package_paths(&lua, host_options.as_ref())
            .map_err(|error| error.to_string())?;
        Self::register_vulcan_module(
            &lua,
            host_options.as_ref(),
            skill_config_store,
            &runtime_skill_roots,
        )
        .map_err(|error| error.to_string())?;
        Self::register_skill_functions(&lua, skills)?;
        Self::populate_vulcan_call_for_lua(
            &lua,
            skills,
            entry_registry,
            host_options,
            lancedb_host,
            sqlite_host,
        )?;
        Ok(LuaVm {
            lua,
            last_used_at: Instant::now(),
        })
    }

    /// Build a dedicated System runtime VM without capturing ordinary Skill state.
    /// 构造不捕获普通 Skill 状态的专用 System 运行时 VM。
    fn create_system_runtime_vm(&self) -> Result<LuaVm, String> {
        // Empty Skill registry prevents System leases from retaining reloadable dispatch entries.
        // 空 Skill 注册表阻止 System 租约保留可重载分发入口。
        let skills = HashMap::new();
        // Empty entry registry keeps `vulcan.call` detached from ordinary Skill targets.
        // 空入口注册表使 `vulcan.call` 与普通 Skill 目标解耦。
        let entry_registry = BTreeMap::new();
        let vm = Self::create_runlua_vm(RunLuaVmBuildContext {
            skills: &skills,
            entry_registry: &entry_registry,
            host_options: Arc::clone(&self.host_options),
            skill_config_store: Arc::clone(&self.skill_config_store),
            runtime_skill_roots: Vec::new(),
            lancedb_host: None,
            sqlite_host: None,
            managed_runtime_services: Arc::clone(&self.managed_runtime_services),
            managed_runtime_workers: Arc::clone(&self.managed_runtime_workers),
        })?;
        // Dedicated System VMs permanently protect host-owned context fields before plugin code runs.
        // 专用 System VM 会在插件代码运行前永久保护宿主所有上下文字段。
        lease::install_system_runtime_context_boundary(&vm.lua)?;
        Ok(vm)
    }

    /// Register all tool-bearing skill entries into a specific Lua VM.
    /// 将所有声明了工具入口的 skill 条目注册到指定 Lua 虚拟机中。
    fn register_skill_functions(
        lua: &Lua,
        skills: &HashMap<String, LoadedSkill>,
    ) -> Result<(), String> {
        for skill in skills.values() {
            for tool in skill.meta.entries() {
                Self::compile_skill_into_lua(lua, skill, tool, false)?;
            }
        }
        Ok(())
    }

    /// Compile one tool entry into the target Lua VM.
    /// 将单个工具入口编译并注册到目标 Lua 虚拟机中。
    fn compile_skill_into_lua(
        lua: &Lua,
        skill: &LoadedSkill,
        tool: &crate::lua_skill::SkillToolMeta,
        always_reload: bool,
    ) -> Result<(), String> {
        let lua_path = tool_entry_path(&skill.dir, tool);
        let source = std::fs::read_to_string(&lua_path).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_log_friendly_path(&lua_path),
                error
            )
        })?;
        if always_reload {
            log_info(format!(
                "[LuaSkill] Hot reload {}: {}",
                tool.lua_module,
                render_log_friendly_path(&lua_path)
            ));
        }

        let chunk = lua.load(&source).set_name(&tool.lua_module);
        let outer: Function = chunk.into_function().map_err(|error| {
            format!(
                "Failed to compile skill '{}::{}': {}",
                skill.meta.name, tool.lua_module, error
            )
        })?;
        let handler: Function = outer.call(()).map_err(|error| {
            format!(
                "Failed to initialize skill '{}::{}': {}",
                skill.meta.name, tool.lua_module, error
            )
        })?;
        lua.globals()
            .set(lua_skill_handler_global_name(&tool.lua_module), handler)
            .map_err(|error| {
                format!(
                    "Failed to register skill '{}::{}': {}",
                    skill.meta.name, tool.lua_module, error
                )
            })?;
        Ok(())
    }

    /// Return generic runtime entry descriptors for all loaded skills.
    /// 返回当前已加载全部 skill 的通用运行时入口描述。
    pub fn list_entries(&self) -> Result<Vec<RuntimeEntryDescriptor>, String> {
        // Entry descriptors collected in canonical registry order for stable host-visible listing.
        // 按 canonical 注册表顺序收集入口描述，确保宿主可见列表稳定。
        let mut descriptors = Vec::with_capacity(self.entry_registry.len());
        for target in self.entry_registry.values() {
            // Loaded skill that must still own the resolved registry target.
            // 必须仍然拥有已解析注册表目标的已加载 skill。
            let skill = self.skills.get(&target.skill_storage_key).ok_or_else(|| {
                format!(
                    "entry registry target '{}' references missing loaded skill storage key '{}'",
                    target.canonical_name, target.skill_storage_key
                )
            })?;
            // Local tool entry that must still exist on the owning skill metadata.
            // 必须仍然存在于所属 skill 元数据中的局部工具入口。
            let tool = skill
                .meta
                .find_tool_by_local_name(&target.local_name)
                .ok_or_else(|| {
                    format!(
                        "entry registry target '{}' references missing local entry '{}' in skill '{}'",
                        target.canonical_name, target.local_name, target.skill_id
                    )
                })?;
            // Resolved schema is a load-time invariant; absence means the runtime registry is internally inconsistent.
            // 已解析 schema 是加载期不变量；缺失说明运行时注册表内部生命周期不一致。
            let input_schema = tool.resolved_input_schema().map_err(|error| {
                format!(
                    "entry registry target '{}' has invalid resolved input schema: {}",
                    target.canonical_name, error
                )
            })?;
            descriptors.push(RuntimeEntryDescriptor {
                canonical_name: target.canonical_name.clone(),
                skill_id: target.skill_id.clone(),
                local_name: target.local_name.clone(),
                root_name: skill.root_name.clone(),
                skill_dir: render_host_visible_path(&skill.dir),
                description: tool.description.clone(),
                parameters: tool
                    .parameters
                    .iter()
                    .map(|parameter| RuntimeEntryParameterDescriptor {
                        name: parameter.name.clone(),
                        param_type: parameter.param_type.clone(),
                        description: parameter.description.clone(),
                        required: parameter.required,
                    })
                    .collect(),
                input_schema: input_schema.clone(),
            });
        }
        Ok(descriptors)
    }

    /// Return runtime entry descriptors visible to one host-injected skill-management authority.
    /// 返回单个宿主注入技能管理权限可见的运行时入口描述。
    pub fn list_entries_for_authority(
        &self,
        authority: SkillManagementAuthority,
    ) -> Result<Vec<RuntimeEntryDescriptor>, String> {
        Ok(self
            .list_entries()?
            .into_iter()
            .filter(|entry| {
                authority == SkillManagementAuthority::System
                    || Self::normalized_skill_root_name(&entry.root_name) != "ROOT"
            })
            .collect())
    }

    /// List all structured help trees currently registered in the runtime.
    /// 列出当前运行时中已注册的全部结构化帮助树。
    pub fn list_skill_help(&self) -> Result<Vec<RuntimeSkillHelpDescriptor>, String> {
        // Help descriptors collected from loaded skills before deterministic skill-id sorting.
        // 在按 skill id 确定性排序前，从已加载 skill 中收集的帮助描述。
        let mut descriptors = Vec::with_capacity(self.skills.len());
        for skill in self.skills.values() {
            // Main help node descriptor resolved against the current entry registry names.
            // 基于当前入口注册表名称解析出的主帮助节点描述。
            let main = self.build_help_node_descriptor(skill, skill.meta.main_help(), true)?;
            // Flow help node descriptors resolved against the current entry registry names.
            // 基于当前入口注册表名称解析出的流程帮助节点描述。
            let mut flows = Vec::new();
            for topic in skill.meta.help_topics() {
                flows.push(self.build_help_node_descriptor(skill, topic, false)?);
            }
            descriptors.push(RuntimeSkillHelpDescriptor {
                skill_id: skill.meta.effective_skill_id().to_string(),
                skill_name: skill.meta.name.clone(),
                skill_version: skill.meta.version().to_string(),
                root_name: skill.root_name.clone(),
                skill_dir: render_host_visible_path(&skill.dir),
                main,
                flows,
            });
        }

        descriptors.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
        Ok(descriptors)
    }

    /// List structured help trees visible to one host-injected skill-management authority.
    /// 列出单个宿主注入技能管理权限可见的结构化帮助树。
    pub fn list_skill_help_for_authority(
        &self,
        authority: SkillManagementAuthority,
    ) -> Result<Vec<RuntimeSkillHelpDescriptor>, String> {
        Ok(self
            .list_skill_help()?
            .into_iter()
            .filter(|help| {
                authority == SkillManagementAuthority::System
                    || Self::normalized_skill_root_name(&help.root_name) != "ROOT"
            })
            .collect())
    }

    /// Render one structured help detail payload for one skill flow node.
    /// 为单个 skill 流程节点渲染一份结构化帮助详情载荷。
    pub fn render_skill_help_detail(
        &self,
        skill_id: &str,
        flow_name: &str,
        request_context: Option<&RuntimeRequestContext>,
    ) -> Result<Option<RuntimeHelpDetail>, String> {
        let Some(skill) = self
            .skills
            .values()
            .find(|skill| skill.meta.effective_skill_id() == skill_id)
        else {
            return Ok(None);
        };

        let normalized_flow_name = flow_name.trim();
        if normalized_flow_name.is_empty() {
            return Err("Help flow name must not be empty".to_string());
        }

        let (selected_help, is_main) = if normalized_flow_name == "main" {
            (skill.meta.main_help(), true)
        } else {
            (
                skill
                    .meta
                    .find_help_topic(normalized_flow_name)
                    .ok_or_else(|| {
                        format!(
                            "Skill '{}' does not declare help flow '{}'",
                            skill.meta.effective_skill_id(),
                            normalized_flow_name
                        )
                    })?,
                false,
            )
        };

        let rendered_body =
            self.render_help_payload(skill, &selected_help.file, request_context)?;
        let descriptor = self.build_help_node_descriptor(skill, selected_help, is_main)?;
        Ok(Some(RuntimeHelpDetail {
            skill_id: skill.meta.effective_skill_id().to_string(),
            skill_name: skill.meta.name.clone(),
            skill_version: skill.meta.version().to_string(),
            root_name: skill.root_name.clone(),
            skill_dir: render_host_visible_path(&skill.dir),
            flow_name: descriptor.flow_name,
            description: descriptor.description,
            related_entries: descriptor.related_entries,
            is_main: descriptor.is_main,
            content_type: "markdown".to_string(),
            content: rendered_body,
        }))
    }

    /// Render help detail only when visible to one host-injected skill-management authority.
    /// 仅在单个宿主注入技能管理权限可见时渲染帮助详情。
    pub fn render_skill_help_detail_for_authority(
        &self,
        authority: SkillManagementAuthority,
        skill_id: &str,
        flow_name: &str,
        request_context: Option<&RuntimeRequestContext>,
    ) -> Result<Option<RuntimeHelpDetail>, String> {
        if authority == SkillManagementAuthority::DelegatedTool
            && self
                .skills
                .values()
                .find(|skill| skill.meta.effective_skill_id() == skill_id)
                .map(|skill| Self::normalized_skill_root_name(&skill.root_name) == "ROOT")
                .unwrap_or(false)
        {
            return Ok(None);
        }
        self.render_skill_help_detail(skill_id, flow_name, request_context)
    }

    /// Build one structured help node descriptor with related canonical entries.
    /// 构建单个结构化帮助节点描述及其关联 canonical 入口列表。
    fn build_help_node_descriptor(
        &self,
        skill: &LoadedSkill,
        help_node: &crate::lua_skill::SkillHelpNodeMeta,
        is_main: bool,
    ) -> Result<RuntimeHelpNodeDescriptor, String> {
        let flow_name = if is_main {
            "main".to_string()
        } else {
            help_node.name.trim().to_string()
        };
        // Related canonical entries that must resolve through the load-time entry registry mapping.
        // 必须通过加载期入口注册表映射解析出的关联 canonical 入口。
        let mut related_entries = Vec::new();
        if is_main {
            for entry in skill.meta.entries() {
                related_entries.push(Self::resolve_help_related_entry_name(
                    skill,
                    entry.name.trim(),
                    &flow_name,
                )?);
            }
        } else {
            for entry in skill.meta.entries_for_help_topic(help_node.name.trim()) {
                related_entries.push(Self::resolve_help_related_entry_name(
                    skill,
                    entry.name.trim(),
                    &flow_name,
                )?);
            }
        }

        Ok(RuntimeHelpNodeDescriptor {
            flow_name,
            description: help_node.description.trim().to_string(),
            related_entries,
            is_main,
        })
    }

    /// Resolve one help-related local entry name into its canonical runtime entry name.
    /// 将单个帮助关联的局部入口名称解析为其 canonical 运行时入口名称。
    ///
    /// The skill parameter is the loaded skill whose registry mapping must contain the local entry.
    /// skill 参数是已加载 skill，其注册表映射必须包含该局部入口。
    ///
    /// The local_name parameter is the manifest-local entry name referenced by one help node.
    /// local_name 参数是某个帮助节点引用的 manifest 局部入口名称。
    ///
    /// The flow_name parameter names the help flow used in diagnostics.
    /// flow_name 参数命名用于诊断的帮助流程。
    ///
    /// Return the canonical runtime entry name, or an explicit consistency error.
    /// 返回 canonical 运行时入口名称；若不一致则返回显式错误。
    fn resolve_help_related_entry_name(
        skill: &LoadedSkill,
        local_name: &str,
        flow_name: &str,
    ) -> Result<String, String> {
        skill
            .resolved_tool_name(local_name)
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "help flow '{}' in skill '{}' references local entry '{}' without a resolved canonical name",
                    flow_name,
                    skill.meta.effective_skill_id(),
                    local_name
                )
            })
    }

    /// Return configured completion candidates for a prompt argument, if declared by a skill.
    /// 返回某个提示词参数在 skill 元数据中声明的候选补全项。
    pub fn prompt_argument_completions(
        &self,
        prompt_name: &str,
        argument_name: &str,
    ) -> Option<Vec<String>> {
        let _ = prompt_name;
        let _ = argument_name;
        None
    }

    /// Return configured completion candidates visible to one host-injected authority.
    /// 返回单个宿主注入权限可见的提示词参数补全候选项。
    pub fn prompt_argument_completions_for_authority(
        &self,
        authority: SkillManagementAuthority,
        prompt_name: &str,
        argument_name: &str,
    ) -> Option<Vec<String>> {
        let _ = authority;
        self.prompt_argument_completions(prompt_name, argument_name)
    }

    /// Return whether one resolved entry target is visible to one host-injected authority.
    /// 返回单个已解析入口目标是否对某个宿主注入权限可见。
    ///
    /// The authority parameter is the host-injected visibility level used for filtering.
    /// authority 参数是用于过滤的宿主注入可见性级别。
    ///
    /// The target parameter is the resolved registry target whose owning skill must still be loaded.
    /// target 参数是已解析的注册表目标，其所属 skill 必须仍处于加载状态。
    ///
    /// Return whether the target is visible, or an explicit error when the registry target is stale.
    /// 返回目标是否可见；当注册表目标已经失效时返回显式错误。
    fn entry_target_visible_to_authority(
        &self,
        authority: SkillManagementAuthority,
        target: &ResolvedEntryTarget,
    ) -> Result<bool, String> {
        // Loaded skill referenced by the entry registry target.
        // 入口注册表目标引用的已加载 skill。
        let skill = self.skills.get(&target.skill_storage_key).ok_or_else(|| {
            format!(
                "entry registry target '{}' references missing loaded skill storage key '{}'",
                target.canonical_name, target.skill_storage_key
            )
        })?;
        if authority == SkillManagementAuthority::System {
            return Ok(true);
        }
        Ok(Self::normalized_skill_root_name(&skill.root_name) != "ROOT")
    }

    /// Check if a tool_name is a Lua skill.
    /// 检查单个工具名是否为 Lua skill 入口。
    pub fn is_skill(&self, name: &str) -> bool {
        self.entry_registry.contains_key(name)
    }

    /// Check if a tool_name is visible as a Lua skill under one host-injected authority.
    /// 检查单个工具名在某个宿主注入权限下是否可见为 Lua skill 入口。
    ///
    /// The authority parameter is the host-injected visibility level used for filtering.
    /// authority 参数是用于过滤的宿主注入可见性级别。
    ///
    /// The name parameter is the canonical tool name being queried.
    /// name 参数是正在查询的 canonical 工具名。
    ///
    /// Return whether the tool is visible, or an explicit error when the entry registry is inconsistent.
    /// 返回工具是否可见；当入口注册表不一致时返回显式错误。
    pub fn is_skill_for_authority(
        &self,
        authority: SkillManagementAuthority,
        name: &str,
    ) -> Result<bool, String> {
        match self.entry_registry.get(name) {
            Some(target) => self.entry_target_visible_to_authority(authority, target),
            None => Ok(false),
        }
    }

    /// Return the owning skill name for an MCP tool name; return `None` when the tool is not provided by a Lua skill.
    /// 根据 MCP 工具名返回所属 skill 名称；未命中时返回 `None`。
    pub fn skill_name_for_tool(&self, tool_name: &str) -> Option<String> {
        self.entry_registry
            .get(tool_name)
            .map(|target| target.skill_id.clone())
    }

    /// Return the visible owning skill name for one tool under one host-injected authority.
    /// 返回某个工具在单个宿主注入权限下可见的所属 skill 名称。
    ///
    /// The authority parameter is the host-injected visibility level used for filtering.
    /// authority 参数是用于过滤的宿主注入可见性级别。
    ///
    /// The tool_name parameter is the canonical tool name being queried.
    /// tool_name 参数是正在查询的 canonical 工具名。
    ///
    /// Return the visible owning skill id, or an explicit error when the entry registry is inconsistent.
    /// 返回可见的所属 skill id；当入口注册表不一致时返回显式错误。
    pub fn skill_name_for_tool_for_authority(
        &self,
        authority: SkillManagementAuthority,
        tool_name: &str,
    ) -> Result<Option<String>, String> {
        match self.entry_registry.get(tool_name) {
            Some(target) => {
                if self.entry_target_visible_to_authority(authority, target)? {
                    Ok(Some(target.skill_id.clone()))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// List flattened skill config records for one optional skill namespace.
    /// 列出某个可选技能命名空间下的扁平化技能配置记录。
    pub fn list_skill_config_entries(
        &self,
        skill_id: Option<&str>,
    ) -> Result<Vec<SkillConfigEntry>, String> {
        self.skill_config_store.list_entries(skill_id)
    }

    /// Read one optional string config value for one `(skill_id, key)` pair.
    /// 读取某个 `(skill_id, key)` 对下的可选字符串配置值。
    pub fn get_skill_config_value(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        self.skill_config_store.get_value(skill_id, key)
    }

    /// Insert or replace one string config value for one `(skill_id, key)` pair.
    /// 为某个 `(skill_id, key)` 对插入或替换一个字符串配置值。
    pub fn set_skill_config_value(
        &mut self,
        skill_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        self.skill_config_store.set_value(skill_id, key, value)
    }

    /// Delete one config key from one skill namespace and report whether one value was removed.
    /// 从某个技能命名空间删除单个配置键，并返回是否移除了一个值。
    pub fn delete_skill_config_value(&mut self, skill_id: &str, key: &str) -> Result<bool, String> {
        self.skill_config_store.delete_value(skill_id, key)
    }

    /// Populate per-request context into the `vulcan` module.
    /// 将单次请求的上下文注入到 `vulcan` 模块中。
    fn populate_vulcan_request_context(
        lua: &Lua,
        invocation_context: Option<&LuaInvocationContext>,
    ) -> Result<(), String> {
        let context_table = get_vulcan_context_table(lua)?;
        let request_context =
            invocation_context.and_then(|context| context.request_context.as_ref());
        let context_value = match request_context {
            Some(context) => serde_json::to_value(context)
                .map_err(|error| format!("Failed to serialize request context: {}", error))?,
            None => Value::Object(serde_json::Map::new()),
        };
        let context_lua = json_value_to_lua(lua, &context_value)
            .map_err(|error| format!("Failed to convert request context to Lua: {}", error))?;
        let client_info_value = match &context_value {
            Value::Object(object) => object.get("client_info").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        let client_capabilities_value = match &context_value {
            Value::Object(object) => object
                .get("client_capabilities")
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
            _ => Value::Object(serde_json::Map::new()),
        };
        let client_info_lua = json_value_to_lua(lua, &client_info_value)
            .map_err(|error| format!("Failed to convert client_info to Lua: {}", error))?;
        let client_capabilities_lua = json_value_to_lua(lua, &client_capabilities_value)
            .map_err(|error| format!("Failed to convert client_capabilities to Lua: {}", error))?;
        let client_budget_value = invocation_context
            .map(|context| context.client_budget.clone())
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let client_budget_lua = json_value_to_lua(lua, &client_budget_value)
            .map_err(|error| format!("Failed to convert client_budget to Lua: {}", error))?;
        let tool_config_value = invocation_context
            .map(|context| context.tool_config.clone())
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let tool_config_lua = json_value_to_lua(lua, &tool_config_value)
            .map_err(|error| format!("Failed to convert tool_config to Lua: {}", error))?;
        let host_result_capability = resolve_host_result_capability(invocation_context)?;
        let host_result_value = host_result_capability_to_json_value(&host_result_capability);
        let host_result_lua = json_value_to_lua(lua, &host_result_value)
            .map_err(|error| format!("Failed to convert host_result helper to Lua: {}", error))?;

        context_table
            .set("request", context_lua)
            .map_err(|error| format!("Failed to set vulcan.context.request: {}", error))?;
        context_table
            .set("client_info", client_info_lua)
            .map_err(|error| format!("Failed to set vulcan.context.client_info: {}", error))?;
        context_table
            .set("client_capabilities", client_capabilities_lua)
            .map_err(|error| {
                format!(
                    "Failed to set vulcan.context.client_capabilities: {}",
                    error
                )
            })?;
        context_table
            .set("client_budget", client_budget_lua)
            .map_err(|error| format!("Failed to set vulcan.context.client_budget: {}", error))?;
        context_table
            .set("tool_config", tool_config_lua)
            .map_err(|error| format!("Failed to set vulcan.context.tool_config: {}", error))?;
        context_table
            .set("host_result", host_result_lua)
            .map_err(|error| format!("Failed to set vulcan.context.host_result: {}", error))?;
        Ok(())
    }

    /// Populate the skill-scoped LanceDB host interface into the `vulcan` module.
    /// 将按 skill 作用域隔离的 LanceDB 宿主接口注入到 `vulcan` 模块中。
    fn populate_vulcan_lancedb_context(
        lua: &Lua,
        binding: Option<Arc<LanceDbSkillBinding>>,
        current_skill_name: Option<&str>,
    ) -> Result<(), String> {
        // Create the provider table and persist the active skill marker in one setup step.
        // 在一个 setup 步骤中创建 provider 表并持久化当前 skill 标记。
        let (vulcan, lancedb_table) = create_provider_context_table(
            lua,
            "lancedb",
            "__lancedb_skill_name",
            current_skill_name,
        )?;

        if let Some(binding) = binding {
            lancedb_table
                .set("enabled", true)
                .map_err(|error| format!("Failed to set vulcan.lancedb.enabled: {}", error))?;
            let info_binding = binding.clone();
            register_provider_json_noarg_method(
                lua,
                &lancedb_table,
                "info",
                "vulcan.lancedb.info",
                "vulcan.lancedb.info",
                move || info_binding.info_json(),
            )?;

            let status_binding = binding.clone();
            register_provider_json_noarg_method(
                lua,
                &lancedb_table,
                "status",
                "vulcan.lancedb.status",
                "vulcan.lancedb.status",
                move || status_binding.status_json(),
            )?;

            let create_binding = binding.clone();
            register_provider_json_method(
                lua,
                &lancedb_table,
                "lancedb",
                "create_table",
                move |input_json| create_binding.create_table_json(input_json),
            )?;

            let upsert_binding = binding.clone();
            let vector_upsert_fn = lua
                .create_function(move |lua, input: LuaValue| {
                    let mut input_json =
                        provider_input_table_to_json(input, "lancedb.vector_upsert")?;
                    let input_object = input_json.as_object_mut().ok_or_else(|| {
                        mlua::Error::runtime("lancedb.vector_upsert input must be an object")
                    })?;

                    let payload_value = if let Some(rows) = input_object.remove("rows") {
                        input_object
                            .entry("input_format".to_string())
                            .or_insert_with(|| Value::String("json".to_string()));
                        rows
                    } else if let Some(data) = input_object.remove("data") {
                        data
                    } else {
                        return Err(mlua::Error::runtime(
                            "lancedb.vector_upsert requires rows or data",
                        ));
                    };

                    let payload_bytes = match payload_value {
                        Value::String(text) => {
                            if !input_object.contains_key("input_format") {
                                input_object.insert(
                                    "input_format".to_string(),
                                    Value::String("arrow_ipc".to_string()),
                                );
                            }
                            text.into_bytes()
                        }
                        Value::Array(_) | Value::Object(_) => {
                            if !input_object.contains_key("input_format") {
                                input_object.insert(
                                    "input_format".to_string(),
                                    Value::String("json".to_string()),
                                );
                            }
                            serde_json::to_vec(&payload_value).map_err(|error| {
                                mlua::Error::runtime(format!(
                                    "failed to encode lancedb upsert payload: {}",
                                    error
                                ))
                            })?
                        }
                        _ => {
                            return Err(mlua::Error::runtime(
                                "lancedb.vector_upsert payload must be string",
                            ));
                        }
                    };

                    let result = upsert_binding
                        .vector_upsert_json(&input_json, &payload_bytes)
                        .map_err(mlua::Error::runtime)?;
                    json_value_to_lua(lua, &result).map_err(mlua::Error::external)
                })
                .map_err(|error| {
                    format!("Failed to create vulcan.lancedb.vector_upsert: {}", error)
                })?;
            lancedb_table
                .set("vector_upsert", vector_upsert_fn)
                .map_err(|error| {
                    format!("Failed to set vulcan.lancedb.vector_upsert: {}", error)
                })?;

            let search_binding = binding.clone();
            let vector_search_fn = lua
                .create_function(move |lua, input: LuaValue| {
                    let mut input_json =
                        provider_input_table_to_json(input, "lancedb.vector_search")?;
                    let input_object = input_json.as_object_mut().ok_or_else(|| {
                        mlua::Error::runtime("lancedb.vector_search input must be an object")
                    })?;
                    input_object
                        .entry("output_format".to_string())
                        .or_insert_with(|| Value::String("json".to_string()));

                    let (meta, raw_bytes) = search_binding
                        .vector_search_json(&input_json)
                        .map_err(mlua::Error::runtime)?;
                    let result_table =
                        json_to_lua_table_inner(lua, &meta).map_err(mlua::Error::external)?;

                    if meta
                        .get("format")
                        .and_then(Value::as_str)
                        .map(|value| value == "json")
                        .unwrap_or(false)
                    {
                        let rows_json: Value =
                            serde_json::from_slice(&raw_bytes).map_err(|error| {
                                mlua::Error::runtime(format!(
                                    "failed to parse LanceDB JSON rows: {}",
                                    error
                                ))
                            })?;
                        result_table
                            .set(
                                "data_json",
                                json_value_to_lua(lua, &rows_json)
                                    .map_err(mlua::Error::external)?,
                            )
                            .map_err(mlua::Error::external)?;
                    } else {
                        result_table
                            .set(
                                "data",
                                LuaValue::String(
                                    lua.create_string(&raw_bytes)
                                        .map_err(mlua::Error::external)?,
                                ),
                            )
                            .map_err(mlua::Error::external)?;
                    }
                    Ok(LuaValue::Table(result_table))
                })
                .map_err(|error| {
                    format!("Failed to create vulcan.lancedb.vector_search: {}", error)
                })?;
            lancedb_table
                .set("vector_search", vector_search_fn)
                .map_err(|error| {
                    format!("Failed to set vulcan.lancedb.vector_search: {}", error)
                })?;

            let delete_binding = binding.clone();
            register_provider_json_method(
                lua,
                &lancedb_table,
                "lancedb",
                "delete",
                move |input_json| delete_binding.delete_json(input_json),
            )?;

            let drop_binding = binding;
            register_provider_json_method(
                lua,
                &lancedb_table,
                "lancedb",
                "drop_table",
                move |input_json| drop_binding.drop_table_json(input_json),
            )?;
        } else {
            install_disabled_provider_context(
                lua,
                &lancedb_table,
                "lancedb",
                disabled_skill_status_json(current_skill_name),
                "current skill has not enabled lancedb",
                &[
                    "create_table",
                    "vector_upsert",
                    "vector_search",
                    "delete",
                    "drop_table",
                ],
            )?;
        }

        // Publish the populated provider table only after all enabled or disabled methods are ready.
        // 仅在全部启用或禁用方法准备完成后发布已填充的 provider 表。
        install_provider_context_table(&vulcan, "lancedb", lancedb_table)?;
        Ok(())
    }

    /// Populate the skill-scoped SQLite host interface into the `vulcan` module.
    /// 将按 skill 作用域隔离的 SQLite 宿主接口注入到 `vulcan` 模块中。
    fn populate_vulcan_sqlite_context(
        lua: &Lua,
        binding: Option<Arc<SqliteSkillBinding>>,
        current_skill_name: Option<&str>,
    ) -> Result<(), String> {
        // Create the provider table and persist the active skill marker in one setup step.
        // 在一个 setup 步骤中创建 provider 表并持久化当前 skill 标记。
        let (vulcan, sqlite_table) = create_provider_context_table(
            lua,
            "sqlite",
            "__sqlite_skill_name",
            current_skill_name,
        )?;

        if let Some(binding) = binding {
            sqlite_table
                .set("enabled", true)
                .map_err(|error| format!("Failed to set vulcan.sqlite.enabled: {}", error))?;

            let info_binding = binding.clone();
            register_provider_json_noarg_method(
                lua,
                &sqlite_table,
                "info",
                "vulcan.sqlite.info",
                "vulcan.sqlite.info",
                move || info_binding.info_json(),
            )?;

            let status_binding = binding.clone();
            register_provider_json_noarg_method(
                lua,
                &sqlite_table,
                "status",
                "vulcan.sqlite.status",
                "vulcan.sqlite.status",
                move || status_binding.status_json(),
            )?;

            let tokenize_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "tokenize_text",
                move |input_json| tokenize_binding.tokenize_text_json(input_json),
            )?;

            let execute_script_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "execute_script",
                move |input_json| execute_script_binding.execute_script(input_json),
            )?;

            let execute_batch_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "execute_batch",
                move |input_json| execute_batch_binding.execute_batch(input_json),
            )?;

            let query_json_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "query_json",
                move |input_json| query_json_binding.query_json(input_json),
            )?;

            let query_stream_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "query_stream",
                move |input_json| query_stream_binding.query_stream(input_json),
            )?;

            let query_stream_wait_metrics_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "query_stream_wait_metrics",
                move |input_json| {
                    query_stream_wait_metrics_binding.query_stream_wait_metrics(input_json)
                },
            )?;

            let query_stream_chunk_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "query_stream_chunk",
                move |input_json| query_stream_chunk_binding.query_stream_chunk(input_json),
            )?;

            let query_stream_close_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "query_stream_close",
                move |input_json| query_stream_close_binding.query_stream_close(input_json),
            )?;

            let upsert_word_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "upsert_custom_word",
                move |input_json| upsert_word_binding.upsert_custom_word_json(input_json),
            )?;

            let remove_word_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "remove_custom_word",
                move |input_json| remove_word_binding.remove_custom_word_json(input_json),
            )?;

            let list_words_binding = binding.clone();
            let list_words_fn = lua
                .create_function(move |lua, ()| {
                    provider_json_result_to_lua(lua, list_words_binding.list_custom_words_json())
                })
                .map_err(|error| {
                    format!(
                        "Failed to create vulcan.sqlite.list_custom_words: {}",
                        error
                    )
                })?;
            sqlite_table
                .set("list_custom_words", list_words_fn)
                .map_err(|error| {
                    format!("Failed to set vulcan.sqlite.list_custom_words: {}", error)
                })?;

            let ensure_index_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "ensure_fts_index",
                move |input_json| ensure_index_binding.ensure_fts_index_json(input_json),
            )?;

            let rebuild_index_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "rebuild_fts_index",
                move |input_json| rebuild_index_binding.rebuild_fts_index_json(input_json),
            )?;

            let upsert_doc_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "upsert_fts_document",
                move |input_json| upsert_doc_binding.upsert_fts_document_json(input_json),
            )?;

            let delete_doc_binding = binding.clone();
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "delete_fts_document",
                move |input_json| delete_doc_binding.delete_fts_document_json(input_json),
            )?;

            let search_binding = binding;
            register_provider_json_method(
                lua,
                &sqlite_table,
                "sqlite",
                "search_fts",
                move |input_json| search_binding.search_fts_json(input_json),
            )?;
        } else {
            install_disabled_provider_context(
                lua,
                &sqlite_table,
                "sqlite",
                disabled_sqlite_skill_status_json(current_skill_name),
                "current skill has not enabled sqlite",
                &[
                    "execute_script",
                    "execute_batch",
                    "query_json",
                    "query_stream",
                    "query_stream_wait_metrics",
                    "query_stream_chunk",
                    "query_stream_close",
                    "tokenize_text",
                    "upsert_custom_word",
                    "remove_custom_word",
                    "list_custom_words",
                    "ensure_fts_index",
                    "rebuild_fts_index",
                    "upsert_fts_document",
                    "delete_fts_document",
                    "search_fts",
                ],
            )?;
        }

        // Publish the populated provider table only after all enabled or disabled methods are ready.
        // 仅在全部启用或禁用方法准备完成后发布已填充的 provider 表。
        install_provider_context_table(&vulcan, "sqlite", sqlite_table)?;
        Ok(())
    }

    /// Populate all `vulcan` subcontexts required before executing one loaded skill entry.
    /// 执行一个已加载 skill 入口前填充所需的全部 `vulcan` 子上下文。
    ///
    /// The lua parameter owns the Lua VM that will execute the loaded skill entry.
    /// lua 参数持有即将执行已加载 skill 入口的 Lua 虚拟机。
    ///
    /// The skill parameter is the loaded skill whose directory, root, metadata, and provider bindings are active.
    /// skill 参数是当前生效的已加载 skill，提供目录、root、元数据与 provider binding。
    ///
    /// The context parameter contains entry-specific metadata and the optional invocation context.
    /// context 参数包含入口专属元数据与可选调用上下文。
    ///
    /// Return `Ok(())` after request, internal, file, dependency handling, and provider contexts are ready.
    /// request、internal、file、dependency 处理与 provider 上下文全部就绪后返回 `Ok(())`。
    fn populate_loaded_skill_lua_context(
        &self,
        lua: &Lua,
        skill: &LoadedSkill,
        context: LoadedSkillLuaContext<'_>,
    ) -> Result<(), String> {
        // Reuse the effective skill id for every subcontext that must agree on skill identity.
        // 对所有必须共享 skill 身份的子上下文复用 effective skill id。
        let skill_name = skill.meta.effective_skill_id();
        // Trusted package installed before the Skill can call managed Python or Node APIs.
        // 在 Skill 调用受管 Python 或 Node API 前安装的可信包。
        replace_lua_managed_package_context(lua, Some(skill.managed_package.clone()));
        Self::populate_vulcan_request_context(lua, context.invocation_context)?;
        populate_vulcan_internal_execution_context(
            lua,
            &VulcanInternalExecutionContext {
                tool_name: Some(context.display_tool_name.to_string()),
                skill_name: Some(skill_name.to_string()),
                entry_name: Some(context.entry_name.to_string()),
                root_name: Some(skill.root_name.clone()),
                luaexec_active: false,
                luaexec_caller_tool_name: None,
            },
        )?;
        populate_vulcan_file_context(lua, Some(&skill.dir), Some(context.entry_path))?;
        populate_vulcan_dependency_context(
            lua,
            self.host_options.as_ref(),
            Some(&skill.dir),
            Some(skill_name),
        )?;
        Self::populate_vulcan_lancedb_context(
            lua,
            skill.lancedb_binding.clone(),
            Some(skill_name),
        )?;
        Self::populate_vulcan_sqlite_context(lua, skill.sqlite_binding.clone(), Some(skill_name))?;
        Ok(())
    }

    /// Populate all `vulcan` subcontexts required before anonymous Lua execution.
    /// 匿名 Lua 执行前填充所需的全部 `vulcan` 子上下文。
    ///
    /// The lua parameter owns the Lua VM that will execute the anonymous code.
    /// lua 参数持有即将执行匿名代码的 Lua 虚拟机。
    ///
    /// The context parameter contains request, internal, file, and dependency behavior metadata.
    /// context 参数包含 request、internal、file 与 dependency 行为元数据。
    ///
    /// Return `Ok(())` after request, internal, file, dependency, and provider contexts are populated.
    /// request、internal、file、dependency 与 provider 上下文全部填充完成后返回 `Ok(())`。
    fn populate_anonymous_lua_context(
        lua: &Lua,
        context: AnonymousLuaExecutionContext<'_>,
    ) -> Result<(), String> {
        // Anonymous execution has no active skill, so provider contexts are installed disabled.
        // 匿名执行没有当前 skill，因此 provider 上下文以禁用状态安装。
        Self::populate_vulcan_request_context(lua, context.invocation_context)?;
        populate_vulcan_internal_execution_context(lua, &context.internal_context)?;
        populate_vulcan_file_context(lua, None, context.entry_file)?;
        match context.dependency_context {
            AnonymousLuaDependencyContext::ClearWithHostOptions(host_options) => {
                populate_vulcan_dependency_context(lua, host_options, None, None)?;
            }
            AnonymousLuaDependencyContext::PreserveCurrent => {}
        }
        match context.managed_package_context {
            AnonymousLuaManagedPackageContext::Clear => {
                replace_lua_managed_package_context(lua, None);
            }
            AnonymousLuaManagedPackageContext::Set(package) => {
                replace_lua_managed_package_context(lua, Some(package.clone()));
            }
            AnonymousLuaManagedPackageContext::PreserveCurrent => {}
        }
        Self::populate_vulcan_lancedb_context(lua, None, None)?;
        Self::populate_vulcan_sqlite_context(lua, None, None)?;
        Ok(())
    }

    /// Resolve the loaded skill metadata required to execute one public `call_skill` request.
    /// 解析执行一次公开 `call_skill` 请求所需的已加载 skill 元数据。
    ///
    /// The tool_name parameter is the host-visible tool name requested by the caller.
    /// tool_name 参数是调用方请求的宿主可见工具名。
    ///
    /// Return the loaded skill, local tool metadata, canonical display name, and local entry name.
    /// 返回已加载 skill、局部工具元数据、canonical 展示名与局部入口名。
    fn resolve_call_skill_invocation_target<'a>(
        &'a self,
        tool_name: &str,
    ) -> Result<CallSkillInvocationTarget<'a>, String> {
        let resolved_target = self
            .entry_registry
            .get(tool_name)
            .ok_or_else(|| format!("Lua skill '{}' not found", tool_name))?;
        let skill = self
            .skills
            .get(&resolved_target.skill_storage_key)
            .ok_or_else(|| format!("Lua skill '{}' not found", tool_name))?;
        let tool = skill
            .meta
            .find_tool_by_local_name(&resolved_target.local_name)
            .ok_or_else(|| format!("Lua skill '{}' not found", tool_name))?;

        Ok(CallSkillInvocationTarget {
            skill,
            tool,
            display_tool_name: &resolved_target.canonical_name,
            local_entry_name: &resolved_target.local_name,
        })
    }

    /// Prepare the Lua VM context needed before invoking one public `call_skill` target.
    /// 准备调用一个公开 `call_skill` 目标前所需的 Lua VM 上下文。
    ///
    /// The lua parameter is the scoped Lua VM that will run the target handler.
    /// lua 参数是即将运行目标 handler 的作用域内 Lua VM。
    ///
    /// The invocation_target parameter contains the already resolved loaded skill and tool metadata.
    /// invocation_target 参数包含已经解析完成的已加载 skill 与工具元数据。
    ///
    /// The invocation_context parameter is the optional request context exposed through `vulcan.context`.
    /// invocation_context 参数是通过 `vulcan.context` 暴露的可选请求上下文。
    ///
    /// Return `Ok(())` after optional debug reload and all loaded-skill subcontexts are installed.
    /// 可选 debug reload 与全部已加载 skill 子上下文安装完成后返回 `Ok(())`。
    fn prepare_call_skill_lua_context(
        &self,
        lua: &Lua,
        invocation_target: &CallSkillInvocationTarget<'_>,
        invocation_context: Option<&LuaInvocationContext>,
    ) -> Result<(), String> {
        if invocation_target.skill.meta.debug {
            Self::compile_skill_into_lua(
                lua,
                invocation_target.skill,
                invocation_target.tool,
                true,
            )?;
        }

        let entry_path = tool_entry_path(&invocation_target.skill.dir, invocation_target.tool);
        self.populate_loaded_skill_lua_context(
            lua,
            invocation_target.skill,
            LoadedSkillLuaContext {
                display_tool_name: invocation_target.display_tool_name,
                entry_name: invocation_target.local_entry_name,
                entry_path: &entry_path,
                invocation_context,
            },
        )
    }

    /// Prepare the loaded-skill Lua context used while rendering one Lua help payload.
    /// 准备渲染单个 Lua 帮助载荷时使用的已加载 skill Lua 上下文。
    ///
    /// The lua parameter is the scoped Lua VM that will execute the help payload.
    /// lua 参数是即将执行帮助载荷的作用域内 Lua VM。
    ///
    /// The skill parameter is the loaded skill that owns the help file.
    /// skill 参数是拥有帮助文件的已加载 skill。
    ///
    /// The relative_path parameter is the manifest-declared help file path.
    /// relative_path 参数是 manifest 中声明的帮助文件路径。
    ///
    /// The helper_path parameter is the resolved help file path used for file context.
    /// helper_path 参数是用于文件上下文的已解析帮助文件路径。
    ///
    /// The request_context parameter is the optional host request context to expose.
    /// request_context 参数是要暴露的可选宿主请求上下文。
    ///
    /// Return `Ok(())` after the `vulcan-help` context is installed.
    /// 安装 `vulcan-help` 上下文后返回 `Ok(())`。
    fn prepare_lua_help_context(
        &self,
        lua: &Lua,
        skill: &LoadedSkill,
        relative_path: &str,
        helper_path: &Path,
        request_context: Option<&RuntimeRequestContext>,
    ) -> Result<(), String> {
        let help_invocation_context = LuaInvocationContext::new(
            request_context.cloned(),
            Value::Object(serde_json::Map::new()),
            Value::Object(serde_json::Map::new()),
        );
        self.populate_loaded_skill_lua_context(
            lua,
            skill,
            LoadedSkillLuaContext {
                display_tool_name: "vulcan-help",
                entry_name: relative_path,
                entry_path: helper_path,
                invocation_context: Some(&help_invocation_context),
            },
        )
    }

    /// Call one active loaded Lua skill with the given JSON arguments.
    /// 使用给定 JSON 参数调用单个已激活的已加载 Lua skill。
    /// Calls are runtime execution, not skill-management authority checks.
    /// 调用属于运行时执行，不属于技能管理权限校验。
    pub fn call_skill(
        &self,
        tool_name: &str,
        args: &Value,
        invocation_context: Option<&LuaInvocationContext>,
    ) -> Result<RuntimeInvocationResult, String> {
        let invocation_target = self.resolve_call_skill_invocation_target(tool_name)?;
        let module_name = invocation_target.tool.lua_module.clone();

        let mut lease = self.acquire_vm()?;
        let scope_guard = LuaVmRequestScopeGuard::new(&mut lease, self.host_options.as_ref())?;
        let lua = scope_guard.lua()?;

        self.prepare_call_skill_lua_context(lua, &invocation_target, invocation_context)?;

        let invocation_input = prepare_call_skill_lua_invocation_input(lua, &module_name, args)?;
        let call_result = invoke_loaded_lua_skill_handler(
            invocation_input.handler,
            invocation_input.args_table,
            invocation_target.display_tool_name,
            invocation_context,
        );
        finish_pooled_vm_request_scope(call_result, scope_guard, "pooled Lua VM cleanup failed")
    }

    /// Render one help payload from either Markdown or Lua.
    /// 从 Markdown 或 Lua 渲染单个帮助载荷。
    fn render_help_payload(
        &self,
        skill: &LoadedSkill,
        relative_path: &str,
        request_context: Option<&RuntimeRequestContext>,
    ) -> Result<String, String> {
        if !is_lua_help_file(relative_path) {
            return read_skill_text_file(&skill.dir, relative_path, "help");
        }

        let help_payload_source = read_lua_help_payload_source(&skill.dir, relative_path)?;
        let mut lease = self.acquire_vm()?;
        let scope_guard = LuaVmRequestScopeGuard::new(&mut lease, self.host_options.as_ref())?;
        let lua = scope_guard.lua()?;
        self.prepare_lua_help_context(
            lua,
            skill,
            relative_path,
            &help_payload_source.helper_path,
            request_context,
        )?;

        let chunk_name = format!("{}-{}", skill.meta.effective_skill_id(), relative_path);
        let rendered_result = render_lua_help_payload_text(
            lua,
            &help_payload_source.helper_path,
            &help_payload_source.source,
            &chunk_name,
        );
        finish_pooled_vm_request_scope(rendered_result, scope_guard, "pooled Lua VM cleanup failed")
    }

    /// Populate the vulcan.call function to dispatch to loaded skills.
    fn populate_vulcan_call_for_lua(
        lua: &Lua,
        skills_map: &HashMap<String, LoadedSkill>,
        entry_registry: &BTreeMap<String, ResolvedEntryTarget>,
        host_options: Arc<LuaRuntimeHostOptions>,
        lancedb_host: Option<Arc<LanceDbSkillHost>>,
        sqlite_host: Option<Arc<SqliteSkillHost>>,
    ) -> Result<(), String> {
        let vulcan: Table = lua
            .globals()
            .get("vulcan")
            .map_err(|e| format!("vulcan module not found: {}", e))?;

        let dispatch_entries = build_lua_call_dispatch_entries(skills_map, entry_registry)?;

        let dispatcher = lua
            .create_function(move |lua, (name, args): (LuaValue, LuaValue)| {
                let name = require_string_arg(name, "call", "name", false)?;
                let args = require_table_arg(args, "call", "args")?;
                let dispatch_entry = resolve_lua_call_dispatch_entry(&dispatch_entries, &name)?;
                let owner_skill_name = &dispatch_entry.owner_skill_id;
                let func = resolve_lua_call_dispatch_handler(lua, dispatch_entry)?;
                // Prepare the nested state and inherited invocation context as one verified unit.
                // 将嵌套状态与继承调用上下文作为一个已验证单元准备。
                let PreparedLuaNestedCallScope {
                    guard: nested_scope_guard,
                    invocation_context: nested_invocation_context,
                } = prepare_lua_nested_call_scope(
                    lua,
                    host_options.clone(),
                    lancedb_host.clone(),
                    sqlite_host.clone(),
                )?;
                dispatch_entry.reject_forbidden_luaexec_call(
                    nested_scope_guard.previous_internal_context(),
                )?;
                let provider_bindings = resolve_lua_call_provider_bindings(
                    owner_skill_name,
                    lancedb_host.as_ref(),
                    sqlite_host.as_ref(),
                )
                .map_err(mlua::Error::runtime)?;
                nested_scope_guard
                    .enter_nested_call(build_lua_nested_call_target(
                        dispatch_entry,
                        &nested_invocation_context,
                        provider_bindings,
                    ))
                    .map_err(mlua::Error::runtime)?;
                let call_result = func.call::<MultiValue>(args);
                nested_scope_guard.finish_nested_call(call_result)
            })
            .map_err(|e| format!("Failed to create vulcan.call dispatcher: {}", e))?;

        vulcan
            .set("call", dispatcher)
            .map_err(|e| format!("Failed to set vulcan.call: {}", e))?;

        Ok(())
    }

    /// Configure package.path and package.cpath to include project-local luarocks tree.
    /// 配置 package.path 与 package.cpath，使其只依赖项目内统一的 lua 目录布局。
    ///
    /// This keeps runtime resolution aligned with the deployed layout under
    /// `lua_packages/share/lua/` and `lua_packages/lib/lua/`, instead of relying on
    /// versioned `5.1` subdirectories that may not exist in the shipped bundle.
    /// This keeps runtime resolution aligned with the deployed layout under
    /// 这会让运行时与已部署的目录布局保持一致，
    /// `lua_packages/share/lua/` and `lua_packages/lib/lua/`, instead of relying on
    /// 即仅依赖 `lua_packages/share/lua/` 与 `lua_packages/lib/lua/`，
    /// versioned `5.1` subdirectories that may not exist in the shipped bundle.
    /// 而不再依赖发布包中可能并不存在的带版本 `5.1` 子目录。
    fn setup_package_paths(
        lua: &Lua,
        host_options: &LuaRuntimeHostOptions,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(lua_packages) = host_options.lua_packages_dir.as_ref() else {
            return Ok(());
        };
        if !configured_package_search_directory_exists(lua_packages, "lua_packages_dir")? {
            return Ok(());
        }

        let host_provided_ffi_root = match host_options.host_provided_ffi_root.as_ref() {
            Some(root) => {
                if configured_package_search_directory_exists(root, "host_provided_ffi_root")? {
                    Some(root.as_path())
                } else {
                    None
                }
            }
            None => None,
        };

        // Build package.cpath entries for C modules (.dll on Windows)
        // 统一使用宿主提供的 lua_packages/lib/lua 目录，并补充宿主提供的 FFI/原生库根目录。
        #[cfg(windows)]
        let cpath_pattern = {
            let mut pattern = format!(
                "{}\\lib\\lua\\?.dll;{}\\lib\\lua\\?\\init.dll;{}\\lib\\lua\\loadall.dll;{}\\?\\?.dll;",
                lua_packages.display(),
                lua_packages.display(),
                lua_packages.display(),
                lua_packages.display()
            );
            if let Some(root) = host_provided_ffi_root {
                pattern.push_str(&format!(
                    "{}\\?.dll;{}\\?\\init.dll;",
                    root.display(),
                    root.display()
                ));
            }
            pattern
        };

        // Build package.cpath entries for C modules (.so on Linux)
        // Linux 下同样严格依赖宿主传入的 lua_packages 根目录，并补充宿主提供的 FFI/原生库根目录。
        #[cfg(target_os = "linux")]
        let cpath_pattern = {
            let mut pattern = format!(
                "{}/lib/lua/?.so;{}/lib/lua/?/init.so;{}/lib/lua/loadall.so;{}/?.so;",
                lua_packages.display(),
                lua_packages.display(),
                lua_packages.display(),
                lua_packages.display()
            );
            if let Some(root) = host_provided_ffi_root {
                pattern.push_str(&format!(
                    "{}/?.so;{}/?/init.so;",
                    root.display(),
                    root.display()
                ));
            }
            pattern
        };

        // Build package.cpath entries for C modules (.dylib on macOS)
        // macOS 下同样严格依赖宿主传入的 lua_packages 根目录，并补充宿主提供的 FFI/原生库根目录。
        #[cfg(target_os = "macos")]
        let cpath_pattern = {
            let mut pattern = format!(
                "{}/lib/lua/?.dylib;{}/lib/lua/?/init.dylib;{}/lib/lua/loadall.dylib;{}/?.dylib;",
                lua_packages.display(),
                lua_packages.display(),
                lua_packages.display(),
                lua_packages.display()
            );
            if let Some(root) = host_provided_ffi_root {
                pattern.push_str(&format!(
                    "{}/?.dylib;{}/?/init.dylib;",
                    root.display(),
                    root.display()
                ));
            }
            pattern
        };

        // Build package.path entries for Lua modules
        // 统一使用宿主提供的 lua_packages/share/lua 目录。
        #[cfg(windows)]
        let path_pattern = format!(
            "{}\\share\\lua\\?.lua;{}\\share\\lua\\?\\init.lua;{}\\?.lua;",
            lua_packages.display(),
            lua_packages.display(),
            lua_packages.display()
        );

        // Build package.path entries for Lua modules on Unix-like systems
        // 类 Unix 平台同样严格依赖宿主传入的 lua_packages 根目录。
        #[cfg(unix)]
        let path_pattern = format!(
            "{}/share/lua/?.lua;{}/share/lua/?/init.lua;{}/?.lua;",
            lua_packages.display(),
            lua_packages.display(),
            lua_packages.display()
        );

        // Prepend to existing paths
        // 将宿主指定路径前置到现有 package 搜索链，避免覆盖 Lua 默认行为。
        let package: Table = lua.globals().get("package")?;
        let old_cpath: mlua::String = package.get("cpath")?;
        let new_cpath = format!("{}{}", cpath_pattern, old_cpath.to_str()?);
        package.set("cpath", lua.create_string(&new_cpath)?)?;

        let old_path: mlua::String = package.get("path")?;
        let new_path = format!("{}{}", path_pattern, old_path.to_str()?);
        package.set("path", lua.create_string(&new_path)?)?;
        Ok(())
    }

    /// Register the strict `vulcan` module in the Lua VM.
    /// 在 Lua 虚拟机中注册严格版 `vulcan` 模块。
    fn register_vulcan_module(
        lua: &Lua,
        host_options: &LuaRuntimeHostOptions,
        skill_config_store: Arc<SkillConfigStore>,
        runtime_skill_roots: &[RuntimeSkillRoot],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let vulcan = lua.create_table()?;
        let runtime = lua.create_table()?;
        let runtime_skills = lua.create_table()?;
        let runtime_internal = lua.create_table()?;
        let runtime_lua = lua.create_table()?;
        let runtime_python = lua.create_table()?;
        let runtime_node = lua.create_table()?;
        let fs = lua.create_table()?;
        let path = lua.create_table()?;
        let process = lua.create_table()?;
        let os = lua.create_table()?;
        let json = lua.create_table()?;
        let cache = lua.create_table()?;
        let config = lua.create_table()?;
        let host = lua.create_table()?;
        let models = lua.create_table()?;
        let context = lua.create_table()?;
        let deps = lua.create_table()?;
        let default_text_encoding = resolve_host_default_text_encoding(host_options)?;
        let vulcan_io = create_vulcan_io_table(lua, default_text_encoding)?;

        let runtime_log_fn = lua.create_function(|_, (level, msg): (LuaValue, LuaValue)| {
            let level = require_string_arg(level, "runtime.log", "level", false)?;
            let msg = require_string_arg(msg, "runtime.log", "message", true)?;
            let normalized_level = level.trim().to_ascii_lowercase();
            let rendered = format!("[LuaSkill:{}] {}", level, msg);
            if normalized_level.contains("error") || normalized_level.contains("fatal") {
                log_error(rendered);
            } else if normalized_level.contains("warn") {
                log_warn(rendered);
            } else {
                log_info(rendered);
            }
            Ok(())
        })?;
        runtime.set("log", runtime_log_fn)?;

        let print_fn = lua.create_function(|_, args: MultiValue| {
            let mut parts = Vec::new();
            for val in args.into_iter() {
                parts.push(render_lua_print_argument(val));
            }
            log_info(format!("[LuaSkill:info] {}", parts.join("\t")));
            Ok(())
        })?;
        lua.globals().set("print", print_fn)?;

        let fs_list_fn = lua.create_function(|_, dir: LuaValue| {
            let dir = require_path_arg(dir, "fs.list", "dir")?;
            let mut entries = Vec::new();
            let dir_path = Path::new(&dir);
            for entry in std::fs::read_dir(&dir)
                .map_err(|e| mlua::Error::runtime(format!("fs.list: {}", e)))?
            {
                let entry = entry.map_err(|e| mlua::Error::runtime(format!("fs.list: {}", e)))?;
                let file_name = entry.file_name().into_string().map_err(|name| {
                    mlua::Error::runtime(format_vulcan_fs_list_non_utf8_file_name_error(
                        dir_path,
                        name.as_os_str(),
                    ))
                })?;
                entries.push(file_name);
            }
            Ok(entries)
        })?;
        fs.set("list", fs_list_fn)?;

        let fs_read_fn = lua.create_function(|_, path: LuaValue| {
            let path = require_path_arg(path, "fs.read", "path")?;
            std::fs::read_to_string(&path)
                .map_err(|e| mlua::Error::runtime(format!("fs.read: {}", e)))
        })?;
        fs.set("read", fs_read_fn)?;

        let fs_write_fn = lua.create_function(|_, (path, content): (LuaValue, LuaValue)| {
            let path = require_path_arg(path, "fs.write", "path")?;
            let content = require_string_arg(content, "fs.write", "content", true)?;
            std::fs::write(&path, content)
                .map_err(|e| mlua::Error::runtime(format!("fs.write: {}", e)))
        })?;
        fs.set("write", fs_write_fn)?;

        let fs_write_bytes_fn =
            lua.create_function(|_, (path, content): (LuaValue, LuaValue)| {
                let path = require_path_arg(path, "fs.write_bytes", "path")?;
                let content = require_string_arg(content, "fs.write_bytes", "content", false)?;
                let bytes = BASE64_STANDARD
                    .decode(content.as_bytes())
                    .map_err(|error| {
                        mlua::Error::runtime(format!(
                            "fs.write_bytes: base64 decode failed: {error}"
                        ))
                    })?;
                fs::write(&path, bytes)
                    .map_err(|error| mlua::Error::runtime(format!("fs.write_bytes: {}", error)))?;
                Ok(true)
            })?;
        fs.set("write_bytes", fs_write_bytes_fn)?;

        let fs_rename_fn =
            lua.create_function(|_, (old_path, new_path): (LuaValue, LuaValue)| {
                let old_path = require_path_arg(old_path, "fs.rename", "old_path")?;
                let new_path = require_path_arg(new_path, "fs.rename", "new_path")?;
                fs::rename(&old_path, &new_path)
                    .map_err(|error| mlua::Error::runtime(format!("fs.rename: {}", error)))?;
                Ok(true)
            })?;
        fs.set("rename", fs_rename_fn)?;

        let fs_remove_fn = lua.create_function(|_, args: MultiValue| {
            let mut values = args.into_iter();
            let path =
                require_path_arg(values.next().unwrap_or(LuaValue::Nil), "fs.remove", "path")?;
            let recursive = parse_vulcan_fs_recursive_option(
                values.next().unwrap_or(LuaValue::Nil),
                "fs.remove",
            )?;
            let target_path = Path::new(&path);
            let metadata = match fs::symlink_metadata(target_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(mlua::Error::runtime(format!("fs.remove: {}", error)));
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                if recursive {
                    fs::remove_dir_all(target_path)
                        .map_err(|error| mlua::Error::runtime(format!("fs.remove: {}", error)))?;
                } else {
                    fs::remove_dir(target_path)
                        .map_err(|error| mlua::Error::runtime(format!("fs.remove: {}", error)))?;
                }
            } else {
                fs::remove_file(target_path)
                    .map_err(|error| mlua::Error::runtime(format!("fs.remove: {}", error)))?;
            }
            Ok(true)
        })?;
        fs.set("remove", fs_remove_fn)?;

        let fs_mkdir_fn = lua.create_function(|_, args: MultiValue| {
            let mut values = args.into_iter();
            let path =
                require_path_arg(values.next().unwrap_or(LuaValue::Nil), "fs.mkdir", "path")?;
            let recursive = parse_vulcan_fs_recursive_option(
                values.next().unwrap_or(LuaValue::Nil),
                "fs.mkdir",
            )?;
            let target_path = Path::new(&path);
            match vulcan_fs_mkdir_target_status(target_path).map_err(mlua::Error::runtime)? {
                VulcanFsMkdirTargetStatus::Missing => {}
                VulcanFsMkdirTargetStatus::ExistingDirectory => return Ok(false),
                VulcanFsMkdirTargetStatus::ExistingNonDirectory => {
                    return Err(mlua::Error::runtime(format!(
                        "fs.mkdir: target already exists and is not a directory: {}",
                        render_log_friendly_path(target_path)
                    )));
                }
            }
            if recursive {
                fs::create_dir_all(target_path)
                    .map_err(|error| mlua::Error::runtime(format!("fs.mkdir: {}", error)))?;
            } else {
                fs::create_dir(target_path)
                    .map_err(|error| mlua::Error::runtime(format!("fs.mkdir: {}", error)))?;
            }
            Ok(true)
        })?;
        fs.set("mkdir", fs_mkdir_fn)?;

        let fs_copy_fn = lua.create_function(|_, args: MultiValue| {
            let mut values = args.into_iter();
            let source_path = require_path_arg(
                values.next().unwrap_or(LuaValue::Nil),
                "fs.copy",
                "src_path",
            )?;
            let target_path = require_path_arg(
                values.next().unwrap_or(LuaValue::Nil),
                "fs.copy",
                "dst_path",
            )?;
            let overwrite = parse_vulcan_fs_overwrite_option(
                values.next().unwrap_or(LuaValue::Nil),
                "fs.copy",
            )?;
            let source = Path::new(&source_path);
            let target = Path::new(&target_path);
            let source_metadata = fs::metadata(source)
                .map_err(|error| mlua::Error::runtime(format!("fs.copy: {}", error)))?;
            let target_exists =
                path_entry_exists(target, "fs.copy").map_err(mlua::Error::runtime)?;
            if target_exists && !overwrite {
                return Ok(false);
            }
            if source_metadata.is_dir() {
                validate_vulcan_fs_copy_directory_target(
                    source,
                    target,
                    overwrite && target_exists,
                )
                .map_err(mlua::Error::runtime)?;
            }
            if target_exists {
                remove_vulcan_fs_copy_target(target).map_err(mlua::Error::runtime)?;
            }
            if source_metadata.is_dir() {
                copy_vulcan_fs_directory_recursive(source, target).map_err(mlua::Error::runtime)?;
            } else {
                fs::copy(source, target)
                    .map_err(|error| mlua::Error::runtime(format!("fs.copy: {}", error)))?;
            }
            Ok(true)
        })?;
        fs.set("copy", fs_copy_fn)?;

        let fs_stat_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "fs.stat", "path")?;
            match fs::symlink_metadata(&path) {
                Ok(metadata) => Ok(LuaValue::Table(create_vulcan_fs_stat_table(
                    lua,
                    &metadata,
                    Path::new(&path),
                )?)),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(LuaValue::Nil),
                Err(error) => Err(mlua::Error::runtime(format!("fs.stat: {}", error))),
            }
        })?;
        fs.set("stat", fs_stat_fn)?;

        let fs_read_bytes_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "fs.read_bytes", "path")?;
            let bytes = fs::read(&path)
                .map_err(|error| mlua::Error::runtime(format!("fs.read_bytes: {}", error)))?;
            lua.create_string(BASE64_STANDARD.encode(bytes))
        })?;
        fs.set("read_bytes", fs_read_bytes_fn)?;

        let fs_exists_fn = lua.create_function(|_, path: LuaValue| {
            let path = require_path_arg(path, "fs.exists", "path")?;
            vulcan_fs_target_exists(Path::new(&path), "fs.exists").map_err(mlua::Error::runtime)
        })?;
        fs.set("exists", fs_exists_fn)?;

        let fs_is_dir_fn = lua.create_function(|_, path: LuaValue| {
            let path = require_path_arg(path, "fs.is_dir", "path")?;
            vulcan_fs_target_is_dir(Path::new(&path), "fs.is_dir").map_err(mlua::Error::runtime)
        })?;
        fs.set("is_dir", fs_is_dir_fn)?;

        let path_join_fn = lua.create_function(|lua, parts: MultiValue| {
            if parts.is_empty() {
                return Err(mlua::Error::runtime(
                    "path.join: expected at least one path segment",
                ));
            }
            let mut joined = PathBuf::new();
            for (index, val) in parts.into_iter().enumerate() {
                let param_name = format!("part[{}]", index + 1);
                let part = require_path_arg(val, "path.join", &param_name)?;
                joined.push(part);
            }
            let result = render_host_visible_path(&joined);
            lua.create_string(&result)
        })?;
        path.set("join", path_join_fn)?;

        let path_dirname_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "path.dirname", "path")?;
            let rendered = render_vulcan_path_dirname(Path::new(&path));
            lua.create_string(&rendered)
        })?;
        path.set("dirname", path_dirname_fn)?;

        let path_basename_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "path.basename", "path")?;
            let rendered =
                render_vulcan_path_component(Path::new(&path).file_name(), "path.basename")?;
            lua.create_string(&rendered)
        })?;
        path.set("basename", path_basename_fn)?;

        let path_stem_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "path.stem", "path")?;
            let rendered = render_vulcan_path_component(Path::new(&path).file_stem(), "path.stem")?;
            lua.create_string(&rendered)
        })?;
        path.set("stem", path_stem_fn)?;

        let path_extname_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "path.extname", "path")?;
            let extension =
                render_vulcan_path_component(Path::new(&path).extension(), "path.extname")?;
            let rendered = if extension.is_empty() {
                String::new()
            } else {
                format!(".{}", extension)
            };
            lua.create_string(&rendered)
        })?;
        path.set("extname", path_extname_fn)?;

        let path_normalize_fn = lua.create_function(|lua, path: LuaValue| {
            let path = require_path_arg(path, "path.normalize", "path")?;
            let rendered = render_vulcan_normalized_path(Path::new(&path));
            lua.create_string(&rendered)
        })?;
        path.set("normalize", path_normalize_fn)?;

        let path_is_abs_fn = lua.create_function(|_, path: LuaValue| {
            let path = require_path_arg(path, "path.is_abs", "path")?;
            Ok(Path::new(&path).is_absolute())
        })?;
        path.set("is_abs", path_is_abs_fn)?;

        let cwd_fn = lua.create_function(|lua, ()| {
            let current_dir = std::env::current_dir()
                .map_err(|error| mlua::Error::runtime(format!("runtime.cwd: {}", error)))?;
            let current_dir_text = render_host_visible_path(&current_dir);
            lua.create_string(&current_dir_text)
        })?;
        runtime.set("cwd", cwd_fn)?;

        let python_status_fn = lua.create_function(|lua, ()| managed_python_status(lua))?;
        runtime_python.set("status", python_status_fn)?;
        let python_invoke_fn =
            lua.create_function(|lua, spec: LuaValue| invoke_managed_python(lua, spec))?;
        runtime_python.set("invoke", python_invoke_fn)?;
        // Persistent Python session namespace backed by the shared process core.
        // 由共享进程核心支撑的持久 Python 会话命名空间。
        let runtime_python_session = lua.create_table()?;
        let python_session_open_fn = lua.create_function(move |lua, spec: LuaValue| {
            open_managed_python_session(lua, spec, default_text_encoding)
        })?;
        runtime_python_session.set("open", python_session_open_fn)?;
        runtime_python.set("session", runtime_python_session)?;

        let node_status_fn = lua.create_function(|lua, ()| managed_node_status(lua))?;
        runtime_node.set("status", node_status_fn)?;
        let node_invoke_fn =
            lua.create_function(|lua, spec: LuaValue| invoke_managed_node(lua, spec))?;
        runtime_node.set("invoke", node_invoke_fn)?;
        // Persistent Node session namespace backed by the shared process core.
        // 由共享进程核心支撑的持久 Node 会话命名空间。
        let runtime_node_session = lua.create_table()?;
        let node_session_open_fn = lua.create_function(move |lua, spec: LuaValue| {
            open_managed_node_session(lua, spec, default_text_encoding)
        })?;
        runtime_node_session.set("open", node_session_open_fn)?;
        runtime_node.set("session", runtime_node_session)?;

        match host_options.temp_dir.as_ref() {
            Some(path_buf) => runtime.set("temp_dir", render_host_visible_path(path_buf))?,
            None => runtime.set("temp_dir", LuaValue::Nil)?,
        }

        match host_options.resources_dir.as_ref() {
            Some(path_buf) => runtime.set("resources_dir", render_host_visible_path(path_buf))?,
            None => runtime.set("resources_dir", LuaValue::Nil)?,
        }

        let exec_default_encoding = default_text_encoding;
        let launchers_fn = lua.create_function(|lua, ()| {
            let info = lua.create_table()?;
            let shells = lua.create_table()?;
            for (index, shell_name) in supported_exec_shell_names().into_iter().enumerate() {
                shells.set(index + 1, shell_name)?;
            }
            info.set("default", default_exec_shell_name())?;
            info.set("shells", shells)?;
            Ok(info)
        })?;
        process.set("launchers", launchers_fn)?;
        let exec_fn = lua.create_function(move |lua, spec: LuaValue| {
            let request = parse_exec_request(spec, "process.exec", exec_default_encoding)?;
            let result = execute_exec_request(request);
            exec_result_to_lua_table(lua, result)
        })?;
        process.set("exec", exec_fn)?;
        let which_fn = lua.create_function(|lua, program: LuaValue| {
            let program = require_string_arg(program, "process.which", "program", false)?;
            match resolve_vulcan_process_which(&program) {
                Ok(Some(found)) => {
                    let rendered = render_host_visible_path(&found);
                    Ok(LuaValue::String(lua.create_string(&rendered)?))
                }
                Ok(None) => Ok(LuaValue::Nil),
                Err(error) => Err(mlua::Error::runtime(error)),
            }
        })?;
        process.set("which", which_fn)?;
        process.set(
            "session",
            create_process_session_table(lua, default_text_encoding)?,
        )?;

        let os_info_fn = lua.create_function(|lua, ()| {
            let current_os = std::env::consts::OS;
            let arch = match std::env::consts::ARCH {
                "x86_64" => "x86_64",
                "x86" => "i686",
                "aarch64" => "aarch64",
                "arm" => "armv7l",
                _ => std::env::consts::ARCH,
            };
            let info = lua.create_table()?;
            info.set("os", current_os)?;
            info.set("arch", arch)?;
            Ok(info)
        })?;
        os.set("info", os_info_fn)?;

        let json_encode_fn =
            lua.create_function(|lua, val: LuaValue| match lua_value_to_json(&val) {
                Ok(value) => {
                    let text = serde_json::to_string(&value).map_err(|error| {
                        mlua::Error::runtime(format!(
                            "json.encode: failed to serialize JSON value: {error}"
                        ))
                    })?;
                    lua.create_string(text)
                }
                Err(error) => Err(mlua::Error::runtime(format!("json.encode: {}", error))),
            })?;
        json.set("encode", json_encode_fn)?;

        let json_decode_fn = lua.create_function(|lua, s: LuaValue| {
            let s = require_string_arg(s, "json.decode", "text", false)?;
            match serde_json::from_str::<Value>(&s) {
                Ok(value) => json_value_to_lua(lua, &value),
                Err(error) => Err(mlua::Error::runtime(format!("json.decode: {}", error))),
            }
        })?;
        json.set("decode", json_decode_fn)?;

        let cache_put_fn = lua.create_function(|lua, (value, ttl_sec): (LuaValue, LuaValue)| {
            let internal = get_vulcan_runtime_internal_table(lua).map_err(mlua::Error::runtime)?;
            let tool_name: Option<String> =
                internal.get("tool_name").map_err(mlua::Error::runtime)?;
            let skill_name: Option<String> =
                internal.get("skill_name").map_err(mlua::Error::runtime)?;
            let scope = tool_name
                .or(skill_name)
                .unwrap_or_else(|| "__runtime".to_string());
            let ttl_secs = optional_u64_arg(ttl_sec, "cache.put", "ttl_sec")?;
            let payload = lua_value_to_json(&value)
                .map_err(|error| mlua::Error::runtime(format!("cache.put: {}", error)))?;
            global_tool_cache()
                .create(&scope, payload, ttl_secs)
                .map_err(|error| mlua::Error::runtime(format!("cache.put: {}", error)))
        })?;
        cache.set("put", cache_put_fn)?;

        let cache_get_fn = lua.create_function(|lua, cache_id: LuaValue| {
            let internal = get_vulcan_runtime_internal_table(lua).map_err(mlua::Error::runtime)?;
            let tool_name: Option<String> =
                internal.get("tool_name").map_err(mlua::Error::runtime)?;
            let skill_name: Option<String> =
                internal.get("skill_name").map_err(mlua::Error::runtime)?;
            let scope = tool_name
                .or(skill_name)
                .unwrap_or_else(|| "__runtime".to_string());
            let cache_id = require_string_arg(cache_id, "cache.get", "cache_id", false)?;
            match global_tool_cache().get(&scope, &cache_id) {
                Some(value) => json_value_to_lua(lua, &value),
                None => Ok(LuaValue::Nil),
            }
        })?;
        cache.set("get", cache_get_fn)?;

        let cache_delete_fn = lua.create_function(|lua, cache_id: LuaValue| {
            let internal = get_vulcan_runtime_internal_table(lua).map_err(mlua::Error::runtime)?;
            let tool_name: Option<String> =
                internal.get("tool_name").map_err(mlua::Error::runtime)?;
            let skill_name: Option<String> =
                internal.get("skill_name").map_err(mlua::Error::runtime)?;
            let scope = tool_name
                .or(skill_name)
                .unwrap_or_else(|| "__runtime".to_string());
            let cache_id = require_string_arg(cache_id, "cache.delete", "cache_id", false)?;
            Ok(global_tool_cache().delete(&scope, &cache_id))
        })?;
        cache.set("delete", cache_delete_fn)?;

        let config_get_store = skill_config_store.clone();
        let config_get_fn = lua.create_function(move |lua, key: LuaValue| {
            let key = require_string_arg(key, "config.get", "key", false)?;
            let skill_id = current_vulcan_config_skill_id(lua, "vulcan.config.get")?;
            match config_get_store
                .get_value(&skill_id, &key)
                .map_err(mlua::Error::runtime)?
            {
                Some(value) => Ok(LuaValue::String(
                    lua.create_string(&value).map_err(mlua::Error::runtime)?,
                )),
                None => Ok(LuaValue::Nil),
            }
        })?;
        config.set("get", config_get_fn)?;

        let config_has_store = skill_config_store.clone();
        let config_has_fn = lua.create_function(move |lua, key: LuaValue| {
            let key = require_string_arg(key, "config.has", "key", false)?;
            let skill_id = current_vulcan_config_skill_id(lua, "vulcan.config.has")?;
            config_has_store
                .has_value(&skill_id, &key)
                .map_err(mlua::Error::runtime)
        })?;
        config.set("has", config_has_fn)?;

        let config_set_store = skill_config_store.clone();
        let config_set_fn =
            lua.create_function(move |lua, (key, value): (LuaValue, LuaValue)| {
                let key = require_string_arg(key, "config.set", "key", false)?;
                let value = require_string_arg(value, "config.set", "value", true)?;
                let skill_id = current_vulcan_config_skill_id(lua, "vulcan.config.set")?;
                config_set_store
                    .set_value(&skill_id, &key, &value)
                    .map_err(mlua::Error::runtime)?;
                Ok(true)
            })?;
        config.set("set", config_set_fn)?;

        let config_delete_store = skill_config_store.clone();
        let config_delete_fn = lua.create_function(move |lua, key: LuaValue| {
            let key = require_string_arg(key, "config.delete", "key", false)?;
            let skill_id = current_vulcan_config_skill_id(lua, "vulcan.config.delete")?;
            config_delete_store
                .delete_value(&skill_id, &key)
                .map_err(mlua::Error::runtime)
        })?;
        config.set("delete", config_delete_fn)?;

        let config_list_store = skill_config_store.clone();
        let config_list_fn = lua.create_function(move |lua, ()| {
            let skill_id = current_vulcan_config_skill_id(lua, "vulcan.config.list")?;
            let items = config_list_store
                .list_skill_values(&skill_id)
                .map_err(mlua::Error::runtime)?;
            let table = lua.create_table().map_err(mlua::Error::runtime)?;
            for (key, value) in items {
                table
                    .set(
                        key,
                        LuaValue::String(lua.create_string(&value).map_err(mlua::Error::runtime)?),
                    )
                    .map_err(mlua::Error::runtime)?;
            }
            Ok(LuaValue::Table(table))
        })?;
        config.set("list", config_list_fn)?;

        host.set("list", create_host_tool_list_fn(lua)?)?;
        let host_has_fn = create_host_tool_has_fn(lua)?;
        host.set("has", host_has_fn.clone())?;
        host.set("has_tool", host_has_fn)?;
        host.set("call", create_host_tool_call_fn(lua)?)?;

        models.set("status", create_model_status_fn(lua)?)?;
        models.set("has", create_model_has_fn(lua)?)?;
        models.set("embed", create_model_embed_fn(lua)?)?;
        models.set("llm", create_model_llm_fn(lua)?)?;

        context.set("request", lua.create_table()?)?;
        context.set("client_info", LuaValue::Nil)?;
        context.set("client_capabilities", lua.create_table()?)?;
        context.set("client_budget", lua.create_table()?)?;
        context.set("tool_config", lua.create_table()?)?;
        context.set("host_result", lua.create_table()?)?;
        context.set("skill_dir", LuaValue::Nil)?;
        context.set("entry_dir", LuaValue::Nil)?;
        context.set("entry_file", LuaValue::Nil)?;
        deps.set("tools_path", LuaValue::Nil)?;
        deps.set("lua_path", LuaValue::Nil)?;
        deps.set("ffi_path", LuaValue::Nil)?;

        let skill_management_enabled = host_options.capabilities.enable_skill_management_bridge;
        runtime_skills.set("enabled", skill_management_enabled)?;

        let runtime_skills_status_fn = lua.create_function(move |lua, ()| {
            let status = lua.create_table()?;
            let callback_registered = try_has_skill_management_callback();
            status.set("enabled", skill_management_enabled)?;
            status.set("callback_registered", callback_registered)?;
            status.set("mode", "host_callback")?;
            let message = if !skill_management_enabled {
                "Skill management bridge is disabled by host policy"
            } else if callback_registered {
                "Skill management bridge is enabled and ready"
            } else {
                "Skill management bridge is enabled but no host callback is registered"
            };
            status.set("message", message)?;
            Ok(status)
        })?;
        runtime_skills.set("status", runtime_skills_status_fn)?;
        runtime_skills.set(
            "layers",
            create_runtime_skill_layers_fn(lua, runtime_skill_roots, skill_management_enabled)?,
        )?;
        runtime_skills.set(
            "install",
            create_runtime_skill_management_bridge_fn(
                lua,
                skill_management_enabled,
                RuntimeSkillManagementAction::Install,
                "install",
            )?,
        )?;
        runtime_skills.set(
            "update",
            create_runtime_skill_management_bridge_fn(
                lua,
                skill_management_enabled,
                RuntimeSkillManagementAction::Update,
                "update",
            )?,
        )?;
        runtime_skills.set(
            "uninstall",
            create_runtime_skill_management_bridge_fn(
                lua,
                skill_management_enabled,
                RuntimeSkillManagementAction::Uninstall,
                "uninstall",
            )?,
        )?;
        runtime_skills.set(
            "enable",
            create_runtime_skill_management_bridge_fn(
                lua,
                skill_management_enabled,
                RuntimeSkillManagementAction::Enable,
                "enable",
            )?,
        )?;
        runtime_skills.set(
            "disable",
            create_runtime_skill_management_bridge_fn(
                lua,
                skill_management_enabled,
                RuntimeSkillManagementAction::Disable,
                "disable",
            )?,
        )?;

        let overflow_type = lua.create_table()?;
        overflow_type.set("truncate", "truncate")?;
        overflow_type.set("page", "page")?;
        runtime.set("overflow_type", overflow_type)?;

        runtime_internal.set("tool_name", LuaValue::Nil)?;
        runtime_internal.set("skill_name", LuaValue::Nil)?;
        runtime_internal.set("entry_name", LuaValue::Nil)?;
        runtime_internal.set("root_name", LuaValue::Nil)?;
        runtime_internal.set("luaexec_active", false)?;
        runtime_internal.set("luaexec_caller_tool_name", LuaValue::Nil)?;
        runtime.set("internal", runtime_internal)?;
        runtime.set("skills", runtime_skills)?;
        runtime.set("lua", runtime_lua)?;
        runtime.set("python", runtime_python)?;
        runtime.set("node", runtime_node)?;

        let call_stub = lua.create_function(|_, _: (LuaValue, LuaValue)| {
            Err::<(), _>(mlua::Error::runtime("vulcan.call not initialized"))
        })?;
        vulcan.set("call", call_stub)?;
        vulcan.set("runtime", runtime)?;
        vulcan.set("fs", fs)?;
        vulcan.set("io", vulcan_io)?;
        vulcan.set("path", path)?;
        vulcan.set("process", process)?;
        vulcan.set("os", os)?;
        vulcan.set("json", json)?;
        vulcan.set("cache", cache)?;
        vulcan.set("config", config)?;
        vulcan.set("host", host)?;
        vulcan.set("models", models)?;
        vulcan.set("context", context)?;
        vulcan.set("deps", deps)?;

        lua.globals().set("vulcan", vulcan)?;
        Ok(())
    }
}

// ============================================================
// JSON ↔ Lua Value conversion
// ============================================================

fn json_to_lua_table(lua: &Lua, json: &Value) -> Result<Table, String> {
    json_to_lua_table_inner(lua, json).map_err(|e| e.to_string())
}

/// Convert one provider Lua `input` table argument into the JSON value passed to provider bindings.
/// 将一个 provider Lua `input` 表参数转换为传递给 provider binding 的 JSON 值。
///
/// The input parameter is the raw Lua argument received by one provider proxy function.
/// input 参数是某个 provider 代理函数收到的原始 Lua 参数。
///
/// The api_name parameter is the stable API name used in argument validation errors.
/// api_name 参数是参数校验错误中使用的稳定 API 名称。
///
/// Return the JSON value that will be forwarded to the provider binding.
/// 返回即将转发给 provider binding 的 JSON 值。
fn provider_input_table_to_json(input: LuaValue, api_name: &str) -> mlua::Result<Value> {
    let input_table = require_table_arg(input, api_name, "input")?;
    lua_value_to_json(&LuaValue::Table(input_table)).map_err(mlua::Error::runtime)
}

/// Convert one provider JSON result into a Lua value while preserving provider runtime errors.
/// 将一个 provider JSON 结果转换为 Lua 值，同时保留 provider 运行时错误。
///
/// The lua parameter owns the Lua state used to allocate returned strings and tables.
/// lua 参数持有用于分配返回字符串与表的 Lua 状态。
///
/// The result parameter is the provider binding result that should be exposed to Lua.
/// result 参数是需要暴露给 Lua 的 provider binding 结果。
///
/// Return the Lua value that will be returned by the provider proxy function.
/// 返回 provider 代理函数即将返回的 Lua 值。
fn provider_json_result_to_lua(lua: &Lua, result: Result<Value, String>) -> mlua::Result<LuaValue> {
    let result = result.map_err(mlua::Error::runtime)?;
    json_value_to_lua(lua, &result).map_err(mlua::Error::external)
}

/// Create one provider table and write the current skill marker onto `vulcan`.
/// 创建一个 provider 表，并把当前 skill 标记写入 `vulcan`。
///
/// The lua parameter owns the Lua state whose globals contain the `vulcan` module.
/// lua 参数持有全局表中包含 `vulcan` 模块的 Lua 状态。
///
/// The provider_name parameter is the provider table name under `vulcan`.
/// provider_name 参数是 `vulcan` 下的 provider 表名。
///
/// The skill_marker_key parameter is the private `vulcan` key that stores the current skill name.
/// skill_marker_key 参数是保存当前 skill 名称的 `vulcan` 私有键。
///
/// The current_skill_name parameter is the optional skill name associated with the provider context.
/// current_skill_name 参数是与 provider 上下文关联的可选 skill 名称。
///
/// Return the shared `vulcan` table and the newly created provider table.
/// 返回共享的 `vulcan` 表与新创建的 provider 表。
fn create_provider_context_table(
    lua: &Lua,
    provider_name: &str,
    skill_marker_key: &str,
    current_skill_name: Option<&str>,
) -> Result<(Table, Table), String> {
    // Resolve the shared root table before creating provider-specific state.
    // 在创建 provider 专属状态前先解析共享根表。
    let vulcan = get_vulcan_table(lua)?;

    // Allocate a fresh provider table so repeated scope changes cannot leak old methods.
    // 分配新的 provider 表，避免重复作用域切换泄漏旧方法。
    let provider_table = lua
        .create_table()
        .map_err(|error| format!("Failed to create vulcan.{} table: {}", provider_name, error))?;

    // Normalize the absent skill name to the existing empty-string marker contract.
    // 将缺失的 skill 名称归一化为现有空字符串标记约定。
    let current_skill = current_skill_name.unwrap_or("");
    vulcan
        .set(skill_marker_key, current_skill)
        .map_err(|error| format!("Failed to set vulcan.{}: {}", skill_marker_key, error))?;

    Ok((vulcan, provider_table))
}

/// Install one populated provider table onto the shared `vulcan` module.
/// 将一个已填充的 provider 表安装到共享的 `vulcan` 模块。
///
/// The vulcan parameter is the shared `vulcan` module table.
/// vulcan 参数是共享的 `vulcan` 模块表。
///
/// The provider_name parameter is the provider table name under `vulcan`.
/// provider_name 参数是 `vulcan` 下的 provider 表名。
///
/// The provider_table parameter is the fully populated provider table.
/// provider_table 参数是已经填充完成的 provider 表。
///
/// Return `Ok(())` after the provider table is installed.
/// provider 表安装完成后返回 `Ok(())`。
fn install_provider_context_table(
    vulcan: &Table,
    provider_name: &str,
    provider_table: Table,
) -> Result<(), String> {
    // Publish after population so Lua callers never observe a half-filled provider table.
    // 填充完成后再发布，避免 Lua 调用方观察到半填充的 provider 表。
    vulcan
        .set(provider_name, provider_table)
        .map_err(|error| format!("Failed to set vulcan.{}: {}", provider_name, error))?;
    Ok(())
}

/// Register one provider method whose Lua input table maps directly to a provider JSON result.
/// 注册一个 Lua 输入表可直接映射为 provider JSON 结果的 provider 方法。
///
/// The lua parameter owns the Lua state used to create the callable proxy.
/// lua 参数持有用于创建可调用代理的 Lua 状态。
///
/// The provider_table parameter is the `vulcan.<provider>` table that receives the method.
/// provider_table 参数是接收该方法的 `vulcan.<provider>` 表。
///
/// The provider_name parameter is the stable provider table name used in Lua-visible error messages.
/// provider_name 参数是 Lua 可见错误消息中使用的稳定 provider 表名。
///
/// The method_name parameter is the method key under the provider table and the API suffix used for input validation.
/// method_name 参数是 provider 表下的方法键，也是输入校验所用的 API 后缀。
///
/// The invoke parameter forwards the converted JSON input into the provider binding.
/// invoke 参数将转换后的 JSON 输入转发给 provider binding。
///
/// Return `Ok(())` after the Lua function is created and installed on the provider table.
/// Lua 函数创建并安装到 provider 表后返回 `Ok(())`。
fn register_provider_json_method<F>(
    lua: &Lua,
    provider_table: &Table,
    provider_name: &str,
    method_name: &str,
    invoke: F,
) -> Result<(), String>
where
    F: Fn(&Value) -> Result<Value, String> + Send + 'static,
{
    let api_name = format!("{}.{}", provider_name, method_name);
    let lua_path = format!("vulcan.{}", api_name);
    let input_api_name = api_name;
    let method_fn = lua
        .create_function(move |lua, input: LuaValue| {
            let input_json = provider_input_table_to_json(input, &input_api_name)?;
            provider_json_result_to_lua(lua, invoke(&input_json))
        })
        .map_err(|error| format!("Failed to create {}: {}", lua_path, error))?;
    provider_table
        .set(method_name, method_fn)
        .map_err(|error| format!("Failed to set {}: {}", lua_path, error))?;
    Ok(())
}

/// Register one provider method that takes no Lua arguments and returns one JSON value.
/// 注册一个不接收 Lua 参数并返回 JSON 值的 provider 方法。
///
/// The lua parameter owns the Lua state used to create the callable proxy.
/// lua 参数持有用于创建可调用代理的 Lua 状态。
///
/// The provider_table parameter is the `vulcan.<provider>` table that receives the method.
/// provider_table 参数是接收该方法的 `vulcan.<provider>` 表。
///
/// The method_name parameter is the method key under the provider table.
/// method_name 参数是 provider 表下的方法键。
///
/// The create_error_subject parameter is the subject text used when Lua function creation fails.
/// create_error_subject 参数是 Lua 函数创建失败时使用的主体文本。
///
/// The set_error_subject parameter is the subject text used when table installation fails.
/// set_error_subject 参数是表安装失败时使用的主体文本。
///
/// The resolve parameter produces the JSON value or diagnostic returned by the Lua method.
/// resolve 参数生成 Lua 方法返回的 JSON 值或诊断错误。
///
/// Return `Ok(())` after the Lua function is created and installed.
/// Lua 函数创建并安装完成后返回 `Ok(())`。
fn register_provider_json_noarg_method<F>(
    lua: &Lua,
    provider_table: &Table,
    method_name: &str,
    create_error_subject: &str,
    set_error_subject: &str,
    resolve: F,
) -> Result<(), String>
where
    F: Fn() -> Result<Value, String> + Send + 'static,
{
    let method_fn = lua
        .create_function(move |lua, ()| {
            let value = resolve().map_err(mlua::Error::runtime)?;
            json_value_to_lua(lua, &value).map_err(mlua::Error::external)
        })
        .map_err(|error| format!("Failed to create {}: {}", create_error_subject, error))?;
    provider_table
        .set(method_name, method_fn)
        .map_err(|error| format!("Failed to set {}: {}", set_error_subject, error))?;
    Ok(())
}

/// Install the disabled provider status, info, and proxy methods for one skill without a binding.
/// 为没有 binding 的 skill 安装禁用状态、信息与代理方法。
///
/// The lua parameter owns the Lua state used to create disabled proxy functions.
/// lua 参数持有用于创建禁用代理函数的 Lua 状态。
///
/// The provider_table parameter is the `vulcan.<provider>` table being populated.
/// provider_table 参数是正在填充的 `vulcan.<provider>` 表。
///
/// The provider_name parameter is the stable provider name used in error messages.
/// provider_name 参数是错误消息中使用的稳定 provider 名称。
///
/// The disabled_status parameter is the provider-specific disabled status JSON exposed by `status` and `info`.
/// disabled_status 参数是 `status` 与 `info` 暴露的 provider 专属禁用状态 JSON。
///
/// The disabled_error parameter is the runtime error returned by disabled operation proxies.
/// disabled_error 参数是禁用操作代理返回的运行时错误。
///
/// The method_names parameter lists operation names that should reject calls while the binding is absent.
/// method_names 参数列出 binding 缺失时应拒绝调用的操作名称。
///
/// Return `Ok(())` after all disabled methods are installed.
/// 所有禁用方法安装完成后返回 `Ok(())`。
fn install_disabled_provider_context(
    lua: &Lua,
    provider_table: &Table,
    provider_name: &str,
    disabled_status: Value,
    disabled_error: &str,
    method_names: &[&str],
) -> Result<(), String> {
    provider_table
        .set("enabled", false)
        .map_err(|error| format!("Failed to set vulcan.{}.enabled: {}", provider_name, error))?;

    let status_value = disabled_status.clone();
    register_provider_json_noarg_method(
        lua,
        provider_table,
        "status",
        &format!("disabled vulcan.{}.status", provider_name),
        &format!("vulcan.{}.status", provider_name),
        move || Ok(status_value.clone()),
    )?;

    register_provider_json_noarg_method(
        lua,
        provider_table,
        "info",
        &format!("disabled vulcan.{}.info", provider_name),
        &format!("disabled vulcan.{}.info", provider_name),
        move || Ok(disabled_status.clone()),
    )?;

    for method_name in method_names {
        let error_text = disabled_error.to_string();
        let fn_value = lua
            .create_function(move |_, _: MultiValue| {
                Err::<LuaValue, _>(mlua::Error::runtime(error_text.clone()))
            })
            .map_err(|error| {
                format!(
                    "Failed to create disabled vulcan.{} proxy: {}",
                    provider_name, error
                )
            })?;
        provider_table
            .set(*method_name, fn_value)
            .map_err(|error| format!("Failed to set disabled method {}: {}", method_name, error))?;
    }
    Ok(())
}

fn json_to_lua_table_inner(lua: &Lua, json: &Value) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    if let Value::Object(obj) = json {
        for (k, v) in obj {
            table.set(k.as_str(), json_value_to_lua(lua, v)?)?;
        }
    } else if let Value::Array(arr) = json {
        for (i, v) in arr.iter().enumerate() {
            table.set(i + 1, json_value_to_lua(lua, v)?)?;
        }
    }
    Ok(table)
}

fn json_value_to_lua(lua: &Lua, json: &Value) -> mlua::Result<LuaValue> {
    match json {
        Value::Null => Ok(LuaValue::Nil),
        Value::Bool(b) => Ok(LuaValue::Boolean(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(LuaValue::Integer(i))
            } else {
                Ok(LuaValue::Number(n.as_f64().unwrap_or(0.0)))
            }
        }
        Value::String(s) => Ok(LuaValue::String(lua.create_string(s)?)),
        Value::Array(_) | Value::Object(_) => {
            Ok(LuaValue::Table(json_to_lua_table_inner(lua, json)?))
        }
    }
}

/// Convert one Lua value into a JSON value without lossy text coercion.
/// 将单个 Lua 值转换为 JSON 值，且不执行有损文本转换。
///
/// The val parameter is the Lua value captured from runtime-visible tables or function output.
/// val 参数是从运行时可见表或函数输出中捕获的 Lua 值。
///
/// Return the JSON representation, or an explicit error when a Lua value cannot be represented.
/// 返回 JSON 表示；当 Lua 值无法表示时返回显式错误。
fn lua_value_to_json(val: &LuaValue) -> Result<Value, String> {
    match val {
        LuaValue::Nil => Ok(Value::Null),
        LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
        LuaValue::Integer(i) => Ok(Value::Number((*i).into())),
        LuaValue::Number(f) => {
            if let Some(n) = serde_json::Number::from_f64(*f) {
                Ok(Value::Number(n))
            } else {
                Ok(Value::Null)
            }
        }
        LuaValue::String(s) => s
            .to_str()
            .map(|text| Value::String(text.to_string()))
            .map_err(|error| format!("Cannot convert Lua string to JSON: invalid UTF-8: {error}")),
        LuaValue::Table(t) => {
            // Heuristic: if raw_len() > 0, treat as array. Otherwise as object.
            if t.raw_len() > 0 {
                let arr = lua_table_to_array(t)?;
                Ok(Value::Array(arr))
            } else {
                lua_table_to_object(t)
            }
        }
        LuaValue::Function(_) => Err("Cannot convert Lua function to JSON".to_string()),
        LuaValue::Thread(_) => Err("Cannot convert Lua thread to JSON".to_string()),
        LuaValue::UserData(userdata) => {
            if let Ok(context) = userdata.borrow::<self::lease::ReadonlyJsonContextValue>() {
                return Ok(context.json_value().clone());
            }
            Err("Cannot convert Lua userdata to JSON".to_string())
        }
        LuaValue::LightUserData(_) => Err("Cannot convert light userdata to JSON".to_string()),
        _ => Err("Unknown Lua value type".to_string()),
    }
}

fn lua_table_to_array(t: &Table) -> Result<Vec<Value>, String> {
    let len = t.raw_len();
    if len == 0 {
        // Could be empty object or empty array, default to array
        return Ok(Vec::new());
    }
    let mut arr = Vec::with_capacity(len);
    for i in 1..=len {
        let val: LuaValue = t.get(i).map_err(|e| format!("Array index {}: {}", i, e))?;
        arr.push(lua_value_to_json(&val)?);
    }
    Ok(arr)
}

fn lua_table_to_object(t: &Table) -> Result<Value, String> {
    let mut obj = serde_json::Map::new();
    for pair in t.pairs::<String, LuaValue>() {
        let (k, v) = pair.map_err(|e| format!("Table key: {}", e))?;
        obj.insert(k, lua_value_to_json(&v)?);
    }
    // Empty Lua table has no distinction between array and object.
    // If there are no string keys, treat as empty array.
    if obj.is_empty() && t.raw_len() == 0 {
        return Ok(Value::Array(Vec::new()));
    }
    Ok(Value::Object(obj))
}

#[cfg(test)]
pub(crate) mod tests;
