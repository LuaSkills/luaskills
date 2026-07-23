use std::collections::BTreeMap;
use std::io::{self, Write};

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::{
    LuaEngine, SKILL_CONFIG_MAX_TOOL_RESPONSE_BYTES, SkillPackageConfigDescribeMode,
    SkillPackageConfigInputValue,
};

/// Counting writer that rejects a serialized tool response before it crosses its hard limit.
/// 在序列化工具响应越过硬上限前拒绝写入的计数写入器。
struct RuntimeConfigResponseSizeWriter {
    /// Maximum accepted encoded response size.
    /// 允许的最大响应编码大小。
    maximum_bytes: usize,
    /// Encoded bytes accepted so far.
    /// 当前已经接受的编码字节数。
    bytes_written: usize,
    /// Whether one attempted write crossed the configured limit.
    /// 是否已有一次写入尝试越过配置上限。
    exceeded: bool,
}

impl RuntimeConfigResponseSizeWriter {
    /// Create one bounded response-size writer.
    /// 创建一个有界的响应大小写入器。
    fn new(maximum_bytes: usize) -> Self {
        Self {
            maximum_bytes,
            bytes_written: 0,
            exceeded: false,
        }
    }
}

impl Write for RuntimeConfigResponseSizeWriter {
    /// Count one serialized chunk or reject it before crossing the hard response limit.
    /// 统计一个序列化分块，或在其越过响应硬上限前拒绝它。
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next_size = self.bytes_written.saturating_add(buffer.len());
        if next_size > self.maximum_bytes {
            self.exceeded = true;
            return Err(io::Error::other("runtime-config response limit exceeded"));
        }
        self.bytes_written = next_size;
        Ok(buffer.len())
    }

    /// Accept flush because the counting writer retains no buffered bytes.
    /// 接受刷新操作，因为计数写入器不保留任何缓冲字节。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stable action set accepted by the unified `runtime-config` dispatcher.
/// 统一 `runtime-config` 分发器接受的稳定动作集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSkillConfigToolAction {
    /// Describe effective or physically installed declarations.
    /// 描述有效或物理已安装声明。
    Describe,
    /// Validate one effective package without writing.
    /// 在不写入的情况下校验一个有效技能包。
    Validate,
    /// List raw persisted entries.
    /// 列出原始持久化条目。
    List,
    /// Read one raw persisted value.
    /// 读取一个原始持久化值。
    Get,
    /// Atomically set one or multiple declared values.
    /// 原子设置一个或多个已声明值。
    Set,
    /// Atomically delete one explicit or orphaned value.
    /// 原子删除一个显式值或遗留值。
    Delete,
    /// Explicitly refresh one or both stores.
    /// 显式刷新一个或两个存储。
    Refresh,
}

/// Strict unified request accepted by both model-visible and host-private integrations.
/// 模型可见与宿主私有对接共同接受的严格统一请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSkillConfigToolRequest {
    /// Requested dispatcher action.
    /// 请求的分发动作。
    pub action: RuntimeSkillConfigToolAction,
    /// Optional effective package identifier.
    /// 可选的有效技能包标识符。
    #[serde(default)]
    pub skill_id: Option<String>,
    /// Optional single-key set or get/delete key.
    /// 可选的单键设置或读取/删除键。
    #[serde(default)]
    pub key: Option<String>,
    /// Optional typed single-key set value.
    /// 可选的类型化单键设置值。
    #[serde(default)]
    pub value: Option<SkillPackageConfigInputValue>,
    /// Optional typed batch set values.
    /// 可选的类型化批量设置值。
    #[serde(
        default,
        deserialize_with = "deserialize_optional_unique_config_values"
    )]
    pub values: Option<BTreeMap<String, SkillPackageConfigInputValue>>,
    /// Optional compare-and-swap revision.
    /// 可选的比较并交换修订号。
    #[serde(default)]
    pub expected_revision: Option<String>,
    /// Host-selected raw value disclosure switch.
    /// 宿主选择的原始值披露开关。
    #[serde(default)]
    pub include_values: bool,
    /// Declaration discovery mode.
    /// 声明发现模式。
    #[serde(default)]
    pub mode: SkillPackageConfigDescribeMode,
    /// Optional physical root filter accepted by installed describe mode.
    /// 已安装描述模式接受的可选物理根过滤器。
    #[serde(default)]
    pub root_name: Option<String>,
    /// Optional persisted store scope accepted by refresh.
    /// 刷新动作接受的可选持久化存储作用域。
    #[serde(default)]
    pub store_scope: Option<String>,
}

/// Deserialize one configuration value object while rejecting duplicate keys.
/// 反序列化单个配置值对象并拒绝重复键。
pub(crate) fn deserialize_unique_config_values<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, SkillPackageConfigInputValue>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(UniqueConfigValuesVisitor)
}

/// Deserialize one optional configuration value object with duplicate detection.
/// 反序列化单个可选配置值对象并检测重复键。
fn deserialize_optional_unique_config_values<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, SkillPackageConfigInputValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalUniqueConfigValuesVisitor;

    impl<'de> Visitor<'de> for OptionalUniqueConfigValuesVisitor {
        type Value = Option<BTreeMap<String, SkillPackageConfigInputValue>>;

        /// Describe one optional typed configuration object.
        /// 描述单个可选类型化配置对象。
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("null or an object mapping unique configuration keys to scalars")
        }

        /// Decode one explicit null as an absent batch.
        /// 把一个显式 null 解码为缺失批次。
        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        /// Decode one unit value as an absent batch.
        /// 把一个 unit 值解码为缺失批次。
        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        /// Decode one present object through the duplicate-rejecting visitor.
        /// 通过拒绝重复项的访问器解码一个已存在对象。
        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserialize_unique_config_values(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalUniqueConfigValuesVisitor)
}

/// Strict visitor for one typed package configuration value object.
/// 单个类型化技能包配置值对象使用的严格访问器。
struct UniqueConfigValuesVisitor;

impl<'de> Visitor<'de> for UniqueConfigValuesVisitor {
    type Value = BTreeMap<String, SkillPackageConfigInputValue>;

    /// Describe one typed configuration value object.
    /// 描述单个类型化配置值对象。
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an object mapping unique configuration keys to scalar values")
    }

    /// Decode entries while rejecting one repeated key before its value can overwrite state.
    /// 解码条目并在重复键覆盖状态前拒绝它。
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate skill configuration key '{key}'"
                )));
            }
            values.insert(key, map.next_value::<SkillPackageConfigInputValue>()?);
        }
        Ok(values)
    }
}

/// Structured dispatcher error shared by every integration style.
/// 每种对接方式共享的结构化分发器错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSkillConfigToolError {
    /// Stable machine-readable error code.
    /// 稳定的机器可读错误码。
    pub code: String,
    /// Human-readable English error message.
    /// 人类可读英文错误消息。
    pub message: String,
    /// Extensible machine-readable details object.
    /// 可扩展的机器可读详情对象。
    pub details: JsonMap<String, JsonValue>,
}

/// Stable response envelope returned by the unified dispatcher.
/// 统一分发器返回的稳定响应包络。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSkillConfigToolResponse {
    /// Whether the requested action succeeded.
    /// 请求动作是否成功。
    pub ok: bool,
    /// Echoed action.
    /// 回显的动作。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<RuntimeSkillConfigToolAction>,
    /// Structured success payload.
    /// 结构化成功载荷。
    pub result: Option<JsonValue>,
    /// Structured failure payload.
    /// 结构化失败载荷。
    pub error: Option<RuntimeSkillConfigToolError>,
}

impl LuaEngine {
    /// Dispatch one already-authorized runtime configuration request.
    /// 分发一个已由宿主授权的运行时配置请求。
    pub fn dispatch_runtime_config_tool(
        &mut self,
        request: RuntimeSkillConfigToolRequest,
    ) -> RuntimeSkillConfigToolResponse {
        let action = request.action;
        let response = match self.dispatch_runtime_config_tool_inner(request) {
            Ok(result) => RuntimeSkillConfigToolResponse {
                ok: true,
                action: Some(action),
                result: Some(result),
                error: None,
            },
            Err(message) => RuntimeSkillConfigToolResponse {
                ok: false,
                action: Some(action),
                result: None,
                error: Some(runtime_config_tool_error(message)),
            },
        };
        match validate_runtime_config_tool_response_size(&response) {
            Ok(()) => response,
            Err(message) => RuntimeSkillConfigToolResponse {
                ok: false,
                action: Some(action),
                result: None,
                error: Some(runtime_config_tool_error(message)),
            },
        }
    }

    /// Dispatch one strict JSON request and return the stable JSON response envelope.
    /// 分发一个严格 JSON 请求并返回稳定 JSON 响应包络。
    pub fn dispatch_runtime_config_tool_json(&mut self, input: &str) -> String {
        let response = match serde_json::from_str::<RuntimeSkillConfigToolRequest>(input) {
            Ok(request) => self.dispatch_runtime_config_tool(request),
            Err(error) => RuntimeSkillConfigToolResponse {
                ok: false,
                action: None,
                result: None,
                error: Some(RuntimeSkillConfigToolError {
                    code: "CONFIG_DECLARATION_INVALID".to_string(),
                    message: format!("invalid runtime-config request: {error}"),
                    details: JsonMap::new(),
                }),
            },
        };
        serde_json::to_string(&response)
            .expect("runtime configuration response serialization cannot fail")
    }

    /// Execute one normalized dispatcher action after strict request decoding.
    /// 在严格请求解码后执行一个规范化分发动作。
    fn dispatch_runtime_config_tool_inner(
        &mut self,
        request: RuntimeSkillConfigToolRequest,
    ) -> Result<JsonValue, String> {
        validate_runtime_config_tool_shape(&request)?;
        match request.action {
            RuntimeSkillConfigToolAction::Describe => match request.mode {
                SkillPackageConfigDescribeMode::Effective => {
                    if request.root_name.is_some() {
                        return Err("CONFIG_BATCH_ARGUMENT_CONFLICT: root_name is accepted only in installed describe mode".to_string());
                    }
                    serde_json::to_value(self.describe_skill_package_config(
                        request.skill_id.as_deref(),
                        request.include_values,
                    )?)
                    .map_err(runtime_config_tool_serialization_error)
                }
                SkillPackageConfigDescribeMode::Installed => {
                    if request.include_values {
                        return Err("CONFIG_BATCH_ARGUMENT_CONFLICT: installed describe mode never returns persisted values".to_string());
                    }
                    serde_json::to_value(self.describe_installed_skill_package_config(
                        request.skill_id.as_deref(),
                        request.root_name.as_deref(),
                    ))
                    .map_err(runtime_config_tool_serialization_error)
                }
            },
            RuntimeSkillConfigToolAction::Validate => {
                let skill_id = required_tool_field(request.skill_id, "skill_id")?;
                serde_json::to_value(self.validate_skill_package_config(&skill_id)?)
                    .map_err(runtime_config_tool_serialization_error)
            }
            RuntimeSkillConfigToolAction::List => {
                serde_json::to_value(self.list_skill_config_entries(request.skill_id.as_deref())?)
                    .map_err(runtime_config_tool_serialization_error)
            }
            RuntimeSkillConfigToolAction::Get => {
                let skill_id = required_tool_field(request.skill_id, "skill_id")?;
                let key = required_tool_field(request.key, "key")?;
                let value = self.get_skill_config_value(&skill_id, &key)?;
                Ok(json!({
                    "found": value.is_some(),
                    "skill_id": skill_id,
                    "key": key,
                    "value": value,
                }))
            }
            RuntimeSkillConfigToolAction::Set => {
                let skill_id = required_tool_field(request.skill_id, "skill_id")?;
                let values = match (request.values, request.key, request.value) {
                    (Some(values), None, None) if !values.is_empty() => values,
                    (None, Some(key), Some(value)) => BTreeMap::from([(key, value)]),
                    (Some(values), None, None) if values.is_empty() => {
                        return Err(
                            "CONFIG_BATCH_EMPTY: configuration batch must not be empty".to_string()
                        );
                    }
                    _ => {
                        return Err("CONFIG_BATCH_ARGUMENT_CONFLICT: provide values or one key/value pair, but not both".to_string());
                    }
                };
                serde_json::to_value(self.set_skill_config_values(
                    &skill_id,
                    values,
                    request.expected_revision.as_deref(),
                )?)
                .map_err(runtime_config_tool_serialization_error)
            }
            RuntimeSkillConfigToolAction::Delete => {
                let skill_id = required_tool_field(request.skill_id, "skill_id")?;
                let key = required_tool_field(request.key, "key")?;
                serde_json::to_value(self.delete_skill_config_value(
                    &skill_id,
                    &key,
                    request.expected_revision.as_deref(),
                )?)
                .map_err(runtime_config_tool_serialization_error)
            }
            RuntimeSkillConfigToolAction::Refresh => {
                serde_json::to_value(self.refresh_skill_config(request.store_scope.as_deref())?)
                    .map_err(runtime_config_tool_serialization_error)
            }
        }
    }
}

/// Convert one impossible dispatcher result-serialization failure into a stable public error.
/// 把单个不应发生的分发结果序列化失败转换为稳定公共错误。
fn runtime_config_tool_serialization_error(error: serde_json::Error) -> String {
    format!("CONFIG_DECLARATION_INVALID: failed to serialize runtime-config result: {error}")
}

/// Reject fields that are known globally but irrelevant to the selected action.
/// 拒绝全局已知但与所选动作无关的字段。
fn validate_runtime_config_tool_shape(
    request: &RuntimeSkillConfigToolRequest,
) -> Result<(), String> {
    let mut unexpected = Vec::new();
    let mut reject = |present: bool, name: &'static str| {
        if present {
            unexpected.push(name);
        }
    };
    match request.action {
        RuntimeSkillConfigToolAction::Describe => {
            reject(request.key.is_some(), "key");
            reject(request.value.is_some(), "value");
            reject(request.values.is_some(), "values");
            reject(request.expected_revision.is_some(), "expected_revision");
            reject(request.store_scope.is_some(), "store_scope");
        }
        RuntimeSkillConfigToolAction::Validate | RuntimeSkillConfigToolAction::List => {
            reject(request.key.is_some(), "key");
            reject(request.value.is_some(), "value");
            reject(request.values.is_some(), "values");
            reject(request.expected_revision.is_some(), "expected_revision");
            reject(request.include_values, "include_values");
            reject(
                request.mode != SkillPackageConfigDescribeMode::Effective,
                "mode",
            );
            reject(request.root_name.is_some(), "root_name");
            reject(request.store_scope.is_some(), "store_scope");
        }
        RuntimeSkillConfigToolAction::Get => {
            reject(request.value.is_some(), "value");
            reject(request.values.is_some(), "values");
            reject(request.expected_revision.is_some(), "expected_revision");
            reject(request.include_values, "include_values");
            reject(
                request.mode != SkillPackageConfigDescribeMode::Effective,
                "mode",
            );
            reject(request.root_name.is_some(), "root_name");
            reject(request.store_scope.is_some(), "store_scope");
        }
        RuntimeSkillConfigToolAction::Set => {
            reject(request.include_values, "include_values");
            reject(
                request.mode != SkillPackageConfigDescribeMode::Effective,
                "mode",
            );
            reject(request.root_name.is_some(), "root_name");
            reject(request.store_scope.is_some(), "store_scope");
        }
        RuntimeSkillConfigToolAction::Delete => {
            reject(request.value.is_some(), "value");
            reject(request.values.is_some(), "values");
            reject(request.include_values, "include_values");
            reject(
                request.mode != SkillPackageConfigDescribeMode::Effective,
                "mode",
            );
            reject(request.root_name.is_some(), "root_name");
            reject(request.store_scope.is_some(), "store_scope");
        }
        RuntimeSkillConfigToolAction::Refresh => {
            reject(request.skill_id.is_some(), "skill_id");
            reject(request.key.is_some(), "key");
            reject(request.value.is_some(), "value");
            reject(request.values.is_some(), "values");
            reject(request.expected_revision.is_some(), "expected_revision");
            reject(request.include_values, "include_values");
            reject(
                request.mode != SkillPackageConfigDescribeMode::Effective,
                "mode",
            );
            reject(request.root_name.is_some(), "root_name");
        }
    }
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "CONFIG_BATCH_ARGUMENT_CONFLICT: action '{}' does not accept field(s): {}",
            serde_json::to_value(request.action)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_string()),
            unexpected.join(", ")
        ))
    }
}

/// Require one nonempty action field.
/// 要求一个非空动作字段。
fn required_tool_field(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("CONFIG_DECLARATION_INVALID: {field} is required"))
}

/// Convert one internal error string into the stable dispatcher error shape.
/// 把一个内部错误字符串转换为稳定分发器错误形态。
fn runtime_config_tool_error(message: String) -> RuntimeSkillConfigToolError {
    let code = message
        .split_once(':')
        .map(|(code, _)| code)
        .filter(|code| code.starts_with("CONFIG_"))
        .unwrap_or("CONFIG_DECLARATION_INVALID")
        .to_string();
    RuntimeSkillConfigToolError {
        code,
        message,
        details: JsonMap::new(),
    }
}

/// Validate one complete response envelope through bounded streaming JSON serialization.
/// 通过有界流式 JSON 序列化校验一个完整响应包络。
fn validate_runtime_config_tool_response_size(
    response: &RuntimeSkillConfigToolResponse,
) -> Result<(), String> {
    let mut writer = RuntimeConfigResponseSizeWriter::new(SKILL_CONFIG_MAX_TOOL_RESPONSE_BYTES);
    match serde_json::to_writer(&mut writer, response) {
        Ok(()) => Ok(()),
        Err(_) if writer.exceeded => Err(format!(
            "CONFIG_RESPONSE_TOO_LARGE: runtime-config response exceeds the hard limit {} encoded bytes",
            SKILL_CONFIG_MAX_TOOL_RESPONSE_BYTES
        )),
        Err(error) => Err(format!(
            "CONFIG_DECLARATION_INVALID: failed to serialize runtime-config response: {error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify typed batch decoding rejects one repeated configuration key.
    /// 验证类型化批次解码会拒绝重复配置键。
    #[test]
    fn runtime_config_tool_rejects_duplicate_batch_keys() {
        let error = serde_json::from_str::<RuntimeSkillConfigToolRequest>(
            r#"{"action":"set","skill_id":"demo-skill","values":{"retries":1,"retries":2}}"#,
        )
        .expect_err("duplicate configuration keys must fail");
        assert!(
            error
                .to_string()
                .contains("duplicate skill configuration key")
        );
    }

    /// Verify known fields irrelevant to the selected action are rejected explicitly.
    /// 验证与所选动作无关的已知字段会被显式拒绝。
    #[test]
    fn runtime_config_tool_rejects_irrelevant_action_fields() {
        let request = serde_json::from_str::<RuntimeSkillConfigToolRequest>(
            r#"{"action":"get","skill_id":"demo-skill","key":"token","store_scope":"skills"}"#,
        )
        .expect("decode strict request");
        let error =
            validate_runtime_config_tool_shape(&request).expect_err("irrelevant field must fail");
        assert!(error.contains("store_scope"));
    }

    /// Verify set accepts exactly one nonempty batch form.
    /// 验证 set 只接受一种非空批次形式。
    #[test]
    fn runtime_config_tool_accepts_one_batch_form() {
        let request = serde_json::from_str::<RuntimeSkillConfigToolRequest>(
            r#"{"action":"set","skill_id":"demo-skill","values":{"retries":2}}"#,
        )
        .expect("decode strict request");
        validate_runtime_config_tool_shape(&request).expect("batch form should be valid");
    }

    /// Verify stable configuration errors retain their machine-readable code.
    /// 验证稳定配置错误会保留其机器可读代码。
    #[test]
    fn runtime_config_tool_preserves_stable_error_codes() {
        let error = runtime_config_tool_error(
            "CONFIG_PACKAGE_NOT_FOUND: skill package 'missing' is not loaded or effective"
                .to_string(),
        );
        assert_eq!(error.code, "CONFIG_PACKAGE_NOT_FOUND");
        assert!(error.details.is_empty());
    }

    /// Verify bounded response serialization rejects overflow without retaining oversized bytes.
    /// 验证有界响应序列化会在不保留超大字节的情况下拒绝溢出。
    #[test]
    fn runtime_config_tool_response_size_writer_rejects_overflow() {
        // Tiny test-only limit that exercises the same streaming writer as production.
        // 用于走通生产环境同一流式写入器的微型测试上限。
        let mut writer = RuntimeConfigResponseSizeWriter::new(8);
        let error = serde_json::to_writer(&mut writer, &json!({"value": "too-large"}))
            .expect_err("encoded response must exceed the test limit");
        assert!(writer.exceeded);
        assert!(error.to_string().contains("response limit exceeded"));
        assert!(writer.bytes_written <= writer.maximum_bytes);
    }
}
