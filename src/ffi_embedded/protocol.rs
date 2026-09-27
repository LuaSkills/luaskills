use super::commands::{RUNTIME_COMMAND_NAMES, RuntimeCommand};
use super::runtime::RuntimeSlot;
use super::{EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus, transport::Transport};
use crate::{
    LuaEngineOptions,
    runtime::embedded::{EmbeddedResult, EmbeddedRuntimeConfig},
};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

/// Strict versioned request; unknown fields and commands are explicit protocol errors.
/// 严格版本化请求；未知字段与命令是明确协议错误。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    /// Explicit wire version, checked before dispatch.
    /// 分发前检查的显式线协议版本。
    protocol_version: u32,
    /// One typed operation; no legacy envelope aliases are inferred.
    /// 一个类型化操作；不推断旧信封别名。
    command: Command,
}

/// Implemented commands only; new runtime commands are advertised when actually wired.
/// 仅包含已实现命令；新的运行时命令在实际接通后才公布。
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    /// Execute one typed operation on an exact initialized runtime.
    /// 在精确已初始化运行时上执行一个类型化操作。
    Runtime {
        /// Exact transport-local runtime identity.
        /// 精确传输局部运行时身份。
        runtime_id: String,
        /// Typed core operation with no legacy command aliases.
        /// 不含旧命令别名的类型化核心操作。
        operation: Box<RuntimeCommand>,
    },
    /// Inspect effective transport limits and implemented protocol commands.
    /// 查看有效传输限制与已实现协议命令。
    Describe {},
    /// Allocate a bounded metadata-only runtime identity before construction can begin.
    /// 在构造能够开始前分配有界且仅含元数据的运行时身份。
    RuntimeReserve {},
    /// Attempt construction exactly once; the existing identity retains the actual outcome for query.
    /// 精确尝试构造一次；已有身份保留实际结果供查询。
    RuntimeInitialize {
        /// Exact identity returned by runtime_reserve in this transport.
        /// 此传输中 runtime_reserve 返回的精确身份。
        runtime_id: String,
        /// Explicit core engine options, using the existing engine option contract.
        /// 显式核心引擎选项，使用现有引擎选项契约。
        engine_options: Box<LuaEngineOptions>,
        /// Explicit formal runtime budgets validated before worker construction.
        /// 工作线程构造前校验的显式正式运行时预算。
        runtime_config: Box<EmbeddedRuntimeConfig>,
    },
    /// Read construction outcome and actual worker closure evidence.
    /// 读取构造结果与实际工作线程关闭证据。
    RuntimeStatus {
        /// Exact retained runtime identity in this transport.
        /// 此传输中保留的精确运行时身份。
        runtime_id: String,
    },
    /// Close existing admission, including a construction attempt that is still running.
    /// 关闭已有入场，包含仍在运行的构造尝试。
    RuntimeClose {
        /// Exact retained runtime identity in this transport.
        /// 此传输中保留的精确运行时身份。
        runtime_id: String,
    },
    /// Remove an explicitly closed and fully drained runtime registration.
    /// 移除显式关闭且完全排空的运行时注册。
    RuntimeFree {
        /// Exact retained runtime identity in this transport.
        /// 此传输中保留的精确运行时身份。
        runtime_id: String,
    },
}

/// Bounded serializer sink; a host budget limits growth while bytes are produced.
/// 有界序列化写入器；宿主预算在字节生成时限制增长。
struct ResponseWriter {
    /// Initialized response bytes, never exceeding `limit`.
    /// 已初始化响应字节，绝不超过 `limit`。
    bytes: Vec<u8>,
    /// Effective per-response byte ceiling.
    /// 有效逐响应字节上限。
    limit: usize,
    /// Exact transport cause preserved independently of serde's error text.
    /// 独立于 serde 错误文本保留的精确传输原因。
    failure: Option<EmbeddedFfiStatus>,
}

impl Write for ResponseWriter {
    /// Append `bytes` completely within the budget or return an error without a partial write.
    /// 在预算内完整追加 `bytes`，否则返回错误且不部分写入。
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            self.failure = Some(EmbeddedFfiStatus::CapacityExceeded);
            return Err(io::Error::other(
                "FFI response exceeds the declared byte limit",
            ));
        }
        if self.bytes.try_reserve_exact(bytes.len()).is_err() {
            self.failure = Some(EmbeddedFfiStatus::CapacityExceeded);
            return Err(io::Error::other("FFI response allocation failed"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    /// Return success because this in-memory sink has no separately buffered output.
    /// 返回成功，因为此内存写入器没有单独缓冲的输出。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Parse bounded caller `bytes`; return a validated request or an explicit format/version failure.
/// 解析有界调用方 `bytes`；返回已校验请求或明确格式／版本失败。
pub(super) fn parse(bytes: &[u8]) -> Result<Request, EmbeddedFfiStatus> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| EmbeddedFfiStatus::InvalidArgument)?;
    if request.protocol_version != EMBEDDED_FFI_PROTOCOL_VERSION {
        return Err(EmbeddedFfiStatus::Unsupported);
    }
    Ok(request)
}

/// Execute one validated `request` for `transport`; serialize within its pre-admitted response ceiling.
/// 为 `transport` 执行一个已校验 `request`；在其预先接纳的响应上限内序列化。
pub(super) fn execute(
    transport: &Transport,
    request: Request,
) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    let limit = transport.config.max_response_bytes;
    match request.command {
        Command::Describe {} => respond(
            Ok(serde_json::json!({
                "core_version": env!("CARGO_PKG_VERSION"),
                "protocol_version": EMBEDDED_FFI_PROTOCOL_VERSION,
                "abi_structure_version": EMBEDDED_FFI_PROTOCOL_VERSION,
                "commands": ["describe", "runtime_reserve", "runtime_initialize", "runtime_status", "runtime_close", "runtime_free", "runtime"],
                "runtime_commands": RUNTIME_COMMAND_NAMES,
                "limits": transport.config,
            })),
            limit,
        ),
        Command::RuntimeReserve {} => {
            let slot = RuntimeSlot::new()?;
            // No native construction or registration can precede proof that its identity fits the response.
            // 在证明身份适合响应之前，不得发生原生构造或注册。
            let response = receipt(&slot.id, limit)?;
            match transport.register_runtime(slot) {
                Ok(()) => Ok(response),
                Err(error) => respond::<()>(Err(error), limit),
            }
        }
        Command::RuntimeInitialize {
            runtime_id,
            engine_options,
            runtime_config,
        } => {
            let response = receipt(&runtime_id, limit)?;
            match transport
                .runtime(&runtime_id)
                .and_then(|slot| slot.initialize(*engine_options, *runtime_config))
            {
                Ok(()) => Ok(response),
                Err(error) => respond::<()>(Err(error), limit),
            }
        }
        Command::RuntimeStatus { runtime_id } => respond(
            transport
                .runtime(&runtime_id)
                .and_then(|slot| slot.snapshot()),
            limit,
        ),
        Command::RuntimeClose { runtime_id } => {
            let response = receipt(&runtime_id, limit)?;
            match transport
                .runtime(&runtime_id)
                .and_then(|slot| slot.request_close())
            {
                Ok(()) => Ok(response),
                Err(error) => respond::<()>(Err(error), limit),
            }
        }
        Command::RuntimeFree { runtime_id } => {
            let response = receipt(&runtime_id, limit)?;
            match transport.release_runtime(&runtime_id) {
                Ok(()) => Ok(response),
                Err(error) => respond::<()>(Err(error), limit),
            }
        }
        Command::Runtime {
            runtime_id,
            operation,
        } => match transport.runtime(&runtime_id) {
            Ok(slot) => super::control::execute(&slot, *operation, limit),
            Err(error) => respond::<()>(Err(error), limit),
        },
    }
}

/// Encode a fixed acknowledgement for known `id` before any lifecycle mutation can occur.
/// 在任何生命周期变更能够发生前，为已知 `id` 编码固定确认。
fn receipt(id: &str, limit: usize) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    respond(Ok(serde_json::json!({ "runtime_id": id })), limit)
}

/// Encode the core `result` as an explicit success or structured failure within `limit`.
/// 在 `limit` 内将核心 `result` 编码为明确成功或结构化失败。
pub(super) fn respond<T: Serialize>(
    result: EmbeddedResult<T>,
    limit: usize,
) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    match result {
        Ok(result) => encode(
            &SuccessEnvelope {
                protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
                status: "ok",
                result: &result,
            },
            limit,
        ),
        Err(error) => encode(
            &serde_json::json!({
                "protocol_version": EMBEDDED_FFI_PROTOCOL_VERSION, "status": "error", "error": error,
            }),
            limit,
        ),
    }
}

/// Borrowed success envelope avoids cloning application output during native response publication.
/// 借用成功信封，避免原生响应发布期间克隆应用输出。
#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    /// Single protocol version authority.
    /// 唯一协议版本权威。
    protocol_version: u32,
    /// Exact success discriminator.
    /// 精确成功判别。
    status: &'static str,
    /// Borrowed result whose owner lives through serialization.
    /// 借用结果，其所有者跨序列化存活。
    result: &'a T,
}

/// Owned response allocation proved large enough before a command can mutate core state.
/// 在命令能够变更核心状态前，已证明足够大的拥有型响应分配。
pub(super) struct PreparedSuccess {
    /// The same allocation is reused for the actual success response after mutation.
    /// 变更后实际成功响应复用同一分配。
    writer: ResponseWriter,
}

impl PreparedSuccess {
    /// Encode worst-case `sample` within `limit` and retain its allocation for later publication.
    /// 在 `limit` 内编码最坏情况 `sample`，并保留其分配供稍后发布。
    pub(super) fn new(sample: &impl Serialize, limit: usize) -> Result<Self, EmbeddedFfiStatus> {
        let bytes = encode(
            &SuccessEnvelope {
                protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
                status: "ok",
                result: sample,
            },
            limit,
        )?;
        Ok(Self {
            writer: ResponseWriter {
                limit: bytes.len(),
                bytes,
                failure: None,
            },
        })
    }

    /// Serialize actual `result` into the retained allocation; exceeding the proven sample is an internal contract failure.
    /// 将实际 `result` 序列化到保留分配；超过已证明样本属于内部契约失败。
    pub(super) fn finish(mut self, result: &impl Serialize) -> Result<Vec<u8>, EmbeddedFfiStatus> {
        self.writer.bytes.clear();
        serde_json::to_writer(
            &mut self.writer,
            &SuccessEnvelope {
                protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
                status: "ok",
                result,
            },
        )
        .map_err(|_| EmbeddedFfiStatus::Internal)?;
        Ok(self.writer.bytes)
    }
}

/// Encode `response` without growing beyond `limit`; return owned bytes or the exact transport failure.
/// 编码 `response` 且增长不超过 `limit`；返回拥有所有权的字节或精确传输失败。
fn encode(response: &impl Serialize, limit: usize) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    let mut writer = ResponseWriter {
        bytes: Vec::new(),
        limit,
        failure: None,
    };
    serde_json::to_writer(&mut writer, response)
        .map_err(|_| writer.failure.unwrap_or(EmbeddedFfiStatus::Internal))?;
    Ok(writer.bytes)
}
