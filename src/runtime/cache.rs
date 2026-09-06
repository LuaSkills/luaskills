use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default maximum number of cached entries for the process-wide shared tool cache.
/// 工具缓存默认最大条目数，用于限制进程内共享缓存的总体容量。
pub const DEFAULT_TOOL_CACHE_MAX_ENTRIES: usize = 1000;

/// Default cache entry lifetime in seconds, used when callers do not provide an explicit TTL.
/// 工具缓存默认存活时间，单位为秒；未显式指定时使用该值。
pub const DEFAULT_TOOL_CACHE_DEFAULT_TTL_SECS: u64 = 30 * 60;

/// Maximum cache entry lifetime in seconds; larger requested TTL values are clamped to this ceiling.
/// 工具缓存允许的最长存活时间，单位为秒；超过该值会被自动钳制。
pub const DEFAULT_TOOL_CACHE_MAX_TTL_SECS: u64 = 30 * 60;

/// Runtime configuration for the shared tool cache, controlling capacity and expiration behavior.
/// 共享工具缓存的运行时配置，控制容量与过期策略。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolCacheConfig {
    /// Maximum number of entries; oldest entries are evicted when the cache exceeds this size.
    /// 缓存最大条目数，超出后会按创建顺序淘汰最旧条目。
    pub max_entries: usize,
    /// Default TTL in seconds used when callers omit a TTL.
    /// 默认 TTL（秒），调用方未传 TTL 时使用。
    pub default_ttl_secs: u64,
    /// Maximum TTL in seconds; requested TTL values are clamped to this ceiling.
    /// 最大 TTL（秒），请求 TTL 会被限制在该范围内。
    pub max_ttl_secs: u64,
}

impl Default for ToolCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_TOOL_CACHE_MAX_ENTRIES,
            default_ttl_secs: DEFAULT_TOOL_CACHE_DEFAULT_TTL_SECS,
            max_ttl_secs: DEFAULT_TOOL_CACHE_MAX_TTL_SECS,
        }
    }
}

/// Internal representation of one cache entry, recording the owning tool, payload, creation order, and expiration time.
/// 单个缓存条目的内部表示，记录归属工具、内容、创建顺序与过期时间。
#[derive(Clone, Debug)]
struct ToolCacheEntry {
    /// Tool or skill name that owns this entry, used to isolate cache namespaces.
    /// 写入该条目的工具/技能名称，用于隔离不同工具的缓存空间。
    tool_name: String,
    /// Cached JSON payload returned to callers as-is on reads.
    /// 缓存的 JSON 值，会在读取时原样返回给调用方。
    value: Value,
    /// Monotonic creation sequence used to evict the oldest entry when capacity is exceeded.
    /// 创建序号，用于在容量超限时淘汰最旧条目。
    created_seq: u64,
    /// Expiration instant; reads after this moment trigger automatic cleanup.
    /// 条目失效时刻；超过该时间后读取会自动清理。
    expires_at: Instant,
}

/// Mutable storage backing the shared cache, protected by a read-write lock.
/// 共享缓存的可变存储体，受读写锁保护。
#[derive(Default)]
struct ToolCacheStore {
    entries: HashMap<String, ToolCacheEntry>,
}

/// Process-wide shared cache for all Lua skills, intended for short-lived pagination and tool state handoff.
/// 主程序级共享工具缓存，供所有 Lua 技能复用短时分页/状态数据。
pub struct SharedToolCache {
    store: RwLock<ToolCacheStore>,
    config: ToolCacheConfig,
    counter: AtomicU64,
}

impl SharedToolCache {
    /// Create a shared cache instance with the provided configuration.
    /// 使用指定配置创建共享缓存实例。
    pub fn new(config: ToolCacheConfig) -> Self {
        Self {
            store: RwLock::new(ToolCacheStore::default()),
            config,
            counter: AtomicU64::new(1),
        }
    }

    /// Store one cache record; missing TTL falls back to the default and values above the ceiling are clamped.
    /// 写入一条缓存记录；TTL 为空时使用默认值，超出上限时会自动钳制。
    ///
    /// The tool_name parameter is the namespace that owns the cached payload.
    /// tool_name 参数是拥有该缓存载荷的命名空间。
    ///
    /// The value parameter is the JSON payload stored for later retrieval.
    /// value 参数是保存后供后续读取的 JSON 载荷。
    ///
    /// The ttl_secs parameter optionally overrides the configured default TTL.
    /// ttl_secs 参数可选地覆盖配置中的默认 TTL。
    ///
    /// Returns the generated cache id when the cache record is stored.
    /// 成功写入缓存记录时返回生成的缓存编号。
    ///
    /// Returns an error when the cache id timestamp cannot be represented.
    /// 当缓存编号时间戳无法表示时返回错误。
    pub fn create(
        &self,
        tool_name: &str,
        value: Value,
        ttl_secs: Option<u64>,
    ) -> Result<String, String> {
        // Current monotonic instant used for expiration and cleanup decisions.
        // 用于过期与清理判断的当前单调时刻。
        let now = Instant::now();
        // Effective TTL after defaulting, clamping, and minimum enforcement.
        // 经过默认值、上限裁剪和最小值约束后的最终 TTL。
        let ttl = self.resolve_ttl(ttl_secs);
        // Public cache id generated before insertion so failures stop the write.
        // 插入前生成的公开缓存编号，失败时会阻止写入。
        let cache_id = self.next_cache_id()?;
        // Complete cache entry stored under the generated id.
        // 存放在生成编号下的完整缓存条目。
        let entry = ToolCacheEntry {
            tool_name: tool_name.to_string(),
            value,
            created_seq: self.counter.fetch_add(1, Ordering::Relaxed),
            expires_at: now + ttl,
        };

        // Writable cache store guard used for bounded cleanup, insertion, and capacity enforcement.
        // 用于有界清理、插入和容量约束的可写缓存存储保护对象。
        let mut store = self.write_store();
        // Expired entries are scanned only when the store reaches capacity instead of on every create.
        // 仅在存储达到容量时扫描过期条目，不再每次创建都执行扫描。
        if store.entries.len() >= self.config.max_entries.max(1) {
            self.cleanup_expired_locked(&mut store, now);
        }
        store.entries.insert(cache_id.clone(), entry);
        self.enforce_capacity_locked(&mut store);
        Ok(cache_id)
    }

    /// Read a cached entry by tool name and cache id; expired hits are removed and returned as empty.
    /// 按工具名和缓存编号读取缓存；命中但已过期时会自动删除并返回空。
    pub fn get(&self, tool_name: &str, cache_id: &str) -> Option<Value> {
        // Current monotonic instant shared by the read check and exact expired-key removal.
        // 读检查与精确过期键删除共用的当前单调时刻。
        let now = Instant::now();

        {
            // Read guard serves hits, pure misses, and scope mismatches without write-lock escalation.
            // 读保护直接处理命中、纯未命中和作用域不匹配，不升级写锁。
            let store = self.read_store();
            match store.entries.get(cache_id) {
                Some(entry) if entry.tool_name == tool_name && entry.expires_at > now => {
                    return Some(entry.value.clone());
                }
                Some(entry) if entry.expires_at <= now => {}
                Some(_) | None => return None,
            }
        }

        // Write guard is acquired only for the exact cache id observed as expired.
        // 仅对已确认过期的精确缓存编号获取写保护。
        let mut store = self.write_store();
        // Removal is revalidated after lock upgrade so a concurrent state change cannot be deleted.
        // 锁升级后重新验证删除条件，避免删除并发变化后的状态。
        if store
            .entries
            .get(cache_id)
            .is_some_and(|entry| entry.expires_at <= now)
        {
            store.entries.remove(cache_id);
        }
        None
    }

    /// Delete one cache entry under the given tool namespace and return whether an entry was actually removed.
    /// 删除指定工具名下的缓存条目；返回是否确实删除了条目。
    pub fn delete(&self, tool_name: &str, cache_id: &str) -> bool {
        // Current instant separates logically expired records from live caller-owned records.
        // 当前时刻用于区分逻辑过期记录与调用方拥有的有效记录。
        let now = Instant::now();
        // Write guard is needed only for the caller-selected cache id.
        // 仅为调用方指定的缓存编号获取写保护。
        let mut store = self.write_store();
        // EntryState is copied before removal so the immutable map borrow does not cross mutation.
        // EntryState 在删除前复制，避免不可变映射借用跨越修改操作。
        let Some((expired, owner_matches)) = store
            .entries
            .get(cache_id)
            .map(|entry| (entry.expires_at <= now, entry.tool_name == tool_name))
        else {
            return false;
        };
        if expired {
            store.entries.remove(cache_id);
            return false;
        }
        if owner_matches {
            store.entries.remove(cache_id);
            return true;
        }
        false
    }

    /// Acquire a read guard and return the current cache store, recovering it after another cache operation panics while holding the lock.
    /// 获取并返回当前缓存存储读保护；如果其它缓存操作持锁 panic，则恢复缓存存储继续使用。
    fn read_store(&self) -> RwLockReadGuard<'_, ToolCacheStore> {
        self.store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquire a write guard and return the mutable cache store, recovering it after another cache operation panics while holding the lock.
    /// 获取并返回当前缓存存储写保护；如果其它缓存操作持锁 panic，则恢复缓存存储继续使用。
    fn write_store(&self) -> RwLockWriteGuard<'_, ToolCacheStore> {
        self.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Resolve the effective TTL by applying defaulting, clamping to the configured maximum, and enforcing a 1-second minimum.
    /// 解析最终 TTL，未传使用默认值，超限后按最大值裁剪，最小保证为 1 秒。
    fn resolve_ttl(&self, ttl_secs: Option<u64>) -> Duration {
        let requested = ttl_secs.unwrap_or(self.config.default_ttl_secs);
        let clamped = requested.max(1).min(self.config.max_ttl_secs.max(1));
        Duration::from_secs(clamped)
    }

    /// Generate a cache id by combining a timestamp with a monotonic counter to reduce collision risk.
    /// 生成缓存编号，结合时间戳与自增计数以降低碰撞风险。
    ///
    /// Returns the generated cache id, or an error when the system clock cannot be represented.
    /// 返回生成的缓存编号；当系统时钟无法表示时返回错误。
    fn next_cache_id(&self) -> Result<String, String> {
        // Wall-clock millisecond component used to make cache ids sortable and inspectable.
        // 用于让缓存编号可排序、可检查的墙钟毫秒组成部分。
        let unix_ms =
            system_time_to_cache_id_unix_millis(SystemTime::now(), "tool cache id timestamp")?;
        // Monotonic process-local sequence component used to avoid same-millisecond collisions.
        // 用于避免同一毫秒内碰撞的进程内单调序号组成部分。
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        Ok(format!("tc-{}-{}", unix_ms, seq))
    }

    /// Remove all expired entries so subsequent reads and writes operate on the current valid view.
    /// 清理所有已过期条目，保证后续读写看到的是当前有效视图。
    fn cleanup_expired_locked(&self, store: &mut ToolCacheStore, now: Instant) {
        store.entries.retain(|_, entry| entry.expires_at > now);
    }

    /// Evict the oldest entries while the cache is above its configured capacity.
    /// 在缓存超出上限时淘汰最旧条目，直到条目数回落到配置范围内。
    fn enforce_capacity_locked(&self, store: &mut ToolCacheStore) {
        while store.entries.len() > self.config.max_entries.max(1) {
            let oldest_id = store
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.created_seq)
                .map(|(cache_id, _)| cache_id.clone());
            match oldest_id {
                Some(cache_id) => {
                    store.entries.remove(&cache_id);
                }
                None => break,
            }
        }
    }
}

/// Convert one system time into the Unix millisecond component used by cache ids.
/// 将单个系统时间转换为缓存编号使用的 Unix 毫秒组成部分。
///
/// The time parameter is the wall-clock timestamp to encode into one cache id.
/// time 参数是要编码进单个缓存编号的墙钟时间戳。
///
/// The context parameter names the caller for precise error diagnostics.
/// context 参数命名调用方，用于精确错误诊断。
///
/// Returns the Unix millisecond component for a cache id.
/// 返回缓存编号使用的 Unix 毫秒组成部分。
fn system_time_to_cache_id_unix_millis(time: SystemTime, context: &str) -> Result<u128, String> {
    // Duration measured from the Unix epoch for one cache id timestamp.
    // 单个缓存编号时间戳相对于 Unix epoch 的持续时间。
    let duration = time.duration_since(UNIX_EPOCH).map_err(|error| {
        format!(
            "{} is before Unix epoch and cannot be used for a tool cache id: {}",
            context, error
        )
    })?;
    Ok(duration.as_millis())
}

/// Stable error prefix returned when a caller requests a conflicting process-wide cache configuration.
/// 当调用方请求与进程级缓存配置冲突的配置时返回的稳定错误前缀。
pub const TOOL_CACHE_CONFIG_CONFLICT_ERROR: &str =
    "tool cache configuration conflicts with the process-wide configuration";

/// Compare one requested cache configuration with the configuration already owned by a cache.
/// 比较一个请求的缓存配置与缓存已持有的配置。
///
/// The cache parameter is the initialized cache whose immutable configuration is authoritative.
/// cache 参数是已初始化缓存，其不可变配置为权威配置。
///
/// The requested parameter is the configuration requested by the current caller.
/// requested 参数是当前调用方请求的配置。
///
/// Returns success for an identical configuration or a stable conflict error for a mismatch.
/// 配置相同时返回成功，不同时返回稳定的冲突错误。
fn ensure_tool_cache_config_matches(
    cache: &SharedToolCache,
    requested: &ToolCacheConfig,
) -> Result<(), String> {
    if cache.config == *requested {
        Ok(())
    } else {
        Err(format!(
            "{TOOL_CACHE_CONFIG_CONFLICT_ERROR}: existing={:?}, requested={requested:?}",
            cache.config
        ))
    }
}

/// Configure one cache cell exactly once while accepting identical concurrent requests.
/// 对一个缓存单元只配置一次，同时接受完全相同的并发请求。
///
/// The cell parameter owns the immutable process-level cache instance.
/// cell 参数持有不可变的进程级缓存实例。
///
/// The config parameter is the exact configuration requested by the caller.
/// config 参数是调用方请求的确切配置。
///
/// Returns success when initialization wins or matches the winner, otherwise a conflict error.
/// 初始化成功或与胜出配置一致时返回成功，否则返回冲突错误。
fn configure_tool_cache_cell(
    cell: &OnceLock<Arc<SharedToolCache>>,
    config: ToolCacheConfig,
) -> Result<(), String> {
    if let Some(configured_cache) = cell.get() {
        return ensure_tool_cache_config_matches(configured_cache, &config);
    }

    // attempted_cache retains the requested configuration for race-result comparison.
    // attempted_cache 保留请求配置，用于并发竞态结果比较。
    let attempted_cache = Arc::new(SharedToolCache::new(config));
    match cell.set(attempted_cache.clone()) {
        Ok(()) => Ok(()),
        Err(_) => {
            // configured_cache is the single cache that won concurrent initialization.
            // configured_cache 是并发初始化中唯一胜出的缓存。
            let configured_cache = cell
                .get()
                .expect("cache cell must be initialized after OnceLock::set loses a race");
            ensure_tool_cache_config_matches(configured_cache, &attempted_cache.config)
        }
    }
}

/// Configure one cache cell through the legacy unit-returning API and fail loudly on conflicts.
/// 通过旧版 unit 返回 API 配置缓存单元，并在冲突时显式失败。
///
/// The cell parameter owns the immutable process-level cache instance.
/// cell 参数持有不可变的进程级缓存实例。
///
/// The config parameter is the exact configuration requested by the legacy caller.
/// config 参数是旧版调用方请求的确切配置。
///
/// Panics when a different configuration already owns the process-wide cache because the legacy signature cannot return an error.
/// 当不同配置已经占用进程级缓存时触发 panic，因为旧版签名无法返回错误。
fn configure_tool_cache_cell_legacy(
    cell: &OnceLock<Arc<SharedToolCache>>,
    config: ToolCacheConfig,
) {
    configure_tool_cache_cell(cell, config)
        .unwrap_or_else(|error| panic!("Failed to configure global tool cache: {error}"));
}

/// Read one cache cell, initializing it with the default configuration when still empty.
/// 读取一个缓存单元；若仍为空，则使用默认配置初始化。
///
/// The cell parameter owns the immutable cache instance.
/// cell 参数持有不可变缓存实例。
///
/// Returns a shared handle to the initialized cache.
/// 返回已初始化缓存的共享句柄。
fn tool_cache_from_cell(cell: &OnceLock<Arc<SharedToolCache>>) -> Arc<SharedToolCache> {
    cell.get_or_init(|| Arc::new(SharedToolCache::new(ToolCacheConfig::default())))
        .clone()
}

/// Process-wide immutable cache cell shared by every engine.
/// 由所有引擎共享的进程级不可变缓存单元。
static GLOBAL_TOOL_CACHE: OnceLock<Arc<SharedToolCache>> = OnceLock::new();

/// Initialize the global shared cache through the legacy source-compatible call surface.
/// 通过保持源码兼容的旧调用界面初始化全局共享缓存。
///
/// The config parameter is retained for callers compiled against the original unit-returning API.
/// config 参数为针对原有 unit 返回 API 编写的调用方保留。
///
/// Panics on a conflicting second configuration so the legacy API never continues with silently ignored settings.
/// 当第二次配置冲突时触发 panic，确保旧版 API 不会在配置被静默忽略后继续运行。
pub fn configure_global_tool_cache(config: ToolCacheConfig) {
    configure_tool_cache_cell_legacy(&GLOBAL_TOOL_CACHE, config);
}

/// Initialize the global shared cache and report a conflicting process-wide configuration explicitly.
/// 初始化全局共享缓存，并显式报告冲突的进程级配置。
///
/// The config parameter is the exact immutable configuration requested by the current engine.
/// config 参数是当前引擎请求的精确不可变配置。
///
/// Returns success for the first or identical configuration and a stable conflict error otherwise.
/// 首次配置或相同配置返回成功；其他情况返回稳定冲突错误。
pub fn try_configure_global_tool_cache(config: ToolCacheConfig) -> Result<(), String> {
    configure_tool_cache_cell(&GLOBAL_TOOL_CACHE, config)
}

/// Get the global shared cache, lazily creating it with default settings if startup did not configure it explicitly.
/// 获取全局共享缓存；若尚未初始化则使用默认配置惰性创建。
pub fn global_tool_cache() -> Arc<SharedToolCache> {
    tool_cache_from_cell(&GLOBAL_TOOL_CACHE)
}

#[cfg(test)]
mod tests {
    use super::{
        SharedToolCache, TOOL_CACHE_CONFIG_CONFLICT_ERROR, ToolCacheConfig,
        configure_global_tool_cache, configure_tool_cache_cell, configure_tool_cache_cell_legacy,
        system_time_to_cache_id_unix_millis, tool_cache_from_cell,
    };
    use serde_json::json;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Arc, Barrier, OnceLock, mpsc};
    use std::thread;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    /// Verify the legacy global cache configurator retains its original unit-returning function type.
    /// 验证旧版全局缓存配置函数保留原有的 unit 返回函数类型。
    #[test]
    fn global_cache_configurator_keeps_legacy_function_signature() {
        // LegacySignature fails to compile if the public return type changes again.
        // LegacySignature 会在公共返回类型再次变化时导致编译失败。
        let _legacy_signature: fn(ToolCacheConfig) = configure_global_tool_cache;
    }

    /// Verify the unit-returning legacy configurator cannot silently discard a conflicting request.
    /// 验证返回 unit 的旧版配置器不会静默丢弃冲突请求。
    #[test]
    fn legacy_cache_configurator_panics_on_conflict() {
        // Cell isolates the legacy conflict behavior from process-global test state.
        // Cell 将旧版冲突行为与进程级测试状态隔离。
        let cell = OnceLock::new();
        configure_tool_cache_cell_legacy(&cell, test_cache_config(10, 5, 5));

        // PanicResult proves the source-compatible unit API stops on rejected configuration.
        // PanicResult 证明源码兼容的 unit API 会在配置被拒绝时停止。
        let panic_result = panic::catch_unwind(AssertUnwindSafe(|| {
            configure_tool_cache_cell_legacy(&cell, test_cache_config(20, 5, 5));
        }));

        assert!(panic_result.is_err());
    }

    /// Build one deterministic cache config used by unit tests.
    /// 为单元测试构造一份稳定可预测的缓存配置。
    fn test_cache_config(
        max_entries: usize,
        default_ttl_secs: u64,
        max_ttl_secs: u64,
    ) -> ToolCacheConfig {
        ToolCacheConfig {
            max_entries,
            default_ttl_secs,
            max_ttl_secs,
        }
    }

    /// Verify an unspecified first configuration initializes the default once and reuses it.
    /// 验证首次未指定配置时只初始化一次默认配置并持续复用。
    #[test]
    fn cache_cell_default_initialization_is_reused() {
        // cell is an isolated process-cache model for this contract test.
        // cell 是本契约测试使用的隔离进程缓存模型。
        let cell = OnceLock::new();
        // first_cache initializes the default configuration for an unspecified first engine.
        // first_cache 为首次未指定配置的引擎初始化默认配置。
        let first_cache = tool_cache_from_cell(&cell);
        // second_cache represents another unspecified engine reusing the initialized cache.
        // second_cache 表示另一个未指定配置的引擎复用已初始化缓存。
        let second_cache = tool_cache_from_cell(&cell);

        assert!(Arc::ptr_eq(&first_cache, &second_cache));
        assert_eq!(first_cache.config, ToolCacheConfig::default());
    }

    /// Verify identical explicit requests and later unspecified requests reuse the first cache.
    /// 验证相同显式请求与后续未指定请求都会复用第一个缓存。
    #[test]
    fn cache_cell_accepts_identical_explicit_configuration() {
        // cell is an isolated process-cache model for explicit initialization.
        // cell 是用于显式初始化的隔离进程缓存模型。
        let cell = OnceLock::new();
        // config is the authoritative explicit configuration selected by the first engine.
        // config 是第一个引擎选择的权威显式配置。
        let config = test_cache_config(17, 23, 29);

        configure_tool_cache_cell(&cell, config.clone()).expect("first configuration must win");
        configure_tool_cache_cell(&cell, config.clone())
            .expect("identical configuration must be accepted");
        // reused_cache represents a later engine with no explicit configuration.
        // reused_cache 表示后续没有显式配置的引擎。
        let reused_cache = tool_cache_from_cell(&cell);

        assert_eq!(reused_cache.config, config);
    }

    /// Verify explicit requests that differ from a default-initialized cache fail clearly.
    /// 验证与默认初始化缓存不同的显式请求会明确失败。
    #[test]
    fn cache_cell_rejects_explicit_configuration_after_default() {
        // cell is initialized through the unspecified/default path before the conflict.
        // cell 在冲突前通过未指定配置的默认路径初始化。
        let cell = OnceLock::new();
        drop(tool_cache_from_cell(&cell));
        // requested is deliberately different from every default cache field.
        // requested 被刻意设置为与默认缓存的每个字段不同。
        let requested = test_cache_config(3, 5, 7);
        // error is the stable conflict returned instead of silently ignoring the request.
        // error 是返回的稳定冲突错误，不再静默忽略请求。
        let error = configure_tool_cache_cell(&cell, requested)
            .expect_err("configuration after default initialization must conflict");

        assert!(error.starts_with(TOOL_CACHE_CONFIG_CONFLICT_ERROR));
        assert_eq!(
            cell.get()
                .expect("default cache must remain initialized")
                .config,
            ToolCacheConfig::default()
        );
    }

    /// Verify two different explicit configurations cannot silently overwrite each other.
    /// 验证两个不同的显式配置不能静默覆盖彼此。
    #[test]
    fn cache_cell_rejects_conflicting_explicit_configuration() {
        // cell is an isolated process-cache model for sequential conflicting requests.
        // cell 是用于顺序冲突请求的隔离进程缓存模型。
        let cell = OnceLock::new();
        // first_config is the immutable winner retained by the cell.
        // first_config 是缓存单元保留的不可变胜出配置。
        let first_config = test_cache_config(11, 13, 17);
        // second_config is the incompatible later request.
        // second_config 是后续不兼容请求。
        let second_config = test_cache_config(19, 23, 29);

        configure_tool_cache_cell(&cell, first_config.clone())
            .expect("first explicit configuration must win");
        // error identifies the rejected conflicting request.
        // error 标识被拒绝的冲突请求。
        let error = configure_tool_cache_cell(&cell, second_config)
            .expect_err("different explicit configuration must conflict");

        assert!(error.starts_with(TOOL_CACHE_CONFIG_CONFLICT_ERROR));
        assert_eq!(
            cell.get()
                .expect("first cache must remain initialized")
                .config,
            first_config
        );
    }

    /// Verify concurrent first initialization has one winner and one deterministic conflict.
    /// 验证并发首次初始化只有一个胜出者，另一个得到确定的冲突。
    #[test]
    fn cache_cell_concurrent_conflicting_initialization_has_one_winner() {
        // cell is shared by both racing initialization attempts.
        // cell 由两个竞态初始化请求共享。
        let cell = Arc::new(OnceLock::new());
        // barrier releases both attempts at the same instant after the test thread is ready.
        // barrier 在测试线程就绪后同时释放两个初始化请求。
        let barrier = Arc::new(Barrier::new(3));
        // first_config is one valid contender configuration.
        // first_config 是一个有效的竞争配置。
        let first_config = test_cache_config(31, 37, 41);
        // second_config is the other incompatible contender configuration.
        // second_config 是另一个不兼容的竞争配置。
        let second_config = test_cache_config(43, 47, 53);

        // first_thread submits the first configuration after the common barrier.
        // first_thread 在公共屏障后提交第一个配置。
        let first_thread = {
            // thread_cell shares the single initialization cell with the first contender.
            // thread_cell 与第一个竞争者共享唯一初始化单元。
            let thread_cell = cell.clone();
            // thread_barrier synchronizes the first contender with the other participants.
            // thread_barrier 将第一个竞争者与其他参与方同步。
            let thread_barrier = barrier.clone();
            // thread_config owns the first contender's exact requested configuration.
            // thread_config 持有第一个竞争者请求的确切配置。
            let thread_config = first_config.clone();
            thread::spawn(move || {
                thread_barrier.wait();
                configure_tool_cache_cell(&thread_cell, thread_config)
            })
        };
        // second_thread submits the second configuration after the common barrier.
        // second_thread 在公共屏障后提交第二个配置。
        let second_thread = {
            // thread_cell shares the single initialization cell with the second contender.
            // thread_cell 与第二个竞争者共享唯一初始化单元。
            let thread_cell = cell.clone();
            // thread_barrier synchronizes the second contender with the other participants.
            // thread_barrier 将第二个竞争者与其他参与方同步。
            let thread_barrier = barrier.clone();
            // thread_config owns the second contender's exact requested configuration.
            // thread_config 持有第二个竞争者请求的确切配置。
            let thread_config = second_config.clone();
            thread::spawn(move || {
                thread_barrier.wait();
                configure_tool_cache_cell(&thread_cell, thread_config)
            })
        };

        barrier.wait();
        // first_result is the first contender's completed initialization outcome.
        // first_result 是第一个竞争者完成后的初始化结果。
        let first_result = first_thread.join().expect("first contender must not panic");
        // second_result is the second contender's completed initialization outcome.
        // second_result 是第二个竞争者完成后的初始化结果。
        let second_result = second_thread
            .join()
            .expect("second contender must not panic");
        // winner_config is the single immutable configuration stored by the race winner.
        // winner_config 是竞态胜出者存入的唯一不可变配置。
        let winner_config = &cell
            .get()
            .expect("one contender must initialize the cell")
            .config;

        assert_ne!(first_result.is_ok(), second_result.is_ok());
        assert!(winner_config == &first_config || winner_config == &second_config);
        // conflict_error is the losing contender's stable diagnostic.
        // conflict_error 是失败竞争者的稳定诊断信息。
        let conflict_error = first_result
            .err()
            .or_else(|| second_result.err())
            .expect("one contender must report a conflict");
        assert!(conflict_error.starts_with(TOOL_CACHE_CONFIG_CONFLICT_ERROR));
    }

    /// Verify cache id timestamp conversion accepts normal post-epoch system times.
    /// 验证缓存编号时间戳转换会接受正常的 epoch 之后系统时间。
    #[test]
    fn cache_id_unix_millis_accepts_post_epoch_time() {
        // Timestamp one millisecond after the Unix epoch.
        // Unix epoch 之后一毫秒的时间戳。
        let timestamp = UNIX_EPOCH + Duration::from_millis(1);

        assert_eq!(
            system_time_to_cache_id_unix_millis(timestamp, "test cache id timestamp")
                .expect("post-epoch timestamp should convert"),
            1
        );
    }

    /// Verify cache id timestamp conversion rejects pre-epoch system times.
    /// 验证缓存编号时间戳转换会拒绝早于 epoch 的系统时间。
    #[test]
    fn cache_id_unix_millis_rejects_pre_epoch_time() {
        // Timestamp one millisecond before the Unix epoch.
        // Unix epoch 之前一毫秒的时间戳。
        let timestamp = UNIX_EPOCH - Duration::from_millis(1);

        // Error returned for a pre-epoch cache id timestamp conversion attempt.
        // 早于 epoch 的缓存编号时间戳转换尝试返回的错误。
        let error = system_time_to_cache_id_unix_millis(timestamp, "test cache id timestamp")
            .expect_err("pre-epoch timestamp should fail");

        assert!(
            error.starts_with(
                "test cache id timestamp is before Unix epoch and cannot be used for a tool cache id:"
            ),
            "unexpected error: {}",
            error
        );
    }

    /// Verify entries are isolated by tool namespace and cannot be read across scopes.
    /// 验证缓存条目按工具命名空间隔离，不能跨作用域读取。
    #[test]
    fn cache_entries_are_isolated_by_tool_name() {
        let cache = SharedToolCache::new(test_cache_config(10, 5, 5));
        // Cache id created under the first skill namespace.
        // 在第一个技能命名空间下创建的缓存编号。
        let cache_id = cache
            .create("skill-a", json!({"value": 1}), None)
            .expect("cache entry should be created");

        assert_eq!(cache.get("skill-a", &cache_id), Some(json!({"value": 1})));
        assert_eq!(cache.get("skill-b", &cache_id), None);
    }

    /// Verify pure misses and scope mismatches complete while another reader prevents write-lock escalation.
    /// 验证纯未命中与作用域不匹配会在另一个读者阻止写锁升级时正常完成。
    #[test]
    fn cache_nonexpired_misses_do_not_escalate_to_write_lock() {
        // Shared cache containing one live record used for both miss shapes.
        // 包含一个有效记录的共享缓存，用于两种未命中形态。
        let cache = Arc::new(SharedToolCache::new(test_cache_config(10, 5, 5)));
        // Existing cache id whose namespace will intentionally be queried incorrectly.
        // 将被有意使用错误命名空间查询的现有缓存编号。
        let cache_id = cache
            .create("skill-a", json!({"value": 1}), None)
            .expect("cache entry should be created");
        // Read guard that permits other readers but would block any attempted writer escalation.
        // 允许其他读者但会阻止任何写锁升级尝试的读保护。
        let read_guard = cache.read_store();
        // Result channel used to distinguish a completed read-only path from blocked escalation.
        // 用于区分已完成只读路径和被阻塞升级的结果通道。
        let (result_tx, result_rx) = mpsc::channel();
        // Cache clone transferred to the independent reader thread.
        // 传入独立读取线程的缓存副本。
        let worker_cache = Arc::clone(&cache);
        // Worker executes one pure miss and one live scope mismatch while the guard is retained.
        // 工作线程在读保护保留期间执行一次纯未命中和一次有效作用域不匹配。
        let worker = thread::spawn(move || {
            // Pair of read results that must both complete without a write lock.
            // 必须都在不获取写锁情况下完成的一对读取结果。
            let results = (
                worker_cache.get("skill-a", "missing-cache-id"),
                worker_cache.get("skill-b", &cache_id),
            );
            result_tx
                .send(results)
                .expect("cache miss results should be observed");
        });
        // Timed observation remains pending only if the implementation attempts a write lock.
        // 仅当实现尝试获取写锁时才会超时的观测结果。
        let observed = result_rx.recv_timeout(Duration::from_millis(500));
        drop(read_guard);
        worker.join().expect("cache miss worker should finish");

        assert_eq!(
            observed.expect("cache misses should not wait for a write lock"),
            (None, None)
        );
    }

    /// Verify cache entries expire according to the configured default TTL.
    /// 验证缓存条目会按照配置的默认 TTL 正常过期。
    #[test]
    fn cache_entries_expire_after_default_ttl() {
        let cache = SharedToolCache::new(test_cache_config(10, 1, 1));
        // Cache id expected to expire after the configured default TTL.
        // 预期会在配置默认 TTL 后过期的缓存编号。
        let cache_id = cache
            .create("skill-a", json!({"value": 1}), None)
            .expect("cache entry should be created");

        thread::sleep(Duration::from_millis(1100));

        assert_eq!(cache.get("skill-a", &cache_id), None);
    }

    /// Verify deleting a logically expired record returns false while removing its stale physical entry.
    /// 验证删除逻辑过期记录会返回 false，同时移除其陈旧物理条目。
    #[test]
    fn cache_delete_returns_false_for_expired_record() {
        // Cache owns one record whose expiry is forced without a real-time sleep.
        // Cache 持有一条无需真实等待即可强制过期的记录。
        let cache = SharedToolCache::new(test_cache_config(10, 5, 5));
        // CacheId identifies the exact record whose logical deletion contract is checked.
        // CacheId 标识用于检查逻辑删除契约的精确记录。
        let cache_id = cache
            .create("skill-a", json!({"value": 1}), None)
            .expect("cache entry should be created");
        {
            // StoreGuard mutates only the test record's monotonic expiry instant.
            // StoreGuard 只修改测试记录的单调过期时刻。
            let mut store_guard = cache.write_store();
            store_guard
                .entries
                .get_mut(&cache_id)
                .expect("cache entry should exist")
                .expires_at = Instant::now();
        }

        assert!(!cache.delete("skill-a", &cache_id));
        assert!(!cache.read_store().entries.contains_key(&cache_id));
    }

    /// Verify requested TTL values are clamped to the configured maximum TTL.
    /// 验证调用方请求的 TTL 会被正确限制到配置允许的最大值。
    #[test]
    fn cache_requested_ttl_is_clamped_to_maximum() {
        let cache = SharedToolCache::new(test_cache_config(10, 5, 1));
        // Cache id created with a caller TTL that should be clamped to the maximum.
        // 使用调用方 TTL 创建且预期会被限制到最大 TTL 的缓存编号。
        let cache_id = cache
            .create("skill-a", json!({"value": 1}), Some(60))
            .expect("cache entry should be created");

        thread::sleep(Duration::from_millis(1100));

        assert_eq!(cache.get("skill-a", &cache_id), None);
    }

    /// Verify the oldest cache entry is evicted when the capacity is exceeded.
    /// 验证缓存容量超限时会淘汰最早创建的条目。
    #[test]
    fn cache_evicts_oldest_entry_when_capacity_is_exceeded() {
        let cache = SharedToolCache::new(test_cache_config(2, 5, 5));
        // Oldest cache id expected to be evicted after capacity is exceeded.
        // 容量超限后预期会被淘汰的最早缓存编号。
        let first_id = cache
            .create("skill-a", json!({"value": 1}), None)
            .expect("first cache entry should be created");
        // Middle cache id expected to remain after evicting the oldest entry.
        // 淘汰最早条目后预期仍保留的中间缓存编号。
        let second_id = cache
            .create("skill-a", json!({"value": 2}), None)
            .expect("second cache entry should be created");
        // Newest cache id expected to remain after capacity enforcement.
        // 容量约束后预期仍保留的最新缓存编号。
        let third_id = cache
            .create("skill-a", json!({"value": 3}), None)
            .expect("third cache entry should be created");

        assert_eq!(cache.get("skill-a", &first_id), None);
        assert_eq!(cache.get("skill-a", &second_id), Some(json!({"value": 2})));
        assert_eq!(cache.get("skill-a", &third_id), Some(json!({"value": 3})));
    }

    /// Verify cache operations recover after one writer panics while holding the internal store lock.
    /// 验证某个写入者持有内部存储锁时 panic 后，缓存操作仍可恢复。
    #[test]
    fn cache_recovers_after_poisoned_write_lock() {
        // Cache instance used to verify every public operation after lock poisoning.
        // 用于验证锁 poison 后所有公开操作仍可工作的缓存实例。
        let cache = SharedToolCache::new(test_cache_config(4, 5, 5));

        // Captured panic result produced while the write guard is still alive.
        // 写保护仍存活时触发并捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to mark the cache lock as poisoned for this recovery test.
            // 仅用于为本恢复测试制造缓存锁 poison 的保护对象。
            let _guard = cache.store.write().expect("initial tool cache write lock");
            panic!("poison tool cache for recovery test");
        }));

        assert!(poison_result.is_err());

        // Cache id created after poisoning, proving write recovery is effective.
        // poison 后创建的缓存编号，用于证明写恢复有效。
        let cache_id = cache
            .create("skill-a", json!({"value": 1}), None)
            .expect("cache entry should be created after poison recovery");

        assert_eq!(cache.get("skill-a", &cache_id), Some(json!({"value": 1})));
        assert!(cache.delete("skill-a", &cache_id));
        assert_eq!(cache.get("skill-a", &cache_id), None);
    }
}
