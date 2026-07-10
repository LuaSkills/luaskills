use mlua::{MultiValue, Value as LuaValue};
use serde_json::{Value, json};
use std::path::Path;

use crate::runtime_options::LuaInvocationContext;
use crate::runtime_result::{
    NON_STRING_TOOL_RESULT_ERROR, RuntimeHostResult, RuntimeInvocationResult, ToolOverflowMode,
};

use super::{lua_value_to_json, lua_value_type_name};

/// Stable delete mode used by canonical `change_set` delete file records.
/// canonical `change_set` delete 文件记录使用的稳定内容模式。
const CHANGE_SET_DELETE_CONTENT_MODE_FULL: &str = "full";

/// Stable truncated delete mode used when one deleted file body is intentionally summarized.
/// 当删除文件正文需要摘要化时使用的稳定截断模式。
const CHANGE_SET_DELETE_CONTENT_MODE_TRUNCATED: &str = "truncated";

/// Maximum delete line count that may stay in full-content mode before runtime truncation kicks in.
/// 删除内容在运行时强制截断前允许保留全文模式的最大行数。
const CHANGE_SET_DELETE_TRUNCATE_LINE_LIMIT: usize = 500;

/// Number of leading and trailing lines preserved in truncated delete mode.
/// 截断删除模式中保留的前后片段行数。
const CHANGE_SET_DELETE_TRUNCATED_EDGE_LINE_COUNT: usize = 50;

/// Canonical delete content mode used by `change_set` file lifecycle records.
/// `change_set` 文件生命周期记录使用的 canonical delete 内容模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeSetDeleteContentMode {
    /// Full deleted-file content stays inline.
    /// 删除文件的完整内容以内联方式保留。
    Full,
    /// Deleted-file content is summarized into leading and trailing snippets.
    /// 删除文件内容被摘要为前后片段。
    Truncated,
}

impl ChangeSetDeleteContentMode {
    /// Parse one optional delete content-mode value, defaulting to full mode when absent.
    /// 解析可选的 delete 内容模式；缺失时默认视为全文模式。
    fn parse(value: Option<&Value>) -> Option<Self> {
        match value.and_then(Value::as_str).map(str::trim) {
            None | Some("") | Some(CHANGE_SET_DELETE_CONTENT_MODE_FULL) => Some(Self::Full),
            Some(CHANGE_SET_DELETE_CONTENT_MODE_TRUNCATED) => Some(Self::Truncated),
            _ => None,
        }
    }

    /// Return the stable serialized string form used in host-facing payloads.
    /// 返回宿主侧 payload 使用的稳定序列化字符串形式。
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => CHANGE_SET_DELETE_CONTENT_MODE_FULL,
            Self::Truncated => CHANGE_SET_DELETE_CONTENT_MODE_TRUNCATED,
        }
    }
}

/// Structured host-result capability snapshot derived from request context.
/// 从请求上下文导出的结构化宿主结果能力快照。
#[derive(Debug, Clone)]
pub(super) struct RuntimeHostResultCapability {
    /// Whether the host explicitly enables structured host results for the current request.
    /// 宿主是否为当前请求显式开启结构化宿主结果。
    enabled: bool,
    /// Allowed result kinds when the host wants to restrict the bridge surface.
    /// 当宿主希望限制桥接面时允许的结果类型集合。
    allowed_kinds: Vec<String>,
    /// Optional maximum payload byte size accepted by the host.
    /// 宿主接受的可选最大载荷字节数。
    max_payload_bytes: Option<usize>,
}

impl RuntimeHostResultCapability {
    /// Return whether one result kind is accepted by the current host capability snapshot.
    /// 返回当前宿主能力快照是否接受某个结果类型。
    fn allows_kind(&self, kind: &str) -> bool {
        self.allowed_kinds.is_empty() || self.allowed_kinds.iter().any(|item| item == kind)
    }
}

/// Return the disabled host-result capability snapshot used when the host does not opt in.
/// 返回宿主未开启时使用的禁用态 host-result 能力快照。
fn disabled_host_result_capability() -> RuntimeHostResultCapability {
    RuntimeHostResultCapability {
        enabled: false,
        allowed_kinds: Vec::new(),
        max_payload_bytes: None,
    }
}

/// Resolve one host-result capability snapshot from one optional invocation context.
/// 从可选调用上下文中解析一份宿主结果能力快照。
///
/// The invocation_context parameter is the optional host-provided context for the current call.
/// invocation_context 参数是当前调用可选的宿主注入上下文。
///
/// Returns the resolved host-result capability snapshot.
/// 返回解析后的 host-result 能力快照。
///
/// Returns an error when an explicitly provided host-result capability field is malformed.
/// 当显式提供的 host-result 能力字段格式错误时返回错误。
pub(super) fn resolve_host_result_capability(
    invocation_context: Option<&LuaInvocationContext>,
) -> Result<RuntimeHostResultCapability, String> {
    // Request context provided by the host for the current invocation.
    // 宿主为当前调用提供的请求上下文。
    let Some(request_context) =
        invocation_context.and_then(|context| context.request_context.as_ref())
    else {
        return Ok(disabled_host_result_capability());
    };
    // Raw client capability object that owns the optional host_result capability block.
    // 拥有可选 host_result 能力块的原始客户端能力对象。
    let Value::Object(capabilities) = &request_context.client_capabilities else {
        return Ok(disabled_host_result_capability());
    };
    // Host-result capability value, when the host declares one.
    // 宿主声明 host-result 能力时提供的能力值。
    let Some(host_result_value) = capabilities.get("host_result") else {
        return Ok(disabled_host_result_capability());
    };
    let Value::Object(host_result) = host_result_value else {
        return Err(
            "runtime request client_capabilities.host_result must be an object".to_string(),
        );
    };
    // Host-result enabled flag; absence keeps the bridge disabled.
    // host-result 开启标记；缺失时保持桥接禁用。
    let enabled = match host_result.get("enabled") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(
                "runtime request client_capabilities.host_result.enabled must be a boolean"
                    .to_string(),
            );
        }
    };
    // Allowed host-result kind list; absence means the host does not restrict result kinds.
    // 允许的 host-result 类型列表；缺失表示宿主不限制结果类型。
    let allowed_kinds = match host_result.get("allowed_kinds") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            // Parsed allowed kind names after trimming every host-provided list item.
            // 修剪每个宿主提供列表项后得到的已解析允许类型名称。
            let mut parsed = Vec::new();
            for (index, item) in items.iter().enumerate() {
                // Allowed kind text extracted from one array item.
                // 从单个数组项提取出的允许类型文本。
                let kind = item
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "runtime request client_capabilities.host_result.allowed_kinds[{}] must be one non-empty string",
                            index
                        )
                    })?;
                parsed.push(kind.to_string());
            }
            parsed
        }
        Some(_) => {
            return Err(
                "runtime request client_capabilities.host_result.allowed_kinds must be an array of non-empty strings"
                    .to_string(),
            );
        }
    };
    // Optional maximum payload byte limit accepted by the host-result bridge.
    // host-result 桥接接受的可选最大载荷字节上限。
    let max_payload_bytes = match host_result.get("max_payload_bytes") {
        None | Some(Value::Null) => None,
        Some(Value::Number(value)) => Some(
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    "runtime request client_capabilities.host_result.max_payload_bytes must be one positive integer"
                        .to_string()
                })?,
        ),
        Some(_) => {
            return Err(
                "runtime request client_capabilities.host_result.max_payload_bytes must be one positive integer"
                    .to_string(),
            );
        }
    };
    Ok(RuntimeHostResultCapability {
        enabled,
        allowed_kinds,
        max_payload_bytes,
    })
}

/// Convert one host-result capability snapshot into one Lua helper table payload.
/// 将宿主结果能力快照转换为一份 Lua helper 表载荷。
pub(super) fn host_result_capability_to_json_value(
    capability: &RuntimeHostResultCapability,
) -> Value {
    let allowed_kinds = capability
        .allowed_kinds
        .iter()
        .cloned()
        .map(Value::String)
        .collect::<Vec<Value>>();
    json!({
        "enabled": capability.enabled,
        "allowed_kinds": allowed_kinds,
        "max_payload_bytes": capability.max_payload_bytes,
    })
}

/// Parse one optional fourth Lua return value into one structured host result.
/// 将可选的第四个 Lua 返回值解析为结构化宿主结果。
fn parse_host_result_value(
    value: Option<&LuaValue>,
    display_name: &str,
    capability: &RuntimeHostResultCapability,
) -> Result<Option<RuntimeHostResult>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, LuaValue::Nil) {
        return Ok(None);
    }
    if !capability.enabled {
        return Ok(None);
    }
    let host_result_json = lua_value_to_json(value)?;
    let Value::Object(object) = host_result_json else {
        return Err(format!(
            "Lua skill '{}' must return host_result as an object with kind and payload",
            display_name
        ));
    };
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "Lua skill '{}' must return host_result.kind as one non-empty string",
                display_name
            )
        })?
        .to_string();
    if !capability.allows_kind(&kind) {
        return Err(format!(
            "Lua skill '{}' returned host_result.kind '{}' which is not allowed by the host",
            display_name, kind
        ));
    }
    let payload = object.get("payload").cloned().ok_or_else(|| {
        format!(
            "Lua skill '{}' must return host_result.payload when host_result is enabled",
            display_name
        )
    })?;
    let normalized_payload = normalize_host_result_payload(&kind, payload);
    validate_host_result_payload(
        display_name,
        &kind,
        &normalized_payload,
        capability.max_payload_bytes,
    )?;
    Ok(Some(RuntimeHostResult {
        kind,
        payload: normalized_payload,
    }))
}

/// Normalize one host-result payload into the canonical host-facing shape before validation.
/// 在校验前把宿主结果 payload 归一化为 canonical 宿主输出形态。
fn normalize_host_result_payload(kind: &str, payload: Value) -> Value {
    if kind == "change_set" {
        return normalize_change_set_payload(payload);
    }
    payload
}

/// Normalize one `change_set` payload so delete records expose one stable content contract.
/// 归一化 `change_set` payload，确保 delete 记录暴露统一稳定的内容协议。
pub(super) fn normalize_change_set_payload(mut payload: Value) -> Value {
    let Value::Object(object) = &mut payload else {
        return payload;
    };
    let Some(Value::Array(files)) = object.get_mut("files") else {
        return payload;
    };
    for file in files.iter_mut() {
        normalize_change_set_delete_file_record(file);
    }
    payload
}

/// Normalize one delete file record in-place while preserving every non-delete record unchanged.
/// 原地归一化单个 delete 文件记录，并保持所有非 delete 记录不变。
fn normalize_change_set_delete_file_record(file: &mut Value) {
    let Value::Object(file) = file else {
        return;
    };
    let Some("delete") = file.get("change").and_then(Value::as_str).map(str::trim) else {
        return;
    };
    let requested_mode = ChangeSetDeleteContentMode::parse(file.get("content_mode"))
        .unwrap_or(ChangeSetDeleteContentMode::Full);
    let Some(content) = file
        .get("content")
        .and_then(Value::as_str)
        .map(ToString::to_string)
    else {
        return;
    };
    let normalized_content = normalize_change_set_text(&content);
    let lines = split_change_set_lines(&normalized_content);
    let total_line_count = lines.len();
    file.insert(
        "total_line_count".to_string(),
        Value::Number(serde_json::Number::from(total_line_count as u64)),
    );
    let should_truncate = requested_mode == ChangeSetDeleteContentMode::Truncated
        || total_line_count > CHANGE_SET_DELETE_TRUNCATE_LINE_LIMIT;
    if should_truncate {
        let head_count = total_line_count.min(CHANGE_SET_DELETE_TRUNCATED_EDGE_LINE_COUNT);
        let tail_count = CHANGE_SET_DELETE_TRUNCATED_EDGE_LINE_COUNT
            .min(total_line_count.saturating_sub(head_count));
        let head_lines = lines
            .iter()
            .take(head_count)
            .copied()
            .collect::<Vec<&str>>();
        let tail_lines = if tail_count == 0 {
            Vec::new()
        } else {
            lines[total_line_count - tail_count..].to_vec()
        };
        file.insert(
            "content_mode".to_string(),
            Value::String(CHANGE_SET_DELETE_CONTENT_MODE_TRUNCATED.to_string()),
        );
        file.insert(
            "content_head".to_string(),
            Value::String(head_lines.join("\n")),
        );
        file.insert(
            "content_tail".to_string(),
            Value::String(tail_lines.join("\n")),
        );
        file.remove("content");
        return;
    }
    file.insert(
        "content_mode".to_string(),
        Value::String(CHANGE_SET_DELETE_CONTENT_MODE_FULL.to_string()),
    );
    file.remove("content_head");
    file.remove("content_tail");
}

/// Validate one structured host-result payload against common and kind-specific rules.
/// 按通用规则与特定 kind 规则校验结构化宿主结果载荷。
fn validate_host_result_payload(
    display_name: &str,
    kind: &str,
    payload: &Value,
    max_payload_bytes: Option<usize>,
) -> Result<(), String> {
    let payload_json = serde_json::to_vec(payload).map_err(|error| {
        format!(
            "Lua skill '{}' returned one host_result payload that cannot be serialized: {}",
            display_name, error
        )
    })?;
    if let Some(limit) = max_payload_bytes
        && payload_json.len() > limit
    {
        return Err(format!(
            "Lua skill '{}' returned host_result payload {} bytes larger than host limit {}",
            display_name,
            payload_json.len(),
            limit
        ));
    }
    if kind == "change_set" {
        validate_change_set_payload(display_name, payload)?;
    }
    Ok(())
}

/// Validate the canonical `change_set` payload contract used by IDE-oriented edit results.
/// 校验面向 IDE 编辑结果使用的 canonical `change_set` 载荷协议。
pub(super) fn validate_change_set_payload(
    display_name: &str,
    payload: &Value,
) -> Result<(), String> {
    let Value::Object(object) = payload else {
        return Err(format!(
            "Lua skill '{}' must return change_set payload as an object",
            display_name
        ));
    };
    let mode = object.get("mode").and_then(Value::as_str).ok_or_else(|| {
        format!(
            "Lua skill '{}' change_set.mode must be a string",
            display_name
        )
    })?;
    if !matches!(mode, "preview" | "applied") {
        return Err(format!(
            "Lua skill '{}' change_set.mode must be 'preview' or 'applied'",
            display_name
        ));
    }
    if let Some(summary) = object.get("summary")
        && !summary.is_string()
        && !summary.is_null()
    {
        return Err(format!(
            "Lua skill '{}' change_set.summary must be a string when present",
            display_name
        ));
    }
    let files = object.get("files").ok_or_else(|| {
        format!(
            "Lua skill '{}' change_set.files must be present as one array",
            display_name
        )
    })?;
    let Value::Array(files) = files else {
        return Err(format!(
            "Lua skill '{}' change_set.files must be an array",
            display_name
        ));
    };
    for (index, file) in files.iter().enumerate() {
        validate_change_set_file_payload(display_name, index, file)?;
    }
    if let Some(diagnostics) = object.get("diagnostics") {
        let Value::Array(diagnostics) = diagnostics else {
            return Err(format!(
                "Lua skill '{}' change_set.diagnostics must be an array when present",
                display_name
            ));
        };
        for (index, diagnostic) in diagnostics.iter().enumerate() {
            let Value::Object(diagnostic) = diagnostic else {
                return Err(format!(
                    "Lua skill '{}' change_set.diagnostics[{}] must be an object",
                    display_name, index
                ));
            };
            let _level = diagnostic
                .get("level")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "Lua skill '{}' change_set.diagnostics[{}].level must be a string",
                        display_name, index
                    )
                })?;
            let _message = diagnostic
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "Lua skill '{}' change_set.diagnostics[{}].message must be a string",
                        display_name, index
                    )
                })?;
        }
    }
    Ok(())
}

/// Validate one file-level record inside one canonical `change_set` payload.
/// 校验 canonical `change_set` 载荷中的单个文件级记录。
fn validate_change_set_file_payload(
    display_name: &str,
    file_index: usize,
    file: &Value,
) -> Result<(), String> {
    let Value::Object(file) = file else {
        return Err(format!(
            "Lua skill '{}' change_set.files[{}] must be an object",
            display_name, file_index
        ));
    };
    let change = file.get("change").and_then(Value::as_str).ok_or_else(|| {
        format!(
            "Lua skill '{}' change_set.files[{}].change must be a string",
            display_name, file_index
        )
    })?;
    if let Some(patch) = file.get("patch")
        && !patch.is_string()
        && !patch.is_null()
    {
        return Err(format!(
            "Lua skill '{}' change_set.files[{}].patch must be a string when present",
            display_name, file_index
        ));
    }
    match change {
        "modify" => {
            let _path =
                validate_change_set_absolute_path_field(display_name, file_index, file, "path")?;
            let hunks = file.get("hunks").ok_or_else(|| {
                format!(
                    "Lua skill '{}' change_set.files[{}].hunks must be present as one non-empty array for modify changes",
                    display_name, file_index
                )
            })?;
            let Value::Array(hunks) = hunks else {
                return Err(format!(
                    "Lua skill '{}' change_set.files[{}].hunks must be a non-empty array for modify changes",
                    display_name, file_index
                ));
            };
            if hunks.is_empty() {
                return Err(format!(
                    "Lua skill '{}' change_set.files[{}].hunks must be a non-empty array for modify changes",
                    display_name, file_index
                ));
            }
            for (hunk_index, hunk) in hunks.iter().enumerate() {
                validate_change_set_modify_hunk(display_name, file_index, hunk_index, hunk)?;
            }
        }
        "create" => {
            let _path =
                validate_change_set_absolute_path_field(display_name, file_index, file, "path")?;
            validate_change_set_required_string_field(
                display_name,
                &format!("change_set.files[{}].content", file_index),
                file.get("content"),
            )?;
        }
        "delete" => {
            let _path =
                validate_change_set_absolute_path_field(display_name, file_index, file, "path")?;
            let content_mode =
                validate_change_set_delete_content_mode(display_name, file_index, file)?;
            match content_mode {
                ChangeSetDeleteContentMode::Full => {
                    validate_change_set_required_string_field(
                        display_name,
                        &format!("change_set.files[{}].content", file_index),
                        file.get("content"),
                    )?;
                    validate_change_set_optional_non_negative_integer_field(
                        display_name,
                        &format!("change_set.files[{}].total_line_count", file_index),
                        file.get("total_line_count"),
                    )?;
                }
                ChangeSetDeleteContentMode::Truncated => {
                    if file.get("content").is_some() {
                        validate_change_set_required_string_field(
                            display_name,
                            &format!("change_set.files[{}].content", file_index),
                            file.get("content"),
                        )?;
                        validate_change_set_optional_non_negative_integer_field(
                            display_name,
                            &format!("change_set.files[{}].total_line_count", file_index),
                            file.get("total_line_count"),
                        )?;
                    } else {
                        validate_change_set_required_non_negative_integer_field(
                            display_name,
                            &format!("change_set.files[{}].total_line_count", file_index),
                            file.get("total_line_count"),
                        )?;
                        validate_change_set_required_string_field(
                            display_name,
                            &format!("change_set.files[{}].content_head", file_index),
                            file.get("content_head"),
                        )?;
                        validate_change_set_required_string_field(
                            display_name,
                            &format!("change_set.files[{}].content_tail", file_index),
                            file.get("content_tail"),
                        )?;
                    }
                }
            }
        }
        "rename" => {
            let _old_path = validate_change_set_absolute_path_field(
                display_name,
                file_index,
                file,
                "old_path",
            )?;
            let _new_path = validate_change_set_absolute_path_field(
                display_name,
                file_index,
                file,
                "new_path",
            )?;
        }
        _ => {
            return Err(format!(
                "Lua skill '{}' change_set.files[{}].change is unsupported",
                display_name, file_index
            ));
        }
    }
    Ok(())
}

/// Validate one delete content-mode field and default it to full mode when the field is absent.
/// 校验 delete 内容模式字段，并在字段缺失时默认回落为全文模式。
fn validate_change_set_delete_content_mode(
    display_name: &str,
    file_index: usize,
    file: &serde_json::Map<String, Value>,
) -> Result<ChangeSetDeleteContentMode, String> {
    ChangeSetDeleteContentMode::parse(file.get("content_mode")).ok_or_else(|| {
        format!(
            "Lua skill '{}' change_set.files[{}].content_mode must be '{}' or '{}'",
            display_name,
            file_index,
            ChangeSetDeleteContentMode::Full.as_str(),
            ChangeSetDeleteContentMode::Truncated.as_str()
        )
    })
}

/// Validate one absolute path field inside one canonical `change_set` file record.
/// 校验 canonical `change_set` 文件记录中的单个绝对路径字段。
fn validate_change_set_absolute_path_field(
    display_name: &str,
    file_index: usize,
    file: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<String, String> {
    let path = file
        .get(field_name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "Lua skill '{}' change_set.files[{}].{} must be one non-empty string",
                display_name, file_index, field_name
            )
        })?;
    if !Path::new(path).is_absolute() {
        return Err(format!(
            "Lua skill '{}' change_set.files[{}].{} must be absolute",
            display_name, file_index, field_name
        ));
    }
    Ok(path.to_string())
}

/// Validate one required string field inside one canonical `change_set` object path.
/// 校验 canonical `change_set` 对象路径中的单个必填字符串字段。
fn validate_change_set_required_string_field(
    display_name: &str,
    field_path: &str,
    value: Option<&Value>,
) -> Result<(), String> {
    match value {
        Some(Value::String(_)) => Ok(()),
        _ => Err(format!(
            "Lua skill '{}' {} must be a string",
            display_name, field_path
        )),
    }
}

/// Validate one required non-negative integer field inside one canonical `change_set` object path.
/// 校验 canonical `change_set` 对象路径中的单个必填非负整数字段。
fn validate_change_set_required_non_negative_integer_field(
    display_name: &str,
    field_path: &str,
    value: Option<&Value>,
) -> Result<usize, String> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            format!(
                "Lua skill '{}' {} must be one non-negative integer",
                display_name, field_path
            )
        })
}

/// Validate one optional non-negative integer field inside one canonical `change_set` object path.
/// 校验 canonical `change_set` 对象路径中的单个可选非负整数字段。
fn validate_change_set_optional_non_negative_integer_field(
    display_name: &str,
    field_path: &str,
    value: Option<&Value>,
) -> Result<Option<usize>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => validate_change_set_required_non_negative_integer_field(
            display_name,
            field_path,
            Some(value),
        )
        .map(Some),
    }
}

/// Normalize line endings so delete content truncation uses one stable newline convention.
/// 规范化换行，确保删除内容截断使用稳定统一的换行约定。
fn normalize_change_set_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Split normalized delete content into logical lines without treating one trailing newline as an extra line.
/// 把规范化后的删除内容拆成逻辑行，且不会把结尾换行误算成额外一行。
fn split_change_set_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.lines().collect()
    }
}

/// Validate one modify hunk record inside one canonical `change_set` file.
/// 校验 canonical `change_set` 文件中的单个 modify hunk 记录。
fn validate_change_set_modify_hunk(
    display_name: &str,
    file_index: usize,
    hunk_index: usize,
    hunk: &Value,
) -> Result<(), String> {
    let Value::Object(hunk) = hunk else {
        return Err(format!(
            "Lua skill '{}' change_set.files[{}].hunks[{}] must be an object",
            display_name, file_index, hunk_index
        ));
    };
    validate_change_set_required_string_field(
        display_name,
        &format!(
            "change_set.files[{}].hunks[{}].before",
            file_index, hunk_index
        ),
        hunk.get("before"),
    )?;
    validate_change_set_required_string_field(
        display_name,
        &format!(
            "change_set.files[{}].hunks[{}].after",
            file_index, hunk_index
        ),
        hunk.get("after"),
    )?;
    let deleted_count = validate_change_set_hunk_line_entries(
        display_name,
        file_index,
        hunk_index,
        "delete",
        hunk.get("delete"),
    )?;
    let inserted_count = validate_change_set_hunk_line_entries(
        display_name,
        file_index,
        hunk_index,
        "insert",
        hunk.get("insert"),
    )?;
    if deleted_count == 0 && inserted_count == 0 {
        return Err(format!(
            "Lua skill '{}' change_set.files[{}].hunks[{}] must include at least one deleted or inserted line",
            display_name, file_index, hunk_index
        ));
    }
    Ok(())
}

/// Validate one ordered delete/insert line list inside one canonical `change_set` hunk.
/// 校验 canonical `change_set` hunk 中的单个有序 delete/insert 行列表。
fn validate_change_set_hunk_line_entries(
    display_name: &str,
    file_index: usize,
    hunk_index: usize,
    entry_name: &str,
    value: Option<&Value>,
) -> Result<usize, String> {
    let value = value.ok_or_else(|| {
        format!(
            "Lua skill '{}' change_set.files[{}].hunks[{}].{} must be an array",
            display_name, file_index, hunk_index, entry_name
        )
    })?;
    let Value::Array(entries) = value else {
        return Err(format!(
            "Lua skill '{}' change_set.files[{}].hunks[{}].{} must be an array",
            display_name, file_index, hunk_index, entry_name
        ));
    };
    let mut previous_line = 0_i64;
    for (entry_index, entry) in entries.iter().enumerate() {
        let Value::Object(entry) = entry else {
            return Err(format!(
                "Lua skill '{}' change_set.files[{}].hunks[{}].{}[{}] must be an object",
                display_name, file_index, hunk_index, entry_name, entry_index
            ));
        };
        let line = entry
            .get("line")
            .and_then(Value::as_i64)
            .filter(|line| *line > 0)
            .ok_or_else(|| {
                format!(
                    "Lua skill '{}' change_set.files[{}].hunks[{}].{}[{}].line must be one positive integer",
                    display_name, file_index, hunk_index, entry_name, entry_index
                )
            })?;
        if entry_index > 0 && line <= previous_line {
            return Err(format!(
                "Lua skill '{}' change_set.files[{}].hunks[{}].{} line numbers must be strictly increasing",
                display_name, file_index, hunk_index, entry_name
            ));
        }
        previous_line = line;
        validate_change_set_required_string_field(
            display_name,
            &format!(
                "change_set.files[{}].hunks[{}].{}[{}].content",
                file_index, hunk_index, entry_name, entry_index
            ),
            entry.get("content"),
        )?;
    }
    Ok(entries.len())
}

/// Parse Lua multi-return values into the host's unified string-result protocol.
/// 把 Lua 工具的多返回值解析为宿主统一字符串结果协议。
pub(super) fn parse_tool_call_output(
    values: MultiValue,
    display_name: &str,
    invocation_context: Option<&LuaInvocationContext>,
) -> Result<RuntimeInvocationResult, String> {
    let host_result_capability = resolve_host_result_capability(invocation_context)?;
    let values_vec: Vec<LuaValue> = values.into_vec();
    if values_vec.is_empty() {
        return Err(format!(
            "Lua skill '{}' must return a plain string result",
            display_name
        ));
    }

    if values_vec.len() > 4 {
        return Err(format!(
            "Lua skill '{}' must return content[, overflow_mode[, template_hint[, host_result]]]",
            display_name
        ));
    }

    let content = match &values_vec[0] {
        LuaValue::String(text) => text
            .to_str()
            .map_err(|error| {
                format!(
                    "Lua skill '{}' returned an invalid UTF-8 string: {}",
                    display_name, error
                )
            })?
            .to_string(),
        other => {
            return Err(format!(
                "{} (skill='{}', actual_type='{}')",
                NON_STRING_TOOL_RESULT_ERROR,
                display_name,
                lua_value_type_name(other)
            ));
        }
    };

    let overflow_mode = match values_vec.get(1) {
        None | Some(LuaValue::Nil) => None,
        Some(LuaValue::String(text)) => {
            let mode_text = text.to_str().map_err(|error| {
                format!(
                    "Lua skill '{}' returned an invalid overflow mode string: {}",
                    display_name, error
                )
            })?;
            Some(ToolOverflowMode::parse(&mode_text).ok_or_else(|| {
                format!(
                    "Lua skill '{}' returned an unsupported overflow mode: {}",
                    display_name, mode_text
                )
            })?)
        }
        Some(_) => {
            return Err(format!(
                "Lua skill '{}' must return overflow mode as a string constant",
                display_name
            ));
        }
    };

    let template_hint = match values_vec.get(2) {
        None | Some(LuaValue::Nil) => None,
        Some(LuaValue::String(text)) => {
            let name = text.to_str().map_err(|error| {
                format!(
                    "Lua skill '{}' returned an invalid template name: {}",
                    display_name, error
                )
            })?;
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(_) => {
            return Err(format!(
                "Lua skill '{}' must return template_hint as a string",
                display_name
            ));
        }
    };

    let host_result =
        parse_host_result_value(values_vec.get(3), display_name, &host_result_capability)?;

    Ok(RuntimeInvocationResult::from_content_parts(
        content,
        overflow_mode,
        template_hint,
        host_result,
    ))
}
