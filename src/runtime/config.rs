use crate::lua_skill::validate_luaskills_identifier;
#[cfg(windows)]
use crate::runtime::path::normalize_host_visible_path_text;
use crate::runtime::path::render_host_visible_path;
use crate::skill::config::{
    SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE, SKILL_CONFIG_MAX_VALUE_BYTES, is_valid_skill_config_key,
};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

/// Render one skill-config filesystem path for user-facing error messages.
/// 为面向用户的技能配置错误消息渲染单个文件系统路径。
fn render_skill_config_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// One flattened skill-config record exposed to hosts and FFI consumers.
/// 暴露给宿主与 FFI 消费方的单条扁平化技能配置记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillConfigEntry {
    /// Persisted store scope containing this raw record.
    /// 包含当前原始记录的持久化存储作用域。
    pub store_scope: String,
    /// Stable skill identifier that owns the current config key.
    /// 拥有当前配置键的稳定技能标识符。
    pub skill_id: String,
    /// Stable config key stored under the current skill namespace.
    /// 存放在当前技能命名空间下的稳定配置键。
    pub key: String,
    /// String config value stored for the current `(skill_id, key)` pair.
    /// 当前 `(skill_id, key)` 对应存储的字符串配置值。
    pub value: String,
}

/// Only persisted skill configuration format accepted by this release.
/// 当前版本唯一接受的持久化技能配置格式。
pub const SKILL_CONFIG_FORMAT_VERSION: u32 = 1;

/// Maximum number of skill package namespaces stored in one configuration document.
/// 单份配置文档允许存储的最大技能包命名空间数量。
pub const SKILL_CONFIG_MAX_PACKAGES_PER_DOCUMENT: usize = 10_000;

/// Maximum encoded byte size of one complete persisted configuration document.
/// 单份完整持久化配置文档允许的最大编码字节数。
pub const SKILL_CONFIG_MAX_DOCUMENT_BYTES: u64 = 64 * 1_024 * 1_024;

/// Maximum encoded byte size of one unified runtime-config tool response.
/// 单个统一 runtime-config 工具响应允许的最大编码字节数。
pub const SKILL_CONFIG_MAX_TOOL_RESPONSE_BYTES: usize = 64 * 1_024 * 1_024;

/// Default cross-process configuration lock timeout in milliseconds.
/// 默认配置跨进程锁超时毫秒数。
pub const SKILL_CONFIG_DEFAULT_LOCK_TIMEOUT_MS: u64 = 5_000;

/// Maximum cross-process configuration lock timeout in milliseconds.
/// 最大配置跨进程锁超时毫秒数。
pub const SKILL_CONFIG_MAX_LOCK_TIMEOUT_MS: u64 = 60_000;

/// Default test wait duration for acquiring one cross-process configuration lock.
/// 测试中获取单个跨进程配置锁的默认等待时长。
#[cfg(test)]
pub const SKILL_CONFIG_DEFAULT_LOCK_TIMEOUT: Duration =
    Duration::from_millis(SKILL_CONFIG_DEFAULT_LOCK_TIMEOUT_MS);

/// Maximum number of keys accepted by one atomic package configuration transaction.
/// 单个原子技能包配置事务允许接受的最大键数量。
pub const SKILL_CONFIG_MAX_BATCH_KEYS: usize = SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE;

/// One persisted skill-config document stored in the strict versioned config file.
/// 存储在严格版本化配置文件中的单个技能配置文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillConfigDocument {
    /// Exact persisted format version.
    /// 精确的持久化格式版本。
    format_version: u32,
    /// Monotonic document revision encoded as one decimal string.
    /// 编码为十进制字符串的单调文档修订号。
    #[serde(serialize_with = "revision_string::serialize")]
    revision: u64,
    /// Per-skill string key-value map grouped by stable skill identifiers.
    /// 按稳定技能标识符分组的每技能字符串键值映射。
    skills: BTreeMap<String, BTreeMap<String, String>>,
}

impl<'de> Deserialize<'de> for SkillConfigDocument {
    /// Deserialize the strict document while rejecting duplicate fields at every object level.
    /// 反序列化严格文档，并拒绝每一层对象中的重复字段。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SkillConfigDocumentVisitor)
    }
}

/// Strict serde visitor for the top-level configuration document.
/// 配置文档顶层对象使用的严格 serde 访问器。
struct SkillConfigDocumentVisitor;

impl<'de> Visitor<'de> for SkillConfigDocumentVisitor {
    type Value = SkillConfigDocument;

    /// Describe the only accepted top-level object shape.
    /// 描述唯一接受的顶层对象形态。
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a strict versioned skill configuration object")
    }

    /// Decode the object and reject unknown, missing, or duplicate fields.
    /// 解码对象，并拒绝未知、缺失或重复字段。
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut format_version = None;
        let mut revision = None;
        let mut skills = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "format_version" => {
                    if format_version.is_some() {
                        return Err(de::Error::duplicate_field("format_version"));
                    }
                    format_version = Some(map.next_value::<u32>()?);
                }
                "revision" => {
                    if revision.is_some() {
                        return Err(de::Error::duplicate_field("revision"));
                    }
                    let raw = map.next_value::<String>()?;
                    revision = Some(parse_revision_text(&raw).map_err(de::Error::custom)?);
                }
                "skills" => {
                    if skills.is_some() {
                        return Err(de::Error::duplicate_field("skills"));
                    }
                    skills = Some(map.next_value::<StrictSkillConfigNamespaces>()?.0);
                }
                _ => return Err(de::Error::unknown_field(&field, CONFIG_DOCUMENT_FIELDS)),
            }
        }
        Ok(SkillConfigDocument {
            format_version: format_version
                .ok_or_else(|| de::Error::missing_field("format_version"))?,
            revision: revision.ok_or_else(|| de::Error::missing_field("revision"))?,
            skills: skills.ok_or_else(|| de::Error::missing_field("skills"))?,
        })
    }
}

/// Exact top-level field names accepted by the strict document visitor.
/// 严格文档访问器接受的精确顶层字段名。
const CONFIG_DOCUMENT_FIELDS: &[&str] = &["format_version", "revision", "skills"];

/// Strict package namespace object that rejects duplicate skill identifiers.
/// 拒绝重复技能标识符的严格技能包命名空间对象。
struct StrictSkillConfigNamespaces(BTreeMap<String, BTreeMap<String, String>>);

impl<'de> Deserialize<'de> for StrictSkillConfigNamespaces {
    /// Deserialize all package namespaces with duplicate detection.
    /// 在检测重复项的同时反序列化全部技能包命名空间。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StrictSkillConfigNamespacesVisitor)
    }
}

/// Strict visitor for the package namespace map.
/// 技能包命名空间映射使用的严格访问器。
struct StrictSkillConfigNamespacesVisitor;

impl<'de> Visitor<'de> for StrictSkillConfigNamespacesVisitor {
    type Value = StrictSkillConfigNamespaces;

    /// Describe the required package namespace object.
    /// 描述所需的技能包命名空间对象。
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an object mapping skill identifiers to string maps")
    }

    /// Decode package namespaces and reject duplicate identifiers.
    /// 解码技能包命名空间并拒绝重复标识符。
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut skills = BTreeMap::new();
        while let Some(skill_id) = map.next_key::<String>()? {
            if skills.contains_key(&skill_id) {
                return Err(de::Error::custom(format!(
                    "duplicate skill configuration namespace '{}'",
                    skill_id
                )));
            }
            let values = map.next_value::<StrictSkillConfigValues>()?.0;
            skills.insert(skill_id, values);
        }
        Ok(StrictSkillConfigNamespaces(skills))
    }
}

/// Strict string value map that rejects duplicate configuration keys.
/// 拒绝重复配置键的严格字符串值映射。
struct StrictSkillConfigValues(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for StrictSkillConfigValues {
    /// Deserialize one package value map with duplicate detection.
    /// 在检测重复项的同时反序列化单个技能包值映射。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StrictSkillConfigValuesVisitor)
    }
}

/// Strict visitor for one package value map.
/// 单个技能包值映射使用的严格访问器。
struct StrictSkillConfigValuesVisitor;

impl<'de> Visitor<'de> for StrictSkillConfigValuesVisitor {
    type Value = StrictSkillConfigValues;

    /// Describe one string-to-string package configuration object.
    /// 描述单个字符串到字符串的技能包配置对象。
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an object mapping configuration keys to strings")
    }

    /// Decode one package map and reject duplicate keys.
    /// 解码单个技能包映射并拒绝重复键。
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate skill configuration key '{}'",
                    key
                )));
            }
            values.insert(key, map.next_value::<String>()?);
        }
        Ok(StrictSkillConfigValues(values))
    }
}

impl Default for SkillConfigDocument {
    /// Build the in-memory revision-zero document used before the first write.
    /// 构建首次写入前使用的内存 revision-zero 文档。
    fn default() -> Self {
        Self {
            format_version: SKILL_CONFIG_FORMAT_VERSION,
            revision: 0,
            skills: BTreeMap::new(),
        }
    }
}

/// Cached immutable document paired with the concrete file path that produced it.
/// 与生成它的具体文件路径配对的缓存不可变文档。
#[derive(Debug, Clone)]
struct SkillConfigSnapshot {
    /// Concrete normalized file path represented by this snapshot.
    /// 当前快照表示的具体规范化文件路径。
    file_path: PathBuf,
    /// SHA-256 digest of the exact file content, or the empty-state marker.
    /// 精确文件内容或空状态标记的 SHA-256 摘要。
    content_digest: String,
    /// Last known valid document.
    /// 最后一个已知合法文档。
    document: SkillConfigDocument,
}

/// Outcome produced after the destination file has already been atomically replaced.
/// 目标文件已经完成原子替换后产生的提交结果。
struct SkillConfigCommit {
    /// SHA-256 digest of the exact committed file content.
    /// 已提交文件精确内容的 SHA-256 摘要。
    content_digest: String,
    /// Optional post-commit durability failure that must still be surfaced to the caller.
    /// 仍须向调用方报告的可选提交后耐久化失败。
    durability_error: Option<String>,
}

/// Result of one explicit or watcher-triggered configuration refresh.
/// 单次显式或监听触发配置刷新的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillConfigRefreshResult {
    /// Revision visible after the refresh attempt.
    /// 刷新尝试完成后可见的修订号。
    pub revision: String,
    /// Whether a newer external document replaced the cached snapshot.
    /// 是否有更新的外部文档替换了缓存快照。
    pub changed: bool,
    /// Changed keys grouped by skill package for an accepted external reload.
    /// 对已接受外部重载按技能包分组的变更键。
    pub changes: BTreeMap<String, Vec<String>>,
}

/// Result of one atomic package configuration write.
/// 单次原子技能包配置写入的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillConfigWriteResult {
    /// Revision visible after the transaction.
    /// 事务完成后可见的修订号。
    pub revision: String,
    /// Whether the transaction changed persisted data.
    /// 当前事务是否改变了持久化数据。
    pub changed: bool,
    /// Canonical persisted values submitted by this transaction.
    /// 当前事务提交的规范持久化值。
    pub values: BTreeMap<String, String>,
    /// Stable keys whose persisted values changed.
    /// 持久化值发生变化的稳定键。
    pub changed_keys: Vec<String>,
    /// Post-commit durability failure retained only for the internal service event ordering.
    /// 仅为内部服务事件排序保留的提交后耐久化失败。
    #[serde(skip)]
    pub(crate) durability_error: Option<String>,
}

/// Result of one atomic single-key package configuration deletion.
/// 单次原子技能包配置单键删除结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillConfigDeleteResult {
    /// Revision visible after the deletion transaction.
    /// 删除事务完成后可见的修订号。
    pub revision: String,
    /// Whether one persisted value was removed.
    /// 是否移除了一个持久化值。
    pub deleted: bool,
    /// Exact stable key targeted by the transaction.
    /// 当前事务定位的精确稳定键。
    pub key: String,
    /// Post-commit durability failure retained only for the internal service event ordering.
    /// 仅为内部服务事件排序保留的提交后耐久化失败。
    #[serde(skip)]
    pub(crate) durability_error: Option<String>,
}

/// Shared store that owns one concrete versioned skill configuration file.
/// 拥有一个具体版本化技能配置文件的共享存储。
#[derive(Debug)]
pub struct SkillConfigStore {
    /// Absolute configuration file path injected by the host-owned store router.
    /// 由宿主所有的存储路由器注入的绝对配置文件路径。
    file_path: PathBuf,
    /// Last known valid immutable snapshot used by all ordinary reads.
    /// 所有常规读取使用的最后一个合法不可变快照。
    snapshot: RwLock<Option<SkillConfigSnapshot>>,
    /// Maximum duration allowed for acquiring the cross-process lock.
    /// 获取跨进程锁允许的最大时长。
    lock_timeout: Duration,
}

/// Live parent-directory watcher that debounces events for one exact configuration file.
/// 对单个精确配置文件事件执行防抖的活动父目录监听器。
pub(crate) struct SkillConfigReloadWatcher {
    /// Native watcher retained for the complete lifetime of the worker.
    /// 在工作线程完整生命周期内保留的原生监听器。
    _watcher: RecommendedWatcher,
    /// Cooperative shutdown signal observed by the worker.
    /// 工作线程观察的协作停止信号。
    stop: Arc<AtomicBool>,
    /// Debounce worker that owns refresh sequencing.
    /// 拥有刷新排序逻辑的防抖工作线程。
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SkillConfigReloadWatcher {
    /// Render watcher diagnostics without exposing backend internals.
    /// 在不暴露后端内部状态的情况下渲染监听器诊断信息。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SkillConfigReloadWatcher")
            .field("stopped", &self.stop.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SkillConfigReloadWatcher {
    /// Start one exact-file watcher and route accepted or failed reload attempts to a callback.
    /// 启动单个精确文件监听器，并把接受或失败的重载尝试路由到回调。
    pub(crate) fn start(
        store: Arc<SkillConfigStore>,
        debounce: Duration,
        callback: Arc<dyn Fn(Result<SkillConfigRefreshResult, String>) + Send + Sync>,
    ) -> Result<Self, String> {
        if debounce.is_zero() || debounce > Duration::from_secs(5) {
            return Err(
                "CONFIG_WATCHER_FAILED: skill_config_watch_debounce_ms must be between 1 and 5000"
                    .to_string(),
            );
        }
        let file_path = store.file_path()?;
        let parent = file_path.parent().ok_or_else(|| {
            "CONFIG_WATCHER_FAILED: configuration file has no parent directory".to_string()
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "CONFIG_WATCHER_FAILED: failed to create configuration watch directory '{}': {}",
                render_skill_config_path(parent),
                error
            )
        })?;
        let (event_tx, event_rx) = mpsc::channel::<Result<Event, notify::Error>>();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _send_result = event_tx.send(event);
        })
        .map_err(|error| format!("CONFIG_WATCHER_FAILED: {error}"))?;
        watcher
            .watch(parent, RecursiveMode::NonRecursive)
            .map_err(|error| format!("CONFIG_WATCHER_FAILED: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_file_path = file_path;
        let worker = thread::Builder::new()
            .name("luaskills-config-watch".to_string())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match event_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Ok(event)) if config_event_targets_file(&event, &worker_file_path) => {
                            let mut backend_error = None;
                            let started_at = Instant::now();
                            let maximum_window = std::cmp::max(debounce, Duration::from_secs(2))
                                .min(Duration::from_secs(5));
                            let maximum_deadline = started_at + maximum_window;
                            let mut quiet_deadline =
                                std::cmp::min(started_at + debounce, maximum_deadline);
                            loop {
                                if worker_stop.load(Ordering::Acquire) {
                                    return;
                                }
                                let now = Instant::now();
                                if now >= quiet_deadline || now >= maximum_deadline {
                                    break;
                                }
                                let wait = quiet_deadline
                                    .saturating_duration_since(now)
                                    .min(Duration::from_millis(100));
                                match event_rx.recv_timeout(wait) {
                                    Ok(Ok(next))
                                        if config_event_targets_file(&next, &worker_file_path) =>
                                    {
                                        quiet_deadline = std::cmp::min(
                                            Instant::now() + debounce,
                                            maximum_deadline,
                                        );
                                    }
                                    Ok(Ok(_)) => {}
                                    Ok(Err(error)) => {
                                        backend_error = Some(error.to_string());
                                    }
                                    Err(RecvTimeoutError::Timeout) => {}
                                    Err(RecvTimeoutError::Disconnected) => return,
                                }
                            }
                            if let Some(error) = backend_error {
                                callback(Err(format!("CONFIG_WATCHER_FAILED: {error}")));
                            } else {
                                callback(store.refresh());
                            }
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            callback(Err(format!("CONFIG_WATCHER_FAILED: {error}")));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|error| format!("CONFIG_WATCHER_FAILED: {error}"))?;
        Ok(Self {
            _watcher: watcher,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for SkillConfigReloadWatcher {
    /// Stop and join the debounce worker before releasing the native watcher.
    /// 在释放原生监听器前停止并回收防抖工作线程。
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _join_result = worker.join();
        }
    }
}

/// Return whether one native event references the exact watched configuration file.
/// 返回单个原生事件是否引用精确的被监听配置文件。
fn config_event_targets_file(event: &Event, file_path: &Path) -> bool {
    event
        .paths
        .iter()
        .any(|path| skill_config_paths_match(path, file_path))
}

/// Compare one watcher path with the absolute target using host filesystem semantics.
/// 使用宿主文件系统语义比较一条监听路径与绝对目标。
fn skill_config_paths_match(candidate: &Path, target: &Path) -> bool {
    let candidate = match std::path::absolute(candidate) {
        Ok(candidate) => candidate,
        Err(_) => return false,
    };
    #[cfg(windows)]
    {
        normalize_host_visible_path_text(&candidate.to_string_lossy()).to_lowercase()
            == normalize_host_visible_path_text(&target.to_string_lossy()).to_lowercase()
    }
    #[cfg(not(windows))]
    {
        candidate == target
    }
}

impl SkillConfigStore {
    /// Create one skill-config store from one explicit file path.
    /// 基于一条显式文件路径创建技能配置存储。
    #[cfg(test)]
    pub fn new(file_path: PathBuf) -> Result<Self, String> {
        Self::with_lock_timeout(file_path, SKILL_CONFIG_DEFAULT_LOCK_TIMEOUT)
    }

    /// Create one skill-config store with an explicit cross-process lock timeout.
    /// 使用显式跨进程锁超时创建技能配置存储。
    pub fn with_lock_timeout(file_path: PathBuf, lock_timeout: Duration) -> Result<Self, String> {
        if lock_timeout.is_zero() || lock_timeout > Duration::from_secs(60) {
            return Err(
                "CONFIG_PATH_INVALID: lock timeout must be between 1 millisecond and 60 seconds"
                    .to_string(),
            );
        }
        let file_path = resolve_explicit_skill_config_file_path(&file_path)?;
        let store = Self {
            file_path: file_path.clone(),
            snapshot: RwLock::new(None),
            lock_timeout,
        };
        let snapshot = store.read_snapshot_from(&file_path)?;
        *store.lock_snapshot_write() = Some(snapshot);
        Ok(store)
    }

    /// Return the concrete skill-config file path.
    /// 返回具体技能配置文件路径。
    pub fn file_path(&self) -> Result<PathBuf, String> {
        Ok(self.file_path.clone())
    }

    /// List flattened config records for one optional skill namespace.
    /// 列出某个可选技能命名空间下的扁平化配置记录。
    pub fn list_entries(
        &self,
        store_scope: &str,
        skill_id: Option<&str>,
    ) -> Result<Vec<SkillConfigEntry>, String> {
        let document = self.with_document_read(|document| Ok(document.clone()))?;
        match skill_id {
            Some(skill_id) => {
                let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
                let store_scope = store_scope.to_string();
                Ok(document
                    .skills
                    .get(&normalized_skill_id)
                    .into_iter()
                    .flat_map(|items| {
                        items.iter().map(|(key, value)| SkillConfigEntry {
                            store_scope: store_scope.clone(),
                            skill_id: normalized_skill_id.clone(),
                            key: key.clone(),
                            value: value.clone(),
                        })
                    })
                    .collect())
            }
            None => {
                let store_scope = store_scope.to_string();
                Ok(document
                    .skills
                    .iter()
                    .flat_map(|(skill_id, items)| {
                        let store_scope = store_scope.clone();
                        items.iter().map(move |(key, value)| SkillConfigEntry {
                            store_scope: store_scope.clone(),
                            skill_id: skill_id.clone(),
                            key: key.clone(),
                            value: value.clone(),
                        })
                    })
                    .collect())
            }
        }
    }

    /// List the complete key-value map owned by one skill namespace.
    /// 列出某个技能命名空间拥有的完整键值映射。
    pub fn list_skill_values(&self, skill_id: &str) -> Result<BTreeMap<String, String>, String> {
        self.skill_values_snapshot(skill_id)
            .map(|(_, values)| values)
    }

    /// Read one package value map and its revision from the same immutable snapshot.
    /// 从同一不可变快照读取一个技能包值映射及其修订号。
    pub fn skill_values_snapshot(
        &self,
        skill_id: &str,
    ) -> Result<(String, BTreeMap<String, String>), String> {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        self.with_document_read(|document| {
            Ok((
                document.revision.to_string(),
                document
                    .skills
                    .get(&normalized_skill_id)
                    .cloned()
                    .unwrap_or_default(),
            ))
        })
    }

    /// Read one string config value stored under one `(skill_id, key)` pair.
    /// 读取某个 `(skill_id, key)` 对下存储的单个字符串配置值。
    pub fn get_value(&self, skill_id: &str, key: &str) -> Result<Option<String>, String> {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        let normalized_key = validate_skill_config_key(key)?;
        self.with_document_read(|document| {
            Ok(document
                .skills
                .get(&normalized_skill_id)
                .and_then(|items| items.get(&normalized_key))
                .cloned())
        })
    }

    /// Insert one value through the batch implementation for store-level tests.
    /// 在存储级测试中通过批量实现写入一个值。
    #[cfg(test)]
    fn set_value(&self, skill_id: &str, key: &str, value: &str) -> Result<(), String> {
        self.set_values(
            skill_id,
            BTreeMap::from([(key.to_string(), value.to_string())]),
            None,
        )
        .map(|_| ())
    }

    /// Atomically insert or replace multiple values owned by one skill package.
    /// 原子插入或替换单个技能包拥有的多个值。
    #[cfg(test)]
    pub fn set_values(
        &self,
        skill_id: &str,
        values: BTreeMap<String, String>,
        expected_revision: Option<&str>,
    ) -> Result<SkillConfigWriteResult, String> {
        self.set_values_validated(skill_id, values, expected_revision, |_| Ok(()))
    }

    /// Atomically write one package batch after validating the complete latest candidate map.
    /// 在校验完整最新候选映射后原子写入一个技能包批次。
    pub(crate) fn set_values_validated<F>(
        &self,
        skill_id: &str,
        values: BTreeMap<String, String>,
        expected_revision: Option<&str>,
        validate_candidate: F,
    ) -> Result<SkillConfigWriteResult, String>
    where
        F: FnOnce(&BTreeMap<String, String>) -> Result<(), String>,
    {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        if values.is_empty() {
            return Err("CONFIG_BATCH_EMPTY: configuration batch must not be empty".to_string());
        }
        if values.len() > SKILL_CONFIG_MAX_BATCH_KEYS {
            return Err(format!(
                "CONFIG_BATCH_TOO_LARGE: configuration batch contains {} keys, exceeding the hard limit {}",
                values.len(),
                SKILL_CONFIG_MAX_BATCH_KEYS
            ));
        }
        let mut normalized_values = BTreeMap::new();
        for (key, value) in values {
            let normalized_key = validate_skill_config_key(&key)?;
            if value.len() > SKILL_CONFIG_MAX_VALUE_BYTES {
                return Err(format!(
                    "CONFIG_VALUE_TOO_LONG: configuration '{}' UTF-8 byte length exceeds the hard limit {}",
                    normalized_key, SKILL_CONFIG_MAX_VALUE_BYTES
                ));
            }
            normalized_values.insert(normalized_key, value);
        }
        let expected_revision = expected_revision.map(parse_expected_revision).transpose()?;
        let submitted_values = normalized_values.clone();
        let (changed_keys, revision, changed, durability_error) =
            self.with_document_mut(expected_revision, |document| {
                let package_values = document
                    .skills
                    .entry(normalized_skill_id.clone())
                    .or_default();
                let mut changed_keys = Vec::new();
                for (key, value) in &normalized_values {
                    if package_values.get(key) != Some(value) {
                        package_values.insert(key.clone(), value.clone());
                        changed_keys.push(key.clone());
                    }
                }
                validate_candidate(package_values)?;
                Ok((changed_keys.clone(), !changed_keys.is_empty()))
            })?;
        Ok(SkillConfigWriteResult {
            revision: revision.to_string(),
            changed,
            values: submitted_values,
            changed_keys,
            durability_error,
        })
    }

    /// Delete one config key under one skill namespace and report whether one value was removed.
    /// 删除某个技能命名空间下的单个配置键，并返回是否移除了一个值。
    pub fn delete_value(
        &self,
        skill_id: &str,
        key: &str,
        expected_revision: Option<&str>,
    ) -> Result<SkillConfigDeleteResult, String> {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        let normalized_key = validate_skill_config_key(key)?;
        let expected_revision = expected_revision.map(parse_expected_revision).transpose()?;
        let deleted_key = normalized_key.clone();
        let (deleted, revision, _, durability_error) =
            self.with_document_mut(expected_revision, |document| {
                let deleted = document
                    .skills
                    .get_mut(&normalized_skill_id)
                    .and_then(|items| items.remove(&normalized_key))
                    .is_some();
                if let Some(items) = document.skills.get(&normalized_skill_id)
                    && items.is_empty()
                {
                    document.skills.remove(&normalized_skill_id);
                }
                Ok((deleted, deleted))
            })?;
        Ok(SkillConfigDeleteResult {
            revision: revision.to_string(),
            deleted,
            key: deleted_key,
            durability_error,
        })
    }

    /// Return the revision of the current last-known-valid snapshot.
    /// 返回当前最后合法快照的修订号。
    pub fn revision(&self) -> Result<String, String> {
        self.with_document_read(|document| Ok(document.revision.to_string()))
    }

    /// Explicitly reload the persisted file and replace the read cache when it is valid.
    /// 显式重新加载持久化文件，并在合法时替换读取缓存。
    pub fn refresh(&self) -> Result<SkillConfigRefreshResult, String> {
        let file_path = self.file_path()?;
        let path_lock = shared_skill_config_path_lock(&file_path)?;
        let _path_guard = lock_shared_skill_config_path(&path_lock);
        let candidate = self.read_snapshot_from(&file_path)?;
        let mut guard = self.lock_snapshot_write();
        let current = guard.as_ref().ok_or_else(|| {
            "CONFIG_SNAPSHOT_UNAVAILABLE: configuration cache is empty".to_string()
        })?;
        validate_snapshot_progression(current, &candidate, "external")?;
        if candidate.document.revision == current.document.revision {
            return Ok(SkillConfigRefreshResult {
                revision: candidate.document.revision.to_string(),
                changed: false,
                changes: BTreeMap::new(),
            });
        }
        let changes = changed_skill_config_keys(&current.document, &candidate.document);
        let revision = candidate.document.revision.to_string();
        *guard = Some(candidate);
        Ok(SkillConfigRefreshResult {
            revision,
            changed: true,
            changes,
        })
    }

    /// Execute one read-only operation against the last-known-valid immutable snapshot.
    /// 针对最后合法的不可变快照执行一次只读操作。
    fn with_document_read<T, F>(&self, action: F) -> Result<T, String>
    where
        F: FnOnce(&SkillConfigDocument) -> Result<T, String>,
    {
        let file_path = self.file_path()?;
        if let Some(snapshot) = self.lock_snapshot_read().as_ref()
            && snapshot.file_path == file_path
        {
            return action(&snapshot.document);
        }
        let path_lock = shared_skill_config_path_lock(&file_path)?;
        let _path_guard = lock_shared_skill_config_path(&path_lock);
        let snapshot = self.read_snapshot_from(&file_path)?;
        let mut guard = self.lock_snapshot_write();
        if let Some(current) = guard.as_ref() {
            validate_snapshot_progression(current, &snapshot, "disk")?;
        }
        let document = snapshot.document.clone();
        *guard = Some(snapshot);
        action(&document)
    }

    /// Execute one locked read-modify-write transaction against the latest disk revision.
    /// 针对磁盘最新修订执行一次加锁读改写事务。
    fn with_document_mut<T, F>(
        &self,
        expected_revision: Option<u64>,
        action: F,
    ) -> Result<(T, u64, bool, Option<String>), String>
    where
        F: FnOnce(&mut SkillConfigDocument) -> Result<(T, bool), String>,
    {
        let file_path = self.file_path()?;
        let path_lock = shared_skill_config_path_lock(&file_path)?;
        let _path_guard = lock_shared_skill_config_path(&path_lock);
        let lock_file = acquire_cross_process_config_lock(&file_path, self.lock_timeout)?;
        let disk_snapshot = self.read_snapshot_from(&file_path)?;
        {
            let guard = self.lock_snapshot_read();
            let current = guard.as_ref().ok_or_else(|| {
                "CONFIG_SNAPSHOT_UNAVAILABLE: configuration cache is empty".to_string()
            })?;
            validate_snapshot_progression(current, &disk_snapshot, "disk")?;
        }
        let disk_content_digest = disk_snapshot.content_digest;
        let mut document = disk_snapshot.document;
        if let Some(expected_revision) = expected_revision
            && document.revision != expected_revision
        {
            return Err(format!(
                "CONFIG_REVISION_CONFLICT: expected revision {} but disk revision is {}",
                expected_revision, document.revision
            ));
        }
        let (result, changed) = action(&mut document)?;
        let (content_digest, durability_error) = if changed {
            document.revision = document.revision.checked_add(1).ok_or_else(|| {
                "CONFIG_REVISION_EXHAUSTED: configuration revision cannot be incremented"
                    .to_string()
            })?;
            let commit = self.write_document_to(&file_path, &document)?;
            (commit.content_digest, commit.durability_error)
        } else {
            (disk_content_digest, None)
        };
        let revision = document.revision;
        let committed_snapshot = SkillConfigSnapshot {
            file_path,
            content_digest,
            document,
        };
        self.install_committed_snapshot(committed_snapshot);
        drop(lock_file);
        Ok((result, revision, changed, durability_error))
    }

    /// Install the exact committed snapshot before the service handles post-commit outcomes.
    /// 在服务处理提交后结果之前安装精确的已提交快照。
    fn install_committed_snapshot(&self, snapshot: SkillConfigSnapshot) {
        *self.lock_snapshot_write() = Some(snapshot);
    }

    /// Acquire the immutable snapshot cache for reading and recover from poisoning.
    /// 获取不可变快照缓存读锁，并在 poison 后恢复。
    fn lock_snapshot_read(&self) -> RwLockReadGuard<'_, Option<SkillConfigSnapshot>> {
        self.snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquire the immutable snapshot cache for writing and recover from poisoning.
    /// 获取不可变快照缓存写锁，并在 poison 后恢复。
    fn lock_snapshot_write(&self) -> RwLockWriteGuard<'_, Option<SkillConfigSnapshot>> {
        self.snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Load one complete valid snapshot, treating a missing file as revision-zero empty state.
    /// 加载一个完整合法快照，并把缺失文件视为 revision-zero 空状态。
    fn read_snapshot_from(&self, file_path: &Path) -> Result<SkillConfigSnapshot, String> {
        if !skill_config_file_is_file(file_path)? {
            return Ok(SkillConfigSnapshot {
                file_path: file_path.to_path_buf(),
                content_digest: "missing".to_string(),
                document: SkillConfigDocument::default(),
            });
        }
        let metadata = fs::metadata(file_path).map_err(|error| {
            format!(
                "CONFIG_PATH_UNAVAILABLE: failed to inspect skill config file '{}': {}",
                render_skill_config_path(file_path),
                error
            )
        })?;
        if metadata.len() > SKILL_CONFIG_MAX_DOCUMENT_BYTES {
            return Err(format!(
                "CONFIG_FILE_TOO_LARGE: skill config file '{}' contains {} bytes, exceeding the hard limit {}",
                render_skill_config_path(file_path),
                metadata.len(),
                SKILL_CONFIG_MAX_DOCUMENT_BYTES
            ));
        }
        let bytes = fs::read(file_path).map_err(|error| {
            format!(
                "CONFIG_PATH_UNAVAILABLE: failed to read skill config file '{}': {}",
                render_skill_config_path(file_path),
                error
            )
        })?;
        let document = serde_json::from_slice::<SkillConfigDocument>(&bytes).map_err(|error| {
            format!(
                "CONFIG_FORMAT_INVALID: failed to parse skill config file '{}': {}",
                render_skill_config_path(file_path),
                error
            )
        })?;
        validate_skill_config_document(&document)?;
        Ok(SkillConfigSnapshot {
            file_path: file_path.to_path_buf(),
            content_digest: hex_sha256(&bytes),
            document,
        })
    }

    /// Persist one complete document with one temp-file write followed by one replacement rename.
    /// 通过“先写临时文件再替换重命名”的方式持久化整份文档。
    fn write_document_to(
        &self,
        file_path: &Path,
        document: &SkillConfigDocument,
    ) -> Result<SkillConfigCommit, String> {
        validate_skill_config_document(document)?;
        let parent = file_path.parent().ok_or_else(|| {
            format!(
                "CONFIG_PATH_INVALID: skill config file '{}' has no parent directory",
                render_skill_config_path(file_path)
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "CONFIG_PATH_UNAVAILABLE: failed to create skill config directory '{}': {}",
                render_skill_config_path(parent),
                error
            )
        })?;
        let mut serialized = serde_json::to_vec_pretty(document).map_err(|error| {
            format!(
                "CONFIG_ATOMIC_REPLACE_FAILED: failed to serialize skill config document: {}",
                error
            )
        })?;
        serialized.push(b'\n');
        if serialized.len() as u64 > SKILL_CONFIG_MAX_DOCUMENT_BYTES {
            return Err(format!(
                "CONFIG_FILE_TOO_LARGE: serialized skill config document contains {} bytes, exceeding the hard limit {}",
                serialized.len(),
                SKILL_CONFIG_MAX_DOCUMENT_BYTES
            ));
        }
        let temp_path = unique_skill_config_temp_path(file_path)?;
        let write_result = (|| -> Result<(), String> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .map_err(|error| {
                    format!(
                        "CONFIG_ATOMIC_REPLACE_FAILED: failed to create skill config temp file '{}': {}",
                        render_skill_config_path(&temp_path),
                        error
                    )
                })?;
            file.write_all(&serialized).map_err(|error| {
                format!(
                    "CONFIG_ATOMIC_REPLACE_FAILED: failed to write skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
            file.flush().map_err(|error| {
                format!(
                    "CONFIG_ATOMIC_REPLACE_FAILED: failed to flush skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "CONFIG_ATOMIC_REPLACE_FAILED: failed to sync skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _cleanup_result = fs::remove_file(&temp_path);
            return Err(error);
        }
        replace_file_atomically(&temp_path, file_path).map_err(|error| {
            let _cleanup_result = fs::remove_file(&temp_path);
            format!(
                "CONFIG_ATOMIC_REPLACE_FAILED: failed to promote skill config temp file '{}' to '{}': {}",
                render_skill_config_path(&temp_path),
                render_skill_config_path(file_path),
                error
            )
        })?;
        let content_digest = hex_sha256(&serialized);
        let durability_error = sync_skill_config_parent(parent).err();
        Ok(SkillConfigCommit {
            content_digest,
            durability_error,
        })
    }
}

/// Reject a candidate snapshot that regresses or rewrites one observed revision.
/// 拒绝回退修订号或改写已观察修订号的候选快照。
///
/// `source` identifies the candidate origin in value-safe diagnostics.
/// `source` 在不包含配置值的诊断中标识候选来源。
fn validate_snapshot_progression(
    current: &SkillConfigSnapshot,
    candidate: &SkillConfigSnapshot,
    source: &str,
) -> Result<(), String> {
    if candidate.document.revision < current.document.revision {
        return Err(format!(
            "CONFIG_REVISION_REGRESSION: {} revision {} is older than cached revision {}",
            source, candidate.document.revision, current.document.revision
        ));
    }
    if candidate.document.revision == current.document.revision
        && candidate.content_digest != current.content_digest
    {
        return Err(format!(
            "CONFIG_REVISION_CONFLICT: {} revision {} content differs from the cached snapshot",
            source, candidate.document.revision
        ));
    }
    Ok(())
}

/// Compute one lowercase SHA-256 digest for exact persisted bytes.
/// 为精确持久化字节计算一个小写 SHA-256 摘要。
fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

/// Compare two valid documents and return every changed key grouped by package.
/// 比较两份合法文档，并按技能包返回所有发生变化的键。
fn changed_skill_config_keys(
    before: &SkillConfigDocument,
    after: &SkillConfigDocument,
) -> BTreeMap<String, Vec<String>> {
    let skill_ids = before
        .skills
        .keys()
        .chain(after.skills.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut changes = BTreeMap::new();
    for skill_id in skill_ids {
        let before_values = before.skills.get(&skill_id);
        let after_values = after.skills.get(&skill_id);
        let keys = before_values
            .into_iter()
            .flat_map(BTreeMap::keys)
            .chain(after_values.into_iter().flat_map(BTreeMap::keys))
            .cloned()
            .collect::<BTreeSet<_>>();
        let changed = keys
            .into_iter()
            .filter(|key| {
                before_values.and_then(|values| values.get(key))
                    != after_values.and_then(|values| values.get(key))
            })
            .collect::<Vec<_>>();
        if !changed.is_empty() {
            changes.insert(skill_id, changed);
        }
    }
    changes
}

/// Serialize configuration revisions as strict decimal strings.
/// 把配置修订号序列化为严格十进制字符串。
mod revision_string {
    use serde::Serializer;

    /// Serialize one revision without exposing it as a JSON number.
    /// 序列化单个修订号，同时避免把它暴露为 JSON 数字。
    pub(super) fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }
}

/// Parse one host-provided expected revision.
/// 解析一个宿主提供的预期修订号。
fn parse_expected_revision(value: &str) -> Result<u64, String> {
    parse_revision_text(value)
        .map_err(|error| format!("CONFIG_REVISION_INVALID: expected_revision {}", error))
}

/// Parse one strict unsigned decimal revision string.
/// 解析一个严格的无符号十进制修订字符串。
fn parse_revision_text(value: &str) -> Result<u64, String> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err("must be a canonical unsigned decimal string".to_string());
    }
    value
        .parse::<u64>()
        .map_err(|error| format!("is outside the unsigned 64-bit range: {}", error))
}

/// Validate one complete persisted configuration document before it reaches the cache.
/// 在完整持久化配置文档进入缓存前校验它。
fn validate_skill_config_document(document: &SkillConfigDocument) -> Result<(), String> {
    if document.format_version != SKILL_CONFIG_FORMAT_VERSION {
        return Err(format!(
            "CONFIG_FORMAT_VERSION_UNSUPPORTED: expected format_version {} but found {}",
            SKILL_CONFIG_FORMAT_VERSION, document.format_version
        ));
    }
    if document.skills.len() > SKILL_CONFIG_MAX_PACKAGES_PER_DOCUMENT {
        return Err(format!(
            "CONFIG_FILE_TOO_LARGE: configuration document contains {} skill packages, exceeding the hard limit {}",
            document.skills.len(),
            SKILL_CONFIG_MAX_PACKAGES_PER_DOCUMENT
        ));
    }
    if document.revision == 0 && !document.skills.is_empty() {
        return Err(
            "CONFIG_REVISION_INVALID: revision 0 is reserved for an empty unwritten document"
                .to_string(),
        );
    }
    for (skill_id, values) in &document.skills {
        validate_skill_config_skill_id(skill_id)?;
        if values.is_empty() {
            return Err(format!(
                "CONFIG_FORMAT_INVALID: skill package '{}' must not persist an empty object",
                skill_id
            ));
        }
        if values.len() > SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE {
            return Err(format!(
                "CONFIG_FILE_TOO_LARGE: skill package '{}' contains {} values, exceeding the hard limit {}",
                skill_id,
                values.len(),
                SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE
            ));
        }
        for (key, value) in values {
            validate_skill_config_key(key)?;
            if value.len() > SKILL_CONFIG_MAX_VALUE_BYTES {
                return Err(format!(
                    "CONFIG_VALUE_TOO_LONG: skill package '{}' configuration '{}' exceeds the hard limit {} UTF-8 bytes",
                    skill_id, key, SKILL_CONFIG_MAX_VALUE_BYTES
                ));
            }
        }
    }
    Ok(())
}

/// Open and exclusively lock the stable companion lock file for one configuration document.
/// 打开并独占锁定单个配置文档的稳定伴随锁文件。
fn acquire_cross_process_config_lock(file_path: &Path, timeout: Duration) -> Result<File, String> {
    let parent = file_path.parent().ok_or_else(|| {
        format!(
            "CONFIG_PATH_INVALID: skill config file '{}' has no parent directory",
            render_skill_config_path(file_path)
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "CONFIG_PATH_UNAVAILABLE: failed to create skill config directory '{}': {}",
            render_skill_config_path(parent),
            error
        )
    })?;
    let lock_path = skill_config_companion_lock_path(file_path)?;
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "CONFIG_LOCK_FAILED: failed to open companion lock file '{}': {}",
                render_skill_config_path(&lock_path),
                error
            )
        })?;
    let started_at = Instant::now();
    loop {
        match lock_file.try_lock() {
            Ok(()) => return Ok(lock_file),
            Err(TryLockError::WouldBlock) => {
                let elapsed = started_at.elapsed();
                let Some(delay) = skill_config_lock_retry_delay(timeout, elapsed) else {
                    return Err(format!(
                        "CONFIG_LOCK_TIMEOUT: timed out after {} milliseconds waiting for '{}'",
                        timeout.as_millis(),
                        render_skill_config_path(&lock_path)
                    ));
                };
                thread::sleep(delay);
            }
            Err(TryLockError::Error(error)) => {
                return Err(format!(
                    "CONFIG_LOCK_FAILED: failed to lock '{}': {}",
                    render_skill_config_path(&lock_path),
                    error
                ));
            }
        }
    }
}

/// Return the bounded next lock retry delay, or none after the configured deadline.
/// 返回有界的下一次锁重试等待时间；超过配置期限后返回空。
fn skill_config_lock_retry_delay(timeout: Duration, elapsed: Duration) -> Option<Duration> {
    timeout
        .checked_sub(elapsed)
        .map(|remaining| Duration::from_millis(25).min(remaining))
        .filter(|delay| !delay.is_zero())
}

/// Derive the stable companion lock-file path without locking the replaceable data file.
/// 派生稳定伴随锁文件路径，同时避免锁定可被替换的数据文件。
fn skill_config_companion_lock_path(file_path: &Path) -> Result<PathBuf, String> {
    let file_name = file_path.file_name().ok_or_else(|| {
        format!(
            "CONFIG_PATH_INVALID: skill config file '{}' has no file name",
            render_skill_config_path(file_path)
        )
    })?;
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    Ok(file_path.with_file_name(lock_name))
}

/// Build one process-unique same-directory temporary path for atomic replacement.
/// 为原子替换构建一个进程唯一的同目录临时路径。
fn unique_skill_config_temp_path(file_path: &Path) -> Result<PathBuf, String> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    static STARTUP_FACTOR: OnceLock<Result<u128, String>> = OnceLock::new();
    let file_name = file_path.file_name().ok_or_else(|| {
        format!(
            "CONFIG_PATH_INVALID: skill config file '{}' has no file name",
            render_skill_config_path(file_path)
        )
    })?;
    let startup_factor = STARTUP_FACTOR
        .get_or_init(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .map_err(|error| {
                    format!(
                        "CONFIG_ATOMIC_REPLACE_FAILED: failed to derive config temp-file factor: {error}"
                    )
                })
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = file_name.to_os_string();
    temp_name.push(format!(
        ".{}.{}.{}.tmp",
        std::process::id(),
        *startup_factor,
        counter
    ));
    Ok(file_path.with_file_name(temp_name))
}

/// Synchronize the containing directory after one atomic configuration replacement.
/// 在完成一次配置原子替换后同步其父目录。
fn sync_skill_config_parent(parent: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!(
                    "CONFIG_ATOMIC_REPLACE_FAILED: failed to sync config directory '{}': {}",
                    render_skill_config_path(parent),
                    error
                )
            })?;
    }
    #[cfg(not(unix))]
    let _parent = parent;
    Ok(())
}

/// Inspect whether one skill-config file path is a file without hiding filesystem metadata errors.
/// 检查单个技能配置文件路径是否为文件，同时不隐藏文件系统元数据错误。
///
/// The file_path parameter is the effective persisted skill-config file path.
/// file_path 参数是生效的持久化技能配置文件路径。
///
/// Return true for an existing config file, false for a confirmed missing config file, or an explicit probe/type error.
/// 已存在配置文件返回 true，确认缺失配置文件返回 false；探测或类型异常时返回显式错误。
fn skill_config_file_is_file(file_path: &Path) -> Result<bool, String> {
    match fs::metadata(file_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "CONFIG_PATH_INVALID: skill config file is not a file '{}'",
            render_skill_config_path(file_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "CONFIG_PATH_UNAVAILABLE: failed to inspect skill config file '{}': {}",
            render_skill_config_path(file_path),
            error
        )),
    }
}

/// Return the process-wide lock registry keyed by effective skill-config file path.
/// 返回按生效技能配置文件路径建立索引的进程级锁注册表。
fn skill_config_lock_registry() -> &'static Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Acquire the process-wide skill-config lock registry and return its guard, recovering after lock poisoning.
/// 获取并返回进程级 skill-config 锁注册表保护对象；如果锁已 poison，则恢复继续使用。
fn lock_skill_config_lock_registry() -> MutexGuard<'static, BTreeMap<PathBuf, Arc<Mutex<()>>>> {
    skill_config_lock_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Resolve one stable lock key from one effective skill-config file path.
/// 基于单个生效技能配置文件路径解析稳定锁键。
fn skill_config_lock_key(file_path: &Path) -> Result<PathBuf, String> {
    let resolved_path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(file_path))
            .map_err(|error| {
                format!(
                    "CONFIG_PATH_UNAVAILABLE: failed to resolve current directory for skill config lock: {}",
                    error
                )
            })?
    };
    // Keep lexical normalization separate from platform identity normalization so each boundary can fail explicitly.
    // 将词法规整与平台身份规整分开，确保每个边界都可以显式失败。
    let normalized_path = normalize_skill_config_lock_path(&resolved_path);
    normalize_skill_config_lock_identity_path(&normalized_path)
}

/// Resolve one explicit host-provided skill-config file path into one fixed absolute path.
/// 将单个宿主显式提供的技能配置文件路径解析成固定的绝对路径。
fn resolve_explicit_skill_config_file_path(file_path: &Path) -> Result<PathBuf, String> {
    if !file_path.is_absolute() {
        return Err(format!(
            "CONFIG_PATH_INVALID: skill config file path '{}' must be absolute",
            render_skill_config_path(file_path)
        ));
    }
    Ok(normalize_skill_config_lock_path(file_path))
}

/// Normalize one skill-config lock path with stable lexical component folding.
/// 使用稳定的词法组件折叠规则规范化单个技能配置锁路径。
fn normalize_skill_config_lock_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut can_pop_normal = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
                can_pop_normal = false;
            }
            Component::RootDir => {
                normalized.push(component.as_os_str());
                can_pop_normal = false;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if can_pop_normal && normalized.pop() {
                    can_pop_normal = !matches!(
                        normalized.components().next_back(),
                        Some(Component::Prefix(_)) | Some(Component::RootDir) | None
                    );
                } else if !path.is_absolute() {
                    normalized.push(component.as_os_str());
                    can_pop_normal = false;
                }
            }
            Component::Normal(part) => {
                normalized.push(part);
                can_pop_normal = true;
            }
        }
    }
    normalized
}

/// Normalize one lexically folded lock path into one platform-stable lock identity.
/// 将一个已完成词法规整的锁路径进一步规范为平台稳定的锁标识。
///
/// The path parameter is the lexically normalized effective skill-config file path.
/// path 参数是已经完成词法规整的生效技能配置文件路径。
///
/// Return the path identity used as the process-wide lock-registry key.
/// 返回用作进程级锁注册表键的路径身份。
fn normalize_skill_config_lock_identity_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        normalize_windows_skill_config_lock_identity_path(path)
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

/// Normalize one Windows lock path so case aliases and drive/UNC verbatim forms share one identity.
/// 规范化单个 Windows 锁路径，使大小写别名与盘符/UNC verbatim 形式共享同一标识。
///
/// The path parameter is the lexically normalized Windows skill-config file path.
/// path 参数是已经完成词法规整的 Windows 技能配置文件路径。
///
/// Return the Windows-normalized lock identity or an explicit error for non-UTF-8 path text.
/// 返回 Windows 归一化后的锁身份；如果路径文本不是有效 UTF-8，则返回显式错误。
#[cfg(windows)]
fn normalize_windows_skill_config_lock_identity_path(path: &Path) -> Result<PathBuf, String> {
    // UTF-8 Windows spelling required by the case-insensitive process-local lock identity.
    // 进程内大小写不敏感锁身份所需的 UTF-8 Windows 路径形式。
    let rendered = path.to_str().ok_or_else(|| {
        "CONFIG_PATH_INVALID: skill config lock path must be valid UTF-8 on Windows".to_string()
    })?;
    // Shared host-visible spelling covering drive, UNC, case, and separator variants uniformly.
    // 统一覆盖盘符、UNC、大小写与分隔符变体的共享宿主可见形式。
    let without_verbatim = normalize_host_visible_path_text(rendered);
    Ok(PathBuf::from(without_verbatim.to_lowercase()))
}

/// Return one process-wide shared mutex for the current effective skill-config file path.
/// 返回当前生效技能配置文件路径对应的进程级共享互斥锁。
fn shared_skill_config_path_lock(file_path: &Path) -> Result<Arc<Mutex<()>>, String> {
    let lock_key = skill_config_lock_key(file_path)?;
    let mut registry = lock_skill_config_lock_registry();
    Ok(registry
        .entry(lock_key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

/// Acquire one shared skill-config file IO lock and return its guard, recovering after lock poisoning.
/// 获取并返回单个共享 skill-config 文件 IO 锁保护对象；如果锁已 poison，则恢复继续使用。
fn lock_shared_skill_config_path(path_lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    path_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Validate one skill identifier used by the unified config store.
/// 校验统一配置存储使用的单个技能标识符。
fn validate_skill_config_skill_id(skill_id: &str) -> Result<String, String> {
    if skill_id.trim() != skill_id {
        return Err(
            "CONFIG_PACKAGE_NOT_FOUND: skill_id must not contain surrounding whitespace"
                .to_string(),
        );
    }
    validate_luaskills_identifier(skill_id, "skill_id")
        .map(|_| skill_id.to_string())
        .map_err(|error| format!("CONFIG_PACKAGE_NOT_FOUND: {}", error))
}

/// Validate one config key used inside one skill namespace.
/// 校验技能命名空间内使用的单个配置键。
fn validate_skill_config_key(key: &str) -> Result<String, String> {
    if !is_valid_skill_config_key(key) {
        return Err(format!(
            "CONFIG_KEY_INVALID: configuration key '{}' does not satisfy the strict package key contract",
            key
        ));
    }
    Ok(key.to_string())
}

/// Replace one destination file with one temp file using one platform-safe atomic commit strategy.
/// 使用平台安全的原子提交策略，以临时文件替换目标文件。
fn replace_file_atomically(
    temp_path: &Path,
    destination_path: &Path,
) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        // Destination existence probe kept explicit so metadata errors are not folded into a rename attempt.
        // 显式探测目标文件是否存在，避免将元数据错误折叠成一次重命名尝试。
        let destination_exists = destination_path.try_exists()?;
        if !destination_exists {
            return fs::rename(temp_path, destination_path);
        }

        let destination_wide: Vec<u16> = destination_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let temp_wide: Vec<u16> = temp_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let replaced = unsafe {
            ReplaceFileW(
                destination_wide.as_ptr(),
                temp_wide.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp_path, destination_path)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::normalize_windows_skill_config_lock_identity_path;
    use super::{
        SKILL_CONFIG_FORMAT_VERSION, SkillConfigDocument, SkillConfigEntry,
        SkillConfigReloadWatcher, SkillConfigSnapshot, SkillConfigStore,
        shared_skill_config_path_lock, skill_config_companion_lock_path,
        skill_config_lock_registry, skill_config_lock_retry_delay,
    };
    use crate::runtime::path::render_host_visible_path;
    use std::collections::BTreeMap;
    use std::fs::{self, OpenOptions};
    use std::panic::{self, AssertUnwindSafe};
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::{Child, Command};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Instant;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Create one unique temporary runtime root used by config-store tests.
    /// 创建一个供配置存储测试使用的唯一临时运行时根目录。
    fn unique_temp_runtime_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("luaskills_skill_config_{}_{}", label, nonce))
    }

    /// Verify config values persist inside one explicit unified file path.
    /// 验证配置值会持久化到单个显式统一文件路径中。
    #[test]
    fn skill_config_store_persists_values_in_explicit_file() {
        let runtime_root = unique_temp_runtime_root("persist");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path.clone()).expect("create explicit store");
        store
            .set_value("demo-skill", "api_token", "sk-123")
            .expect("set config value");
        assert_eq!(
            store
                .get_value("demo-skill", "api_token")
                .expect("get config value"),
            Some("sk-123".to_string())
        );
        assert!(file_path.exists());
        let reloaded = SkillConfigStore::new(file_path).expect("create reloaded explicit store");
        assert_eq!(
            reloaded
                .get_value("demo-skill", "api_token")
                .expect("reload config value"),
            Some("sk-123".to_string())
        );
    }

    /// Verify skill-config parse errors render paths through the host-visible formatter.
    /// 验证技能配置解析错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn skill_config_parse_error_uses_host_visible_path() {
        // Runtime root that isolates the invalid config file fixture.
        // 隔离非法配置文件夹具的运行时根目录。
        let runtime_root = unique_temp_runtime_root("parse_error_path");
        // Explicit config file path used by the store.
        // 配置存储使用的显式配置文件路径。
        let file_path = runtime_root.join("custom").join("skill_config.json");
        // Parent directory created before writing the invalid JSON file.
        // 写入非法 JSON 文件前创建的父目录。
        let parent = file_path
            .parent()
            .expect("config file path should have a parent");
        fs::create_dir_all(parent).expect("config parent should be created");
        fs::write(&file_path, "{not-json").expect("invalid config file should be written");
        // Error returned by strict initialization before configuration becomes available.
        // 配置能力可用前由严格初始化返回的错误。
        let error =
            SkillConfigStore::new(file_path.clone()).expect_err("invalid config JSON should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "CONFIG_FORMAT_INVALID: failed to parse skill config file '{}':",
            render_host_visible_path(&file_path)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify config file path probe errors fail instead of behaving like missing files.
    /// 验证配置文件路径探测错误会失败，而不是表现得像文件缺失。
    ///
    /// This test has no parameters and fails through assertions when path metadata errors are hidden.
    /// 本测试不接收参数；当路径元数据错误被隐藏时会通过断言失败。
    ///
    /// Return unit after validating the read path reports a file-inspection diagnostic.
    /// 校验读取路径会报告文件探测诊断后返回 unit。
    #[test]
    fn skill_config_store_reports_file_path_probe_errors() {
        // Runtime root used only to build one deterministic invalid config path.
        // 仅用于构造确定性非法配置路径的运行时根目录。
        let runtime_root = unique_temp_runtime_root("probe_error_path");
        // Explicit config file path containing one embedded NUL that filesystem metadata cannot inspect.
        // 包含一个内嵌 NUL 的显式配置文件路径，文件系统元数据无法探测该路径。
        let file_path = runtime_root.join("custom").join("skill_config\0.json");
        // Error returned by initialization before the invalid path can behave like an absent file.
        // 在非法路径表现得像缺失文件之前由初始化返回的错误。
        let error = SkillConfigStore::new(file_path.clone())
            .expect_err("invalid config path probe should fail");

        assert!(error.starts_with("CONFIG_PATH_UNAVAILABLE:"), "{error}");
        assert!(error.contains("failed to inspect skill config file"));
        assert!(
            error.contains("skill_config"),
            "unexpected probe error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify config file paths that exist as directories fail before JSON reading.
    /// 验证以目录形式存在的配置文件路径会在 JSON 读取前失败。
    #[test]
    fn skill_config_store_rejects_directory_config_file() {
        // Runtime root that isolates the directory-backed config file fixture.
        // 隔离目录型配置文件夹具的运行时根目录。
        let runtime_root = unique_temp_runtime_root("directory_config_file");
        // Explicit config file path that must be a regular JSON file.
        // 必须是普通 JSON 文件的显式配置文件路径。
        let file_path = runtime_root.join("custom").join("skill_config.json");
        fs::create_dir_all(&file_path).expect("directory config file path should be created");
        // Error returned before the directory can be passed to file reading.
        // 在目录被传递给文件读取前返回的错误。
        let error = SkillConfigStore::new(file_path.clone())
            .expect_err("directory config file should fail");

        assert!(error.starts_with("CONFIG_PATH_INVALID:"), "{error}");
        assert!(
            error.contains("skill config file is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&file_path)),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify the store returns flattened records for hosts that need one cross-skill management view.
    /// 验证存储会为需要跨技能管理视图的宿主返回扁平化记录列表。
    #[test]
    fn skill_config_store_lists_flattened_entries() {
        let runtime_root = unique_temp_runtime_root("list");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path).expect("create flattened-list store");
        store
            .set_value("alpha-skill", "api_token", "alpha-token")
            .expect("set alpha token");
        store
            .set_value("beta-skill", "endpoint", "https://example.test")
            .expect("set beta endpoint");
        assert_eq!(
            store.list_entries("skills", None).expect("list entries"),
            vec![
                SkillConfigEntry {
                    store_scope: "skills".to_string(),
                    skill_id: "alpha-skill".to_string(),
                    key: "api_token".to_string(),
                    value: "alpha-token".to_string(),
                },
                SkillConfigEntry {
                    store_scope: "skills".to_string(),
                    skill_id: "beta-skill".to_string(),
                    key: "endpoint".to_string(),
                    value: "https://example.test".to_string(),
                },
            ]
        );
    }

    /// Verify the store exposes one per-skill key-value map for Lua `vulcan.config.list()`.
    /// 验证存储会为 Lua `vulcan.config.list()` 暴露单个技能级键值映射。
    #[test]
    fn skill_config_store_lists_one_skill_value_map() {
        let runtime_root = unique_temp_runtime_root("skill_map");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path).expect("create skill-map store");
        store
            .set_value("demo-skill", "api_token", "sk-123")
            .expect("set api token");
        store
            .set_value("demo-skill", "endpoint", "https://example.test")
            .expect("set endpoint");
        let mut expected = BTreeMap::new();
        expected.insert("api_token".to_string(), "sk-123".to_string());
        expected.insert("endpoint".to_string(), "https://example.test".to_string());
        assert_eq!(
            store
                .list_skill_values("demo-skill")
                .expect("list one skill values"),
            expected
        );
    }

    /// Verify deleting one config key removes the value and prunes an empty skill namespace.
    /// 验证删除单个配置键会移除对应值并清理空技能命名空间。
    #[test]
    fn skill_config_store_delete_prunes_empty_skill_namespace() {
        let runtime_root = unique_temp_runtime_root("delete");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path.clone()).expect("create delete store");
        store
            .set_value("demo-skill", "api_token", "sk-123")
            .expect("set api token");
        assert!(
            store
                .delete_value("demo-skill", "api_token", None)
                .expect("delete api token")
                .deleted
        );
        assert_eq!(
            store
                .get_value("demo-skill", "api_token")
                .expect("read deleted value"),
            None
        );
        let persisted =
            fs::read_to_string(file_path).expect("skill config file should still be readable");
        assert_eq!(
            persisted.trim(),
            "{\n  \"format_version\": 1,\n  \"revision\": \"2\",\n  \"skills\": {}\n}"
        );
    }

    /// Verify stores that target the same config file path share one process-wide IO lock.
    /// 验证指向同一配置文件路径的存储会共享同一把进程级 IO 锁。
    #[test]
    fn skill_config_store_uses_process_wide_lock_per_effective_path() {
        let runtime_root = unique_temp_runtime_root("shared_lock");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let first_lock =
            shared_skill_config_path_lock(&file_path).expect("resolve first shared lock");
        let second_lock =
            shared_skill_config_path_lock(&file_path).expect("resolve second shared lock");
        assert!(Arc::ptr_eq(&first_lock, &second_lock));
    }

    /// Verify one relative explicit config file path is rejected.
    /// 验证单个相对显式配置文件路径会被拒绝。
    #[test]
    fn skill_config_store_rejects_relative_explicit_path() {
        let relative_path = PathBuf::from("config").join("skill_config.json");
        let error = SkillConfigStore::new(relative_path)
            .expect_err("relative explicit-path store must fail");
        assert!(error.contains("must be absolute"), "{error}");
    }

    /// Verify lexically equivalent config-file paths reuse the same shared lock.
    /// 验证词法等价的配置文件路径会复用同一把共享锁。
    #[test]
    fn skill_config_store_normalizes_equivalent_paths_for_shared_lock() {
        let runtime_root = unique_temp_runtime_root("shared_lock_normalized");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let alias_path = runtime_root
            .join("custom")
            .join(".")
            .join("child")
            .join("..")
            .join("skill_config.json");
        let first_lock =
            shared_skill_config_path_lock(&file_path).expect("resolve canonical shared lock");
        let second_lock =
            shared_skill_config_path_lock(&alias_path).expect("resolve alias shared lock");
        assert!(Arc::ptr_eq(&first_lock, &second_lock));
    }

    /// Verify the process-wide skill-config lock registry recovers after poisoning.
    /// 验证进程级 skill-config 锁注册表 poison 后仍可恢复。
    #[test]
    fn skill_config_lock_registry_recovers_after_poisoned_lock() {
        // Config file path used to request a shared lock after registry poisoning.
        // 注册表 poison 后用于请求共享锁的配置文件路径。
        let file_path = unique_temp_runtime_root("lock_registry_poison")
            .join("custom")
            .join("skill_config.json");

        // Captured panic result from a writer that poisons the global lock registry.
        // 全局锁注册表写入者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the process-wide skill-config lock registry.
            // 仅用于制造进程级 skill-config 锁注册表 poison 的保护对象。
            let _guard = skill_config_lock_registry()
                .lock()
                .expect("initial skill config lock registry");
            panic!("poison skill config lock registry for recovery test");
        }));

        assert!(poison_result.is_err());
        let first_lock =
            shared_skill_config_path_lock(&file_path).expect("resolve shared lock after poison");
        let second_lock =
            shared_skill_config_path_lock(&file_path).expect("resolve second shared lock");
        assert!(Arc::ptr_eq(&first_lock, &second_lock));
    }

    /// Verify the per-file skill-config IO lock recovers after poisoning.
    /// 验证单文件 skill-config IO 锁 poison 后仍可恢复。
    #[test]
    fn skill_config_shared_io_lock_recovers_after_poisoned_lock() {
        // Config file path whose shared IO lock is intentionally poisoned for this test.
        // 本测试中会被故意 poison 共享 IO 锁的配置文件路径。
        let file_path = unique_temp_runtime_root("shared_io_poison")
            .join("custom")
            .join("skill_config.json");
        // Store that writes to the poisoned per-file IO lock.
        // 写入已 poison 单文件 IO 锁的配置存储。
        let store =
            SkillConfigStore::new(file_path.clone()).expect("create shared-io poison store");
        // Shared IO lock resolved before poisoning.
        // poison 前解析出的共享 IO 锁。
        let path_lock = shared_skill_config_path_lock(&file_path).expect("resolve shared io lock");

        // Captured panic result from an IO actor that poisons the shared file lock.
        // 共享文件 IO 执行者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the per-file skill-config IO lock.
            // 仅用于制造单文件 skill-config IO 锁 poison 的保护对象。
            let _guard = path_lock.lock().expect("initial shared io lock");
            panic!("poison shared skill config io lock for recovery test");
        }));

        assert!(poison_result.is_err());
        store
            .set_value("demo-skill", "api_token", "sk-recovered")
            .expect("write config after shared io poison");
        assert_eq!(
            store
                .get_value("demo-skill", "api_token")
                .expect("read config after shared io poison"),
            Some("sk-recovered".to_string())
        );
    }

    /// Verify the store rejects the unreleased unversioned configuration shape.
    /// 验证存储拒绝未发布的无版本配置结构。
    #[test]
    fn skill_config_store_rejects_unversioned_documents() {
        let runtime_root = unique_temp_runtime_root("unversioned");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        fs::create_dir_all(file_path.parent().expect("config parent"))
            .expect("create config parent");
        fs::write(
            &file_path,
            r#"{"skills":{"demo-skill":{"api_token":"legacy"}}}"#,
        )
        .expect("write unversioned document");
        let error = SkillConfigStore::new(file_path).expect_err("unversioned documents must fail");
        assert!(error.contains("CONFIG_FORMAT_INVALID"));
        assert!(error.contains("format_version"));
    }

    /// Verify one batch increments revision once and compare-and-swap rejects stale writers.
    /// 验证单个批次只增加一次修订号，且比较并交换会拒绝过期写入者。
    #[test]
    fn skill_config_batch_write_is_atomic_and_revision_guarded() {
        let runtime_root = unique_temp_runtime_root("batch_revision");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path).expect("create batch store");
        let values = BTreeMap::from([
            ("api_token".to_string(), "sk-123".to_string()),
            ("retry_count".to_string(), "3".to_string()),
        ]);
        let first = store
            .set_values("demo-skill", values, Some("0"))
            .expect("write first guarded batch");
        assert_eq!(first.revision, "1");
        assert_eq!(first.changed_keys, vec!["api_token", "retry_count"]);
        let error = store
            .set_values(
                "demo-skill",
                BTreeMap::from([("retry_count".to_string(), "4".to_string())]),
                Some("0"),
            )
            .expect_err("stale revision must fail");
        assert!(error.contains("CONFIG_REVISION_CONFLICT"));
        assert_eq!(
            store
                .get_value("demo-skill", "retry_count")
                .expect("read unchanged guarded value"),
            Some("3".to_string())
        );
    }

    /// Verify ordinary reads use the cache until one explicit valid refresh replaces it.
    /// 验证常规读取持续使用缓存，直到一次显式合法刷新替换它。
    #[test]
    fn skill_config_reads_use_last_valid_snapshot_until_refresh() {
        let runtime_root = unique_temp_runtime_root("snapshot_refresh");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path.clone()).expect("create cache store");
        store
            .set_value("demo-skill", "retry_count", "3")
            .expect("write initial cache value");
        fs::write(
            &file_path,
            "{\n  \"format_version\": 1,\n  \"revision\": \"2\",\n  \"skills\": {\n    \"demo-skill\": {\n      \"retry_count\": \"4\"\n    }\n  }\n}\n",
        )
        .expect("replace config externally");
        assert_eq!(
            store
                .get_value("demo-skill", "retry_count")
                .expect("read cached value"),
            Some("3".to_string())
        );
        assert_eq!(
            store.refresh().expect("refresh valid document").revision,
            "2"
        );
        assert_eq!(
            store
                .get_value("demo-skill", "retry_count")
                .expect("read refreshed value"),
            Some("4".to_string())
        );
        fs::write(&file_path, "{invalid").expect("write invalid external document");
        assert!(store.refresh().is_err());
        assert_eq!(
            store
                .get_value("demo-skill", "retry_count")
                .expect("retain last valid value"),
            Some("4".to_string())
        );
    }

    /// Verify a post-commit durability error cannot leave the process on its previous snapshot.
    /// 验证提交后耐久化错误不会让当前进程继续停留在旧快照。
    #[test]
    fn post_commit_durability_error_keeps_committed_snapshot_visible() {
        // Isolated unwritten store whose cache initially contains revision zero.
        // 缓存初始处于零修订的隔离未写入存储。
        let runtime_root = unique_temp_runtime_root("post_commit_durability");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path.clone()).expect("create cache store");
        // Exact document that models bytes already promoted to the destination file.
        // 模拟已经提升到目标文件的精确文档。
        let committed_document = SkillConfigDocument {
            format_version: SKILL_CONFIG_FORMAT_VERSION,
            revision: 1,
            skills: BTreeMap::from([(
                "demo-skill".to_string(),
                BTreeMap::from([("retry_count".to_string(), "4".to_string())]),
            )]),
        };
        store.install_committed_snapshot(SkillConfigSnapshot {
            file_path,
            content_digest: "committed-digest".to_string(),
            document: committed_document,
        });
        assert_eq!(store.revision().expect("read committed revision"), "1");
        assert_eq!(
            store
                .get_value("demo-skill", "retry_count")
                .expect("read committed cached value"),
            Some("4".to_string())
        );
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify writes refuse externally rewritten or regressed disk snapshots.
    /// 验证写入会拒绝被外部改写或回退的磁盘快照。
    #[test]
    fn skill_config_write_rejects_disk_snapshot_rewrite_and_regression() {
        let runtime_root = unique_temp_runtime_root("write_snapshot_progression");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(file_path.clone()).expect("create cache store");
        store
            .set_value("demo-skill", "retry_count", "3")
            .expect("write initial cache value");

        fs::write(
            &file_path,
            "{\n  \"format_version\": 1,\n  \"revision\": \"1\",\n  \"skills\": {\n    \"demo-skill\": {\n      \"retry_count\": \"4\"\n    }\n  }\n}\n",
        )
        .expect("rewrite current revision externally");
        let conflict = store
            .set_value("demo-skill", "retry_count", "5")
            .expect_err("rewritten current revision must fail");
        assert!(conflict.contains("CONFIG_REVISION_CONFLICT"));

        fs::write(
            &file_path,
            "{\n  \"format_version\": 1,\n  \"revision\": \"0\",\n  \"skills\": {}\n}\n",
        )
        .expect("regress disk revision externally");
        let regression = store
            .set_value("demo-skill", "retry_count", "5")
            .expect_err("regressed disk revision must fail");
        assert!(regression.contains("CONFIG_REVISION_REGRESSION"));
        assert_eq!(
            store
                .get_value("demo-skill", "retry_count")
                .expect("retain last valid cached value"),
            Some("3".to_string())
        );
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify the parent-directory watcher accepts one atomic external replacement.
    /// 验证父目录监听器会接受一次外部原子替换。
    #[test]
    fn watcher_reloads_external_atomic_replacement() {
        let runtime_root = unique_temp_runtime_root("watcher_external_replace");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let watched_store = Arc::new(
            SkillConfigStore::new(file_path.clone()).expect("create watched config store"),
        );
        let writer_store =
            SkillConfigStore::new(file_path.clone()).expect("create external writer store");
        let (result_tx, result_rx) = mpsc::channel();
        let callback = Arc::new(move |result| {
            let _send_result = result_tx.send(result);
        });
        let watcher = SkillConfigReloadWatcher::start(
            Arc::clone(&watched_store),
            Duration::from_millis(25),
            callback,
        )
        .expect("start config watcher");

        writer_store
            .set_values(
                "demo-skill",
                BTreeMap::from([("retry_count".to_string(), "3".to_string())]),
                Some("0"),
            )
            .expect("write external atomic replacement");
        let refresh = result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("watcher should report external replacement")
            .expect("external replacement should reload");
        assert!(refresh.changed);
        assert_eq!(refresh.revision, "1");
        assert_eq!(
            watched_store
                .get_value("demo-skill", "retry_count")
                .expect("read reloaded value"),
            Some("3".to_string())
        );

        drop(watcher);
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify strict persisted decoding rejects duplicate package and value keys.
    /// 验证严格持久化解码会拒绝重复技能包键与配置键。
    #[test]
    fn persisted_document_rejects_duplicate_object_keys() {
        let runtime_root = unique_temp_runtime_root("duplicate_keys");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        fs::create_dir_all(file_path.parent().expect("config parent"))
            .expect("create duplicate-key test directory");
        fs::write(
            &file_path,
            r#"{"format_version":1,"revision":"1","skills":{"demo-skill":{"token":"a","token":"b"}}}"#,
        )
        .expect("write duplicate config key document");
        let value_error =
            SkillConfigStore::new(file_path.clone()).expect_err("duplicate config keys must fail");
        assert!(value_error.contains("duplicate skill configuration key"));

        fs::write(
            &file_path,
            r#"{"format_version":1,"revision":"1","skills":{"demo-skill":{"token":"a"},"demo-skill":{"token":"b"}}}"#,
        )
        .expect("write duplicate package key document");
        let package_error =
            SkillConfigStore::new(file_path).expect_err("duplicate package keys must fail");
        assert!(package_error.contains("duplicate skill configuration namespace"));
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify the stable companion file provides a bounded cross-handle lock wait.
    /// 验证稳定伴随文件提供有界的跨句柄锁等待。
    #[test]
    fn skill_config_write_times_out_while_companion_lock_is_held() {
        assert_eq!(
            skill_config_lock_retry_delay(Duration::from_millis(1), Duration::ZERO),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            skill_config_lock_retry_delay(Duration::from_millis(50), Duration::from_millis(20)),
            Some(Duration::from_millis(25))
        );
        assert_eq!(
            skill_config_lock_retry_delay(Duration::from_millis(50), Duration::from_millis(50)),
            None
        );
        let runtime_root = unique_temp_runtime_root("cross_process_lock");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        fs::create_dir_all(file_path.parent().expect("config parent"))
            .expect("create config parent");
        let lock_path =
            skill_config_companion_lock_path(&file_path).expect("resolve companion lock path");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .expect("open companion lock");
        lock_file.lock().expect("hold companion lock");
        let store = SkillConfigStore::with_lock_timeout(file_path, Duration::from_millis(50))
            .expect("create bounded lock store");
        let error = store
            .set_value("demo-skill", "retry_count", "3")
            .expect_err("held lock must time out");
        assert!(error.contains("CONFIG_LOCK_TIMEOUT"));
    }

    /// Hold one companion lock when launched as the isolated child process for the parent test.
    /// 作为父测试的隔离子进程启动时持有一个伴随锁。
    #[test]
    fn skill_config_cross_process_lock_holder() {
        let Some(lock_path) = std::env::var_os("LUASKILLS_CONFIG_LOCK_CHILD_PATH") else {
            return;
        };
        let ready_path = std::env::var_os("LUASKILLS_CONFIG_LOCK_CHILD_READY")
            .expect("child ready path must accompany the child lock path");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .expect("child opens companion lock");
        lock_file.lock().expect("child holds companion lock");
        fs::write(ready_path, b"ready").expect("child publishes lock readiness");
        thread::sleep(Duration::from_secs(2));
    }

    /// Execute one isolated child-process configuration write for concurrency tests.
    /// 为并发测试执行一次隔离子进程配置写入。
    #[test]
    fn skill_config_cross_process_writer() {
        let Some(file_path) = std::env::var_os("LUASKILLS_CONFIG_WRITE_CHILD_PATH") else {
            return;
        };
        let skill_id = std::env::var("LUASKILLS_CONFIG_WRITE_CHILD_SKILL")
            .expect("child skill id must accompany the config path");
        let key = std::env::var("LUASKILLS_CONFIG_WRITE_CHILD_KEY")
            .expect("child key must accompany the config path");
        let value = std::env::var("LUASKILLS_CONFIG_WRITE_CHILD_VALUE")
            .expect("child value must accompany the config path");
        let expected_revision = std::env::var("LUASKILLS_CONFIG_WRITE_CHILD_EXPECTED")
            .ok()
            .filter(|value| !value.is_empty());
        let result_path = std::env::var_os("LUASKILLS_CONFIG_WRITE_CHILD_RESULT")
            .expect("child result path must accompany the config path");
        let store = SkillConfigStore::new(PathBuf::from(file_path))
            .expect("child creates configuration store");
        let result = store.set_values(
            &skill_id,
            BTreeMap::from([(key, value)]),
            expected_revision.as_deref(),
        );
        let result_text = match result {
            Ok(result) => format!("ok:{}", result.revision),
            Err(error) => format!("error:{error}"),
        };
        fs::write(result_path, result_text).expect("child writes configuration result");
    }

    /// Spawn one isolated configuration writer using the current test binary.
    /// 使用当前测试二进制启动一个隔离配置写入进程。
    fn spawn_config_writer_child(
        file_path: &Path,
        key: &str,
        value: &str,
        expected_revision: Option<&str>,
        result_path: &Path,
    ) -> Child {
        let mut command =
            Command::new(std::env::current_exe().expect("resolve current test binary"));
        command
            .arg("--exact")
            .arg("runtime::config::tests::skill_config_cross_process_writer")
            .arg("--nocapture")
            .env("LUASKILLS_CONFIG_WRITE_CHILD_PATH", file_path)
            .env("LUASKILLS_CONFIG_WRITE_CHILD_SKILL", "demo-skill")
            .env("LUASKILLS_CONFIG_WRITE_CHILD_KEY", key)
            .env("LUASKILLS_CONFIG_WRITE_CHILD_VALUE", value)
            .env("LUASKILLS_CONFIG_WRITE_CHILD_RESULT", result_path);
        if let Some(expected_revision) = expected_revision {
            command.env("LUASKILLS_CONFIG_WRITE_CHILD_EXPECTED", expected_revision);
        }
        command.spawn().expect("spawn configuration writer child")
    }

    /// Verify concurrent processes merge writes against the latest locked document.
    /// 验证并发进程会基于加锁后的最新文档合并写入。
    #[test]
    fn skill_config_cross_process_writes_do_not_lose_updates() {
        let runtime_root = unique_temp_runtime_root("cross_process_merge");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let first_result_path = runtime_root.join("first-result");
        let second_result_path = runtime_root.join("second-result");
        let mut first =
            spawn_config_writer_child(&file_path, "first", "one", None, &first_result_path);
        let mut second =
            spawn_config_writer_child(&file_path, "second", "two", None, &second_result_path);
        assert!(first.wait().expect("wait for first writer").success());
        assert!(second.wait().expect("wait for second writer").success());
        assert!(
            fs::read_to_string(&first_result_path)
                .expect("read first result")
                .starts_with("ok:")
        );
        assert!(
            fs::read_to_string(&second_result_path)
                .expect("read second result")
                .starts_with("ok:")
        );

        let store = SkillConfigStore::new(file_path).expect("open merged configuration store");
        assert_eq!(
            store
                .list_skill_values("demo-skill")
                .expect("read merged values"),
            BTreeMap::from([
                ("first".to_string(), "one".to_string()),
                ("second".to_string(), "two".to_string()),
            ])
        );
        assert_eq!(store.revision().expect("read merged revision"), "2");
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify compare-and-swap allows only one cross-process writer at one revision.
    /// 验证比较并交换在同一修订号下只允许一个跨进程写入者成功。
    #[test]
    fn skill_config_cross_process_compare_and_swap_has_one_winner() {
        let runtime_root = unique_temp_runtime_root("cross_process_cas");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let first_result_path = runtime_root.join("first-result");
        let second_result_path = runtime_root.join("second-result");
        let mut first =
            spawn_config_writer_child(&file_path, "first", "one", Some("0"), &first_result_path);
        let mut second =
            spawn_config_writer_child(&file_path, "second", "two", Some("0"), &second_result_path);
        assert!(first.wait().expect("wait for first CAS writer").success());
        assert!(second.wait().expect("wait for second CAS writer").success());
        let outcomes = [
            fs::read_to_string(&first_result_path).expect("read first CAS result"),
            fs::read_to_string(&second_result_path).expect("read second CAS result"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.starts_with("ok:"))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.contains("CONFIG_REVISION_CONFLICT"))
                .count(),
            1
        );
        let store = SkillConfigStore::new(file_path).expect("open CAS configuration store");
        assert_eq!(store.revision().expect("read CAS revision"), "1");
        assert_eq!(
            store
                .list_skill_values("demo-skill")
                .expect("read CAS winner values")
                .len(),
            1
        );
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify a separate process holding the companion file causes a bounded write timeout.
    /// 验证独立进程持有伴随文件时会导致有界写入超时。
    #[test]
    fn skill_config_write_times_out_across_processes() {
        let runtime_root = unique_temp_runtime_root("true_cross_process_lock");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        fs::create_dir_all(file_path.parent().expect("config parent"))
            .expect("create config parent");
        let lock_path =
            skill_config_companion_lock_path(&file_path).expect("resolve companion lock path");
        let ready_path = runtime_root.join("child-ready");
        let mut child = Command::new(std::env::current_exe().expect("resolve current test binary"))
            .arg("--exact")
            .arg("runtime::config::tests::skill_config_cross_process_lock_holder")
            .arg("--nocapture")
            .env("LUASKILLS_CONFIG_LOCK_CHILD_PATH", &lock_path)
            .env("LUASKILLS_CONFIG_LOCK_CHILD_READY", &ready_path)
            .spawn()
            .expect("spawn lock-holder child process");
        let ready_deadline = Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() {
            if let Some(status) = child.try_wait().expect("inspect lock-holder child") {
                panic!("lock-holder child exited before readiness with {status}");
            }
            assert!(
                Instant::now() < ready_deadline,
                "lock-holder child did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let store = SkillConfigStore::with_lock_timeout(file_path, Duration::from_millis(100))
            .expect("create cross-process lock store");
        let error = store
            .set_value("demo-skill", "retry_count", "3")
            .expect_err("separate process lock must time out");
        assert!(error.contains("CONFIG_LOCK_TIMEOUT"));
        assert!(
            child.wait().expect("wait for lock-holder child").success(),
            "lock-holder child failed"
        );
        let _ = fs::remove_dir_all(runtime_root);
    }

    /// Verify Windows path aliases that differ only by drive-letter casing or verbatim prefix reuse the same shared lock.
    /// 验证仅在盘符大小写或 verbatim 前缀上存在差异的 Windows 路径别名会复用同一把共享锁。
    #[cfg(windows)]
    #[test]
    fn skill_config_store_normalizes_windows_aliases_for_shared_lock() {
        let runtime_root = unique_temp_runtime_root("shared_lock_windows_alias");
        let canonical_path = runtime_root.join("custom").join("skill_config.json");
        let canonical_text = crate::runtime::path::render_host_visible_path(&canonical_path);
        let drive_letter = canonical_text
            .chars()
            .next()
            .expect("canonical windows path should have a drive letter");
        let alias_text = format!(
            "{}{}",
            drive_letter.to_ascii_lowercase(),
            &canonical_text[drive_letter.len_utf8()..]
        );
        let verbatim_alias = format!(r"\\?\{}", alias_text);

        let first_lock =
            shared_skill_config_path_lock(&canonical_path).expect("resolve canonical shared lock");
        let second_lock = shared_skill_config_path_lock(std::path::Path::new(&verbatim_alias))
            .expect("resolve windows alias shared lock");
        assert!(Arc::ptr_eq(&first_lock, &second_lock));

        // Lowercase verbatim UNC spelling normalized by the same lock-identity boundary.
        // 由同一锁身份边界归一化的小写 verbatim UNC 写法。
        let lowercase_unc = normalize_windows_skill_config_lock_identity_path(Path::new(
            r"\\?\unc\SERVER\Share\Config.JSON",
        ))
        .expect("normalize lowercase verbatim UNC lock path");
        assert_eq!(lowercase_unc, PathBuf::from(r"\\server\share\config.json"));
        // Forward-slash verbatim UNC spelling accepted from JSON-oriented hosts.
        // 从面向 JSON 的宿主接受的正斜杠 verbatim UNC 写法。
        let forward_unc = normalize_windows_skill_config_lock_identity_path(Path::new(
            "//?/UNC/SERVER/Share/Config.JSON",
        ))
        .expect("normalize forward verbatim UNC lock path");
        assert_eq!(forward_unc, PathBuf::from(r"\\server\share\config.json"));
    }
}
