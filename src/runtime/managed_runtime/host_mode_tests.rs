//! Real-process validation for the shared uv/pnpm environment-install launcher.
//! uv 与 pnpm 共用环境安装启动器的真实进程验证。

use super::*;

/// Assert the installer subprocess owns no console window when explicitly enabled as a fixture.
/// 显式启用为夹具时，断言安装器子进程没有控制台窗口。
#[test]
fn installer_window_probe_fixture() {
    if std::env::var_os("LUASKILLS_INSTALLER_WINDOW_PROBE").is_none() {
        return;
    }
    // SAFETY: This read-only query has no pointers and observes only the current process.
    // 安全性：此只读查询没有指针，仅观察当前进程。
    assert!(unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null());
}

/// Exercise the exact launcher used by uv venv, uv sync, and pnpm install; returns after the child assertion.
/// 验证 uv venv、uv sync 和 pnpm install 使用的精确启动器；子进程断言通过后返回。
#[test]
fn desktop_host_mode_environment_installer() {
    // The test executable is a real console-subsystem process and needs no package/network fixture.
    // 测试可执行文件是真实控制台子系统进程，不需要包或网络夹具。
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "runtime::managed_runtime::host_mode_tests::installer_window_probe_fixture",
        "--nocapture",
    ]);
    command.env("LUASKILLS_INSTALLER_WINDOW_PROBE", "1");
    run_command(
        &mut command,
        "probe desktop installer",
        LuaRuntimeHostMode::Desktop,
    )
    .unwrap();
}
