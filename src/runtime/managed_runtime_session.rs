use serde_json::to_string;
use std::collections::VecDeque;
#[cfg(windows)]
use std::ffi::OsString;
#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
#[cfg(windows)]
use windows_sys::Win32::Foundation::GENERIC_READ;
#[cfg(windows)]
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GetFileAttributesW, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    INVALID_FILE_ATTRIBUTES, OPEN_EXISTING, SetFileAttributesW, VOLUME_NAME_DOS,
};

use crate::runtime::encoding::RuntimeTextEncoding;
use crate::runtime::managed_package::{
    ManagedFilesystemObjectIdentity, ManagedRuntimePackageContext,
};
use crate::runtime::managed_runtime::{
    ManagedRuntimeEnvLease, ManagedRuntimeEnvPlan, acquire_ready_managed_env_lease,
    configure_managed_command_base_environment,
    current_managed_runtime_persistent_session_capability, ensure_managed_env,
    resolve_node_env_plan, resolve_python_env_plan,
};
use crate::runtime::path::{host_process_path_argument, render_host_visible_path};
use crate::runtime::process_session::{
    ManagedProcessSessionCleanup, ManagedProcessSessionCore, ManagedProcessSessionLaunchOptions,
    ManagedProcessSessionObserver,
};
use crate::runtime_logging::warn as log_warn;

/// Process-local sequence used to make every managed package snapshot directory unique.
/// 用于生成每个受管包快照唯一目录的进程内序号。
static NEXT_MANAGED_PACKAGE_SNAPSHOT: AtomicU64 = AtomicU64::new(0);
/// Maximum atomic directory-creation attempts before treating snapshot namespace contention as fatal.
/// 将快照命名空间竞争视为致命错误前的最大原子目录创建尝试次数。
const MAX_MANAGED_PACKAGE_SNAPSHOT_CREATE_ATTEMPTS: usize = 64;
/// Maximum number of snapshot cleanup records retained by the process-wide retry worker.
/// 进程级重试工作线程最多保留的快照清理记录数量。
const MANAGED_SNAPSHOT_CLEANUP_CAPACITY: usize = 256;
/// Delay between retry passes for retained snapshot cleanup records.
/// 已保留快照清理记录的重试轮次间隔。
const MANAGED_SNAPSHOT_CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Lazily initialized bounded owner for snapshot cleanup failures.
/// 延迟初始化的快照清理失败有界所有者。
static MANAGED_SNAPSHOT_CLEANUP_RETRY: OnceLock<Result<ManagedSnapshotCleanupRetry, String>> =
    OnceLock::new();
/// Windows standard DELETE access used to make non-delete-sharing directory handles exclusive.
/// 用于使不共享删除的目录句柄具备排他性的 Windows 标准 DELETE 访问权。
#[cfg(windows)]
const WINDOWS_DELETE_ACCESS: u32 = 0x0001_0000;

/// Strict language-runtime session request parsed from one Lua `session.open` call.
/// 从单个 Lua `session.open` 调用解析得到的严格语言运行时会话请求。
pub(crate) struct ManagedRuntimeSessionOpenRequest {
    /// Package-relative executable source file.
    /// 包相对可执行源文件。
    pub(crate) file: String,
    /// Direct child-process arguments passed without shell interpolation.
    /// 不经过 shell 插值直接传给子进程的参数。
    pub(crate) args: Vec<String>,
    /// Optional working directory authorized under the package or workspace root.
    /// 在包根或工作区根下授权的可选工作目录。
    pub(crate) cwd: Option<String>,
    /// Encoding used for stdout decoding.
    /// stdout 解码使用的编码。
    pub(crate) stdout_encoding: RuntimeTextEncoding,
    /// Encoding used for stderr decoding.
    /// stderr 解码使用的编码。
    pub(crate) stderr_encoding: RuntimeTextEncoding,
    /// Encoding used for stdin writes.
    /// stdin 写入使用的编码。
    pub(crate) stdin_encoding: RuntimeTextEncoding,
    /// Maximum retained bytes for each output stream.
    /// 每个输出流最多保留的字节数。
    pub(crate) buffer_limit_bytes: usize,
}

/// Pure Rust launch result converted into the shared Lua userdata by the engine layer.
/// 由引擎层转换为共享 Lua userdata 的纯 Rust 启动结果。
pub(crate) struct ManagedRuntimeSessionLaunch {
    /// Shared process-session core owning the child process tree.
    /// 拥有子进程树的共享进程会话核心。
    pub(crate) core: ManagedProcessSessionCore,
    /// Optional cleanup executed after process-tree teardown.
    /// 在进程树清理后执行的可选清理回调。
    pub(crate) cleanup: Option<ManagedProcessSessionCleanup>,
}

/// RAII owner for one unique managed package snapshot.
/// 单个唯一受管包快照的 RAII 所有者。
#[derive(Debug)]
pub(crate) struct ManagedPackageSnapshot {
    /// Atomic record/permit ownership retained until deletion or retry handoff succeeds.
    /// 保留到删除或重试交接成功为止的原子记录与许可所有权。
    ownership: Option<ManagedSnapshotCleanupOwnership>,
    /// Pinned snapshot objects preventing mutation or name replacement during execution.
    /// 防止执行期间发生修改或名称替换的固定快照对象。
    execution_pin: Option<ManagedSnapshotExecutionPin>,
}

/// Platform execution pin retained for the complete managed child lifetime.
/// 在完整受管子进程生命周期内保留的平台执行固定器。
#[derive(Debug)]
enum ManagedSnapshotExecutionPin {
    /// Unix root descriptor anchors the copied directory object.
    /// Unix 根描述符锚定已复制目录对象。
    #[cfg(unix)]
    Unix {
        /// Root descriptor retained solely for object lifetime anchoring.
        /// 仅用于对象生命周期锚定而保留的根描述符。
        directory: OwnedFd,
    },
    /// Windows handles deny write/delete sharing for every copied object.
    /// Windows 句柄拒绝每个已复制对象的写入/删除共享。
    #[cfg(windows)]
    Windows {
        /// Handles retained solely to deny write/delete sharing.
        /// 仅用于拒绝写入/删除共享而保留的句柄集合。
        _handles: Vec<fs::File>,
    },
}

/// Inseparable cleanup record and its pre-reserved retry capacity permit.
/// 不可分割的清理记录及其预留重试容量许可。
#[derive(Debug)]
struct ManagedSnapshotCleanupOwnership {
    /// Mutable cleanup record following atomic path transitions.
    /// 跟随原子路径转换的可变清理记录。
    record: ManagedSnapshotCleanupRecord,
    /// Capacity permit guaranteeing persistent handoff on every failure.
    /// 保证每次失败均可持久交接的容量许可。
    permit: ManagedSnapshotCleanupPermit,
}

/// Mutable cleanup identity that follows a snapshot into its atomic quarantine path.
/// 可变清理身份，会跟随快照进入其原子隔离路径。
#[derive(Debug)]
struct ManagedSnapshotCleanupRecord {
    /// Shared environment lease retained until snapshot deletion or retry completion.
    /// 在快照删除或重试完成前保留的共享环境租约。
    _environment_lease: Option<ManagedRuntimeEnvLease>,
    /// Canonical package-partitioned parent authorized for cleanup isolation.
    /// 授权执行清理隔离的规范包级父目录。
    snapshots_root: PathBuf,
    /// Current owned path, updated immediately after atomic quarantine rename.
    /// 当前拥有路径，在原子隔离重命名后立即更新。
    snapshot_root: PathBuf,
    /// Optional quarantine directory removed after its captured child is gone.
    /// 捕获子目录删除后需要移除的可选隔离目录。
    quarantine_root: Option<PathBuf>,
    /// Empty quarantine created before a failed rename and still awaiting confirmed deletion.
    /// rename 失败前已创建且仍等待确认删除的空隔离目录。
    orphan_quarantine_root: Option<PathBuf>,
    /// Whether the owned snapshot directory object still exists inside its current phase.
    /// 所拥有的快照目录对象在当前阶段是否仍然存在。
    snapshot_present: bool,
    /// Filesystem identity once captured; absent only while the new directory must remain empty.
    /// 捕获后的文件系统身份；仅在新目录必须保持为空的阶段缺失。
    identity: Option<ManagedPackageSnapshotIdentity>,
    /// Whether one retry failure diagnostic has already been emitted.
    /// 是否已输出过一次重试失败诊断。
    error_reported: bool,
}

/// Process-wide bounded owner for failed snapshot cleanup records.
/// 进程级有界所有者，用于保留失败的快照清理记录。
#[derive(Debug)]
struct ManagedSnapshotCleanupRetry {
    /// Shared queue whose ownership survives worker failures.
    /// 所有权可跨工作线程失败继续存活的共享队列。
    queue: Arc<ManagedSnapshotCleanupQueue>,
    /// Total active permits and queued records enforcing the strict bound.
    /// 施加严格上限的活动许可与已排队记录总数。
    reserved: Arc<AtomicUsize>,
}

/// Shared snapshot cleanup queue independent from its retry worker thread.
/// 独立于重试工作线程的共享快照清理队列。
#[derive(Debug)]
struct ManagedSnapshotCleanupQueue {
    /// Records retried until deletion succeeds.
    /// 重试直至删除成功的记录。
    records: Mutex<VecDeque<ManagedSnapshotCleanupRecord>>,
    /// Notification waking the worker after one failed cleanup is handed off.
    /// 单个失败清理完成交接后唤醒工作线程的通知量。
    changed: Condvar,
}

/// Capacity permit acquired before a snapshot directory is created.
/// 在创建快照目录前获取的容量许可。
#[derive(Debug)]
struct ManagedSnapshotCleanupPermit {
    /// Initialized retry owner guaranteed to retain this permit's record.
    /// 已初始化且保证保留当前许可记录的重试所有者。
    retry: &'static ManagedSnapshotCleanupRetry,
    /// Whether dropping this permit must release an unused reservation.
    /// 丢弃当前许可时是否必须释放未使用预留。
    reserved: bool,
}

/// Platform-native identity proving one snapshot path still names its created directory object.
/// 平台原生身份，用于证明快照路径仍指向其创建时的目录对象。
#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedPackageSnapshotIdentity {
    /// Unix filesystem device identifier.
    /// Unix 文件系统设备标识。
    #[cfg(unix)]
    device: u64,
    /// Unix inode identifier unique within the device.
    /// Unix 设备内唯一的 inode 标识。
    #[cfg(unix)]
    inode: u64,
    /// Windows volume serial number containing the directory.
    /// 包含该目录的 Windows 卷序列号。
    #[cfg(windows)]
    volume_serial_number: u32,
    /// Windows file identifier unique within the volume.
    /// Windows 卷内唯一的文件标识。
    #[cfg(windows)]
    file_index: u64,
}

impl ManagedPackageSnapshot {
    /// Return the exact snapshot root containing copied package files.
    /// 返回包含已复制包文件的精确快照根目录。
    pub(crate) fn root(&self) -> &Path {
        &self
            .ownership
            .as_ref()
            .expect("live managed snapshot must retain cleanup ownership")
            .record
            .snapshot_root
    }

    /// Return a source path anchored to the pinned snapshot object on each platform.
    /// 返回在各平台锚定到固定快照对象的源码路径。
    ///
    /// `relative_path` is a previously validated package-relative source path. Unix uses an
    /// inherited directory-fd namespace; Windows uses a share-locked snapshot name.
    /// `relative_path` 是此前已验证的包相对源码路径。Unix 使用继承的目录 fd 命名空间；
    /// Windows 使用共享锁定的快照名称。
    pub(crate) fn execution_source_path(&self, relative_path: &str) -> Result<PathBuf, String> {
        let pin = self
            .execution_pin
            .as_ref()
            .ok_or_else(|| "managed snapshot execution pin is unavailable".to_string())?;
        #[cfg(target_os = "linux")]
        {
            let ManagedSnapshotExecutionPin::Unix { directory } = pin;
            let descriptor_root = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
            Ok(descriptor_root.join(relative_path))
        }
        #[cfg(target_os = "macos")]
        {
            let ManagedSnapshotExecutionPin::Unix { directory } = pin;
            macos_snapshot_execution_source_path(
                directory.as_raw_fd(),
                Path::new(relative_path),
                self.root(),
            )
        }
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        {
            let _ = pin;
            Err("managed snapshot execution paths are unsupported on this Unix target".to_string())
        }
        #[cfg(windows)]
        {
            let _ = pin;
            Ok(self.root().join(relative_path))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pin;
            Err("managed snapshot execution paths are unsupported".to_string())
        }
    }

    /// Return a Node-compatible source path while retaining the exact snapshot object pin.
    /// 返回与 Node 兼容的源码路径，同时保留精确快照对象固定器。
    ///
    /// `relative_path` is the previously validated package-relative source. Windows Node treats
    /// verbatim drive paths as malformed CLI entry points, so this method removes only the native
    /// verbatim prefix without performing another filesystem lookup.
    /// `relative_path` 是此前已校验的包相对源码。Windows Node 会把 verbatim 驱动器路径视为
    /// 非法 CLI 入口，因此本方法只移除原生 verbatim 前缀，不再次查询文件系统。
    pub(crate) fn node_execution_source_path(
        &self,
        relative_path: &str,
    ) -> Result<PathBuf, String> {
        let source_path = self.execution_source_path(relative_path)?;
        #[cfg(windows)]
        {
            windows_non_verbatim_path(&source_path)
        }
        #[cfg(not(windows))]
        {
            Ok(source_path)
        }
    }

    /// Return one validated pooled-worker source path with platform-appropriate lookup semantics.
    /// 返回一个已校验且具有平台适配查找语义的池化 Worker 源码路径。
    ///
    /// `relative_path` is the already authorized package-relative entry. Unix validates it through
    /// the pinned directory descriptor and returns the relative name used after `fchdir`; Windows
    /// validates and returns the share-locked absolute snapshot path.
    /// `relative_path` 是已授权的包相对入口。Unix 会通过固定目录描述符校验，并返回在 `fchdir`
    /// 后使用的相对名称；Windows 会校验并返回受共享锁保护的绝对快照路径。
    ///
    /// Returns a regular-file source path or a stable lexical, lookup, or type error.
    /// 返回常规文件源码路径，或稳定的词法、查找及类型错误。
    pub(crate) fn worker_source_path(&self, relative_path: &str) -> Result<PathBuf, String> {
        let relative = Path::new(relative_path);
        if relative_path.is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err("managed worker source must be a non-empty safe relative path".to_string());
        }
        let pin = self
            .execution_pin
            .as_ref()
            .ok_or_else(|| "managed snapshot execution pin is unavailable".to_string())?;
        #[cfg(unix)]
        {
            let ManagedSnapshotExecutionPin::Unix { directory } = pin;
            validate_unix_snapshot_regular_file(directory.as_raw_fd(), relative, self.root())?;
            Ok(relative.to_path_buf())
        }
        #[cfg(windows)]
        {
            let _ = pin;
            let source = self.root().join(relative);
            ensure_regular_file(&source, "managed worker snapshot source")?;
            Ok(source)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pin;
            Err("managed worker snapshot source paths are unsupported".to_string())
        }
    }

    /// Return one validated Node pooled-worker source path accepted by native ESM loading.
    /// 返回一个可被原生 ESM 加载接受的已校验 Node 池化 Worker 源码路径。
    ///
    /// `relative_path` follows `worker_source_path`; Windows additionally removes only supported
    /// DOS/UNC verbatim prefixes without reopening the fixed snapshot object.
    /// `relative_path` 遵循 `worker_source_path`；Windows 还会在不重新打开固定快照对象的前提下，
    /// 仅移除受支持的 DOS/UNC verbatim 前缀。
    ///
    /// Returns a relative Unix path or absolute Windows Node-compatible path.
    /// 返回 Unix 相对路径或 Windows 上与 Node 兼容的绝对路径。
    pub(crate) fn node_worker_source_path(&self, relative_path: &str) -> Result<PathBuf, String> {
        let source_path = self.worker_source_path(relative_path)?;
        #[cfg(windows)]
        {
            windows_non_verbatim_path(&source_path)
        }
        #[cfg(not(windows))]
        {
            Ok(source_path)
        }
    }

    /// Return the import root installed into one pooled Python worker's isolated `sys.path`.
    /// 返回安装到单个池化 Python Worker 隔离 `sys.path` 中的导入根。
    ///
    /// Unix workers enter the pinned snapshot before `exec`, while Windows workers use the exact
    /// absolute share-locked root independently from their neutral process cwd.
    /// Unix Worker 会在 `exec` 前进入固定快照；Windows Worker 则独立于中立进程 cwd 使用受
    /// 共享锁保护的精确绝对根。
    ///
    /// Returns the platform-specific root without another filesystem lookup.
    /// 返回平台专属根，且不再次查询文件系统。
    pub(crate) fn python_worker_import_root(&self) -> Result<PathBuf, String> {
        #[cfg(unix)]
        {
            Ok(PathBuf::from("."))
        }
        #[cfg(windows)]
        {
            Ok(self.root().to_path_buf())
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("managed Python worker import roots are unsupported".to_string())
        }
    }

    /// Configure only the intended child to inherit the pinned snapshot root descriptor.
    /// 配置仅由目标 child 继承固定快照根描述符。
    pub(crate) fn configure_source_inheritance(&self, command: &mut Command) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            let Some(ManagedSnapshotExecutionPin::Unix { directory }) = self.execution_pin.as_ref()
            else {
                return Err("managed snapshot Unix execution pin is unavailable".to_string());
            };
            let directory_fd = directory.as_raw_fd();
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(directory_fd, libc::F_GETFD);
                    if flags < 0
                        || libc::fcntl(directory_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                    {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        #[cfg(windows)]
        {
            let _ = command;
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
            return Err("managed snapshot source inheritance is unsupported".to_string());
        }
        Ok(())
    }

    /// Revalidate a macOS object-derived source name immediately before interpreter execution.
    /// 在解释器执行前立即重新校验 macOS 对象派生的源码名称。
    ///
    /// `command` is the prepared managed interpreter command, `relative_path` is the authorized
    /// package-relative entry, and `source_path` is derived from `F_GETPATH` on the pinned root.
    /// `command` 是已准备的受管解释器命令，`relative_path` 是已授权的包相对入口，
    /// `source_path` 是从固定根目录的 `F_GETPATH` 派生出的路径。
    ///
    /// Returns unit after installing a child-side device/inode check, or a stable validation error.
    /// 安装子进程侧设备号/inode 校验后返回空值，否则返回稳定校验错误。
    pub(crate) fn configure_execution_path_revalidation(
        &self,
        command: &mut Command,
        relative_path: &str,
        source_path: &Path,
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::process::CommandExt;

            let Some(ManagedSnapshotExecutionPin::Unix { directory }) = self.execution_pin.as_ref()
            else {
                return Err("managed snapshot macOS execution pin is unavailable".to_string());
            };
            let directory_fd = directory.as_raw_fd();
            let source = path_to_c_string(source_path, "managed snapshot execution source")?;
            let expected_root =
                macos_file_identity_from_fd(directory_fd).map_err(|error| error.to_string())?;
            let expected_source = macos_file_identity_from_snapshot(
                directory_fd,
                Path::new(relative_path),
                self.root(),
            )?;
            macos_validate_named_execution_source(&source, expected_source)
                .map_err(|error| error.to_string())?;
            unsafe {
                command.pre_exec(move || {
                    let actual_root = macos_file_identity_from_fd(directory_fd)?;
                    if actual_root != expected_root {
                        return Err(io::Error::other(
                            "managed snapshot root identity changed before exec",
                        ));
                    }
                    macos_validate_named_execution_source(&source, expected_source)
                });
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = command;
            let _ = relative_path;
            let _ = source_path;
        }
        Ok(())
    }

    /// Configure a pooled worker to enter this exact immutable snapshot root before execution.
    /// 配置池化 Worker，使其在执行前进入当前精确不可变快照根。
    ///
    /// The worker imports only validated relative paths from its pinned working directory, so the
    /// directory descriptor is needed by `fchdir` before `exec` but must retain `FD_CLOEXEC`.
    /// Worker 仅从固定工作目录导入已校验的相对路径，因此目录描述符只在 `exec` 前供
    /// `fchdir` 使用，并且必须保留 `FD_CLOEXEC`。
    pub(crate) fn configure_worker_command(&self, command: &mut Command) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            let Some(ManagedSnapshotExecutionPin::Unix { directory }) = self.execution_pin.as_ref()
            else {
                return Err("managed snapshot Unix execution pin is unavailable".to_string());
            };
            let directory_fd = directory.as_raw_fd();
            unsafe {
                command.pre_exec(move || {
                    if libc::fchdir(directory_fd) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
        }
        #[cfg(windows)]
        {
            // NeutralCwd is the short drive/share root of the fixed snapshot and never inherits the
            // host process cwd or passes a long snapshot path to CreateProcessW.
            // NeutralCwd 是固定快照所在的短驱动器/共享根，既不继承宿主进程 cwd，也不会把长快照
            // 路径传给 CreateProcessW。
            let neutral_cwd = windows_worker_neutral_cwd(self.root())?;
            command.current_dir(neutral_cwd);
        }
        Ok(())
    }
}

/// Validate one relative source as a regular file beneath an already pinned Unix snapshot root.
/// 在已固定 Unix 快照根下校验一个相对源码是否为常规文件。
///
/// `root_fd` owns the authoritative directory object, `relative_path` contains only normal
/// components, and `display_root` is used solely for host-visible diagnostics.
/// `root_fd` 拥有权威目录对象，`relative_path` 仅包含普通组件，`display_root` 只用于宿主可见诊断。
///
/// Returns unit after every component is opened with `O_NOFOLLOW`, or a lookup/type error.
/// 每个组件均通过 `O_NOFOLLOW` 打开后返回空值，否则返回查找或类型错误。
#[cfg(unix)]
fn validate_unix_snapshot_regular_file(
    root_fd: RawFd,
    relative_path: &Path,
    display_root: &Path,
) -> Result<(), String> {
    open_unix_snapshot_regular_file(root_fd, relative_path, display_root).map(drop)
}

/// Open and retain one regular file beneath a pinned Unix snapshot root without following links.
/// 在固定 Unix 快照根下打开并保留一个常规文件，且不跟随任何链接。
///
/// `root_fd` is authoritative, `relative_path` contains only normal components, and
/// `display_root` is used only for diagnostics.
/// `root_fd` 是权威对象，`relative_path` 仅包含普通组件，`display_root` 只用于诊断。
///
/// Returns the final opened descriptor after every parent was traversed with `O_NOFOLLOW`.
/// 在每个父级均通过 `O_NOFOLLOW` 遍历后返回最终打开的描述符。
#[cfg(unix)]
fn open_unix_snapshot_regular_file(
    root_fd: RawFd,
    relative_path: &Path,
    display_root: &Path,
) -> Result<OwnedFd, String> {
    let components = relative_path.components().collect::<Vec<_>>();
    let mut opened_directories = Vec::<OwnedFd>::new();
    let mut parent_fd = root_fd;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err("managed worker source contains a non-normal path component".to_string());
        };
        let encoded_name = CString::new(name.as_bytes()).map_err(|_| {
            format!(
                "managed worker source contains an interior NUL: {}",
                render_host_visible_path(&display_root.join(relative_path))
            )
        })?;
        let is_final = index + 1 == components.len();
        let flags = if is_final {
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW
        } else {
            libc::O_RDONLY
                | libc::O_NONBLOCK
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
        };
        let opened_fd = unsafe { libc::openat(parent_fd, encoded_name.as_ptr(), flags) };
        if opened_fd < 0 {
            return Err(format!(
                "failed to open managed worker snapshot source {}: {}",
                render_host_visible_path(&display_root.join(relative_path)),
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(opened_fd) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(opened.as_raw_fd(), &mut stat) } != 0 {
            return Err(format!(
                "failed to inspect managed worker snapshot source {}: {}",
                render_host_visible_path(&display_root.join(relative_path)),
                std::io::Error::last_os_error()
            ));
        }
        let object_type = stat.st_mode & libc::S_IFMT;
        if is_final {
            if object_type != libc::S_IFREG {
                return Err(format!(
                    "managed worker snapshot source is not a regular file: {}",
                    render_host_visible_path(&display_root.join(relative_path))
                ));
            }
            return Ok(opened);
        } else {
            if object_type != libc::S_IFDIR {
                return Err(format!(
                    "managed worker snapshot source parent is not a directory: {}",
                    render_host_visible_path(&display_root.join(relative_path))
                ));
            }
            parent_fd = opened.as_raw_fd();
            opened_directories.push(opened);
        }
    }
    Err("managed worker source must contain at least one path component".to_string())
}

/// Native macOS device/inode identity used to bind an object-derived path to an open descriptor.
/// 用于把对象派生路径绑定到已打开描述符的 macOS 原生设备号/inode 身份。
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MacosFileIdentity {
    /// Filesystem device containing the object.
    /// 包含该对象的文件系统设备号。
    device: u64,
    /// Inode identifying the object within its device.
    /// 在设备内标识该对象的 inode。
    inode: u64,
}

/// Convert a path into a syscall-safe C string without lossy encoding.
/// 在不进行有损编码的情况下把路径转换为系统调用安全的 C 字符串。
///
/// `path` is the exact native path and `label` identifies it in diagnostics.
/// `path` 是精确原生路径，`label` 用于诊断标识。
///
/// Returns the encoded path or an error when an interior NUL is present.
/// 返回编码后的路径；若包含内部 NUL 则返回错误。
#[cfg(target_os = "macos")]
fn path_to_c_string(path: &Path, label: &str) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        format!(
            "{label} contains an interior NUL: {}",
            render_host_visible_path(path)
        )
    })
}

/// Read a stable macOS filesystem identity from an already open descriptor.
/// 从已打开描述符读取稳定的 macOS 文件系统身份。
///
/// `fd` must reference the authoritative snapshot object.
/// `fd` 必须引用权威快照对象。
///
/// Returns its device/inode pair or the underlying `fstat` error.
/// 返回其设备号/inode 对，或底层 `fstat` 错误。
#[cfg(target_os = "macos")]
fn macos_file_identity_from_fd(fd: RawFd) -> Result<MacosFileIdentity, io::Error> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (device, inode) = unix_stat_device_inode(&stat).map_err(io::Error::other)?;
    Ok(MacosFileIdentity { device, inode })
}

/// Convert native Unix stat identity fields without silent sign or width changes.
/// 在不发生静默符号或位宽变化的前提下转换 Unix 原生 stat 身份字段。
///
/// `stat` is the successful result of `fstat`. The returned tuple contains the device and inode
/// identifiers as stable unsigned values, or an error when the native representation is invalid.
/// `stat` 是成功的 `fstat` 结果。返回元组包含稳定的无符号设备号与 inode；原生表示无效时返回错误。
#[cfg(unix)]
fn unix_stat_device_inode(stat: &libc::stat) -> Result<(u64, u64), String> {
    let device = unix_identity_component_to_u64(stat.st_dev, "device")?;
    let inode = unix_identity_component_to_u64(stat.st_ino, "inode")?;
    Ok((device, inode))
}

/// Convert one platform-native Unix identity component into the stable representation.
/// 将一个平台原生 Unix 身份组件转换为稳定表示。
///
/// `value` is a device or inode identifier and `component` labels conversion failures. The generic
/// boundary keeps same-width platforms lossless while retaining checked conversion on other ABIs.
/// `value` 是设备号或 inode，`component` 标记转换失败。泛型边界让同位宽平台保持无损，
/// 同时在其他 ABI 上保留检查式转换。
#[cfg(unix)]
fn unix_identity_component_to_u64<T>(value: T, component: &str) -> Result<u64, String>
where
    u64: TryFrom<T>,
{
    u64::try_from(value)
        .map_err(|_| format!("Unix filesystem {component} identifier cannot be represented as u64"))
}

/// Open one snapshot-relative macOS source component-by-component and return its identity.
/// 逐组件打开一个 macOS 快照相对源码并返回其身份。
///
/// `root_fd` is the pinned snapshot root, `relative_path` is the validated entry name, and
/// `display_root` is used only for diagnostics.
/// `root_fd` 是固定快照根，`relative_path` 是已校验入口名称，`display_root` 只用于诊断。
///
/// Returns a regular-file identity while rejecting symbolic links in every component.
/// 返回常规文件身份，并拒绝任意组件中的符号链接。
#[cfg(target_os = "macos")]
fn macos_file_identity_from_snapshot(
    root_fd: RawFd,
    relative_path: &Path,
    display_root: &Path,
) -> Result<MacosFileIdentity, String> {
    let source = open_unix_snapshot_regular_file(root_fd, relative_path, display_root)?;
    macos_file_identity_from_fd(source.as_raw_fd()).map_err(|error| error.to_string())
}

/// Validate that a named macOS source still resolves to the expected native object.
/// 校验具名 macOS 源码是否仍解析到预期原生对象。
///
/// `source_path` is object-derived and `expected` came from descriptor-relative `openat`.
/// `source_path` 由对象派生，`expected` 来自描述符相对 `openat`。
///
/// Returns unit only when the path opens the same regular file without following a final symlink.
/// 仅当该路径在不跟随最终符号链接的情况下打开同一常规文件时返回空值。
#[cfg(target_os = "macos")]
fn macos_validate_named_execution_source(
    source_path: &CStr,
    expected: MacosFileIdentity,
) -> Result<(), io::Error> {
    let raw_fd = unsafe {
        libc::open(
            source_path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let source = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let actual = macos_file_identity_from_fd(source.as_raw_fd())?;
    if actual != expected {
        return Err(io::Error::other(
            "managed snapshot execution source identity changed",
        ));
    }
    Ok(())
}

/// Derive and validate one macOS source path from the pinned snapshot directory object.
/// 从固定快照目录对象派生并校验一个 macOS 源码路径。
///
/// `root_fd` is authoritative, `relative_path` is package-relative, and `display_root` is diagnostic.
/// `root_fd` 是权威对象，`relative_path` 是包相对路径，`display_root` 用于诊断。
///
/// Returns an object-bound path suitable for Python or Node while the root descriptor remains live.
/// 在根描述符保持存活期间，返回适用于 Python 或 Node 的对象绑定路径。
#[cfg(target_os = "macos")]
fn macos_snapshot_execution_source_path(
    root_fd: RawFd,
    relative_path: &Path,
    display_root: &Path,
) -> Result<PathBuf, String> {
    validate_unix_snapshot_regular_file(root_fd, relative_path, display_root)?;
    let mut buffer = [0_u8; libc::PATH_MAX as usize];
    if unsafe { libc::fcntl(root_fd, libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(format!(
            "failed to derive managed snapshot path from macOS directory descriptor: {}",
            io::Error::last_os_error()
        ));
    }
    let object_path = unsafe { CStr::from_ptr(buffer.as_ptr().cast()) };
    let root_path = PathBuf::from(std::ffi::OsStr::from_bytes(object_path.to_bytes()));
    let named_root = path_to_c_string(&root_path, "managed snapshot object path")?;
    let named_root_fd = unsafe {
        libc::open(
            named_root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if named_root_fd < 0 {
        return Err(format!(
            "failed to reopen macOS managed snapshot object path {}: {}",
            render_host_visible_path(&root_path),
            io::Error::last_os_error()
        ));
    }
    let named_root_owner = unsafe { OwnedFd::from_raw_fd(named_root_fd) };
    let expected_root = macos_file_identity_from_fd(root_fd).map_err(|error| error.to_string())?;
    let actual_root = macos_file_identity_from_fd(named_root_owner.as_raw_fd())
        .map_err(|error| error.to_string())?;
    if actual_root != expected_root {
        return Err("macOS managed snapshot object path identity changed".to_string());
    }
    let source_path = root_path.join(relative_path);
    let expected_source = macos_file_identity_from_snapshot(root_fd, relative_path, display_root)?;
    let named_source = path_to_c_string(&source_path, "managed snapshot execution source")?;
    macos_validate_named_execution_source(&named_source, expected_source)
        .map_err(|error| error.to_string())?;
    Ok(source_path)
}

impl Drop for ManagedPackageSnapshot {
    /// Remove the snapshot after process teardown or any unpublished launch failure.
    /// 在进程清理或任何未发布启动失败后删除快照。
    fn drop(&mut self) {
        drop(self.execution_pin.take());
        cleanup_or_handoff_managed_snapshot(self.ownership.take());
    }
}

/// Securely clean one optional snapshot ownership value or hand it to the persistent retry owner.
/// 安全清理一个可选快照所有权值，或将其交给持久重试所有者。
///
/// `ownership` is consumed exactly once. Successful deletion releases its permit; failure transfers
/// the record without opening a gap in ownership.
/// `ownership` 只会被消费一次。删除成功时释放许可；失败时无所有权空窗地移交记录。
fn cleanup_or_handoff_managed_snapshot(ownership: Option<ManagedSnapshotCleanupOwnership>) {
    cleanup_or_handoff_managed_snapshot_with(ownership, remove_managed_package_snapshot);
}

/// Securely clean one snapshot through an injectable remover while preserving panic ownership.
/// 通过可注入删除器安全清理单个快照，同时在 panic 时保留所有权。
///
/// `ownership` contains the inseparable record and permit; `remove_snapshot` performs the immediate
/// cleanup attempt and exists as an injection point for deterministic ownership tests.
/// `ownership` 包含不可分割的记录与许可；`remove_snapshot` 执行即时清理，并作为确定性所有权
/// 测试的注入点。
fn cleanup_or_handoff_managed_snapshot_with<F>(
    ownership: Option<ManagedSnapshotCleanupOwnership>,
    remove_snapshot: F,
) where
    F: FnOnce(&mut ManagedSnapshotCleanupRecord) -> Result<(), String>,
{
    let Some(ManagedSnapshotCleanupOwnership { mut record, permit }) = ownership else {
        return;
    };
    // ImmediateCleanup is isolated so stack unwinding cannot discard the record and its permit.
    // 隔离即时清理调用，使栈展开无法丢弃记录及其许可。
    let immediate_cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        remove_snapshot(&mut record)
    }));
    // Every ordinary error or panic transfers the exact mutable record into persistent ownership.
    // 每个普通错误或 panic 都会把精确可变记录移交给持久所有权。
    let cleanup_error = match immediate_cleanup {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some("managed snapshot immediate cleanup panicked".to_string()),
    };
    if let Some(error) = cleanup_error {
        log_warn(format!(
            "[LuaSkill:warn] managed package snapshot cleanup failed and was queued for retry: {error}"
        ));
        permit.handoff(record);
    }
}

/// Provisional RAII owner covering every fallible step after atomic directory creation.
/// 覆盖原子目录创建后每个可能失败步骤的临时 RAII 所有者。
struct ManagedSnapshotPreparation {
    /// Ownership promoted into the published snapshot only after copying completes.
    /// 仅在复制完成后提升为已发布快照的所有权。
    ownership: Option<ManagedSnapshotCleanupOwnership>,
}

impl Drop for ManagedSnapshotPreparation {
    /// Clean or hand off any snapshot that was not successfully published.
    /// 清理或交接任何未成功发布的快照。
    fn drop(&mut self) {
        cleanup_or_handoff_managed_snapshot(self.ownership.take());
    }
}

impl ManagedSnapshotCleanupPermit {
    /// Transfer one failed cleanup record into its pre-reserved persistent retry queue.
    /// 将一个失败清理记录移入其预留的持久重试队列。
    fn handoff(mut self, record: ManagedSnapshotCleanupRecord) {
        self.retry
            .queue
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(record);
        self.reserved = false;
        self.retry.queue.changed.notify_one();
    }
}

impl Drop for ManagedSnapshotCleanupPermit {
    /// Release capacity after successful synchronous cleanup or pre-creation failure.
    /// 在同步清理成功或创建前失败后释放容量。
    fn drop(&mut self) {
        if self.reserved {
            self.retry.reserved.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Reserve one bounded cleanup slot before any snapshot filesystem object is created.
/// 在创建任何快照文件系统对象前预留一个有界清理槽位。
///
/// Returns a permit tied to the initialized process-wide retry owner.
/// 返回绑定到已初始化进程级重试所有者的许可。
fn reserve_managed_snapshot_cleanup_slot() -> Result<ManagedSnapshotCleanupPermit, String> {
    let retry = managed_snapshot_cleanup_retry()?;
    retry
        .reserved
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
            (reserved < MANAGED_SNAPSHOT_CLEANUP_CAPACITY).then_some(reserved + 1)
        })
        .map_err(|_| {
            format!(
                "managed snapshot cleanup capacity is exhausted ({MANAGED_SNAPSHOT_CLEANUP_CAPACITY})"
            )
        })?;
    Ok(ManagedSnapshotCleanupPermit {
        retry,
        reserved: true,
    })
}

/// Return the lazily initialized process-wide snapshot cleanup retry owner.
/// 返回延迟初始化的进程级快照清理重试所有者。
///
/// The worker is started before the owner becomes available, so every issued permit has a live
/// static queue capable of retaining its failed cleanup record.
/// 工作线程会在所有者可用前启动，因此每个已发放许可都有可保留失败清理记录的静态队列。
fn managed_snapshot_cleanup_retry() -> Result<&'static ManagedSnapshotCleanupRetry, String> {
    MANAGED_SNAPSHOT_CLEANUP_RETRY
        .get_or_init(|| {
            let retry = ManagedSnapshotCleanupRetry {
                queue: Arc::new(ManagedSnapshotCleanupQueue {
                    records: Mutex::new(VecDeque::new()),
                    changed: Condvar::new(),
                }),
                reserved: Arc::new(AtomicUsize::new(0)),
            };
            let queue = Arc::clone(&retry.queue);
            std::thread::Builder::new()
                .name("luaskills-snapshot-cleanup".to_string())
                .spawn(move || managed_snapshot_cleanup_worker(queue))
                .map_err(|error| {
                    format!("failed to start managed snapshot cleanup worker: {error}")
                })?;
            Ok(retry)
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Persistently retry every retained snapshot cleanup record until secure deletion succeeds.
/// 持续重试每个已保留快照清理记录，直到安全删除成功。
///
/// `queue` is process-static through its owner; each record is retained outside the guarded cleanup
/// call, so a cleanup panic cannot discard filesystem ownership.
/// `queue` 通过其所有者具备进程级静态生命周期；每条记录都保留在受保护的清理调用之外，
/// 因此清理恐慌不会丢失文件系统所有权。
fn managed_snapshot_cleanup_worker(queue: Arc<ManagedSnapshotCleanupQueue>) {
    loop {
        let mut record = {
            let mut records = queue
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while records.is_empty() {
                records = queue
                    .changed
                    .wait(records)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            records
                .pop_front()
                .expect("non-empty managed snapshot cleanup queue must yield one record")
        };
        // Filesystem work runs without the queue lock, so Drop handoff stays bounded by one push.
        // 文件系统操作不持有队列锁，使析构交接只受一次入队操作约束。
        let cleanup_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            remove_managed_package_snapshot(&mut record)
        }));
        match cleanup_result {
            Ok(Ok(())) => {
                if let Ok(retry) = managed_snapshot_cleanup_retry() {
                    retry.reserved.fetch_sub(1, Ordering::AcqRel);
                }
            }
            Ok(Err(error)) => {
                if !record.error_reported {
                    log_warn(format!(
                        "[LuaSkill:warn] managed package snapshot cleanup retry remains pending: {error}"
                    ));
                    record.error_reported = true;
                }
                queue
                    .records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(record);
                std::thread::sleep(MANAGED_SNAPSHOT_CLEANUP_RETRY_DELAY);
            }
            Err(_) => {
                log_warn(
                    "[LuaSkill:warn] managed snapshot cleanup worker panicked; retaining record for retry"
                        .to_string(),
                );
                queue
                    .records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(record);
                std::thread::sleep(MANAGED_SNAPSHOT_CLEANUP_RETRY_DELAY);
            }
        }
    }
}

/// Reject persistent managed-runtime sessions before allocating resources on unsupported targets.
/// 在不支持的目标上分配资源前拒绝受管持久运行时会话。
///
/// This function has no parameters and must run before session reservation or filesystem mutation.
/// 本函数不接收参数，且必须在会话预留或文件系统变更前运行。
///
/// Returns unit on supported targets or an error carrying the stable machine-readable reason code.
/// 在受支持目标上返回空值，否则返回包含稳定机器可读原因码的错误。
pub(crate) fn ensure_persistent_managed_runtime_session_platform_supported() -> Result<(), String> {
    let capability = current_managed_runtime_persistent_session_capability();
    if capability.supported {
        return Ok(());
    }
    Err(format!(
        "persistent managed runtime sessions are unsupported: {}",
        capability
            .reason
            .unwrap_or_else(|| "platform_family_is_not_supported".to_string())
    ))
}

/// Launch one persistent Python process inside the package-declared managed environment.
/// 在包声明的受管环境中启动一个持久 Python 进程。
pub(crate) fn launch_managed_python_session(
    package: &ManagedRuntimePackageContext,
    request: ManagedRuntimeSessionOpenRequest,
    observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
) -> Result<ManagedRuntimeSessionLaunch, String> {
    ensure_persistent_managed_runtime_session_platform_supported()?;
    // Authoritative package manifest captured by the trusted loader.
    // 由可信加载器捕获的权威包清单。
    let manifest = package
        .dependency_manifest()
        .ok_or_else(|| "dependencies.yaml is required for managed Python sessions".to_string())?;
    // Exact Python runtime declaration owned by this package.
    // 当前包拥有的精确 Python 运行时声明。
    let runtime_spec = manifest
        .python_runtime
        .as_ref()
        .ok_or_else(|| "python_runtime is not declared".to_string())?;
    // Reproducible environment plan resolved only from the trusted package context.
    // 仅从可信包上下文解析得到的可复现环境计划。
    let plan = resolve_python_env_plan(package, runtime_spec)?;
    ensure_managed_env(&plan)?;
    // Original source validation rejects unsafe lexical input before fixed-object snapshot copying.
    // 原始源码校验会在固定对象快照复制前拒绝不安全词法输入。
    let _source_file = package.resolve_existing_file(&request.file, "file")?;
    // Private immutable package snapshot prevents the interpreter from reopening mutable package paths.
    // 私有不可变包快照防止解释器重新打开可变包路径。
    let snapshot = Arc::new(prepare_managed_package_snapshot(&plan, package, ".ls-s")?);
    // The exclusive cwd pin is acquired after package copying and retained through CreateProcess.
    // 在包复制完成后获取排他 cwd 固定器，并保留至 CreateProcess 完成。
    let cwd = resolve_managed_session_cwd(package, request.cwd.as_deref())?;
    // Environment-specific Python executable created by uv.
    // 由 uv 创建的环境专属 Python 可执行文件。
    let python_executable = managed_python_session_executable(&plan);
    ensure_regular_file(&python_executable, "managed Python executable")?;
    // Fully configured command whose environment is rebuilt from the managed baseline.
    // 根据受管基线重新构建环境的完整命令。
    let mut command = Command::new(&python_executable);
    snapshot.configure_source_inheritance(&mut command)?;
    let snapshot_source = snapshot.execution_source_path(&request.file)?;
    snapshot.configure_execution_path_revalidation(
        &mut command,
        &request.file,
        &snapshot_source,
    )?;
    ensure_regular_file(&snapshot_source, "managed Python snapshot source")?;
    let execution_cwd = cwd.canonical.clone();
    command
        .arg(host_process_path_argument(&snapshot_source))
        .args(&request.args);
    configure_managed_command_cwd(&mut command, &cwd)?;
    // Canonical Python virtual-environment directory.
    // 规范 Python 虚拟环境目录。
    configure_managed_python_command_environment(&mut command, &plan)?;
    install_controlled_context_environment(&mut command, package)?;
    // Shared process-core launch options preserving the Lua-facing IO contract.
    // 保持 Lua IO 契约的共享进程核心启动选项。
    let options = launch_options_from_request(&request);
    // Pure Rust process core reused by ordinary and managed runtime sessions.
    // 普通与受管运行时会话共同复用的纯 Rust 进程核心。
    let reaper_snapshot = Arc::clone(&snapshot);
    let core = launch_managed_process_core(
        command,
        options,
        observer,
        Some(Box::new(move || drop(reaper_snapshot))),
    )
    .map_err(|error| {
        format!(
            "{error}; managed Python snapshot source {}; execution cwd {}",
            render_host_visible_path(&snapshot_source),
            render_host_visible_path(&execution_cwd)
        )
    })?;
    let cleanup: ManagedProcessSessionCleanup = Box::new(move || drop(snapshot));
    Ok(ManagedRuntimeSessionLaunch {
        core,
        cleanup: Some(cleanup),
    })
}

/// Launch one persistent Node process inside a per-session package snapshot.
/// 在每会话独立包快照中启动一个持久 Node 进程。
pub(crate) fn launch_managed_node_session(
    package: &ManagedRuntimePackageContext,
    request: ManagedRuntimeSessionOpenRequest,
    observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
) -> Result<ManagedRuntimeSessionLaunch, String> {
    ensure_persistent_managed_runtime_session_platform_supported()?;
    // Authoritative package manifest captured by the trusted loader.
    // 由可信加载器捕获的权威包清单。
    let manifest = package
        .dependency_manifest()
        .ok_or_else(|| "dependencies.yaml is required for managed Node sessions".to_string())?;
    // Exact Node runtime declaration owned by this package.
    // 当前包拥有的精确 Node 运行时声明。
    let runtime_spec = manifest
        .node_runtime
        .as_ref()
        .ok_or_else(|| "node_runtime is not declared".to_string())?;
    // Reproducible environment plan resolved only from the trusted package context.
    // 仅从可信包上下文解析得到的可复现环境计划。
    let plan = resolve_node_env_plan(package, runtime_spec)?;
    ensure_managed_env(&plan)?;
    ensure_regular_file(&plan.runtime_executable, "managed Node executable")?;
    // Canonical original file validated before copying any package content.
    // 在复制任何包内容前校验的规范原始文件。
    let _source_file = package.resolve_existing_file(&request.file, "file")?;
    // Unique package snapshot kept under the dependency environment for Node resolution.
    // 保存在依赖环境下以支持 Node 解析的唯一包快照。
    let snapshot = Arc::new(prepare_managed_package_snapshot(&plan, package, ".ls-s")?);
    // The exclusive cwd pin is acquired after package copying and retained through CreateProcess.
    // 在包复制完成后获取排他 cwd 固定器，并保留至 CreateProcess 完成。
    let cwd = resolve_managed_session_cwd(package, request.cwd.as_deref())?;
    // Snapshot source path corresponding to the already validated relative file.
    // 与已校验相对文件对应的快照源路径。
    // Fully configured command using the managed Node executable.
    // 使用受管 Node 可执行文件的完整命令。
    let mut command = Command::new(&plan.runtime_executable);
    snapshot.configure_source_inheritance(&mut command)?;
    let snapshot_source = snapshot.node_execution_source_path(&request.file)?;
    snapshot.configure_execution_path_revalidation(
        &mut command,
        &request.file,
        &snapshot_source,
    )?;
    ensure_regular_file(&snapshot_source, "managed Node snapshot source")?;
    let execution_cwd = cwd.canonical.clone();
    command.arg(&snapshot_source).args(&request.args);
    configure_managed_command_cwd(&mut command, &cwd)?;
    configure_managed_node_command_environment(&mut command, &plan)?;
    install_controlled_context_environment(&mut command, package)?;
    // Shared process-core launch options preserving the Lua-facing IO contract.
    // 保持 Lua IO 契约的共享进程核心启动选项。
    let options = launch_options_from_request(&request);
    // Launch result whose failure path must remove the unpublished snapshot immediately.
    // 启动失败路径必须立即删除未发布快照的启动结果。
    let reaper_snapshot = Arc::clone(&snapshot);
    let core = launch_managed_process_core(
        command,
        options,
        observer,
        Some(Box::new(move || drop(reaper_snapshot))),
    )
    .map_err(|error| {
        format!(
            "{error}; managed Node snapshot source {}; execution cwd {}",
            render_host_visible_path(&snapshot_source),
            render_host_visible_path(&execution_cwd)
        )
    })?;
    // One-shot cleanup executed only after the userdata tears down the process tree.
    // 仅在 userdata 清理进程树后执行的一次性清理。
    let cleanup: ManagedProcessSessionCleanup = Box::new(move || drop(snapshot));
    Ok(ManagedRuntimeSessionLaunch {
        core,
        cleanup: Some(cleanup),
    })
}

/// Launch the shared process core with optional background observation.
/// 使用可选后台观察启动共享进程核心。
fn launch_managed_process_core(
    command: Command,
    options: ManagedProcessSessionLaunchOptions,
    observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
    keepalive: Option<Box<dyn FnOnce() + Send>>,
) -> Result<ManagedProcessSessionCore, String> {
    ManagedProcessSessionCore::launch_with_optional_observer_and_keepalive(
        command, options, observer, keepalive,
    )
}

/// Copy one package tree while rejecting symbolic links and nested dependency directories.
/// 复制单个包目录树，同时拒绝符号链接并跳过嵌套依赖目录。
#[cfg(test)]
pub(crate) fn copy_managed_package_tree(source: &Path, destination: &Path) -> Result<(), String> {
    copy_managed_package_tree_from_fixed_root(source, destination, None, None).map(drop)
}

/// Copy one package tree from a fixed root object, optionally enforcing activation identity.
/// 从固定根对象复制单个包目录树，并可选择强制校验激活身份。
///
/// `source` and `destination` name source and private snapshot roots; `expected_identity` binds the
/// opened source root to the package context when supplied.
/// `source` 与 `destination` 分别命名源根和私有快照根；提供 `expected_identity` 时会把已打开
/// 源根绑定到包上下文。
///
/// Returns an execution pin only after every source entry was copied from and into fixed objects.
/// 仅当每个源条目均从固定对象复制到固定对象后返回执行固定器。
fn copy_managed_package_tree_from_fixed_root(
    source: &Path,
    destination: &Path,
    expected_identity: Option<&ManagedFilesystemObjectIdentity>,
    expected_destination_identity: Option<&ManagedPackageSnapshotIdentity>,
) -> Result<ManagedSnapshotExecutionPin, String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "failed to create {}: {error}",
            render_host_visible_path(destination)
        )
    })?;
    #[cfg(unix)]
    {
        let encoded_source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
            format!(
                "managed package source contains an interior NUL: {}",
                render_host_visible_path(source)
            )
        })?;
        let raw_fd = unsafe {
            libc::open(
                encoded_source.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw_fd < 0 {
            return Err(format!(
                "failed to pin managed package source {}: {}",
                render_host_visible_path(source),
                std::io::Error::last_os_error()
            ));
        }
        let source_directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let actual_identity = unix_managed_package_identity(source_directory.as_raw_fd(), source)?;
        if expected_identity.is_some_and(|expected| expected != &actual_identity) {
            return Err(format!(
                "managed package root changed before snapshot copy: {}",
                render_host_visible_path(source)
            ));
        }
        let encoded_destination =
            CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
                format!(
                    "managed snapshot destination contains an interior NUL: {}",
                    render_host_visible_path(destination)
                )
            })?;
        let destination_fd = unsafe {
            libc::open(
                encoded_destination.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if destination_fd < 0 {
            return Err(format!(
                "failed to pin managed snapshot destination {}: {}",
                render_host_visible_path(destination),
                std::io::Error::last_os_error()
            ));
        }
        let destination_directory = unsafe { OwnedFd::from_raw_fd(destination_fd) };
        if let Some(expected) = expected_destination_identity {
            let actual =
                unix_managed_package_identity(destination_directory.as_raw_fd(), destination)?;
            let actual = ManagedPackageSnapshotIdentity {
                device: actual.device,
                inode: actual.inode,
            };
            if &actual != expected {
                return Err(format!(
                    "managed snapshot destination changed before copy: {}",
                    render_host_visible_path(destination)
                ));
            }
        }
        let execution_fd =
            unsafe { libc::fcntl(destination_directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if execution_fd < 0 {
            return Err(format!(
                "failed to reserve managed snapshot execution descriptor {}: {}",
                render_host_visible_path(destination),
                std::io::Error::last_os_error()
            ));
        }
        copy_unix_managed_package_directory(
            source_directory.as_raw_fd(),
            destination_directory.as_raw_fd(),
            source,
            destination,
        )?;
        Ok(ManagedSnapshotExecutionPin::Unix {
            directory: unsafe { OwnedFd::from_raw_fd(execution_fd) },
        })
    }
    #[cfg(windows)]
    {
        let (source_directory, information) = open_windows_managed_package_object(source)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(format!(
                "managed package source is not a directory: {}",
                render_host_visible_path(source)
            ));
        }
        let actual_identity = ManagedFilesystemObjectIdentity {
            volume_serial_number: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        };
        if expected_identity.is_some_and(|expected| expected != &actual_identity) {
            return Err(format!(
                "managed package root changed before snapshot copy: {}",
                render_host_visible_path(source)
            ));
        }
        copy_windows_managed_package_tree_pinned(
            source_directory,
            source,
            destination,
            expected_destination_identity,
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = expected_identity;
        let _ = expected_destination_identity;
        Err(format!(
            "fixed-object managed package copy is unsupported: {}",
            render_host_visible_path(source)
        ))
    }
}

/// Return one Unix directory identity from an already pinned descriptor.
/// 从一个已固定描述符返回 Unix 目录身份。
#[cfg(unix)]
fn unix_managed_package_identity(
    directory_fd: RawFd,
    display_path: &Path,
) -> Result<ManagedFilesystemObjectIdentity, String> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(directory_fd, &mut stat) } != 0 {
        return Err(format!(
            "failed to inspect pinned managed package directory {}: {}",
            render_host_visible_path(display_path),
            std::io::Error::last_os_error()
        ));
    }
    let (device, inode) = unix_stat_device_inode(&stat)?;
    Ok(ManagedFilesystemObjectIdentity { device, inode })
}

/// Recursively copy one Unix package directory through descriptor-relative no-follow opens.
/// 通过描述符相对且不跟随链接的打开操作递归复制 Unix 包目录。
#[cfg(unix)]
fn copy_unix_managed_package_directory(
    source_directory_fd: RawFd,
    destination_directory_fd: RawFd,
    source_display_path: &Path,
    destination: &Path,
) -> Result<(), String> {
    use std::os::unix::ffi::OsStringExt;

    let duplicate_fd = unsafe { libc::dup(source_directory_fd) };
    if duplicate_fd < 0 {
        return Err(format!(
            "failed to duplicate managed package directory {}: {}",
            render_host_visible_path(source_display_path),
            std::io::Error::last_os_error()
        ));
    }
    let stream = unsafe { libc::fdopendir(duplicate_fd) };
    if stream.is_null() {
        unsafe { libc::close(duplicate_fd) };
        return Err(format!(
            "failed to enumerate managed package directory {}: {}",
            render_host_visible_path(source_display_path),
            std::io::Error::last_os_error()
        ));
    }
    let mut names = Vec::<CString>::new();
    loop {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        names.push(CString::new(name.to_bytes()).map_err(|_| {
            format!(
                "managed package entry contains an interior NUL under {}",
                render_host_visible_path(source_display_path)
            )
        })?);
    }
    unsafe { libc::closedir(stream) };
    for name in names {
        let name_bytes = name.as_bytes();
        if name_bytes == b"node_modules" {
            continue;
        }
        let name_os = std::ffi::OsString::from_vec(name_bytes.to_vec());
        let source_path = source_display_path.join(&name_os);
        let destination_path = destination.join(&name_os);
        let source_fd = unsafe {
            libc::openat(
                source_directory_fd,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if source_fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ELOOP) {
                return Err(format!(
                    "unsupported file type: {}",
                    render_host_visible_path(&source_path)
                ));
            }
            return Err(format!(
                "failed to pin managed package entry {}: {}",
                render_host_visible_path(&source_path),
                error
            ));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(source_fd) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(opened.as_raw_fd(), &mut stat) } != 0 {
            return Err(format!(
                "failed to inspect pinned managed package entry {}: {}",
                render_host_visible_path(&source_path),
                std::io::Error::last_os_error()
            ));
        }
        let file_kind = stat.st_mode & libc::S_IFMT;
        if file_kind == libc::S_IFDIR {
            if unsafe { libc::mkdirat(destination_directory_fd, name.as_ptr(), 0o700) } != 0 {
                return Err(format!(
                    "failed to create snapshot directory {}: {}",
                    render_host_visible_path(&destination_path),
                    std::io::Error::last_os_error()
                ));
            }
            let destination_child_fd = unsafe {
                libc::openat(
                    destination_directory_fd,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if destination_child_fd < 0 {
                return Err(format!(
                    "failed to pin snapshot directory {}: {}",
                    render_host_visible_path(&destination_path),
                    std::io::Error::last_os_error()
                ));
            }
            let destination_child = unsafe { OwnedFd::from_raw_fd(destination_child_fd) };
            copy_unix_managed_package_directory(
                opened.as_raw_fd(),
                destination_child.as_raw_fd(),
                &source_path,
                &destination_path,
            )?;
        } else if file_kind == libc::S_IFREG {
            let mut source_file = fs::File::from(opened);
            let destination_fd = unsafe {
                libc::openat(
                    destination_directory_fd,
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW,
                    0o600,
                )
            };
            if destination_fd < 0 {
                return Err(format!(
                    "failed to copy {} to {}: {}",
                    render_host_visible_path(&source_path),
                    render_host_visible_path(&destination_path),
                    std::io::Error::last_os_error()
                ));
            }
            let mut destination_file = unsafe { fs::File::from_raw_fd(destination_fd) };
            io::copy(&mut source_file, &mut destination_file).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    render_host_visible_path(&source_path),
                    render_host_visible_path(&destination_path)
                )
            })?;
        } else {
            return Err(format!(
                "unsupported file type: {}",
                render_host_visible_path(&source_path)
            ));
        }
    }
    Ok(())
}

/// Open one Windows package object without write/delete sharing and return handle metadata.
/// 在不共享写入/删除的情况下打开 Windows 包对象并返回句柄元数据。
#[cfg(windows)]
fn open_windows_managed_package_object(
    path: &Path,
) -> Result<(fs::File, BY_HANDLE_FILE_INFORMATION), String> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let raw_handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw_handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed to pin managed package object {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_handle(raw_handle as _) };
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0 {
        return Err(format!(
            "failed to inspect pinned managed package object {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "unsupported file type: {}",
            render_host_visible_path(path)
        ));
    }
    Ok((file, information))
}

/// Copy a Windows package and atomically transfer destination handles into the execution pin.
/// 复制 Windows 包，并把目标句柄原子转移到执行固定器。
#[cfg(windows)]
fn copy_windows_managed_package_tree_pinned(
    source_directory: fs::File,
    source: &Path,
    destination: &Path,
    expected_destination_identity: Option<&ManagedPackageSnapshotIdentity>,
) -> Result<ManagedSnapshotExecutionPin, String> {
    let destination_root = open_windows_mutable_snapshot_directory(destination)?;
    if let Some(expected) = expected_destination_identity {
        let actual = windows_snapshot_identity_from_file(&destination_root, destination)?;
        if &actual != expected {
            return Err(format!(
                "managed snapshot destination changed before copy: {}",
                render_host_visible_path(destination)
            ));
        }
    }
    let mut mutable_directories = vec![destination_root];
    let mut directory_paths = vec![destination.to_path_buf()];
    let mut execution_handles = Vec::new();
    copy_windows_managed_package_directory(
        source_directory,
        source,
        destination,
        &mut mutable_directories,
        &mut directory_paths,
        &mut execution_handles,
    )?;
    // Strict directory handles are acquired while mutable pins still prevent name replacement;
    // releasing mutable pins afterwards leaves no unpinned transition window.
    // 在可变固定器仍阻止名称替换时获取严格目录句柄；随后释放可变固定器，不产生未固定窗口。
    for directory_path in directory_paths {
        let (handle, information) = open_windows_managed_package_object(&directory_path)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(format!(
                "managed snapshot directory changed during pin transfer: {}",
                render_host_visible_path(&directory_path)
            ));
        }
        execution_handles.push(handle);
    }
    drop(mutable_directories);
    Ok(ManagedSnapshotExecutionPin::Windows {
        _handles: execution_handles,
    })
}

/// Read one Windows snapshot identity from an already opened file-backed directory handle.
/// 从一个已打开的文件型目录句柄读取 Windows 快照身份。
#[cfg(windows)]
fn windows_snapshot_identity_from_file(
    file: &fs::File,
    path: &Path,
) -> Result<ManagedPackageSnapshotIdentity, String> {
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0 {
        return Err(format!(
            "failed to inspect managed snapshot destination {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    Ok(ManagedPackageSnapshotIdentity {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
    })
}

/// Open one mutable Windows snapshot directory without delete sharing during construction.
/// 在构建期间以不共享删除的方式打开一个可变 Windows 快照目录。
#[cfg(windows)]
fn open_windows_mutable_snapshot_directory(path: &Path) -> Result<fs::File, String> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let raw_handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw_handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed to pin mutable snapshot directory {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_handle(raw_handle as _) };
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(format!(
            "mutable snapshot path is not a real directory: {}",
            render_host_visible_path(path)
        ));
    }
    Ok(file)
}

/// Recursively copy Windows entries while retaining every created destination object handle.
/// 在保留每个已创建目标对象句柄的同时递归复制 Windows 条目。
#[cfg(windows)]
fn copy_windows_managed_package_directory(
    _source_directory: fs::File,
    source: &Path,
    destination: &Path,
    mutable_directories: &mut Vec<fs::File>,
    directory_paths: &mut Vec<PathBuf>,
    execution_handles: &mut Vec<fs::File>,
) -> Result<(), String> {
    let entries = fs::read_dir(source).map_err(|error| {
        format!(
            "failed to enumerate pinned managed package directory {}: {error}",
            render_host_visible_path(source)
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read managed package entry under {}: {error}",
                render_host_visible_path(source)
            )
        })?;
        let name = entry.file_name();
        if name == "node_modules" {
            continue;
        }
        let source_path = source.join(&name);
        let destination_path = destination.join(&name);
        let (mut source_object, information) = open_windows_managed_package_object(&source_path)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            fs::create_dir(&destination_path).map_err(|error| {
                format!(
                    "failed to create snapshot directory {}: {error}",
                    render_host_visible_path(&destination_path)
                )
            })?;
            let destination_directory = open_windows_mutable_snapshot_directory(&destination_path)?;
            mutable_directories.push(destination_directory);
            directory_paths.push(destination_path.clone());
            copy_windows_managed_package_directory(
                source_object,
                &source_path,
                &destination_path,
                mutable_directories,
                directory_paths,
                execution_handles,
            )?;
        } else {
            let mut destination_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ)
                .open(&destination_path)
                .map_err(|error| {
                    format!(
                        "failed to copy {} to {}: {error}",
                        render_host_visible_path(&source_path),
                        render_host_visible_path(&destination_path)
                    )
                })?;
            io::copy(&mut source_object, &mut destination_file).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    render_host_visible_path(&source_path),
                    render_host_visible_path(&destination_path)
                )
            })?;
            clear_windows_readonly_attribute(&destination_path)?;
            execution_handles.push(destination_file);
        }
    }
    Ok(())
}

/// Pin every executable snapshot object for the complete child-process lifetime.
/// 在完整子进程生命周期内固定每个可执行快照对象。
#[cfg(all(test, windows))]
fn pin_managed_snapshot_execution_tree(
    snapshot_root: &Path,
) -> Result<ManagedSnapshotExecutionPin, String> {
    #[cfg(unix)]
    {
        let encoded = CString::new(snapshot_root.as_os_str().as_bytes()).map_err(|_| {
            format!(
                "managed snapshot path contains an interior NUL: {}",
                render_host_visible_path(snapshot_root)
            )
        })?;
        let raw_fd = unsafe {
            libc::open(
                encoded.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw_fd < 0 {
            return Err(format!(
                "failed to pin managed snapshot root {}: {}",
                render_host_visible_path(snapshot_root),
                std::io::Error::last_os_error()
            ));
        }
        let original = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let safe_fd = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if safe_fd < 0 {
            return Err(format!(
                "failed to reserve a non-stdio managed snapshot descriptor {}: {}",
                render_host_visible_path(snapshot_root),
                std::io::Error::last_os_error()
            ));
        }
        Ok(ManagedSnapshotExecutionPin::Unix {
            directory: unsafe { OwnedFd::from_raw_fd(safe_fd) },
        })
    }
    #[cfg(windows)]
    {
        let mut handles = Vec::new();
        collect_windows_snapshot_execution_pins(snapshot_root, &mut handles)?;
        Ok(ManagedSnapshotExecutionPin::Windows { _handles: handles })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!(
            "managed snapshot execution pinning is unsupported: {}",
            render_host_visible_path(snapshot_root)
        ))
    }
}

/// Recursively retain Windows handles that deny mutation and deletion of snapshot objects.
/// 递归保留拒绝修改和删除快照对象的 Windows 句柄。
#[cfg(all(test, windows))]
fn collect_windows_snapshot_execution_pins(
    path: &Path,
    handles: &mut Vec<fs::File>,
) -> Result<(), String> {
    let (handle, information) = open_windows_managed_package_object(path)?;
    let is_directory = information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    handles.push(handle);
    if is_directory {
        for entry in fs::read_dir(path).map_err(|error| {
            format!(
                "failed to enumerate managed snapshot {} while pinning: {error}",
                render_host_visible_path(path)
            )
        })? {
            let entry = entry.map_err(|error| {
                format!(
                    "failed to read managed snapshot entry under {}: {error}",
                    render_host_visible_path(path)
                )
            })?;
            collect_windows_snapshot_execution_pins(&entry.path(), handles)?;
        }
    }
    Ok(())
}

/// Clear only the Windows read-only file attribute while preserving every other attribute.
/// 仅清除 Windows 文件只读属性，同时保留全部其他属性。
///
/// `path` must identify the copied regular file; success makes later snapshot deletion reliable.
/// `path` 必须标识已复制的普通文件；成功后可保证后续快照删除可靠。
///
/// Returns unit or a host-visible Win32 filesystem error.
/// 返回空值或宿主可见的 Win32 文件系统错误。
#[cfg(windows)]
fn clear_windows_readonly_attribute(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    // NUL-terminated UTF-16 path passed directly to Win32 without lossy conversion.
    // 直接传给 Win32 且不进行有损转换的 NUL 结尾 UTF-16 路径。
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(format!(
            "copied Node session file path contains an embedded NUL: {}",
            render_host_visible_path(path)
        ));
    }
    wide.push(0);
    // Existing attributes read once so hidden, system, and archive flags remain unchanged.
    // 仅读取一次既有属性，使隐藏、系统与存档标记保持不变。
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(format!(
            "failed to inspect copied Node session file {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    if attributes & FILE_ATTRIBUTE_READONLY == 0 {
        return Ok(());
    }
    let status =
        unsafe { SetFileAttributesW(wide.as_ptr(), attributes & !FILE_ATTRIBUTE_READONLY) };
    if status == 0 {
        return Err(format!(
            "failed to clear read-only attribute on copied Node session file {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Convert one parsed session request into shared process-core launch options.
/// 将解析后的会话请求转换为共享进程核心启动选项。
fn launch_options_from_request(
    request: &ManagedRuntimeSessionOpenRequest,
) -> ManagedProcessSessionLaunchOptions {
    ManagedProcessSessionLaunchOptions {
        stdout_encoding: request.stdout_encoding,
        stderr_encoding: request.stderr_encoding,
        stdin_encoding: request.stdin_encoding,
        buffer_limit_bytes: request.buffer_limit_bytes,
    }
}

/// Canonical authorized cwd selected for one managed session.
/// 为单个受管会话选定的规范授权 cwd。
struct ManagedSessionCwd {
    /// Canonical authorized directory pinned during process creation.
    /// 在进程创建期间固定的规范授权目录。
    canonical: PathBuf,
    /// Platform object pin acquired before authorization is finalized.
    /// 在授权最终确定前获取的平台对象固定器。
    pin: ManagedSessionCwdPin,
}

/// Platform object pin binding cwd authorization to process creation.
/// 把 cwd 授权绑定到进程创建的平台对象固定器。
enum ManagedSessionCwdPin {
    /// Unix directory descriptor consumed by child-side fchdir.
    /// 由 child 侧 fchdir 使用的 Unix 目录描述符。
    #[cfg(unix)]
    Unix(OwnedFd),
    /// Windows non-delete-sharing directory handle.
    /// Windows 不共享删除的目录句柄。
    #[cfg(windows)]
    Windows(OwnedHandle),
}

/// Resolve a managed-session cwd under the trusted package or authorized workspace root.
/// 在可信包根或已授权工作区根下解析受管会话 cwd。
fn resolve_managed_session_cwd(
    package: &ManagedRuntimePackageContext,
    requested: Option<&str>,
) -> Result<ManagedSessionCwd, String> {
    // Candidate defaults to the canonical package root.
    // 默认采用规范包根的候选路径。
    let candidate = match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                package.package_root().join(path)
            }
        }
        None => package.package_root().to_path_buf(),
    };
    // Initial canonicalization resolves an existing candidate solely for exact-object opening.
    // 初始规范化仅用于解析现有候选路径，以便打开精确对象。
    let resolved_candidate = fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "failed to canonicalize session cwd {}: {error}",
            render_host_visible_path(&candidate)
        )
    })?;
    let pin = pin_managed_session_cwd(&resolved_candidate)?;
    // Authorization uses the path reported by the same retained object handle used at spawn.
    // 授权使用由启动时保留的同一对象句柄报告的路径。
    let canonical = authoritative_managed_session_cwd_path(&pin, &resolved_candidate)?;
    // Optional canonical workspace root explicitly authorized by the System lease host.
    // 由 System 租约宿主显式授权的可选规范工作区根。
    let workspace_root = package
        .lease_binding()
        .and_then(|binding| binding.workspace_root());
    if !canonical.starts_with(package.package_root())
        && !workspace_root.is_some_and(|root| canonical.starts_with(root))
    {
        return Err(format!(
            "session cwd {} is outside the package root and authorized workspace root",
            render_host_visible_path(&canonical)
        ));
    }
    Ok(ManagedSessionCwd { canonical, pin })
}

/// Open and verify the exact cwd object before containment authorization is finalized.
/// 在最终确定包含授权前打开并验证精确 cwd 对象。
fn pin_managed_session_cwd(path: &Path) -> Result<ManagedSessionCwdPin, String> {
    #[cfg(unix)]
    {
        let encoded = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            format!(
                "managed session cwd contains an interior NUL: {}",
                render_host_visible_path(path)
            )
        })?;
        let raw_fd = unsafe {
            libc::open(
                encoded.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw_fd < 0 {
            return Err(format!(
                "failed to pin managed session cwd {}: {}",
                render_host_visible_path(path),
                std::io::Error::last_os_error()
            ));
        }
        Ok(ManagedSessionCwdPin::Unix(unsafe {
            OwnedFd::from_raw_fd(raw_fd)
        }))
    }
    #[cfg(windows)]
    {
        open_windows_pinned_session_cwd(path).map(ManagedSessionCwdPin::Windows)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!(
            "pinned managed session cwd is unsupported: {}",
            render_host_visible_path(path)
        ))
    }
}

/// Derive the authoritative authorization path from the retained cwd object pin.
/// 从所保留的 cwd 对象固定器派生权威授权路径。
///
/// `pin` is the exact object retained through process creation and `resolved_candidate` is the
/// initial canonical path used to open it. Returns the platform-authoritative absolute path.
/// `pin` 是贯穿进程创建而保留的精确对象，`resolved_candidate` 是用于打开它的初始规范路径；
/// 返回平台权威绝对路径。
fn authoritative_managed_session_cwd_path(
    pin: &ManagedSessionCwdPin,
    resolved_candidate: &Path,
) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let ManagedSessionCwdPin::Unix(_directory) = pin;
        Ok(resolved_candidate.to_path_buf())
    }
    #[cfg(windows)]
    {
        let ManagedSessionCwdPin::Windows(directory) = pin;
        final_windows_path_from_handle(directory, resolved_candidate)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pin;
        Err(format!(
            "authoritative managed session cwd is unsupported: {}",
            render_host_visible_path(resolved_candidate)
        ))
    }
}

/// Configure a command to enter one pinned cwd object during process creation.
/// 配置命令，使其在创建进程期间进入一个固定 cwd 对象。
fn configure_managed_command_cwd(
    command: &mut Command,
    cwd: &ManagedSessionCwd,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let ManagedSessionCwdPin::Unix(directory) = &cwd.pin;
        let directory_fd = directory.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::fchdir(directory_fd) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let ManagedSessionCwdPin::Windows(_directory) = &cwd.pin;
        command.current_dir(&cwd.canonical);
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
        Err(format!(
            "pinned managed session cwd is unsupported: {}",
            render_host_visible_path(&cwd.canonical)
        ))
    }
}

/// Open a Windows cwd without delete sharing until process creation resolves the directory.
/// 在进程创建解析目录前，以不共享删除的方式打开 Windows cwd。
#[cfg(windows)]
fn open_windows_pinned_session_cwd(path: &Path) -> Result<OwnedHandle, String> {
    let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide_path.push(0);
    let raw_handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw_handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed to pin managed session cwd {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle as _) };
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle() as _, &mut information) } == 0 {
        return Err(format!(
            "failed to inspect pinned managed session cwd {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(format!(
            "managed session cwd is not a real directory: {}",
            render_host_visible_path(path)
        ));
    }
    Ok(handle)
}

/// Resolve one normalized DOS path from the exact retained Windows directory handle.
/// 从所保留的精确 Windows 目录句柄解析规范化 DOS 路径。
///
/// `handle` must be the non-delete-sharing directory pin and `opened_path` labels diagnostics.
/// Returns the final path belonging to that handle, not a second name-based lookup.
/// `handle` 必须是不共享删除的目录固定句柄，`opened_path` 用于标记诊断；返回属于该句柄的
/// 最终路径，而不是再次按名称查询的结果。
#[cfg(windows)]
fn final_windows_path_from_handle(
    handle: &OwnedHandle,
    opened_path: &Path,
) -> Result<PathBuf, String> {
    // Buffer grows only according to the kernel-reported required UTF-16 length.
    // 缓冲区仅依据内核报告的 UTF-16 所需长度增长。
    let mut buffer = vec![0_u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).map_err(|_| {
            format!(
                "authoritative session cwd path is too long: {}",
                render_host_visible_path(opened_path)
            )
        })?;
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle() as _,
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if written == 0 {
            return Err(format!(
                "failed to resolve pinned managed session cwd {}: {}",
                render_host_visible_path(opened_path),
                std::io::Error::last_os_error()
            ));
        }
        if written < capacity {
            buffer.truncate(written as usize);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        // An insufficient-buffer result includes the trailing NUL in its required capacity.
        // 缓冲区不足的返回值在所需容量中包含结尾 NUL。
        buffer.resize(written as usize, 0);
    }
}

/// Derive one short neutral Windows worker cwd from an already canonical snapshot path.
/// 从已规范化的快照路径派生一个短中立 Windows Worker cwd。
///
/// `snapshot_root` must use a DOS drive or UNC share namespace. The returned drive/share root
/// prevents host-cwd inheritance and avoids passing a long snapshot path as `lpCurrentDirectory`.
/// `snapshot_root` 必须使用 DOS 驱动器或 UNC 共享命名空间。返回的驱动器/共享根会阻止继承宿主
/// cwd，并避免把长快照路径作为 `lpCurrentDirectory` 传入。
///
/// Returns a root spelling shorter than `MAX_PATH`, or a namespace or length error.
/// 返回短于 `MAX_PATH` 的根路径写法，或命名空间及长度错误。
#[cfg(windows)]
fn windows_worker_neutral_cwd_path(snapshot_root: &Path) -> Result<PathBuf, String> {
    use std::path::Prefix;

    let mut components = snapshot_root.components();
    let Some(Component::Prefix(prefix_component)) = components.next() else {
        return Err(format!(
            "managed worker snapshot lacks a Windows drive or UNC prefix: {}",
            render_host_visible_path(snapshot_root)
        ));
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(format!(
            "managed worker snapshot is not rooted after its Windows prefix: {}",
            render_host_visible_path(snapshot_root)
        ));
    }
    // NeutralRoot preserves exact UTF-16 server/share units and converts verbatim forms only when
    // an ordinary DOS/UNC equivalent exists.
    // NeutralRoot 精确保留 UTF-16 服务器/共享单元，并且仅在存在普通 DOS/UNC 等价形式时转换
    // verbatim 写法。
    let neutral_root = match prefix_component.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            PathBuf::from(OsString::from_wide(&[
                u16::from(letter),
                u16::from(b':'),
                92,
            ]))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            let mut encoded = vec![92, 92];
            encoded.extend(server.encode_wide());
            encoded.push(92);
            encoded.extend(share.encode_wide());
            encoded.push(92);
            PathBuf::from(OsString::from_wide(&encoded))
        }
        _ => {
            return Err(format!(
                "unsupported Windows namespace for managed worker cwd: {}",
                render_host_visible_path(snapshot_root)
            ));
        }
    };
    let encoded_length = neutral_root.as_os_str().encode_wide().count();
    if encoded_length.saturating_add(1) >= 260 {
        return Err(format!(
            "managed worker neutral Windows cwd reaches MAX_PATH: {}",
            render_host_visible_path(&neutral_root)
        ));
    }
    Ok(neutral_root)
}

/// Validate and return one existing neutral Windows worker cwd.
/// 校验并返回一个现有的中立 Windows Worker cwd。
///
/// `snapshot_root` is converted only by `windows_worker_neutral_cwd_path`; its exact drive/share
/// root must exist as a directory before it is installed into `Command`.
/// `snapshot_root` 仅由 `windows_worker_neutral_cwd_path` 转换；其精确驱动器/共享根必须作为
/// 目录存在，随后才能安装到 `Command`。
///
/// Returns the validated root, or a host-visible filesystem/type error.
/// 返回已校验根，或宿主可见的文件系统及类型错误。
#[cfg(windows)]
fn windows_worker_neutral_cwd(snapshot_root: &Path) -> Result<PathBuf, String> {
    let neutral_root = windows_worker_neutral_cwd_path(snapshot_root)?;
    let metadata = fs::metadata(&neutral_root).map_err(|error| {
        format!(
            "failed to inspect managed worker neutral Windows cwd {}: {error}",
            render_host_visible_path(&neutral_root)
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "managed worker neutral Windows cwd is not a directory: {}",
            render_host_visible_path(&neutral_root)
        ));
    }
    Ok(neutral_root)
}

/// Remove one Windows verbatim prefix without lossy string conversion or filesystem lookup.
/// 在不进行有损字符串转换或文件系统查询的情况下移除 Windows verbatim 前缀。
///
/// `path` is already anchored by a retained filesystem handle. Returns the equivalent DOS or UNC
/// spelling accepted by Node's CLI main-module resolver.
/// `path` 已由保留的文件系统句柄锚定；返回 Node CLI 主模块解析器可接受的等价 DOS 或 UNC 写法。
#[cfg(windows)]
fn windows_non_verbatim_path(path: &Path) -> Result<PathBuf, String> {
    // UTF-16 units preserve every Windows path code unit exactly.
    // UTF-16 单元会精确保留 Windows 路径的每个代码单元。
    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    // Canonical verbatim UNC prefix `\\?\UNC\` converted to the ordinary leading `\\`.
    // 规范 verbatim UNC 前缀 `\\?\UNC\` 会被转换为普通前导 `\\`。
    const VERBATIM_UNC_PREFIX: [u16; 8] = [92, 92, 63, 92, 85, 78, 67, 92];
    // Canonical verbatim local prefix `\\?\` removed before a drive-qualified path.
    // 规范 verbatim 本地前缀 `\\?\` 会在驱动器限定路径前被移除。
    const VERBATIM_PREFIX: [u16; 4] = [92, 92, 63, 92];
    if encoded.starts_with(&VERBATIM_UNC_PREFIX) {
        let mut normalized = Vec::with_capacity(encoded.len().saturating_sub(6));
        normalized.extend_from_slice(&[92, 92]);
        normalized.extend_from_slice(&encoded[VERBATIM_UNC_PREFIX.len()..]);
        return Ok(PathBuf::from(OsString::from_wide(&normalized)));
    }
    if encoded.starts_with(&VERBATIM_PREFIX) {
        // Only a drive-qualified verbatim path has an equivalent ordinary DOS spelling.
        // 只有驱动器限定的 verbatim 路径才具备等价普通 DOS 写法。
        let remainder = &encoded[VERBATIM_PREFIX.len()..];
        let drive_qualified = remainder.len() >= 3
            && (remainder[0] >= u16::from(b'A') && remainder[0] <= u16::from(b'Z')
                || remainder[0] >= u16::from(b'a') && remainder[0] <= u16::from(b'z'))
            && remainder[1] == u16::from(b':')
            && remainder[2] == u16::from(b'\\');
        if drive_qualified {
            return Ok(PathBuf::from(OsString::from_wide(remainder)));
        }
        return Err(format!(
            "unsupported Windows verbatim namespace for managed Node source: {}",
            render_host_visible_path(path)
        ));
    }
    Ok(path.to_path_buf())
}

/// Return the environment-specific Python executable path.
/// 返回环境专属 Python 可执行文件路径。
fn managed_python_session_executable(plan: &ManagedRuntimeEnvPlan) -> PathBuf {
    if cfg!(windows) {
        plan.env_dir
            .join(".venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        plan.env_dir.join(".venv").join("bin").join("python")
    }
}

/// Return the parent directory of one absolute managed executable.
/// 返回单个绝对受管可执行文件的父目录。
fn executable_parent(executable: &Path) -> Result<&Path, String> {
    executable.parent().ok_or_else(|| {
        format!(
            "managed executable has no parent directory: {}",
            render_host_visible_path(executable)
        )
    })
}

/// Apply the isolated managed Python environment to one prepared command.
/// 将隔离的受管 Python 环境应用到一个已准备命令。
///
/// `command` is the not-yet-spawned interpreter command and `plan` supplies its exact managed
/// environment plus canonical base-runtime executable.
/// `command` 是尚未启动的解释器命令，`plan` 提供其精确受管环境与规范基础运行时可执行文件。
///
/// Returns unit after replacing host inheritance with controlled Python values and PATH entries.
/// 使用受控 Python 值与 PATH 条目替换宿主继承后返回空值。
pub(crate) fn configure_managed_python_command_environment(
    command: &mut Command,
    plan: &ManagedRuntimeEnvPlan,
) -> Result<(), String> {
    // Canonical virtual environment created under the resolved environment directory.
    // 在已解析环境目录下创建的规范虚拟环境。
    let virtual_env = plan.env_dir.join(".venv");
    // Environment-specific Python executable used to locate the Scripts/bin directory.
    // 用于定位 Scripts/bin 目录的环境专属 Python 可执行文件。
    let python_executable = managed_python_session_executable(plan);
    // Minimal baseline clears every host variable before installing controlled Python values.
    // 最小基线会先清除全部宿主变量，再安装受控 Python 值。
    configure_managed_command_base_environment(
        command,
        &plan.env_dir,
        &[
            executable_parent(&python_executable)?.to_path_buf(),
            executable_parent(&plan.runtime_executable)?.to_path_buf(),
        ],
    )?;
    command.env("VIRTUAL_ENV", virtual_env);
    command.env("PYTHONNOUSERSITE", "1");
    command.env("PYTHONUTF8", "1");
    Ok(())
}

/// Apply the isolated managed Node dependency environment to one prepared command.
/// 将隔离的受管 Node 依赖环境应用到一个已准备命令。
///
/// `command` is the not-yet-spawned Node command and `plan` supplies its exact managed dependency
/// environment plus canonical runtime executable.
/// `command` 是尚未启动的 Node 命令，`plan` 提供其精确受管依赖环境与规范运行时可执行文件。
///
/// Returns unit after replacing host inheritance with controlled Node values and PATH entries.
/// 使用受控 Node 值与 PATH 条目替换宿主继承后返回空值。
pub(crate) fn configure_managed_node_command_environment(
    command: &mut Command,
    plan: &ManagedRuntimeEnvPlan,
) -> Result<(), String> {
    // Minimal baseline exposes only the managed Node binary and installed dependency executables.
    // 最小基线仅暴露受管 Node 二进制文件与已安装依赖可执行文件。
    configure_managed_command_base_environment(
        command,
        &plan.env_dir,
        &[
            executable_parent(&plan.runtime_executable)?.to_path_buf(),
            plan.env_dir.join("node_modules").join(".bin"),
        ],
    )?;
    command.env("NODE_PATH", plan.env_dir.join("node_modules"));
    Ok(())
}

/// Install the controlled package/lease context without copying arbitrary host environment data.
/// 安装受控包与租约上下文，同时不复制任意宿主环境数据。
fn install_controlled_context_environment(
    command: &mut Command,
    package: &ManagedRuntimePackageContext,
) -> Result<(), String> {
    // Compact JSON context containing package and authorized lease metadata only.
    // 仅包含包与已授权租约元数据的紧凑 JSON 上下文。
    let context = to_string(&package.worker_context_json())
        .map_err(|error| format!("failed to encode managed session context: {error}"))?;
    command.env("LUASKILLS_MANAGED_CONTEXT_JSON", context);
    Ok(())
}

/// Create one unique package snapshot under a trusted internal namespace.
/// 在可信内部命名空间下创建一个唯一包快照。
///
/// `plan` supplies the canonical environment, `package` supplies source identity and ownership,
/// and `namespace` must be one safe internal directory component selected by the host.
/// `plan` 提供规范环境，`package` 提供源身份与所有权，`namespace` 必须是宿主选择的单个安全内部目录组件。
///
/// Returns an RAII snapshot owner after a complete copy, or a rollback-complete error.
/// 完整复制后返回 RAII 快照所有者，或返回已完成回滚的错误。
pub(crate) fn prepare_managed_package_snapshot(
    plan: &ManagedRuntimeEnvPlan,
    package: &ManagedRuntimePackageContext,
    namespace: &str,
) -> Result<ManagedPackageSnapshot, String> {
    // Lease acquisition repeats readiness validation while holding the shared lifecycle lock, so
    // no publisher can replace the environment between `ensure_managed_env` and snapshot creation.
    // 租约获取会在持有共享生命周期锁时重新校验就绪状态，因此发布方无法在
    // `ensure_managed_env` 与快照创建之间替换环境。
    let environment_lease = acquire_ready_managed_env_lease(plan)?;
    prepare_managed_package_snapshot_with_lease(plan, package, namespace, Some(environment_lease))
}

/// Create a test fixture snapshot without requiring a published managed-environment marker.
/// 创建无需已发布受管环境 marker 的测试夹具快照。
///
/// `plan`, `package`, and `namespace` retain the production path and copy validation contract;
/// only the environment lifecycle lease is omitted for tests that exercise pre-publication errors.
/// `plan`、`package` 与 `namespace` 保留生产路径及复制校验契约；仅对验证发布前错误的测试省略
/// 环境生命周期租约。
///
/// Returns the same RAII snapshot type as production, or a fully diagnosed preparation error.
/// 返回与生产相同的 RAII 快照类型，或完整诊断的准备错误。
#[cfg(test)]
pub(crate) fn prepare_unleased_managed_package_snapshot(
    plan: &ManagedRuntimeEnvPlan,
    package: &ManagedRuntimePackageContext,
    namespace: &str,
) -> Result<ManagedPackageSnapshot, String> {
    prepare_managed_package_snapshot_with_lease(plan, package, namespace, None)
}

/// Build one immutable package snapshot while transferring its environment lease into cleanup.
/// 构建一个不可变包快照，同时把环境租约移交给清理所有权。
///
/// `environment_lease` is present for every production snapshot and absent only in explicit test
/// fixtures. It remains owned through cleanup retries so environment replacement cannot orphan them.
/// 每个生产快照都具有 `environment_lease`，仅显式测试夹具可缺省；该租约会在清理重试期间
/// 持续被拥有，防止环境替换使重试记录失去归属。
///
/// Returns one published RAII snapshot after atomic setup and fixed-object copying.
/// 在原子设置与固定对象复制完成后返回一个已发布 RAII 快照。
fn prepare_managed_package_snapshot_with_lease(
    plan: &ManagedRuntimeEnvPlan,
    package: &ManagedRuntimePackageContext,
    namespace: &str,
    environment_lease: Option<ManagedRuntimeEnvLease>,
) -> Result<ManagedPackageSnapshot, String> {
    if namespace.is_empty()
        || Path::new(namespace).components().count() != 1
        || namespace == "."
        || namespace == ".."
    {
        return Err("managed snapshot namespace must be one safe path component".to_string());
    }
    // Capacity is reserved before creation so every later failure has a guaranteed static owner.
    // 在创建前预留容量，使之后的每个失败都有确定的静态所有者。
    let cleanup_permit = reserve_managed_snapshot_cleanup_slot()?;
    // Compact environment-local parent keeps native ESM lookup beneath the exact node_modules
    // tree without reintroducing Windows MAX_PATH failures. PID, owner token, and sequence provide
    // collision-free package/session partitioning directly below this private namespace.
    // 紧凑的环境内父目录使原生 ESM 查找位于精确 node_modules 树之下，同时避免重新引入
    // Windows MAX_PATH 故障；PID、所有者令牌与序号直接在此私有命名空间下提供无冲突的
    // 包/会话分区。
    let snapshots_root = plan.env_dir.join(namespace);
    fs::create_dir_all(&snapshots_root).map_err(|error| {
        format!(
            "failed to create managed package snapshot root {}: {error}",
            render_host_visible_path(&snapshots_root)
        )
    })?;
    // Canonical environment root and snapshot parent prevent an existing link from redirecting writes.
    // 规范环境根与快照父目录可防止既有链接重定向写入位置。
    let canonical_env_root = fs::canonicalize(&plan.env_dir).map_err(|error| {
        format!(
            "failed to canonicalize managed environment root {}: {error}",
            render_host_visible_path(&plan.env_dir)
        )
    })?;
    let canonical_snapshots_root = fs::canonicalize(&snapshots_root).map_err(|error| {
        format!(
            "failed to canonicalize managed package snapshot root {}: {error}",
            render_host_visible_path(&snapshots_root)
        )
    })?;
    if canonical_snapshots_root == canonical_env_root
        || !canonical_snapshots_root.starts_with(&canonical_env_root)
    {
        return Err(format!(
            "managed package snapshot root {} escapes managed environment root {}",
            render_host_visible_path(&canonical_snapshots_root),
            render_host_visible_path(&canonical_env_root)
        ));
    }
    // Atomically created unique directory tolerating stale crash remnants and PID reuse.
    // 通过原子创建获得的唯一目录，可容忍崩溃残留与 PID 复用。
    let mut snapshot_root = None;
    // Compact package hash preserves filesystem provenance without restoring the long path segment.
    // 紧凑包哈希在不恢复长路径片段的前提下保留文件系统来源信息。
    let compact_package_hash = package
        .identity()
        .stable_hash()
        .chars()
        .take(8)
        .collect::<String>();
    for _attempt in 0..MAX_MANAGED_PACKAGE_SNAPSHOT_CREATE_ATTEMPTS {
        // Nonzero snapshot sequence that refuses to wrap.
        // 拒绝回绕的非零快照序号。
        let sequence = NEXT_MANAGED_PACKAGE_SNAPSHOT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| "managed package snapshot sequence is exhausted".to_string())?;
        // Candidate name partitions concurrent processes, package lifetimes, and local sessions.
        // 候选名称隔离并发进程、包生命周期及本地会话。
        let candidate = canonical_snapshots_root.join(format!(
            "{:x}-{:x}-{sequence:x}-{compact_package_hash}",
            std::process::id(),
            package.owner_token()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                snapshot_root = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create unique managed package snapshot {}: {error}",
                    render_host_visible_path(&candidate)
                ));
            }
        }
    }
    let snapshot_root = snapshot_root.ok_or_else(|| {
        format!(
            "failed to allocate a unique managed package snapshot after {MAX_MANAGED_PACKAGE_SNAPSHOT_CREATE_ATTEMPTS} attempts"
        )
    })?;
    // Provisional ownership begins immediately after creation; until identity capture succeeds,
    // cleanup is restricted to removing this still-empty directory name non-recursively.
    // 临时所有权在创建后立即生效；身份捕获成功前，清理仅限非递归删除这个仍为空的目录名称。
    let mut preparation = ManagedSnapshotPreparation {
        ownership: Some(ManagedSnapshotCleanupOwnership {
            record: ManagedSnapshotCleanupRecord {
                _environment_lease: environment_lease,
                snapshots_root: canonical_snapshots_root.clone(),
                snapshot_root: snapshot_root.clone(),
                quarantine_root: None,
                orphan_quarantine_root: None,
                snapshot_present: true,
                identity: None,
                error_reported: false,
            },
            permit: cleanup_permit,
        }),
    };
    // Created directory identity captured before copying makes later cleanup replacement-proof.
    // 在复制前捕获创建目录身份，使之后的清理能够抵御路径替换。
    let snapshot_metadata = fs::symlink_metadata(&snapshot_root).map_err(|error| {
        format!(
            "failed to inspect newly created managed package snapshot {}: {error}",
            render_host_visible_path(&snapshot_root)
        )
    })?;
    let snapshot_identity = managed_package_snapshot_identity(&snapshot_metadata, &snapshot_root)?;
    preparation
        .ownership
        .as_mut()
        .expect("live snapshot preparation must retain ownership")
        .record
        .identity = Some(snapshot_identity);
    let canonical_snapshot_root = fs::canonicalize(&snapshot_root).map_err(|error| {
        format!(
            "failed to canonicalize newly created managed package snapshot {}: {error}",
            render_host_visible_path(&snapshot_root)
        )
    })?;
    if canonical_snapshot_root == canonical_snapshots_root
        || !canonical_snapshot_root.starts_with(&canonical_snapshots_root)
    {
        return Err(format!(
            "new managed package snapshot {} escapes verified parent {}",
            render_host_visible_path(&canonical_snapshot_root),
            render_host_visible_path(&canonical_snapshots_root)
        ));
    }
    preparation
        .ownership
        .as_mut()
        .expect("live snapshot preparation must retain ownership")
        .record
        .snapshot_root = canonical_snapshot_root;
    let expected_snapshot_identity = preparation
        .ownership
        .as_ref()
        .and_then(|ownership| ownership.record.identity.as_ref())
        .cloned()
        .ok_or_else(|| "managed snapshot identity is unavailable before copy".to_string())?;
    let execution_pin = copy_managed_package_tree_from_fixed_root(
        package.package_root(),
        &snapshot_root,
        Some(package.package_root_filesystem_identity()),
        Some(&expected_snapshot_identity),
    )?;
    Ok(ManagedPackageSnapshot {
        ownership: preparation.ownership.take(),
        execution_pin: Some(execution_pin),
    })
}

/// Allocate one unique empty quarantine directory under the verified snapshot parent.
/// 在已验证快照父目录下分配一个唯一的空隔离目录。
fn allocate_managed_snapshot_quarantine(snapshots_root: &Path) -> Result<PathBuf, String> {
    for _attempt in 0..MAX_MANAGED_PACKAGE_SNAPSHOT_CREATE_ATTEMPTS {
        let sequence = NEXT_MANAGED_PACKAGE_SNAPSHOT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| "managed package snapshot sequence is exhausted".to_string())?;
        let quarantine = snapshots_root.join(format!(".cleanup-{}-{sequence}", std::process::id()));
        match fs::create_dir(&quarantine) {
            Ok(()) => return Ok(quarantine),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create managed snapshot quarantine {}: {error}",
                    render_host_visible_path(&quarantine)
                ));
            }
        }
    }
    Err(format!(
        "failed to allocate a unique managed snapshot quarantine after {MAX_MANAGED_PACKAGE_SNAPSHOT_CREATE_ATTEMPTS} attempts"
    ))
}

/// Atomically isolate and remove one managed package snapshot after identity validation.
/// 在身份验证后原子隔离并删除单个受管包快照。
fn remove_managed_package_snapshot(
    record: &mut ManagedSnapshotCleanupRecord,
) -> Result<(), String> {
    if let Some(orphan_quarantine_root) = record.orphan_quarantine_root.as_ref() {
        match fs::remove_dir(orphan_quarantine_root) {
            Ok(()) => record.orphan_quarantine_root = None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                record.orphan_quarantine_root = None;
            }
            Err(error) => {
                return Err(format!(
                    "failed to remove orphaned managed snapshot quarantine {}: {error}",
                    render_host_visible_path(orphan_quarantine_root)
                ));
            }
        }
    }
    let Some(expected_identity) = record.identity.as_ref() else {
        // Before identity capture no package data has been copied, so only an empty-name removal is
        // authorized. A replaced or populated object remains owned and is retried without recursion.
        // 身份捕获前尚未复制包数据，因此仅授权删除空目录名称。被替换或填充的对象仍被保留所有权，
        // 并在不递归删除的前提下重试。
        return match fs::remove_dir(&record.snapshot_root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "failed to remove empty provisional managed snapshot {}: {error}",
                render_host_visible_path(&record.snapshot_root)
            )),
        };
    };
    if record.quarantine_root.is_none() {
        // Canonical parent identity is established before the atomic isolation rename.
        // 在原子隔离重命名前建立规范父目录身份。
        let canonical_parent = fs::canonicalize(&record.snapshots_root).map_err(|error| {
            format!(
                "failed to canonicalize managed package snapshot parent {}: {error}",
                render_host_visible_path(&record.snapshots_root)
            )
        })?;
        if canonical_parent != record.snapshots_root {
            return Err(format!(
                "refusing to remove managed package snapshot through a replaced parent: {}",
                render_host_visible_path(&record.snapshots_root)
            ));
        }
        // A fresh private quarantine parent makes rename capture one exact path object atomically.
        // 新建的私有隔离父目录使重命名能够原子捕获一个精确路径对象。
        let quarantine_root = allocate_managed_snapshot_quarantine(&canonical_parent)?;
        let isolated_snapshot = quarantine_root.join("captured");
        match fs::rename(&record.snapshot_root, &isolated_snapshot) {
            Ok(()) => {
                // Ownership follows the successful rename before any fallible validation/deletion.
                // 在任何可能失败的验证或删除前，让所有权立即跟随成功的重命名。
                record.snapshot_root = isolated_snapshot;
                record.quarantine_root = Some(quarantine_root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // The owned snapshot name is gone; retain the created quarantine until its empty
                // directory deletion is also confirmed.
                // 所拥有的快照名称已消失；继续保留新建隔离目录，直到确认其空目录删除完成。
                record.snapshot_root = isolated_snapshot;
                record.quarantine_root = Some(quarantine_root);
                record.snapshot_present = false;
            }
            Err(error) => {
                record.orphan_quarantine_root = Some(quarantine_root);
                let isolation_error = format!(
                    "failed to isolate managed package snapshot {}: {error}",
                    render_host_visible_path(&record.snapshot_root)
                );
                return match fs::remove_dir(
                    record
                        .orphan_quarantine_root
                        .as_ref()
                        .expect("failed rename must retain orphan quarantine ownership"),
                ) {
                    Ok(()) => {
                        record.orphan_quarantine_root = None;
                        Err(isolation_error)
                    }
                    Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {
                        record.orphan_quarantine_root = None;
                        Err(isolation_error)
                    }
                    Err(cleanup_error) => Err(format!(
                        "{isolation_error}; orphan quarantine cleanup also failed: {cleanup_error}"
                    )),
                };
            }
        }
    }
    let quarantine_root = record
        .quarantine_root
        .as_ref()
        .expect("isolated managed snapshot must retain its quarantine root");
    if !record.snapshot_present {
        return match fs::remove_dir(quarantine_root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "failed to remove managed snapshot quarantine {}: {error}",
                render_host_visible_path(quarantine_root)
            )),
        };
    }
    let isolated_snapshot = &record.snapshot_root;

    // Identity is checked only after atomic isolation, so a pre-rename path swap is never deleted.
    // 仅在原子隔离后检查身份，因此重命名前发生的路径替换绝不会被删除。
    let metadata = fs::symlink_metadata(isolated_snapshot).map_err(|error| {
        format!(
            "failed to inspect isolated managed package snapshot {}: {error}",
            render_host_visible_path(isolated_snapshot)
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "refusing to remove replaced managed package snapshot object: {}",
            render_host_visible_path(isolated_snapshot)
        ));
    }
    let actual_identity = managed_package_snapshot_identity(&metadata, isolated_snapshot)?;
    if &actual_identity != expected_identity {
        return Err(format!(
            "refusing to remove managed package snapshot whose filesystem identity changed: {}",
            render_host_visible_path(isolated_snapshot)
        ));
    }
    let canonical_quarantine = fs::canonicalize(quarantine_root).map_err(|error| {
        format!(
            "failed to canonicalize managed snapshot quarantine {}: {error}",
            render_host_visible_path(quarantine_root)
        )
    })?;
    let canonical_isolated = fs::canonicalize(isolated_snapshot).map_err(|error| {
        format!(
            "failed to canonicalize isolated managed package snapshot {}: {error}",
            render_host_visible_path(isolated_snapshot)
        )
    })?;
    if canonical_quarantine != *quarantine_root
        || canonical_quarantine.parent() != Some(record.snapshots_root.as_path())
        || canonical_isolated != *isolated_snapshot
        || canonical_isolated.parent() != Some(canonical_quarantine.as_path())
    {
        return Err(format!(
            "refusing to remove managed package snapshot outside its atomic quarantine: {}",
            render_host_visible_path(&canonical_isolated)
        ));
    }
    remove_verified_managed_snapshot_tree(isolated_snapshot, expected_identity)?;
    // Advance the irreversible phase before removing the parent so retries never inspect a deleted
    // captured path and can finish the quarantine-only phase independently.
    // 在删除父目录前推进不可逆阶段，使重试不会检查已删除的 captured 路径，并可独立完成仅剩
    // 隔离父目录的阶段。
    record.snapshot_present = false;
    match fs::remove_dir(quarantine_root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove managed snapshot quarantine {}: {error}",
            render_host_visible_path(quarantine_root)
        )),
    }
}

/// Remove one verified snapshot tree through a platform object handle rather than a mutable path.
/// 通过平台对象句柄而不是可变路径删除一个已验证快照树。
fn remove_verified_managed_snapshot_tree(
    snapshot_root: &Path,
    expected_identity: &ManagedPackageSnapshotIdentity,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let path = CString::new(snapshot_root.as_os_str().as_bytes()).map_err(|_| {
            format!(
                "managed snapshot path contains an interior NUL: {}",
                render_host_visible_path(snapshot_root)
            )
        })?;
        let raw_fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw_fd < 0 {
            return Err(format!(
                "failed to open isolated managed snapshot {}: {}",
                render_host_visible_path(snapshot_root),
                std::io::Error::last_os_error()
            ));
        }
        let directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(directory.as_raw_fd(), &mut stat) } != 0 {
            return Err(format!(
                "failed to inspect isolated managed snapshot handle {}: {}",
                render_host_visible_path(snapshot_root),
                std::io::Error::last_os_error()
            ));
        }
        let (device, inode) = unix_stat_device_inode(&stat)?;
        let actual_identity = ManagedPackageSnapshotIdentity { device, inode };
        if &actual_identity != expected_identity {
            return Err(format!(
                "isolated managed snapshot handle identity changed: {}",
                render_host_visible_path(snapshot_root)
            ));
        }
        remove_unix_snapshot_directory_contents(directory.as_raw_fd(), snapshot_root)?;
        drop(directory);
        // Only remove the now-empty name. A post-handle path replacement with live content fails
        // safely instead of recursively deleting the replacement object.
        // 仅移除当前已为空的名称。句柄释放后的路径若被活动内容替换，会安全失败而不会递归删除替换对象。
        fs::remove_dir(snapshot_root).map_err(|error| {
            format!(
                "failed to remove empty isolated managed snapshot {}: {error}",
                render_host_visible_path(snapshot_root)
            )
        })
    }
    #[cfg(windows)]
    {
        // Omitting FILE_SHARE_DELETE prevents root rename/replacement while contents are removed.
        // 不共享 FILE_SHARE_DELETE 可在删除内容期间阻止根目录重命名或替换。
        let directory = open_windows_snapshot_directory(snapshot_root, false)?;
        let actual_identity = windows_snapshot_identity_from_handle(&directory, snapshot_root)?;
        if &actual_identity != expected_identity {
            return Err(format!(
                "isolated managed snapshot handle identity changed: {}",
                render_host_visible_path(snapshot_root)
            ));
        }
        remove_windows_snapshot_directory_contents(snapshot_root)?;
        drop(directory);
        fs::remove_dir(snapshot_root).map_err(|error| {
            format!(
                "failed to remove empty isolated managed snapshot {}: {error}",
                render_host_visible_path(snapshot_root)
            )
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = expected_identity;
        Err(format!(
            "handle-anchored managed snapshot deletion is unsupported on this platform: {}",
            render_host_visible_path(snapshot_root)
        ))
    }
}

/// Recursively unlink one Unix snapshot directory through its already verified directory fd.
/// 通过已验证目录 fd 递归 unlink 一个 Unix 快照目录。
#[cfg(unix)]
fn remove_unix_snapshot_directory_contents(
    directory_fd: RawFd,
    display_path: &Path,
) -> Result<(), String> {
    let duplicate_fd = unsafe { libc::dup(directory_fd) };
    if duplicate_fd < 0 {
        return Err(format!(
            "failed to duplicate managed snapshot directory handle {}: {}",
            render_host_visible_path(display_path),
            std::io::Error::last_os_error()
        ));
    }
    let directory_stream = unsafe { libc::fdopendir(duplicate_fd) };
    if directory_stream.is_null() {
        unsafe { libc::close(duplicate_fd) };
        return Err(format!(
            "failed to enumerate managed snapshot directory handle {}: {}",
            render_host_visible_path(display_path),
            std::io::Error::last_os_error()
        ));
    }
    let mut names = Vec::<CString>::new();
    loop {
        let entry = unsafe { libc::readdir(directory_stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        names.push(CString::new(name.to_bytes()).map_err(|_| {
            format!(
                "managed snapshot entry contains an interior NUL under {}",
                render_host_visible_path(display_path)
            )
        })?);
    }
    unsafe { libc::closedir(directory_stream) };

    for name in names {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                directory_fd,
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(format!(
                "failed to inspect managed snapshot entry under {}: {}",
                render_host_visible_path(display_path),
                std::io::Error::last_os_error()
            ));
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            let child_fd = unsafe {
                libc::openat(
                    directory_fd,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if child_fd < 0 {
                return Err(format!(
                    "failed to open managed snapshot child directory under {}: {}",
                    render_host_visible_path(display_path),
                    std::io::Error::last_os_error()
                ));
            }
            let child = unsafe { OwnedFd::from_raw_fd(child_fd) };
            remove_unix_snapshot_directory_contents(child.as_raw_fd(), display_path)?;
            drop(child);
            if unsafe { libc::unlinkat(directory_fd, name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                return Err(format!(
                    "failed to remove managed snapshot child directory under {}: {}",
                    render_host_visible_path(display_path),
                    std::io::Error::last_os_error()
                ));
            }
        } else if unsafe { libc::unlinkat(directory_fd, name.as_ptr(), 0) } != 0 {
            return Err(format!(
                "failed to remove managed snapshot entry under {}: {}",
                render_host_visible_path(display_path),
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

/// Remove Windows snapshot contents while a non-delete-sharing handle pins the root object.
/// 在不共享删除的句柄固定根对象期间删除 Windows 快照内容。
#[cfg(windows)]
fn remove_windows_snapshot_directory_contents(directory: &Path) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to enumerate pinned managed snapshot {}: {error}",
            render_host_visible_path(directory)
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read pinned managed snapshot entry under {}: {error}",
                render_host_visible_path(directory)
            )
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "failed to inspect pinned managed snapshot entry {}: {error}",
                render_host_visible_path(&path)
            )
        })?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            clear_windows_readonly_attribute(&path)?;
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "failed to remove pinned managed snapshot file {}: {error}",
                    render_host_visible_path(&path)
                )
            })?;
        } else if metadata.is_dir() {
            remove_windows_snapshot_directory_contents(&path)?;
            fs::remove_dir(&path).map_err(|error| {
                format!(
                    "failed to remove pinned managed snapshot directory {}: {error}",
                    render_host_visible_path(&path)
                )
            })?;
        } else {
            return Err(format!(
                "refusing to remove unsupported managed snapshot entry: {}",
                render_host_visible_path(&path)
            ));
        }
    }
    Ok(())
}

/// Open one Windows snapshot directory, optionally allowing later delete-sharing operations.
/// 打开一个 Windows 快照目录，并可选择是否允许后续共享删除操作。
#[cfg(windows)]
fn open_windows_snapshot_directory(path: &Path, share_delete: bool) -> Result<OwnedHandle, String> {
    let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide_path.push(0);
    let share_mode =
        FILE_SHARE_READ | FILE_SHARE_WRITE | if share_delete { FILE_SHARE_DELETE } else { 0 };
    let desired_access = FILE_READ_ATTRIBUTES
        | if share_delete {
            0
        } else {
            WINDOWS_DELETE_ACCESS
        };
    let raw_handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            desired_access,
            share_mode,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw_handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed to open managed package snapshot identity {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(raw_handle as _) })
}

/// Read one stable Windows filesystem identity from an already opened directory handle.
/// 从一个已打开目录句柄读取稳定的 Windows 文件系统身份。
#[cfg(windows)]
fn windows_snapshot_identity_from_handle(
    handle: &OwnedHandle,
    path: &Path,
) -> Result<ManagedPackageSnapshotIdentity, String> {
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let status =
        unsafe { GetFileInformationByHandle(handle.as_raw_handle() as _, &mut information) };
    if status == 0 {
        return Err(format!(
            "failed to read managed package snapshot identity {}: {}",
            render_host_visible_path(path),
            std::io::Error::last_os_error()
        ));
    }
    let volume_serial_number = information.dwVolumeSerialNumber;
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    if volume_serial_number == 0 && file_index == 0 {
        return Err(format!(
            "managed package snapshot has no Windows filesystem identity: {}",
            render_host_visible_path(path)
        ));
    }
    Ok(ManagedPackageSnapshotIdentity {
        volume_serial_number,
        file_index,
    })
}

/// Derive a stable platform-native identity from one verified snapshot directory metadata value.
/// 从一个已验证快照目录的元数据值派生稳定的平台原生身份。
fn managed_package_snapshot_identity(
    metadata: &fs::Metadata,
    path: &Path,
) -> Result<ManagedPackageSnapshotIdentity, String> {
    #[cfg(unix)]
    {
        let _ = path;
        Ok(ManagedPackageSnapshotIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        let handle = open_windows_snapshot_directory(path, true)?;
        windows_snapshot_identity_from_handle(&handle, path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Err(format!(
            "managed package snapshot identity is unsupported on this platform: {}",
            render_host_visible_path(path)
        ))
    }
}

/// Require one managed executable to be a real regular file.
/// 要求单个受管可执行文件是真实普通文件。
fn ensure_regular_file(path: &Path, label: &str) -> Result<(), String> {
    // Metadata follows links because managed runtime manifests already constrain installation roots.
    // 元数据会跟随链接，因为受管运行时清单已经限制安装根目录。
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "failed to inspect {label} {}: {error}",
            render_host_visible_path(path)
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "{label} is not a file: {}",
            render_host_visible_path(path)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one initial cleanup record for a freshly created canonical snapshot path.
    /// 为一个新建的规范快照路径构造初始清理记录。
    ///
    /// `snapshots_root` and `snapshot_root` must already be canonical; `identity` identifies the
    /// originally created directory object. Returns a record before quarantine transition.
    /// `snapshots_root` 与 `snapshot_root` 必须已规范化；`identity` 标识原始创建的目录对象。
    /// 返回尚未进入隔离转换的记录。
    fn snapshot_cleanup_test_record(
        snapshots_root: PathBuf,
        snapshot_root: PathBuf,
        identity: ManagedPackageSnapshotIdentity,
    ) -> ManagedSnapshotCleanupRecord {
        ManagedSnapshotCleanupRecord {
            _environment_lease: None,
            snapshots_root,
            snapshot_root,
            quarantine_root: None,
            orphan_quarantine_root: None,
            snapshot_present: true,
            identity: Some(identity),
            error_reported: false,
        }
    }

    /// Create one unique filesystem root for snapshot identity tests.
    /// 为快照身份测试创建一个唯一文件系统根目录。
    fn snapshot_identity_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "luaskills-snapshot-identity-{label}-{}-{}",
            std::process::id(),
            NEXT_MANAGED_PACKAGE_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Return whether one test tree still contains a regular file with the requested name.
    /// 返回一个测试目录树是否仍包含指定名称的普通文件。
    fn snapshot_test_tree_contains_file(root: &Path, file_name: &str) -> bool {
        let Ok(entries) = fs::read_dir(root) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_file() && entry.file_name() == file_name {
                return true;
            }
            if metadata.is_dir() && snapshot_test_tree_contains_file(&path, file_name) {
                return true;
            }
        }
        false
    }

    /// A synchronous cleanup panic must transfer the exact snapshot record to persistent retry.
    /// 同步清理 panic 必须把精确快照记录移交给持久重试。
    #[test]
    fn managed_snapshot_cleanup_panic_is_retried_without_losing_ownership() {
        // Root and Snapshot create one identity-bound directory that the retry worker must delete.
        // Root 与 Snapshot 创建一个必须由重试 Worker 删除的身份绑定目录。
        let root = snapshot_identity_test_root("cleanup-panic");
        let snapshot = root.join("snapshot");
        fs::create_dir_all(&snapshot).expect("create cleanup panic snapshot");
        fs::write(snapshot.join("owned.txt"), b"owned")
            .expect("write cleanup panic snapshot evidence");
        let canonical_root = fs::canonicalize(&root).expect("canonicalize cleanup panic root");
        let canonical_snapshot =
            fs::canonicalize(&snapshot).expect("canonicalize cleanup panic snapshot");
        let identity = managed_package_snapshot_identity(
            &fs::symlink_metadata(&canonical_snapshot).expect("inspect cleanup panic snapshot"),
            &canonical_snapshot,
        )
        .expect("capture cleanup panic snapshot identity");
        let permit =
            reserve_managed_snapshot_cleanup_slot().expect("reserve cleanup panic retry capacity");
        let ownership = ManagedSnapshotCleanupOwnership {
            record: snapshot_cleanup_test_record(
                canonical_root,
                canonical_snapshot.clone(),
                identity,
            ),
            permit,
        };

        cleanup_or_handoff_managed_snapshot_with(Some(ownership), |_record| {
            panic!("forced managed snapshot cleanup panic");
        });

        // Deadline bounds persistent cleanup while tolerating full-suite scheduling pressure.
        // Deadline 限制持久清理等待时间，同时容忍全量测试调度压力。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while canonical_snapshot.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !canonical_snapshot.exists(),
            "persistent retry must remove the snapshot retained across panic"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Replacing a snapshot path with another directory object must never delete the replacement.
    /// 使用另一个目录对象替换快照路径后绝不能删除替换对象。
    #[test]
    fn managed_snapshot_cleanup_rejects_directory_identity_replacement() {
        let root = snapshot_identity_test_root("directory");
        let snapshot = root.join("snapshot");
        let original = root.join("original");
        fs::create_dir_all(&snapshot).expect("create original snapshot directory");
        let canonical_root = fs::canonicalize(&root).expect("canonicalize snapshot test root");
        let canonical_snapshot =
            fs::canonicalize(&snapshot).expect("canonicalize original snapshot");
        let identity = managed_package_snapshot_identity(
            &fs::symlink_metadata(&snapshot).expect("inspect original snapshot"),
            &snapshot,
        )
        .expect("capture original snapshot identity");
        fs::rename(&snapshot, &original).expect("move original snapshot object");
        fs::create_dir(&snapshot).expect("create replacement snapshot directory");
        fs::write(snapshot.join("replacement.txt"), b"replacement")
            .expect("write replacement marker");

        let mut record = snapshot_cleanup_test_record(canonical_root, canonical_snapshot, identity);
        let error = remove_managed_package_snapshot(&mut record)
            .expect_err("replacement directory identity must be rejected");
        assert!(error.contains("filesystem identity changed"));
        assert!(record.quarantine_root.is_some());
        assert_eq!(
            record
                .snapshot_root
                .file_name()
                .and_then(|name| name.to_str()),
            Some("captured")
        );
        assert!(snapshot_test_tree_contains_file(&root, "replacement.txt"));
        fs::remove_dir_all(&root).expect("remove snapshot identity test root");
    }

    /// A Windows cleanup handle must prevent snapshot-root rename throughout content deletion.
    /// Windows 清理句柄必须在内容删除期间阻止快照根目录重命名。
    #[cfg(windows)]
    #[test]
    fn windows_snapshot_cleanup_handle_prevents_root_replacement() {
        let root = snapshot_identity_test_root("windows-handle");
        let snapshot = root.join("snapshot");
        let replacement = root.join("replacement");
        fs::create_dir_all(&snapshot).expect("create pinned snapshot directory");
        let handle = open_windows_snapshot_directory(&snapshot, false)
            .expect("open non-delete-sharing snapshot handle");
        let rename_error = fs::rename(&snapshot, &replacement)
            .expect_err("pinned Windows snapshot root must reject rename");
        assert!(
            snapshot.is_dir(),
            "pinned snapshot root must remain in place"
        );
        assert!(!replacement.exists());
        drop(rename_error);
        drop(handle);
        fs::remove_dir_all(&root).expect("remove Windows handle test root");
    }

    /// A Windows cwd authorization path must come from the retained non-replaceable handle.
    /// Windows cwd 授权路径必须来自所保留且不可替换的句柄。
    #[cfg(windows)]
    #[test]
    fn windows_pinned_session_cwd_uses_handle_authoritative_path() {
        // Unique parent isolates both path identity and replacement attempts.
        // 唯一父目录隔离路径身份与替换尝试。
        let root = snapshot_identity_test_root("windows-cwd-pin");
        // Exact directory whose handle must remain authoritative through spawn.
        // 其句柄必须在启动期间保持权威的精确目录。
        let cwd = root.join("cwd");
        // Alternate name used to prove the pin denies path replacement.
        // 用于证明固定器拒绝路径替换的备用名称。
        let replacement = root.join("replacement");
        fs::create_dir_all(&cwd).expect("create pinned Windows session cwd");
        // Initial resolved name is used only to acquire the object handle.
        // 初始解析名称仅用于获取对象句柄。
        let resolved = fs::canonicalize(&cwd).expect("canonicalize Windows session cwd");
        // Platform object pin retained by the managed-session launch transaction.
        // 由受管会话启动事务保留的平台对象固定器。
        let pin = pin_managed_session_cwd(&resolved).expect("pin Windows session cwd");
        // Final authorization path derived from that same retained handle.
        // 从同一保留句柄派生的最终授权路径。
        let authoritative = authoritative_managed_session_cwd_path(&pin, &resolved)
            .expect("derive authoritative Windows session cwd");

        assert_eq!(authoritative, resolved);
        fs::rename(&cwd, &replacement)
            .expect_err("pinned Windows session cwd must reject replacement");
        assert!(cwd.is_dir());
        assert!(!replacement.exists());

        drop(pin);
        fs::remove_dir_all(&root).expect("remove Windows cwd-pin test root");
    }

    /// Windows Node path conversion must reject verbatim namespaces without DOS equivalents.
    /// Windows Node 路径转换必须拒绝不存在 DOS 等价形式的 verbatim 命名空间。
    #[cfg(windows)]
    #[test]
    fn windows_node_path_rejects_non_dos_verbatim_namespace() {
        // Volume GUID namespace is absolute only while its verbatim prefix remains present.
        // 卷 GUID 命名空间仅在保留 verbatim 前缀时才是绝对路径。
        let volume_path = Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\entry.mjs");
        let error = windows_non_verbatim_path(volume_path)
            .expect_err("volume GUID path must not become a relative Node source");
        assert!(error.contains("unsupported Windows verbatim namespace"));
    }

    /// Windows worker cwd derivation must normalize every supported drive and UNC spelling.
    /// Windows Worker cwd 派生必须规范化每一种受支持的驱动器与 UNC 写法。
    #[cfg(windows)]
    #[test]
    fn windows_worker_neutral_cwd_derives_drive_and_share_roots() {
        assert_eq!(
            windows_worker_neutral_cwd_path(Path::new(r"C:\deep\snapshot"))
                .expect("derive DOS drive root"),
            PathBuf::from(r"C:\")
        );
        assert_eq!(
            windows_worker_neutral_cwd_path(Path::new(r"\\?\C:\deep\snapshot"))
                .expect("derive verbatim DOS drive root"),
            PathBuf::from(r"C:\")
        );
        assert_eq!(
            windows_worker_neutral_cwd_path(Path::new(r"\\server\share\deep\snapshot"))
                .expect("derive UNC share root"),
            PathBuf::from(r"\\server\share\")
        );
        assert_eq!(
            windows_worker_neutral_cwd_path(Path::new(r"\\?\UNC\server\share\deep\snapshot",))
                .expect("derive verbatim UNC share root"),
            PathBuf::from(r"\\server\share\")
        );
    }

    /// Windows worker cwd derivation must reject namespaces without a safe short root.
    /// Windows Worker cwd 派生必须拒绝没有安全短根的命名空间。
    #[cfg(windows)]
    #[test]
    fn windows_worker_neutral_cwd_rejects_unsupported_or_oversized_roots() {
        let volume_error = windows_worker_neutral_cwd_path(Path::new(
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\snapshot",
        ))
        .expect_err("Volume GUID cwd must be rejected");
        assert!(volume_error.contains("unsupported Windows namespace"));

        let oversized_server = "s".repeat(250);
        let oversized_path = PathBuf::from(format!(r"\\{oversized_server}\share\snapshot"));
        let length_error = windows_worker_neutral_cwd_path(&oversized_path)
            .expect_err("oversized UNC root must be rejected");
        assert!(length_error.contains("reaches MAX_PATH"));
    }

    /// Windows execution pins must deny snapshot mutation and replacement until released.
    /// Windows 执行固定器必须在释放前拒绝快照修改与替换。
    #[cfg(windows)]
    #[test]
    fn windows_snapshot_execution_pin_denies_mutation_and_replacement() {
        let root = snapshot_identity_test_root("windows-execution-pin");
        let snapshot = root.join("snapshot");
        let replacement = root.join("replacement");
        let source = snapshot.join("runtime.js");
        fs::create_dir_all(&snapshot).expect("create execution snapshot directory");
        fs::write(&source, "console.log('pinned')").expect("write execution snapshot source");
        let pin = pin_managed_snapshot_execution_tree(&snapshot)
            .expect("pin complete Windows execution snapshot");

        fs::write(&source, "console.log('replaced')")
            .expect_err("pinned snapshot source must reject writes");
        fs::rename(&snapshot, &replacement)
            .expect_err("pinned snapshot root must reject replacement");

        drop(pin);
        fs::remove_dir_all(&root).expect("remove Windows execution-pin test root");
    }

    /// Unix worker source validation must follow only regular files beneath the pinned root.
    /// Unix Worker 源码校验必须只跟随固定根下的常规文件。
    #[cfg(unix)]
    #[test]
    fn unix_worker_source_validation_is_fd_relative_and_symlink_safe() {
        let root = snapshot_identity_test_root("unix-worker-source");
        let runtime_dir = root.join("runtime");
        let source = runtime_dir.join("entry.py");
        let outside = root.join("outside.py");
        fs::create_dir_all(&runtime_dir).expect("create Unix worker source directory");
        fs::write(&source, b"print('ok')\n").expect("write Unix worker source");
        fs::write(&outside, b"print('outside')\n").expect("write outside Unix worker source");
        let encoded_root = CString::new(root.as_os_str().as_bytes()).expect("encode Unix root");
        let raw_fd = unsafe {
            libc::open(
                encoded_root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        assert!(raw_fd >= 0, "open Unix worker source root");
        let root_directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        validate_unix_snapshot_regular_file(
            root_directory.as_raw_fd(),
            Path::new("runtime/entry.py"),
            &root,
        )
        .expect("validate fd-relative Unix worker source");

        let redirected = runtime_dir.join("redirected.py");
        std::os::unix::fs::symlink(&outside, &redirected)
            .expect("create redirected Unix worker source");
        let error = validate_unix_snapshot_regular_file(
            root_directory.as_raw_fd(),
            Path::new("runtime/redirected.py"),
            &root,
        )
        .expect_err("Unix worker source symlink must be rejected");
        assert!(error.contains("failed to open managed worker snapshot source"));
        let outside_directory = root.join("outside-directory");
        fs::create_dir_all(&outside_directory).expect("create outside Unix source directory");
        fs::write(
            outside_directory.join("entry.py"),
            b"print('outside directory')\n",
        )
        .expect("write outside directory source");
        std::os::unix::fs::symlink(&outside_directory, root.join("redirected-directory"))
            .expect("create redirected Unix source directory");
        let intermediate_error = validate_unix_snapshot_regular_file(
            root_directory.as_raw_fd(),
            Path::new("redirected-directory/entry.py"),
            &root,
        )
        .expect_err("Unix worker source intermediate symlink must be rejected");
        assert!(intermediate_error.contains("failed to open managed worker snapshot source"));
        fs::remove_dir_all(&root).expect("remove Unix worker source validation root");
    }

    /// Replacing a snapshot path with a symlink must never redirect cleanup to its target.
    /// 使用符号链接替换快照路径后绝不能把清理重定向到其目标。
    #[cfg(unix)]
    #[test]
    fn managed_snapshot_cleanup_rejects_symlink_redirection() {
        let root = snapshot_identity_test_root("symlink");
        let snapshot = root.join("snapshot");
        let original = root.join("original");
        let sibling = root.join("sibling");
        fs::create_dir_all(&snapshot).expect("create original snapshot directory");
        fs::create_dir_all(&sibling).expect("create sibling snapshot directory");
        fs::write(sibling.join("active.txt"), b"active").expect("write sibling marker");
        let canonical_root = fs::canonicalize(&root).expect("canonicalize snapshot test root");
        let canonical_snapshot =
            fs::canonicalize(&snapshot).expect("canonicalize original snapshot");
        let identity = managed_package_snapshot_identity(
            &fs::symlink_metadata(&snapshot).expect("inspect original snapshot"),
            &snapshot,
        )
        .expect("capture original snapshot identity");
        fs::rename(&snapshot, &original).expect("move original snapshot object");
        std::os::unix::fs::symlink(&sibling, &snapshot).expect("redirect snapshot symlink");

        let mut record = snapshot_cleanup_test_record(canonical_root, canonical_snapshot, identity);
        let error = remove_managed_package_snapshot(&mut record)
            .expect_err("snapshot symlink redirection must be rejected");
        assert!(error.contains("replaced managed package snapshot object"));
        assert!(sibling.join("active.txt").is_file());
        fs::remove_dir_all(&root).expect("remove snapshot symlink test root");
    }

    /// Unix fd-relative deletion must stay on the opened object after its path is replaced.
    /// Unix fd 相对删除必须在路径被替换后仍锚定已打开对象。
    #[cfg(unix)]
    #[test]
    fn unix_snapshot_cleanup_stays_anchored_after_root_replacement() {
        let root = snapshot_identity_test_root("unix-handle");
        let snapshot = root.join("snapshot");
        let moved = root.join("moved");
        fs::create_dir_all(&snapshot).expect("create Unix anchored snapshot");
        fs::write(snapshot.join("owned.txt"), b"owned").expect("write owned snapshot file");
        let path = CString::new(snapshot.as_os_str().as_bytes()).expect("encode snapshot path");
        let raw_fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        assert!(raw_fd >= 0, "open Unix snapshot directory fd");
        let directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        fs::rename(&snapshot, &moved).expect("move opened Unix snapshot");
        fs::create_dir(&snapshot).expect("create replacement Unix snapshot path");
        fs::write(snapshot.join("replacement.txt"), b"replacement")
            .expect("write replacement Unix snapshot marker");

        remove_unix_snapshot_directory_contents(directory.as_raw_fd(), &snapshot)
            .expect("remove contents through anchored Unix fd");
        assert!(!moved.join("owned.txt").exists());
        assert!(snapshot.join("replacement.txt").is_file());
        drop(directory);
        fs::remove_dir_all(&root).expect("remove Unix anchored snapshot test root");
    }

    /// macOS object-derived execution paths must follow the pinned root after name replacement.
    /// macOS 对象派生执行路径必须在名称替换后继续跟随固定根对象。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_execution_path_follows_pinned_root_object() {
        let root = snapshot_identity_test_root("macos-execution-object");
        let snapshot = root.join("snapshot");
        let moved = root.join("moved");
        fs::create_dir_all(snapshot.join("runtime")).expect("create macOS snapshot source tree");
        fs::write(snapshot.join("runtime/entry.py"), b"trusted\n")
            .expect("write trusted macOS snapshot source");
        let encoded_snapshot =
            path_to_c_string(&snapshot, "macOS snapshot test root").expect("encode snapshot root");
        let raw_fd = unsafe {
            libc::open(
                encoded_snapshot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        assert!(raw_fd >= 0, "open macOS snapshot root");
        let directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        fs::rename(&snapshot, &moved).expect("move pinned macOS snapshot root");
        let canonical_moved = fs::canonicalize(&moved).expect("canonicalize moved macOS snapshot");
        fs::create_dir_all(snapshot.join("runtime")).expect("create replacement snapshot root");
        fs::write(snapshot.join("runtime/entry.py"), b"replacement\n")
            .expect("write replacement macOS source");

        let execution_path = macos_snapshot_execution_source_path(
            directory.as_raw_fd(),
            Path::new("runtime/entry.py"),
            &snapshot,
        )
        .expect("derive path from pinned macOS root object");

        assert_eq!(
            fs::read(&execution_path).expect("read object-derived macOS source"),
            b"trusted\n"
        );
        assert!(execution_path.starts_with(&canonical_moved));
        fs::remove_dir_all(&root).expect("remove macOS execution object test root");
    }

    /// macOS pre-exec source validation must reject a same-name file replacement.
    /// macOS 执行前源码校验必须拒绝同名文件替换。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_execution_source_revalidation_rejects_replacement() {
        let root = snapshot_identity_test_root("macos-source-replacement");
        let snapshot = root.join("snapshot");
        let source = snapshot.join("entry.py");
        let moved_source = snapshot.join("entry-original.py");
        fs::create_dir_all(&snapshot).expect("create macOS source replacement root");
        fs::write(&source, b"trusted\n").expect("write original macOS source");
        let encoded_snapshot =
            path_to_c_string(&snapshot, "macOS source test root").expect("encode snapshot root");
        let raw_fd = unsafe {
            libc::open(
                encoded_snapshot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        assert!(raw_fd >= 0, "open macOS source replacement root");
        let directory = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let expected = macos_file_identity_from_snapshot(
            directory.as_raw_fd(),
            Path::new("entry.py"),
            &snapshot,
        )
        .expect("capture original macOS source identity");
        fs::rename(&source, &moved_source).expect("move original macOS source");
        fs::write(&source, b"replacement\n").expect("write replacement macOS source");
        let encoded_source =
            path_to_c_string(&source, "macOS named source").expect("encode named source");

        let error = macos_validate_named_execution_source(&encoded_source, expected)
            .expect_err("replacement macOS source must fail identity validation");

        assert!(error.to_string().contains("identity changed"));
        fs::remove_dir_all(&root).expect("remove macOS source replacement test root");
    }
}
