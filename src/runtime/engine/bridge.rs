use super::runlua::{require_string_arg, require_table_arg};
use super::*;

/// Create one Lua-facing runtime skill-management bridge function.
/// 创建一个面向 Lua 的运行时技能管理桥接函数。
pub(super) fn create_runtime_skill_management_bridge_fn(
    lua: &Lua,
    enabled: bool,
    action: RuntimeSkillManagementAction,
    function_name: &'static str,
) -> mlua::Result<Function> {
    let action_name = function_name.to_string();
    lua.create_function(move |lua, input: LuaValue| {
        if !enabled {
            return Err(mlua::Error::runtime(format!(
                "vulcan.runtime.skills.{} is disabled by host policy",
                action_name
            )));
        }

        let payload = lua_value_to_json(&input).map_err(|error| {
            mlua::Error::runtime(format!("vulcan.runtime.skills.{}: {}", action_name, error))
        })?;
        if management_payload_targets_root_layer(&payload) {
            return Err(mlua::Error::runtime(format!(
                "vulcan.runtime.skills.{} cannot target the system-controlled ROOT layer",
                action_name
            )));
        }
        let result = dispatch_skill_management_request(&RuntimeSkillManagementRequest {
            action: action.clone(),
            authority: SkillManagementAuthority::DelegatedTool,
            input: payload,
        })
        .map_err(|error| {
            mlua::Error::runtime(format!("vulcan.runtime.skills.{}: {}", action_name, error))
        })?;
        json_value_to_lua(lua, &result)
    })
}

/// Convert one host-tool bridge error into the stable Lua table result envelope.
/// 将单个宿主工具桥接错误转换为稳定的 Lua table 结果包络。
fn host_tool_error_value(code: &str, message: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

/// Convert one host-tool callback response into a Lua table-friendly result value.
/// 将单个宿主工具回调响应转换为便于 Lua table 通讯的结果值。
fn normalize_host_tool_call_response(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        other => json!({
            "ok": true,
            "value": other,
        }),
    }
}

/// Parse the host-tool `has` callback response into one boolean.
/// 将宿主工具 `has` 回调响应解析为布尔值。
fn parse_host_tool_has_response(value: &Value) -> Result<bool, String> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::Object(object) => {
            for key in ["exists", "has", "available"] {
                if let Some(Value::Bool(value)) = object.get(key) {
                    return Ok(*value);
                }
            }
            Err("host tool has callback must return a boolean or an object with boolean exists/has/available".to_string())
        }
        _ => Err("host tool has callback must return a boolean".to_string()),
    }
}

/// Convert a Lua host-tool args table into JSON while preserving empty args as an object.
/// 将 Lua 宿主工具参数表转换为 JSON，并把空参数保持为空对象。
fn host_tool_args_table_to_json(args_table: Table) -> Result<Value, String> {
    if args_table.raw_len() == 0 && args_table.pairs::<String, LuaValue>().next().is_none() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    lua_value_to_json(&LuaValue::Table(args_table))
}

/// Create the Lua-facing `vulcan.host.list` function.
/// 创建面向 Lua 的 `vulcan.host.list` 函数。
pub(super) fn create_host_tool_list_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |lua, ()| {
        if !try_has_host_tool_callback() {
            return Ok(LuaValue::Table(lua.create_table()?));
        }
        let result = dispatch_host_tool_request(&RuntimeHostToolRequest {
            action: RuntimeHostToolAction::List,
            tool_name: None,
            args: json!({}),
        })
        .map_err(|error| mlua::Error::runtime(format!("vulcan.host.list: {}", error)))?;
        json_value_to_lua(lua, &result)
    })
}

/// Create the Lua-facing `vulcan.host.has` and `vulcan.host.has_tool` function.
/// 创建面向 Lua 的 `vulcan.host.has` 与 `vulcan.host.has_tool` 函数。
pub(super) fn create_host_tool_has_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |_, tool_name: LuaValue| {
        let tool_name = require_string_arg(tool_name, "host.has", "tool_name", false)?;
        if !try_has_host_tool_callback() {
            return Ok(false);
        }
        let result = dispatch_host_tool_request(&RuntimeHostToolRequest {
            action: RuntimeHostToolAction::Has,
            tool_name: Some(tool_name),
            args: json!({}),
        })
        .map_err(|error| mlua::Error::runtime(format!("vulcan.host.has: {}", error)))?;
        parse_host_tool_has_response(&result)
            .map_err(|error| mlua::Error::runtime(format!("vulcan.host.has: {}", error)))
    })
}

/// Create the Lua-facing `vulcan.host.call` function.
/// 创建面向 Lua 的 `vulcan.host.call` 函数。
pub(super) fn create_host_tool_call_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |lua, (tool_name, args): (LuaValue, LuaValue)| {
        let tool_name = require_string_arg(tool_name, "host.call", "tool_name", false)?;
        let args_table = require_table_arg(args, "host.call", "args")?;
        let args_value = host_tool_args_table_to_json(args_table).map_err(|error| {
            mlua::Error::runtime(format!("vulcan.host.call: invalid args table: {}", error))
        })?;
        let result = if try_has_host_tool_callback() {
            match dispatch_host_tool_request(&RuntimeHostToolRequest {
                action: RuntimeHostToolAction::Call,
                tool_name: Some(tool_name.clone()),
                args: args_value,
            }) {
                Ok(value) => normalize_host_tool_call_response(value),
                Err(error) => host_tool_error_value("host_tool_callback_error", error),
            }
        } else {
            host_tool_error_value(
                "host_tool_callback_missing",
                format!(
                    "host tool bridge has no registered callback for '{}'",
                    tool_name
                ),
            )
        };
        json_value_to_lua(lua, &result)
    })
}

/// Convert one optional model usage object into a JSON object.
/// 将单个可选模型用量对象转换为 JSON 对象。
fn model_usage_value(usage: RuntimeModelUsage) -> Value {
    let mut usage_object = serde_json::Map::new();
    if let Some(input_tokens) = usage.input_tokens {
        usage_object.insert("input_tokens".to_string(), json!(input_tokens));
    }
    if let Some(output_tokens) = usage.output_tokens {
        usage_object.insert("output_tokens".to_string(), json!(output_tokens));
    }
    Value::Object(usage_object)
}

/// Convert one structured model error into the stable Lua table result envelope.
/// 将单个结构化模型错误转换为稳定的 Lua table 返回包络。
fn runtime_model_error_value(error: RuntimeModelError) -> Value {
    let mut error_object = serde_json::Map::new();
    error_object.insert(
        "code".to_string(),
        Value::String(error.code.as_str().to_string()),
    );
    error_object.insert("message".to_string(), Value::String(error.message));
    if let Some(provider_message) = error.provider_message {
        error_object.insert(
            "provider_message".to_string(),
            Value::String(provider_message),
        );
    }
    if let Some(provider_code) = error.provider_code {
        error_object.insert("provider_code".to_string(), Value::String(provider_code));
    }
    if let Some(provider_status) = error.provider_status {
        error_object.insert("provider_status".to_string(), json!(provider_status));
    }
    json!({
        "ok": false,
        "error": Value::Object(error_object),
    })
}

/// Build one structured model error without provider-specific fields.
/// 构造一个不带 provider 特定字段的结构化模型错误。
fn runtime_model_error(
    code: RuntimeModelErrorCode,
    message: impl Into<String>,
) -> RuntimeModelError {
    RuntimeModelError {
        code,
        message: message.into(),
        provider_message: None,
        provider_code: None,
        provider_status: None,
    }
}

/// Convert one successful embedding callback response into the Lua result envelope.
/// 将单个成功的 embedding 回调响应转换为 Lua 返回包络。
fn runtime_model_embed_response_value(response: RuntimeModelEmbedResponse) -> Value {
    let mut result = json!({
        "ok": true,
        "vector": response.vector,
        "dimensions": response.dimensions,
    });
    if let Some(usage) = response.usage
        && let Value::Object(object) = &mut result
    {
        object.insert("usage".to_string(), model_usage_value(usage));
    }
    result
}

/// Convert one successful LLM callback response into the Lua result envelope.
/// 将单个成功的 LLM 回调响应转换为 Lua 返回包络。
fn runtime_model_llm_response_value(response: RuntimeModelLlmResponse) -> Value {
    let mut result = json!({
        "ok": true,
        "assistant": response.assistant,
    });
    if let Some(usage) = response.usage
        && let Value::Object(object) = &mut result
    {
        object.insert("usage".to_string(), model_usage_value(usage));
    }
    result
}

/// Read one exact non-empty UTF-8 string argument for a model function.
/// 为模型函数读取一个精确的非空 UTF-8 字符串参数。
fn runtime_model_string_arg(
    values: &[LuaValue],
    index: usize,
    fn_name: &str,
    param_name: &str,
) -> Result<String, RuntimeModelError> {
    let value = values.get(index).ok_or_else(|| {
        runtime_model_error(
            RuntimeModelErrorCode::InvalidArgument,
            format!("{fn_name}: {param_name} is required"),
        )
    })?;
    let text = match value {
        LuaValue::String(text) => text
            .to_str()
            .map_err(|_| {
                runtime_model_error(
                    RuntimeModelErrorCode::InvalidArgument,
                    format!("{fn_name}: {param_name} must be a valid UTF-8 string"),
                )
            })?
            .to_string(),
        other => {
            return Err(runtime_model_error(
                RuntimeModelErrorCode::InvalidArgument,
                format!(
                    "{fn_name}: {param_name} must be a string, got {}",
                    lua_value_type_name(other)
                ),
            ));
        }
    };
    if text.trim().is_empty() {
        return Err(runtime_model_error(
            RuntimeModelErrorCode::InvalidArgument,
            format!("{fn_name}: {param_name} must not be empty"),
        ));
    }
    if text.contains('\0') {
        return Err(runtime_model_error(
            RuntimeModelErrorCode::InvalidArgument,
            format!("{fn_name}: {param_name} must not contain NUL bytes"),
        ));
    }
    Ok(text)
}

/// Validate the exact argument count for one fixed model API.
/// 校验单个固定模型 API 的精确参数数量。
fn validate_runtime_model_arg_count(
    actual: usize,
    expected: usize,
    fn_name: &str,
) -> Result<(), RuntimeModelError> {
    if actual == expected {
        return Ok(());
    }
    Err(runtime_model_error(
        RuntimeModelErrorCode::InvalidArgument,
        format!("{fn_name}: expected {expected} argument(s), got {actual}"),
    ))
}

/// Capture the current runtime caller context for one host model callback.
/// 为单个宿主模型回调捕获当前运行时调用方上下文。
fn current_runtime_model_caller(lua: &Lua) -> Result<RuntimeModelCaller, String> {
    let internal = get_vulcan_runtime_internal_table(lua)?;
    let context = get_vulcan_context_table(lua)?;
    let request_value: LuaValue = context
        .get("request")
        .map_err(|error| format!("Failed to read vulcan.context.request: {}", error))?;
    let request_json = lua_value_to_json(&request_value)
        .map_err(|error| format!("Failed to convert request context to JSON: {}", error))?;
    let request_context =
        parse_runtime_request_context_json(request_json, "vulcan.context.request")?;
    let client_name = request_context
        .as_ref()
        .and_then(|context| context.client_name.clone())
        .or_else(|| {
            request_context
                .as_ref()
                .and_then(|context| context.client_info.as_ref())
                .and_then(|client_info| client_info.name.clone())
        });
    let request_id = request_context
        .as_ref()
        .and_then(|context| context.request_id.clone());
    Ok(RuntimeModelCaller {
        skill_id: internal.get("skill_name").map_err(|error| {
            format!(
                "Failed to read vulcan.runtime.internal.skill_name: {}",
                error
            )
        })?,
        entry_name: internal.get("entry_name").map_err(|error| {
            format!(
                "Failed to read vulcan.runtime.internal.entry_name: {}",
                error
            )
        })?,
        canonical_tool_name: internal.get("tool_name").map_err(|error| {
            format!(
                "Failed to read vulcan.runtime.internal.tool_name: {}",
                error
            )
        })?,
        root_name: internal.get("root_name").map_err(|error| {
            format!(
                "Failed to read vulcan.runtime.internal.root_name: {}",
                error
            )
        })?,
        skill_dir: context
            .get("skill_dir")
            .map_err(|error| format!("Failed to read vulcan.context.skill_dir: {}", error))?,
        client_name,
        request_id,
    })
}

/// Create the Lua-facing `vulcan.models.status` function.
/// 创建面向 Lua 的 `vulcan.models.status` 函数。
pub(super) fn create_model_status_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |lua, _: MultiValue| {
        let result = json!({
            "ok": true,
            "capabilities": {
                "embed": try_has_model_embed_callback(),
                "llm": try_has_model_llm_callback(),
            },
        });
        json_value_to_lua(lua, &result)
    })
}

/// Create the Lua-facing `vulcan.models.has` function.
/// 创建面向 Lua 的 `vulcan.models.has` 函数。
pub(super) fn create_model_has_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |_, args: MultiValue| {
        let values = args.into_vec();
        if values.len() != 1 {
            return Ok(false);
        }
        let capability = match &values[0] {
            LuaValue::String(text) => text.to_str().map(|text| text.to_string()).ok(),
            _ => None,
        };
        let available = match capability.as_deref() {
            Some("embed") => try_has_model_embed_callback(),
            Some("llm") => try_has_model_llm_callback(),
            _ => false,
        };
        Ok(available)
    })
}

/// Create the Lua-facing `vulcan.models.embed` function.
/// 创建面向 Lua 的 `vulcan.models.embed` 函数。
pub(super) fn create_model_embed_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let values = args.into_vec();
        let result = (|| -> Result<Value, RuntimeModelError> {
            validate_runtime_model_arg_count(values.len(), 1, "vulcan.models.embed")?;
            let text = runtime_model_string_arg(&values, 0, "vulcan.models.embed", "text")?;
            let caller = current_runtime_model_caller(lua).map_err(|error| {
                runtime_model_error(RuntimeModelErrorCode::InternalError, error)
            })?;
            dispatch_model_embed_request(&RuntimeModelEmbedRequest { text, caller })
                .map(runtime_model_embed_response_value)
        })();
        let value = match result {
            Ok(value) => value,
            Err(error) => runtime_model_error_value(error),
        };
        json_value_to_lua(lua, &value)
    })
}

/// Create the Lua-facing `vulcan.models.llm` function.
/// 创建面向 Lua 的 `vulcan.models.llm` 函数。
pub(super) fn create_model_llm_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let values = args.into_vec();
        let result = (|| -> Result<Value, RuntimeModelError> {
            validate_runtime_model_arg_count(values.len(), 2, "vulcan.models.llm")?;
            let system = runtime_model_string_arg(&values, 0, "vulcan.models.llm", "system")?;
            let user = runtime_model_string_arg(&values, 1, "vulcan.models.llm", "user")?;
            let caller = current_runtime_model_caller(lua).map_err(|error| {
                runtime_model_error(RuntimeModelErrorCode::InternalError, error)
            })?;
            dispatch_model_llm_request(&RuntimeModelLlmRequest {
                system,
                user,
                caller,
            })
            .map(runtime_model_llm_response_value)
        })();
        let value = match result {
            Ok(value) => value,
            Err(error) => runtime_model_error_value(error),
        };
        json_value_to_lua(lua, &value)
    })
}

/// Return whether one JSON value is exactly the ROOT layer label.
/// 返回单个 JSON 值是否正好是 ROOT 层标签。
fn payload_string_is_root_layer(value: &Value) -> bool {
    value
        .as_str()
        .map(|value| value.trim().eq_ignore_ascii_case("ROOT"))
        .unwrap_or(false)
}

/// Return whether one target-root payload value identifies the ROOT layer.
/// 返回单个目标根载荷值是否指向 ROOT 层。
fn root_target_payload_value_targets_root(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized_key = key.replace(['_', '-'], "").to_ascii_lowercase();
            let is_identity_key = matches!(
                normalized_key.as_str(),
                "name" | "label" | "rootname" | "layer" | "targetlayer"
            );
            (is_identity_key && payload_string_is_root_layer(value))
                || root_target_payload_value_targets_root(value)
        }),
        Value::Array(items) => items.iter().any(root_target_payload_value_targets_root),
        _ => payload_string_is_root_layer(value),
    }
}

/// Return whether one Lua skill-management payload explicitly requests the ROOT layer.
/// 返回单个 Lua 技能管理载荷是否显式请求 ROOT 层。
fn management_payload_targets_root_layer(payload: &Value) -> bool {
    match payload {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized_key = key.replace(['_', '-'], "").to_ascii_lowercase();
            let is_layer_key = matches!(
                normalized_key.as_str(),
                "layer" | "targetlayer" | "root" | "rootname" | "targetroot" | "targetrootname"
            );
            let targets_root = is_layer_key && root_target_payload_value_targets_root(value);
            targets_root || management_payload_targets_root_layer(value)
        }),
        Value::Array(items) => items.iter().any(management_payload_targets_root_layer),
        _ => false,
    }
}

/// Return whether a root chain contains one formal layer label.
/// 返回根链中是否包含指定正式层级标签。
fn runtime_skill_roots_contain_label(skill_roots: &[RuntimeSkillRoot], label: &str) -> bool {
    skill_roots
        .iter()
        .any(|root| root.name.trim().eq_ignore_ascii_case(label))
}

/// Build the Lua-visible layer discovery response for ordinary skill management.
/// 构造普通技能管理在 Lua 侧可见的层级发现响应。
pub(super) fn create_runtime_skill_layers_fn(
    lua: &Lua,
    skill_roots: &[RuntimeSkillRoot],
    skill_management_enabled: bool,
) -> mlua::Result<Function> {
    let mut available_layers = Vec::new();
    for label in ["PROJECT", "USER"] {
        if runtime_skill_roots_contain_label(skill_roots, label) {
            available_layers.push(label.to_string());
        }
    }
    let default_layer = if available_layers.iter().any(|label| label == "USER") {
        Some("USER".to_string())
    } else if available_layers.iter().any(|label| label == "PROJECT") {
        Some("PROJECT".to_string())
    } else {
        None
    };
    let any_layer_writable = skill_management_enabled && !available_layers.is_empty();
    lua.create_function(move |lua, ()| {
        let result = lua.create_table()?;
        if let Some(default_layer) = default_layer.as_deref() {
            result.set("default", default_layer)?;
        }
        result.set("writable", any_layer_writable)?;

        let labels = lua.create_table()?;
        for (index, label) in available_layers.iter().enumerate() {
            labels.set(index + 1, label.as_str())?;
        }
        result.set("labels", labels)?;

        let layers = lua.create_table()?;
        for (index, label) in available_layers.iter().enumerate() {
            let layer = lua.create_table()?;
            layer.set("label", label.as_str())?;
            layer.set("writable", skill_management_enabled)?;
            let description = match label.as_str() {
                "PROJECT" => "Project skill layer",
                "USER" => "User skill layer",
                _ => "Skill layer",
            };
            layer.set("description", description)?;
            layers.set(index + 1, layer)?;
        }
        result.set("layers", layers)?;

        Ok(result)
    })
}
