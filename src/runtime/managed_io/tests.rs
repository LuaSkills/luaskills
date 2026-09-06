use super::*;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use crate::runtime::render_host_visible_path;

#[cfg(windows)]
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Return the Windows process lifetime peak working-set size in bytes.
/// 返回 Windows 当前进程生命周期峰值工作集字节数。
///
/// Returns a native process-memory snapshot or the operating-system diagnostic.
/// 返回原生进程内存快照，失败时返回操作系统诊断。
#[cfg(windows)]
fn windows_peak_working_set_bytes() -> Result<u64, String> {
    // Counters receives the native process-memory structure populated by Windows.
    // Counters 接收由 Windows 填充的原生进程内存结构。
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // Status reports whether the native snapshot was populated successfully.
    // Status 表示原生快照是否成功填充。
    let status = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if status == 0 {
        return Err(format!(
            "GetProcessMemoryInfo failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(counters.PeakWorkingSetSize as u64)
}

/// Build a platform shell command that emits a large stderr stream and optional stdout marker.
/// 构造一个输出大量 stderr 以及可选 stdout 标记的平台 shell 命令。
///
/// # Parameters
/// - `include_stdout`: Whether the command should emit the success marker on stdout.
/// - `include_stdout`：命令是否应在 stdout 输出成功标记。
///
/// # Returns
/// Shell command text accepted by the production popen implementation.
/// 生产 popen 实现可接受的 shell 命令文本。
fn large_stderr_command(include_stdout: bool) -> String {
    #[cfg(windows)]
    {
        // stdout_statement preserves the observable stdout contract for the mixed-stream case.
        // stdout_statement 为混合输出流场景保留可观察的 stdout 契约。
        let stdout_statement = if include_stdout {
            " & echo stdout-ok"
        } else {
            ""
        };
        format!(
            "(for /L %i in (1,1,65536) do @echo 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef 1>&2){stdout_statement}"
        )
    }

    #[cfg(not(windows))]
    {
        // stdout_statement preserves the observable stdout contract for the mixed-stream case.
        // stdout_statement 为混合输出流场景保留可观察的 stdout 契约。
        let stdout_statement = if include_stdout {
            "; printf 'stdout-ok'"
        } else {
            ""
        };
        format!("head -c 4194304 /dev/zero 1>&2{stdout_statement}")
    }
}

/// Verify a large ignored stderr stream cannot block or inflate the managed popen result.
/// 验证大量被忽略的 stderr 不会阻塞托管 popen，也不会扩大其返回结果。
#[test]
fn managed_popen_discards_large_stderr_without_blocking_stdout() {
    // command emits well beyond ordinary pipe capacity before the stdout marker.
    // command 会先输出远超普通管道容量的内容，再输出 stdout 标记。
    let command = large_stderr_command(true);
    // output is the public managed popen result, which must contain stdout only.
    // output 是托管 popen 的公开结果，必须只包含 stdout。
    let output = run_managed_popen_read(
        &command,
        ManagedPopenOptions {
            encoding: RuntimeTextEncoding::Utf8,
            timeout_ms: 30_000,
        },
    )
    .expect("large stderr command must complete");

    assert!(output.success);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "stdout-ok");
}

/// Verify a command that only emits large stderr completes with an empty captured buffer.
/// 验证只输出大量 stderr 的命令能够完成，并返回空捕获缓冲区。
#[test]
fn managed_popen_discards_stderr_only_output() {
    // command emits only ignored stderr bytes.
    // command 只输出被忽略的 stderr 字节。
    let command = large_stderr_command(false);
    // output proves stderr is not retained in the captured stdout buffer.
    // output 证明 stderr 不会保留在捕获的 stdout 缓冲区中。
    let output = run_managed_popen_read(
        &command,
        ManagedPopenOptions {
            encoding: RuntimeTextEncoding::Utf8,
            timeout_ms: 30_000,
        },
    )
    .expect("stderr-only command must complete");

    assert!(output.success);
    assert!(output.stdout.is_empty());
}

/// Verify a 100 MiB stderr producer does not inflate the parent process working set.
/// 验证 100 MiB stderr 生产者不会扩大父进程工作集。
#[cfg(windows)]
#[test]
#[ignore = "explicit Windows peak-working-set acceptance measurement"]
fn large_ignored_stderr_has_bounded_parent_peak_working_set() {
    // Payload is one 1024-byte line body reused by the Windows command loop.
    // Payload 是由 Windows 命令循环复用的 1024 字节行内容。
    let payload = "x".repeat(1024);
    // Command emits more than 100 MiB through the same proven cmd.exe loop as functional tests.
    // Command 通过与功能测试相同且已经验证的 cmd.exe 循环输出超过 100 MiB。
    let command = format!("(for /L %i in (1,1,102400) do @echo {payload} 1>&2)");
    // PeakBefore snapshots cumulative parent-process peak immediately before the operation.
    // PeakBefore 在操作前立即记录父进程累计峰值。
    let peak_before = windows_peak_working_set_bytes().expect("read pre-popen peak memory");
    // StartedAt measures complete process launch, output production, and reap time.
    // StartedAt 测量完整进程启动、输出产生与回收耗时。
    let started_at = std::time::Instant::now();
    // Output must remain empty because the workload writes only ignored stderr.
    // Output 必须保持为空，因为工作负载只写入被忽略的 stderr。
    let output = run_managed_popen_read(
        &command,
        ManagedPopenOptions {
            encoding: RuntimeTextEncoding::Utf8,
            timeout_ms: 120_000,
        },
    )
    .expect("100 MiB ignored stderr command should complete");
    // Elapsed records the end-to-end measured duration.
    // Elapsed 记录端到端实测时长。
    let elapsed = started_at.elapsed();
    // PeakAfter includes the complete managed popen operation in the process lifetime maximum.
    // PeakAfter 在进程生命周期峰值中包含完整托管 popen 操作。
    let peak_after = windows_peak_working_set_bytes().expect("read post-popen peak memory");
    // PeakDelta is the additional parent working set attributable to the isolated test.
    // PeakDelta 是当前隔离测试可归因的父进程额外工作集。
    let peak_delta = peak_after.saturating_sub(peak_before);

    assert!(output.success);
    assert!(output.stdout.is_empty());
    assert!(
        peak_delta < 32 * 1024 * 1024,
        "ignored 100 MiB stderr must keep parent peak delta below 32 MiB, measured {peak_delta} bytes"
    );
    println!(
        "MANAGED_POPEN_PERF stderr_bytes=104857600 peak_before={peak_before} peak_after={peak_after} peak_delta={peak_delta} elapsed_ms={}",
        elapsed.as_millis()
    );
}

/// Verify managed IO removes a Windows verbatim prefix before filesystem lookup.
/// 验证托管 IO 会在文件系统寻址前移除 Windows 逐字路径前缀。
#[cfg(windows)]
#[test]
fn managed_io_path_argument_strips_windows_verbatim_prefix() {
    // Lua state used to create the exact string value accepted by the managed IO boundary.
    // 用于创建托管 IO 边界所接收精确字符串值的 Lua 状态。
    let lua = Lua::new();
    // Normalized path returned by the production argument parser.
    // 生产参数解析器返回的归一化路径。
    let normalized = require_path_arg(
        LuaValue::String(
            lua.create_string(r"\\?\C:\runtime\data.txt")
                .expect("create verbatim path string"),
        ),
        "vulcan.io.read_text",
        "path",
    )
    .expect("normalize managed IO path");
    assert_eq!(normalized, r"C:\runtime\data.txt");
}

/// Verify managed IO rejects Windows verbatim namespaces without ordinary path equivalents.
/// 验证托管 IO 会拒绝不存在普通路径等价形式的 Windows verbatim 命名空间。
#[cfg(windows)]
#[test]
fn managed_io_path_argument_rejects_unsupported_verbatim_namespace() {
    // Lua state used to create the exact unsupported volume-namespace string.
    // 用于创建精确不受支持卷命名空间字符串的 Lua 状态。
    let lua = Lua::new();
    // Parser failure returned before managed IO performs any filesystem operation.
    // 在托管 IO 执行任何文件系统操作前返回的解析失败。
    let error = require_path_arg(
        LuaValue::String(
            lua.create_string(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\data.txt")
                .expect("create unsupported verbatim path string"),
        ),
        "vulcan.io.read_text",
        "path",
    )
    .expect_err("unsupported managed IO namespace must fail");
    assert!(
        error
            .to_string()
            .contains("unsupported Windows verbatim path namespace"),
        "unexpected managed IO error: {error}"
    );
}

/// Verify managed read_text decodes GB18030 content.
/// 验证托管 read_text 可以解码 GB18030 内容。
#[test]
fn managed_io_read_text_decodes_gb18030() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_gb18030_{}.txt",
        std::process::id()
    ));
    let bytes =
        encode_runtime_text("中文", RuntimeTextEncoding::Gb18030).expect("encode gb18030 content");
    fs::write(&path, bytes).expect("write test file");
    lua.globals().set("vio", io_table).expect("set io table");
    let script = format!(
        "return vio.read_text({}, {{ encoding = 'gb18030' }})",
        lua_path_literal(&path)
    );
    let value: String = lua.load(&script).eval().expect("read text through Lua");
    assert_eq!(value, "中文");
    let _ = fs::remove_file(path);
}

/// Verify managed file state remains usable after its lock is poisoned.
/// 验证托管文件状态锁 poison 后仍可继续使用。
#[test]
fn managed_io_file_state_recovers_after_poisoned_lock() {
    // Managed file handle whose internal state lock is intentionally poisoned.
    // 内部状态锁会被故意 poison 的托管文件句柄。
    let file = ManagedIoFile::from_read_buffer(
        "poisoned-managed-file".to_string(),
        ManagedIoOpenMode {
            kind: ManagedIoModeKind::Read,
            binary: false,
            update: false,
        },
        RuntimeTextEncoding::Utf8,
        b"hello".to_vec(),
        None,
    );

    // Captured panic result from a holder that poisons the managed file state lock.
    // 托管文件状态锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the managed file state lock.
        // 仅用于制造托管文件状态锁 poison 的保护对象。
        let _guard = file.state.lock().expect("initial managed file state lock");
        panic!("poison managed file state lock for recovery test");
    }));

    assert!(poison_result.is_err());
    assert!(
        !file
            .is_closed()
            .expect("read managed file state after poison")
    );
}

/// Verify managed IO compatibility state remains usable after its lock is poisoned.
/// 验证托管 IO 兼容状态锁 poison 后仍可继续使用。
#[test]
fn managed_io_compat_state_recovers_after_poisoned_lock() {
    // Compatibility state whose lock is intentionally poisoned.
    // 锁会被故意 poison 的兼容状态。
    let state = Arc::new(Mutex::new(ManagedIoCompatState {
        current_input: None,
        current_output: None,
    }));

    // Captured panic result from a holder that poisons the compatibility state lock.
    // 兼容状态锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the managed IO compatibility state lock.
        // 仅用于制造托管 IO 兼容状态锁 poison 的保护对象。
        let _guard = state.lock().expect("initial managed io compat state lock");
        panic!("poison managed io compat state lock for recovery test");
    }));

    assert!(poison_result.is_err());
    assert!(flush_compat_output(state).expect("flush compat output after poison"));
}

/// Verify managed read_text uses the table default encoding when options are omitted.
/// 验证托管 read_text 在省略选项时会使用表级默认编码。
#[test]
fn managed_io_read_text_uses_default_encoding() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Gb18030).expect("create vulcan.io");
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_default_gb18030_{}.txt",
        std::process::id()
    ));
    let bytes = encode_runtime_text("默认编码", RuntimeTextEncoding::Gb18030)
        .expect("encode default gb18030 content");
    fs::write(&path, bytes).expect("write default encoding test file");
    lua.globals().set("vio", io_table).expect("set io table");
    let script = format!("return vio.read_text({})", lua_path_literal(&path));
    let value: String = lua
        .load(&script)
        .eval()
        .expect("read text through default encoding");
    assert_eq!(value, "默认编码");
    let _ = fs::remove_file(path);
}

/// Verify io compatibility open supports read-all calls.
/// 验证 io 兼容 open 支持读取全部内容。
#[test]
fn managed_io_compat_open_reads_all() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    install_managed_io_compat(&lua, &io_table, RuntimeTextEncoding::Utf8)
        .expect("install managed io compat");
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_compat_{}.txt",
        std::process::id()
    ));
    fs::write(&path, "hello").expect("write test file");
    let script = format!(
        "local f = io.open({}, 'r'); local v = f:read('*a'); f:close(); return v",
        lua_path_literal(&path)
    );
    let value: String = lua.load(&script).eval().expect("read through io.open");
    assert_eq!(value, "hello");
    let _ = fs::remove_file(path);
}

/// Verify io.input sets the managed default input used by io.read.
/// 验证 io.input 会设置 io.read 使用的托管默认输入。
#[test]
fn managed_io_compat_input_feeds_read() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    install_managed_io_compat(&lua, &io_table, RuntimeTextEncoding::Utf8)
        .expect("install managed io compat");
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_input_{}.txt",
        std::process::id()
    ));
    fs::write(&path, "input-value").expect("write test file");
    let script = format!(
        "io.input({}); return io.read('*a')",
        lua_path_literal(&path)
    );
    let value: String = lua.load(&script).eval().expect("read through io.input");
    assert_eq!(value, "input-value");
    let _ = fs::remove_file(path);
}

/// Verify io.output sets the managed default output used by io.write.
/// 验证 io.output 会设置 io.write 使用的托管默认输出。
#[test]
fn managed_io_compat_output_receives_write() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    install_managed_io_compat(&lua, &io_table, RuntimeTextEncoding::Utf8)
        .expect("install managed io compat");
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_output_{}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    let script = format!(
        "io.output({}); io.write('out', '-', 'value'); io.close(); return true",
        lua_path_literal(&path)
    );
    let value: bool = lua.load(&script).eval().expect("write through io.output");
    assert!(value);
    assert_eq!(
        fs::read_to_string(&path).expect("read output file"),
        "out-value"
    );
    let _ = fs::remove_file(path);
}

/// Verify managed io.tmpfile supports write, seek, read, and close.
/// 验证托管 io.tmpfile 支持写入、定位、读取与关闭。
#[test]
fn managed_io_compat_tmpfile_supports_update_reads() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    install_managed_io_compat(&lua, &io_table, RuntimeTextEncoding::Utf8)
        .expect("install managed io compat");
    let script = "local f = io.tmpfile(); f:write('tmp-value'); f:seek('set', 0); local value = f:read('*a'); local ok = f:close(); return value, ok";
    let (value, ok): (String, bool) = lua.load(script).eval().expect("use managed tmpfile");
    assert_eq!(value, "tmp-value");
    assert!(ok);
}

/// Verify explicit temporary-file close reports deletion failure and remains retryable.
/// 验证显式关闭临时文件会报告删除失败，并保持可重试状态。
#[test]
fn managed_io_tmpfile_close_reports_delete_failure() {
    // TempPath is a directory so the close-time remove_file operation deterministically fails.
    // TempPath 是目录，因此关闭时的 remove_file 操作会确定性失败。
    let temp_path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_close_delete_failure_{}",
        std::process::id()
    ));
    if temp_path.exists() {
        // Stale fixture cleanup result is intentionally ignored before recreation.
        // 重建前对陈旧夹具的清理结果有意忽略。
        let _ = fs::remove_dir_all(&temp_path);
    }
    fs::create_dir_all(&temp_path).expect("temporary directory should be created");
    // File uses read mode so flush succeeds and the test reaches only the delete-on-close branch.
    // File 使用读取模式，使刷新成功且测试只到达关闭删除分支。
    let file = ManagedIoFile::from_read_buffer(
        "delete-failure-fixture".to_string(),
        ManagedIoOpenMode {
            kind: ManagedIoModeKind::Read,
            binary: false,
            update: false,
        },
        RuntimeTextEncoding::Utf8,
        Vec::new(),
        None,
    );
    {
        // State redirects the controlled handle to the directory fixture and enables temporary cleanup.
        // State 把受控句柄指向目录夹具，并启用临时文件清理。
        let mut state = file.lock_state();
        state.path = temp_path.clone();
        state.delete_on_close = true;
    }

    // Error must be observable through explicit close instead of being converted to success.
    // Error 必须通过显式 close 可见，而不能被转换为成功。
    let error = file
        .close()
        .expect_err("directory removal through remove_file should fail");

    assert!(
        error
            .to_string()
            .contains("failed to remove temporary file"),
        "unexpected error: {error}"
    );
    assert!(
        !file.lock_state().closed,
        "failed close must remain retryable"
    );
    // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
    // 对临时测试产物的清理结果按最佳努力原则有意忽略。
    let _ = fs::remove_dir_all(&temp_path);
}

/// Verify managed update modes support the common write-seek-read flow.
/// 验证托管更新模式支持常见的写入、回退定位、读取流程。
#[test]
fn managed_io_open_update_mode_supports_seek_read() {
    let lua = Lua::new();
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    install_managed_io_compat(&lua, &io_table, RuntimeTextEncoding::Utf8)
        .expect("install managed io compat");
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_update_{}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    let script = format!(
        "local f = io.open({}, 'w+'); f:write('update-value'); f:seek('set', 0); local value = f:read('*a'); f:close(); return value",
        lua_path_literal(&path)
    );
    let value: String = lua.load(&script).eval().expect("use managed update mode");
    assert_eq!(value, "update-value");
    assert_eq!(
        fs::read_to_string(&path).expect("read update mode file"),
        "update-value"
    );
    let _ = fs::remove_file(path);
}

/// Verify append-update mode starts from an empty buffer when the target file is missing.
/// 验证追加更新模式在目标文件缺失时会从空缓冲区开始。
#[test]
fn managed_io_open_append_update_creates_missing_file() {
    // Lua state used to exercise the public managed IO table.
    // 用于触发公开托管 IO 表的 Lua 状态。
    let lua = Lua::new();
    // Managed IO table under test.
    // 被测托管 IO 表。
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    // Missing file path opened through append-update mode.
    // 通过追加更新模式打开的缺失文件路径。
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_append_update_missing_{}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    lua.globals().set("vio", io_table).expect("set io table");
    let script = format!(
        "local f = vio.open({}, 'a+'); f:write('append-new'); f:seek('set', 0); local value = f:read('*a'); f:close(); return value",
        lua_path_literal(&path)
    );

    // Text read back from the append-update handle before it is closed.
    // 在关闭前从追加更新句柄读回的文本。
    let value: String = lua.load(&script).eval().expect("use append update mode");

    assert_eq!(value, "append-new");
    assert_eq!(
        fs::read_to_string(&path).expect("read append update output"),
        "append-new"
    );
    let _ = fs::remove_file(path);
}

/// Verify append-update mode reports non-missing read failures instead of treating them as empty files.
/// 验证追加更新模式会报告非缺失类读取失败，而不是把它们当成空文件。
#[test]
fn managed_io_open_append_update_rejects_directory_path() {
    // Lua state used to exercise the public managed IO table.
    // 用于触发公开托管 IO 表的 Lua 状态。
    let lua = Lua::new();
    // Managed IO table under test.
    // 被测托管 IO 表。
    let io_table =
        create_vulcan_io_table(&lua, RuntimeTextEncoding::Utf8).expect("create vulcan.io");
    // Directory path that must not be treated as an empty append target.
    // 不能被当作空追加目标处理的目录路径。
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_append_update_dir_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create append update directory target");
    lua.globals().set("vio", io_table).expect("set io table");
    let script = format!(
        "local ok, err = pcall(function() return vio.open({}, 'a+') end); return ok, tostring(err)",
        lua_path_literal(&path)
    );

    // Protected call result from opening a directory in append-update mode.
    // 以追加更新模式打开目录时保护调用返回的结果。
    let (ok, error): (bool, String) = lua
        .load(&script)
        .eval()
        .expect("append update directory open should be captured");

    assert!(!ok);
    assert!(error.contains("vulcan.io.open"));
    let _ = fs::remove_dir_all(path);
}

/// Verify repeated write-mode flushes persist only each newly appended byte after first publication.
/// 验证写入模式重复刷新在首次发布后只落盘每个新增字节。
#[test]
fn managed_io_write_repeated_flush_has_linear_physical_payload() {
    // Isolated output path used for deterministic physical-write accounting.
    // 用于确定性物理写入计数的隔离输出路径。
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_linear_write_{}_{}.bin",
        std::process::id(),
        TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_file(&path);
    // Binary write handle under the production incremental flush implementation.
    // 使用生产增量刷新实现的二进制写入句柄。
    let file = ManagedIoFile::open(
        path.clone(),
        parse_open_mode("wb").expect("binary write mode should parse"),
        RuntimeTextEncoding::Utf8,
    )
    .expect("managed write handle should open");
    // Lua state used to allocate the one-byte value passed through the real write conversion.
    // 用于分配经真实写入转换传递的单字节值的 Lua 状态。
    let lua = Lua::new();
    for _ in 0..1000 {
        // One-byte argument for the current logical write and immediate flush.
        // 当前逻辑写入并立即刷新的单字节参数。
        let value = LuaValue::String(
            lua.create_string(b"x")
                .expect("one-byte Lua string should be created"),
        );
        file.write_values(MultiValue::from_vec(vec![value]))
            .expect("one-byte managed write should succeed");
        file.flush()
            .expect("incremental managed flush should succeed");
    }
    // Test-only physical payload counter accumulated only after successful writes.
    // 仅在成功写入后累计的测试专属物理载荷计数。
    let physical_write_bytes = file.lock_state().physical_write_bytes;
    // Final bytes read from the backing file after every incremental flush.
    // 每次增量刷新后从底层文件读取的最终字节。
    let persisted = fs::read(&path).expect("incremental output should be readable");

    assert_eq!(persisted, vec![b'x'; 1000]);
    assert_eq!(physical_write_bytes, 1000);
    file.close().expect("managed write handle should close");
    let _ = fs::remove_file(path);
}

/// Verify update mode writes only the dirty union and preserves untouched existing bytes.
/// 验证更新模式只写入脏区并保留未触及的已有字节。
#[test]
fn managed_io_update_flush_writes_only_dirty_ranges() {
    // Existing update target whose untouched prefix, middle, and suffix must remain stable.
    // 未触及前缀、中段和后缀都必须保持稳定的已有更新目标。
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_dirty_update_{}_{}.bin",
        std::process::id(),
        TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_file(&path);
    fs::write(&path, b"abcdefghij").expect("update fixture should be written");
    // Binary read-update handle that starts synchronized with the existing ten-byte file.
    // 初始与已有十字节文件同步的二进制读更新句柄。
    let file = ManagedIoFile::open(
        path.clone(),
        parse_open_mode("r+b").expect("binary read-update mode should parse"),
        RuntimeTextEncoding::Utf8,
    )
    .expect("managed update handle should open");
    // Lua state used to allocate both real update-write values.
    // 用于分配两个真实更新写入值的 Lua 状态。
    let lua = Lua::new();

    file.seek(Some("set".to_string()), Some(2))
        .expect("first update seek should succeed");
    file.write_values(MultiValue::from_vec(vec![LuaValue::String(
        lua.create_string(b"XY")
            .expect("first update string should be created"),
    )]))
    .expect("first update write should succeed");
    file.flush().expect("first dirty flush should succeed");
    file.seek(Some("set".to_string()), Some(7))
        .expect("second update seek should succeed");
    file.write_values(MultiValue::from_vec(vec![LuaValue::String(
        lua.create_string(b"Z")
            .expect("second update string should be created"),
    )]))
    .expect("second update write should succeed");
    file.flush().expect("second dirty flush should succeed");

    // Test-only payload count proving two dirty writes did not rewrite the ten-byte file.
    // 证明两次脏写没有重写十字节文件的测试专属载荷计数。
    let physical_write_bytes = file.lock_state().physical_write_bytes;
    // Final update target retaining all bytes outside the two dirty ranges.
    // 保留两个脏区之外所有字节的最终更新目标。
    let persisted = fs::read(&path).expect("updated output should be readable");

    assert_eq!(persisted, b"abXYefgZij");
    assert_eq!(physical_write_bytes, 3);
    file.close().expect("managed update handle should close");
    let _ = fs::remove_file(path);
}

/// Overwrite one managed backing file until the host exposes a distinct filesystem generation.
/// 覆盖一个托管底层文件，直到宿主暴露不同的文件系统变更代。
///
/// The file parameter supplies the last successful managed generation.
/// file 参数提供上次成功的托管变更代。
///
/// The path parameter identifies the backing file changed by the external writer.
/// path 参数标识由外部写入方更改的底层文件。
///
/// The payload parameter is written repeatedly only across the host timestamp-resolution window.
/// payload 参数仅在宿主时间戳分辨率窗口内重复写入。
///
/// Returns after a distinct generation is observable; panics when the host never exposes one.
/// 在观察到不同变更代后返回；若宿主始终不暴露则触发 panic。
fn overwrite_until_managed_io_generation_changes(
    file: &ManagedIoFile,
    path: &Path,
    payload: &[u8],
) {
    // ExpectedFingerprint is the generation published by the preceding managed flush or open.
    // ExpectedFingerprint 是前一次托管刷新或打开所发布的变更代。
    let expected_fingerprint = file
        .lock_state()
        .flushed_fingerprint
        .clone()
        .expect("managed generation should exist before external overwrite");
    // Deadline bounds filesystems whose observable timestamps have unexpectedly coarse resolution.
    // Deadline 限制文件系统可观察时间戳分辨率异常粗糙的等待时间。
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        fs::write(path, payload).expect("external overwrite should succeed");
        // ActualFingerprint belongs to the observable file generation opened after the external write.
        // ActualFingerprint 属于外部写入后打开的可观察文件变更代。
        let actual_fingerprint = ManagedIoBackingFingerprint::from_file(
            &File::open(path).expect("externally changed file should open"),
        )
        .expect("external fingerprint should resolve");
        if actual_fingerprint != expected_fingerprint {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "filesystem never exposed a distinct generation for the external overwrite"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

/// Verify write-mode incremental flush restores the authoritative buffer after an observable external replacement.
/// 验证写入模式增量刷新会在可观察外部替换后恢复权威缓冲区。
#[test]
fn managed_io_write_flush_falls_back_after_external_replacement() {
    // Path isolates the externally replaced write target from parallel tests.
    // Path 将被外部替换的写入目标与并行测试隔离。
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_external_write_{}_{}.bin",
        std::process::id(),
        TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup_result = fs::remove_file(&path);
    // File retains one authoritative in-memory generation across both flushes.
    // File 在两次刷新之间保留单个权威内存代。
    let file = ManagedIoFile::open(
        path.clone(),
        parse_open_mode("wb").expect("binary write mode should parse"),
        RuntimeTextEncoding::Utf8,
    )
    .expect("managed write handle should open");
    // Lua allocates the exact byte strings passed through production conversion.
    // Lua 分配通过生产转换传递的精确字节字符串。
    let lua = Lua::new();
    file.write_values(MultiValue::from_vec(vec![LuaValue::String(
        lua.create_string(b"abc")
            .expect("initial string should be created"),
    )]))
    .expect("initial write should succeed");
    file.flush().expect("initial flush should succeed");
    overwrite_until_managed_io_generation_changes(&file, &path, b"XYZ");
    file.write_values(MultiValue::from_vec(vec![LuaValue::String(
        lua.create_string(b"d")
            .expect("suffix string should be created"),
    )]))
    .expect("suffix write should succeed");

    file.flush()
        .expect("changed backing file should trigger full publication");

    assert_eq!(
        fs::read(&path).expect("restored output should be readable"),
        b"abcd"
    );
    file.close().expect("managed write handle should close");
    let _cleanup_result = fs::remove_file(path);
}

/// Verify update-mode flush restores an observably changed file even when no new dirty range exists.
/// 验证更新模式刷新即使没有新脏区，也会恢复发生可观察变化的文件。
#[test]
fn managed_io_update_flush_restores_external_change_without_new_writes() {
    // Path isolates the update target whose backing bytes change behind the open handle.
    // Path 隔离在已打开句柄背后发生字节变化的更新目标。
    let path = std::env::temp_dir().join(format!(
        "luaskills_managed_io_external_update_{}_{}.bin",
        std::process::id(),
        TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup_result = fs::remove_file(&path);
    fs::write(&path, b"abcdef").expect("initial update fixture should be written");
    // File loads the original bytes as its authoritative update buffer.
    // File 把原始字节加载为权威更新缓冲区。
    let file = ManagedIoFile::open(
        path.clone(),
        parse_open_mode("r+b").expect("binary update mode should parse"),
        RuntimeTextEncoding::Utf8,
    )
    .expect("managed update handle should open");
    overwrite_until_managed_io_generation_changes(&file, &path, b"UVWXYZ");

    file.flush()
        .expect("unchanged logical buffer should restore its disk generation");

    assert_eq!(
        fs::read(&path).expect("restored update target should be readable"),
        b"abcdef"
    );
    file.close().expect("managed update handle should close");
    let _cleanup_result = fs::remove_file(path);
}

/// Verify a failed initial flush preserves pending write state for a later successful retry.
/// 验证首次刷新失败会保留待写状态，以供后续成功重试。
#[test]
fn managed_io_failed_flush_preserves_retryable_write_state() {
    // Initially missing parent directory that deterministically makes the first flush fail.
    // 初始缺失并会确定性导致首次刷新失败的父目录。
    let parent = std::env::temp_dir().join(format!(
        "luaskills_managed_io_flush_retry_{}_{}",
        std::process::id(),
        TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&parent);
    // Output path retained by the handle across the failed and successful flush attempts.
    // 句柄在失败与成功刷新尝试之间保留的输出路径。
    let path = parent.join("retry.bin");
    // Binary write handle whose open operation intentionally performs no early filesystem write.
    // 打开操作有意不提前执行文件系统写入的二进制写句柄。
    let file = ManagedIoFile::open(
        path.clone(),
        parse_open_mode("wb").expect("binary write mode should parse"),
        RuntimeTextEncoding::Utf8,
    )
    .expect("managed retry handle should open");
    // Lua state used to allocate the pending retry payload.
    // 用于分配待重试载荷的 Lua 状态。
    let lua = Lua::new();
    file.write_values(MultiValue::from_vec(vec![LuaValue::String(
        lua.create_string(b"retry")
            .expect("retry string should be created"),
    )]))
    .expect("managed retry write should succeed");

    assert!(file.flush().is_err());
    fs::create_dir_all(&parent).expect("retry parent should be created");
    file.flush()
        .expect("second flush should retry pending bytes");
    // Physical payload count excludes the failed filesystem operation.
    // 物理载荷计数不包含失败的文件系统操作。
    let physical_write_bytes = file.lock_state().physical_write_bytes;

    assert_eq!(
        fs::read(&path).expect("retry output should exist"),
        b"retry"
    );
    assert_eq!(physical_write_bytes, 5);
    file.close().expect("managed retry handle should close");
    let _ = fs::remove_dir_all(parent);
}

/// Verify stdout-backed io.write rejects Lua byte strings that are not valid UTF-8.
/// 验证写入 stdout 的 io.write 会拒绝非法 UTF-8 的 Lua 字节字符串。
#[test]
fn managed_io_display_text_rejects_invalid_utf8_string() {
    // Lua state used to allocate one raw byte string for the conversion helper.
    // 用于为转换辅助函数分配原始字节字符串的 Lua 状态。
    let lua = Lua::new();
    // Invalid Lua byte string that cannot be represented as UTF-8 text.
    // 无法表示为 UTF-8 文本的非法 Lua 字节字符串。
    let invalid_text = LuaValue::String(
        lua.create_string([0xff])
            .expect("create invalid utf-8 lua string"),
    );
    // Conversion error returned by the stdout display-text boundary.
    // stdout 展示文本边界返回的转换错误。
    let error =
        lua_value_to_display_text(invalid_text).expect_err("invalid utf-8 should be rejected");
    assert!(
        error.to_string().contains("valid UTF-8"),
        "unexpected error: {error}"
    );
}

/// Quote one Rust string for a compact Lua literal in tests.
/// 为测试生成一个紧凑的 Lua 字符串字面量。
fn lua_quote(value: &str) -> String {
    format!("{:?}", value)
}

/// Render one filesystem path as a Lua string literal for managed IO tests.
/// 将单个文件系统路径渲染为 managed IO 测试使用的 Lua 字符串字面量。
///
/// The path parameter is the host filesystem path passed into Lua test code.
/// path 参数是传入 Lua 测试代码的宿主文件系统路径。
///
/// Return a quoted Lua string literal containing the host-visible path text.
/// 返回包含宿主可见路径文本的 Lua 字符串字面量。
fn lua_path_literal(path: &Path) -> String {
    lua_quote(&render_host_visible_path(path))
}
