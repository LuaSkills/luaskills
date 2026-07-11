use crate::host::database::{LuaRuntimeDatabaseCallbackMode, LuaRuntimeDatabaseProviderMode};
use crate::runtime_context::RuntimeRequestContext;
use crate::tool_cache::ToolCacheConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// Fixed directory layout derived from one LuaSkills runtime root.
/// 从单个 LuaSkills 运行时根目录推导出的固定目录布局。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LuaRuntimeLayout {
    /// Canonical root directory owned by the LuaSkills runtime.
    /// LuaSkills 运行时拥有的规范根目录。
    pub runtime_root: PathBuf,
}

impl LuaRuntimeLayout {
    /// Construct one fixed runtime layout from one root directory.
    /// 基于单个根目录构造固定运行时布局。
    pub fn new(runtime_root: impl Into<PathBuf>) -> Self {
        Self {
            runtime_root: runtime_root.into(),
        }
    }

    /// Return the runtime bin directory used for host-provided tools.
    /// 返回用于宿主提供工具的运行时 bin 目录。
    pub fn bin_dir(&self) -> PathBuf {
        self.runtime_root.join("bin")
    }

    /// Return the runtime native library directory used for FFI and DLL dependencies.
    /// 返回用于 FFI 与 DLL 依赖的运行时原生库目录。
    pub fn libs_dir(&self) -> PathBuf {
        self.runtime_root.join("libs")
    }

    /// Return the runtime Lua packages directory used by `package.path` and `package.cpath`.
    /// 返回供 `package.path` 与 `package.cpath` 使用的运行时 Lua 包目录。
    pub fn lua_packages_dir(&self) -> PathBuf {
        self.runtime_root.join("lua_packages")
    }

    /// Return the runtime resources directory used for packaged metadata and assets.
    /// 返回用于打包元数据与资源的运行时 resources 目录。
    pub fn resources_dir(&self) -> PathBuf {
        self.runtime_root.join("resources")
    }

    /// Return the runtime skills directory containing installed LuaSkills packages.
    /// 返回包含已安装 LuaSkills 包的运行时 skills 目录。
    pub fn skills_dir(&self) -> PathBuf {
        self.runtime_root.join("skills")
    }

    /// Return the runtime temporary directory.
    /// 返回运行时临时目录。
    pub fn temp_dir(&self) -> PathBuf {
        self.runtime_root.join("temp")
    }

    /// Return the runtime download-cache directory.
    /// 返回运行时下载缓存目录。
    pub fn downloads_dir(&self) -> PathBuf {
        self.temp_dir().join("downloads")
    }

    /// Return the runtime dependency directory.
    /// 返回运行时依赖目录。
    pub fn dependencies_dir(&self) -> PathBuf {
        self.runtime_root.join("dependencies")
    }

    /// Return the runtime state directory.
    /// 返回运行时状态目录。
    pub fn state_dir(&self) -> PathBuf {
        self.runtime_root.join("state")
    }

    /// Return the runtime database directory.
    /// 返回运行时数据库目录。
    pub fn databases_dir(&self) -> PathBuf {
        self.runtime_root.join("databases")
    }

    /// Return the runtime config directory.
    /// 返回运行时配置目录。
    pub fn config_dir(&self) -> PathBuf {
        self.runtime_root.join("config")
    }

    /// Return the unified runtime skill-config file path.
    /// 返回统一运行时技能配置文件路径。
    pub fn skill_config_file_path(&self) -> PathBuf {
        self.config_dir().join("skill_config.json")
    }

    /// Return the host-owned system Lua library directory.
    /// 返回宿主自有系统 Lua 库目录。
    pub fn system_lua_lib_dir(&self) -> PathBuf {
        self.runtime_root.join("system_lua_lib")
    }
}

/// Process mode used when the runtime auto-spawns one local space controller process.
/// 运行时自动拉起本地空间控制器进程时使用的进程模式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LuaRuntimeSpaceControllerProcessMode {
    /// Service mode keeps the controller process alive until an external stop happens.
    /// Service 模式会让控制器进程持续存活，直到外部显式停止。
    Service,
    /// Managed mode allows the controller process to stop itself after idle timeouts.
    /// Managed 模式允许控制器进程在空闲超时后自行停止。
    #[default]
    Managed,
}

/// Host-provided controller client options used when one database backend chooses `space_controller`.
/// 当数据库后端选择 `space_controller` 时使用的宿主侧控制器客户端选项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LuaRuntimeSpaceControllerOptions {
    /// Optional explicit controller endpoint; when omitted the shared default endpoint is used.
    /// 可选的显式控制器端点；缺失时使用共享默认端点。
    pub endpoint: Option<String>,
    /// Whether the runtime may auto-spawn the controller when the endpoint is unavailable.
    /// 当控制器端点不可用时，运行时是否允许自动唤起控制器。
    pub auto_spawn: bool,
    /// Optional local executable path copied and managed by the host.
    /// 由宿主复制并管理的可选本地可执行文件路径。
    pub executable_path: Option<PathBuf>,
    /// Process mode used when auto-spawning one controller process.
    /// 自动唤起控制器进程时使用的进程模式。
    pub process_mode: LuaRuntimeSpaceControllerProcessMode,
    /// Minimum uptime in seconds passed to one auto-spawned controller.
    /// 传递给自动唤起控制器的最小存活秒数。
    pub minimum_uptime_secs: u64,
    /// Idle timeout in seconds passed to one auto-spawned controller.
    /// 传递给自动唤起控制器的空闲超时秒数。
    pub idle_timeout_secs: u64,
    /// Default lease TTL in seconds passed to one auto-spawned controller.
    /// 传递给自动唤起控制器的默认租约 TTL 秒数。
    pub default_lease_ttl_secs: u64,
    /// Transport connect timeout in seconds used by the controller client proxy.
    /// 控制器客户端代理使用的传输连接超时秒数。
    pub connect_timeout_secs: u64,
    /// Startup timeout in seconds used while waiting for one spawned controller to become ready.
    /// 等待已唤起控制器就绪时使用的启动超时秒数。
    pub startup_timeout_secs: u64,
    /// Startup retry interval in milliseconds used while polling one spawned controller.
    /// 轮询已唤起控制器时使用的启动重试间隔毫秒数。
    pub startup_retry_interval_ms: u64,
    /// Lease renew interval in seconds used by the background controller client task.
    /// 后台控制器客户端任务使用的租约续约间隔秒数。
    pub lease_renew_interval_secs: u64,
}

impl Default for LuaRuntimeSpaceControllerOptions {
    /// Return one safe-by-default shared controller configuration.
    /// 返回一套默认安全且面向共享控制器的配置。
    fn default() -> Self {
        Self {
            endpoint: None,
            auto_spawn: true,
            executable_path: None,
            process_mode: LuaRuntimeSpaceControllerProcessMode::Managed,
            minimum_uptime_secs: 300,
            idle_timeout_secs: 900,
            default_lease_ttl_secs: 120,
            connect_timeout_secs: 5,
            startup_timeout_secs: 15,
            startup_retry_interval_ms: 250,
            lease_renew_interval_secs: 30,
        }
    }
}

/// Host-controlled toggles for optional Lua-exposed runtime bridges.
/// 宿主控制的可选 Lua 暴露运行时桥接开关集合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LuaRuntimeCapabilityOptions {
    /// Whether `vulcan.runtime.skills.*` management bridges are exposed to Lua.
    /// 是否将 `vulcan.runtime.skills.*` 管理桥接暴露给 Lua。
    #[serde(default)]
    pub enable_skill_management_bridge: bool,
    /// Whether luaexec and runtime sessions replace Lua's global `io` table with managed IO.
    /// luaexec 与持久运行时会话是否使用托管 IO 替换 Lua 全局 `io` 表。
    pub enable_managed_io_compat: bool,
}

impl Default for LuaRuntimeCapabilityOptions {
    /// Return one secure-by-default capability set.
    /// 返回一组默认安全的能力开关集合。
    fn default() -> Self {
        Self {
            enable_skill_management_bridge: false,
            enable_managed_io_compat: true,
        }
    }
}

/// One named skill root injected by the host, used to build ordered override environments.
/// 由宿主注入的单个命名技能根，用于构建有序覆盖环境。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RuntimeSkillRoot {
    /// Stable root name, limited to ROOT, PROJECT, or USER.
    /// 稳定根名称，仅限 ROOT、PROJECT 或 USER。
    pub name: String,
    /// Physical skills directory represented by the current named root.
    /// 当前命名根所代表的物理 skills 目录。
    pub skills_dir: PathBuf,
}

/// Host-provided pool configuration for isolated runlua VMs.
/// 宿主提供的隔离 runlua 虚拟机池配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LuaRuntimeRunLuaPoolConfig {
    /// Minimum number of isolated runlua VMs kept warm.
    /// 隔离 runlua 虚拟机需要常驻保温的最小数量。
    pub min_size: usize,
    /// Maximum number of isolated runlua VMs allowed in the pool.
    /// 隔离 runlua 虚拟机池允许存在的最大数量。
    pub max_size: usize,
    /// Idle TTL in seconds before one excess isolated runlua VM may be retired.
    /// 多余隔离 runlua 虚拟机在空闲多少秒后允许回收。
    pub idle_ttl_secs: u64,
}

/// Host-provided filesystem and runtime paths consumed by the LuaSkills library.
/// 宿主提供给 LuaSkills 库消费的文件系统与运行时路径集合。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LuaRuntimeHostOptions {
    /// Optional canonical LuaSkills runtime root used to derive the fixed runtime layout.
    /// 用于推导固定运行时布局的可选规范 LuaSkills 运行时根目录。
    #[serde(default)]
    pub runtime_root: Option<PathBuf>,
    /// Host-managed temporary directory used by luaexec spill files and similar transient artifacts.
    /// 宿主管理的临时目录，供 luaexec 请求文件等短生命周期产物使用。
    pub temp_dir: Option<PathBuf>,
    /// Optional host-managed resources directory exposed to Lua as `vulcan.runtime.resources_dir`.
    /// 以 `vulcan.runtime.resources_dir` 形式暴露给 Lua 的可选宿主管理资源目录。
    pub resources_dir: Option<PathBuf>,
    /// Optional lua_packages root used to build `package.path` and `package.cpath`.
    /// 用于拼接 `package.path` 与 `package.cpath` 的可选 lua_packages 根目录。
    pub lua_packages_dir: Option<PathBuf>,
    /// Host-managed root directory used only to probe host-provided tool dependencies.
    /// 仅用于探测宿主提供工具依赖的宿主管理根目录。
    pub host_provided_tool_root: Option<PathBuf>,
    /// Host-managed root directory used only to probe host-provided Lua package dependencies.
    /// 仅用于探测宿主提供 Lua 包依赖的宿主管理根目录。
    pub host_provided_lua_root: Option<PathBuf>,
    /// Host-managed root directory used only to probe host-provided FFI/native dependencies.
    /// 仅用于探测宿主提供 FFI/原生依赖的宿主管理根目录。
    pub host_provided_ffi_root: Option<PathBuf>,
    /// Optional fixed host-owned system Lua library directory used by `system_lua_lib` leases.
    /// 供 `system_lua_lib` 租约使用的可选固定宿主系统 Lua 库目录。
    pub system_lua_lib_dir: Option<PathBuf>,
    /// Host-managed cache directory used for downloaded archives and remote manifests.
    /// 宿主管理的下载缓存目录，用于归档文件和远程清单缓存。
    pub download_cache_root: Option<PathBuf>,
    /// Fixed sibling directory name used under one skill-root parent to store dependencies.
    /// 在单个技能根父目录下存放依赖时使用的固定兄弟目录名称。
    #[serde(default)]
    pub dependency_dir_name: String,
    /// Fixed sibling directory name used under one skill-root parent to store skill state.
    /// 在单个技能根父目录下存放技能状态时使用的固定兄弟目录名称。
    #[serde(default)]
    pub state_dir_name: String,
    /// Fixed sibling directory name used under one skill-root parent to store skill databases.
    /// 在单个技能根父目录下存放技能数据库时使用的固定兄弟目录名称。
    #[serde(default)]
    pub database_dir_name: String,
    /// Optional unified skill config file path owned by the host.
    /// 由宿主拥有的可选统一技能配置文件路径。
    pub skill_config_file_path: Option<PathBuf>,
    /// Whether the runtime is allowed to perform network downloads while installing dependencies.
    /// 运行时在安装依赖时是否允许执行网络下载。
    pub allow_network_download: bool,
    /// Optional GitHub site base URL override used to rewrite browser download URLs.
    /// 可选的 GitHub 站点基址覆盖，用于重写浏览器下载地址。
    pub github_base_url: Option<String>,
    /// Optional GitHub API base URL override used to resolve release metadata.
    /// 可选的 GitHub API 基址覆盖，用于解析 release 元数据。
    pub github_api_base_url: Option<String>,
    /// Optional official LuaSkills Hub base URL used by managed Hub installs.
    /// 受管 Hub 安装使用的可选官方 LuaSkills Hub 基址。
    #[serde(default)]
    pub official_skill_hub_base_url: Option<String>,
    /// Whether trusted system operations may install from private URL manifests.
    /// 可信 system 操作是否允许从私有 URL manifest 安装。
    #[serde(default)]
    pub enable_private_url_skill_install: bool,
    /// Host-controlled URL prefixes allowed for private skill manifests.
    /// 宿主管控的私有技能 manifest 允许 URL 前缀。
    #[serde(default)]
    pub private_skill_source_allowlist: Vec<String>,
    /// Optional default text encoding label used by managed IO and process APIs.
    /// 托管 IO 与进程 API 使用的可选默认文本编码标签。
    #[serde(default)]
    pub default_text_encoding: Option<String>,
    /// Explicit SQLite dynamic-library path owned by the host.
    /// 由宿主显式提供的 SQLite 动态库路径。
    pub sqlite_library_path: Option<PathBuf>,
    /// SQLite database provider mode selected by the host.
    /// 宿主为 SQLite 数据库选择的 provider 模式。
    #[serde(default)]
    pub sqlite_provider_mode: LuaRuntimeDatabaseProviderMode,
    /// SQLite callback transport mode selected by the host when provider mode is `host_callback`.
    /// 当 provider 模式为 `host_callback` 时，宿主为 SQLite 选择的回调传输模式。
    #[serde(default)]
    pub sqlite_callback_mode: LuaRuntimeDatabaseCallbackMode,
    /// Explicit LanceDB dynamic-library path owned by the host.
    /// 由宿主显式提供的 LanceDB 动态库路径。
    pub lancedb_library_path: Option<PathBuf>,
    /// LanceDB database provider mode selected by the host.
    /// 宿主为 LanceDB 数据库选择的 provider 模式。
    #[serde(default)]
    pub lancedb_provider_mode: LuaRuntimeDatabaseProviderMode,
    /// LanceDB callback transport mode selected by the host when provider mode is `host_callback`.
    /// 当 provider 模式为 `host_callback` 时，宿主为 LanceDB 选择的回调传输模式。
    #[serde(default)]
    pub lancedb_callback_mode: LuaRuntimeDatabaseCallbackMode,
    /// Shared controller client options used when one database backend selects `space_controller`.
    /// 当数据库后端选择 `space_controller` 时所使用的共享控制器客户端选项。
    #[serde(default)]
    pub space_controller: LuaRuntimeSpaceControllerOptions,
    /// Host-provided transient cache policy consumed by `vulcan.cache`.
    /// 由宿主提供并供 `vulcan.cache` 消费的临时缓存策略。
    pub cache_config: Option<ToolCacheConfig>,
    /// Optional dedicated pool configuration for isolated `vulcan.runtime.lua.exec` VMs.
    /// 供隔离 `vulcan.runtime.lua.exec` 虚拟机使用的可选独立池配置。
    #[serde(default)]
    pub runlua_pool_config: Option<LuaRuntimeRunLuaPoolConfig>,
    /// Host-reserved public entry names that LuaSkills canonical name generation must never occupy directly.
    /// 宿主保留的公开入口名称集合，LuaSkills 在生成 canonical 名称时必须直接避开这些名称。
    pub reserved_entry_names: Vec<String>,
    /// Host-forced skill identifiers that must be skipped before dependency or database setup.
    /// 宿主强制跳过的技能标识符列表，会在依赖或数据库初始化前生效。
    #[serde(default)]
    pub ignored_skill_ids: Vec<String>,
    /// Host-controlled optional runtime capability toggles.
    /// 由宿主控制的可选运行时能力开关集合。
    #[serde(default)]
    pub capabilities: LuaRuntimeCapabilityOptions,
}

impl LuaRuntimeHostOptions {
    /// Build host options from one canonical runtime root and its fixed derived layout.
    /// 基于单个规范运行时根目录及其固定派生布局构造宿主选项。
    pub fn with_runtime_root(runtime_root: impl Into<PathBuf>) -> Self {
        let mut options = Self {
            runtime_root: Some(runtime_root.into()),
            ..Self::default()
        };
        options.apply_runtime_root_layout();
        options
    }

    /// Return the fixed layout when a canonical runtime root has been configured.
    /// 当已配置规范运行时根目录时返回固定布局。
    pub fn runtime_layout(&self) -> Option<LuaRuntimeLayout> {
        self.runtime_root.clone().map(LuaRuntimeLayout::new)
    }

    /// Apply the fixed runtime-root layout to all legacy directory fields.
    /// 将固定 runtime-root 布局应用到所有兼容目录字段。
    pub fn apply_runtime_root_layout(&mut self) {
        let Some(layout) = self.runtime_layout() else {
            return;
        };

        self.temp_dir = Some(layout.temp_dir());
        self.resources_dir = Some(layout.resources_dir());
        self.lua_packages_dir = Some(layout.lua_packages_dir());
        self.host_provided_tool_root = Some(layout.bin_dir());
        self.host_provided_lua_root = Some(layout.lua_packages_dir());
        self.host_provided_ffi_root = Some(layout.libs_dir());
        self.system_lua_lib_dir = Some(layout.system_lua_lib_dir());
        self.download_cache_root = Some(layout.downloads_dir());
        self.dependency_dir_name = "dependencies".to_string();
        self.state_dir_name = "state".to_string();
        self.database_dir_name = "databases".to_string();
        self.skill_config_file_path = Some(layout.skill_config_file_path());
    }

    /// Return a normalized copy where `runtime_root` owns every derived runtime directory.
    /// 返回一份规范化副本，其中 `runtime_root` 统一拥有所有派生运行时目录。
    pub fn normalized(mut self) -> Self {
        self.apply_runtime_root_layout();
        self
    }
}

/// Host-injected invocation context delivered alongside one skill or runlua call.
/// 宿主在单次 skill 或 runlua 调用时一并注入的调用上下文。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LuaInvocationContext {
    /// Optional transport/request metadata preserved for Lua consumption.
    /// 供 Lua 消费的可选传输层/请求层元数据。
    pub request_context: Option<RuntimeRequestContext>,
    /// Host-resolved client budget object injected into `vulcan.context.client_budget`.
    /// 宿主解析后的客户端预算对象，将被注入到 `vulcan.context.client_budget`。
    pub client_budget: Value,
    /// Host-resolved tool configuration object injected into `vulcan.context.tool_config`.
    /// 宿主解析后的工具配置对象，将被注入到 `vulcan.context.tool_config`。
    pub tool_config: Value,
}

/// Host-resolved effective budget scope used by host-side render logic.
/// 供宿主侧渲染逻辑使用的宿主已解析生效预算场景结构。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct EffectiveBudgetScope {
    /// Effective byte limit for the current scope.
    /// 当前场景的生效字节上限。
    pub bytes: u64,
    /// Effective line limit for the current scope. `-1` means unlimited.
    /// 当前场景的生效行数上限，`-1` 表示不限。
    pub lines: i64,
}

/// Host-resolved client budget snapshot consumed by host-side overflow rendering.
/// 供宿主侧超限渲染消费的宿主已解析客户端预算快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientBudgetSnapshot {
    /// Optional client name of the active caller.
    /// 当前调用方的可选客户端名称。
    pub client_name: Option<String>,
    /// Optional resolved tool name.
    /// 可选的已解析工具名称。
    pub tool_name: Option<String>,
    /// Optional resolved skill name.
    /// 可选的已解析 skill 名称。
    pub skill_name: Option<String>,
    /// Optional matched client pattern decided by the host.
    /// 宿主判定出的可选客户端匹配模式。
    pub matched_client_pattern: Option<String>,
    /// Effective tool-result budget.
    /// 工具正文输出的生效预算。
    pub tool_result: EffectiveBudgetScope,
    /// Effective file-read budget.
    /// 文件读取场景的生效预算。
    pub file_read: EffectiveBudgetScope,
    /// Tool-scoped host configuration snapshot.
    /// 工具作用域下的宿主配置快照。
    pub tool_config: Value,
}

impl LuaInvocationContext {
    /// Construct one invocation context and normalize non-object JSON payloads into empty objects.
    /// 构造一次调用上下文，并把非对象类型的 JSON 载荷归一化为空对象。
    pub fn new(
        request_context: Option<RuntimeRequestContext>,
        client_budget: Value,
        tool_config: Value,
    ) -> Self {
        Self {
            request_context,
            client_budget: normalize_context_object(client_budget),
            tool_config: normalize_context_object(tool_config),
        }
    }

    /// Return an empty invocation context with stable empty-object payloads.
    /// 返回一个空调用上下文，并使用稳定的空对象载荷。
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Normalize one host context payload so the runtime always sees an object.
/// 归一化单个宿主上下文载荷，确保运行时始终看到对象结构。
fn normalize_context_object(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        _ => Value::Object(Map::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::{LuaRuntimeCapabilityOptions, LuaRuntimeHostOptions};
    use serde_json::json;
    use std::path::PathBuf;

    /// Verify capability payloads now require an explicit managed io compatibility flag.
    /// 验证 capability 载荷现在必须显式提供 managed io 兼容标记。
    #[test]
    fn capability_options_require_explicit_managed_io_compat_flag() {
        let error = serde_json::from_value::<LuaRuntimeCapabilityOptions>(json!({
            "enable_skill_management_bridge": true
        }))
        .expect_err("partial capability options should fail without managed io flag");
        assert!(
            error.to_string().contains("enable_managed_io_compat"),
            "missing-field error should mention enable_managed_io_compat, got: {error}"
        );
    }

    /// Verify one runtime root expands into the fixed LuaSkills directory layout.
    /// 验证单个运行时根目录会展开为固定 LuaSkills 目录布局。
    #[test]
    fn runtime_root_expands_fixed_layout() {
        let runtime_root = PathBuf::from("D:/runtime");
        let options = LuaRuntimeHostOptions::with_runtime_root(runtime_root.clone());

        assert_eq!(options.temp_dir, Some(runtime_root.join("temp")));
        assert_eq!(options.resources_dir, Some(runtime_root.join("resources")));
        assert_eq!(
            options.lua_packages_dir,
            Some(runtime_root.join("lua_packages"))
        );
        assert_eq!(
            options.host_provided_tool_root,
            Some(runtime_root.join("bin"))
        );
        assert_eq!(
            options.host_provided_lua_root,
            Some(runtime_root.join("lua_packages"))
        );
        assert_eq!(
            options.host_provided_ffi_root,
            Some(runtime_root.join("libs"))
        );
        assert_eq!(
            options.system_lua_lib_dir,
            Some(runtime_root.join("system_lua_lib"))
        );
        assert_eq!(
            options.download_cache_root,
            Some(runtime_root.join("temp").join("downloads"))
        );
        assert_eq!(
            options.skill_config_file_path,
            Some(runtime_root.join("config").join("skill_config.json"))
        );
        assert_eq!(options.dependency_dir_name, "dependencies");
        assert_eq!(options.state_dir_name, "state");
        assert_eq!(options.database_dir_name, "databases");
    }
}
