//! Synchronous shared file-watcher foundation adapted from Vulcan Code.
//! 从 Vulcan Code 适配的同步共享文件监听基础层。
//!
//! The path registry, reference counting, nearest-existing-ancestor fallback, and
//! registration lifecycle are adapted from `vulcan-file-watcher` under the MIT
//! License, Copyright (c) 2026 OpenVulcan.
//! 路径注册表、引用计数、最近存在祖先回退与注册生命周期基于 MIT 许可的
//! `vulcan-file-watcher` 适配，Copyright (c) 2026 OpenVulcan。

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

/// One requested or actual native watch path.
/// 单个请求或实际原生监听路径。
#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveWatchPath {
    /// Filesystem path owned by this watch record.
    /// 当前监听记录拥有的文件系统路径。
    path: PathBuf,
    /// Whether the native backend must watch descendants recursively.
    /// 原生后端是否必须递归监听后代路径。
    recursive: bool,
}

/// Reference counts for one actual backend path.
/// 单个实际后端路径的引用计数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PathWatchCounts {
    /// Number of non-recursive registrations.
    /// 非递归注册数量。
    non_recursive: usize,
    /// Number of recursive registrations.
    /// 递归注册数量。
    recursive: usize,
}

impl PathWatchCounts {
    /// Increase the matching reference-count bucket.
    /// 增加匹配的引用计数桶。
    fn increment(&mut self, recursive: bool) {
        if recursive {
            self.recursive += 1;
        } else {
            self.non_recursive += 1;
        }
    }

    /// Decrease the matching reference-count bucket.
    /// 减少匹配的引用计数桶。
    fn decrement(&mut self, recursive: bool) {
        if recursive {
            self.recursive = self.recursive.saturating_sub(1);
        } else {
            self.non_recursive = self.non_recursive.saturating_sub(1);
        }
    }

    /// Return the effective native mode required by current registrations.
    /// 返回当前注册所需的有效原生监听模式。
    fn effective_mode(self) -> Option<RecursiveMode> {
        if self.recursive > 0 {
            Some(RecursiveMode::Recursive)
        } else if self.non_recursive > 0 {
            Some(RecursiveMode::NonRecursive)
        } else {
            None
        }
    }

    /// Return whether both reference-count buckets are empty.
    /// 返回两个引用计数桶是否均为空。
    fn is_empty(self) -> bool {
        self.non_recursive == 0 && self.recursive == 0
    }
}

/// Native notify backend and its currently installed path modes.
/// 原生 notify 后端及其当前安装的路径模式。
struct LiveFileWatcherBackend {
    /// Single native watcher shared by every logical registration.
    /// 所有逻辑注册共享的单个原生监听器。
    watcher: RecommendedWatcher,
    /// Effective native modes installed for canonical backend paths.
    /// 为规范后端路径安装的有效原生模式。
    watched_paths: HashMap<PathBuf, RecursiveMode>,
}

/// One logical registration retained in the shared registry.
/// 共享注册表中保留的单个逻辑注册。
#[derive(Clone, Debug)]
struct LiveWatchRegistrationState {
    /// Original absolute path requested by the consumer.
    /// 消费方请求的原始绝对路径。
    requested: LiveWatchPath,
    /// Current existing path registered with the native backend.
    /// 当前向原生后端注册的已存在路径。
    actual: LiveWatchPath,
    /// Whether the current backend path is an ancestor fallback.
    /// 当前后端路径是否为祖先回退路径。
    fallback: bool,
}

/// Shared logical registration state.
/// 共享逻辑注册状态。
#[derive(Debug, Default)]
struct LiveFileWatcherRegistry {
    /// Next monotonically increasing registration identifier.
    /// 下一个单调递增的注册标识符。
    next_registration_id: u64,
    /// Logical registrations keyed by their stable identifier.
    /// 按稳定标识符索引的逻辑注册。
    registrations: HashMap<u64, LiveWatchRegistrationState>,
    /// Reference counts keyed by actual backend path.
    /// 按实际后端路径索引的引用计数。
    path_ref_counts: HashMap<PathBuf, PathWatchCounts>,
}

/// Shared ownership object containing backend and registry locks.
/// 包含后端锁与注册表锁的共享所有权对象。
struct SharedLiveFileWatcherInner {
    /// Single native backend protected for registration changes.
    /// 为注册变更提供保护的单个原生后端。
    backend: Mutex<LiveFileWatcherBackend>,
    /// Logical path registry protected for reference-count changes.
    /// 为引用计数变更提供保护的逻辑路径注册表。
    registry: Mutex<LiveFileWatcherRegistry>,
}

/// Synchronous shared watcher that emits raw native events through one channel.
/// 通过单一通道发送原生事件的同步共享监听器。
#[derive(Clone)]
pub(crate) struct SharedLiveFileWatcher {
    /// Shared backend and registry ownership.
    /// 共享后端与注册表所有权。
    inner: Arc<SharedLiveFileWatcherInner>,
}

impl std::fmt::Debug for SharedLiveFileWatcher {
    /// Render stable diagnostics without exposing notify backend internals.
    /// 在不暴露 notify 后端内部状态的情况下渲染稳定诊断。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedLiveFileWatcher")
            .field("registration_count", &self.registration_count())
            .field("backend_watch_path_count", &self.backend_watch_path_count())
            .finish()
    }
}

/// RAII handle that unregisters one logical path exactly once on drop.
/// 在析构时恰好注销一次逻辑路径的 RAII 句柄。
#[derive(Debug)]
pub(crate) struct LiveWatchRegistration {
    /// Stable logical registration identifier.
    /// 稳定的逻辑注册标识符。
    registration_id: u64,
    /// Weak owner reference that avoids a registration-owner cycle.
    /// 避免注册与所有者循环的弱所有者引用。
    owner: Weak<SharedLiveFileWatcherInner>,
}

impl Drop for LiveWatchRegistration {
    /// Remove this logical registration and release its native path reference.
    /// 移除当前逻辑注册并释放其原生路径引用。
    fn drop(&mut self) {
        // Upgraded shared owner used only while unregistering this handle.
        // 仅在注销当前句柄期间使用的已升级共享所有者。
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let _unregister_result = unregister_path(owner.as_ref(), self.registration_id);
    }
}

impl SharedLiveFileWatcher {
    /// Create one native watcher and return its single raw event receiver.
    /// 创建一个原生监听器并返回其唯一原始事件接收器。
    pub(crate) fn new() -> Result<(Self, Receiver<notify::Result<Event>>), String> {
        // Raw native event channel shared by the notify callback and synchronous worker.
        // notify 回调与同步工作线程共享的原生事件通道。
        let (event_tx, event_rx) = mpsc::channel::<notify::Result<Event>>();
        // Native watcher callback that performs no business work or blocking refresh.
        // 不执行业务工作或阻塞刷新的原生监听器回调。
        let watcher = notify::recommended_watcher(move |event| {
            let _send_result = event_tx.send(event);
        })
        .map_err(|error| format!("CONFIG_WATCHER_FAILED: {error}"))?;
        // Shared inner owner initialized with one backend and an empty registry.
        // 使用单个后端和空注册表初始化的共享内部所有者。
        let inner = Arc::new(SharedLiveFileWatcherInner {
            backend: Mutex::new(LiveFileWatcherBackend {
                watcher,
                watched_paths: HashMap::new(),
            }),
            registry: Mutex::new(LiveFileWatcherRegistry::default()),
        });
        Ok((Self { inner }, event_rx))
    }

    /// Register one absolute logical path and return an automatic unregister handle.
    /// 注册一个绝对逻辑路径并返回自动注销句柄。
    pub(crate) fn register_path(
        &self,
        path: PathBuf,
        recursive: bool,
    ) -> Result<LiveWatchRegistration, String> {
        // Absolute requested path prevents cwd-dependent registration behavior.
        // 绝对请求路径用于避免依赖当前工作目录的注册行为。
        let requested_path = std::path::absolute(&path).map_err(|error| {
            format!(
                "CONFIG_WATCHER_FAILED: failed to normalize watch path '{}': {error}",
                path.display()
            )
        })?;
        // Logical watch request retained for fallback recalculation.
        // 为回退重算保留的逻辑监听请求。
        let requested = LiveWatchPath {
            path: requested_path,
            recursive,
        };
        // Current actual backend path and fallback status.
        // 当前实际后端路径与回退状态。
        let (actual, fallback) = actual_watch_path(&requested);
        // Registry guard establishes the stable registration identifier and counts.
        // 注册表保护对象用于建立稳定注册标识符与计数。
        let mut registry = lock_registry(self.inner.as_ref());
        // Monotonic identifier allocated before backend reconfiguration.
        // 在后端重配置前分配的单调标识符。
        let registration_id = registry.next_registration_id;
        registry.next_registration_id = registry.next_registration_id.saturating_add(1);
        // Previous and next native modes for this actual path.
        // 当前实际路径的前一与后一原生模式。
        let counts = registry
            .path_ref_counts
            .entry(actual.path.clone())
            .or_default();
        let previous_mode = counts.effective_mode();
        counts.increment(actual.recursive);
        let next_mode = counts.effective_mode();
        if previous_mode != next_mode {
            // Backend guard applies the minimum required mode transition.
            // 后端保护对象用于应用最小必要模式转换。
            let mut backend = lock_backend(self.inner.as_ref());
            if let Err(error) =
                reconfigure_watch(&mut backend, &actual.path, previous_mode, next_mode)
            {
                counts.decrement(actual.recursive);
                if counts.is_empty() {
                    registry.path_ref_counts.remove(&actual.path);
                }
                return Err(error);
            }
        }
        registry.registrations.insert(
            registration_id,
            LiveWatchRegistrationState {
                requested,
                actual,
                fallback,
            },
        );
        Ok(LiveWatchRegistration {
            registration_id,
            owner: Arc::downgrade(&self.inner),
        })
    }

    /// Re-evaluate ancestor fallbacks after filesystem topology changes.
    /// 文件系统拓扑变化后重新计算祖先回退。
    pub(crate) fn refresh_fallback_paths(&self) -> Result<(), String> {
        // Registry guard serializes fallback moves with registration drops.
        // 注册表保护对象用于串行化回退迁移与注册析构。
        let mut registry = lock_registry(self.inner.as_ref());
        // Registration identifiers whose fallback path may now be closer.
        // 回退路径当前可能更接近目标的注册标识符。
        let candidate_ids = registry
            .registrations
            .iter()
            .filter_map(|(registration_id, state)| state.fallback.then_some(*registration_id))
            .collect::<Vec<_>>();
        for registration_id in candidate_ids {
            // Stable snapshot avoids holding a mutable entry across count-map updates.
            // 稳定快照用于避免在更新计数映射时持续借用可变条目。
            let Some(current) = registry.registrations.get(&registration_id).cloned() else {
                continue;
            };
            // Recomputed actual path and fallback status for the current topology.
            // 基于当前拓扑重算的实际路径与回退状态。
            let (next_actual, next_fallback) = actual_watch_path(&current.requested);
            if current.actual == next_actual {
                if let Some(state) = registry.registrations.get_mut(&registration_id) {
                    state.fallback = next_fallback;
                }
                continue;
            }
            move_actual_watch(
                self.inner.as_ref(),
                &mut registry,
                &current.actual,
                &next_actual,
            )?;
            if let Some(state) = registry.registrations.get_mut(&registration_id) {
                state.actual = next_actual;
                state.fallback = next_fallback;
            }
        }
        Ok(())
    }

    /// Return the number of active logical registrations.
    /// 返回活动逻辑注册数量。
    fn registration_count(&self) -> usize {
        lock_registry(self.inner.as_ref()).registrations.len()
    }

    /// Return the number of actual native backend paths.
    /// 返回实际原生后端路径数量。
    fn backend_watch_path_count(&self) -> usize {
        lock_backend(self.inner.as_ref()).watched_paths.len()
    }

    /// Return backend-path count for lifecycle tests.
    /// 返回供生命周期测试使用的后端路径数量。
    #[cfg(test)]
    pub(crate) fn backend_watch_path_count_for_test(&self) -> usize {
        self.backend_watch_path_count()
    }
}

/// Recover the shared registry guard after poisoning.
/// 在锁中毒后恢复共享注册表保护对象。
fn lock_registry(inner: &SharedLiveFileWatcherInner) -> MutexGuard<'_, LiveFileWatcherRegistry> {
    inner
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Recover the native backend guard after poisoning.
/// 在锁中毒后恢复原生后端保护对象。
fn lock_backend(inner: &SharedLiveFileWatcherInner) -> MutexGuard<'_, LiveFileWatcherBackend> {
    inner
        .backend
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Reconfigure one native path while preserving the previous mode on failure.
/// 重配置单个原生路径，并在失败时保留先前模式。
fn reconfigure_watch(
    backend: &mut LiveFileWatcherBackend,
    path: &Path,
    previous_mode: Option<RecursiveMode>,
    next_mode: Option<RecursiveMode>,
) -> Result<(), String> {
    if previous_mode == next_mode {
        return Ok(());
    }
    if previous_mode.is_some() {
        backend
            .watcher
            .unwatch(path)
            .map_err(|error| format!("CONFIG_WATCHER_FAILED: {error}"))?;
        backend.watched_paths.remove(path);
    }
    if let Some(next_mode) = next_mode {
        if let Err(error) = backend.watcher.watch(path, next_mode) {
            if let Some(previous_mode) = previous_mode {
                let _restore_result = backend.watcher.watch(path, previous_mode);
                backend
                    .watched_paths
                    .insert(path.to_path_buf(), previous_mode);
            }
            return Err(format!("CONFIG_WATCHER_FAILED: {error}"));
        }
        backend.watched_paths.insert(path.to_path_buf(), next_mode);
    }
    Ok(())
}

/// Move one registration reference between actual backend paths.
/// 在实际后端路径之间迁移单个注册引用。
fn move_actual_watch(
    inner: &SharedLiveFileWatcherInner,
    registry: &mut LiveFileWatcherRegistry,
    previous: &LiveWatchPath,
    next: &LiveWatchPath,
) -> Result<(), String> {
    // Previous path modes surrounding one reference removal.
    // 移除单个引用前后的旧路径模式。
    let (previous_old_mode, previous_next_mode) = {
        let counts = registry
            .path_ref_counts
            .get_mut(&previous.path)
            .expect("registered actual path must have reference counts");
        let old_mode = counts.effective_mode();
        counts.decrement(previous.recursive);
        (old_mode, counts.effective_mode())
    };
    // Next path modes surrounding one reference addition.
    // 增加单个引用前后的新路径模式。
    let (next_old_mode, next_next_mode) = {
        let counts = registry
            .path_ref_counts
            .entry(next.path.clone())
            .or_default();
        let old_mode = counts.effective_mode();
        counts.increment(next.recursive);
        (old_mode, counts.effective_mode())
    };
    // Backend guard applies both transitions as one serialized move.
    // 后端保护对象用于把两个转换作为一次串行迁移应用。
    let mut backend = lock_backend(inner);
    if previous_old_mode != previous_next_mode {
        reconfigure_watch(
            &mut backend,
            &previous.path,
            previous_old_mode,
            previous_next_mode,
        )?;
    }
    if next_old_mode != next_next_mode
        && let Err(error) =
            reconfigure_watch(&mut backend, &next.path, next_old_mode, next_next_mode)
    {
        // Restored count state keeps logical and backend ownership aligned.
        // 恢复后的计数状态用于保持逻辑与后端所有权一致。
        if let Some(counts) = registry.path_ref_counts.get_mut(&next.path) {
            counts.decrement(next.recursive);
            if counts.is_empty() {
                registry.path_ref_counts.remove(&next.path);
            }
        }
        let counts = registry
            .path_ref_counts
            .entry(previous.path.clone())
            .or_default();
        counts.increment(previous.recursive);
        let _restore_result = reconfigure_watch(
            &mut backend,
            &previous.path,
            previous_next_mode,
            previous_old_mode,
        );
        return Err(error);
    }
    if registry
        .path_ref_counts
        .get(&previous.path)
        .is_some_and(|counts| counts.is_empty())
    {
        registry.path_ref_counts.remove(&previous.path);
    }
    Ok(())
}

/// Remove one logical registration and release its backend reference.
/// 移除单个逻辑注册并释放其后端引用。
fn unregister_path(inner: &SharedLiveFileWatcherInner, registration_id: u64) -> Result<(), String> {
    // Registry guard serializes the final reference transition.
    // 注册表保护对象用于串行化最后一次引用转换。
    let mut registry = lock_registry(inner);
    // Removed state identifies the exact actual path reference to release.
    // 被移除状态用于标识需要释放的精确实际路径引用。
    let Some(state) = registry.registrations.remove(&registration_id) else {
        return Ok(());
    };
    // Previous and next effective modes surrounding the decrement.
    // 引用递减前后的有效模式。
    let Some(counts) = registry.path_ref_counts.get_mut(&state.actual.path) else {
        return Ok(());
    };
    let previous_mode = counts.effective_mode();
    counts.decrement(state.actual.recursive);
    let next_mode = counts.effective_mode();
    if previous_mode != next_mode {
        // Backend guard releases or downgrades the physical watch.
        // 后端保护对象用于释放或降级物理监听。
        let mut backend = lock_backend(inner);
        reconfigure_watch(&mut backend, &state.actual.path, previous_mode, next_mode)?;
    }
    if counts.is_empty() {
        registry.path_ref_counts.remove(&state.actual.path);
    }
    Ok(())
}

/// Resolve one requested path to itself or its nearest existing ancestor.
/// 将单个请求路径解析为自身或最近存在的祖先。
fn actual_watch_path(requested: &LiveWatchPath) -> (LiveWatchPath, bool) {
    if requested.path.exists() {
        // Canonical existing path avoids duplicate backend registrations.
        // 规范化已存在路径用于避免重复后端注册。
        let actual_path = requested
            .path
            .canonicalize()
            .unwrap_or_else(|_| requested.path.clone());
        return (
            LiveWatchPath {
                path: actual_path,
                recursive: requested.recursive,
            },
            false,
        );
    }
    // Ancestor cursor searches upward from the missing path's parent.
    // 祖先游标从缺失路径的父路径开始向上查找。
    let mut ancestor = requested.path.parent();
    while let Some(path) = ancestor {
        if path.is_dir() {
            // Canonical fallback path shares equivalent ancestor registrations.
            // 规范回退路径用于共享等价祖先注册。
            let actual_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            return (
                LiveWatchPath {
                    path: actual_path,
                    recursive: false,
                },
                true,
            );
        }
        ancestor = path.parent();
    }
    (requested.clone(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Create one unique absolute temporary root for watcher lifecycle tests.
    /// 为监听生命周期测试创建一个唯一绝对临时根目录。
    fn unique_temp_root(label: &str) -> PathBuf {
        // Process-wide sequence prevents collisions within one test binary.
        // 进程级序列用于避免同一测试二进制内的路径冲突。
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        // Stable unique suffix combines process and monotonic identifiers.
        // 稳定唯一后缀组合进程标识符与单调标识符。
        let suffix = format!(
            "luaskills_shared_watcher_{label}_{}_{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        std::env::temp_dir().join(suffix)
    }

    /// Verify duplicate logical registrations share one native path and unregister by Drop.
    /// 验证重复逻辑注册共享一个原生路径并通过 Drop 注销。
    #[test]
    fn duplicate_registrations_share_one_backend_path() {
        // Existing directory registered twice through the shared path registry.
        // 通过共享路径注册表注册两次的已存在目录。
        let root = unique_temp_root("dedupe");
        fs::create_dir_all(&root).expect("create watcher test root");
        // One native watcher instance and its unused raw receiver.
        // 单个原生监听器实例及其未使用原始接收器。
        let (watcher, _event_rx) = SharedLiveFileWatcher::new().expect("create shared watcher");
        // Two logical handles targeting the same physical directory.
        // 指向同一物理目录的两个逻辑句柄。
        let first = watcher
            .register_path(root.clone(), false)
            .expect("register first path");
        let second = watcher
            .register_path(root.clone(), false)
            .expect("register duplicate path");
        assert_eq!(watcher.registration_count(), 2);
        assert_eq!(watcher.backend_watch_path_count_for_test(), 1);
        drop(first);
        assert_eq!(watcher.registration_count(), 1);
        assert_eq!(watcher.backend_watch_path_count_for_test(), 1);
        drop(second);
        assert_eq!(watcher.registration_count(), 0);
        assert_eq!(watcher.backend_watch_path_count_for_test(), 0);
        let _cleanup_result = fs::remove_dir_all(root);
    }

    /// Verify a missing path moves from its nearest ancestor when the target appears.
    /// 验证缺失路径在目标出现后会从最近祖先迁移。
    #[test]
    fn missing_path_fallback_moves_to_created_target() {
        // Existing root and missing nested target used to exercise fallback migration.
        // 用于验证回退迁移的已存在根目录与缺失嵌套目标。
        let root = unique_temp_root("fallback");
        let target = root.join("nested").join("config");
        fs::create_dir_all(&root).expect("create fallback root");
        // Shared watcher and registration initially backed by the existing root.
        // 初始由已存在根目录承载的共享监听器与注册。
        let (watcher, _event_rx) = SharedLiveFileWatcher::new().expect("create shared watcher");
        let registration = watcher
            .register_path(target.clone(), false)
            .expect("register missing target");
        assert_eq!(watcher.backend_watch_path_count_for_test(), 1);
        fs::create_dir_all(&target).expect("create requested target");
        watcher
            .refresh_fallback_paths()
            .expect("move fallback to target");
        assert_eq!(watcher.backend_watch_path_count_for_test(), 1);
        drop(registration);
        assert_eq!(watcher.backend_watch_path_count_for_test(), 0);
        let _cleanup_result = fs::remove_dir_all(root);
    }
}
