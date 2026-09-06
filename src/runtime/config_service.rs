use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;
use std::time::Instant;

use mlua::{
    Function as LuaFunction, HookTriggers, Lua, LuaOptions, StdLib, Table as LuaTable,
    Value as LuaValue, VmState,
};

use crate::host::options::RuntimeSkillRoot;
use crate::lua_skill::{validate_luaskills_identifier, validate_luaskills_version};
use crate::runtime::config::{
    SkillConfigDeleteResult, SkillConfigEntry, SkillConfigRefreshResult, SkillConfigReloadTarget,
    SkillConfigReloadWatcher, SkillConfigStore, SkillConfigWriteResult,
};
use crate::skill::config::{
    SkillPackageConfigConstraints, SkillPackageConfigDeclaration, SkillPackageConfigFormat,
    SkillPackageConfigType, SkillPackageConfigValueError, is_valid_skill_config_key,
};
use crate::skill::manifest::SkillMeta;

/// Maximum number of configuration events retained by one engine.
/// 单个引擎保留的最大配置事件数量。
pub const SKILL_CONFIG_EVENT_QUEUE_CAPACITY: usize = 4_096;

/// Maximum number of configuration events returned by one poll.
/// 单次轮询返回的最大配置事件数量。
pub const SKILL_CONFIG_MAX_EVENT_POLL_LIMIT: usize = 1_024;

/// Default configuration file watcher debounce interval in milliseconds.
/// 默认配置文件监听防抖毫秒数。
pub const SKILL_CONFIG_DEFAULT_WATCH_DEBOUNCE_MS: u64 = 200;

/// Maximum configuration file watcher debounce interval in milliseconds.
/// 最大配置文件监听防抖毫秒数。
pub const SKILL_CONFIG_MAX_WATCH_DEBOUNCE_MS: u64 = 5_000;

/// Unambiguous runtime state of one package configuration item.
/// 单个技能包配置项的无歧义运行时状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPackageConfigItemState {
    /// One optional item has neither a stored value nor a default.
    /// 单个可选项既没有持久化值也没有默认值。
    Unset,
    /// One required item has neither a stored value nor a default.
    /// 单个必填项既没有持久化值也没有默认值。
    Missing,
    /// The declaration-provided default is effective.
    /// 声明提供的默认值当前生效。
    Default,
    /// One valid explicit persisted value is effective.
    /// 一个合法的显式持久化值当前生效。
    Configured,
    /// One persisted value fails the current declaration.
    /// 一个持久化值不满足当前声明。
    Invalid,
}

/// One typed scalar accepted by host and SDK configuration write requests.
/// 宿主与 SDK 配置写入请求接受的单个类型化标量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillPackageConfigInputValue {
    /// String input used by string, enum, and CLI-friendly scalar paths.
    /// 字符串、枚举和 CLI 友好标量路径使用的字符串输入。
    String(String),
    /// Signed integer input constrained by the common safe-integer contract.
    /// 受公共安全整数契约约束的有符号整数输入。
    Integer(i64),
    /// Finite floating-point input.
    /// 有限浮点输入。
    Float(f64),
    /// Strict boolean input.
    /// 严格布尔输入。
    Boolean(bool),
}

impl SkillPackageConfigInputValue {
    /// Convert one typed public input into the exact raw scalar accepted by a declaration.
    /// 把一个类型化公共输入转换为声明接受的精确原始标量。
    fn raw_for_declaration(
        &self,
        declaration: &SkillPackageConfigDeclaration,
    ) -> Result<String, String> {
        match (declaration.value_type, self) {
            (
                SkillPackageConfigType::String | SkillPackageConfigType::Enum,
                Self::String(value),
            )
            | (
                SkillPackageConfigType::Integer
                | SkillPackageConfigType::Float
                | SkillPackageConfigType::Boolean,
                Self::String(value),
            ) => Ok(value.clone()),
            (
                SkillPackageConfigType::Integer | SkillPackageConfigType::Float,
                Self::Integer(value),
            ) => Ok(value.to_string()),
            (SkillPackageConfigType::Float, Self::Float(value)) => {
                if !value.is_finite() {
                    return Err(format!(
                        "CONFIG_VALUE_TYPE_INVALID: configuration '{}' requires one finite float",
                        declaration.key
                    ));
                }
                Ok(value.to_string())
            }
            (SkillPackageConfigType::Boolean, Self::Boolean(value)) => Ok(value.to_string()),
            _ => Err(format!(
                "CONFIG_VALUE_TYPE_INVALID: configuration '{}' requires a {} input",
                declaration.key,
                declaration.value_type.as_str()
            )),
        }
    }
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
    /// Optional typed default value declared by the package and always safe to disclose.
    /// 技能包声明且始终可披露的可选类型化默认值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<JsonValue>,
    /// Optional short title used by host configuration interfaces.
    /// 宿主配置界面使用的可选短标题。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional host-facing grouping hint.
    /// 可选的宿主侧分组提示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Optional display order inside the selected group.
    /// 所选分组内的可选显示顺序。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    /// Whether hosts should present the item as advanced.
    /// 宿主是否应把该项展示为高级选项。
    pub advanced: bool,
    /// Optional host input placeholder.
    /// 可选宿主输入占位文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Optional typed example value.
    /// 可选类型化示例值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<JsonValue>,
    /// Optional host rendering format.
    /// 可选宿主渲染格式。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<SkillPackageConfigFormat>,
    /// Whether host-managed restart work may be required after a change.
    /// 修改后是否可能需要宿主管理的重启操作。
    pub restart_required: bool,
    /// Whether this declaration is deprecated.
    /// 当前声明是否已弃用。
    pub deprecated: bool,
    /// Optional deprecation guidance.
    /// 可选弃用说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation_message: Option<String>,
    /// Single unambiguous runtime state.
    /// 单一无歧义运行时状态。
    pub state: SkillPackageConfigItemState,
    /// Whether the current item satisfies package completeness.
    /// 当前配置项是否满足技能包完整性。
    pub satisfied: bool,
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

/// One optional-key cross-field issue returned by an isolated package validator.
/// 隔离技能包校验器返回的单个可选键跨字段问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillPackageConfigBusinessIssue {
    /// Optional declared configuration key associated with the issue.
    /// 与当前问题关联的可选已声明配置键。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Stable package-namespaced machine-readable issue code.
    /// 稳定且带技能包命名空间的机器可读问题代码。
    pub code: String,
    /// Human-readable issue explanation authored by the package.
    /// 由技能包编写的人类可读问题说明。
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
    /// Revision of the immutable snapshot used for this status.
    /// 当前状态使用的不可变快照修订号。
    pub revision: String,
    /// Persisted store scope selected by the effective package root.
    /// 由有效技能包根选择的持久化存储作用域。
    pub store_scope: String,
    /// Required declarations without one explicit or default value.
    /// 缺少显式值和默认值的必填声明。
    pub missing: Vec<RuntimeSkillPackageConfigIssue>,
    /// Persisted declared values that fail the current declaration.
    /// 不满足当前声明的已持久化声明值。
    pub invalid: Vec<RuntimeSkillPackageConfigIssue>,
    /// Cross-field issues returned by the isolated package validator.
    /// 隔离技能包校验器返回的跨字段问题。
    pub business_issues: Vec<RuntimeSkillPackageConfigBusinessIssue>,
    /// Persisted keys no longer declared by the effective package.
    /// 当前有效技能包不再声明的持久化键。
    pub orphaned: Vec<String>,
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
    /// Revision of the immutable snapshot used for this descriptor.
    /// 当前描述使用的不可变快照修订号。
    pub revision: String,
    /// Persisted store scope selected by the effective package root.
    /// 由有效技能包根选择的持久化存储作用域。
    pub store_scope: String,
    /// Number of required declarations that are currently missing.
    /// 当前缺失的必填声明数量。
    pub missing_count: usize,
    /// Number of persisted declared values invalid under the current declaration.
    /// 当前声明下非法的持久化已声明值数量。
    pub invalid_count: usize,
    /// Number of cross-field business validation issues.
    /// 跨字段业务校验问题数量。
    pub business_issue_count: usize,
    /// Number of persisted keys no longer declared by this package.
    /// 当前技能包不再声明的持久化 key 数量。
    pub orphaned_count: usize,
    /// Persisted keys no longer declared by this package.
    /// 当前技能包不再声明的持久化键。
    pub orphaned: Vec<String>,
    /// Package-level configuration item descriptors.
    /// 包级配置项描述列表。
    pub items: Vec<RuntimeSkillPackageConfigItemDescriptor>,
}

/// Read-only declaration discovery mode used by host configuration interfaces.
/// 宿主配置界面使用的只读声明发现模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillPackageConfigDescribeMode {
    /// Return only the effective package instance for each skill identifier.
    /// 每个技能标识符只返回有效技能包实例。
    #[default]
    Effective,
    /// Return every physical installed package instance without executing package Lua.
    /// 返回每个物理已安装技能包实例且不执行技能包 Lua。
    Installed,
}

/// One physical installed package declaration discovered without executing package Lua.
/// 在不执行技能包 Lua 的情况下发现的单个物理已安装技能包声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeInstalledSkillPackageConfigDescriptor {
    /// Directory-derived package identifier.
    /// 从目录派生的技能包标识符。
    pub skill_id: String,
    /// Named root that owns this physical package.
    /// 拥有当前物理技能包的命名根。
    pub root_name: String,
    /// Absolute physical package path for host-private diagnostics.
    /// 用于宿主私有诊断的绝对物理技能包路径。
    pub absolute_path: PathBuf,
    /// Whether the manifest enables this package.
    /// 清单是否启用当前技能包。
    pub enabled: bool,
    /// Whether an earlier root claims the same identifier.
    /// 是否有更高优先级根声明了相同标识符。
    pub shadowed: bool,
    /// Whether this physical instance is the effective declaration candidate.
    /// 当前物理实例是否为有效声明候选。
    pub effective: bool,
    /// Whether the manifest and package-level configuration declarations are valid.
    /// 清单及包级配置声明是否合法。
    pub manifest_valid: bool,
    /// Optional structured manifest issue for invalid or disable-marker directories.
    /// 非法或停用标记目录对应的可选结构化清单问题。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_issue: Option<RuntimeSkillConfigEventError>,
    /// Optional semantic package version from a valid manifest.
    /// 合法清单中的可选语义化技能包版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_version: Option<String>,
    /// Package-level declarations from a valid manifest.
    /// 合法清单中的包级配置声明。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<SkillPackageConfigDeclaration>,
}

/// Stable error object attached to one failed configuration reload event.
/// 附加到单个配置重载失败事件的稳定错误对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillConfigEventError {
    /// Stable machine-readable configuration error code.
    /// 稳定的机器可读配置错误码。
    pub code: String,
    /// Human-readable error message that never includes persisted secret values.
    /// 绝不包含持久化秘密值的人类可读错误消息。
    pub message: String,
}

/// One ordered skill configuration change or reload failure event.
/// 单个有序技能配置变更或重载失败事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillConfigEvent {
    /// Monotonic engine-local event sequence encoded as a decimal string.
    /// 编码为十进制字符串的引擎内单调事件序号。
    pub sequence: String,
    /// Stable event type.
    /// 稳定事件类型。
    #[serde(rename = "type")]
    pub event_type: String,
    /// Persisted store scope, either `skills` or `system-skills`.
    /// 持久化存储作用域，只能是 `skills` 或 `system-skills`。
    pub store_scope: String,
    /// Package changed by the event when one package can be identified.
    /// 当能够识别单个技能包时由事件变更的技能包。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    /// Last known valid store revision.
    /// 最后一个已知合法存储修订号。
    pub revision: String,
    /// Stable sorted keys changed for the package.
    /// 当前技能包发生变化的稳定排序键。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_keys: Vec<String>,
    /// Event source, either `local_write` or `external_reload`.
    /// 事件来源，只能是 `local_write` 或 `external_reload`。
    pub source: String,
    /// Changed keys whose declaration recommends a restart.
    /// 声明建议重启的已变更键。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_required_keys: Vec<String>,
    /// Package completeness after a local transaction when available.
    /// 本地事务完成后可用的技能包完整性。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
    /// Structured failure for reload and watcher errors.
    /// 重载与监听错误使用的结构化失败信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RuntimeSkillConfigEventError>,
}

/// Bounded ordered event batch returned to hosts and SDKs.
/// 返回给宿主与 SDK 的有界有序事件批次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillConfigEventBatch {
    /// Events whose sequence is greater than the requested cursor.
    /// 序号大于请求游标的事件。
    pub events: Vec<RuntimeSkillConfigEvent>,
    /// Highest sequence observed while producing this batch.
    /// 生成当前批次时观察到的最高序号。
    pub next_sequence: String,
}

/// One store refresh result returned by the explicit host management action.
/// 显式宿主管理动作返回的单个存储刷新结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSkillConfigStoreRefresh {
    /// Refreshed store scope.
    /// 已刷新的存储作用域。
    pub store_scope: String,
    /// Revision visible after refresh.
    /// 刷新后可见的修订号。
    pub revision: String,
    /// Whether a newer snapshot was installed.
    /// 是否安装了更新快照。
    pub changed: bool,
}

/// Bounded engine-local configuration event queue.
/// 有界的引擎内配置事件队列。
#[derive(Debug)]
struct SkillConfigEventQueue {
    /// Monotonic event sequence allocator.
    /// 单调事件序号分配器。
    next_sequence: AtomicU64,
    /// Oldest-first retained events.
    /// 按最旧优先顺序保留的事件。
    events: Mutex<VecDeque<RuntimeSkillConfigEvent>>,
}

impl SkillConfigEventQueue {
    /// Create one empty bounded event queue.
    /// 创建一个空的有界事件队列。
    fn new() -> Self {
        Self {
            next_sequence: AtomicU64::new(0),
            events: Mutex::new(VecDeque::new()),
        }
    }

    /// Append one event and assign its engine-local sequence.
    /// 追加单个事件并分配引擎内序号。
    fn publish(&self, mut event: RuntimeSkillConfigEvent) {
        // Serialize allocation and insertion together so concurrent publishers cannot reorder cursors.
        // 将序号分配与入队一并串行化，避免并发发布者打乱游标顺序。
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Stop accepting events after sequence exhaustion instead of wrapping and corrupting cursor order.
        // 序号耗尽后停止接收事件，避免回绕并破坏游标顺序。
        let Ok(previous) =
            self.next_sequence
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(1)
                })
        else {
            return;
        };
        let sequence = previous + 1;
        event.sequence = sequence.to_string();
        events.push_back(event);
        while events.len() > SKILL_CONFIG_EVENT_QUEUE_CAPACITY {
            events.pop_front();
        }
    }

    /// Return a bounded batch after one strict decimal cursor.
    /// 返回严格十进制游标之后的一个有界批次。
    fn poll(
        &self,
        after_sequence: Option<&str>,
        limit: usize,
    ) -> Result<RuntimeSkillConfigEventBatch, String> {
        if limit == 0 || limit > SKILL_CONFIG_MAX_EVENT_POLL_LIMIT {
            return Err(format!(
                "CONFIG_BATCH_TOO_LARGE: event poll limit must be between 1 and {}",
                SKILL_CONFIG_MAX_EVENT_POLL_LIMIT
            ));
        }
        let after = match after_sequence {
            Some(value)
                if !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && (value.len() == 1 || !value.starts_with('0')) =>
            {
                value.parse::<u64>().map_err(|_| {
                    "CONFIG_REVISION_INVALID: event cursor is outside the unsigned 64-bit range"
                        .to_string()
                })?
            }
            Some(_) => {
                return Err(
                    "CONFIG_REVISION_INVALID: event cursor must be a canonical unsigned decimal string"
                        .to_string(),
                );
            }
            None => 0,
        };
        let events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.next_sequence.load(Ordering::Acquire);
        if after > current {
            return Err(format!(
                "CONFIG_EVENT_CURSOR_INVALID: event cursor {after} is newer than the current sequence {current}"
            ));
        }
        if let Some(oldest) = events
            .front()
            .and_then(|event| event.sequence.parse::<u64>().ok())
            && after.saturating_add(1) < oldest
        {
            return Err(format!(
                "CONFIG_EVENT_CURSOR_EXPIRED: event cursor {after} precedes the oldest retained sequence {oldest}"
            ));
        }
        let batch: Vec<RuntimeSkillConfigEvent> = events
            .iter()
            .filter(|event| {
                event
                    .sequence
                    .parse::<u64>()
                    .is_ok_and(|sequence| sequence > after)
            })
            .take(limit)
            .cloned()
            .collect();
        let next_sequence = batch
            .last()
            .map(|event| event.sequence.clone())
            .unwrap_or_else(|| after.to_string());
        Ok(RuntimeSkillConfigEventBatch {
            events: batch,
            next_sequence,
        })
    }
}

/// One immutable effective package configuration schema stored in the runtime registry.
/// 运行时注册表存储的单个不可变有效技能包配置结构。
#[derive(Debug, Clone)]
struct SkillPackageConfigRegistryEntry {
    /// Semantic package version.
    /// 语义化技能包版本。
    skill_version: String,
    /// Named root that owns the effective package instance.
    /// 拥有有效技能包实例的命名根。
    root_name: String,
    /// Optional validated package-owned business validator path.
    /// 可选的已校验技能包所有业务校验器路径。
    config_validator_path: Option<PathBuf>,
    /// Package-level declarations indexed in manifest order.
    /// 按清单顺序保存的包级声明。
    declarations: Vec<SkillPackageConfigDeclaration>,
}

impl SkillPackageConfigRegistryEntry {
    /// Build one effective registry entry from one fully validated skill manifest.
    /// 从一个已完整校验的技能清单构建有效注册表项。
    fn from_meta(meta: &SkillMeta, root_name: &str, skill_dir: &std::path::Path) -> Self {
        Self {
            skill_version: meta.version().to_string(),
            root_name: root_name.to_string(),
            config_validator_path: meta
                .config_validator
                .as_ref()
                .map(|relative| skill_dir.join(relative)),
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

/// Host-level configuration stores split by effective package root ownership.
/// 按有效技能包根归属拆分的宿主级配置存储。
#[derive(Debug)]
struct SkillConfigStoreRouter {
    /// User-level store for every effective package outside the ROOT skill root.
    /// 用于 ROOT 技能根以外所有有效技能包的用户级存储。
    normal: Arc<SkillConfigStore>,
    /// User-level store dedicated to effective packages from the ROOT skill root.
    /// 专用于 ROOT 技能根有效技能包的用户级存储。
    system: Arc<SkillConfigStore>,
}

impl SkillConfigStoreRouter {
    /// Build both strict stores under one explicit absolute user configuration root.
    /// 在一个显式绝对用户配置根下构建两个严格存储。
    fn new(skill_config_root: PathBuf, lock_timeout: Duration) -> Result<Self, String> {
        if !skill_config_root.is_absolute() {
            return Err(
                "CONFIG_PATH_INVALID: host_options.skill_config_root must be an absolute path"
                    .to_string(),
            );
        }
        Ok(Self {
            normal: Arc::new(SkillConfigStore::with_lock_timeout(
                skill_config_root.join("skills").join("config.json"),
                lock_timeout,
            )?),
            system: Arc::new(SkillConfigStore::with_lock_timeout(
                skill_config_root.join("system-skills").join("config.json"),
                lock_timeout,
            )?),
        })
    }

    /// Resolve the only store allowed for one effective package root.
    /// 解析单个有效技能包根唯一允许使用的存储。
    fn for_root(&self, root_name: &str) -> &SkillConfigStore {
        if root_name == "ROOT" {
            self.system.as_ref()
        } else {
            self.normal.as_ref()
        }
    }
}

/// Unified package configuration service that composes declarations and persisted values.
/// 组合声明与持久化值的统一技能包配置服务。
#[derive(Debug)]
pub(crate) struct SkillPackageConfigService {
    /// Root-aware normal and system package configuration stores.
    /// 感知根归属的普通与系统技能包配置存储。
    stores: Option<SkillConfigStoreRouter>,
    /// Single shared native file watcher retained for the service lifetime.
    /// 在服务生命周期内保留的单个共享原生文件监听器。
    _watcher: Option<SkillConfigReloadWatcher>,
    /// Ordered configuration event queue shared with watcher callbacks.
    /// 与监听回调共享的有序配置事件队列。
    events: Arc<SkillConfigEventQueue>,
    /// Effective package declaration registry replaced after successful runtime loading.
    /// 运行时成功加载后替换的有效技能包声明注册表。
    registry: RwLock<BTreeMap<String, SkillPackageConfigRegistryEntry>>,
    /// Physical installed package declarations discovered without executing Lua.
    /// 在不执行 Lua 的情况下发现的物理已安装技能包声明。
    installed: RwLock<Vec<RuntimeInstalledSkillPackageConfigDescriptor>>,
}

impl SkillPackageConfigService {
    /// Create one package configuration service from one explicit user configuration root.
    /// 基于一个显式用户配置根创建技能包配置服务。
    pub(crate) fn new(
        skill_config_root: Option<PathBuf>,
        lock_timeout: Duration,
        watch_debounce: Duration,
    ) -> Result<Self, String> {
        if lock_timeout.is_zero() || lock_timeout > Duration::from_secs(60) {
            return Err(
                "CONFIG_PATH_INVALID: skill_config_lock_timeout_ms must be between 1 and 60000"
                    .to_string(),
            );
        }
        if watch_debounce.is_zero()
            || watch_debounce > Duration::from_millis(SKILL_CONFIG_MAX_WATCH_DEBOUNCE_MS)
        {
            return Err(format!(
                "CONFIG_WATCHER_FAILED: skill_config_watch_debounce_ms must be between 1 and {}",
                SKILL_CONFIG_MAX_WATCH_DEBOUNCE_MS
            ));
        }
        // Optional root-aware store router created only when persistence is enabled.
        // 仅在启用持久化时创建的可选根感知存储路由器。
        let stores = skill_config_root
            .map(|root| SkillConfigStoreRouter::new(root, lock_timeout))
            .transpose()?;
        // Ordered event queue shared by synchronous API calls and watcher callbacks.
        // 同步 API 调用与监听器回调共享的有序事件队列。
        let events = Arc::new(SkillConfigEventQueue::new());
        // One native watcher shared by normal and system configuration domains.
        // 普通与系统配置域共享的单个原生监听器。
        let watcher = if let Some(stores) = stores.as_ref() {
            // Exact target list routed through the shared watcher worker.
            // 通过共享监听工作线程路由的精确目标列表。
            let mut targets = Vec::with_capacity(2);
            for (scope, store) in [
                ("skills", Arc::clone(&stores.normal)),
                ("system-skills", Arc::clone(&stores.system)),
            ] {
                // Queue reference captured by this domain's ordered callback.
                // 当前配置域有序回调捕获的队列引用。
                let callback_events = Arc::clone(&events);
                // Store reference used to project refresh events after reload.
                // 重载后用于投影刷新事件的存储引用。
                let callback_store = Arc::clone(&store);
                // Stable store scope included in every emitted event.
                // 每个发出事件包含的稳定存储作用域。
                let callback_scope = scope.to_string();
                // Domain callback preserving existing event projection and ordering.
                // 保留现有事件投影与顺序的配置域回调。
                let callback = Arc::new(move |result: Result<SkillConfigRefreshResult, String>| {
                    publish_external_refresh_events(
                        callback_events.as_ref(),
                        &callback_scope,
                        callback_store.as_ref(),
                        result,
                    );
                });
                targets.push(SkillConfigReloadTarget::new(store, callback));
            }
            retain_config_watcher_or_publish_start_failure(
                SkillConfigReloadWatcher::start(targets, watch_debounce),
                stores,
                events.as_ref(),
            )
        } else {
            None
        };
        Ok(Self {
            stores,
            _watcher: watcher,
            events,
            registry: RwLock::new(BTreeMap::new()),
            installed: RwLock::new(Vec::new()),
        })
    }

    /// Atomically replace the effective package declaration registry from loaded manifests.
    /// 基于已加载清单原子替换有效技能包声明注册表。
    pub(crate) fn replace_registry<'a, I>(&self, packages: I)
    where
        I: IntoIterator<Item = (&'a SkillMeta, &'a str, &'a std::path::Path)>,
    {
        let next = packages
            .into_iter()
            .map(|(meta, root_name, skill_dir)| {
                (
                    meta.effective_skill_id().to_string(),
                    SkillPackageConfigRegistryEntry::from_meta(meta, root_name, skill_dir),
                )
            })
            .collect();
        *self.lock_registry_write() = next;
    }

    /// Rebuild the physical installed declaration catalog without executing package Lua.
    /// 在不执行技能包 Lua 的情况下重建物理已安装声明目录。
    pub(crate) fn replace_installed_catalog(
        &self,
        roots: &[RuntimeSkillRoot],
    ) -> Result<(), String> {
        let mut claimed = BTreeSet::new();
        let mut installed = Vec::new();
        for root in roots {
            if !root.skills_dir.try_exists().map_err(|error| {
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to inspect skill root '{}': {}",
                    root.skills_dir.display(),
                    error
                )
            })? {
                continue;
            }
            let root_entries = std::fs::read_dir(&root.skills_dir).map_err(|error| {
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to read skill root '{}': {}",
                    root.skills_dir.display(),
                    error
                )
            })?;
            let mut directories = Vec::new();
            for entry in root_entries {
                let entry = entry.map_err(|error| {
                    format!(
                        "CONFIG_DECLARATION_INVALID: failed to enumerate skill root '{}': {}",
                        root.skills_dir.display(),
                        error
                    )
                })?;
                let file_type = entry.file_type().map_err(|error| {
                    format!(
                        "CONFIG_DECLARATION_INVALID: failed to inspect installed entry '{}': {}",
                        entry.path().display(),
                        error
                    )
                })?;
                if file_type.is_dir() {
                    directories.push(entry);
                }
            }
            directories.sort_by_key(std::fs::DirEntry::file_name);
            for directory in directories {
                let skill_dir = directory.path();
                let skill_id = directory.file_name().to_string_lossy().into_owned();
                let shadowed = claimed.contains(&skill_id);
                claimed.insert(skill_id.clone());
                let absolute_path = std::path::absolute(&skill_dir).map_err(|error| {
                    format!(
                        "CONFIG_DECLARATION_INVALID: failed to resolve absolute installed path '{}': {}",
                        skill_dir.display(),
                        error
                    )
                })?;
                let manifest_path = skill_dir.join("skill.yaml");
                let manifest_metadata = std::fs::metadata(&manifest_path);
                if manifest_metadata
                    .as_ref()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    let mut entries = std::fs::read_dir(&skill_dir).map_err(|error| {
                        format!(
                            "CONFIG_DECLARATION_INVALID: failed to inspect installed skill directory '{}': {}",
                            skill_dir.display(),
                            error
                        )
                    })?;
                    let disable_marker = entries.next().transpose().map_err(|error| {
                        format!(
                            "CONFIG_DECLARATION_INVALID: failed to enumerate installed skill directory '{}': {}",
                            skill_dir.display(),
                            error
                        )
                    })?.is_none();
                    installed.push(RuntimeInstalledSkillPackageConfigDescriptor {
                        skill_id,
                        root_name: root.name.clone(),
                        absolute_path,
                        enabled: false,
                        shadowed,
                        effective: false,
                        manifest_valid: disable_marker,
                        manifest_issue: (!disable_marker).then(|| RuntimeSkillConfigEventError {
                            code: "CONFIG_DECLARATION_INVALID".to_string(),
                            message: "installed skill directory does not contain skill.yaml"
                                .to_string(),
                        }),
                        skill_version: None,
                        config: Vec::new(),
                    });
                    continue;
                }
                let manifest_metadata = manifest_metadata.map_err(|error| {
                    format!(
                        "CONFIG_DECLARATION_INVALID: failed to inspect installed manifest '{}': {}",
                        manifest_path.display(),
                        error
                    )
                })?;
                if !manifest_metadata.is_file() {
                    installed.push(RuntimeInstalledSkillPackageConfigDescriptor {
                        skill_id,
                        root_name: root.name.clone(),
                        absolute_path,
                        enabled: false,
                        shadowed,
                        effective: false,
                        manifest_valid: false,
                        manifest_issue: Some(RuntimeSkillConfigEventError {
                            code: "CONFIG_DECLARATION_INVALID".to_string(),
                            message: "installed skill.yaml is not a regular file".to_string(),
                        }),
                        skill_version: None,
                        config: Vec::new(),
                    });
                    continue;
                }
                let parsed = (|| -> Result<SkillMeta, String> {
                    validate_luaskills_identifier(&skill_id, "skill_id")?;
                    let source = std::fs::read_to_string(&manifest_path)
                        .map_err(|error| error.to_string())?;
                    let mut meta = serde_yaml::from_str::<SkillMeta>(&source)
                        .map_err(|error| error.to_string())?;
                    meta.bind_directory_skill_id(skill_id.clone());
                    validate_luaskills_version(meta.version(), "version")?;
                    meta.resolve_entry_input_schemas(&skill_dir)?;
                    Ok(meta)
                })();
                match parsed {
                    Ok(meta) => installed.push(RuntimeInstalledSkillPackageConfigDescriptor {
                        skill_id,
                        root_name: root.name.clone(),
                        absolute_path,
                        enabled: meta.enable,
                        shadowed,
                        effective: !shadowed && meta.enable,
                        manifest_valid: true,
                        manifest_issue: None,
                        skill_version: Some(meta.version().to_string()),
                        config: meta.package_config().cloned().collect(),
                    }),
                    Err(message) => {
                        installed.push(RuntimeInstalledSkillPackageConfigDescriptor {
                            skill_id,
                            root_name: root.name.clone(),
                            absolute_path,
                            enabled: false,
                            shadowed,
                            effective: false,
                            manifest_valid: false,
                            manifest_issue: Some(RuntimeSkillConfigEventError {
                                code: "CONFIG_DECLARATION_INVALID".to_string(),
                                message,
                            }),
                            skill_version: None,
                            config: Vec::new(),
                        });
                    }
                }
            }
        }
        *self
            .installed
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = installed;
        Ok(())
    }

    /// Return physical installed declarations filtered by optional package and root identifiers.
    /// 返回按可选技能包与根标识过滤的物理已安装声明。
    pub(crate) fn describe_installed(
        &self,
        skill_id: Option<&str>,
        root_name: Option<&str>,
    ) -> Vec<RuntimeInstalledSkillPackageConfigDescriptor> {
        self.installed
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|entry| skill_id.is_none_or(|value| entry.skill_id == value))
            .filter(|entry| root_name.is_none_or(|value| entry.root_name == value))
            .cloned()
            .collect()
    }

    /// List raw persisted configuration records for one optional package namespace.
    /// 列出某个可选技能包命名空间的原始持久化配置记录。
    pub(crate) fn list_raw_entries(
        &self,
        skill_id: Option<&str>,
    ) -> Result<Vec<SkillConfigEntry>, String> {
        match skill_id {
            Some(skill_id) => {
                let package = self.package(skill_id)?;
                let store_scope = if package.root_name == "ROOT" {
                    "system-skills"
                } else {
                    "skills"
                };
                self.store_for_package(&package)?
                    .list_entries(store_scope, Some(skill_id))
            }
            None => {
                let stores = self.stores()?;
                let mut entries = stores.normal.list_entries("skills", None)?;
                entries.extend(stores.system.list_entries("system-skills", None)?);
                entries.sort_by(|left, right| {
                    (&left.store_scope, &left.skill_id, &left.key).cmp(&(
                        &right.store_scope,
                        &right.skill_id,
                        &right.key,
                    ))
                });
                Ok(entries)
            }
        }
    }

    /// Read one raw persisted configuration value for the host management plane.
    /// 为宿主管理面读取单个原始持久化配置值。
    pub(crate) fn get_raw_value(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        self.store_for_skill(skill_id)?.get_value(skill_id, key)
    }

    /// Validate, normalize, and persist one declared package configuration value.
    /// 校验、规范化并持久化单个已声明技能包配置值。
    pub(crate) fn set_declared_value(
        &self,
        skill_id: &str,
        key: &str,
        value: &str,
    ) -> Result<String, String> {
        let result = self.set_declared_values(
            skill_id,
            BTreeMap::from([(
                key.to_string(),
                SkillPackageConfigInputValue::String(value.to_string()),
            )]),
            None,
        )?;
        result.values.get(key).cloned().ok_or_else(|| {
            format!(
                "CONFIG_ATOMIC_REPLACE_FAILED: committed batch omitted configuration '{}'",
                key
            )
        })
    }

    /// Validate, normalize, and atomically persist one package configuration batch.
    /// 校验、规范化并原子持久化单个技能包配置批次。
    pub(crate) fn set_declared_values(
        &self,
        skill_id: &str,
        values: BTreeMap<String, SkillPackageConfigInputValue>,
        expected_revision: Option<&str>,
    ) -> Result<SkillConfigWriteResult, String> {
        if values.is_empty() {
            return Err("CONFIG_BATCH_EMPTY: configuration batch must not be empty".to_string());
        }
        let package = self.package(skill_id)?;
        let mut normalized_values = BTreeMap::new();
        for (key, input_value) in values {
            let declaration = package.find(&key).ok_or_else(|| {
                format!(
                    "CONFIG_KEY_UNDECLARED: configuration key '{}' is not declared by skill package '{}'",
                    key, skill_id
                )
            })?;
            let raw_value = input_value.raw_for_declaration(declaration)?;
            let normalized = declaration
                .normalize_value_detailed(&raw_value)
                .map_err(|error| {
                    let code = public_value_error_code(error.code);
                    let message = if declaration.sensitive {
                        format!(
                            "invalid sensitive configuration value for key '{}'",
                            declaration.key
                        )
                    } else {
                        error.message
                    };
                    format!(
                        "{}: invalid configuration value for skill package '{}': {}",
                        code, skill_id, message
                    )
                })?;
            normalized_values.insert(key, normalized);
        }
        let result = self
            .store_for_package(&package)?
            .set_values_validated(
                skill_id,
                normalized_values,
                expected_revision,
                |candidate| {
                    let issues = run_business_validator(&package, candidate)?;
                    if issues.is_empty() {
                        Ok(())
                    } else {
                        let codes = issues
                            .iter()
                            .map(|issue| issue.code.as_str())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>()
                            .join(",");
                        Err(format!(
                            "CONFIG_VALIDATOR_FAILED: skill package '{}' configuration failed business validation with {} issue(s) [{}]",
                            skill_id,
                            issues.len(),
                            codes
                        ))
                    }
                },
            )?;
        self.finish_local_write(skill_id, &package, result)
    }

    /// Delete one raw persisted value, including one orphaned value.
    /// 删除单个原始持久化值，包括遗留未声明值。
    pub(crate) fn delete_raw_value(
        &self,
        skill_id: &str,
        key: &str,
        expected_revision: Option<&str>,
    ) -> Result<SkillConfigDeleteResult, String> {
        let package = self.package(skill_id)?;
        let result =
            self.store_for_package(&package)?
                .delete_value(skill_id, key, expected_revision)?;
        self.finish_local_delete(skill_id, &package, result)
    }

    /// Read one declared effective value for code running inside the owning package.
    /// 为所属技能包内部运行代码读取单个已声明有效值。
    pub(crate) fn get_effective_value(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        // Package is cloned once while the registry read lock is held and reused after lock release.
        // 包注册项仅在持有注册表读锁时克隆一次，并在锁释放后复用。
        let package = self.package(skill_id)?;
        // Declaration is resolved from the same immutable package snapshot used for store routing.
        // 声明从同一不可变包快照解析，并与存储路由共用该快照。
        let declaration = package.find(key).cloned().ok_or_else(|| {
            format!(
                "CONFIG_KEY_UNDECLARED: configuration key '{}' is not declared by skill package '{}'",
                key, skill_id
            )
        })?;
        // Store is selected from the already-resolved package without another registry lookup.
        // 存储从已解析包中选择，不再执行第二次注册表查找。
        let store = self.store_for_package(&package)?;
        if let Some(stored) = store.get_value(skill_id, key)? {
            return declaration
                .normalize_value_detailed(&stored)
                .map(Some)
                .map_err(|error| stored_config_validation_error(&declaration, error));
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
        let stored = self
            .store_for_package(&package)?
            .list_skill_values(skill_id)?;
        let mut values = BTreeMap::new();
        for declaration in &package.declarations {
            match stored.get(&declaration.key) {
                Some(value) => {
                    let normalized = declaration
                        .normalize_value_detailed(value)
                        .map_err(|error| stored_config_validation_error(declaration, error))?;
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
    pub(crate) fn delete_declared_value(
        &self,
        skill_id: &str,
        key: &str,
    ) -> Result<SkillConfigDeleteResult, String> {
        self.declaration(skill_id, key)?;
        self.delete_raw_value(skill_id, key, None)
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
                        format!(
                            "CONFIG_PACKAGE_NOT_FOUND: skill package '{}' is not loaded or effective",
                            skill_id
                        )
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
        let (revision, stored) = self
            .store_for_package(&package)?
            .skill_values_snapshot(skill_id)?;
        Self::status_for_package(skill_id, &package, &stored, revision)
    }

    /// Poll ordered configuration events after one optional engine-local cursor.
    /// 轮询一个可选引擎内游标之后的有序配置事件。
    pub(crate) fn poll_events(
        &self,
        after_sequence: Option<&str>,
        limit: usize,
    ) -> Result<RuntimeSkillConfigEventBatch, String> {
        self.events.poll(after_sequence, limit)
    }

    /// Explicitly refresh one selected store scope or both configured stores.
    /// 显式刷新一个选定存储作用域或两个已配置存储。
    pub(crate) fn refresh(
        &self,
        store_scope: Option<&str>,
    ) -> Result<Vec<RuntimeSkillConfigStoreRefresh>, String> {
        let configured = self.stores()?;
        let stores = match store_scope {
            Some("skills") => vec![("skills", configured.normal.as_ref())],
            Some("system-skills") => {
                vec![("system-skills", configured.system.as_ref())]
            }
            Some(other) => {
                return Err(format!(
                    "CONFIG_PATH_INVALID: unknown configuration store_scope '{}'",
                    other
                ));
            }
            None => vec![
                ("skills", configured.normal.as_ref()),
                ("system-skills", configured.system.as_ref()),
            ],
        };
        let mut results = Vec::with_capacity(stores.len());
        for (scope, store) in stores {
            let result = store.refresh();
            publish_external_refresh_events(self.events.as_ref(), scope, store, result.clone());
            let result = result?;
            results.push(RuntimeSkillConfigStoreRefresh {
                store_scope: scope.to_string(),
                revision: result.revision,
                changed: result.changed,
            });
        }
        Ok(results)
    }

    /// Build one package descriptor from one immutable registry entry.
    /// 基于单个不可变注册表项构建技能包描述。
    fn describe_package(
        &self,
        skill_id: &str,
        package: &SkillPackageConfigRegistryEntry,
        include_values: bool,
    ) -> Result<RuntimeSkillPackageConfigDescriptor, String> {
        let (revision, stored) = self
            .store_for_package(package)?
            .skill_values_snapshot(skill_id)?;
        let status = Self::status_for_package(skill_id, package, &stored, revision)?;
        let items = package
            .declarations
            .iter()
            .map(|declaration| {
                Self::describe_item(
                    declaration,
                    include_values,
                    &stored,
                    status
                        .business_issues
                        .iter()
                        .find(|issue| issue.key.as_deref() == Some(declaration.key.as_str())),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RuntimeSkillPackageConfigDescriptor {
            skill_id: skill_id.to_string(),
            skill_version: package.skill_version.clone(),
            complete: status.complete,
            revision: status.revision,
            store_scope: status.store_scope,
            missing_count: status.missing.len(),
            invalid_count: status.invalid.len(),
            business_issue_count: status.business_issues.len(),
            orphaned_count: status.orphaned_count,
            orphaned: status.orphaned,
            items,
        })
    }

    /// Build one item descriptor and its current persisted/default state.
    /// 构建单个配置项描述及其当前持久化/默认状态。
    fn describe_item(
        declaration: &SkillPackageConfigDeclaration,
        include_values: bool,
        stored: &BTreeMap<String, String>,
        business_issue: Option<&RuntimeSkillPackageConfigBusinessIssue>,
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
        let normalized_default = declaration.normalized_default_value()?;
        let stored_value = stored.get(&declaration.key);
        let (mut state, mut satisfied, mut validation_error, effective_value) = match stored_value {
            Some(stored) => match declaration.normalize_value_detailed(stored) {
                Ok(normalized) => (
                    SkillPackageConfigItemState::Configured,
                    true,
                    None,
                    Some(normalized),
                ),
                Err(error) => (
                    SkillPackageConfigItemState::Invalid,
                    false,
                    Some(RuntimeSkillPackageConfigValidationError {
                        code: error.code.to_string(),
                        message: stored_value_issue_message(declaration, error.code),
                    }),
                    Some(stored.clone()),
                ),
            },
            None => match normalized_default {
                Some(default) => (
                    SkillPackageConfigItemState::Default,
                    true,
                    None,
                    Some(default),
                ),
                None if declaration.required => {
                    (SkillPackageConfigItemState::Missing, false, None, None)
                }
                None => (SkillPackageConfigItemState::Unset, true, None, None),
            },
        };
        if let Some(issue) = business_issue {
            state = SkillPackageConfigItemState::Invalid;
            satisfied = false;
            validation_error = Some(RuntimeSkillPackageConfigValidationError {
                code: issue.code.clone(),
                message: issue.message.clone(),
            });
        }

        Ok(RuntimeSkillPackageConfigItemDescriptor {
            key: declaration.key.clone(),
            value_type: declaration.value_type,
            required: declaration.required,
            sensitive: declaration.sensitive,
            description: declaration.description.clone(),
            constraints: declaration.constraints.clone(),
            options,
            default: declaration.default.clone(),
            title: declaration.title.clone(),
            group: declaration.group.clone(),
            order: declaration.order,
            advanced: declaration.advanced,
            placeholder: declaration.placeholder.clone(),
            example: declaration.example.clone(),
            format: declaration.format,
            restart_required: declaration.restart_required,
            deprecated: declaration.deprecated,
            deprecation_message: declaration.deprecation_message.clone(),
            state,
            satisfied,
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
        revision: String,
    ) -> Result<RuntimeSkillPackageConfigStatus, String> {
        let declared_keys = package
            .declarations
            .iter()
            .map(|declaration| declaration.key.as_str())
            .collect::<BTreeSet<_>>();
        let orphaned = stored
            .keys()
            .filter(|key| !declared_keys.contains(key.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let orphaned_count = orphaned.len();
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

        let business_issues = if invalid.is_empty() {
            run_business_validator(package, stored)?
        } else {
            Vec::new()
        };
        Ok(RuntimeSkillPackageConfigStatus {
            skill_id: skill_id.to_string(),
            complete: missing.is_empty() && invalid.is_empty() && business_issues.is_empty(),
            revision,
            store_scope: if package.root_name == "ROOT" {
                "system-skills".to_string()
            } else {
                "skills".to_string()
            },
            missing,
            invalid,
            business_issues,
            orphaned,
            orphaned_count,
        })
    }

    /// Clone one effective package registry entry.
    /// 克隆单个有效技能包注册表项。
    fn package(&self, skill_id: &str) -> Result<SkillPackageConfigRegistryEntry, String> {
        self.lock_registry_read()
            .get(skill_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "CONFIG_PACKAGE_NOT_FOUND: skill package '{}' is not loaded or effective",
                    skill_id
                )
            })
    }

    /// Resolve the store selected by one immutable package registry entry.
    /// 根据一个不可变技能包注册表项解析所选存储。
    fn store_for_package(
        &self,
        package: &SkillPackageConfigRegistryEntry,
    ) -> Result<&SkillConfigStore, String> {
        Ok(self.stores()?.for_root(&package.root_name))
    }

    /// Resolve the only store authorized for one effective skill package.
    /// 解析单个有效技能包唯一获准使用的存储。
    fn store_for_skill(&self, skill_id: &str) -> Result<&SkillConfigStore, String> {
        let package = self.package(skill_id)?;
        self.store_for_package(&package)
    }

    /// Publish one local transaction event with declaration-derived restart and status metadata.
    /// 发布一个包含声明派生重启与状态元数据的本地事务事件。
    fn publish_local_change(
        &self,
        skill_id: &str,
        package: &SkillPackageConfigRegistryEntry,
        revision: &str,
        changed_keys: &[String],
    ) {
        let changed_key_set = changed_keys
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let restart_required_keys = package
            .declarations
            .iter()
            .filter(|declaration| {
                declaration.restart_required && changed_key_set.contains(declaration.key.as_str())
            })
            .map(|declaration| declaration.key.clone())
            .collect();
        let complete = self
            .status(skill_id)
            .map(|status| status.complete)
            .unwrap_or(false);
        self.events.publish(RuntimeSkillConfigEvent {
            sequence: String::new(),
            event_type: "skill_config_changed".to_string(),
            store_scope: if package.root_name == "ROOT" {
                "system-skills".to_string()
            } else {
                "skills".to_string()
            },
            skill_id: Some(skill_id.to_string()),
            revision: revision.to_string(),
            changed_keys: changed_keys.to_vec(),
            source: "local_write".to_string(),
            restart_required_keys,
            complete: Some(complete),
            error: None,
        });
    }

    /// Publish one committed write before surfacing its optional durability failure.
    /// 在报告可选耐久化失败之前发布一次已提交写入事件。
    fn finish_local_write(
        &self,
        skill_id: &str,
        package: &SkillPackageConfigRegistryEntry,
        result: SkillConfigWriteResult,
    ) -> Result<SkillConfigWriteResult, String> {
        if result.changed {
            self.publish_local_change(skill_id, package, &result.revision, &result.changed_keys);
        }
        match result.durability_error.clone() {
            Some(error) => Err(error),
            None => Ok(result),
        }
    }

    /// Publish one committed deletion before surfacing its optional durability failure.
    /// 在报告可选耐久化失败之前发布一次已提交删除事件。
    fn finish_local_delete(
        &self,
        skill_id: &str,
        package: &SkillPackageConfigRegistryEntry,
        result: SkillConfigDeleteResult,
    ) -> Result<SkillConfigDeleteResult, String> {
        if result.deleted {
            self.publish_local_change(
                skill_id,
                package,
                &result.revision,
                std::slice::from_ref(&result.key),
            );
        }
        match result.durability_error.clone() {
            Some(error) => Err(error),
            None => Ok(result),
        }
    }

    /// Return initialized stores or a stable host-configuration error.
    /// 返回已初始化存储，或稳定的宿主配置错误。
    fn stores(&self) -> Result<&SkillConfigStoreRouter, String> {
        self.stores.as_ref().ok_or_else(|| {
            "CONFIG_PATH_UNAVAILABLE: host_options.skill_config_root must be configured before using skill package configuration"
                .to_string()
        })
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
                "CONFIG_KEY_UNDECLARED: configuration key '{}' is not declared by skill package '{}'",
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

/// Retain one successfully started configuration watcher or publish a recoverable startup failure for every store domain.
/// 保留一个成功启动的配置监听器，或为每个存储域发布可恢复的启动失败事件。
///
/// The watcher_result parameter is the single shared backend startup result.
/// watcher_result 参数是单个共享后端的启动结果。
///
/// The stores parameter provides the exact ordinary and system stores associated with the failed backend.
/// stores 参数提供与失败后端关联的精确普通存储和系统存储。
///
/// The events parameter receives one ordered failure event per affected store domain.
/// events 参数为每个受影响的存储域接收一条有序失败事件。
///
/// Returns the live watcher on success or None after publishing a recoverable failure.
/// 成功时返回活动监听器；发布可恢复失败后返回 None。
fn retain_config_watcher_or_publish_start_failure(
    watcher_result: Result<SkillConfigReloadWatcher, String>,
    stores: &SkillConfigStoreRouter,
    events: &SkillConfigEventQueue,
) -> Option<SkillConfigReloadWatcher> {
    match watcher_result {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            for (store_scope, store) in [
                ("skills", stores.normal.as_ref()),
                ("system-skills", stores.system.as_ref()),
            ] {
                publish_external_refresh_events(events, store_scope, store, Err(error.clone()));
            }
            None
        }
    }
}

/// Publish per-package external changes or one structured reload failure.
/// 发布逐技能包外部变更事件，或一个结构化重载失败事件。
fn publish_external_refresh_events(
    events: &SkillConfigEventQueue,
    store_scope: &str,
    store: &SkillConfigStore,
    result: Result<SkillConfigRefreshResult, String>,
) {
    match result {
        Ok(result) if result.changed => {
            for (skill_id, changed_keys) in result.changes {
                events.publish(RuntimeSkillConfigEvent {
                    sequence: String::new(),
                    event_type: "skill_config_changed".to_string(),
                    store_scope: store_scope.to_string(),
                    skill_id: Some(skill_id),
                    revision: result.revision.clone(),
                    changed_keys,
                    source: "external_reload".to_string(),
                    restart_required_keys: Vec::new(),
                    complete: None,
                    error: None,
                });
            }
        }
        Ok(_) => {}
        Err(message) => {
            let code = message
                .split_once(':')
                .map(|(code, _)| code)
                .filter(|code| code.starts_with("CONFIG_"))
                .unwrap_or("CONFIG_RELOAD_FAILED")
                .to_string();
            events.publish(RuntimeSkillConfigEvent {
                sequence: String::new(),
                event_type: "skill_config_reload_failed".to_string(),
                store_scope: store_scope.to_string(),
                skill_id: None,
                revision: store.revision().unwrap_or_else(|_| "0".to_string()),
                changed_keys: Vec::new(),
                source: "external_reload".to_string(),
                restart_required_keys: Vec::new(),
                complete: None,
                error: Some(RuntimeSkillConfigEventError { code, message }),
            });
        }
    }
}

/// Maximum validator source size accepted from one skill package.
/// 单个技能包允许的最大校验器源码大小。
const SKILL_CONFIG_VALIDATOR_MAX_SOURCE_BYTES: u64 = 256 * 1_024;
/// Maximum Lua memory retained by one isolated validator state.
/// 单个隔离校验器 Lua 状态允许保留的最大内存。
const SKILL_CONFIG_VALIDATOR_MAX_MEMORY_BYTES: usize = 8 * 1_024 * 1_024;
/// Maximum approximate Lua instructions executed by one validator.
/// 单个校验器允许执行的近似最大 Lua 指令数。
const SKILL_CONFIG_VALIDATOR_MAX_INSTRUCTIONS: u64 = 1_000_000;
/// Maximum wall-clock duration of one validator invocation.
/// 单个校验器调用允许的最大墙钟时长。
const SKILL_CONFIG_VALIDATOR_TIMEOUT: Duration = Duration::from_millis(100);
/// Maximum number of structured issues returned by one validator.
/// 单个校验器允许返回的结构化问题最大数量。
const SKILL_CONFIG_VALIDATOR_MAX_ISSUES: usize = 1_024;

/// Execute one optional package business validator in an isolated capability-free Lua state.
/// 在隔离且无能力的 Lua 状态中执行一个可选技能包业务校验器。
fn run_business_validator(
    package: &SkillPackageConfigRegistryEntry,
    stored: &BTreeMap<String, String>,
) -> Result<Vec<RuntimeSkillPackageConfigBusinessIssue>, String> {
    let Some(validator_path) = package.config_validator_path.as_ref() else {
        return Ok(Vec::new());
    };
    let metadata = std::fs::metadata(validator_path).map_err(|error| {
        format!(
            "CONFIG_VALIDATOR_UNAVAILABLE: failed to inspect config validator '{}': {}",
            validator_path.display(),
            error
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "CONFIG_VALIDATOR_UNAVAILABLE: config validator '{}' is not a regular file",
            validator_path.display()
        ));
    }
    if metadata.len() > SKILL_CONFIG_VALIDATOR_MAX_SOURCE_BYTES {
        return Err(format!(
            "CONFIG_VALIDATOR_LIMIT_EXCEEDED: config validator source contains {} bytes, exceeding {}",
            metadata.len(),
            SKILL_CONFIG_VALIDATOR_MAX_SOURCE_BYTES
        ));
    }
    let source = std::fs::read_to_string(validator_path).map_err(|error| {
        format!(
            "CONFIG_VALIDATOR_UNAVAILABLE: failed to read config validator '{}': {}",
            validator_path.display(),
            error
        )
    })?;
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH,
        LuaOptions::default(),
    )
    .map_err(|error| format!("CONFIG_VALIDATOR_UNAVAILABLE: {error}"))?;
    lua.set_memory_limit(SKILL_CONFIG_VALIDATOR_MAX_MEMORY_BYTES)
        .map_err(|error| format!("CONFIG_VALIDATOR_LIMIT_EXCEEDED: {error}"))?;
    let globals = lua.globals();
    for forbidden in [
        "dofile", "loadfile", "load", "require", "package", "io", "os", "debug", "ffi", "jit",
    ] {
        globals
            .set(forbidden, LuaValue::Nil)
            .map_err(|error| format!("CONFIG_VALIDATOR_UNAVAILABLE: {error}"))?;
    }
    let instruction_count = Arc::new(AtomicU64::new(0));
    let hook_count = Arc::clone(&instruction_count);
    let deadline = Instant::now() + SKILL_CONFIG_VALIDATOR_TIMEOUT;
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1_000),
        move |_, _| {
            let executed = hook_count.fetch_add(1_000, Ordering::AcqRel) + 1_000;
            if executed > SKILL_CONFIG_VALIDATOR_MAX_INSTRUCTIONS {
                return Err(mlua::Error::runtime(
                    "CONFIG_VALIDATOR_LIMIT_EXCEEDED: instruction limit exceeded",
                ));
            }
            if Instant::now() >= deadline {
                return Err(mlua::Error::runtime(
                    "CONFIG_VALIDATOR_TIMEOUT: execution timed out",
                ));
            }
            Ok(VmState::Continue)
        },
    )
    .map_err(|error| format!("CONFIG_VALIDATOR_UNAVAILABLE: {error}"))?;
    let validator = lua
        .load(&source)
        .set_name(validator_path.to_string_lossy())
        .eval::<LuaFunction>()
        .map_err(|error| format!("CONFIG_VALIDATOR_FAILED: {error}"))?;
    let values = lua
        .create_table()
        .map_err(|error| format!("CONFIG_VALIDATOR_FAILED: {error}"))?;
    for declaration in &package.declarations {
        let normalized = match stored.get(&declaration.key) {
            Some(value) => Some(
                declaration
                    .normalize_value_detailed(value)
                    .map_err(|error| stored_config_validation_error(declaration, error))?,
            ),
            None => declaration.normalized_default_value()?,
        };
        if let Some(normalized) = normalized {
            set_validator_typed_value(&values, declaration, &normalized)?;
        }
    }
    let returned = validator
        .call::<LuaValue>(values)
        .map_err(|error| map_validator_runtime_error(&error.to_string()))?;
    lua.remove_hook();
    let table = match returned {
        LuaValue::Table(table) => table,
        _ => {
            return Err(
                "CONFIG_VALIDATOR_FAILED: validator must return an array table of issues"
                    .to_string(),
            );
        }
    };
    let issue_count = table.raw_len();
    let mut issue_member_count = 0_usize;
    for pair in table.clone().pairs::<LuaValue, LuaValue>() {
        let (key, _) =
            pair.map_err(|error| format!("CONFIG_VALIDATOR_FAILED: invalid issue array: {error}"))?;
        issue_member_count = issue_member_count.saturating_add(1);
        match key {
            LuaValue::Integer(index)
                if index >= 1 && usize::try_from(index).is_ok_and(|index| index <= issue_count) => {
            }
            _ => {
                return Err(
                    "CONFIG_VALIDATOR_FAILED: validator must return one dense array without extra keys"
                        .to_string(),
                );
            }
        }
    }
    if issue_member_count != issue_count {
        return Err(
            "CONFIG_VALIDATOR_FAILED: validator must return one dense array without holes"
                .to_string(),
        );
    }
    if issue_count > SKILL_CONFIG_VALIDATOR_MAX_ISSUES {
        return Err(format!(
            "CONFIG_VALIDATOR_LIMIT_EXCEEDED: validator returned {} issues, exceeding {}",
            issue_count, SKILL_CONFIG_VALIDATOR_MAX_ISSUES
        ));
    }
    let declared_keys = package
        .declarations
        .iter()
        .map(|declaration| declaration.key.as_str())
        .collect::<BTreeSet<_>>();
    let mut issues = Vec::with_capacity(issue_count);
    for index in 1..=issue_count {
        let issue = table.get::<LuaTable>(index).map_err(|error| {
            format!(
                "CONFIG_VALIDATOR_FAILED: validator issue {} must be an object: {}",
                index, error
            )
        })?;
        for pair in issue.clone().pairs::<LuaValue, LuaValue>() {
            let (field, _) = pair.map_err(|error| {
                format!("CONFIG_VALIDATOR_FAILED: invalid issue object: {error}")
            })?;
            let field = match field {
                LuaValue::String(field) => field.to_str().map_err(|error| {
                    format!("CONFIG_VALIDATOR_FAILED: invalid UTF-8 issue field: {error}")
                })?,
                _ => {
                    return Err(
                        "CONFIG_VALIDATOR_FAILED: validator issue fields must be strings"
                            .to_string(),
                    );
                }
            };
            if !matches!(field.as_ref(), "key" | "code" | "message") {
                return Err(
                    "CONFIG_VALIDATOR_FAILED: validator issue accepts only key, code, and message fields"
                        .to_string(),
                );
            }
        }
        let key = issue
            .get::<Option<String>>("key")
            .map_err(|error| format!("CONFIG_VALIDATOR_FAILED: invalid issue key: {error}"))?;
        if let Some(key) = key.as_deref()
            && !declared_keys.contains(key)
        {
            return Err(format!(
                "CONFIG_VALIDATOR_FAILED: validator issue references undeclared key '{}'",
                key
            ));
        }
        let code = issue
            .get::<String>("code")
            .map_err(|error| format!("CONFIG_VALIDATOR_FAILED: invalid issue code: {error}"))?;
        if !is_valid_skill_config_key(&code) {
            return Err(
                "CONFIG_VALIDATOR_FAILED: validator issue code must satisfy the config key contract"
                    .to_string(),
            );
        }
        let message = issue
            .get::<String>("message")
            .map_err(|error| format!("CONFIG_VALIDATOR_FAILED: invalid issue message: {error}"))?;
        if message.is_empty() || message.len() > 8_192 {
            return Err(
                "CONFIG_VALIDATOR_FAILED: validator issue message must contain 1 to 8192 UTF-8 bytes"
                .to_string(),
            );
        }
        let message = redact_sensitive_validator_values(package, stored, message)?;
        issues.push(RuntimeSkillPackageConfigBusinessIssue {
            key,
            code: format!("skill.{code}"),
            message,
        });
    }
    Ok(issues)
}

/// Insert one canonical persisted value into the validator table using its declared type.
/// 使用声明类型把一个规范持久化值插入校验器表。
fn set_validator_typed_value(
    values: &LuaTable,
    declaration: &SkillPackageConfigDeclaration,
    normalized: &str,
) -> Result<(), String> {
    match declaration.value_type {
        SkillPackageConfigType::Integer => values.set(
            declaration.key.as_str(),
            normalized
                .parse::<i64>()
                .map_err(|error| format!("CONFIG_VALUE_TYPE_INVALID: {error}"))?,
        ),
        SkillPackageConfigType::Float => values.set(
            declaration.key.as_str(),
            normalized
                .parse::<f64>()
                .map_err(|error| format!("CONFIG_VALUE_TYPE_INVALID: {error}"))?,
        ),
        SkillPackageConfigType::Boolean => {
            values.set(declaration.key.as_str(), normalized == "true")
        }
        SkillPackageConfigType::String | SkillPackageConfigType::Enum => {
            values.set(declaration.key.as_str(), normalized)
        }
    }
    .map_err(|error| format!("CONFIG_VALIDATOR_FAILED: {error}"))
}

/// Preserve stable timeout and limit codes from one validator runtime error.
/// 从单个校验器运行时错误中保留稳定的超时与限制错误码。
fn map_validator_runtime_error(message: &str) -> String {
    if message.contains("CONFIG_VALIDATOR_TIMEOUT") {
        "CONFIG_VALIDATOR_TIMEOUT: validator execution timed out".to_string()
    } else if message.contains("CONFIG_VALIDATOR_LIMIT_EXCEEDED")
        || message.contains("memory error")
    {
        "CONFIG_VALIDATOR_LIMIT_EXCEEDED: validator resource limit exceeded".to_string()
    } else {
        "CONFIG_VALIDATOR_FAILED: validator execution failed".to_string()
    }
}

/// Redact every nonempty sensitive effective value from one validator-authored issue message.
/// 从单条校验器问题消息中遮盖所有非空敏感有效值。
fn redact_sensitive_validator_values(
    package: &SkillPackageConfigRegistryEntry,
    stored: &BTreeMap<String, String>,
    mut message: String,
) -> Result<String, String> {
    for declaration in package
        .declarations
        .iter()
        .filter(|declaration| declaration.sensitive)
    {
        let value = match stored.get(&declaration.key) {
            Some(value) => Some(
                declaration
                    .normalize_value_detailed(value)
                    .map_err(|error| stored_config_validation_error(declaration, error))?,
            ),
            None => declaration.normalized_default_value()?,
        };
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            message = message.replace(&value, "[REDACTED]");
        }
    }
    Ok(message)
}

/// Convert one invalid persisted value into a stable error without exposing sensitive content.
/// 把单个非法持久化值转换为稳定错误，同时不暴露敏感内容。
fn stored_config_validation_error(
    declaration: &SkillPackageConfigDeclaration,
    error: SkillPackageConfigValueError,
) -> String {
    let code = public_value_error_code(error.code);
    let message = if declaration.sensitive {
        format!(
            "stored sensitive configuration '{}' is invalid",
            declaration.key
        )
    } else {
        format!(
            "stored configuration '{}' is invalid: {}",
            declaration.key, error.message
        )
    };
    format!("{code}: {message}")
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

/// Map one detailed declaration issue onto the stable public error-code vocabulary.
/// 把单个详细声明问题映射到稳定的公共错误码词汇。
fn public_value_error_code(code: &str) -> &'static str {
    match code {
        "integer_out_of_range" | "float_out_of_range" | "string_too_short" => {
            "CONFIG_VALUE_OUT_OF_RANGE"
        }
        "string_too_long" => "CONFIG_VALUE_TOO_LONG",
        "enum_value_not_allowed" => "CONFIG_ENUM_VALUE_INVALID",
        _ => "CONFIG_VALUE_TYPE_INVALID",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify detailed value failures map onto stable public error codes.
    /// 验证详细值失败会映射到稳定公共错误码。
    #[test]
    fn detailed_value_failures_use_stable_public_error_codes() {
        assert_eq!(
            public_value_error_code("integer_out_of_range"),
            "CONFIG_VALUE_OUT_OF_RANGE"
        );
        assert_eq!(
            public_value_error_code("enum_value_not_allowed"),
            "CONFIG_ENUM_VALUE_INVALID"
        );
        assert_eq!(
            public_value_error_code("string_too_long"),
            "CONFIG_VALUE_TOO_LONG"
        );
        assert_eq!(
            public_value_error_code("invalid_boolean"),
            "CONFIG_VALUE_TYPE_INVALID"
        );
    }
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

    /// Return one process-unique explicit configuration root used by service tests.
    /// 返回服务测试使用的进程唯一显式配置根目录。
    fn test_config_file(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "luaskills_package_config_service_{}_{}",
            std::process::id(),
            label
        ))
    }

    /// Build one validated package manifest with an isolated business validator file.
    /// 构建一个包含隔离业务校验器文件的合法技能包清单。
    fn package_manifest_with_validator(
        skill_dir: &Path,
        skill_id: &str,
        validator_source: &str,
    ) -> SkillMeta {
        let validator_path = skill_dir.join("runtime").join("config-validator.lua");
        std::fs::create_dir_all(
            validator_path
                .parent()
                .expect("validator path must have a parent"),
        )
        .expect("create validator directory");
        std::fs::write(&validator_path, validator_source).expect("write validator source");
        let yaml = format!(
            r#"name: {skill_id}
version: 1.2.3
enable: true
debug: false
config_validator: runtime/config-validator.lua
config:
  - key: token
    type: string
    required: true
    sensitive: true
    description: Access token
  - key: mode
    type: enum
    required: true
    description: Runtime mode
    options:
      - value: local
        label: Local
        description: Local execution
      - value: remote
        label: Remote
        description: Remote execution
entries: []
"#
        );
        let mut manifest = serde_yaml::from_str::<SkillMeta>(&yaml).expect("manifest should parse");
        manifest.bind_directory_skill_id(skill_id.to_string());
        manifest
            .resolve_entry_input_schemas(skill_dir)
            .expect("manifest configuration should validate");
        manifest
    }

    /// Write one minimal valid installed manifest without executing package Lua.
    /// 写入一个无需执行技能包 Lua 的最小合法已安装清单。
    fn write_installed_manifest(skill_dir: &Path, version: &str) {
        std::fs::create_dir_all(skill_dir).expect("create installed skill directory");
        std::fs::write(
            skill_dir.join("skill.yaml"),
            format!(
                "name: demo\nversion: {version}\nenable: true\ndebug: false\nconfig: []\nentries: []\n"
            ),
        )
        .expect("write installed manifest");
    }

    /// Verify service writes require loaded declarations and persist canonical values.
    /// 验证服务写入要求存在已加载声明并持久化规范值。
    #[test]
    fn service_rejects_undeclared_keys_and_persists_canonical_values() {
        let config_file = test_config_file("declared_write");
        let _ = std::fs::remove_dir_all(&config_file);
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
        let service = SkillPackageConfigService::new(
            Some(config_file.clone()),
            Duration::from_secs(5),
            Duration::from_millis(200),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", Path::new("."))]);

        let undeclared = service
            .set_declared_value("demo-package", "unknown", "1")
            .expect_err("undeclared key must fail");
        assert!(undeclared.contains("is not declared by skill package"));
        let whitespace_error = service
            .set_declared_value("demo-package", "retries", " 003 ")
            .expect_err("integer syntax with surrounding whitespace must fail");
        assert!(whitespace_error.contains("CONFIG_VALUE_TYPE_INVALID"));
        assert_eq!(
            service
                .set_declared_value("demo-package", "retries", "003")
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

        let _ = std::fs::remove_dir_all(&config_file);
    }

    /// Verify declaration-level failures never echo one sensitive candidate value.
    /// 验证声明级失败绝不回显敏感候选值。
    #[test]
    fn sensitive_declaration_failure_redacts_candidate_value() {
        let test_root = test_config_file("sensitive_declaration");
        let _ = std::fs::remove_dir_all(&test_root);
        let manifest = package_manifest(
            "demo-package",
            r#"  - key: token
    type: enum
    sensitive: true
    description: Private access token
    options:
      - value: approved-token
        label: Approved token
        description: Approved private token"#,
        );
        let service = SkillPackageConfigService::new(
            Some(test_root.join("config")),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", Path::new("."))]);
        let secret = "unapproved-sensitive-token";
        let error = service
            .set_declared_value("demo-package", "token", secret)
            .expect_err("undeclared sensitive enum option must fail");
        assert!(error.contains("CONFIG_ENUM_VALUE_INVALID"));
        assert!(!error.contains(secret));
        let _ = std::fs::remove_dir_all(&test_root);
    }

    /// Verify externally persisted invalid sensitive values never appear in runtime errors.
    /// 验证外部持久化的非法敏感值绝不会出现在运行时错误中。
    #[test]
    fn invalid_persisted_sensitive_value_is_redacted() {
        // Isolated configuration root for the persisted sensitive-value fixture.
        // 持久化敏感值夹具使用的隔离配置根目录。
        let test_root = test_config_file("invalid_persisted_sensitive");
        let _ = std::fs::remove_dir_all(&test_root);
        // Manifest declaring one sensitive enum whose raw store can be invalidated externally.
        // 声明单个敏感枚举的清单，其原始存储可被外部写成非法值。
        let manifest = package_manifest(
            "demo-package",
            r#"  - key: token
    type: enum
    sensitive: true
    description: Private access token
    options:
      - value: approved-token
        label: Approved token
        description: Approved private token"#,
        );
        // Service under test with the package declaration registered.
        // 已注册技能包声明的被测服务。
        let service = SkillPackageConfigService::new(
            Some(test_root.join("config")),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", Path::new("."))]);
        // Sensitive raw value emulating an out-of-process file edit.
        // 模拟进程外文件编辑的敏感原始值。
        let secret = "externally-invalid-sensitive-token";
        service
            .store_for_skill("demo-package")
            .expect("resolve package store")
            .set_values(
                "demo-package",
                BTreeMap::from([("token".to_string(), secret.to_string())]),
                None,
            )
            .expect("persist raw invalid sensitive value");
        // Stable runtime failure that must omit the raw secret.
        // 必须省略原始密钥的稳定运行时失败。
        let error = service
            .get_effective_value("demo-package", "token")
            .expect_err("invalid persisted enum must fail");

        assert!(error.starts_with("CONFIG_ENUM_VALUE_INVALID:"), "{error}");
        assert!(!error.contains(secret), "{error}");
        let _ = std::fs::remove_dir_all(&test_root);
    }

    /// Verify business validation is atomic and never echoes one sensitive candidate value.
    /// 验证业务校验保持原子性且绝不回显敏感候选值。
    #[test]
    fn business_validator_rejects_atomically_and_redacts_sensitive_values() {
        let test_root = test_config_file("business_validator");
        let _ = std::fs::remove_dir_all(&test_root);
        let skill_dir = test_root.join("package");
        let manifest = package_manifest_with_validator(
            &skill_dir,
            "demo-package",
            r#"return function(values)
  if values.mode == "remote" then
    return {{key = "mode", code = "remote_requires_local", message = "rejected token " .. values.token}}
  end
  return {}
end"#,
        );
        let service = SkillPackageConfigService::new(
            Some(test_root.join("config")),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", skill_dir.as_path())]);
        let secret = "sensitive-candidate-value";
        let error = service
            .set_declared_values(
                "demo-package",
                BTreeMap::from([
                    (
                        "token".to_string(),
                        SkillPackageConfigInputValue::String(secret.to_string()),
                    ),
                    (
                        "mode".to_string(),
                        SkillPackageConfigInputValue::String("remote".to_string()),
                    ),
                ]),
                Some("0"),
            )
            .expect_err("business issue must reject the complete batch");
        assert!(error.contains("CONFIG_VALIDATOR_FAILED"));
        assert!(!error.contains(secret));
        assert_eq!(
            service
                .stores
                .as_ref()
                .expect("initialized stores")
                .normal
                .revision()
                .expect("read unchanged revision"),
            "0"
        );
        assert!(
            service
                .list_raw_entries(Some("demo-package"))
                .expect("list unchanged package")
                .is_empty()
        );

        service
            .stores
            .as_ref()
            .expect("initialized stores")
            .normal
            .set_values(
                "demo-package",
                BTreeMap::from([
                    ("token".to_string(), secret.to_string()),
                    ("mode".to_string(), "remote".to_string()),
                ]),
                Some("0"),
            )
            .expect("inject preexisting values for status diagnostics");
        let status = service
            .status("demo-package")
            .expect("validate package status");
        assert_eq!(status.business_issues.len(), 1);
        assert_eq!(status.business_issues[0].key.as_deref(), Some("mode"));
        assert!(status.business_issues[0].message.contains("[REDACTED]"));
        assert!(!status.business_issues[0].message.contains(secret));

        let _ = std::fs::remove_dir_all(&test_root);
    }

    /// Verify an infinite validator is stopped by the isolated execution budget.
    /// 验证无限循环校验器会被隔离执行预算终止。
    #[test]
    fn business_validator_enforces_execution_budget() {
        let test_root = test_config_file("business_validator_timeout");
        let _ = std::fs::remove_dir_all(&test_root);
        let skill_dir = test_root.join("package");
        let manifest = package_manifest_with_validator(
            &skill_dir,
            "demo-package",
            "return function(values) while true do end end",
        );
        let service = SkillPackageConfigService::new(
            Some(test_root.join("config")),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", skill_dir.as_path())]);
        let error = service
            .set_declared_values(
                "demo-package",
                BTreeMap::from([
                    (
                        "token".to_string(),
                        SkillPackageConfigInputValue::String("secret".to_string()),
                    ),
                    (
                        "mode".to_string(),
                        SkillPackageConfigInputValue::String("local".to_string()),
                    ),
                ]),
                None,
            )
            .expect_err("infinite validator must fail");
        assert!(
            error.contains("CONFIG_VALIDATOR_TIMEOUT")
                || error.contains("CONFIG_VALIDATOR_LIMIT_EXCEEDED")
        );
        assert!(
            service
                .list_raw_entries(Some("demo-package"))
                .expect("list unchanged package")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&test_root);
    }

    /// Verify validator results reject sparse arrays and undeclared issue object fields.
    /// 验证校验器结果会拒绝稀疏数组与未声明的问题对象字段。
    #[test]
    fn business_validator_rejects_noncanonical_issue_shapes() {
        for (label, source, expected) in [
            (
                "sparse",
                r#"return function(values)
  return {
    [1] = {code = "first", message = "first issue"},
    [3] = {code = "third", message = "third issue"},
  }
end"#,
                "dense array",
            ),
            (
                "unknown_field",
                r#"return function(values)
  return {
    {code = "invalid_shape", message = "invalid issue", unexpected = true},
  }
end"#,
                "only key, code, and message",
            ),
        ] {
            let test_root = test_config_file(label);
            let _ = std::fs::remove_dir_all(&test_root);
            let skill_dir = test_root.join("package");
            let manifest = package_manifest_with_validator(&skill_dir, "demo-package", source);
            let service = SkillPackageConfigService::new(
                Some(test_root.join("config")),
                Duration::from_secs(5),
                Duration::from_millis(20),
            )
            .expect("create service");
            service.replace_registry([(&manifest, "USER", skill_dir.as_path())]);
            let error = service
                .status("demo-package")
                .expect_err("noncanonical validator issue shape must fail");
            assert!(error.contains("CONFIG_VALIDATOR_FAILED"));
            assert!(error.contains(expected), "unexpected error: {error}");
            let _ = std::fs::remove_dir_all(&test_root);
        }
    }

    /// Verify installed discovery reports shadowing and disable markers without executing Lua.
    /// 验证已安装发现无需执行 Lua 即可报告遮蔽与停用标记。
    #[test]
    fn installed_catalog_reports_physical_shadowing_and_disable_markers() {
        let test_root = test_config_file("installed_catalog");
        let _ = std::fs::remove_dir_all(&test_root);
        let project_root = test_root.join("project");
        let user_root = test_root.join("user");
        write_installed_manifest(&project_root.join("demo-package"), "1.0.0");
        write_installed_manifest(&user_root.join("demo-package"), "2.0.0");
        std::fs::create_dir_all(user_root.join("disabled-package"))
            .expect("create disable marker directory");
        let service = SkillPackageConfigService::new(
            Some(test_root.join("config")),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        service
            .replace_installed_catalog(&[
                RuntimeSkillRoot {
                    name: "PROJECT".to_string(),
                    skills_dir: project_root,
                },
                RuntimeSkillRoot {
                    name: "USER".to_string(),
                    skills_dir: user_root,
                },
            ])
            .expect("build installed catalog");
        let demo = service.describe_installed(Some("demo-package"), None);
        assert_eq!(demo.len(), 2);
        assert!(demo[0].effective);
        assert!(!demo[0].shadowed);
        assert!(demo[1].shadowed);
        assert!(!demo[1].effective);
        let disabled = service.describe_installed(Some("disabled-package"), None);
        assert_eq!(disabled.len(), 1);
        assert!(disabled[0].manifest_valid);
        assert!(!disabled[0].enabled);
        assert!(disabled[0].manifest_issue.is_none());
        let _ = std::fs::remove_dir_all(&test_root);
    }

    /// Verify committed mutations publish their events before returning durability failures.
    /// 验证已提交变更会先发布事件，再返回耐久化失败。
    #[test]
    fn post_commit_durability_errors_preserve_local_event_ordering() {
        // Effective package registry used to derive the concrete ordinary store scope.
        // 用于派生具体普通存储作用域的有效技能包注册表。
        let config_root = test_config_file("post_commit_event_ordering");
        let skill_dir = config_root.join("package");
        let service = SkillPackageConfigService::new(
            Some(config_root.clone()),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        let manifest = package_manifest(
            "demo-package",
            r#"
  - key: token
    type: string
    description: Service token.
"#,
        );
        service.replace_registry([(&manifest, "USER", skill_dir.as_path())]);
        let package = service.package("demo-package").expect("resolve package");
        // Stable simulated failure that occurs only after the file replacement committed.
        // 仅在文件替换提交后发生的稳定模拟失败。
        let durability_error =
            "CONFIG_ATOMIC_REPLACE_FAILED: failed to sync config directory".to_string();

        let write_error = service
            .finish_local_write(
                "demo-package",
                &package,
                SkillConfigWriteResult {
                    revision: "1".to_string(),
                    changed: true,
                    values: BTreeMap::from([("token".to_string(), "value".to_string())]),
                    changed_keys: vec!["token".to_string()],
                    durability_error: Some(durability_error.clone()),
                },
            )
            .expect_err("write durability failure must remain visible");
        assert_eq!(write_error, durability_error);
        let first = service
            .poll_events(None, 10)
            .expect("poll committed write event");
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].source, "local_write");
        assert_eq!(first.events[0].revision, "1");

        let delete_error = service
            .finish_local_delete(
                "demo-package",
                &package,
                SkillConfigDeleteResult {
                    revision: "2".to_string(),
                    deleted: true,
                    key: "token".to_string(),
                    durability_error: Some(durability_error.clone()),
                },
            )
            .expect_err("delete durability failure must remain visible");
        assert_eq!(delete_error, durability_error);
        let second = service
            .poll_events(Some(&first.next_sequence), 10)
            .expect("poll committed delete event");
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].revision, "2");
        let _ = std::fs::remove_dir_all(config_root);
    }

    /// Verify paginated event cursors advance only through events actually returned.
    /// 验证分页事件游标只前进到实际返回的事件。
    #[test]
    fn event_poll_cursor_does_not_skip_limited_events() {
        let queue = SkillConfigEventQueue::new();
        for revision in 1..=3 {
            queue.publish(RuntimeSkillConfigEvent {
                sequence: String::new(),
                event_type: "skill_config_changed".to_string(),
                store_scope: "skills".to_string(),
                skill_id: Some("demo-package".to_string()),
                revision: revision.to_string(),
                changed_keys: vec!["retry_count".to_string()],
                source: "local_write".to_string(),
                restart_required_keys: Vec::new(),
                complete: Some(true),
                error: None,
            });
        }
        let first = queue.poll(None, 2).expect("poll first event page");
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.next_sequence, "2");
        let second = queue
            .poll(Some(&first.next_sequence), 2)
            .expect("poll second event page");
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].sequence, "3");
        assert_eq!(second.next_sequence, "3");
    }

    /// Verify unfiltered raw listing disambiguates identical package keys across both stores.
    /// 验证未过滤原始列表可区分两个存储中的同名技能包配置键。
    #[test]
    fn raw_listing_includes_concrete_store_scope() {
        // Isolated root holding both ordinary and system raw records.
        // 同时保存普通与系统原始记录的隔离根目录。
        let config_root = test_config_file("raw_list_store_scope");
        let _ = std::fs::remove_dir_all(&config_root);
        // Service whose private stores are populated directly to model retained historical data.
        // 直接填充私有存储以模拟历史保留数据的服务。
        let service = SkillPackageConfigService::new(
            Some(config_root.clone()),
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .expect("create service");
        // Initialized store router shared by both retained records.
        // 两条保留记录共享的已初始化存储路由器。
        let stores = service.stores.as_ref().expect("resolve stores");
        stores
            .normal
            .set_values(
                "same-package",
                BTreeMap::from([("token".to_string(), "ordinary".to_string())]),
                None,
            )
            .expect("write ordinary raw record");
        stores
            .system
            .set_values(
                "same-package",
                BTreeMap::from([("token".to_string(), "system".to_string())]),
                None,
            )
            .expect("write system raw record");
        // Unfiltered result retaining the physical origin of both otherwise identical records.
        // 保留两条其余字段相同记录物理来源的未过滤结果。
        let entries = service
            .list_raw_entries(None)
            .expect("list both raw stores");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].store_scope, "skills");
        assert_eq!(entries[0].value, "ordinary");
        assert_eq!(entries[1].store_scope, "system-skills");
        assert_eq!(entries[1].value, "system");
        let _ = std::fs::remove_dir_all(&config_root);
    }

    /// Verify concurrent publishers retain exact sequence order in the shared queue.
    /// 验证并发发布者在共享队列中保持严格序号顺序。
    #[test]
    fn concurrent_event_publishers_preserve_cursor_order() {
        // Shared queue receiving events from every worker.
        // 接收所有工作线程事件的共享队列。
        let queue = Arc::new(SkillConfigEventQueue::new());
        // Publisher threads exercising sequence allocation and insertion concurrently.
        // 并发执行序号分配与入队的发布线程。
        let workers = (0..8)
            .map(|worker_index| {
                // Queue reference owned by the current publisher.
                // 当前发布线程持有的队列引用。
                let queue = Arc::clone(&queue);
                std::thread::spawn(move || {
                    for event_index in 0..100 {
                        queue.publish(RuntimeSkillConfigEvent {
                            sequence: String::new(),
                            event_type: "skill_config_changed".to_string(),
                            store_scope: "skills".to_string(),
                            skill_id: Some(format!("package-{worker_index}")),
                            revision: event_index.to_string(),
                            changed_keys: vec!["retry_count".to_string()],
                            source: "local_write".to_string(),
                            restart_required_keys: Vec::new(),
                            complete: Some(true),
                            error: None,
                        });
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("concurrent publisher should finish");
        }
        // Complete event batch used to verify retained cursor ordering.
        // 用于验证保留游标顺序的完整事件批次。
        let batch = queue
            .poll(None, 800)
            .expect("poll every concurrently published event");

        assert_eq!(batch.events.len(), 800);
        for (index, event) in batch.events.iter().enumerate() {
            assert_eq!(event.sequence, (index + 1).to_string());
        }
        assert_eq!(batch.next_sequence, "800");
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
        let service = SkillPackageConfigService::new(
            Some(config_file.clone()),
            Duration::from_secs(5),
            Duration::from_millis(200),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", Path::new("."))]);
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

        let _ = std::fs::remove_dir_all(&config_file);
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
        let service = SkillPackageConfigService::new(
            Some(config_file.clone()),
            Duration::from_secs(5),
            Duration::from_millis(200),
        )
        .expect("create service");
        service.replace_registry([(&manifest, "USER", Path::new("."))]);
        service
            .stores
            .as_ref()
            .expect("initialized stores")
            .normal
            .set_values(
                "demo-package",
                BTreeMap::from([("retries".to_string(), "99".to_string())]),
                None,
            )
            .expect("inject old invalid value");
        service
            .stores
            .as_ref()
            .expect("initialized stores")
            .normal
            .set_values(
                "demo-package",
                BTreeMap::from([("removed_key".to_string(), "legacy".to_string())]),
                None,
            )
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
                .delete_raw_value("demo-package", "removed_key", None)
                .expect("delete orphan through host path")
                .deleted
        );

        let _ = std::fs::remove_dir_all(&config_file);
    }

    /// Verify one empty package registry describes all effective packages in stable id order.
    /// 验证配置结构查询会按稳定标识顺序描述全部有效技能包。
    #[test]
    fn describe_all_packages_uses_stable_identifier_order() {
        let config_file = test_config_file("all_packages");
        let package_b = package_manifest("package-b", "  []");
        let package_a = package_manifest("package-a", "  []");
        let service = SkillPackageConfigService::new(
            Some(config_file.clone()),
            Duration::from_secs(5),
            Duration::from_millis(200),
        )
        .expect("create service");
        service.replace_registry([
            (&package_b, "USER", Path::new(".")),
            (&package_a, "USER", Path::new(".")),
        ]);
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

        let _ = std::fs::remove_dir_all(&config_file);
    }

    /// Verify a shared watcher startup failure is reported for both domains without becoming a service-construction error.
    /// 验证共享监听器启动失败会向两个配置域报告，同时不会升级为服务构造错误。
    #[test]
    fn watcher_start_failure_is_published_as_recoverable_domain_events() {
        // ConfigRoot provides two valid stores while the watcher failure itself is injected deterministically.
        // ConfigRoot 提供两个有效存储，同时以确定性方式注入监听器失败。
        let config_root = test_config_file("watcher_start_failure");
        // Stores model the already-usable persistence layer that must survive watcher unavailability.
        // Stores 模拟监听器不可用时仍必须保留的可用持久化层。
        let stores = SkillConfigStoreRouter::new(config_root.clone(), Duration::from_secs(5))
            .expect("create config stores");
        // Events receive the recoverable failure projections for both exact domains.
        // Events 接收两个精确配置域的可恢复失败投影。
        let events = SkillConfigEventQueue::new();

        let watcher = retain_config_watcher_or_publish_start_failure(
            Err("CONFIG_WATCHER_FAILED: injected startup failure".to_string()),
            &stores,
            &events,
        );

        assert!(watcher.is_none());
        // Batch proves both domains remain independently observable after one shared backend failure.
        // Batch 证明单个共享后端失败后两个配置域仍可被独立观察。
        let batch = events.poll(None, 10).expect("poll watcher failures");
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0].store_scope, "skills");
        assert_eq!(batch.events[1].store_scope, "system-skills");
        assert!(batch.events.iter().all(|event| {
            event.event_type == "skill_config_reload_failed"
                && event
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == "CONFIG_WATCHER_FAILED")
        }));

        let _cleanup_result = std::fs::remove_dir_all(config_root);
    }
}
