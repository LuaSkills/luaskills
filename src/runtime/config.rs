use crate::lua_skill::validate_luaskills_identifier;
#[cfg(windows)]
use crate::runtime::path::normalize_host_visible_path_text;
use crate::runtime::path::render_host_visible_path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
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

/// One persisted skill-config document stored in the unified runtime config file.
/// 存储在统一运行时配置文件中的单个技能配置文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct SkillConfigDocument {
    /// Per-skill string key-value map grouped by stable skill identifiers.
    /// 按稳定技能标识符分组的每技能字符串键值映射。
    #[serde(default)]
    skills: BTreeMap<String, BTreeMap<String, String>>,
}

/// Shared unified store that owns the single runtime skill config file.
/// 拥有单一运行时技能配置文件的共享统一存储。
#[derive(Debug)]
pub struct SkillConfigStore {
    /// Optional explicit file path injected by the host.
    /// 由宿主注入的可选显式配置文件路径。
    explicit_file_path: Option<PathBuf>,
    /// Lazily captured default runtime root used when the host does not pass one explicit path.
    /// 当宿主未传显式路径时懒加载记录的默认运行时根目录。
    default_runtime_root: Mutex<Option<PathBuf>>,
}

impl SkillConfigStore {
    /// Create one unified skill-config store from an optional explicit file path.
    /// 基于一个可选显式文件路径创建统一技能配置存储。
    pub fn new(explicit_file_path: Option<PathBuf>) -> Result<Self, String> {
        let explicit_file_path = explicit_file_path
            .map(|path| resolve_explicit_skill_config_file_path(&path))
            .transpose()?;
        Ok(Self {
            explicit_file_path,
            default_runtime_root: Mutex::new(None),
        })
    }

    /// Return whether the host has already pinned one explicit unified config file path.
    /// 返回宿主是否已经固定了一条显式统一配置文件路径。
    pub fn has_explicit_file_path(&self) -> bool {
        self.explicit_file_path.is_some()
    }

    /// Capture the runtime root used by the default config path when no explicit file path exists.
    /// 在不存在显式文件路径时记录默认配置路径所使用的运行时根目录。
    pub fn set_default_runtime_root(&self, runtime_root: &Path) -> Result<(), String> {
        let mut guard = self.lock_default_runtime_root();
        *guard = Some(runtime_root.to_path_buf());
        Ok(())
    }

    /// Return the effective unified skill-config file path.
    /// 返回生效中的统一技能配置文件路径。
    pub fn file_path(&self) -> Result<PathBuf, String> {
        if let Some(path) = self.explicit_file_path.as_ref() {
            return Ok(path.clone());
        }
        let guard = self.lock_default_runtime_root();
        let runtime_root = guard.as_ref().ok_or_else(|| {
            "skill config file path is unresolved; set host_options.skill_config_file_path or load at least one skill root first".to_string()
        })?;
        Ok(runtime_root.join("config").join("skill_config.json"))
    }

    /// List flattened config records for one optional skill namespace.
    /// 列出某个可选技能命名空间下的扁平化配置记录。
    pub fn list_entries(&self, skill_id: Option<&str>) -> Result<Vec<SkillConfigEntry>, String> {
        let document = self.with_document_read(|document| Ok(document.clone()))?;
        match skill_id {
            Some(skill_id) => {
                let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
                Ok(document
                    .skills
                    .get(&normalized_skill_id)
                    .into_iter()
                    .flat_map(|items| {
                        items.iter().map(|(key, value)| SkillConfigEntry {
                            skill_id: normalized_skill_id.clone(),
                            key: key.clone(),
                            value: value.clone(),
                        })
                    })
                    .collect())
            }
            None => Ok(document
                .skills
                .iter()
                .flat_map(|(skill_id, items)| {
                    items.iter().map(|(key, value)| SkillConfigEntry {
                        skill_id: skill_id.clone(),
                        key: key.clone(),
                        value: value.clone(),
                    })
                })
                .collect()),
        }
    }

    /// List the complete key-value map owned by one skill namespace.
    /// 列出某个技能命名空间拥有的完整键值映射。
    pub fn list_skill_values(&self, skill_id: &str) -> Result<BTreeMap<String, String>, String> {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        let document = self.with_document_read(|document| Ok(document.clone()))?;
        Ok(document
            .skills
            .get(&normalized_skill_id)
            .cloned()
            .unwrap_or_default())
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

    /// Return whether one `(skill_id, key)` pair currently exists in the store.
    /// 返回某个 `(skill_id, key)` 对当前是否存在于存储中。
    pub fn has_value(&self, skill_id: &str, key: &str) -> Result<bool, String> {
        Ok(self.get_value(skill_id, key)?.is_some())
    }

    /// Insert or replace one string config value under one `(skill_id, key)` pair.
    /// 在某个 `(skill_id, key)` 对下插入或替换单个字符串配置值。
    pub fn set_value(&self, skill_id: &str, key: &str, value: &str) -> Result<(), String> {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        let normalized_key = validate_skill_config_key(key)?;
        self.with_document_mut(|document| {
            document
                .skills
                .entry(normalized_skill_id)
                .or_default()
                .insert(normalized_key, value.to_string());
            Ok(())
        })
    }

    /// Delete one config key under one skill namespace and report whether one value was removed.
    /// 删除某个技能命名空间下的单个配置键，并返回是否移除了一个值。
    pub fn delete_value(&self, skill_id: &str, key: &str) -> Result<bool, String> {
        let normalized_skill_id = validate_skill_config_skill_id(skill_id)?;
        let normalized_key = validate_skill_config_key(key)?;
        self.with_document_mut(|document| {
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
            Ok(deleted)
        })
    }

    /// Execute one read-only document operation under one process-wide lock derived from the effective config file path.
    /// 在由生效配置文件路径派生的进程级锁下执行一次只读文档操作。
    fn with_document_read<T, F>(&self, action: F) -> Result<T, String>
    where
        F: FnOnce(&SkillConfigDocument) -> Result<T, String>,
    {
        let file_path = self.file_path()?;
        let path_lock = shared_skill_config_path_lock(&file_path)?;
        let _path_guard = lock_shared_skill_config_path(&path_lock);
        let document = self.read_document_from(&file_path)?;
        action(&document)
    }

    /// Execute one read-modify-write document operation under one process-wide lock derived from the effective config file path.
    /// 在由生效配置文件路径派生的进程级锁下执行一次读改写文档操作。
    fn with_document_mut<T, F>(&self, action: F) -> Result<T, String>
    where
        F: FnOnce(&mut SkillConfigDocument) -> Result<T, String>,
    {
        let file_path = self.file_path()?;
        let path_lock = shared_skill_config_path_lock(&file_path)?;
        let _path_guard = lock_shared_skill_config_path(&path_lock);
        let mut document = self.read_document_from(&file_path)?;
        let result = action(&mut document)?;
        self.write_document_to(&file_path, &document)?;
        Ok(result)
    }

    /// Acquire the default runtime-root lock and return its guard, recovering after lock poisoning.
    /// 获取并返回默认 runtime-root 锁保护对象；如果锁已 poison，则恢复继续使用。
    fn lock_default_runtime_root(&self) -> MutexGuard<'_, Option<PathBuf>> {
        self.default_runtime_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Load the current persisted document, treating a missing file as one empty config set.
    /// 加载当前持久化文档，并把缺失文件视为一份空配置集合。
    fn read_document_from(&self, file_path: &Path) -> Result<SkillConfigDocument, String> {
        if !skill_config_file_is_file(file_path)? {
            return Ok(SkillConfigDocument::default());
        }
        let text = fs::read_to_string(file_path).map_err(|error| {
            format!(
                "failed to read skill config file '{}': {}",
                render_skill_config_path(file_path),
                error
            )
        })?;
        serde_json::from_str::<SkillConfigDocument>(&text).map_err(|error| {
            format!(
                "failed to parse skill config file '{}': {}",
                render_skill_config_path(file_path),
                error
            )
        })
    }

    /// Persist one complete document with one temp-file write followed by one replacement rename.
    /// 通过“先写临时文件再替换重命名”的方式持久化整份文档。
    fn write_document_to(
        &self,
        file_path: &Path,
        document: &SkillConfigDocument,
    ) -> Result<(), String> {
        let parent = file_path.parent().ok_or_else(|| {
            format!(
                "skill config file '{}' has no parent directory",
                render_skill_config_path(file_path)
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create skill config directory '{}': {}",
                render_skill_config_path(parent),
                error
            )
        })?;
        let serialized = serde_json::to_vec_pretty(document)
            .map_err(|error| format!("failed to serialize skill config document: {}", error))?;
        let temp_path = file_path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                format!(
                    "failed to create skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
            file.write_all(&serialized).map_err(|error| {
                format!(
                    "failed to write skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
            file.flush().map_err(|error| {
                format!(
                    "failed to flush skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "failed to sync skill config temp file '{}': {}",
                    render_skill_config_path(&temp_path),
                    error
                )
            })?;
        }
        replace_file_atomically(&temp_path, file_path).map_err(|error| {
            format!(
                "failed to promote skill config temp file '{}' to '{}': {}",
                render_skill_config_path(&temp_path),
                render_skill_config_path(file_path),
                error
            )
        })
    }
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
            "skill config file is not a file '{}'",
            render_skill_config_path(file_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect skill config file '{}': {}",
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
                    "failed to resolve current directory for skill config lock: {}",
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
    let resolved_path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(file_path))
            .map_err(|error| {
                format!(
                    "failed to resolve current directory for explicit skill config path: {}",
                    error
                )
            })?
    };
    Ok(normalize_skill_config_lock_path(&resolved_path))
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
    let rendered = path
        .to_str()
        .ok_or_else(|| "skill config lock path must be valid UTF-8 on Windows".to_string())?;
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
    let normalized = skill_id.trim();
    validate_luaskills_identifier(normalized, "skill_id")
        .map(|_| normalized.to_string())
        .map_err(|error| format!("invalid skill config skill_id: {}", error))
}

/// Validate one config key used inside one skill namespace.
/// 校验技能命名空间内使用的单个配置键。
fn validate_skill_config_key(key: &str) -> Result<String, String> {
    let normalized = key.trim();
    if normalized.is_empty() {
        return Err("skill config key must not be empty".to_string());
    }
    Ok(normalized.to_string())
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
        SkillConfigEntry, SkillConfigStore, shared_skill_config_path_lock,
        skill_config_lock_registry,
    };
    use crate::runtime::path::render_host_visible_path;
    use std::collections::BTreeMap;
    use std::fs;
    use std::panic::{self, AssertUnwindSafe};
    #[cfg(windows)]
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create one unique temporary runtime root used by config-store tests.
    /// 创建一个供配置存储测试使用的唯一临时运行时根目录。
    fn unique_temp_runtime_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("luaskills_skill_config_{}_{}", label, nonce))
    }

    /// Verify the store resolves the default config file under `<runtime_root>/config/skill_config.json`.
    /// 验证存储会把默认配置文件解析到 `<runtime_root>/config/skill_config.json`。
    #[test]
    fn skill_config_store_resolves_default_path_from_runtime_root() {
        let runtime_root = unique_temp_runtime_root("default_path");
        let store = SkillConfigStore::new(None).expect("create default path store");
        store
            .set_default_runtime_root(&runtime_root)
            .expect("set runtime root");
        assert_eq!(
            store.file_path().expect("resolve config path"),
            runtime_root.join("config").join("skill_config.json")
        );
    }

    /// Verify the default runtime root tracks the latest root instead of keeping the first one forever.
    /// 验证默认运行时根目录会跟随最新根目录更新，而不是永久保留第一次的值。
    #[test]
    fn skill_config_store_updates_default_path_when_runtime_root_changes() {
        let first_root = unique_temp_runtime_root("default_path_first");
        let second_root = unique_temp_runtime_root("default_path_second");
        let store = SkillConfigStore::new(None).expect("create update path store");
        store
            .set_default_runtime_root(&first_root)
            .expect("set first runtime root");
        store
            .set_default_runtime_root(&second_root)
            .expect("set second runtime root");
        assert_eq!(
            store.file_path().expect("resolve updated config path"),
            second_root.join("config").join("skill_config.json")
        );
    }

    /// Verify the default runtime-root lock recovers after poisoning.
    /// 验证默认 runtime-root 锁 poison 后仍可恢复。
    #[test]
    fn skill_config_default_runtime_root_recovers_after_poisoned_lock() {
        // Runtime root installed after poisoning to prove the setter lock path recovered.
        // poison 后写入的 runtime root，用于证明 setter 锁路径已恢复。
        let runtime_root = unique_temp_runtime_root("default_root_poison");
        // Store whose default runtime-root lock is intentionally poisoned for this test.
        // 本测试中会被故意 poison 默认 runtime-root 锁的存储实例。
        let store = SkillConfigStore::new(None).expect("create poisoned default-root store");

        // Captured panic result from a writer that poisons the default runtime-root lock.
        // 默认 runtime-root 写入者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the default runtime-root lock.
            // 仅用于制造默认 runtime-root 锁 poison 的保护对象。
            let _guard = store
                .default_runtime_root
                .lock()
                .expect("initial default runtime-root lock");
            panic!("poison default runtime-root lock for recovery test");
        }));

        assert!(poison_result.is_err());
        store
            .set_default_runtime_root(&runtime_root)
            .expect("set runtime root after poison");
        assert_eq!(
            store.file_path().expect("resolve config path after poison"),
            runtime_root.join("config").join("skill_config.json")
        );
    }

    /// Verify config values persist inside one explicit unified file path.
    /// 验证配置值会持久化到单个显式统一文件路径中。
    #[test]
    fn skill_config_store_persists_values_in_explicit_file() {
        let runtime_root = unique_temp_runtime_root("persist");
        let file_path = runtime_root.join("custom").join("skill_config.json");
        let store = SkillConfigStore::new(Some(file_path.clone())).expect("create explicit store");
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
        let reloaded =
            SkillConfigStore::new(Some(file_path)).expect("create reloaded explicit store");
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
        // Store that reads the invalid explicit config file.
        // 读取非法显式配置文件的配置存储。
        let store = SkillConfigStore::new(Some(file_path.clone())).expect("create explicit store");
        // Error returned by the real flattened-entry read path.
        // 真实扁平化配置读取路径返回的错误。
        let error = store
            .list_entries(None)
            .expect_err("invalid config JSON should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "failed to parse skill config file '{}':",
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
        // Store that reads through the explicit invalid path.
        // 通过显式非法路径读取的配置存储。
        let store = SkillConfigStore::new(Some(file_path.clone())).expect("create explicit store");
        // Error returned by the read path before the invalid path can behave like an absent file.
        // 在非法路径表现得像缺失文件之前，读取路径返回的错误。
        let error = store
            .get_value("demo-skill", "api_token")
            .expect_err("invalid config path probe should fail");

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
        // Store that reads through the explicit directory-backed config path.
        // 通过显式目录型配置路径读取的配置存储。
        let store = SkillConfigStore::new(Some(file_path.clone())).expect("create explicit store");

        // Error returned before the directory can be passed to read_to_string.
        // 在目录被传递给 read_to_string 之前返回的错误。
        let error = store
            .get_value("demo-skill", "api_token")
            .expect_err("directory config file should fail");

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
        let store = SkillConfigStore::new(Some(file_path)).expect("create flattened-list store");
        store
            .set_value("alpha-skill", "api_token", "alpha-token")
            .expect("set alpha token");
        store
            .set_value("beta-skill", "endpoint", "https://example.test")
            .expect("set beta endpoint");
        assert_eq!(
            store.list_entries(None).expect("list entries"),
            vec![
                SkillConfigEntry {
                    skill_id: "alpha-skill".to_string(),
                    key: "api_token".to_string(),
                    value: "alpha-token".to_string(),
                },
                SkillConfigEntry {
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
        let store = SkillConfigStore::new(Some(file_path)).expect("create skill-map store");
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
        let store = SkillConfigStore::new(Some(file_path.clone())).expect("create delete store");
        store
            .set_value("demo-skill", "api_token", "sk-123")
            .expect("set api token");
        assert!(
            store
                .delete_value("demo-skill", "api_token")
                .expect("delete api token")
        );
        assert_eq!(
            store
                .get_value("demo-skill", "api_token")
                .expect("read deleted value"),
            None
        );
        let persisted =
            fs::read_to_string(file_path).expect("skill config file should still be readable");
        assert_eq!(persisted.trim(), "{\n  \"skills\": {}\n}");
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

    /// Verify one relative explicit config file path gets fixed to one absolute path at creation time.
    /// 验证单个相对显式配置文件路径会在创建时固定成绝对路径。
    #[test]
    fn skill_config_store_freezes_relative_explicit_path_at_creation_time() {
        let relative_path = PathBuf::from("config").join("skill_config.json");
        let expected_path = std::env::current_dir()
            .expect("resolve current directory")
            .join(&relative_path);
        let store = SkillConfigStore::new(Some(relative_path))
            .expect("create relative explicit-path store");
        assert_eq!(
            store.file_path().expect("resolve frozen explicit path"),
            expected_path
        );
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
            SkillConfigStore::new(Some(file_path.clone())).expect("create shared-io poison store");
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
