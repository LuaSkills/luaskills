use super::{default_ffi_runtime_session_timeout_ms, default_json_object};
use crate::runtime::managed_runtime::ManagedRuntimeKind;
use crate::runtime_context::RuntimeRequestContext;
use crate::runtime_options::{LuaInvocationContext, RuntimeSkillRoot};
use crate::{
    LuaEngineOptions, SkillInstallRequest, SkillManagementAuthority, SkillUninstallOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// One JSON request used to create one runtime engine instance.
/// 用于创建单个运行时引擎实例的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EngineNewJsonRequest {
    /// Engine construction options forwarded to the Rust runtime.
    /// 直接转发给 Rust 运行时的引擎构造选项。
    pub(super) options: LuaEngineOptions,
}

/// One JSON request used to resolve a host-visible managed runtime installation.
/// 用于解析宿主可见受管运行时安装的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ManagedRuntimeResolveJsonRequest {
    /// Existing absolute root containing the fixed `python` and `node` distribution families.
    /// 包含固定 `python` 与 `node` 发行资产族的现有绝对根目录。
    pub(super) distribution_root: PathBuf,
    /// Managed Python or Node runtime kind selected by the host.
    /// 宿主选择的受管 Python 或 Node 运行时类型。
    pub(super) runtime: ManagedRuntimeKind,
    /// Exact semantic runtime version used by the installation name and manifest.
    /// 安装名称与清单使用的精确语义化运行时版本。
    pub(super) version: String,
    /// Exact normalized supported platform key such as `windows-x64` or `macos-arm64`.
    /// 精确的受支持规范平台键，例如 `windows-x64` 或 `macos-arm64`。
    pub(super) platform: String,
}

/// One JSON result containing one stable engine handle id.
/// 包含单个稳定引擎句柄标识的 JSON 结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EngineHandleJsonResult {
    /// Stable numeric FFI handle id of the created engine.
    /// 已创建引擎对应的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
}

/// One JSON request that targets one existing engine handle.
/// 定位单个现有引擎句柄的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EngineIdJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
}

/// One JSON request that targets one engine with host-injected query authority.
/// 携带宿主注入查询权限并定位单个引擎的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EngineAuthorityJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Host-injected authority required by query JSON entrypoints.
    /// 查询 JSON 入口必填的宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request that targets one engine together with an ordered root chain.
/// 同时携带单个引擎与一条有序根链的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EngineRootsJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Ordered skill roots used by the current operation.
    /// 当前操作使用的有序技能根链。
    pub(super) skill_roots: Vec<RuntimeSkillRoot>,
}

/// One JSON request used to render help detail for one skill flow.
/// 用于渲染单个技能帮助详情的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RenderHelpJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Stable skill identifier of the target help tree.
    /// 目标帮助树所属的稳定技能标识符。
    pub(super) skill_id: String,
    /// Stable help flow name, where `main` means the main help node.
    /// 稳定帮助流程名，其中 `main` 表示主帮助节点。
    pub(super) flow_name: String,
    /// Optional request context injected during help rendering.
    /// 在帮助渲染时一并注入的可选请求上下文。
    #[serde(default)]
    pub(super) request_context: Option<RuntimeRequestContext>,
    /// Host-injected authority required by query JSON entrypoints.
    /// 查询 JSON 入口必填的宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to query prompt argument completions.
/// 用于查询提示词参数补全项的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PromptCompletionJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Host-injected authority required by prompt completion JSON entrypoints.
    /// 提示词补全 JSON 入口必填的宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
    /// Stable prompt name supplied by the host.
    /// 由宿主提供的稳定提示词名称。
    pub(super) prompt_name: String,
    /// Stable prompt argument name supplied by the host.
    /// 由宿主提供的稳定提示词参数名称。
    pub(super) argument_name: String,
}

/// One JSON request used to check whether one canonical tool name is a Lua skill entry.
/// 用于检查某个 canonical 工具名是否为 Lua 技能入口的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct IsSkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Tool name to resolve against the runtime entry registry.
    /// 需要在运行时入口注册表中解析的工具名称。
    pub(super) tool_name: String,
    /// Host-injected authority required by query JSON entrypoints.
    /// 查询 JSON 入口必填的宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON result that answers one boolean runtime query.
/// 用于返回单个布尔型运行时查询结果的 JSON 结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BoolJsonResult {
    /// Boolean value returned by the runtime query.
    /// 运行时查询返回的布尔值。
    pub(super) value: bool,
}

/// One JSON request used to resolve the owning skill id of one canonical tool name.
/// 用于解析某个 canonical 工具名所属技能标识符的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillNameForToolJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Tool name to resolve against the runtime entry registry.
    /// 需要在运行时入口注册表中解析的工具名称。
    pub(super) tool_name: String,
    /// Host-injected authority required by query JSON entrypoints.
    /// 查询 JSON 入口必填的宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON result containing the optional owning skill id of one tool.
/// 包含某个工具可选所属技能标识符的 JSON 结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OptionalSkillNameJsonResult {
    /// Optional owning skill id resolved from the current runtime registry.
    /// 当前运行时注册表解析出的可选所属技能标识符。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) skill_id: Option<String>,
}

/// One JSON request used to list flattened skill config records.
/// 用于列出扁平化技能配置记录的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillConfigListJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Optional skill identifier used to restrict the listing scope.
    /// 用于限制列举范围的可选技能标识符。
    #[serde(default)]
    pub(super) skill_id: Option<String>,
}

/// One JSON request used to describe effective package configuration declarations.
/// 用于描述有效技能包配置声明的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SkillPackageConfigDescribeJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Optional package identifier used to restrict the structure query.
    /// 用于限制结构查询范围的可选技能包标识符。
    #[serde(default)]
    pub(super) skill_id: Option<String>,
    /// Whether raw effective values must be included without masking.
    /// 是否必须无掩码包含原始有效值。
    #[serde(default)]
    pub(super) include_values: bool,
}

/// One JSON request used to validate an effective package configuration.
/// 用于校验有效技能包配置的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SkillPackageConfigValidateJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Stable effective package identifier to validate.
    /// 需要校验的稳定有效技能包标识符。
    pub(super) skill_id: String,
}

/// One JSON request used to resolve one `(skill_id, key)` config pair.
/// 用于解析单个 `(skill_id, key)` 配置对的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillConfigGetJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Stable skill identifier that owns the current key.
    /// 拥有当前键的稳定技能标识符。
    pub(super) skill_id: String,
    /// Stable config key inside the current skill namespace.
    /// 当前技能命名空间内的稳定配置键。
    pub(super) key: String,
}

/// One JSON request used to insert or replace one `(skill_id, key)` config pair.
/// 用于插入或替换单个 `(skill_id, key)` 配置对的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillConfigSetJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Stable skill identifier that owns the current key.
    /// 拥有当前键的稳定技能标识符。
    pub(super) skill_id: String,
    /// Stable config key inside the current skill namespace.
    /// 当前技能命名空间内的稳定配置键。
    pub(super) key: String,
    /// String config value written into the unified skill config store.
    /// 写入统一技能配置存储的字符串配置值。
    pub(super) value: String,
}

/// One JSON result describing the lookup state of one skill config value.
/// 描述单个技能配置值查找状态的 JSON 结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillConfigGetJsonResult {
    /// Whether the requested config value currently exists.
    /// 请求的配置值当前是否存在。
    pub(super) found: bool,
    /// Stable skill identifier that was queried.
    /// 被查询的稳定技能标识符。
    pub(super) skill_id: String,
    /// Stable config key that was queried.
    /// 被查询的稳定配置键。
    pub(super) key: String,
    /// Optional string value returned when `found=true`.
    /// 当 `found=true` 时返回的可选字符串值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) value: Option<String>,
}

/// One JSON result describing one successful skill config mutation.
/// 描述单次技能配置变更成功结果的 JSON 结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillConfigMutationJsonResult {
    /// Mutation action name such as `set` or `delete`.
    /// 变更动作名称，例如 `set` 或 `delete`。
    pub(super) action: String,
    /// Stable skill identifier that owns the current key.
    /// 拥有当前键的稳定技能标识符。
    pub(super) skill_id: String,
    /// Stable config key touched by the mutation.
    /// 当前变更触及的稳定配置键。
    pub(super) key: String,
    /// Optional value returned for `set`.
    /// 为 `set` 动作返回的可选值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) value: Option<String>,
    /// Optional deletion flag returned for `delete`.
    /// 为 `delete` 动作返回的可选删除标记。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) deleted: Option<bool>,
}

/// One JSON request used to call one loaded skill entry.
/// 用于调用单个已加载技能入口的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CallSkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Canonical tool name of the target Lua skill entry.
    /// 目标 Lua 技能入口的 canonical 工具名称。
    pub(super) tool_name: String,
    /// JSON arguments forwarded to the target skill entry.
    /// 转发给目标技能入口的 JSON 参数。
    #[serde(default = "default_json_object")]
    pub(super) args: Value,
    /// Optional invocation context injected into the runtime call.
    /// 注入到运行时调用中的可选调用上下文。
    #[serde(default)]
    pub(super) invocation_context: Option<LuaInvocationContext>,
}

/// One JSON request used to execute arbitrary Lua code.
/// 用于执行任意 Lua 代码的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RunLuaJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Inline Lua source code executed by the runtime engine.
    /// 由运行时引擎执行的内联 Lua 源代码。
    pub(super) code: String,
    /// JSON arguments exposed to the Lua snippet as `args`.
    /// 作为 `args` 暴露给 Lua 片段的 JSON 参数。
    #[serde(default = "default_json_object")]
    pub(super) args: Value,
    /// Optional invocation context injected into the runtime call.
    /// 注入到运行时调用中的可选调用上下文。
    #[serde(default)]
    pub(super) invocation_context: Option<LuaInvocationContext>,
}

/// One JSON request used to create one persistent runtime session.
/// 用于创建单个持久运行时会话的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RuntimeSessionCreateJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Stable session identifier supplied by the host.
    /// 宿主提供的稳定会话标识。
    pub(super) sid: String,
    /// Requested lease TTL in seconds.
    /// 请求的租约有效期秒数。
    #[serde(default)]
    pub(super) ttl_sec: Option<u64>,
    /// Whether an existing session with the same SID should be replaced.
    /// 是否替换同一 SID 下已经存在的会话。
    #[serde(default)]
    pub(super) replace: bool,
    /// Optional lease cwd controlled by the host.
    /// 由宿主控制的可选租约 cwd。
    #[serde(default)]
    pub(super) cwd: Option<String>,
    /// Optional workspace root recorded on the lease.
    /// 记录在租约上的可选工作区根目录。
    #[serde(default)]
    pub(super) workspace_root: Option<String>,
    /// Optional extra Lua module roots prepended to the lease VM.
    /// 前置追加到租约虚拟机中的可选 Lua 模块根目录集合。
    #[serde(default)]
    pub(super) lua_roots: Vec<String>,
    /// Optional extra native module roots prepended to the lease VM.
    /// 前置追加到租约虚拟机中的可选原生模块根目录集合。
    #[serde(default)]
    pub(super) c_roots: Vec<String>,
    /// Optional host-owned structured mount metadata.
    /// 宿主拥有的可选结构化挂载元数据。
    #[serde(default = "default_json_object")]
    pub(super) mounts: Value,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// Trusted System Plugin package descriptor used by the System lease JSON surface.
/// System 租约 JSON 接口使用的可信 System Plugin 包描述符。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SystemRuntimePackageJsonRequest {
    /// Stable System Plugin package identifier.
    /// 稳定的 System Plugin 包标识符。
    pub(super) id: String,
    /// Absolute package root under the configured System Lua trust root.
    /// 位于已配置 System Lua 信任根下的绝对包根目录。
    pub(super) root: String,
    /// Package-relative dependency manifest file.
    /// 包相对依赖清单文件。
    pub(super) dependencies_file: String,
}

/// Strict high-level JSON request used to create one System runtime lease.
/// 用于创建单个 System 运行时租约的严格高层 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SystemRuntimeSessionCreateJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Stable System session identifier supplied by the host.
    /// 宿主提供的稳定 System 会话标识。
    pub(super) sid: String,
    /// Requested lease TTL in seconds; zero means infinite.
    /// 请求的租约有效期秒数；零表示无限期。
    #[serde(default)]
    pub(super) ttl_sec: Option<u64>,
    /// Whether an existing System lease with the same SID should be replaced.
    /// 是否替换同一 SID 下已有的 System 租约。
    #[serde(default)]
    pub(super) replace: bool,
    /// Optional host-controlled lease cwd.
    /// 可选的宿主控制租约 cwd。
    #[serde(default)]
    pub(super) cwd: Option<String>,
    /// Optional workspace root exposed through the package context.
    /// 通过包上下文暴露的可选工作区根目录。
    #[serde(default)]
    pub(super) workspace_root: Option<String>,
    /// Host-owned structured mount metadata.
    /// 宿主拥有的结构化挂载元数据。
    #[serde(default = "default_json_object")]
    pub(super) mounts: Value,
    /// Required trusted System Plugin package descriptor.
    /// 必需的可信 System Plugin 包描述符。
    pub(super) system_package: SystemRuntimePackageJsonRequest,
    /// Explicit authority required by the high-level System entrypoint.
    /// 高层 System 入口必需的显式权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// Strict high-level JSON request used to poll managed-session events without waiting.
/// 用于无等待轮询受管会话事件的严格高层 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManagedSessionEventsPollJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Positive maximum number of events removed by this destructive poll.
    /// 本次破坏性轮询最多移除的正数事件数量。
    pub(super) max_events: usize,
    /// Explicit host-injected authority required by the managed-session event surface.
    /// 受管会话事件接口要求的显式宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// Strict high-level JSON request used to wait for managed-session events.
/// 用于等待受管会话事件的严格高层 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManagedSessionEventsWaitJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Positive maximum number of events removed by this destructive wait.
    /// 本次破坏性等待最多移除的正数事件数量。
    pub(super) max_events: usize,
    /// Finite wait timeout in milliseconds; zero performs a true nonblocking poll.
    /// 有限等待超时毫秒数；零表示真正的非阻塞轮询。
    pub(super) timeout_ms: u64,
    /// Explicit host-injected authority required by the managed-session event surface.
    /// 受管会话事件接口要求的显式宿主注入权限等级。
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to evaluate code in a persistent runtime session.
/// 用于在持久运行时会话中执行代码的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RuntimeSessionEvalJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Opaque lease identifier returned by create.
    /// create 返回的不透明租约标识。
    pub(super) lease_id: String,
    /// Optional stable session identifier echoed by the host wrapper.
    /// 由宿主包装层回传的可选稳定会话标识。
    #[serde(default)]
    pub(super) sid: Option<String>,
    /// Optional SID-local generation echoed by the host wrapper.
    /// 由宿主包装层回传的可选 SID 内 generation。
    #[serde(default)]
    pub(super) generation: Option<u64>,
    /// Inline Lua source code executed by the persistent runtime VM.
    /// 由持久运行时 VM 执行的内联 Lua 源码。
    pub(super) code: String,
    /// JSON arguments exposed to Lua as `args`.
    /// 作为 `args` 暴露给 Lua 的 JSON 参数。
    #[serde(default = "default_json_object")]
    pub(super) args: Value,
    /// Maximum execution time in milliseconds.
    /// 最大执行时长（毫秒）。
    #[serde(default = "default_ffi_runtime_session_timeout_ms")]
    pub(super) timeout_ms: u64,
    /// Optional invocation context injected into the runtime lease evaluation.
    /// 注入到运行时租约执行中的可选调用上下文。
    #[serde(default)]
    pub(super) invocation_context: Option<LuaInvocationContext>,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to address a persistent runtime session lease.
/// 用于定位持久运行时会话租约的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RuntimeSessionLeaseJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Opaque lease identifier returned by create.
    /// create 返回的不透明租约标识。
    pub(super) lease_id: String,
    /// Optional stable session identifier echoed by the host wrapper.
    /// 由宿主包装层回传的可选稳定会话标识。
    #[serde(default)]
    pub(super) sid: Option<String>,
    /// Optional SID-local generation echoed by the host wrapper.
    /// 由宿主包装层回传的可选 SID 内 generation。
    #[serde(default)]
    pub(super) generation: Option<u64>,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to list active persistent runtime sessions.
/// 用于列出活跃持久运行时会话的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RuntimeSessionListJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Optional stable session identifier used to filter the active lease list.
    /// 用于过滤活跃租约列表的可选稳定会话标识。
    #[serde(default)]
    pub(super) sid: Option<String>,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to disable one skill in one ordered root chain.
/// 用于在一条有序根链中停用单个技能的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct DisableSkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Ordered skill roots used by the current lifecycle operation.
    /// 当前生命周期操作使用的有序技能根链。
    pub(super) skill_roots: Vec<RuntimeSkillRoot>,
    /// Stable target skill identifier.
    /// 稳定的目标技能标识符。
    pub(super) skill_id: String,
    /// Optional disable reason persisted into the skill state.
    /// 持久化到技能状态中的可选停用原因。
    #[serde(default)]
    pub(super) reason: Option<String>,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to enable one skill in one ordered root chain.
/// 用于在一条有序根链中启用单个技能的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EnableSkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Ordered skill roots used by the current lifecycle operation.
    /// 当前生命周期操作使用的有序技能根链。
    pub(super) skill_roots: Vec<RuntimeSkillRoot>,
    /// Stable target skill identifier.
    /// 稳定的目标技能标识符。
    pub(super) skill_id: String,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to uninstall one skill in one ordered root chain.
/// 用于在一条有序根链中卸载单个技能的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct UninstallSkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Ordered skill roots used by the current lifecycle operation.
    /// 当前生命周期操作使用的有序技能根链。
    pub(super) skill_roots: Vec<RuntimeSkillRoot>,
    /// Stable target skill identifier.
    /// 稳定的目标技能标识符。
    pub(super) skill_id: String,
    /// Optional explicit target root used by advanced SDK wrappers.
    /// 高级 SDK 封装使用的可选显式目标根。
    #[serde(default)]
    pub(super) target_root: Option<RuntimeSkillRoot>,
    /// Optional database cleanup switches applied after uninstall commit succeeds.
    /// 在卸载提交成功后应用的可选数据库清理开关。
    #[serde(default)]
    pub(super) options: SkillUninstallOptions,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used to install or update one managed skill in one ordered root chain.
/// 用于在一条有序根链中安装或更新单个受管技能的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApplySkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Ordered skill roots used by the current lifecycle operation.
    /// 当前生命周期操作使用的有序技能根链。
    pub(super) skill_roots: Vec<RuntimeSkillRoot>,
    /// Managed install or update request forwarded to the Rust runtime.
    /// 直接转发给 Rust 运行时的受管安装或更新请求。
    pub(super) request: SkillInstallRequest,
    /// Optional explicit target root used by advanced SDK wrappers.
    /// 高级 SDK 封装使用的可选显式目标根。
    #[serde(default)]
    pub(super) target_root: Option<RuntimeSkillRoot>,
    /// Optional authority required by system JSON entrypoints and ignored by ordinary entrypoints.
    /// system JSON 入口必填、普通入口忽略的可选权限等级。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON request used by host-private URL-manifest skill installation entrypoints.
/// 供宿主私有 URL manifest 技能安装入口使用的 JSON 请求。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PrivateUrlManifestSkillJsonRequest {
    /// Stable numeric FFI handle id of the target engine.
    /// 目标引擎的稳定数值 FFI 句柄标识。
    pub(super) engine_id: u64,
    /// Ordered skill roots used by the current lifecycle operation.
    /// 当前生命周期操作使用的有序技能根链。
    pub(super) skill_roots: Vec<RuntimeSkillRoot>,
    /// Stable target skill identifier expected inside the private manifest.
    /// 私有 manifest 内必须匹配的稳定目标技能标识符。
    pub(super) skill_id: String,
    /// Host-approved private manifest URL.
    /// 宿主已批准的私有 manifest URL。
    pub(super) manifest_url: String,
    /// Optional explicit target root used by advanced SDK wrappers.
    /// 高级 SDK 封装使用的可选显式目标根。
    #[serde(default)]
    pub(super) target_root: Option<RuntimeSkillRoot>,
    /// Required system authority injected by the host.
    /// 宿主注入的必填 system 权限。
    #[serde(default)]
    pub(super) authority: Option<SkillManagementAuthority>,
}

/// One JSON result that lists the currently exported FFI entrypoints.
/// 列出当前已导出 FFI 入口点的 JSON 结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct FfiDescribeJsonResult {
    /// Stable FFI version string for compatibility checks.
    /// 用于兼容性检查的稳定 FFI 版本字符串。
    pub(super) ffi_version: String,
    /// Exported JSON entrypoint names currently provided by the library.
    /// 当前由库提供的已导出 JSON 入口点名称列表。
    pub(super) exported_functions: Vec<String>,
}
