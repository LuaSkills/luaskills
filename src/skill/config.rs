use serde::{Deserialize, Serialize};
use serde_json::{Number as JsonNumber, Value as JsonValue};
use std::collections::BTreeSet;

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
            .map(|value| {
                let raw_value = match (self.value_type, value) {
                    (SkillPackageConfigType::Integer, JsonValue::Number(number))
                        if number.as_i64().is_some() =>
                    {
                        number.to_string()
                    }
                    (SkillPackageConfigType::Float, JsonValue::Number(number)) => {
                        number.to_string()
                    }
                    (
                        SkillPackageConfigType::String | SkillPackageConfigType::Enum,
                        JsonValue::String(text),
                    ) => text.clone(),
                    (SkillPackageConfigType::Boolean, JsonValue::Bool(flag)) => flag.to_string(),
                    _ => {
                        return Err(format!(
                            "configuration '{}' default must use the declared {} type",
                            self.key,
                            self.value_type.as_str()
                        ));
                    }
                };
                self.normalize_value(&raw_value)
            })
            .transpose()
    }

    /// Normalize and validate one signed 64-bit integer value.
    /// 规范化并校验一个有符号 64 位整数值。
    fn normalize_integer(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        let value = raw_value.trim().parse::<i64>().map_err(|error| {
            value_error(
                "invalid_integer",
                format!(
                    "configuration '{}' requires one signed 64-bit integer: {}",
                    self.key, error
                ),
            )
        })?;
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
        let value = raw_value.trim().parse::<f64>().map_err(|error| {
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
        Ok(value.to_string())
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
        Err(value_error(
            "enum_value_not_allowed",
            format!(
                "configuration '{}' value '{}' is not allowed; expected one of: {}",
                self.key, raw_value, allowed
            ),
        ))
    }

    /// Normalize one strict lowercase boolean value.
    /// 规范化一个严格小写布尔值。
    fn normalize_boolean(&self, raw_value: &str) -> Result<String, SkillPackageConfigValueError> {
        match raw_value.trim() {
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

/// Validate all package-level configuration declarations in one manifest.
/// 校验单个清单内的全部包级配置声明。
pub fn validate_skill_package_config_declarations(
    skill_id: &str,
    declarations: &mut [SkillPackageConfigDeclaration],
) -> Result<(), String> {
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
    if declaration.key.trim() != declaration.key || declaration.key.is_empty() {
        return Err(format!(
            "skill package '{}' configuration key must be non-empty and contain no surrounding whitespace",
            skill_id
        ));
    }
    validate_non_empty_text(
        &declaration.description,
        &format!("configuration '{}' description", declaration.key),
    )?;
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
                            "configuration '{}' integer minimum must be one signed 64-bit integer",
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
                            "configuration '{}' integer maximum must be one signed 64-bit integer",
                            declaration.key
                        )
                    })
                })
                .transpose()?;
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
    let mut values = BTreeSet::new();
    for option in &mut declaration.options {
        if option.value.is_empty() || option.value.trim() != option.value {
            return Err(format!(
                "configuration '{}' enum option value must be non-empty and contain no surrounding whitespace",
                declaration.key
            ));
        }
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
        validate_non_empty_text(
            &option.description,
            &format!(
                "configuration '{}' enum option '{}' description",
                declaration.key, option.value
            ),
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

        assert_eq!(declarations[0].normalize_value(" 003 ").unwrap(), "3");
        assert!(declarations[0].normalize_value("11").is_err());
        assert_eq!(declarations[1].normalize_value("中文").unwrap(), "中文");
        assert!(declarations[1].normalize_value("中文字符多").is_err());
        assert_eq!(declarations[2].normalize_value(" 0.500 ").unwrap(), "0.5");
        assert!(declarations[2].normalize_value("NaN").is_err());
        assert_eq!(declarations[3].normalize_value("openai").unwrap(), "openai");
        assert!(declarations[3].normalize_value("unknown").is_err());
        assert_eq!(declarations[4].normalize_value(" true ").unwrap(), "true");
        assert!(declarations[4].normalize_value("TRUE").is_err());
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
}
