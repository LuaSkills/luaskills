use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataMethods, Value as LuaValue};

use crate::runtime::encoding::{RuntimeTextEncoding, decode_runtime_text, encode_runtime_text};
use crate::runtime::path::normalize_host_input_path_text;

#[cfg(unix)]
use libc::{ESRCH, SIGKILL};
#[cfg(windows)]
use std::mem::size_of;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED, ERROR_NO_MORE_FILES, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, GetCurrentProcess,
    GetExitCodeProcess, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

const DEFAULT_SESSION_READ_TIMEOUT_MS: u64 = 100;
const DEFAULT_SESSION_CLOSE_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_SESSION_MAX_READ_BYTES: usize = 64 * 1024;
const DEFAULT_SESSION_BUFFER_LIMIT_BYTES: usize = 1024 * 1024;
/// Largest portable finite timeout, one millisecond below the Windows infinite-wait sentinel.
/// 最大可移植有限超时，比 Windows 无限等待哨兵少一毫秒。
const MAX_SESSION_TIMEOUT_MS: u64 = u32::MAX as u64 - 1;
/// Maximum time one lifecycle call may spend reaping a directly owned child after forced kill.
/// 单次生命周期调用在强制终止后回收直接拥有子进程的最长时间。
const FORCED_CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll interval used by bounded direct-child reaping.
/// 有界直接子进程回收使用的轮询间隔。
const FORCED_CHILD_REAP_POLL: Duration = Duration::from_millis(10);
/// Maximum number of killed attach-failure children retained by the global asynchronous reaper.
/// 全局异步回收器最多保留的已终止附加失败子进程数量。
const DETACHED_CHILD_REAPER_CAPACITY: usize = 256;

/// Lazily initialized bounded reaper shared by rare attach-failure timeout paths.
/// 由少见附加失败超时路径共享的延迟初始化有界回收器。
static DETACHED_CHILD_REAPER: OnceLock<Result<DetachedChildReaper, String>> = OnceLock::new();
/// Maximum time allowed for Windows descendant attachment or termination convergence.
/// Windows 后代归属或终止收敛允许的最长时间。
#[cfg(windows)]
const WINDOWS_PROCESS_TREE_CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay separating Windows convergence snapshots so late pre-assignment children become visible.
/// 分隔 Windows 收敛快照的延迟，使较晚出现的归属前子进程可见。
#[cfg(windows)]
const WINDOWS_PROCESS_TREE_CONVERGENCE_POLL: Duration = Duration::from_millis(10);

/// Lightweight process status snapshot used by non-reaping status probes.
/// 由非 reap 状态探测返回的轻量进程状态快照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessStatusSnapshot {
    /// Whether the process is still running.
    /// 进程是否仍在运行。
    pub(crate) running: bool,
    /// Whether the process has exited.
    /// 进程是否已经退出。
    pub(crate) exited: bool,
    /// Optional success flag when the exit result is known.
    /// 退出结果已知时的可选成功标记。
    pub(crate) success: Option<bool>,
    /// Optional numeric exit code when the platform exposes one.
    /// 平台可提供数值退出码时的可选退出码。
    pub(crate) code: Option<i32>,
}

/// Cumulative diagnostics for one bounded managed-process output stream.
/// 单个有界托管进程输出流的累计诊断信息。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ManagedProcessOutputStats {
    /// Bytes currently retained and available for a future read.
    /// 当前保留并可供后续读取的字节数。
    pub(crate) buffered_bytes: usize,
    /// Total bytes received from the child process since launch.
    /// 自进程启动以来从子进程累计接收的字节数。
    pub(crate) total_bytes: u64,
    /// Total oldest bytes discarded because the configured buffer limit was exceeded.
    /// 因超过已配置缓冲上限而累计丢弃的最旧字节数。
    pub(crate) dropped_bytes: u64,
}

/// Complete status returned by the pure Rust managed-process session core.
/// 纯 Rust 托管进程会话核心返回的完整状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedProcessSessionStatus {
    /// Direct-child lifecycle snapshot.
    /// 直接子进程生命周期快照。
    pub(crate) process: ProcessStatusSnapshot,
    /// Current stdout buffer diagnostics.
    /// 当前 stdout 缓冲诊断信息。
    pub(crate) stdout: ManagedProcessOutputStats,
    /// Current stderr buffer diagnostics.
    /// 当前 stderr 缓冲诊断信息。
    pub(crate) stderr: ManagedProcessOutputStats,
    /// Whether close or kill has been requested for this session.
    /// 当前会话是否已经请求 close 或 kill。
    pub(crate) closed: bool,
}

/// Launch options consumed by the pure Rust managed-process session core.
/// 纯 Rust 托管进程会话核心消费的启动选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedProcessSessionLaunchOptions {
    /// Encoding used for stdout decoding.
    /// stdout 解码使用的编码。
    pub(crate) stdout_encoding: RuntimeTextEncoding,
    /// Encoding used for stderr decoding.
    /// stderr 解码使用的编码。
    pub(crate) stderr_encoding: RuntimeTextEncoding,
    /// Encoding used for stdin writes.
    /// stdin 写入使用的编码。
    pub(crate) stdin_encoding: RuntimeTextEncoding,
    /// Maximum retained bytes per output stream.
    /// 每个输出流最多保留的字节数。
    pub(crate) buffer_limit_bytes: usize,
}

/// Parsed process session creation request.
/// 解析后的进程会话创建请求。
struct ProcessSessionOpenRequest {
    /// Program executable to spawn.
    /// 需要启动的程序可执行文件。
    program: String,
    /// Program argument list passed without shell interpolation.
    /// 不经过 shell 插值直接传递的程序参数列表。
    args: Vec<String>,
    /// Optional process working directory.
    /// 可选的进程工作目录。
    cwd: Option<String>,
    /// Encoding used for stdout decoding.
    /// stdout 解码使用的编码。
    stdout_encoding: RuntimeTextEncoding,
    /// Encoding used for stderr decoding.
    /// stderr 解码使用的编码。
    stderr_encoding: RuntimeTextEncoding,
    /// Encoding used for stdin writes.
    /// stdin 写入使用的编码。
    stdin_encoding: RuntimeTextEncoding,
    /// Maximum retained bytes per output stream.
    /// 每个输出流最多保留的字节数。
    buffer_limit_bytes: usize,
}

/// Read behavior requested by one process session read call.
/// 单次进程会话读取调用请求的读取行为。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedProcessSessionReadRequest {
    /// Maximum wait time before returning available data.
    /// 返回可用数据前最多等待的时间。
    pub(crate) timeout_ms: u64,
    /// Maximum number of bytes drained per stream.
    /// 每个输出流最多取出的字节数。
    pub(crate) max_bytes: usize,
    /// Optional text marker that stops the wait when observed.
    /// 可选的文本标记，观察到后停止等待。
    pub(crate) until_text: Option<String>,
}

/// Decoded output and diagnostics returned by one pure Rust session read.
/// 单次纯 Rust 会话读取返回的已解码输出与诊断信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedProcessSessionReadResult {
    /// Decoded stdout text.
    /// 已解码的 stdout 文本。
    pub(crate) stdout: String,
    /// Decoded stderr text.
    /// 已解码的 stderr 文本。
    pub(crate) stderr: String,
    /// Actual stdout encoding label used by decoding.
    /// stdout 解码实际使用的编码标签。
    pub(crate) stdout_encoding: String,
    /// Actual stderr encoding label used by decoding.
    /// stderr 解码实际使用的编码标签。
    pub(crate) stderr_encoding: String,
    /// Whether stdout decoding required replacement or fallback behavior.
    /// stdout 解码是否发生替换或兜底行为。
    pub(crate) stdout_lossy: bool,
    /// Whether stderr decoding required replacement or fallback behavior.
    /// stderr 解码是否发生替换或兜底行为。
    pub(crate) stderr_lossy: bool,
    /// Optional Base64 stdout payload for byte-preserving mode.
    /// 字节保真模式下可选的 Base64 stdout 载荷。
    pub(crate) stdout_base64: Option<String>,
    /// Optional Base64 stderr payload for byte-preserving mode.
    /// 字节保真模式下可选的 Base64 stderr 载荷。
    pub(crate) stderr_base64: Option<String>,
    /// Whether the requested wait elapsed without readable output or completed readers.
    /// 请求等待是否在没有可读输出或已完成读取器的情况下结束。
    pub(crate) timed_out: bool,
    /// Stdout diagnostics after this read drained its bytes.
    /// 本次读取取出字节后的 stdout 诊断信息。
    pub(crate) stdout_stats: ManagedProcessOutputStats,
    /// Stderr diagnostics after this read drained its bytes.
    /// 本次读取取出字节后的 stderr 诊断信息。
    pub(crate) stderr_stats: ManagedProcessOutputStats,
}

/// Close behavior requested by one process session close call.
/// 单次进程会话关闭调用请求的关闭行为。
struct ProcessSessionCloseRequest {
    /// Maximum graceful wait time before killing the process.
    /// 强制杀死进程前最多等待的优雅退出时间。
    timeout_ms: u64,
}

/// Bounded byte queue and cumulative counters for one output stream.
/// 单个输出流的有界字节队列与累计计数器。
#[derive(Debug, Default)]
struct ManagedProcessOutputBuffer {
    /// Retained bytes ordered from oldest to newest.
    /// 按从最旧到最新顺序保留的字节。
    bytes: VecDeque<u8>,
    /// Total bytes appended since the reader started.
    /// 自读取器启动以来累计追加的字节数。
    total_bytes: u64,
    /// Total bytes discarded from the oldest side after overflow.
    /// 溢出后从最旧一侧累计丢弃的字节数。
    dropped_bytes: u64,
}

/// Output stream whose reader produced one background notification.
/// 产生单个后台通知的输出流。
#[derive(Clone, Copy)]
enum ManagedProcessOutputStream {
    /// Standard output stream.
    /// 标准输出流。
    Stdout,
    /// Standard error stream.
    /// 标准错误流。
    Stderr,
}

/// Package-agnostic observer for managed-process background lifecycle events.
/// 托管进程后台生命周期事件的包无关观察器。
pub(crate) trait ManagedProcessSessionObserver: Send + Sync + 'static {
    /// Notify that stdout received bytes retained by the bounded buffer.
    /// 通知 stdout 已收到并保留到有界缓冲区的字节。
    fn stdout_readable(&self);

    /// Notify that stderr received bytes retained by the bounded buffer.
    /// 通知 stderr 已收到并保留到有界缓冲区的字节。
    fn stderr_readable(&self);

    /// Notify that the direct child reached a terminal state without reaping it.
    /// 通知直接子进程已进入终态，但尚未被回收。
    fn exited(&self);

    /// Notify that one background reader or exit probe failed.
    /// 通知某个后台读取器或退出探测发生失败。
    fn failed(&self);
}

/// Shared mutable state owned by one managed process session.
/// 单个托管进程会话拥有的共享可变状态。
struct ManagedProcessSessionState {
    /// Spawned child process protected for status and lifecycle calls.
    /// 受保护的子进程，用于状态与生命周期调用。
    child: Mutex<Option<Child>>,
    /// Pre-spawn reaper capacity retained until definitive reap or final ownership handoff.
    /// 从启动前保留到确定回收或最终所有权交接的回收器容量。
    reaper_permit: Mutex<Option<DetachedChildReaperPermit>>,
    /// Deferred managed-resource cleanup transferred only if final Drop hands off the child.
    /// 仅在最终析构移交子进程时一并转移的延迟受管资源清理。
    final_reaper_keepalive: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    /// Platform-specific process-tree controller used to kill descendants as one unit.
    /// 平台相关的进程树控制器，用于把派生进程作为一个整体回收。
    process_tree: ProcessTreeController,
    /// Optional stdin pipe used by write calls until closed.
    /// 写入调用使用的可选 stdin 管道，关闭后为空。
    stdin: Mutex<Option<ChildStdin>>,
    /// Accumulated stdout bytes drained by the background reader.
    /// 后台读取器排空并累计的 stdout 字节。
    stdout_buffer: Arc<Mutex<ManagedProcessOutputBuffer>>,
    /// Accumulated stderr bytes drained by the background reader.
    /// 后台读取器排空并累计的 stderr 字节。
    stderr_buffer: Arc<Mutex<ManagedProcessOutputBuffer>>,
    /// Encoding used for stdout reads.
    /// stdout 读取使用的编码。
    stdout_encoding: RuntimeTextEncoding,
    /// Encoding used for stderr reads.
    /// stderr 读取使用的编码。
    stderr_encoding: RuntimeTextEncoding,
    /// Encoding used for stdin writes.
    /// stdin 写入使用的编码。
    stdin_encoding: RuntimeTextEncoding,
    /// Background stdout reader thread joined during explicit close or implicit drop.
    /// 在显式关闭或隐式析构时需要等待退出的 stdout 后台读取线程。
    stdout_reader: Mutex<Option<SessionPipeReader>>,
    /// Background stderr reader thread joined during explicit close or implicit drop.
    /// 在显式关闭或隐式析构时需要等待退出的 stderr 后台读取线程。
    stderr_reader: Mutex<Option<SessionPipeReader>>,
    /// Whether a close or kill operation has been requested.
    /// 是否已经请求过关闭或杀死操作。
    closed: Mutex<bool>,
    /// Final process status cached after an explicit tree teardown reaps the direct child.
    /// 显式进程树清理并回收直接子进程后缓存的最终进程状态。
    final_status: Mutex<Option<ProcessStatusSnapshot>>,
    /// Whether the full process tree has already been terminated successfully.
    /// 是否已经成功终止完整进程树。
    process_tree_terminated: Mutex<bool>,
    /// Optional background event observer supplied by a managed runtime.
    /// 由受管运行时提供的可选后台事件观察器。
    observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
    /// Serialized notification gate closed before explicit or implicit teardown proceeds.
    /// 在显式或隐式清理继续前关闭的串行化通知门。
    observer_notifications_open: Arc<Mutex<bool>>,
}

/// Background pipe reader completion handle.
/// 后台管道读取器完成句柄。
struct SessionPipeReader {
    /// Reader thread joined when shutdown finishes promptly.
    /// 在关闭能及时完成时用于 join 的读取线程。
    handle: thread::JoinHandle<()>,
    /// One-shot completion signal emitted when the reader reaches EOF or exits on error.
    /// 读取器在 EOF 或错误退出时发出的单次完成信号。
    done_rx: mpsc::Receiver<()>,
    /// Shared completion flag used by read polling without consuming the join signal.
    /// 共享完成标记，供 read 轮询时使用且不会消耗 join 信号。
    done: Arc<AtomicBool>,
}

/// Background thread role created during one managed-process launch transaction.
/// 单次托管进程启动事务期间创建的后台线程角色。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedProcessBackgroundThread {
    /// Reader draining the direct child's stdout pipe.
    /// 排空直接子进程 stdout 管道的读取器。
    StdoutReader,
    /// Reader draining the direct child's stderr pipe.
    /// 排空直接子进程 stderr 管道的读取器。
    StderrReader,
    /// Observer-only direct-child exit watcher.
    /// 仅用于观察器的直接子进程退出监视器。
    ExitWatcher,
}

/// Heap-owned background task accepted by the injectable thread spawner.
/// 可注入线程启动器接收的堆所有权后台任务。
type ManagedProcessBackgroundTask = Box<dyn FnOnce() + Send + 'static>;

/// Fallible background thread factory used by production and deterministic launch tests.
/// 生产代码与确定性启动测试使用的可失败后台线程工厂。
type ManagedProcessBackgroundThreadSpawner = fn(
    ManagedProcessBackgroundThread,
    ManagedProcessBackgroundTask,
) -> Result<thread::JoinHandle<()>, std::io::Error>;

/// Platform-specific process-tree controller retained for one managed session.
/// 单个托管会话保留的平台相关进程树控制器。
struct ProcessTreeController {
    #[cfg(windows)]
    job: WindowsProcessJob,
    /// Pre-created Job handle reserved for asynchronous final descendant convergence.
    /// 为异步最终后代收敛预创建的 Job 句柄。
    detached_guard: Mutex<Option<DetachedProcessTreeGuard>>,
}

/// Process-tree ownership retained by the detached reaper after direct-child exit.
/// 直接子进程退出后由分离回收器保留的进程树所有权。
struct DetachedProcessTreeGuard {
    /// Duplicate Windows Job handle used for authoritative active-process accounting.
    /// 用于权威活动进程记账的 Windows Job 重复句柄。
    #[cfg(windows)]
    job_handle: OwnedHandle,
}

/// Process-wide bounded handoff for killed children that miss a synchronous reap deadline.
/// 进程级有界交接器，用于处理错过同步回收截止时间的已终止子进程。
struct DetachedChildReaper {
    /// Shared ownership queue that cannot disconnect or lose children if its worker fails.
    /// 即使工作线程失败也不会断线或丢失子进程的共享所有权队列。
    queue: Arc<DetachedChildReaperQueue>,
    /// Total queued or actively polled children reserved against the global capacity.
    /// 针对全局容量预留的已排队或正在轮询子进程总数。
    reserved: Arc<AtomicUsize>,
}

/// Shared bounded child ownership retained independently from the polling worker thread.
/// 独立于轮询工作线程保留的共享有界子进程所有权。
struct DetachedChildReaperQueue {
    /// Killed children retained until nonblocking polling proves they were reaped.
    /// 保留到非阻塞轮询证明已回收为止的已终止子进程。
    children: Mutex<Vec<DetachedChildReapRecord>>,
    /// Notification waking the worker after a pre-reserved child is handed off.
    /// 在预留子进程完成交接后唤醒工作线程的通知量。
    changed: std::sync::Condvar,
}

/// Capacity permit acquired before spawning a child that could require asynchronous reaping.
/// 在启动可能需要异步回收的子进程前获取的容量许可。
pub(crate) struct DetachedChildReaperPermit {
    /// Already initialized reaper guaranteed to accept this permit's child.
    /// 已初始化且保证接收当前许可对应子进程的回收器。
    reaper: &'static DetachedChildReaper,
    /// Whether dropping this permit must release its unused reservation.
    /// 丢弃当前许可时是否必须释放其未使用预留。
    reserved: bool,
}

impl DetachedChildReaperPermit {
    /// Transfer a child plus an optional runtime resource that must outlive that child.
    /// 移交子进程及必须比该子进程存活更久的可选运行时资源。
    ///
    /// `keepalive` is released only after nonblocking polling confirms definitive child reap.
    /// `keepalive` 仅在非阻塞轮询确认子进程已确定回收后释放。
    pub(crate) fn handoff_with_keepalive(
        self,
        child: Child,
        keepalive: Option<Box<dyn FnOnce() + Send>>,
    ) {
        self.handoff_with_resources(child, keepalive, None);
    }

    /// Transfer child, deferred resources, and process-tree convergence ownership atomically.
    /// 原子转移子进程、延迟资源与进程树收敛所有权。
    fn handoff_with_resources(
        mut self,
        child: Child,
        keepalive: Option<Box<dyn FnOnce() + Send>>,
        process_tree_guard: Option<DetachedProcessTreeGuard>,
    ) {
        self.reaper
            .queue
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(DetachedChildReapRecord {
                child,
                _keepalive: keepalive.map(|action| DetachedChildKeepalive {
                    action: Some(action),
                }),
                process_tree_guard,
                poll_error_reported: false,
            });
        self.reserved = false;
        self.reaper.queue.changed.notify_one();
    }
}

impl Drop for DetachedChildReaperPermit {
    /// Release an unused spawn-time reservation after definitive reap or spawn failure.
    /// 在确定回收或启动失败后释放未使用的启动期预留。
    fn drop(&mut self) {
        if self.reserved {
            self.reaper.reserved.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// One asynchronously reaped child with bounded one-time poll-error diagnostics.
/// 一个异步回收子进程，并带有有界的一次性轮询错误诊断。
struct DetachedChildReapRecord {
    /// Direct child retained until `try_wait` proves it exited and a final cached wait completes.
    /// 保留到 `try_wait` 证明其已退出且最终缓存 wait 完成的直接子进程。
    child: Child,
    /// Runtime resource retained until the direct child has been definitively reaped.
    /// 保留到直接子进程确定回收为止的运行时资源。
    _keepalive: Option<DetachedChildKeepalive>,
    /// Optional Windows Job guard retained until authoritative descendant count reaches zero.
    /// 保留到权威后代计数归零为止的可选 Windows Job 固定器。
    process_tree_guard: Option<DetachedProcessTreeGuard>,
    /// Whether one polling failure has already been logged for this child.
    /// 是否已为当前子进程记录过一次轮询失败。
    poll_error_reported: bool,
}

/// One deferred cleanup action executed only when its detached child record is released.
/// 仅在对应分离子进程记录释放时执行的延迟清理动作。
struct DetachedChildKeepalive {
    /// Exactly-once action retaining and then releasing child-dependent resources.
    /// 以恰好一次方式保留并最终释放子进程依赖资源的动作。
    action: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for DetachedChildKeepalive {
    /// Run the retained action after definitive reap removes the containing record.
    /// 在确定回收并移除所属记录后运行保留动作。
    fn drop(&mut self) {
        if let Some(action) = self.action.take() {
            action();
        }
    }
}

/// Combine multiple deferred resource actions into one detached-reaper keepalive.
/// 把多个延迟资源动作合并为一个分离回收器保活动作。
fn combine_detached_child_keepalives(
    actions: Vec<Box<dyn FnOnce() + Send>>,
) -> Option<Box<dyn FnOnce() + Send>> {
    if actions.is_empty() {
        None
    } else {
        Some(Box::new(move || {
            for action in actions {
                action();
            }
        }))
    }
}

/// Package-agnostic process-tree attachment used by managed runtime workers outside Lua userdata.
/// 供 Lua userdata 之外的受管运行时 Worker 使用的包无关进程树附件。
pub(crate) struct ManagedChildProcessTree {
    /// Platform controller retained for the complete child lifetime.
    /// 在完整子进程生命周期内保留的平台控制器。
    controller: ProcessTreeController,
}

impl ManagedChildProcessTree {
    /// Spawn with child-dependent resources retained across every partial attachment rollback.
    /// 启动进程，并让依赖子进程的资源跨越每个部分附加回滚继续保留。
    pub(crate) fn spawn_with_keepalive(
        command: &mut Command,
        label: &str,
        mut keepalive: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<(Child, Self, DetachedChildReaperPermit), String> {
        // Breakaway request and platform spawn flags must be decided before the child exists.
        // 必须在子进程存在前确定脱离请求与平台启动标记。
        let breakaway_requested = ProcessTreeController::prepare_command(command)?;
        // Reaper capacity is initialized and reserved before spawn, so attach failure cannot lose ownership.
        // 在启动前初始化并预留回收器容量，因此附加失败不可能丢失所有权。
        let reaper_permit = reserve_detached_child_reaper_slot()?;
        let child = ProcessTreeController::spawn_prepared_command(command, breakaway_requested)
            .map_err(|error| format!("spawn {label}: {error}"))?;
        let controller = match ProcessTreeController::attach(&child) {
            Ok(controller) => controller,
            Err(attach_error) => {
                let cleanup_error = terminate_spawned_child_after_attach_failure(
                    child,
                    reaper_permit,
                    keepalive.take(),
                )
                .err();
                return Err(match cleanup_error {
                    Some(cleanup_error) => format!(
                        "attach {label} process tree: {attach_error}; spawned-process cleanup also failed: {cleanup_error}"
                    ),
                    None => format!("attach {label} process tree: {attach_error}"),
                });
            }
        };
        drop(keepalive);
        Ok((child, Self { controller }, reaper_permit))
    }

    /// Terminate every process belonging to the attached child tree.
    /// 终止属于已附加子进程树的全部进程。
    ///
    /// `child` is the unchanged direct-child handle returned by `spawn`.
    /// `child` 是 `spawn` 返回且未替换的直接子进程句柄。
    pub(crate) fn terminate(&self, child: &Child) -> Result<(), String> {
        self.controller.terminate(child)
    }

    /// Advance and verify final convergence of the retained descendant tree.
    /// 推进并校验所保留后代进程树的最终收敛。
    ///
    /// Returns `true` only when the platform's authoritative tree owner reports no process left.
    /// 仅当平台权威进程树所有者报告已无任何进程时返回 `true`。
    pub(crate) fn detached_tree_is_empty(&self) -> Result<bool, String> {
        self.controller.detached_tree_is_empty()
    }

    /// Transfer child and the reserved descendant-convergence guard to the static reaper.
    /// 把子进程与预留的后代收敛固定器转移到静态回收器。
    pub(crate) fn handoff_to_reaper(
        &self,
        permit: DetachedChildReaperPermit,
        child: Child,
        keepalive: Option<Box<dyn FnOnce() + Send>>,
    ) {
        permit.handoff_with_resources(child, keepalive, self.controller.take_detached_guard());
    }

    /// Release the unused detached guard after synchronous worker-tree convergence.
    /// 在同步 Worker 进程树收敛后释放未使用的分离固定器。
    pub(crate) fn clear_detached_guard(&self) {
        self.controller.clear_detached_guard();
    }
}

#[cfg(windows)]
/// Windows Job Object wrapper that kills the entire process tree on termination or final drop.
/// Windows Job Object 封装，在终止或最终析构时杀掉整个进程树。
struct WindowsProcessJob {
    handle: OwnedHandle,
}

/// Pure Rust managed-process session shared by Lua and managed runtimes.
/// 由 Lua 与受管运行时共同复用的纯 Rust 托管进程会话。
#[derive(Clone)]
pub(crate) struct ManagedProcessSessionCore {
    /// Shared process, pipe, buffer, and lifecycle state.
    /// 共享的进程、管道、缓冲与生命周期状态。
    state: Arc<ManagedProcessSessionState>,
}

/// One package-agnostic cleanup callback executed after userdata process teardown.
/// 在 userdata 进程清理后执行的单个包无关清理回调。
pub(crate) type ManagedProcessSessionCleanup = Box<dyn FnOnce() + Send + Sync + 'static>;

/// One nonblocking request that schedules a failed managed-session teardown retry.
/// 用于调度失败受管会话清理重试的单个非阻塞请求。
pub(crate) type ManagedProcessSessionCleanupRetry = Box<dyn Fn() + Send + Sync + 'static>;

/// Shared one-shot lifecycle cleanup runnable by either engine retirement or Lua userdata teardown.
/// 可由引擎退役或 Lua userdata 清理任一方执行的共享单次生命周期清理句柄。
pub(crate) struct ManagedProcessSessionCleanupHandle {
    /// Callback removed before invocation so every competing lifecycle path remains idempotent.
    /// 在调用前取出的回调，使每条竞争生命周期路径都保持幂等。
    cleanup: Mutex<Option<ManagedProcessSessionCleanup>>,
    /// Optional service callback that persistently retries teardown after userdata Drop failure.
    /// userdata 析构清理失败后持久重试清理的可选服务回调。
    retry_teardown: Option<ManagedProcessSessionCleanupRetry>,
}

impl ManagedProcessSessionCleanupHandle {
    /// Wrap one package-specific cleanup callback in a shared exactly-once handle.
    /// 将一个包专属清理回调包装为共享且恰好执行一次的句柄。
    ///
    /// `cleanup` owns registry and filesystem cleanup that must survive beyond one caller.
    /// `cleanup` 拥有必须跨越单个调用方存活的注册表与文件系统清理逻辑。
    ///
    /// Returns a shared handle suitable for both the engine registry and Lua userdata.
    /// 返回适合由引擎注册表与 Lua userdata 共同持有的共享句柄。
    #[cfg(test)]
    pub(crate) fn new(cleanup: ManagedProcessSessionCleanup) -> Arc<Self> {
        Arc::new(Self {
            cleanup: Mutex::new(Some(cleanup)),
            retry_teardown: None,
        })
    }

    /// Wrap lifecycle cleanup together with one nonblocking teardown-retry scheduler.
    /// 将生命周期清理与一个非阻塞清理重试调度器共同包装。
    ///
    /// `cleanup` releases registry and package resources only after process teardown succeeds.
    /// `cleanup` 仅在进程清理成功后释放注册表与包资源。
    ///
    /// `retry_teardown` must only enqueue durable service-owned work and must not block Lua GC.
    /// `retry_teardown` 必须只入队由服务持久拥有的任务，且不得阻塞 Lua GC。
    ///
    /// Returns one shared handle used by the Lua userdata and engine service record.
    /// 返回由 Lua userdata 与引擎服务记录共同使用的共享句柄。
    pub(crate) fn new_with_retry(
        cleanup: ManagedProcessSessionCleanup,
        retry_teardown: ManagedProcessSessionCleanupRetry,
    ) -> Arc<Self> {
        Arc::new(Self {
            cleanup: Mutex::new(Some(cleanup)),
            retry_teardown: Some(retry_teardown),
        })
    }

    /// Run and consume the lifecycle callback exactly once across all shared owners.
    /// 在全部共享所有者之间恰好执行一次并消费生命周期回调。
    pub(crate) fn run_once(&self) {
        // Callback is detached before invocation so reentrancy and panic cannot repeat cleanup.
        // 回调在调用前被摘除，使重入与 panic 都无法重复执行清理。
        let cleanup = self
            .cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(cleanup) = cleanup {
            cleanup();
        }
    }

    /// Return whether the one-shot callback is still available for a future lifecycle retry.
    /// 返回单次回调是否仍可用于未来的生命周期重试。
    pub(crate) fn is_pending(&self) -> bool {
        self.cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Request one exact service-owned retry when lifecycle cleanup remains pending.
    /// 当生命周期清理仍待完成时请求一次精确的服务所有重试。
    pub(crate) fn request_teardown_retry(&self) {
        if self.is_pending()
            && let Some(retry_teardown) = self.retry_teardown.as_ref()
        {
            retry_teardown();
        }
    }
}

/// Rust-backed interactive process session exposed to Lua.
/// 暴露给 Lua 的 Rust 托管交互式进程会话。
struct ManagedProcessSession {
    /// Pure Rust session core delegated to by every Lua-visible method.
    /// 每个 Lua 可见方法委托调用的纯 Rust 会话核心。
    core: ManagedProcessSessionCore,
    /// Engine-local managed-session identifier used to correlate host events, when present.
    /// 存在时用于关联宿主事件的引擎本地受管会话标识。
    managed_session_id: Option<u64>,
    /// Optional shared one-shot cleanup for registry state and per-session filesystem resources.
    /// 用于注册表状态与每会话文件系统资源的可选共享单次清理句柄。
    cleanup: Option<Arc<ManagedProcessSessionCleanupHandle>>,
}

/// Acquire one process-session output buffer and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话输出缓冲区保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_output_buffer(
    buffer: &Arc<Mutex<ManagedProcessOutputBuffer>>,
) -> MutexGuard<'_, ManagedProcessOutputBuffer> {
    buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire the process-tree termination flag and recover after lock poisoning.
/// 获取进程树终止标记，并在锁 poison 后恢复继续使用。
fn lock_process_tree_terminated_flag(terminated: &Mutex<bool>) -> MutexGuard<'_, bool> {
    terminated
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire the observer-notification gate and recover after lock poisoning.
/// 获取观察器通知门，并在锁 poison 后恢复继续使用。
fn lock_observer_notification_gate(open: &Mutex<bool>) -> MutexGuard<'_, bool> {
    open.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one process-session reader slot and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话 reader 槽位保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_reader_slot(
    handle: &Mutex<Option<SessionPipeReader>>,
) -> MutexGuard<'_, Option<SessionPipeReader>> {
    handle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one process-session stdin pipe slot and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话 stdin 管道槽位保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_stdin_pipe(
    stdin: &Mutex<Option<ChildStdin>>,
) -> MutexGuard<'_, Option<ChildStdin>> {
    stdin
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one process-session closed flag and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话关闭标记保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_closed_flag(closed: &Mutex<bool>) -> MutexGuard<'_, bool> {
    closed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one process-session final status cache and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话最终状态缓存保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_final_status(
    final_status: &Mutex<Option<ProcessStatusSnapshot>>,
) -> MutexGuard<'_, Option<ProcessStatusSnapshot>> {
    final_status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one process-session child process and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话子进程保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_child(child: &Mutex<Option<Child>>) -> MutexGuard<'_, Option<Child>> {
    child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ManagedProcessSessionCore {
    /// Install one action that runs only after an asynchronously handed-off child is reaped.
    /// 安装一个仅在异步移交的子进程完成回收后运行的动作。
    ///
    /// `keepalive` retains one resource layer across final state Drop; multiple layers are composed
    /// in installation order and transferred together when asynchronous reaping is required.
    /// `keepalive` 在最终状态析构后保留一层资源；多层动作按安装顺序组合，并在需要异步回收时
    /// 一起转移。
    pub(crate) fn install_final_reaper_keepalive(
        &self,
        keepalive: Box<dyn FnOnce() + Send>,
    ) -> Result<(), String> {
        let mut slot = self
            .state
            .final_reaper_keepalive
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.push(keepalive);
        Ok(())
    }

    /// Launch one fully configured command with managed pipes and process-tree ownership.
    /// 使用托管管道与进程树所有权启动一个已完整配置的命令。
    /// `command` supplies executable, arguments, cwd, and environment; `options` supplies IO policy.
    /// `command` 提供可执行文件、参数、cwd 与环境；`options` 提供 IO 策略。
    /// Returns one owning core or a rollback-complete launch error.
    /// 返回一个拥有资源的核心，或返回已完成回滚的启动错误。
    pub(crate) fn launch(
        command: Command,
        options: ManagedProcessSessionLaunchOptions,
    ) -> Result<Self, String> {
        Self::launch_with_attacher_and_observer(
            command,
            options,
            None,
            None,
            ProcessTreeController::attach,
            spawn_managed_process_background_thread,
        )
    }

    /// Launch with optional observation and child-dependent cleanup installed before spawn.
    /// 使用可选观察器启动，并在 spawn 前安装依赖子进程的清理动作。
    ///
    /// `keepalive` is retained through every partial-launch rollback and runs only after an
    /// asynchronously handed-off child is definitively reaped.
    /// `keepalive` 会跨越每个部分启动回滚保留，并仅在异步移交子进程确定回收后运行。
    pub(crate) fn launch_with_optional_observer_and_keepalive(
        command: Command,
        options: ManagedProcessSessionLaunchOptions,
        observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
        keepalive: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<Self, String> {
        Self::launch_with_attacher_and_observer(
            command,
            options,
            observer,
            keepalive,
            ProcessTreeController::attach,
            spawn_managed_process_background_thread,
        )
    }

    /// Launch one observed process in tests without a package-resource keepalive.
    /// 在测试中启动一个不带包资源保活动作的受观察进程。
    #[cfg(test)]
    fn launch_with_observer(
        command: Command,
        options: ManagedProcessSessionLaunchOptions,
        observer: Arc<dyn ManagedProcessSessionObserver>,
    ) -> Result<Self, String> {
        Self::launch_with_optional_observer_and_keepalive(command, options, Some(observer), None)
    }

    /// Launch one command through an injectable tree attacher used by failure-path tests.
    /// 通过可注入的进程树附加器启动命令，供失败路径测试使用。
    /// `command` and `options` define the child; `attach` deterministically controls tree attachment.
    /// `command` 与 `options` 定义子进程；`attach` 以确定方式控制进程树附加。
    /// Returns the owning core or an error after spawned-child rollback completes.
    /// 返回拥有所有权的核心，或在已启动子进程回滚完成后返回错误。
    #[cfg(test)]
    fn launch_with_attacher<F>(
        command: Command,
        options: ManagedProcessSessionLaunchOptions,
        attach: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(&Child) -> Result<ProcessTreeController, String>,
    {
        Self::launch_with_attacher_and_observer(
            command,
            options,
            None,
            None,
            attach,
            spawn_managed_process_background_thread,
        )
    }

    /// Launch through optional observation and an injectable process-tree attacher.
    /// 通过可选观察器与可注入进程树附加器启动会话。
    /// `observer` is `None` for ordinary Lua sessions and present for managed runtime sessions.
    /// 普通 Lua 会话的 `observer` 为 `None`，受管运行时会话则提供观察器。
    /// Returns one owning core after reader and exit monitoring are fully installed.
    /// 在读取器与退出监视完全安装后返回一个拥有资源的核心。
    fn launch_with_attacher_and_observer<F>(
        mut command: Command,
        options: ManagedProcessSessionLaunchOptions,
        observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
        mut final_reaper_keepalive: Option<Box<dyn FnOnce() + Send>>,
        attach: F,
        spawn_thread: ManagedProcessBackgroundThreadSpawner,
    ) -> Result<Self, String>
    where
        F: FnOnce(&Child) -> Result<ProcessTreeController, String>,
    {
        if options.buffer_limit_bytes == 0 {
            return Err("buffer_limit_bytes must be greater than zero".to_string());
        }
        // BreakawayRequested records whether Windows first attempts to leave an outer host job.
        // BreakawayRequested 记录 Windows 是否先尝试脱离外层宿主 Job。
        let breakaway_requested = ProcessTreeController::prepare_command(&mut command)?;
        // Pre-spawn reservation makes every attach-failure timeout handoff capacity-safe.
        // 启动前预留使每次附加失败超时交接都具备容量安全性。
        let reaper_permit = reserve_detached_child_reaper_slot()?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Child is the newly spawned process that must be rolled back if tree attachment fails.
        // Child 是刚启动的进程；若进程树附加失败，必须回滚它。
        let mut child =
            ProcessTreeController::spawn_prepared_command(&mut command, breakaway_requested)
                .map_err(|error| format!("spawn managed process: {error}"))?;
        // ProcessTree retains the platform controller for the whole session lifetime.
        // ProcessTree 在整个会话生命周期内保留平台进程树控制器。
        let process_tree = match attach(&child) {
            Ok(process_tree) => process_tree,
            Err(attach_error) => {
                // CleanupError preserves any rollback failure next to the primary attach error.
                // CleanupError 在主要附加错误旁保留任何回滚失败。
                let cleanup_error = terminate_spawned_child_after_attach_failure(
                    child,
                    reaper_permit,
                    final_reaper_keepalive.take(),
                )
                .err();
                return Err(match cleanup_error {
                    Some(cleanup_error) => format!(
                        "attach managed process tree: {attach_error}; spawned-process cleanup also failed: {cleanup_error}"
                    ),
                    None => format!("attach managed process tree: {attach_error}"),
                });
            }
        };
        // Stdin is detached from Child and owned by the synchronized session state.
        // Stdin 从 Child 中取出，并由同步会话状态拥有。
        let stdin = child.stdin.take();
        // StdoutPipe is held by the launch transaction until its fallible reader starts.
        // StdoutPipe 由启动事务持有，直到其可失败读取器启动成功。
        let stdout_pipe = child.stdout.take();
        // StderrPipe remains independently owned until its reader starts or rollback drops it.
        // StderrPipe 保持独立所有权，直到读取器启动或回滚将其释放。
        let stderr_pipe = child.stderr.take();
        // StdoutBuffer is the bounded ring shared with the stdout reader thread.
        // StdoutBuffer 是与 stdout 读取线程共享的有界环形缓冲区。
        let stdout_buffer = Arc::new(Mutex::new(ManagedProcessOutputBuffer::default()));
        // StderrBuffer is the independent bounded ring shared with the stderr reader thread.
        // StderrBuffer 是与 stderr 读取线程共享的独立有界环形缓冲区。
        let stderr_buffer = Arc::new(Mutex::new(ManagedProcessOutputBuffer::default()));
        // ObserverNotificationsOpen serializes notification delivery against session teardown.
        // ObserverNotificationsOpen 将通知投递与会话清理串行化。
        let observer_notifications_open = Arc::new(Mutex::new(observer.is_some()));
        // State owns the process tree before any fallible thread creation begins.
        // State 在任何可失败线程创建开始前拥有进程树。
        let state = Arc::new(ManagedProcessSessionState {
            child: Mutex::new(Some(child)),
            reaper_permit: Mutex::new(Some(reaper_permit)),
            final_reaper_keepalive: Mutex::new(final_reaper_keepalive.into_iter().collect()),
            process_tree,
            stdin: Mutex::new(stdin),
            stdout_buffer,
            stderr_buffer,
            stdout_encoding: options.stdout_encoding,
            stderr_encoding: options.stderr_encoding,
            stdin_encoding: options.stdin_encoding,
            stdout_reader: Mutex::new(None),
            stderr_reader: Mutex::new(None),
            closed: Mutex::new(false),
            final_status: Mutex::new(None),
            process_tree_terminated: Mutex::new(false),
            observer,
            observer_notifications_open,
        });
        if let Some(stdout) = stdout_pipe {
            // StdoutReader continuously drains the child pipe so output cannot block the child.
            // StdoutReader 持续排空子进程管道，防止输出阻塞子进程。
            let stdout_reader = match spawn_session_pipe_reader(
                stdout,
                state.stdout_buffer.clone(),
                options.buffer_limit_bytes,
                ManagedProcessOutputStream::Stdout,
                state.observer.clone(),
                state.observer_notifications_open.clone(),
                spawn_thread,
            ) {
                Ok(reader) => reader,
                Err(error) => {
                    drop(stderr_pipe);
                    return Err(rollback_managed_process_launch(&state, error));
                }
            };
            *lock_session_reader_slot(&state.stdout_reader) = Some(stdout_reader);
        }
        if let Some(stderr) = stderr_pipe {
            // StderrReader independently drains diagnostics under the same bounded policy.
            // StderrReader 在相同有界策略下独立排空诊断输出。
            let stderr_reader = match spawn_session_pipe_reader(
                stderr,
                state.stderr_buffer.clone(),
                options.buffer_limit_bytes,
                ManagedProcessOutputStream::Stderr,
                state.observer.clone(),
                state.observer_notifications_open.clone(),
                spawn_thread,
            ) {
                Ok(reader) => reader,
                Err(error) => return Err(rollback_managed_process_launch(&state, error)),
            };
            *lock_session_reader_slot(&state.stderr_reader) = Some(stderr_reader);
        }
        if let Err(error) = spawn_direct_child_exit_watcher(&state, spawn_thread) {
            return Err(rollback_managed_process_launch(&state, error));
        }
        Ok(Self { state })
    }

    /// Encode and write one text fragment to stdin, then flush it.
    /// 编码一个文本片段并写入 stdin，随后刷新管道。
    /// `text` is encoded with the launch-time stdin encoding; success returns no payload.
    /// `text` 使用启动时 stdin 编码；成功时不返回载荷。
    pub(crate) fn write_text(&self, text: &str) -> Result<(), String> {
        if *lock_session_closed_flag(&self.state.closed) {
            return Err("session is closed".to_string());
        }
        // Bytes contains the configured encoding representation written to the child pipe.
        // Bytes 包含按配置编码后写入子进程管道的表示。
        let bytes = encode_runtime_text(text, self.state.stdin_encoding)?;
        // StdinGuard serializes writes and protects the optional closed pipe slot.
        // StdinGuard 串行化写入并保护可选的已关闭管道槽位。
        let mut stdin = lock_session_stdin_pipe(&self.state.stdin);
        // Stdin is the live writable pipe required by this operation.
        // Stdin 是本次操作所需的活动可写管道。
        let stdin = stdin
            .as_mut()
            .ok_or_else(|| "stdin is closed".to_string())?;
        stdin.write_all(&bytes).map_err(|error| error.to_string())?;
        stdin.flush().map_err(|error| error.to_string())
    }

    /// Wait for readable output and drain at most the requested bytes from each stream.
    /// 等待可读输出，并从每个流最多取出请求的字节数。
    /// `request` controls timeout, per-stream drain size, and optional marker matching.
    /// `request` 控制超时、每流取出大小与可选标记匹配。
    /// Returns decoded data plus cumulative post-drain diagnostics.
    /// 返回已解码数据与取出后的累计诊断信息。
    pub(crate) fn read(
        &self,
        request: &ManagedProcessSessionReadRequest,
    ) -> Result<ManagedProcessSessionReadResult, String> {
        if request.max_bytes == 0 {
            return Err("max_bytes must be greater than zero".to_string());
        }
        // Deadline is validated before any wait so oversized values never mutate session state.
        // Deadline 在任何等待前完成校验，使超大值永远不会修改会话状态。
        let deadline = checked_timeout_deadline(request.timeout_ms, "read timeout_ms")?;
        // TimedOut distinguishes deadline expiry from readable data or reader completion.
        // TimedOut 用于区分截止时间到期与数据可读或读取器完成。
        let mut timed_out = false;
        loop {
            if self.has_readable_output(&request.until_text)
                || self.state.output_readers_drained()?
            {
                break;
            }
            // Now is captured once per iteration for consistent deadline comparison and sleep.
            // Now 每轮只捕获一次，用于一致的截止点比较与休眠。
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                break;
            }
            thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }

        // StdoutBytes and StdoutStats are captured atomically from the stdout ring.
        // StdoutBytes 与 StdoutStats 从 stdout 环形缓冲区原子获取。
        let (stdout_bytes, stdout_stats) =
            drain_output_buffer(&self.state.stdout_buffer, request.max_bytes);
        // StderrBytes and StderrStats are captured independently from the stderr ring.
        // StderrBytes 与 StderrStats 从 stderr 环形缓冲区独立获取。
        let (stderr_bytes, stderr_stats) =
            drain_output_buffer(&self.state.stderr_buffer, request.max_bytes);
        // Stdout is the decoded representation plus loss and Base64 metadata.
        // Stdout 是解码后的表示以及损失和 Base64 元数据。
        let stdout = decode_runtime_text(&stdout_bytes, self.state.stdout_encoding);
        // Stderr is decoded with its independently configured stream encoding.
        // Stderr 使用独立配置的流编码进行解码。
        let stderr = decode_runtime_text(&stderr_bytes, self.state.stderr_encoding);
        Ok(ManagedProcessSessionReadResult {
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_encoding: stdout.encoding,
            stderr_encoding: stderr.encoding,
            stdout_lossy: stdout.lossy,
            stderr_lossy: stderr.lossy,
            stdout_base64: stdout.base64,
            stderr_base64: stderr.base64,
            timed_out,
            stdout_stats,
            stderr_stats,
        })
    }

    /// Return process lifecycle state and current output diagnostics.
    /// 返回进程生命周期状态与当前输出诊断信息。
    /// Returns a non-consuming snapshot and never drains output buffers.
    /// 返回不消费数据的快照，且永远不会取出输出缓冲区。
    pub(crate) fn status(&self) -> Result<ManagedProcessSessionStatus, String> {
        Ok(ManagedProcessSessionStatus {
            process: self.state.peek_status_snapshot()?,
            stdout: output_buffer_stats(&self.state.stdout_buffer),
            stderr: output_buffer_stats(&self.state.stderr_buffer),
            closed: *lock_session_closed_flag(&self.state.closed),
        })
    }

    /// Close stdin, wait for graceful exit, then terminate and reap the full process tree.
    /// 关闭 stdin、等待优雅退出，随后终止并回收完整进程树。
    /// `timeout_ms` is a checked finite graceful-wait duration and may be zero.
    /// `timeout_ms` 是经过检查的有限优雅等待时长，允许为零。
    /// Returns the reaped direct-child status after complete tree teardown.
    /// 在完整进程树清理后返回已回收的直接子进程状态。
    pub(crate) fn close(&self, timeout_ms: u64) -> Result<ProcessStatusSnapshot, String> {
        // Deadline is validated before stdin is closed so invalid input has no side effects.
        // Deadline 在关闭 stdin 前完成校验，使无效输入不产生副作用。
        let deadline = checked_timeout_deadline(timeout_ms, "close timeout_ms")?;
        // Closed is published before dropping stdin so concurrent writers cannot start new writes.
        // Closed 会在丢弃 stdin 前发布，避免并发写入方开始新的写操作。
        self.state.mark_closed()?;
        self.state.close_stdin_pipe()?;
        loop {
            if self.state.peek_status_snapshot()?.exited {
                break;
            }
            // Now is shared by the timeout comparison and bounded polling sleep.
            // Now 由超时比较与有界轮询休眠共享。
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
        // FinalStatus is returned only after tree termination and direct-child reaping.
        // FinalStatus 只会在进程树终止并回收直接子进程后返回。
        let final_status = self.state.kill_process_tree_and_wait()?;
        self.state.join_reader_threads()?;
        Ok(final_status)
    }

    /// Immediately terminate and reap the full process tree.
    /// 立即终止并回收完整进程树。
    /// Returns the direct-child terminal status and is idempotent across repeated calls.
    /// 返回直接子进程终态，并在重复调用时保持幂等。
    pub(crate) fn kill(&self) -> Result<ProcessStatusSnapshot, String> {
        self.state.mark_closed()?;
        self.state.close_stdin_pipe()?;
        // FinalStatus records the direct child result after immediate full-tree termination.
        // FinalStatus 记录立即终止完整进程树后的直接子进程结果。
        let final_status = self.state.kill_process_tree_and_wait()?;
        self.state.join_reader_threads()?;
        Ok(final_status)
    }

    /// Perform complete retryable teardown before a userdata cleanup callback runs.
    /// 在 userdata 清理回调运行前执行完整且可重试的清理。
    ///
    /// Return success only after process-tree termination, direct-child reaping, and reader joins.
    /// 仅在进程树终止、直接子进程回收与读取器 join 全部完成后返回成功。
    fn teardown_before_userdata_cleanup(&self) -> Result<(), String> {
        self.kill().map(|_| ())
    }

    /// Return whether either output stream is readable under one optional marker condition.
    /// 返回在可选标记条件下任一输出流是否可读。
    fn has_readable_output(&self, until_text: &Option<String>) -> bool {
        // StdoutGuard protects one consistent stdout marker scan.
        // StdoutGuard 保护一次一致的 stdout 标记扫描。
        let stdout = lock_session_output_buffer(&self.state.stdout_buffer);
        // StderrGuard protects the corresponding independent stderr scan.
        // StderrGuard 保护对应的独立 stderr 扫描。
        let stderr = lock_session_output_buffer(&self.state.stderr_buffer);
        if stdout.bytes.is_empty() && stderr.bytes.is_empty() {
            return false;
        }
        if let Some(marker) = until_text {
            // StdoutBytes linearizes the ring only for encoding-aware marker matching.
            // StdoutBytes 仅为编码感知的标记匹配线性化环形缓冲区。
            let stdout_bytes = stdout.bytes.iter().copied().collect::<Vec<_>>();
            // StderrBytes linearizes the other stream without changing retained data.
            // StderrBytes 线性化另一个流，且不修改保留数据。
            let stderr_bytes = stderr.bytes.iter().copied().collect::<Vec<_>>();
            // StdoutText is the decoded marker-search view.
            // StdoutText 是用于标记搜索的解码视图。
            let stdout_text = decode_runtime_text(&stdout_bytes, self.state.stdout_encoding).text;
            // StderrText is decoded independently for the same marker.
            // StderrText 针对同一标记独立解码。
            let stderr_text = decode_runtime_text(&stderr_bytes, self.state.stderr_encoding).text;
            return stdout_text.contains(marker) || stderr_text.contains(marker);
        }
        true
    }
}

/// Roll back a partially initialized managed-process launch and preserve both failure causes.
/// 回滚一个部分初始化的托管进程启动，并保留两类失败原因。
///
/// `state` already owns the child tree and every successfully created reader thread.
/// `state` 已拥有子进程树与全部已成功创建的读取器线程。
///
/// `launch_error` is returned alone after complete teardown or combined with a retryable cleanup error.
/// 完整清理后仅返回 `launch_error`；清理失败时会与可重试清理错误组合返回。
fn rollback_managed_process_launch(
    state: &Arc<ManagedProcessSessionState>,
    launch_error: String,
) -> String {
    notify_managed_process_observer(
        state.observer.as_ref(),
        &state.observer_notifications_open,
        ManagedProcessSessionObserver::failed,
    );
    // RollbackCore reuses the exact public teardown ordering without creating a second state owner.
    // RollbackCore 复用精确的公共清理顺序，且不会创建第二份状态所有权。
    let rollback_core = ManagedProcessSessionCore {
        state: state.clone(),
    };
    match rollback_core.teardown_before_userdata_cleanup() {
        Ok(()) => launch_error,
        Err(cleanup_error) => {
            format!("{launch_error}; managed process launch rollback also failed: {cleanup_error}")
        }
    }
}

impl ManagedProcessSession {
    /// Build and launch the pure Rust core for one parsed Lua open request.
    /// 为一个已解析 Lua 打开请求构建并启动纯 Rust 核心。
    fn launch_core(request: ProcessSessionOpenRequest) -> mlua::Result<ManagedProcessSessionCore> {
        // Command receives only Lua-authorized executable, arguments, and cwd configuration.
        // Command 只接收 Lua 已授权的可执行文件、参数与 cwd 配置。
        let mut command = Command::new(&request.program);
        command.args(&request.args);
        // Cwd is the optional working directory already validated by the Lua request parser.
        // Cwd 是已经由 Lua 请求解析器验证的可选工作目录。
        if let Some(cwd) = request.cwd.as_deref() {
            command.current_dir(cwd);
        }
        // Options transfers encoding and buffering policy into the package-agnostic core.
        // Options 把编码与缓冲策略传入包无关核心。
        let options = ManagedProcessSessionLaunchOptions {
            stdout_encoding: request.stdout_encoding,
            stderr_encoding: request.stderr_encoding,
            stdin_encoding: request.stdin_encoding,
            buffer_limit_bytes: request.buffer_limit_bytes,
        };
        // Core owns every process resource after successful launch.
        // Core 在成功启动后拥有全部进程资源。
        let core = ManagedProcessSessionCore::launch(command, options)
            .map_err(|error| mlua::Error::runtime(format!("process.session.open: {error}")))?;
        Ok(core)
    }

    /// Spawn a new Lua userdata wrapper around the pure Rust session core.
    /// 围绕纯 Rust 会话核心启动一个新的 Lua userdata 包装。
    #[cfg(test)]
    fn open(request: ProcessSessionOpenRequest) -> mlua::Result<Self> {
        // Core is wrapped exactly like the public userdata path, without a package cleanup.
        // Core 与公共 userdata 路径完全相同地包装，但不附加包清理。
        let core = Self::launch_core(request)?;
        Ok(Self::from_core(core, None, None))
    }

    /// Wrap one pure Rust core and optional one-shot lifecycle cleanup.
    /// 包装一个纯 Rust 核心与可选的单次生命周期清理回调。
    /// `managed_session_id` correlates System host events and is absent for ordinary process sessions.
    /// `managed_session_id` 用于关联 System 宿主事件；普通进程会话中不存在。
    fn from_core(
        core: ManagedProcessSessionCore,
        cleanup: Option<Arc<ManagedProcessSessionCleanupHandle>>,
        managed_session_id: Option<u64>,
    ) -> Self {
        Self {
            core,
            managed_session_id,
            cleanup,
        }
    }

    /// Run and consume the attached lifecycle cleanup exactly once.
    /// 仅执行一次并消费附加的生命周期清理回调。
    ///
    /// Explicit close, explicit kill, and userdata drop share this method so registry and
    /// per-session filesystem resources are released at the earliest terminal boundary.
    /// 显式关闭、显式终止与 userdata 析构共享此方法，使注册表及每会话文件系统资源在最早终态边界释放。
    fn run_cleanup_once(&self) {
        if let Some(cleanup) = self.cleanup.as_ref() {
            cleanup.run_once();
        }
    }

    /// Convert Lua values to text and delegate stdin writes to the pure Rust core.
    /// 将 Lua 值转换为文本，并把 stdin 写入委托给纯 Rust 核心。
    fn write_values(&self, values: MultiValue) -> mlua::Result<bool> {
        // Value iterates the ordered Lua fragments passed to one write call.
        // Value 按顺序遍历单次 write 调用传入的 Lua 片段。
        for value in values {
            // Text is the strict Lua-to-string representation consumed by the core encoder.
            // Text 是核心编码器消费的严格 Lua 到字符串表示。
            let text = lua_value_to_session_text(value, "process.session.write")?;
            self.core
                .write_text(&text)
                .map_err(|error| mlua::Error::runtime(format!("process.session.write: {error}")))?;
        }
        Ok(true)
    }

    /// Convert one pure Rust read result into the Lua table contract.
    /// 将一个纯 Rust 读取结果转换为 Lua 表契约。
    fn read(&self, lua: &Lua, args: MultiValue) -> mlua::Result<Table> {
        // Request is the fully validated Lua read request.
        // Request 是完成全部校验的 Lua 读取请求。
        let request = parse_session_read_request(args)?;
        // Read is the package-agnostic result converted below without lifecycle logic.
        // Read 是下方不含生命周期逻辑地转换的包无关结果。
        let read = self
            .core
            .read(&request)
            .map_err(|error| mlua::Error::runtime(format!("process.session.read: {error}")))?;
        process_session_read_result_to_lua_table(lua, read)
    }

    /// Return the core lifecycle state and output diagnostics as a Lua table.
    /// 以 Lua 表返回核心生命周期状态与输出诊断信息。
    fn status(&self, lua: &Lua) -> mlua::Result<Table> {
        // Status is one consistent core snapshot rendered into the Lua contract.
        // Status 是渲染到 Lua 契约的一致核心快照。
        let status = self
            .core
            .status()
            .map_err(|error| mlua::Error::runtime(format!("process.session.status: {error}")))?;
        let table = process_session_status_to_lua_table(lua, &status)?;
        if let Some(managed_session_id) = self.managed_session_id {
            table.set("managed_session_id", managed_session_id)?;
        }
        Ok(table)
    }

    /// Delegate graceful close and forced tree cleanup to the pure Rust core.
    /// 将优雅关闭与强制进程树清理委托给纯 Rust 核心。
    fn close(&self, lua: &Lua, args: MultiValue) -> mlua::Result<Table> {
        // Request contains the validated non-negative graceful timeout.
        // Request 包含经过校验的非负优雅退出超时。
        let request = parse_session_close_request(args)?;
        // FinalStatus is converted only after the core completes full teardown.
        // FinalStatus 只会在核心完成完整清理后进行转换。
        let final_status = self
            .core
            .close(request.timeout_ms)
            .map_err(|error| mlua::Error::runtime(format!("process.session.close: {error}")))?;
        // Cleanup runs only after successful process-tree teardown so a failed close remains retryable.
        // 仅在进程树成功清理后执行 cleanup，使失败的 close 仍可重试。
        self.run_cleanup_once();
        process_status_snapshot_to_lua_table(lua, &final_status)
    }

    /// Delegate immediate tree termination to the pure Rust core.
    /// 将立即终止进程树委托给纯 Rust 核心。
    fn kill(&self) -> mlua::Result<bool> {
        // FinalStatus is intentionally discarded because the historical Lua contract returns true.
        // FinalStatus 被有意丢弃，因为既有 Lua 契约返回 true。
        let _ = self
            .core
            .kill()
            .map_err(|error| mlua::Error::runtime(format!("process.session.kill: {error}")))?;
        // Cleanup runs only after successful process-tree teardown so owner retirement can retry failures.
        // 仅在进程树成功清理后执行 cleanup，使所有者退役可以重试失败。
        self.run_cleanup_once();
        Ok(true)
    }

    /// Kill the full process tree through the core for focused lifecycle tests.
    /// 通过核心终止完整进程树，供聚焦生命周期测试使用。
    #[cfg(test)]
    fn kill_child(&self) -> mlua::Result<ProcessStatusSnapshot> {
        self.core
            .state
            .kill_process_tree_and_wait()
            .map_err(|error| mlua::Error::runtime(format!("process.session.kill: {error}")))
    }

    /// Close stdin through the core state for focused lifecycle tests.
    /// 通过核心状态关闭 stdin，供聚焦生命周期测试使用。
    #[cfg(test)]
    fn close_stdin(&self, operation_name: &str) -> mlua::Result<()> {
        self.core
            .state
            .close_stdin_pipe()
            .map_err(|error| mlua::Error::runtime(format!("{operation_name}: {error}")))
    }

    /// Mark the core closed for focused lifecycle tests.
    /// 将核心标记为已关闭，供聚焦生命周期测试使用。
    #[cfg(test)]
    fn mark_closed(&self, operation_name: &str) -> mlua::Result<()> {
        self.core
            .state
            .mark_closed()
            .map_err(|error| mlua::Error::runtime(format!("{operation_name}: {error}")))
    }

    /// Join core reader threads for focused lifecycle tests.
    /// 等待核心读取线程退出，供聚焦生命周期测试使用。
    #[cfg(test)]
    fn join_reader_threads(&self, operation_name: &str) -> mlua::Result<()> {
        self.core
            .state
            .join_reader_threads()
            .map_err(|error| mlua::Error::runtime(format!("{operation_name}: {error}")))
    }
}

impl Drop for ManagedProcessSession {
    /// Stop and reap the process tree before deleting any attached per-session resources.
    /// 在删除任何附加的每会话资源前停止并回收进程树。
    fn drop(&mut self) {
        // Cleanup remains unconsumed on failure while the engine-owned service schedules an exact retry.
        // 清理失败时保留 cleanup 未消费，同时由引擎所有服务调度一次精确重试。
        match self.core.teardown_before_userdata_cleanup() {
            Ok(()) => self.run_cleanup_once(),
            Err(error) => {
                if let Some(cleanup) = self.cleanup.as_ref() {
                    cleanup.request_teardown_retry();
                }
                crate::runtime_logging::warn(format!(
                    "[LuaSkill:warn] managed process userdata teardown failed and was scheduled for retry when service-owned: {error}"
                ));
            }
        }
    }
}

impl ManagedProcessSessionState {
    /// Return whether background observer notifications may still be delivered.
    /// 返回后台观察器通知当前是否仍可投递。
    fn observer_notifications_are_open(&self) -> bool {
        *lock_observer_notification_gate(&self.observer_notifications_open)
    }

    /// Close the serialized observer gate before process teardown begins.
    /// 在进程清理开始前关闭串行化观察器通知门。
    fn close_observer_notifications(&self) {
        // OpenGuard prevents every later notification reservation from crossing this boundary.
        // OpenGuard 防止此边界之后的任何通知预留继续通过。
        let mut open = lock_observer_notification_gate(&self.observer_notifications_open);
        *open = false;
    }

    /// Return the cached terminal status if one explicit teardown already reaped this session.
    /// 返回已缓存的终态状态；当显式清理已经回收该会话时生效。
    fn cached_final_status(&self) -> Result<Option<ProcessStatusSnapshot>, String> {
        let final_status = lock_session_final_status(&self.final_status);
        Ok(*final_status)
    }

    /// Cache one terminal status snapshot after direct-child reaping completes.
    /// 在直接子进程完成回收后缓存一份终态状态快照。
    fn store_final_status(&self, status: ProcessStatusSnapshot) -> Result<(), String> {
        let mut final_status = lock_session_final_status(&self.final_status);
        *final_status = Some(status);
        Ok(())
    }

    /// Return whether one optional reader has already signaled completion.
    /// 返回某个可选读取器是否已经发出完成信号。
    fn reader_completed(handle: &Mutex<Option<SessionPipeReader>>) -> bool {
        let reader_slot = lock_session_reader_slot(handle);
        reader_slot
            .as_ref()
            .map(|reader| reader.done.load(Ordering::Acquire))
            .unwrap_or(true)
    }

    /// Return whether all output readers have already finished draining their pipes.
    /// 返回全部输出读取器是否都已经完成并排空各自管道。
    fn output_readers_drained(&self) -> Result<bool, String> {
        Ok(Self::reader_completed(&self.stdout_reader)
            && Self::reader_completed(&self.stderr_reader))
    }

    /// Drop the session stdin pipe so the child can observe EOF.
    /// 丢弃会话的 stdin 管道，让子进程可以观察到 EOF。
    fn close_stdin_pipe(&self) -> Result<(), String> {
        let mut stdin = lock_session_stdin_pipe(&self.stdin);
        stdin.take();
        Ok(())
    }

    /// Mark the shared session state as closed.
    /// 将共享会话状态标记为已关闭。
    fn mark_closed(&self) -> Result<(), String> {
        let mut closed = lock_session_closed_flag(&self.closed);
        *closed = true;
        drop(closed);
        self.close_observer_notifications();
        Ok(())
    }

    /// Terminate the full process tree exactly once, retrying after any failed attempt.
    /// 只成功终止完整进程树一次，并在失败后允许重试。
    fn terminate_process_tree_once(&self, child: &Child) -> Result<(), String> {
        // TerminatedGuard serializes the one successful platform tree-termination call.
        // TerminatedGuard 串行化唯一一次成功的平台进程树终止调用。
        let mut terminated = lock_process_tree_terminated_flag(&self.process_tree_terminated);
        if *terminated {
            return Ok(());
        }
        self.process_tree.terminate(child)?;
        *terminated = true;
        Ok(())
    }

    /// Peek the current process status without reaping the child on Unix.
    /// 观察当前进程状态，并在 Unix 上避免提前 reap 子进程。
    fn peek_status_snapshot(&self) -> Result<ProcessStatusSnapshot, String> {
        if let Some(status) = self.cached_final_status()? {
            return Ok(status);
        }
        #[cfg(unix)]
        {
            let child = lock_session_child(&self.child);
            let child = child.as_ref().ok_or_else(|| {
                "managed process direct child ownership was transferred".to_string()
            })?;
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id() as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    return Ok(ProcessStatusSnapshot {
                        running: false,
                        exited: true,
                        success: None,
                        code: None,
                    });
                }
                return Err(format!("waitid: {error}"));
            }
            let info = unsafe { info.assume_init() };
            let reported_pid = unsafe { info.si_pid() };
            if reported_pid == 0 {
                return Ok(ProcessStatusSnapshot {
                    running: true,
                    exited: false,
                    success: None,
                    code: None,
                });
            }
            let status_code = unsafe { info.si_status() };
            let signal_code = info.si_code;
            let (success, code) = if signal_code == libc::CLD_EXITED {
                (Some(status_code == 0), Some(status_code))
            } else {
                (Some(false), None)
            };
            Ok(ProcessStatusSnapshot {
                running: false,
                exited: true,
                success,
                code,
            })
        }
        #[cfg(windows)]
        {
            let child = lock_session_child(&self.child);
            let child = child.as_ref().ok_or_else(|| {
                "managed process direct child ownership was transferred".to_string()
            })?;
            peek_windows_process_status(child.as_raw_handle() as HANDLE)
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            let mut child = lock_session_child(&self.child);
            let child = child.as_mut().ok_or_else(|| {
                "managed process direct child ownership was transferred".to_string()
            })?;
            match child.try_wait().map_err(|error| error.to_string())? {
                Some(status) => Ok(ProcessStatusSnapshot {
                    running: false,
                    exited: true,
                    success: Some(status.success()),
                    code: status.code(),
                }),
                None => Ok(ProcessStatusSnapshot {
                    running: true,
                    exited: false,
                    success: None,
                    code: None,
                }),
            }
        }
    }

    /// Kill the child process if it is still running and wait for one final exit status.
    /// 如果进程树仍在运行则整体杀掉它，并等待直接子进程最终退出完成回收。
    fn kill_process_tree_and_wait(&self) -> Result<ProcessStatusSnapshot, String> {
        // ChildGuard serializes termination and reaping across every lifecycle caller.
        // ChildGuard 将所有生命周期调用方的终止与回收操作串行化。
        let mut child_guard = lock_session_child(&self.child);
        let child = child_guard
            .as_mut()
            .ok_or_else(|| "managed process direct child ownership was transferred".to_string())?;
        self.terminate_process_tree_once(child)?;
        if let Some(status) = self.cached_final_status()? {
            return Ok(status);
        }
        // Direct-child reaping uses one absolute deadline; the synchronized Child remains owned
        // for a later retry when an uninterruptible kernel wait outlives this lifecycle call.
        // 直接子进程回收使用一个绝对截止时间；当不可中断内核等待超过当前生命周期调用时，
        // 同步 Child 会继续被拥有以供后续重试。
        let status = Some(wait_for_child_exit_until(
            child,
            Instant::now() + FORCED_CHILD_REAP_TIMEOUT,
            "managed process direct child",
        )?);
        // Snapshot is cached only after the direct child has been definitively reaped.
        // Snapshot 只会在直接子进程确定完成回收后写入缓存。
        let snapshot = process_status_snapshot_from_exit_status(status);
        self.store_final_status(snapshot)?;
        drop(child_guard);
        // Definitive reap releases the lifetime reservation without involving the background reaper.
        // 确定回收后释放生命周期预留，无需使用后台回收器。
        drop(
            self.reaper_permit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        // Definitive reap releases any launch-time resource keepalive before lifecycle cleanup.
        // 确定回收后在生命周期清理前释放任何启动期资源保活动作。
        drop(std::mem::take(
            &mut *self
                .final_reaper_keepalive
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ));
        self.process_tree.clear_detached_guard();
        Ok(snapshot)
    }

    /// Join one optional background reader thread after process shutdown.
    /// 在进程关闭后等待一个可选的后台读取线程退出。
    fn join_one_reader(
        handle: &Mutex<Option<SessionPipeReader>>,
        stream_name: &'static str,
    ) -> Result<(), String> {
        // CurrentThreadId prevents reentrant observer teardown from waiting on its own reader.
        // CurrentThreadId 防止观察器重入清理等待其所在的读取器线程自身。
        let current_thread_id = thread::current().id();
        let should_take = {
            let mut reader_slot = lock_session_reader_slot(handle);
            if reader_slot
                .as_ref()
                .is_some_and(|reader| reader.handle.thread().id() == current_thread_id)
            {
                // Taking and dropping the current JoinHandle detaches it; the closed gate prevents more events.
                // 取出并丢弃当前 JoinHandle 会将其分离；已关闭通知门会阻止更多事件。
                reader_slot.take();
                return Ok(());
            }
            let Some(reader) = reader_slot.as_mut() else {
                return Ok(());
            };
            match reader
                .done_rx
                .recv_timeout(Duration::from_millis(DEFAULT_SESSION_CLOSE_TIMEOUT_MS))
            {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "{stream_name} reader shutdown timed out after {DEFAULT_SESSION_CLOSE_TIMEOUT_MS}ms"
                    ));
                }
            }
        };
        if should_take {
            let reader = lock_session_reader_slot(handle).take();
            if let Some(reader) = reader {
                reader
                    .handle
                    .join()
                    .map_err(|_| format!("{stream_name} reader panicked"))?;
            }
        }
        Ok(())
    }

    /// Join all background reader threads retained by this session state.
    /// 等待当前会话状态持有的全部后台读取线程退出。
    fn join_reader_threads(&self) -> Result<(), String> {
        Self::join_one_reader(&self.stdout_reader, "stdout")?;
        Self::join_one_reader(&self.stderr_reader, "stderr")?;
        Ok(())
    }

    /// Best-effort teardown used when the final session handle is dropped.
    /// 在最后一个会话句柄被释放时执行尽力清理。
    fn cleanup_on_drop(&mut self) {
        // Each result is explicitly discarded because Drop cannot return a teardown error.
        // 每个结果都会被显式丢弃，因为 Drop 无法返回清理错误。
        drop(self.mark_closed());
        drop(self.close_stdin_pipe());
        let kill_result = self.kill_process_tree_and_wait();
        if kill_result.is_err() {
            // Final-drop failure transfers the direct child into the pre-reserved static reaper;
            // ownership is never discarded merely because the synchronous deadline expired.
            // 最终析构失败会把直接子进程移入预留的静态回收器；绝不会仅因同步截止时间
            // 到达而丢弃所有权。
            let child = self
                .child
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let permit = self
                .reaper_permit
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let (Some(mut child), Some(permit)) = (child, permit) {
                let _ = child.kill();
                let keepalives = std::mem::take(
                    self.final_reaper_keepalive
                        .get_mut()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                let keepalive = combine_detached_child_keepalives(keepalives);
                let process_tree_guard = self.process_tree.take_detached_guard();
                permit.handoff_with_resources(child, keepalive, process_tree_guard);
            }
        }
        drop(self.join_reader_threads());
    }
}

impl Drop for ManagedProcessSessionState {
    /// Best-effort cleanup for orphaned managed process sessions.
    /// 为失去引用的托管进程会话执行尽力清理。
    fn drop(&mut self) {
        self.cleanup_on_drop();
    }
}

impl UserData for ManagedProcessSession {
    /// Register Lua-visible methods for process session userdata.
    /// 为进程会话 userdata 注册 Lua 可见方法。
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("write", |_, session, values: MultiValue| {
            session.write_values(values)
        });
        methods.add_method("read", |lua, session, args: MultiValue| {
            session.read(lua, args)
        });
        methods.add_method("status", |lua, session, ()| session.status(lua));
        methods.add_method("close", |lua, session, args: MultiValue| {
            session.close(lua, args)
        });
        methods.add_method("kill", |_, session, ()| session.kill());
    }
}

/// Expose one managed-process core through the shared private Lua userdata implementation.
/// 通过共享的私有 Lua userdata 实现暴露一个托管进程核心。
/// `core` supplies the session; `cleanup` optionally deletes package-agnostic per-session resources.
/// `core` 提供会话；`cleanup` 可选删除包无关的每会话资源。
/// `managed_session_id` optionally exposes the engine-local identifier carried by host events.
/// `managed_session_id` 可选暴露宿主事件携带的引擎本地标识。
/// Returns userdata whose drop stops and waits for the process tree before invoking cleanup.
/// 返回一个 userdata，其析构会在调用 cleanup 前停止并等待进程树。
pub(crate) fn create_managed_process_session_userdata(
    lua: &Lua,
    core: ManagedProcessSessionCore,
    cleanup: Option<Arc<ManagedProcessSessionCleanupHandle>>,
    managed_session_id: Option<u64>,
) -> mlua::Result<AnyUserData> {
    lua.create_userdata(ManagedProcessSession::from_core(
        core,
        cleanup,
        managed_session_id,
    ))
}

/// Build the `vulcan.process.session` Lua table.
/// 构建 `vulcan.process.session` Lua 表。
pub(crate) fn create_process_session_table(
    lua: &Lua,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "open",
        lua.create_function(move |lua, spec: LuaValue| {
            let request = parse_session_open_request(spec, default_encoding)?;
            // Core is wrapped by the same userdata factory used by managed runtime sessions.
            // Core 由受管运行时会话共同使用的同一个 userdata 工厂包装。
            let core = ManagedProcessSession::launch_core(request)?;
            create_managed_process_session_userdata(lua, core, None, None)
        })?,
    )?;
    Ok(table)
}

/// Parse a Lua value into one process session open request.
/// 将 Lua 值解析为一个进程会话打开请求。
fn parse_session_open_request(
    value: LuaValue,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<ProcessSessionOpenRequest> {
    let table = match value {
        LuaValue::Table(table) => table,
        other => {
            return Err(mlua::Error::runtime(format!(
                "process.session.open: spec must be a table, got {}",
                lua_value_type_name(&other)
            )));
        }
    };
    // Program spelling exposed by Lua must not retain a Windows verbatim prefix.
    // Lua 暴露的程序写法不得保留 Windows verbatim 前缀。
    let program = normalize_host_input_path_text(&require_string_field(
        &table,
        "program",
        "process.session.open",
    )?)
    .map_err(|error| mlua::Error::runtime(format!("process.session.open: program: {error}")))?;
    let args = parse_string_array_field(&table, "args", "process.session.open")?;
    // Optional cwd normalized at the Lua-to-process boundary.
    // 在 Lua 到进程边界规范化可选 cwd。
    let cwd = parse_optional_string_field(&table, "cwd", "process.session.open")?
        .map(|path| {
            normalize_host_input_path_text(&path).map_err(|error| {
                mlua::Error::runtime(format!("process.session.open: cwd: {error}"))
            })
        })
        .transpose()?;
    let encoding = parse_optional_encoding_field(&table, "encoding", "process.session.open")?
        .unwrap_or(default_encoding);
    let stdout_encoding =
        parse_optional_encoding_field(&table, "stdout_encoding", "process.session.open")?
            .unwrap_or(encoding);
    let stderr_encoding =
        parse_optional_encoding_field(&table, "stderr_encoding", "process.session.open")?
            .unwrap_or(encoding);
    let stdin_encoding =
        parse_optional_encoding_field(&table, "stdin_encoding", "process.session.open")?
            .unwrap_or(encoding);
    let buffer_limit_bytes = parse_optional_usize_field(
        &table,
        "buffer_limit_bytes",
        "process.session.open",
        DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
    )?;
    Ok(ProcessSessionOpenRequest {
        program,
        args,
        cwd,
        stdout_encoding,
        stderr_encoding,
        stdin_encoding,
        buffer_limit_bytes,
    })
}

/// Parse one process session read request from Lua arguments.
/// 从 Lua 参数解析一个进程会话读取请求。
fn parse_session_read_request(args: MultiValue) -> mlua::Result<ManagedProcessSessionReadRequest> {
    let mut values = args.into_iter();
    let value = values.next().unwrap_or(LuaValue::Nil);
    match value {
        LuaValue::Nil => Ok(ManagedProcessSessionReadRequest {
            timeout_ms: DEFAULT_SESSION_READ_TIMEOUT_MS,
            max_bytes: DEFAULT_SESSION_MAX_READ_BYTES,
            until_text: None,
        }),
        LuaValue::Table(table) => Ok(ManagedProcessSessionReadRequest {
            timeout_ms: parse_optional_timeout_ms_field(
                &table,
                "timeout_ms",
                "process.session.read",
                DEFAULT_SESSION_READ_TIMEOUT_MS,
            )?,
            max_bytes: parse_optional_usize_field(
                &table,
                "max_bytes",
                "process.session.read",
                DEFAULT_SESSION_MAX_READ_BYTES,
            )?,
            until_text: parse_optional_string_field(&table, "until_text", "process.session.read")?,
        }),
        other => Err(mlua::Error::runtime(format!(
            "process.session.read: options must be a table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Parse one process session close request from Lua arguments.
/// 从 Lua 参数解析一个进程会话关闭请求。
fn parse_session_close_request(args: MultiValue) -> mlua::Result<ProcessSessionCloseRequest> {
    let mut values = args.into_iter();
    let value = values.next().unwrap_or(LuaValue::Nil);
    match value {
        LuaValue::Nil => Ok(ProcessSessionCloseRequest {
            timeout_ms: DEFAULT_SESSION_CLOSE_TIMEOUT_MS,
        }),
        LuaValue::Table(table) => Ok(ProcessSessionCloseRequest {
            timeout_ms: parse_optional_timeout_ms_field(
                &table,
                "timeout_ms",
                "process.session.close",
                DEFAULT_SESSION_CLOSE_TIMEOUT_MS,
            )?,
        }),
        other => Err(mlua::Error::runtime(format!(
            "process.session.close: options must be a table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

impl ManagedProcessBackgroundThread {
    /// Return the stable operating-system thread name for this launch role.
    /// 返回当前启动角色对应的稳定操作系统线程名。
    fn name(self) -> &'static str {
        match self {
            Self::StdoutReader => "managed-process-stdout-reader",
            Self::StderrReader => "managed-process-stderr-reader",
            Self::ExitWatcher => "managed-process-exit-watcher",
        }
    }
}

/// Spawn one named managed-process background thread through the fallible standard builder.
/// 通过可失败的标准构造器启动一个具名托管进程后台线程。
///
/// `role` provides a stable diagnostic name and `task` owns the complete background operation.
/// `role` 提供稳定诊断名称，`task` 拥有完整后台操作。
///
/// Return the join handle or the operating-system thread creation error without panicking.
/// 返回 join 句柄或操作系统线程创建错误，且不会 panic。
fn spawn_managed_process_background_thread(
    role: ManagedProcessBackgroundThread,
    task: ManagedProcessBackgroundTask,
) -> Result<thread::JoinHandle<()>, std::io::Error> {
    thread::Builder::new()
        .name(role.name().to_string())
        .spawn(task)
}

/// Spawn one fallible pipe reader that appends bytes into a bounded buffer.
/// 启动一个可失败的管道读取器，将字节追加到有界缓冲区。
/// `reader`, `target`, and `limit_bytes` define the pipe and retention policy; observer inputs are optional.
/// `reader`、`target` 与 `limit_bytes` 定义管道及保留策略；观察器输入为可选项。
/// `spawn_thread` is injectable only so tests can deterministically exercise OS creation failures.
/// `spawn_thread` 仅为测试可注入，以确定性验证操作系统创建失败。
///
/// Return the joinable reader state or a launch error before ownership can escape.
/// 返回可等待的读取器状态，或在所有权逃逸前返回启动错误。
fn spawn_session_pipe_reader<R>(
    mut reader: R,
    target: Arc<Mutex<ManagedProcessOutputBuffer>>,
    limit_bytes: usize,
    stream: ManagedProcessOutputStream,
    observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
    observer_notifications_open: Arc<Mutex<bool>>,
    spawn_thread: ManagedProcessBackgroundThreadSpawner,
) -> Result<SessionPipeReader, String>
where
    R: Read + Send + 'static,
{
    // DoneChannel lets teardown wait for EOF or failure before joining the thread.
    // DoneChannel 让清理流程在 join 线程前等待 EOF 或失败。
    let (done_tx, done_rx) = mpsc::channel();
    // Done is the non-consuming completion flag used by read polling.
    // Done 是 read 轮询使用的不消费式完成标记。
    let done = Arc::new(AtomicBool::new(false));
    // DoneFlag transfers completion publication into the reader thread.
    // DoneFlag 把完成状态发布能力传入读取器线程。
    let done_flag = done.clone();
    // Role selects the stable thread name and deterministic failure-injection stage.
    // Role 选择稳定线程名与确定性失败注入阶段。
    let role = match stream {
        ManagedProcessOutputStream::Stdout => ManagedProcessBackgroundThread::StdoutReader,
        ManagedProcessOutputStream::Stderr => ManagedProcessBackgroundThread::StderrReader,
    };
    // Task owns the pipe and completion publishers before the fallible spawn attempt.
    // Task 在可失败启动尝试前拥有管道与完成发布器。
    let task: ManagedProcessBackgroundTask = Box::new(move || {
        // Chunk is the fixed-size temporary read window reused across pipe reads.
        // Chunk 是跨管道读取重复使用的固定大小临时窗口。
        let mut chunk = [0_u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    {
                        // BufferGuard is released before calling package-owned observer code.
                        // BufferGuard 会在调用包侧观察器代码前释放。
                        let mut buffer = lock_session_output_buffer(&target);
                        append_bounded(&mut buffer, &chunk[..count], limit_bytes);
                    }
                    notify_managed_process_observer(
                        observer.as_ref(),
                        &observer_notifications_open,
                        |observer| stream.notify_readable(observer),
                    );
                }
                Err(error) => {
                    notify_managed_process_observer(
                        observer.as_ref(),
                        &observer_notifications_open,
                        ManagedProcessSessionObserver::failed,
                    );
                    crate::runtime_logging::warn(format!(
                        "[LuaSkill:warn] process.session {} reader failed: {error}",
                        stream.name()
                    ));
                    break;
                }
            }
        }
        done_flag.store(true, Ordering::Release);
        // Send failure only means teardown already discarded its completion receiver.
        // Send 失败只表示清理流程已经释放完成接收端。
        let _ = done_tx.send(());
    });
    // Handle owns the background reader until deterministic teardown joins it.
    // Handle 拥有后台读取器，直到确定性清理将其 join。
    let handle = spawn_thread(role, task)
        .map_err(|error| format!("spawn managed process {} reader: {error}", stream.name()))?;
    Ok(SessionPipeReader {
        handle,
        done_rx,
        done,
    })
}

impl ManagedProcessOutputStream {
    /// Return the stable diagnostic name for this output stream.
    /// 返回当前输出流的稳定诊断名称。
    fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }

    /// Invoke the matching readable callback on one observer.
    /// 在一个观察器上调用与当前流匹配的可读回调。
    /// `observer` receives exactly one notification for one successful pipe append.
    /// 每次成功追加管道数据时，`observer` 恰好接收一次通知。
    fn notify_readable(self, observer: &dyn ManagedProcessSessionObserver) {
        match self {
            Self::Stdout => observer.stdout_readable(),
            Self::Stderr => observer.stderr_readable(),
        }
    }
}

/// Invoke one observer callback only while the serialized notification gate remains open.
/// 仅在串行化通知门仍开启时调用一次观察器回调。
/// `observer` may be absent for ordinary Lua sessions; callbacks reserved before close may finish.
/// 普通 Lua 会话可以没有 `observer`；关闭前已获准的回调可以完成。
fn notify_managed_process_observer<F>(
    observer: Option<&Arc<dyn ManagedProcessSessionObserver>>,
    observer_notifications_open: &Mutex<bool>,
    notify: F,
) where
    F: FnOnce(&dyn ManagedProcessSessionObserver),
{
    // Observer is the live package-agnostic event sink, if this launch requested observation.
    // Observer 是当前启动请求观察时存在的活动包无关事件接收器。
    let Some(observer) = observer else {
        return;
    };
    // ShouldNotify reserves this callback before close without holding internal locks in host code.
    // ShouldNotify 会在关闭前预留当前回调，同时避免在宿主代码中持有内部锁。
    let should_notify = *lock_observer_notification_gate(observer_notifications_open);
    if should_notify {
        notify(observer.as_ref());
    }
}

/// Spawn a direct-child exit monitor that owns only a weak session reference.
/// 启动一个仅持有会话弱引用的直接子进程退出监视器。
/// `state` supplies non-reaping probes and `spawn_thread` performs fallible thread creation.
/// `state` 提供非回收式探测，`spawn_thread` 执行可失败线程创建。
///
/// Success leaves a detached bounded-poll watcher; failure retains ownership in the launch transaction.
/// 成功时留下分离式有界轮询监视器；失败时所有权仍保留在启动事务中。
fn spawn_direct_child_exit_watcher(
    state: &Arc<ManagedProcessSessionState>,
    spawn_thread: ManagedProcessBackgroundThreadSpawner,
) -> Result<(), String> {
    if state.observer.is_none() {
        return Ok(());
    }
    // WeakState ensures the watcher can never keep an otherwise abandoned process session alive.
    // WeakState 确保监视器永远不会让本应释放的进程会话继续存活。
    let weak_state = Arc::downgrade(state);
    // Task owns only a weak state reference so it cannot extend the process lifetime.
    // Task 只拥有状态弱引用，因此不会延长进程生命周期。
    let task: ManagedProcessBackgroundTask = Box::new(move || {
        loop {
            // State is upgraded only for one probe and dropped before the polling sleep.
            // State 仅在单次探测期间升级，并会在轮询休眠前释放。
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            if !state.observer_notifications_are_open() {
                return;
            }
            // Status probes the direct child independently of stdout and stderr EOF.
            // Status 独立于 stdout 与 stderr EOF 探测直接子进程。
            let status = match state.peek_status_snapshot() {
                Ok(status) => status,
                Err(error) => {
                    notify_managed_process_observer(
                        state.observer.as_ref(),
                        &state.observer_notifications_open,
                        ManagedProcessSessionObserver::failed,
                    );
                    crate::runtime_logging::warn(format!(
                        "[LuaSkill:warn] process.session exit watcher failed: {error}"
                    ));
                    return;
                }
            };
            if status.exited {
                notify_managed_process_observer(
                    state.observer.as_ref(),
                    &state.observer_notifications_open,
                    ManagedProcessSessionObserver::exited,
                );
                return;
            }
            drop(state);
            thread::sleep(Duration::from_millis(10));
        }
    });
    // SpawnResult reports operating-system thread creation failure to the launch transaction.
    // SpawnResult 把操作系统线程创建失败报告给启动事务。
    let spawn_result = spawn_thread(ManagedProcessBackgroundThread::ExitWatcher, task);
    match spawn_result {
        Ok(handle) => {
            // Handle is intentionally detached; WeakState guarantees prompt self-termination.
            // Handle 被有意分离；WeakState 保证其及时自行终止。
            drop(handle);
            Ok(())
        }
        Err(error) => Err(format!("spawn managed process exit watcher: {error}")),
    }
}

impl ProcessTreeController {
    /// Prepare one command to run inside an isolated process tree.
    /// 配置命令使其运行在隔离的进程树中。
    fn prepare_command(command: &mut Command) -> Result<bool, String> {
        #[cfg(unix)]
        {
            command.process_group(0);
            Ok(false)
        }
        #[cfg(windows)]
        {
            let in_job = current_process_is_in_job()?;
            let creation_flags = if in_job {
                CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB | CREATE_SUSPENDED
            } else {
                CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED
            };
            command.creation_flags(creation_flags);
            Ok(in_job)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
            Ok(false)
        }
    }

    /// Spawn one command that was already configured for process-tree isolation.
    /// 启动一个已经配置好进程树隔离选项的命令。
    fn spawn_prepared_command(
        command: &mut Command,
        breakaway_requested: bool,
    ) -> Result<Child, std::io::Error> {
        #[cfg(windows)]
        {
            match command.spawn() {
                Ok(child) => Ok(child),
                Err(error)
                    if breakaway_requested
                        && error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
                {
                    crate::runtime_logging::warn(
                        "[LuaSkill:warn] process.session falling back to inherited host job because CREATE_BREAKAWAY_FROM_JOB was denied"
                            .to_string(),
                    );
                    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
                    command.spawn()
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = breakaway_requested;
            command.spawn()
        }
    }

    /// Attach one freshly spawned child process to the current process-tree controller.
    /// 将一个刚启动的子进程接入当前进程树控制器。
    fn attach(child: &Child) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let job = WindowsProcessJob::create()?;
            job.assign(child)?;
            job.require_contains(child.as_raw_handle() as HANDLE, child.id())?;
            let detached_guard = job.create_detached_guard()?;
            // CREATE_SUSPENDED guarantees the root cannot create descendants before Job ownership.
            // CREATE_SUSPENDED 保证根进程在归属 Job 前无法创建后代。
            resume_suspended_windows_process(child.id())?;
            Ok(Self {
                job,
                detached_guard: Mutex::new(Some(detached_guard)),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self {
                detached_guard: Mutex::new(None),
            })
        }
    }

    /// Terminate the full process tree rooted at one managed child process.
    /// 终止由某个托管子进程作为根的整棵进程树。
    fn terminate(&self, _child: &Child) -> Result<(), String> {
        #[cfg(unix)]
        {
            let result = unsafe { libc::kill(-(_child.id() as i32), SIGKILL) };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ESRCH) {
                return Ok(());
            }
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(libc::EPERM) && macos_direct_child_has_exited(_child)? {
                return Ok(());
            }
            Err(format!("kill process group: {error}"))
        }
        #[cfg(windows)]
        {
            self.job.terminate_and_wait_empty()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = _child;
            Ok(())
        }
    }

    /// Transfer the pre-created asynchronous process-tree guard exactly once.
    /// 恰好一次转移预创建的异步进程树固定器。
    fn take_detached_guard(&self) -> Option<DetachedProcessTreeGuard> {
        self.detached_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Advance termination and query the retained asynchronous tree guard without transferring it.
    /// 在不转移所有权的情况下推进终止并查询所保留的异步进程树固定器。
    ///
    /// Returns `true` when no guard is needed on this platform or the guarded tree is empty.
    /// 当当前平台无需固定器，或固定器所辖进程树已经为空时返回 `true`。
    fn detached_tree_is_empty(&self) -> Result<bool, String> {
        let guard = self
            .detached_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_ref() {
            Some(guard) => guard.is_empty(),
            None => Ok(true),
        }
    }

    /// Release the unused asynchronous guard after synchronous tree convergence.
    /// 在同步进程树收敛后释放未使用的异步固定器。
    fn clear_detached_guard(&self) {
        let _released_guard = self.take_detached_guard();
    }
}

#[cfg(target_os = "macos")]
/// Return whether Darwin reports the direct child as exited without reaping its process identity.
/// 返回 Darwin 是否报告直接子进程已经退出，同时不回收其进程身份。
///
/// `child` is the still-owned process-group leader. The result distinguishes Darwin's `EPERM`
/// for an exited zombie-only group from a live process tree whose termination must remain retryable.
/// `child` 是仍被持有的进程组组长。返回值用于区分 Darwin 对仅含已退出僵尸进程的组返回的
/// `EPERM`，以及终止操作必须保持可重试的存活进程树。
fn macos_direct_child_has_exited(child: &Child) -> Result<bool, String> {
    // Info is initialized by waitid while WNOWAIT preserves the child for authoritative reaping.
    // Info 由 waitid 初始化，同时 WNOWAIT 会保留子进程，供后续权威回收。
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // Result performs a nonblocking identity-specific Darwin lifecycle query.
    // Result 执行一次非阻塞且绑定具体进程身份的 Darwin 生命周期查询。
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        // Error preserves every unexpected native lifecycle failure.
        // Error 保留每个非预期的原生生命周期失败。
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            return Ok(true);
        }
        return Err(format!("waitid after macOS process-group EPERM: {error}"));
    }
    // Info is initialized after successful waitid; a zero pid means the child is still running.
    // waitid 成功后 Info 已初始化；pid 为零表示子进程仍在运行。
    let info = unsafe { info.assume_init() };
    Ok(unsafe { info.si_pid() } != 0)
}

/// Terminate and reap a child whose process-tree attachment failed after spawn.
/// 终止并回收在启动后附加进程树失败的子进程。
fn terminate_spawned_child_after_attach_failure(
    mut child: Child,
    reaper_permit: DetachedChildReaperPermit,
    keepalive: Option<Box<dyn FnOnce() + Send>>,
) -> Result<(), String> {
    // TreeResult records whether the platform tree fallback reached every known descendant.
    // TreeResult 记录平台进程树回退是否处理到所有已知后代。
    #[cfg(unix)]
    let tree_result = {
        // Result is the process-group signal outcome for the freshly spawned group leader.
        // Result 是向刚启动进程组 leader 发送信号的结果。
        let result = unsafe { libc::kill(-(child.id() as i32), SIGKILL) };
        if result == 0 {
            Ok(())
        } else {
            // Error preserves a non-ESRCH process-group termination failure.
            // Error 保留非 ESRCH 的进程组终止失败。
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ESRCH) {
                Ok(())
            } else {
                Err(format!("kill unattached process group: {error}"))
            }
        }
    };
    // A Windows attach failure occurs before the CREATE_SUSPENDED root is resumed, so the direct
    // process is the complete tree and can be terminated without heuristic snapshot traversal.
    // Windows 附加失败发生在 CREATE_SUSPENDED 根进程恢复前，因此直接进程就是完整进程树，
    // 无需启发式快照遍历即可终止。
    #[cfg(windows)]
    let tree_result = match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => child.kill().map_err(|error| error.to_string()),
        Err(probe_error) => match child.kill() {
            Ok(()) => Err(format!(
                "probe unattached direct child before kill: {probe_error}"
            )),
            Err(kill_error) => Err(format!(
                "probe unattached direct child before kill: {probe_error}; kill: {kill_error}"
            )),
        },
    };
    // TreeResult falls back to direct-child termination on unsupported platforms.
    // TreeResult 在不受支持的平台上退回到直接子进程终止。
    #[cfg(not(any(unix, windows)))]
    let tree_result: Result<(), String> = child.kill().map_err(|error| error.to_string());

    // FallbackKillResult guarantees a direct kill attempt whenever full-tree termination failed.
    // FallbackKillResult 保证在完整进程树终止失败时仍尝试直接杀死子进程。
    let fallback_kill_result = if tree_result.is_err() {
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => child.kill().map_err(|error| error.to_string()),
            Err(probe_error) => match child.kill() {
                Ok(()) => Err(format!(
                    "probe direct child before fallback kill: {probe_error}"
                )),
                Err(kill_error) => Err(format!(
                    "probe direct child before fallback kill: {probe_error}; kill: {kill_error}"
                )),
            },
        }
    } else {
        Ok(())
    };
    // Bounded reap prevents an uninterruptible kernel wait from blocking the spawning thread.
    // 有界回收防止不可中断内核等待阻塞启动线程。
    let wait_result = wait_for_child_exit_until(
        &mut child,
        Instant::now() + FORCED_CHILD_REAP_TIMEOUT,
        "unattached direct child",
    )
    .map(|_| ());
    // A single bounded global reaper retains ownership after the synchronous deadline.
    // 同步截止时间后由单个有界全局回收器继续保留所有权。
    if wait_result.is_err() {
        reaper_permit.handoff_with_keepalive(child, keepalive);
    } else {
        drop(reaper_permit);
        drop(keepalive);
    }

    match (tree_result, fallback_kill_result, wait_result) {
        (Ok(()), Ok(()), Ok(())) | (Ok(()), Ok(()), Err(_)) => Ok(()),
        (tree_result, fallback_kill_result, wait_result) => {
            // Errors preserves every cleanup failure without hiding the first failed operation.
            // Errors 保留所有清理失败，且不隐藏首个失败操作。
            let mut errors = Vec::new();
            if let Err(error) = tree_result {
                errors.push(error);
            }
            if let Err(error) = fallback_kill_result {
                errors.push(format!("kill direct child: {error}"));
            }
            if let Err(error) = wait_result {
                errors.push(format!("wait direct child: {error}"));
            }
            Err(errors.join("; "))
        }
    }
}

/// Poll one directly owned child until it exits or one absolute deadline is reached.
/// 轮询一个直接拥有的子进程，直至其退出或到达绝对截止时间。
pub(crate) fn wait_for_child_exit_until(
    child: &mut Child,
    deadline: Instant,
    label: &str,
) -> Result<std::process::ExitStatus, String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("poll {label}: {error}"))?
        {
            return Ok(status);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(format!(
                "{label} did not exit before its forced-reap deadline"
            ));
        };
        thread::sleep(remaining.min(FORCED_CHILD_REAP_POLL));
    }
}

/// Initialize the global reaper and reserve one slot before any child process is spawned.
/// 在启动任何子进程前初始化全局回收器并预留一个槽位。
fn reserve_detached_child_reaper_slot() -> Result<DetachedChildReaperPermit, String> {
    let reaper = detached_child_reaper()?;
    reaper
        .reserved
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
            (reserved < DETACHED_CHILD_REAPER_CAPACITY).then_some(reserved + 1)
        })
        .map_err(|_| {
            format!("detached child reaper capacity exceeded: {DETACHED_CHILD_REAPER_CAPACITY}")
        })?;
    Ok(DetachedChildReaperPermit {
        reaper,
        reserved: true,
    })
}

/// Return the lazily initialized detached-child reaper or its stable startup error.
/// 返回延迟初始化的分离子进程回收器，或其稳定启动错误。
fn detached_child_reaper() -> Result<&'static DetachedChildReaper, String> {
    match DETACHED_CHILD_REAPER.get_or_init(|| {
        let queue = Arc::new(DetachedChildReaperQueue {
            children: Mutex::new(Vec::new()),
            changed: std::sync::Condvar::new(),
        });
        let reserved = Arc::new(AtomicUsize::new(0));
        let worker_queue = Arc::clone(&queue);
        let worker_reserved = Arc::clone(&reserved);
        thread::Builder::new()
            .name("luaskills-detached-child-reaper".to_string())
            .spawn(move || {
                loop {
                    let queue = Arc::clone(&worker_queue);
                    let reserved = Arc::clone(&worker_reserved);
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_detached_child_reaper(queue, reserved);
                    }))
                    .is_ok()
                    {
                        return;
                    }
                    crate::runtime_logging::warn(
                        "[LuaSkill:warn] detached child reaper recovered after an internal panic"
                            .to_string(),
                    );
                }
            })
            .map_err(|error| format!("spawn detached child reaper: {error}"))?;
        Ok(DetachedChildReaper { queue, reserved })
    }) {
        Ok(reaper) => Ok(reaper),
        Err(error) => Err(error.clone()),
    }
}

/// Poll all handed-off children without ever blocking the single reaper on one process.
/// 非阻塞轮询全部已移交子进程，绝不让单个进程阻塞唯一回收器。
fn run_detached_child_reaper(queue: Arc<DetachedChildReaperQueue>, reserved: Arc<AtomicUsize>) {
    loop {
        let mut children = queue
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while children.is_empty() {
            children = queue
                .changed
                .wait(children)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut reaped_children = Vec::new();
        let mut index = 0;
        while index < children.len() {
            let tree_empty = match children[index].process_tree_guard.as_ref() {
                Some(guard) => guard.is_empty(),
                None => Ok(true),
            };
            let tree_empty = match tree_empty {
                Ok(tree_empty) => tree_empty,
                Err(error) => {
                    if !children[index].poll_error_reported {
                        crate::runtime_logging::warn(format!(
                            "[LuaSkill:warn] detached child reaper retained a tree whose convergence could not be queried: {error}"
                        ));
                        children[index].poll_error_reported = true;
                    }
                    index += 1;
                    continue;
                }
            };
            match children[index].child.try_wait() {
                Ok(Some(_)) if tree_empty => {
                    reaped_children.push(children.swap_remove(index));
                }
                Ok(Some(_)) => index += 1,
                Ok(None) => index += 1,
                Err(error) => {
                    if !children[index].poll_error_reported {
                        crate::runtime_logging::warn(format!(
                            "[LuaSkill:warn] detached child reaper will retain and retry an unpollable killed child: {error}"
                        ));
                        children[index].poll_error_reported = true;
                    }
                    index += 1;
                }
            }
        }
        drop(children);
        for mut reaped in reaped_children {
            // `try_wait` already proved exit, so this final cached wait cannot block. Dropping the
            // record and its keepalive also happens outside the global handoff queue lock.
            // `try_wait` 已证明进程退出，因此最终缓存 wait 不会阻塞。记录及其保活动作的析构
            // 同样发生在全局交接队列锁之外。
            let _ = reaped.child.wait();
            reserved.fetch_sub(1, Ordering::AcqRel);
        }
        thread::sleep(FORCED_CHILD_REAP_POLL);
    }
}

#[cfg(windows)]
/// Return whether the current host process is already running inside one Job Object.
/// 返回当前宿主进程是否已经运行在某个 Job Object 中。
fn current_process_is_in_job() -> Result<bool, String> {
    let mut in_job = 0;
    let status = unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) };
    if status == 0 {
        return Err(format!(
            "IsProcessInJob: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(in_job != 0)
}

#[cfg(windows)]
/// Peek one Windows process handle status without reaping the owned `Child`.
/// 基于 Windows 进程句柄观察状态，而不提前 reap 持有的 `Child`。
fn peek_windows_process_status(handle: HANDLE) -> Result<ProcessStatusSnapshot, String> {
    let wait_status = unsafe { WaitForSingleObject(handle, 0) };
    match wait_status {
        WAIT_TIMEOUT => Ok(ProcessStatusSnapshot {
            running: true,
            exited: false,
            success: None,
            code: None,
        }),
        WAIT_OBJECT_0 => {
            let mut exit_code = 0_u32;
            let status = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
            if status == 0 {
                return Err(format!(
                    "GetExitCodeProcess: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let code = exit_code as i32;
            Ok(ProcessStatusSnapshot {
                running: false,
                exited: true,
                success: Some(code == 0),
                code: Some(code),
            })
        }
        WAIT_FAILED => Err(format!(
            "WaitForSingleObject(process status): {}",
            std::io::Error::last_os_error()
        )),
        other => Err(format!(
            "WaitForSingleObject(process status) returned unexpected status {other}"
        )),
    }
}

#[cfg(windows)]
/// Resume every initial thread of one CREATE_SUSPENDED process after Job attachment is proven.
/// 在证明 Job 归属后恢复单个 CREATE_SUSPENDED 进程的全部初始线程。
///
/// `process_id` is the exact identifier returned by the still-owned `Child` handle.
/// `process_id` 是仍由 `Child` 句柄拥有的精确进程标识。
///
/// Returns unit only after at least one matching thread had exactly one suspension removed.
/// 仅当至少一个匹配线程恰好移除一次挂起后返回空值。
fn resume_suspended_windows_process(process_id: u32) -> Result<(), String> {
    let deadline = Instant::now() + WINDOWS_PROCESS_TREE_CONVERGENCE_TIMEOUT;
    loop {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "CreateToolhelp32Snapshot(threads): {}",
                std::io::Error::last_os_error()
            ));
        }
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot as _) };
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut has_entry =
            unsafe { Thread32First(snapshot.as_raw_handle() as HANDLE, &mut entry) } != 0;
        if !has_entry {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(format!("Thread32First: {error}"));
            }
        }
        let mut resumed_threads = 0usize;
        loop {
            if has_entry && entry.th32OwnerProcessID == process_id {
                let raw_thread =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if raw_thread.is_null() {
                    return Err(format!(
                        "OpenThread({}, process {process_id}): {}",
                        entry.th32ThreadID,
                        std::io::Error::last_os_error()
                    ));
                }
                let thread_handle = unsafe { OwnedHandle::from_raw_handle(raw_thread as _) };
                let previous_count =
                    unsafe { ResumeThread(thread_handle.as_raw_handle() as HANDLE) };
                if previous_count == u32::MAX {
                    return Err(format!(
                        "ResumeThread({}, process {process_id}): {}",
                        entry.th32ThreadID,
                        std::io::Error::last_os_error()
                    ));
                }
                if previous_count != 1 {
                    return Err(format!(
                        "ResumeThread({}, process {process_id}) observed unexpected suspend count {previous_count}",
                        entry.th32ThreadID
                    ));
                }
                resumed_threads = resumed_threads.saturating_add(1);
            }
            if !has_entry {
                break;
            }
            has_entry =
                unsafe { Thread32Next(snapshot.as_raw_handle() as HANDLE, &mut entry) } != 0;
            if !has_entry {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                    return Err(format!("Thread32Next: {error}"));
                }
            }
        }
        if resumed_threads > 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "CREATE_SUSPENDED process {process_id} exposed no resumable thread before the {}ms deadline",
                WINDOWS_PROCESS_TREE_CONVERGENCE_TIMEOUT.as_millis()
            ));
        }
        thread::sleep(WINDOWS_PROCESS_TREE_CONVERGENCE_POLL);
    }
}

#[cfg(windows)]
impl WindowsProcessJob {
    /// Create a Job Object configured to kill all attached processes when closed.
    /// 创建一个在关闭时会杀掉所有附属进程的 Job Object。
    fn create() -> Result<Self, String> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(format!(
                "CreateJobObjectW: {}",
                std::io::Error::last_os_error()
            ));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let status = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle() as _,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if status == 0 {
            return Err(format!(
                "SetInformationJobObject: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { handle })
    }

    /// Duplicate the Job handle before child resume for guaranteed detached-reaper ownership.
    /// 在 child 恢复前重复 Job 句柄，以保证分离回收器所有权。
    fn create_detached_guard(&self) -> Result<DetachedProcessTreeGuard, String> {
        let process = unsafe { GetCurrentProcess() };
        let mut duplicate = std::ptr::null_mut();
        let status = unsafe {
            DuplicateHandle(
                process,
                self.handle.as_raw_handle() as HANDLE,
                process,
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if status == 0 || duplicate.is_null() {
            return Err(format!(
                "DuplicateHandle(managed Job): {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(DetachedProcessTreeGuard {
            job_handle: unsafe { OwnedHandle::from_raw_handle(duplicate as _) },
        })
    }

    /// Attach one direct child process to the current Job Object.
    /// 把一个直接子进程附着到当前 Job Object。
    ///
    /// `child` remains CREATE_SUSPENDED, so assignment failure cannot leave descendants behind.
    /// `child` 仍处于 CREATE_SUSPENDED，因此归属失败不可能遗留后代进程。
    ///
    /// Returns unit only when Windows accepts strict Job ownership; no snapshot fallback is allowed.
    /// 仅当 Windows 接受严格 Job 所有权时返回空值；不允许快照降级。
    fn assign(&self, child: &Child) -> Result<(), String> {
        let status = unsafe {
            AssignProcessToJobObject(self.handle.as_raw_handle() as _, child.as_raw_handle() as _)
        };
        if status == 0 {
            let error = std::io::Error::last_os_error();
            return Err(format!(
                "AssignProcessToJobObject(process {}): {error}",
                child.id()
            ));
        }
        Ok(())
    }

    /// Require one process handle to belong to this exact Job Object.
    /// 要求单个进程句柄属于当前精确 Job Object。
    ///
    /// `process` is a live process handle and `process_id` labels diagnostics.
    /// `process` 是活动进程句柄，`process_id` 用于标记诊断。
    ///
    /// Returns unit only after `IsProcessInJob` proves membership.
    /// 仅在 `IsProcessInJob` 证明成员关系后返回空值。
    fn require_contains(&self, process: HANDLE, process_id: u32) -> Result<(), String> {
        let mut in_job = 0;
        let status =
            unsafe { IsProcessInJob(process, self.handle.as_raw_handle() as HANDLE, &mut in_job) };
        if status == 0 {
            return Err(format!(
                "IsProcessInJob(process {process_id}): {}",
                std::io::Error::last_os_error()
            ));
        }
        if in_job == 0 {
            return Err(format!(
                "process {process_id} was not attached to its managed Job Object"
            ));
        }
        Ok(())
    }

    /// Query the number of processes still active in this exact Job Object.
    /// 查询当前精确 Job Object 中仍然活动的进程数量。
    ///
    /// The function has no parameters and returns the authoritative Job accounting count.
    /// 此函数不接收参数，并返回权威 Job 记账数量。
    fn active_processes(&self) -> Result<u32, String> {
        windows_job_active_processes(self.handle.as_raw_handle() as HANDLE)
    }

    /// Terminate the Job and prove it becomes empty under one absolute deadline.
    /// 终止 Job，并在单个绝对截止时间内证明其变为空。
    ///
    /// The function has no parameters and returns unit only after active-process accounting is zero.
    /// 此函数不接收参数；仅在活动进程记账归零后返回空值。
    fn terminate_and_wait_empty(&self) -> Result<(), String> {
        let status = unsafe { TerminateJobObject(self.handle.as_raw_handle() as _, 1) };
        let terminate_error = (status == 0).then(std::io::Error::last_os_error);
        let deadline = Instant::now() + WINDOWS_PROCESS_TREE_CONVERGENCE_TIMEOUT;
        loop {
            let active_processes = self.active_processes()?;
            if active_processes == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let primary_error = terminate_error
                    .as_ref()
                    .map(|error| format!("; TerminateJobObject failed: {error}"))
                    .unwrap_or_default();
                return Err(format!(
                    "managed Job Object still has {active_processes} active processes after {}ms{primary_error}",
                    WINDOWS_PROCESS_TREE_CONVERGENCE_TIMEOUT.as_millis()
                ));
            }
            thread::sleep(WINDOWS_PROCESS_TREE_CONVERGENCE_POLL);
        }
    }
}

impl DetachedProcessTreeGuard {
    /// Return whether every process in the retained tree has definitively exited.
    /// 返回所保留进程树中的全部进程是否已确定退出。
    fn is_empty(&self) -> Result<bool, String> {
        #[cfg(windows)]
        {
            if unsafe { TerminateJobObject(self.job_handle.as_raw_handle() as HANDLE, 1) } == 0 {
                return Err(format!(
                    "TerminateJobObject(detached reaper): {}",
                    std::io::Error::last_os_error()
                ));
            }
            windows_job_active_processes(self.job_handle.as_raw_handle() as HANDLE)
                .map(|active| active == 0)
        }
        #[cfg(not(windows))]
        {
            Ok(true)
        }
    }
}

/// Query authoritative active-process accounting for one Windows Job handle.
/// 查询单个 Windows Job 句柄的权威活动进程记账。
#[cfg(windows)]
fn windows_job_active_processes(handle: HANDLE) -> Result<u32, String> {
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let status = unsafe {
        QueryInformationJobObject(
            handle,
            JobObjectBasicAccountingInformation,
            &mut accounting as *mut _ as *mut core::ffi::c_void,
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status == 0 {
        return Err(format!(
            "QueryInformationJobObject: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(accounting.ActiveProcesses)
}

/// Append bytes while retaining only the newest bounded window.
/// 追加字节并只保留最新的有界窗口。
/// `buffer` receives `bytes`; `limit_bytes` bounds retention while cumulative counters remain monotonic.
/// `buffer` 接收 `bytes`；`limit_bytes` 限制保留量，同时累计计数保持单调。
fn append_bounded(buffer: &mut ManagedProcessOutputBuffer, bytes: &[u8], limit_bytes: usize) {
    // EffectiveLimit prevents an empty queue contract even for an invalid internal caller.
    // EffectiveLimit 即使面对无效内部调用方也会防止出现零容量队列契约。
    let effective_limit = limit_bytes.max(1);
    // IncomingCount records every byte received before any bounded retention decision.
    // IncomingCount 在执行任何有界保留决策前记录全部接收字节。
    let incoming_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    buffer.total_bytes = buffer.total_bytes.saturating_add(incoming_count);
    if bytes.len() >= effective_limit {
        // DroppedCount includes the old queue plus the incoming prefix outside the newest window.
        // DroppedCount 包含旧队列以及新输入中位于最新窗口之外的前缀。
        let dropped_count = buffer
            .bytes
            .len()
            .saturating_add(bytes.len().saturating_sub(effective_limit));
        buffer.dropped_bytes = buffer
            .dropped_bytes
            .saturating_add(u64::try_from(dropped_count).unwrap_or(u64::MAX));
        buffer.bytes.clear();
        buffer
            .bytes
            .extend(bytes[bytes.len() - effective_limit..].iter().copied());
        return;
    }
    // TotalLen is the retained queue size before enforcing the configured bound.
    // TotalLen 是执行配置上限前的保留队列长度。
    let total_len = buffer.bytes.len().saturating_add(bytes.len());
    if total_len > effective_limit {
        // Overflow is the exact number of oldest retained bytes that must be discarded.
        // Overflow 是必须丢弃的最旧保留字节精确数量。
        let overflow = total_len - effective_limit;
        // Drain removes the exact oldest prefix without per-byte temporary bindings.
        // Drain 会移除精确的最旧前缀，且不产生逐字节临时绑定。
        drop(buffer.bytes.drain(..overflow));
        buffer.dropped_bytes = buffer
            .dropped_bytes
            .saturating_add(u64::try_from(overflow).unwrap_or(u64::MAX));
    }
    buffer.bytes.extend(bytes.iter().copied());
}

/// Return one immutable diagnostic snapshot for a locked output buffer.
/// 返回单个已锁定输出缓冲区的不可变诊断快照。
/// `buffer` is already synchronized by the caller; the return value never mutates queue state.
/// `buffer` 已由调用方完成同步；返回值永远不会修改队列状态。
fn locked_output_buffer_stats(buffer: &ManagedProcessOutputBuffer) -> ManagedProcessOutputStats {
    ManagedProcessOutputStats {
        buffered_bytes: buffer.bytes.len(),
        total_bytes: buffer.total_bytes,
        dropped_bytes: buffer.dropped_bytes,
    }
}

/// Return current diagnostics for one shared output buffer.
/// 返回单个共享输出缓冲区的当前诊断信息。
/// `buffer` is locked internally and the returned counters form one consistent snapshot.
/// `buffer` 会在内部加锁，返回的计数器构成一份一致快照。
fn output_buffer_stats(
    buffer: &Arc<Mutex<ManagedProcessOutputBuffer>>,
) -> ManagedProcessOutputStats {
    // Guard protects one consistent counter and queue-length snapshot.
    // Guard 保护一份一致的计数器与队列长度快照。
    let guard = lock_session_output_buffer(buffer);
    locked_output_buffer_stats(&guard)
}

/// Drain bytes and return post-drain diagnostics from one shared output buffer.
/// 从单个共享输出缓冲区取出字节并返回取出后的诊断信息。
/// `max_bytes` bounds FIFO removal; the returned diagnostics are captured after that removal.
/// `max_bytes` 限制 FIFO 取出量；返回诊断会在该取出操作后捕获。
fn drain_output_buffer(
    buffer: &Arc<Mutex<ManagedProcessOutputBuffer>>,
    max_bytes: usize,
) -> (Vec<u8>, ManagedProcessOutputStats) {
    // Guard keeps draining and diagnostic capture in one atomic buffer operation.
    // Guard 让字节取出与诊断捕获处于同一个原子缓冲操作中。
    let mut guard = lock_session_output_buffer(buffer);
    // Count limits this read without modifying cumulative overflow counters.
    // Count 限制本次读取数量，且不会修改累计溢出计数器。
    let count = max_bytes.min(guard.bytes.len());
    // Drained preserves FIFO order while removing bytes from the ring front.
    // Drained 在从环形队列前端移除字节时保持 FIFO 顺序。
    let drained = (0..count)
        .filter_map(|_| guard.bytes.pop_front())
        .collect::<Vec<_>>();
    // Stats reflects remaining buffered bytes after the current drain.
    // Stats 反映本次取出后仍保留的缓冲字节。
    let stats = locked_output_buffer_stats(&guard);
    (drained, stats)
}

/// Build a checked monotonic deadline for one non-negative timeout.
/// 为单个非负超时构造经过检查的单调时钟截止点。
/// `timeout_ms` is validated against the portable finite bound; `field_name` identifies errors.
/// `timeout_ms` 会按可移植有限上限校验；`field_name` 用于标识错误。
fn checked_timeout_deadline(timeout_ms: u64, field_name: &str) -> Result<Instant, String> {
    if timeout_ms > MAX_SESSION_TIMEOUT_MS {
        return Err(format!(
            "{field_name} is too large; maximum is {MAX_SESSION_TIMEOUT_MS}"
        ));
    }
    Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or_else(|| format!("{field_name} is too large"))
}

/// Convert one optional `ExitStatus` into the lightweight snapshot shape used by Lua tables.
/// 将一个可选 `ExitStatus` 转换为 Lua 表使用的轻量状态快照。
fn process_status_snapshot_from_exit_status(
    status: Option<std::process::ExitStatus>,
) -> ProcessStatusSnapshot {
    match status {
        Some(status) => ProcessStatusSnapshot {
            running: false,
            exited: true,
            success: Some(status.success()),
            code: status.code(),
        },
        None => ProcessStatusSnapshot {
            running: true,
            exited: false,
            success: None,
            code: None,
        },
    }
}

/// Convert one lightweight process status snapshot into a Lua table.
/// 将一个轻量进程状态快照转换为 Lua 表。
fn process_status_snapshot_to_lua_table(
    lua: &Lua,
    status: &ProcessStatusSnapshot,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("running", status.running)?;
    table.set("exited", status.exited)?;
    match status.success {
        Some(success) => table.set("success", success)?,
        None => table.set("success", LuaValue::Nil)?,
    }
    match status.code {
        Some(code) => table.set("code", code)?,
        None => table.set("code", LuaValue::Nil)?,
    }
    Ok(table)
}

/// Add one stream's bounded-buffer diagnostics to a Lua result table.
/// 向 Lua 结果表添加单个流的有界缓冲诊断信息。
fn set_output_stats_on_lua_table(
    table: &Table,
    stream_name: &str,
    stats: ManagedProcessOutputStats,
) -> mlua::Result<()> {
    table.set(
        format!("{stream_name}_buffered_bytes"),
        stats.buffered_bytes,
    )?;
    table.set(format!("{stream_name}_total_bytes"), stats.total_bytes)?;
    table.set(format!("{stream_name}_dropped_bytes"), stats.dropped_bytes)?;
    Ok(())
}

/// Convert one pure Rust read result into the Lua session read contract.
/// 将一个纯 Rust 读取结果转换为 Lua 会话读取契约。
fn process_session_read_result_to_lua_table(
    lua: &Lua,
    read: ManagedProcessSessionReadResult,
) -> mlua::Result<Table> {
    // Result is the single table returned by the Lua read method.
    // Result 是 Lua read 方法返回的唯一表。
    let result = lua.create_table()?;
    result.set("stdout", read.stdout)?;
    result.set("stderr", read.stderr)?;
    result.set("stdout_encoding", read.stdout_encoding)?;
    result.set("stderr_encoding", read.stderr_encoding)?;
    result.set("stdout_lossy", read.stdout_lossy)?;
    result.set("stderr_lossy", read.stderr_lossy)?;
    result.set("stdout_base64", read.stdout_base64)?;
    result.set("stderr_base64", read.stderr_base64)?;
    result.set("timed_out", read.timed_out)?;
    set_output_stats_on_lua_table(&result, "stdout", read.stdout_stats)?;
    set_output_stats_on_lua_table(&result, "stderr", read.stderr_stats)?;
    Ok(result)
}

/// Convert one complete pure Rust core status into the Lua session status contract.
/// 将一个完整的纯 Rust 核心状态转换为 Lua 会话状态契约。
fn process_session_status_to_lua_table(
    lua: &Lua,
    status: &ManagedProcessSessionStatus,
) -> mlua::Result<Table> {
    // Result starts with the stable direct-child lifecycle fields.
    // Result 先写入稳定的直接子进程生命周期字段。
    let result = process_status_snapshot_to_lua_table(lua, &status.process)?;
    result.set("closed", status.closed)?;
    set_output_stats_on_lua_table(&result, "stdout", status.stdout)?;
    set_output_stats_on_lua_table(&result, "stderr", status.stderr)?;
    Ok(result)
}

/// Parse a required string field from a Lua table.
/// 从 Lua 表中解析必需字符串字段。
fn require_string_field(table: &Table, key: &str, fn_name: &str) -> mlua::Result<String> {
    let value: LuaValue = table.get(key)?;
    require_string_value(value, fn_name, key, false)
}

/// Parse an optional string field from a Lua table.
/// 从 Lua 表中解析可选字符串字段。
fn parse_optional_string_field(
    table: &Table,
    key: &str,
    fn_name: &str,
) -> mlua::Result<Option<String>> {
    let value: LuaValue = table.get(key)?;
    match value {
        LuaValue::Nil => Ok(None),
        value => Ok(Some(require_string_value(value, fn_name, key, false)?)),
    }
}

/// Parse an optional text encoding field from a Lua table.
/// 从 Lua 表中解析可选文本编码字段。
fn parse_optional_encoding_field(
    table: &Table,
    key: &str,
    fn_name: &str,
) -> mlua::Result<Option<RuntimeTextEncoding>> {
    let value: LuaValue = table.get(key)?;
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(text) => {
            let label = text
                .to_str()
                .map_err(|_| mlua::Error::runtime(format!("{fn_name}: {key} must be UTF-8")))?;
            RuntimeTextEncoding::parse(label.as_ref())
                .map(Some)
                .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {error}")))
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {key} must be a string, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Parse a string array field from a Lua table.
/// 从 Lua 表中解析字符串数组字段。
fn parse_string_array_field(table: &Table, key: &str, fn_name: &str) -> mlua::Result<Vec<String>> {
    let value: LuaValue = table.get(key)?;
    match value {
        LuaValue::Nil => Ok(Vec::new()),
        LuaValue::Table(items) => {
            let mut output = Vec::new();
            for pair in items.sequence_values::<LuaValue>() {
                output.push(require_string_value(pair?, fn_name, key, true)?);
            }
            Ok(output)
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {key} must be an array table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Parse an optional non-negative integer timeout from a Lua table.
/// 从 Lua 表中解析可选非负整数超时。
fn parse_optional_timeout_ms_field(
    table: &Table,
    key: &str,
    fn_name: &str,
    default_value: u64,
) -> mlua::Result<u64> {
    // Value is the untrusted Lua field validated without lossy numeric conversion.
    // Value 是未经信任的 Lua 字段，会在无损数值转换前完成校验。
    let value: LuaValue = table.get(key)?;
    match value {
        LuaValue::Nil => Ok(default_value),
        LuaValue::Integer(number) if number >= 0 => Ok(number as u64),
        LuaValue::Number(number)
            if number.is_finite()
                && number >= 0.0
                && number.fract() == 0.0
                && number <= u64::MAX as f64 =>
        {
            Ok(number as u64)
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {key} must be a non-negative integer, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Parse an optional positive u64 field from a Lua table.
/// 从 Lua 表中解析可选正数 u64 字段。
fn parse_optional_u64_field(
    table: &Table,
    key: &str,
    fn_name: &str,
    default_value: u64,
) -> mlua::Result<u64> {
    let value: LuaValue = table.get(key)?;
    match value {
        LuaValue::Nil => Ok(default_value),
        LuaValue::Integer(number) if number > 0 => Ok(number as u64),
        LuaValue::Number(number) if number.is_finite() && number > 0.0 => Ok(number as u64),
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {key} must be a positive number, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Parse an optional positive usize field from a Lua table.
/// 从 Lua 表中解析可选正数 usize 字段。
fn parse_optional_usize_field(
    table: &Table,
    key: &str,
    fn_name: &str,
    default_value: usize,
) -> mlua::Result<usize> {
    let value = parse_optional_u64_field(table, key, fn_name, default_value as u64)?;
    usize::try_from(value).map_err(|_| {
        mlua::Error::runtime(format!("{fn_name}: {key} is too large for this platform"))
    })
}

/// Convert one Lua value into text for session stdin writes.
/// 将一个 Lua 值转换为会话 stdin 写入文本。
fn lua_value_to_session_text(value: LuaValue, fn_name: &str) -> mlua::Result<String> {
    match value {
        LuaValue::String(text) => Ok(text
            .to_str()
            .map_err(|_| mlua::Error::runtime(format!("{fn_name}: string must be valid UTF-8")))?
            .to_string()),
        LuaValue::Integer(number) => Ok(number.to_string()),
        LuaValue::Number(number) => Ok(number.to_string()),
        LuaValue::Boolean(flag) => Ok(flag.to_string()),
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: unsupported value {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Require a Lua value to be a strict UTF-8 string.
/// 要求 Lua 值为严格 UTF-8 字符串。
fn require_string_value(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
    allow_blank: bool,
) -> mlua::Result<String> {
    let text = match value {
        LuaValue::String(text) => text
            .to_str()
            .map_err(|_| {
                mlua::Error::runtime(format!("{fn_name}: {param_name} must be valid UTF-8"))
            })?
            .to_string(),
        other => {
            return Err(mlua::Error::runtime(format!(
                "{fn_name}: {param_name} must be a string, got {}",
                lua_value_type_name(&other)
            )));
        }
    };
    if !allow_blank && text.trim().is_empty() {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must not be empty"
        )));
    }
    if text.contains('\0') {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must not contain NUL bytes"
        )));
    }
    Ok(text)
}

/// Return a compact Lua value type name for diagnostics.
/// 返回用于诊断的紧凑 Lua 值类型名。
fn lua_value_type_name(value: &LuaValue) -> &'static str {
    match value {
        LuaValue::Nil => "nil",
        LuaValue::Boolean(_) => "boolean",
        LuaValue::LightUserData(_) => "lightuserdata",
        LuaValue::Integer(_) | LuaValue::Number(_) => "number",
        LuaValue::String(_) => "string",
        LuaValue::Table(_) => "table",
        LuaValue::Function(_) => "function",
        LuaValue::Thread(_) => "thread",
        LuaValue::UserData(_) => "userdata",
        LuaValue::Error(_) => "error",
        LuaValue::Other(_) => "other",
    }
}

#[cfg(test)]
mod tests;
