//! Reproducible offline schemas derived from the live embedded request and response types.
//! 从实际嵌入式请求和响应类型派生的可复现离线 Schema。

use super::commands::RUNTIME_COMMAND_NAMES;
use super::protocol::{ROOT_COMMAND_NAMES, Request, SuccessEnvelope};
use super::runtime::RuntimeSnapshot;
use super::wire::{ErrorEnvelope, RuntimeReceipt, TransportDescription};
use super::{EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[cfg(test)]
pub(crate) mod tests;

/// Hash source `text` after normalizing Git's platform-dependent line endings.
/// 对源码 `text` 归一化 Git 平台相关换行后计算摘要。
/// Return a lowercase SHA-256 identity independent of CRLF checkout conversion.
/// 返回独立于 CRLF 检出转换的小写 SHA-256 身份。
fn source_digest(text: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(text.replace("\r\n", "\n").as_bytes())
    )
}

/// Generate the serialize-direction success envelope for actual result type `T`.
/// 为实际结果类型 `T` 生成序列化方向的成功信封。
/// Return one standalone schema whose local references resolve within its own root.
/// 返回局部引用在其自身根内解析的一个独立 Schema。
pub(super) fn response<T: JsonSchema + Serialize>() -> Value {
    serde_json::to_value(
        SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator()
            .into_root_schema_for::<SuccessEnvelope<'_, T>>(),
    )
    .expect("schema serialization contains only JSON values")
}

/// Build the current compiler-checked wire contract; no engine, callback or network is started.
/// 构建当前经编译器校验的线契约；不启动引擎、回调或网络。
/// Return deterministic JSON with independently rooted request and response schemas.
/// 返回含独立根请求及响应 Schema 的确定性 JSON。
pub fn document() -> Value {
    // Request generation uses Deserialize semantics; responses use Serialize semantics, including omitted fields.
    // 请求生成使用反序列化语义；响应使用序列化语义，包含省略字段。
    let request = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<Request>();
    let error = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<ErrorEnvelope<'_>>();
    // The registry maps commands to real Rust result types; tests verify coverage against derived command discriminators.
    // 注册表将命令映射到实际 Rust 结果类型；测试对照派生命令判别验证覆盖范围。
    let runtime_responses = super::responses::schemas();
    json!({
        "contract_version": EMBEDDED_FFI_PROTOCOL_VERSION,
        "core_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": EMBEDDED_FFI_PROTOCOL_VERSION,
        "generator": {
            "name": "luaskills/generate_embedded_contract",
            "source_version": env!("CARGO_PKG_VERSION"),
            "source_sha256": source_digest(include_str!("contract.rs")),
            "cargo_lock_sha256": source_digest(include_str!("../../Cargo.lock")),
            "schema_draft": "2020-12",
        },
        "commands": ROOT_COMMAND_NAMES,
        "runtime_commands": RUNTIME_COMMAND_NAMES,
        "native_status": {
            "ok": EmbeddedFfiStatus::Ok as i32,
            "invalid_argument": EmbeddedFfiStatus::InvalidArgument as i32,
            "not_found": EmbeddedFfiStatus::NotFound as i32,
            "busy": EmbeddedFfiStatus::Busy as i32,
            "capacity_exceeded": EmbeddedFfiStatus::CapacityExceeded as i32,
            "closed": EmbeddedFfiStatus::Closed as i32,
            "internal": EmbeddedFfiStatus::Internal as i32,
            "unsupported": EmbeddedFfiStatus::Unsupported as i32,
        },
        "request": request,
        "error_response": error,
        "root_responses": {
            "describe": response::<TransportDescription<'_>>(),
            "runtime_reserve": response::<RuntimeReceipt>(),
            "runtime_initialize": response::<RuntimeReceipt>(),
            "runtime_status": response::<RuntimeSnapshot>(),
            "runtime_close": response::<RuntimeReceipt>(),
            "runtime_free": response::<RuntimeReceipt>(),
        },
        "runtime_responses": runtime_responses,
    })
}

/// Encode `document()` with canonical object ordering and a single LF terminator.
/// 使用规范对象顺序及单个 LF 终止符编码 `document()`。
/// Return exact distributable UTF-8 bytes or a serialization error.
/// 返回精确可分发 UTF-8 字节或序列化错误。
pub fn bytes() -> Result<Vec<u8>, serde_json::Error> {
    let mut encoded = serde_json::to_vec_pretty(&document())?;
    encoded.push(b'\n');
    Ok(encoded)
}
