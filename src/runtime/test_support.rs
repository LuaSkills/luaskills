use std::ffi::OsString;
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
        .expect("lock process env test guard")
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
