use crate::download::manager::{DownloadProgress, DownloadProgressCallback};
use crate::runtime::entry::RuntimeEntryDescriptor;
use crate::skill::manager::{SkillLifecycleAction, SkillManagementAuthority, SkillOperationPlane};
use crate::skill::source::SkillInstallSourceType;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Callback type used by hosts to receive runtime skill-lifecycle events.
/// 宿主用于接收运行时技能生命周期事件的回调类型。
pub type RuntimeSkillLifecycleCallback = Arc<dyn Fn(&RuntimeSkillLifecycleEvent) + Send + Sync>;

/// Callback type used by hosts to receive fine-grained skill-operation progress events.
/// 宿主用于接收细粒度技能操作进度事件的回调类型。
pub type RuntimeSkillOperationProgressCallback =
    Arc<dyn Fn(&RuntimeSkillOperationProgressEvent) + Send + Sync>;

/// Callback type used by hosts to receive runtime entry-registry change events.
/// 宿主用于接收运行时入口注册表变化事件的回调类型。
pub type RuntimeEntryRegistryCallback = Arc<dyn Fn(&RuntimeEntryRegistryDelta) + Send + Sync>;

/// Callback type used by hosts to handle one Lua-triggered runtime skill-management request.
/// 宿主用于处理单个由 Lua 触发的运行时技能管理请求的回调类型。
pub type RuntimeSkillManagementCallback =
    Arc<dyn Fn(&RuntimeSkillManagementRequest) -> Result<Value, String> + Send + Sync>;

/// Callback type used by hosts to handle one Lua-triggered host-tool bridge request.
/// 宿主用于处理单个由 Lua 触发的宿主工具桥接请求的回调类型。
pub type RuntimeHostToolCallback =
    Arc<dyn Fn(&RuntimeHostToolRequest) -> Result<Value, String> + Send + Sync>;

/// Callback type used by hosts to handle one standard model embedding request.
/// 宿主用于处理单个标准模型 embedding 请求的回调类型。
pub type RuntimeModelEmbedCallback = Arc<
    dyn Fn(&RuntimeModelEmbedRequest) -> Result<RuntimeModelEmbedResponse, RuntimeModelError>
        + Send
        + Sync,
>;

/// Callback type used by hosts to handle one standard non-streaming LLM request.
/// 宿主用于处理单个标准非流式 LLM 请求的回调类型。
pub type RuntimeModelLlmCallback = Arc<
    dyn Fn(&RuntimeModelLlmRequest) -> Result<RuntimeModelLlmResponse, RuntimeModelError>
        + Send
        + Sync,
>;

/// Structured host-tool bridge actions that Lua may request through `vulcan.host.*`.
/// Lua 可以通过 `vulcan.host.*` 请求的结构化宿主工具桥接动作集合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostToolAction {
    /// Request the current host-visible tool list.
    /// 请求当前对宿主可见的工具列表。
    List,
    /// Request whether one host tool exists.
    /// 请求判断某个宿主工具是否存在。
    Has,
    /// Request one host-tool invocation.
    /// 请求执行一次宿主工具调用。
    Call,
}

/// Structured Lua-triggered host-tool bridge request forwarded to the host.
/// 转发给宿主的结构化 Lua 触发宿主工具桥接请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeHostToolRequest {
    /// Requested host-tool bridge action kind.
    /// 请求的宿主工具桥接动作类型。
    pub action: RuntimeHostToolAction,
    /// Optional host tool name, required for `has` and `call`.
    /// 可选宿主工具名称，`has` 与 `call` 动作必填。
    pub tool_name: Option<String>,
    /// JSON payload converted from the Lua table argument.
    /// 从 Lua table 参数转换得到的 JSON 载荷。
    pub args: Value,
}

/// Caller context automatically attached to one standard model capability request.
/// 自动附加到单个标准模型能力请求上的调用方上下文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeModelCaller {
    /// Skill identifier that owns the currently executing Lua entry when available.
    /// 当前执行 Lua 入口所属的 skill 标识符（如果存在）。
    pub skill_id: Option<String>,
    /// Local entry name declared by the owning skill when available.
    /// 所属 skill 声明的局部入口名称（如果存在）。
    pub entry_name: Option<String>,
    /// Canonical runtime tool name currently executing when available.
    /// 当前正在执行的 canonical 运行时工具名称（如果存在）。
    pub canonical_tool_name: Option<String>,
    /// Runtime root name that owns the current skill when available.
    /// 拥有当前 skill 的运行时根名称（如果存在）。
    pub root_name: Option<String>,
    /// Host-visible absolute skill directory when available.
    /// 对宿主可见的绝对 skill 目录（如果存在）。
    pub skill_dir: Option<String>,
    /// Host-provided client name from the current request context when available.
    /// 当前请求上下文中的宿主提供客户端名称（如果存在）。
    pub client_name: Option<String>,
    /// Host-provided request identifier from the current request context when available.
    /// 当前请求上下文中的宿主提供请求标识符（如果存在）。
    pub request_id: Option<String>,
}

/// Standard embedding request passed from LuaSkills into the host callback.
/// 从 LuaSkills 传递给宿主回调的标准 embedding 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeModelEmbedRequest {
    /// Single input text that Lua requested to embed.
    /// Lua 请求进行 embedding 的单条输入文本。
    pub text: String,
    /// Caller context captured from the active Lua runtime scope.
    /// 从当前 Lua 运行时作用域捕获的调用方上下文。
    pub caller: RuntimeModelCaller,
}

/// Standard embedding response returned by the host callback.
/// 宿主回调返回的标准 embedding 响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeModelEmbedResponse {
    /// Embedding vector returned by the host-managed model provider.
    /// 宿主管理的模型 provider 返回的 embedding 向量。
    pub vector: Vec<f32>,
    /// Number of vector dimensions reported by the host.
    /// 宿主报告的向量维度数量。
    pub dimensions: usize,
    /// Optional token usage metadata reported by the host.
    /// 宿主报告的可选 token 用量元数据。
    pub usage: Option<RuntimeModelUsage>,
}

/// Standard non-streaming LLM request passed from LuaSkills into the host callback.
/// 从 LuaSkills 传递给宿主回调的标准非流式 LLM 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeModelLlmRequest {
    /// System instruction text supplied by Lua.
    /// Lua 提供的 system 指令文本。
    pub system: String,
    /// User message text supplied by Lua.
    /// Lua 提供的 user 消息文本。
    pub user: String,
    /// Caller context captured from the active Lua runtime scope.
    /// 从当前 Lua 运行时作用域捕获的调用方上下文。
    pub caller: RuntimeModelCaller,
}

/// Standard non-streaming LLM response returned by the host callback.
/// 宿主回调返回的标准非流式 LLM 响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeModelLlmResponse {
    /// Assistant text returned by the host-managed model provider.
    /// 宿主管理的模型 provider 返回的 assistant 文本。
    pub assistant: String,
    /// Optional token usage metadata reported by the host.
    /// 宿主报告的可选 token 用量元数据。
    pub usage: Option<RuntimeModelUsage>,
}

/// Optional token usage metadata returned by a host-managed model provider.
/// 宿主管理的模型 provider 返回的可选 token 用量元数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeModelUsage {
    /// Optional input token count reported by the host.
    /// 宿主报告的可选输入 token 数量。
    pub input_tokens: Option<u64>,
    /// Optional output token count reported by the host.
    /// 宿主报告的可选输出 token 数量。
    pub output_tokens: Option<u64>,
}

/// Structured model error returned by LuaSkills or the host model callback.
/// LuaSkills 或宿主模型回调返回的结构化模型错误。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeModelError {
    /// Stable LuaSkills-level model error code.
    /// 稳定的 LuaSkills 级模型错误码。
    pub code: RuntimeModelErrorCode,
    /// Human-readable error summary.
    /// 人类可读的错误摘要。
    pub message: String,
    /// Optional raw provider error text after host-side redaction.
    /// 宿主侧脱敏后的可选 provider 原始错误文本。
    pub provider_message: Option<String>,
    /// Optional raw provider error code.
    /// 可选 provider 原始错误码。
    pub provider_code: Option<String>,
    /// Optional provider status such as an HTTP status code.
    /// 可选 provider 状态，例如 HTTP 状态码。
    pub provider_status: Option<u16>,
}

/// Stable model error codes exposed through the Lua result envelope.
/// 通过 Lua 返回包络暴露的稳定模型错误码。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeModelErrorCode {
    /// The requested model capability has no registered host callback.
    /// 请求的模型能力没有注册宿主回调。
    ModelUnavailable,
    /// Lua supplied an invalid argument count, type, or empty string.
    /// Lua 提供了非法参数数量、类型或空字符串。
    InvalidArgument,
    /// The host model provider returned an error.
    /// 宿主模型 provider 返回了错误。
    ProviderError,
    /// The host model invocation timed out.
    /// 宿主模型调用超时。
    Timeout,
    /// The host rejected the invocation because a budget or limit was exceeded.
    /// 宿主因预算或限制超额拒绝了本次调用。
    BudgetExceeded,
    /// LuaSkills or the host bridge hit an internal failure.
    /// LuaSkills 或宿主桥接遇到内部故障。
    InternalError,
}

impl RuntimeModelErrorCode {
    /// Return the stable snake_case code string used by Lua result envelopes.
    /// 返回 Lua 返回包络使用的稳定 snake_case 错误码字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ModelUnavailable => "model_unavailable",
            Self::InvalidArgument => "invalid_argument",
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::BudgetExceeded => "budget_exceeded",
            Self::InternalError => "internal_error",
        }
    }

    /// Convert a stable snake_case model error code into the internal enum.
    /// 将稳定的 snake_case 模型错误码转换为内部枚举。
    pub fn from_code_str(value: &str) -> Self {
        match value {
            "model_unavailable" => Self::ModelUnavailable,
            "invalid_argument" => Self::InvalidArgument,
            "provider_error" => Self::ProviderError,
            "timeout" => Self::Timeout,
            "budget_exceeded" => Self::BudgetExceeded,
            _ => Self::InternalError,
        }
    }
}

/// Structured management actions that one Lua-exposed runtime bridge may request from the host.
/// Lua 暴露的运行时桥接可能向宿主请求的结构化管理动作集合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSkillManagementAction {
    /// Request one managed install operation.
    /// 请求执行一次受管安装操作。
    Install,
    /// Request one managed update operation.
    /// 请求执行一次受管更新操作。
    Update,
    /// Request one uninstall operation.
    /// 请求执行一次卸载操作。
    Uninstall,
    /// Request one enable operation.
    /// 请求执行一次启用操作。
    Enable,
    /// Request one disable operation.
    /// 请求执行一次停用操作。
    Disable,
}

/// Structured Lua-triggered skill-management request forwarded to the host.
/// 转发给宿主的结构化 Lua 触发技能管理请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeSkillManagementRequest {
    /// Requested management action kind.
    /// 请求的管理动作类型。
    pub action: RuntimeSkillManagementAction,
    /// Host-injected authority level for this ordinary runtime bridge request.
    /// 当前普通运行时桥接请求的宿主注入权限等级。
    pub authority: SkillManagementAuthority,
    /// Arbitrary JSON payload supplied by the Lua caller.
    /// 由 Lua 调用方提供的任意 JSON 载荷。
    pub input: Value,
}

/// Structured lifecycle event emitted after one skill-management operation is evaluated.
/// 在评估一次技能管理操作后发出的结构化生命周期事件。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeSkillLifecycleEvent {
    /// Operation plane that triggered the lifecycle event.
    /// 触发生命周期事件的操作平面。
    pub plane: SkillOperationPlane,
    /// Lifecycle action represented by the current event.
    /// 当前事件所表示的生命周期动作。
    pub action: SkillLifecycleAction,
    /// Skill identifier targeted by the current lifecycle operation.
    /// 当前生命周期操作对应的技能标识符。
    pub skill_id: String,
    /// Optional named skill root that owns the effective target skill instance.
    /// 拥有当前生效目标技能实例的可选命名技能根。
    pub root_name: Option<String>,
    /// Optional physical skill directory of the effective target skill instance.
    /// 当前生效目标技能实例的可选物理技能目录。
    pub skill_dir: Option<String>,
    /// High-level event status such as completed, failed, or blocked.
    /// 当前事件的高层状态，例如 completed、failed 或 blocked。
    pub status: String,
    /// Optional human-readable explanation of the current lifecycle outcome.
    /// 当前生命周期结果的可选人类可读说明。
    pub message: Option<String>,
}

/// Fine-grained progress event emitted during one skill install or update operation.
/// 单次技能安装或更新操作过程中发出的细粒度进度事件。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RuntimeSkillOperationProgressEvent {
    /// Stable operation id shared by all events from the same lifecycle operation.
    /// 同一生命周期操作全部事件共享的稳定操作标识符。
    pub operation_id: String,
    /// Monotonic sequence number within the current operation.
    /// 当前操作内单调递增的序号。
    pub sequence: u64,
    /// Operation plane that owns the current lifecycle operation.
    /// 当前生命周期操作所属的操作平面。
    pub plane: SkillOperationPlane,
    /// Lifecycle action represented by the current operation.
    /// 当前操作所表示的生命周期动作。
    pub action: SkillLifecycleAction,
    /// Machine-readable phase name such as `resolving_source` or `downloading_archive`.
    /// 机器可读阶段名称，例如 `resolving_source` 或 `downloading_archive`。
    pub phase: String,
    /// Machine-readable status such as `started`, `progress`, `completed`, or `failed`.
    /// 机器可读状态，例如 `started`、`progress`、`completed` 或 `failed`。
    pub status: String,
    /// Optional skill id targeted by the operation.
    /// 当前操作目标的可选技能标识符。
    pub skill_id: Option<String>,
    /// Optional named skill root targeted by the operation.
    /// 当前操作目标的可选命名技能根。
    pub root_name: Option<String>,
    /// Optional source type involved in the current phase.
    /// 当前阶段涉及的可选来源类型。
    pub source_type: Option<SkillInstallSourceType>,
    /// Optional source locator involved in the current phase.
    /// 当前阶段涉及的可选来源定位值。
    pub source_locator: Option<String>,
    /// Optional completed byte count for download phases.
    /// 下载阶段的可选已完成字节数。
    pub bytes_done: Option<u64>,
    /// Optional total byte count for download phases.
    /// 下载阶段的可选总字节数。
    pub bytes_total: Option<u64>,
    /// Optional percentage for determinate progress phases.
    /// 确定性进度阶段的可选百分比。
    pub percent: Option<f64>,
    /// Optional human-readable progress message.
    /// 可选的人类可读进度消息。
    pub message: Option<String>,
}

/// Lightweight helper that preserves operation identity and progress ordering.
/// 用于保持操作身份与进度顺序的轻量辅助器。
#[derive(Clone)]
pub(crate) struct RuntimeSkillOperationProgressEmitter {
    /// Stable operation id emitted on every progress event.
    /// 每条进度事件都会携带的稳定操作标识符。
    operation_id: String,
    /// Shared monotonic sequence counter for this operation.
    /// 当前操作共享的单调序号计数器。
    sequence: Arc<AtomicU64>,
    /// Operation plane emitted on every progress event.
    /// 每条进度事件都会携带的操作平面。
    plane: SkillOperationPlane,
    /// Lifecycle action emitted on every progress event.
    /// 每条进度事件都会携带的生命周期动作。
    action: SkillLifecycleAction,
    /// Optional root name emitted on every progress event.
    /// 每条进度事件都会携带的可选根名称。
    root_name: Option<String>,
    /// Optional default skill id emitted when a phase does not override it.
    /// 阶段没有覆盖时使用的可选默认技能标识符。
    skill_id: Option<String>,
}

impl RuntimeSkillOperationProgressEmitter {
    /// Create one progress emitter for a single lifecycle operation.
    /// 为单次生命周期操作创建一个进度发射器。
    ///
    /// The plane parameter is the operation plane emitted on every progress event.
    /// plane 参数是每条进度事件都会携带的操作平面。
    ///
    /// The action parameter is the lifecycle action represented by the current operation.
    /// action 参数是当前操作所表示的生命周期动作。
    ///
    /// The root_name parameter is the optional skill root name emitted on progress events.
    /// root_name 参数是进度事件中携带的可选技能根名称。
    ///
    /// The skill_id parameter is the optional default skill id emitted when phases do not override it.
    /// skill_id 参数是阶段未覆盖时进度事件携带的可选默认技能标识符。
    ///
    /// Returns one initialized progress emitter with a stable operation id.
    /// 返回一个携带稳定操作标识符的已初始化进度发射器。
    ///
    /// Returns an error when the operation id timestamp cannot be represented.
    /// 当操作标识符时间戳无法表示时返回错误。
    pub(crate) fn new(
        plane: SkillOperationPlane,
        action: SkillLifecycleAction,
        root_name: Option<String>,
        skill_id: Option<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            operation_id: build_skill_operation_id(action, skill_id.as_deref())?,
            sequence: Arc::new(AtomicU64::new(0)),
            plane,
            action,
            root_name,
            skill_id,
        })
    }

    /// Emit one phase-level progress event.
    /// 发出一条阶段级进度事件。
    pub(crate) fn emit(&self, phase: &str, status: &str, message: Option<String>) {
        self.emit_detail(RuntimeSkillOperationProgressDetail {
            phase,
            status,
            skill_id: None,
            source_type: None,
            source_locator: None,
            bytes_done: None,
            bytes_total: None,
            message,
        });
    }

    /// Emit one detailed progress event with optional source and byte metadata.
    /// 发出一条携带可选来源与字节元数据的详细进度事件。
    pub(crate) fn emit_detail(&self, detail: RuntimeSkillOperationProgressDetail<'_>) {
        let bytes_total = detail.bytes_total;
        let percent = match (detail.bytes_done, bytes_total) {
            (Some(done), Some(total)) if total > 0 => {
                Some(((done as f64 / total as f64) * 100.0).min(100.0))
            }
            _ => None,
        };
        emit_skill_operation_progress_event(&RuntimeSkillOperationProgressEvent {
            operation_id: self.operation_id.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
            plane: self.plane,
            action: self.action,
            phase: detail.phase.to_string(),
            status: detail.status.to_string(),
            skill_id: detail
                .skill_id
                .map(ToOwned::to_owned)
                .or_else(|| self.skill_id.clone()),
            root_name: self.root_name.clone(),
            source_type: detail.source_type,
            source_locator: detail.source_locator.map(ToOwned::to_owned),
            bytes_done: detail.bytes_done,
            bytes_total,
            percent,
            message: detail.message,
        });
    }

    /// Build a download progress callback bound to this lifecycle operation.
    /// 构造绑定到当前生命周期操作的下载进度回调。
    pub(crate) fn download_callback(
        &self,
        source_type: SkillInstallSourceType,
        skill_id: String,
    ) -> DownloadProgressCallback {
        let emitter = self.clone();
        Arc::new(move |progress: &DownloadProgress| {
            emitter.emit_detail(RuntimeSkillOperationProgressDetail {
                phase: "downloading_archive",
                status: if progress.cached {
                    "cached"
                } else {
                    "progress"
                },
                skill_id: Some(skill_id.as_str()),
                source_type: Some(source_type),
                source_locator: Some(progress.source_locator.as_str()),
                bytes_done: Some(progress.bytes_done),
                bytes_total: progress.bytes_total,
                message: if progress.cached {
                    Some("download cache hit".to_string())
                } else {
                    None
                },
            });
        })
    }
}

/// Borrowed detail object used to build one progress event without a long parameter list.
/// 用于构造单条进度事件并避免冗长参数列表的借用详情对象。
pub(crate) struct RuntimeSkillOperationProgressDetail<'a> {
    /// Machine-readable progress phase.
    /// 机器可读进度阶段。
    pub phase: &'a str,
    /// Machine-readable progress status.
    /// 机器可读进度状态。
    pub status: &'a str,
    /// Optional phase-specific skill id.
    /// 阶段特定的可选技能标识符。
    pub skill_id: Option<&'a str>,
    /// Optional phase-specific source type.
    /// 阶段特定的可选来源类型。
    pub source_type: Option<SkillInstallSourceType>,
    /// Optional phase-specific source locator.
    /// 阶段特定的可选来源定位值。
    pub source_locator: Option<&'a str>,
    /// Optional downloaded byte count.
    /// 可选已下载字节数。
    pub bytes_done: Option<u64>,
    /// Optional total byte count.
    /// 可选总字节数。
    pub bytes_total: Option<u64>,
    /// Optional human-readable message.
    /// 可选人类可读消息。
    pub message: Option<String>,
}

/// Structured entry-registry delta emitted when one reload changes exposed runtime entries.
/// 当一次重载改变已暴露运行时入口时发出的结构化入口注册表差异。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeEntryRegistryDelta {
    /// Newly added runtime entries after the latest reload.
    /// 最近一次重载后新增的运行时入口。
    pub added_entries: Vec<RuntimeEntryDescriptor>,
    /// Canonical names removed after the latest reload.
    /// 最近一次重载后移除的 canonical 入口名称。
    pub removed_entry_names: Vec<String>,
    /// Existing canonical entries whose structure changed after the latest reload.
    /// 最近一次重载后结构发生变化的既有 canonical 入口。
    pub updated_entries: Vec<RuntimeEntryDescriptor>,
}

/// Acquire one process-wide callback registry lock and return its guard, recovering after another registry operation panics while holding it.
/// 获取并返回单个进程级回调注册表锁；如果其它注册表操作持锁 panic，则恢复注册表继续使用。
fn lock_callback_registry<T>(
    registry: &'static Mutex<Option<T>>,
) -> MutexGuard<'static, Option<T>> {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Replace the callback stored inside one process-wide callback registry.
/// 替换单个进程级回调注册表中保存的回调。
/// `registry` selects the concrete process-wide callback storage, and `callback` is the new value.
/// `registry` 选择具体的进程级回调存储，`callback` 是新的回调值。
fn set_callback_registry_value<T>(registry: &'static Mutex<Option<T>>, callback: Option<T>) {
    let mut guard = lock_callback_registry(registry);
    *guard = callback;
}

/// Clone the callback currently stored inside one process-wide callback registry.
/// 克隆单个进程级回调注册表中当前保存的回调。
/// `registry` selects the concrete process-wide callback storage.
/// `registry` 选择具体的进程级回调存储。
/// Returns the cloned callback option so callers can release the lock before invoking host code.
/// 返回克隆后的可选回调，使调用方可以在执行宿主代码前释放锁。
fn clone_callback_registry_value<T: Clone>(registry: &'static Mutex<Option<T>>) -> Option<T> {
    lock_callback_registry(registry).clone()
}

/// Install or clear the process-wide skill-lifecycle callback used by the host.
/// 安装或清理供宿主使用的进程级技能生命周期回调。
pub fn set_skill_lifecycle_callback(callback: Option<RuntimeSkillLifecycleCallback>) {
    set_callback_registry_value(skill_lifecycle_callback_registry(), callback);
}

/// Install or clear the process-wide skill-operation progress callback used by the host.
/// 安装或清理供宿主使用的进程级技能操作进度回调。
pub fn set_skill_operation_progress_callback(
    callback: Option<RuntimeSkillOperationProgressCallback>,
) {
    set_callback_registry_value(skill_operation_progress_callback_registry(), callback);
}

/// Install or clear the process-wide entry-registry callback used by the host.
/// 安装或清理供宿主使用的进程级入口注册表回调。
pub fn set_entry_registry_callback(callback: Option<RuntimeEntryRegistryCallback>) {
    set_callback_registry_value(entry_registry_callback_registry(), callback);
}

/// Install or clear the process-wide Lua-triggered skill-management callback used by the host.
/// 安装或清理由宿主使用的进程级 Lua 触发技能管理回调。
pub fn set_skill_management_callback(callback: Option<RuntimeSkillManagementCallback>) {
    set_callback_registry_value(skill_management_callback_registry(), callback);
}

/// Install or clear the process-wide Lua-triggered host-tool callback used by the host.
/// 安装或清理由宿主使用的进程级 Lua 触发宿主工具回调。
pub fn set_host_tool_callback(callback: Option<RuntimeHostToolCallback>) {
    set_callback_registry_value(host_tool_callback_registry(), callback);
}

/// Install or clear the process-wide standard model embedding callback used by the host.
/// 安装或清理由宿主使用的进程级标准模型 embedding 回调。
pub fn set_model_embed_callback(callback: Option<RuntimeModelEmbedCallback>) {
    set_callback_registry_value(model_embed_callback_registry(), callback);
}

/// Install or clear the process-wide standard non-streaming LLM callback used by the host.
/// 安装或清理由宿主使用的进程级标准非流式 LLM 回调。
pub fn set_model_llm_callback(callback: Option<RuntimeModelLlmCallback>) {
    set_callback_registry_value(model_llm_callback_registry(), callback);
}

/// Guard one process-wide model callback test and clear global callback state on drop.
/// 保护单个进程级模型回调测试，并在释放时清理全局回调状态。
#[cfg(test)]
pub(crate) struct RuntimeModelCallbackTestGuard {
    /// Hold the process-wide mutex guard until the current test finishes.
    /// 持有进程级互斥锁直到当前测试结束。
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for RuntimeModelCallbackTestGuard {
    /// Clear global model callbacks when one guarded test finishes.
    /// 当受保护测试结束时清理全局模型回调。
    fn drop(&mut self) {
        set_model_embed_callback(None);
        set_model_llm_callback(None);
    }
}

/// Acquire the process-wide model callback test guard.
/// 获取进程级模型回调测试保护锁。
#[cfg(test)]
pub(crate) fn runtime_model_callback_test_guard() -> RuntimeModelCallbackTestGuard {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("lock model callback test guard");
    set_model_embed_callback(None);
    set_model_llm_callback(None);
    RuntimeModelCallbackTestGuard { _guard: guard }
}

/// Emit one skill-lifecycle event to the currently registered host callback when it exists.
/// 当宿主已注册回调时向其发送一条技能生命周期事件。
pub(crate) fn emit_skill_lifecycle_event(event: &RuntimeSkillLifecycleEvent) {
    let callback = clone_callback_registry_value(skill_lifecycle_callback_registry());
    if let Some(callback) = callback {
        callback(event);
    }
}

/// Emit one skill-operation progress event to the currently registered host callback when it exists.
/// 当宿主已注册回调时向其发送一条技能操作进度事件。
pub(crate) fn emit_skill_operation_progress_event(event: &RuntimeSkillOperationProgressEvent) {
    let callback = clone_callback_registry_value(skill_operation_progress_callback_registry());
    if let Some(callback) = callback {
        callback(event);
    }
}

/// Emit one entry-registry delta to the currently registered host callback when it exists.
/// 当宿主已注册回调时向其发送一条入口注册表差异事件。
pub(crate) fn emit_entry_registry_delta(delta: &RuntimeEntryRegistryDelta) {
    let callback = clone_callback_registry_value(entry_registry_callback_registry());
    if let Some(callback) = callback {
        callback(delta);
    }
}

/// Dispatch one Lua-triggered skill-management request into the currently registered host callback.
/// 将单个 Lua 触发的技能管理请求分发给当前已注册的宿主回调。
pub(crate) fn dispatch_skill_management_request(
    request: &RuntimeSkillManagementRequest,
) -> Result<Value, String> {
    let callback = clone_callback_registry_value(skill_management_callback_registry());
    let callback = callback.ok_or_else(|| {
        "Runtime skill management bridge is enabled but no host callback is registered".to_string()
    })?;
    callback(request)
}

/// Dispatch one Lua-triggered host-tool request into the currently registered host callback.
/// 将单个 Lua 触发的宿主工具请求分发给当前已注册的宿主回调。
pub(crate) fn dispatch_host_tool_request(
    request: &RuntimeHostToolRequest,
) -> Result<Value, String> {
    let callback = clone_callback_registry_value(host_tool_callback_registry());
    let callback = callback.ok_or_else(|| {
        "Host tool bridge is enabled but no host callback is registered".to_string()
    })?;
    callback(request)
}

/// Build a model unavailable error for a missing capability callback.
/// 为缺失能力回调构造模型不可用错误。
fn model_unavailable_error(message: impl Into<String>) -> RuntimeModelError {
    RuntimeModelError {
        code: RuntimeModelErrorCode::ModelUnavailable,
        message: message.into(),
        provider_message: None,
        provider_code: None,
        provider_status: None,
    }
}

/// Dispatch one standard embedding request into the currently registered host callback.
/// 将单个标准 embedding 请求分发给当前已注册的宿主回调。
pub(crate) fn dispatch_model_embed_request(
    request: &RuntimeModelEmbedRequest,
) -> Result<RuntimeModelEmbedResponse, RuntimeModelError> {
    let callback = clone_callback_registry_value(model_embed_callback_registry());
    let callback =
        callback.ok_or_else(|| model_unavailable_error("embedding callback is not registered"))?;
    callback(request)
}

/// Dispatch one standard non-streaming LLM request into the currently registered host callback.
/// 将单个标准非流式 LLM 请求分发给当前已注册的宿主回调。
pub(crate) fn dispatch_model_llm_request(
    request: &RuntimeModelLlmRequest,
) -> Result<RuntimeModelLlmResponse, RuntimeModelError> {
    let callback = clone_callback_registry_value(model_llm_callback_registry());
    let callback =
        callback.ok_or_else(|| model_unavailable_error("llm callback is not registered"))?;
    callback(request)
}

/// Return whether one host callback is currently registered for runtime skill-management dispatch.
/// 返回当前是否已为运行时技能管理分发注册宿主回调。
pub(crate) fn try_has_skill_management_callback() -> bool {
    clone_callback_registry_value(skill_management_callback_registry()).is_some()
}

/// Return whether one host callback is currently registered for host-tool dispatch.
/// 返回当前是否已为宿主工具分发注册宿主回调。
pub(crate) fn try_has_host_tool_callback() -> bool {
    clone_callback_registry_value(host_tool_callback_registry()).is_some()
}

/// Return whether one host callback is currently registered for standard embedding dispatch.
/// 返回当前是否已为标准 embedding 分发注册宿主回调。
pub(crate) fn try_has_model_embed_callback() -> bool {
    clone_callback_registry_value(model_embed_callback_registry()).is_some()
}

/// Return whether one host callback is currently registered for standard LLM dispatch.
/// 返回当前是否已为标准 LLM 分发注册宿主回调。
pub(crate) fn try_has_model_llm_callback() -> bool {
    clone_callback_registry_value(model_llm_callback_registry()).is_some()
}

/// Build one host-visible skill operation id from action, skill id, and current time.
/// 根据动作、技能标识符与当前时间构造一个宿主可见的技能操作标识符。
///
/// The action parameter is the lifecycle action encoded into the operation id.
/// action 参数是编码进操作标识符的生命周期动作。
///
/// The skill_id parameter is the optional skill identifier encoded into the operation id.
/// skill_id 参数是编码进操作标识符的可选技能标识符。
///
/// Returns the stable operation id for the current lifecycle operation.
/// 返回当前生命周期操作使用的稳定操作标识符。
///
/// Returns an error when the operation id timestamp cannot be represented.
/// 当操作标识符时间戳无法表示时返回错误。
fn build_skill_operation_id(
    action: SkillLifecycleAction,
    skill_id: Option<&str>,
) -> Result<String, String> {
    let action_name = match action {
        SkillLifecycleAction::Install => "install",
        SkillLifecycleAction::Update => "update",
        SkillLifecycleAction::Reload => "reload",
        SkillLifecycleAction::Uninstall => "uninstall",
        SkillLifecycleAction::Enable => "enable",
        SkillLifecycleAction::Disable => "disable",
    };
    let skill_fragment = skill_id
        .map(|value| {
            value
                .chars()
                .map(|ch| match ch {
                    'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
                    _ => '-',
                })
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    // Wall-clock timestamp component included in the host-visible operation id.
    // 包含在宿主可见操作标识符中的墙钟时间戳组成部分。
    let timestamp = system_time_to_skill_operation_unix_millis(
        SystemTime::now(),
        "skill operation progress id timestamp",
    )?;
    Ok(format!(
        "skill-{}-{}-{}",
        action_name, skill_fragment, timestamp
    ))
}

/// Convert one system time into the Unix millisecond component used by skill operation ids.
/// 将单个系统时间转换为技能操作标识符使用的 Unix 毫秒组成部分。
///
/// The time parameter is the wall-clock timestamp to encode into the operation id.
/// time 参数是要编码进操作标识符的墙钟时间戳。
///
/// The context parameter names the caller for precise error diagnostics.
/// context 参数命名调用方，用于精确错误诊断。
///
/// Returns the Unix millisecond timestamp used in the skill operation id.
/// 返回技能操作标识符使用的 Unix 毫秒时间戳。
///
/// Returns an error when the timestamp is before the Unix epoch.
/// 当时间戳早于 Unix epoch 时返回错误。
fn system_time_to_skill_operation_unix_millis(
    time: SystemTime,
    context: &str,
) -> Result<u128, String> {
    // Duration measured from the Unix epoch for the skill operation id timestamp.
    // 技能操作标识符时间戳相对于 Unix epoch 的持续时间。
    let duration = time.duration_since(UNIX_EPOCH).map_err(|error| {
        format!(
            "{} is before Unix epoch and cannot be used for a skill operation id: {}",
            context, error
        )
    })?;
    Ok(duration.as_millis())
}

/// Return the process-wide lifecycle callback storage.
/// 返回进程级生命周期回调存储。
fn skill_lifecycle_callback_registry() -> &'static Mutex<Option<RuntimeSkillLifecycleCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeSkillLifecycleCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide skill-operation progress callback storage.
/// 返回进程级技能操作进度回调存储。
fn skill_operation_progress_callback_registry()
-> &'static Mutex<Option<RuntimeSkillOperationProgressCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeSkillOperationProgressCallback>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide entry-registry callback storage.
/// 返回进程级入口注册表回调存储。
fn entry_registry_callback_registry() -> &'static Mutex<Option<RuntimeEntryRegistryCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeEntryRegistryCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide skill-management callback storage.
/// 返回进程级技能管理回调存储。
fn skill_management_callback_registry() -> &'static Mutex<Option<RuntimeSkillManagementCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeSkillManagementCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide host-tool callback storage.
/// 返回进程级宿主工具回调存储。
fn host_tool_callback_registry() -> &'static Mutex<Option<RuntimeHostToolCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeHostToolCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide standard embedding callback storage.
/// 返回进程级标准 embedding 回调存储。
fn model_embed_callback_registry() -> &'static Mutex<Option<RuntimeModelEmbedCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeModelEmbedCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide standard LLM callback storage.
/// 返回进程级标准 LLM 回调存储。
fn model_llm_callback_registry() -> &'static Mutex<Option<RuntimeModelLlmCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeModelLlmCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeSkillOperationProgressCallback, RuntimeSkillOperationProgressDetail,
        RuntimeSkillOperationProgressEmitter, RuntimeSkillOperationProgressEvent,
        set_skill_operation_progress_callback, skill_operation_progress_callback_registry,
        system_time_to_skill_operation_unix_millis,
    };
    use crate::skill::manager::{SkillLifecycleAction, SkillOperationPlane};
    use crate::skill::source::SkillInstallSourceType;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, UNIX_EPOCH};

    /// Return one shared guard that serializes tests touching the global progress callback.
    /// 返回一把用于串行化访问全局进度回调的共享测试锁。
    fn progress_callback_test_guard() -> MutexGuard<'static, ()> {
        static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        match TEST_MUTEX.get_or_init(|| Mutex::new(())).lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Verify skill operation id timestamps accept normal post-epoch system times.
    /// 验证技能操作标识符时间戳会接受正常的 epoch 之后系统时间。
    #[test]
    fn skill_operation_unix_millis_accepts_post_epoch_time() {
        // Timestamp one millisecond after the Unix epoch.
        // Unix epoch 之后一毫秒的时间戳。
        let timestamp = UNIX_EPOCH + Duration::from_millis(1);

        assert_eq!(
            system_time_to_skill_operation_unix_millis(
                timestamp,
                "test skill operation id timestamp"
            )
            .expect("post-epoch timestamp should convert"),
            1
        );
    }

    /// Verify skill operation id timestamps reject pre-epoch system times.
    /// 验证技能操作标识符时间戳会拒绝早于 epoch 的系统时间。
    #[test]
    fn skill_operation_unix_millis_rejects_pre_epoch_time() {
        // Timestamp one millisecond before the Unix epoch.
        // Unix epoch 之前一毫秒的时间戳。
        let timestamp = UNIX_EPOCH - Duration::from_millis(1);

        // Error returned for a pre-epoch skill operation id timestamp conversion attempt.
        // 早于 epoch 的技能操作标识符时间戳转换尝试返回的错误。
        let error = system_time_to_skill_operation_unix_millis(
            timestamp,
            "test skill operation id timestamp",
        )
        .expect_err("pre-epoch timestamp should fail");

        assert!(
            error.starts_with(
                "test skill operation id timestamp is before Unix epoch and cannot be used for a skill operation id:"
            ),
            "unexpected error: {}",
            error
        );
    }

    /// Verify progress emitters preserve operation ordering and compute determinate percentages.
    /// 验证进度发射器会保持操作内顺序并计算确定性百分比。
    #[test]
    fn progress_emitter_reports_sequence_and_percent() {
        let _guard = progress_callback_test_guard();
        let captured = Arc::new(Mutex::new(Vec::<RuntimeSkillOperationProgressEvent>::new()));
        let callback_events = captured.clone();
        let callback: RuntimeSkillOperationProgressCallback =
            Arc::new(move |event: &RuntimeSkillOperationProgressEvent| {
                callback_events
                    .lock()
                    .expect("capture progress events")
                    .push(event.clone());
            });
        set_skill_operation_progress_callback(Some(callback));

        let emitter = RuntimeSkillOperationProgressEmitter::new(
            SkillOperationPlane::System,
            SkillLifecycleAction::Install,
            Some("ROOT".to_string()),
            Some("demo-skill".to_string()),
        )
        .expect("progress emitter should be created");
        emitter.emit_detail(RuntimeSkillOperationProgressDetail {
            phase: "downloading_archive",
            status: "progress",
            skill_id: Some("demo-skill"),
            source_type: Some(SkillInstallSourceType::OfficialHub),
            source_locator: Some("https://hub.example.invalid/demo.zip"),
            bytes_done: Some(5),
            bytes_total: Some(10),
            message: None,
        });
        emitter.emit("completed", "completed", Some("done".to_string()));
        set_skill_operation_progress_callback(None);

        let events = captured.lock().expect("read captured progress events");
        let operation_id = events
            .iter()
            .find(|event| {
                event.skill_id.as_deref() == Some("demo-skill")
                    && event.phase == "downloading_archive"
            })
            .expect("download progress event should be captured")
            .operation_id
            .clone();
        let operation_events = events
            .iter()
            .filter(|event| event.operation_id == operation_id)
            .collect::<Vec<_>>();
        assert_eq!(operation_events.len(), 2);
        assert_eq!(operation_events[0].sequence, 1);
        assert_eq!(operation_events[1].sequence, 2);
        assert_eq!(operation_events[0].percent, Some(50.0));
        assert_eq!(
            operation_events[0].source_type,
            Some(SkillInstallSourceType::OfficialHub)
        );
    }

    /// Verify the progress callback registry remains writable and readable after its lock is poisoned.
    /// 验证进度回调注册表锁 poison 后仍可写入并读取。
    #[test]
    fn progress_callback_registry_recovers_after_poisoned_lock() {
        let _guard = progress_callback_test_guard();
        set_skill_operation_progress_callback(None);

        // Captured panic result from a registry writer that poisons the progress callback lock.
        // 进度回调注册表写入者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the process-wide progress callback registry.
            // 仅用于制造进程级进度回调注册表 poison 的保护对象。
            let _registry_guard = skill_operation_progress_callback_registry()
                .lock()
                .expect("initial progress callback registry lock");
            panic!("poison progress callback registry for recovery test");
        }));

        assert!(poison_result.is_err());

        // Captured progress events emitted through the recovered registry callback.
        // 通过已恢复注册表回调捕获到的进度事件。
        let captured = Arc::new(Mutex::new(Vec::<RuntimeSkillOperationProgressEvent>::new()));
        // Shared event buffer moved into the callback closure.
        // 移入回调闭包的共享事件缓冲区。
        let callback_events = captured.clone();
        // Progress callback installed after registry poisoning to prove setter recovery.
        // 在注册表 poison 后安装的进度回调，用于证明 setter 已恢复。
        let callback: RuntimeSkillOperationProgressCallback =
            Arc::new(move |event: &RuntimeSkillOperationProgressEvent| {
                callback_events
                    .lock()
                    .expect("capture recovered progress event")
                    .push(event.clone());
            });
        set_skill_operation_progress_callback(Some(callback));

        // Progress emitter used to prove clone-and-emit recovery after the setter succeeds.
        // 用于证明 setter 成功后 clone 与 emit 路径也已恢复的进度发射器。
        let emitter = RuntimeSkillOperationProgressEmitter::new(
            SkillOperationPlane::System,
            SkillLifecycleAction::Reload,
            Some("ROOT".to_string()),
            Some("demo-skill".to_string()),
        )
        .expect("progress emitter should be created after registry recovery");
        emitter.emit(
            "registry_recovered",
            "completed",
            Some("recovered".to_string()),
        );
        set_skill_operation_progress_callback(None);

        // Event list captured after emitting through the recovered global registry.
        // 通过已恢复全局注册表发射后捕获的事件列表。
        let events = captured
            .lock()
            .expect("read recovered progress callback events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].phase, "registry_recovered");
        assert_eq!(events[0].status, "completed");
        assert_eq!(events[0].skill_id.as_deref(), Some("demo-skill"));
    }
}
