use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::runtime::config::{SkillConfigEntry, SkillConfigStore};
use crate::skill::config::{
    SkillPackageConfigConstraints, SkillPackageConfigDeclaration, SkillPackageConfigType,
};
use crate::skill::manifest::SkillMeta;

/// Effective value source reported by package configuration structure queries.
/// 技能包配置结构查询报告的有效值来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPackageConfigValueSource {
    /// One explicit value is persisted in the unified configuration file.
    /// 统一配置文件中持久化了一个显式值。
    Stored,
    /// No explicit value exists and the declaration supplies one default.
    /// 不存在显式值，声明提供了一个默认值。
    Default,
    /// Neither one explicit value nor one default exists.
    /// 既不存在显式值，也不存在默认值。
    Unset,
}

/// One enumeration option returned by runtime structure queries.
/// 运行时结构查询返回的单个枚举选项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigEnumOption {
    /// Stable machine value persisted by this option.
    /// 当前选项持久化的稳定机器值。
    pub value: String,
    /// Human-readable label written by the package author.
    /// 技能包作者编写的人类可读名称。
    pub label: String,
    /// Human-readable description written by the package author.
    /// 技能包作者编写的人类可读说明。
    pub description: String,
}

/// One package-level configuration item returned by runtime structure queries.
/// 运行时结构查询返回的单个包级配置项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigItemDescriptor {
    /// Stable package-level configuration key.
    /// 稳定包级配置键。
    pub key: String,
    /// Declared configuration value type.
    /// 声明的配置值类型。
    #[serde(rename = "type")]
    pub value_type: SkillPackageConfigType,
    /// Whether one effective value is required for completeness.
    /// 完整性是否要求存在一个有效值。
    pub required: bool,
    /// Sensitive-value hint consumed only by host-side policy.
    /// 仅供宿主侧策略使用的敏感值提示。
    pub sensitive: bool,
    /// Human-readable description written by the package author.
    /// 技能包作者编写的人类可读说明。
    pub description: String,
    /// Type-specific declaration constraints.
    /// 类型专属声明约束。
    pub constraints: SkillPackageConfigConstraints,
    /// Enumeration options, empty for non-enum types.
    /// 枚举选项，非枚举类型为空。
    pub options: Vec<RuntimeSkillPackageConfigEnumOption>,
    /// Optional normalized default value declared by the package.
    /// 技能包声明的可选规范化默认值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// Whether one explicit persisted value currently exists.
    /// 当前是否存在一个显式持久化值。
    pub configured: bool,
    /// Source of the current effective value.
    /// 当前有效值的来源。
    pub source: SkillPackageConfigValueSource,
    /// Whether the current persisted or default value satisfies the declaration.
    /// 当前持久化值或默认值是否满足声明。
    pub valid: bool,
    /// Validation failure for one configured value invalidated by a newer declaration.
    /// 被新声明判定为非法的已配置值对应的校验错误。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_error: Option<RuntimeSkillPackageConfigValidationError>,
    /// Optional effective value included only when the host explicitly requests values.
    /// 仅在宿主显式请求值时包含的可选有效值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// One package configuration issue returned by completeness validation.
/// 完整性校验返回的单个技能包配置问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigIssue {
    /// Stable configuration key that owns the issue.
    /// 拥有当前问题的稳定配置键。
    pub key: String,
    /// Stable machine-readable issue code.
    /// 稳定机器可读问题代码。
    pub code: String,
    /// Human-readable issue explanation.
    /// 人类可读问题说明。
    pub message: String,
}

/// Structured validation failure attached to one configuration item descriptor.
/// 附加到单个配置项描述的结构化校验失败信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigValidationError {
    /// Stable machine-readable validation code.
    /// 稳定机器可读校验代码。
    pub code: String,
    /// Human-readable validation explanation.
    /// 人类可读校验说明。
    pub message: String,
}

/// Completeness and validity status of one effective skill package configuration.
/// 单个有效技能包配置的完整性与合法性状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigStatus {
    /// Stable package identifier.
    /// 稳定技能包标识。
    pub skill_id: String,
    /// Whether every required and configured declared item is valid.
    /// 每个必填项和已配置声明项是否均合法。
    pub complete: bool,
    /// Required declarations without one explicit or default value.
    /// 缺少显式值和默认值的必填声明。
    pub missing: Vec<RuntimeSkillPackageConfigIssue>,
    /// Persisted declared values that fail the current declaration.
    /// 不满足当前声明的已持久化声明值。
    pub invalid: Vec<RuntimeSkillPackageConfigIssue>,
    /// Number of persisted keys no longer declared by the effective package.
    /// 当前有效技能包不再声明的持久化 key 数量。
    pub orphaned_count: usize,
}

/// Full package configuration structure returned to Rust and FFI hosts.
/// 返回给 Rust 与 FFI 宿主的完整技能包配置结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigDescriptor {
    /// Stable package identifier.
    /// 稳定技能包标识。
    pub skill_id: String,
    /// Semantic package version that owns this declaration.
    /// 拥有当前声明的语义化技能包版本。
    pub skill_version: String,
    /// Whether the package configuration is complete and valid.
    /// 当前技能包配置是否完整且合法。
    pub complete: bool,
    /// Number of persisted keys no longer declared by this package.
    /// 当前技能包不再声明的持久化 key 数量。
    pub orphaned_count: usize,
    /// Package-level configuration item descriptors.
    /// 包级配置项描述列表。
    pub items: Vec<RuntimeSkillPackageConfigItemDescriptor>,
}

/// One immutable effective package configuration schema stored in the runtime registry.
/// 运行时注册表存储的单个不可变有效技能包配置结构。
#[derive(Debug, Clone)]
struct SkillPackageConfigRegistryEntry {
    /// Semantic package version.
    /// 语义化技能包版本。
    skill_version: String,
    /// Package-level declarations indexed in manifest order.
    /// 按清单顺序保存的包级声明。
    declarations: Vec<SkillPackageConfigDeclaration>,
}

impl SkillPackageConfigRegistryEntry {
    /// Build one effective registry entry from one fully validated skill manifest.
    /// 从一个已完整校验的技能清单构建有效注册表项。
    fn from_meta(meta: &SkillMeta) -> Self {
        Self {
            skill_version: meta.version().to_string(),
            declarations: meta.package_config().cloned().collect(),
        }
    }

    /// Find one package-level declaration by its exact stable key.
    /// 根据精确稳定 key 查找单个包级声明。
    fn find(&self, key: &str) -> Option<&SkillPackageConfigDeclaration> {
        self.declarations
            .iter()
            .find(|declaration| declaration.key == key)
    }
}

/// Unified package configuration service that composes declarations and persisted values.
/// 组合声明与持久化值的统一技能包配置服务。
#[derive(Debug)]
pub(crate) struct SkillPackageConfigService {
    /// Unified raw configuration store.
    /// 统一原始配置存储。
    store: SkillConfigStore,
    /// Effective package declaration registry replaced after successful runtime loading.
    /// 运行时成功加载后替换的有效技能包声明注册表。
    registry: RwLock<BTreeMap<String, SkillPackageConfigRegistryEntry>>,
}

impl SkillPackageConfigService {
    /// Create one package configuration service from an optional explicit store path.
    /// 基于可选显式存储路径创建技能包配置服务。
    pub(crate) fn new(explicit_file_path: Option<PathBuf>) -> Result<Self, String> {
        Ok(Self {
            store: SkillConfigStore::new(explicit_file_path)?,
            registry: RwLock::new(BTreeMap::new()),
        })
    }

    /// Return whether the host pinned one explicit unified configuration file.
    /// 返回宿主是否固定了显式统一配置文件。
    pub(crate) fn has_explicit_file_path(&self) -> bool {
        self.store.has_explicit_file_path()
    }

    /// Resolve the effective unified configuration file path.
    /// 解析生效的统一配置文件路径。
    pub(crate) fn file_path(&self) -> Result<PathBuf, String> {
        self.store.file_path()
    }

    /// Capture the default runtime root used by the unified configuration file.
    /// 记录统一配置文件使用的默认运行时根目录。
    pub(crate) fn set_default_runtime_root(&self, runtime_root: &Path) -> Result<(), String> {
        self.store.set_default_runtime_root(runtime_root)
    }

    /// Atomically replace the effective package declaration registry from loaded manifests.
    /// 基于已加载清单原子替换有效技能包声明注册表。
    pub(crate) fn replace_registry<'a, I>(&self, manifests: I)
    where
        I: IntoIterator<Item = &'a SkillMeta>,
    {
        let next = manifests
            .into_iter()
            .map(|meta| {
                (
                    meta.effective_skill_id().to_string(),
                    SkillPackageConfigRegistryEntry::from_meta(meta),
                )
            })
            .collect();
        *self.lock_registry_write() = next;
    }

    /// List raw persisted configuration records for one optional package namespace.
    /// 列出某个可选技能包命名空间的原始持久化配置记录。
    pub(crate) fn list_raw_entries(
        &self,
        skill_id: Option<&str>,
    ) -> Result<Vec<SkillConfigEntry>, String> {
        self.store.list_entries(skill_id)
    }

    /// Read one raw persisted configuration value for the host management plane.
    /// 为宿主管理面读取单个原始持久化配置值。
    pub(crate) fn get_raw_value(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        self.store.get_value(skill_id, key)
    }

    /// Validate, normalize, and persist one declared package configuration value.
    /// 校验、规范化并持久化单个已声明技能包配置值。
    pub(crate) fn set_declared_value(
        &self,
        skill_id: &str,
        key: &str,
        value: &str,
    ) -> Result<String, String> {
        let declaration = self.declaration(skill_id, key)?;
        let normalized = declaration.normalize_value(value).map_err(|error| {
            format!(
                "invalid configuration value for skill package '{}': {}",
                skill_id, error
            )
        })?;
        self.store.set_value(skill_id, key, &normalized)?;
        Ok(normalized)
    }

    /// Delete one raw persisted value, including one orphaned value.
    /// 删除单个原始持久化值，包括遗留未声明值。
    pub(crate) fn delete_raw_value(&self, skill_id: &str, key: &str) -> Result<bool, String> {
        self.store.delete_value(skill_id, key)
    }

    /// Read one declared effective value for code running inside the owning package.
    /// 为所属技能包内部运行代码读取单个已声明有效值。
    pub(crate) fn get_effective_value(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        let declaration = self.declaration(skill_id, key)?;
        if let Some(stored) = self.store.get_value(skill_id, key)? {
            return declaration
                .normalize_value(&stored)
                .map(Some)
                .map_err(|error| {
                    format!(
                        "stored configuration for skill package '{}' is invalid: {}",
                        skill_id, error
                    )
                });
        }
        declaration.normalized_default_value()
    }

    /// Return whether one declared effective value exists for the owning package.
    /// 返回所属技能包是否存在单个已声明有效值。
    pub(crate) fn has_effective_value(&self, skill_id: &str, key: &str) -> Result<bool, String> {
        Ok(self.get_effective_value(skill_id, key)?.is_some())
    }

    /// List every declared effective value visible inside the owning package.
    /// 列出所属技能包内部可见的全部已声明有效值。
    pub(crate) fn list_effective_values(
        &self,
        skill_id: &str,
    ) -> Result<BTreeMap<String, String>, String> {
        let package = self.package(skill_id)?;
        let stored = self.store.list_skill_values(skill_id)?;
        let mut values = BTreeMap::new();
        for declaration in &package.declarations {
            match stored.get(&declaration.key) {
                Some(value) => {
                    let normalized = declaration.normalize_value(value).map_err(|error| {
                        format!(
                            "stored configuration for skill package '{}' is invalid: {}",
                            skill_id, error
                        )
                    })?;
                    values.insert(declaration.key.clone(), normalized);
                }
                None => {
                    if let Some(default) = declaration.normalized_default_value()? {
                        values.insert(declaration.key.clone(), default);
                    }
                }
            }
        }
        Ok(values)
    }

    /// Delete one declared explicit value from inside the owning package.
    /// 从所属技能包内部删除单个已声明显式值。
    pub(crate) fn delete_declared_value(&self, skill_id: &str, key: &str) -> Result<bool, String> {
        self.declaration(skill_id, key)?;
        self.store.delete_value(skill_id, key)
    }

    /// Describe one optional package scope with declared structure and optional values.
    /// 使用已声明结构和可选值描述一个可选技能包范围。
    pub(crate) fn describe(
        &self,
        skill_id: Option<&str>,
        include_values: bool,
    ) -> Result<Vec<RuntimeSkillPackageConfigDescriptor>, String> {
        let packages = {
            let registry = self.lock_registry_read();
            match skill_id {
                Some(skill_id) => vec![(
                    skill_id.to_string(),
                    registry.get(skill_id).cloned().ok_or_else(|| {
                        format!("skill package '{}' is not loaded or effective", skill_id)
                    })?,
                )],
                None => registry
                    .iter()
                    .map(|(skill_id, package)| (skill_id.clone(), package.clone()))
                    .collect(),
            }
        };
        packages
            .into_iter()
            .map(|(package_id, package)| {
                self.describe_package(&package_id, &package, include_values)
            })
            .collect()
    }

    /// Validate one effective package configuration without mutating persisted state.
    /// 在不修改持久化状态的前提下校验单个有效技能包配置。
    pub(crate) fn status(&self, skill_id: &str) -> Result<RuntimeSkillPackageConfigStatus, String> {
        let package = self.package(skill_id)?;
        let stored = self.store.list_skill_values(skill_id)?;
        Self::status_for_package(skill_id, &package, &stored)
    }

    /// Build one package descriptor from one immutable registry entry.
    /// 基于单个不可变注册表项构建技能包描述。
    fn describe_package(
        &self,
        skill_id: &str,
        package: &SkillPackageConfigRegistryEntry,
        include_values: bool,
    ) -> Result<RuntimeSkillPackageConfigDescriptor, String> {
        let stored = self.store.list_skill_values(skill_id)?;
        let items = package
            .declarations
            .iter()
            .map(|declaration| Self::describe_item(declaration, include_values, &stored))
            .collect::<Result<Vec<_>, _>>()?;
        let status = Self::status_for_package(skill_id, package, &stored)?;
        Ok(RuntimeSkillPackageConfigDescriptor {
            skill_id: skill_id.to_string(),
            skill_version: package.skill_version.clone(),
            complete: status.complete,
            orphaned_count: status.orphaned_count,
            items,
        })
    }

    /// Build one item descriptor and its current persisted/default state.
    /// 构建单个配置项描述及其当前持久化/默认状态。
    fn describe_item(
        declaration: &SkillPackageConfigDeclaration,
        include_values: bool,
        stored: &BTreeMap<String, String>,
    ) -> Result<RuntimeSkillPackageConfigItemDescriptor, String> {
        let options = declaration
            .options
            .iter()
            .map(|option| RuntimeSkillPackageConfigEnumOption {
                value: option.value.clone(),
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect();
        let default_value = declaration.normalized_default_value()?;
        let stored_value = stored.get(&declaration.key);
        let (configured, source, valid, validation_error, effective_value) = match stored_value {
            Some(stored) => match declaration.normalize_value_detailed(stored) {
                Ok(normalized) => (
                    true,
                    SkillPackageConfigValueSource::Stored,
                    true,
                    None,
                    Some(normalized),
                ),
                Err(error) => (
                    true,
                    SkillPackageConfigValueSource::Stored,
                    false,
                    Some(RuntimeSkillPackageConfigValidationError {
                        code: error.code.to_string(),
                        message: stored_value_issue_message(declaration, error.code),
                    }),
                    Some(stored.clone()),
                ),
            },
            None => match default_value.clone() {
                Some(default) => (
                    false,
                    SkillPackageConfigValueSource::Default,
                    true,
                    None,
                    Some(default),
                ),
                None => (
                    false,
                    SkillPackageConfigValueSource::Unset,
                    true,
                    None,
                    None,
                ),
            },
        };

        Ok(RuntimeSkillPackageConfigItemDescriptor {
            key: declaration.key.clone(),
            value_type: declaration.value_type,
            required: declaration.required,
            sensitive: declaration.sensitive,
            description: declaration.description.clone(),
            constraints: declaration.constraints.clone(),
            options,
            default_value,
            configured,
            source,
            valid,
            validation_error,
            value: include_values.then_some(effective_value).flatten(),
        })
    }

    /// Build one package status from declarations and raw persisted values.
    /// 基于声明与原始持久化值构建单个技能包状态。
    fn status_for_package(
        skill_id: &str,
        package: &SkillPackageConfigRegistryEntry,
        stored: &BTreeMap<String, String>,
    ) -> Result<RuntimeSkillPackageConfigStatus, String> {
        let declared_keys = package
            .declarations
            .iter()
            .map(|declaration| declaration.key.as_str())
            .collect::<BTreeSet<_>>();
        let orphaned_count = stored
            .keys()
            .filter(|key| !declared_keys.contains(key.as_str()))
            .count();
        let mut missing = Vec::new();
        let mut invalid = Vec::new();

        for declaration in &package.declarations {
            match stored.get(&declaration.key) {
                Some(value) => {
                    if let Err(error) = declaration.normalize_value_detailed(value) {
                        invalid.push(RuntimeSkillPackageConfigIssue {
                            key: declaration.key.clone(),
                            code: error.code.to_string(),
                            message: format!(
                                "{}: {}",
                                declaration.description,
                                stored_value_issue_message(declaration, error.code)
                            ),
                        });
                    }
                }
                None if declaration.required
                    && declaration.normalized_default_value()?.is_none() =>
                {
                    missing.push(RuntimeSkillPackageConfigIssue {
                        key: declaration.key.clone(),
                        code: "config_value_missing".to_string(),
                        message: format!(
                            "required configuration '{}' ({}) has no stored or default value",
                            declaration.key, declaration.description
                        ),
                    });
                }
                None => {}
            }
        }

        Ok(RuntimeSkillPackageConfigStatus {
            skill_id: skill_id.to_string(),
            complete: missing.is_empty() && invalid.is_empty(),
            missing,
            invalid,
            orphaned_count,
        })
    }

    /// Clone one effective package registry entry.
    /// 克隆单个有效技能包注册表项。
    fn package(&self, skill_id: &str) -> Result<SkillPackageConfigRegistryEntry, String> {
        self.lock_registry_read()
            .get(skill_id)
            .cloned()
            .ok_or_else(|| format!("skill package '{}' is not loaded or effective", skill_id))
    }

    /// Clone one declared item from one effective package.
    /// 从单个有效技能包克隆一个已声明配置项。
    fn declaration(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<SkillPackageConfigDeclaration, String> {
        let package = self.package(skill_id)?;
        package.find(key).cloned().ok_or_else(|| {
            format!(
                "configuration key '{}' is not declared by skill package '{}'",
                key, skill_id
            )
        })
    }

    /// Acquire one read guard for the declaration registry after lock poisoning.
    /// 在锁中毒后恢复并获取声明注册表读锁。
    fn lock_registry_read(
        &self,
    ) -> RwLockReadGuard<'_, BTreeMap<String, SkillPackageConfigRegistryEntry>> {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquire one write guard for the declaration registry after lock poisoning.
    /// 在锁中毒后恢复并获取声明注册表写锁。
    fn lock_registry_write(
        &self,
    ) -> RwLockWriteGuard<'_, BTreeMap<String, SkillPackageConfigRegistryEntry>> {
        self.registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Build a validation message that never embeds one persisted configuration value.
/// 构建绝不嵌入持久化配置值的校验消息。
fn stored_value_issue_message(declaration: &SkillPackageConfigDeclaration, code: &str) -> String {
    let reason = match code {
        "invalid_integer" => "stored value is not one signed 64-bit integer",
        "integer_out_of_range" => "stored integer is outside the declared inclusive range",
        "string_too_short" => "stored string is shorter than the declared minimum length",
        "string_too_long" => "stored string is longer than the declared maximum length",
        "invalid_float" => "stored value is not one 64-bit floating-point number",
        "float_not_finite" => "stored floating-point value is not finite",
        "float_out_of_range" => {
            "stored floating-point value is outside the declared inclusive range"
        }
        "enum_value_not_allowed" => "stored value is not one declared enumeration option",
        "invalid_boolean" => "stored value is not exactly 'true' or 'false'",
        _ => "stored value does not satisfy the current declaration",
    };
    format!("configuration '{}': {}", declaration.key, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Parse one package manifest and complete the same config validation used by runtime loading.
    /// 解析单个技能包清单，并完成与运行时加载相同的配置校验。
    ///
    /// `skill_id` is bound as the physical package identifier.
    /// `skill_id` 绑定为物理技能包标识符。
    ///
    /// `config_yaml` is inserted below the top-level config field.
    /// `config_yaml` 被插入顶层 config 字段下。
    ///
    /// Returns one validated package manifest.
    /// 返回一个已校验技能包清单。
    fn package_manifest(skill_id: &str, config_yaml: &str) -> SkillMeta {
        let yaml = format!(
            "name: {skill_id}\nversion: 1.2.3\nenable: true\ndebug: false\nconfig:\n{config_yaml}\nentries: []\n"
        );
        let mut manifest = serde_yaml::from_str::<SkillMeta>(&yaml).expect("manifest should parse");
        manifest.bind_directory_skill_id(skill_id.to_string());
        manifest
            .resolve_entry_input_schemas(Path::new("."))
            .expect("manifest configuration should validate");
        manifest
    }

    /// Return one process-unique explicit configuration file used by service tests.
    /// 返回服务测试使用的进程唯一显式配置文件。
    fn test_config_file(label: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "luaskills_package_config_service_{}_{}",
                std::process::id(),
                label
            ))
            .join("skill_config.json")
    }

    /// Verify service writes require loaded declarations and persist canonical values.
    /// 验证服务写入要求存在已加载声明并持久化规范值。
    #[test]
    fn service_rejects_undeclared_keys_and_persists_canonical_values() {
        let config_file = test_config_file("declared_write");
        if let Some(root) = config_file.parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(root);
        }
        let manifest = package_manifest(
            "demo-package",
            r#"  - key: retries
    type: integer
    required: true
    description: Retry count
    constraints:
      minimum: 0
      maximum: 10
  - key: enabled
    type: boolean
    default: true
    description: Whether the feature is enabled"#,
        );
        let service =
            SkillPackageConfigService::new(Some(config_file.clone())).expect("create service");
        service.replace_registry([&manifest]);

        let undeclared = service
            .set_declared_value("demo-package", "unknown", "1")
            .expect_err("undeclared key must fail");
        assert!(undeclared.contains("is not declared by skill package"));
        assert_eq!(
            service
                .set_declared_value("demo-package", "retries", " 003 ")
                .expect("set declared integer"),
            "3"
        );
        assert_eq!(
            service
                .get_raw_value("demo-package", "retries")
                .expect("read raw integer"),
            Some("3".to_string())
        );
        assert_eq!(
            service
                .get_effective_value("demo-package", "enabled")
                .expect("resolve boolean default"),
            Some("true".to_string())
        );

        if let Some(root) = config_file.parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// Verify structure queries preserve author metadata and omit values unless explicitly requested.
    /// 验证结构查询会保留作者元数据，并且仅在显式请求时包含配置值。
    #[test]
    fn describe_preserves_metadata_and_honors_include_values() {
        let config_file = test_config_file("describe");
        let manifest = package_manifest(
            "demo-package",
            r#"  - key: provider
    type: enum
    required: true
    description: Service provider
    options:
      - value: openai
        label: OpenAI
        description: OpenAI service"#,
        );
        let service =
            SkillPackageConfigService::new(Some(config_file.clone())).expect("create service");
        service.replace_registry([&manifest]);
        service
            .set_declared_value("demo-package", "provider", "openai")
            .expect("set enum value");

        let hidden = service
            .describe(Some("demo-package"), false)
            .expect("describe without values");
        assert_eq!(hidden[0].items[0].description, "Service provider");
        assert_eq!(hidden[0].items[0].options[0].description, "OpenAI service");
        let hidden_json = serde_json::to_value(&hidden).expect("serialize hidden descriptor");
        assert!(hidden_json[0]["items"][0].get("value").is_none());

        let visible = service
            .describe(Some("demo-package"), true)
            .expect("describe with values");
        assert_eq!(
            serde_json::to_value(&visible).expect("serialize visible descriptor")[0]["items"][0]["value"],
            "openai"
        );

        if let Some(root) = config_file.parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// Verify status reports missing, invalid, and orphaned states without mutating raw records.
    /// 验证状态查询会报告缺失、非法与遗留状态且不修改原始记录。
    #[test]
    fn status_reports_missing_invalid_and_orphaned_records() {
        let config_file = test_config_file("status");
        let manifest = package_manifest(
            "demo-package",
            r#"  - key: retries
    type: integer
    required: true
    description: Retry count
    constraints:
      minimum: 0
      maximum: 10
  - key: token
    type: string
    required: true
    sensitive: true
    description: Access token"#,
        );
        let service =
            SkillPackageConfigService::new(Some(config_file.clone())).expect("create service");
        service.replace_registry([&manifest]);
        service
            .store
            .set_value("demo-package", "retries", "99")
            .expect("inject old invalid value");
        service
            .store
            .set_value("demo-package", "removed_key", "legacy")
            .expect("inject orphaned value");

        let status = service.status("demo-package").expect("query status");
        assert!(!status.complete);
        assert_eq!(status.missing[0].key, "token");
        assert_eq!(status.invalid[0].code, "integer_out_of_range");
        assert!(!status.invalid[0].message.contains("99"));
        assert_eq!(status.orphaned_count, 1);

        let descriptor = service
            .describe(Some("demo-package"), true)
            .expect("describe invalid raw value");
        assert_eq!(descriptor[0].items[0].value.as_deref(), Some("99"));
        assert_eq!(
            descriptor[0].items[0]
                .validation_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("integer_out_of_range")
        );
        assert!(
            !descriptor[0].items[0]
                .validation_error
                .as_ref()
                .expect("invalid value should include validation error")
                .message
                .contains("99")
        );
        assert_eq!(
            service
                .get_raw_value("demo-package", "removed_key")
                .expect("read orphan after status"),
            Some("legacy".to_string())
        );
        assert!(
            service
                .delete_raw_value("demo-package", "removed_key")
                .expect("delete orphan through host path")
        );

        if let Some(root) = config_file.parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// Verify one empty package registry describes all effective packages in stable id order.
    /// 验证配置结构查询会按稳定标识顺序描述全部有效技能包。
    #[test]
    fn describe_all_packages_uses_stable_identifier_order() {
        let config_file = test_config_file("all_packages");
        let package_b = package_manifest("package-b", "  []");
        let package_a = package_manifest("package-a", "  []");
        let service =
            SkillPackageConfigService::new(Some(config_file.clone())).expect("create service");
        service.replace_registry([&package_b, &package_a]);
        let descriptors = service
            .describe(None, false)
            .expect("describe all packages");
        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| descriptor.skill_id.as_str())
                .collect::<Vec<_>>(),
            vec!["package-a", "package-b"]
        );
        assert_eq!(
            serde_json::to_value(descriptors).unwrap()[0]["items"],
            json!([])
        );

        if let Some(root) = config_file.parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
