use std::ffi::{OsStr, OsString};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Acquire the process-wide environment mutation guard shared by tests that mutate or depend on PATH.
/// 获取由修改或依赖 PATH 的测试共享的进程级环境变量保护锁。
pub(crate) fn process_env_test_guard() -> MutexGuard<'static, ()> {
    /// Process-wide lock that serializes tests touching executable discovery environment.
    /// 用于串行化触碰可执行文件发现环境的测试的进程级锁。
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();

    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire the process-wide guard shared by every test that replaces the global runtime log callback.
/// 获取由所有替换全局运行时日志回调的测试共享的进程级保护锁。
/// Without it, parallel tests overwrite each other's callback and lose or misattribute observations.
/// 若无此锁，并行测试会互相覆盖回调，导致观测丢失或归属错误。
pub(crate) fn runtime_log_callback_test_guard() -> MutexGuard<'static, ()> {
    /// Process-wide lock that serializes tests owning the global log callback.
    /// 用于串行化拥有全局日志回调的测试的进程级锁。
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();

    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Restore one process environment variable after a test mutates it.
/// 在测试修改环境变量后恢复单个进程环境变量。
fn restore_test_env_var(name: &str, previous: Option<OsString>) {
    match previous {
        Some(value) => unsafe { std::env::set_var(name, value) },
        None => unsafe { std::env::remove_var(name) },
    }
}

/// Restore one batch of mutated environment variables when a PATH-sensitive test finishes.
/// 当依赖 PATH 的测试结束时，恢复一批被修改过的环境变量。
pub(crate) struct TestEnvRestoreGuard {
    /// Recorded previous values keyed by variable name.
    /// 按变量名记录的旧值集合。
    entries: Vec<(String, Option<OsString>)>,
}

impl TestEnvRestoreGuard {
    /// Capture one named environment variable before the current test mutates it.
    /// 在当前测试修改环境变量前，捕获单个具名环境变量。
    pub(crate) fn capture(name: &str) -> Self {
        Self {
            entries: vec![(name.to_string(), std::env::var_os(name))],
        }
    }

    /// Capture one additional named environment variable before the current test mutates it.
    /// 在当前测试修改环境变量前，再额外捕获一个具名环境变量。
    #[cfg(windows)]
    pub(crate) fn and_capture(mut self, name: &str) -> Self {
        self.entries
            .push((name.to_string(), std::env::var_os(name)));
        self
    }
}

impl Drop for TestEnvRestoreGuard {
    /// Restore every captured environment variable in reverse order on drop.
    /// 在释放时按逆序恢复所有已捕获的环境变量。
    fn drop(&mut self) {
        while let Some((name, previous)) = self.entries.pop() {
            restore_test_env_var(&name, previous);
        }
    }
}

/// Environment variable naming the one test a child test process was started to run in isolation.
/// 子测试进程被启动来隔离运行的那个测试的名称所在的环境变量。
const ISOLATED_ENV_TEST: &str = "LUASKILLS_ISOLATED_ENV_TEST";

/// Run test `test_path` in its own child test process unless this process already is that child.
/// 除非当前进程本身就是该子进程，否则在独立的子测试进程中运行测试 `test_path`。
///
/// Tests that change process-wide state such as `PATH` or cwd call this first, so the change happens
/// only in the child and parallel tests in this process never observe it. The parent asserts the
/// child ran exactly that one test and it passed.
/// 修改 `PATH` 或工作目录等进程级状态的测试会先调用它，使修改只发生在子进程中，本进程内并行的测试永远看不到。
/// 父进程断言子进程恰好运行了这一个测试并且通过。
///
/// Return `true` in the child, where the caller continues with its body, and `false` in the parent.
/// 在子进程中返回 `true`，调用方继续执行测试体；在父进程中返回 `false`。
pub(crate) fn run_in_isolated_env_process(test_path: &str) -> bool {
    if std::env::var_os(ISOLATED_ENV_TEST).as_deref() == Some(OsStr::new(test_path)) {
        return true;
    }
    let executable = std::env::current_exe().expect("locate the running test executable");
    // The child writes to a file, not a pipe: on Windows, processes spawned concurrently by other
    // tests can inherit a pipe handle and hold its EOF back long after the child itself exits.
    // 子进程输出写入文件而不是管道：在 Windows 上，其他测试并发启动的进程可能继承管道句柄，
    // 在子进程退出很久之后仍拖住 EOF。
    let log_path = std::env::temp_dir().join(format!(
        "luaskills-isolated-{}-{}.log",
        std::process::id(),
        test_path.replace("::", "-")
    ));
    let log = std::fs::File::create(&log_path).expect("create the isolated test log");
    // An ordinary file-based test may have temporarily changed the parent's working directory.
    // 普通文件模式测试可能已临时改动父进程的工作目录。
    let status = Command::new(executable)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([test_path, "--exact", "--test-threads=1", "--nocapture"])
        .env(ISOLATED_ENV_TEST, test_path)
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("share the isolated test log"))
        .stderr(log)
        .status()
        .expect("start the isolated test process");
    let output = std::fs::read_to_string(&log_path).unwrap_or_default();
    let _ = std::fs::remove_file(&log_path);
    assert!(
        status.success() && output.contains("test result: ok. 1 passed"),
        "isolated test {test_path} failed in its child process
{output}"
    );
    false
}

/// Re-run the enclosing test in its own process before it touches process-wide state.
/// 在所在测试触碰进程级状态之前，把它放到独立进程中重新运行。
///
/// Expands to an early `return` in the parent; only the child process executes the rest of the test.
/// 在父进程中展开为提前 `return`；只有子进程会执行测试的其余部分。
macro_rules! isolate_process_env {
    () => {{
        fn marker() {}
        let marker_path = std::any::type_name_of_val(&marker);
        let test_path = marker_path
            .strip_suffix("::marker")
            .expect("isolation marker path");
        // Test filters omit the crate name that `type_name` includes.
        // 测试过滤器不包含 `type_name` 中带有的 crate 名。
        let test_path = test_path
            .split_once("::")
            .map_or(test_path, |(_, rest)| rest);
        if !$crate::runtime::test_support::run_in_isolated_env_process(test_path) {
            return;
        }
    }};
}
pub(crate) use isolate_process_env;
