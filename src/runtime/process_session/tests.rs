use super::*;
use crate::runtime::encoding::default_runtime_text_encoding;
use crate::runtime::test_support::process_env_test_guard;
use std::panic::{self, AssertUnwindSafe};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

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
fn make_descendant_cleanup_request() -> ProcessSessionOpenRequest {
    let encoding = default_runtime_text_encoding();
    if cfg!(windows) {
        ProcessSessionOpenRequest {
                program: "python".to_string(),
                args: vec![
                    "-c".to_string(),
                    "import subprocess, sys, time; child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)']); print(child.pid, flush=True); time.sleep(0.3)".to_string(),
                ],
                cwd: None,
                stdout_encoding: encoding,
                stderr_encoding: encoding,
                stdin_encoding: encoding,
                buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
            }
    } else {
        ProcessSessionOpenRequest {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "sleep 30 & echo $!; sleep 0.3; exit 0".to_string(),
            ],
            cwd: None,
            stdout_encoding: encoding,
            stderr_encoding: encoding,
            stdin_encoding: encoding,
            buffer_limit_bytes: DEFAULT_SESSION_BUFFER_LIMIT_BYTES,
        }
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

/// Return whether the selected process id is still alive on the current platform.
/// 返回当前平台上指定进程 id 是否仍然存活。
fn process_exists(pid: u32) -> bool {
    if cfg!(windows) {
        Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "if (Get-Process -Id {} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}",
                        pid
                    ),
                ])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
    } else {
        Command::new("sh")
            .args(["-c", &format!("kill -0 {} 2>/dev/null", pid)])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

/// Wait for one process id to disappear within the expected timeout.
/// 在预期超时时间内等待某个进程 id 消失。
fn assert_process_exits(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("process {pid} should have exited after session drop");
}

/// Wait for one session to publish a descendant pid to stdout.
/// 等待某个会话把后代进程 pid 输出到 stdout。
fn wait_for_descendant_pid(session: &ManagedProcessSession, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        #[cfg(windows)]
        {
            let root_pid = session
                .state
                .child
                .lock()
                .expect("lock child process for descendant snapshot")
                .id();
            if let Ok(descendants) = collect_windows_descendant_processes(root_pid)
                && let Some(descendant) = descendants.into_iter().map(|entry| entry.pid).next()
            {
                return descendant;
            }
        }
        let stdout = session
            .state
            .stdout_buffer
            .lock()
            .expect("lock stdout buffer");
        if !stdout.is_empty() {
            let pid_lines = stdout
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
        thread::sleep(Duration::from_millis(25));
    }
    panic!("descendant pid should be published before cleanup");
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
    let pid = session.state.child.lock().expect("lock child process").id();
    assert!(
        process_exists(pid),
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
    let session = ManagedProcessSession::open(make_descendant_cleanup_request())
        .expect("open descendant cleanup session");
    let descendant_pid = wait_for_descendant_pid(&session, Duration::from_secs(15));
    assert!(
        process_exists(descendant_pid),
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
    let buffer = Arc::new(Mutex::new(Vec::from(&b"ready"[..])));
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
    let drained = drain_buffer(&buffer, 3).expect("drain poisoned output buffer");
    assert_eq!(drained, b"rea");
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
            .state
            .final_status
            .lock()
            .expect("initial process session final status lock");
        panic!("poison process session final status for recovery test");
    }));
    assert!(final_status_poison.is_err());

    // Final status returned by the normal kill path after final-status lock recovery.
    // 最终状态锁恢复后通过正常 kill 路径返回的终态状态。
    let killed_status = session
        .kill_child()
        .expect("kill child through poisoned final status cache");
    // Cached final status read back through the recovered cache lock.
    // 通过已恢复缓存锁回读到的最终状态。
    let cached_status = session
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
    let session = ManagedProcessSession::open(make_descendant_cleanup_request())
        .expect("open close descendant cleanup session");
    let descendant_pid = wait_for_descendant_pid(&session, Duration::from_secs(15));
    assert!(
        process_exists(descendant_pid),
        "descendant process should be running before close cleanup"
    );

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
            .state
            .peek_status_snapshot()
            .expect("recheck immediate exit status")
            .exited,
        "immediate exit process should finish before read regression check"
    );
    session
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
        .state
        .stdout_reader
        .lock()
        .expect("lock stdout reader slot for synthetic install") = Some(stdout_reader);
    *session
        .state
        .stderr_reader
        .lock()
        .expect("lock stderr reader slot for synthetic install") = Some(stderr_reader);

    let stdout_buffer = session.state.stdout_buffer.clone();
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
        .state
        .join_reader_threads()
        .expect("join synthetic session readers");
}

#[cfg(windows)]
/// Verify snapshot-time identity filtering rejects processes created after the caller cutoff.
/// 验证快照时间身份过滤会拒绝截止时间之后才创建的进程。
#[test]
fn windows_snapshot_open_rejects_future_identity() {
    let handle = try_open_windows_process_for_snapshot(std::process::id(), 0)
        .expect("open current process for snapshot identity test");
    assert!(
        handle.is_none(),
        "process created after cutoff should be rejected to avoid PID reuse confusion"
    );
}

#[cfg(windows)]
/// Verify snapshot identity filtering still accepts one process that clearly predates the cutoff.
/// 验证快照身份过滤仍会接受那些明显早于截止时间创建的进程。
#[test]
fn windows_snapshot_open_accepts_existing_identity_before_cutoff() {
    let cutoff = current_windows_time_ticks().expect("capture current windows cutoff");
    let handle = try_open_windows_process_for_snapshot(std::process::id(), cutoff)
        .expect("open current process before cutoff");
    assert!(
        handle.is_some(),
        "existing process should be accepted when it predates the snapshot cutoff"
    );
}
