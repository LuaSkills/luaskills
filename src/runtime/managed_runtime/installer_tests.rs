//! Deadline and output bounds of the shared uv/pnpm environment-installer launcher.
//! uv 与 pnpm 共用环境安装启动器的截止时间与输出上限。

use super::*;

/// Environment variable selecting the role of [`installer_fixture`] in a child test process.
/// 在子测试进程中选择 [`installer_fixture`] 角色的环境变量。
const INSTALLER_FIXTURE_ENV: &str = "LUASKILLS_INSTALLER_FIXTURE";

/// Exact test path of [`installer_fixture`], used to re-run this test binary as an installer.
/// [`installer_fixture`] 的精确测试路径，用于把当前测试二进制作为安装器重新运行。
const INSTALLER_FIXTURE_TEST: &str = "runtime::managed_runtime::installer_tests::installer_fixture";

/// Build a command that runs [`installer_fixture`] in `role`.
/// 构造以 `role` 运行 [`installer_fixture`] 的命令。
fn installer_fixture_command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args(["--exact", INSTALLER_FIXTURE_TEST, "--nocapture"])
        .env(INSTALLER_FIXTURE_ENV, role);
    command
}

/// Fake installer driven by [`INSTALLER_FIXTURE_ENV`]; a no-op in an ordinary test run.
/// 由 [`INSTALLER_FIXTURE_ENV`] 驱动的伪安装器；普通测试运行中不做任何事。
///
/// `hang` starts a `hang-child` that inherits the output pipes, like a lifecycle script, and then
/// both sleep far past any test deadline. `noisy` writes far more stderr than the retained tail and
/// fails.
/// `hang` 会像生命周期脚本那样启动一个继承输出管道的 `hang-child`，然后两者都休眠到远超任何
/// 测试截止时间。`noisy` 写出远多于保留末尾的 stderr 后失败退出。
#[test]
fn installer_fixture() {
    let Some(role) = std::env::var_os(INSTALLER_FIXTURE_ENV) else {
        return;
    };
    match role.to_str() {
        Some("hang") => {
            let mut child = installer_fixture_command("hang-child")
                .spawn()
                .expect("spawn hanging installer descendant");
            println!("installer fixture waiting with a descendant");
            thread::sleep(Duration::from_secs(120));
            let _ = child.kill();
            let _ = child.wait();
        }
        Some("hang-child") => thread::sleep(Duration::from_secs(120)),
        Some("noisy") => {
            let line = "x".repeat(1023);
            for _ in 0..1024 {
                eprintln!("{line}");
            }
            eprintln!("installer fixture final line");
            std::process::exit(3);
        }
        other => panic!("unknown installer fixture role {other:?}"),
    }
}

/// An installer still running at its deadline is killed together with its descendants.
/// 到截止时间仍在运行的安装器会与其后代一起被终止。
///
/// The descendant holds the output pipes, so if only the direct child were killed the pipe readers
/// would wait for the descendant's whole sleep.
/// 后代持有输出管道，因此若只终止直接子进程，管道读取线程会一直等到后代休眠结束。
#[test]
fn installer_past_deadline_is_killed_with_its_descendants() {
    let mut command = installer_fixture_command("hang");
    let started_at = Instant::now();
    let error = run_command(
        &mut command,
        "run hanging installer",
        LuaRuntimeHostMode::Console,
        Instant::now() + Duration::from_millis(1500),
    )
    .expect_err("hanging installer must fail at its deadline");
    let elapsed = started_at.elapsed();
    assert!(
        error.contains("did not finish within") && error.contains("was terminated"),
        "unexpected error: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "installer outlived its deadline: {elapsed:?}"
    );
}

/// A failing installer reports only the bounded tail of its output.
/// 失败的安装器只报告其输出的有界末尾。
#[test]
fn failed_installer_reports_only_output_tail() {
    let mut command = installer_fixture_command("noisy");
    let error = run_command(
        &mut command,
        "run noisy installer",
        LuaRuntimeHostMode::Console,
        Instant::now() + Duration::from_secs(120),
    )
    .expect_err("noisy installer exits with a failure status");
    assert!(error.contains("status="), "unexpected error: {error}");
    assert!(error.contains("dropped_bytes="), "tail marker missing");
    assert!(
        error.contains("installer fixture final line"),
        "the tail must keep the last output"
    );
    assert!(
        error.len() < 3 * MANAGED_ENV_INSTALLER_OUTPUT_TAIL_BYTES,
        "error text is not bounded: {} bytes",
        error.len()
    );
}
