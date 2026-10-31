//! Exercise public host modes from a genuinely console-less Windows host process.
//! 从真实无控制台 Windows 宿主进程验证公开宿主模式。

use luaskills::{LuaRuntimeHostMode, LuaRuntimeHostOptions};

/// Verify omitted JSON retains legacy console behavior and invalid values are rejected; no inputs or outputs.
/// 验证省略 JSON 字段时保留旧命令行行为，并拒绝无效值；无输入或输出。
#[test]
fn host_mode_json_compatibility() {
    // A serialized legacy request is reconstructed by removing only the newly introduced field.
    // 仅移除新增字段，重建序列化的旧请求。
    let mut value = serde_json::to_value(LuaRuntimeHostOptions::default()).unwrap();
    value.as_object_mut().unwrap().remove("host_mode");
    assert_eq!(
        serde_json::from_value::<LuaRuntimeHostOptions>(value.clone())
            .unwrap()
            .host_mode,
        LuaRuntimeHostMode::Console
    );
    value["host_mode"] = serde_json::json!("desktop");
    assert_eq!(
        serde_json::from_value::<LuaRuntimeHostOptions>(value.clone())
            .unwrap()
            .host_mode,
        LuaRuntimeHostMode::Desktop
    );
    for invalid in [
        serde_json::json!("auto"),
        serde_json::json!(2),
        serde_json::Value::Null,
    ] {
        value["host_mode"] = invalid;
        assert!(serde_json::from_value::<LuaRuntimeHostOptions>(value.clone()).is_err());
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use luaskills::{LuaEngine, LuaEngineOptions, LuaVmPoolConfig};
    use std::io::Read;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
    use windows_sys::Win32::System::Threading::{DETACHED_PROCESS, GetCurrentProcess};

    // Separate host fixtures serialize console allocation; each fixture still runs both engine modes concurrently.
    // 独立宿主夹具串行分配控制台；各夹具内部仍并发运行两种引擎模式。
    static HOST_FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Return whether this exact subprocess was explicitly launched as a test fixture.
    /// 返回本精确子进程是否被显式启动为测试夹具。
    fn fixture_enabled() -> bool {
        std::env::var_os("LUASKILLS_WINDOW_FIXTURE").is_some()
    }

    /// Assert a desktop child has no console window; its output marks successful real-process observation.
    /// 断言桌面子进程没有控制台窗口；其输出标记成功的真实进程观察。
    #[test]
    fn desktop_probe_child() {
        if !fixture_enabled() {
            return;
        }
        // SAFETY: This read-only Windows query takes no pointers and observes only the current process.
        // 安全性：此只读 Windows 查询不接收指针，仅观察当前进程。
        assert!(
            unsafe { GetConsoleWindow() }.is_null(),
            "desktop child allocated a console window"
        );
        println!("CONSOLE_PROBE_OK");
    }

    /// Assert a console-mode child retains the platform's console allocation behavior.
    /// 断言命令行模式子进程保留平台控制台分配行为。
    #[test]
    fn console_probe_child() {
        if !fixture_enabled() {
            return;
        }
        // SAFETY: This read-only API has no pointer arguments.
        // 安全性：此只读 API 没有指针参数。
        assert!(
            !unsafe { GetConsoleWindow() }.is_null(),
            "console mode unexpectedly hid the console"
        );
        println!("CONSOLE_PROBE_OK");
    }

    /// Read raw stdin bytes and verify the write-mode pipe plus hidden console; no return value.
    /// 读取原始标准输入字节，并验证写模式管道及隐藏控制台；无返回值。
    #[test]
    fn stdin_probe_child() {
        if !fixture_enabled() {
            return;
        }
        // SAFETY: GetConsoleWindow reads only the current process state.
        // 安全性：GetConsoleWindow 仅读取当前进程状态。
        assert!(unsafe { GetConsoleWindow() }.is_null());
        // The fixture reads until EOF, proving pclose closes stdin before waiting for the child.
        // 夹具一直读取到文件结束，证明 pclose 在等待子进程前关闭标准输入。
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"hello\0world");
    }

    /// Execute `code` through the public runlua path and require its success marker in the rendered result.
    /// 通过公开 runlua 路径执行 `code`，并要求渲染结果中包含成功标记。
    fn evaluate(engine: &LuaEngine, code: String) {
        // The result format intentionally renders Lua failures instead of returning a Rust error.
        // 结果格式有意渲染 Lua 失败，而不是返回 Rust 错误。
        let result = engine
            .execute_runlua_request_json_inline(&serde_json::json!({"code": code}).to_string())
            .unwrap();
        assert!(
            result.contains("HOST_MODE_OK"),
            "{result}\nLua source:\n{code}"
        );
    }

    /// Build one independent engine from `mode` and the native/managed IO preference; returns that engine.
    /// 根据 `mode` 和原生或托管 IO 偏好构建独立引擎；返回该引擎。
    fn engine(mode: LuaRuntimeHostMode, managed_io: bool) -> LuaEngine {
        // Host options are copied into each engine; no process-wide environment selects their mode.
        // 宿主选项复制到每个引擎；没有进程级环境变量选择模式。
        let mut host = LuaRuntimeHostOptions {
            host_mode: mode,
            ..LuaRuntimeHostOptions::default()
        };
        host.capabilities.enable_managed_io_compat = managed_io;
        LuaEngine::new(LuaEngineOptions::new(
            LuaVmPoolConfig {
                min_size: 0,
                max_size: 2,
                idle_ttl_secs: 30,
            },
            host,
        ))
        .unwrap()
    }

    /// Exercise direct execution, shells, popen, sessions and os.execute for one engine's selected mode.
    /// 验证单个引擎所选模式下的直接执行、shell、popen、会话及 os.execute。
    fn exercise_engine(engine: &LuaEngine, mode: LuaRuntimeHostMode) {
        // The current test executable is a known console-subsystem binary, suitable for observing allocation.
        // 当前测试程序是已知控制台子系统二进制，适用于观察控制台分配。
        let executable = std::env::current_exe().unwrap();
        // The exact child test determines the expected console behavior without mutating the host environment.
        // 精确子测试决定预期控制台行为，不改变宿主环境。
        let fixture = match mode {
            LuaRuntimeHostMode::Desktop => "windows::desktop_probe_child",
            LuaRuntimeHostMode::Console => "windows::console_probe_child",
        };
        // This shell expression targets only the verified test executable and static test names.
        // 本 shell 表达式仅指向已验证测试程序及静态测试名称。
        let command = format!(
            ".\\{} --exact {fixture} --nocapture",
            executable.file_name().unwrap().to_str().unwrap()
        );
        // JSON string quoting is also valid Lua quoting for these UTF-8 paths and shell text.
        // 对这些 UTF-8 路径及 shell 文本，JSON 字符串引号也是有效 Lua 引号。
        let program = serde_json::to_string(&executable.to_str().unwrap()).unwrap();
        // The serialized string is inserted into Lua source, never into an outer shell command.
        // 序列化字符串插入 Lua 源码，绝不插入外层 shell 命令。
        let shell = serde_json::to_string(&command).unwrap();
        evaluate(
            engine,
            format!(
                r#"
            -- Direct program execution preserves exit status and output capture.
            -- 直接程序执行保留退出状态和输出捕获。
            local direct = vulcan.process.exec({{program={program}, args={{"--exact", "{fixture}", "--nocapture"}}, timeout_ms=10000}})
            assert(direct.success, vulcan.json.encode(direct))
            assert(direct.stdout:find("CONSOLE_PROBE_OK", 1, true))
            -- Shell execution applies the same policy before creating cmd.exe.
            -- shell 执行在创建 cmd.exe 前应用相同策略。
            local shell = vulcan.process.exec({{command={shell}, timeout_ms=10000}})
            assert(shell.success, vulcan.json.encode(shell))
            -- This entry is native or managed according to this engine's explicit capability option.
            -- 本入口根据引擎的显式能力选项采用原生或托管实现。
            local pipe = assert(io.popen({shell}, "r"))
            assert(pipe:read("*a"):find("CONSOLE_PROBE_OK", 1, true))
            assert(pipe:close(), "io.popen close failed")
            -- The explicit managed pipe remains covered when native compatibility is disabled.
            -- 关闭原生兼容替换时，显式托管管道仍在覆盖范围内。
            local managed = vulcan.io.popen({shell}, "r")
            -- Retain all probe output when the assertion fails so console observations remain diagnosable.
            -- 断言失败时保留全部探针输出，使控制台观察仍可诊断。
            local managed_output = managed:read("*a")
            assert(managed_output:find("CONSOLE_PROBE_OK", 1, true), "managed pipe output: " .. managed_output)
            assert(managed:close(), "vulcan.io.popen close failed")
            -- Standard execute returns the LuaJIT 5.2 exit triple.
            -- 标准执行返回 LuaJIT 5.2 退出三元组。
            local ok, reason, code = os.execute({shell})
            assert(ok == true and reason == "exit" and code == 0, "os.execute status: " .. tostring(ok) .. "/" .. tostring(reason) .. "/" .. tostring(code))
            -- Persistent sessions share the process-tree launcher and preserve their read/status API.
            -- 持久会话共享进程树启动器，保留其读取及状态 API。
            local session = vulcan.process.session.open({{program={program}, args={{"--exact", "{fixture}", "--nocapture"}}}})
            -- Bounded read iterations keep this acceptance test finite if the child cannot complete.
            -- 有界读取迭代使子进程无法完成时本验收测试仍能终止。
            local output = ""
            for index = 1, 100 do
                -- Each batch is appended before inspecting the child's terminal state.
                -- 每批输出在检查子进程终态之前追加。
                local batch = session:read({{timeout_ms=100}})
                output = output .. batch.stdout
                if session:status().exited and output:find("CONSOLE_PROBE_OK", 1, true) then break end
            end
            assert(output:find("CONSOLE_PROBE_OK", 1, true), output)
            assert(session:close().success)
            return "HOST_MODE_OK"
        "#
            ),
        );
    }

    /// Run the public engines inside the hidden fixture host; ordinary test runs do not execute this body.
    /// 在隐藏夹具宿主中运行公开引擎；普通测试运行不会执行本函数体。
    #[test]
    fn windowless_host_fixture() {
        if !fixture_enabled() {
            return;
        }
        // SAFETY: Read-only assertion proves the parent starts without an inherited console.
        // 安全性：只读断言证明父进程启动时没有继承控制台。
        assert!(unsafe { GetConsoleWindow() }.is_null());
        // Optional non-breakaway job forces the real ERROR_ACCESS_DENIED spawn retry path.
        // 可选不可脱离 Job 强制触发真实 ERROR_ACCESS_DENIED 启动重试路径。
        let _job = if std::env::var_os("LUASKILLS_WINDOW_BLOCK_BREAKAWAY").is_some() {
            // SAFETY: Null attributes and name request an unnamed job with default restrictions.
            // 安全性：空属性及名称请求具有默认限制的匿名 Job。
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            assert!(!job.is_null());
            // SAFETY: Both handles are valid; only this isolated fixture process joins the job.
            // 安全性：两个句柄均有效；仅此隔离夹具进程加入该 Job。
            assert_ne!(
                unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) },
                0
            );
            // SAFETY: This is the sole owner of the new job handle; default jobs do not kill on close.
            // 安全性：这是新 Job 句柄的唯一所有者；默认 Job 不会在关闭时终止进程。
            Some(unsafe { OwnedHandle::from_raw_handle(job) })
        } else {
            None
        };
        // Both engines coexist so a global toggle cannot accidentally satisfy the test.
        // 两个引擎同时存在，防止全局开关偶然满足测试。
        let desktop = engine(LuaRuntimeHostMode::Desktop, false);
        // The console engine must retain the old behavior even after a desktop engine was created.
        // 在创建桌面引擎后，命令行引擎仍必须保留旧行为。
        let console = engine(LuaRuntimeHostMode::Console, true);
        std::thread::scope(|scope| {
            scope.spawn(|| exercise_engine(&desktop, LuaRuntimeHostMode::Desktop));
            scope.spawn(|| exercise_engine(&console, LuaRuntimeHostMode::Console));
        });
        exercise_engine(
            &engine(LuaRuntimeHostMode::Desktop, true),
            LuaRuntimeHostMode::Desktop,
        );
        // The write fixture verifies binary bytes, EOF-before-wait ordering, and userdata identity.
        // 写夹具验证二进制字节、等待前文件结束的顺序及 userdata 身份。
        let write_command = serde_json::to_string(&format!(
            "\"{}\" --exact windows::stdin_probe_child --nocapture",
            std::env::current_exe().unwrap().display()
        ))
        .unwrap();
        evaluate(
            &desktop,
            format!(
                r#"
            -- This pipe remains streaming and returns itself from successful writes.
            -- 此管道保持流式行为，并在写入成功时返回自身。
            local pipe = assert(io.popen({write_command}, "wb"))
            assert(io.type(pipe) == "file")
            assert(pipe:setvbuf("no"))
            assert(pipe:write("hello\0world") == pipe)
            assert(pipe:flush())
            -- Closing through the io table must use the custom process exit status.
            -- 通过 io 表关闭必须使用自定义进程退出状态。
            local ok, reason, code = io.close(pipe)
            assert(ok and reason == "exit" and code == 0)
            assert(io.type(pipe) == "closed file")
            assert(not pcall(function() pipe:read() end))
            assert(os.execute() == true)
            -- Native read formats and default input dispatch preserve CRT behavior.
            -- 原生读取格式和默认输入分派保留 CRT 行为。
            local reader = assert(io.popen("echo 12.5 alpha", "r"))
            io.input(reader)
            assert(io.read("*n") == 12.5)
            assert(io.read("*l") == " alpha")
            assert(io.read("*a") == "")
            assert(reader:close())
            -- Nonzero commands must return nil/exit/code rather than claiming success.
            -- 非零命令必须返回空值、exit、退出码，不能声称成功。
            local failed, why, status = os.execute("exit /b 7")
            assert(failed == nil and why == "exit" and status == 7)
            -- Abandoned buffered writers must kill non-reading children before flushing, under the host deadline.
            -- 在宿主截止时间内，被放弃的缓冲写入器必须先终止不读取输入的子进程，再刷新。
            local abandoned = assert(io.popen("ping -n 60 127.0.0.1 >nul", "wb"))
            assert(abandoned:setvbuf("full", 1048576))
            assert(abandoned:write(string.rep("x", 524288)))
            abandoned = nil
            collectgarbage("collect")
            return "HOST_MODE_OK"
        "#
            ),
        );
    }

    /// Launch the acceptance host without a console and bound total duration; returns no value.
    /// 无控制台启动验收宿主并限制总时长；无返回值。
    #[test]
    fn desktop_and_console_modes_from_windowless_host() {
        run_windowless_host(false);
    }

    /// Force a non-breakaway host job and verify every managed retry retains desktop window suppression.
    /// 强制不可脱离宿主 Job，并验证所有托管重试都保留桌面窗口隐藏策略。
    #[test]
    fn desktop_window_policy_survives_job_retry() {
        run_windowless_host(true);
    }

    /// Launch the isolated host; `block_breakaway` selects a restrictive job; returns after bounded verification.
    /// 启动隔离宿主；`block_breakaway` 选择限制性 Job；在有界验证后返回。
    fn run_windowless_host(block_breakaway: bool) {
        // Concurrent detached hosts produced Windows DLL_INIT_FAILED before console probe code could run.
        // 并发分离宿主曾在控制台探针代码运行前触发 Windows DLL_INIT_FAILED。
        let _fixture_guard = HOST_FIXTURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Configure the child environment locally, without modifying this test process's environment.
        // 局部配置子进程环境，不改变本测试进程的环境。
        let mut command = Command::new(std::env::current_exe().unwrap());
        if block_breakaway {
            command.env("LUASKILLS_WINDOW_BLOCK_BREAKAWAY", "1");
        }
        // Both streams are drained concurrently to prevent child test output from filling a pipe.
        // 并发排空两个输出流，防止子测试输出填满管道。
        let mut child = command
            .args(["--exact", "windows::windowless_host_fixture", "--nocapture"])
            .current_dir(std::env::current_exe().unwrap().parent().unwrap())
            .env("LUASKILLS_WINDOW_FIXTURE", "1")
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Stdout reader owns exactly the child's captured stdout endpoint.
        // 标准输出读取器恰好拥有子进程捕获的标准输出端点。
        let stdout = child.stdout.take().unwrap();
        // Stderr reader independently drains diagnostics while child assertions run.
        // 标准错误读取器在子进程断言运行期间独立排空诊断。
        let stderr = child.stderr.take().unwrap();
        // Fixed deadline prevents a regression in popen EOF handling from hanging the suite.
        // 固定截止时间防止 popen 文件结束处理回归导致测试套件挂起。
        let deadline = Instant::now() + Duration::from_secs(90);
        std::thread::scope(|scope| {
            // The stdout task returns complete captured evidence on both success and failure.
            // 标准输出任务在成功和失败时都返回完整捕获证据。
            let output = scope.spawn(move || read_all(stdout));
            // The stderr task returns independent diagnostic evidence.
            // 标准错误任务返回独立诊断证据。
            let errors = scope.spawn(move || read_all(stderr));
            // Polling keeps cleanup under the parent test's explicit deadline.
            // 轮询使清理受父测试显式截止时间约束。
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    break child.wait().unwrap();
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(
                status.success(),
                "stdout={}\nstderr={}",
                output.join().unwrap(),
                errors.join().unwrap()
            );
        });
    }

    /// Drain `reader` completely and return its UTF-8 diagnostics without stopping at embedded non-UTF8 data.
    /// 完全排空 `reader` 并返回 UTF-8 诊断，不因内嵌非 UTF-8 数据而停止。
    fn read_all(mut reader: impl Read) -> String {
        // Byte storage is owned by the reader task until diagnostics are joined by the parent.
        // 字节存储由读取任务拥有，直到父任务合并诊断。
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
