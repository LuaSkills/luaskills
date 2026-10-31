//! Exercise real C callback pointers and per-engine database registration ownership.
//! 检验真实 C 回调指针及每引擎数据库注册所有权。

use super::*;
use serde_json::json;

/// Build one explicit UTF-8 binding/input fixture; return a request with no filesystem side effects.
/// 构造显式 UTF-8 绑定及输入夹具；返回不产生文件系统副作用的请求。
fn parameter_request() -> RuntimeSqliteProviderRequest {
    RuntimeSqliteProviderRequest {
        action: RuntimeSqliteProviderAction::QueryJson,
        binding: crate::RuntimeDatabaseBindingContext::new(
            crate::RuntimeDatabaseBindingContextSpec {
                space_label: "ROOT".into(),
                skill_id: "parameter-owner".into(),
                root_name: "ROOT".into(),
                space_root: "D:/数据库/databases".into(),
                skill_dir: "D:/数据库/skills/parameter-owner".into(),
                skill_dir_name: "parameter-owner".into(),
                database_kind: crate::RuntimeDatabaseKind::Sqlite,
                default_database_path:
                    "D:/数据库/databases/sqlite/parameter-owner/parameter-owner.sqlite3".into(),
            },
        ),
        input: json!({"params":[null,false,0,1.25,"中文\u{0}参数"],"array":[],"object":{},"nested":{"value":null}}),
    }
}

/// Copy a live UTF-8 C string into an owned value for callback observations; return the exact text.
/// 将有效 UTF-8 C 字符串复制为回调观测用自有值；返回精确文本。
/// # Safety
/// # 安全性
/// `value` must point to a terminated string that remains readable for this call.
/// `value` 必须指向本次调用期间可读且以零结尾的字符串。
unsafe fn callback_text(value: *const c_char) -> String {
    // SAFETY: The caller supplies one live borrowed C string from the runtime request.
    // 安全性：调用方提供运行时请求中的有效借用 C 字符串。
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .unwrap()
        .to_owned()
}

/// Echo all standard callback fields into an allocated JSON response; return the allocation status.
/// 将标准回调全部字段回传至已分配 JSON 响应；返回分配状态。
unsafe extern "C" fn echo_standard_parameters(
    request: *const FfiSqliteProviderRequest,
    user_data: *mut c_void,
    response: *mut FfiOwnedBuffer,
    error: *mut FfiOwnedBuffer,
) -> i32 {
    // SAFETY: The runtime retains the complete request and all nested borrowed storage during this call.
    // 安全性：运行时在调用期间保留完整请求及全部嵌套借用存储。
    let request = unsafe { &*request };
    // SAFETY: The input buffer belongs to the same live request and is valid for its declared length.
    // 安全性：输入缓冲属于同一个有效请求，且其声明长度范围有效。
    let input: Value = serde_json::from_slice(unsafe {
        std::slice::from_raw_parts(request.input_json.ptr, request.input_json.len)
    })
    .unwrap();
    // All eight string fields are copied while their runtime-owned CString storage is still alive.
    // 在运行时拥有的 CString 存储仍有效时复制全部八个字符串字段。
    let body = unsafe { json!({"action":request.action, "user_data":user_data as usize, "input":input,
        "binding":{"space_label":callback_text(request.binding.space_label),"skill_id":callback_text(request.binding.skill_id),
        "binding_tag":callback_text(request.binding.binding_tag),"root_name":callback_text(request.binding.root_name),
        "space_root":callback_text(request.binding.space_root),"skill_dir":callback_text(request.binding.skill_dir),
        "skill_dir_name":callback_text(request.binding.skill_dir_name),"database_kind":request.binding.database_kind,
        "default_database_path":callback_text(request.binding.default_database_path)}}) }.to_string();
    // SAFETY: Output pointers are writable and borrowed response bytes outlive their synchronous copy.
    // 安全性：输出指针可写，借用响应字节存活超过同步复制过程。
    unsafe { luaskills_ffi_buffer_clone(body.as_ptr(), body.len(), response, error) }
}

/// Verify all stable action integers, complete binding fields, and lossless JSON input through real C calls.
/// 通过真实 C 调用验证全部稳定动作整数、完整绑定字段及无损 JSON 输入。
#[test]
fn standard_ffi_database_parameters_preserve_every_field() {
    // This order is the public action numbering contract, independently checked against the bridge.
    // 此顺序是公开动作编号契约，独立用于核验桥接。
    let actions = [
        "execute_script",
        "execute_batch",
        "query_json",
        "query_stream",
        "query_stream_wait_metrics",
        "query_stream_chunk",
        "query_stream_close",
        "tokenize_text",
        "upsert_custom_word",
        "remove_custom_word",
        "list_custom_words",
        "ensure_fts_index",
        "rebuild_fts_index",
        "upsert_fts_document",
        "delete_fts_document",
        "search_fts",
    ];
    for (code, action) in actions.into_iter().enumerate() {
        // Only the requested action varies; every other field must survive unmodified.
        // 仅请求动作发生变化；其他每个字段必须保持不变。
        let mut request = parameter_request();
        request.action = serde_json::from_value(json!(action)).unwrap();
        // The wire represents SQLite by its documented integer rather than the Rust enum string.
        // 线协议使用文档规定的整数表示 SQLite，而非 Rust 枚举字符串。
        let mut binding = serde_json::to_value(&request.binding).unwrap();
        binding["database_kind"] = json!(0);
        assert_eq!(
            invoke_standard_sqlite_provider_callback(echo_standard_parameters, 71, &request)
                .unwrap(),
            json!({"action":code,"user_data":71,"binding":binding,"input":request.input})
        );
    }
}

/// Return allocated response bytes plus malformed UTF-8 diagnostics; `user_data` selects success or failure.
/// 返回已分配响应字节及非法 UTF-8 诊断；`user_data` 选择成功或失败。
unsafe extern "C" fn malformed_json_diagnostic(
    _request: FfiBorrowedBuffer,
    user_data: *mut c_void,
    response: *mut FfiOwnedBuffer,
    error: *mut FfiOwnedBuffer,
) -> i32 {
    // SAFETY: Both output slots are writable; each allocation transfers exactly once to the bridge.
    // 安全性：两个输出槽均可写；每个分配仅向桥接转移一次。
    unsafe {
        *response = alloc_owned_buffer_from_vec(b"{}".to_vec());
        *error = alloc_owned_buffer_from_vec(vec![0xff]);
    }
    user_data as usize as i32
}

/// Supply the same malformed diagnostic fixture through the structured ABI; return the selected status.
/// 通过结构化 ABI 提供相同非法诊断夹具；返回所选状态。
unsafe extern "C" fn malformed_standard_diagnostic(
    _request: *const FfiSqliteProviderRequest,
    user_data: *mut c_void,
    response: *mut FfiOwnedBuffer,
    error: *mut FfiOwnedBuffer,
) -> i32 {
    // SAFETY: The delegated fixture does not inspect its request; both owned output slots are live.
    // 安全性：委托夹具不读取请求；两个自有输出槽均有效。
    unsafe {
        malformed_json_diagnostic(
            FfiBorrowedBuffer {
                ptr: ptr::null(),
                len: 0,
            },
            user_data,
            response,
            error,
        )
    }
}

/// Invalid diagnostic bytes remain explicit failures for both transports and both callback statuses.
/// 两种传输和两种回调状态下，非法诊断字节均保持显式失败。
#[test]
fn ffi_database_callbacks_reject_malformed_diagnostics() {
    for status in [0, 1] {
        assert!(
            invoke_json_provider_callback(malformed_json_diagnostic, status, "{}")
                .unwrap_err()
                .contains("non-utf8")
        );
        assert!(
            invoke_standard_sqlite_provider_callback(
                malformed_standard_diagnostic,
                status,
                &parameter_request()
            )
            .unwrap_err()
            .contains("non-utf8")
        );
    }
}

/// Return JSON identifying `user_data`; request and output pointers are borrowed for this call.
/// 返回标识 `user_data` 的 JSON；请求及输出指针仅在本次调用期间借用。
/// Return zero on successful allocation, preserving the public LuaSkills buffer ownership contract.
/// 成功分配时返回零，保留公开 LuaSkills 缓冲所有权契约。
unsafe extern "C" fn sqlite_callback(
    request: *const FfiSqliteProviderRequest,
    user_data: *mut c_void,
    response: *mut FfiOwnedBuffer,
    error: *mut FfiOwnedBuffer,
) -> i32 {
    // SAFETY: The runtime supplies a live request throughout this synchronous callback.
    // 安全性：运行时在整个同步回调期间提供有效请求。
    assert_eq!(
        unsafe { (*request).action },
        FFI_SQLITE_PROVIDER_ACTION_QUERY_JSON
    );
    // Identity travels through the opaque host pointer without dereferencing host memory.
    // 身份通过不透明宿主指针传递，不解引用宿主内存。
    let body = format!("{{\"owner\":{}}}", user_data as usize);
    // SAFETY: All buffers are valid for this call and the allocator copies the borrowed bytes.
    // 安全性：全部缓冲在调用期间有效，分配器复制借用字节。
    unsafe { luaskills_ffi_buffer_clone(body.as_ptr(), body.len(), response, error) }
}

/// Echo `user_data` through the JSON callback ABI; verify the exact query envelope and return allocation status.
/// 通过 JSON 回调 ABI 回传 `user_data`；核验精确查询包络并返回分配状态。
unsafe extern "C" fn sqlite_json_callback(
    request: FfiBorrowedBuffer,
    user_data: *mut c_void,
    response: *mut FfiOwnedBuffer,
    error: *mut FfiOwnedBuffer,
) -> i32 {
    // SAFETY: The runtime supplies borrowed bytes valid until this callback returns.
    // 安全性：运行时提供的借用字节在此回调返回前有效。
    let request: crate::RuntimeSqliteProviderRequest =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(request.ptr, request.len) })
            .unwrap();
    assert_eq!(request.action, RuntimeSqliteProviderAction::QueryJson);
    assert_eq!(request.binding.skill_id, "database-host");
    assert_eq!(request.input, json!({"sql":"select 1"}));
    // The host pointer remains opaque; only its integer fixture identity is observed.
    // 宿主指针保持不透明；仅观察其整数夹具身份。
    let body = format!("{{\"owner\":{}}}", user_data as usize);
    // SAFETY: All output buffers are writable and the allocator copies live input bytes.
    // 安全性：所有输出缓冲均可写，分配器复制有效输入字节。
    unsafe { luaskills_ffi_buffer_clone(body.as_ptr(), body.len(), response, error) }
}

/// Create two engines through the new ABI and invoke actual skills; verify callback context and null rejection.
/// 通过新 ABI 创建两个引擎并调用真实技能；验证回调上下文及空值拒绝。
#[test]
fn standard_ffi_instance_database_registration() {
    // This unique fixture owns the test package and is removed only after both engines are freed.
    // 此唯一夹具拥有测试包，仅在两个引擎释放后删除。
    let directory = std::env::temp_dir().join(format!(
        "luaskills-instance-database-{}",
        std::process::id()
    ));
    // Package contents use the production manifest, loader and SQLite Lua bridge.
    // 包内容使用生产清单、加载器和 SQLite Lua 桥接。
    let skill_dir = directory.join("skills/database-host");
    std::fs::create_dir_all(skill_dir.join("runtime")).unwrap();
    std::fs::write(skill_dir.join("skill.yaml"), "name: database-host\nversion: 0.1.0\nenable: true\nsqlite:\n  enable: true\nentries:\n  - name: query\n    lua_entry: runtime/query.lua\n    lua_module: database_host_query\n").unwrap();
    std::fs::write(skill_dir.join("runtime/query.lua"), "return function() return vulcan.json.encode(vulcan.sqlite.query_json({sql='select 1'})) end").unwrap();
    // CString storage outlives every FFI constructor and all borrowed options.
    // CString 存储存活超过每个 FFI 构造器及全部借用选项。
    let root = ffi_test_path_cstring(&directory, "root");
    // Reuse the validated v4 host-mode fixture with an initially empty pool.
    // 复用经过校验的 v4 宿主模式夹具，虚拟机池初始为空。
    let mut options = host_mode::options(&root, 0);
    // Every native handle is explicitly freed before callback context can expire.
    // 每个原生句柄均在回调上下文失效前显式释放。
    let mut handles = Vec::new();
    for owner in [11usize, 22] {
        options.base.host.base.base.sqlite_callback_mode = if owner == 11 {
            FFI_CALLBACK_MODE_STANDARD
        } else {
            FFI_CALLBACK_MODE_JSON
        };
        // Only the callbacks supplied in this descriptor belong to the new engine.
        // 只有此描述符中提供的回调属于新引擎。
        let callbacks = FfiDatabaseProviderCallbacksV1 {
            user_data: owner as *mut c_void,
            sqlite: if owner == 11 {
                Some(sqlite_callback)
            } else {
                None
            },
            sqlite_json: if owner == 22 {
                Some(sqlite_json_callback)
            } else {
                None
            },
        };
        // Nonzero initial handle verifies failure/success output initialization.
        // 非零初始句柄验证失败及成功时的输出初始化。
        let mut handle = u64::MAX;
        // Owned diagnostics are empty on success and freed on failure.
        // 成功时自有诊断为空，失败时予以释放。
        let mut error = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: The complete descriptor and nested string storage remain live throughout construction.
        // 安全性：完整描述符及嵌套字符串存储在整个构造期间保持有效。
        assert_eq!(
            unsafe {
                luaskills_ffi_engine_new_with_database_providers_v1(
                    &options,
                    &callbacks,
                    &mut handle,
                    &mut error,
                )
            },
            FFI_STATUS_OK,
            "{}",
            read_owned_buffer_text(&error)
        );
        crate::ffi::with_engine_mut(handle, |engine| {
            engine
                .load_from_roots(&[RuntimeSkillRoot {
                    name: "ROOT".into(),
                    skills_dir: directory.join("skills"),
                }])
                .map_err(|error| error.to_string())
        })
        .unwrap();
        handles.push((owner, handle));
    }
    for (owner, handle) in handles {
        // The real callback returns its original host context after both engines have been registered.
        // 两个引擎注册后，真实回调仍返回其原始宿主上下文。
        let result = crate::ffi::with_engine(handle, |engine| {
            engine.call_skill("database-host-query", &json!({}), None)
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result.content).unwrap(),
            json!({"owner":owner})
        );
        // Writable error storage remains valid during free.
        // 可写错误存储在释放期间保持有效。
        let mut error = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: The handle is live and freed exactly once here.
        // 安全性：句柄有效，且仅在此释放一次。
        assert_eq!(
            unsafe { luaskills_ffi_engine_free(handle, &mut error) },
            FFI_STATUS_OK
        );
    }
    // Missing callback descriptors are invalid and must never publish a handle.
    // 缺失的回调描述符无效，绝不能发布句柄。
    let mut handle = u64::MAX;
    // The diagnostic buffer is reclaimed after checking null-input rejection.
    // 检查空输入拒绝后回收诊断缓冲。
    let mut error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    // SAFETY: Null descriptor is an explicitly supported validation input; other pointers are valid.
    // 安全性：空描述符是显式支持的校验输入；其他指针有效。
    assert_ne!(
        unsafe {
            luaskills_ffi_engine_new_with_database_providers_v1(
                &options,
                ptr::null(),
                &mut handle,
                &mut error,
            )
        },
        FFI_STATUS_OK
    );
    assert_eq!(handle, 0);
    // SAFETY: The diagnostic was allocated by LuaSkills and is freed once.
    // 安全性：诊断由 LuaSkills 分配，且仅释放一次。
    unsafe { luaskills_ffi_buffer_free(error) };
    std::fs::remove_dir_all(directory).unwrap();
}
