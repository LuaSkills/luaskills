use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Generic host-side client identity information passed into the LuaSkills runtime.
/// 传入 LuaSkills 运行时的通用宿主客户端身份信息。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct RuntimeClientInfo {
    /// Stable host-defined client kind, such as `mcp`, `ide`, or `desktop`.
    /// 宿主定义的稳定客户端类型，例如 `mcp`、`ide` 或 `desktop`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable client name reported by the host.
    /// 由宿主上报的人类可读客户端名称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional client version string.
    /// 可选的客户端版本字符串。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Generic request-scoped context injected by the host into one runtime invocation.
/// 宿主在单次运行时调用中注入的通用请求级上下文。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct RuntimeRequestContext {
    /// Optional host-defined request identifier for audit and cost attribution.
    /// 可选的宿主请求标识符，用于审计和成本归因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Optional host-defined client name for audit and cost attribution.
    /// 可选的宿主客户端名称，用于审计和成本归因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    /// Optional host-defined transport name for the current request.
    /// 当前请求的可选宿主传输层名称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_name: Option<String>,
    /// Optional host-defined session identifier.
    /// 可选的宿主会话标识。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional host-side client metadata.
    /// 可选的宿主客户端元数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<RuntimeClientInfo>,
    /// Optional host-provided raw client capabilities object.
    /// 可选的宿主原始客户端能力对象。
    #[serde(default = "default_runtime_client_capabilities")]
    pub client_capabilities: Value,
}

/// Return the default empty capabilities object.
/// 返回默认的空能力对象。
fn default_runtime_client_capabilities() -> Value {
    Value::Object(serde_json::Map::new())
}
