use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread;
use std::time::Duration;

use crate::runtime::managed_package::{
    ManagedFilesystemObjectIdentity, ManagedRuntimePackageContext,
    capture_managed_directory_identity, validate_managed_directory_identity,
};
use crate::runtime::path::{host_process_path_argument, render_host_visible_path};
use crate::skill::dependencies::{
    NodeRuntimeDependencySpec, NodeRuntimePackageManager, PythonRuntimeDependencySpec,
    PythonRuntimePackageManager,
};

/// Schema version used by managed runtime environment markers.
/// 受管运行时环境标记文件使用的 schema 版本。
pub const MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION: u32 = 1;

/// Stable reason code returned when persistent sessions are queried on Windows ARM.
/// 在 Windows ARM 上查询持久会话能力时返回的稳定原因码。
pub const WINDOWS_ARM_PERSISTENT_SESSION_UNSUPPORTED_REASON: &str = "windows_arm_is_not_supported";

/// Machine-readable capability for persistent managed Python and Node sessions.
/// 受管 Python 与 Node 持久会话的机器可读能力。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedRuntimePersistentSessionCapability {
    /// Whether `session.open` is supported without creating any runtime resources first.
    /// 是否可在不预先创建任何运行时资源的情况下支持 `session.open`。
    pub supported: bool,
    /// Rust target operating-system identifier for the current binary.
    /// 当前二进制的 Rust 目标操作系统标识。
    pub target_os: String,
    /// Rust target CPU-architecture identifier for the current binary.
    /// 当前二进制的 Rust 目标 CPU 架构标识。
    pub target_arch: String,
    /// Stable unsupported reason code; absent for supported targets.
    /// 稳定的不支持原因码；受支持目标上不存在。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Return the current target's persistent managed-session capability without side effects.
/// 无副作用地返回当前目标的受管持久会话能力。
///
/// This function accepts no parameters and performs no environment, snapshot, or process work.
/// 本函数不接收参数，也不会执行环境、快照或进程操作。
///
/// Returns an unsupported result only for Windows ARM among officially supported target families.
/// 在官方支持的目标族中，仅 Windows ARM 返回不支持结果。
pub fn current_managed_runtime_persistent_session_capability()
-> ManagedRuntimePersistentSessionCapability {
    // Windows ARM is the sole explicitly unsupported target and must fail before resource creation.
    // Windows ARM 是唯一明确不支持的目标，且必须在资源创建前失败。
    let windows_arm = cfg!(all(windows, target_arch = "aarch64"));
    // Linux/macOS share descriptor-anchored primitives; non-ARM Windows uses native handles.
    // Linux/macOS 共享描述符锚定原语；非 ARM Windows 使用原生句柄。
    let supported_family = cfg!(any(target_os = "linux", target_os = "macos"))
        || cfg!(all(windows, not(target_arch = "aarch64")));
    ManagedRuntimePersistentSessionCapability {
        supported: supported_family && !windows_arm,
        target_os: env::consts::OS.to_string(),
        target_arch: env::consts::ARCH.to_string(),
        reason: windows_arm.then(|| WINDOWS_ARM_PERSISTENT_SESSION_UNSUPPORTED_REASON.to_string()),
    }
}

/// Tests for the stable persistent-session platform capability contract.
/// 稳定持久会话平台能力契约测试。
#[cfg(test)]
mod persistent_session_capability_tests {
    use super::current_managed_runtime_persistent_session_capability;

    /// Verify every supported production target reports an enabled structured capability.
    /// 验证每个受支持生产目标都会报告已启用的结构化能力。
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        all(windows, not(target_arch = "aarch64"))
    ))]
    #[test]
    fn supported_target_reports_persistent_sessions() {
        let capability = current_managed_runtime_persistent_session_capability();

        assert!(capability.supported);
        assert_eq!(capability.target_os, std::env::consts::OS);
        assert_eq!(capability.target_arch, std::env::consts::ARCH);
        assert_eq!(capability.reason, None);
    }

    /// Verify Windows ARM reports the sole stable unsupported reason without ambiguity.
    /// 验证 Windows ARM 会无歧义地报告唯一稳定的不支持原因。
    #[cfg(all(windows, target_arch = "aarch64"))]
    #[test]
    fn windows_arm_reports_stable_unsupported_reason() {
        let capability = current_managed_runtime_persistent_session_capability();

        assert!(!capability.supported);
        assert_eq!(capability.target_os, "windows");
        assert_eq!(capability.target_arch, "aarch64");
        assert_eq!(
            capability.reason.as_deref(),
            Some(super::WINDOWS_ARM_PERSISTENT_SESSION_UNSUPPORTED_REASON)
        );
    }
}

/// Process-local sequence used to isolate unpublished environment build directories.
/// 用于隔离未发布环境构建目录的进程内序号。
static NEXT_MANAGED_RUNTIME_BUILD_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Maximum atomic build-directory attempts used to bypass stale crash remnants.
/// 用于绕过崩溃残留的最大原子构建目录尝试次数。
const MAX_MANAGED_RUNTIME_BUILD_DIR_ATTEMPTS: usize = 64;
/// Maximum replaced-environment backups retained by the process-wide recovery worker.
/// 进程级恢复 Worker 最多保留的已替换环境备份数量。
const MAX_MANAGED_RUNTIME_BACKUP_RECOVERY_RECORDS: usize = 1_024;
/// Fixed delay between failed replaced-environment recovery attempts.
/// 已替换环境恢复失败后的固定重试间隔。
const MANAGED_RUNTIME_BACKUP_RECOVERY_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Minimum Node.js version supported by the asynchronous ESM worker protocol.
/// 异步 ESM Worker 协议支持的最低 Node.js 版本。
const MIN_MANAGED_NODE_RUNTIME_VERSION: &str = "22.0.0";

/// Process-wide durable owner for replaced managed-environment recovery records.
/// 已替换受管环境恢复记录的进程级持久所有者。
struct ManagedRuntimeBackupRecoveryCenter {
    /// Recovery records retained across transient filesystem failures.
    /// 跨瞬态文件系统失败保留的恢复记录。
    records: Mutex<VecDeque<ManagedRuntimeBackupRecoveryRecord>>,
    /// Notification waking the single recovery worker after one record is published.
    /// 单个记录发布后唤醒唯一恢复 Worker 的通知量。
    changed: Condvar,
    /// Total reserved records, including records still owned by a publication transaction.
    /// 已预留记录总数，包括仍由发布事务拥有的记录。
    reserved: Arc<AtomicUsize>,
}

/// One pre-reserved capacity permit guaranteeing lossless recovery handoff.
/// 保证恢复任务无损交接的单个预留容量许可。
struct ManagedRuntimeBackupRecoveryPermit {
    /// Shared reservation counter decremented only after final recovery completion.
    /// 仅在最终恢复完成后递减的共享预留计数器。
    reserved: Arc<AtomicUsize>,
}

impl Drop for ManagedRuntimeBackupRecoveryPermit {
    /// Release one replaced-environment recovery reservation.
    /// 释放单个已替换环境恢复预留。
    fn drop(&mut self) {
        self.reserved.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Required recovery action after an environment replacement transaction leaves one backup.
/// 环境替换事务留下单个备份后所需的恢复动作。
enum ManagedRuntimeBackupRecoveryAction {
    /// Delete an obsolete backup after the new environment was committed successfully.
    /// 新环境成功提交后删除已过时备份。
    RemoveCommittedBackup,
    /// Restore the old environment name after publication and synchronous rollback both failed.
    /// 发布与同步回滚均失败后恢复旧环境名称。
    RestoreFailedPublication {
        /// Exact plan whose stable environment name must be restored or superseded safely.
        /// 必须被安全恢复或替代的稳定环境名称对应精确计划。
        plan: Box<ManagedRuntimeEnvPlan>,
    },
}

/// Identity-bound replaced-environment backup retained until recovery succeeds.
/// 通过身份绑定并保留到恢复成功的已替换环境备份。
struct ManagedRuntimeBackupRecoveryRecord {
    /// Unique same-parent backup path created by the publication transaction.
    /// 发布事务创建的唯一同父级备份路径。
    backup_path: PathBuf,
    /// Native identity captured before the original environment name was moved.
    /// 原环境名称移动前捕获的平台原生身份。
    identity: ManagedFilesystemObjectIdentity,
    /// Recovery action selected from the transaction commit state.
    /// 根据事务提交状态选择的恢复动作。
    action: ManagedRuntimeBackupRecoveryAction,
    /// Capacity ownership retained through every retry.
    /// 跨越每次重试保留的容量所有权。
    _permit: ManagedRuntimeBackupRecoveryPermit,
    /// Whether the first persistent failure has already been logged.
    /// 是否已经记录首次持久失败。
    error_reported: bool,
}

/// Cross-process advisory file lock held for one exact managed environment construction.
/// 为单个精确受管环境构建持有的跨进程建议文件锁。
struct ManagedRuntimeEnvFileLock {
    /// Open lock-file handle whose lifetime owns the operating-system lock.
    /// 其生命周期拥有操作系统锁的已打开锁文件句柄。
    file: File,
    /// Stable host-visible path used only for unlock diagnostics.
    /// 仅用于解锁诊断的稳定宿主可见路径。
    path: PathBuf,
}

impl Drop for ManagedRuntimeEnvFileLock {
    /// Release the operating-system environment lock without panicking during teardown.
    /// 在清理期间释放操作系统环境锁且不触发 panic。
    fn drop(&mut self) {
        if let Err(error) = unlock_managed_runtime_env_file(&self.file) {
            crate::runtime_logging::warn(format!(
                "[LuaSkill:warn] failed to unlock managed runtime environment {}: {error}",
                render_host_visible_path(&self.path)
            ));
        }
    }
}

/// Shared cross-process lifecycle lease retained while one managed environment is in active use.
/// 当一个受管环境处于活动使用状态时保留的跨进程共享生命周期租约。
#[derive(Debug)]
pub(crate) struct ManagedRuntimeEnvLease {
    /// Operating-system lease guard whose lifetime prevents environment publication or replacement.
    /// 其生命周期阻止环境发布或替换的操作系统租约保护器。
    _guard: ManagedRuntimeEnvLeaseFileLock,
}

/// Operating-system lock owner shared by active and publishing environment lease modes.
/// 由活动环境租约模式与发布环境租约模式共同使用的操作系统锁所有者。
#[derive(Debug)]
struct ManagedRuntimeEnvLeaseFileLock {
    /// Open lease-file handle whose lifetime owns the selected shared or exclusive lock.
    /// 其生命周期拥有选定共享锁或独占锁的已打开租约文件句柄。
    file: File,
    /// Stable host-visible lease path used only for unlock diagnostics.
    /// 仅用于解锁诊断的稳定宿主可见租约路径。
    path: PathBuf,
}

impl Drop for ManagedRuntimeEnvLeaseFileLock {
    /// Release one lifecycle lease without panicking during environment or process teardown.
    /// 在环境或进程清理期间释放一个生命周期租约且不触发 panic。
    fn drop(&mut self) {
        if let Err(error) = unlock_managed_runtime_env_lease_file(&self.file) {
            crate::runtime_logging::warn(format!(
                "[LuaSkill:warn] failed to unlock managed runtime environment lifecycle lease {}: {error}",
                render_host_visible_path(&self.path)
            ));
        }
    }
}

/// Return the process-wide weak registry of per-environment construction locks.
/// 返回进程级每环境构建锁弱引用注册表。
fn managed_runtime_env_lock_registry() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<()>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return one shared construction lock for the exact resolved environment directory.
/// 返回精确已解析环境目录对应的共享构建锁。
fn managed_runtime_env_lock(env_dir: &Path) -> Arc<Mutex<()>> {
    // Registry guard recovered after poisoning so one failed builder cannot wedge all environments.
    // 在锁中毒后恢复注册表保护，避免单个失败构建器阻塞全部环境。
    let mut registry = managed_runtime_env_lock_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = registry.get(env_dir).and_then(Weak::upgrade) {
        return lock;
    }
    // Fresh lock published as a weak entry so unused environment identities do not leak forever.
    // 以弱引用条目发布的新锁，避免未使用环境身份永久泄漏。
    let lock = Arc::new(Mutex::new(()));
    registry.insert(env_dir.to_path_buf(), Arc::downgrade(&lock));
    lock
}

/// Return the process-wide replaced-environment recovery center, creating its worker once.
/// 返回进程级已替换环境恢复中心，并仅创建一次其 Worker。
fn managed_runtime_backup_recovery_center()
-> Result<Arc<ManagedRuntimeBackupRecoveryCenter>, String> {
    static CENTER: OnceLock<Result<Arc<ManagedRuntimeBackupRecoveryCenter>, String>> =
        OnceLock::new();
    match CENTER.get_or_init(initialize_managed_runtime_backup_recovery_center) {
        Ok(center) => Ok(Arc::clone(center)),
        Err(error) => Err(error.clone()),
    }
}

/// Create the durable recovery queue and its single process-wide worker.
/// 创建持久恢复队列及其唯一进程级 Worker。
fn initialize_managed_runtime_backup_recovery_center()
-> Result<Arc<ManagedRuntimeBackupRecoveryCenter>, String> {
    // Shared reservation counter retained independently from transient publication callers.
    // 独立于瞬态发布调用方保留的共享预留计数器。
    let reserved = Arc::new(AtomicUsize::new(0));
    // Center published only after its worker starts successfully.
    // 仅在 Worker 成功启动后发布的恢复中心。
    let center = Arc::new(ManagedRuntimeBackupRecoveryCenter {
        records: Mutex::new(VecDeque::new()),
        changed: Condvar::new(),
        reserved,
    });
    let worker_center = Arc::clone(&center);
    thread::Builder::new()
        .name("luaskills-managed-env-backup-recovery".to_string())
        .spawn(move || run_managed_runtime_backup_recovery_worker(worker_center))
        .map_err(|error| {
            format!("failed to start managed environment backup recovery worker: {error}")
        })?;
    Ok(center)
}

/// Reserve one bounded recovery slot before any published environment name is mutated.
/// 在修改任何已发布环境名称前预留一个有界恢复槽。
///
/// Returns the durable center and one permit that can be transferred into a retry record.
/// 返回持久恢复中心及一个可移交到重试记录的许可。
fn reserve_managed_runtime_backup_recovery_slot() -> Result<
    (
        Arc<ManagedRuntimeBackupRecoveryCenter>,
        ManagedRuntimeBackupRecoveryPermit,
    ),
    String,
> {
    let center = managed_runtime_backup_recovery_center()?;
    center
        .reserved
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
            (reserved < MAX_MANAGED_RUNTIME_BACKUP_RECOVERY_RECORDS).then_some(reserved + 1)
        })
        .map_err(|reserved| {
            format!(
                "managed environment backup recovery capacity is exhausted; reserved={reserved} max={MAX_MANAGED_RUNTIME_BACKUP_RECOVERY_RECORDS}"
            )
        })?;
    let permit = ManagedRuntimeBackupRecoveryPermit {
        reserved: Arc::clone(&center.reserved),
    };
    Ok((center, permit))
}

impl ManagedRuntimeBackupRecoveryCenter {
    /// Publish one pre-reserved recovery record without any fallible filesystem work.
    /// 发布单个已预留恢复记录，且不执行任何可失败文件系统操作。
    ///
    /// `record` carries both exact backup identity and its capacity permit.
    /// `record` 同时携带精确备份身份与容量许可。
    fn enqueue(&self, record: ManagedRuntimeBackupRecoveryRecord) {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records.push_back(record);
        self.changed.notify_one();
    }

    /// Wait for and remove one retained recovery record.
    /// 等待并移除单个已保留恢复记录。
    fn wait_next(&self) -> ManagedRuntimeBackupRecoveryRecord {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while records.is_empty() {
            records = self
                .changed
                .wait(records)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        records
            .pop_front()
            .expect("non-empty managed environment recovery queue must yield one record")
    }
}

/// Run the process-wide replaced-environment recovery worker forever.
/// 永久运行进程级已替换环境恢复 Worker。
///
/// `center` is process-static through the global once cell; every record remains owned outside the
/// fallible recovery call so an error or panic cannot discard its backup identity or permit.
/// `center` 通过全局一次性单元具备进程静态生命周期；每个记录都在可失败恢复调用之外保持所有权，
/// 因此错误或 panic 不会丢失其备份身份或许可。
fn run_managed_runtime_backup_recovery_worker(center: Arc<ManagedRuntimeBackupRecoveryCenter>) {
    loop {
        let mut record = center.wait_next();
        let recovery = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recover_managed_runtime_backup(&mut record)
        }));
        match recovery {
            Ok(Ok(())) => {
                // Dropping the completed record releases its bounded capacity reservation.
                // 丢弃已完成记录会释放其有界容量预留。
                drop(record);
            }
            Ok(Err(error)) => {
                if !record.error_reported {
                    crate::runtime_logging::warn(format!(
                        "[LuaSkill:warn] managed environment backup recovery remains pending for {}: {error}",
                        render_host_visible_path(&record.backup_path)
                    ));
                    record.error_reported = true;
                }
                thread::sleep(MANAGED_RUNTIME_BACKUP_RECOVERY_RETRY_DELAY);
                center.enqueue(record);
            }
            Err(_) => {
                if !record.error_reported {
                    crate::runtime_logging::warn(format!(
                        "[LuaSkill:warn] managed environment backup recovery panicked and remains pending for {}",
                        render_host_visible_path(&record.backup_path)
                    ));
                    record.error_reported = true;
                }
                thread::sleep(MANAGED_RUNTIME_BACKUP_RECOVERY_RETRY_DELAY);
                center.enqueue(record);
            }
        }
    }
}

/// Complete the exact action retained by one replaced-environment recovery record.
/// 完成单个已替换环境恢复记录保留的精确动作。
///
/// Returns unit only after the obsolete backup is removed or the failed publication name is
/// restored/safely superseded by a ready environment.
/// 仅在过时备份已删除，或失败发布名称已恢复/被就绪环境安全替代后返回空值。
fn recover_managed_runtime_backup(
    record: &mut ManagedRuntimeBackupRecoveryRecord,
) -> Result<(), String> {
    match &record.action {
        ManagedRuntimeBackupRecoveryAction::RemoveCommittedBackup => {
            remove_identity_bound_managed_runtime_backup(&record.backup_path, &record.identity)
        }
        ManagedRuntimeBackupRecoveryAction::RestoreFailedPublication { plan } => {
            recover_failed_managed_runtime_publication(plan, &record.backup_path, &record.identity)
        }
    }
}

/// Remove one obsolete backup only while its native directory identity remains unchanged.
/// 仅在平台原生目录身份未变化时删除单个过时备份。
fn remove_identity_bound_managed_runtime_backup(
    backup_path: &Path,
    identity: &ManagedFilesystemObjectIdentity,
) -> Result<(), String> {
    match fs::symlink_metadata(backup_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to inspect replaced managed environment {}: {error}",
                render_managed_runtime_path(backup_path)
            ));
        }
    }
    validate_managed_directory_identity(backup_path, identity, "replaced managed environment")?;
    match fs::remove_dir_all(backup_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove replaced managed environment {}: {error}",
            render_managed_runtime_path(backup_path)
        )),
    }
}

/// Restore an old environment name after publication and immediate rollback both failed.
/// 在发布与即时回滚均失败后恢复旧环境名称。
fn recover_failed_managed_runtime_publication(
    plan: &ManagedRuntimeEnvPlan,
    backup_path: &Path,
    identity: &ManagedFilesystemObjectIdentity,
) -> Result<(), String> {
    // Process-local and cross-process construction locks serialize recovery with every installer.
    // 进程内及跨进程构建锁使恢复与每个安装器串行化。
    let build_lock = managed_runtime_env_lock(&plan.env_dir);
    let _build_guard = build_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _file_lock = acquire_managed_runtime_env_file_lock(plan)?;
    if managed_env_is_ready(plan)? {
        // Another installer safely superseded the failed transaction; only the old backup remains.
        // 另一个安装器已安全替代失败事务；此时只剩旧备份需要删除。
        return remove_identity_bound_managed_runtime_backup(backup_path, identity);
    }
    match fs::symlink_metadata(&plan.env_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(format!(
                "cannot restore replaced managed environment while target exists: {}",
                render_managed_runtime_path(&plan.env_dir)
            ));
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect managed environment recovery target {}: {error}",
                render_managed_runtime_path(&plan.env_dir)
            ));
        }
    }
    let _publish_lease = try_acquire_managed_runtime_env_publish_lease(plan)?;
    validate_managed_directory_identity(backup_path, identity, "replaced managed environment")?;
    fs::rename(backup_path, &plan.env_dir).map_err(|error| {
        format!(
            "failed to restore replaced managed environment {} as {}: {error}",
            render_managed_runtime_path(backup_path),
            render_managed_runtime_path(&plan.env_dir)
        )
    })
}

/// Acquire one cross-process exclusive construction lock for an exact environment plan.
/// 为一个精确环境计划获取跨进程独占构建锁。
///
/// `plan` supplies the stable environment parent and hash used to derive the lock-file identity.
/// `plan` 提供用于派生锁文件身份的稳定环境父目录与哈希。
///
/// Returns a guard whose drop releases the OS lock, or a host-visible filesystem/locking error.
/// 返回析构时释放操作系统锁的保护对象，或宿主可见的文件系统/加锁错误。
fn acquire_managed_runtime_env_file_lock(
    plan: &ManagedRuntimeEnvPlan,
) -> Result<ManagedRuntimeEnvFileLock, String> {
    let parent = plan.env_dir.parent().ok_or_else(|| {
        format!(
            "managed env directory has no parent: {}",
            render_managed_runtime_path(&plan.env_dir)
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Failed to create managed environment parent {}: {error}",
            render_managed_runtime_path(parent)
        )
    })?;
    // Persistent lock file is namespaced by runtime/version parent plus the exact environment hash.
    // 持久锁文件由运行时/版本父目录与精确环境哈希共同划分命名空间。
    let path = parent.join(format!(".luaskills-env-{}.lock", plan.env_hash));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            format!(
                "Failed to open managed environment lock {}: {error}",
                render_managed_runtime_path(&path)
            )
        })?;
    lock_managed_runtime_env_file(&file).map_err(|error| {
        format!(
            "Failed to lock managed environment {}: {error}",
            render_managed_runtime_path(&path)
        )
    })?;
    Ok(ManagedRuntimeEnvFileLock { file, path })
}

/// Ensure one environment, acquire its shared lifecycle lease, and revalidate readiness.
/// 确保一个环境就绪、获取其共享生命周期租约，并重新校验就绪状态。
///
/// `plan` identifies the exact environment whose published directory must remain stable.
/// `plan` 标识其已发布目录必须保持稳定的精确环境。
///
/// Returns a shared lease only after the expected marker is observed while that lease is held.
/// 仅在持有租约期间观察到期望 marker 后返回共享租约。
pub(crate) fn acquire_ready_managed_env_lease(
    plan: &ManagedRuntimeEnvPlan,
) -> Result<ManagedRuntimeEnvLease, String> {
    ensure_managed_env(plan)?;
    // SharedGuard closes the ensure-to-use replacement window before marker revalidation.
    // SharedGuard 会在 marker 复验前关闭从 ensure 到使用之间的替换窗口。
    let guard = acquire_managed_runtime_env_shared_lease(plan)?;
    if !managed_env_is_ready(plan)? {
        return Err(format!(
            "managed runtime environment changed before lifecycle lease acquisition: {}",
            render_managed_runtime_path(&plan.env_dir)
        ));
    }
    Ok(ManagedRuntimeEnvLease { _guard: guard })
}

/// Open the stable per-environment lifecycle lease file used by shared users and publishers.
/// 打开由共享使用方与发布方共同使用的稳定每环境生命周期租约文件。
///
/// `plan` supplies the exact environment parent and identity hash.
/// `plan` 提供精确环境父目录与身份哈希。
///
/// Returns the open file and its diagnostic path without acquiring a lock.
/// 返回尚未加锁的已打开文件及其诊断路径。
fn open_managed_runtime_env_lease_file(
    plan: &ManagedRuntimeEnvPlan,
) -> Result<(File, PathBuf), String> {
    // Stable environment parent that owns both the published directory and lifecycle lease file.
    // 同时承载已发布目录与生命周期租约文件的稳定环境父目录。
    let parent = plan.env_dir.parent().ok_or_else(|| {
        format!(
            "managed env directory has no parent: {}",
            render_managed_runtime_path(&plan.env_dir)
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Failed to create managed environment parent {}: {error}",
            render_managed_runtime_path(parent)
        )
    })?;
    // LeasePath is separate from the construction lock so active use never blocks build staging.
    // LeasePath 与构建锁相互独立，因此活动使用不会阻止构建暂存。
    let path = parent.join(format!(".luaskills-env-{}.lease", plan.env_hash));
    // Read-write handle required by both Unix flock and Windows byte-range locking primitives.
    // Unix flock 与 Windows 字节范围锁原语共同需要的读写句柄。
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            format!(
                "Failed to open managed environment lifecycle lease {}: {error}",
                render_managed_runtime_path(&path)
            )
        })?;
    Ok((file, path))
}

/// Acquire one blocking shared lifecycle lease for an active managed environment user.
/// 为一个活动受管环境使用方获取阻塞式共享生命周期租约。
///
/// `plan` identifies the stable lease file shared with environment publication.
/// `plan` 标识与环境发布共享的稳定租约文件。
///
/// Returns a guard that releases the shared operating-system lock on drop.
/// 返回一个在析构时释放共享操作系统锁的保护器。
fn acquire_managed_runtime_env_shared_lease(
    plan: &ManagedRuntimeEnvPlan,
) -> Result<ManagedRuntimeEnvLeaseFileLock, String> {
    // Lease handle and stable path retained together for ownership and diagnostics.
    // 为所有权与诊断共同保留的租约句柄及稳定路径。
    let (file, path) = open_managed_runtime_env_lease_file(plan)?;
    lock_managed_runtime_env_lease_shared(&file).map_err(|error| {
        format!(
            "Failed to acquire shared managed environment lifecycle lease {}: {error}",
            render_managed_runtime_path(&path)
        )
    })?;
    Ok(ManagedRuntimeEnvLeaseFileLock { file, path })
}

/// Try to acquire one exclusive lifecycle lease before touching a published environment directory.
/// 在接触已发布环境目录前尝试获取一个独占生命周期租约。
///
/// `plan` identifies the environment whose active users must prevent replacement.
/// `plan` 标识其活动使用方必须阻止替换的环境。
///
/// Returns immediately with a stable busy error when any shared lifecycle lease is active.
/// 任一共享生命周期租约处于活动状态时立即返回稳定 busy 错误。
fn try_acquire_managed_runtime_env_publish_lease(
    plan: &ManagedRuntimeEnvPlan,
) -> Result<ManagedRuntimeEnvLeaseFileLock, String> {
    // Lease handle and stable path used for the immediate exclusive publication attempt.
    // 用于立即尝试独占发布的租约句柄及稳定路径。
    let (file, path) = open_managed_runtime_env_lease_file(plan)?;
    match try_lock_managed_runtime_env_lease_exclusive(&file) {
        Ok(true) => Ok(ManagedRuntimeEnvLeaseFileLock { file, path }),
        Ok(false) => Err(format!(
            "managed runtime environment is busy with active lifecycle leases: {}",
            render_managed_runtime_path(&plan.env_dir)
        )),
        Err(error) => Err(format!(
            "Failed to acquire exclusive managed environment lifecycle lease {}: {error}",
            render_managed_runtime_path(&path)
        )),
    }
}

/// Block until one exclusive operating-system lock is held on the supplied file.
/// 阻塞直至在提供的文件上持有一个操作系统独占锁。
///
/// `file` remains open for the complete lock lifetime.
/// `file` 在完整锁生命周期内保持打开。
#[cfg(unix)]
fn lock_managed_runtime_env_file(file: &File) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;

    loop {
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if status == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Release one Unix advisory environment lock.
/// 释放一个 Unix 建议环境锁。
#[cfg(unix)]
fn unlock_managed_runtime_env_file(file: &File) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;

    let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Block until one exclusive Windows byte-range lock is held on the supplied file.
/// 阻塞直至在提供的文件上持有一个 Windows 独占字节范围锁。
#[cfg(windows)]
fn lock_managed_runtime_env_file(file: &File) -> Result<(), std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LockFileEx};
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // Zero offset plus the full 64-bit range makes every contender lock the same file identity.
    // 零偏移加完整 64 位范围，使每个竞争方都锁定同一文件身份。
    let mut overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
    let status = unsafe {
        LockFileEx(
            file.as_raw_handle() as _,
            LOCKFILE_EXCLUSIVE_LOCK,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if status != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Release one Windows byte-range environment lock.
/// 释放一个 Windows 字节范围环境锁。
#[cfg(windows)]
fn unlock_managed_runtime_env_file(file: &File) -> Result<(), std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
    let status = unsafe {
        UnlockFileEx(
            file.as_raw_handle() as _,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if status != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Reject managed environment construction on platforms without a reliable file-lock primitive.
/// 在缺少可靠文件锁原语的平台上拒绝受管环境构建。
#[cfg(not(any(unix, windows)))]
fn lock_managed_runtime_env_file(_file: &File) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "managed runtime cross-process file locking is unsupported on this platform",
    ))
}

/// No-op signature counterpart for unsupported platforms where acquisition always fails.
/// 在获取始终失败的不受支持平台上提供同签名空操作对应函数。
#[cfg(not(any(unix, windows)))]
fn unlock_managed_runtime_env_file(_file: &File) -> Result<(), std::io::Error> {
    Ok(())
}

/// Block until one Unix shared lifecycle lease is held on the supplied file.
/// 阻塞直至在提供的文件上持有一个 Unix 共享生命周期租约。
#[cfg(unix)]
fn lock_managed_runtime_env_lease_shared(file: &File) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;

    loop {
        // Kernel flock result retried only when interrupted before lock acquisition completes.
        // 仅当锁获取完成前被中断时重试的内核 flock 结果。
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
        if status == 0 {
            return Ok(());
        }
        // Exact operating-system error used to distinguish retryable interruption from failure.
        // 用于区分可重试中断与失败的精确操作系统错误。
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Try one nonblocking Unix exclusive lifecycle lease acquisition.
/// 尝试一次非阻塞 Unix 独占生命周期租约获取。
///
/// Returns `false` only when another shared or exclusive lease currently owns the file.
/// 仅当另一个共享或独占租约当前拥有该文件时返回 `false`。
#[cfg(unix)]
fn try_lock_managed_runtime_env_lease_exclusive(file: &File) -> Result<bool, std::io::Error> {
    use std::os::fd::AsRawFd;

    loop {
        // Nonblocking kernel flock result preserving contention as a normal busy outcome.
        // 将锁竞争保留为正常 busy 结果的非阻塞内核 flock 结果。
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if status == 0 {
            return Ok(true);
        }
        // Exact operating-system error classified into contention, interruption, or hard failure.
        // 被分类为竞争、中断或硬失败的精确操作系统错误。
        let error = std::io::Error::last_os_error();
        match error.kind() {
            std::io::ErrorKind::WouldBlock => return Ok(false),
            std::io::ErrorKind::Interrupted => continue,
            _ => return Err(error),
        }
    }
}

/// Release one Unix shared or exclusive lifecycle lease.
/// 释放一个 Unix 共享或独占生命周期租约。
#[cfg(unix)]
fn unlock_managed_runtime_env_lease_file(file: &File) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;

    // Kernel unlock result surfaced to the guard drop path for diagnostic logging.
    // 向保护器析构路径暴露以便诊断记录的内核解锁结果。
    let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Block until one Windows shared lifecycle byte-range lease is held.
/// 阻塞直至持有一个 Windows 共享生命周期字节范围租约。
#[cfg(windows)]
fn lock_managed_runtime_env_lease_shared(file: &File) -> Result<(), std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::LockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // A zero flag value requests a shared lock and permits the kernel to wait for availability.
    // 零标志值请求共享锁，并允许内核等待其变为可用。
    let mut overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
    // Kernel lock result owning the full synthetic byte range when successful.
    // 成功时拥有完整合成字节范围的内核加锁结果。
    let status = unsafe {
        LockFileEx(
            file.as_raw_handle() as _,
            0,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if status != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Try one nonblocking Windows exclusive lifecycle byte-range lease acquisition.
/// 尝试一次非阻塞 Windows 独占生命周期字节范围租约获取。
///
/// Returns `false` only for the operating-system lock-conflict status.
/// 仅在操作系统返回锁冲突状态时返回 `false`。
#[cfg(windows)]
fn try_lock_managed_runtime_env_lease_exclusive(file: &File) -> Result<bool, std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // Zero-offset range descriptor shared with the blocking and unlock operations.
    // 与阻塞加锁及解锁操作共享的零偏移范围描述符。
    let mut overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
    // Immediate exclusive lock result used to distinguish contention from hard failure.
    // 用于区分竞争与硬失败的立即独占加锁结果。
    let status = unsafe {
        LockFileEx(
            file.as_raw_handle() as _,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if status != 0 {
        return Ok(true);
    }
    // Exact Windows status classified solely by the documented lock-violation code.
    // 仅依据文档规定的锁冲突代码分类的精确 Windows 状态。
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Release one Windows shared or exclusive lifecycle byte-range lease.
/// 释放一个 Windows 共享或独占生命周期字节范围租约。
#[cfg(windows)]
fn unlock_managed_runtime_env_lease_file(file: &File) -> Result<(), std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // Zero-offset range descriptor matching the original whole-range lock acquisition.
    // 与原始全范围加锁操作匹配的零偏移范围描述符。
    let mut overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
    // Kernel unlock result surfaced to the guard drop path for diagnostic logging.
    // 向保护器析构路径暴露以便诊断记录的内核解锁结果。
    let status = unsafe {
        UnlockFileEx(
            file.as_raw_handle() as _,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if status != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Reject shared lifecycle leases on platforms without a reliable file-lock primitive.
/// 在缺少可靠文件锁原语的平台上拒绝共享生命周期租约。
#[cfg(not(any(unix, windows)))]
fn lock_managed_runtime_env_lease_shared(_file: &File) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "managed runtime environment lifecycle leasing is unsupported on this platform",
    ))
}

/// Reject exclusive lifecycle leases on platforms without a reliable file-lock primitive.
/// 在缺少可靠文件锁原语的平台上拒绝独占生命周期租约。
#[cfg(not(any(unix, windows)))]
fn try_lock_managed_runtime_env_lease_exclusive(_file: &File) -> Result<bool, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "managed runtime environment lifecycle leasing is unsupported on this platform",
    ))
}

/// No-op unlock counterpart for unsupported lifecycle-lease platforms.
/// 为不受支持的生命周期租约平台提供空操作解锁对应函数。
#[cfg(not(any(unix, windows)))]
fn unlock_managed_runtime_env_lease_file(_file: &File) -> Result<(), std::io::Error> {
    Ok(())
}

/// Allocate one nonzero build sequence and reject exhaustion instead of wrapping.
/// 分配一个非零构建序号，并在耗尽时拒绝回绕。
fn allocate_managed_runtime_build_sequence() -> Result<u64, String> {
    NEXT_MANAGED_RUNTIME_BUILD_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| "managed runtime build sequence is exhausted".to_string())
}

/// Render one managed runtime filesystem path for user-facing error messages.
/// 为面向用户的受管运行时错误消息渲染单个文件系统路径。
fn render_managed_runtime_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Managed child runtime kind that Lua can invoke through `vulcan.runtime.*`.
/// Lua 可通过 `vulcan.runtime.*` 调用的受管子运行时类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRuntimeKind {
    /// Managed Python runtime backed by a host-provided CPython bundle.
    /// 由宿主提供的 CPython 包支撑的受管 Python 运行时。
    Python,
    /// Managed Node.js runtime backed by a host-provided Node bundle.
    /// 由宿主提供的 Node 包支撑的受管 Node.js 运行时。
    Node,
}

impl ManagedRuntimeKind {
    /// Return the stable lowercase identifier used in paths and hashes.
    /// 返回用于路径与哈希的稳定小写标识符。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Node => "node",
        }
    }
}

/// Request used to compute one reusable managed runtime environment identity.
/// 用于计算一个可复用受管运行时环境身份的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntimeEnvHashInput {
    /// Managed runtime kind used by the target environment.
    /// 目标环境使用的受管运行时类型。
    pub runtime: ManagedRuntimeKind,
    /// Exact interpreter runtime version.
    /// 精确解释器运行时版本。
    pub runtime_version: String,
    /// Normalized platform key such as `windows-x64`.
    /// 标准平台键，例如 `windows-x64`。
    pub platform: String,
    /// Package manager name such as `uv` or `pnpm`.
    /// 包管理器名称，例如 `uv` 或 `pnpm`。
    pub package_manager: String,
    /// Exact package-manager version.
    /// 精确包管理器版本。
    pub package_manager_version: String,
    /// SHA-256 digest of the dependency lockfile content.
    /// 依赖锁文件内容的 SHA-256 摘要。
    pub lock_hash: String,
    /// Optional SHA-256 digest of an additional package manifest such as package.json.
    /// package.json 等附加包清单的可选 SHA-256 摘要。
    pub package_manifest_hash: Option<String>,
}

/// Stable marker written into every managed runtime environment directory.
/// 写入每个受管运行时环境目录的稳定标记文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRuntimeEnvMarker {
    /// Marker schema version.
    /// 标记文件 schema 版本。
    pub schema_version: u32,
    /// Managed runtime kind stored as a stable lowercase identifier.
    /// 以稳定小写标识符保存的受管运行时类型。
    pub runtime: String,
    /// Exact interpreter runtime version.
    /// 精确解释器运行时版本。
    pub runtime_version: String,
    /// Package manager name such as `uv` or `pnpm`.
    /// 包管理器名称，例如 `uv` 或 `pnpm`。
    pub package_manager: String,
    /// Exact package-manager version.
    /// 精确包管理器版本。
    pub package_manager_version: String,
    /// Normalized platform key such as `windows-x64`.
    /// 标准平台键，例如 `windows-x64`。
    pub platform: String,
    /// SHA-256 digest of the dependency lockfile content.
    /// 依赖锁文件内容的 SHA-256 摘要。
    pub lock_hash: String,
    /// Optional SHA-256 digest of an additional package manifest such as package.json.
    /// package.json 等附加包清单的可选 SHA-256 摘要。
    pub package_manifest_hash: Option<String>,
    /// Environment identity hash derived from all reproducibility inputs.
    /// 由全部可复现输入派生出的环境身份哈希。
    pub env_hash: String,
}

/// Manifest written next to each managed runtime or package-manager installation.
/// 写入每个受管运行时或包管理器安装目录旁的清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRuntimeInstallManifest {
    /// Manifest schema version.
    /// 清单 schema 版本。
    pub schema_version: u32,
    /// Runtime or package-manager identifier such as `python`, `node`, `uv`, or `pnpm`.
    /// 运行时或包管理器标识，例如 `python`、`node`、`uv` 或 `pnpm`。
    pub runtime: String,
    /// Exact installed version.
    /// 已安装的精确版本。
    pub version: String,
    /// Platform key stored by the installation script.
    /// 安装脚本写入的平台键。
    pub platform: String,
    /// Executable path relative to the installation directory.
    /// 相对安装目录的可执行文件路径。
    pub executable: String,
}

/// Resolved managed runtime environment plan used before creation or invocation.
/// 创建环境或调用前使用的受管运行时环境解析计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntimeEnvPlan {
    /// Managed runtime kind described by this plan.
    /// 当前计划描述的受管运行时类型。
    pub runtime: ManagedRuntimeKind,
    /// Current platform key.
    /// 当前平台键。
    pub platform: String,
    /// Canonical runtime root used to locate shared stores and runtime assets.
    /// 用于定位共享存储与运行时资产的规范运行时根目录。
    pub runtime_root: PathBuf,
    /// Exact interpreter runtime version.
    /// 精确解释器运行时版本。
    pub runtime_version: String,
    /// Absolute interpreter executable path.
    /// 解释器可执行文件绝对路径。
    pub runtime_executable: PathBuf,
    /// Package manager name such as `uv` or `pnpm`.
    /// 包管理器名称，例如 `uv` 或 `pnpm`。
    pub package_manager: String,
    /// Exact package-manager version.
    /// 精确包管理器版本。
    pub package_manager_version: String,
    /// Absolute package-manager executable path.
    /// 包管理器可执行文件绝对路径。
    pub package_manager_executable: PathBuf,
    /// Optional package manifest path such as package.json.
    /// package.json 等可选包清单路径。
    pub package_manifest_path: Option<PathBuf>,
    /// Required lockfile path.
    /// 必需的锁文件路径。
    pub lockfile_path: PathBuf,
    /// SHA-256 digest of the dependency lockfile content.
    /// 依赖锁文件内容的 SHA-256 摘要。
    pub lock_hash: String,
    /// Optional SHA-256 digest of an additional package manifest such as package.json.
    /// package.json 等附加包清单的可选 SHA-256 摘要。
    pub package_manifest_hash: Option<String>,
    /// Environment identity hash.
    /// 环境身份哈希。
    pub env_hash: String,
    /// Directory that stores this reusable environment.
    /// 保存当前可复用环境的目录。
    pub env_dir: PathBuf,
    /// Marker expected after environment creation.
    /// 环境创建完成后期望写入的标记。
    pub expected_marker: ManagedRuntimeEnvMarker,
}

impl ManagedRuntimeEnvMarker {
    /// Build one expected marker from a hash input and precomputed environment hash.
    /// 基于哈希输入与预计算环境哈希构造一个期望标记。
    pub fn expected(input: &ManagedRuntimeEnvHashInput, env_hash: String) -> Self {
        Self {
            schema_version: MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION,
            runtime: input.runtime.as_str().to_string(),
            runtime_version: input.runtime_version.clone(),
            package_manager: input.package_manager.clone(),
            package_manager_version: input.package_manager_version.clone(),
            platform: input.platform.clone(),
            lock_hash: input.lock_hash.clone(),
            package_manifest_hash: input.package_manifest_hash.clone(),
            env_hash,
        }
    }
}

impl PythonRuntimePackageManager {
    /// Return the stable lowercase package-manager identifier.
    /// 返回稳定小写包管理器标识符。
    fn as_managed_runtime_str(self) -> &'static str {
        match self {
            Self::Uv => "uv",
        }
    }
}

impl NodeRuntimePackageManager {
    /// Return the stable lowercase package-manager identifier.
    /// 返回稳定小写包管理器标识符。
    fn as_managed_runtime_str(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
        }
    }
}

/// Return the current platform key used by managed runtime assets.
/// 返回受管运行时资产使用的当前平台键。
pub fn current_managed_runtime_platform_key() -> Result<String, String> {
    let arch_key = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(format!(
                "unsupported managed runtime architecture: {}",
                other
            ));
        }
    };
    let os_key = match std::env::consts::OS {
        "windows" => {
            if arch_key != "x64" {
                return Err("managed runtime assets currently support Windows x64 only".to_string());
            }
            "windows"
        }
        "linux" => "linux",
        "macos" => "macos",
        other => {
            return Err(format!(
                "unsupported managed runtime operating system: {}",
                other
            ));
        }
    };
    Ok(format!("{}-{}", os_key, arch_key))
}

/// Compute the SHA-256 digest of one byte slice as lowercase hex.
/// 将一个字节切片计算为小写十六进制 SHA-256 摘要。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{:x}", digest)
}

/// Compute the SHA-256 digest of one file.
/// 计算单个文件的 SHA-256 摘要。
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(path),
            error
        )
    })?;
    Ok(sha256_hex(&bytes))
}

/// Verify one copied build input against the digest captured by its environment plan.
/// 根据环境计划捕获的摘要校验单个已复制构建输入。
///
/// `path` is the private unpublished copy, `expected_hash` is the plan-time SHA-256 digest, and
/// `label` identifies the dependency input in diagnostics.
/// `path` 是私有未发布副本，`expected_hash` 是计划阶段的 SHA-256 摘要，`label` 用于在诊断中
/// 标识依赖输入。
///
/// Returns unit only when the exact bytes consumed by the package manager match the plan identity.
/// 仅当包管理器消费的精确字节与计划身份匹配时返回空值。
fn verify_managed_runtime_build_input_hash(
    path: &Path,
    expected_hash: &str,
    label: &str,
) -> Result<(), String> {
    let actual_hash = sha256_file(path)?;
    if actual_hash != expected_hash {
        return Err(format!(
            "managed runtime {label} changed after environment planning: expected {expected_hash}, got {actual_hash}"
        ));
    }
    Ok(())
}

/// Compute one stable environment hash from reproducibility inputs.
/// 根据可复现输入计算一个稳定环境哈希。
pub fn compute_managed_runtime_env_hash(input: &ManagedRuntimeEnvHashInput) -> String {
    let mut hasher = Sha256::new();
    hash_field(
        &mut hasher,
        "schema_version",
        MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION.to_string(),
    );
    hash_field(&mut hasher, "runtime", input.runtime.as_str());
    hash_field(&mut hasher, "runtime_version", &input.runtime_version);
    hash_field(&mut hasher, "platform", &input.platform);
    hash_field(&mut hasher, "package_manager", &input.package_manager);
    hash_field(
        &mut hasher,
        "package_manager_version",
        &input.package_manager_version,
    );
    hash_field(&mut hasher, "lock_hash", &input.lock_hash);
    hash_field(
        &mut hasher,
        "package_manifest_hash",
        input.package_manifest_hash.as_deref().unwrap_or(""),
    );
    format!("{:x}", hasher.finalize())
}

/// Return the managed environment root for one runtime version and environment hash.
/// 返回单个运行时版本与环境哈希对应的受管环境根目录。
pub fn managed_env_dir(
    runtime_root: &Path,
    runtime: ManagedRuntimeKind,
    runtime_version: &str,
    env_hash: &str,
) -> PathBuf {
    runtime_root
        .join("dependencies")
        .join("envs")
        .join(runtime.as_str())
        .join(format!("{}-{}", runtime_prefix(runtime), runtime_version))
        .join(env_hash)
}

/// Return the marker path used by one managed runtime environment directory.
/// 返回单个受管运行时环境目录使用的标记文件路径。
pub fn managed_env_marker_path(env_dir: &Path) -> PathBuf {
    env_dir.join(".luaskills-env.json")
}

/// Inspect whether one managed environment marker path is a file without hiding filesystem probe errors.
/// 检查单个受管环境标记路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The marker_path parameter is the concrete `.luaskills-env.json` path for one managed environment.
/// marker_path 参数是单个受管环境对应的具体 `.luaskills-env.json` 路径。
///
/// Return true for an existing marker file, false for a confirmed missing marker, or an explicit probe/type error.
/// 已存在标记文件返回 true，确认缺失标记返回 false；探测或类型异常时返回显式错误。
fn managed_env_marker_path_is_file(marker_path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(marker_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "Managed runtime env marker must not be a symbolic link: {}",
            render_managed_runtime_path(marker_path)
        )),
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Managed runtime env marker is not a file: {}",
            render_managed_runtime_path(marker_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect managed runtime env marker {}: {}",
            render_managed_runtime_path(marker_path),
            error
        )),
    }
}

/// Inspect whether one managed-runtime directory path is a directory without hiding filesystem probe errors.
/// 检查单个受管运行时目录路径是否为目录，同时不隐藏文件系统探测错误。
///
/// The path parameter is the concrete managed runtime directory path used by an environment lifecycle step.
/// path 参数是环境生命周期步骤使用的具体受管运行时目录路径。
///
/// Return true for an existing directory, false for a confirmed missing directory, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失目录返回 false；探测或类型异常时返回显式错误。
fn managed_runtime_directory_path_is_directory(
    path: &Path,
    directory_label: &str,
) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "{} must not be a symbolic link: {}",
            directory_label,
            render_managed_runtime_path(path)
        )),
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "{} is not a directory: {}",
            directory_label,
            render_managed_runtime_path(path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect {} {}: {}",
            directory_label,
            render_managed_runtime_path(path),
            error
        )),
    }
}

/// Read one managed runtime environment marker from disk.
/// 从磁盘读取一个受管运行时环境标记。
pub fn read_managed_env_marker(path: &Path) -> Result<ManagedRuntimeEnvMarker, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(path),
            error
        )
    })?;
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_managed_runtime_path(path),
            error
        )
    })
}

/// Return whether an environment marker matches the expected marker exactly.
/// 返回某个环境标记是否与期望标记完全匹配。
pub fn managed_env_marker_matches(
    actual: &ManagedRuntimeEnvMarker,
    expected: &ManagedRuntimeEnvMarker,
) -> bool {
    actual == expected
}

/// Ensure one managed runtime environment exists and matches its expected marker.
/// 确保一个受管运行时环境存在并且匹配其期望标记。
pub fn ensure_managed_env(plan: &ManagedRuntimeEnvPlan) -> Result<(), String> {
    if managed_env_is_ready(plan)? {
        return Ok(());
    }
    // Per-environment construction lock shared by every engine in this process.
    // 当前进程中所有引擎共享的每环境构建锁。
    let build_lock = managed_runtime_env_lock(&plan.env_dir);
    // Build guard recovered after poisoning so a failed installer does not permanently wedge retries.
    // 在锁中毒后恢复构建保护，避免失败安装永久阻塞重试。
    let _build_guard = build_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Cross-process lock prevents another host process from staging or replacing the same env.
    // 跨进程锁防止另一个宿主进程同时暂存或替换同一个环境。
    let _file_lock = acquire_managed_runtime_env_file_lock(plan)?;
    // Double-check under the construction lock because another caller may have published the environment.
    // 在构建锁内二次检查，因为其他调用方可能已经发布环境。
    if managed_env_is_ready(plan)? {
        return Ok(());
    }
    match plan.runtime {
        ManagedRuntimeKind::Python => create_python_env(plan),
        ManagedRuntimeKind::Node => create_node_env(plan),
    }
}

/// Return whether a managed runtime environment already matches its expected marker.
/// 返回一个受管运行时环境是否已经匹配其期望标记。
pub fn managed_env_is_ready(plan: &ManagedRuntimeEnvPlan) -> Result<bool, String> {
    // Environment directory itself is validated without following links before its marker is read.
    // 在读取 marker 前先以不跟随链接的方式校验环境目录本身。
    if !managed_runtime_directory_path_is_directory(
        &plan.env_dir,
        "managed runtime environment directory",
    )? {
        return Ok(false);
    }
    let marker_path = managed_env_marker_path(&plan.env_dir);
    if !managed_env_marker_path_is_file(&marker_path)? {
        return Ok(false);
    }
    let actual = read_managed_env_marker(&marker_path)?;
    Ok(managed_env_marker_matches(&actual, &plan.expected_marker))
}

/// Create one Python virtual environment and synchronize it from the lockfile.
/// 创建一个 Python 虚拟环境并按锁文件同步依赖。
///
/// `plan` supplies canonical managed runtime/package-manager executables, lockfile identity, and
/// the unpublished-to-published environment destination.
/// `plan` 提供规范受管运行时与包管理器可执行文件、锁文件身份及从未发布到已发布的环境目标。
///
/// Returns unit after atomic publication, or a cleanup-complete build/command error.
/// 原子发布后返回空值；失败时返回已完成清理的构建或命令错误。
fn create_python_env(plan: &ManagedRuntimeEnvPlan) -> Result<(), String> {
    // Unique unpublished same-parent directory used for the complete isolated uv build.
    // 完整隔离 uv 构建使用的唯一同父级未发布目录。
    let build_dir = prepare_build_dir(plan)?;
    // Build result retained so every failure removes only the unpublished directory.
    // 保留构建结果，使每次失败都只删除未发布目录。
    let build_result = (|| {
        // Private lockfile copy is verified before uv can consume it, closing the plan/build race.
        // 在 uv 消费前校验私有锁文件副本，关闭计划与构建之间的竞态窗口。
        let build_lockfile = build_dir.join("requirements.lock");
        fs::copy(&plan.lockfile_path, &build_lockfile).map_err(|error| {
            format!(
                "Failed to copy {} into {}: {}",
                render_managed_runtime_path(&plan.lockfile_path),
                render_managed_runtime_path(&build_lockfile),
                error
            )
        })?;
        verify_managed_runtime_build_input_hash(
            &build_lockfile,
            &plan.lock_hash,
            "Python lockfile",
        )?;
        // Virtual environment created wholly inside the unpublished build directory.
        // 完全在未发布构建目录内创建的虚拟环境。
        let venv_dir = build_dir.join(".venv");
        // First uv command whose environment contains only managed executable locations.
        // 首个 uv 命令，其环境仅包含受管可执行文件位置。
        let mut create_command = Command::new(&plan.package_manager_executable);
        create_command
            .arg("venv")
            .arg("--python")
            .arg(&plan.runtime_executable)
            .arg(&venv_dir)
            .current_dir(&build_dir);
        configure_managed_command_base_environment(
            &mut create_command,
            &build_dir,
            &[
                managed_executable_parent(&plan.package_manager_executable)?.to_path_buf(),
                managed_executable_parent(&plan.runtime_executable)?.to_path_buf(),
            ],
        )?;
        // uv must not discover user or system configuration outside the managed build.
        // uv 不得发现受管构建目录之外的用户或系统配置。
        create_command.env("UV_NO_CONFIG", "1");
        run_command(
            &mut create_command,
            "create managed Python virtual environment",
        )?;
        // Environment interpreter used by the reproducible lockfile synchronization step.
        // 可复现锁文件同步步骤使用的环境解释器。
        let python_executable = python_venv_executable(&venv_dir);
        // Second uv command isolated from host credentials and startup overrides.
        // 与宿主凭据及启动覆盖隔离的第二个 uv 命令。
        let mut sync_command = Command::new(&plan.package_manager_executable);
        sync_command
            .arg("pip")
            .arg("sync")
            .arg(&build_lockfile)
            .arg("--python")
            .arg(&python_executable)
            .arg("--cache-dir")
            .arg(package_store_dir_for_plan(plan))
            .current_dir(&build_dir);
        configure_managed_command_base_environment(
            &mut sync_command,
            &build_dir,
            &[
                managed_executable_parent(&plan.package_manager_executable)?.to_path_buf(),
                managed_executable_parent(&python_executable)?.to_path_buf(),
                managed_executable_parent(&plan.runtime_executable)?.to_path_buf(),
            ],
        )?;
        sync_command.env("UV_NO_CONFIG", "1");
        run_command(&mut sync_command, "synchronize managed Python environment")?;
        finish_build_dir(plan, build_dir.clone())
    })();
    cleanup_unpublished_build_after_result(&build_dir, build_result)
}

/// Create one Node dependency environment and install dependencies from the lockfile.
/// 创建一个 Node 依赖环境并按锁文件安装依赖。
///
/// `plan` supplies canonical Node/pnpm executables, package inputs, lockfile identity, and the
/// unpublished-to-published environment destination.
/// `plan` 提供规范 Node/pnpm 可执行文件、包输入、锁文件身份及从未发布到已发布的环境目标。
///
/// Returns unit after atomic publication, or a cleanup-complete copy/install error.
/// 原子发布后返回空值；失败时返回已完成清理的复制或安装错误。
fn create_node_env(plan: &ManagedRuntimeEnvPlan) -> Result<(), String> {
    // Unpublished same-parent build directory used for the entire pnpm installation.
    // 整个 pnpm 安装期间使用的同父级未发布构建目录。
    let build_dir = prepare_build_dir(plan)?;
    // Required package manifest declared by the authoritative package context.
    // 由权威包上下文声明的必需 package 清单。
    let package_json = plan.package_manifest_path.as_ref().ok_or_else(|| {
        "node package_json is required to create a managed Node environment".to_string()
    })?;
    // Installation result retained so every failure removes only the unpublished build directory.
    // 保留安装结果，使每次失败都只删除未发布构建目录。
    let build_package_json = build_dir.join("package.json");
    let build_lockfile = build_dir.join("pnpm-lock.yaml");
    let install_result = fs::copy(package_json, &build_package_json)
        .map_err(|error| {
            format!(
                "Failed to copy {} into {}: {}",
                render_managed_runtime_path(package_json),
                render_managed_runtime_path(&build_dir),
                error
            )
        })
        .and_then(|_| {
            fs::copy(&plan.lockfile_path, &build_lockfile).map_err(|error| {
                format!(
                    "Failed to copy {} into {}: {}",
                    render_managed_runtime_path(&plan.lockfile_path),
                    render_managed_runtime_path(&build_dir),
                    error
                )
            })
        })
        .and_then(|_| {
            let expected_package_hash = plan.package_manifest_hash.as_deref().ok_or_else(|| {
                "node package_json hash is unavailable during environment creation".to_string()
            })?;
            // Both private inputs are verified before pnpm opens either build-directory name.
            // 在 pnpm 打开任一构建目录名称前校验两个私有输入。
            verify_managed_runtime_build_input_hash(
                &build_package_json,
                expected_package_hash,
                "Node package manifest",
            )?;
            verify_managed_runtime_build_input_hash(
                &build_lockfile,
                &plan.lock_hash,
                "Node lockfile",
            )
        })
        .and_then(|_| {
            // pnpm command launched through the exact managed Node executable.
            // 通过精确受管 Node 可执行文件启动的 pnpm 命令。
            let mut install_command = Command::new(&plan.runtime_executable);
            install_command
                .arg(host_process_path_argument(&plan.package_manager_executable))
                .arg("install")
                .arg("--frozen-lockfile")
                // Hoisted installs avoid Windows absolute junctions that would retain the unpublished
                // build-directory name after atomic publication.
                // Hoisted 安装可避免 Windows 绝对目录联接在原子发布后仍指向未发布构建目录名。
                .arg("--node-linker")
                .arg("hoisted")
                .arg("--store-dir")
                .arg(host_process_path_argument(&package_store_dir_for_plan(
                    plan,
                )))
                .current_dir(&build_dir);
            configure_managed_command_base_environment(
                &mut install_command,
                &build_dir,
                &[
                    managed_executable_parent(&plan.runtime_executable)?.to_path_buf(),
                    managed_executable_parent(&plan.package_manager_executable)?.to_path_buf(),
                    build_dir.join("node_modules").join(".bin"),
                ],
            )?;
            // pnpm home is kept inside the unpublished managed environment.
            // pnpm 主目录保存在未发布受管环境内部。
            install_command.env(
                "PNPM_HOME",
                host_process_path_argument(managed_executable_parent(
                    &plan.package_manager_executable,
                )?),
            );
            run_command(&mut install_command, "install managed Node environment")
        })
        .and_then(|()| finish_build_dir(plan, build_dir.clone()));
    cleanup_unpublished_build_after_result(&build_dir, install_result)
}

/// Preserve one build result while removing a failed unpublished environment with full diagnostics.
/// 保留单次构建结果，并在失败时删除未发布环境且提供完整诊断。
///
/// `build_dir` is the exact atomically created unpublished directory; `result` is its build outcome.
/// `build_dir` 是原子创建的精确未发布目录；`result` 是其构建结果。
///
/// Returns success unchanged or the primary failure augmented with any cleanup failure.
/// 原样返回成功结果；失败时附加任何清理失败信息。
fn cleanup_unpublished_build_after_result(
    build_dir: &Path,
    result: Result<(), String>,
) -> Result<(), String> {
    let Err(primary_error) = result else {
        return Ok(());
    };
    match fs::remove_dir_all(build_dir) {
        Ok(()) => Err(primary_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(primary_error),
        Err(cleanup_error) => Err(format!(
            "{primary_error}; failed to remove unpublished managed environment {}: {cleanup_error}",
            render_managed_runtime_path(build_dir)
        )),
    }
}

/// Prepare one clean temporary build directory next to the target environment.
/// 在目标环境旁准备一个干净的临时构建目录。
fn prepare_build_dir(plan: &ManagedRuntimeEnvPlan) -> Result<PathBuf, String> {
    prepare_build_dir_with_sequence(plan, allocate_managed_runtime_build_sequence)
}

/// Prepare one unpublished build directory with an injected nonwrapping sequence allocator.
/// 使用注入的不可回绕序号分配器准备一个未发布构建目录。
///
/// `plan` supplies the exact same-parent environment identity and `allocate_sequence` supplies
/// collision-resistant candidate suffixes without hiding allocation exhaustion.
/// `plan` 提供精确的同父级环境身份，`allocate_sequence` 提供抗碰撞候选后缀且不会掩盖分配耗尽。
///
/// Returns one atomically created directory or a deterministic filesystem/allocation error.
/// 返回一个原子创建的目录，或确定性的文件系统/分配错误。
fn prepare_build_dir_with_sequence<F>(
    plan: &ManagedRuntimeEnvPlan,
    mut allocate_sequence: F,
) -> Result<PathBuf, String>
where
    F: FnMut() -> Result<u64, String>,
{
    // Existing parent that owns both unpublished and final environment directories.
    // 同时拥有未发布与最终环境目录的既有父目录。
    let parent = plan.env_dir.parent().ok_or_else(|| {
        format!(
            "managed env directory has no parent: {}",
            render_managed_runtime_path(&plan.env_dir)
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_managed_runtime_path(parent),
            error
        )
    })?;
    // Atomically created same-parent directory tolerating stale PID-reuse crash remnants.
    // 原子创建的同父级目录，可容忍 PID 复用产生的崩溃残留。
    for _attempt in 0..MAX_MANAGED_RUNTIME_BUILD_DIR_ATTEMPTS {
        // Nonwrapping process-local sequence combined with the process id for cross-process isolation.
        // 与进程 id 组合用于跨进程隔离的不可回绕进程内序号。
        let sequence = allocate_sequence()?;
        // Same-parent unpublished candidate so final publication uses one filesystem rename.
        // 与最终目录同父级的未发布候选，使最终发布使用一次文件系统重命名。
        let build_dir = parent.join(format!(
            ".building-{}-{}-{sequence}",
            plan.env_hash,
            std::process::id()
        ));
        match fs::create_dir(&build_dir) {
            Ok(()) => return Ok(build_dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create {}: {}",
                    render_managed_runtime_path(&build_dir),
                    error
                ));
            }
        }
    }
    Err(format!(
        "failed to allocate managed runtime build directory after {MAX_MANAGED_RUNTIME_BUILD_DIR_ATTEMPTS} attempts"
    ))
}

/// Finalize one temporary build directory into its stable environment directory.
/// 将一个临时构建目录收尾成稳定环境目录。
fn finish_build_dir(plan: &ManagedRuntimeEnvPlan, build_dir: PathBuf) -> Result<(), String> {
    finish_build_dir_with_backup_remover(
        plan,
        build_dir,
        remove_identity_bound_managed_runtime_backup,
    )
}

/// Finalize one build with an injectable immediate obsolete-backup remover.
/// 使用可注入的即时过时备份删除器收尾单个构建。
///
/// `plan` and `build_dir` define the publication transaction; `remove_backup` is invoked only after
/// the new environment name commits and allows deterministic retry-handoff tests.
/// `plan` 与 `build_dir` 定义发布事务；`remove_backup` 仅在新环境名称提交后调用，并支持确定性
/// 重试交接测试。
///
/// Returns success once publication commits, even when obsolete-backup deletion is retained for
/// durable retry. Pre-commit failures remain explicit errors.
/// 发布一旦提交即返回成功，即使过时备份删除已交给持久重试；提交前失败仍返回显式错误。
fn finish_build_dir_with_backup_remover<F>(
    plan: &ManagedRuntimeEnvPlan,
    build_dir: PathBuf,
    remove_backup: F,
) -> Result<(), String>
where
    F: FnOnce(&Path, &ManagedFilesystemObjectIdentity) -> Result<(), String>,
{
    write_expected_marker(&build_dir, &plan.expected_marker)?;
    // PublishLease is acquired before the first probe or mutation of the published environment.
    // PublishLease 会在首次探测或修改已发布环境之前获取。
    let _publish_lease = try_acquire_managed_runtime_env_publish_lease(plan)?;
    // Optional same-parent backup retaining the previously published environment during replacement.
    // 替换期间保留此前已发布环境的可选同父级备份。
    let mut backup =
        if managed_runtime_directory_path_is_directory(&plan.env_dir, "managed env directory")? {
            // Native identity captured before rename remains bound to the same directory object.
            // rename 前捕获的平台原生身份会继续绑定同一个目录对象。
            let identity =
                capture_managed_directory_identity(&plan.env_dir, "published managed environment")?;
            // Recovery capacity is guaranteed before the old published name is moved.
            // 在移动旧已发布名称前保证恢复容量。
            let (recovery_center, recovery_permit) =
                reserve_managed_runtime_backup_recovery_slot()?;
            // Unique backup path that cannot collide with another process-local replacement.
            // 不会与其他进程内替换发生碰撞的唯一备份路径。
            let sequence = allocate_managed_runtime_build_sequence()?;
            let parent = plan.env_dir.parent().ok_or_else(|| {
                format!(
                    "managed env directory has no parent: {}",
                    render_managed_runtime_path(&plan.env_dir)
                )
            })?;
            // Canonical parent makes the retained backup path stable across Windows verbatim names.
            // 规范父目录使保留备份路径在 Windows 逐字名称下保持稳定。
            let canonical_parent = fs::canonicalize(parent).map_err(|error| {
                format!(
                    "failed to canonicalize managed environment parent {}: {error}",
                    render_managed_runtime_path(parent)
                )
            })?;
            let backup = canonical_parent.join(format!(
                ".replaced-{}-{}-{sequence}",
                plan.env_hash,
                std::process::id()
            ));
            fs::rename(&plan.env_dir, &backup).map_err(|error| {
                format!(
                    "Failed to stage existing environment {} as {}: {}",
                    render_managed_runtime_path(&plan.env_dir),
                    render_managed_runtime_path(&backup),
                    error
                )
            })?;
            Some((backup, identity, recovery_center, recovery_permit))
        } else {
            None
        };
    // Single same-filesystem rename publishes only a complete environment.
    // 单次同文件系统重命名只发布完整环境。
    if let Err(publish_error) = fs::rename(&build_dir, &plan.env_dir) {
        if let Some((backup_path, _, _, _)) = backup.as_ref() {
            // Immediate rollback is attempted while construction and publication locks remain held.
            // 在构建锁与发布锁仍被持有时尝试即时回滚。
            if let Err(rollback_error) = fs::rename(backup_path, &plan.env_dir) {
                let (backup_path, identity, recovery_center, recovery_permit) = backup
                    .take()
                    .expect("present managed environment backup must remain owned");
                recovery_center.enqueue(ManagedRuntimeBackupRecoveryRecord {
                    backup_path,
                    identity,
                    action: ManagedRuntimeBackupRecoveryAction::RestoreFailedPublication {
                        plan: Box::new(plan.clone()),
                    },
                    _permit: recovery_permit,
                    error_reported: false,
                });
                return Err(format!(
                    "Failed to publish {} as {}: {}; rollback also failed and was scheduled for persistent recovery: {}",
                    render_managed_runtime_path(&build_dir),
                    render_managed_runtime_path(&plan.env_dir),
                    publish_error,
                    rollback_error
                ));
            }
            // Successful rollback leaves no backup path and releases the unused reservation.
            // 成功回滚不会留下备份路径，并释放未使用预留。
            drop(backup.take());
        }
        return Err(format!(
            "Failed to publish {} as {}: {}",
            render_managed_runtime_path(&build_dir),
            render_managed_runtime_path(&plan.env_dir),
            publish_error
        ));
    }
    if let Some((backup_path, identity, recovery_center, recovery_permit)) = backup.take() {
        // ImmediateCleanup is isolated so a panic cannot discard the only retained backup owner.
        // 隔离即时清理调用，确保 panic 不会丢弃唯一保留的备份所有者。
        let immediate_cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            remove_backup(&backup_path, &identity)
        }));
        // RecoveryRecord receives ownership for every non-successful immediate cleanup outcome.
        // 每一种未成功的即时清理结果都会把所有权移交给恢复记录。
        let recovery_error = match immediate_cleanup {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some("obsolete-backup cleanup panicked".to_string()),
        };
        if let Some(error) = recovery_error {
            crate::runtime_logging::warn(format!(
                "[LuaSkill:warn] managed environment publication committed; obsolete backup cleanup was scheduled for retry: {error}"
            ));
            recovery_center.enqueue(ManagedRuntimeBackupRecoveryRecord {
                backup_path,
                identity,
                action: ManagedRuntimeBackupRecoveryAction::RemoveCommittedBackup,
                _permit: recovery_permit,
                error_reported: true,
            });
        }
    }
    Ok(())
}

/// Write the expected marker into one environment directory.
/// 将期望标记写入一个环境目录。
fn write_expected_marker(env_dir: &Path, marker: &ManagedRuntimeEnvMarker) -> Result<(), String> {
    let marker_path = managed_env_marker_path(env_dir);
    let text = serde_json::to_string_pretty(marker)
        .map_err(|error| format!("Failed to serialize managed env marker: {}", error))?;
    fs::write(&marker_path, format!("{}\n", text)).map_err(|error| {
        format!(
            "Failed to write {}: {}",
            render_managed_runtime_path(&marker_path),
            error
        )
    })
}

/// Run one process command and convert failures into stable error text.
/// 运行一个进程命令并将失败转换为稳定错误文本。
fn run_command(command: &mut Command, operation: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("Failed to {}: {}", operation, error))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!(
        "Failed to {}: status={} stdout={} stderr={}",
        operation, output.status, stdout, stderr
    ))
}

/// Return the Python executable inside one venv directory.
/// 返回单个 venv 目录中的 Python 可执行文件。
fn python_venv_executable(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// Return the package-store directory used by one environment plan.
/// 返回单个环境计划使用的包存储目录。
fn package_store_dir_for_plan(plan: &ManagedRuntimeEnvPlan) -> PathBuf {
    let family = match plan.runtime {
        ManagedRuntimeKind::Python => "python",
        ManagedRuntimeKind::Node => "node",
    };
    let store_name = match plan.runtime {
        ManagedRuntimeKind::Python => "uv-cache",
        ManagedRuntimeKind::Node => "pnpm-store",
    };
    plan.runtime_root
        .join("dependencies")
        .join("package_store")
        .join(family)
        .join(store_name)
}

/// Return the parent directory of one validated managed executable.
/// 返回一个已验证受管可执行文件的父目录。
///
/// `executable` is an absolute canonical runtime or package-manager executable path.
/// `executable` 是运行时或包管理器可执行文件的绝对规范路径。
///
/// Returns the parent directory or an explicit structural error.
/// 返回父目录，或显式的结构错误。
fn managed_executable_parent(executable: &Path) -> Result<&Path, String> {
    executable.parent().ok_or_else(|| {
        format!(
            "managed executable has no parent directory: {}",
            render_managed_runtime_path(executable)
        )
    })
}

/// Replace one command's inherited environment with the minimal managed-runtime baseline.
/// 使用最小受管运行时基线替换单个命令继承的环境。
///
/// `command` is the not-yet-spawned child command, `controlled_home` is its isolated writable
/// home/temp root, and `path_entries` are the only executable-search directories it may receive.
/// `command` 是尚未启动的子命令，`controlled_home` 是其隔离可写 home/temp 根，
/// `path_entries` 是它可以接收的唯一可执行文件搜索目录。
///
/// Returns unit after installing the controlled environment, or an explicit path/platform error.
/// 安装受控环境后返回空值；路径或平台异常时返回显式错误。
pub(crate) fn configure_managed_command_base_environment(
    command: &mut Command,
    controlled_home: &Path,
    path_entries: &[PathBuf],
) -> Result<(), String> {
    if !controlled_home.is_absolute() {
        return Err(format!(
            "managed command home is not absolute: {}",
            render_managed_runtime_path(controlled_home)
        ));
    }
    if path_entries.is_empty() {
        return Err("managed command PATH requires at least one controlled directory".to_string());
    }
    for directory in path_entries {
        if !directory.is_absolute() {
            return Err(format!(
                "managed command PATH entry is not absolute: {}",
                render_managed_runtime_path(directory)
            ));
        }
    }
    // Clearing first is the security boundary: proxy credentials, cloud tokens, startup hooks,
    // loader overrides, and every other host variable stay unavailable to package processes.
    // 首先清空是安全边界：代理凭据、云令牌、启动钩子、加载器覆盖及其他全部宿主变量均不会暴露给包进程。
    command.env_clear();
    // ChildHome uses the equivalent non-verbatim spelling accepted by Node.js 24 path parsers.
    // ChildHome 使用 Node.js 24 路径解析器可接受的等价非 verbatim 写法。
    let child_home = host_process_path_argument(controlled_home);
    command.env("HOME", &child_home);
    command.env("TMPDIR", &child_home);
    // Platform-native PATH assembled exclusively from caller-authorized managed directories.
    // 仅根据调用方授权受管目录组装的平台原生 PATH。
    // ChildPathEntries keep the validated directories while removing only Windows verbatim syntax.
    // ChildPathEntries 保留已校验目录，仅移除 Windows verbatim 语法。
    let child_path_entries = path_entries
        .iter()
        .map(|path| host_process_path_argument(path))
        .collect::<Vec<_>>();
    let joined_path = env::join_paths(child_path_entries.iter())
        .map_err(|error| format!("failed to construct controlled managed-runtime PATH: {error}"))?;
    command.env("PATH", joined_path);
    #[cfg(windows)]
    install_required_windows_command_environment(command, &child_home)?;
    Ok(())
}

/// Restore only validated Windows process-bootstrap variables after `env_clear`.
/// 在 `env_clear` 后仅恢复经过验证的 Windows 进程启动变量。
///
/// `command` receives the required SystemRoot/ComSpec contract and `controlled_home` replaces all
/// user-profile and temporary directories so no host-user paths or credentials are inherited.
/// `command` 接收必需的 SystemRoot/ComSpec 契约，`controlled_home` 替换全部用户配置与临时目录，
/// 从而不继承宿主用户路径或凭据。
///
/// Returns unit or an explicit validation error for an unusable Windows bootstrap environment.
/// 返回空值；Windows 启动环境不可用时返回显式校验错误。
#[cfg(windows)]
fn install_required_windows_command_environment(
    command: &mut Command,
    controlled_home: &Path,
) -> Result<(), String> {
    // SystemRoot is the sole trusted root for the command interpreter required by Windows tools.
    // SystemRoot 是 Windows 工具所需命令解释器的唯一可信根。
    let system_root = env::var_os("SystemRoot")
        .ok_or_else(|| "required Windows environment variable SystemRoot is missing".to_string())?;
    if !Path::new(&system_root).is_absolute() {
        return Err(format!(
            "Windows SystemRoot is not absolute: {}",
            render_managed_runtime_path(Path::new(&system_root))
        ));
    }
    // Canonical root used only for validation; children receive the native non-verbatim value.
    // 仅用于校验的规范根；子进程接收原生非逐字路径值。
    let canonical_system_root = fs::canonicalize(&system_root).map_err(|error| {
        format!(
            "failed to canonicalize Windows SystemRoot {}: {error}",
            render_managed_runtime_path(Path::new(&system_root))
        )
    })?;
    if !canonical_system_root.is_dir() {
        return Err(format!(
            "Windows SystemRoot is not a directory: {}",
            render_managed_runtime_path(&canonical_system_root)
        ));
    }
    // ComSpec is canonicalized and constrained to SystemRoot to prevent executable injection.
    // 对 ComSpec 进行规范化并限制在 SystemRoot 内，以防止可执行文件注入。
    let com_spec = env::var_os("ComSpec")
        .ok_or_else(|| "required Windows environment variable ComSpec is missing".to_string())?;
    if !Path::new(&com_spec).is_absolute() {
        return Err(format!(
            "Windows ComSpec is not absolute: {}",
            render_managed_runtime_path(Path::new(&com_spec))
        ));
    }
    // Canonical interpreter used only to prove type and SystemRoot containment.
    // 仅用于证明类型及 SystemRoot 包含关系的规范解释器。
    let canonical_com_spec = fs::canonicalize(&com_spec).map_err(|error| {
        format!(
            "failed to canonicalize Windows ComSpec {}: {error}",
            render_managed_runtime_path(Path::new(&com_spec))
        )
    })?;
    if canonical_com_spec == canonical_system_root
        || !canonical_com_spec.starts_with(&canonical_system_root)
        || !canonical_com_spec.is_file()
    {
        return Err(format!(
            "Windows ComSpec is not a regular file strictly inside SystemRoot: {}",
            render_managed_runtime_path(&canonical_com_spec)
        ));
    }
    command.env("SystemRoot", &system_root);
    command.env("WINDIR", &system_root);
    command.env("ComSpec", &com_spec);
    command.env("PATHEXT", ".COM;.EXE;.BAT;.CMD");
    // Optional non-secret drive bootstrap value used by Windows command scripts.
    // Windows 命令脚本使用的可选非密钥驱动器启动值。
    if let Some(system_drive) = env::var_os("SystemDrive") {
        command.env("SystemDrive", system_drive);
    }
    command.env("USERPROFILE", controlled_home);
    command.env("APPDATA", controlled_home);
    command.env("LOCALAPPDATA", controlled_home);
    command.env("TEMP", controlled_home);
    command.env("TMP", controlled_home);
    Ok(())
}

/// Recursively copy one directory tree.
/// 递归复制一个目录树。
#[cfg(test)]
fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_managed_runtime_path(destination),
            error
        )
    })?;
    for entry in fs::read_dir(source).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(source),
            error
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "Failed to read directory entry under {}: {}",
                render_managed_runtime_path(source),
                error
            )
        })?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        // Entry type read from the directory iterator without following symlinks.
        // 从目录迭代器读取且不跟随符号链接的目录项类型。
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "Failed to inspect {} under {}: {}",
                render_managed_runtime_path(&source_path),
                render_managed_runtime_path(source),
                error
            )
        })?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).map_err(|error| {
                format!(
                    "Failed to copy {} to {}: {}",
                    render_managed_runtime_path(&source_path),
                    render_managed_runtime_path(&destination_path),
                    error
                )
            })?;
        } else {
            return Err(format!(
                "Failed to copy {} to {}: unsupported file type",
                render_managed_runtime_path(&source_path),
                render_managed_runtime_path(&destination_path)
            ));
        }
    }
    Ok(())
}

/// Resolve one Python managed runtime environment plan from a skill declaration.
/// 根据 skill 声明解析一个 Python 受管运行时环境计划。
pub(crate) fn resolve_python_env_plan(
    package: &ManagedRuntimePackageContext,
    spec: &PythonRuntimeDependencySpec,
) -> Result<ManagedRuntimeEnvPlan, String> {
    // Canonical runtime root owned by the trusted package context.
    // 可信包上下文拥有的规范运行时根。
    let runtime_root = package.runtime_root();
    let platform = current_managed_runtime_platform_key()?;
    let runtime_dir = runtime_install_dir(
        runtime_root,
        "python",
        &format!("cpython-{}-{}", spec.version, platform),
    );
    let runtime_executable =
        resolve_install_executable(&runtime_dir, "python", &spec.version, &platform)?;
    let package_manager = spec.package_manager.as_managed_runtime_str().to_string();
    let package_manager_install_dir = runtime_install_dir(
        runtime_root,
        "python",
        &format!("uv-{}-{}", spec.package_manager_version, platform),
    );
    let package_manager_executable = resolve_install_executable(
        &package_manager_install_dir,
        "uv",
        &spec.package_manager_version,
        &platform,
    )?;
    // Canonical lockfile constrained to the trusted package root.
    // 限制在可信包根内的规范锁文件。
    let lockfile_path = package.resolve_existing_file(&spec.lockfile, "python_runtime.lockfile")?;
    let lock_hash = sha256_file(&lockfile_path)?;
    let hash_input = ManagedRuntimeEnvHashInput {
        runtime: ManagedRuntimeKind::Python,
        runtime_version: spec.version.clone(),
        platform,
        package_manager,
        package_manager_version: spec.package_manager_version.clone(),
        lock_hash,
        package_manifest_hash: None,
    };
    build_env_plan(
        runtime_root,
        ManagedRuntimeKind::Python,
        runtime_executable,
        package_manager_executable,
        None,
        lockfile_path,
        hash_input,
    )
}

/// Validate one declared Node.js version against the supported asynchronous ESM baseline.
/// 根据受支持的异步 ESM 基线校验一个声明的 Node.js 版本。
///
/// `version` must be an exact semantic version from `node_runtime.version` or an interpreter probe.
/// `version` 必须是来自 `node_runtime.version` 或解释器探针的精确语义版本。
///
/// Returns unit for a supported runtime, otherwise a stable declaration or minimum-version error.
/// 受支持时返回空值，否则返回稳定的声明错误或最低版本错误。
pub(crate) fn validate_managed_node_runtime_version(version: &str) -> Result<(), String> {
    // DeclaredVersion is parsed before any filesystem lookup so unsupported runtimes fail without
    // creating environments or launching a worker that may crash inside its ESM loader.
    // DeclaredVersion 会在任何文件系统查找前解析，使不受支持的运行时在创建环境或启动可能于
    // ESM 加载器内部崩溃的 Worker 前失败。
    let declared_version = Version::parse(version.trim()).map_err(|error| {
        format!("node_runtime.version is not valid semantic version '{version}': {error}")
    })?;
    // MinimumVersion matches the first supported long-term Node runtime line for this protocol.
    // MinimumVersion 对应当前协议首个受支持的长期 Node 运行时版本线。
    let minimum_version = Version::new(22, 0, 0);
    if declared_version < minimum_version {
        return Err(format!(
            "managed Node runtime {version} is unsupported; minimum supported version is {MIN_MANAGED_NODE_RUNTIME_VERSION}"
        ));
    }
    Ok(())
}

/// Resolve one Node.js managed runtime environment plan from a trusted package declaration.
/// 根据可信包声明解析一个 Node.js 受管运行时环境计划。
///
/// `package` supplies canonical package/runtime roots and `spec` supplies the exact supported Node,
/// pnpm, manifest, and lockfile declaration.
/// `package` 提供规范包根与运行时根，`spec` 提供精确且受支持的 Node、pnpm、清单及锁文件声明。
///
/// Returns a reproducible environment plan, or a version, containment, manifest, or hashing error.
/// 返回可复现环境计划，或版本、包含关系、清单及哈希错误。
pub(crate) fn resolve_node_env_plan(
    package: &ManagedRuntimePackageContext,
    spec: &NodeRuntimeDependencySpec,
) -> Result<ManagedRuntimeEnvPlan, String> {
    validate_managed_node_runtime_version(&spec.version)?;
    // Canonical runtime root owned by the trusted package context.
    // 可信包上下文拥有的规范运行时根。
    let runtime_root = package.runtime_root();
    let platform = current_managed_runtime_platform_key()?;
    let runtime_dir = runtime_install_dir(
        runtime_root,
        "node",
        &format!("node-{}-{}", spec.version, platform),
    );
    let runtime_executable =
        resolve_install_executable(&runtime_dir, "node", &spec.version, &platform)?;
    let package_manager = spec.package_manager.as_managed_runtime_str().to_string();
    let package_manager_install_dir = runtime_install_dir(
        runtime_root,
        "node",
        &format!("pnpm-{}", spec.package_manager_version),
    );
    let package_manager_executable = resolve_install_executable(
        &package_manager_install_dir,
        "pnpm",
        &spec.package_manager_version,
        "any",
    )?;
    // Optional canonical package manifest constrained to the trusted package root.
    // 限制在可信包根内的可选规范包清单。
    let package_manifest_path = if spec.package_json.trim().is_empty() {
        None
    } else {
        Some(package.resolve_existing_file(&spec.package_json, "node_runtime.package_json")?)
    };
    let package_manifest_hash = package_manifest_path
        .as_ref()
        .map(|path| sha256_file(path))
        .transpose()?;
    // Canonical lockfile constrained to the trusted package root.
    // 限制在可信包根内的规范锁文件。
    let lockfile_path = package.resolve_existing_file(&spec.lockfile, "node_runtime.lockfile")?;
    let lock_hash = sha256_file(&lockfile_path)?;
    let hash_input = ManagedRuntimeEnvHashInput {
        runtime: ManagedRuntimeKind::Node,
        runtime_version: spec.version.clone(),
        platform,
        package_manager,
        package_manager_version: spec.package_manager_version.clone(),
        lock_hash,
        package_manifest_hash,
    };
    build_env_plan(
        runtime_root,
        ManagedRuntimeKind::Node,
        runtime_executable,
        package_manager_executable,
        package_manifest_path,
        lockfile_path,
        hash_input,
    )
}

/// Read one managed runtime installation manifest from an installation directory.
/// 从安装目录读取一个受管运行时安装清单。
pub fn read_install_manifest(install_dir: &Path) -> Result<ManagedRuntimeInstallManifest, String> {
    let manifest_path = install_dir.join("runtime-manifest.json");
    managed_runtime_install_manifest_path_is_file(&manifest_path)?;
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(&manifest_path),
            error
        )
    })?;
    // PowerShell 5.1 may write UTF-8 JSON files with a BOM when Set-Content is used.
    // PowerShell 5.1 使用 Set-Content 写 UTF-8 JSON 时可能带有 BOM。
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_json::from_str(text).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_managed_runtime_path(&manifest_path),
            error
        )
    })
}

/// Inspect whether one managed-runtime install manifest path is a file without hiding filesystem probe errors.
/// 检查单个受管运行时安装清单路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The manifest_path parameter is the concrete `runtime-manifest.json` path under one install directory.
/// manifest_path 参数是单个安装目录下的具体 `runtime-manifest.json` 路径。
///
/// Return unit for an existing manifest file, or an explicit missing/probe/type error.
/// 已存在清单文件时返回 unit；缺失、探测或类型异常时返回显式错误。
fn managed_runtime_install_manifest_path_is_file(manifest_path: &Path) -> Result<(), String> {
    match fs::metadata(manifest_path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(format!(
            "managed runtime install manifest is not a file: {}",
            render_managed_runtime_path(manifest_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "managed runtime install manifest not found: {}",
            render_managed_runtime_path(manifest_path)
        )),
        Err(error) => Err(format!(
            "Failed to inspect managed runtime install manifest {}: {}",
            render_managed_runtime_path(manifest_path),
            error
        )),
    }
}

/// Build one complete managed runtime environment plan from normalized inputs.
/// 基于规范化输入构造一个完整受管运行时环境计划。
fn build_env_plan(
    runtime_root: &Path,
    runtime: ManagedRuntimeKind,
    runtime_executable: PathBuf,
    package_manager_executable: PathBuf,
    package_manifest_path: Option<PathBuf>,
    lockfile_path: PathBuf,
    hash_input: ManagedRuntimeEnvHashInput,
) -> Result<ManagedRuntimeEnvPlan, String> {
    let env_hash = compute_managed_runtime_env_hash(&hash_input);
    let env_dir = managed_env_dir(
        runtime_root,
        runtime,
        &hash_input.runtime_version,
        &env_hash,
    );
    let expected_marker = ManagedRuntimeEnvMarker::expected(&hash_input, env_hash.clone());
    Ok(ManagedRuntimeEnvPlan {
        runtime,
        platform: hash_input.platform,
        runtime_root: runtime_root.to_path_buf(),
        runtime_version: hash_input.runtime_version,
        runtime_executable,
        package_manager: hash_input.package_manager,
        package_manager_version: hash_input.package_manager_version,
        package_manager_executable,
        package_manifest_path,
        lockfile_path,
        lock_hash: hash_input.lock_hash,
        package_manifest_hash: hash_input.package_manifest_hash,
        env_hash,
        env_dir,
        expected_marker,
    })
}

/// Return one managed runtime installation directory under `runtime_root`.
/// 返回 `runtime_root` 下的单个受管运行时安装目录。
fn runtime_install_dir(runtime_root: &Path, family: &str, name: &str) -> PathBuf {
    runtime_root
        .join("dependencies")
        .join("runtimes")
        .join(family)
        .join(name)
}

/// Resolve and validate one executable from an installation manifest.
/// 从安装清单中解析并校验一个可执行文件。
///
/// `install_dir` is the declared installation boundary; the expected identity fields must match
/// the parsed manifest exactly before its untrusted executable path is resolved.
/// `install_dir` 是声明的安装边界；在解析其不可信可执行文件路径前，期望身份字段必须与清单精确匹配。
///
/// Returns the canonical real ordinary file strictly inside the canonical installation directory.
/// 返回严格位于规范安装目录内的规范真实普通文件。
fn resolve_install_executable(
    install_dir: &Path,
    expected_runtime: &str,
    expected_version: &str,
    expected_platform: &str,
) -> Result<PathBuf, String> {
    // Parsed installation metadata whose identity and executable path remain untrusted.
    // 已解析的安装元数据，其身份与可执行文件路径仍不可信。
    let manifest = read_install_manifest(install_dir)?;
    if manifest.schema_version != 1 {
        return Err(format!(
            "managed runtime manifest {} uses unsupported schema_version {}",
            render_managed_runtime_path(install_dir),
            manifest.schema_version
        ));
    }
    if manifest.runtime != expected_runtime {
        return Err(format!(
            "managed runtime manifest {} has runtime '{}', expected '{}'",
            render_managed_runtime_path(install_dir),
            manifest.runtime,
            expected_runtime
        ));
    }
    if manifest.version != expected_version {
        return Err(format!(
            "managed runtime manifest {} has version '{}', expected '{}'",
            render_managed_runtime_path(install_dir),
            manifest.version,
            expected_version
        ));
    }
    if manifest.platform != expected_platform {
        return Err(format!(
            "managed runtime manifest {} has platform '{}', expected '{}'",
            render_managed_runtime_path(install_dir),
            manifest.platform,
            expected_platform
        ));
    }
    // Manifest paths are data, not authority: only nonempty normal relative components are valid.
    // 清单路径只是数据而非授权：仅允许非空的普通相对路径组件。
    let relative_executable = Path::new(&manifest.executable);
    if !managed_install_executable_relative_path_is_safe(relative_executable) {
        return Err(format!(
            "managed runtime manifest {} has unsafe relative executable path '{}'",
            render_managed_runtime_path(install_dir),
            manifest.executable
        ));
    }
    // Canonical installation root is the authority boundary used after following every link.
    // 规范安装根是在跟随全部链接后使用的授权边界。
    let canonical_install_dir = fs::canonicalize(install_dir).map_err(|error| {
        format!(
            "Failed to canonicalize managed runtime install directory {}: {}",
            render_managed_runtime_path(install_dir),
            error
        )
    })?;
    if !canonical_install_dir.is_dir() {
        return Err(format!(
            "managed runtime install path is not a directory: {}",
            render_managed_runtime_path(&canonical_install_dir)
        ));
    }
    // Canonical executable resolution rejects missing targets and exposes symlink escapes.
    // 规范可执行文件解析会拒绝缺失目标并暴露符号链接逃逸。
    let executable_candidate = canonical_install_dir.join(relative_executable);
    let canonical_executable = fs::canonicalize(&executable_candidate).map_err(|error| {
        format!(
            "Failed to canonicalize managed runtime executable {}: {}",
            render_managed_runtime_path(&executable_candidate),
            error
        )
    })?;
    if canonical_executable == canonical_install_dir
        || !canonical_executable.starts_with(&canonical_install_dir)
    {
        return Err(format!(
            "managed runtime executable {} escapes install directory {}",
            render_managed_runtime_path(&canonical_executable),
            render_managed_runtime_path(&canonical_install_dir)
        ));
    }
    // Followed-target metadata used to require one real ordinary file rather than a directory.
    // 跟随目标后的元数据，用于要求真实普通文件而非目录。
    let executable_metadata = fs::metadata(&canonical_executable).map_err(|error| {
        format!(
            "Failed to inspect managed runtime executable {}: {}",
            render_managed_runtime_path(&canonical_executable),
            error
        )
    })?;
    if !executable_metadata.is_file() {
        return Err(format!(
            "managed runtime executable is not a file: {}",
            render_managed_runtime_path(&canonical_executable)
        ));
    }
    Ok(canonical_executable)
}

/// Return whether one manifest executable is a nonempty platform-safe relative path.
/// 返回单个清单可执行文件是否为非空且平台安全的相对路径。
///
/// `path` is untrusted manifest data and therefore may contain roots, parent traversal, prefixes,
/// or Windows alternate-data-stream syntax.
/// `path` 是不可信清单数据，因此可能包含根、父目录穿越、前缀或 Windows 备用数据流语法。
///
/// Returns true only when every component is an ordinary safe filename component.
/// 仅当每个组件都是普通安全文件名组件时返回 true。
fn managed_install_executable_relative_path_is_safe(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|component| match component {
            Component::Normal(value) => managed_install_executable_component_is_safe(value),
            _ => false,
        })
}

/// Return whether one normal manifest path component is safe on Windows.
/// 返回单个普通清单路径组件在 Windows 上是否安全。
///
/// `component` is one normal relative component; a colon would select an NTFS alternate data
/// stream rather than the real ordinary file required by the install contract.
/// `component` 是一个普通相对组件；冒号会选择 NTFS 备用数据流，而非安装契约要求的真实普通文件。
///
/// Returns false for alternate-data-stream syntax and true for other normal components.
/// 对备用数据流语法返回 false，对其他普通组件返回 true。
#[cfg(windows)]
fn managed_install_executable_component_is_safe(component: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    !component.encode_wide().any(|unit| unit == u16::from(b':'))
}

/// Return whether one normal manifest path component is safe on non-Windows platforms.
/// 返回单个普通清单路径组件在非 Windows 平台上是否安全。
///
/// `component` has already been classified as an ordinary relative component.
/// `component` 已被分类为普通相对组件。
///
/// Returns true because non-Windows component parsing has no NTFS alternate-stream syntax.
/// 返回 true，因为非 Windows 组件解析不存在 NTFS 备用数据流语法。
#[cfg(not(windows))]
fn managed_install_executable_component_is_safe(_component: &std::ffi::OsStr) -> bool {
    true
}

/// Feed one labeled string field into a stable SHA-256 hasher.
/// 将一个带标签的字符串字段写入稳定 SHA-256 hasher。
fn hash_field(hasher: &mut Sha256, label: &str, value: impl AsRef<str>) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_ref().as_bytes());
    hasher.update([0xff]);
}

/// Return the directory prefix used for runtime-version environment groups.
/// 返回运行时版本环境分组使用的目录前缀。
fn runtime_prefix(runtime: ManagedRuntimeKind) -> &'static str {
    match runtime {
        ManagedRuntimeKind::Python => "py",
        ManagedRuntimeKind::Node => "node",
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::managed_install_executable_relative_path_is_safe;
    use super::{
        ManagedRuntimeBackupRecoveryAction, ManagedRuntimeBackupRecoveryRecord,
        ManagedRuntimeEnvHashInput, ManagedRuntimeEnvMarker, ManagedRuntimeEnvPlan,
        ManagedRuntimeKind, acquire_managed_runtime_env_file_lock, acquire_ready_managed_env_lease,
        capture_managed_directory_identity, compute_managed_runtime_env_hash,
        configure_managed_command_base_environment, copy_dir_recursive,
        current_managed_runtime_platform_key, finish_build_dir,
        finish_build_dir_with_backup_remover, managed_env_dir, managed_env_is_ready,
        managed_env_marker_matches, managed_env_marker_path, prepare_build_dir_with_sequence,
        read_install_manifest, reserve_managed_runtime_backup_recovery_slot,
        resolve_install_executable, resolve_node_env_plan, resolve_python_env_plan, sha256_hex,
        verify_managed_runtime_build_input_hash, write_expected_marker,
    };
    use crate::runtime::managed_package::ManagedRuntimePackageContext;
    use crate::runtime::path::render_host_visible_path;
    use crate::skill::dependencies::{
        NodeRuntimeDependencySpec, NodeRuntimePackageManager, PythonRuntimeDependencySpec,
        PythonRuntimePackageManager,
    };
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink as create_unix_symlink;
    #[cfg(windows)]
    use std::os::windows::fs::{
        symlink_dir as create_windows_directory_symlink,
        symlink_file as create_windows_file_symlink,
    };
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    /// Create one test file symlink that points at the requested target path.
    /// 创建一个指向指定目标路径的测试文件符号链接。
    #[cfg(unix)]
    fn create_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
        create_unix_symlink(target_path, link_path).expect("create test file symlink");
        true
    }

    /// Create one test directory symlink that points at the requested target path.
    /// 创建一个指向指定目标路径的测试目录符号链接。
    #[cfg(unix)]
    fn create_test_directory_symlink(link_path: &Path, target_path: &Path) -> bool {
        create_unix_symlink(target_path, link_path).expect("create test directory symlink");
        true
    }

    /// Build one trusted Skill package context backed by real temporary directories.
    /// 基于真实临时目录构造一个可信 Skill 包上下文。
    ///
    /// `package_root` identifies the package directory used to resolve dependency files.
    /// `package_root` 标识用于解析依赖文件的包目录。
    ///
    /// `runtime_root` identifies the runtime directory that owns managed installations and environments.
    /// `runtime_root` 标识拥有受管安装与环境的运行时目录。
    ///
    /// Return the validated package context required by managed runtime plan resolvers.
    /// 返回受管运行时计划解析器所需的已验证包上下文。
    fn make_test_package_context(
        package_root: &Path,
        runtime_root: &Path,
    ) -> Arc<ManagedRuntimePackageContext> {
        fs::create_dir_all(package_root).expect("create managed runtime test package root");
        fs::create_dir_all(runtime_root).expect("create managed runtime test runtime root");
        ManagedRuntimePackageContext::for_skill(
            "managed-runtime-test",
            package_root,
            runtime_root,
            None,
        )
        .expect("build managed runtime test package context")
    }

    /// Return whether one Windows symlink-dependent test should be skipped because the host lacks symlink privileges.
    /// 返回当前 Windows 符号链接相关测试是否应因宿主缺少符号链接权限而跳过。
    #[cfg(windows)]
    fn should_skip_windows_symlink_test(error: &std::io::Error) -> bool {
        /// Windows privilege error returned when symlink creation requires elevation or Developer Mode.
        /// 当符号链接创建需要管理员权限或开发者模式时 Windows 返回的权限错误码。
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

        error.kind() == std::io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
    }

    /// Create one test file symlink that points at the requested target path.
    /// 创建一个指向指定目标路径的测试文件符号链接。
    #[cfg(windows)]
    fn create_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
        match create_windows_file_symlink(target_path, link_path) {
            Ok(()) => true,
            Err(error) if should_skip_windows_symlink_test(&error) => {
                eprintln!(
                    "skip symlink-dependent test because Windows symlink privileges are unavailable: {error}"
                );
                false
            }
            Err(error) => panic!("create test file symlink: {error}"),
        }
    }

    /// Create one test directory symlink that points at the requested target path.
    /// 创建一个指向指定目标路径的测试目录符号链接。
    #[cfg(windows)]
    fn create_test_directory_symlink(link_path: &Path, target_path: &Path) -> bool {
        match create_windows_directory_symlink(target_path, link_path) {
            Ok(()) => true,
            Err(error) if should_skip_windows_symlink_test(&error) => {
                eprintln!(
                    "skip directory-symlink test because Windows symlink privileges are unavailable: {error}"
                );
                false
            }
            Err(error) => panic!("create test directory symlink: {error}"),
        }
    }

    /// Verify that environment hashes are stable and include lockfile content changes.
    /// 验证环境哈希保持稳定，并且会纳入锁文件内容变化。
    #[test]
    fn env_hash_is_stable_and_lock_sensitive() {
        let input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Python,
            runtime_version: "3.12.8".to_string(),
            platform: "windows-x64".to_string(),
            package_manager: "uv".to_string(),
            package_manager_version: "0.11.28".to_string(),
            lock_hash: sha256_hex(b"requests==2.32.3"),
            package_manifest_hash: None,
        };
        let same_hash = compute_managed_runtime_env_hash(&input);
        let repeated_hash = compute_managed_runtime_env_hash(&input);
        let changed_hash = compute_managed_runtime_env_hash(&ManagedRuntimeEnvHashInput {
            lock_hash: sha256_hex(b"requests==2.32.4"),
            ..input
        });

        assert_eq!(same_hash, repeated_hash);
        assert_ne!(same_hash, changed_hash);
    }

    /// Verify that a copied build input cannot diverge from its plan-time environment identity.
    /// 验证已复制构建输入不能偏离其计划阶段环境身份。
    #[test]
    fn managed_runtime_build_input_requires_exact_planned_hash() {
        // PrivateCopy models the only file path later supplied to uv or pnpm.
        // PrivateCopy 模拟之后传给 uv 或 pnpm 的唯一文件路径。
        let root = make_test_root("verified-build-input");
        fs::create_dir_all(&root).expect("create verified build input root");
        let private_copy = root.join("dependency.lock");
        fs::write(&private_copy, b"version=one").expect("write planned build input");
        let expected_hash = sha256_hex(b"version=one");

        verify_managed_runtime_build_input_hash(
            &private_copy,
            &expected_hash,
            "test dependency input",
        )
        .expect("accept exact planned build input");
        fs::write(&private_copy, b"version=two").expect("replace copied build input bytes");
        let error = verify_managed_runtime_build_input_hash(
            &private_copy,
            &expected_hash,
            "test dependency input",
        )
        .expect_err("reject build input changed after planning");
        assert!(error.contains("changed after environment planning"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that expected markers match only exact environment identities.
    /// 验证期望标记只匹配完全一致的环境身份。
    #[test]
    fn marker_match_requires_exact_identity() {
        let input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Node,
            runtime_version: "24.18.0".to_string(),
            platform: "linux-x64".to_string(),
            package_manager: "pnpm".to_string(),
            package_manager_version: "11.11.0".to_string(),
            lock_hash: "lock".to_string(),
            package_manifest_hash: Some("package".to_string()),
        };
        let env_hash = compute_managed_runtime_env_hash(&input);
        let expected = ManagedRuntimeEnvMarker::expected(&input, env_hash);
        let mut actual = expected.clone();

        assert!(managed_env_marker_matches(&actual, &expected));
        actual.lock_hash = "other".to_string();
        assert!(!managed_env_marker_matches(&actual, &expected));
    }

    /// Verify that managed environment directories use runtime and version group names.
    /// 验证受管环境目录会使用运行时与版本分组名称。
    #[test]
    fn managed_env_dir_uses_runtime_version_group() {
        let path = managed_env_dir(
            Path::new("runtime-root"),
            ManagedRuntimeKind::Python,
            "3.12.8",
            "abc",
        );
        assert!(path.ends_with(Path::new("dependencies/envs/python/py-3.12.8/abc")));
    }

    /// Verify that marker readiness checks return true only after writing the expected marker.
    /// 验证 marker 就绪检查只有在写入期望标记后才返回 true。
    #[test]
    fn managed_env_ready_checks_expected_marker() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("ready-marker");
        let runtime_root = root.join("runtime");
        let package_root = root.join("skill");
        fs::create_dir_all(package_root.join("python")).unwrap();
        fs::write(
            package_root.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.14.4-{}", platform)),
            "python",
            "3.14.4",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );
        let package = make_test_package_context(&package_root, &runtime_root);
        let plan = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect("python env plan should resolve");

        assert!(!managed_env_is_ready(&plan).expect("ready check should work"));
        fs::create_dir_all(&plan.env_dir).unwrap();
        write_expected_marker(&plan.env_dir, &plan.expected_marker).unwrap();
        assert!(managed_env_is_ready(&plan).expect("ready check should work"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify a matching marker cannot make a symbolic-link environment directory trusted.
    /// 验证匹配 marker 不能使符号链接环境目录获得信任。
    #[test]
    fn managed_env_ready_rejects_symbolic_link_environment_directory() {
        // Plan points at one absent stable name while Outside contains a forged matching marker.
        // Plan 指向一个不存在的稳定名称，Outside 包含伪造的匹配 marker。
        let root = make_test_root("symlink-environment-directory");
        let plan = make_environment_file_lock_test_plan(&root);
        let outside = root.join("outside-environment");
        fs::create_dir_all(plan.env_dir.parent().expect("managed env parent"))
            .expect("create managed env parent");
        fs::create_dir(&outside).expect("create outside environment");
        write_expected_marker(&outside, &plan.expected_marker)
            .expect("write forged outside environment marker");
        if !create_test_directory_symlink(&plan.env_dir, &outside) {
            let _ = fs::remove_dir_all(root);
            return;
        }

        let error = managed_env_is_ready(&plan)
            .expect_err("symbolic-link environment directory must be rejected");
        assert!(error.contains("must not be a symbolic link"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify a real environment cannot accept a symbolic-link readiness marker.
    /// 验证真实环境不能接受符号链接形式的就绪 marker。
    #[test]
    fn managed_env_ready_rejects_symbolic_link_marker() {
        // OutsideMarker carries valid bytes but its foreign name cannot become trusted by linking.
        // OutsideMarker 携带有效字节，但其外部名称不能通过链接获得信任。
        let root = make_test_root("symlink-environment-marker");
        let plan = make_environment_file_lock_test_plan(&root);
        fs::create_dir_all(&plan.env_dir).expect("create real managed environment");
        let outside = root.join("outside-marker-root");
        fs::create_dir(&outside).expect("create outside marker root");
        write_expected_marker(&outside, &plan.expected_marker)
            .expect("write forged outside marker");
        let outside_marker = managed_env_marker_path(&outside);
        let marker_link = managed_env_marker_path(&plan.env_dir);
        if !create_test_file_symlink(&marker_link, &outside_marker) {
            let _ = fs::remove_dir_all(root);
            return;
        }

        let error = managed_env_is_ready(&plan)
            .expect_err("symbolic-link environment marker must be rejected");
        assert!(error.contains("must not be a symbolic link"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify marker readiness checks reject marker paths that exist as directories.
    /// 验证 marker 就绪检查会拒绝以目录形式存在的 marker 路径。
    #[test]
    fn managed_env_ready_rejects_directory_marker() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates the directory marker fixture.
        // 隔离目录型 marker 夹具的临时根目录。
        let root = make_test_root("directory-marker");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Skill directory containing the Python lockfile fixture.
        // 包含 Python lockfile 夹具的 skill 目录。
        let package_root = root.join("skill");
        fs::create_dir_all(package_root.join("python")).unwrap();
        fs::write(
            package_root.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.14.4-{}", platform)),
            "python",
            "3.14.4",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );
        // Environment plan whose marker path will be occupied by a directory.
        // 其 marker 路径将被目录占用的环境计划。
        let package = make_test_package_context(&package_root, &runtime_root);
        let plan = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect("python env plan should resolve");
        // Directory occupying the exact marker file path.
        // 占据精确 marker 文件路径的目录。
        let marker_path = managed_env_marker_path(&plan.env_dir);
        fs::create_dir_all(&marker_path).expect("directory marker should be created");

        // Error returned before the directory marker can behave like a missing marker.
        // 在目录型 marker 表现得像缺失 marker 之前返回的错误。
        let error =
            managed_env_is_ready(&plan).expect_err("directory marker readiness check should fail");

        assert!(
            error.contains("Managed runtime env marker is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&marker_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify install manifest readers reject directory manifests before JSON reading.
    /// 验证安装清单读取器会在 JSON 读取前拒绝目录型清单。
    #[test]
    fn read_install_manifest_rejects_directory_manifest() {
        // Temporary root that isolates the directory install-manifest fixture.
        // 隔离目录型安装清单夹具的临时根目录。
        let root = make_test_root("directory-install-manifest");
        // Install directory whose runtime-manifest path will be occupied by a directory.
        // 其 runtime-manifest 路径将被目录占用的安装目录。
        let install_dir = root.join("runtime-install");
        // Directory occupying the exact runtime-manifest.json path.
        // 占据精确 runtime-manifest.json 路径的目录。
        let manifest_path = install_dir.join("runtime-manifest.json");
        fs::create_dir_all(&manifest_path).expect("directory install manifest should be created");

        // Error returned before the directory manifest can be passed to read_to_string.
        // 在目录型清单被传递给 read_to_string 之前返回的错误。
        let error =
            read_install_manifest(&install_dir).expect_err("directory manifest should fail");

        assert!(
            error.contains("managed runtime install manifest is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&manifest_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify install manifests reject absolute and parent-traversing executable paths.
    /// 验证安装清单会拒绝绝对及父目录穿越形式的可执行文件路径。
    #[test]
    fn install_manifest_rejects_unsafe_relative_executable_paths() {
        // Temporary root containing one isolated synthetic installation.
        // 包含单个隔离合成安装的临时根目录。
        let root = make_test_root("unsafe-install-executable-path");
        // Installation directory whose manifest is rewritten for each unsafe path.
        // 针对每个不安全路径重写清单的安装目录。
        let install_dir = root.join("runtime-install");
        fs::create_dir_all(&install_dir).expect("create unsafe manifest install directory");
        // Existing outside file used to form a concrete absolute-path attack.
        // 用于构造具体绝对路径攻击的既有外部文件。
        let outside_executable = root.join("outside-tool");
        fs::write(&outside_executable, b"outside").expect("write outside executable fixture");
        // Unsafe paths that must be rejected before filesystem target authorization.
        // 必须在文件系统目标授权前被拒绝的不安全路径。
        let unsafe_paths = [
            outside_executable.to_string_lossy().into_owned(),
            "../outside-tool".to_string(),
            "bin/../outside-tool".to_string(),
            "./tool".to_string(),
        ];
        for unsafe_path in unsafe_paths {
            // Strict manifest payload carrying the current unsafe executable value.
            // 携带当前不安全可执行文件值的严格清单载荷。
            let payload = serde_json::json!({
                "schema_version": 1,
                "runtime": "tool",
                "version": "1.0.0",
                "platform": "test",
                "executable": unsafe_path,
            });
            fs::write(
                install_dir.join("runtime-manifest.json"),
                serde_json::to_vec_pretty(&payload).expect("encode unsafe install manifest"),
            )
            .expect("write unsafe install manifest");

            // Explicit validation error proving the untrusted path never becomes authority.
            // 证明不可信路径绝不会成为授权依据的显式校验错误。
            let error = resolve_install_executable(&install_dir, "tool", "1.0.0", "test")
                .expect_err("unsafe install executable path should fail");

            assert!(
                error.contains("unsafe relative executable path"),
                "unexpected error for {unsafe_path:?}: {error}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Verify Windows alternate-data-stream syntax is not accepted as a real executable file.
    /// 验证 Windows 备用数据流语法不会被接受为真实可执行文件。
    #[cfg(windows)]
    #[test]
    fn install_manifest_rejects_windows_alternate_data_stream_path() {
        assert!(!managed_install_executable_relative_path_is_safe(
            Path::new("bin/tool.exe:payload")
        ));
    }

    /// Verify a manifest executable symlink cannot escape its canonical installation root.
    /// 验证清单可执行文件符号链接无法逃逸其规范安装根。
    #[test]
    fn install_manifest_rejects_executable_symlink_escape() {
        // Temporary root separating the installation boundary from the outside target.
        // 将安装边界与外部目标隔离的临时根目录。
        let root = make_test_root("install-executable-symlink-escape");
        // Installation directory that owns the untrusted symlink entry.
        // 拥有不可信符号链接条目的安装目录。
        let install_dir = root.join("runtime-install");
        fs::create_dir_all(install_dir.join("bin")).expect("create symlink install bin directory");
        // Real ordinary file intentionally located outside the installation boundary.
        // 有意位于安装边界之外的真实普通文件。
        let outside_executable = root.join("outside-tool");
        fs::write(&outside_executable, b"outside").expect("write symlink target fixture");
        // Manifest-relative link whose canonical target leaves the install directory.
        // 规范目标离开安装目录的清单相对链接。
        let executable_link = install_dir.join("bin/tool");
        if !create_test_file_symlink(&executable_link, &outside_executable) {
            let _ = fs::remove_dir_all(root);
            return;
        }
        // Validly shaped manifest isolates the canonical containment check from syntax checks.
        // 形态合法的清单可将规范包含校验与语法校验隔离。
        let payload = serde_json::json!({
            "schema_version": 1,
            "runtime": "tool",
            "version": "1.0.0",
            "platform": "test",
            "executable": "bin/tool",
        });
        fs::write(
            install_dir.join("runtime-manifest.json"),
            serde_json::to_vec_pretty(&payload).expect("encode symlink escape manifest"),
        )
        .expect("write symlink escape manifest");

        // Error emitted only after the symlink target has been canonicalized.
        // 仅在符号链接目标完成规范化后产生的错误。
        let error = resolve_install_executable(&install_dir, "tool", "1.0.0", "test")
            .expect_err("symlink escape should fail");

        assert!(
            error.contains("escapes install directory"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains(&render_host_visible_path(
                &fs::canonicalize(&outside_executable).expect("canonical outside executable")
            )),
            "unexpected error: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify the managed command baseline removes secrets and preserves only controlled PATH entries.
    /// 验证受管命令基线会移除密钥，并仅保留受控 PATH 条目。
    #[test]
    fn managed_command_environment_hides_host_secrets_and_keeps_controlled_path() {
        // Temporary controlled home and executable-search directories.
        // 临时受控 home 与可执行文件搜索目录。
        let root = make_test_root("managed-command-environment");
        // Isolated home/temp root supplied to the child.
        // 提供给子进程的隔离 home/temp 根。
        let controlled_home = root.join("home");
        // Two exact PATH entries proving order and exclusivity.
        // 用于证明顺序与排他性的两个精确 PATH 条目。
        let first_bin = root.join("runtime-bin");
        let second_bin = root.join("package-manager-bin");
        fs::create_dir_all(&controlled_home).expect("create controlled command home");
        fs::create_dir_all(&first_bin).expect("create first controlled bin");
        fs::create_dir_all(&second_bin).expect("create second controlled bin");
        // Current test executable is a real ordinary binary that must still launch after env_clear.
        // 当前测试可执行文件是真实普通二进制文件，在 env_clear 后仍必须能够启动。
        let current_executable = std::env::current_exe().expect("resolve current test executable");
        // Child test-harness invocation seeded with a representative host credential.
        // 预置代表性宿主凭据的子测试框架调用。
        let mut command = Command::new(&current_executable);
        command
            .arg("--list")
            .env("AWS_SECRET_ACCESS_KEY", "must-not-cross-managed-boundary");
        configure_managed_command_base_environment(
            &mut command,
            &controlled_home,
            &[first_bin.clone(), second_bin.clone()],
        )
        .expect("configure isolated managed command environment");
        // Explicit environment entries retained after env_clear and controlled reconstruction.
        // env_clear 与受控重建后保留的显式环境条目。
        let command_environment = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_os_string()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert!(!command_environment.contains_key("AWS_SECRET_ACCESS_KEY"));
        // Platform-native PATH value reconstructed from only the two authorized directories.
        // 仅根据两个已授权目录重建的平台原生 PATH 值。
        let expected_path =
            std::env::join_paths([&first_bin, &second_bin]).expect("join expected controlled PATH");
        assert_eq!(
            command_environment
                .get("PATH")
                .and_then(Option::as_ref)
                .expect("controlled PATH environment"),
            &expected_path
        );
        assert_eq!(
            command_environment
                .get("HOME")
                .and_then(Option::as_ref)
                .expect("controlled HOME environment"),
            controlled_home.as_os_str()
        );
        // Exit status proving a real executable remains launchable with the minimal environment.
        // 用于证明真实可执行文件在最小环境中仍可启动的退出状态。
        let status = command
            .status()
            .expect("launch with managed command environment");
        assert!(
            status.success(),
            "minimal managed environment should launch"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Build one synthetic environment plan shared by the parent and child lock probes.
    /// 构造一份由父子锁探针共享的合成环境计划。
    ///
    /// `root` is the cross-process fixture root and the return value uses one stable env hash.
    /// `root` 是跨进程夹具根，返回值使用一个稳定环境哈希。
    fn make_environment_file_lock_test_plan(root: &Path) -> ManagedRuntimeEnvPlan {
        let hash_input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Python,
            runtime_version: "3.14.4".to_string(),
            platform: "test-platform".to_string(),
            package_manager: "uv".to_string(),
            package_manager_version: "0.11.28".to_string(),
            lock_hash: "lock".to_string(),
            package_manifest_hash: None,
        };
        let env_hash = "cross-process-lock-hash".to_string();
        ManagedRuntimeEnvPlan {
            runtime: ManagedRuntimeKind::Python,
            platform: hash_input.platform.clone(),
            runtime_root: root.to_path_buf(),
            runtime_version: hash_input.runtime_version.clone(),
            runtime_executable: PathBuf::from("python"),
            package_manager: hash_input.package_manager.clone(),
            package_manager_version: hash_input.package_manager_version.clone(),
            package_manager_executable: PathBuf::from("uv"),
            package_manifest_path: None,
            lockfile_path: root.join("requirements.lock"),
            lock_hash: hash_input.lock_hash.clone(),
            package_manifest_hash: None,
            env_hash: env_hash.clone(),
            env_dir: root.join("envs").join(&env_hash),
            expected_marker: ManagedRuntimeEnvMarker::expected(&hash_input, env_hash),
        }
    }

    /// Child-process probe that blocks on the exact OS file lock selected by its parent test.
    /// 子进程探针，会在父测试选定的精确操作系统文件锁上阻塞。
    #[test]
    fn managed_runtime_environment_file_lock_child_probe() {
        let Some(root) = std::env::var_os("LUASKILLS_TEST_ENV_LOCK_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let started_path = root.join("child-started");
        let acquired_path = root.join("child-acquired");
        fs::write(&started_path, b"started").expect("write child lock start signal");
        let plan = make_environment_file_lock_test_plan(&root);
        let guard = acquire_managed_runtime_env_file_lock(&plan)
            .expect("child acquires managed environment file lock");
        fs::write(&acquired_path, b"acquired").expect("write child lock acquisition signal");
        drop(guard);
    }

    /// Verify separate processes serialize construction for one exact environment identity.
    /// 验证独立进程会为同一个精确环境身份串行化构建过程。
    #[test]
    fn managed_runtime_environment_file_lock_serializes_processes() {
        let root = make_test_root("cross-process-env-lock");
        fs::create_dir_all(&root).expect("create file-lock test root");
        let plan = make_environment_file_lock_test_plan(&root);
        // FirstGuard owns the OS lock while another process opens and locks a distinct handle.
        // FirstGuard 持有操作系统锁，同时另一个进程会打开并锁定独立句柄。
        let first_guard = acquire_managed_runtime_env_file_lock(&plan)
            .expect("acquire first managed environment file lock");
        let started_path = root.join("child-started");
        let acquired_path = root.join("child-acquired");
        let current_test_executable =
            std::env::current_exe().expect("resolve current test executable");
        let mut child = Command::new(current_test_executable)
            .arg("runtime::managed_runtime::tests::managed_runtime_environment_file_lock_child_probe")
            .arg("--exact")
            .arg("--nocapture")
            .env("LUASKILLS_TEST_ENV_LOCK_ROOT", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cross-process file-lock probe");
        // Full-suite process pressure can delay test-binary startup substantially on Windows CI.
        // Windows CI 全量测试的进程压力可能显著延迟测试二进制启动。
        let start_deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !started_path.exists() && std::time::Instant::now() < start_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started_path.exists(), "child lock probe must start");
        thread::sleep(Duration::from_millis(150));
        assert!(
            !acquired_path.exists(),
            "child process must block while the parent lock is held"
        );
        drop(first_guard);
        let exit_deadline = std::time::Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll file-lock child") {
                break status;
            }
            if std::time::Instant::now() >= exit_deadline {
                let _ = child.kill();
                let reap_deadline = std::time::Instant::now() + Duration::from_secs(5);
                while child.try_wait().ok().flatten().is_none()
                    && std::time::Instant::now() < reap_deadline
                {
                    thread::sleep(Duration::from_millis(10));
                }
                panic!("file-lock child did not finish after parent released the lock");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "file-lock child failed: {status}");
        assert!(
            acquired_path.exists(),
            "child must acquire the released lock"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Child-process probe that retains one shared environment lifecycle lease until released.
    /// 子进程探针，会保留一个共享环境生命周期租约直至收到释放信号。
    #[test]
    fn managed_runtime_environment_lifecycle_lease_child_probe() {
        // Root is present only when the parent cross-process lifecycle test launches this probe.
        // Root 仅在父级跨进程生命周期测试启动当前探针时存在。
        let Some(root) = std::env::var_os("LUASKILLS_TEST_ENV_LEASE_ROOT") else {
            return;
        };
        // RootPath identifies the exact ready environment and synchronization files.
        // RootPath 标识精确就绪环境与同步文件。
        let root = PathBuf::from(root);
        // Plan matches the parent process environment identity exactly.
        // Plan 与父进程环境身份完全匹配。
        let plan = make_environment_file_lock_test_plan(&root);
        // Lease is retained until the parent explicitly permits process exit.
        // Lease 会保留到父进程显式允许当前进程退出为止。
        let lease = acquire_ready_managed_env_lease(&plan)
            .expect("child acquires shared managed environment lifecycle lease");
        // AcquiredPath publishes that the operating-system shared lock is already held.
        // AcquiredPath 发布操作系统共享锁已经持有这一事实。
        let acquired_path = root.join("lease-acquired");
        fs::write(&acquired_path, b"acquired").expect("write lifecycle lease acquisition signal");
        // ReleasePath is created by the parent after it observes the expected busy result.
        // ReleasePath 会在父进程观察到预期 busy 结果后由父进程创建。
        let release_path = root.join("lease-release");
        // Deadline prevents one failed parent from leaving the child probe alive indefinitely.
        // Deadline 防止失败的父进程让子探针无限期存活。
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !release_path.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            release_path.exists(),
            "parent must release lifecycle lease probe"
        );
        drop(lease);
    }

    /// Verify a cross-process shared lifecycle lease blocks replacement until it is released.
    /// 验证跨进程共享生命周期租约会阻止替换，直至该租约被释放。
    #[test]
    fn managed_runtime_environment_lifecycle_lease_guards_publication() {
        // Root isolates the published environment, replacement build, lease file, and child signals.
        // Root 隔离已发布环境、替换构建、租约文件及子进程信号。
        let root = make_test_root("cross-process-env-lifecycle-lease");
        fs::create_dir_all(&root).expect("create lifecycle lease test root");
        // Plan supplies one stable environment identity shared with the child probe.
        // Plan 提供与子探针共享的稳定环境身份。
        let plan = make_environment_file_lock_test_plan(&root);
        fs::create_dir_all(&plan.env_dir).expect("create published lifecycle test environment");
        write_expected_marker(&plan.env_dir, &plan.expected_marker)
            .expect("write published lifecycle test marker");
        // OldMarker proves the active published environment remains untouched after a busy result.
        // OldMarker 证明返回 busy 后活动的已发布环境保持未被修改。
        let old_marker = plan.env_dir.join("old-environment");
        fs::write(&old_marker, b"old").expect("write old environment marker");
        // BuildDir is the complete unpublished replacement reused after lease release.
        // BuildDir 是在租约释放后复用的完整未发布替换目录。
        let build_dir = root.join("replacement-build");
        fs::create_dir_all(&build_dir).expect("create lifecycle replacement build");
        // NewMarker proves the replacement becomes authoritative only after successful publication.
        // NewMarker 证明替换仅在成功发布后成为权威环境。
        let new_marker_name = "new-environment";
        fs::write(build_dir.join(new_marker_name), b"new")
            .expect("write replacement environment marker");
        // ChildExecutable is the current test binary used for a real cross-process lease owner.
        // ChildExecutable 是用于创建真实跨进程租约所有者的当前测试二进制文件。
        let child_executable = std::env::current_exe().expect("resolve lifecycle test executable");
        // Child retains the shared lease while the parent attempts nonblocking publication.
        // Child 会在父进程尝试非阻塞发布期间保留共享租约。
        let mut child = Command::new(child_executable)
            .arg(
                "runtime::managed_runtime::tests::managed_runtime_environment_lifecycle_lease_child_probe",
            )
            .arg("--exact")
            .arg("--nocapture")
            .env("LUASKILLS_TEST_ENV_LEASE_ROOT", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lifecycle lease child probe");
        // AcquiredPath is written only after the child owns its shared OS lock.
        // AcquiredPath 仅在子进程拥有共享操作系统锁后写入。
        let acquired_path = root.join("lease-acquired");
        // AcquireDeadline tolerates slow test-binary startup under full-suite process pressure.
        // AcquireDeadline 容忍全量测试进程压力下较慢的测试二进制启动。
        let acquire_deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !acquired_path.exists() && std::time::Instant::now() < acquire_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !acquired_path.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child lifecycle lease probe must acquire its shared lock");
        }

        // BusyError proves publication is nonblocking and leaves both directories owned in place.
        // BusyError 证明发布为非阻塞操作，并让两个目录都保持原位且所有权不变。
        let busy_error = finish_build_dir(&plan, build_dir.clone())
            .expect_err("active shared lifecycle lease must block environment replacement");
        assert!(
            busy_error.contains("busy with active lifecycle leases"),
            "unexpected lifecycle lease error: {busy_error}"
        );
        assert!(old_marker.is_file());
        assert!(build_dir.join(new_marker_name).is_file());

        // ReleasePath permits the child to drop its shared lease before the second publication.
        // ReleasePath 允许子进程在第二次发布前释放其共享租约。
        let release_path = root.join("lease-release");
        fs::write(&release_path, b"release").expect("release lifecycle lease child probe");
        // ExitDeadline bounds child shutdown after its lease is released.
        // ExitDeadline 限制子进程释放租约后的退出等待时长。
        let exit_deadline = std::time::Instant::now() + Duration::from_secs(10);
        // ChildStatus is captured only after definitive child exit.
        // ChildStatus 仅在子进程确定退出后捕获。
        let child_status = loop {
            if let Some(status) = child.try_wait().expect("poll lifecycle lease child") {
                break status;
            }
            if std::time::Instant::now() >= exit_deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("lifecycle lease child did not finish after release");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(
            child_status.success(),
            "lifecycle lease child failed: {child_status}"
        );

        finish_build_dir(&plan, build_dir)
            .expect("environment replacement succeeds after shared lease release");
        assert!(plan.env_dir.join(new_marker_name).is_file());
        assert!(!plan.env_dir.join("old-environment").exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Verify committed publication succeeds while obsolete-backup deletion retries durably.
    /// 验证已提交发布会成功返回，同时持久重试过时备份删除。
    #[test]
    fn managed_runtime_committed_backup_cleanup_failure_is_retried() {
        // Root and Plan isolate one published environment plus its complete replacement build.
        // Root 与 Plan 隔离单个已发布环境及其完整替换构建。
        let root = make_test_root("committed-backup-cleanup-retry");
        fs::create_dir_all(&root).expect("create committed backup retry root");
        let plan = make_environment_file_lock_test_plan(&root);
        fs::create_dir_all(&plan.env_dir).expect("create old managed environment");
        fs::write(plan.env_dir.join("old-environment"), b"old")
            .expect("write old managed environment evidence");
        let build_dir = root.join("replacement-build");
        fs::create_dir(&build_dir).expect("create replacement build directory");
        fs::write(build_dir.join("new-environment"), b"new")
            .expect("write new managed environment evidence");
        // CapturedBackup receives the exact identity-bound path handed to the retry worker.
        // CapturedBackup 接收移交给重试 Worker 的精确身份绑定路径。
        let captured_backup = Arc::new(Mutex::new(None::<PathBuf>));
        let captured_backup_for_remover = Arc::clone(&captured_backup);

        let result = finish_build_dir_with_backup_remover(
            &plan,
            build_dir,
            move |backup_path, _identity| {
                *captured_backup_for_remover
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(backup_path.to_path_buf());
                Err("forced obsolete-backup removal failure".to_string())
            },
        );

        assert!(
            result.is_ok(),
            "committed publication must remain successful"
        );
        assert!(plan.env_dir.join("new-environment").is_file());
        assert!(!plan.env_dir.join("old-environment").exists());
        let backup_path = captured_backup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("capture committed backup retry path");
        // Deadline bounds the persistent worker while tolerating full-suite scheduling pressure.
        // Deadline 限制持久 Worker 等待时间，同时容忍全量测试调度压力。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while backup_path.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !backup_path.exists(),
            "persistent recovery worker must delete the obsolete backup"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify an immediate cleanup panic cannot lose the committed backup recovery record.
    /// 验证即时清理 panic 不会丢失已提交备份的恢复记录。
    #[test]
    fn managed_runtime_committed_backup_cleanup_panic_is_retried() {
        // Root and Plan isolate one replacement whose immediate remover panics after capture.
        // Root 与 Plan 隔离单个替换事务，其即时删除器会在捕获路径后 panic。
        let root = make_test_root("committed-backup-cleanup-panic-retry");
        fs::create_dir_all(&root).expect("create committed backup panic retry root");
        let plan = make_environment_file_lock_test_plan(&root);
        fs::create_dir_all(&plan.env_dir).expect("create old managed environment");
        fs::write(plan.env_dir.join("old-environment"), b"old")
            .expect("write old managed environment evidence");
        let build_dir = root.join("replacement-build");
        fs::create_dir(&build_dir).expect("create replacement build directory");
        fs::write(build_dir.join("new-environment"), b"new")
            .expect("write new managed environment evidence");
        // CapturedBackup proves the panicking call operated on the exact retained backup path.
        // CapturedBackup 证明 panic 调用操作的是精确保留备份路径。
        let captured_backup = Arc::new(Mutex::new(None::<PathBuf>));
        let captured_backup_for_remover = Arc::clone(&captured_backup);

        let result = finish_build_dir_with_backup_remover(
            &plan,
            build_dir,
            move |backup_path, _identity| {
                *captured_backup_for_remover
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(backup_path.to_path_buf());
                panic!("forced obsolete-backup cleanup panic");
            },
        );

        assert!(
            result.is_ok(),
            "committed publication must survive immediate cleanup panic"
        );
        assert!(plan.env_dir.join("new-environment").is_file());
        assert!(!plan.env_dir.join("old-environment").exists());
        let backup_path = captured_backup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("capture committed backup panic retry path");
        // Deadline bounds the durable retry after the panic is isolated.
        // Deadline 限制 panic 被隔离后的持久重试等待时间。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while backup_path.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !backup_path.exists(),
            "persistent recovery worker must delete the backup retained across panic"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify a retained failed-publication record restores its exact old directory object.
    /// 验证已保留的失败发布记录会恢复其精确旧目录对象。
    #[test]
    fn managed_runtime_failed_publication_backup_is_restored() {
        // Plan target starts absent while Backup owns the old environment object and evidence.
        // Plan 目标初始不存在，Backup 拥有旧环境对象及证据。
        let root = make_test_root("failed-publication-backup-restore");
        fs::create_dir_all(&root).expect("create failed publication recovery root");
        let plan = make_environment_file_lock_test_plan(&root);
        let env_parent = plan.env_dir.parent().expect("managed env parent");
        fs::create_dir_all(env_parent).expect("create managed env parent");
        let backup_path = env_parent.join(".replaced-restore-test");
        fs::create_dir(&backup_path).expect("create failed publication backup");
        fs::write(backup_path.join("old-environment"), b"old")
            .expect("write failed publication backup evidence");
        let identity = capture_managed_directory_identity(
            &fs::canonicalize(&backup_path).expect("canonicalize failed publication backup"),
            "test failed publication backup",
        )
        .expect("capture failed publication backup identity");
        let canonical_backup = fs::canonicalize(&backup_path)
            .expect("retain canonical failed publication backup path");
        let (recovery_center, recovery_permit) = reserve_managed_runtime_backup_recovery_slot()
            .expect("reserve failed publication recovery slot");
        recovery_center.enqueue(ManagedRuntimeBackupRecoveryRecord {
            backup_path: canonical_backup,
            identity,
            action: ManagedRuntimeBackupRecoveryAction::RestoreFailedPublication {
                plan: Box::new(plan.clone()),
            },
            _permit: recovery_permit,
            error_reported: false,
        });

        // Deadline bounds the worker restore and verifies the old object returns under env_dir.
        // Deadline 限制 Worker 恢复时间，并验证旧对象返回 env_dir 名称下。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !plan.env_dir.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(plan.env_dir.join("old-environment").is_file());
        assert!(!backup_path.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Verify build preparation skips a stale file collision and atomically creates a new candidate.
    /// 验证构建准备会跳过陈旧文件碰撞，并原子创建一个新候选目录。
    #[test]
    fn prepare_build_dir_skips_stale_file_collision() {
        // Temporary root that isolates the file-backed build directory fixture.
        // 隔离文件型构建目录夹具的临时根目录。
        let root = make_test_root("file-build-dir");
        // Reproducibility input used to build the expected marker for this synthetic plan.
        // 用于为这个合成计划构造期望标记的可复现输入。
        let hash_input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Python,
            runtime_version: "3.14.4".to_string(),
            platform: "test-platform".to_string(),
            package_manager: "uv".to_string(),
            package_manager_version: "0.11.28".to_string(),
            lock_hash: "lock".to_string(),
            package_manifest_hash: None,
        };
        // Fixed environment hash that lets the test occupy the exact derived build directory.
        // 固定环境哈希，便于测试占用精确派生出的构建目录。
        let env_hash = "file-build-hash".to_string();
        // Expected marker matching the synthetic environment plan.
        // 与合成环境计划匹配的期望标记。
        let expected_marker = ManagedRuntimeEnvMarker::expected(&hash_input, env_hash.clone());
        // Environment plan whose candidate directory is deterministic under the injected sequence.
        // 在注入序号下具有确定候选目录的环境计划。
        let plan = ManagedRuntimeEnvPlan {
            runtime: ManagedRuntimeKind::Python,
            platform: hash_input.platform.clone(),
            runtime_root: root.clone(),
            runtime_version: hash_input.runtime_version.clone(),
            runtime_executable: PathBuf::from("python"),
            package_manager: hash_input.package_manager.clone(),
            package_manager_version: hash_input.package_manager_version.clone(),
            package_manager_executable: PathBuf::from("uv"),
            package_manifest_path: None,
            lockfile_path: root.join("requirements.lock"),
            lock_hash: hash_input.lock_hash.clone(),
            package_manifest_hash: None,
            env_hash: env_hash.clone(),
            env_dir: root.join("env"),
            expected_marker,
        };
        // First deterministic candidate occupied by a stale regular file from an interrupted build.
        // 被中断构建留下的陈旧普通文件占据的第一个确定候选路径。
        let stale_build_path =
            root.join(format!(".building-{}-{}-17", env_hash, std::process::id()));
        fs::write(&stale_build_path, b"stale build marker")
            .expect("stale build file should be written");
        // Deterministic candidate suffixes proving the allocator advances after AlreadyExists.
        // 用于证明遇到 AlreadyExists 后分配器会继续前进的确定候选后缀。
        let mut sequences = [17_u64, 18_u64].into_iter();

        // Fresh directory returned after the stale file candidate is skipped without deletion.
        // 跳过陈旧文件候选且不删除它之后返回的新目录。
        let build_dir = prepare_build_dir_with_sequence(&plan, || {
            sequences
                .next()
                .ok_or_else(|| "test build sequence exhausted".to_string())
        })
        .expect("stale build file collision should be skipped");
        let expected_build_dir =
            root.join(format!(".building-{}-{}-18", env_hash, std::process::id()));

        assert_eq!(build_dir, expected_build_dir);
        assert!(build_dir.is_dir());
        assert_eq!(
            fs::read(&stale_build_path).expect("read preserved stale build file"),
            b"stale build marker"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify copy fallback rejects symlink entries instead of following them.
    /// 验证复制降级会拒绝符号链接目录项，而不是跟随它们。
    #[test]
    fn copy_dir_recursive_rejects_symlink_entry() {
        // Temporary root that isolates the symlink copy fixture.
        // 隔离符号链接复制夹具的临时根目录。
        let root = make_test_root("copy-symlink-entry");
        // Source build directory consumed by the recursive copy fallback.
        // 递归复制降级使用的源构建目录。
        let source_dir = root.join("source");
        // Destination env directory consumed by the recursive copy fallback.
        // 递归复制降级使用的目标环境目录。
        let destination_dir = root.join("destination");
        fs::create_dir_all(&source_dir).expect("copy source dir should be created");
        // Real file target used only to create the symlink fixture.
        // 仅用于创建符号链接夹具的真实文件目标。
        let real_file_path = root.join("real-handler.py");
        fs::write(&real_file_path, b"print('ok')").expect("real file should be written");
        // Symlink entry inside the source build directory.
        // 源构建目录内的符号链接目录项。
        let symlink_path = source_dir.join("handler-link.py");
        if !create_test_file_symlink(&symlink_path, &real_file_path) {
            let _ = fs::remove_dir_all(root);
            return;
        }

        // Error returned before the symlink source entry can be silently followed.
        // 在符号链接源目录项被静默跟随之前返回的错误。
        let error = copy_dir_recursive(&source_dir, &destination_dir)
            .expect_err("symlink entry should fail managed runtime copy");

        assert!(
            error.contains("unsupported file type"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&symlink_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify copy fallback rejects unsupported Unix filesystem entry types explicitly.
    /// 验证复制降级会显式拒绝不支持的 Unix 文件系统目录项类型。
    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_rejects_unsupported_unix_file_type() {
        use std::os::unix::fs::FileTypeExt;

        // Temporary root that isolates the unsupported-file-type copy fixture.
        // 隔离不支持文件类型复制夹具的临时根目录。
        let root = make_test_root("copy-unsupported-type");
        // Source build directory consumed by the recursive copy fallback.
        // 递归复制降级使用的源构建目录。
        let source_dir = root.join("source");
        // Destination env directory consumed by the recursive copy fallback.
        // 递归复制降级使用的目标环境目录。
        let destination_dir = root.join("destination");
        fs::create_dir_all(&source_dir).expect("copy source dir should be created");
        // FIFO path that is neither a regular file nor a directory.
        // 既不是普通文件也不是目录的 FIFO 路径。
        let fifo_path = source_dir.join("events.pipe");
        // Command status returned by mkfifo for the FIFO fixture creation.
        // mkfifo 为创建 FIFO 夹具返回的命令状态。
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo should run");
        assert!(status.success(), "mkfifo should create FIFO fixture");
        assert!(
            fs::metadata(&fifo_path)
                .expect("FIFO metadata should be readable")
                .file_type()
                .is_fifo(),
            "fixture should be FIFO"
        );

        // Error returned before the unsupported entry can reach fs::copy.
        // 在不支持的目录项进入 fs::copy 之前返回的错误。
        let error = copy_dir_recursive(&source_dir, &destination_dir)
            .expect_err("unsupported FIFO entry should fail managed runtime copy");

        assert!(
            error.contains("unsupported file type"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&fifo_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that Python environment plans resolve runtime manifests, lockfiles, and env markers.
    /// 验证 Python 环境计划会解析运行时清单、锁文件与环境标记。
    #[test]
    fn python_env_plan_resolves_manifests_and_lockfile() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("python-plan");
        let runtime_root = root.join("runtime");
        let package_root = root.join("skill");
        fs::create_dir_all(package_root.join("python")).unwrap();
        fs::write(
            package_root.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.14.4-{}", platform)),
            "python",
            "3.14.4",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );

        let package = make_test_package_context(&package_root, &runtime_root);
        let plan = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect("python env plan should resolve");

        assert_eq!(plan.runtime, ManagedRuntimeKind::Python);
        assert_eq!(plan.package_manager, "uv");
        assert_eq!(plan.lock_hash, sha256_hex(b"requests==2.32.3"));
        assert_eq!(plan.expected_marker.env_hash, plan.env_hash);
        assert!(
            plan.env_dir
                .starts_with(package.runtime_root().join("dependencies/envs/python"))
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify directory executables are reported as type errors instead of missing executables.
    /// 验证目录型 executable 会被报告为类型错误，而不是缺失可执行文件。
    #[test]
    fn python_env_plan_rejects_directory_runtime_executable() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates the directory executable fixture.
        // 隔离目录型 executable 夹具的临时根目录。
        let root = make_test_root("directory-runtime-executable");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Package directory containing the Python lockfile fixture.
        // 包含 Python lockfile 夹具的包目录。
        let package_root = root.join("skill");
        fs::create_dir_all(package_root.join("python")).unwrap();
        fs::write(
            package_root.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        // Python runtime install directory whose manifest executable path is occupied by a directory.
        // 其清单 executable 路径被目录占据的 Python 运行时安装目录。
        let python_install_dir = runtime_root
            .join("dependencies")
            .join("runtimes")
            .join("python")
            .join(format!("cpython-3.14.4-{}", platform));
        // Manifest-declared executable path that must be a file.
        // 清单声明且必须为文件的 executable 路径。
        let executable_path = python_install_dir.join(platform_executable("python"));
        fs::create_dir_all(&executable_path).expect("directory executable should be created");
        // Runtime manifest declaring the directory-backed executable path.
        // 声明目录型 executable 路径的运行时清单。
        let payload = serde_json::json!({
            "schema_version": 1,
            "runtime": "python",
            "version": "3.14.4",
            "platform": platform,
            "executable": platform_executable("python"),
        });
        fs::write(
            python_install_dir.join("runtime-manifest.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );

        // Error returned by the full Python environment plan resolver.
        // 完整 Python 环境计划解析器返回的错误。
        let package = make_test_package_context(&package_root, &runtime_root);
        let error = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect_err("directory runtime executable should be rejected");
        // Error text expected from the shared host-visible path formatter.
        // 由共享宿主可见路径渲染器生成的期望错误文本。
        // Canonical directory path matching the production resolver diagnostic.
        // 与生产解析器诊断一致的规范目录路径。
        let canonical_executable_path = fs::canonicalize(&executable_path)
            .expect("canonicalize directory runtime executable fixture");
        let expected_error = format!(
            "managed runtime executable is not a file: {}",
            render_host_visible_path(&canonical_executable_path)
        );

        assert_eq!(error, expected_error);
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that Node environment plans include package.json in the environment identity.
    /// 验证 Node 环境计划会把 package.json 纳入环境身份。
    #[test]
    fn node_env_plan_includes_package_manifest_hash() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("node-plan");
        let runtime_root = root.join("runtime");
        let package_root = root.join("skill");
        fs::create_dir_all(package_root.join("node")).unwrap();
        fs::write(
            package_root.join("node/package.json"),
            br#"{"dependencies":{}}"#,
        )
        .unwrap();
        fs::write(
            package_root.join("node/pnpm-lock.yaml"),
            b"lockfileVersion: '9.0'",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/node")
                .join(format!("node-24.18.0-{}", platform)),
            "node",
            "24.18.0",
            &platform,
            platform_executable("node"),
        );
        write_install_manifest(
            &runtime_root.join("dependencies/runtimes/node/pnpm-11.11.0"),
            "pnpm",
            "11.11.0",
            "any",
            "bin/pnpm.cjs",
        );

        let package = make_test_package_context(&package_root, &runtime_root);
        let plan = resolve_node_env_plan(
            package.as_ref(),
            &NodeRuntimeDependencySpec {
                version: "24.18.0".to_string(),
                package_manager: NodeRuntimePackageManager::Pnpm,
                package_manager_version: "11.11.0".to_string(),
                package_json: "node/package.json".to_string(),
                lockfile: "node/pnpm-lock.yaml".to_string(),
                required: true,
            },
        )
        .expect("node env plan should resolve");

        assert_eq!(plan.runtime, ManagedRuntimeKind::Node);
        assert_eq!(plan.package_manager, "pnpm");
        assert_eq!(plan.lock_hash, sha256_hex(b"lockfileVersion: '9.0'"));
        assert_eq!(
            plan.package_manifest_hash,
            Some(sha256_hex(br#"{"dependencies":{}}"#))
        );
        assert!(
            plan.env_dir
                .starts_with(package.runtime_root().join("dependencies/envs/node"))
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify Node environment planning rejects EOL runtimes before any installation lookup.
    /// 验证 Node 环境规划会在任何安装查找前拒绝已终止维护的运行时。
    #[test]
    fn node_env_plan_rejects_runtime_older_than_supported_esm_baseline() {
        // Root owns a real trusted package context while intentionally containing no runtime assets.
        // Root 拥有真实可信包上下文，同时有意不包含任何运行时资产。
        let root = make_test_root("unsupported-node-runtime-version");
        let runtime_root = root.join("runtime");
        let package_root = root.join("skill");
        let package = make_test_package_context(&package_root, &runtime_root);
        let error = resolve_node_env_plan(
            package.as_ref(),
            &NodeRuntimeDependencySpec {
                version: "18.19.1".to_string(),
                package_manager: NodeRuntimePackageManager::Pnpm,
                package_manager_version: "11.11.0".to_string(),
                package_json: "node/package.json".to_string(),
                lockfile: "node/pnpm-lock.yaml".to_string(),
                required: true,
            },
        )
        .expect_err("pre-22 Node runtime must be rejected before installation lookup");

        assert_eq!(
            error,
            "managed Node runtime 18.19.1 is unsupported; minimum supported version is 22.0.0"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify malformed Node versions fail as declarations rather than filesystem errors.
    /// 验证格式错误的 Node 版本会作为声明错误失败，而不是文件系统错误。
    #[test]
    fn node_env_plan_rejects_non_semantic_runtime_version() {
        let root = make_test_root("invalid-node-runtime-version");
        let runtime_root = root.join("runtime");
        let package_root = root.join("skill");
        let package = make_test_package_context(&package_root, &runtime_root);
        let error = resolve_node_env_plan(
            package.as_ref(),
            &NodeRuntimeDependencySpec {
                version: "current".to_string(),
                package_manager: NodeRuntimePackageManager::Pnpm,
                package_manager_version: "11.11.0".to_string(),
                package_json: "node/package.json".to_string(),
                lockfile: "node/pnpm-lock.yaml".to_string(),
                required: true,
            },
        )
        .expect_err("non-semantic Node runtime version must be rejected");

        assert!(error.starts_with("node_runtime.version is not valid semantic version 'current':"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that managed runtime declarations cannot escape the package root.
    /// 验证受管运行时声明不能逃逸包根目录。
    #[test]
    fn managed_runtime_plan_rejects_package_path_traversal() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("path-traversal");
        let runtime_root = root.join("runtime");
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.14.4-{}", platform)),
            "python",
            "3.14.4",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );
        let package_root = root.join("skill");
        let package = make_test_package_context(&package_root, &runtime_root);
        let error = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "../outside.lock".to_string(),
                required: true,
            },
        )
        .expect_err("path traversal should be rejected");

        assert!(error.contains("safe path under the package root"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that missing lockfile errors render paths through the host-visible path formatter.
    /// 验证缺失锁文件错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn python_env_plan_missing_lockfile_error_uses_host_visible_path() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates all filesystem fixtures for this test.
        // 隔离当前测试所有文件系统夹具的临时根目录。
        let root = make_test_root("missing-lockfile-path");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Package directory used as the base for the missing relative lockfile.
        // 作为缺失相对锁文件基准目录的包目录。
        let package_root = root.join("skill");
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.14.4-{}", platform)),
            "python",
            "3.14.4",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );

        // Error returned by the full Python environment plan resolver.
        // 完整 Python 环境计划解析器返回的错误。
        let package = make_test_package_context(&package_root, &runtime_root);
        // Native-separator candidate derived from the canonical trusted package root.
        // 从规范可信包根派生的原生分隔符候选路径。
        let expected_lockfile = package.package_root().join("python").join("missing.lock");
        let error = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "python/missing.lock".to_string(),
                required: true,
            },
        )
        .expect_err("missing lockfile should be reported");
        assert!(
            error.contains("failed to canonicalize python_runtime.lockfile"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains(&render_host_visible_path(&expected_lockfile)),
            "unexpected error: {error}"
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(root);
    }

    /// Verify directory lockfiles are reported as type errors instead of missing files.
    /// 验证目录型 lockfile 会被报告为类型错误，而不是缺失文件。
    #[test]
    fn python_env_plan_rejects_directory_lockfile() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates the directory lockfile fixture.
        // 隔离目录型 lockfile 夹具的临时根目录。
        let root = make_test_root("directory-lockfile");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Package directory used as the base for the directory lockfile.
        // 作为目录型 lockfile 基准目录的包目录。
        let package_root = root.join("skill");
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.14.4-{}", platform)),
            "python",
            "3.14.4",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.28-{}", platform)),
            "uv",
            "0.11.28",
            &platform,
            platform_executable("uv"),
        );
        // Directory occupying the lockfile path declared by the Python runtime dependency.
        // 占据 Python 运行时依赖声明 lockfile 路径的目录。
        let lockfile_path = package_root.join("python/requirements.lock");
        fs::create_dir_all(&lockfile_path).expect("directory lockfile should be created");

        // Error returned by the full Python environment plan resolver.
        // 完整 Python 环境计划解析器返回的错误。
        let package = make_test_package_context(&package_root, &runtime_root);
        let error = resolve_python_env_plan(
            package.as_ref(),
            &PythonRuntimeDependencySpec {
                version: "3.14.4".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.28".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect_err("directory lockfile should be rejected");
        // Error text expected from the shared host-visible path formatter.
        // 由共享宿主可见路径渲染器生成的期望错误文本。
        // Canonical directory path matching the production resolver diagnostic.
        // 与生产解析器诊断一致的规范目录路径。
        let canonical_lockfile_path =
            fs::canonicalize(&lockfile_path).expect("canonicalize directory lockfile fixture");
        let expected_error = format!(
            "python_runtime.lockfile is not a file: {}",
            render_host_visible_path(&canonical_lockfile_path)
        );

        assert_eq!(error, expected_error);
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(root);
    }

    /// Create a unique temporary test root.
    /// 创建一个唯一的临时测试根目录。
    fn make_test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "luaskills-managed-runtime-{}-{}-{}",
            label,
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Return a simple unique suffix for temporary test paths.
    /// 返回用于临时测试路径的简单唯一后缀。
    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    /// Write one install manifest and create its declared executable.
    /// 写入一个安装清单并创建其声明的可执行文件。
    fn write_install_manifest(
        install_dir: &Path,
        runtime: &str,
        version: &str,
        platform: &str,
        executable: &str,
    ) {
        fs::create_dir_all(
            install_dir.join(Path::new(executable).parent().unwrap_or(Path::new(""))),
        )
        .unwrap();
        fs::write(install_dir.join(executable), b"executable").unwrap();
        let payload = serde_json::json!({
            "schema_version": 1,
            "runtime": runtime,
            "version": version,
            "platform": platform,
            "executable": executable,
        });
        fs::write(
            install_dir.join("runtime-manifest.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
    }

    /// Return a platform-shaped executable path for install manifest tests.
    /// 返回用于安装清单测试的平台形态可执行路径。
    fn platform_executable(kind: &str) -> &'static str {
        match (std::env::consts::OS, kind) {
            ("windows", "python") => "python.exe",
            ("windows", "uv") => "uv.exe",
            ("windows", "node") => "node.exe",
            (_, "python") => "bin/python3",
            (_, "uv") => "uv",
            (_, "node") => "bin/node",
            _ => "tool",
        }
    }
}
