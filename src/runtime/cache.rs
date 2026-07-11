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
#[derive(Clone, Debug, Serialize, Deserialize)]
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

        // Writable cache store guard used for cleanup, insertion, and capacity enforcement.
        // 用于清理、插入和容量约束的可写缓存存储保护对象。
        let mut store = self.write_store();
        self.cleanup_expired_locked(&mut store, now);
        store.entries.insert(cache_id.clone(), entry);
        self.enforce_capacity_locked(&mut store);
        Ok(cache_id)
    }

    /// Read a cached entry by tool name and cache id; expired hits are removed and returned as empty.
    /// 按工具名和缓存编号读取缓存；命中但已过期时会自动删除并返回空。
    pub fn get(&self, tool_name: &str, cache_id: &str) -> Option<Value> {
        let now = Instant::now();

        {
            let store = self.read_store();
            if let Some(entry) = store.entries.get(cache_id)
                && entry.tool_name == tool_name
                && entry.expires_at > now
            {
                return Some(entry.value.clone());
            }
        }

        let mut store = self.write_store();
        self.cleanup_expired_locked(&mut store, now);
        match store.entries.get(cache_id) {
            Some(entry) if entry.tool_name == tool_name && entry.expires_at > now => {
                Some(entry.value.clone())
            }
            _ => None,
        }
    }

    /// Delete one cache entry under the given tool namespace and return whether an entry was actually removed.
    /// 删除指定工具名下的缓存条目；返回是否确实删除了条目。
    pub fn delete(&self, tool_name: &str, cache_id: &str) -> bool {
        let mut store = self.write_store();
        self.cleanup_expired_locked(&mut store, Instant::now());
        if let Some(entry) = store.entries.get(cache_id)
            && entry.tool_name == tool_name
        {
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

static GLOBAL_TOOL_CACHE: OnceLock<Arc<SharedToolCache>> = OnceLock::new();

/// Initialize the global shared cache, typically once during process startup.
/// 初始化全局共享缓存；通常在主程序启动时执行一次。
pub fn configure_global_tool_cache(config: ToolCacheConfig) {
    let _ = GLOBAL_TOOL_CACHE.set(Arc::new(SharedToolCache::new(config)));
}

/// Get the global shared cache, lazily creating it with default settings if startup did not configure it explicitly.
/// 获取全局共享缓存；若尚未初始化则使用默认配置惰性创建。
pub fn global_tool_cache() -> Arc<SharedToolCache> {
    GLOBAL_TOOL_CACHE
        .get_or_init(|| Arc::new(SharedToolCache::new(ToolCacheConfig::default())))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::{SharedToolCache, ToolCacheConfig, system_time_to_cache_id_unix_millis};
    use serde_json::json;
    use std::panic::{self, AssertUnwindSafe};
    use std::thread;
    use std::time::{Duration, UNIX_EPOCH};

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
