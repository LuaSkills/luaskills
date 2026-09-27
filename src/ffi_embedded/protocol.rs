use super::{EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus, transport::Transport};
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
    /// Inspect effective transport limits and implemented protocol commands.
    /// 查看有效传输限制与已实现协议命令。
    Describe {},
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
    match request.command {
        Command::Describe {} => encode(
            &serde_json::json!({
                "protocol_version": EMBEDDED_FFI_PROTOCOL_VERSION,
                "status": "ok",
                "result": {
                    "core_version": env!("CARGO_PKG_VERSION"),
                    "protocol_version": EMBEDDED_FFI_PROTOCOL_VERSION,
                    "abi_structure_version": EMBEDDED_FFI_PROTOCOL_VERSION,
                    "commands": ["describe"],
                    "limits": transport.config,
                },
            }),
            transport.config.max_response_bytes,
        ),
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
