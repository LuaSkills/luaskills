use super::*;
use crate::ffi::luaskills_ffi_string_free;
use crate::host::callbacks::{
    RuntimeModelCaller, dispatch_model_embed_request, dispatch_model_llm_request,
    runtime_model_callback_test_guard,
};
use crate::runtime::path::render_host_visible_path;
use crate::runtime_help::{
    RuntimeHelpDetail as RuntimeHelpDetailModel,
    RuntimeHelpNodeDescriptor as RuntimeHelpNodeDescriptorModel,
    RuntimeSkillHelpDescriptor as RuntimeSkillHelpDescriptorModel,
};
use crate::{
    RuntimeEntryDescriptor as RuntimeEntryDescriptorModel,
    RuntimeEntryParameterDescriptor as RuntimeEntryParameterDescriptorModel,
};
use std::path::Path;

/// Read one owned UTF-8 buffer into one Rust string without freeing it.
/// 将一个拥有型 UTF-8 缓冲读取为 Rust 字符串但不执行释放。
fn read_owned_buffer_text(buffer: &FfiOwnedBuffer) -> String {
    if buffer.ptr.is_null() || buffer.len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) };
    String::from_utf8(bytes.to_vec()).expect("buffer text must be utf-8")
}

/// Build one borrowed buffer view over one UTF-8 text while keeping backing storage alive.
/// 在保持底层存储存活的前提下，为一段 UTF-8 文本构造借用缓冲视图。
fn make_borrowed_buffer(text: &str) -> (Vec<u8>, FfiBorrowedBuffer) {
    let bytes = text.as_bytes().to_vec();
    let buffer = if bytes.is_empty() {
        FfiBorrowedBuffer {
            ptr: ptr::null(),
            len: 0,
        }
    } else {
        FfiBorrowedBuffer {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        }
    };
    (bytes, buffer)
}

/// Verify invocation allocation preserves a structured host_result payload.
/// 验证调用结果分配会保留结构化 host_result 载荷。
///
/// This test has no parameters and fails through assertions when the FFI allocation drops host_result data.
/// 本测试不接收参数；当 FFI 分配丢失 host_result 数据时会通过断言失败。
///
/// Return unit after validating the allocated payload and freeing the heap-owned result.
/// 校验已分配载荷并释放堆拥有结果后返回 unit。
#[test]
fn alloc_invocation_result_preserves_host_result_payload() {
    // Runtime invocation result carrying one structured host_result payload.
    // 携带单个结构化 host_result 载荷的运行时调用结果。
    let result = RuntimeInvocationResult::from_content_parts(
        "content".to_string(),
        None,
        None,
        Some(RuntimeHostResult {
            kind: "change_set".to_string(),
            payload: serde_json::json!({
                "mode": "preview",
                "files": []
            }),
        }),
    );

    // FFI invocation result allocated from the runtime result.
    // 从运行时结果分配得到的 FFI 调用结果。
    let ffi_result =
        alloc_invocation_result(&result).expect("host_result allocation should succeed");

    assert!(!ffi_result.host_result.is_null());
    // FFI host_result pointer exposed inside the invocation result.
    // 调用结果中暴露的 FFI host_result 指针。
    let host_result = unsafe { &*ffi_result.host_result };
    assert_eq!(read_owned_buffer_text(&host_result.kind), "change_set");

    // JSON payload copied into the C ABI host_result buffer.
    // 复制到 C ABI host_result 缓冲中的 JSON 载荷。
    let payload_json = read_owned_buffer_text(&host_result.payload_json);
    // Parsed JSON payload used to avoid depending on object key order.
    // 用于避免依赖对象键顺序的已解析 JSON 载荷。
    let payload: serde_json::Value =
        serde_json::from_str(&payload_json).expect("host_result payload should be json");

    assert_eq!(payload["mode"], "preview");
    assert_eq!(payload["files"], serde_json::json!([]));
    assert_eq!(host_result.payload_bytes, payload_json.len());

    // Heap-allocated invocation result used only to exercise the public free helper.
    // 仅用于触发公开释放辅助函数的堆分配调用结果。
    let result_ptr = Box::into_raw(Box::new(ffi_result));
    unsafe { luaskills_ffi_invocation_result_free(result_ptr) };
}

/// Build one CString for a test path using the shared host-visible path renderer.
/// 使用共享的宿主可见路径渲染器为测试路径构造 CString。
///
/// The path parameter is the filesystem path passed into the FFI boundary.
/// path 参数是传入 FFI 边界的文件系统路径。
///
/// The label parameter names the field in panic messages.
/// label 参数用于在 panic 消息中标识字段名称。
///
/// Return a CString whose bytes remain owned by the caller.
/// 返回字节所有权由调用方持有的 CString。
fn ffi_test_path_cstring(path: &Path, label: &str) -> CString {
    CString::new(render_host_visible_path(path))
        .unwrap_or_else(|_| panic!("{} path should not contain nul bytes", label))
}

/// Verify the string clone helper copies valid UTF-8 text into LuaSkills-owned memory.
/// 验证字符串克隆辅助函数会把有效 UTF-8 文本复制到 LuaSkills 拥有的内存中。
#[test]
fn ffi_string_clone_copies_valid_utf8_text() {
    // Host-owned C string passed into the clone helper.
    // 传入克隆辅助函数的宿主拥有 C 字符串。
    let input = CString::new("hello").expect("valid clone input cstring");
    // LuaSkills-owned clone returned by the FFI helper.
    // FFI 辅助函数返回的 LuaSkills 拥有克隆字符串。
    let cloned = unsafe { luaskills_ffi_string_clone(input.as_ptr()) };

    assert!(!cloned.is_null());
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(cloned) }
            .to_str()
            .expect("cloned string should be utf-8"),
        "hello"
    );
    unsafe { luaskills_ffi_string_free(cloned) };
}

/// Verify the string clone helper treats null input as an owned empty C string.
/// 验证字符串克隆辅助函数会把 null 输入处理为拥有所有权的空 C 字符串。
#[test]
fn ffi_string_clone_null_input_returns_owned_empty_string() {
    // LuaSkills-owned clone returned for null host input.
    // null 宿主输入对应的 LuaSkills 拥有克隆字符串。
    let cloned = unsafe { luaskills_ffi_string_clone(ptr::null()) };

    assert!(!cloned.is_null());
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(cloned) }
            .to_str()
            .expect("empty cloned string should be utf-8"),
        ""
    );
    unsafe { luaskills_ffi_string_free(cloned) };
}

/// Verify the string clone helper rejects invalid UTF-8 input instead of lossy replacement.
/// 验证字符串克隆辅助函数会拒绝非法 UTF-8 输入，而不是执行有损替换。
#[test]
fn ffi_string_clone_rejects_invalid_utf8_text() {
    // NUL-terminated byte sequence that is not valid UTF-8.
    // NUL 结尾但不是有效 UTF-8 的字节序列。
    let invalid_input = [0xff, 0x00];
    // Clone result returned for invalid UTF-8 input.
    // 非法 UTF-8 输入对应的克隆结果。
    let cloned =
        unsafe { luaskills_ffi_string_clone(invalid_input.as_ptr().cast::<std::ffi::c_char>()) };

    assert!(cloned.is_null());
}

/// Build one empty standard FFI host-options value with null pointers and disabled optional features.
/// 构建一个指针为空且可选功能关闭的标准 FFI host-options 空值。
///
/// Return a host-options baseline that tests may customize field by field.
/// 返回测试可逐字段定制的 host-options 基准值。
fn empty_ffi_runtime_host_options() -> FfiLuaRuntimeHostOptions {
    FfiLuaRuntimeHostOptions {
        temp_dir: ptr::null(),
        resources_dir: ptr::null(),
        lua_packages_dir: ptr::null(),
        host_provided_tool_root: ptr::null(),
        host_provided_lua_root: ptr::null(),
        host_provided_ffi_root: ptr::null(),
        system_lua_lib_dir: ptr::null(),
        download_cache_root: ptr::null(),
        dependency_dir_name: ptr::null(),
        state_dir_name: ptr::null(),
        database_dir_name: ptr::null(),
        skill_config_file_path: ptr::null(),
        allow_network_download: 0,
        github_base_url: ptr::null(),
        github_api_base_url: ptr::null(),
        official_skill_hub_base_url: ptr::null(),
        enable_private_url_skill_install: 0,
        private_skill_source_allowlist: ptr::null(),
        private_skill_source_allowlist_len: 0,
        sqlite_library_path: ptr::null(),
        sqlite_provider_mode: FFI_PROVIDER_MODE_DYNAMIC_LIBRARY,
        sqlite_callback_mode: FFI_CALLBACK_MODE_STANDARD,
        lancedb_library_path: ptr::null(),
        lancedb_provider_mode: FFI_PROVIDER_MODE_DYNAMIC_LIBRARY,
        lancedb_callback_mode: FFI_CALLBACK_MODE_STANDARD,
        space_controller_endpoint: ptr::null(),
        space_controller_auto_spawn: 0,
        space_controller_executable_path: ptr::null(),
        space_controller_process_mode: FFI_SPACE_CONTROLLER_PROCESS_MODE_SERVICE,
        cache_config: ptr::null(),
        runlua_pool_config: ptr::null(),
        reserved_entry_names: ptr::null(),
        reserved_entry_names_len: 0,
        ignored_skill_ids: ptr::null(),
        ignored_skill_ids_len: 0,
        enable_skill_management_bridge: 0,
        default_text_encoding: ptr::null(),
        disable_managed_io_compat: 0,
    }
}

/// Owned CString fixture backing one FfiLuaRuntimeHostOptions test value.
/// 为单个 FfiLuaRuntimeHostOptions 测试值提供 CString 所有权的夹具。
struct FfiStandardHostOptionsFixture {
    /// CString backing the temp_dir pointer.
    /// 支撑 temp_dir 指针的 CString。
    temp_dir_text: CString,
    /// CString backing the resources_dir pointer.
    /// 支撑 resources_dir 指针的 CString。
    resources_dir_text: CString,
    /// CString backing the lua_packages_dir and host_provided_lua_root pointers.
    /// 支撑 lua_packages_dir 与 host_provided_lua_root 指针的 CString。
    lua_packages_dir_text: CString,
    /// CString backing the host_provided_tool_root pointer.
    /// 支撑 host_provided_tool_root 指针的 CString。
    tool_root_dir_text: CString,
    /// CString backing the host_provided_ffi_root pointer.
    /// 支撑 host_provided_ffi_root 指针的 CString。
    ffi_root_dir_text: CString,
    /// CString backing the dependency_dir_name pointer.
    /// 支撑 dependency_dir_name 指针的 CString。
    dependency_dir_name: CString,
    /// CString backing the state_dir_name pointer.
    /// 支撑 state_dir_name 指针的 CString。
    state_dir_name: CString,
    /// CString backing the database_dir_name pointer.
    /// 支撑 database_dir_name 指针的 CString。
    database_dir_name: CString,
    /// Optional CString backing the skill_config_file_path pointer.
    /// 支撑 skill_config_file_path 指针的可选 CString。
    skill_config_file_path: Option<CString>,
}

impl FfiStandardHostOptionsFixture {
    /// Create one fixture using the default standard FFI test directories under a temp root.
    /// 使用临时根目录下的默认标准 FFI 测试目录创建夹具。
    ///
    /// The temp_root parameter is the root directory prepared by the current test.
    /// temp_root 参数是当前测试准备的根目录。
    ///
    /// Return a fixture whose CString fields must outlive the generated options.
    /// 返回 CString 字段必须比生成的 options 存活更久的夹具。
    fn new(temp_root: &Path) -> Self {
        Self::with_skill_config_file_path(temp_root, None)
    }

    /// Create one fixture and optionally include a skill-config file path.
    /// 创建夹具，并可选包含 skill-config 文件路径。
    ///
    /// The temp_root parameter is the root directory prepared by the current test.
    /// temp_root 参数是当前测试准备的根目录。
    ///
    /// The skill_config_file_path parameter is the optional config file path exposed to the FFI host options.
    /// skill_config_file_path 参数是暴露给 FFI host options 的可选配置文件路径。
    ///
    /// Return a fixture that owns every CString referenced by host_options.
    /// 返回持有 host_options 所引用全部 CString 的夹具。
    fn with_skill_config_file_path(
        temp_root: &Path,
        skill_config_file_path: Option<&Path>,
    ) -> Self {
        Self {
            temp_dir_text: ffi_test_path_cstring(&temp_root.join("temp"), "temp_dir"),
            resources_dir_text: ffi_test_path_cstring(
                &temp_root.join("resources"),
                "resources_dir",
            ),
            lua_packages_dir_text: ffi_test_path_cstring(
                &temp_root.join("lua_packages"),
                "lua_packages_dir",
            ),
            tool_root_dir_text: ffi_test_path_cstring(
                &temp_root.join("bin").join("tools"),
                "tool_root_dir",
            ),
            ffi_root_dir_text: ffi_test_path_cstring(&temp_root.join("libs"), "ffi_root"),
            dependency_dir_name: CString::new("dependencies").expect("dependencies cstring"),
            state_dir_name: CString::new("state").expect("state cstring"),
            database_dir_name: CString::new("databases").expect("databases cstring"),
            skill_config_file_path: skill_config_file_path
                .map(|path| ffi_test_path_cstring(path, "skill_config_file_path")),
        }
    }

    /// Build one borrowed FFI host-options value from the owned CString fixture.
    /// 从持有 CString 的夹具构建单个借用型 FFI host-options 值。
    ///
    /// Return host options whose raw pointers remain valid while this fixture is alive.
    /// 返回在当前夹具存活期间裸指针保持有效的 host options。
    fn host_options(&self) -> FfiLuaRuntimeHostOptions {
        let mut host_options = empty_ffi_runtime_host_options();
        host_options.temp_dir = self.temp_dir_text.as_ptr();
        host_options.resources_dir = self.resources_dir_text.as_ptr();
        host_options.lua_packages_dir = self.lua_packages_dir_text.as_ptr();
        host_options.host_provided_tool_root = self.tool_root_dir_text.as_ptr();
        host_options.host_provided_lua_root = self.lua_packages_dir_text.as_ptr();
        host_options.host_provided_ffi_root = self.ffi_root_dir_text.as_ptr();
        host_options.dependency_dir_name = self.dependency_dir_name.as_ptr();
        host_options.state_dir_name = self.state_dir_name.as_ptr();
        host_options.database_dir_name = self.database_dir_name.as_ptr();
        host_options.skill_config_file_path = self
            .skill_config_file_path
            .as_ref()
            .map_or(ptr::null(), |path| path.as_ptr());
        host_options
    }
}

/// Verify buffer_clone copies one byte payload into luaskills-owned storage.
/// 验证 buffer_clone 会把单个字节载荷复制到 luaskills 自主管理存储中。
#[test]
fn buffer_clone_copies_payload_into_owned_storage() {
    let input = b"ffi-buffer-demo";
    let mut buffer_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        luaskills_ffi_buffer_clone(input.as_ptr(), input.len(), &mut buffer_out, &mut error_out)
    };
    assert_eq!(status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());
    assert_eq!(error_out.len, 0);
    let copied = unsafe { std::slice::from_raw_parts(buffer_out.ptr, buffer_out.len) };
    assert_eq!(copied, input);
    unsafe { luaskills_ffi_buffer_free(buffer_out) };
}

/// Verify JSON provider callback bridge accepts borrowed buffers and owned-buffer responses.
/// 验证 JSON provider callback 桥接可接受借用缓冲输入并处理拥有型缓冲输出。
#[test]
fn json_provider_callback_bridge_round_trips_owned_buffers() {
    unsafe extern "C" fn callback(
        request_json: FfiBorrowedBuffer,
        _user_data: *mut c_void,
        response_out: *mut FfiOwnedBuffer,
        error_out: *mut FfiOwnedBuffer,
    ) -> i32 {
        let request_bytes =
            unsafe { std::slice::from_raw_parts(request_json.ptr, request_json.len) };
        let request_text = std::str::from_utf8(request_bytes).expect("request must be utf-8");
        let response_text = format!("{{\"echo\":{}}}", request_text);
        unsafe {
            *response_out = alloc_owned_buffer_from_bytes(response_text.as_bytes());
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
        FFI_STATUS_OK
    }

    let response = invoke_json_provider_callback(callback, 0, "{\"value\":1}")
        .expect("callback bridge should succeed");
    assert_eq!(response, "{\"echo\":{\"value\":1}}");
}

/// Verify model JSON FFI callback setters round-trip requests, responses, and provider error fields.
/// 验证模型 JSON FFI callback setter 会往返传递请求、响应和 provider 错误字段。
#[test]
fn model_json_callback_setters_round_trip_response_and_provider_error() {
    unsafe extern "C" fn embed_callback(
        request_json: FfiBorrowedBuffer,
        _user_data: *mut c_void,
        response_out: *mut FfiOwnedBuffer,
        error_out: *mut FfiOwnedBuffer,
    ) -> i32 {
        let request_bytes =
            unsafe { std::slice::from_raw_parts(request_json.ptr, request_json.len) };
        let request: Value = match serde_json::from_slice(request_bytes) {
            Ok(request) => request,
            Err(error) => {
                unsafe {
                    *error_out =
                        alloc_owned_buffer_from_string(format!("invalid request: {}", error));
                }
                return FFI_STATUS_ERROR;
            }
        };
        if request["text"] != "hello"
            || request["caller"]["skill_id"] != "ffi-skill"
            || request["caller"]["request_id"] != "req-ffi-1"
        {
            unsafe {
                *error_out =
                    alloc_owned_buffer_from_string(format!("unexpected request: {}", request));
            }
            return FFI_STATUS_ERROR;
        }
        unsafe {
            *response_out = alloc_owned_buffer_from_string(
                r#"{"ok":true,"vector":[0.1,0.2],"dimensions":2,"usage":{"input_tokens":3}}"#,
            );
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
        FFI_STATUS_OK
    }

    unsafe extern "C" fn llm_callback(
        request_json: FfiBorrowedBuffer,
        _user_data: *mut c_void,
        response_out: *mut FfiOwnedBuffer,
        error_out: *mut FfiOwnedBuffer,
    ) -> i32 {
        let request_bytes =
            unsafe { std::slice::from_raw_parts(request_json.ptr, request_json.len) };
        let request: Value = match serde_json::from_slice(request_bytes) {
            Ok(request) => request,
            Err(error) => {
                unsafe {
                    *error_out =
                        alloc_owned_buffer_from_string(format!("invalid request: {}", error));
                }
                return FFI_STATUS_ERROR;
            }
        };
        if request["system"] != "system" || request["user"] != "user" {
            unsafe {
                *error_out =
                    alloc_owned_buffer_from_string(format!("unexpected request: {}", request));
            }
            return FFI_STATUS_ERROR;
        }
        unsafe {
            *response_out = alloc_owned_buffer_from_string(
                r#"{"ok":false,"error":{"code":"provider_error","message":"provider failed","provider_message":"raw provider message","provider_code":"invalid_api_key","provider_status":401}}"#,
            );
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
        FFI_STATUS_OK
    }

    let _guard = runtime_model_callback_test_guard();
    let exported = crate::ffi::exported_ffi_function_names();
    assert!(
        exported
            .iter()
            .any(|name| name == "luaskills_ffi_set_model_embed_json_callback")
    );
    assert!(
        exported
            .iter()
            .any(|name| name == "luaskills_ffi_set_model_llm_json_callback")
    );

    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let embed_status = unsafe {
        luaskills_ffi_set_model_embed_json_callback(
            Some(embed_callback),
            ptr::null_mut(),
            &mut error_out,
        )
    };
    assert_eq!(embed_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());
    let llm_status = unsafe {
        luaskills_ffi_set_model_llm_json_callback(
            Some(llm_callback),
            ptr::null_mut(),
            &mut error_out,
        )
    };
    assert_eq!(llm_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());

    let caller = RuntimeModelCaller {
        skill_id: Some("ffi-skill".to_string()),
        entry_name: Some("entry".to_string()),
        canonical_tool_name: Some("ffi-skill-entry".to_string()),
        root_name: Some("ROOT".to_string()),
        skill_dir: Some("D:/skills/ffi-skill".to_string()),
        client_name: Some("sdk-test".to_string()),
        request_id: Some("req-ffi-1".to_string()),
    };
    let embed_response = dispatch_model_embed_request(&RuntimeModelEmbedRequest {
        text: "hello".to_string(),
        caller: caller.clone(),
    })
    .expect("embed JSON callback should return a response");
    assert_eq!(embed_response.vector, vec![0.1, 0.2]);
    assert_eq!(embed_response.dimensions, 2);
    assert_eq!(
        embed_response.usage.and_then(|usage| usage.input_tokens),
        Some(3)
    );

    let llm_error = dispatch_model_llm_request(&RuntimeModelLlmRequest {
        system: "system".to_string(),
        user: "user".to_string(),
        caller,
    })
    .expect_err("llm JSON callback should return a provider error");
    assert_eq!(llm_error.code, RuntimeModelErrorCode::ProviderError);
    assert_eq!(llm_error.message, "provider failed");
    assert_eq!(
        llm_error.provider_message.as_deref(),
        Some("raw provider message")
    );
    assert_eq!(llm_error.provider_code.as_deref(), Some("invalid_api_key"));
    assert_eq!(llm_error.provider_status, Some(401));
}

/// Verify one entry list allocates nested owned buffers for entry and parameter text fields.
/// 验证入口列表会为入口及参数文本字段分配嵌套拥有型缓冲。
#[test]
fn entry_list_free_handles_nested_owned_buffers() {
    let runtime_entry = RuntimeEntryDescriptorModel {
        canonical_name: "demo-entry".to_string(),
        skill_id: "demo-skill".to_string(),
        local_name: "entry".to_string(),
        root_name: "ROOT".to_string(),
        skill_dir: "/tmp/demo-skill".to_string(),
        description: "Demo entry description".to_string(),
        parameters: vec![RuntimeEntryParameterDescriptorModel {
            name: "note".to_string(),
            param_type: "string".to_string(),
            description: "Optional note".to_string(),
            required: false,
        }],
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "Optional note"
                }
            }
        }),
    };

    let mut items =
        vec![alloc_entry_descriptor(&runtime_entry).expect("entry descriptor should allocate")];
    let list = FfiRuntimeEntryDescriptorList {
        items: items.as_mut_ptr(),
        len: items.len(),
    };
    std::mem::forget(items);
    let list_ptr = Box::into_raw(Box::new(list));

    let list_ref = unsafe { &*list_ptr };
    assert_eq!(list_ref.len, 1);
    let first_entry = unsafe { &*list_ref.items };
    assert_eq!(
        read_owned_buffer_text(&first_entry.canonical_name),
        "demo-entry"
    );
    assert_eq!(read_owned_buffer_text(&first_entry.skill_id), "demo-skill");
    assert_eq!(
        read_owned_buffer_text(&first_entry.description),
        "Demo entry description"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read_owned_buffer_text(
            &first_entry.input_schema_json
        ))
        .expect("parse entry input schema json"),
        serde_json::json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "Optional note"
                }
            }
        })
    );
    assert_eq!(first_entry.parameters_len, 1);

    let first_parameter = unsafe { &*first_entry.parameters };
    assert_eq!(read_owned_buffer_text(&first_parameter.name), "note");
    assert_eq!(
        read_owned_buffer_text(&first_parameter.param_type),
        "string"
    );
    assert_eq!(
        read_owned_buffer_text(&first_parameter.description),
        "Optional note"
    );
    assert_eq!(first_parameter.required, 0);

    unsafe { luaskills_ffi_entry_list_free(list_ptr) };
}

/// Verify one help detail and one help list allocate nested owned buffers for text and related-entry arrays.
/// 验证帮助详情与帮助列表会为文本字段和关联入口数组分配嵌套拥有型缓冲。
#[test]
fn help_results_free_handle_nested_owned_buffers() {
    let help_detail = RuntimeHelpDetailModel {
        skill_id: "demo-skill".to_string(),
        skill_name: "Demo Skill".to_string(),
        skill_version: "0.1.0".to_string(),
        root_name: "ROOT".to_string(),
        skill_dir: "/tmp/demo-skill".to_string(),
        flow_name: "main".to_string(),
        description: "Demo help detail".to_string(),
        related_entries: vec!["demo-entry".to_string(), "demo-entry-2".to_string()],
        is_main: true,
        content_type: "markdown".to_string(),
        content: "# Demo".to_string(),
    };
    let detail_ptr = Box::into_raw(Box::new(alloc_help_detail(&help_detail)));

    let detail_ref = unsafe { &*detail_ptr };
    assert_eq!(read_owned_buffer_text(&detail_ref.skill_id), "demo-skill");
    assert_eq!(read_owned_buffer_text(&detail_ref.flow_name), "main");
    assert_eq!(detail_ref.related_entries_len, 2);
    let related_entries = unsafe {
        std::slice::from_raw_parts(detail_ref.related_entries, detail_ref.related_entries_len)
    };
    assert_eq!(read_owned_buffer_text(&related_entries[0]), "demo-entry");
    assert_eq!(read_owned_buffer_text(&related_entries[1]), "demo-entry-2");

    unsafe { luaskills_ffi_help_detail_free(detail_ptr) };

    let help_descriptor = RuntimeSkillHelpDescriptorModel {
        skill_id: "demo-skill".to_string(),
        skill_name: "Demo Skill".to_string(),
        skill_version: "0.1.0".to_string(),
        root_name: "ROOT".to_string(),
        skill_dir: "/tmp/demo-skill".to_string(),
        main: RuntimeHelpNodeDescriptorModel {
            flow_name: "main".to_string(),
            description: "Main help node".to_string(),
            related_entries: vec!["demo-entry".to_string()],
            is_main: true,
        },
        flows: vec![RuntimeHelpNodeDescriptorModel {
            flow_name: "secondary".to_string(),
            description: "Secondary node".to_string(),
            related_entries: vec!["demo-entry-2".to_string()],
            is_main: false,
        }],
    };

    let mut items = vec![alloc_help_descriptor(&help_descriptor)];
    let list = FfiRuntimeSkillHelpDescriptorList {
        items: items.as_mut_ptr(),
        len: items.len(),
    };
    std::mem::forget(items);
    let list_ptr = Box::into_raw(Box::new(list));

    let list_ref = unsafe { &*list_ptr };
    assert_eq!(list_ref.len, 1);
    let first_help = unsafe { &*list_ref.items };
    assert_eq!(read_owned_buffer_text(&first_help.skill_name), "Demo Skill");
    assert_eq!(read_owned_buffer_text(&first_help.main.flow_name), "main");
    assert_eq!(first_help.main.related_entries_len, 1);
    let main_related_entries = unsafe {
        std::slice::from_raw_parts(
            first_help.main.related_entries,
            first_help.main.related_entries_len,
        )
    };
    assert_eq!(
        read_owned_buffer_text(&main_related_entries[0]),
        "demo-entry"
    );
    assert_eq!(first_help.flows_len, 1);
    let first_flow = unsafe { &*first_help.flows };
    assert_eq!(read_owned_buffer_text(&first_flow.flow_name), "secondary");

    unsafe { luaskills_ffi_help_list_free(list_ptr) };
}

/// Verify the standard FFI load/list pipeline returns one entry for one minimal temporary skill root.
/// 验证标准 FFI 的加载与列举链路会为最小临时技能根返回一个入口。
#[test]
fn standard_ffi_load_and_list_entries_round_trip() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_entry_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    let root_skills_root = temp_root.join("root_skills");
    let skills_root = temp_root.join("skills");
    let skill_dir = skills_root.join("demo-skill");
    std::fs::create_dir_all(&root_skills_root).expect("create root skills root");
    std::fs::create_dir_all(skill_dir.join("runtime")).expect("create runtime directory");
    std::fs::create_dir_all(temp_root.join("temp")).expect("create temp directory");
    std::fs::create_dir_all(temp_root.join("resources")).expect("create resources directory");
    std::fs::create_dir_all(temp_root.join("lua_packages")).expect("create lua_packages directory");
    std::fs::create_dir_all(temp_root.join("bin").join("tools")).expect("create tools directory");
    std::fs::create_dir_all(temp_root.join("libs")).expect("create libs directory");
    std::fs::write(
            skill_dir.join("skill.yaml"),
            "name: demo-skill\nversion: 0.1.0\nenable: true\nentries:\n  - name: ping\n    description: Ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: demo_skill_ping\n    parameters:\n      - name: note\n        type: string\n        description: Optional note.\n        required: false\n",
        )
        .expect("write skill yaml");
    std::fs::write(
        skill_dir.join("runtime").join("ping.lua"),
        "return function(args)\n  return 'ok'\nend\n",
    )
    .expect("write runtime lua");

    let host_fixture = FfiStandardHostOptionsFixture::new(&temp_root);
    let root_name = CString::new(" ROOT ").expect("root name cstring");
    let skills_root_text = ffi_test_path_cstring(&skills_root, "skills_root");
    let tool_name = CString::new("demo-skill-ping").expect("tool name cstring");

    let host_options = host_fixture.host_options();
    let engine_options = FfiLuaEngineOptions {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };

    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let engine_status =
        unsafe { luaskills_ffi_engine_new(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(engine_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());

    let ffi_skill_roots = [FfiRuntimeSkillRoot {
        name: root_name.as_ptr(),
        skills_dir: skills_root_text.as_ptr(),
    }];
    let mut load_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let load_status = unsafe {
        luaskills_ffi_load_from_roots(
            engine_id,
            ffi_skill_roots.as_ptr(),
            ffi_skill_roots.len(),
            &mut load_error,
        )
    };
    assert_eq!(load_status, FFI_STATUS_OK);
    assert!(load_error.ptr.is_null());

    let mut entries_out: *mut FfiRuntimeEntryDescriptorList = ptr::null_mut();
    let mut list_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let list_status = unsafe {
        luaskills_ffi_list_entries(
            engine_id,
            FFI_SKILL_AUTHORITY_SYSTEM,
            &mut entries_out,
            &mut list_error,
        )
    };
    assert_eq!(list_status, FFI_STATUS_OK);
    assert!(list_error.ptr.is_null());
    assert!(!entries_out.is_null());

    let entries_ref = unsafe { &*entries_out };
    assert_eq!(entries_ref.len, 1);
    let entry_ref = unsafe { &*entries_ref.items };
    assert_eq!(
        read_owned_buffer_text(&entry_ref.canonical_name),
        "demo-skill-ping"
    );
    assert_eq!(read_owned_buffer_text(&entry_ref.skill_id), "demo-skill");
    assert_eq!(read_owned_buffer_text(&entry_ref.local_name), "ping");
    assert_eq!(read_owned_buffer_text(&entry_ref.root_name), " ROOT ");
    assert_eq!(
        read_owned_buffer_text(&entry_ref.description),
        "Ping entry."
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read_owned_buffer_text(
            &entry_ref.input_schema_json
        ))
        .expect("parse listed entry input schema json"),
        serde_json::json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "Optional note."
                }
            }
        })
    );
    assert_eq!(entry_ref.parameters_len, 1);
    let parameter_ref = unsafe { &*entry_ref.parameters };
    assert_eq!(read_owned_buffer_text(&parameter_ref.name), "note");
    assert_eq!(read_owned_buffer_text(&parameter_ref.param_type), "string");
    assert_eq!(
        read_owned_buffer_text(&parameter_ref.description),
        "Optional note."
    );
    assert_eq!(parameter_ref.required, 0);

    unsafe { luaskills_ffi_entry_list_free(entries_out) };

    entries_out = ptr::null_mut();
    list_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let delegated_list_status = unsafe {
        luaskills_ffi_list_entries(
            engine_id,
            FFI_SKILL_AUTHORITY_DELEGATED_TOOL,
            &mut entries_out,
            &mut list_error,
        )
    };
    assert_eq!(delegated_list_status, FFI_STATUS_OK);
    assert!(list_error.ptr.is_null());
    assert!(!entries_out.is_null());
    let delegated_entries_ref = unsafe { &*entries_out };
    assert_eq!(delegated_entries_ref.len, 0);
    unsafe { luaskills_ffi_entry_list_free(entries_out) };

    let mut is_skill_out = 0_u8;
    let mut is_skill_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let delegated_is_skill_status = unsafe {
        luaskills_ffi_is_skill(
            engine_id,
            FFI_SKILL_AUTHORITY_DELEGATED_TOOL,
            tool_name.as_ptr(),
            &mut is_skill_out,
            &mut is_skill_error,
        )
    };
    assert_eq!(delegated_is_skill_status, FFI_STATUS_OK);
    assert!(is_skill_error.ptr.is_null());
    assert_eq!(is_skill_out, 0);

    let mut skill_id_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut skill_name_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let delegated_skill_name_status = unsafe {
        luaskills_ffi_skill_name_for_tool(
            engine_id,
            FFI_SKILL_AUTHORITY_DELEGATED_TOOL,
            tool_name.as_ptr(),
            &mut skill_id_out,
            &mut skill_name_error,
        )
    };
    assert_eq!(delegated_skill_name_status, FFI_STATUS_OK);
    assert!(skill_name_error.ptr.is_null());
    assert_eq!(read_owned_buffer_text(&skill_id_out), "");
    unsafe { luaskills_ffi_buffer_free(skill_id_out) };

    let (call_args_storage, call_args_buffer) = make_borrowed_buffer("{}");
    let (run_args_storage, run_args_buffer) = make_borrowed_buffer("{}");
    let mut call_result_out: *mut FfiRuntimeInvocationResult = ptr::null_mut();
    let mut call_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let call_status = unsafe {
        luaskills_ffi_call_skill(
            engine_id,
            tool_name.as_ptr(),
            call_args_buffer,
            ptr::null(),
            &mut call_result_out,
            &mut call_error,
        )
    };
    assert_eq!(call_status, FFI_STATUS_OK);
    assert!(call_error.ptr.is_null());
    assert!(!call_result_out.is_null());
    let call_result_ref = unsafe { &*call_result_out };
    assert_eq!(read_owned_buffer_text(&call_result_ref.content), "ok");
    unsafe { luaskills_ffi_invocation_result_free(call_result_out) };

    let run_code =
        CString::new("return vulcan.call('demo-skill-ping', {})").expect("run code cstring");
    let mut run_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut run_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let run_status = unsafe {
        luaskills_ffi_run_lua(
            engine_id,
            run_code.as_ptr(),
            run_args_buffer,
            ptr::null(),
            &mut run_out,
            &mut run_error,
        )
    };
    assert_eq!(run_status, FFI_STATUS_OK);
    assert!(run_error.ptr.is_null());
    assert_eq!(read_owned_buffer_text(&run_out), "\"ok\"");
    unsafe { luaskills_ffi_buffer_free(run_out) };
    let _ = (call_args_storage, run_args_storage);

    let prompt_name = CString::new("demo").expect("prompt name cstring");
    let argument_name = CString::new("target").expect("argument name cstring");
    let mut prompt_values_out: *mut FfiStringArray = ptr::null_mut();
    let mut prompt_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let prompt_status = unsafe {
        luaskills_ffi_prompt_argument_completions(
            engine_id,
            FFI_SKILL_AUTHORITY_DELEGATED_TOOL,
            prompt_name.as_ptr(),
            argument_name.as_ptr(),
            &mut prompt_values_out,
            &mut prompt_error,
        )
    };
    assert_eq!(prompt_status, FFI_STATUS_OK);
    assert!(prompt_error.ptr.is_null());
    assert!(!prompt_values_out.is_null());
    let prompt_values_ref = unsafe { &*prompt_values_out };
    assert_eq!(prompt_values_ref.len, 0);
    unsafe { luaskills_ffi_string_array_free(prompt_values_out) };

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify the standard C ABI can create one engine from only the canonical runtime root.
/// 验证标准 C ABI 可以只通过规范 runtime_root 创建一个引擎。
#[test]
fn standard_ffi_runtime_root_only_host_options_round_trip() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_runtime_root_only_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    std::fs::create_dir_all(&temp_root).expect("create runtime root");
    let runtime_root_text = ffi_test_path_cstring(&temp_root, "runtime_root");

    let host_options = FfiLuaRuntimeHostOptionsV2 {
        base: empty_ffi_runtime_host_options(),
        runtime_root: runtime_root_text.as_ptr(),
    };
    let engine_options = FfiLuaEngineOptionsV2 {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };

    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let engine_status =
        unsafe { luaskills_ffi_engine_new_v2(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(
        engine_status,
        FFI_STATUS_OK,
        "engine_new failed: {}",
        read_owned_buffer_text(&error_out)
    );
    assert!(error_out.ptr.is_null());

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify standard C ABI v3 accepts independent managed roots and one resource policy.
/// 验证标准 C ABI v3 会接受独立受管根与一份资源策略。
#[test]
fn standard_ffi_v3_managed_runtime_roots_and_config_round_trip() {
    // TempRoot owns three distinct host boundaries used by the v3 engine constructor.
    // TempRoot 拥有 v3 引擎构造器使用的三个独立宿主边界。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_v3_managed_roots_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let runtime_root = temp_root.join("runtime data");
    let distribution_root = temp_root.join("application assets").join("runtimes");
    let environment_root = temp_root.join("user data").join("managed envs");
    std::fs::create_dir_all(&runtime_root).expect("create v3 runtime root");
    std::fs::create_dir_all(&distribution_root).expect("create v3 distribution root");
    let runtime_root_text = ffi_test_path_cstring(&runtime_root, "runtime_root");
    let distribution_root_text =
        ffi_test_path_cstring(&distribution_root, "managed_runtime_distribution_root");
    let environment_root_text =
        ffi_test_path_cstring(&environment_root, "managed_runtime_environment_root");
    // ManagedRuntimeConfig carries nondefault B3-B7 values through the fixed C layout.
    // ManagedRuntimeConfig 通过固定 C 布局携带非默认 B3-B7 值。
    let managed_runtime_config = FfiLuaRuntimeManagedRuntimeConfig {
        worker_pool_max_size_per_environment: 5,
        worker_idle_ttl_secs: 45,
        persistent_session_limit_per_engine: 32,
        persistent_session_default_buffer_limit_bytes_per_stream: 524_288,
        has_invoke_default_timeout_ms: 1,
        invoke_default_timeout_ms: 15_000,
    };
    let host_options = FfiLuaRuntimeHostOptionsV3 {
        base: FfiLuaRuntimeHostOptionsV2 {
            base: empty_ffi_runtime_host_options(),
            runtime_root: runtime_root_text.as_ptr(),
        },
        managed_runtime_distribution_root: distribution_root_text.as_ptr(),
        managed_runtime_environment_root: environment_root_text.as_ptr(),
        managed_runtime_config: &managed_runtime_config,
    };
    let engine_options = FfiLuaEngineOptionsV3 {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };
    // ParsedOptions proves the v3 pointer is copied into the exact Rust host policy before creation.
    // ParsedOptions 证明 v3 指针会在创建前复制到精确 Rust 宿主策略中。
    let parsed_options = parse_engine_options_v3(&engine_options)
        .expect("parse v3 managed runtime roots and config");
    assert_eq!(
        parsed_options.host_options.managed_runtime_config,
        LuaRuntimeManagedRuntimeConfig {
            worker_pool_max_size_per_environment: 5,
            worker_idle_ttl_secs: 45,
            persistent_session_limit_per_engine: 32,
            persistent_session_default_buffer_limit_bytes_per_stream: 524_288,
            invoke_default_timeout_ms: Some(15_000),
        }
    );
    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };

    let engine_status =
        unsafe { luaskills_ffi_engine_new_v3(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(
        engine_status,
        FFI_STATUS_OK,
        "engine_new_v3 failed: {}",
        read_owned_buffer_text(&error_out)
    );
    assert!(error_out.ptr.is_null());
    assert!(engine_id > 0);
    assert!(environment_root.is_dir());

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify the v3 timeout presence byte is strict instead of accepting ambiguous nonzero values.
/// 验证 v3 超时存在字节采用严格语义，不接受含糊的非零值。
#[test]
fn standard_ffi_v3_managed_runtime_config_rejects_invalid_presence_flag() {
    // Config is otherwise valid so the presence byte is the unique rejection cause.
    // Config 其余字段均合法，使存在字节成为唯一拒绝原因。
    let config = FfiLuaRuntimeManagedRuntimeConfig {
        worker_pool_max_size_per_environment: 4,
        worker_idle_ttl_secs: 60,
        persistent_session_limit_per_engine: 256,
        persistent_session_default_buffer_limit_bytes_per_stream: 1_048_576,
        has_invoke_default_timeout_ms: 2,
        invoke_default_timeout_ms: 10_000,
    };
    // Error is produced before any engine or filesystem resource exists.
    // Error 会在任何引擎或文件系统资源存在前生成。
    let error =
        parse_managed_runtime_config(&config).expect_err("invalid timeout presence flag must fail");

    assert!(error.contains("has_invoke_default_timeout_ms"));
}

/// Verify standard call_skill accepts borrowed JSON buffers for args and invocation context.
/// 验证标准 call_skill 会接受作为 args 与调用上下文输入的借用 JSON 缓冲。
#[test]
fn standard_ffi_call_skill_accepts_borrowed_json_buffers() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_callskill_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    let root_skills_root = temp_root.join("root_skills");
    let skills_root = temp_root.join("skills");
    let skill_dir = skills_root.join("demo-skill");
    std::fs::create_dir_all(&root_skills_root).expect("create root skills root");
    std::fs::create_dir_all(skill_dir.join("runtime")).expect("create runtime directory");
    std::fs::create_dir_all(temp_root.join("temp")).expect("create temp directory");
    std::fs::create_dir_all(temp_root.join("resources")).expect("create resources directory");
    std::fs::create_dir_all(temp_root.join("lua_packages")).expect("create lua_packages directory");
    std::fs::create_dir_all(temp_root.join("bin").join("tools")).expect("create tools directory");
    std::fs::create_dir_all(temp_root.join("libs")).expect("create libs directory");
    std::fs::write(
            skill_dir.join("skill.yaml"),
            "name: demo-skill\nversion: 0.1.0\nenable: true\nentries:\n  - name: ping\n    description: Ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: demo_skill_ping\n    parameters:\n      - name: note\n        type: string\n        description: Optional note.\n        required: false\n",
        )
        .expect("write skill yaml");
    std::fs::write(
            skill_dir.join("runtime").join("ping.lua"),
            "return function(args)\n  local note = ''\n  if type(args) == 'table' and type(args.note) == 'string' then\n    note = args.note\n  end\n  if note ~= '' then\n    return 'standard-ffi-test:' .. note\n  end\n  return 'standard-ffi-test:ok'\nend\n",
        )
        .expect("write runtime lua");

    let host_fixture = FfiStandardHostOptionsFixture::new(&temp_root);
    let root_name = CString::new("ROOT").expect("root name cstring");
    let skills_root_text = ffi_test_path_cstring(&skills_root, "skills_root");
    let tool_name = CString::new("demo-skill-ping").expect("tool name cstring");

    let host_options = host_fixture.host_options();
    let engine_options = FfiLuaEngineOptions {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };

    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let engine_status =
        unsafe { luaskills_ffi_engine_new(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(engine_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());

    let ffi_skill_roots = [FfiRuntimeSkillRoot {
        name: root_name.as_ptr(),
        skills_dir: skills_root_text.as_ptr(),
    }];
    let mut load_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let load_status = unsafe {
        luaskills_ffi_load_from_roots(
            engine_id,
            ffi_skill_roots.as_ptr(),
            ffi_skill_roots.len(),
            &mut load_error,
        )
    };
    assert_eq!(load_status, FFI_STATUS_OK);
    assert!(load_error.ptr.is_null());

    let (_args_storage, args_buffer) = make_borrowed_buffer(r#"{"note":"ffi"}"#);
    let (_request_storage, request_buffer) =
        make_borrowed_buffer(r#"{"transport_name":"ffi-test"}"#);
    let (_budget_storage, budget_buffer) = make_borrowed_buffer(r#"{"budget":7}"#);
    let (_tool_storage, tool_buffer) = make_borrowed_buffer(r#"{"mode":"demo-mode"}"#);
    let invocation_context = FfiLuaInvocationContext {
        request_context_json: request_buffer,
        client_budget_json: budget_buffer,
        tool_config_json: tool_buffer,
    };

    let mut result_out: *mut FfiRuntimeInvocationResult = ptr::null_mut();
    let mut call_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let call_status = unsafe {
        luaskills_ffi_call_skill(
            engine_id,
            tool_name.as_ptr(),
            args_buffer,
            &invocation_context,
            &mut result_out,
            &mut call_error,
        )
    };
    assert_eq!(call_status, FFI_STATUS_OK);
    assert!(call_error.ptr.is_null());
    assert!(!result_out.is_null());

    let result_ref = unsafe { &*result_out };
    assert_eq!(
        read_owned_buffer_text(&result_ref.content),
        "standard-ffi-test:ffi"
    );
    assert_eq!(result_ref.content_lines, 1);
    unsafe { luaskills_ffi_invocation_result_free(result_out) };

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify standard run_lua accepts borrowed JSON buffers for args and invocation context.
/// 验证标准 run_lua 会接受作为 args 与调用上下文输入的借用 JSON 缓冲。
#[test]
fn standard_ffi_run_lua_accepts_borrowed_json_buffers() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_runlua_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    std::fs::create_dir_all(temp_root.join("temp")).expect("create temp directory");
    std::fs::create_dir_all(temp_root.join("resources")).expect("create resources directory");
    std::fs::create_dir_all(temp_root.join("lua_packages")).expect("create lua_packages directory");
    std::fs::create_dir_all(temp_root.join("bin").join("tools")).expect("create tools directory");
    std::fs::create_dir_all(temp_root.join("libs")).expect("create libs directory");

    let host_fixture = FfiStandardHostOptionsFixture::new(&temp_root);
    let host_options = host_fixture.host_options();
    let engine_options = FfiLuaEngineOptions {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };

    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let engine_status =
        unsafe { luaskills_ffi_engine_new(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(engine_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());

    let code =
            CString::new("return { note = args.note, transport = vulcan.context.request.transport_name, budget = vulcan.context.client_budget.budget, mode = vulcan.context.tool_config.mode }")
                .expect("code cstring");
    let (_args_storage, args_buffer) = make_borrowed_buffer(r#"{"note":"demo"}"#);
    let (_request_storage, request_buffer) =
        make_borrowed_buffer(r#"{"transport_name":"ffi-test"}"#);
    let (_budget_storage, budget_buffer) = make_borrowed_buffer(r#"{"budget":7}"#);
    let (_tool_storage, tool_buffer) = make_borrowed_buffer(r#"{"mode":"demo-mode"}"#);
    let invocation_context = FfiLuaInvocationContext {
        request_context_json: request_buffer,
        client_budget_json: budget_buffer,
        tool_config_json: tool_buffer,
    };

    let mut result_json_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut run_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let run_status = unsafe {
        luaskills_ffi_run_lua(
            engine_id,
            code.as_ptr(),
            args_buffer,
            &invocation_context,
            &mut result_json_out,
            &mut run_error,
        )
    };
    assert_eq!(run_status, FFI_STATUS_OK);
    assert!(run_error.ptr.is_null());

    let result_json_text = read_owned_buffer_text(&result_json_out);
    let result_json: Value =
        serde_json::from_str(&result_json_text).expect("run_lua result must be valid json");
    assert_eq!(result_json["note"], "demo");
    assert_eq!(result_json["transport"], "ffi-test");
    assert_eq!(result_json["budget"], 7);
    assert_eq!(result_json["mode"], "demo-mode");
    unsafe { luaskills_ffi_buffer_free(result_json_out) };

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify the standard C ABI skill-config helpers support one full set/get/list/delete roundtrip.
/// 验证标准 C ABI 的技能配置辅助接口支持完整的 set/get/list/delete 往返流程。
#[test]
fn standard_ffi_skill_config_round_trip() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_skill_config_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    std::fs::create_dir_all(temp_root.join("temp")).expect("create temp directory");
    std::fs::create_dir_all(temp_root.join("resources")).expect("create resources directory");
    std::fs::create_dir_all(temp_root.join("lua_packages")).expect("create lua_packages directory");
    std::fs::create_dir_all(temp_root.join("bin").join("tools")).expect("create tools directory");
    std::fs::create_dir_all(temp_root.join("libs")).expect("create libs directory");

    let skill_config_file_path = temp_root.join("config").join("skill_config.json");
    let host_fixture = FfiStandardHostOptionsFixture::with_skill_config_file_path(
        &temp_root,
        Some(&skill_config_file_path),
    );
    let skill_id = CString::new("demo-skill").expect("skill_id cstring");
    let key = CString::new("api_token").expect("key cstring");
    let value = CString::new("sk-standard-ffi").expect("value cstring");

    let host_options = host_fixture.host_options();
    let engine_options = FfiLuaEngineOptions {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };

    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let engine_status =
        unsafe { luaskills_ffi_engine_new(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(engine_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());

    let mut set_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let set_status = unsafe {
        luaskills_ffi_skill_config_set(
            engine_id,
            skill_id.as_ptr(),
            key.as_ptr(),
            value.as_ptr(),
            &mut set_error,
        )
    };
    assert_eq!(set_status, FFI_STATUS_OK);
    assert!(set_error.ptr.is_null());

    let mut value_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut found_out = 0_u8;
    let mut get_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let get_status = unsafe {
        luaskills_ffi_skill_config_get(
            engine_id,
            skill_id.as_ptr(),
            key.as_ptr(),
            &mut value_out,
            &mut found_out,
            &mut get_error,
        )
    };
    assert_eq!(get_status, FFI_STATUS_OK);
    assert!(get_error.ptr.is_null());
    assert_eq!(found_out, 1);
    assert_eq!(read_owned_buffer_text(&value_out), "sk-standard-ffi");
    unsafe { luaskills_ffi_buffer_free(value_out) };

    let empty_value = CString::new("").expect("empty value cstring");
    let mut empty_set_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let empty_set_status = unsafe {
        luaskills_ffi_skill_config_set(
            engine_id,
            skill_id.as_ptr(),
            key.as_ptr(),
            empty_value.as_ptr(),
            &mut empty_set_error,
        )
    };
    assert_eq!(empty_set_status, FFI_STATUS_OK);
    assert!(empty_set_error.ptr.is_null());

    let mut empty_value_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut empty_found_out = 0_u8;
    let mut empty_get_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let empty_get_status = unsafe {
        luaskills_ffi_skill_config_get(
            engine_id,
            skill_id.as_ptr(),
            key.as_ptr(),
            &mut empty_value_out,
            &mut empty_found_out,
            &mut empty_get_error,
        )
    };
    assert_eq!(empty_get_status, FFI_STATUS_OK);
    assert!(empty_get_error.ptr.is_null());
    assert_eq!(empty_found_out, 1);
    assert_eq!(read_owned_buffer_text(&empty_value_out), "");
    unsafe { luaskills_ffi_buffer_free(empty_value_out) };

    let mut list_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut list_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let list_status = unsafe {
        luaskills_ffi_skill_config_list(
            engine_id,
            skill_id.as_ptr(),
            &mut list_out,
            &mut list_error,
        )
    };
    assert_eq!(list_status, FFI_STATUS_OK);
    assert!(list_error.ptr.is_null());
    let list_json: serde_json::Value = serde_json::from_str(&read_owned_buffer_text(&list_out))
        .expect("skill config list json should parse");
    assert_eq!(list_json.as_array().map(Vec::len), Some(1));
    assert_eq!(list_json[0]["skill_id"], "demo-skill");
    assert_eq!(list_json[0]["key"], "api_token");
    assert_eq!(list_json[0]["value"], "");
    unsafe { luaskills_ffi_buffer_free(list_out) };

    let mut deleted_out = 0_u8;
    let mut delete_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let delete_status = unsafe {
        luaskills_ffi_skill_config_delete(
            engine_id,
            skill_id.as_ptr(),
            key.as_ptr(),
            &mut deleted_out,
            &mut delete_error,
        )
    };
    assert_eq!(delete_status, FFI_STATUS_OK);
    assert!(delete_error.ptr.is_null());
    assert_eq!(deleted_out, 1);

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify standard disable/enable lifecycle calls update the runtime view in place.
/// 验证标准 disable/enable 生命周期调用会原地更新运行时视图。
#[test]
fn standard_ffi_disable_and_enable_skill_round_trip() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_standard_ffi_lifecycle_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    let root_skills_root = temp_root.join("root_skills");
    let skills_root = temp_root.join("skills");
    let skill_dir = skills_root.join("demo-skill");
    std::fs::create_dir_all(&root_skills_root).expect("create root skills root");
    std::fs::create_dir_all(skill_dir.join("runtime")).expect("create runtime directory");
    std::fs::create_dir_all(temp_root.join("temp")).expect("create temp directory");
    std::fs::create_dir_all(temp_root.join("resources")).expect("create resources directory");
    std::fs::create_dir_all(temp_root.join("lua_packages")).expect("create lua_packages directory");
    std::fs::create_dir_all(temp_root.join("bin").join("tools")).expect("create tools directory");
    std::fs::create_dir_all(temp_root.join("libs")).expect("create libs directory");
    std::fs::write(
            skill_dir.join("skill.yaml"),
            "name: demo-skill\nversion: 0.1.0\nenable: true\nentries:\n  - name: ping\n    description: Ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: demo_skill_ping\n    parameters:\n      - name: note\n        type: string\n        description: Optional note.\n        required: false\n",
        )
        .expect("write skill yaml");
    std::fs::write(
            skill_dir.join("runtime").join("ping.lua"),
            "return function(args)\n  local note = ''\n  if type(args) == 'table' and type(args.note) == 'string' then\n    note = args.note\n  end\n  if note ~= '' then\n    return 'lifecycle:' .. note\n  end\n  return 'lifecycle:ok'\nend\n",
        )
        .expect("write runtime lua");

    let host_fixture = FfiStandardHostOptionsFixture::new(&temp_root);
    let root_name = CString::new("ROOT").expect("root name cstring");
    let user_name = CString::new("USER").expect("user name cstring");
    let root_skills_root_text = ffi_test_path_cstring(&root_skills_root, "root_skills_root");
    let skills_root_text = ffi_test_path_cstring(&skills_root, "skills_root");
    let skill_id = CString::new("demo-skill").expect("skill_id cstring");
    let tool_name = CString::new("demo-skill-ping").expect("tool_name cstring");
    let disable_reason = CString::new("maintenance").expect("disable reason cstring");

    let host_options = host_fixture.host_options();
    let engine_options = FfiLuaEngineOptions {
        pool: FfiLuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        host: host_options,
    };

    let mut engine_id = 0_u64;
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let engine_status =
        unsafe { luaskills_ffi_engine_new(&engine_options, &mut engine_id, &mut error_out) };
    assert_eq!(engine_status, FFI_STATUS_OK);
    assert!(error_out.ptr.is_null());

    let ffi_skill_roots = [
        FfiRuntimeSkillRoot {
            name: root_name.as_ptr(),
            skills_dir: root_skills_root_text.as_ptr(),
        },
        FfiRuntimeSkillRoot {
            name: user_name.as_ptr(),
            skills_dir: skills_root_text.as_ptr(),
        },
    ];

    let mut load_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let load_status = unsafe {
        luaskills_ffi_load_from_roots(
            engine_id,
            ffi_skill_roots.as_ptr(),
            ffi_skill_roots.len(),
            &mut load_error,
        )
    };
    assert_eq!(load_status, FFI_STATUS_OK);
    assert!(load_error.ptr.is_null());

    let mut entries_out: *mut FfiRuntimeEntryDescriptorList = ptr::null_mut();
    let mut list_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let list_status = unsafe {
        luaskills_ffi_list_entries(
            engine_id,
            FFI_SKILL_AUTHORITY_SYSTEM,
            &mut entries_out,
            &mut list_error,
        )
    };
    assert_eq!(list_status, FFI_STATUS_OK);
    assert!(list_error.ptr.is_null());
    assert!(!entries_out.is_null());
    let entries_ref = unsafe { &*entries_out };
    assert_eq!(entries_ref.len, 1);
    unsafe { luaskills_ffi_entry_list_free(entries_out) };

    let (_before_disable_args_storage, before_disable_args_buffer) =
        make_borrowed_buffer(r#"{"note":"before-disable"}"#);
    let mut result_out: *mut FfiRuntimeInvocationResult = ptr::null_mut();
    let mut call_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let call_status = unsafe {
        luaskills_ffi_call_skill(
            engine_id,
            tool_name.as_ptr(),
            before_disable_args_buffer,
            ptr::null(),
            &mut result_out,
            &mut call_error,
        )
    };
    assert_eq!(call_status, FFI_STATUS_OK);
    assert!(call_error.ptr.is_null());
    let result_ref = unsafe { &*result_out };
    assert_eq!(
        read_owned_buffer_text(&result_ref.content),
        "lifecycle:before-disable"
    );
    unsafe { luaskills_ffi_invocation_result_free(result_out) };

    let mut disable_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let disable_status = unsafe {
        luaskills_ffi_disable_skill(
            engine_id,
            ffi_skill_roots.as_ptr(),
            ffi_skill_roots.len(),
            skill_id.as_ptr(),
            disable_reason.as_ptr(),
            &mut disable_error,
        )
    };
    assert_eq!(disable_status, FFI_STATUS_OK);
    assert!(disable_error.ptr.is_null());

    entries_out = ptr::null_mut();
    list_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let disabled_list_status = unsafe {
        luaskills_ffi_list_entries(
            engine_id,
            FFI_SKILL_AUTHORITY_SYSTEM,
            &mut entries_out,
            &mut list_error,
        )
    };
    assert_eq!(disabled_list_status, FFI_STATUS_OK);
    assert!(list_error.ptr.is_null());
    assert!(!entries_out.is_null());
    let disabled_entries_ref = unsafe { &*entries_out };
    assert_eq!(disabled_entries_ref.len, 0);
    unsafe { luaskills_ffi_entry_list_free(entries_out) };

    result_out = ptr::null_mut();
    call_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let (_disabled_args_storage, disabled_args_buffer) =
        make_borrowed_buffer(r#"{"note":"before-disable"}"#);
    let disabled_call_status = unsafe {
        luaskills_ffi_call_skill(
            engine_id,
            tool_name.as_ptr(),
            disabled_args_buffer,
            ptr::null(),
            &mut result_out,
            &mut call_error,
        )
    };
    assert_ne!(disabled_call_status, FFI_STATUS_OK);
    assert!(result_out.is_null());
    assert!(!call_error.ptr.is_null());
    unsafe { luaskills_ffi_buffer_free(call_error) };

    let mut enable_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let enable_status = unsafe {
        luaskills_ffi_enable_skill(
            engine_id,
            ffi_skill_roots.as_ptr(),
            ffi_skill_roots.len(),
            skill_id.as_ptr(),
            &mut enable_error,
        )
    };
    assert_eq!(enable_status, FFI_STATUS_OK);
    assert!(enable_error.ptr.is_null());

    entries_out = ptr::null_mut();
    list_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let enabled_list_status = unsafe {
        luaskills_ffi_list_entries(
            engine_id,
            FFI_SKILL_AUTHORITY_SYSTEM,
            &mut entries_out,
            &mut list_error,
        )
    };
    assert_eq!(enabled_list_status, FFI_STATUS_OK);
    assert!(list_error.ptr.is_null());
    assert!(!entries_out.is_null());
    let enabled_entries_ref = unsafe { &*entries_out };
    assert_eq!(enabled_entries_ref.len, 1);
    unsafe { luaskills_ffi_entry_list_free(entries_out) };

    let (_enabled_args_storage, enabled_args_buffer) =
        make_borrowed_buffer(r#"{"note":"after-enable"}"#);
    result_out = ptr::null_mut();
    call_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let enabled_call_status = unsafe {
        luaskills_ffi_call_skill(
            engine_id,
            tool_name.as_ptr(),
            enabled_args_buffer,
            ptr::null(),
            &mut result_out,
            &mut call_error,
        )
    };
    assert_eq!(enabled_call_status, FFI_STATUS_OK);
    assert!(call_error.ptr.is_null());
    let enabled_result_ref = unsafe { &*result_out };
    assert_eq!(
        read_owned_buffer_text(&enabled_result_ref.content),
        "lifecycle:after-enable"
    );
    unsafe { luaskills_ffi_invocation_result_free(result_out) };

    let mut free_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let free_status = unsafe { luaskills_ffi_engine_free(engine_id, &mut free_error) };
    assert_eq!(free_status, FFI_STATUS_OK);
    assert!(free_error.ptr.is_null());

    let _ = std::fs::remove_dir_all(&temp_root);
}
