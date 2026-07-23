#ifndef LUASKILLS_FFI_H
#define LUASKILLS_FFI_H

#include <stddef.h>
#include <stdint.h>

/*
Stable standard C ABI exported by luaskills.
luaskills 导出的稳定标准 C ABI 接口面。
*/

/*
 Beta integration contract for v0.1.x:
- This header is the low-level standard ABI for controlled host integrations.
- Public high-level JSON FFI declarations are provided by luaskills_json_ffi.h.
- Returned memory must be released only with the matching luaskills free function.
- Global callback-based providers and services must be registered before engine creation.
- The managed-session wake callback is per-engine and must be registered after engine creation.
- Callbacks must not unwind across the C ABI boundary.
- Same-thread reentry into the same engine is not supported.
- Skills are treated as trusted code by default; this ABI does not promise sandbox isolation.
v0.1.x beta 集成契约：
- 当前头文件是面向受控宿主集成的低层标准 ABI。
- 公共高层 JSON FFI 声明位于 luaskills_json_ffi.h。
- 所有返回内存都只能使用匹配的 luaskills 释放函数处理。
- 全局 callback 型 provider 与服务必须先注册，再创建 engine。
- 受管会话唤醒 callback 按 engine 注册，必须在 engine 创建后设置。
- callback 不允许把异常跨越 C ABI 边界传播。
- 不支持同一线程内对同一 engine 的重入调用。
- 当前默认将 skill 视为受信代码，本 ABI 不承诺沙箱隔离。
*/

#ifdef __cplusplus
extern "C" {
#endif

/*
Skill-management authority value for host-owned system operations and visibility queries.
宿主侧 system 操作与可见性查询使用的技能管理权限值。
*/
#define LUASKILLS_SKILL_AUTHORITY_SYSTEM 0

/*
Delegated-tool authority value for ordinary-tool system wrappers and visibility queries.
普通 tools 的 system 封装与可见性查询使用的委托工具权限值。
*/
#define LUASKILLS_SKILL_AUTHORITY_DELEGATED_TOOL 1

typedef struct FfiLuaVmPoolConfig {
    size_t min_size;
    size_t max_size;
    uint64_t idle_ttl_secs;
} FfiLuaVmPoolConfig;

typedef struct FfiToolCacheConfig {
    size_t max_entries;
    uint64_t default_ttl_secs;
    uint64_t max_ttl_secs;
} FfiToolCacheConfig;

typedef struct FfiBorrowedBuffer {
    const uint8_t *ptr;
    size_t len;
} FfiBorrowedBuffer;

typedef struct FfiOwnedBuffer {
    uint8_t *ptr;
    size_t len;
} FfiOwnedBuffer;

typedef struct FfiLuaRuntimeHostOptions {
    const char *temp_dir;
    const char *resources_dir;
    const char *lua_packages_dir;
    const char *host_provided_tool_root;
    const char *host_provided_lua_root;
    const char *host_provided_ffi_root;
    /*
    Optional fixed host-owned `system_lua_lib` directory path.
    可选固定宿主自有 `system_lua_lib` 目录路径。
    */
    const char *system_lua_lib_dir;
    const char *download_cache_root;
    const char *dependency_dir_name;
    const char *state_dir_name;
    const char *database_dir_name;
    /*
    Optional unified skill config file path owned by the host.
    由宿主拥有的可选统一技能配置文件路径。
    */
  const char *skill_config_root;
  uint64_t skill_config_lock_timeout_ms;
  uint64_t skill_config_watch_debounce_ms;
    uint8_t allow_network_download;
    const char *github_base_url;
    const char *github_api_base_url;
    /*
    Optional official LuaSkills Hub base URL used by managed Hub installs.
    受管 Hub 安装使用的可选官方 LuaSkills Hub 基址。
    */
    const char *official_skill_hub_base_url;
    /*
    Whether trusted system operations may install from private URL manifests.
    可信 system 操作是否允许从私有 URL manifest 安装。
    */
    uint8_t enable_private_url_skill_install;
    /*
    Host-controlled URL prefixes allowed for private skill manifests.
    宿主管控的私有技能 manifest 允许 URL 前缀。
    */
    const char **private_skill_source_allowlist;
    /*
    Number of private skill manifest allowlist entries.
    私有技能 manifest 允许前缀数量。
    */
    size_t private_skill_source_allowlist_len;
    const char *sqlite_library_path;
    /*
    SQLite provider mode where 0=dynamic_library, 1=host_callback, and 2=space_controller.
    SQLite provider 模式，其中 0=dynamic_library、1=host_callback、2=space_controller。
    */
    int32_t sqlite_provider_mode;
    /*
    SQLite callback mode used only when sqlite_provider_mode=host_callback.
    sqlite_provider_mode=host_callback 时使用的 SQLite 回调模式。
    */
    int32_t sqlite_callback_mode;
    const char *lancedb_library_path;
    /*
    LanceDB provider mode where 0=dynamic_library, 1=host_callback, and 2=space_controller.
    LanceDB provider 模式，其中 0=dynamic_library、1=host_callback、2=space_controller。
    */
    int32_t lancedb_provider_mode;
    /*
    LanceDB callback mode used only when lancedb_provider_mode=host_callback.
    lancedb_provider_mode=host_callback 时使用的 LanceDB 回调模式。
    */
    int32_t lancedb_callback_mode;
    /*
    Optional shared space-controller endpoint used when one provider mode is space_controller.
    当某个 provider 模式为 space_controller 时使用的可选共享空间控制器端点。
    */
    const char *space_controller_endpoint;
    /*
    Whether the runtime may auto-spawn one space-controller process when the endpoint is unavailable.
    当空间控制器端点不可用时，运行时是否允许自动唤起空间控制器进程。
    */
    uint8_t space_controller_auto_spawn;
    /*
    Optional copied local controller executable path managed by the host.
    由宿主复制并管理的可选本地控制器可执行文件路径。
    */
    const char *space_controller_executable_path;
    /*
    Space-controller process mode where 0=service and 1=managed.
    空间控制器进程模式，其中 0=service、1=managed。
    */
    int32_t space_controller_process_mode;
    const FfiToolCacheConfig *cache_config;
    /*
    Optional dedicated isolated runlua VM pool config.
    可选的隔离 runlua 虚拟机独立池配置。
    */
    const FfiLuaVmPoolConfig *runlua_pool_config;
    const char **reserved_entry_names;
    size_t reserved_entry_names_len;
    /*
    Host-forced skill identifiers skipped before dependency and database setup.
    在依赖与数据库初始化前由宿主强制跳过的技能标识符数组。
    */
    const char **ignored_skill_ids;
    /*
    Number of host-forced ignored skill identifiers.
    宿主强制忽略的技能标识符数组长度。
    */
    size_t ignored_skill_ids_len;
    /*
    Whether Lua may use `vulcan.runtime.skills.*` management bridges.
    Lua 是否允许使用 `vulcan.runtime.skills.*` 管理桥接。
    */
    uint8_t enable_skill_management_bridge;
    /*
    Optional default text encoding label used by managed IO and process APIs.
    托管 IO 与进程 API 使用的可选默认文本编码标签。
    */
    const char *default_text_encoding;
    /*
    Whether luaexec and runtime leases must keep Lua's native `io` table.
    luaexec 与持久运行时租约是否必须保留 Lua 原生 `io` 表。
    */
    uint8_t disable_managed_io_compat;
} FfiLuaRuntimeHostOptions;

typedef struct FfiLuaRuntimeHostOptionsV2 {
    /*
    Stable v1 host options kept byte-for-byte compatible with the original standard ABI.
    与原始标准 ABI 保持逐字节兼容的稳定 v1 宿主选项。
    */
    FfiLuaRuntimeHostOptions base;
    /*
    Optional canonical runtime root used to derive the fixed LuaSkills layout.
    可选规范运行时根目录，用于推导固定 LuaSkills 布局。
    */
    const char *runtime_root;
} FfiLuaRuntimeHostOptionsV2;

typedef struct FfiLuaRuntimeManagedRuntimeConfig {
    /*
    Maximum live workers for one exact environment and package-owner pool key.
    单个精确环境与包所有者池键允许的最大活动 Worker 数量。
    */
    size_t worker_pool_max_size_per_environment;
    /*
    Idle seconds after which an unused worker may be retired.
    未使用 Worker 可被回收前的空闲秒数。
    */
    uint64_t worker_idle_ttl_secs;
    /*
    Maximum launching or live persistent sessions retained by one engine.
    单个引擎允许保留的启动中或活动持久会话最大数量。
    */
    size_t persistent_session_limit_per_engine;
    /*
    Default retained bytes for each persistent-session stdout or stderr stream.
    每个持久会话 stdout 或 stderr 流默认保留的字节数。
    */
    size_t persistent_session_default_buffer_limit_bytes_per_stream;
    /*
    Whether invoke_default_timeout_ms contains one configured positive timeout.
    invoke_default_timeout_ms 是否包含一个已配置的正数超时。
    */
    uint8_t has_invoke_default_timeout_ms;
    /*
    Default invoke timeout in milliseconds when the matching presence flag is one.
    对应存在标记为一时使用的默认 invoke 超时毫秒数。
    */
    uint64_t invoke_default_timeout_ms;
} FfiLuaRuntimeManagedRuntimeConfig;

typedef struct FfiLuaRuntimeHostOptionsV3 {
    /*
    Stable v2 host options kept byte-for-byte compatible with the published v2 ABI.
    与已发布 v2 ABI 保持逐字节兼容的稳定 v2 宿主选项。
    */
    FfiLuaRuntimeHostOptionsV2 base;
    /*
    Optional existing absolute root containing managed Python, Node, uv, and pnpm distributions.
    包含受管 Python、Node、uv 与 pnpm 发行包的可选现有绝对根目录。
    */
    const char *managed_runtime_distribution_root;
    /*
    Optional absolute writable root containing reusable managed Python and Node environments.
    包含可复用受管 Python 与 Node 环境的可选绝对可写根目录。
    */
    const char *managed_runtime_environment_root;
    /*
    Optional managed Worker/session policy; NULL preserves all stable defaults.
    可选的受管 Worker/会话策略；NULL 保留全部稳定默认值。
    */
    const FfiLuaRuntimeManagedRuntimeConfig *managed_runtime_config;
} FfiLuaRuntimeHostOptionsV3;

typedef struct FfiLuaEngineOptions {
    FfiLuaVmPoolConfig pool;
    FfiLuaRuntimeHostOptions host;
} FfiLuaEngineOptions;

typedef struct FfiLuaEngineOptionsV2 {
    FfiLuaVmPoolConfig pool;
    FfiLuaRuntimeHostOptionsV2 host;
} FfiLuaEngineOptionsV2;

typedef struct FfiLuaEngineOptionsV3 {
    FfiLuaVmPoolConfig pool;
    FfiLuaRuntimeHostOptionsV3 host;
} FfiLuaEngineOptionsV3;

typedef struct FfiRuntimeSkillRoot {
    const char *name;
    const char *skills_dir;
} FfiRuntimeSkillRoot;

typedef struct FfiLuaInvocationContext {
    FfiBorrowedBuffer request_context_json;
    FfiBorrowedBuffer client_budget_json;
    FfiBorrowedBuffer tool_config_json;
} FfiLuaInvocationContext;

/*
Stable source-type integers used by standard install and update requests/results.
标准安装与更新请求及结果使用的稳定来源类型整数。
*/
enum {
    FFI_SOURCE_TYPE_ABSENT = -1,
    FFI_SOURCE_TYPE_GITHUB = 0,
    FFI_SOURCE_TYPE_URL = 1,
    FFI_SOURCE_TYPE_OFFICIAL_HUB = 2,
    FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST = 3
};

enum {
    FFI_PROVIDER_MODE_DYNAMIC_LIBRARY = 0,
    FFI_PROVIDER_MODE_HOST_CALLBACK = 1,
    FFI_PROVIDER_MODE_SPACE_CONTROLLER = 2
};

/*
Stable callback-mode integers used when one provider mode is host_callback.
当 provider 模式为 host_callback 时所使用的稳定回调模式整数。
*/
enum {
    FFI_CALLBACK_MODE_STANDARD = 0,
    FFI_CALLBACK_MODE_JSON = 1
};

/*
Stable process-mode integers used when one provider mode is space_controller.
当 provider 模式为 space_controller 时所使用的稳定进程模式整数。
*/
enum {
    FFI_SPACE_CONTROLLER_PROCESS_MODE_SERVICE = 0,
    FFI_SPACE_CONTROLLER_PROCESS_MODE_MANAGED = 1
};

enum {
    FFI_DATABASE_KIND_SQLITE = 0,
    FFI_DATABASE_KIND_LANCEDB = 1
};

enum {
    FFI_SQLITE_PROVIDER_ACTION_EXECUTE_SCRIPT = 0,
    FFI_SQLITE_PROVIDER_ACTION_EXECUTE_BATCH = 1,
    FFI_SQLITE_PROVIDER_ACTION_QUERY_JSON = 2,
    FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM = 3,
    FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_WAIT_METRICS = 4,
    FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_CHUNK = 5,
    FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_CLOSE = 6,
    FFI_SQLITE_PROVIDER_ACTION_TOKENIZE_TEXT = 7,
    FFI_SQLITE_PROVIDER_ACTION_UPSERT_CUSTOM_WORD = 8,
    FFI_SQLITE_PROVIDER_ACTION_REMOVE_CUSTOM_WORD = 9,
    FFI_SQLITE_PROVIDER_ACTION_LIST_CUSTOM_WORDS = 10,
    FFI_SQLITE_PROVIDER_ACTION_ENSURE_FTS_INDEX = 11,
    FFI_SQLITE_PROVIDER_ACTION_REBUILD_FTS_INDEX = 12,
    FFI_SQLITE_PROVIDER_ACTION_UPSERT_FTS_DOCUMENT = 13,
    FFI_SQLITE_PROVIDER_ACTION_DELETE_FTS_DOCUMENT = 14,
    FFI_SQLITE_PROVIDER_ACTION_SEARCH_FTS = 15
};

enum {
    FFI_LANCEDB_PROVIDER_ACTION_CREATE_TABLE = 0,
    FFI_LANCEDB_PROVIDER_ACTION_VECTOR_UPSERT = 1,
    FFI_LANCEDB_PROVIDER_ACTION_VECTOR_SEARCH = 2,
    FFI_LANCEDB_PROVIDER_ACTION_DELETE = 3,
    FFI_LANCEDB_PROVIDER_ACTION_DROP_TABLE = 4
};

typedef struct FfiSkillInstallRequest {
    const char *skill_id;
    const char *source;
    /* FFI_SOURCE_TYPE_GITHUB, FFI_SOURCE_TYPE_OFFICIAL_HUB, FFI_SOURCE_TYPE_URL, or FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST. */
    /* FFI_SOURCE_TYPE_GITHUB、FFI_SOURCE_TYPE_OFFICIAL_HUB、FFI_SOURCE_TYPE_URL 或 FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST。 */
    int32_t source_type;
} FfiSkillInstallRequest;

typedef struct FfiSkillUninstallOptions {
    uint8_t remove_sqlite;
    uint8_t remove_lancedb;
} FfiSkillUninstallOptions;

typedef struct FfiRuntimeDatabaseBindingContext {
    const char *space_label;
    const char *skill_id;
    const char *binding_tag;
    const char *root_name;
    const char *space_root;
    const char *skill_dir;
    const char *skill_dir_name;
    int32_t database_kind;
    const char *default_database_path;
} FfiRuntimeDatabaseBindingContext;

typedef struct FfiSqliteProviderRequest {
    int32_t action;
    FfiRuntimeDatabaseBindingContext binding;
    FfiBorrowedBuffer input_json;
} FfiSqliteProviderRequest;

typedef struct FfiLanceDbProviderRequest {
    int32_t action;
    FfiRuntimeDatabaseBindingContext binding;
    FfiBorrowedBuffer input_json;
} FfiLanceDbProviderRequest;

typedef struct FfiStringArray {
    FfiOwnedBuffer *items;
    size_t len;
} FfiStringArray;

typedef struct FfiRuntimeEntryParameterDescriptor {
    FfiOwnedBuffer name;
    FfiOwnedBuffer param_type;
    FfiOwnedBuffer description;
    uint8_t required;
} FfiRuntimeEntryParameterDescriptor;

typedef struct FfiRuntimeEntryDescriptor {
    FfiOwnedBuffer canonical_name;
    FfiOwnedBuffer skill_id;
    FfiOwnedBuffer local_name;
    FfiOwnedBuffer root_name;
    FfiOwnedBuffer skill_dir;
    FfiOwnedBuffer description;
    /*
    JSON schema for this entry's input object.
    当前入口输入对象的 JSON schema。
    */
    FfiOwnedBuffer input_schema_json;
    struct FfiRuntimeEntryParameterDescriptor *parameters;
    size_t parameters_len;
} FfiRuntimeEntryDescriptor;

typedef struct FfiRuntimeEntryDescriptorList {
    struct FfiRuntimeEntryDescriptor *items;
    size_t len;
} FfiRuntimeEntryDescriptorList;

typedef struct FfiRuntimeHelpNodeDescriptor {
    FfiOwnedBuffer flow_name;
    FfiOwnedBuffer description;
    FfiOwnedBuffer *related_entries;
    size_t related_entries_len;
    uint8_t is_main;
} FfiRuntimeHelpNodeDescriptor;

typedef struct FfiRuntimeSkillHelpDescriptor {
    FfiOwnedBuffer skill_id;
    FfiOwnedBuffer skill_name;
    FfiOwnedBuffer skill_version;
    FfiOwnedBuffer root_name;
    FfiOwnedBuffer skill_dir;
    struct FfiRuntimeHelpNodeDescriptor main;
    struct FfiRuntimeHelpNodeDescriptor *flows;
    size_t flows_len;
} FfiRuntimeSkillHelpDescriptor;

typedef struct FfiRuntimeSkillHelpDescriptorList {
    struct FfiRuntimeSkillHelpDescriptor *items;
    size_t len;
} FfiRuntimeSkillHelpDescriptorList;

typedef struct FfiRuntimeHelpDetail {
    FfiOwnedBuffer skill_id;
    FfiOwnedBuffer skill_name;
    FfiOwnedBuffer skill_version;
    FfiOwnedBuffer root_name;
    FfiOwnedBuffer skill_dir;
    FfiOwnedBuffer flow_name;
    FfiOwnedBuffer description;
    FfiOwnedBuffer *related_entries;
    size_t related_entries_len;
    uint8_t is_main;
    FfiOwnedBuffer content_type;
    FfiOwnedBuffer content;
} FfiRuntimeHelpDetail;

typedef struct FfiRuntimeHostResult {
    FfiOwnedBuffer kind;
    FfiOwnedBuffer payload_json;
    size_t payload_bytes;
} FfiRuntimeHostResult;

typedef struct FfiRuntimeInvocationResult {
    FfiOwnedBuffer content;
    int32_t overflow_mode;
    FfiOwnedBuffer template_hint;
    size_t content_bytes;
    size_t content_lines;
    FfiRuntimeHostResult *host_result;
} FfiRuntimeInvocationResult;

typedef struct FfiSkillApplyResult {
    FfiOwnedBuffer skill_id;
    FfiOwnedBuffer status;
    FfiOwnedBuffer message;
    FfiOwnedBuffer version;
    /* FFI_SOURCE_TYPE_ABSENT, FFI_SOURCE_TYPE_GITHUB, FFI_SOURCE_TYPE_OFFICIAL_HUB, FFI_SOURCE_TYPE_URL, or FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST. */
    /* FFI_SOURCE_TYPE_ABSENT、FFI_SOURCE_TYPE_GITHUB、FFI_SOURCE_TYPE_OFFICIAL_HUB、FFI_SOURCE_TYPE_URL 或 FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST。 */
    int32_t source_type;
    FfiOwnedBuffer source_locator;
} FfiSkillApplyResult;

typedef struct FfiSkillUninstallResult {
    FfiOwnedBuffer skill_id;
    uint8_t skill_removed;
    uint8_t sqlite_removed;
    uint8_t lancedb_removed;
    uint8_t sqlite_retained;
    uint8_t lancedb_retained;
    FfiOwnedBuffer message;
} FfiSkillUninstallResult;

/*
Standard callbacks must fill outputs with luaskills-owned allocations and must never unwind across the ABI boundary.
标准 callback 必须写入 luaskills 所有的输出内存，且绝不能把异常跨越 ABI 边界传播。
*/
typedef int32_t (*FfiSqliteProviderCallback)(
    const FfiSqliteProviderRequest *request,
    void *user_data,
    FfiOwnedBuffer *response_json_out,
    FfiOwnedBuffer *error_out
);
typedef int32_t (*FfiLanceDbProviderCallback)(
    const FfiLanceDbProviderRequest *request,
    void *user_data,
    FfiOwnedBuffer *meta_json_out,
    FfiOwnedBuffer *data_out,
    FfiOwnedBuffer *error_out
);

/*
Edge-triggered callback that only schedules host work for pending managed-session events.
仅为待处理受管会话事件调度宿主工作的边沿触发回调。
The callback may run on an arbitrary background thread and must not synchronously enter Lua.
该回调可能在任意后台线程运行，且不得同步进入 Lua。
The callback implementation and user_data must be safe to access from any such thread.
回调实现与 user_data 必须能够从任意此类线程安全访问。
Fill error_out with luaskills_ffi_buffer_clone before returning nonzero.
返回非零前必须使用 luaskills_ffi_buffer_clone 填充 error_out。
Nonzero returns are retried asynchronously with bounded exponential backoff while the same queue edge remains pending.
当同一队列边沿仍待处理时，非零返回会通过有界指数退避异步重试。
Registration against an already nonempty queue may invoke one catch-up callback before returning.
针对已非空队列注册时，返回前可能调用一次补偿回调。
*/
typedef int32_t (*FfiManagedSessionWakeCallback)(
    uint64_t engine_id,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Clone one host-owned byte buffer into one luaskills-owned owned-buffer container.
将宿主拥有的字节缓冲克隆为 luaskills 自主管理的拥有型缓冲容器。
*/
int32_t luaskills_ffi_buffer_clone(
    const uint8_t *value,
    size_t len,
    FfiOwnedBuffer *buffer_out,
    FfiOwnedBuffer *error_out
);
/*
Clone one host-owned byte buffer into one luaskills-owned heap buffer for callback returns.
将宿主拥有的字节缓冲克隆为 luaskills 自主管理的堆缓冲，供 callback 返回使用。
*/
uint8_t *luaskills_ffi_bytes_clone(const uint8_t *value, size_t len);
/*
Free one luaskills-owned buffer container created by luaskills_ffi_buffer_clone.
释放由 luaskills_ffi_buffer_clone 创建的 luaskills 自主管理缓冲容器。
*/
void luaskills_ffi_buffer_free(FfiOwnedBuffer value);
/*
Free one luaskills-owned heap byte buffer created by luaskills_ffi_bytes_clone.
释放由 luaskills_ffi_bytes_clone 创建的 luaskills 自主管理堆字节缓冲。
*/
void luaskills_ffi_bytes_free(uint8_t *value, size_t len);
/*
Register or clear the SQLite host callback before engine creation.
在创建 engine 前注册或清理 SQLite 宿主 callback。
*/
int32_t luaskills_ffi_set_sqlite_provider_callback(
    FfiSqliteProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);
/*
Register or clear the LanceDB host callback before engine creation.
在创建 engine 前注册或清理 LanceDB 宿主 callback。
*/
int32_t luaskills_ffi_set_lancedb_provider_callback(
    FfiLanceDbProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);
/*
Free one heap-allocated string-array result returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的字符串数组结果。
*/
void luaskills_ffi_string_array_free(FfiStringArray *value);

/*
Free one heap-allocated entry descriptor list returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的入口描述列表。
*/
void luaskills_ffi_entry_list_free(FfiRuntimeEntryDescriptorList *value);

/*
Free one heap-allocated help descriptor list returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的帮助描述列表。
*/
void luaskills_ffi_help_list_free(FfiRuntimeSkillHelpDescriptorList *value);

/*
Free one heap-allocated help detail returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的帮助详情。
*/
void luaskills_ffi_help_detail_free(FfiRuntimeHelpDetail *value);

/*
Free one heap-allocated invocation result returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的调用结果。
*/
void luaskills_ffi_invocation_result_free(FfiRuntimeInvocationResult *value);

/*
Free one heap-allocated skill apply result returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的技能安装或更新结果。
*/
void luaskills_ffi_skill_apply_result_free(FfiSkillApplyResult *value);

/*
Free one heap-allocated skill uninstall result returned by the standard FFI layer.
释放一段由标准 FFI 层返回并在堆上分配的技能卸载结果。
*/
void luaskills_ffi_skill_uninstall_result_free(FfiSkillUninstallResult *value);

/*
Return one stable FFI version string through the standard C ABI surface.
通过标准 C ABI 接口返回稳定的 FFI 版本字符串。
*/
int32_t luaskills_ffi_version(FfiOwnedBuffer *version_out, FfiOwnedBuffer *error_out);

/*
Return exported FFI entrypoint names through the standard C ABI surface.
通过标准 C ABI 接口返回已导出 FFI 入口点名称。
*/
int32_t luaskills_ffi_describe(FfiStringArray **functions_out, FfiOwnedBuffer *error_out);

/*
Create one LuaSkills engine through the standard C ABI surface.
通过标准 C ABI 接口创建一个 LuaSkills 引擎。
*/
int32_t luaskills_ffi_engine_new(
    const FfiLuaEngineOptions *options,
    uint64_t *engine_id_out,
    FfiOwnedBuffer *error_out
);

/*
Create one LuaSkills engine through the standard C ABI v2 surface.
通过标准 C ABI v2 接口创建一个 LuaSkills 引擎。
*/
int32_t luaskills_ffi_engine_new_v2(
    const FfiLuaEngineOptionsV2 *options,
    uint64_t *engine_id_out,
    FfiOwnedBuffer *error_out
);

/*
Create one LuaSkills engine through the standard C ABI v3 surface.
通过标准 C ABI v3 接口创建一个 LuaSkills 引擎。
*/
int32_t luaskills_ffi_engine_new_v3(
    const FfiLuaEngineOptionsV3 *options,
    uint64_t *engine_id_out,
    FfiOwnedBuffer *error_out
);

/*
Free one LuaSkills engine through the standard C ABI surface.
通过标准 C ABI 接口释放一个 LuaSkills 引擎。
*/
int32_t luaskills_ffi_engine_free(uint64_t engine_id, FfiOwnedBuffer *error_out);

/*
Load skills from one ordered root chain through the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链加载技能。
*/
int32_t luaskills_ffi_load_from_roots(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    FfiOwnedBuffer *error_out
);

/*
Reload skills from one ordered root chain through the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链重载技能。
*/
int32_t luaskills_ffi_reload_from_roots(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    FfiOwnedBuffer *error_out
);

/*
List runtime entries visible to one host-injected authority through the standard C ABI surface.
通过标准 C ABI 接口列出单个宿主注入权限可见的运行时入口。
*/
int32_t luaskills_ffi_list_entries(
    uint64_t engine_id,
    int32_t authority,
    FfiRuntimeEntryDescriptorList **entries_out,
    FfiOwnedBuffer *error_out
);

/*
List runtime help trees visible to one host-injected authority through the standard C ABI surface.
通过标准 C ABI 接口列出单个宿主注入权限可见的运行时帮助树。
*/
int32_t luaskills_ffi_list_skill_help(
    uint64_t engine_id,
    int32_t authority,
    FfiRuntimeSkillHelpDescriptorList **help_out,
    FfiOwnedBuffer *error_out
);

/*
Render one help detail visible to one host-injected authority through the standard C ABI surface.
通过标准 C ABI 接口渲染单个宿主注入权限可见的帮助详情。
*/
int32_t luaskills_ffi_render_skill_help_detail(
    uint64_t engine_id,
    int32_t authority,
    const char *skill_id,
    const char *flow_name,
    FfiBorrowedBuffer request_context_json,
    FfiRuntimeHelpDetail **detail_out,
    FfiOwnedBuffer *error_out
);

/*
Resolve prompt argument completions through the authority-gated standard C ABI surface.
通过带权限边界的标准 C ABI 接口解析提示词参数补全项。
*/
int32_t luaskills_ffi_prompt_argument_completions(
    uint64_t engine_id,
    int32_t authority,
    const char *prompt_name,
    const char *argument_name,
    FfiStringArray **values_out,
    FfiOwnedBuffer *error_out
);

/*
Check whether one tool belongs to a visible Lua skill through the standard C ABI surface.
通过标准 C ABI 接口检查单个工具是否属于可见 Lua 技能。
*/
int32_t luaskills_ffi_is_skill(
    uint64_t engine_id,
    int32_t authority,
    const char *tool_name,
    uint8_t *value_out,
    FfiOwnedBuffer *error_out
);

/*
Resolve the visible owning skill id of one tool through the standard C ABI surface.
通过标准 C ABI 接口解析单个工具可见的所属技能标识符。
*/
int32_t luaskills_ffi_skill_name_for_tool(
    uint64_t engine_id,
    int32_t authority,
    const char *tool_name,
    FfiOwnedBuffer *skill_id_out,
    FfiOwnedBuffer *error_out
);

/*
List flattened skill config records through the standard C ABI surface.
通过标准 C ABI 接口列出扁平化技能配置记录。
*/
int32_t luaskills_ffi_skill_config_list(
    uint64_t engine_id,
    const char *skill_id,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Describe effective package configuration declarations through the standard C ABI.
通过标准 C ABI 描述有效技能包配置声明。

skill_id may be NULL. include_values must be 0 or 1. The host must
authorize value disclosure before passing 1; returned values are not masked.
skill_id 可以为 NULL。include_values 必须为 0 或 1。宿主在传入 1
之前必须完成值披露授权；返回值不会被遮罩。
*/
int32_t luaskills_ffi_skill_config_describe(
    uint64_t engine_id,
    const char *skill_id,
    uint8_t include_values,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Validate one effective package configuration without changing persisted state.
在不修改持久化状态的前提下校验单个有效技能包配置。
*/
int32_t luaskills_ffi_skill_config_validate(
    uint64_t engine_id,
    const char *skill_id,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Read one optional skill config value through the standard C ABI surface.
通过标准 C ABI 接口读取单个可选技能配置值。
*/
int32_t luaskills_ffi_skill_config_get(
    uint64_t engine_id,
    const char *skill_id,
    const char *key,
    FfiOwnedBuffer *value_out,
    uint8_t *found_out,
    FfiOwnedBuffer *error_out
);

/*
Atomically insert or replace one package configuration batch through the standard C ABI.
通过标准 C ABI 原子插入或替换单个技能包配置批次。
*/
int32_t luaskills_ffi_skill_config_set_values(
    uint64_t engine_id,
    const char *skill_id,
    const char *values_json,
    const char *expected_revision,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Delete one skill config key through the standard C ABI surface.
通过标准 C ABI 接口删除单个技能配置键。
*/
int32_t luaskills_ffi_skill_config_delete(
    uint64_t engine_id,
    const char *skill_id,
    const char *key,
    const char *expected_revision,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Explicitly refresh one selected skill configuration store or both stores.
显式刷新一个选定技能配置存储或两个存储。
*/
int32_t luaskills_ffi_skill_config_refresh(
    uint64_t engine_id,
    const char *store_scope,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Poll ordered skill configuration events through the standard C ABI.
通过标准 C ABI 轮询有序技能配置事件。
*/
int32_t luaskills_ffi_skill_config_events_poll(
    uint64_t engine_id,
    const char *after_sequence,
    uint64_t limit,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Call one active loaded skill entry through the standard C ABI surface.
通过标准 C ABI 接口调用单个已激活的已加载技能入口。
*/
int32_t luaskills_ffi_call_skill(
    uint64_t engine_id,
    const char *tool_name,
    FfiBorrowedBuffer args_json,
    const FfiLuaInvocationContext *invocation_context,
    FfiRuntimeInvocationResult **result_out,
    FfiOwnedBuffer *error_out
);

/*
Execute arbitrary Lua code through the standard C ABI surface.
通过标准 C ABI 接口执行任意 Lua 代码。
*/
int32_t luaskills_ffi_run_lua(
    uint64_t engine_id,
    const char *code,
    FfiBorrowedBuffer args_json,
    const FfiLuaInvocationContext *invocation_context,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Open one public runtime lease through the standard C ABI surface.
通过标准 C ABI 接口打开一个公共运行时租约。
*/
int32_t luaskills_ffi_runtime_lease_create(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Evaluate one public runtime lease through the standard C ABI surface.
通过标准 C ABI 接口执行一个公共运行时租约。
*/
int32_t luaskills_ffi_runtime_lease_eval(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Read one public runtime lease status through the standard C ABI surface.
通过标准 C ABI 接口读取一个公共运行时租约状态。
*/
int32_t luaskills_ffi_runtime_lease_status(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
List public runtime leases through the standard C ABI surface.
通过标准 C ABI 接口列出公共运行时租约。
*/
int32_t luaskills_ffi_runtime_lease_list(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Close one public runtime lease through the standard C ABI surface.
通过标准 C ABI 接口关闭一个公共运行时租约。
*/
int32_t luaskills_ffi_runtime_lease_close(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Open one system_lua_lib runtime lease through the standard C ABI surface.
通过标准 C ABI 接口打开一个 system_lua_lib 运行时租约。

request_json is the strict System create body only: sid, ttl_sec, replace, cwd, workspace_root,
mounts, and required system_package. Do not include engine_id or authority; engine_id is the
separate first argument and the standard ABI performs no JSON authority envelope parsing.
request_json 仅包含严格 System 创建正文：sid、ttl_sec、replace、cwd、workspace_root、
mounts 与必填 system_package。不得包含 engine_id 或 authority；engine_id 已是独立首参数，
标准 ABI 不解析 JSON authority 外层。
*/
int32_t luaskills_ffi_system_runtime_lease_create(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Evaluate one system_lua_lib runtime lease through the standard C ABI surface.
通过标准 C ABI 接口执行一个 system_lua_lib 运行时租约。
*/
int32_t luaskills_ffi_system_runtime_lease_eval(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Read one system_lua_lib runtime lease status through the standard C ABI surface.
通过标准 C ABI 接口读取一个 system_lua_lib 运行时租约状态。
*/
int32_t luaskills_ffi_system_runtime_lease_status(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
List system_lua_lib runtime leases through the standard C ABI surface.
通过标准 C ABI 接口列出 system_lua_lib 运行时租约。
*/
int32_t luaskills_ffi_system_runtime_lease_list(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Close one system_lua_lib runtime lease through the standard C ABI surface.
通过标准 C ABI 接口关闭一个 system_lua_lib 运行时租约。
*/
int32_t luaskills_ffi_system_runtime_lease_close(
    uint64_t engine_id,
    FfiBorrowedBuffer request_json,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Destructively poll at most max_events managed-session events without waiting.
无等待地破坏性轮询至多 max_events 个受管会话事件。
result_json_out receives direct JSON with events, remaining, and timed_out fields.
result_json_out 接收包含 events、remaining 与 timed_out 字段的直接 JSON。
Closed and empty centers return an explicit error; free the result with luaskills_ffi_buffer_free.
关闭且队列为空时返回显式错误；结果必须使用 luaskills_ffi_buffer_free 释放。
*/
int32_t luaskills_ffi_managed_session_events_poll(
    uint64_t engine_id,
    size_t max_events,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Wait for and destructively drain at most max_events managed-session events.
等待并破坏性排空至多 max_events 个受管会话事件。
timeout_ms=0 is a true nonblocking poll; timeout returns success with timed_out=true.
timeout_ms=0 表示真正的非阻塞轮询；超时以 timed_out=true 的成功批次返回。
Closed and empty centers return an explicit error.
关闭且队列为空的事件中心返回显式错误。
*/
int32_t luaskills_ffi_managed_session_events_wait(
    uint64_t engine_id,
    size_t max_events,
    uint64_t timeout_ms,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

/*
Register, replace, or clear one per-engine managed-session wake callback.
注册、替换或清除单个 engine 的受管会话唤醒回调。
Pass a null callback to clear it.
传入空 callback 表示清除。
Replacement and clearing return only after retired calls finish, so user_data may be released after success.
替换与清除仅在退役调用结束后返回，因此成功返回后可释放 user_data。
Callback dispatch and retries run on one serial per-engine worker and never block event publishers.
回调投递与重试运行在每个 engine 的单个串行工作线程上，绝不阻塞事件发布者。
New callback user_data must already be valid because a catch-up invocation may occur before return.
新回调的 user_data 必须已经有效，因为返回前可能发生补偿调用。
*/
int32_t luaskills_ffi_set_managed_session_wake_callback(
    uint64_t engine_id,
    FfiManagedSessionWakeCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Disable one skill through one ordered root chain via the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链停用单个技能。
*/
int32_t luaskills_ffi_disable_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    const char *skill_id,
    const char *reason,
    FfiOwnedBuffer *error_out
);

/*
Disable one skill on the system plane through one ordered root chain.
通过标准 C ABI 接口按一条有序根链在 system 平面停用单个技能。
*/
int32_t luaskills_ffi_system_disable_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    int32_t authority,
    const char *skill_id,
    const char *reason,
    FfiOwnedBuffer *error_out
);

/*
Enable one skill through one ordered root chain via the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链启用单个技能。
*/
int32_t luaskills_ffi_enable_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    const char *skill_id,
    FfiOwnedBuffer *error_out
);

/*
Enable one skill on the system plane through one ordered root chain.
通过标准 C ABI 接口按一条有序根链在 system 平面启用单个技能。
*/
int32_t luaskills_ffi_system_enable_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    int32_t authority,
    const char *skill_id,
    FfiOwnedBuffer *error_out
);

/*
Uninstall one skill through one ordered root chain via the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链卸载单个技能。
*/
int32_t luaskills_ffi_uninstall_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    const char *skill_id,
    const FfiSkillUninstallOptions *options,
    FfiSkillUninstallResult **result_out,
    FfiOwnedBuffer *error_out
);

/*
Uninstall one skill on the system plane through one ordered root chain.
通过标准 C ABI 接口按一条有序根链在 system 平面卸载单个技能。
*/
int32_t luaskills_ffi_system_uninstall_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    int32_t authority,
    const char *skill_id,
    const FfiSkillUninstallOptions *options,
    FfiSkillUninstallResult **result_out,
    FfiOwnedBuffer *error_out
);

/*
Install one managed skill through one ordered root chain via the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链安装单个受管技能。
*/
int32_t luaskills_ffi_install_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    const FfiSkillInstallRequest *request,
    FfiSkillApplyResult **result_out,
    FfiOwnedBuffer *error_out
);

/*
Install one managed skill on the system plane through one ordered root chain.
通过标准 C ABI 接口按一条有序根链在 system 平面安装单个受管技能。
*/
int32_t luaskills_ffi_system_install_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    int32_t authority,
    const FfiSkillInstallRequest *request,
    FfiSkillApplyResult **result_out,
    FfiOwnedBuffer *error_out
);

/*
Update one managed skill through one ordered root chain via the standard C ABI surface.
通过标准 C ABI 接口按一条有序根链更新单个受管技能。
*/
int32_t luaskills_ffi_update_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    const FfiSkillInstallRequest *request,
    FfiSkillApplyResult **result_out,
    FfiOwnedBuffer *error_out
);

/*
Update one managed skill on the system plane through one ordered root chain.
通过标准 C ABI 接口按一条有序根链在 system 平面更新单个受管技能。
*/
int32_t luaskills_ffi_system_update_skill(
    uint64_t engine_id,
    const FfiRuntimeSkillRoot *skill_roots,
    size_t skill_roots_len,
    int32_t authority,
    const FfiSkillInstallRequest *request,
    FfiSkillApplyResult **result_out,
    FfiOwnedBuffer *error_out
);

#ifdef __cplusplus
}
#endif

#endif
