use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable machine-readable failures shared by Rust and the versioned FFI protocol.
/// Rust 与版本化 FFI 协议共享的稳定机器可读错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddedErrorCode {
    /// The supplied configuration or request violates its declared contract.
    /// 提供的配置或请求违反其声明的契约。
    InvalidArgument,
    /// The requested identity does not exist or its retained record has expired.
    /// 请求的身份不存在，或其保留记录已经过期。
    NotFound,
    /// The caller supplied an identity from an inactive generation.
    /// 调用方提供了非活动代次的身份。
    StaleGeneration,
    /// A bounded resource cannot admit more work.
    /// 有界资源无法接纳更多工作。
    CapacityExceeded,
    /// The requested state transition conflicts with live work.
    /// 请求的状态变更与仍在运行的工作冲突。
    Busy,
    /// This exact request already has a completion owner or retained terminal result.
    /// 此精确请求已具有完成所有者或保留的终态结果。
    AlreadyCompleted,
    /// The runtime or registration no longer accepts work.
    /// 运行时或注册项已停止接纳工作。
    Closed,
    /// The caller requested cooperative cancellation.
    /// 调用方请求了协作取消。
    Cancelled,
    /// The original end-to-end execution budget expired.
    /// 原始端到端执行预算已经耗尽。
    DeadlineExceeded,
    /// The trusted host did not grant the requested capability.
    /// 可信宿主未授予所请求的能力。
    PermissionDenied,
    /// The requested backend or protocol feature is unavailable.
    /// 请求的后端或协议功能不可用。
    Unsupported,
    /// Plugin initialization or execution failed.
    /// 插件初始化或执行失败。
    ExecutionFailed,
    /// Resource teardown failed and still owns its capacity.
    /// 资源清理失败且仍占有容量。
    CleanupFailed,
    /// An internal invariant failed; the caller must not retry mutations blindly.
    /// 内部不变量失败，调用方不得盲目重试有副作用的操作。
    Internal,
}

/// Structured error; `code` is stable and `message` is an English diagnostic.
/// 结构化错误；`code` 稳定，`message` 为英文诊断信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedError {
    /// Stable classification consumed by SDKs instead of parsing text.
    /// 供 SDK 使用的稳定分类，避免解析文案。
    pub code: EmbeddedErrorCode,
    /// Human-readable detail without credentials or plugin input dumps.
    /// 不含凭据或插件输入转储的可读详情。
    pub message: String,
}

/// Common result type for the embedded plugin runtime.
/// 嵌入式插件运行时的通用结果类型。
pub type EmbeddedResult<T> = Result<T, EmbeddedError>;

impl EmbeddedError {
    /// Build an error from a stable `code` and owned diagnostic `message`.
    /// 根据稳定的 `code` 与拥有所有权的诊断 `message` 构造错误。
    pub fn new(code: EmbeddedErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Return an invalid-argument error describing the supplied `message`.
    /// 返回由给定 `message` 描述的参数无效错误。
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(EmbeddedErrorCode::InvalidArgument, message)
    }
}

impl fmt::Display for EmbeddedError {
    /// Render the diagnostic into `formatter`, returning its formatting status.
    /// 将诊断写入 `formatter`，返回格式化结果。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for EmbeddedError {}
