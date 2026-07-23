use std::ffi::{c_char, c_void};

/// Plain C ABI engine pool config used by standard non-JSON FFI calls.
/// 标准非 JSON FFI 调用使用的原生 C ABI 引擎池配置。
#[repr(C)]
pub struct FfiLuaVmPoolConfig {
    /// Minimum number of warm Lua VMs kept by the pool.
    /// 池内保持预热的最小 Lua VM 数量。
    pub min_size: usize,
    /// Maximum number of Lua VMs allowed in the pool.
    /// 池内允许存在的最大 Lua VM 数量。
    pub max_size: usize,
    /// Idle TTL in seconds applied to surplus Lua VMs.
    /// 应用于多余 Lua VM 的空闲过期秒数。
    pub idle_ttl_secs: u64,
}

/// Plain C ABI tool-cache config used by standard non-JSON FFI calls.
/// 标准非 JSON FFI 调用使用的原生 C ABI 工具缓存配置。
#[repr(C)]
pub struct FfiToolCacheConfig {
    /// Maximum number of shared tool-cache entries.
    /// 共享工具缓存允许存在的最大条目数。
    pub max_entries: usize,
    /// Default TTL in seconds applied when callers omit TTL.
    /// 当调用方省略 TTL 时应用的默认秒数。
    pub default_ttl_secs: u64,
    /// Maximum TTL in seconds accepted by the cache.
    /// 缓存允许接受的最大 TTL 秒数。
    pub max_ttl_secs: u64,
}

/// Borrowed byte-buffer view used by the refactored FFI callback surface.
/// 重构后 FFI 回调接口面使用的借用字节缓冲视图。
#[repr(C)]
pub struct FfiBorrowedBuffer {
    /// Borrowed pointer valid only for the duration of the current callback.
    /// 仅在当前回调执行期间有效的借用指针。
    pub ptr: *const u8,
    /// Number of readable bytes starting at `ptr`.
    /// 从 `ptr` 开始可读的字节数。
    pub len: usize,
}

/// Owned byte-buffer container transferred across the FFI boundary.
/// 跨 FFI 边界传递的拥有型字节缓冲容器。
#[repr(C)]
pub struct FfiOwnedBuffer {
    /// Owned heap pointer allocated by `luaskills` helper functions.
    /// 由 `luaskills` 辅助函数分配的拥有型堆指针。
    pub ptr: *mut u8,
    /// Number of owned bytes starting at `ptr`.
    /// 从 `ptr` 开始拥有的字节数。
    pub len: usize,
}

/// Plain C ABI host options used by standard non-JSON engine creation.
/// 标准非 JSON 引擎创建使用的原生 C ABI 宿主选项。
#[repr(C)]
pub struct FfiLuaRuntimeHostOptions {
    /// Optional temporary directory path.
    /// 可选临时目录路径。
    pub temp_dir: *const c_char,
    /// Optional resources directory path.
    /// 可选资源目录路径。
    pub resources_dir: *const c_char,
    /// Optional lua_packages directory path.
    /// 可选 lua_packages 目录路径。
    pub lua_packages_dir: *const c_char,
    /// Optional host-provided tools root path.
    /// 可选宿主提供工具根目录路径。
    pub host_provided_tool_root: *const c_char,
    /// Optional host-provided Lua packages root path.
    /// 可选宿主提供 Lua 包根目录路径。
    pub host_provided_lua_root: *const c_char,
    /// Optional host-provided FFI root path.
    /// 可选宿主提供 FFI 根目录路径。
    pub host_provided_ffi_root: *const c_char,
    /// Optional fixed host-owned `system_lua_lib` directory path.
    /// 可选固定宿主自有 `system_lua_lib` 目录路径。
    pub system_lua_lib_dir: *const c_char,
    /// Optional download-cache root path.
    /// 可选下载缓存根目录路径。
    pub download_cache_root: *const c_char,
    /// Required dependency sibling directory name.
    /// 必填的依赖兄弟目录名称。
    pub dependency_dir_name: *const c_char,
    /// Required state sibling directory name.
    /// 必填的状态兄弟目录名称。
    pub state_dir_name: *const c_char,
    /// Required database sibling directory name.
    /// 必填的数据库兄弟目录名称。
    pub database_dir_name: *const c_char,
    /// Optional explicit user-level skill configuration root.
    /// 可选的显式用户级技能配置根目录。
    pub skill_config_root: *const c_char,
    /// Optional configuration lock timeout in milliseconds, where zero selects the default.
    /// 可选的配置锁超时毫秒数，其中零表示选择默认值。
    pub skill_config_lock_timeout_ms: u64,
    /// Optional configuration watcher debounce in milliseconds, where zero selects the default.
    /// 可选的配置监听防抖毫秒数，其中零表示选择默认值。
    pub skill_config_watch_debounce_ms: u64,
    /// Whether the runtime may perform network downloads.
    /// 运行时是否允许执行网络下载。
    pub allow_network_download: u8,
    /// Optional GitHub site base URL.
    /// 可选 GitHub 站点基址。
    pub github_base_url: *const c_char,
    /// Optional GitHub API base URL.
    /// 可选 GitHub API 基址。
    pub github_api_base_url: *const c_char,
    /// Optional official LuaSkills Hub base URL.
    /// 可选官方 LuaSkills Hub 基址。
    pub official_skill_hub_base_url: *const c_char,
    /// Whether trusted system operations may install from private URL manifests.
    /// 可信 system 操作是否允许从私有 URL manifest 安装。
    pub enable_private_url_skill_install: u8,
    /// Host-controlled private skill source URL allowlist.
    /// 宿主管控的私有技能来源 URL 允许列表。
    pub private_skill_source_allowlist: *const *const c_char,
    /// Number of private skill source allowlist entries.
    /// 私有技能来源允许列表条目数量。
    pub private_skill_source_allowlist_len: usize,
    /// Optional host SQLite dynamic library path.
    /// 可选宿主 SQLite 动态库路径。
    pub sqlite_library_path: *const c_char,
    /// SQLite provider mode integer where `0=dynamic_library`, `1=host_callback`, and `2=space_controller`.
    /// SQLite provider 模式整数，其中 `0=dynamic_library`、`1=host_callback`、`2=space_controller`。
    pub sqlite_provider_mode: i32,
    /// SQLite callback mode integer where `0=standard` and `1=json`.
    /// SQLite 回调模式整数，其中 `0=standard`、`1=json`。
    pub sqlite_callback_mode: i32,
    /// Optional host LanceDB dynamic library path.
    /// 可选宿主 LanceDB 动态库路径。
    pub lancedb_library_path: *const c_char,
    /// LanceDB provider mode integer where `0=dynamic_library` and `1=host_callback` and `2=space_controller`.
    /// LanceDB provider 模式整数，其中 `0=dynamic_library`、`1=host_callback`、`2=space_controller`。
    pub lancedb_provider_mode: i32,
    /// LanceDB callback mode integer where `0=standard` and `1=json`.
    /// LanceDB 回调模式整数，其中 `0=standard`、`1=json`。
    pub lancedb_callback_mode: i32,
    /// Optional shared space-controller endpoint.
    /// 可选共享空间控制器端点。
    pub space_controller_endpoint: *const c_char,
    /// Whether the runtime may auto-spawn the space-controller process.
    /// 运行时是否允许自动唤起空间控制器进程。
    pub space_controller_auto_spawn: u8,
    /// Optional copied local controller executable path.
    /// 可选的本地复制控制器可执行文件路径。
    pub space_controller_executable_path: *const c_char,
    /// Space-controller process mode integer where `0=service` and `1=managed`.
    /// 空间控制器进程模式整数，其中 `0=service`、`1=managed`。
    pub space_controller_process_mode: i32,
    /// Optional tool-cache config pointer.
    /// 可选工具缓存配置指针。
    pub cache_config: *const FfiToolCacheConfig,
    /// Optional dedicated isolated runlua VM pool config.
    /// 可选的隔离 runlua 虚拟机独立池配置。
    pub runlua_pool_config: *const FfiLuaVmPoolConfig,
    /// Reserved public entry names.
    /// 保留公开入口名称数组。
    pub reserved_entry_names: *const *const c_char,
    /// Number of reserved public entry names.
    /// 保留公开入口名称数组长度。
    pub reserved_entry_names_len: usize,
    /// Host-forced ignored skill identifiers.
    /// 宿主强制忽略的技能标识符数组。
    pub ignored_skill_ids: *const *const c_char,
    /// Number of host-forced ignored skill identifiers.
    /// 宿主强制忽略的技能标识符数组长度。
    pub ignored_skill_ids_len: usize,
    /// Whether Lua may use `vulcan.runtime.skills.*` management bridges.
    /// Lua 是否允许使用 `vulcan.runtime.skills.*` 管理桥接。
    pub enable_skill_management_bridge: u8,
    /// Optional default text encoding label for managed IO and process APIs.
    /// 托管 IO 与进程 API 使用的可选默认文本编码标签。
    pub default_text_encoding: *const c_char,
    /// Whether luaexec and runtime sessions must keep Lua's native `io` table.
    /// luaexec 与持久运行时会话是否必须保留 Lua 原生 `io` 表。
    pub disable_managed_io_compat: u8,
}

/// Plain C ABI v2 host options used by standard non-JSON engine creation with runtime_root.
/// 支持 runtime_root 的标准非 JSON 引擎创建使用的原生 C ABI v2 宿主选项。
#[repr(C)]
pub struct FfiLuaRuntimeHostOptionsV2 {
    /// Stable v1 host options kept byte-for-byte compatible with the original standard ABI.
    /// 与原始标准 ABI 保持逐字节兼容的稳定 v1 宿主选项。
    pub base: FfiLuaRuntimeHostOptions,
    /// Optional canonical runtime root used to derive the fixed LuaSkills layout.
    /// 可选规范运行时根目录，用于推导固定 LuaSkills 布局。
    pub runtime_root: *const c_char,
}

/// Plain C ABI managed Python/Node resource policy used by v3 engine creation.
/// 标准 C ABI v3 引擎创建使用的受管 Python/Node 资源策略。
#[repr(C)]
pub struct FfiLuaRuntimeManagedRuntimeConfig {
    /// Maximum live workers for one exact environment and package-owner pool key.
    /// 单个精确环境与包所有者池键允许的最大活动 Worker 数量。
    pub worker_pool_max_size_per_environment: usize,
    /// Idle seconds after which an unused worker may be retired.
    /// 未使用 Worker 可被回收前的空闲秒数。
    pub worker_idle_ttl_secs: u64,
    /// Maximum launching or live persistent sessions retained by one engine.
    /// 单个引擎允许保留的启动中或活动持久会话最大数量。
    pub persistent_session_limit_per_engine: usize,
    /// Default retained bytes for each persistent-session stdout or stderr stream.
    /// 每个持久会话 stdout 或 stderr 流默认保留的字节数。
    pub persistent_session_default_buffer_limit_bytes_per_stream: usize,
    /// Whether `invoke_default_timeout_ms` contains one configured positive timeout.
    /// `invoke_default_timeout_ms` 是否包含一个已配置的正数超时。
    pub has_invoke_default_timeout_ms: u8,
    /// Default invoke timeout in milliseconds when the matching presence flag is one.
    /// 对应存在标记为一时使用的默认 invoke 超时毫秒数。
    pub invoke_default_timeout_ms: u64,
}

/// Plain C ABI v3 host options adding independent managed roots and an optional B3-B7 policy.
/// 新增独立受管根与可选 B3-B7 策略的标准非 JSON C ABI v3 宿主选项。
#[repr(C)]
pub struct FfiLuaRuntimeHostOptionsV3 {
    /// Stable v2 host options kept byte-for-byte compatible with the published v2 ABI.
    /// 与已发布 v2 ABI 保持逐字节兼容的稳定 v2 宿主选项。
    pub base: FfiLuaRuntimeHostOptionsV2,
    /// Optional existing absolute root containing managed Python, Node, uv, and pnpm distributions.
    /// 包含受管 Python、Node、uv 与 pnpm 发行包的可选现有绝对根目录。
    pub managed_runtime_distribution_root: *const c_char,
    /// Optional absolute writable root containing reusable managed Python and Node environments.
    /// 包含可复用受管 Python 与 Node 环境的可选绝对可写根目录。
    pub managed_runtime_environment_root: *const c_char,
    /// Optional managed Worker/session policy; null preserves all stable defaults.
    /// 可选的受管 Worker/会话策略；空指针保留全部稳定默认值。
    pub managed_runtime_config: *const FfiLuaRuntimeManagedRuntimeConfig,
}

/// C ABI JSON provider callback used by non-Rust hosts to bridge database requests.
/// 供非 Rust 宿主桥接数据库请求使用的 C ABI JSON provider 回调。
pub type FfiJsonProviderCallback = unsafe extern "C" fn(
    request_json: FfiBorrowedBuffer,
    user_data: *mut c_void,
    response_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32;

/// C ABI SQLite provider callback used by standard host integration.
/// 标准宿主集成使用的 C ABI SQLite provider 回调。
pub type FfiSqliteProviderCallback = unsafe extern "C" fn(
    request: *const FfiSqliteProviderRequest,
    user_data: *mut c_void,
    response_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32;

/// C ABI LanceDB provider callback used by standard host integration.
/// 标准宿主集成使用的 C ABI LanceDB provider 回调。
pub type FfiLanceDbProviderCallback = unsafe extern "C" fn(
    request: *const FfiLanceDbProviderRequest,
    user_data: *mut c_void,
    meta_json_out: *mut FfiOwnedBuffer,
    data_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32;

/// C ABI edge-triggered wake callback for engine-level managed-session events.
/// 用于引擎级受管会话事件的 C ABI 边沿触发唤醒回调。
///
/// `engine_id` identifies the event source and `user_data` is the opaque host registration value.
/// `engine_id` 标识事件来源，`user_data` 是不透明的宿主注册值。
///
/// `error_out` may receive one LuaSkills-owned diagnostic buffer when the callback returns failure.
/// 回调返回失败时，`error_out` 可以接收一段 LuaSkills 所有的诊断缓冲。
///
/// The callback and `user_data` must be safe to use from arbitrary background threads.
/// 回调与 `user_data` 必须能够从任意后台线程安全使用。
///
/// Return zero after scheduling host work, or nonzero after filling `error_out`.
/// 成功调度宿主工作后返回零；填充 `error_out` 后返回非零。
pub type FfiManagedSessionWakeCallback = unsafe extern "C" fn(
    engine_id: u64,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32;

/// Plain C ABI engine options used by standard non-JSON engine creation.
/// 标准非 JSON 引擎创建使用的原生 C ABI 引擎选项。
#[repr(C)]
pub struct FfiLuaEngineOptions {
    /// Pool config applied to the runtime engine.
    /// 应用于运行时引擎的池配置。
    pub pool: FfiLuaVmPoolConfig,
    /// Host options applied to the runtime engine.
    /// 应用于运行时引擎的宿主选项。
    pub host: FfiLuaRuntimeHostOptions,
}

/// Plain C ABI v2 engine options used by standard non-JSON engine creation with runtime_root.
/// 支持 runtime_root 的标准非 JSON 引擎创建使用的原生 C ABI v2 引擎选项。
#[repr(C)]
pub struct FfiLuaEngineOptionsV2 {
    /// Pool config applied to the runtime engine.
    /// 应用于运行时引擎的池配置。
    pub pool: FfiLuaVmPoolConfig,
    /// V2 host options applied to the runtime engine.
    /// 应用于运行时引擎的 v2 宿主选项。
    pub host: FfiLuaRuntimeHostOptionsV2,
}

/// Plain C ABI v3 engine options carrying independent managed roots and an optional B3-B7 policy.
/// 携带独立受管根与可选 B3-B7 策略的标准非 JSON C ABI v3 引擎选项。
#[repr(C)]
pub struct FfiLuaEngineOptionsV3 {
    /// Pool config applied to the runtime engine.
    /// 应用于运行时引擎的池配置。
    pub pool: FfiLuaVmPoolConfig,
    /// V3 host options applied to the runtime engine.
    /// 应用于运行时引擎的 v3 宿主选项。
    pub host: FfiLuaRuntimeHostOptionsV3,
}

/// Plain C ABI skill root used by standard non-JSON lifecycle and load calls.
/// 标准非 JSON 生命周期与加载调用使用的原生 C ABI 技能根结构。
#[repr(C)]
pub struct FfiRuntimeSkillRoot {
    /// Stable root name, limited to ROOT, PROJECT, or USER.
    /// 稳定根名称，仅限 ROOT、PROJECT 或 USER。
    pub name: *const c_char,
    /// Physical skills directory path.
    /// 物理 skills 目录路径。
    pub skills_dir: *const c_char,
}

/// Plain C ABI invocation context used by standard non-JSON call paths.
/// 标准非 JSON 调用路径使用的原生 C ABI 调用上下文。
#[repr(C)]
pub struct FfiLuaInvocationContext {
    /// Optional request context encoded as one borrowed JSON byte buffer.
    /// 以借用 JSON 字节缓冲编码的可选请求上下文。
    pub request_context_json: FfiBorrowedBuffer,
    /// Optional client budget encoded as one borrowed JSON byte buffer.
    /// 以借用 JSON 字节缓冲编码的可选客户端预算。
    pub client_budget_json: FfiBorrowedBuffer,
    /// Optional tool config encoded as one borrowed JSON byte buffer.
    /// 以借用 JSON 字节缓冲编码的可选工具配置。
    pub tool_config_json: FfiBorrowedBuffer,
}

/// Plain C ABI managed install request used by standard lifecycle calls.
/// 标准生命周期调用使用的原生 C ABI 受管安装请求。
#[repr(C)]
pub struct FfiSkillInstallRequest {
    /// Optional target skill id.
    /// 可选目标技能标识符。
    pub skill_id: *const c_char,
    /// Optional source locator.
    /// 可选来源定位值。
    pub source: *const c_char,
    /// Managed source type encoded as one integer.
    /// 以整数编码的受管来源类型。
    pub source_type: i32,
}

/// Plain C ABI uninstall options used by standard lifecycle calls.
/// 标准生命周期调用使用的原生 C ABI 卸载选项。
#[repr(C)]
pub struct FfiSkillUninstallOptions {
    /// Remove SQLite data when non-zero.
    /// 非零时删除 SQLite 数据。
    pub remove_sqlite: u8,
    /// Remove LanceDB data when non-zero.
    /// 非零时删除 LanceDB 数据。
    pub remove_lancedb: u8,
}

/// Plain C ABI database binding context forwarded into one host-managed database provider.
/// 转发给宿主管理数据库 provider 的原生 C ABI 数据库绑定上下文。
#[repr(C)]
pub struct FfiRuntimeDatabaseBindingContext {
    /// Stable host-provided space label such as ROOT, PROJECT, or USER.
    /// 由宿主提供的稳定空间标签，例如 ROOT、PROJECT 或 USER。
    pub space_label: *const c_char,
    /// Stable skill identifier owning the current binding.
    /// 拥有当前绑定的稳定技能标识符。
    pub skill_id: *const c_char,
    /// Stable database binding tag composed from space label and skill id.
    /// 由空间标签与技能标识符组合得到的稳定数据库绑定标签。
    pub binding_tag: *const c_char,
    /// Effective physical skill root label that resolved the current skill instance.
    /// 解析出当前技能实例的生效物理技能根标签。
    pub root_name: *const c_char,
    /// Physical space root directory path.
    /// 物理空间根目录路径。
    pub space_root: *const c_char,
    /// Physical skill directory path.
    /// 物理技能目录路径。
    pub skill_dir: *const c_char,
    /// Basename of the physical skill directory.
    /// 物理技能目录名称。
    pub skill_dir_name: *const c_char,
    /// Database kind integer where `0=sqlite` and `1=lancedb`.
    /// 数据库类型整数，其中 `0=sqlite`、`1=lancedb`。
    pub database_kind: i32,
    /// Library-computed default embedded database path for diagnostics and fallback.
    /// 由库计算出的默认内嵌数据库路径，用于诊断和回退。
    pub default_database_path: *const c_char,
}

/// Plain C ABI SQLite provider request delivered to one host callback.
/// 传递给宿主回调的原生 C ABI SQLite provider 请求。
#[repr(C)]
pub struct FfiSqliteProviderRequest {
    /// Requested action integer defined by `FFI_SQLITE_PROVIDER_ACTION_*`.
    /// 由 `FFI_SQLITE_PROVIDER_ACTION_*` 定义的请求动作整数。
    pub action: i32,
    /// Stable binding context of the current skill-scoped database.
    /// 当前 skill 级数据库的稳定绑定上下文。
    pub binding: FfiRuntimeDatabaseBindingContext,
    /// Action-specific input payload encoded as one borrowed JSON byte buffer.
    /// 以借用 JSON 字节缓冲编码的动作专属输入载荷。
    pub input_json: FfiBorrowedBuffer,
}

/// Plain C ABI LanceDB provider request delivered to one host callback.
/// 传递给宿主回调的原生 C ABI LanceDB provider 请求。
#[repr(C)]
pub struct FfiLanceDbProviderRequest {
    /// Requested action integer defined by `FFI_LANCEDB_PROVIDER_ACTION_*`.
    /// 由 `FFI_LANCEDB_PROVIDER_ACTION_*` 定义的请求动作整数。
    pub action: i32,
    /// Stable binding context of the current skill-scoped database.
    /// 当前 skill 级数据库的稳定绑定上下文。
    pub binding: FfiRuntimeDatabaseBindingContext,
    /// Action-specific input payload encoded as one borrowed JSON byte buffer.
    /// 以借用 JSON 字节缓冲编码的动作专属输入载荷。
    pub input_json: FfiBorrowedBuffer,
}

/// Plain C ABI string-array result.
/// 原生 C ABI 字符串数组结果。
#[repr(C)]
pub struct FfiStringArray {
    /// Owned UTF-8 string buffers.
    /// 拥有所有权的 UTF-8 字符串缓冲数组。
    pub items: *mut FfiOwnedBuffer,
    /// Number of string items.
    /// 字符串条目数量。
    pub len: usize,
}

/// Plain C ABI entry parameter descriptor.
/// 原生 C ABI 入口参数描述结构。
#[repr(C)]
pub struct FfiRuntimeEntryParameterDescriptor {
    /// Stable parameter name.
    /// 稳定参数名。
    pub name: FfiOwnedBuffer,
    /// Runtime parameter type string.
    /// 运行时参数类型字符串。
    pub param_type: FfiOwnedBuffer,
    /// Human-readable parameter description.
    /// 人类可读参数说明。
    pub description: FfiOwnedBuffer,
    /// Whether the parameter is required.
    /// 当前参数是否必填。
    pub required: u8,
}

/// Plain C ABI entry descriptor.
/// 原生 C ABI 入口描述结构。
#[repr(C)]
pub struct FfiRuntimeEntryDescriptor {
    /// Canonical runtime entry name.
    /// canonical 运行时入口名称。
    pub canonical_name: FfiOwnedBuffer,
    /// Owning skill id.
    /// 所属技能标识符。
    pub skill_id: FfiOwnedBuffer,
    /// Local entry name declared by the skill.
    /// 技能声明的局部入口名。
    pub local_name: FfiOwnedBuffer,
    /// Effective root name.
    /// 生效根名称。
    pub root_name: FfiOwnedBuffer,
    /// Effective physical skill directory.
    /// 生效物理技能目录。
    pub skill_dir: FfiOwnedBuffer,
    /// Human-readable entry description.
    /// 人类可读入口描述。
    pub description: FfiOwnedBuffer,
    /// Final AI-facing input schema encoded as UTF-8 JSON text.
    /// 编码为 UTF-8 JSON 文本的最终面向 AI 输入 schema。
    pub input_schema_json: FfiOwnedBuffer,
    /// Parameter descriptor array.
    /// 参数描述数组。
    pub parameters: *mut FfiRuntimeEntryParameterDescriptor,
    /// Parameter descriptor count.
    /// 参数描述数量。
    pub parameters_len: usize,
}

/// Plain C ABI entry descriptor list.
/// 原生 C ABI 入口描述列表。
#[repr(C)]
pub struct FfiRuntimeEntryDescriptorList {
    /// Entry descriptor items.
    /// 入口描述条目。
    pub items: *mut FfiRuntimeEntryDescriptor,
    /// Entry descriptor count.
    /// 入口描述数量。
    pub len: usize,
}

/// Plain C ABI help node descriptor.
/// 原生 C ABI 帮助节点描述结构。
#[repr(C)]
pub struct FfiRuntimeHelpNodeDescriptor {
    /// Help flow name.
    /// 帮助流程名。
    pub flow_name: FfiOwnedBuffer,
    /// Human-readable node description.
    /// 人类可读节点描述。
    pub description: FfiOwnedBuffer,
    /// Related canonical runtime entry names.
    /// 关联的 canonical 运行时入口名称。
    pub related_entries: *mut FfiOwnedBuffer,
    /// Number of related canonical runtime entry names.
    /// 关联 canonical 运行时入口名称数量。
    pub related_entries_len: usize,
    /// Whether the node is the main help node.
    /// 当前节点是否为主帮助节点。
    pub is_main: u8,
}

/// Plain C ABI help tree descriptor.
/// 原生 C ABI 帮助树描述结构。
#[repr(C)]
pub struct FfiRuntimeSkillHelpDescriptor {
    /// Stable skill id.
    /// 稳定技能标识符。
    pub skill_id: FfiOwnedBuffer,
    /// Human-readable skill name.
    /// 人类可读技能名称。
    pub skill_name: FfiOwnedBuffer,
    /// Semantic skill version.
    /// 语义化技能版本。
    pub skill_version: FfiOwnedBuffer,
    /// Effective root name.
    /// 生效根名称。
    pub root_name: FfiOwnedBuffer,
    /// Effective physical skill directory.
    /// 生效物理技能目录。
    pub skill_dir: FfiOwnedBuffer,
    /// Main help node descriptor.
    /// 主帮助节点描述。
    pub main: FfiRuntimeHelpNodeDescriptor,
    /// Flow help node descriptor array.
    /// 流程帮助节点描述数组。
    pub flows: *mut FfiRuntimeHelpNodeDescriptor,
    /// Flow help node descriptor count.
    /// 流程帮助节点描述数量。
    pub flows_len: usize,
}

/// Plain C ABI help tree descriptor list.
/// 原生 C ABI 帮助树描述列表。
#[repr(C)]
pub struct FfiRuntimeSkillHelpDescriptorList {
    /// Help descriptor items.
    /// 帮助描述条目。
    pub items: *mut FfiRuntimeSkillHelpDescriptor,
    /// Help descriptor count.
    /// 帮助描述数量。
    pub len: usize,
}

/// Plain C ABI help detail descriptor.
/// 原生 C ABI 帮助详情描述结构。
#[repr(C)]
pub struct FfiRuntimeHelpDetail {
    /// Stable skill id.
    /// 稳定技能标识符。
    pub skill_id: FfiOwnedBuffer,
    /// Human-readable skill name.
    /// 人类可读技能名称。
    pub skill_name: FfiOwnedBuffer,
    /// Semantic skill version.
    /// 语义化技能版本。
    pub skill_version: FfiOwnedBuffer,
    /// Effective root name.
    /// 生效根名称。
    pub root_name: FfiOwnedBuffer,
    /// Effective physical skill directory.
    /// 生效物理技能目录。
    pub skill_dir: FfiOwnedBuffer,
    /// Flow name.
    /// 流程名称。
    pub flow_name: FfiOwnedBuffer,
    /// Human-readable description.
    /// 人类可读描述。
    pub description: FfiOwnedBuffer,
    /// Related canonical runtime entries.
    /// 关联的 canonical 运行时入口。
    pub related_entries: *mut FfiOwnedBuffer,
    /// Number of related canonical runtime entries.
    /// 关联 canonical 运行时入口数量。
    pub related_entries_len: usize,
    /// Whether the node is the main help node.
    /// 当前节点是否为主帮助节点。
    pub is_main: u8,
    /// Structured content type.
    /// 结构化内容类型。
    pub content_type: FfiOwnedBuffer,
    /// Final rendered help content.
    /// 最终渲染出的帮助内容。
    pub content: FfiOwnedBuffer,
}

/// Plain C ABI invocation result.
/// 原生 C ABI 调用结果结构。
#[repr(C)]
pub struct FfiRuntimeHostResult {
    /// Stable host-result kind.
    /// 稳定宿主结果类型。
    pub kind: FfiOwnedBuffer,
    /// Serialized host-result payload JSON.
    /// 序列化后的宿主结果载荷 JSON。
    pub payload_json: FfiOwnedBuffer,
    /// Serialized host-result payload byte size.
    /// 序列化后宿主结果载荷字节数。
    pub payload_bytes: usize,
}

/// Plain C ABI invocation result.
/// 原生 C ABI 调用结果结构。
#[repr(C)]
pub struct FfiRuntimeInvocationResult {
    /// Tool body content.
    /// 工具正文内容。
    pub content: FfiOwnedBuffer,
    /// Overflow mode encoded as 0 none, 1 truncate, 2 page.
    /// 以 0 无、1 截断、2 分页编码的超限模式。
    pub overflow_mode: i32,
    /// Optional template hint.
    /// 可选模板提示名。
    pub template_hint: FfiOwnedBuffer,
    /// Content byte count.
    /// 内容字节数。
    pub content_bytes: usize,
    /// Content line count.
    /// 内容行数。
    pub content_lines: usize,
    /// Optional structured host-side result pointer.
    /// 可选结构化宿主结果指针。
    pub host_result: *mut FfiRuntimeHostResult,
}

/// Plain C ABI install or update result.
/// 原生 C ABI 安装或更新结果结构。
#[repr(C)]
pub struct FfiSkillApplyResult {
    /// Stable skill id.
    /// 稳定技能标识符。
    pub skill_id: FfiOwnedBuffer,
    /// High-level result status.
    /// 高层结果状态。
    pub status: FfiOwnedBuffer,
    /// Human-readable message.
    /// 人类可读消息。
    pub message: FfiOwnedBuffer,
    /// Optional semantic version.
    /// 可选语义化版本。
    pub version: FfiOwnedBuffer,
    /// Optional source type encoded as integer, where -1 means absent.
    /// 以整数编码的可选来源类型，-1 表示不存在。
    pub source_type: i32,
    /// Optional source locator.
    /// 可选来源定位值。
    pub source_locator: FfiOwnedBuffer,
}

/// Plain C ABI uninstall result.
/// 原生 C ABI 卸载结果结构。
#[repr(C)]
pub struct FfiSkillUninstallResult {
    /// Stable skill id.
    /// 稳定技能标识符。
    pub skill_id: FfiOwnedBuffer,
    /// Whether the skill directory was removed.
    /// 技能目录是否已被删除。
    pub skill_removed: u8,
    /// Whether the SQLite database was removed.
    /// SQLite 数据库是否已被删除。
    pub sqlite_removed: u8,
    /// Whether the LanceDB database was removed.
    /// LanceDB 数据库是否已被删除。
    pub lancedb_removed: u8,
    /// Whether the SQLite database was intentionally retained.
    /// SQLite 数据库是否被有意保留。
    pub sqlite_retained: u8,
    /// Whether the LanceDB database was intentionally retained.
    /// LanceDB 数据库是否被有意保留。
    pub lancedb_retained: u8,
    /// Human-readable message.
    /// 人类可读消息。
    pub message: FfiOwnedBuffer,
}
