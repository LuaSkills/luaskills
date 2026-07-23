use serde::{Deserialize, Serialize};
use serde_json::{Number as JsonNumber, Value as JsonValue};
use std::collections::BTreeSet;

/// Lowest integer accepted by every supported LuaSkills language boundary.
/// 所有 LuaSkills 受支持语言边界都能接受的最小整数。
pub const SKILL_CONFIG_MIN_SAFE_INTEGER: i64 = -9_007_199_254_740_991;

/// Highest integer accepted by every supported LuaSkills language boundary.
/// 所有 LuaSkills 受支持语言边界都能接受的最大整数。
pub const SKILL_CONFIG_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Maximum number of declared configuration items in one skill package.
/// 单个技能包允许声明的最大配置项数量。
pub const SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE: usize = 1_024;

/// Maximum UTF-8 byte length of one persisted configuration value.
/// 单个持久化配置值允许的最大 UTF-8 字节数。
pub const SKILL_CONFIG_MAX_VALUE_BYTES: usize = 1_048_576;

/// Maximum Unicode scalar count accepted by one declared string length limit.
/// 单个字符串声明长度限制允许的最大 Unicode 标量数量。
pub const SKILL_CONFIG_MAX_STRING_CHARS: usize = 1_048_576;

/// Maximum number of enumeration options declared by one configuration item.
/// 单个配置项允许声明的最大枚举选项数量。
pub const SKILL_CONFIG_MAX_ENUM_OPTIONS: usize = 1_024;

/// Maximum UTF-8 byte length of one long human-readable declaration text.
/// 单个较长人类可读声明文本允许的最大 UTF-8 字节数。
pub const SKILL_CONFIG_MAX_LONG_TEXT_BYTES: usize = 16_384;

/// Maximum UTF-8 byte length of one short human-readable declaration text.
/// 单个较短人类可读声明文本允许的最大 UTF-8 字节数。
pub const SKILL_CONFIG_MAX_SHORT_TEXT_BYTES: usize = 1_024;

/// Maximum UTF-8 byte length of one configuration group identifier.
/// 单个配置分组标识允许的最大 UTF-8 字节数。
pub const SKILL_CONFIG_MAX_GROUP_BYTES: usize = 256;

/// Maximum UTF-8 byte length of one placeholder or textual example.
/// 单个占位文本或文本示例允许的最大 UTF-8 字节数。
pub const SKILL_CONFIG_MAX_HINT_BYTES: usize = 8_192;

/// Maximum UTF-8 byte length of one non-sensitive value preview in diagnostics.
/// 诊断中单个非敏感值预览允许的最大 UTF-8 字节数。
pub const SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES: usize = 4_096;

/// Reserved configuration key prefix owned exclusively by LuaSkills.
/// 仅供 LuaSkills 自身使用的保留配置键前缀。
pub const SKILL_CONFIG_RESERVED_KEY_PREFIX: &str = "luaskills.";

/// Stable package-level configuration value type declared by one skill package.
/// 单个技能包声明的稳定包级配置值类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPackageConfigType {
    /// Signed 64-bit integer value.
    /// 有符号 64 位整数值。
    Integer,
    /// UTF-8 string value.
    /// UTF-8 字符串值。
    String,
    /// Finite 64-bit floating-point value.
    /// 有限 64 位浮点值。
    Float,
    /// Stable string enumeration value.
    /// 稳定字符串枚举值。
    Enum,
    /// Strict boolean value.
    /// 严格布尔值。
    Boolean,
}

/// Host-facing rendering hint for one package configuration item.
/// 单个技能包配置项面向宿主的渲染提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPackageConfigFormat {
    /// One-line plain text input.
    /// 单行纯文本输入。
    Text,
    /// Secret-style input whose disclosure policy remains host-owned.
    /// 披露策略仍由宿主负责的秘密样式输入。
    Password,
    /// URI input hint.
    /// URI 输入提示。
    Uri,
    /// Generic filesystem path input hint.
    /// 通用文件系统路径输入提示。
    Path,
    /// File path input hint.
    /// 文件路径输入提示。
    File,
    /// Directory path input hint.
    /// 目录路径输入提示。
    Directory,
    /// Multi-line text input hint.
    /// 多行文本输入提示。
    Multiline,
}

/// Structured runtime value-validation failure used by the package configuration service.
/// 技能包配置服务使用的结构化运行时值校验失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillPackageConfigValueError {
    /// Stable machine-readable issue code.
    /// 稳定机器可读问题代码。
    pub(crate) code: &'static str,
    /// Human-readable validation explanation.
    /// 人类可读校验说明。
    pub(crate) message: String,
}

impl SkillPackageConfigType {
    /// Return the stable manifest and wire name of this configuration type.
    /// 返回当前配置类型的稳定清单与线协议名称。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::String => "string",
            Self::Float => "float",
            Self::Enum => "enum",
            Self::Boolean => "boolean",
        }
    }
}

/// Type-specific constraints declared for one package-level configuration item.
/// 单个包级配置项声明的类型专属约束。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkillPackageConfigConstraints {
    /// Inclusive numeric minimum used by integer and float values.
    /// 整数与浮点值使用的包含式数值下界。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<JsonNumber>,
    /// Inclusive numeric maximum used by integer and float values.
    /// 整数与浮点值使用的包含式数值上界。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<JsonNumber>,
    /// Minimum Unicode scalar-value count used by strings.
    /// 字符串使用的最小 Unicode 标量值数量。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// Maximum Unicode scalar-value count used by strings.
    /// 字符串使用的最大 Unicode 标量值数量。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
}

impl SkillPackageConfigConstraints {
    /// Return whether no constraint field is declared.
    /// 返回是否未声明任何约束字段。
    pub fn is_empty(&self) -> bool {
        self.minimum.is_none()
            && self.maximum.is_none()
            && self.min_length.is_none()
            && self.max_length.is_none()
    }
}

/// One stable enumeration option declared by a package-level configuration item.
/// 包级配置项声明的单个稳定枚举选项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackageConfigEnumOption {
    /// Stable machine value persisted in the package configuration store.
    /// 持久化到技能包配置存储中的稳定机器值。
    pub value: String,
    /// Human-readable display label written in the package author's chosen language.
    /// 使用技能包作者所选语言编写的人类可读显示名称。
    pub label: String,
    /// Human-readable explanation written in the package author's chosen language.
    /// 使用技能包作者所选语言编写的人类可读说明。
    pub description: String,
}

/// One package-level configuration declaration loaded from top-level `skill.yaml`.
/// 从顶层 `skill.yaml` 加载的单个包级配置声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackageConfigDeclaration {
    /// Stable configuration key within the owning package namespace.
    /// 所属技能包命名空间内的稳定配置键。
    pub key: String,
    /// Declared package configuration value type.
    /// 声明的技能包配置值类型。
    #[serde(rename = "type")]
    pub value_type: SkillPackageConfigType,
    /// Whether one explicit or default value is required for a complete package configuration.
    /// 完整技能包配置是否需要显式值或默认值。
    #[serde(default)]
    pub required: bool,
    /// Optional typed default value retained in the manifest and never persisted automatically.
    /// 保留在清单中且永不自动持久化的可选类型化默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<JsonValue>,
    /// Sensitive-value hint consumed exclusively by host-side policy.
    /// 仅供宿主侧策略使用的敏感值提示。
    #[serde(default)]
    pub sensitive: bool,
    /// Human-readable description written in the package author's chosen language.
    /// 使用技能包作者所选语言编写的人类可读说明。
    pub description: String,
    /// Type-specific value constraints.
    /// 类型专属值约束。
    #[serde(default)]
    pub constraints: SkillPackageConfigConstraints,
    /// Stable enumeration options required only by the enum type.
    /// 仅枚举类型需要的稳定枚举选项。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SkillPackageConfigEnumOption>,
    /// Optional short title used by host configuration interfaces.
    /// 宿主配置界面使用的可选短标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional host-facing grouping hint.
    /// 可选的宿主侧分组提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Optional stable display order within the host-selected group.
    /// 宿主所选分组内的可选稳定显示顺序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    /// Whether hosts should present the item as an advanced option.
    /// 宿主是否应把该项展示为高级选项。
    #[serde(default)]
    pub advanced: bool,
    /// Optional input placeholder shown by hosts.
    /// 宿主显示的可选输入占位文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Optional typed example value validated with the declaration.
    /// 与声明共同校验的可选类型化示例值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<JsonValue>,
    /// Optional rendering format hint that never replaces validation.
    /// 永不替代校验的可选渲染格式提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<SkillPackageConfigFormat>,
    /// Whether changing the item may require host-managed restart work.
    /// 修改该项是否可能需要宿主管理的重启操作。
    #[serde(default)]
    pub restart_required: bool,
    /// Whether the item is deprecated for new configurations.
    /// 该配置项是否已不建议用于新配置。
    #[serde(default)]
    pub deprecated: bool,
    /// Required human-readable replacement guidance for deprecated items.
    /// 已弃用配置项必需的人类可读替代说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecation_message: Option<String>,
}

impl SkillPackageConfigDeclaration {
    /// Validate and normalize one raw string value according to this declaration.
    /// 根据当前声明校验并规范化一个原始字符串值。
    pub fn normalize_value(&self, raw_value: &str) -> Result<String, String> {
        self.normalize_value_detailed(raw_value)
            .map_err(|error| error.message)
    }

    /// Validate and normalize one raw value while retaining a stable issue code.
    /// 校验并规范化一个原始值，同时保留稳定问题代码。
    pub(crate) fn normalize_value_detailed(
        &self,
        raw_value: &str,
    ) -> Result<String, SkillPackageConfigValueError> {
        match self.value_type {
            SkillPackageConfigType::Integer => self.normalize_integer(raw_value),
            SkillPackageConfigType::String => self.normalize_string(raw_value),
            SkillPackageConfigType::Float => self.normalize_float(raw_value),
            SkillPackageConfigType::Enum => self.normalize_enum(raw_value),
            SkillPackageConfigType::Boolean => self.normalize_boolean(raw_value),
        }
    }

    /// Resolve and normalize the optional typed default value.
    /// 解析并规范化可选的类型化默认值。
    pub fn normalized_default_value(&self) -> Result<Option<String>, String> {
        self.default
            .as_ref()
            .map(|value| self.normalize_typed_manifest_value(value, "default"))
            .transpose()
    }

    /// Resolve and normalize the optional typed example value.
    /// 解析并规范化可选的类型化示例值。
    pub fn normalized_example_value(&self) -> Result<Option<String>, String> {
        self.example
            .as_ref()
            .map(|value| self.normalize_typed_manifest_value(value, "example"))
            .transpose()
    }

    /// Normalize one typed manifest scalar through the runtime value rules.
    /// 通过运行时值规则规范化一个类型化清单标量。
    fn normalize_typed_manifest_value(
        &self,
        value: &JsonValue,
        field_name: &str,
    ) -> Result<String, String> {
        let raw_value = match (self.value_type, value) {
            (SkillPackageConfigType::Integer, JsonValue::Number(number))
                if number.as_i64().is_some() =>
            {
                number.to_string()
            }
            (SkillPackageConfigType::Float, JsonValue::Number(number)) => number.to_string(),
            (
                SkillPackageConfigType::String | SkillPackageConfigType::Enum,
                JsonValue::String(text),
            ) => text.clone(),
            (SkillPackageConfigType::Boolean, JsonValue::Bool(flag)) => flag.to_string(),
            _ => {
                return Err(format!(
                    "configuration '{}' {} must use the declared {} type",
                    self.key,
                    field_name,
                    self.value_type.as_str()
                ));
            }
        };
        self.normalize_value(&raw_value)
    }

    /// Normalize and validate one cross-language safe integer value.
    /// 规范化并校验一个跨语言安全整数值。
    fn normalize_integer(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        let value = raw_value.parse::<i64>().map_err(|error| {
            value_error(
                "invalid_integer",
                format!(
                    "configuration '{}' requires one cross-language safe integer: {}",
                    self.key, error
                ),
            )
        })?;
        if !(SKILL_CONFIG_MIN_SAFE_INTEGER..=SKILL_CONFIG_MAX_SAFE_INTEGER).contains(&value) {
            return Err(value_error(
                "integer_out_of_range",
                format!(
                    "configuration '{}' value must be between {} and {}",
                    self.key, SKILL_CONFIG_MIN_SAFE_INTEGER, SKILL_CONFIG_MAX_SAFE_INTEGER
                ),
            ));
        }
        if let Some(minimum) = self
            .constraints
            .minimum
            .as_ref()
            .and_then(JsonNumber::as_i64)
            && value < minimum
        {
            return Err(value_error(
                "integer_out_of_range",
                format!(
                    "configuration '{}' value {} is below minimum {}",
                    self.key, value, minimum
                ),
            ));
        }
        if let Some(maximum) = self
            .constraints
            .maximum
            .as_ref()
            .and_then(JsonNumber::as_i64)
            && value > maximum
        {
            return Err(value_error(
                "integer_out_of_range",
                format!(
                    "configuration '{}' value {} exceeds maximum {}",
                    self.key, value, maximum
                ),
            ));
        }
        Ok(value.to_string())
    }

    /// Validate one UTF-8 string without altering its content.
    /// 在不改变内容的前提下校验一个 UTF-8 字符串。
    fn normalize_string(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        if raw_value.len() > SKILL_CONFIG_MAX_VALUE_BYTES {
            return Err(value_error(
                "string_too_long",
                format!(
                    "configuration '{}' UTF-8 byte length exceeds the hard limit {}",
                    self.key, SKILL_CONFIG_MAX_VALUE_BYTES
                ),
            ));
        }
        let length = raw_value.chars().count();
        if let Some(minimum) = self.constraints.min_length
            && length < minimum
        {
            return Err(value_error(
                "string_too_short",
                format!(
                    "configuration '{}' length {} is below minimum {}",
                    self.key, length, minimum
                ),
            ));
        }
        if let Some(maximum) = self.constraints.max_length
            && length > maximum
        {
            return Err(value_error(
                "string_too_long",
                format!(
                    "configuration '{}' length {} exceeds maximum {}",
                    self.key, length, maximum
                ),
            ));
        }
        Ok(raw_value.to_string())
    }

    /// Normalize and validate one finite 64-bit floating-point value.
    /// 规范化并校验一个有限 64 位浮点值。
    fn normalize_float(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        let value = raw_value.parse::<f64>().map_err(|error| {
            value_error(
                "invalid_float",
                format!(
                    "configuration '{}' requires one 64-bit floating-point value: {}",
                    self.key, error
                ),
            )
        })?;
        if !value.is_finite() {
            return Err(value_error(
                "float_not_finite",
                format!(
                    "configuration '{}' requires one finite floating-point value",
                    self.key
                ),
            ));
        }
        if let Some(minimum) = self
            .constraints
            .minimum
            .as_ref()
            .and_then(JsonNumber::as_f64)
            && value < minimum
        {
            return Err(value_error(
                "float_out_of_range",
                format!(
                    "configuration '{}' value {} is below minimum {}",
                    self.key, value, minimum
                ),
            ));
        }
        if let Some(maximum) = self
            .constraints
            .maximum
            .as_ref()
            .and_then(JsonNumber::as_f64)
            && value > maximum
        {
            return Err(value_error(
                "float_out_of_range",
                format!(
                    "configuration '{}' value {} exceeds maximum {}",
                    self.key, value, maximum
                ),
            ));
        }
        if value == 0.0 {
            Ok("0".to_string())
        } else {
            Ok(value.to_string())
        }
    }

    /// Validate one stable enumeration machine value.
    /// 校验一个稳定枚举机器值。
    fn normalize_enum(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        if self.options.iter().any(|option| option.value == raw_value) {
            return Ok(raw_value.to_string());
        }
        let allowed = self
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let value_preview = diagnostic_value_preview(raw_value);
        let allowed_preview = diagnostic_value_preview(&allowed);
        Err(value_error(
            "enum_value_not_allowed",
            format!(
                "configuration '{}' value '{}' is not allowed; expected one of: {}",
                self.key, value_preview, allowed_preview
            ),
        ))
    }

    /// Normalize one strict lowercase boolean value.
    /// 规范化一个严格小写布尔值。
    fn normalize_boolean(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        match raw_value {
            "true" => Ok("true".to_string()),
            "false" => Ok("false".to_string()),
            _ => Err(value_error(
                "invalid_boolean",
                format!(
                    "configuration '{}' requires exactly 'true' or 'false'",
                    self.key
                ),
            )),
        }
    }
}

/// Build one structured value-validation failure.
/// 构建一个结构化值校验失败。
fn value_error(code: &'static str, message: String) -> SkillPackageConfigValueError {
    SkillPackageConfigValueError { code, message }
}

/// Build one bounded non-sensitive diagnostic preview at a valid UTF-8 boundary.
/// 在合法 UTF-8 边界构建一个有界的非敏感诊断预览。
fn diagnostic_value_preview(value: &str) -> String {
    const TRUNCATED_MARKER: &str = "[truncated]";
    if value.len() <= SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES {
        return value.to_string();
    }
    let maximum_prefix_bytes =
        SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES - TRUNCATED_MARKER.len();
    let mut boundary = maximum_prefix_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut preview = String::with_capacity(SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES);
    preview.push_str(&value[..boundary]);
    preview.push_str(TRUNCATED_MARKER);
    preview
}

/// Validate all package-level configuration declarations in one manifest.
/// 校验单个清单内的全部包级配置声明。
pub fn validate_skill_package_config_declarations(
    skill_id: &str,
    declarations: &mut [SkillPackageConfigDeclaration],
) -> Result<(), String> {
    if declarations.len() > SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE {
        return Err(format!(
            "skill package '{}' declares {} configuration items, exceeding the hard limit {}",
            skill_id,
            declarations.len(),
            SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE
        ));
    }
    let mut keys = BTreeSet::new();
    for declaration in declarations {
        validate_declaration(skill_id, declaration)?;
        if !keys.insert(declaration.key.clone()) {
            return Err(format!(
                "skill package '{}' declares duplicate configuration key '{}'",
                skill_id, declaration.key
            ));
        }
    }
    Ok(())
}

/// Validate and normalize one package configuration declaration.
/// 校验并规范化单个技能包配置声明。
fn validate_declaration(
    skill_id: &str,
    declaration: &mut SkillPackageConfigDeclaration,
) -> Result<(), String> {
    if !is_valid_skill_config_key(&declaration.key) {
        return Err(format!(
            "skill package '{}' configuration key '{}' must match ^[a-z][a-z0-9_.-]{{0,127}}$, must not use empty segments or leading/trailing punctuation, and must not use the reserved '{}' prefix",
            skill_id, declaration.key, SKILL_CONFIG_RESERVED_KEY_PREFIX
        ));
    }
    validate_non_empty_text(
        &declaration.description,
        &format!("configuration '{}' description", declaration.key),
    )?;
    validate_text_limit(
        &declaration.description,
        &format!("configuration '{}' description", declaration.key),
        SKILL_CONFIG_MAX_LONG_TEXT_BYTES,
    )?;
    validate_optional_text_limit(
        declaration.title.as_deref(),
        &format!("configuration '{}' title", declaration.key),
        SKILL_CONFIG_MAX_SHORT_TEXT_BYTES,
    )?;
    validate_optional_non_empty_text(
        declaration.title.as_deref(),
        &format!("configuration '{}' title", declaration.key),
    )?;
    validate_optional_text_limit(
        declaration.group.as_deref(),
        &format!("configuration '{}' group", declaration.key),
        SKILL_CONFIG_MAX_GROUP_BYTES,
    )?;
    validate_optional_non_empty_text(
        declaration.group.as_deref(),
        &format!("configuration '{}' group", declaration.key),
    )?;
    validate_optional_text_limit(
        declaration.placeholder.as_deref(),
        &format!("configuration '{}' placeholder", declaration.key),
        SKILL_CONFIG_MAX_HINT_BYTES,
    )?;
    validate_optional_non_empty_text(
        declaration.placeholder.as_deref(),
        &format!("configuration '{}' placeholder", declaration.key),
    )?;
    validate_optional_text_limit(
        declaration.deprecation_message.as_deref(),
        &format!("configuration '{}' deprecation_message", declaration.key),
        SKILL_CONFIG_MAX_LONG_TEXT_BYTES,
    )?;
    if declaration.deprecated
        && declaration
            .deprecation_message
            .as_deref()
            .is_none_or(|message| message.trim().is_empty())
    {
        return Err(format!(
            "configuration '{}' deprecated=true requires a non-empty deprecation_message",
            declaration.key
        ));
    }
    if !declaration.deprecated && declaration.deprecation_message.is_some() {
        return Err(format!(
            "configuration '{}' must not declare deprecation_message unless deprecated=true",
            declaration.key
        ));
    }
    validate_constraints(declaration)?;

    if declaration.value_type == SkillPackageConfigType::Enum {
        validate_enum_options(declaration)?;
    } else if !declaration.options.is_empty() {
        return Err(format!(
            "configuration '{}' type {} must not declare options",
            declaration.key,
            declaration.value_type.as_str()
        ));
    }

    declaration.normalized_default_value()?;
    let normalized_example = declaration.normalized_example_value()?;
    if matches!(declaration.value_type, SkillPackageConfigType::String)
        && normalized_example
            .as_deref()
            .is_some_and(|value| value.len() > SKILL_CONFIG_MAX_HINT_BYTES)
    {
        return Err(format!(
            "configuration '{}' textual example exceeds the hard limit {} UTF-8 bytes",
            declaration.key, SKILL_CONFIG_MAX_HINT_BYTES
        ));
    }
    Ok(())
}

/// Validate the type-specific constraint combination of one declaration.
/// 校验单个声明的类型专属约束组合。
fn validate_constraints(declaration: &SkillPackageConfigDeclaration) -> Result<(), String> {
    match declaration.value_type {
        SkillPackageConfigType::Integer => {
            reject_length_constraints(declaration)?;
            let minimum = declaration
                .constraints
                .minimum
                .as_ref()
                .map(|number| {
                    number.as_i64().ok_or_else(|| {
                        format!(
                            "configuration '{}' integer minimum must be one cross-language safe integer",
                            declaration.key
                        )
                    })
                })
                .transpose()?;
            let maximum = declaration
                .constraints
                .maximum
                .as_ref()
                .map(|number| {
                    number.as_i64().ok_or_else(|| {
                        format!(
                            "configuration '{}' integer maximum must be one cross-language safe integer",
                            declaration.key
                        )
                    })
                })
                .transpose()?;
            for (field_name, value) in [("minimum", minimum), ("maximum", maximum)] {
                if let Some(value) = value
                    && !(SKILL_CONFIG_MIN_SAFE_INTEGER..=SKILL_CONFIG_MAX_SAFE_INTEGER)
                        .contains(&value)
                {
                    return Err(format!(
                        "configuration '{}' integer {} must be between {} and {}",
                        declaration.key,
                        field_name,
                        SKILL_CONFIG_MIN_SAFE_INTEGER,
                        SKILL_CONFIG_MAX_SAFE_INTEGER
                    ));
                }
            }
            if let (Some(minimum), Some(maximum)) = (minimum, maximum)
                && minimum > maximum
            {
                return Err(format!(
                    "configuration '{}' minimum {} exceeds maximum {}",
                    declaration.key, minimum, maximum
                ));
            }
        }
        SkillPackageConfigType::Float => {
            reject_length_constraints(declaration)?;
            let minimum =
                finite_constraint(declaration, "minimum", &declaration.constraints.minimum)?;
            let maximum =
                finite_constraint(declaration, "maximum", &declaration.constraints.maximum)?;
            if let (Some(minimum), Some(maximum)) = (minimum, maximum)
                && minimum > maximum
            {
                return Err(format!(
                    "configuration '{}' minimum {} exceeds maximum {}",
                    declaration.key, minimum, maximum
                ));
            }
        }
        SkillPackageConfigType::String => {
            if declaration.constraints.minimum.is_some()
                || declaration.constraints.maximum.is_some()
            {
                return Err(format!(
                    "configuration '{}' string type must not declare numeric minimum or maximum",
                    declaration.key
                ));
            }
            if let (Some(minimum), Some(maximum)) = (
                declaration.constraints.min_length,
                declaration.constraints.max_length,
            ) && minimum > maximum
            {
                return Err(format!(
                    "configuration '{}' min_length {} exceeds max_length {}",
                    declaration.key, minimum, maximum
                ));
            }
            for (field_name, value) in [
                ("min_length", declaration.constraints.min_length),
                ("max_length", declaration.constraints.max_length),
            ] {
                if value.is_some_and(|length| length > SKILL_CONFIG_MAX_STRING_CHARS) {
                    return Err(format!(
                        "configuration '{}' {} exceeds the hard limit {}",
                        declaration.key, field_name, SKILL_CONFIG_MAX_STRING_CHARS
                    ));
                }
            }
        }
        SkillPackageConfigType::Enum | SkillPackageConfigType::Boolean => {
            if !declaration.constraints.is_empty() {
                return Err(format!(
                    "configuration '{}' type {} must not declare constraints",
                    declaration.key,
                    declaration.value_type.as_str()
                ));
            }
        }
    }
    Ok(())
}

/// Validate and normalize every enum option of one declaration.
/// 校验并规范化单个声明的全部枚举选项。
fn validate_enum_options(declaration: &mut SkillPackageConfigDeclaration) -> Result<(), String> {
    if declaration.options.is_empty() {
        return Err(format!(
            "configuration '{}' enum type must declare at least one option",
            declaration.key
        ));
    }
    if declaration.options.len() > SKILL_CONFIG_MAX_ENUM_OPTIONS {
        return Err(format!(
            "configuration '{}' declares {} enum options, exceeding the hard limit {}",
            declaration.key,
            declaration.options.len(),
            SKILL_CONFIG_MAX_ENUM_OPTIONS
        ));
    }
    let mut values = BTreeSet::new();
    for option in &mut declaration.options {
        if option.value.is_empty() || option.value.trim() != option.value {
            return Err(format!(
                "configuration '{}' enum option value must be non-empty and contain no surrounding whitespace",
                declaration.key
            ));
        }
        validate_text_limit(
            &option.value,
            &format!("configuration '{}' enum option value", declaration.key),
            SKILL_CONFIG_MAX_SHORT_TEXT_BYTES,
        )?;
        if !values.insert(option.value.clone()) {
            return Err(format!(
                "configuration '{}' declares duplicate enum value '{}'",
                declaration.key, option.value
            ));
        }
        validate_non_empty_text(
            &option.label,
            &format!(
                "configuration '{}' enum option '{}' label",
                declaration.key, option.value
            ),
        )?;
        validate_text_limit(
            &option.label,
            &format!(
                "configuration '{}' enum option '{}' label",
                declaration.key, option.value
            ),
            SKILL_CONFIG_MAX_SHORT_TEXT_BYTES,
        )?;
        validate_non_empty_text(
            &option.description,
            &format!(
                "configuration '{}' enum option '{}' description",
                declaration.key, option.value
            ),
        )?;
        validate_text_limit(
            &option.description,
            &format!(
                "configuration '{}' enum option '{}' description",
                declaration.key, option.value
            ),
            SKILL_CONFIG_MAX_LONG_TEXT_BYTES,
        )?;
    }
    Ok(())
}

/// Reject string-length constraints on one numeric declaration.
/// 拒绝单个数值声明上的字符串长度约束。
fn reject_length_constraints(declaration: &SkillPackageConfigDeclaration) -> Result<(), String> {
    if declaration.constraints.min_length.is_some() || declaration.constraints.max_length.is_some()
    {
        return Err(format!(
            "configuration '{}' type {} must not declare string length constraints",
            declaration.key,
            declaration.value_type.as_str()
        ));
    }
    Ok(())
}

/// Parse one optional finite floating-point constraint.
/// 解析一个可选有限浮点约束。
fn finite_constraint(
    declaration: &SkillPackageConfigDeclaration,
    field_name: &str,
    value: &Option<JsonNumber>,
) -> Result<Option<f64>, String> {
    value
        .as_ref()
        .map(|number| {
            let parsed = number.as_f64().ok_or_else(|| {
                format!(
                    "configuration '{}' {} must be one floating-point number",
                    declaration.key, field_name
                )
            })?;
            if !parsed.is_finite() {
                return Err(format!(
                    "configuration '{}' {} must be finite",
                    declaration.key, field_name
                ));
            }
            Ok(parsed)
        })
        .transpose()
}

/// Validate one required human-readable text field.
/// 校验一个必填人类可读文本字段。
fn validate_non_empty_text(value: &str, field_label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{} must not be empty", field_label));
    }
    Ok(())
}

/// Validate one optional human-readable text when it is present.
/// 校验一个存在时必须非空的可选人类可读文本。
fn validate_optional_non_empty_text(value: Option<&str>, field_label: &str) -> Result<(), String> {
    if let Some(value) = value {
        validate_non_empty_text(value, field_label)?;
    }
    Ok(())
}

/// Return whether one package configuration key satisfies the stable cross-language contract.
/// 判断单个技能包配置键是否满足稳定的跨语言契约。
pub fn is_valid_skill_config_key(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 || value.starts_with(SKILL_CONFIG_RESERVED_KEY_PREFIX)
    {
        return false;
    }
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if !characters.clone().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '_' | '.' | '-')
    }) {
        return false;
    }
    if value.ends_with(['.', '_', '-']) || value.contains("..") {
        return false;
    }
    !value.split('.').any(|segment| {
        segment.is_empty() || segment.starts_with(['_', '-']) || segment.ends_with(['_', '-'])
    })
}

/// Validate one required human-readable text against a UTF-8 byte limit.
/// 按 UTF-8 字节上限校验一个必需的人类可读文本。
fn validate_text_limit(value: &str, field_label: &str, maximum: usize) -> Result<(), String> {
    if value.len() > maximum {
        return Err(format!(
            "{} exceeds the hard limit {} UTF-8 bytes",
            field_label, maximum
        ));
    }
    Ok(())
}

/// Validate one optional human-readable text against a UTF-8 byte limit.
/// 按 UTF-8 字节上限校验一个可选的人类可读文本。
fn validate_optional_text_limit(
    value: Option<&str>,
    field_label: &str,
    maximum: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_text_limit(value, field_label, maximum)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse and validate one declaration list used by configuration model tests.
    /// 解析并校验配置模型测试使用的单个声明列表。
    ///
    /// `yaml` is a YAML sequence of package-level declarations.
    /// `yaml` 是包级声明组成的 YAML 序列。
    ///
    /// Returns the validated declarations.
    /// 返回校验后的声明。
    fn parse_declarations(yaml: &str) -> Vec<SkillPackageConfigDeclaration> {
        let mut declarations = serde_yaml::from_str::<Vec<SkillPackageConfigDeclaration>>(yaml)
            .expect("declarations should parse");
        validate_skill_package_config_declarations("configuration-test-package", &mut declarations)
            .expect("declarations should validate");
        declarations
    }

    /// Verify every supported type validates and persists one canonical string representation.
    /// 验证每种受支持类型都会校验并持久化一种规范字符串表示。
    #[test]
    fn all_supported_types_normalize_to_canonical_strings() {
        let declarations = parse_declarations(
            r#"
- key: retries
  type: integer
  description: Retry count.
  constraints:
    minimum: 0
    maximum: 10
- key: title
  type: string
  description: Display title.
  constraints:
    min_length: 2
    max_length: 4
- key: ratio
  type: float
  description: Sampling ratio.
  constraints:
    minimum: 0.0
    maximum: 1.0
- key: provider
  type: enum
  description: Service provider.
  options:
    - value: openai
      label: OpenAI
      description: OpenAI service.
- key: enabled
  type: boolean
  description: Feature switch.
"#,
        );

        assert_eq!(declarations[0].normalize_value("003").unwrap(), "3");
        assert!(declarations[0].normalize_value(" 003 ").is_err());
        assert!(declarations[0].normalize_value("11").is_err());
        assert_eq!(declarations[1].normalize_value("中文").unwrap(), "中文");
        assert!(declarations[1].normalize_value("中文字符多").is_err());
        assert_eq!(declarations[2].normalize_value("0.500").unwrap(), "0.5");
        assert!(declarations[2].normalize_value(" 0.500 ").is_err());
        assert!(declarations[2].normalize_value("NaN").is_err());
        assert_eq!(declarations[3].normalize_value("openai").unwrap(), "openai");
        assert!(declarations[3].normalize_value("unknown").is_err());
        assert_eq!(declarations[4].normalize_value("true").unwrap(), "true");
        assert!(declarations[4].normalize_value(" true ").is_err());
        assert!(declarations[4].normalize_value("TRUE").is_err());
    }

    /// Verify enum diagnostics bound non-sensitive previews at a valid UTF-8 boundary.
    /// 验证枚举诊断会在合法 UTF-8 边界限制非敏感预览。
    #[test]
    fn enum_diagnostic_preview_is_utf8_safe_and_bounded() {
        // One valid enum declaration used to reject an oversized multibyte candidate.
        // 用于拒绝超大多字节候选值的合法枚举声明。
        let declaration = parse_declarations(
            r#"
- key: provider
  type: enum
  description: Service provider.
  options:
    - value: openai
      label: OpenAI
      description: OpenAI service.
"#,
        )
        .remove(0);
        // Candidate whose byte size greatly exceeds the diagnostic preview limit.
        // 字节大小远超诊断预览上限的候选值。
        let candidate = "中".repeat(SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES);
        let preview = diagnostic_value_preview(&candidate);

        assert!(preview.len() <= SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES);
        assert!(preview.ends_with("[truncated]"));
        let error = declaration
            .normalize_value(&candidate)
            .expect_err("undeclared enum value must fail");
        assert!(error.contains("[truncated]"));
        assert!(!error.contains(&candidate));
    }

    /// Verify declaration validation rejects unknown fields and invalid schema combinations.
    /// 验证声明校验会拒绝未知字段与非法结构组合。
    #[test]
    fn declaration_validation_rejects_unknown_and_invalid_shapes() {
        let unknown_field = serde_yaml::from_str::<SkillPackageConfigDeclaration>(
            r#"
key: retries
type: integer
description: Retry count.
unknown: true
"#,
        )
        .expect_err("unknown declaration fields must fail");
        assert!(unknown_field.to_string().contains("unknown field"));

        let mut duplicate_keys = serde_yaml::from_str::<Vec<SkillPackageConfigDeclaration>>(
            r#"
- key: retries
  type: integer
  description: First.
- key: retries
  type: integer
  description: Second.
"#,
        )
        .expect("duplicate declarations should parse before semantic validation");
        let error = validate_skill_package_config_declarations(
            "configuration-test-package",
            &mut duplicate_keys,
        )
        .expect_err("duplicate keys must fail");
        assert!(error.contains("duplicate configuration key"));

        let mut invalid_constraints = serde_yaml::from_str::<Vec<SkillPackageConfigDeclaration>>(
            r#"
- key: retries
  type: integer
  description: Retry count.
  constraints:
    min_length: 1
"#,
        )
        .expect("invalid constraint shape should parse before semantic validation");
        let error = validate_skill_package_config_declarations(
            "configuration-test-package",
            &mut invalid_constraints,
        )
        .expect_err("integer length constraints must fail");
        assert!(error.contains("must not declare string length constraints"));
    }

    /// Verify localization-only declaration fields are rejected as unknown schema.
    /// 验证仅用于本地化的声明字段会作为未知结构被拒绝。
    #[test]
    fn localization_fields_are_not_part_of_package_config_schema() {
        let error = serde_yaml::from_str::<SkillPackageConfigDeclaration>(
            r#"
key: provider
type: enum
description: Service provider
description_i18n:
  zh-CN: 服务提供商
options:
  - value: openai
    label: OpenAI
    description: OpenAI service
"#,
        )
        .expect_err("localization fields must not be accepted");
        assert!(error.to_string().contains("unknown field"));
    }

    /// Verify typed defaults use the same normalization and constraint path as stored values.
    /// 验证类型化默认值使用与持久化值相同的规范化与约束路径。
    #[test]
    fn typed_defaults_share_runtime_value_validation() {
        let declarations = parse_declarations(
            r#"
- key: retries
  type: integer
  default: 3
  description: Retry count.
  constraints:
    minimum: 0
    maximum: 10
- key: enabled
  type: boolean
  default: true
  description: Feature switch.
"#,
        );
        assert_eq!(
            declarations[0].normalized_default_value().unwrap(),
            Some("3".to_string())
        );
        assert_eq!(
            declarations[1].normalized_default_value().unwrap(),
            Some("true".to_string())
        );
    }

    /// Verify UI hints remain single-language metadata and typed examples share value validation.
    /// 验证 UI 提示保持单语言元数据，且类型化示例共用值校验。
    #[test]
    fn ui_metadata_and_typed_examples_are_strictly_validated() {
        let declarations = parse_declarations(
            r#"
- key: retry_count
  type: integer
  title: Retry count
  description: Request retry count.
  group: network
  order: 10
  advanced: true
  placeholder: Enter a retry count
  example: 4
  format: text
  restart_required: true
  deprecated: true
  deprecation_message: Use retry_policy instead.
  constraints:
    minimum: 0
    maximum: 10
"#,
        );
        let declaration = &declarations[0];
        assert_eq!(declaration.title.as_deref(), Some("Retry count"));
        assert_eq!(declaration.group.as_deref(), Some("network"));
        assert_eq!(declaration.order, Some(10));
        assert!(declaration.advanced);
        assert_eq!(
            declaration.normalized_example_value().unwrap(),
            Some("4".to_string())
        );
        assert_eq!(declaration.format, Some(SkillPackageConfigFormat::Text));
        assert!(declaration.restart_required);
        assert!(declaration.deprecated);

        let mut invalid = serde_yaml::from_str::<Vec<SkillPackageConfigDeclaration>>(
            r#"
- key: retry_count
  type: integer
  description: Request retry count.
  example: eleven
"#,
        )
        .expect("invalid typed example should parse before semantic validation");
        let error =
            validate_skill_package_config_declarations("configuration-test-package", &mut invalid)
                .expect_err("wrong typed example must fail");
        assert!(error.contains("example must use the declared integer type"));

        let mut empty_title = serde_yaml::from_str::<Vec<SkillPackageConfigDeclaration>>(
            r#"
- key: retry_count
  type: integer
  title: " "
  description: Request retry count.
"#,
        )
        .expect("empty title shape should parse before semantic validation");
        let error = validate_skill_package_config_declarations(
            "configuration-test-package",
            &mut empty_title,
        )
        .expect_err("empty optional UI text must fail");
        assert!(error.contains("title must not be empty"));
    }

    /// Verify configuration keys follow the strict package-local naming contract.
    /// 验证配置键遵循严格的包内命名契约。
    #[test]
    fn configuration_keys_follow_the_strict_cross_language_contract() {
        for valid in ["a", "retry_count", "network.retry-count", "v2.enabled"] {
            assert!(is_valid_skill_config_key(valid), "{valid} should be valid");
        }
        for invalid in [
            "",
            "Retry",
            "_retry",
            "-retry",
            "retry_",
            "retry-",
            "network..retry",
            "network.-retry",
            "luaskills.internal",
        ] {
            assert!(
                !is_valid_skill_config_key(invalid),
                "{invalid} should be invalid"
            );
        }
    }

    /// Verify integer declarations and values stay inside the shared safe-integer range.
    /// 验证整数声明和值保持在共享安全整数范围内。
    #[test]
    fn integers_use_the_shared_safe_integer_range() {
        let declarations = parse_declarations(&format!(
            r#"
- key: lower
  type: integer
  description: Lower safe integer.
  default: {minimum}
- key: upper
  type: integer
  description: Upper safe integer.
  default: {maximum}
"#,
            minimum = SKILL_CONFIG_MIN_SAFE_INTEGER,
            maximum = SKILL_CONFIG_MAX_SAFE_INTEGER
        ));
        assert_eq!(
            declarations[0].normalized_default_value().unwrap(),
            Some(SKILL_CONFIG_MIN_SAFE_INTEGER.to_string())
        );
        assert_eq!(
            declarations[1].normalized_default_value().unwrap(),
            Some(SKILL_CONFIG_MAX_SAFE_INTEGER.to_string())
        );
        assert!(
            declarations[0]
                .normalize_value("-9007199254740992")
                .is_err()
        );
        assert!(declarations[1].normalize_value("9007199254740992").is_err());
    }

    /// Verify negative floating-point zero has one canonical persisted representation.
    /// 验证负浮点零只有一种规范持久化表示。
    #[test]
    fn negative_float_zero_normalizes_to_zero() {
        let declarations = parse_declarations(
            r#"
- key: ratio
  type: float
  description: Sampling ratio.
"#,
        );
        assert_eq!(declarations[0].normalize_value("-0.0").unwrap(), "0");
    }
}
