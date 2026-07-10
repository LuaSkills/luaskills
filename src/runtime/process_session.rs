use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{Lua, MultiValue, Table, UserData, UserDataMethods, Value as LuaValue};

use crate::runtime::encoding::{RuntimeTextEncoding, decode_runtime_text, encode_runtime_text};

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
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, GetCurrentProcess, GetExitCodeProcess,
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    TerminateProcess, WaitForSingleObject,
};

const DEFAULT_SESSION_READ_TIMEOUT_MS: u64 = 100;
const DEFAULT_SESSION_CLOSE_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_SESSION_MAX_READ_BYTES: usize = 64 * 1024;
const DEFAULT_SESSION_BUFFER_LIMIT_BYTES: usize = 1024 * 1024;
#[cfg(windows)]
const WINDOWS_SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

/// Lightweight process status snapshot used by non-reaping status probes.
/// 由非 reap 状态探测返回的轻量进程状态快照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessStatusSnapshot {
    /// Whether the process is still running.
    /// 进程是否仍在运行。
    running: bool,
    /// Whether the process has exited.
    /// 进程是否已经退出。
    exited: bool,
    /// Optional success flag when the exit result is known.
    /// 退出结果已知时的可选成功标记。
    success: Option<bool>,
    /// Optional numeric exit code when the platform exposes one.
    /// 平台可提供数值退出码时的可选退出码。
    code: Option<i32>,
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
struct ProcessSessionReadRequest {
    /// Maximum wait time before returning available data.
    /// 返回可用数据前最多等待的时间。
    timeout_ms: u64,
    /// Maximum number of bytes drained per stream.
    /// 每个输出流最多取出的字节数。
    max_bytes: usize,
    /// Optional text marker that stops the wait when observed.
    /// 可选的文本标记，观察到后停止等待。
    until_text: Option<String>,
}

/// Close behavior requested by one process session close call.
/// 单次进程会话关闭调用请求的关闭行为。
struct ProcessSessionCloseRequest {
    /// Maximum graceful wait time before killing the process.
    /// 强制杀死进程前最多等待的优雅退出时间。
    timeout_ms: u64,
}

/// Shared mutable state owned by one managed process session.
/// 单个托管进程会话拥有的共享可变状态。
struct ManagedProcessSessionState {
    /// Spawned child process protected for status and lifecycle calls.
    /// 受保护的子进程，用于状态与生命周期调用。
    child: Mutex<Child>,
    /// Platform-specific process-tree controller used to kill descendants as one unit.
    /// 平台相关的进程树控制器，用于把派生进程作为一个整体回收。
    process_tree: ProcessTreeController,
    /// Optional stdin pipe used by write calls until closed.
    /// 写入调用使用的可选 stdin 管道，关闭后为空。
    stdin: Mutex<Option<ChildStdin>>,
    /// Accumulated stdout bytes drained by the background reader.
    /// 后台读取器排空并累计的 stdout 字节。
    stdout_buffer: Arc<Mutex<Vec<u8>>>,
    /// Accumulated stderr bytes drained by the background reader.
    /// 后台读取器排空并累计的 stderr 字节。
    stderr_buffer: Arc<Mutex<Vec<u8>>>,
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

/// Platform-specific process-tree controller retained for one managed session.
/// 单个托管会话保留的平台相关进程树控制器。
struct ProcessTreeController {
    #[cfg(windows)]
    strategy: WindowsProcessTreeStrategy,
}

#[cfg(windows)]
/// Windows Job Object wrapper that kills the entire process tree on termination or final drop.
/// Windows Job Object 封装，在终止或最终析构时杀掉整个进程树。
struct WindowsProcessJob {
    handle: OwnedHandle,
}

#[cfg(windows)]
/// Windows process-tree cleanup strategy selected for one managed session.
/// 为单个托管会话选择的 Windows 进程树清理策略。
enum WindowsProcessTreeStrategy {
    /// Dedicated Job Object attachment used when breakaway and assign both succeed.
    /// 当 breakaway 与 assign 都成功时使用专用 Job Object。
    Job(WindowsProcessJob),
    /// ToolHelp snapshot traversal used when the host job setup prevents reassignment.
    /// 当宿主 Job 环境阻止重新归属时，退回到 ToolHelp 快照遍历。
    Snapshot,
}

#[cfg(windows)]
/// One snapshot-tracked descendant entry whose handle is pinned during traversal.
/// 一个在快照遍历期间固定住句柄的后代进程条目。
struct WindowsSnapshotProcessEntry {
    /// Descendant process identifier captured from ToolHelp.
    /// 通过 ToolHelp 捕获到的后代进程标识。
    pid: u32,
    /// Optional pinned handle opened while walking the snapshot to prevent PID reuse races.
    /// 在遍历快照时立即打开的可选固定句柄，用于避免 PID 复用竞态。
    handle: Option<OwnedHandle>,
}

/// Rust-backed interactive process session exposed to Lua.
/// 暴露给 Lua 的 Rust 托管交互式进程会话。
#[derive(Clone)]
struct ManagedProcessSession {
    /// Shared session state behind the Lua userdata handle.
    /// Lua userdata 句柄背后的共享会话状态。
    state: Arc<ManagedProcessSessionState>,
}

/// Acquire one process-session output buffer and return its guard, recovering after lock poisoning.
/// 获取并返回单个进程会话输出缓冲区保护对象；如果锁已 poison，则恢复继续使用。
fn lock_session_output_buffer(buffer: &Arc<Mutex<Vec<u8>>>) -> MutexGuard<'_, Vec<u8>> {
    buffer
        .lock()
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
fn lock_session_child(child: &Mutex<Child>) -> MutexGuard<'_, Child> {
    child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ManagedProcessSession {
    /// Spawn a new managed process session from a parsed request.
    /// 根据解析后的请求启动一个新的托管进程会话。
    fn open(request: ProcessSessionOpenRequest) -> mlua::Result<Self> {
        let mut command = Command::new(&request.program);
        command.args(&request.args);
        let breakaway_requested = ProcessTreeController::prepare_command(&mut command)
            .map_err(|error| mlua::Error::runtime(format!("process.session.open: {error}")))?;
        if let Some(cwd) = request.cwd.as_deref() {
            command.current_dir(cwd);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            ProcessTreeController::spawn_prepared_command(&mut command, breakaway_requested)
                .map_err(|error| mlua::Error::runtime(format!("process.session.open: {error}")))?;
        let process_tree = ProcessTreeController::attach(&child)
            .map_err(|error| mlua::Error::runtime(format!("process.session.open: {error}")))?;

        let stdin = child.stdin.take();
        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let stdout_reader = child.stdout.take().map(|stdout| {
            spawn_session_pipe_reader(
                stdout,
                stdout_buffer.clone(),
                request.buffer_limit_bytes,
                "stdout",
            )
        });
        let stderr_reader = child.stderr.take().map(|stderr| {
            spawn_session_pipe_reader(
                stderr,
                stderr_buffer.clone(),
                request.buffer_limit_bytes,
                "stderr",
            )
        });

        Ok(Self {
            state: Arc::new(ManagedProcessSessionState {
                child: Mutex::new(child),
                process_tree,
                stdin: Mutex::new(stdin),
                stdout_buffer,
                stderr_buffer,
                stdout_encoding: request.stdout_encoding,
                stderr_encoding: request.stderr_encoding,
                stdin_encoding: request.stdin_encoding,
                stdout_reader: Mutex::new(stdout_reader),
                stderr_reader: Mutex::new(stderr_reader),
                closed: Mutex::new(false),
                final_status: Mutex::new(None),
            }),
        })
    }

    /// Write text to the process stdin using the configured input encoding.
    /// 使用配置的输入编码向进程 stdin 写入文本。
    fn write_values(&self, values: MultiValue) -> mlua::Result<bool> {
        let mut stdin = lock_session_stdin_pipe(&self.state.stdin);
        let stdin = stdin
            .as_mut()
            .ok_or_else(|| mlua::Error::runtime("process.session.write: stdin is closed"))?;
        for value in values {
            let text = lua_value_to_session_text(value, "process.session.write")?;
            let bytes = encode_runtime_text(&text, self.state.stdin_encoding)
                .map_err(|error| mlua::Error::runtime(format!("process.session.write: {error}")))?;
            stdin
                .write_all(&bytes)
                .map_err(|error| mlua::Error::runtime(format!("process.session.write: {error}")))?;
        }
        stdin
            .flush()
            .map_err(|error| mlua::Error::runtime(format!("process.session.write: {error}")))?;
        Ok(true)
    }

    /// Read and drain available stdout and stderr data into a Lua table.
    /// 读取并取出可用的 stdout 与 stderr 数据到 Lua 表中。
    fn read(&self, lua: &Lua, args: MultiValue) -> mlua::Result<Table> {
        let request = parse_session_read_request(args)?;
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
        let mut timed_out = false;
        loop {
            if self.has_readable_output(&request.until_text)? || self.output_streams_drained()? {
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let stdout_bytes = drain_buffer(&self.state.stdout_buffer, request.max_bytes)?;
        let stderr_bytes = drain_buffer(&self.state.stderr_buffer, request.max_bytes)?;
        let stdout = decode_runtime_text(&stdout_bytes, self.state.stdout_encoding);
        let stderr = decode_runtime_text(&stderr_bytes, self.state.stderr_encoding);

        let result = lua.create_table()?;
        result.set("stdout", stdout.text)?;
        result.set("stderr", stderr.text)?;
        result.set("stdout_encoding", stdout.encoding)?;
        result.set("stderr_encoding", stderr.encoding)?;
        result.set("stdout_lossy", stdout.lossy)?;
        result.set("stderr_lossy", stderr.lossy)?;
        result.set("stdout_base64", stdout.base64)?;
        result.set("stderr_base64", stderr.base64)?;
        result.set("timed_out", timed_out)?;
        Ok(result)
    }

    /// Return the current process status as a Lua table.
    /// 以 Lua 表返回当前进程状态。
    fn status(&self, lua: &Lua) -> mlua::Result<Table> {
        let status = self
            .state
            .peek_status_snapshot()
            .map_err(|error| mlua::Error::runtime(format!("process.session.status: {error}")))?;
        process_status_snapshot_to_lua_table(lua, &status)
    }

    /// Close stdin and wait briefly for process exit, killing on timeout.
    /// 关闭 stdin 并短暂等待进程退出，超时后强制杀死。
    fn close(&self, lua: &Lua, args: MultiValue) -> mlua::Result<Table> {
        let request = parse_session_close_request(args)?;
        self.close_stdin("process.session.close")?;

        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
        loop {
            if self
                .state
                .peek_status_snapshot()
                .map_err(|error| mlua::Error::runtime(format!("process.session.close: {error}")))?
                .exited
            {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        self.mark_closed("process.session.close")?;
        let final_status = self.kill_child()?;
        self.join_reader_threads("process.session.close")?;
        process_status_snapshot_to_lua_table(lua, &final_status)
    }

    /// Kill the child process and close this managed session.
    /// 杀死子进程并关闭此托管会话。
    fn kill(&self) -> mlua::Result<bool> {
        self.mark_closed("process.session.kill")?;
        let _ = self.kill_child()?;
        self.join_reader_threads("process.session.kill")?;
        Ok(true)
    }

    /// Return whether output buffers contain data or the requested marker.
    /// 返回输出缓冲区是否包含数据或请求的标记。
    fn has_readable_output(&self, until_text: &Option<String>) -> mlua::Result<bool> {
        let stdout = lock_session_output_buffer(&self.state.stdout_buffer);
        let stderr = lock_session_output_buffer(&self.state.stderr_buffer);
        if stdout.is_empty() && stderr.is_empty() {
            return Ok(false);
        }
        if let Some(marker) = until_text {
            let stdout_text = decode_runtime_text(&stdout, self.state.stdout_encoding).text;
            let stderr_text = decode_runtime_text(&stderr, self.state.stderr_encoding).text;
            return Ok(stdout_text.contains(marker) || stderr_text.contains(marker));
        }
        Ok(true)
    }

    /// Return whether all output reader threads have already reached EOF or failed.
    /// 返回所有输出读取线程是否都已经到达 EOF 或因错误退出。
    fn output_streams_drained(&self) -> mlua::Result<bool> {
        self.state
            .output_readers_drained()
            .map_err(|error| mlua::Error::runtime(format!("process.session.read: {error}")))
    }

    /// Kill the child process if it is still running.
    /// 如果进程树仍在运行则整体杀死它。
    fn kill_child(&self) -> mlua::Result<ProcessStatusSnapshot> {
        self.state
            .kill_process_tree_and_wait()
            .map_err(|error| mlua::Error::runtime(format!("process.session.kill: {error}")))
    }

    /// Close stdin by dropping the writable pipe.
    /// 通过丢弃可写管道来关闭 stdin。
    fn close_stdin(&self, operation_name: &str) -> mlua::Result<()> {
        self.state
            .close_stdin_pipe()
            .map_err(|error| mlua::Error::runtime(format!("{operation_name}: {error}")))
    }

    /// Mark this session closed for lifecycle tracking.
    /// 为生命周期跟踪将当前会话标记为已关闭。
    fn mark_closed(&self, operation_name: &str) -> mlua::Result<()> {
        self.state
            .mark_closed()
            .map_err(|error| mlua::Error::runtime(format!("{operation_name}: {error}")))
    }

    /// Join background pipe readers after the child process has stopped.
    /// 在子进程停止后等待后台管道读取线程退出。
    fn join_reader_threads(&self, operation_name: &str) -> mlua::Result<()> {
        self.state
            .join_reader_threads()
            .map_err(|error| mlua::Error::runtime(format!("{operation_name}: {error}")))
    }
}

impl ManagedProcessSessionState {
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
            return Ok(ProcessStatusSnapshot {
                running: false,
                exited: true,
                success,
                code,
            });
        }
        #[cfg(windows)]
        {
            let child = lock_session_child(&self.child);
            peek_windows_process_status(child.as_raw_handle() as HANDLE)
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            let mut child = lock_session_child(&self.child);
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
        if let Some(status) = self.cached_final_status()? {
            return Ok(status);
        }
        let mut child = lock_session_child(&self.child);
        self.process_tree.terminate(&child)?;
        let status = match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => Some(status),
            None => child.wait().map(Some).map_err(|error| error.to_string())?,
        };
        let snapshot = process_status_snapshot_from_exit_status(status);
        self.store_final_status(snapshot)?;
        Ok(snapshot)
    }

    /// Join one optional background reader thread after process shutdown.
    /// 在进程关闭后等待一个可选的后台读取线程退出。
    fn join_one_reader(
        handle: &Mutex<Option<SessionPipeReader>>,
        stream_name: &'static str,
    ) -> Result<(), String> {
        let should_take = {
            let mut reader_slot = lock_session_reader_slot(handle);
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
    fn cleanup_on_drop(&self) {
        let _ = self.mark_closed();
        let _ = self.close_stdin_pipe();
        if self.cached_final_status().ok().flatten().is_none() {
            let _ = self.kill_process_tree_and_wait();
        }
        let _ = self.join_reader_threads();
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
            let session = ManagedProcessSession::open(request)?;
            lua.create_userdata(session)
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
    let program = require_string_field(&table, "program", "process.session.open")?;
    let args = parse_string_array_field(&table, "args", "process.session.open")?;
    let cwd = parse_optional_string_field(&table, "cwd", "process.session.open")?;
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
fn parse_session_read_request(args: MultiValue) -> mlua::Result<ProcessSessionReadRequest> {
    let mut values = args.into_iter();
    let value = values.next().unwrap_or(LuaValue::Nil);
    match value {
        LuaValue::Nil => Ok(ProcessSessionReadRequest {
            timeout_ms: DEFAULT_SESSION_READ_TIMEOUT_MS,
            max_bytes: DEFAULT_SESSION_MAX_READ_BYTES,
            until_text: None,
        }),
        LuaValue::Table(table) => Ok(ProcessSessionReadRequest {
            timeout_ms: parse_optional_u64_field(
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
            timeout_ms: parse_optional_u64_field(
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

/// Spawn one detached pipe reader that appends bytes into a bounded buffer.
/// 启动一个分离的管道读取器，将字节追加到有界缓冲区。
fn spawn_session_pipe_reader<R>(
    mut reader: R,
    target: Arc<Mutex<Vec<u8>>>,
    limit_bytes: usize,
    stream_name: &'static str,
) -> SessionPipeReader
where
    R: Read + Send + 'static,
{
    let (done_tx, done_rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let done_flag = done.clone();
    let handle = thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    let mut buffer = lock_session_output_buffer(&target);
                    append_bounded(&mut buffer, &chunk[..count], limit_bytes);
                }
                Err(error) => {
                    crate::runtime_logging::warn(format!(
                        "[LuaSkill:warn] process.session {stream_name} reader failed: {error}"
                    ));
                    break;
                }
            }
        }
        done_flag.store(true, Ordering::Release);
        let _ = done_tx.send(());
    });
    SessionPipeReader {
        handle,
        done_rx,
        done,
    }
}

impl ProcessTreeController {
    /// Prepare one command to run inside an isolated process tree.
    /// 配置命令使其运行在隔离的进程树中。
    fn prepare_command(command: &mut Command) -> Result<bool, String> {
        #[cfg(unix)]
        {
            command.process_group(0);
            return Ok(false);
        }
        #[cfg(windows)]
        {
            let in_job = current_process_is_in_job()?;
            let creation_flags = if in_job {
                CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB
            } else {
                CREATE_NEW_PROCESS_GROUP
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
                    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
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
            match job.assign(child) {
                Ok(()) => Ok(Self {
                    strategy: WindowsProcessTreeStrategy::Job(job),
                }),
                Err(WindowsJobAssignError::AccessDenied(message)) => {
                    crate::runtime_logging::warn(format!(
                        "[LuaSkill:warn] process.session is reusing ToolHelp process-tree fallback because Job Object assignment was denied: {message}"
                    ));
                    Ok(Self {
                        strategy: WindowsProcessTreeStrategy::Snapshot,
                    })
                }
                Err(WindowsJobAssignError::Other(message)) => Err(message),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self {})
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
            return Err(format!("kill process group: {error}"));
        }
        #[cfg(windows)]
        {
            self.strategy.terminate(_child)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = _child;
            Ok(())
        }
    }
}

#[cfg(windows)]
/// Result classification for one Job Object assignment attempt.
/// 单次 Job Object 归属尝试的结果分类。
enum WindowsJobAssignError {
    /// Assignment failed because the process is already constrained by one outer host job.
    /// 由于进程已受外层宿主 Job 约束而导致的归属失败。
    AccessDenied(String),
    /// Assignment failed for another unexpected reason.
    /// 由于其他非预期原因导致的归属失败。
    Other(String),
}

#[cfg(windows)]
impl WindowsProcessTreeStrategy {
    /// Terminate the current managed process tree using the selected cleanup strategy.
    /// 使用当前选中的清理策略终止托管进程树。
    fn terminate(&self, child: &Child) -> Result<(), String> {
        match self {
            Self::Job(job) => job.terminate(),
            Self::Snapshot => terminate_windows_process_tree_snapshot(child),
        }
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
/// Terminate one process tree by walking the current process snapshot.
/// 通过遍历当前进程快照终止一棵进程树。
fn terminate_windows_process_tree_snapshot(child: &Child) -> Result<(), String> {
    let root_pid = child.id();
    let descendants = collect_windows_descendant_processes(root_pid)?;
    let mut first_error: Option<String> = None;
    for descendant in descendants.into_iter().rev() {
        if let Some(handle) = descendant.handle {
            let label = format!("process {}", descendant.pid);
            if let Err(error) =
                terminate_windows_process_handle(handle.as_raw_handle() as HANDLE, &label, false)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
    }
    if let Err(error) = terminate_windows_process_handle(
        child.as_raw_handle() as HANDLE,
        "process.session root process",
        true,
    ) && first_error.is_none()
    {
        first_error = Some(error);
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
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
/// Collect descendant process entries while pinning handles during the snapshot walk.
/// 在快照遍历期间固定住句柄并收集某个根进程的全部后代进程条目。
fn collect_windows_descendant_processes(
    root_pid: u32,
) -> Result<Vec<WindowsSnapshotProcessEntry>, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "CreateToolhelp32Snapshot: {}",
            std::io::Error::last_os_error()
        ));
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot as _) };
    // Capture the identity cutoff after the ToolHelp snapshot handle exists.
    // 在 ToolHelp 快照句柄建立之后再捕获身份截止时间。
    let snapshot_captured_ticks = current_windows_time_ticks()?;
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut children_by_parent =
        std::collections::HashMap::<u32, Vec<WindowsSnapshotProcessEntry>>::new();
    let mut has_entry =
        unsafe { Process32FirstW(snapshot.as_raw_handle() as HANDLE, &mut entry) } != 0;
    while has_entry {
        let pid = entry.th32ProcessID;
        children_by_parent
            .entry(entry.th32ParentProcessID)
            .or_default()
            .push(WindowsSnapshotProcessEntry {
                pid,
                handle: try_open_windows_process_for_snapshot(pid, snapshot_captured_ticks)?,
            });
        has_entry = unsafe { Process32NextW(snapshot.as_raw_handle() as HANDLE, &mut entry) } != 0;
    }

    let mut ordered = Vec::new();
    let mut stack = vec![root_pid];
    while let Some(parent_pid) = stack.pop() {
        if let Some(children) = children_by_parent.remove(&parent_pid) {
            for child in children {
                stack.push(child.pid);
                ordered.push(child);
            }
        }
    }
    Ok(ordered)
}

#[cfg(windows)]
/// Try opening one process while rejecting identities created after the snapshot started.
/// 尝试打开一个进程，并拒绝那些在快照开始后才创建的身份。
fn try_open_windows_process_for_snapshot(
    pid: u32,
    snapshot_started_ticks: u64,
) -> Result<Option<OwnedHandle>, String> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | WINDOWS_SYNCHRONIZE_ACCESS,
            0,
            pid,
        )
    };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code)
                if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_INVALID_PARAMETER as i32 =>
            {
                return Ok(None);
            }
            _ => return Err(format!("OpenProcess({pid}): {error}")),
        }
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle as _) };
    let created_ticks = get_windows_process_creation_ticks(handle.as_raw_handle() as HANDLE)?;
    if created_ticks > snapshot_started_ticks {
        return Ok(None);
    }
    Ok(Some(handle))
}

#[cfg(windows)]
/// Return current wall-clock time encoded as Windows FILETIME ticks.
/// 以 Windows FILETIME tick 格式返回当前墙钟时间。
fn current_windows_time_ticks() -> Result<u64, String> {
    const WINDOWS_TO_UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;
    let unix_elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("SystemTime before UNIX_EPOCH: {error}"))?;
    Ok(
        (unix_elapsed.as_secs() + WINDOWS_TO_UNIX_EPOCH_SECONDS) * 10_000_000
            + u64::from(unix_elapsed.subsec_nanos() / 100),
    )
}

#[cfg(windows)]
/// Return the process creation timestamp encoded as Windows FILETIME ticks.
/// 返回按 Windows FILETIME tick 编码的进程创建时间戳。
fn get_windows_process_creation_ticks(handle: HANDLE) -> Result<u64, String> {
    let mut creation_time = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit_time = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel_time = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user_time = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let status = unsafe {
        GetProcessTimes(
            handle,
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        )
    };
    if status == 0 {
        return Err(format!(
            "GetProcessTimes: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(((creation_time.dwHighDateTime as u64) << 32) | creation_time.dwLowDateTime as u64)
}

#[cfg(windows)]
/// Terminate one process handle and wait briefly for it to stop.
/// 终止一个进程句柄，并短暂等待其停止。
fn terminate_windows_process_handle(
    handle: HANDLE,
    label: &str,
    allow_timeout: bool,
) -> Result<(), String> {
    let terminate_status = unsafe { TerminateProcess(handle, 1) };
    if terminate_status == 0 {
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code)
                if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_INVALID_PARAMETER as i32 => {
            }
            _ => return Err(format!("TerminateProcess({label}): {error}")),
        }
    }

    let wait_status =
        unsafe { WaitForSingleObject(handle, DEFAULT_SESSION_CLOSE_TIMEOUT_MS as u32) };
    match wait_status {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT if allow_timeout => Ok(()),
        WAIT_TIMEOUT => Err(format!(
            "WaitForSingleObject({label}) timed out after {DEFAULT_SESSION_CLOSE_TIMEOUT_MS}ms"
        )),
        WAIT_FAILED => Err(format!(
            "WaitForSingleObject({label}): {}",
            std::io::Error::last_os_error()
        )),
        other => Err(format!(
            "WaitForSingleObject({label}) returned unexpected status {other}"
        )),
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

    /// Attach one direct child process to the current Job Object.
    /// 把一个直接子进程附着到当前 Job Object。
    fn assign(&self, child: &Child) -> Result<(), WindowsJobAssignError> {
        let status = unsafe {
            AssignProcessToJobObject(self.handle.as_raw_handle() as _, child.as_raw_handle() as _)
        };
        if status == 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(code) if code == ERROR_ACCESS_DENIED as i32 => {
                    Err(WindowsJobAssignError::AccessDenied(error.to_string()))
                }
                _ => Err(WindowsJobAssignError::Other(format!(
                    "AssignProcessToJobObject: {error}"
                ))),
            };
        }
        Ok(())
    }

    /// Terminate the whole Job Object process tree.
    /// 终止整个 Job Object 进程树。
    fn terminate(&self) -> Result<(), String> {
        let status = unsafe { TerminateJobObject(self.handle.as_raw_handle() as _, 1) };
        if status == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                return Ok(());
            }
            return Err(format!("TerminateJobObject: {error}"));
        }
        Ok(())
    }
}

/// Append bytes while retaining only the newest bounded window.
/// 追加字节并只保留最新的有界窗口。
fn append_bounded(buffer: &mut Vec<u8>, bytes: &[u8], limit_bytes: usize) {
    let limit_bytes = limit_bytes.max(1);
    if bytes.len() >= limit_bytes {
        buffer.clear();
        buffer.extend_from_slice(&bytes[bytes.len() - limit_bytes..]);
        return;
    }
    let total_len = buffer.len() + bytes.len();
    if total_len > limit_bytes {
        let overflow = total_len - limit_bytes;
        buffer.drain(0..overflow.min(buffer.len()));
    }
    buffer.extend_from_slice(bytes);
}

/// Drain up to a maximum number of bytes from one shared output buffer.
/// 从一个共享输出缓冲区取出最多指定数量的字节。
fn drain_buffer(buffer: &Arc<Mutex<Vec<u8>>>, max_bytes: usize) -> mlua::Result<Vec<u8>> {
    let mut guard = lock_session_output_buffer(buffer);
    let count = max_bytes.min(guard.len());
    Ok(guard.drain(0..count).collect())
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
