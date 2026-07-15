use super::*;
use crate::runtime::encoding::default_runtime_text_encoding;
use crate::runtime::test_support::process_env_test_guard;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicUsize;
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

/// Windows synchronize access required for non-destructive process-existence probes.
/// Windows 非破坏性进程存在性探测所需的同步访问权限。
#[cfg(windows)]
const WINDOWS_SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

/// Maximum startup allowance for a descendant probe under full-suite process pressure.
/// 全量测试进程压力下后代探针允许的最大启动时长。
const DESCENDANT_PROBE_START_TIMEOUT: Duration = Duration::from_secs(60);

/// Monotonic suffix that isolates descendant pid fixtures inside one parallel test process.
/// 在单个并行测试进程内隔离后代 pid 夹具的单调后缀。
static DESCENDANT_FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(1);

/// Verify persistent process sessions reject unsupported Windows verbatim program and cwd paths.
/// 验证持久进程会话会拒绝不受支持的 Windows verbatim 程序与 cwd 路径。
#[cfg(windows)]
#[test]
fn process_session_open_rejects_unsupported_windows_verbatim_namespaces() {
    // Lua state owning both request tables passed through the production parser.
    // 持有两个通过生产解析器请求表的 Lua 状态。
    let lua = Lua::new();
    // Volume namespace without an ordinary DOS/UNC equivalent.
    // 不存在普通 DOS/UNC 等价形式的卷命名空间。
    let volume_path = r"\\?\Volume{00000000-0000-0000-0000-000000000000}\runtime.exe";

    // Program request rejected before Command construction or process creation.
    // 在构造 Command 或创建进程前被拒绝的程序请求。
    let program_spec = lua.create_table().expect("create program request table");
    program_spec
        .set("program", volume_path)
        .expect("set unsupported program path");
    let program_error = parse_session_open_request(
        LuaValue::Table(program_spec),
        default_runtime_text_encoding(),
    )
    .err()
    .expect("unsupported program namespace must fail");
    assert!(
        program_error
            .to_string()
            .contains("unsupported Windows verbatim path namespace"),
        "unexpected program error: {program_error}"
    );

    // Cwd request rejected independently while a normal program name remains accepted.
    // 在普通程序名保持可接受时独立拒绝的 cwd 请求。
    let cwd_spec = lua.create_table().expect("create cwd request table");
    cwd_spec
        .set("program", "cmd.exe")
        .expect("set ordinary program name");
    cwd_spec
        .set("cwd", volume_path)
        .expect("set unsupported cwd path");
    let cwd_error =
        parse_session_open_request(LuaValue::Table(cwd_spec), default_runtime_text_encoding())
            .err()
            .expect("unsupported cwd namespace must fail");
    assert!(
        cwd_error
            .to_string()
            .contains("unsupported Windows verbatim path namespace"),
        "unexpected cwd error: {cwd_error}"
    );
}

/// A direct-child reap helper must honor its absolute deadline without calling blocking wait.
/// 直接子进程回收辅助函数必须遵守绝对截止时间，且不得调用阻塞式 wait。
#[test]
fn direct_child_reaping_is_bounded_by_absolute_deadline() {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
        command
    } else {
        let mut command = Command::new("sleep");
        command.arg("30");
        command
    };
    let mut child = command.spawn().expect("spawn bounded reap test child");
    let started = Instant::now();
    let error = wait_for_child_exit_until(
        &mut child,
        Instant::now() + Duration::from_millis(20),
        "bounded reap test child",
    )
    .expect_err("live child should outlast short reap deadline");
    assert!(error.contains("forced-reap deadline"));
    assert!(started.elapsed() < Duration::from_secs(1));
    child.kill().expect("kill bounded reap test child");
    wait_for_child_exit_until(
        &mut child,
        Instant::now() + Duration::from_secs(2),
        "killed bounded reap test child",
    )
    .expect("reap killed bounded reap test child");
}

/// Build one long-running process request used to verify drop-based cleanup.
/// 构建一个用于验证析构清理的长时间运行进程请求。
fn make_drop_cleanup_request() -> ProcessSessionOpenRequest {
    let encoding = default_runtime_text_encoding();
    if cfg!(windows) {
        ProcessSessionOpenRequest {
            program: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Start-Sleep -Seconds 30".to_string(),
            ],
            cwd: None,
            stdout_encoding: encoding,
            stderr_encoding: encoding,
            stdin_encoding: encoding,
            buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        }
    } else {
        ProcessSessionOpenRequest {
            program: "sleep".to_string(),
            args: vec!["30".to_string()],
            cwd: None,
            stdout_encoding: encoding,
            stderr_encoding: encoding,
            stdin_encoding: encoding,
            buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        }
    }
}

/// Build one process request whose direct child exits after spawning one descendant.
/// 构建一个直接子进程在拉起后代后立即退出的进程请求。
fn make_descendant_cleanup_request() -> (ProcessSessionOpenRequest, Option<PathBuf>) {
    let encoding = default_runtime_text_encoding();
    if cfg!(windows) {
        (ProcessSessionOpenRequest {
                program: "python".to_string(),
                args: vec![
                    "-c".to_string(),
                    "import subprocess, sys, time; child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); print(child.pid, flush=True); time.sleep(0.3)".to_string(),
                ],
                cwd: None,
                stdout_encoding: encoding,
                stderr_encoding: encoding,
                stdin_encoding: encoding,
                buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
            }, None)
    } else {
        // FixtureSequence gives each parallel Unix process-tree test a private pid file.
        // FixtureSequence 为每个并行 Unix 进程树测试提供私有 pid 文件。
        let fixture_sequence = DESCENDANT_FIXTURE_SEQUENCE.fetch_add(1, Ordering::AcqRel);
        // PidPath is an out-of-band identity channel independent of stdout-reader scheduling.
        // PidPath 是独立于 stdout reader 调度的带外身份通道。
        let pid_path = std::env::temp_dir().join(format!(
            "luaskills-process-descendant-{}-{fixture_sequence}.pid",
            std::process::id()
        ));
        let _ = fs::remove_file(&pid_path);
        (ProcessSessionOpenRequest {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "/bin/sleep 30 </dev/null >/dev/null 2>&1 & echo $!; echo $! > \"$1\"; /bin/sleep 0.3; exit 0".to_string(),
                "managed-descendant-fixture".to_string(),
                pid_path.to_string_lossy().into_owned(),
            ],
            cwd: None,
            stdout_encoding: encoding,
            stderr_encoding: encoding,
            stdin_encoding: encoding,
            buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        }, Some(pid_path))
    }
}

/// Build one process request whose direct child exits immediately.
/// 构建一个直接子进程会立即退出的进程请求。
fn make_immediate_exit_request() -> ProcessSessionOpenRequest {
    let encoding = default_runtime_text_encoding();
    if cfg!(windows) {
        ProcessSessionOpenRequest {
            program: "cmd".to_string(),
            args: vec!["/c".to_string(), "exit 0".to_string()],
            cwd: None,
            stdout_encoding: encoding,
            stderr_encoding: encoding,
            stdin_encoding: encoding,
            buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        }
    } else {
        ProcessSessionOpenRequest {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 0".to_string()],
            cwd: None,
            stdout_encoding: encoding,
            stderr_encoding: encoding,
            stdin_encoding: encoding,
            buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        }
    }
}

/// Convert one parsed test request into the configured command accepted by the pure Rust core.
/// 将一个已解析测试请求转换为纯 Rust 核心接受的已配置命令。
fn command_from_open_request(request: &ProcessSessionOpenRequest) -> Command {
    // Command carries only executable, arguments, and cwd; the core owns pipes and tree setup.
    // Command 只携带可执行文件、参数与 cwd；管道和进程树配置由核心拥有。
    let mut command = Command::new(&request.program);
    command.args(&request.args);
    if let Some(cwd) = request.cwd.as_deref() {
        command.current_dir(cwd);
    }
    command
}

/// Build the pure Rust launch options shared by focused core tests.
/// 构建聚焦核心测试共享的纯 Rust 启动选项。
fn make_core_launch_options(buffer_limit_bytes: usize) -> ManagedProcessSessionLaunchOptions {
    // Encoding is the platform default already used by the public Lua process session.
    // Encoding 是 Lua 公共进程会话已经使用的平台默认编码。
    let encoding = default_runtime_text_encoding();
    ManagedProcessSessionLaunchOptions {
        stdout_encoding: encoding,
        stderr_encoding: encoding,
        stdin_encoding: encoding,
        buffer_limit_bytes,
    }
}

/// Build one command that emits deterministic output and remains alive for inspection.
/// 构建一个输出确定内容并保持存活以供检查的命令。
fn make_output_then_sleep_command(output: &str) -> Command {
    if cfg!(windows) {
        // Script writes without a newline so overflow assertions are byte-exact on Windows.
        // Script 在 Windows 上不写换行，使溢出断言保持字节精确。
        let script = format!(
            "[Console]::Out.Write('{}'); [Console]::Out.Flush(); Start-Sleep -Seconds 30",
            output.replace('\'', "''")
        );
        // Command invokes the platform shell only as a deterministic test child.
        // Command 仅把平台 shell 作为确定性的测试子进程调用。
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", &script]);
        command
    } else {
        // Script quotes one test-controlled ASCII payload and then keeps the process group alive.
        // Script 引用一个测试控制的 ASCII 载荷，随后保持进程组存活。
        let script = format!("printf '%s' '{}'; sleep 30", output.replace('\'', "'\\''"));
        // Command invokes POSIX sh with a fixed test-only script.
        // Command 使用固定测试脚本调用 POSIX sh。
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        command
    }
}

/// Build one command whose direct child exits while a descendant keeps output pipes open.
/// 构建一个直接子进程退出、但后代继续持有输出管道的命令。
fn make_inherited_pipe_descendant_command() -> Command {
    if cfg!(windows) {
        // Command uses the Python runtime already required by Windows descendant lifecycle tests.
        // Command 使用 Windows 后代生命周期测试已经依赖的 Python 运行时。
        let mut command = Command::new("python");
        command.args([
            "-c",
            "import subprocess, sys; child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], stdin=subprocess.DEVNULL); print(child.pid, flush=True)",
        ]);
        command
    } else {
        // Command leaves stdout and stderr inherited by the sleeping POSIX descendant.
        // Command 让休眠中的 POSIX 后代继承 stdout 与 stderr。
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 </dev/null & echo $!; exit 0"]);
        command
    }
}

/// Wait until one core has received at least the requested stdout byte count.
/// 等待某个核心至少接收到请求数量的 stdout 字节。
fn wait_for_stdout_total(
    core: &ManagedProcessSessionCore,
    minimum_total_bytes: u64,
    timeout: Duration,
) {
    // Deadline bounds asynchronous pipe-reader startup and scheduling.
    // Deadline 为异步管道读取器启动与调度设置上限。
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // Status supplies the latest cumulative byte count without draining output.
        // Status 提供最新累计字节数，且不会取出输出。
        let status = core.status().expect("read managed core status");
        if status.stdout.total_bytes >= minimum_total_bytes {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("stdout should receive at least {minimum_total_bytes} bytes before timeout");
}

/// Thread-safe counters used to verify package-agnostic process observer delivery.
/// 用于验证包无关进程观察器投递行为的线程安全计数器。
#[derive(Default)]
struct CountingProcessSessionObserver {
    /// Number of stdout-readable callbacks received.
    /// 已接收的 stdout 可读回调数量。
    stdout_readable: AtomicUsize,
    /// Number of stderr-readable callbacks received.
    /// 已接收的 stderr 可读回调数量。
    stderr_readable: AtomicUsize,
    /// Number of direct-child exit callbacks received.
    /// 已接收的直接子进程退出回调数量。
    exited: AtomicUsize,
    /// Number of background failure callbacks received.
    /// 已接收的后台失败回调数量。
    failed: AtomicUsize,
}

impl ManagedProcessSessionObserver for CountingProcessSessionObserver {
    /// Record one stdout-readable callback.
    /// 记录一次 stdout 可读回调。
    fn stdout_readable(&self) {
        self.stdout_readable.fetch_add(1, Ordering::SeqCst);
    }

    /// Record one stderr-readable callback.
    /// 记录一次 stderr 可读回调。
    fn stderr_readable(&self) {
        self.stderr_readable.fetch_add(1, Ordering::SeqCst);
    }

    /// Record one direct-child exit callback.
    /// 记录一次直接子进程退出回调。
    fn exited(&self) {
        self.exited.fetch_add(1, Ordering::SeqCst);
    }

    /// Record one background failure callback.
    /// 记录一次后台失败回调。
    fn failed(&self) {
        self.failed.fetch_add(1, Ordering::SeqCst);
    }
}

/// Wait until one observer counter reaches the requested value.
/// 等待某个观察器计数器达到请求值。
/// `counter` is sampled until `minimum` is reached or `timeout` expires.
/// 持续采样 `counter`，直到达到 `minimum` 或 `timeout` 到期。
fn wait_for_observer_count(counter: &AtomicUsize, minimum: usize, timeout: Duration) {
    // Deadline bounds asynchronous reader and exit-watcher scheduling.
    // Deadline 为异步读取器与退出监视器调度设置上限。
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if counter.load(Ordering::SeqCst) >= minimum {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("observer counter should reach {minimum} before timeout");
}

/// Spawn every background role except one deterministically rejected test stage.
/// 启动除一个被确定性拒绝测试阶段之外的全部后台角色。
///
/// `role` and `task` are the production launch request; `rejected` selects failure injection.
/// `role` 与 `task` 是生产启动请求；`rejected` 选择失败注入点。
///
/// Return a forced creation error for the selected role or delegate to the real thread builder.
/// 对选中角色返回强制创建错误，否则委托给真实线程构造器。
fn spawn_background_thread_except(
    role: ManagedProcessBackgroundThread,
    task: ManagedProcessBackgroundTask,
    rejected: ManagedProcessBackgroundThread,
) -> Result<thread::JoinHandle<()>, std::io::Error> {
    if role == rejected {
        return Err(std::io::Error::other(format!(
            "forced {} creation failure",
            role.name()
        )));
    }
    spawn_managed_process_background_thread(role, task)
}

/// Reject stdout-reader creation while preserving real behavior for later stages.
/// 拒绝 stdout 读取器创建，同时为后续阶段保留真实行为。
fn fail_stdout_reader_spawn(
    role: ManagedProcessBackgroundThread,
    task: ManagedProcessBackgroundTask,
) -> Result<thread::JoinHandle<()>, std::io::Error> {
    spawn_background_thread_except(role, task, ManagedProcessBackgroundThread::StdoutReader)
}

/// Reject stderr-reader creation after the real stdout reader has started.
/// 在真实 stdout 读取器启动后拒绝 stderr 读取器创建。
fn fail_stderr_reader_spawn(
    role: ManagedProcessBackgroundThread,
    task: ManagedProcessBackgroundTask,
) -> Result<thread::JoinHandle<()>, std::io::Error> {
    spawn_background_thread_except(role, task, ManagedProcessBackgroundThread::StderrReader)
}

/// Reject exit-watcher creation after both real pipe readers have started.
/// 在两个真实管道读取器启动后拒绝退出监视器创建。
fn fail_exit_watcher_spawn(
    role: ManagedProcessBackgroundThread,
    task: ManagedProcessBackgroundTask,
) -> Result<thread::JoinHandle<()>, std::io::Error> {
    spawn_background_thread_except(role, task, ManagedProcessBackgroundThread::ExitWatcher)
}

/// Return whether one process exists without treating probe failures as successful exit.
/// 返回某个进程是否存在，且不会把探测失败当作成功退出。
fn process_exists(pid: u32) -> Result<bool, String> {
    #[cfg(windows)]
    {
        // Handle pins the selected process identity while its signaled state is inspected.
        // Handle 在检查信号状态期间固定所选进程身份。
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | WINDOWS_SYNCHRONIZE_ACCESS,
                0,
                pid,
            )
        };
        if handle.is_null() {
            // Error distinguishes an absent PID from a real process-probe failure.
            // Error 用于区分 PID 不存在与真实进程探测失败。
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(false);
            }
            return Err(format!("OpenProcess({pid}): {error}"));
        }
        // HandleOwner closes the native process handle on every return path.
        // HandleOwner 会在每条返回路径上关闭原生进程句柄。
        let handle_owner = unsafe { OwnedHandle::from_raw_handle(handle as _) };
        // WaitResult reports whether the process handle is already signaled.
        // WaitResult 报告进程句柄是否已经进入信号状态。
        let wait_result = unsafe { WaitForSingleObject(handle_owner.as_raw_handle() as HANDLE, 0) };
        match wait_result {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            WAIT_FAILED => Err(format!(
                "WaitForSingleObject({pid}): {}",
                std::io::Error::last_os_error()
            )),
            other => Err(format!(
                "WaitForSingleObject({pid}) returned unexpected status {other}"
            )),
        }
    }
    #[cfg(unix)]
    {
        // Result is the signal-free POSIX existence probe outcome.
        // Result 是不发送信号的 POSIX 存活探测结果。
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result == 0 {
            return Ok(true);
        }
        // Error distinguishes an absent process from permission and probe failures.
        // Error 用于区分进程不存在、权限限制与探测失败。
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(format!("kill({pid}, 0): {error}")),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!(
            "process existence probing is unsupported on {}",
            std::env::consts::OS
        ))
    }
}

/// Wait for one process id to disappear within the expected timeout.
/// 在预期超时时间内等待某个进程 id 消失。
fn assert_process_exits(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid).expect("probe process existence") {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("process {pid} should have exited after session drop");
}

/// Wait for one session to publish a descendant pid to stdout.
/// 等待某个会话把后代进程 pid 输出到 stdout。
fn wait_for_descendant_pid(
    session: &ManagedProcessSession,
    pid_path: Option<&Path>,
    timeout: Duration,
) -> u32 {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let stdout = session
            .core
            .state
            .stdout_buffer
            .lock()
            .expect("lock stdout buffer");
        if !stdout.bytes.is_empty() {
            let pid_lines = stdout
                .bytes
                .iter()
                .filter_map(|byte| match byte {
                    b'0'..=b'9' => Some(char::from(*byte)),
                    b'\r' | b'\n' => Some('\n'),
                    _ => None,
                })
                .collect::<String>();
            drop(stdout);
            for pid_text in pid_lines
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                if let Ok(pid) = pid_text.parse::<u32>() {
                    return pid;
                }
            }
        }
        if let Some(pid_path) = pid_path
            && let Ok(pid_text) = fs::read_to_string(pid_path)
            && let Ok(pid) = pid_text.trim().parse::<u32>()
        {
            return pid;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("descendant pid should be published before cleanup");
}

/// Wait until the direct child reports exit without reaping it.
/// 等待直接子进程报告退出，但暂不回收它。
fn wait_for_root_exit(session: &ManagedProcessSession, timeout: Duration) {
    // Deadline bounds the status polling loop without relying on one fixed sleep.
    // Deadline 为状态轮询循环设置上限，避免依赖一次固定休眠。
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if session
            .core
            .state
            .peek_status_snapshot()
            .expect("peek root process status")
            .exited
        {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("root process should exit before the lifecycle deadline");
}

/// Verify dropping the final session handle kills the child process.
/// 验证释放最后一个会话句柄时会杀掉子进程。
#[test]
fn dropping_process_session_kills_child_process() {
    // Hold the shared PATH guard while the test spawns and probes named executables.
    // 在测试按名称启动并探测可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let session = ManagedProcessSession::open(make_drop_cleanup_request())
        .expect("open drop cleanup session");
    let pid = session
        .core
        .state
        .child
        .lock()
        .expect("lock child process")
        .as_ref()
        .expect("drop cleanup child ownership")
        .id();
    assert!(
        process_exists(pid).expect("probe child before drop"),
        "child process should be running before drop"
    );

    drop(session);

    assert_process_exits(pid, Duration::from_secs(5));
}

/// Verify explicit teardown kills spawned descendants and releases reader threads promptly.
/// 验证显式清理会杀掉派生后代，并及时释放 reader 线程。
#[test]
fn killing_process_session_terminates_descendants_and_releases_readers() {
    // Hold the shared PATH guard while the test spawns and probes named executables.
    // 在测试按名称启动并探测可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let (request, pid_path) = make_descendant_cleanup_request();
    let session = ManagedProcessSession::open(request).expect("open descendant cleanup session");
    let descendant_pid = wait_for_descendant_pid(
        &session,
        pid_path.as_deref(),
        DESCENDANT_PROBE_START_TIMEOUT,
    );
    assert!(
        process_exists(descendant_pid).expect("probe descendant before cleanup"),
        "descendant process should be running before cleanup"
    );

    session
        .mark_closed("process.session.test")
        .expect("mark process session closed");
    let start = Instant::now();
    session
        .kill_child()
        .expect("kill descendant process tree cleanly");
    session
        .join_reader_threads("process.session.test")
        .expect("join process session readers");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "process session cleanup should not block after tree termination"
    );

    assert_process_exits(descendant_pid, Duration::from_secs(5));
    if let Some(pid_path) = pid_path {
        let _ = fs::remove_file(pid_path);
    }
}

#[cfg(windows)]
/// Verify the suspended root is resumed only after both root and later descendants inherit one Job.
/// 验证挂起根进程只在归属 Job 后恢复，且根进程与后续后代共同继承同一 Job。
#[test]
fn windows_suspended_root_and_descendant_share_managed_job() {
    // Hold the shared PATH guard while PowerShell and its descendant are launched and probed.
    // 启动并探测 PowerShell 及其后代期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let (request, pid_path) = make_descendant_cleanup_request();
    let session =
        ManagedProcessSession::open(request).expect("open Windows Job containment session");
    // Descendant output proves the CREATE_SUSPENDED root was resumed successfully.
    // 后代输出证明 CREATE_SUSPENDED 根进程已成功恢复。
    let descendant_pid = wait_for_descendant_pid(
        &session,
        pid_path.as_deref(),
        DESCENDANT_PROBE_START_TIMEOUT,
    );
    let child_guard = session
        .core
        .state
        .child
        .lock()
        .expect("lock Windows Job root child");
    let child = child_guard
        .as_ref()
        .expect("Windows Job root child ownership");
    session
        .core
        .state
        .process_tree
        .job
        .require_contains(child.as_raw_handle() as HANDLE, child.id())
        .expect("root process should belong to managed Job");
    let descendant_handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | WINDOWS_SYNCHRONIZE_ACCESS,
            0,
            descendant_pid,
        )
    };
    assert!(
        !descendant_handle.is_null(),
        "open managed descendant {descendant_pid}: {}",
        std::io::Error::last_os_error()
    );
    let descendant_handle = unsafe { OwnedHandle::from_raw_handle(descendant_handle as _) };
    session
        .core
        .state
        .process_tree
        .job
        .require_contains(descendant_handle.as_raw_handle() as HANDLE, descendant_pid)
        .expect("descendant process should inherit managed Job");
    drop(child_guard);

    session.kill().expect("terminate contained Windows Job");
    assert_process_exits(descendant_pid, Duration::from_secs(5));
}

/// Verify explicit tree teardown becomes idempotent after the direct child is reaped once.
/// 验证显式进程树清理在直接子进程完成一次回收后会变成幂等操作。
#[test]
fn process_session_tree_teardown_is_idempotent_after_explicit_kill() {
    // Hold the shared PATH guard while the test spawns a named executable.
    // 在测试按名称启动可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let session =
        ManagedProcessSession::open(make_drop_cleanup_request()).expect("open idempotent session");
    session
        .mark_closed("process.session.test")
        .expect("mark idempotent session closed");

    let first = session
        .kill_child()
        .expect("first process tree teardown should succeed");
    let second = session
        .kill_child()
        .expect("second process tree teardown should reuse cached final status");

    assert_eq!(first, second);
}

/// Verify cloned process-core handles retain one retryable strong process lifetime.
/// 验证克隆的进程核心句柄会保留一个可重试的强进程生命周期。
#[test]
fn cloned_managed_process_session_core_retains_strong_lifetime() {
    // Hold the shared PATH guard while the long-running child is active.
    // 长时间运行子进程存活期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Request supplies the named long-running child used for lifetime verification.
    // Request 提供用于生命周期验证的命名长时间运行子进程。
    let request = make_drop_cleanup_request();
    // Core is the initial strong owner of the managed process state.
    // Core 是托管进程状态的初始强所有者。
    let core = ManagedProcessSessionCore::launch(
        command_from_open_request(&request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch cloned managed core");
    // RetainedCore models the engine service's strong retry owner.
    // RetainedCore 模拟引擎服务持有的强重试所有者。
    let retained_core = core.clone();
    drop(core);
    assert!(
        retained_core
            .status()
            .expect("read retained managed core status")
            .process
            .running
    );
    retained_core.kill().expect("kill retained managed core");
}

/// Verify observer delivery covers readable data and direct-child exit without pipe EOF.
/// 验证观察器投递覆盖可读数据及不依赖管道 EOF 的直接子进程退出。
#[test]
fn managed_process_session_observer_is_lifecycle_bounded() {
    // Hold the shared PATH guard while the descendant process tree is active.
    // 后代进程树存活期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Observer records every package-agnostic callback emitted by this core.
    // Observer 记录当前核心发出的每个包无关回调。
    let observer = Arc::new(CountingProcessSessionObserver::default());
    // Core runs a root that exits while its descendant deliberately keeps both pipes open.
    // Core 运行一个会退出的根进程，其后代会有意继续持有两个管道。
    let core = ManagedProcessSessionCore::launch_with_observer(
        make_inherited_pipe_descendant_command(),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
        observer.clone(),
    )
    .expect("launch observed managed core");
    wait_for_observer_count(&observer.stdout_readable, 1, Duration::from_secs(5));
    wait_for_observer_count(&observer.exited, 1, Duration::from_secs(5));
    assert!(
        !core
            .state
            .output_readers_drained()
            .expect("inspect inherited output pipes"),
        "direct-child exit notification must not depend on output EOF"
    );
    core.close(0).expect("close observed managed core");
    // StdoutCount snapshots the callback total after close establishes the notification boundary.
    // StdoutCount 在 close 建立通知边界后记录回调总数快照。
    let stdout_count = observer.stdout_readable.load(Ordering::SeqCst);
    notify_managed_process_observer(
        core.state.observer.as_ref(),
        &core.state.observer_notifications_open,
        ManagedProcessSessionObserver::stdout_readable,
    );
    assert_eq!(
        observer.stdout_readable.load(Ordering::SeqCst),
        stdout_count
    );
    assert_eq!(observer.exited.load(Ordering::SeqCst), 1);
    assert_eq!(observer.failed.load(Ordering::SeqCst), 0);
}

/// Verify zero-timeout core reads return immediately for empty and already-readable buffers.
/// 验证核心零超时读取会对空缓冲与已有可读缓冲立即返回。
#[test]
fn process_session_core_zero_timeout_is_truly_non_blocking() {
    // Hold the shared PATH guard while both test children use named platform executables.
    // 两个测试子进程使用平台命名可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // EmptyRequest keeps one child alive without producing output.
    // EmptyRequest 保持一个子进程存活且不产生输出。
    let empty_request = make_drop_cleanup_request();
    // EmptyCore is launched directly through the pure Rust command API.
    // EmptyCore 直接通过纯 Rust 命令 API 启动。
    let empty_core = ManagedProcessSessionCore::launch(
        command_from_open_request(&empty_request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch empty-output managed core");
    // NonBlockingRead requests no waiting while still allowing normal drain limits.
    // NonBlockingRead 请求完全不等待，同时保留正常取出上限。
    let non_blocking_read = ManagedProcessSessionReadRequest {
        timeout_ms: 0,
        max_bytes: DEFAULT_SESSION_MAX_READ_BYTES,
        until_text: None,
    };
    // EmptyStart measures only the zero-wait core call.
    // EmptyStart 只测量核心零等待调用本身。
    let empty_start = Instant::now();
    // EmptyResult must report timeout without blocking or manufacturing output.
    // EmptyResult 必须在不阻塞且不伪造输出的情况下报告超时。
    let empty_result = empty_core
        .read(&non_blocking_read)
        .expect("read empty core without waiting");
    assert!(empty_start.elapsed() < Duration::from_millis(100));
    assert!(empty_result.timed_out);
    assert!(empty_result.stdout.is_empty());
    empty_core.kill().expect("kill empty-output managed core");

    // ReadyCore emits one fixed payload before remaining alive.
    // ReadyCore 在保持存活前输出一个固定载荷。
    let ready_core = ManagedProcessSessionCore::launch(
        make_output_then_sleep_command("ready"),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch ready-output managed core");
    wait_for_stdout_total(&ready_core, 5, Duration::from_secs(5));
    // ReadyStart measures the same zero-wait path with buffered data present.
    // ReadyStart 测量已有缓冲数据时的同一零等待路径。
    let ready_start = Instant::now();
    // ReadyResult must immediately drain the output already retained by the reader.
    // ReadyResult 必须立即取出读取器已经保留的输出。
    let ready_result = ready_core
        .read(&non_blocking_read)
        .expect("read ready core without waiting");
    assert!(ready_start.elapsed() < Duration::from_millis(100));
    assert!(!ready_result.timed_out);
    assert_eq!(ready_result.stdout, "ready");
    ready_core.kill().expect("kill ready-output managed core");
}

/// Verify both Lua-facing timeout parsers preserve an explicit zero value.
/// 验证两个 Lua 可见超时解析器都会保留显式零值。
#[test]
fn process_session_lua_timeout_parsers_accept_zero() {
    // Lua owns the option tables used by the same parsers as userdata methods.
    // Lua 拥有与 userdata 方法相同解析器使用的选项表。
    let lua = Lua::new();
    // ReadOptions carries the explicit non-blocking read timeout.
    // ReadOptions 携带显式非阻塞读取超时。
    let read_options = lua.create_table().expect("create read timeout options");
    read_options
        .set("timeout_ms", 0)
        .expect("set zero read timeout");
    // ReadArgs mirrors one Lua userdata method argument list.
    // ReadArgs 模拟一个 Lua userdata 方法参数列表。
    let mut read_args = MultiValue::new();
    read_args.push_back(LuaValue::Table(read_options));
    // ReadRequest proves the parser preserves the explicit zero value.
    // ReadRequest 证明解析器保留显式零值。
    let read_request = parse_session_read_request(read_args).expect("parse zero read timeout");
    assert_eq!(read_request.timeout_ms, 0);

    // CloseOptions carries the explicit immediate-close timeout.
    // CloseOptions 携带显式立即关闭超时。
    let close_options = lua.create_table().expect("create close timeout options");
    close_options
        .set("timeout_ms", 0)
        .expect("set zero close timeout");
    // CloseArgs mirrors one Lua close method argument list.
    // CloseArgs 模拟一个 Lua close 方法参数列表。
    let mut close_args = MultiValue::new();
    close_args.push_back(LuaValue::Table(close_options));
    // CloseRequest proves the close parser applies the same non-negative policy.
    // CloseRequest 证明 close 解析器应用相同的非负策略。
    let close_request = parse_session_close_request(close_args).expect("parse zero close timeout");
    assert_eq!(close_request.timeout_ms, 0);
}

/// Verify oversized timeouts return stable errors without mutating or panicking.
/// 验证超大超时会返回稳定错误，且不会修改状态或 panic。
#[test]
fn process_session_core_rejects_unrepresentable_timeout_deadlines() {
    // Hold the shared PATH guard while the long-running test child is active.
    // 长时间运行测试子进程存活期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Request supplies the named platform child used by the existing lifecycle tests.
    // Request 提供现有生命周期测试使用的平台命名子进程。
    let request = make_drop_cleanup_request();
    // Core remains alive after both rejected timeout operations.
    // Core 在两个被拒绝的超时操作后仍保持存活。
    let core = ManagedProcessSessionCore::launch(
        command_from_open_request(&request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch timeout validation core");
    // ReadRequest uses the maximum integer timeout that cannot form one Instant deadline.
    // ReadRequest 使用无法构造 Instant 截止点的最大整数超时。
    let read_request = ManagedProcessSessionReadRequest {
        timeout_ms: u64::MAX,
        max_bytes: 1,
        until_text: None,
    };
    // ReadError is the stable validation failure produced before any waiting occurs.
    // ReadError 是任何等待发生前产生的稳定校验错误。
    let read_error = core
        .read(&read_request)
        .expect_err("oversized read timeout should be rejected");
    assert!(read_error.contains("read timeout_ms is too large"));
    // CloseError is produced before stdin or lifecycle state is mutated.
    // CloseError 会在修改 stdin 或生命周期状态前产生。
    let close_error = core
        .close(u64::MAX)
        .expect_err("oversized close timeout should be rejected");
    assert!(close_error.contains("close timeout_ms is too large"));
    assert!(
        core.status()
            .expect("status after timeout errors")
            .process
            .running
    );
    core.kill().expect("kill timeout validation core");
}

/// Verify the VecDeque stream window exposes exact cumulative overflow diagnostics.
/// 验证 VecDeque 流窗口会暴露精确的累计溢出诊断。
#[test]
fn process_session_core_reports_bounded_output_drops() {
    // Hold the shared PATH guard while the deterministic output child is active.
    // 确定性输出子进程存活期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Core retains only the newest half of the sixteen-byte payload.
    // Core 只保留十六字节载荷中最新的一半。
    let core = ManagedProcessSessionCore::launch(
        make_output_then_sleep_command("abcdefghijklmnop"),
        make_core_launch_options(8),
    )
    .expect("launch bounded-output managed core");
    wait_for_stdout_total(&core, 16, Duration::from_secs(5));
    // Status exposes pre-drain retained, total, and dropped byte counts.
    // Status 暴露取出前的保留、累计与丢弃字节数。
    let status = core.status().expect("read bounded-output status");
    assert_eq!(status.stdout.buffered_bytes, 8);
    assert_eq!(status.stdout.total_bytes, 16);
    assert_eq!(status.stdout.dropped_bytes, 8);
    // Read drains the retained tail while preserving cumulative counters.
    // Read 取出保留的尾部，同时保持累计计数器。
    let read = core
        .read(&ManagedProcessSessionReadRequest {
            timeout_ms: 0,
            max_bytes: 8,
            until_text: None,
        })
        .expect("read bounded-output tail");
    assert_eq!(read.stdout, "ijklmnop");
    assert_eq!(read.stdout_stats.buffered_bytes, 0);
    assert_eq!(read.stdout_stats.total_bytes, 16);
    assert_eq!(read.stdout_stats.dropped_bytes, 8);
    core.kill().expect("kill bounded-output managed core");
}

/// Verify process-tree attach failure immediately terminates and reaps the spawned process.
/// 验证进程树附加失败会立即终止并回收已启动进程。
#[test]
fn process_session_core_attach_failure_cleans_spawned_child() {
    // Hold the shared PATH guard while the failing launch uses one named executable.
    // 失败启动使用命名可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Request describes a child that would leak for thirty seconds without rollback.
    // Request 描述一个若无回滚便会泄漏三十秒的子进程。
    let request = make_drop_cleanup_request();
    // SpawnedPid receives the child identity from the injected failing attacher.
    // SpawnedPid 从注入的失败附加器接收子进程身份。
    let spawned_pid = Arc::new(Mutex::new(None));
    // SpawnedPidForAttach moves one shared writer into the injected attacher.
    // SpawnedPidForAttach 把一个共享写入端移动到注入的附加器中。
    let spawned_pid_for_attach = spawned_pid.clone();
    // LaunchResult must contain the forced attach failure after completing rollback.
    // LaunchResult 必须在完成回滚后包含强制附加失败。
    let launch_result = ManagedProcessSessionCore::launch_with_attacher(
        command_from_open_request(&request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
        move |child| {
            *spawned_pid_for_attach
                .lock()
                .expect("store spawned child pid") = Some(child.id());
            Err("forced process-tree attach failure".to_string())
        },
    );
    // Error is extracted without requiring the successfully launched core to implement Debug.
    // Error 在不要求成功启动核心实现 Debug 的情况下取出。
    let error = match launch_result {
        Ok(unexpected_core) => {
            // UnexpectedCore is still torn down before failing the assertion path.
            // UnexpectedCore 在断言失败路径前仍会完成清理。
            drop(unexpected_core.kill());
            panic!("forced attach failure should reject launch");
        }
        Err(error) => error,
    };
    assert!(error.contains("forced process-tree attach failure"));
    // Pid is the exact spawned child identity that must already be absent.
    // Pid 是必须已经消失的精确已启动子进程标识。
    let pid = spawned_pid
        .lock()
        .expect("read spawned child pid")
        .expect("attacher should observe spawned child");
    assert_process_exits(pid, Duration::from_secs(5));
}

/// Verify every fallible background-thread stage rolls back the process tree and prior readers.
/// 验证每个可失败后台线程阶段都会回滚进程树与先前读取器。
#[test]
fn process_session_background_thread_spawn_failures_are_transactional() {
    // Hold the shared PATH guard while each failure case launches one named executable.
    // 每个失败用例启动命名可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Cases cover failure before readers, after one reader, and after both readers.
    // Cases 覆盖读取器启动前、一个读取器启动后与两个读取器启动后的失败。
    let cases: [(
        ManagedProcessBackgroundThread,
        ManagedProcessBackgroundThreadSpawner,
    ); 3] = [
        (
            ManagedProcessBackgroundThread::StdoutReader,
            fail_stdout_reader_spawn,
        ),
        (
            ManagedProcessBackgroundThread::StderrReader,
            fail_stderr_reader_spawn,
        ),
        (
            ManagedProcessBackgroundThread::ExitWatcher,
            fail_exit_watcher_spawn,
        ),
    ];
    for (failed_role, spawn_thread) in cases {
        // SpawnedPid captures the exact direct child that rollback must reap before returning.
        // SpawnedPid 捕获回滚返回前必须回收的精确直接子进程。
        let spawned_pid = Arc::new(Mutex::new(None));
        let spawned_pid_for_attach = spawned_pid.clone();
        // Observer must receive one failure notification before teardown closes its gate.
        // Observer 必须在清理关闭通知门前收到一次失败通知。
        let observer = Arc::new(CountingProcessSessionObserver::default());
        let request = make_drop_cleanup_request();
        let started_at = Instant::now();
        let launch_result = ManagedProcessSessionCore::launch_with_attacher_and_observer(
            command_from_open_request(&request),
            make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
            Some(observer.clone()),
            None,
            move |child| {
                *spawned_pid_for_attach
                    .lock()
                    .expect("store background-failure child pid") = Some(child.id());
                ProcessTreeController::attach(child)
            },
            spawn_thread,
        );
        // Error is extracted without requiring the owning core to implement Debug.
        // Error 在不要求拥有型核心实现 Debug 的情况下取出。
        let error = match launch_result {
            Ok(unexpected_core) => {
                drop(unexpected_core.kill());
                panic!(
                    "{} failure injection should reject launch",
                    failed_role.name()
                );
            }
            Err(error) => error,
        };
        assert!(
            error.contains(failed_role.name()),
            "unexpected background-thread error: {error}"
        );
        assert!(
            started_at.elapsed() < Duration::from_secs(5),
            "{} rollback should join prior readers promptly",
            failed_role.name()
        );
        let pid = spawned_pid
            .lock()
            .expect("read background-failure child pid")
            .expect("tree attacher should observe spawned child");
        assert_process_exits(pid, Duration::from_secs(5));
        assert_eq!(observer.failed.load(Ordering::SeqCst), 1);
    }
}

/// Verify userdata cleanup runs only after the shared core has stopped its process tree.
/// 验证 userdata 清理只会在共享核心停止进程树后运行。
#[test]
fn process_session_userdata_cleanup_runs_after_core_teardown() {
    // Hold the shared PATH guard while userdata owns one named long-running process.
    // userdata 拥有命名长时间运行进程期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Request supplies the long-running child whose state is checked by the cleanup callback.
    // Request 提供由清理回调检查状态的长时间运行子进程。
    let request = make_drop_cleanup_request();
    // Core is moved into userdata so its lifecycle matches production managed sessions.
    // Core 被移动进 userdata，使其生命周期与生产受管会话一致。
    let core = ManagedProcessSessionCore::launch(
        command_from_open_request(&request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch userdata cleanup core");
    // Pid is probed inside cleanup to establish teardown-before-cleanup ordering.
    // Pid 会在 cleanup 内部探测，用于建立先清理进程、后执行 cleanup 的顺序。
    let pid = lock_session_child(&core.state.child)
        .as_ref()
        .expect("userdata cleanup child ownership")
        .id();
    // Cleanup channel reports the process probe performed inside the one-shot callback.
    // Cleanup 通道报告单次回调内部执行的进程探测结果。
    let (cleanup_tx, cleanup_rx) = mpsc::channel();
    // Lua owns the userdata until its final handle is dropped and garbage collection runs.
    // Lua 拥有 userdata，直到最终句柄释放并执行垃圾回收。
    let lua = Lua::new();
    // Userdata owns the core and the one-shot cleanup callback under production semantics.
    // Userdata 按生产语义拥有核心与单次清理回调。
    let userdata = create_managed_process_session_userdata(
        &lua,
        core,
        Some(ManagedProcessSessionCleanupHandle::new(Box::new(
            move || {
                // Probe reports whether process termination completed before this callback began.
                // Probe 报告进程终止是否在当前回调开始前已经完成。
                let probe = process_exists(pid).map(|exists| !exists);
                // Send failure only means the asserting test receiver was already dropped.
                // Send 失败只表示执行断言的测试接收端已经释放。
                drop(cleanup_tx.send(probe));
            },
        ))),
        None,
    )
    .expect("create managed process userdata");
    drop(userdata);
    lua.gc_collect().expect("collect managed process userdata");
    // ExitedBeforeCleanup is the ordering fact observed inside the callback.
    // ExitedBeforeCleanup 是回调内部观察到的顺序事实。
    let exited_before_cleanup = cleanup_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("receive userdata cleanup result")
        .expect("probe process during userdata cleanup");
    assert!(
        exited_before_cleanup,
        "cleanup callback must observe a terminated process"
    );
}

/// Verify userdata drop preserves its cleanup when a reader join reports failure.
/// 验证读取器 join 报错时 userdata 析构会保留其 cleanup。
#[test]
fn process_session_userdata_drop_preserves_cleanup_after_teardown_failure() {
    // Hold the shared PATH guard while the test launches and terminates one named process.
    // 测试启动并终止命名进程期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let request = make_drop_cleanup_request();
    // RetainedCore models the service-owned strong retry handle surviving userdata drop.
    // RetainedCore 模拟在 userdata 析构后仍存活的服务强重试句柄。
    let core = ManagedProcessSessionCore::launch(
        command_from_open_request(&request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch cleanup-retention managed core");
    core.kill()
        .expect("complete real process teardown before injecting reader failure");
    let retained_core = core.clone();
    // DoneChannel allows join_one_reader to proceed directly to the synthetic panicking join.
    // DoneChannel 让 join_one_reader 直接进入合成 panic 线程的 join。
    let (done_tx, done_rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let done_for_thread = done.clone();
    let reader_handle = thread::Builder::new()
        .name("managed-process-test-panicking-reader".to_string())
        .spawn(move || {
            done_for_thread.store(true, Ordering::Release);
            let _ = done_tx.send(());
            panic!("forced reader join failure");
        })
        .expect("spawn synthetic panicking reader");
    *lock_session_reader_slot(&core.state.stdout_reader) = Some(SessionPipeReader {
        handle: reader_handle,
        done_rx,
        done,
    });
    // CleanupCount remains zero until the retained owner observes a fully successful retry.
    // CleanupCount 在保留所有者观察到完整成功重试前保持为零。
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let cleanup_count_for_callback = cleanup_count.clone();
    // RetryCount proves userdata Drop requests durable service work after the injected failure.
    // RetryCount 证明 userdata 析构在注入失败后会请求持久服务任务。
    let retry_count = Arc::new(AtomicUsize::new(0));
    let retry_count_for_callback = Arc::clone(&retry_count);
    let cleanup = ManagedProcessSessionCleanupHandle::new_with_retry(
        Box::new(move || {
            cleanup_count_for_callback.fetch_add(1, Ordering::SeqCst);
        }),
        Box::new(move || {
            retry_count_for_callback.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let session = ManagedProcessSession::from_core(core, Some(cleanup.clone()), None);
    drop(session);
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 0);
    assert_eq!(retry_count.load(Ordering::SeqCst), 1);
    // Retry succeeds because the failed JoinHandle was consumed without consuming cleanup.
    // 重试能够成功，因为失败的 JoinHandle 已被消费，而 cleanup 尚未被消费。
    retained_core
        .teardown_before_userdata_cleanup()
        .expect("retry teardown after synthetic reader failure");
    cleanup.run_once();
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

/// Verify explicit close releases attached lifecycle resources exactly once before userdata drop.
/// 验证显式关闭会在 userdata 析构前恰好一次释放附加生命周期资源。
#[test]
fn process_session_explicit_close_runs_userdata_cleanup_once() {
    // Shared PATH guard protects the named long-running process used by this lifecycle test.
    // 共享 PATH 保护锁用于保护当前生命周期测试使用的命名长时间运行进程。
    let _env_guard = process_env_test_guard();
    // Core remains alive through Lua userdata while close performs complete process-tree teardown.
    // Core 通过 Lua userdata 保持存活，close 会执行完整进程树清理。
    let request = make_drop_cleanup_request();
    let core = ManagedProcessSessionCore::launch(
        command_from_open_request(&request),
        make_core_launch_options(DEFAULT_SESSION_BUFFER_LIMIT_BYTES),
    )
    .expect("launch explicit cleanup core");
    let pid = lock_session_child(&core.state.child)
        .as_ref()
        .expect("explicit cleanup child ownership")
        .id();
    // Cleanup count and probe channel prove both one-shot execution and teardown ordering.
    // Cleanup 计数与探测通道同时证明单次执行及清理顺序。
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let cleanup_count_for_callback = Arc::clone(&cleanup_count);
    let (cleanup_tx, cleanup_rx) = mpsc::channel();
    let lua = Lua::new();
    let userdata = create_managed_process_session_userdata(
        &lua,
        core,
        Some(ManagedProcessSessionCleanupHandle::new(Box::new(
            move || {
                cleanup_count_for_callback.fetch_add(1, Ordering::SeqCst);
                drop(cleanup_tx.send(process_exists(pid).map(|exists| !exists)));
            },
        ))),
        Some(42),
    )
    .expect("create explicit cleanup userdata");
    lua.globals()
        .set("session", userdata.clone())
        .expect("store explicit cleanup userdata");
    // Status exposes the event-correlation identifier attached by managed runtime sessions.
    // Status 会暴露受管运行时会话附加的事件关联标识。
    let managed_session_id = lua
        .load("return session:status().managed_session_id")
        .eval::<u64>()
        .expect("read managed session correlation id");
    assert_eq!(managed_session_id, 42);

    lua.load("return session:close({ timeout_ms = 0 })")
        .eval::<LuaValue>()
        .expect("close managed process userdata");
    assert!(
        cleanup_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("receive explicit cleanup result")
            .expect("probe process during explicit cleanup")
    );
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);

    lua.globals()
        .set("session", LuaValue::Nil)
        .expect("clear explicit cleanup userdata");
    drop(userdata);
    lua.gc_collect()
        .expect("collect explicitly closed userdata");
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

/// Verify reentrant teardown detaches a reader instead of waiting for its current thread.
/// 验证重入清理会分离读取器，而不会等待当前读取器线程自身。
#[test]
fn join_one_reader_detaches_current_reader_thread() {
    // ReaderSlot is shared so the spawned thread can inspect its own installed JoinHandle.
    // ReaderSlot 由双方共享，使已启动线程可以检查自身已安装的 JoinHandle。
    let reader_slot = Arc::new(Mutex::new(None));
    // StartChannel prevents the thread from inspecting the slot before its handle is installed.
    // StartChannel 防止线程在自身句柄安装前检查槽位。
    let (start_tx, start_rx) = mpsc::channel();
    // ResultChannel reports the self-detach outcome without requiring an external join handle.
    // ResultChannel 在不需要外部 join 句柄的情况下报告自分离结果。
    let (result_tx, result_rx) = mpsc::channel();
    // ThreadReaderSlot transfers the shared reader slot into the spawned thread.
    // ThreadReaderSlot 把共享读取器槽位传入已启动线程。
    let thread_reader_slot = reader_slot.clone();
    // Handle belongs to the thread that will invoke join_one_reader reentrantly.
    // Handle 属于将重入调用 join_one_reader 的线程。
    let handle = thread::spawn(move || {
        start_rx.recv().expect("release self-detach reader");
        // Result must return promptly instead of waiting on the current thread's completion channel.
        // Result 必须立即返回，而不是等待当前线程自身的完成通道。
        let result = ManagedProcessSessionState::join_one_reader(&thread_reader_slot, "self");
        let _ = result_tx.send(result);
    });
    // DoneReceiver intentionally remains unsignaled because the self-detach branch must not wait.
    // DoneReceiver 被有意保持无信号，因为自分离分支不得等待。
    let (_done_tx, done_rx) = mpsc::channel();
    *reader_slot.lock().expect("install self reader handle") = Some(SessionPipeReader {
        handle,
        done_rx,
        done: Arc::new(AtomicBool::new(false)),
    });
    start_tx.send(()).expect("start self-detach reader");
    result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("receive self-detach result")
        .expect("detach current reader thread");
    assert!(
        reader_slot
            .lock()
            .expect("inspect self reader slot")
            .is_none()
    );
}

/// Verify reader timeout keeps the reader handle available for later retry.
/// 验证 reader 超时后仍保留句柄，方便后续重试清理。
#[test]
fn join_one_reader_timeout_preserves_reader_handle() {
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let done_flag = done.clone();
    let handle = thread::spawn(move || {
        release_rx.recv().expect("release test reader");
        done_flag.store(true, Ordering::Release);
        let _ = done_tx.send(());
    });
    let reader_slot = Mutex::new(Some(SessionPipeReader {
        handle,
        done_rx,
        done,
    }));

    let error = ManagedProcessSessionState::join_one_reader(&reader_slot, "test")
        .expect_err("reader join should time out before release");
    assert!(
        error.contains("timed out"),
        "timeout error should mention shutdown timeout, got: {error}"
    );
    assert!(
        reader_slot
            .lock()
            .expect("lock reader slot after timeout")
            .is_some(),
        "reader handle should stay available after timeout"
    );

    release_tx.send(()).expect("release test reader thread");
    ManagedProcessSessionState::join_one_reader(&reader_slot, "test")
        .expect("reader join should succeed after release");
    assert!(
        reader_slot
            .lock()
            .expect("lock reader slot after join")
            .is_none(),
        "reader handle should be removed after successful join"
    );
}

/// Verify output buffer reads recover after the shared buffer lock is poisoned.
/// 验证共享输出缓冲区锁 poison 后仍可恢复读取。
#[test]
fn process_session_output_buffer_recovers_after_poisoned_lock() {
    // Shared output buffer used to mimic stdout or stderr storage for one session.
    // 用于模拟单个会话 stdout 或 stderr 存储的共享输出缓冲区。
    let buffer = Arc::new(Mutex::new(ManagedProcessOutputBuffer {
        bytes: VecDeque::from(Vec::from(&b"ready"[..])),
        total_bytes: 5,
        dropped_bytes: 0,
    }));
    // Captured panic result from a holder that poisons only the output buffer lock.
    // 单个输出缓冲区锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the output buffer lock.
        // 仅用于制造输出缓冲区锁 poison 的保护对象。
        let _guard = buffer
            .lock()
            .expect("initial process session output buffer lock");
        panic!("poison process session output buffer for recovery test");
    }));

    assert!(poison_result.is_err());

    // Bytes drained through the production read helper after poison recovery.
    // 通过生产读取辅助函数在 poison 恢复后取出的字节。
    let (drained, stats) = drain_output_buffer(&buffer, 3);
    assert_eq!(drained, b"rea");
    assert_eq!(stats.buffered_bytes, 2);
    assert_eq!(stats.total_bytes, 5);
    assert_eq!(stats.dropped_bytes, 0);
}

/// Verify reader slot completion checks recover after the reader slot lock is poisoned.
/// 验证 reader 槽位锁 poison 后完成状态检查仍可恢复。
#[test]
fn process_session_reader_slot_recovers_after_poisoned_lock() {
    // Empty reader slot used to mimic a session stream without an active reader.
    // 用于模拟没有活动 reader 的会话流空槽位。
    let reader_slot: Mutex<Option<SessionPipeReader>> = Mutex::new(None);
    // Captured panic result from a holder that poisons only the reader slot lock.
    // 单个 reader 槽位锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the reader slot lock.
        // 仅用于制造 reader 槽位锁 poison 的保护对象。
        let _guard = reader_slot
            .lock()
            .expect("initial process session reader slot lock");
        panic!("poison process session reader slot for recovery test");
    }));

    assert!(poison_result.is_err());
    assert!(ManagedProcessSessionState::reader_completed(&reader_slot));
}

/// Verify lightweight lifecycle locks recover after poisoning during session cleanup.
/// 验证会话清理期间轻量生命周期锁 poison 后仍可恢复。
#[test]
fn process_session_lifecycle_state_locks_recover_after_poisoned_lock() {
    // Hold the shared PATH guard while the test spawns a named executable.
    // 在测试按名称启动可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Long-running session whose stdin, closed flag, and final status cache are poisoned and reused.
    // 会被制造 stdin、关闭标记和最终状态缓存 poison 并继续使用的长时间运行会话。
    let session = ManagedProcessSession::open(make_drop_cleanup_request())
        .expect("open lifecycle poison recovery session");

    // Captured panic result from a holder that poisons only the stdin pipe slot.
    // 单个 stdin 管道槽位锁持有者制造 poison 后被捕获的 panic 结果。
    let stdin_poison = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the stdin pipe slot lock.
        // 仅用于制造 stdin 管道槽位锁 poison 的保护对象。
        let _guard = session
            .core
            .state
            .stdin
            .lock()
            .expect("initial process session stdin lock");
        panic!("poison process session stdin for recovery test");
    }));
    assert!(stdin_poison.is_err());
    assert!(
        session
            .write_values(MultiValue::new())
            .expect("write through poisoned stdin lock")
    );
    session
        .close_stdin("process.session.test")
        .expect("close poisoned stdin lock");

    // Captured panic result from a holder that poisons only the closed flag.
    // 单个关闭标记锁持有者制造 poison 后被捕获的 panic 结果。
    let closed_poison = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the closed flag lock.
        // 仅用于制造关闭标记锁 poison 的保护对象。
        let _guard = session
            .core
            .state
            .closed
            .lock()
            .expect("initial process session closed lock");
        panic!("poison process session closed flag for recovery test");
    }));
    assert!(closed_poison.is_err());
    session
        .mark_closed("process.session.test")
        .expect("mark closed through poisoned closed flag");

    // Captured panic result from a holder that poisons only the final status cache.
    // 单个最终状态缓存锁持有者制造 poison 后被捕获的 panic 结果。
    let final_status_poison = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the final status cache lock.
        // 仅用于制造最终状态缓存锁 poison 的保护对象。
        let _guard = session
            .core
            .state
            .final_status
            .lock()
            .expect("initial process session final status lock");
        panic!("poison process session final status for recovery test");
    }));
    assert!(final_status_poison.is_err());

    // Captured panic poisons only the one-shot process-tree termination flag.
    // 捕获到的 panic 只会 poison 单次进程树终止标记。
    let tree_terminated_poison = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard is held solely to verify termination still proceeds after poison recovery.
        // Guard 仅用于验证 poison 恢复后进程树终止仍会继续执行。
        let _guard = session
            .core
            .state
            .process_tree_terminated
            .lock()
            .expect("initial process tree terminated lock");
        panic!("poison process tree terminated flag for recovery test");
    }));
    assert!(tree_terminated_poison.is_err());

    // Final status returned by the normal kill path after final-status lock recovery.
    // 最终状态锁恢复后通过正常 kill 路径返回的终态状态。
    let killed_status = session
        .kill_child()
        .expect("kill child through poisoned final status cache");
    // Cached final status read back through the recovered cache lock.
    // 通过已恢复缓存锁回读到的最终状态。
    let cached_status = session
        .core
        .state
        .cached_final_status()
        .expect("read poisoned final status cache");
    assert_eq!(cached_status, Some(killed_status));
    session
        .join_reader_threads("process.session.test")
        .expect("join readers after lifecycle poison recovery");
}

/// Verify child process lifecycle operations recover after the child lock is poisoned.
/// 验证子进程生命周期操作在 child 锁 poison 后仍可恢复。
#[test]
fn process_session_child_lock_recovers_after_poisoned_lock() {
    // Hold the shared PATH guard while the test spawns a named executable.
    // 在测试按名称启动可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    // Long-running session whose child mutex is poisoned before status and kill operations.
    // 在 status 与 kill 操作前制造 child 互斥锁 poison 的长时间运行会话。
    let session = ManagedProcessSession::open(make_drop_cleanup_request())
        .expect("open child poison session");

    // Captured panic result from a holder that poisons only the child process lock.
    // 单个子进程锁持有者制造 poison 后被捕获的 panic 结果。
    let child_poison = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the child process lock.
        // 仅用于制造子进程锁 poison 的保护对象。
        let _guard = session
            .core
            .state
            .child
            .lock()
            .expect("initial process session child lock");
        panic!("poison process session child for recovery test");
    }));
    assert!(child_poison.is_err());

    // Status snapshot read through the recovered child lock before process teardown.
    // 进程清理前通过已恢复 child 锁读取到的状态快照。
    let status = session
        .core
        .state
        .peek_status_snapshot()
        .expect("peek status through poisoned child lock");
    assert!(status.running || !status.exited);

    // Final status returned after kill and wait use the recovered child lock.
    // kill 与 wait 使用已恢复 child 锁后返回的最终状态。
    let killed_status = session
        .kill_child()
        .expect("kill through poisoned child lock");
    assert!(killed_status.exited);
    session
        .join_reader_threads("process.session.test")
        .expect("join readers after child poison recovery");
}

/// Verify close() keeps the child unreaped until tree cleanup completes.
/// 验证 close() 会在进程树清理完成前保持子进程未被提前 reap。
#[test]
fn closing_process_session_after_child_exit_still_cleans_descendants() {
    // Hold the shared PATH guard while the test spawns and probes named executables.
    // 在测试按名称启动并探测可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let lua = Lua::new();
    let (request, pid_path) = make_descendant_cleanup_request();
    let session =
        ManagedProcessSession::open(request).expect("open close descendant cleanup session");
    let descendant_pid = wait_for_descendant_pid(
        &session,
        pid_path.as_deref(),
        DESCENDANT_PROBE_START_TIMEOUT,
    );
    wait_for_root_exit(&session, Duration::from_secs(5));
    assert!(
        process_exists(descendant_pid).expect("probe descendant before close cleanup"),
        "descendant process should be running before close cleanup"
    );

    // ReapedStatus models an exit monitor that has already reaped and cached the direct child.
    // ReapedStatus 模拟已经回收并缓存直接子进程状态的退出监视器。
    let reaped_status = {
        // ChildGuard serializes direct-child wait with every other lifecycle operation.
        // ChildGuard 将直接子进程 wait 与其他生命周期操作串行化。
        let mut child_guard = lock_session_child(&session.core.state.child);
        child_guard
            .as_mut()
            .expect("exited root child ownership")
            .wait()
            .map(Some)
            .map(process_status_snapshot_from_exit_status)
            .expect("reap exited root process")
    };
    session
        .core
        .state
        .store_final_status(reaped_status)
        .expect("cache reaped root process status");

    let start = Instant::now();
    let status = session
        .close(&lua, MultiValue::new())
        .expect("close descendant cleanup session");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "process.session.close should not block after descendant cleanup"
    );
    let exited: bool = status.get("exited").expect("read close exited flag");
    assert!(exited, "close should report one exited process status");
    assert_process_exits(descendant_pid, Duration::from_secs(5));
    if let Some(pid_path) = pid_path {
        let _ = fs::remove_file(pid_path);
    }
}

/// Verify read() keeps waiting for descendant output even after the root process exits.
/// 验证 read() 会在根进程退出后继续等待后代进程输出。
#[test]
fn read_waits_for_descendant_output_after_root_exit() {
    // Hold the shared PATH guard while the test spawns a named executable.
    // 在测试按名称启动可执行文件期间持有共享 PATH 保护锁。
    let _env_guard = process_env_test_guard();
    let lua = Lua::new();
    let session = ManagedProcessSession::open(make_immediate_exit_request())
        .expect("open immediate exit session");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if session
            .core
            .state
            .peek_status_snapshot()
            .expect("peek immediate exit status")
            .exited
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        session
            .core
            .state
            .peek_status_snapshot()
            .expect("recheck immediate exit status")
            .exited,
        "immediate exit process should finish before read regression check"
    );
    session
        .core
        .state
        .join_reader_threads()
        .expect("join real readers before installing test readers");

    let install_test_reader = || -> (SessionPipeReader, mpsc::Sender<()>, Arc<AtomicBool>) {
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let done_flag = done.clone();
        let handle = thread::spawn(move || {
            release_rx.recv().expect("release synthetic session reader");
            done_flag.store(true, Ordering::Release);
            let _ = done_tx.send(());
        });
        (
            SessionPipeReader {
                handle,
                done_rx,
                done: done.clone(),
            },
            release_tx,
            done,
        )
    };
    let (stdout_reader, stdout_release_tx, _) = install_test_reader();
    let (stderr_reader, stderr_release_tx, _) = install_test_reader();
    *session
        .core
        .state
        .stdout_reader
        .lock()
        .expect("lock stdout reader slot for synthetic install") = Some(stdout_reader);
    *session
        .core
        .state
        .stderr_reader
        .lock()
        .expect("lock stderr reader slot for synthetic install") = Some(stderr_reader);

    let stdout_buffer = session.core.state.stdout_buffer.clone();
    let release_producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        let mut buffer = stdout_buffer
            .lock()
            .expect("lock stdout buffer for synthetic descendant output");
        append_bounded(
            &mut buffer,
            b"child-ready\n",
            DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        );
        drop(buffer);
        stdout_release_tx
            .send(())
            .expect("release synthetic stdout reader");
        stderr_release_tx
            .send(())
            .expect("release synthetic stderr reader");
    });
    let options = lua.create_table().expect("create read options");
    options.set("timeout_ms", 3_000).expect("set read timeout");
    options
        .set("until_text", "child-ready")
        .expect("set read marker");

    let mut args = MultiValue::new();
    args.push_back(LuaValue::Table(options));
    let result = session.read(&lua, args).expect("read descendant output");
    let stdout: String = result.get("stdout").expect("read stdout text");
    let timed_out: bool = result.get("timed_out").expect("read timed_out flag");

    assert!(
        !timed_out,
        "read should finish from descendant output instead of timing out"
    );
    assert!(
        stdout.contains("child-ready"),
        "read should capture descendant output after root exit, got: {stdout:?}"
    );

    release_producer
        .join()
        .expect("join synthetic descendant output producer");
    session
        .core
        .state
        .join_reader_threads()
        .expect("join synthetic session readers");
}
