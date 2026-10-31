use super::*;

/// Build borrowed v4 options from `root`; `mode` is the raw C integer under test; returns a fully initialized value.
/// 根据 `root` 构建借用 v4 选项；`mode` 是待测原始 C 整数；返回完整初始化值。
pub(super) fn options(root: &CString, mode: i32) -> FfiLuaEngineOptionsV4 {
    FfiLuaEngineOptionsV4 {
        base: FfiLuaEngineOptionsV3 {
            pool: FfiLuaVmPoolConfig {
                min_size: 0,
                max_size: 1,
                idle_ttl_secs: 30,
            },
            host: FfiLuaRuntimeHostOptionsV3 {
                base: FfiLuaRuntimeHostOptionsV2 {
                    base: empty_ffi_runtime_host_options(),
                    runtime_root: root.as_ptr(),
                },
                managed_runtime_distribution_root: ptr::null(),
                managed_runtime_environment_root: ptr::null(),
                managed_runtime_config: ptr::null(),
            },
        },
        host_mode: mode,
    }
}

/// Check mode selection, old-parser defaults, and v4 engine creation/free; no inputs or outputs.
/// 检查模式选择、旧解析器默认值及 v4 引擎创建和释放；无输入或输出。
#[test]
fn standard_ffi_v4_host_mode_and_legacy_defaults() {
    // An isolated runtime root supplies the exact lifetime for all borrowed C strings.
    // 隔离运行时根为所有借用 C 字符串提供精确生命周期。
    let directory =
        std::env::temp_dir().join(format!("luaskills-host-mode-ffi-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    // The CString owner remains alive through every create and free call below.
    // CString 所有者在下方每次创建及释放调用期间保持存活。
    let root = ffi_test_path_cstring(&directory, "runtime_root");
    for (mode, expected) in [
        (0, crate::LuaRuntimeHostMode::Console),
        (1, crate::LuaRuntimeHostMode::Desktop),
    ] {
        // Each exact version parser must observe only its own defined fields.
        // 每个精确版本解析器只能观察它自己定义的字段。
        let options = options(&root, mode);
        assert_eq!(
            parse_engine_options_v4(&options)
                .unwrap()
                .host_options
                .host_mode,
            expected
        );
        assert_eq!(
            parse_engine_options_v3(&options.base)
                .unwrap()
                .host_options
                .host_mode,
            crate::LuaRuntimeHostMode::Console
        );
        // A valid writable handle output starts nonzero to prove the ABI initializes it.
        // 有效可写句柄输出初值为非零，用于证明 ABI 会初始化它。
        let mut handle = u64::MAX;
        // The error buffer starts empty and remains empty for successful creation and free.
        // 错误缓冲初始为空，并在成功创建及释放时保持为空。
        let mut error = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: All nested strings and output buffers outlive the synchronous call.
        // 安全性：所有嵌套字符串及输出缓冲都比同步调用存活更久。
        assert_eq!(
            unsafe { luaskills_ffi_engine_new_v4(&options, &mut handle, &mut error) },
            FFI_STATUS_OK,
            "{}",
            read_owned_buffer_text(&error)
        );
        assert!(handle != 0 && handle != u64::MAX);
        assert!(error.ptr.is_null());
        // SAFETY: This handle was successfully returned by the corresponding v4 creation call.
        // 安全性：此句柄由对应 v4 创建调用成功返回。
        assert_eq!(
            unsafe { luaskills_ffi_engine_free(handle, &mut error) },
            FFI_STATUS_OK
        );
        assert!(error.ptr.is_null());
    }
    assert_eq!(std::mem::offset_of!(FfiLuaEngineOptionsV4, base), 0);
    assert_eq!(
        std::mem::offset_of!(FfiLuaEngineOptionsV4, host_mode),
        std::mem::size_of::<FfiLuaEngineOptionsV3>()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

/// Reject invalid integers and null input without publishing an engine or leaking an error buffer.
/// 拒绝无效整数及空输入，不发布引擎或泄漏错误缓冲。
#[test]
fn standard_ffi_v4_rejects_invalid_host_mode() {
    // The path is deliberately nonexistent: mode validation must fail before touching runtime directories.
    // 路径有意不存在：模式验证必须在访问运行时目录之前失败。
    let root = CString::new("unused-host-mode-fixture").unwrap();
    for mode in [-1, 2, i32::MAX] {
        // The raw integer remains invalid instead of being normalized to console or desktop.
        // 原始整数保持无效，不被归一化为命令行或桌面模式。
        let options = options(&root, mode);
        // The handle must be cleared even when creation rejects the supplied mode.
        // 即使创建拒绝所提供模式，也必须清除句柄。
        let mut handle = u64::MAX;
        // The owned error is freed after its exact diagnostic has been checked.
        // 检查精确诊断后释放自有错误。
        let mut error = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: Valid borrowed inputs and writable outputs remain alive throughout this call.
        // 安全性：有效借用输入及可写输出在整个调用期间保持存活。
        assert_ne!(
            unsafe { luaskills_ffi_engine_new_v4(&options, &mut handle, &mut error) },
            FFI_STATUS_OK
        );
        assert_eq!(handle, 0);
        assert!(read_owned_buffer_text(&error).contains("host_mode must be"));
        // SAFETY: The buffer was allocated by LuaSkills and has not been freed.
        // 安全性：缓冲由 LuaSkills 分配且尚未释放。
        unsafe { luaskills_ffi_buffer_free(error) };
    }
    // Null-input checks preserve the same failure convention as v1-v3.
    // 空输入检查保留与 v1-v3 相同的失败约定。
    let mut handle = 99;
    // This final error buffer owns the null-input diagnostic until explicitly freed.
    // 本最终错误缓冲拥有空输入诊断，直到显式释放。
    let mut error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    // SAFETY: Null options are expressly supported as a rejected input; output pointers are valid.
    // 安全性：空选项是明确支持的被拒绝输入；输出指针有效。
    assert_ne!(
        unsafe { luaskills_ffi_engine_new_v4(ptr::null(), &mut handle, &mut error) },
        FFI_STATUS_OK
    );
    assert_eq!(handle, 0);
    // SAFETY: Ownership was returned by the failed creation call above.
    // 安全性：所有权由上方失败的创建调用返回。
    unsafe { luaskills_ffi_buffer_free(error) };
}
