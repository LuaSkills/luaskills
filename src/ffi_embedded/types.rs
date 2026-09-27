use serde::Serialize;

/// Single authority for the new transport protocol version.
/// 新传输协议版本的唯一权威。
pub const EMBEDDED_FFI_PROTOCOL_VERSION: u32 = 1;

/// Stable native transport status; application failures use the structured JSON error envelope.
/// 稳定原生传输状态；应用失败使用结构化 JSON 错误信封。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum EmbeddedFfiStatus {
    /// A result was delivered or the requested lifecycle mutation succeeded.
    /// 结果已交付，或所请求生命周期变更成功。
    Ok = 0,
    /// Pointer shape, structure size, explicit budget or input length is invalid.
    /// 指针形状、结构大小、显式预算或输入长度无效。
    InvalidArgument = 1,
    /// The exact transport or result allocation identity is not retained.
    /// 精确传输或结果分配身份未保留。
    NotFound = 2,
    /// Actual requests, results or runtime ownership prevent release.
    /// 实际请求、结果或运行时所有权阻止释放。
    Busy = 3,
    /// Admission would exceed a configured count or byte limit.
    /// 入场会超过配置的数量或字节上限。
    CapacityExceeded = 4,
    /// This transport identity was permanently released.
    /// 此传输身份已永久释放。
    Closed = 5,
    /// A Rust panic, poisoned owner or exhausted identity prevents the operation.
    /// Rust panic、中毒所有者或身份耗尽阻止操作。
    Internal = 6,
    /// The declared protocol is not implemented by this library.
    /// 此动态库未实现声明的协议。
    Unsupported = 7,
}

/// Explicit version-one C transport budgets; hosts own their effective configuration.
/// 显式版本一 C 传输预算；宿主拥有其有效配置。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FfiEmbeddedTransportConfigV1 {
    /// Exact size of this structure in bytes, checked before reading the full value.
    /// 此结构的精确字节大小，在读取完整值之前校验。
    pub struct_size: u32,
    /// Must equal `EMBEDDED_FFI_PROTOCOL_VERSION`.
    /// 必须等于 `EMBEDDED_FFI_PROTOCOL_VERSION`。
    pub protocol_version: u32,
    /// Maximum simultaneously owned runtime registrations, including draining runtimes.
    /// 同时拥有的运行时注册数量上限，包含正在排空的运行时。
    pub max_runtimes: u64,
    /// Maximum owned response buffers and in-flight response reservations together.
    /// 拥有的响应缓冲与在途响应预留的合计数量上限。
    pub max_result_buffers: u64,
    /// Maximum exact published bytes plus worst-case bytes reserved before dispatch.
    /// 精确已发布字节与分发前预留最坏情况字节的合计上限。
    pub max_result_bytes: u64,
    /// Maximum bytes reserved for one response before its command can execute.
    /// 单个响应在其命令可以执行前预留的字节上限。
    pub max_response_bytes: u64,
    /// Maximum readable request bytes, checked before constructing a Rust slice.
    /// 最大可读请求字节数，在构造 Rust 切片前检查。
    pub max_request_bytes: u64,
}

/// Validated native-sized transport configuration, copied once from the host declaration.
/// 从宿主声明一次性复制的已校验原生大小传输配置。
#[derive(Debug, Clone, Serialize)]
pub(super) struct TransportConfig {
    /// Maximum retained runtime identities.
    /// 保留运行时身份数量上限。
    pub(super) max_runtimes: usize,
    /// Maximum response owners, including pre-dispatch reservations.
    /// 响应所有者数量上限，包含分发前预留。
    pub(super) max_result_buffers: usize,
    /// Aggregate response allocation budget.
    /// 聚合响应分配预算。
    pub(super) max_result_bytes: usize,
    /// Per-response pre-dispatch reservation.
    /// 逐响应分发前预留。
    pub(super) max_response_bytes: usize,
    /// Per-request byte limit.
    /// 逐请求字节上限。
    pub(super) max_request_bytes: usize,
}

impl TryFrom<FfiEmbeddedTransportConfigV1> for TransportConfig {
    type Error = EmbeddedFfiStatus;

    /// Validate host `value` without normalization; return native counters or a stable transport error.
    /// 校验宿主 `value`，不进行归一化；返回原生计数或稳定传输错误。
    fn try_from(value: FfiEmbeddedTransportConfigV1) -> Result<Self, Self::Error> {
        if value.protocol_version != EMBEDDED_FFI_PROTOCOL_VERSION {
            return Err(EmbeddedFfiStatus::Unsupported);
        }
        if value.struct_size as usize != std::mem::size_of::<FfiEmbeddedTransportConfigV1>() {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        let values = [
            value.max_runtimes,
            value.max_result_buffers,
            value.max_result_bytes,
            value.max_response_bytes,
            value.max_request_bytes,
        ]
        .map(|limit| {
            usize::try_from(limit)
                .ok()
                .filter(|limit| *limit > 0 && *limit <= isize::MAX as usize)
                .ok_or(EmbeddedFfiStatus::InvalidArgument)
        });
        let [runtimes, buffers, total, response, request] = values;
        let config = Self {
            max_runtimes: runtimes?,
            max_result_buffers: buffers?,
            max_result_bytes: total?,
            max_response_bytes: response?,
            max_request_bytes: request?,
        };
        if config.max_response_bytes > config.max_result_bytes {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        Ok(config)
    }
}

/// Read-only UTF-8 result allocation, released only by its exact transport and allocation identity.
/// 只读 UTF-8 结果分配，仅通过精确传输和分配身份释放。
/// The pointer is valid until successful release; copying this structure does not duplicate ownership.
/// 指针在成功释放前有效；复制此结构不会复制所有权。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FfiEmbeddedResultV1 {
    /// Exact library-owned byte address; never pass it to the legacy buffer free function.
    /// 动态库拥有的精确字节地址；绝不能交给旧缓冲释放函数。
    pub ptr: *const u8,
    /// Exact readable byte length, including embedded zero bytes when present.
    /// 精确可读字节长度，存在嵌入零字节时同样计入。
    pub len: usize,
    /// Never-reused identity within this loaded library's lifetime; bindings must preserve all 64 bits.
    /// 此次动态库加载寿命内绝不复用的身份；绑定必须保留全部 64 位。
    pub allocation_id: u64,
}

impl Default for FfiEmbeddedResultV1 {
    /// Return the unique empty output shape; it owns no allocation.
    /// 返回唯一空输出形状；它不拥有任何分配。
    fn default() -> Self {
        Self {
            ptr: std::ptr::null(),
            len: 0,
            allocation_id: 0,
        }
    }
}
