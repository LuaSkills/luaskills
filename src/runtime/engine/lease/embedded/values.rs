//! Preserve the continuous integer domain at the embedded LuaJIT application-value boundary.
//! 在嵌入式 LuaJIT 应用值边界保留连续整数域。

use super::*;

impl LuaEngine {
    /// Largest integer in the continuous exact IEEE-754 binary64 domain; all limits derive from it.
    /// IEEE-754 binary64 连续精确整数域的最大整数；全部边界从此派生。
    pub(crate) const EMBEDDED_MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;

    /// Validate application `value` recursively at the host-selected root `path` before Lua conversion.
    /// 在 Lua 转换前于宿主选择的根 `path` 递归校验应用 `value`。
    /// Return InvalidArgument for unsafe JSON integers; explicit floats retain IEEE-754 semantics.
    /// 对不安全 JSON 整数返回 InvalidArgument；显式浮点数保持 IEEE-754 语义。
    /// Keep diagnostics independent of application keys and nesting so an error cannot expand with the value.
    /// 诊断不依赖应用键及嵌套深度，避免错误内容随应用值膨胀。
    pub(crate) fn validate_embedded_json_value(value: &Value, path: &str) -> EmbeddedResult<()> {
        match value {
            Value::Number(number) if number.is_i64() => {
                // This branch follows the actual JSON number variant, never a float's fractional part.
                // 此分支遵循真实 JSON 数字变体，绝不依据浮点数的小数部分判断。
                let integer = number.as_i64().expect("signed JSON integer variant");
                if !(-Self::EMBEDDED_MAX_SAFE_INTEGER..=Self::EMBEDDED_MAX_SAFE_INTEGER)
                    .contains(&integer)
                {
                    return Err(unsafe_integer(path));
                }
            }
            Value::Number(number) if number.is_u64() => {
                if number.as_u64().expect("unsigned JSON integer variant")
                    > Self::EMBEDDED_MAX_SAFE_INTEGER as u64
                {
                    return Err(unsafe_integer(path));
                }
            }
            Value::Array(array) => {
                for value in array {
                    Self::validate_embedded_json_value(value, path)?;
                }
            }
            Value::Object(object) => {
                // Object keys are application content and can exceed the error envelope budget when escaped.
                // 对象键属于应用内容，转义后可能超过错误信封预算。
                for value in object.values() {
                    Self::validate_embedded_json_value(value, path)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Validate `context` values before the shared request-context projector can push them into Lua.
    /// 在共享请求上下文投影器将值送入 Lua 前校验 `context`。
    /// Return a value-domain error or serialization failure without executing module code.
    /// 返回值域错误或序列化失败，不执行模块代码。
    pub(crate) fn validate_embedded_context(context: &LuaInvocationContext) -> EmbeddedResult<()> {
        Self::validate_embedded_json_value(&context.client_budget, "context/client_budget")?;
        Self::validate_embedded_json_value(&context.tool_config, "context/tool_config")?;
        if let Some(request) = &context.request_context {
            // Serialize the exact authoritative request type used by populate_vulcan_request_context.
            // 序列化 populate_vulcan_request_context 使用的精确权威请求类型。
            let value = serde_json::to_value(request).map_err(execution_error)?;
            Self::validate_embedded_json_value(&value, "context/request")?;
        }
        Ok(())
    }
}

/// Return the fixed host root `path` diagnostic without exposing application keys or rejected values.
/// 返回固定宿主根 `path` 诊断，不暴露应用键或被拒绝值。
fn unsafe_integer(path: &str) -> EmbeddedError {
    EmbeddedError::invalid(format!(
        "embedded Lua JSON integer at {path} exceeds the continuous safe integer range"
    ))
}

/// Convert checked application `value` using `lua` and the established container/null mapping.
/// 使用 `lua` 及既有容器、空值映射转换已校验应用 `value`。
/// Return the Lua value or the original structured value-domain/allocation error.
/// 返回 Lua 值或原始结构化值域、分配错误。
pub(super) fn to_lua(lua: &Lua, value: &Value, path: &str) -> mlua::Result<LuaValue> {
    LuaEngine::validate_embedded_json_value(value, path).map_err(mlua::Error::external)?;
    json_value_to_lua(lua, value)
}

/// Normalize serialized Lua `value` recursively, preserving safe integers and explicit floats.
/// 递归规范化已序列化 Lua `value`，保留安全整数及显式浮点数。
/// Return no value; integers outside the safe domain become explicitly floating JSON numbers.
/// 不返回值；安全域外的整数转为明确的 JSON 浮点数。
pub(super) fn normalize_lua_json(value: &mut Value) {
    match value {
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            // LuaJIT stores numbers as binary64; mlua's inferred integer tag cannot prove original JSON type.
            // LuaJIT 以 binary64 存储数字；mlua 推断的整数标签无法证明原始 JSON 类型。
            if LuaEngine::validate_embedded_json_value(value, "result").is_err() {
                // Every integer number converts to a finite binary64; its Lua value already had that precision.
                // 每个整数数字均可转为有限 binary64；其 Lua 值原本就具有该精度。
                let float = value
                    .as_f64()
                    .expect("JSON integer converts to finite float");
                *value =
                    Value::Number(serde_json::Number::from_f64(float).expect("finite integer"));
            }
        }
        Value::Array(array) => array.iter_mut().for_each(normalize_lua_json),
        Value::Object(object) => object.values_mut().for_each(normalize_lua_json),
        _ => {}
    }
}

/// Encode Lua callback `arguments` through the existing JSON mapping and normalize inferred integers.
/// 通过既有 JSON 映射编码 Lua 回调 `arguments`，并规范化推断整数。
/// Return host application arguments or a structured error for a non-JSON Lua value.
/// 返回宿主应用参数；非 JSON Lua 值返回结构化错误。
pub(super) fn callback_arguments(arguments: &LuaValue) -> EmbeddedResult<Value> {
    // Reuse the proven null, container and UTF-8 handling rather than introducing a second serializer.
    // 复用已验证的空值、容器及 UTF-8 处理，不引入第二套序列化器。
    let mut value = lua_value_to_json(arguments)
        .map_err(|_| EmbeddedError::invalid("capability arguments must be JSON values"))?;
    normalize_lua_json(&mut value);
    Ok(value)
}
