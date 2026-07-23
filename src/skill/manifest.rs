use semver::Version;
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::fs;
use std::path::Path;

use crate::runtime::path::render_host_visible_path;
use crate::skill::config::{
    SkillPackageConfigDeclaration, validate_skill_package_config_declarations,
};

// ============================================================
// Lua Skill metadata (loaded from skill.yaml only)
// ============================================================

/// Skill-scoped LanceDB logging level.
/// Skill 级 LanceDB 宿主日志级别配置。
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillLanceDbLogLevel {
    /// Disable host-side LanceDB logs except hard failures.
    /// 除硬错误外关闭宿主侧 LanceDB 日志。
    Off,
    /// Emit informational host-side LanceDB logs.
    /// 输出信息级宿主 LanceDB 日志。
    #[default]
    Info,
    /// Emit only warning/error host-side LanceDB logs.
    /// 仅输出告警/错误级宿主 LanceDB 日志。
    Warning,
}

impl SkillLanceDbLogLevel {
    /// Return the stable wire name of the current log level.
    /// 返回当前日志级别对应的稳定名称。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Info => "info",
            Self::Warning => "warning",
        }
    }
}

/// Skill-scoped SQLite logging level.
/// Skill 级 SQLite 宿主日志级别配置。
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillSqliteLogLevel {
    /// Disable host-side SQLite logs except hard failures.
    /// 除硬错误外关闭宿主侧 SQLite 日志。
    Off,
    /// Emit informational host-side SQLite logs.
    /// 输出信息级宿主 SQLite 日志。
    #[default]
    Info,
    /// Emit only warning/error host-side SQLite logs.
    /// 仅输出告警/错误级宿主 SQLite 日志。
    Warning,
}

impl SkillSqliteLogLevel {
    /// Return the stable wire name of the current log level.
    /// 返回当前日志级别对应的稳定名称。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Info => "info",
            Self::Warning => "warning",
        }
    }
}

/// Skill-scoped LanceDB configuration object.
/// Skill 级 LanceDB 配置对象。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SkillLanceDbMeta {
    /// Whether the current skill should receive a dedicated host-managed LanceDB instance.
    /// 当前 skill 是否需要启用宿主管理的专属 LanceDB 实例。
    #[serde(default)]
    pub enable: bool,
    /// Host-side LanceDB log level.
    /// 宿主侧 LanceDB 日志级别。
    #[serde(default)]
    pub log_level: SkillLanceDbLogLevel,
    /// Whether slow-operation logging is enabled.
    /// 是否开启慢操作日志。
    #[serde(default)]
    pub slow_log_enabled: bool,
    /// Slow-operation threshold in milliseconds.
    /// 慢操作阈值（毫秒）。
    #[serde(default = "default_lancedb_slow_log_threshold_ms")]
    pub slow_log_threshold_ms: u64,
}

/// Skill-scoped SQLite configuration object.
/// Skill 级 SQLite 配置对象。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SkillSqliteMeta {
    /// Whether the current skill should receive a dedicated host-managed SQLite instance.
    /// 当前 skill 是否需要启用宿主管理的专属 SQLite 实例。
    #[serde(default)]
    pub enable: bool,
    /// Host-side SQLite log level.
    /// 宿主侧 SQLite 日志级别。
    #[serde(default)]
    pub log_level: SkillSqliteLogLevel,
    /// Whether slow-operation logging is enabled.
    /// 是否开启慢操作日志。
    #[serde(default)]
    pub slow_log_enabled: bool,
    /// Slow-operation threshold in milliseconds.
    /// 慢操作阈值（毫秒）。
    #[serde(default = "default_sqlite_slow_log_threshold_ms")]
    pub slow_log_threshold_ms: u64,
}

/// Default slow-operation threshold for host-side LanceDB logs.
/// 宿主侧 LanceDB 慢操作日志默认阈值。
fn default_lancedb_slow_log_threshold_ms() -> u64 {
    800
}

/// Default slow-operation threshold for host-side SQLite logs.
/// 宿主侧 SQLite 慢操作日志默认阈值。
fn default_sqlite_slow_log_threshold_ms() -> u64 {
    500
}

/// Help node metadata used by the new help structure.
/// 新 help 结构使用的帮助节点元数据。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SkillHelpNodeMeta {
    /// Optional node name used by topic/workflow nodes.
    /// 供 topic/workflow 节点使用的可选名称。
    #[serde(default)]
    pub name: String,
    /// Optional human-readable description of the node.
    /// 当前节点的人类可读描述。
    #[serde(default)]
    pub description: String,
    /// Relative file path of the help payload under `help/`.
    /// 位于 `help/` 目录下的帮助载荷相对路径。
    #[serde(default)]
    pub file: String,
}

/// Top-level help metadata declared by a skill.
/// skill 顶层声明的 help 元数据。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SkillHelpMeta {
    /// Main help node used for skill-level overview.
    /// 用于 skill 级总览的主帮助节点。
    #[serde(default)]
    pub main: SkillHelpNodeMeta,
    /// Topic or workflow help nodes listed under the main help.
    /// 挂在主帮助下的 topic 或 workflow 子节点。
    #[serde(default)]
    pub topics: Vec<SkillHelpNodeMeta>,
}

/// Shared parameter metadata used by tool entries.
/// 工具入口共用的参数元数据。
#[derive(Deserialize, Debug, Clone)]
pub struct SkillParam {
    /// Parameter name.
    /// 参数名称。
    pub name: String,
    /// Parameter type string used by JSON Schema.
    /// JSON Schema 使用的参数类型字符串。
    #[serde(rename = "type")]
    pub param_type: String,
    /// Parameter description.
    /// 参数描述。
    pub description: String,
    /// Whether the parameter is required.
    /// 参数是否必填。
    #[serde(default)]
    pub required: bool,
}

/// Strict top-level entry metadata used by the new LuaSkills package layout.
/// 新 LuaSkills 包结构使用的严格顶层入口元数据。
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SkillToolMeta {
    /// Local entry name used inside the skill namespace.
    /// skill 命名空间内部使用的局部入口名称。
    pub name: String,
    /// Human-readable entry description shown in tools/list.
    /// 展示在 tools/list 中的人类可读描述。
    #[serde(default)]
    pub description: String,
    /// Relative Lua entry filename under `runtime/`.
    /// 位于 `runtime/` 目录下的相对 Lua 入口文件路径。
    pub lua_entry: String,
    /// Lua module registration name.
    /// Lua 模块注册名称。
    pub lua_module: String,
    /// Parameter definitions specific to this entry.
    /// 当前入口独有的参数定义。
    #[serde(default)]
    pub parameters: Vec<SkillParam>,
    /// Optional inline JSON Schema object used as the final AI-facing input schema.
    /// 作为最终面向 AI 输入 schema 的可选内联 JSON Schema 对象。
    #[serde(default)]
    pub input_schema: Option<JsonValue>,
    /// Optional JSON Schema file path under `schemas/` used to define the final AI-facing input schema.
    /// 位于 `schemas/` 目录下、用于定义最终面向 AI 输入 schema 的可选 JSON Schema 文件路径。
    #[serde(default)]
    pub input_schema_file: String,
    /// Optional help topic/workflow reference name.
    /// 可选的帮助 topic/workflow 引用名称。
    #[serde(default)]
    pub help: String,
}

/// Strict skill-level metadata loaded only from skill.yaml.
/// 仅从 skill.yaml 加载的严格 skill 级元数据。
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SkillMeta {
    /// Internal skill name, for example "vulcan-codekit".
    /// 内部 skill 名称，例如 "vulcan-codekit"。
    pub name: String,
    /// Semantic package version declared by the current skill manifest.
    /// 当前技能清单声明的语义化包版本。
    pub version: String,
    /// Whether the current skill is allowed to load. Defaults to enabled when omitted.
    /// 当前 skill 是否允许被加载；省略时默认启用。
    #[serde(default = "default_skill_enable_flag")]
    pub enable: bool,
    /// Debug mode: reload Lua source from disk on each invocation.
    /// 调试模式：每次调用时都从磁盘热加载 Lua 源文件。
    #[serde(default)]
    pub debug: bool,
    /// Package-level configuration declarations shared by every entry in this skill package.
    /// 当前技能包全部入口共享的包级配置声明。
    #[serde(default)]
    pub config: Vec<SkillPackageConfigDeclaration>,
    /// Structured LanceDB configuration used by the host-managed binding.
    /// 宿主管理的 LanceDB 绑定所使用的结构化配置对象。
    #[serde(default)]
    pub lancedb: SkillLanceDbMeta,
    /// Structured SQLite configuration used by the host-managed binding.
    /// 宿主管理的 SQLite 绑定所使用的结构化配置对象。
    #[serde(default)]
    pub sqlite: SkillSqliteMeta,
    /// Top-level entry declarations used by the strict LuaSkills package layout.
    /// 严格 LuaSkills 包结构使用的顶层入口声明。
    #[serde(default)]
    pub entries: Vec<SkillToolMeta>,
    /// New help metadata used to replace prompt-based guidance.
    /// 用于替代 prompt 说明的新 help 元数据。
    #[serde(default)]
    pub help: SkillHelpMeta,
    /// Stable runtime skill identifier derived from the physical directory name.
    /// 从物理目录名称派生出的稳定运行时技能标识符。
    #[serde(skip)]
    resolved_skill_id: String,
}

/// Return whether one LuaSkills identifier follows the strict lowercase-hyphen rule.
/// 判断某个 LuaSkills 标识符是否满足严格的小写短横线规则。
pub fn is_valid_luaskills_identifier(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    let mut chars = trimmed.chars();
    let Some(first_char) = chars.next() else {
        return false;
    };
    if !first_char.is_ascii_lowercase() {
        return false;
    }
    if trimmed.ends_with('-') {
        return false;
    }

    chars.all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    })
}

/// Validate one LuaSkills identifier and return a bilingual error when it is invalid.
/// 校验单个 LuaSkills 标识符，并在非法时返回双语错误文本。
pub fn validate_luaskills_identifier(value: &str, label: &str) -> Result<(), String> {
    if is_valid_luaskills_identifier(value) {
        return Ok(());
    }

    Err(format!("{label} must match ^[a-z]([a-z0-9-]*[a-z0-9])?$"))
}

/// Validate one LuaSkills semantic version string and return an English error when it is invalid.
/// 校验单个 LuaSkills 语义化版本字符串，并在非法时返回英文错误。
pub fn validate_luaskills_version(value: &str, label: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    Version::parse(trimmed)
        .map(|_| ())
        .map_err(|error| format!("{label} must be a valid semantic version: {}", error))
}

/// Return the default enabled flag for one skill manifest.
/// 返回单个技能清单的默认启用标记。
fn default_skill_enable_flag() -> bool {
    true
}

/// Validate one relative metadata path against a fixed prefix and reject traversal.
/// 按固定目录前缀校验单个 skill 元数据相对路径，并拒绝路径穿越。
fn validate_skill_relative_path(
    relative_path: &str,
    expected_prefix: &str,
    field_label: &str,
) -> Result<(), String> {
    let trimmed = relative_path.trim();
    if trimmed.is_empty() {
        return Err(format!("{field_label} must not be empty"));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "{field_label} must be a relative path under {expected_prefix}"
        ));
    }

    let normalized = trimmed.replace('\\', "/");
    let required_prefix = format!("{expected_prefix}/");
    if !normalized.starts_with(&required_prefix) {
        return Err(format!("{field_label} must start with {required_prefix}"));
    }

    for component in path.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(format!("{field_label} must not contain parent"));
        }
    }

    Ok(())
}

/// Validate one JSON Schema `type` field used by one AI-facing tool schema node.
/// 校验单个面向 AI 的工具 schema 节点使用的 JSON Schema `type` 字段。
fn validate_tool_schema_type_field(
    object: &JsonMap<String, JsonValue>,
    field_label: &str,
) -> Result<(), String> {
    let Some(type_value) = object.get("type") else {
        return Ok(());
    };
    match type_value {
        JsonValue::String(type_name) => {
            if type_name.trim().is_empty() {
                return Err(format!("{field_label}.type must not be empty"));
            }
        }
        JsonValue::Array(items) => {
            if items.is_empty() {
                return Err(format!("{field_label}.type must not be an empty array"));
            }
            for (index, item) in items.iter().enumerate() {
                let type_name = item
                    .as_str()
                    .ok_or_else(|| format!("{field_label}.type[{index}] must be one string"))?;
                if type_name.trim().is_empty() {
                    return Err(format!("{field_label}.type[{index}] must not be empty"));
                }
            }
        }
        _ => {
            return Err(format!(
                "{field_label}.type must be one string or one string array"
            ));
        }
    }
    Ok(())
}

/// Validate one JSON Schema `description` annotation when it is present.
/// 校验单个 JSON Schema `description` 注解存在时的类型。
fn validate_tool_schema_description_field(
    object: &JsonMap<String, JsonValue>,
    field_label: &str,
) -> Result<(), String> {
    if let Some(description) = object.get("description")
        && !description.is_string()
    {
        return Err(format!("{field_label}.description must be one string"));
    }
    Ok(())
}

/// Validate one JSON Schema string-name array such as `required`.
/// 校验单个 JSON Schema 字符串名称数组，例如 `required`。
fn validate_tool_schema_name_array(value: &JsonValue, field_label: &str) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{field_label} must be an array of strings"))?;
    for (index, item) in items.iter().enumerate() {
        let item_text = item
            .as_str()
            .ok_or_else(|| format!("{field_label}[{index}] must be one string"))?;
        if item_text.trim().is_empty() {
            return Err(format!("{field_label}[{index}] must not be empty"));
        }
    }
    Ok(())
}

/// Validate one JSON Schema array of nested schema nodes.
/// 校验单个由嵌套 schema 节点组成的 JSON Schema 数组。
fn validate_tool_schema_node_array(value: &JsonValue, field_label: &str) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{field_label} must be an array of schema objects"))?;
    if items.is_empty() {
        return Err(format!("{field_label} must not be empty"));
    }
    for (index, item) in items.iter().enumerate() {
        validate_tool_schema_node(item, &format!("{field_label}[{index}]"))?;
    }
    Ok(())
}

/// Validate one JSON Schema object-map whose values are nested schema nodes.
/// 校验单个值为嵌套 schema 节点的 JSON Schema 对象映射。
fn validate_tool_schema_object_map(value: &JsonValue, field_label: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{field_label} must be an object"))?;
    for (name, item) in object {
        if name.trim().is_empty() {
            return Err(format!(
                "{field_label} must not contain empty property names"
            ));
        }
        validate_tool_schema_node(item, &format!("{field_label}.{}", name))?;
    }
    Ok(())
}

/// Validate one JSON Schema node used inside one AI-facing tool input schema tree.
/// 校验单个面向 AI 的工具输入 schema 树中使用的 JSON Schema 节点。
fn validate_tool_schema_node(schema: &JsonValue, field_label: &str) -> Result<(), String> {
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{field_label} must be one JSON object schema"))?;

    validate_tool_schema_type_field(object, field_label)?;
    validate_tool_schema_description_field(object, field_label)?;

    if let Some(properties) = object.get("properties") {
        validate_tool_schema_object_map(properties, &format!("{field_label}.properties"))?;
    }
    if let Some(pattern_properties) = object.get("patternProperties") {
        validate_tool_schema_object_map(
            pattern_properties,
            &format!("{field_label}.patternProperties"),
        )?;
    }
    if let Some(definitions) = object.get("$defs").or_else(|| object.get("definitions")) {
        validate_tool_schema_object_map(definitions, &format!("{field_label}.$defs"))?;
    }
    if let Some(required) = object.get("required") {
        validate_tool_schema_name_array(required, &format!("{field_label}.required"))?;
    }
    if let Some(enum_values) = object.get("enum") {
        enum_values
            .as_array()
            .ok_or_else(|| format!("{field_label}.enum must be an array"))?;
    }
    if let Some(items) = object.get("items") {
        match items {
            JsonValue::Object(_) => {
                validate_tool_schema_node(items, &format!("{field_label}.items"))?;
            }
            JsonValue::Array(_) => {
                validate_tool_schema_node_array(items, &format!("{field_label}.items"))?;
            }
            _ => {
                return Err(format!(
                    "{field_label}.items must be one schema object or one schema array"
                ));
            }
        }
    }
    if let Some(prefix_items) = object.get("prefixItems") {
        validate_tool_schema_node_array(prefix_items, &format!("{field_label}.prefixItems"))?;
    }
    if let Some(contains) = object.get("contains") {
        validate_tool_schema_node(contains, &format!("{field_label}.contains"))?;
    }
    if let Some(property_names) = object.get("propertyNames") {
        validate_tool_schema_node(property_names, &format!("{field_label}.propertyNames"))?;
    }
    if let Some(additional_properties) = object.get("additionalProperties") {
        match additional_properties {
            JsonValue::Bool(_) => {}
            JsonValue::Object(_) => validate_tool_schema_node(
                additional_properties,
                &format!("{field_label}.additionalProperties"),
            )?,
            _ => {
                return Err(format!(
                    "{field_label}.additionalProperties must be one boolean or one schema object"
                ));
            }
        }
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(value) = object.get(keyword) {
            validate_tool_schema_node_array(value, &format!("{field_label}.{keyword}"))?;
        }
    }
    if let Some(not_schema) = object.get("not") {
        validate_tool_schema_node(not_schema, &format!("{field_label}.not"))?;
    }
    if let Some(if_schema) = object.get("if") {
        validate_tool_schema_node(if_schema, &format!("{field_label}.if"))?;
    }
    if let Some(then_schema) = object.get("then") {
        validate_tool_schema_node(then_schema, &format!("{field_label}.then"))?;
    }
    if let Some(else_schema) = object.get("else") {
        validate_tool_schema_node(else_schema, &format!("{field_label}.else"))?;
    }

    Ok(())
}

/// Validate one final entry input schema before it is exposed to hosts and models.
/// 在把最终入口输入 schema 暴露给宿主与模型之前进行校验。
fn validate_entry_input_schema_root(schema: &JsonValue, field_label: &str) -> Result<(), String> {
    validate_tool_schema_node(schema, field_label)?;
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{field_label} must be one JSON object schema"))?;
    match object.get("type") {
        Some(JsonValue::String(type_name)) if type_name == "object" => Ok(()),
        Some(_) => Err(format!("{field_label}.type must be \"object\"")),
        None => Err(format!(
            "{field_label}.type must be present and equal to \"object\""
        )),
    }
}

/// Build one legacy object-style input schema from the flat `parameters` list.
/// 基于扁平 `parameters` 列表构造单个旧版对象式输入 schema。
fn build_entry_input_schema_from_parameters(parameters: &[SkillParam]) -> JsonValue {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for parameter in parameters {
        properties.insert(
            parameter.name.clone(),
            build_parameter_schema_fragment(parameter),
        );
        if parameter.required {
            required.push(JsonValue::String(parameter.name.clone()));
        }
    }

    let mut schema = JsonMap::new();
    schema.insert("type".to_string(), JsonValue::String("object".to_string()));
    schema.insert("properties".to_string(), JsonValue::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_string(), JsonValue::Array(required));
    }
    JsonValue::Object(schema)
}

/// Build one simple property schema fragment from one legacy flat parameter descriptor.
/// 基于单个旧版扁平参数描述构造简单属性 schema 片段。
fn build_parameter_schema_fragment(parameter: &SkillParam) -> JsonValue {
    let mut schema = JsonMap::new();
    schema.insert(
        "type".to_string(),
        JsonValue::String(parameter.param_type.clone()),
    );
    if !parameter.description.trim().is_empty() {
        schema.insert(
            "description".to_string(),
            JsonValue::String(parameter.description.clone()),
        );
    }
    JsonValue::Object(schema)
}

/// Derive one readable legacy parameter type string from one JSON Schema property node.
/// 从单个 JSON Schema 属性节点推导可读的旧版参数类型字符串。
fn derive_parameter_type_from_schema(schema: &JsonValue) -> String {
    let Some(object) = schema.as_object() else {
        return "schema".to_string();
    };
    if let Some(type_name) = object.get("type").and_then(JsonValue::as_str) {
        return type_name.to_string();
    }
    if let Some(type_items) = object.get("type").and_then(JsonValue::as_array) {
        let names: Vec<String> = type_items
            .iter()
            .filter_map(JsonValue::as_str)
            .map(ToString::to_string)
            .collect();
        if !names.is_empty() {
            return names.join(" | ");
        }
    }
    if object.contains_key("properties") {
        return "object".to_string();
    }
    if object.contains_key("items") || object.contains_key("prefixItems") {
        return "array".to_string();
    }
    if object.contains_key("oneOf") || object.contains_key("anyOf") || object.contains_key("allOf")
    {
        return "union".to_string();
    }
    "schema".to_string()
}

/// Derive legacy flat parameter descriptors from the top-level object properties of one schema.
/// 从单个 schema 的顶层对象属性中推导旧版扁平参数描述。
fn derive_legacy_parameters_from_input_schema(schema: &JsonValue) -> Vec<SkillParam> {
    let Some(object) = schema.as_object() else {
        return Vec::new();
    };
    let Some(properties) = object.get("properties").and_then(JsonValue::as_object) else {
        return Vec::new();
    };
    let required_names = object
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToString::to_string)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    properties
        .iter()
        .map(|(name, property_schema)| SkillParam {
            name: name.clone(),
            param_type: derive_parameter_type_from_schema(property_schema),
            description: property_schema
                .as_object()
                .and_then(|item| item.get("description"))
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
            required: required_names
                .iter()
                .any(|required_name| required_name == name),
        })
        .collect()
}

/// Load one external JSON Schema file referenced by one skill entry.
/// 加载单个 skill 入口引用的外部 JSON Schema 文件。
fn load_entry_input_schema_file(
    skill_dir: &Path,
    relative_path: &str,
    entry_name: &str,
) -> Result<JsonValue, String> {
    validate_skill_relative_path(relative_path, "schemas", "entry.input_schema_file")?;
    let schema_path = skill_dir.join(relative_path);
    let schema_text = fs::read_to_string(&schema_path).map_err(|error| {
        format!(
            "skill entry {} input_schema_file {} read failed: {}",
            entry_name,
            render_host_visible_path(&schema_path),
            error
        )
    })?;
    serde_json::from_str(&schema_text).map_err(|error| {
        format!(
            "skill entry {} input_schema_file {} parse failed: {}",
            entry_name,
            render_host_visible_path(&schema_path),
            error
        )
    })
}

impl SkillMeta {
    /// Return the effective LanceDB configuration.
    /// 返回生效的 LanceDB 配置。
    pub fn effective_lancedb(&self) -> SkillLanceDbMeta {
        self.lancedb.clone()
    }

    /// Return the effective SQLite configuration.
    /// 返回生效的 SQLite 配置。
    pub fn effective_sqlite(&self) -> SkillSqliteMeta {
        self.sqlite.clone()
    }

    /// Bind the stable runtime skill identifier derived from the physical directory name.
    /// 绑定从物理目录名称派生出的稳定运行时技能标识符。
    pub fn bind_directory_skill_id(&mut self, skill_id: String) {
        self.resolved_skill_id = skill_id;
    }

    /// Resolve every entry input schema into one final object schema before runtime export.
    /// 在运行时导出之前，把每个入口输入 schema 解析为最终对象 schema。
    pub fn resolve_entry_input_schemas(&mut self, skill_dir: &Path) -> Result<(), String> {
        let skill_id = self.effective_skill_id().to_string();
        validate_skill_package_config_declarations(&skill_id, &mut self.config)?;
        for tool in &mut self.entries {
            tool.resolve_input_schema(skill_dir)?;
        }
        Ok(())
    }

    /// Return the effective skill id used by canonical entry names.
    /// 返回用于 canonical 入口名的生效 skill id。
    pub fn effective_skill_id(&self) -> &str {
        self.resolved_skill_id.trim()
    }

    /// Return the semantic package version declared by the current skill.
    /// 返回当前技能声明的语义化包版本。
    pub fn version(&self) -> &str {
        self.version.trim()
    }

    /// Return whether the manifest itself allows the skill to load.
    /// 返回当前清单本身是否允许技能被加载。
    pub fn is_enabled(&self) -> bool {
        self.enable
    }

    /// Iterate over all top-level entries.
    /// 遍历当前 skill 下的全部顶层入口。
    pub fn entries(&self) -> impl Iterator<Item = &SkillToolMeta> {
        self.entries.iter()
    }

    /// Iterate over all package-level configuration declarations.
    /// 遍历当前技能包的全部包级配置声明。
    pub fn package_config(&self) -> impl Iterator<Item = &SkillPackageConfigDeclaration> {
        self.config.iter()
    }

    /// Find one package-level configuration declaration by its stable key.
    /// 根据稳定 key 查找单个包级配置声明。
    pub fn find_package_config(&self, key: &str) -> Option<&SkillPackageConfigDeclaration> {
        self.package_config()
            .find(|declaration| declaration.key == key)
    }

    /// Build the unresolved base name of one entry before conflict indexing.
    /// 构建单个入口在冲突编号前的未解析基础名称。
    pub fn tool_base_name(&self, tool: &SkillToolMeta) -> String {
        format!("{}-{}", self.effective_skill_id(), tool.name.trim())
    }

    /// Find one entry by its strict local name.
    /// 根据严格局部入口名查找单个入口。
    pub fn find_tool_by_local_name(&self, tool_name: &str) -> Option<&SkillToolMeta> {
        self.entries().find(|tool| tool.name.trim() == tool_name)
    }

    /// Return the main help node declared by the skill.
    /// 返回 skill 声明的主帮助节点。
    pub fn main_help(&self) -> &SkillHelpNodeMeta {
        &self.help.main
    }

    /// Iterate over all topic or workflow help nodes declared by the skill.
    /// 遍历当前 skill 声明的全部 topic 或 workflow 帮助节点。
    pub fn help_topics(&self) -> impl Iterator<Item = &SkillHelpNodeMeta> {
        self.help.topics.iter()
    }

    /// Find one help topic or workflow node by its declared name.
    /// 根据声明名称查找单个 help topic 或 workflow 节点。
    pub fn find_help_topic(&self, topic_name: &str) -> Option<&SkillHelpNodeMeta> {
        self.help_topics()
            .find(|topic| topic.name.trim() == topic_name)
    }

    /// Return all entries that reference one help topic/workflow name.
    /// 返回引用某个 help topic/workflow 名称的全部入口。
    pub fn entries_for_help_topic<'a>(
        &'a self,
        topic_name: &'a str,
    ) -> impl Iterator<Item = &'a SkillToolMeta> + 'a {
        self.entries()
            .filter(move |tool| tool.help.trim() == topic_name)
    }
}

impl SkillToolMeta {
    /// Resolve the final AI-facing input schema for one entry from inline schema, file, or legacy parameters.
    /// 基于内联 schema、外部文件或旧版参数，为单个入口解析最终面向 AI 的输入 schema。
    pub fn resolve_input_schema(&mut self, skill_dir: &Path) -> Result<(), String> {
        let entry_name = self.name.trim().to_string();
        let has_inline_input_schema = self.input_schema.is_some();
        let schema_file = self.input_schema_file.trim().to_string();
        if has_inline_input_schema && !schema_file.is_empty() {
            return Err(format!(
                "skill entry {} must not declare both input_schema and input_schema_file",
                entry_name
            ));
        }

        let resolved_input_schema = if !schema_file.is_empty() {
            load_entry_input_schema_file(skill_dir, &schema_file, &entry_name)?
        } else if let Some(schema) = self.input_schema.clone() {
            schema
        } else {
            build_entry_input_schema_from_parameters(&self.parameters)
        };

        validate_entry_input_schema_root(
            &resolved_input_schema,
            &format!("skill entry {} input_schema", entry_name),
        )?;

        if self.parameters.is_empty() {
            self.parameters = derive_legacy_parameters_from_input_schema(&resolved_input_schema);
        }
        self.input_schema = Some(resolved_input_schema);
        Ok(())
    }

    /// Return the resolved AI-facing input schema for one entry.
    /// 返回单个入口已解析完成、面向 AI 的输入 schema。
    ///
    /// Returns the schema when `resolve_input_schema` has already populated it, otherwise an explicit lifecycle error.
    /// 当 `resolve_input_schema` 已填充 schema 时返回该 schema，否则返回显式生命周期错误。
    pub fn resolved_input_schema(&self) -> Result<&JsonValue, String> {
        self.input_schema.as_ref().ok_or_else(|| {
            format!(
                "skill entry {} input_schema has not been resolved",
                self.name.trim()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SkillMeta, SkillParam, build_entry_input_schema_from_parameters,
        derive_legacy_parameters_from_input_schema, is_valid_luaskills_identifier,
        validate_luaskills_identifier, validate_luaskills_version,
    };
    use crate::runtime::path::render_host_visible_path;
    use serde_json::json;
    use std::fs;

    /// Build one unique temporary directory path for manifest tests.
    /// 为 manifest 测试构造单个唯一临时目录路径。
    fn make_manifest_test_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("luaskills_manifest_{label}_{nonce}"))
    }

    /// Verify that legal lowercase-hyphen identifiers are accepted.
    /// 验证合法的小写短横线标识符会被接受。
    #[test]
    fn valid_luaskills_identifiers_are_accepted() {
        assert!(is_valid_luaskills_identifier("vulcan-codekit"));
        assert!(is_valid_luaskills_identifier("codekit2"));
        assert!(is_valid_luaskills_identifier("vulcan-runtime-tools"));
    }

    /// Verify that invalid identifiers are rejected by both helper functions.
    /// 验证非法标识符会被两个辅助函数共同拒绝。
    #[test]
    fn invalid_luaskills_identifiers_are_rejected() {
        for candidate in [
            "",
            "2codekit",
            "Vulcan-codekit",
            "vulcan_codekit",
            "vulcan-codekit-",
            "__demo",
        ] {
            assert!(!is_valid_luaskills_identifier(candidate));
            assert!(validate_luaskills_identifier(candidate, "skill_id").is_err());
        }
    }

    /// Verify that semantic package versions are accepted only when they parse cleanly.
    /// 验证语义化包版本仅在可被正确解析时才会被接受。
    #[test]
    fn semantic_skill_versions_are_validated() {
        assert!(validate_luaskills_version("0.1.0", "version").is_ok());
        assert!(validate_luaskills_version("1.2.3-beta.1", "version").is_ok());
        assert!(validate_luaskills_version("", "version").is_err());
        assert!(validate_luaskills_version("v1.0.0", "version").is_err());
        assert!(validate_luaskills_version("1", "version").is_err());
    }

    /// Verify legacy flat parameters project into one object-style input schema.
    /// 验证旧版扁平参数会被投影为对象式输入 schema。
    #[test]
    fn legacy_parameters_project_to_input_schema() {
        let schema = build_entry_input_schema_from_parameters(&[
            SkillParam {
                name: "path".to_string(),
                param_type: "string".to_string(),
                description: "Absolute file path.".to_string(),
                required: true,
            },
            SkillParam {
                name: "recursive".to_string(),
                param_type: "boolean".to_string(),
                description: "Whether to recurse.".to_string(),
                required: false,
            },
        ]);

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["recursive"]["type"], "boolean");
        assert_eq!(schema["required"], json!(["path"]));
    }

    /// Verify top-level schema properties can be projected back into legacy flat parameters.
    /// 验证顶层 schema 属性能够反向投影回旧版扁平参数。
    #[test]
    fn input_schema_projects_back_to_legacy_parameters() {
        let parameters = derive_legacy_parameters_from_input_schema(&json!({
            "type": "object",
            "properties": {
                "nodes": {
                    "type": "array",
                    "description": "Node selector list."
                },
                "strict": {
                    "type": "boolean",
                    "description": "Enable strict validation."
                }
            },
            "required": ["nodes"]
        }));

        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].name, "nodes");
        assert_eq!(parameters[0].param_type, "array");
        assert!(parameters[0].required);
        assert_eq!(parameters[1].name, "strict");
        assert_eq!(parameters[1].param_type, "boolean");
        assert!(!parameters[1].required);
    }

    /// Verify one external JSON Schema file becomes the resolved entry input schema.
    /// 验证单个外部 JSON Schema 文件会成为已解析的入口输入 schema。
    #[test]
    fn skill_meta_resolves_external_entry_input_schema_file() {
        let skill_dir = make_manifest_test_dir("schema_file");
        fs::create_dir_all(skill_dir.join("schemas")).expect("create schemas dir");
        fs::write(
            skill_dir.join("schemas").join("search.input.schema.json"),
            serde_json::to_string_pretty(&json!({
                "type": "object",
                "properties": {
                    "nodes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "file": { "type": "string" },
                                "structural_path": { "type": "string" }
                            },
                            "required": ["file", "structural_path"]
                        }
                    }
                },
                "required": ["nodes"]
            }))
            .expect("serialize schema file"),
        )
        .expect("write schema file");

        let mut meta: SkillMeta = serde_yaml::from_str(
            r#"
name: demo-skill
version: 0.1.0
enable: true
entries:
  - name: search
    description: Demo search.
    lua_entry: runtime/search.lua
    lua_module: demo_search
    input_schema_file: schemas/search.input.schema.json
"#,
        )
        .expect("parse manifest");
        meta.bind_directory_skill_id("demo-skill".to_string());
        meta.resolve_entry_input_schemas(&skill_dir)
            .expect("resolve entry input schemas");

        let tool = meta
            .find_tool_by_local_name("search")
            .expect("search entry");
        // Resolved schema returned by the accessor after the manifest resolution lifecycle has run.
        // manifest 解析生命周期执行后由访问器返回的已解析 schema。
        let resolved_input_schema = tool
            .resolved_input_schema()
            .expect("entry schema should resolve");
        assert_eq!(resolved_input_schema["type"], "object");
        assert_eq!(resolved_input_schema["required"], json!(["nodes"]));
        assert_eq!(tool.parameters.len(), 1);
        assert_eq!(tool.parameters[0].name, "nodes");
        assert_eq!(tool.parameters[0].param_type, "array");

        fs::remove_dir_all(&skill_dir).expect("cleanup schema file test dir");
    }

    /// Verify unresolved entry input schema access returns an explicit lifecycle error instead of panicking.
    /// 验证未解析的入口输入 schema 访问会返回显式生命周期错误，而不是 panic。
    #[test]
    fn skill_tool_meta_unresolved_input_schema_returns_error() {
        // Skill metadata intentionally left before `resolve_entry_input_schemas` to exercise the lifecycle guard.
        // 有意让技能元数据停留在 `resolve_entry_input_schemas` 之前，以覆盖生命周期保护逻辑。
        let meta: SkillMeta = serde_yaml::from_str(
            r#"
name: demo-skill
version: 0.1.0
enable: true
entries:
  - name: search
    description: Demo search.
    lua_entry: runtime/search.lua
    lua_module: demo_search
"#,
        )
        .expect("parse manifest");
        // Entry metadata whose schema has not passed through manifest resolution yet.
        // 尚未经过 manifest 解析流程的入口元数据。
        let tool = meta
            .find_tool_by_local_name("search")
            .expect("search entry");
        // Explicit lifecycle error returned by the accessor.
        // 访问器返回的显式生命周期错误。
        let error = tool
            .resolved_input_schema()
            .expect_err("unresolved schema should return an error");

        assert_eq!(
            error,
            "skill entry search input_schema has not been resolved"
        );
    }

    /// Verify external schema parse errors render paths through the host-visible formatter.
    /// 验证外部 schema 解析错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn skill_meta_schema_parse_error_uses_host_visible_path() {
        // Skill directory that isolates the invalid schema fixture.
        // 隔离非法 schema 夹具的 skill 目录。
        let skill_dir = make_manifest_test_dir("schema_parse_error_path");
        fs::create_dir_all(skill_dir.join("schemas")).expect("create schemas dir");
        // Schema path referenced by the entry input_schema_file field.
        // entry input_schema_file 字段引用的 schema 路径。
        let schema_path = skill_dir.join("schemas/broken.schema.json");
        fs::write(&schema_path, "{not-json").expect("invalid schema file should be written");
        // Skill metadata that resolves the external schema through the real manifest path.
        // 通过真实 manifest 路径解析外部 schema 的技能元数据。
        let mut meta: SkillMeta = serde_yaml::from_str(
            r#"
name: demo-skill
version: 0.1.0
entries:
  - name: search
    lua_entry: runtime/search.lua
    lua_module: demo_search
    input_schema_file: schemas/broken.schema.json
"#,
        )
        .expect("parse manifest");
        meta.bind_directory_skill_id("demo-skill".to_string());
        // Error returned by the real external-schema resolver.
        // 真实外部 schema 解析器返回的错误。
        let error = meta
            .resolve_entry_input_schemas(&skill_dir)
            .expect_err("invalid schema JSON should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "skill entry search input_schema_file {} parse failed:",
            render_host_visible_path(&schema_path)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(skill_dir);
    }

    /// Verify invalid root input schema types are rejected during resolution.
    /// 验证非法根输入 schema 类型会在解析阶段被拒绝。
    #[test]
    fn skill_meta_rejects_non_object_entry_input_schema() {
        let skill_dir = make_manifest_test_dir("invalid_schema");
        fs::create_dir_all(&skill_dir).expect("create invalid schema dir");

        let mut meta: SkillMeta = serde_yaml::from_str(
            r#"
name: demo-skill
version: 0.1.0
enable: true
entries:
  - name: search
    description: Demo search.
    lua_entry: runtime/search.lua
    lua_module: demo_search
    input_schema:
      type: array
"#,
        )
        .expect("parse invalid manifest");
        meta.bind_directory_skill_id("demo-skill".to_string());
        let error = meta
            .resolve_entry_input_schemas(&skill_dir)
            .expect_err("non-object root schema should fail");
        assert!(error.contains("input_schema.type must be \"object\""));

        fs::remove_dir_all(&skill_dir).expect("cleanup invalid schema dir");
    }

    /// Verify non-string schema descriptions are rejected before legacy parameter projection.
    /// 验证非字符串 schema 描述会在旧版参数投影前被拒绝。
    #[test]
    fn skill_meta_rejects_non_string_input_schema_description() {
        // Skill directory used only to satisfy the manifest resolver interface.
        // 仅用于满足 manifest 解析器接口的技能目录。
        let skill_dir = make_manifest_test_dir("invalid_schema_description");
        fs::create_dir_all(&skill_dir).expect("create invalid schema description dir");
        // Manifest with a property description that cannot be projected to a legacy text description.
        // 包含无法投影为旧版文本描述的属性 description 的清单。
        let mut meta: SkillMeta = serde_yaml::from_str(
            r#"
name: demo-skill
version: 0.1.0
enable: true
entries:
  - name: search
    lua_entry: runtime/search.lua
    lua_module: demo_search
    input_schema:
      type: object
      properties:
        query:
          type: string
          description: 42
"#,
        )
        .expect("parse invalid description manifest");
        meta.bind_directory_skill_id("demo-skill".to_string());
        // Error produced by the schema validator before any legacy parameter fallback is derived.
        // 在派生任何旧版参数兜底前由 schema 校验器产生的错误。
        let error = meta
            .resolve_entry_input_schemas(&skill_dir)
            .expect_err("non-string description should fail");

        assert_eq!(
            error,
            "skill entry search input_schema.properties.query.description must be one string"
        );

        fs::remove_dir_all(&skill_dir).expect("cleanup invalid schema description dir");
    }
}

/// Verify package configuration declarations are accepted only at the manifest top level.
/// 验证技能包配置声明只允许出现在清单顶层。
#[test]
fn package_config_is_rejected_inside_individual_entries() {
    let error = serde_yaml::from_str::<SkillMeta>(
        r#"
name: demo-package
version: 1.0.0
entries:
  - name: ping
    description: Ping.
    lua_entry: runtime/ping.lua
    lua_module: demo-package.ping
    config:
      - key: token
        type: string
        description: Token.
"#,
    )
    .expect_err("entry-local configuration declarations must fail");
    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("config"));
}

/// Verify unknown top-level manifest fields fail instead of being silently ignored.
/// 验证未知清单顶层字段会失败而不是被静默忽略。
#[test]
fn skill_meta_rejects_unknown_top_level_fields() {
    let error = serde_yaml::from_str::<SkillMeta>(
        r#"
name: demo-package
version: 1.0.0
configuration:
  - key: token
    type: string
    description: Token.
entries: []
"#,
    )
    .expect_err("unknown top-level configuration aliases must fail");
    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("configuration"));
}
