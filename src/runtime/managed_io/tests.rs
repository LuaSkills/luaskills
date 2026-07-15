use super::*;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use crate::runtime::render_host_visible_path;

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
