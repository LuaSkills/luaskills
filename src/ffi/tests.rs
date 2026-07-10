use super::{
    EngineHandleJsonResult, EngineIdJsonRequest, EngineNewJsonRequest, FFI_ENGINE_COUNTER,
    FfiEngineSlot, SkillConfigGetJsonRequest, SkillConfigListJsonRequest,
    SkillConfigSetJsonRequest, encode_json_buffer, ffi_engine_registry, lock_ffi_engine_registry,
    luaskills_ffi_call_skill_json, luaskills_ffi_describe_json, luaskills_ffi_engine_free_json,
    luaskills_ffi_engine_new_json, luaskills_ffi_is_skill_json, luaskills_ffi_list_entries_json,
    luaskills_ffi_list_skill_help_json, luaskills_ffi_prompt_argument_completions_json,
    luaskills_ffi_render_skill_help_detail_json, luaskills_ffi_run_lua_json,
    luaskills_ffi_runtime_lease_close_json, luaskills_ffi_runtime_lease_create_json,
    luaskills_ffi_runtime_lease_eval_json, luaskills_ffi_runtime_lease_list_json,
    luaskills_ffi_skill_config_delete_json, luaskills_ffi_skill_config_get_json,
    luaskills_ffi_skill_config_list_json, luaskills_ffi_skill_config_set_json,
    luaskills_ffi_skill_name_for_tool_json,
    luaskills_ffi_system_private_install_skill_from_url_manifest_json,
    luaskills_ffi_system_runtime_lease_close_json, luaskills_ffi_system_runtime_lease_create_json,
    luaskills_ffi_system_runtime_lease_eval_json, luaskills_ffi_system_runtime_lease_list_json,
    with_engine, with_engine_mut,
};
use crate::ffi_standard::{FfiBorrowedBuffer, FfiOwnedBuffer, luaskills_ffi_buffer_free};
use crate::{
    LuaEngine, LuaEngineOptions, LuaVmPoolConfig, RuntimeSkillRoot, SkillManagementAuthority,
};
use serde::ser::{Serialize, Serializer};
use std::ffi::CString;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};

/// Read one FFI JSON response string back into one serde_json value.
/// 将单个 FFI JSON 响应字符串回读为一个 serde_json 值。
unsafe fn decode_response_json(buffer: FfiOwnedBuffer) -> serde_json::Value {
    let bytes = if buffer.ptr.is_null() {
        assert_eq!(buffer.len, 0, "null response pointer must have zero len");
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) }
    };
    let text = std::str::from_utf8(bytes).expect("ffi json must be utf-8");
    let value = serde_json::from_str(text).expect("ffi json must parse");
    unsafe { luaskills_ffi_buffer_free(buffer) };
    value
}

/// Build one borrowed buffer view over one CString JSON payload for JSON FFI tests.
/// 为 JSON FFI 测试中的单个 CString JSON 载荷构造一个借用缓冲视图。
fn borrowed_json_buffer(value: &CString) -> FfiBorrowedBuffer {
    let bytes = value.as_bytes();
    FfiBorrowedBuffer {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    }
}

/// Return one shared test guard that serializes FFI tests touching the global engine registry.
/// 返回一把用于串行化访问全局引擎注册表的共享测试锁。
fn ffi_test_guard() -> MutexGuard<'static, ()> {
    static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    match TEST_MUTEX.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// One test-only serializer that always fails with quoted diagnostic text.
/// 一个仅供测试使用、总是返回带引号诊断文本的失败序列化器。
struct FailingJsonSerialize;

impl Serialize for FailingJsonSerialize {
    /// Return a controlled serializer failure for fallback JSON envelope tests.
    /// 为兜底 JSON 包络测试返回受控的序列化失败。
    ///
    /// The serializer parameter is the active serde serializer selected by the caller.
    /// serializer 参数是调用方选择的当前 serde 序列化器。
    ///
    /// Return a serializer-specific error containing text that must be JSON-escaped.
    /// 返回一条包含必须被 JSON 转义文本的序列化器专属错误。
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(serde::ser::Error::custom("quoted \"serializer\" failure"))
    }
}

/// Verify response serialization failures are returned as escaped JSON error envelopes.
/// 验证响应序列化失败会以已转义的 JSON 错误包络返回。
#[test]
fn encode_json_buffer_serialization_failure_returns_escaped_error_envelope() {
    // Decoded fallback response emitted after the original serializer fails.
    // 原始序列化器失败后产生并解码的兜底响应。
    let response = unsafe { decode_response_json(encode_json_buffer(&FailingJsonSerialize)) };

    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"],
        "Failed to serialize FFI response: quoted \"serializer\" failure"
    );
    assert!(response.get("result").is_none());
}

/// One test-only registered engine handle that cleans itself from the global registry on drop.
/// 一个仅供测试使用的已注册引擎句柄，并在释放时自动从全局注册表清理。
struct TestFfiEngineHandle {
    engine_id: u64,
}

impl Drop for TestFfiEngineHandle {
    fn drop(&mut self) {
        lock_ffi_engine_registry().remove(&self.engine_id);
    }
}

/// Register one minimal engine into the global FFI registry for concurrency tests.
/// 将一个最小引擎注册到全局 FFI 注册表中，用于并发相关测试。
fn register_test_engine() -> TestFfiEngineHandle {
    let engine = LuaEngine::new(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        crate::LuaRuntimeHostOptions::default(),
    ))
    .expect("create ffi test engine");
    let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    lock_ffi_engine_registry().insert(engine_id, FfiEngineSlot::new(engine));
    TestFfiEngineHandle { engine_id }
}

/// Verify runtime session JSON FFI preserves VM state across eval calls.
/// 验证运行时会话 JSON FFI 会在多次 eval 调用之间保留 VM 状态。
#[test]
fn ffi_runtime_session_json_preserves_vm_state() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    let create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-session-test",
            "ttl_sec": 60
        })
        .to_string(),
    )
    .expect("create request");
    let created = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_create_json(
            borrowed_json_buffer(&create_request),
        ))
    };
    assert_eq!(created["ok"], true);
    assert_eq!(created["result"]["ok"], true);
    let lease_id = created["result"]["lease_id"]
        .as_str()
        .expect("lease id")
        .to_string();

    let first_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": lease_id,
            "code": "counter = (counter or 0) + 1; return counter"
        })
        .to_string(),
    )
    .expect("first eval request");
    let first = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_eval_json(borrowed_json_buffer(
            &first_request,
        )))
    };
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"]["ok"], true);
    assert_eq!(first["result"]["result"], serde_json::json!(1));

    let second_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": lease_id,
            "code": "counter = (counter or 0) + 1; return counter"
        })
        .to_string(),
    )
    .expect("second eval request");
    let second = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_eval_json(borrowed_json_buffer(
            &second_request,
        )))
    };
    assert_eq!(second["ok"], true);
    assert_eq!(second["result"]["result"], serde_json::json!(2));

    let close_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": lease_id
        })
        .to_string(),
    )
    .expect("close request");
    let closed = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_close_json(
            borrowed_json_buffer(&close_request),
        ))
    };
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["result"]["closed"], true);
}

/// Verify runtime-session JSON FFI lists active leases and hides closed ones.
/// 验证运行时会话 JSON FFI 会列出活跃租约并隐藏已关闭租约。
#[test]
fn ffi_runtime_session_json_lists_active_leases() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();

    let alpha_create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-alpha-session",
            "ttl_sec": 60
        })
        .to_string(),
    )
    .expect("alpha create request");
    let alpha_created = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_create_json(
            borrowed_json_buffer(&alpha_create_request),
        ))
    };
    assert_eq!(alpha_created["ok"], true);
    let alpha_lease_id = alpha_created["result"]["lease_id"]
        .as_str()
        .expect("alpha lease id")
        .to_string();

    let beta_create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-beta-session",
            "ttl_sec": 60
        })
        .to_string(),
    )
    .expect("beta create request");
    let beta_created = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_create_json(
            borrowed_json_buffer(&beta_create_request),
        ))
    };
    assert_eq!(beta_created["ok"], true);

    let list_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id
        })
        .to_string(),
    )
    .expect("list request");
    let listed = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_list_json(borrowed_json_buffer(
            &list_request,
        )))
    };
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["result"]["ok"], true);
    assert_eq!(listed["result"]["leases"].as_array().map(Vec::len), Some(2));

    let close_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": alpha_lease_id
        })
        .to_string(),
    )
    .expect("alpha close request");
    let closed = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_close_json(
            borrowed_json_buffer(&close_request),
        ))
    };
    assert_eq!(closed["ok"], true);

    let filtered_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-alpha-session"
        })
        .to_string(),
    )
    .expect("filtered list request");
    let filtered = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_list_json(borrowed_json_buffer(
            &filtered_request,
        )))
    };
    assert_eq!(filtered["ok"], true);
    assert_eq!(
        filtered["result"]["leases"].as_array().map(Vec::len),
        Some(0)
    );
}

/// Verify runtime-session JSON FFI rejects mismatched echoed generation values.
/// 验证运行时会话 JSON FFI 会拒绝不匹配的回传 generation。
#[test]
fn ffi_runtime_session_json_rejects_generation_mismatch() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    let create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-generation-session",
            "ttl_sec": 60
        })
        .to_string(),
    )
    .expect("generation create request");
    let created = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_create_json(
            borrowed_json_buffer(&create_request),
        ))
    };
    assert_eq!(created["ok"], true);
    let lease_id = created["result"]["lease_id"]
        .as_str()
        .expect("generation lease id")
        .to_string();
    let sid = created["result"]["sid"]
        .as_str()
        .expect("generation sid")
        .to_string();

    let eval_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": lease_id,
            "sid": sid,
            "generation": 999_u64,
            "code": "return 1"
        })
        .to_string(),
    )
    .expect("generation eval request");
    let eval = unsafe {
        decode_response_json(luaskills_ffi_runtime_lease_eval_json(borrowed_json_buffer(
            &eval_request,
        )))
    };
    assert_eq!(eval["ok"], true);
    assert_eq!(eval["result"]["ok"], false);
    assert_eq!(
        eval["result"]["error_code"],
        serde_json::json!("lease_generation_mismatch")
    );
}

/// Verify the exported JSON FFI descriptor includes system runtime-session entrypoints for SDK probing.
/// 验证导出的 JSON FFI 描述包含供 SDK 探测的 system 运行时会话入口。
#[test]
fn ffi_describe_json_lists_system_runtime_session_exports() {
    let described = unsafe { decode_response_json(luaskills_ffi_describe_json()) };
    assert_eq!(described["ok"], true);
    let exported = described["result"]["exported_functions"]
        .as_array()
        .expect("exported_functions array");
    let exported_names: Vec<&str> = exported
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(exported_names.contains(&"luaskills_ffi_system_runtime_lease_create_json"));
    assert!(exported_names.contains(&"luaskills_ffi_system_runtime_lease_eval_json"));
    assert!(exported_names.contains(&"luaskills_ffi_system_runtime_lease_status_json"));
    assert!(exported_names.contains(&"luaskills_ffi_system_runtime_lease_list_json"));
    assert!(exported_names.contains(&"luaskills_ffi_system_runtime_lease_close_json"));
    assert!(
        exported_names
            .contains(&"luaskills_ffi_system_private_install_skill_from_url_manifest_json")
    );
    assert!(
        exported_names
            .contains(&"luaskills_ffi_system_private_update_skill_from_url_manifest_json")
    );
}

/// Verify host-private URL-manifest JSON FFI requires full system authority.
/// 验证宿主私有 URL manifest JSON FFI 要求完整 system 权限。
#[test]
fn ffi_private_url_manifest_json_requires_system_authority() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    let request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "skill_roots": [{
                "name": "ROOT",
                "skills_dir": "D:/tmp/luaskills-root"
            }],
            "skill_id": "internal-skill",
            "manifest_url": "https://internal.example.com/skills/internal-skill.json",
            "authority": "delegated_tool"
        })
        .to_string(),
    )
    .expect("private manifest request");
    let response = unsafe {
        decode_response_json(
            luaskills_ffi_system_private_install_skill_from_url_manifest_json(
                borrowed_json_buffer(&request),
            ),
        )
    };
    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .expect("private manifest authority error")
            .contains("requires system authority")
    );
}

/// Verify system runtime-session JSON FFI rejects requests that omit authority.
/// 验证 system 运行时会话 JSON FFI 会拒绝缺少 authority 的请求。
#[test]
fn ffi_system_runtime_session_json_requires_authority() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    let create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-system-session",
            "ttl_sec": 60
        })
        .to_string(),
    )
    .expect("system create request");
    let created = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_create_json(
            borrowed_json_buffer(&create_request),
        ))
    };
    assert_eq!(created["ok"], false);
    assert!(
        created["error"]
            .as_str()
            .expect("system runtime session error")
            .contains("requires host-injected authority")
    );
}

/// Verify system runtime-session JSON FFI accepts delegated authority and preserves VM state.
/// 验证 system 运行时会话 JSON FFI 接受 delegated authority 并保留 VM 状态。
#[test]
fn ffi_system_runtime_session_json_supports_delegated_wrapper_flow() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    let create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-system-wrapper-session",
            "ttl_sec": 60,
            "replace": true,
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("system create request");
    let created = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_create_json(
            borrowed_json_buffer(&create_request),
        ))
    };
    assert_eq!(created["ok"], true);
    assert_eq!(created["result"]["ok"], true);
    let lease_id = created["result"]["lease_id"]
        .as_str()
        .expect("system lease id")
        .to_string();
    let sid = created["result"]["sid"]
        .as_str()
        .expect("system sid")
        .to_string();
    let generation = created["result"]["generation"]
        .as_u64()
        .expect("system generation");

    let first_eval_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": lease_id,
            "sid": sid,
            "generation": generation,
            "code": "counter = (counter or 0) + 1; return counter",
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("system first eval request");
    let first = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_eval_json(
            borrowed_json_buffer(&first_eval_request),
        ))
    };
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"]["result"], serde_json::json!(1));

    let second_eval_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": created["result"]["lease_id"],
            "sid": created["result"]["sid"],
            "generation": created["result"]["generation"],
            "code": "counter = (counter or 0) + 1; return counter",
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("system second eval request");
    let second = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_eval_json(
            borrowed_json_buffer(&second_eval_request),
        ))
    };
    assert_eq!(second["ok"], true);
    assert_eq!(second["result"]["result"], serde_json::json!(2));

    let list_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": created["result"]["sid"],
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("system list request");
    let listed = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_list_json(
            borrowed_json_buffer(&list_request),
        ))
    };
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["result"]["leases"].as_array().map(Vec::len), Some(1));

    let close_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": created["result"]["lease_id"],
            "sid": created["result"]["sid"],
            "generation": created["result"]["generation"],
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("system close request");
    let closed = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_close_json(
            borrowed_json_buffer(&close_request),
        ))
    };
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["result"]["closed"], true);
}

/// Write one enabled skill fixture with entry and help metadata for FFI query tests.
/// 为 FFI 查询测试写入带入口与帮助元数据的启用技能夹具。
fn write_query_test_skill(skill_root: &Path, skill_id: &str) -> PathBuf {
    let skill_dir = skill_root.join(skill_id);
    std::fs::create_dir_all(skill_dir.join("runtime")).expect("create query runtime dir");
    std::fs::create_dir_all(skill_dir.join("help")).expect("create query help dir");
    std::fs::write(
            skill_dir.join("skill.yaml"),
            format!(
                "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nhelp:\n  main:\n    description: Main help.\n    file: help/main.md\nentries:\n  - name: ping\n    description: Query ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
            ),
        )
        .expect("write query skill yaml");
    std::fs::write(
        skill_dir.join("runtime").join("ping.lua"),
        "return function(args)\n  return 'query-ok'\nend\n",
    )
    .expect("write query runtime entry");
    std::fs::write(
        skill_dir.join("help").join("main.md"),
        format!("# {skill_id}\n\nQuery help.\n"),
    )
    .expect("write query help file");
    skill_dir
}

/// Write one FFI query-test skill whose final input schema is provided by one external JSON file.
/// 写入一个最终输入 schema 由外部 JSON 文件提供的 FFI 查询测试技能。
fn write_query_schema_test_skill(skill_root: &Path, skill_id: &str) -> PathBuf {
    let skill_dir = skill_root.join(skill_id);
    std::fs::create_dir_all(skill_dir.join("runtime")).expect("create query schema runtime dir");
    std::fs::create_dir_all(skill_dir.join("help")).expect("create query schema help dir");
    std::fs::create_dir_all(skill_dir.join("schemas")).expect("create query schema schema dir");
    std::fs::write(
        skill_dir.join("skill.yaml"),
        format!(
            "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nhelp:\n  main:\n    description: Main help.\n    file: help/main.md\nentries:\n  - name: inspect\n    description: Query schema entry.\n    lua_entry: runtime/inspect.lua\n    lua_module: {skill_id}.inspect\n    input_schema_file: schemas/inspect.input.schema.json\n"
        ),
    )
    .expect("write query schema skill yaml");
    std::fs::write(
        skill_dir.join("schemas").join("inspect.input.schema.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "nodes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file": { "type": "string" },
                            "structural_path": { "type": "string" }
                        },
                        "required": ["file", "structural_path"]
                    }
                }
            },
            "required": ["nodes"]
        }))
        .expect("serialize query schema input schema"),
    )
    .expect("write query schema input schema");
    std::fs::write(
        skill_dir.join("runtime").join("inspect.lua"),
        "return function(args)\n  return 'schema-query-ok'\nend\n",
    )
    .expect("write query schema runtime entry");
    std::fs::write(
        skill_dir.join("help").join("main.md"),
        format!("# {skill_id}\n\nQuery help.\n"),
    )
    .expect("write query schema help file");
    skill_dir
}

/// Verify JSON FFI query entrypoints enforce authority-based ROOT visibility.
/// 验证 JSON FFI 查询入口会执行基于权限的 ROOT 可见性控制。
#[test]
fn ffi_query_json_filters_root_for_delegated_authority() {
    let _guard = ffi_test_guard();
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_ffi_query_authority_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let root_root = RuntimeSkillRoot {
        name: " ROOT ".to_string(),
        skills_dir: temp_root.join("root_skills"),
    };
    let user_root = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: temp_root.join("user_skills"),
    };
    write_query_test_skill(&root_root.skills_dir, "vulcan-root-skill");
    write_query_test_skill(&user_root.skills_dir, "vulcan-user-skill");
    let mut engine = LuaEngine::new(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        crate::LuaRuntimeHostOptions::default(),
    ))
    .expect("create ffi query test engine");
    engine
        .load_from_roots(&[root_root, user_root])
        .expect("load query test roots");
    let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    lock_ffi_engine_registry().insert(engine_id, FfiEngineSlot::new(engine));
    let _handle = TestFfiEngineHandle { engine_id };

    let system_entries_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "authority": SkillManagementAuthority::System
        })
        .to_string(),
    )
    .expect("system entries request");
    let system_entries = unsafe {
        decode_response_json(luaskills_ffi_list_entries_json(borrowed_json_buffer(
            &system_entries_request,
        )))
    };
    assert_eq!(system_entries["ok"], true);
    assert!(
        system_entries["result"]
            .as_array()
            .expect("system entries array")
            .iter()
            .any(|entry| entry["root_name"] == " ROOT ")
    );

    let delegated_entries_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("delegated entries request");
    let delegated_entries = unsafe {
        decode_response_json(luaskills_ffi_list_entries_json(borrowed_json_buffer(
            &delegated_entries_request,
        )))
    };
    assert_eq!(delegated_entries["ok"], true);
    assert!(
        delegated_entries["result"]
            .as_array()
            .expect("delegated entries array")
            .iter()
            .all(|entry| entry["root_name"]
                .as_str()
                .map(|root_name| !root_name.trim().eq_ignore_ascii_case("ROOT"))
                .unwrap_or(false))
    );

    let delegated_help = unsafe {
        decode_response_json(luaskills_ffi_list_skill_help_json(borrowed_json_buffer(
            &delegated_entries_request,
        )))
    };
    assert_eq!(delegated_help["ok"], true);
    assert!(
        delegated_help["result"]
            .as_array()
            .expect("delegated help array")
            .iter()
            .all(|help| help["root_name"]
                .as_str()
                .map(|root_name| !root_name.trim().eq_ignore_ascii_case("ROOT"))
                .unwrap_or(false))
    );

    let delegated_detail_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "authority": SkillManagementAuthority::DelegatedTool,
            "skill_id": "vulcan-root-skill",
            "flow_name": "main"
        })
        .to_string(),
    )
    .expect("delegated detail request");
    let delegated_detail = unsafe {
        decode_response_json(luaskills_ffi_render_skill_help_detail_json(
            borrowed_json_buffer(&delegated_detail_request),
        ))
    };
    assert_eq!(delegated_detail["ok"], true);
    assert!(delegated_detail["result"].is_null());

    let delegated_is_skill_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "authority": SkillManagementAuthority::DelegatedTool,
            "tool_name": "vulcan-root-skill-ping"
        })
        .to_string(),
    )
    .expect("delegated is_skill request");
    let delegated_is_skill = unsafe {
        decode_response_json(luaskills_ffi_is_skill_json(borrowed_json_buffer(
            &delegated_is_skill_request,
        )))
    };
    assert_eq!(delegated_is_skill["ok"], true);
    assert_eq!(delegated_is_skill["result"]["value"], false);

    let delegated_skill_name = unsafe {
        decode_response_json(luaskills_ffi_skill_name_for_tool_json(
            borrowed_json_buffer(&delegated_is_skill_request),
        ))
    };
    assert_eq!(delegated_skill_name["ok"], true);
    assert!(delegated_skill_name["result"]["skill_id"].is_null());

    let root_call_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "tool_name": "vulcan-root-skill-ping",
            "args": {}
        })
        .to_string(),
    )
    .expect("root call request");
    let root_call = unsafe {
        decode_response_json(luaskills_ffi_call_skill_json(borrowed_json_buffer(
            &root_call_request,
        )))
    };
    assert_eq!(root_call["ok"], true);
    assert_eq!(root_call["result"]["content"], "query-ok");

    let root_run_lua_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "code": "return vulcan.call('vulcan-root-skill-ping', {})",
            "args": {}
        })
        .to_string(),
    )
    .expect("root run_lua request");
    let root_run_lua = unsafe {
        decode_response_json(luaskills_ffi_run_lua_json(borrowed_json_buffer(
            &root_run_lua_request,
        )))
    };
    assert_eq!(root_run_lua["ok"], true);
    assert_eq!(root_run_lua["result"], "query-ok");

    let delegated_prompt_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "authority": SkillManagementAuthority::DelegatedTool,
            "prompt_name": "demo",
            "argument_name": "target"
        })
        .to_string(),
    )
    .expect("delegated prompt request");
    let delegated_prompt = unsafe {
        decode_response_json(luaskills_ffi_prompt_argument_completions_json(
            borrowed_json_buffer(&delegated_prompt_request),
        ))
    };
    assert_eq!(delegated_prompt["ok"], true);
    assert!(delegated_prompt["result"].is_null());

    let missing_prompt_authority_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "prompt_name": "demo",
            "argument_name": "target"
        })
        .to_string(),
    )
    .expect("missing prompt authority request");
    let missing_prompt_authority = unsafe {
        decode_response_json(luaskills_ffi_prompt_argument_completions_json(
            borrowed_json_buffer(&missing_prompt_authority_request),
        ))
    };
    assert_eq!(missing_prompt_authority["ok"], false);
    assert!(
        missing_prompt_authority["error"]
            .as_str()
            .expect("missing prompt authority error")
            .contains("requires host-injected authority")
    );

    let missing_authority_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id
        })
        .to_string(),
    )
    .expect("missing authority request");
    let missing_authority = unsafe {
        decode_response_json(luaskills_ffi_list_entries_json(borrowed_json_buffer(
            &missing_authority_request,
        )))
    };
    assert_eq!(missing_authority["ok"], false);
    assert!(
        missing_authority["error"]
            .as_str()
            .expect("missing authority error")
            .contains("requires host-injected authority")
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify JSON FFI entry listing exports the resolved object schema for schema-file based entries.
/// 验证 JSON FFI 入口列表会导出基于 schema 文件入口的已解析对象 schema。
#[test]
fn ffi_list_entries_json_exposes_resolved_input_schema() {
    let _guard = ffi_test_guard();
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_ffi_query_schema_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let root_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: temp_root.join("root_skills"),
    };
    write_query_schema_test_skill(&root_root.skills_dir, "vulcan-schema-skill");
    let mut engine = LuaEngine::new(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        crate::LuaRuntimeHostOptions {
            runtime_root: None,
            temp_dir: Some(temp_root.join("temp")),
            resources_dir: Some(temp_root.join("resources")),
            lua_packages_dir: Some(temp_root.join("lua_packages")),
            host_provided_tool_root: Some(temp_root.join("bin").join("tools")),
            host_provided_lua_root: Some(temp_root.join("lua_packages")),
            host_provided_ffi_root: Some(temp_root.join("libs")),
            download_cache_root: Some(temp_root.join("temp").join("downloads")),
            ..crate::LuaRuntimeHostOptions::default()
        },
    ))
    .expect("create FFI query schema engine");
    engine
        .load_from_roots(&[root_root])
        .expect("load FFI query schema root");
    let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    lock_ffi_engine_registry().insert(engine_id, FfiEngineSlot::new(engine));

    let list_request = CString::new(
        serde_json::json!({
            "engine_id": engine_id,
            "authority": SkillManagementAuthority::System
        })
        .to_string(),
    )
    .expect("schema list request");
    let listed = unsafe {
        decode_response_json(luaskills_ffi_list_entries_json(borrowed_json_buffer(
            &list_request,
        )))
    };
    assert_eq!(listed["ok"], true);
    let result = listed["result"]
        .as_array()
        .expect("schema list result array");
    let entry = result
        .iter()
        .find(|item| item["local_name"] == "inspect")
        .expect("inspect schema entry");
    assert_eq!(entry["input_schema"]["type"], "object");
    assert_eq!(
        entry["input_schema"]["required"],
        serde_json::json!(["nodes"])
    );
    assert_eq!(
        entry["input_schema"]["properties"]["nodes"]["items"]["properties"]["file"]["type"],
        "string"
    );
    assert_eq!(entry["parameters"][0]["name"], "nodes");
    assert_eq!(entry["parameters"][0]["param_type"], "array");

    lock_ffi_engine_registry().remove(&engine_id);
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify JSON FFI engine handles can be created and freed after the global registry lock is poisoned.
/// 验证全局注册表锁 poison 后仍可通过 JSON FFI 创建并释放引擎句柄。
#[test]
fn ffi_engine_registry_recovers_after_poisoned_lock_for_json_handles() {
    let _guard = ffi_test_guard();

    // Captured panic result from a registry writer that poisons the global FFI engine registry.
    // 全局 FFI 引擎注册表写入者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the global FFI engine registry for this recovery test.
        // 仅用于为本恢复测试制造全局 FFI 引擎注册表 poison 的保护对象。
        let _registry_guard = ffi_engine_registry()
            .lock()
            .expect("initial ffi engine registry lock");
        panic!("poison ffi engine registry for recovery test");
    }));

    assert!(poison_result.is_err());

    // Engine creation request executed after registry poisoning.
    // 在注册表 poison 后执行的引擎创建请求。
    let request = EngineNewJsonRequest {
        options: LuaEngineOptions::new(
            LuaVmPoolConfig {
                min_size: 1,
                max_size: 1,
                idle_ttl_secs: 30,
            },
            crate::LuaRuntimeHostOptions::default(),
        ),
    };
    // JSON request buffer passed into the JSON FFI engine creation entrypoint.
    // 传入 JSON FFI 引擎创建入口的 JSON 请求缓冲。
    let request_json =
        CString::new(serde_json::to_string(&request).expect("engine new request json"))
            .expect("engine new request cstring");
    // Engine creation response proving registry insert recovery.
    // 用于证明注册表 insert 已恢复的引擎创建响应。
    let created = unsafe {
        decode_response_json(luaskills_ffi_engine_new_json(borrowed_json_buffer(
            &request_json,
        )))
    };

    assert_eq!(created["ok"], true);

    // Engine id created through the recovered global registry.
    // 通过已恢复全局注册表创建得到的引擎标识。
    let engine_id = created["result"]["engine_id"]
        .as_u64()
        .expect("created engine id");
    // Engine free request used to verify registry remove recovery.
    // 用于验证注册表 remove 已恢复的引擎释放请求。
    let free_request = CString::new(
        serde_json::to_string(&EngineIdJsonRequest { engine_id }).expect("free request json"),
    )
    .expect("free request cstring");
    // Engine free response proving registry removal still succeeds after poisoning.
    // 用于证明 poison 后注册表删除仍会成功的引擎释放响应。
    let freed = unsafe {
        decode_response_json(luaskills_ffi_engine_free_json(borrowed_json_buffer(
            &free_request,
        )))
    };

    assert_eq!(freed["ok"], true);
}

/// Verify that one engine can be created and freed through the JSON FFI surface.
/// 验证可以通过 JSON FFI 入口创建并释放单个引擎。
#[test]
fn ffi_engine_new_and_free_roundtrip() {
    let _guard = ffi_test_guard();
    let temp_root =
        std::env::temp_dir().join(format!("luaskills_ffi_engine_test_{}", std::process::id()));
    let request = EngineNewJsonRequest {
        options: LuaEngineOptions::new(
            LuaVmPoolConfig {
                min_size: 1,
                max_size: 1,
                idle_ttl_secs: 30,
            },
            crate::LuaRuntimeHostOptions {
                runtime_root: None,
                temp_dir: Some(temp_root.join("temp")),
                resources_dir: Some(temp_root.join("resources")),
                lua_packages_dir: Some(temp_root.join("lua_packages")),
                host_provided_tool_root: Some(temp_root.join("bin").join("tools")),
                host_provided_lua_root: Some(temp_root.join("lua_packages")),
                host_provided_ffi_root: Some(temp_root.join("libs")),
                system_lua_lib_dir: None,
                download_cache_root: Some(temp_root.join("temp").join("downloads")),
                dependency_dir_name: "dependencies".to_string(),
                state_dir_name: "state".to_string(),
                database_dir_name: "databases".to_string(),
                skill_config_file_path: None,
                allow_network_download: false,
                github_base_url: None,
                github_api_base_url: None,
                official_skill_hub_base_url: None,
                enable_private_url_skill_install: false,
                private_skill_source_allowlist: Vec::new(),
                default_text_encoding: None,
                sqlite_library_path: None,
                sqlite_provider_mode: crate::LuaRuntimeDatabaseProviderMode::DynamicLibrary,
                sqlite_callback_mode: crate::LuaRuntimeDatabaseCallbackMode::Standard,
                lancedb_library_path: None,
                lancedb_provider_mode: crate::LuaRuntimeDatabaseProviderMode::DynamicLibrary,
                lancedb_callback_mode: crate::LuaRuntimeDatabaseCallbackMode::Standard,
                space_controller: crate::LuaRuntimeSpaceControllerOptions::default(),
                cache_config: None,
                runlua_pool_config: None,
                reserved_entry_names: Vec::new(),
                ignored_skill_ids: Vec::new(),
                capabilities: Default::default(),
            },
        ),
    };
    let input = CString::new(serde_json::to_string(&request).expect("request json"))
        .expect("request cstring");
    let response = unsafe {
        decode_response_json(luaskills_ffi_engine_new_json(borrowed_json_buffer(&input)))
    };
    assert_eq!(response["ok"], true);
    let result: EngineHandleJsonResult =
        serde_json::from_value(response["result"].clone()).expect("engine result should parse");

    let free_request = CString::new(
        serde_json::to_string(&EngineIdJsonRequest {
            engine_id: result.engine_id,
        })
        .expect("free request json"),
    )
    .expect("free request cstring");
    let free_response = unsafe {
        decode_response_json(luaskills_ffi_engine_free_json(borrowed_json_buffer(
            &free_request,
        )))
    };
    assert_eq!(free_response["ok"], true);
}

/// Verify the JSON FFI skill-config helpers support one full set/get/list/delete roundtrip.
/// 验证 JSON FFI 的技能配置辅助接口支持完整的 set/get/list/delete 往返流程。
#[test]
fn ffi_skill_config_json_roundtrip() {
    let _guard = ffi_test_guard();
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_ffi_skill_config_json_test_{}",
        std::process::id()
    ));
    let request = EngineNewJsonRequest {
        options: LuaEngineOptions::new(
            LuaVmPoolConfig {
                min_size: 1,
                max_size: 1,
                idle_ttl_secs: 30,
            },
            crate::LuaRuntimeHostOptions {
                runtime_root: None,
                temp_dir: Some(temp_root.join("temp")),
                resources_dir: Some(temp_root.join("resources")),
                lua_packages_dir: Some(temp_root.join("lua_packages")),
                host_provided_tool_root: Some(temp_root.join("bin").join("tools")),
                host_provided_lua_root: Some(temp_root.join("lua_packages")),
                host_provided_ffi_root: Some(temp_root.join("libs")),
                system_lua_lib_dir: None,
                download_cache_root: Some(temp_root.join("temp").join("downloads")),
                dependency_dir_name: "dependencies".to_string(),
                state_dir_name: "state".to_string(),
                database_dir_name: "databases".to_string(),
                skill_config_file_path: Some(temp_root.join("config").join("skill_config.json")),
                allow_network_download: false,
                github_base_url: None,
                github_api_base_url: None,
                official_skill_hub_base_url: None,
                enable_private_url_skill_install: false,
                private_skill_source_allowlist: Vec::new(),
                default_text_encoding: None,
                sqlite_library_path: None,
                sqlite_provider_mode: crate::LuaRuntimeDatabaseProviderMode::DynamicLibrary,
                sqlite_callback_mode: crate::LuaRuntimeDatabaseCallbackMode::Standard,
                lancedb_library_path: None,
                lancedb_provider_mode: crate::LuaRuntimeDatabaseProviderMode::DynamicLibrary,
                lancedb_callback_mode: crate::LuaRuntimeDatabaseCallbackMode::Standard,
                space_controller: crate::LuaRuntimeSpaceControllerOptions::default(),
                cache_config: None,
                runlua_pool_config: None,
                reserved_entry_names: Vec::new(),
                ignored_skill_ids: Vec::new(),
                capabilities: Default::default(),
            },
        ),
    };
    let input = CString::new(serde_json::to_string(&request).expect("request json"))
        .expect("request cstring");
    let response = unsafe {
        decode_response_json(luaskills_ffi_engine_new_json(borrowed_json_buffer(&input)))
    };
    assert_eq!(response["ok"], true);
    let result: EngineHandleJsonResult =
        serde_json::from_value(response["result"].clone()).expect("engine result should parse");

    let set_request = CString::new(
        serde_json::to_string(&SkillConfigSetJsonRequest {
            engine_id: result.engine_id,
            skill_id: "demo-skill".to_string(),
            key: "api_token".to_string(),
            value: "sk-json-ffi".to_string(),
        })
        .expect("set request json"),
    )
    .expect("set request cstring");
    let set_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_set_json(borrowed_json_buffer(
            &set_request,
        )))
    };
    assert_eq!(set_response["ok"], true);
    assert_eq!(set_response["result"]["action"], "set");
    assert_eq!(set_response["result"]["skill_id"], "demo-skill");
    assert_eq!(set_response["result"]["key"], "api_token");
    assert_eq!(set_response["result"]["value"], "sk-json-ffi");

    let get_request = CString::new(
        serde_json::to_string(&SkillConfigGetJsonRequest {
            engine_id: result.engine_id,
            skill_id: "demo-skill".to_string(),
            key: "api_token".to_string(),
        })
        .expect("get request json"),
    )
    .expect("get request cstring");
    let get_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_get_json(borrowed_json_buffer(
            &get_request,
        )))
    };
    assert_eq!(get_response["ok"], true);
    assert_eq!(get_response["result"]["found"], true);
    assert_eq!(get_response["result"]["value"], "sk-json-ffi");

    let list_request = CString::new(
        serde_json::to_string(&SkillConfigListJsonRequest {
            engine_id: result.engine_id,
            skill_id: Some("demo-skill".to_string()),
        })
        .expect("list request json"),
    )
    .expect("list request cstring");
    let list_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_list_json(borrowed_json_buffer(
            &list_request,
        )))
    };
    assert_eq!(list_response["ok"], true);
    assert_eq!(list_response["result"].as_array().map(Vec::len), Some(1));
    assert_eq!(list_response["result"][0]["skill_id"], "demo-skill");
    assert_eq!(list_response["result"][0]["key"], "api_token");
    assert_eq!(list_response["result"][0]["value"], "sk-json-ffi");

    let delete_request = CString::new(
        serde_json::to_string(&SkillConfigGetJsonRequest {
            engine_id: result.engine_id,
            skill_id: "demo-skill".to_string(),
            key: "api_token".to_string(),
        })
        .expect("delete request json"),
    )
    .expect("delete request cstring");
    let delete_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_delete_json(
            borrowed_json_buffer(&delete_request),
        ))
    };
    assert_eq!(delete_response["ok"], true);
    assert_eq!(delete_response["result"]["action"], "delete");
    assert_eq!(delete_response["result"]["deleted"], true);

    let free_request = CString::new(
        serde_json::to_string(&EngineIdJsonRequest {
            engine_id: result.engine_id,
        })
        .expect("free request json"),
    )
    .expect("free request cstring");
    let free_response = unsafe {
        decode_response_json(luaskills_ffi_engine_free_json(borrowed_json_buffer(
            &free_request,
        )))
    };
    assert_eq!(free_response["ok"], true);
}

/// Verify that one engine operation no longer keeps the global registry mutex while running.
/// 验证单次引擎操作执行期间不会继续持有全局注册表互斥锁。
#[test]
fn with_engine_releases_registry_lock_before_operation() {
    let _guard = ffi_test_guard();
    let handle = register_test_engine();
    let result = with_engine(handle.engine_id, |_engine| {
        // Registry availability after try_lock; Poisoned still proves the mutex was not held.
        // try_lock 后得到的注册表可用性；Poisoned 仍证明互斥锁未被持有。
        let registry_available = match ffi_engine_registry().try_lock() {
            Ok(_guard) => true,
            Err(TryLockError::Poisoned(_poisoned)) => true,
            Err(TryLockError::WouldBlock) => false,
        };
        assert!(
            registry_available,
            "registry lock should be acquirable while engine operation is running"
        );
        Ok(())
    });
    assert!(result.is_ok());
}

/// Verify one FFI engine handle remains usable after its own engine lock is poisoned.
/// 验证单个 FFI 引擎句柄自身的引擎锁 poison 后仍可继续使用。
#[test]
fn with_engine_recovers_after_engine_handle_lock_poisoned() {
    // Test-wide guard that serializes access to the shared FFI engine registry.
    // 串行化共享 FFI 引擎注册表访问的测试级保护对象。
    let _guard = ffi_test_guard();
    // Registered engine used to poison and then exercise the same FFI handle.
    // 用于制造 poison 并继续访问同一 FFI 句柄的已注册引擎。
    let handle = register_test_engine();
    // Shared engine handle cloned from the registry so the poison panic does not hold the registry lock.
    // 从注册表克隆出的共享引擎句柄，确保制造 poison 的 panic 不持有注册表锁。
    let engine_handle =
        super::clone_engine_handle(handle.engine_id).expect("clone ffi engine handle");
    // Captured panic result from a holder that poisons only this engine handle lock.
    // 单个引擎句柄锁持有者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the registered engine handle lock.
        // 仅用于制造已注册引擎句柄锁 poison 的保护对象。
        let _engine = engine_handle
            .lock()
            .expect("initial ffi engine handle lock");
        panic!("poison ffi engine handle for recovery test");
    }));

    assert!(poison_result.is_err());

    // Read-only operation result proving shared FFI access recovers the poisoned engine lock.
    // 只读操作结果，用于证明共享 FFI 访问可恢复已 poison 的引擎锁。
    let read_result = with_engine(handle.engine_id, |_engine| Ok(()));
    assert!(read_result.is_ok());

    // Mutable operation result proving mutating FFI access uses the same recovery path.
    // 可变操作结果，用于证明可变 FFI 访问使用同一条恢复路径。
    let write_result = with_engine_mut(handle.engine_id, |_engine| Ok(()));
    assert!(write_result.is_ok());
}

/// Verify that same-thread reentrant access returns an explicit error instead of deadlocking.
/// 验证同线程重入访问会返回明确错误，而不是直接死锁。
#[test]
fn with_engine_rejects_same_thread_reentry() {
    let _guard = ffi_test_guard();
    let handle = register_test_engine();
    let outer_result = with_engine(handle.engine_id, |_engine| {
        let nested_result = with_engine(handle.engine_id, |_nested| Ok(()));
        let nested_error = nested_result.expect_err("same-thread reentry should fail");
        assert!(nested_error.contains("reentrant access"));
        Ok(())
    });
    assert!(outer_result.is_ok());
}
