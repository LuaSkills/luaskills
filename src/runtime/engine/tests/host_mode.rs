//! Windows host-mode acceptance for ordinary VMs and real managed interpreters.
//! 普通 VM 及真实受管解释器的 Windows 宿主模式验收。

use super::*;
use crate::LuaRuntimeHostMode;
use crate::runtime::process_window::configure_command;
use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleWindow};

/// Inspect the explicitly supplied child PID from an isolated console-free process; returns no value.
/// 从隔离无控制台进程检查显式提供的子进程 PID；无返回值。
#[test]
fn desktop_console_observer_fixture() {
    // The variable is set only on this fixture's Command, never on the parallel parent test process.
    // 此变量仅设置于本夹具的 Command，绝不设置于并行父测试进程。
    let Ok(pid) = std::env::var("LUASKILLS_OBSERVE_CONSOLE_PID") else {
        return;
    };
    // Exact PID comes from the managed worker/session itself.
    // 精确 PID 来自受管工作进程或会话自身。
    let pid: u32 = pid.parse().unwrap();
    // SAFETY: These APIs query this process and attach only its console, not its address space.
    // 安全性：这些 API 查询本进程且仅连接其控制台，不连接地址空间。
    assert!(unsafe { GetConsoleWindow() }.is_null());
    // SAFETY: CREATE_NO_WINDOW can own an invisible console; detach it before observing the target.
    // 安全性：CREATE_NO_WINDOW 可能拥有不可见控制台；观察目标前先分离它。
    assert_ne!(unsafe { FreeConsole() }, 0);
    if unsafe { AttachConsole(pid) } == 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(6),
            "target must lack a console, not be missing or inaccessible"
        );
    } else {
        // SAFETY: A console created without a window must still expose no console HWND after attachment.
        // 安全性：无窗口创建的控制台在连接后仍不得暴露控制台窗口句柄。
        assert!(
            unsafe { GetConsoleWindow() }.is_null(),
            "managed runtime allocated a console window"
        );
    }
}

/// Observe `pid` without detaching the test runner's own console; returns after the child assertion passes.
/// 在不分离测试运行器自身控制台的情况下观察 `pid`；子进程断言通过后返回。
fn assert_hidden_process(pid: u32) {
    // Command runs only the exact observer fixture against the known live process.
    // Command 仅针对已知存活进程运行精确观察器夹具。
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "runtime::engine::tests::host_mode::desktop_console_observer_fixture",
        "--nocapture",
    ]);
    command.env("LUASKILLS_OBSERVE_CONSOLE_PID", pid.to_string());
    configure_command(&mut command, LuaRuntimeHostMode::Desktop, 0);
    // Output retains both diagnostics and the test harness status for failure reporting.
    // Output 保留诊断及测试框架状态，用于失败报告。
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify ordinary skill VM initialization installs desktop-native file semantics; returns no value.
/// 验证普通技能 VM 初始化安装桌面原生文件语义；无返回值。
#[test]
fn desktop_host_mode_ordinary_vm_native_io() {
    // Shell lookup participates in the existing environment lock shared by PATH-mutating tests.
    // shell 查找参与现有环境锁，与修改 PATH 的测试共享。
    let _guard = process_env_test_guard();
    // Native compatibility is disabled explicitly so managed runlua IO cannot hide missing installation.
    // 显式关闭托管兼容替换，防止托管 runlua IO 掩盖安装缺失。
    let mut options = LuaRuntimeHostOptions {
        host_mode: LuaRuntimeHostMode::Desktop,
        ..Default::default()
    };
    options.capabilities.enable_managed_io_compat = false;
    // Engine and VM use the same production initialization as pooled skills.
    // 引擎和 VM 使用与池化技能相同的生产初始化。
    let engine = make_runtime_test_engine_with_host_options(options);
    let vm = engine.create_vm().unwrap();
    vm.lua
        .load(
            r#"
        -- Native pipes preserve iteration and distinguish EOF from an IO failure.
        -- 原生管道保留迭代，并区分文件结束与 IO 失败。
        local reader = assert(io.popen("echo first&echo second", "r"))
        -- The iterator keeps its pipe alive and leaves it open after EOF.
        -- 迭代器保持管道存活，在文件结束后仍保持其打开状态。
        local lines = reader:lines()
        assert(lines() == "first" and lines() == "second" and lines() == nil)
        assert(io.type(reader) == "file")
        assert(reader:close())
        assert(not pcall(function() reader:lines() end))
        -- Reading a write-only pipe is an IO error, which a lines iterator must raise.
        -- 读取只写管道属于 IO 错误，行迭代器必须抛出此错误。
        local writer = assert(io.popen("exit /b 0", "w"))
        assert(not pcall(writer:lines()))
        assert(writer:close())
        -- Ordinary filesystem files still use native LuaJIT file userdata.
        -- 普通文件系统文件仍使用原生 LuaJIT 文件 userdata。
        local file = assert(io.tmpfile())
        assert(file:write("unchanged") == file)
        assert(file:seek("set", 0) == 0 and file:read("*a") == "unchanged")
        assert(io.close(file))
    "#,
        )
        .exec()
        .unwrap();
}

/// Verify desktop mode reaches real Python workers and sessions with default runtime roots.
/// 验证桌面模式通过默认运行时根到达真实 Python 工作进程及会话。
#[test]
fn desktop_host_mode_python_worker_and_session() {
    exercise_managed_runtime(ManagedSessionTestRuntime::Python, false);
}

/// Verify desktop mode reaches real Node workers and sessions with explicit host runtime roots.
/// 验证桌面模式通过显式宿主运行时根到达真实 Node 工作进程及会话。
#[test]
fn desktop_host_mode_node_worker_and_session() {
    exercise_managed_runtime(ManagedSessionTestRuntime::Node, true);
}

/// Launch `runtime` through both production APIs using the selected root source; assert their actual console state.
/// 使用所选根来源通过两种生产 API 启动 `runtime`；断言其真实控制台状态。
/// `host_roots` selects explicit roots; returns after worker/session observation and lease cleanup.
/// `host_roots` 选择显式根；工作进程和会话观察及租约清理完成后返回。
fn exercise_managed_runtime(runtime: ManagedSessionTestRuntime, host_roots: bool) {
    // PATH-sensitive discovery shares the existing suite's environment lock.
    // 依赖 PATH 的发现共享现有套件的环境锁。
    let _guard = process_env_test_guard();
    // Missing runtimes follow the existing suite contract; CI requires both to be installed.
    // 缺失运行时遵循现有套件契约；CI 要求两者均已安装。
    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    // The existing fixture supplies a ready dependency environment without network installation.
    // 现有夹具提供已就绪依赖环境，无需网络安装。
    let layout = if host_roots {
        ManagedSessionSystemLayout::new_with_host_configured_roots("desktop-mode", runtime, &host)
    } else {
        ManagedSessionSystemLayout::new("desktop-mode", runtime, &host)
    };
    // Minimal handlers return their actual PID while production wrappers own the process lifecycle.
    // 最小处理器返回真实 PID，生产包装器拥有进程生命周期。
    let (invoke, session) = match runtime {
        ManagedSessionTestRuntime::Python => (
            "# Access the worker PID.\n# 访问工作进程 PID。\nimport os\n# Return PID; args and ctx are the protocol inputs.\n# 返回 PID；args 和 ctx 为协议输入。\ndef main(args, ctx):\n    return os.getpid()\n",
            "# Emit delimited PID and wait until the owner closes this session.\n# 输出带分隔符 PID 并等待所有者关闭本会话。\nimport os, sys\nprint(str(os.getpid()) + '|', flush=True)\nsys.stdin.read()\n",
        ),
        ManagedSessionTestRuntime::Node => (
            "// Return PID; args and ctx are protocol inputs.\n// 返回 PID；args 和 ctx 为协议输入。\nexport default function main(args, ctx) { return process.pid; }\n",
            "// Emit delimited PID and retain stdin until the session owner closes it.\n// 输出带分隔符 PID 并保留标准输入，直到会话所有者关闭它。\nconsole.log(process.pid + '|');\nprocess.stdin.resume();\n",
        ),
    };
    fs::write(layout.base.package_root.join(layout.invoke_file()), invoke).unwrap();
    fs::write(
        layout.base.package_root.join(layout.sidecar_file()),
        session,
    )
    .unwrap();
    // Mode is copied into the engine and inherited by its strict System VM.
    // 模式复制到引擎，由其严格 System VM 继承。
    let mut options = layout.host_options();
    options.host_mode = LuaRuntimeHostMode::Desktop;
    // Engine owns both managed process paths and their configured runtime roots.
    // 引擎拥有两条受管进程路径及其配置的运行时根。
    let engine = make_runtime_test_engine_with_host_options(options);
    // Lease retains the worker and sidecar while independent observation runs.
    // 租约在独立观察运行时保留工作进程及边车进程。
    let lease = create_managed_session_test_lease(&engine, &layout, "desktop-mode", false);
    // Invoke must succeed before its reported PID can be trusted as a live process.
    // 必须先调用成功，才能将其报告的 PID 作为可信存活进程。
    let invocation = eval_managed_session_test_lease(
        &engine,
        &lease,
        &format!(
            "return {}({{file='{}', args={{}}, timeout_ms=10000}})",
            layout.lua_invoke_api(),
            layout.invoke_file()
        ),
    );
    assert_eq!(invocation["ok"], true, "{invocation}");
    assert_eq!(invocation["result"]["ok"], true, "{invocation}");
    assert_hidden_process(u32::try_from(invocation["result"]["value"].as_u64().unwrap()).unwrap());
    // Session creation uses the real managed session launcher and its separate launch options.
    // 会话创建使用真实受管会话启动器及其独立启动选项。
    let opened = eval_managed_session_test_lease(
        &engine,
        &lease,
        &managed_session_open_lua(&layout, "window_session", 1024, &[]),
    );
    assert_eq!(opened["ok"], true, "{opened}");
    // The first line is the exact PID emitted by the deliberately minimal sidecar above.
    // 第一行是上述有意最小化边车程序输出的精确 PID。
    let read = eval_managed_session_test_lease(
        &engine,
        &lease,
        "return window_session:read({timeout_ms=10000, until_text='|'})",
    );
    assert_eq!(read["ok"], true, "{read}");
    assert_hidden_process(
        read["result"]["stdout"]
            .as_str()
            .unwrap()
            .trim()
            .trim_end_matches('|')
            .parse()
            .unwrap(),
    );
    close_managed_session_test_lease(&engine, &lease);
}
