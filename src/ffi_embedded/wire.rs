use super::types::TransportConfig;
use serde::Serialize;

/// Single legal success discriminator, shared by live encoding and derived contracts.
/// 实际编码及派生契约共享的唯一合法成功判别。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) enum SuccessStatus {
    /// Successfully delivered result, including explicit JSON null.
    /// 已成功交付结果，包含显式 JSON 空值。
    #[serde(rename = "ok")]
    Ok,
}

/// Single legal business-error discriminator, independent of native transport failures.
/// 独立于原生传输失败的唯一合法业务错误判别。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) enum ErrorStatus {
    /// Delivered structured core failure.
    /// 已交付结构化核心失败。
    #[serde(rename = "error")]
    Error,
}

/// Live structured failure envelope whose shape also drives SDK generation.
/// 同时驱动 SDK 生成的实际结构化失败信封。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct ErrorEnvelope<'a> {
    /// Accepted embedded protocol version.
    /// 接受的嵌入式协议版本。
    pub(super) protocol_version: u32,
    /// Exact business-error discriminator.
    /// 精确业务错误判别。
    pub(super) status: ErrorStatus,
    /// Borrowed core error retained until response encoding returns.
    /// 保留到响应编码返回的借用核心错误。
    pub(super) error: &'a crate::runtime::embedded::EmbeddedError,
}

/// Exact lifecycle identity returned before or after a runtime control mutation.
/// 在运行时控制变更前或后返回的精确生命周期身份。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct RuntimeReceipt {
    /// Immutable transport-local runtime identity.
    /// 不可变传输局部运行时身份。
    pub(super) runtime_id: String,
}

/// Actual pool registration acknowledgement shared by capacity preparation and publication.
/// 容量准备及发布共享的实际池注册确认。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct PoolReceipt {
    /// Immutable core pool identity.
    /// 不可变核心池身份。
    pub(super) pool_id: String,
}

/// Actual operation admission acknowledgement; it does not imply execution completion.
/// 实际操作入场确认；不代表执行完成。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct OperationReceipt {
    /// Retained queryable operation identity.
    /// 保留且可查询的操作身份。
    pub(super) operation_id: String,
}

/// Fixed-session reservation and its independently queryable initialization operation.
/// 固定会话预留及其可独立查询的初始化操作。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct SessionReceipt {
    /// Immutable fixed-session identity.
    /// 不可变固定会话身份。
    pub(super) session_id: String,
    /// Initialization operation whose actual outcome must be observed separately.
    /// 必须单独观察实际结果的初始化操作。
    pub(super) operation_id: String,
}

/// Exact identities from one atomic host capability publication.
/// 单次原子宿主能力发布的精确身份。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct RegistrationReceipt {
    /// Identities retain the descriptor batch's original order.
    /// 身份保留描述符批次的原始顺序。
    pub(super) registration_ids: Vec<String>,
}

/// Implemented transport description; every field comes from the running core's own authority.
/// 已实现传输描述；每个字段均来自运行核心自身权威。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct TransportDescription<'a> {
    /// Cargo package version of this exact library build.
    /// 此精确动态库构建的 Cargo 包版本。
    pub(super) core_version: &'static str,
    /// Version of the accepted embedded JSON protocol.
    /// 接受的嵌入式 JSON 协议版本。
    pub(super) protocol_version: u32,
    /// Version of the independent embedded ABI structures.
    /// 独立嵌入式 ABI 结构版本。
    pub(super) abi_structure_version: u32,
    /// Root commands implemented by the exhaustive dispatcher.
    /// 穷尽分发器实现的根命令。
    pub(super) commands: &'static [&'static str],
    /// Runtime operations implemented by the exhaustive dispatcher.
    /// 穷尽分发器实现的运行时操作。
    pub(super) runtime_commands: &'static [&'static str],
    /// Actual validated transport limits supplied at construction.
    /// 构造时提供的实际已校验传输边界。
    pub(super) limits: &'a TransportConfig,
}
