use super::{
    EngineHandleJsonResult, EngineIdJsonRequest, EngineNewJsonRequest, FFI_ENGINE_COUNTER,
    FfiEngineSlot, SkillConfigGetJsonRequest, SkillConfigListJsonRequest,
    SkillConfigSetJsonRequest, SkillPackageConfigDescribeJsonRequest,
    SkillPackageConfigValidateJsonRequest, encode_json_buffer, ffi_engine_registry,
    lock_ffi_engine_registry, luaskills_ffi_call_skill_json, luaskills_ffi_describe_json,
    luaskills_ffi_engine_free_json, luaskills_ffi_engine_new_json, luaskills_ffi_is_skill_json,
    luaskills_ffi_list_entries_json, luaskills_ffi_list_skill_help_json,
    luaskills_ffi_managed_runtime_resolve_json, luaskills_ffi_managed_session_events_poll_json,
    luaskills_ffi_managed_session_events_wait_json, luaskills_ffi_prompt_argument_completions_json,
    luaskills_ffi_render_skill_help_detail_json, luaskills_ffi_run_lua_json,
    luaskills_ffi_runtime_lease_close_json, luaskills_ffi_runtime_lease_create_json,
    luaskills_ffi_runtime_lease_eval_json, luaskills_ffi_runtime_lease_list_json,
    luaskills_ffi_skill_config_delete_json, luaskills_ffi_skill_config_describe_json,
    luaskills_ffi_skill_config_get_json, luaskills_ffi_skill_config_list_json,
    luaskills_ffi_skill_config_set_json, luaskills_ffi_skill_config_validate_json,
    luaskills_ffi_skill_name_for_tool_json,
    luaskills_ffi_system_private_install_skill_from_url_manifest_json,
    luaskills_ffi_system_runtime_lease_close_json, luaskills_ffi_system_runtime_lease_create_json,
    luaskills_ffi_system_runtime_lease_eval_json, luaskills_ffi_system_runtime_lease_list_json,
    luaskills_ffi_system_runtime_lease_status_json, with_engine, with_engine_mut,
};
use crate::ffi_standard::{
    FfiBorrowedBuffer, FfiOwnedBuffer, luaskills_ffi_buffer_clone, luaskills_ffi_buffer_free,
    luaskills_ffi_engine_free, luaskills_ffi_managed_session_events_poll,
    luaskills_ffi_managed_session_events_wait, luaskills_ffi_run_lua,
    luaskills_ffi_set_managed_session_wake_callback, luaskills_ffi_system_runtime_lease_close,
    luaskills_ffi_system_runtime_lease_create, luaskills_ffi_system_runtime_lease_eval,
};
use crate::runtime::managed_session_events::{
    ManagedSessionEventCenter, ManagedSessionEventToken, RuntimeManagedSessionEventKind,
};
use crate::runtime::render_host_visible_path;
use crate::{
    LuaEngine, LuaEngineOptions, LuaRuntimeHostOptions, LuaVmPoolConfig, RuntimeSkillRoot,
    SkillManagementAuthority, SkillPackageConfigDescribeMode, SkillPackageConfigInputValue,
};
use serde::ser::{Serialize, Serializer};
use std::collections::BTreeMap;
use std::ffi::{CString, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, MutexGuard, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

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

/// Read and release one LuaSkills-owned UTF-8 buffer returned by the standard ABI.
/// 读取并释放标准 ABI 返回的一段 LuaSkills 所有 UTF-8 缓冲。
///
/// `buffer` must be empty or contain one valid LuaSkills-owned UTF-8 allocation.
/// `buffer` 必须为空，或包含一段有效的 LuaSkills 所有 UTF-8 分配。
///
/// Return the copied Rust string after releasing the original allocation.
/// 释放原始分配后返回复制得到的 Rust 字符串。
unsafe fn take_owned_buffer_text(buffer: FfiOwnedBuffer) -> String {
    // Owned bytes borrowed only until the matching free helper runs.
    // 仅借用到匹配释放辅助函数运行前的拥有型字节。
    let text = if buffer.ptr.is_null() {
        assert_eq!(buffer.len, 0, "null owned buffer must have zero len");
        String::new()
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) };
        std::str::from_utf8(bytes)
            .expect("owned FFI buffer must be utf-8")
            .to_string()
    };
    unsafe { luaskills_ffi_buffer_free(buffer) };
    text
}

/// Test state updated by one standard managed-session wake callback.
/// 由标准受管会话唤醒回调更新的测试状态。
struct TestManagedSessionWakeState {
    /// Number of callback invocations observed by the host fixture.
    /// 宿主夹具观察到的回调调用次数。
    callback_count: AtomicUsize,
    /// Most recent engine identifier delivered through the ABI callback.
    /// 最近一次通过 ABI 回调传递的引擎标识。
    last_engine_id: AtomicU64,
}

/// Test state whose first standard wake scheduling request is rejected.
/// 首次标准唤醒调度请求会被拒绝的测试状态。
struct TestRetryingManagedSessionWakeState {
    /// Number of callback attempts observed across failure and retry.
    /// 在失败与重试期间观察到的回调尝试次数。
    callback_count: AtomicUsize,
}

/// Reject the first wake attempt and accept the automatically retried attempt.
/// 拒绝首次唤醒尝试，并接受自动重试的尝试。
///
/// `user_data` points to `TestRetryingManagedSessionWakeState`; other arguments follow the ABI.
/// `user_data` 指向 `TestRetryingManagedSessionWakeState`；其余参数遵循 ABI。
///
/// Return one for the first call, zero for later calls, or one for a null state pointer.
/// 首次调用返回一，后续调用返回零；状态指针为空时返回一。
unsafe extern "C" fn reject_first_managed_session_wake(
    _engine_id: u64,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    if !error_out.is_null() {
        unsafe {
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
    }
    if user_data.is_null() {
        return 1;
    }
    let state = unsafe { &*(user_data.cast::<TestRetryingManagedSessionWakeState>()) };
    let attempt = state.callback_count.fetch_add(1, Ordering::AcqRel) + 1;
    if attempt != 1 {
        return 0;
    }
    let diagnostic = b"forced host scheduler rejection";
    let mut clone_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let clone_status = unsafe {
        luaskills_ffi_buffer_clone(
            diagnostic.as_ptr(),
            diagnostic.len(),
            error_out,
            &mut clone_error,
        )
    };
    if clone_status != 0 && !error_out.is_null() {
        unsafe {
            *error_out = clone_error;
        }
    } else {
        unsafe { luaskills_ffi_buffer_free(clone_error) };
    }
    1
}

/// Record one managed-session wake callback without entering LuaSkills again.
/// 记录一次受管会话唤醒回调，且不再次进入 LuaSkills。
///
/// `engine_id` is the event source, `user_data` points to `TestManagedSessionWakeState`, and
/// `error_out` is cleared on success.
/// `engine_id` 是事件来源，`user_data` 指向 `TestManagedSessionWakeState`，成功时清空
/// `error_out`。
///
/// Return zero for a valid state pointer or one for a null host state.
/// 宿主状态指针有效时返回零；为空时返回一。
unsafe extern "C" fn record_managed_session_wake(
    engine_id: u64,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    if !error_out.is_null() {
        unsafe {
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
    }
    if user_data.is_null() {
        return 1;
    }
    // Host-owned callback state kept alive until callback clearing returns.
    // 保持存活直至回调清除返回的宿主拥有回调状态。
    let state = unsafe { &*(user_data.cast::<TestManagedSessionWakeState>()) };
    state.last_engine_id.store(engine_id, Ordering::Release);
    state.callback_count.fetch_add(1, Ordering::AcqRel);
    0
}

/// Host state used when a wake callback immediately polls the same engine.
/// 唤醒回调立即轮询同一引擎时使用的宿主状态。
struct TestWakePollState {
    /// Number of wake callback invocations completed by the host fixture.
    /// 宿主夹具完成的唤醒回调调用次数。
    callback_count: AtomicUsize,
    /// Standard poll status observed inside the most recent callback.
    /// 最近一次回调内部观察到的标准轮询状态。
    poll_status: AtomicI32,
}

/// Poll managed-session events synchronously from one wake callback without entering Lua.
/// 在单次唤醒回调中同步轮询受管会话事件，且不进入 Lua。
///
/// `engine_id` selects the event source, `user_data` points to `TestWakePollState`, and
/// `error_out` is cleared before returning.
/// `engine_id` 选择事件来源，`user_data` 指向 `TestWakePollState`，返回前清空
/// `error_out`。
///
/// Return zero after recording the nested poll status, or one for a null state pointer.
/// 记录嵌套轮询状态后返回零；状态指针为空时返回一。
unsafe extern "C" fn poll_managed_session_events_from_wake(
    engine_id: u64,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    if !error_out.is_null() {
        unsafe {
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
    }
    if user_data.is_null() {
        return 1;
    }
    // Host-owned callback state kept stable until explicit callback clearing.
    // 保持稳定直至显式清除回调的宿主拥有回调状态。
    let state = unsafe { &*(user_data.cast::<TestWakePollState>()) };
    // Nested direct-batch outputs consumed entirely before the callback returns.
    // 在回调返回前被完全消费的嵌套直接批次输出。
    let mut result_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut poll_error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        luaskills_ffi_managed_session_events_poll(
            engine_id,
            1,
            &mut result_out,
            &mut poll_error_out,
        )
    };
    state.poll_status.store(status, Ordering::Release);
    state.callback_count.fetch_add(1, Ordering::AcqRel);
    unsafe { luaskills_ffi_buffer_free(result_out) };
    unsafe { luaskills_ffi_buffer_free(poll_error_out) };
    0
}

/// Host state used to hold one wake callback in flight during engine destruction.
/// 在引擎析构期间保持一个唤醒回调在途的宿主状态。
struct TestBlockingWakeState {
    /// Engine identifier used by the optional nested poll after release.
    /// 释放后可选嵌套轮询使用的引擎标识。
    engine_id: u64,
    /// Signal emitted after the callback has entered its blocking section.
    /// 回调进入阻塞区后发出的信号。
    entered_tx: std::sync::mpsc::Sender<()>,
    /// Two-party release barrier shared by the callback and test thread.
    /// 回调与测试线程共享的双方释放屏障。
    release: Arc<Barrier>,
    /// Whether the callback should probe the registry through standard event poll.
    /// 回调是否应通过标准事件轮询探测注册表。
    should_poll: AtomicBool,
    /// Nested poll status, or a negative sentinel when the probe is skipped.
    /// 嵌套轮询状态；跳过探测时为负数哨兵。
    poll_status: AtomicI32,
}

/// Block one wake callback until engine free has removed its registry entry.
/// 阻塞单次唤醒回调，直至 engine free 已移除对应注册表条目。
///
/// `user_data` points to `TestBlockingWakeState`; the other callback parameters follow the ABI.
/// `user_data` 指向 `TestBlockingWakeState`；其余回调参数遵循 ABI。
///
/// Return zero after the release barrier and optional nested poll complete.
/// 释放屏障与可选嵌套轮询完成后返回零。
unsafe extern "C" fn block_managed_session_wake_during_engine_free(
    _engine_id: u64,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    if !error_out.is_null() {
        unsafe {
            *error_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
        }
    }
    if user_data.is_null() {
        return 1;
    }
    // Host-owned synchronization state valid until engine free has quiesced the callback.
    // 保持有效直至 engine free 已等待回调收敛的宿主拥有同步状态。
    let state = unsafe { &*(user_data.cast::<TestBlockingWakeState>()) };
    let _ = state.entered_tx.send(());
    state.release.wait();
    if state.should_poll.load(Ordering::Acquire) {
        // Nested poll expected to observe the already removed registry entry without blocking.
        // 预期无阻塞观察到注册表条目已移除的嵌套轮询。
        let mut result_out = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        let mut poll_error_out = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            luaskills_ffi_managed_session_events_poll(
                state.engine_id,
                1,
                &mut result_out,
                &mut poll_error_out,
            )
        };
        state.poll_status.store(status, Ordering::Release);
        unsafe { luaskills_ffi_buffer_free(result_out) };
        unsafe { luaskills_ffi_buffer_free(poll_error_out) };
    }
    0
}

/// Register one deterministic managed-session event source on a test engine.
/// 在测试引擎上注册一个确定性的受管会话事件来源。
///
/// `engine_id` selects the engine and `managed_session_id` identifies the reserved session slots.
/// `engine_id` 选择目标引擎，`managed_session_id` 标识预留的会话槽位。
///
/// Return the detached event center and the registered event token.
/// 返回分离式事件中心与已注册事件令牌。
fn register_test_managed_session_event_source(
    engine_id: u64,
    managed_session_id: u64,
) -> (Arc<ManagedSessionEventCenter>, ManagedSessionEventToken) {
    // Detached center acquired through the same short-lock path used by the FFI functions.
    // 通过与 FFI 函数相同的短锁路径获取的分离式事件中心。
    let event_center = super::clone_managed_session_event_center(engine_id)
        .expect("clone managed session event center");
    // Stable test identity whose values are asserted in serialized event batches.
    // 其各项值会在序列化事件批次中断言的稳定测试身份。
    let token =
        ManagedSessionEventToken::new("ffi-system-lease", "ffi-system-sid", 7, managed_session_id);
    event_center
        .register_session(token.clone())
        .expect("register managed session event token");
    (event_center, token)
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
        drop(super::remove_ffi_engine_slot(self.engine_id));
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

/// Verify both JSON and standard C ABI RunLua entrypoints preserve JSON container kinds and null values.
/// 验证 JSON 与标准 C ABI 两种 RunLua 出口均保留 JSON 容器类型及 null 值。
#[test]
fn ffi_run_lua_entrypoints_preserve_json_container_types_and_null() {
    // Global FFI registry guard preventing concurrent engine mutation during this test.
    // 防止测试期间并发修改全局 FFI 引擎注册表的保护器。
    let _guard = ffi_test_guard();
    // Registered production engine shared by the two FFI entrypoint variants.
    // 由两种 FFI 入口变体共享的已注册生产引擎。
    let engine = register_test_engine();
    // Lua source returning the protocol shape that originally regressed in System Plugin hooks.
    // 返回曾在 System Plugin Hook 中回归的协议结构的 Lua 源码。
    let code = r#"
return vulcan.json.decode([[{
  "environment": {},
  "binary_path_requests": [],
  "contributions": [],
  "optional": null,
  "nullable_items": [null]
}]])
"#;
    // Expected result shared by the high-level JSON wrapper and standard ABI.
    // 高层 JSON 包装与标准 ABI 共享的期望结果。
    let expected = serde_json::json!({
        "environment": {},
        "binary_path_requests": [],
        "contributions": [],
        "optional": null,
        "nullable_items": [null]
    });

    // High-level JSON FFI request encoded into one borrowed input buffer.
    // 编码到单个借用输入缓冲区的高层 JSON FFI 请求。
    let json_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "code": code,
            "args": {}
        })
        .to_string(),
    )
    .expect("encode JSON RunLua FFI request");
    // Decoded high-level JSON response envelope.
    // 已解码的高层 JSON 响应包络。
    let json_response = unsafe {
        decode_response_json(luaskills_ffi_run_lua_json(borrowed_json_buffer(
            &json_request,
        )))
    };
    assert_eq!(json_response["ok"], true);
    assert_eq!(json_response["result"], expected);

    // Standard ABI code and empty argument buffers retained for the complete call duration.
    // 在完整调用期间保持存活的标准 ABI 代码与空参数缓冲区。
    let standard_code = CString::new(code).expect("encode standard ABI RunLua code");
    let standard_args = CString::new("{}").expect("encode standard ABI RunLua args");
    // LuaSkills-owned result and error slots initialized to the required empty state.
    // 初始化为所需空状态的 LuaSkills 所有结果与错误槽。
    let mut result_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    // Standard C ABI status produced by the same engine conversion boundary.
    // 由同一引擎转换边界产生的标准 C ABI 状态。
    let status = unsafe {
        luaskills_ffi_run_lua(
            engine.engine_id,
            standard_code.as_ptr(),
            borrowed_json_buffer(&standard_args),
            ptr::null(),
            &mut result_out,
            &mut error_out,
        )
    };
    assert_eq!(status, 0);
    assert!(error_out.ptr.is_null());
    // Standard ABI JSON result decoded before releasing its LuaSkills-owned allocation.
    // 在释放 LuaSkills 所有分配前解码的标准 ABI JSON 结果。
    let standard_result: serde_json::Value =
        serde_json::from_str(&unsafe { take_owned_buffer_text(result_out) })
            .expect("decode standard ABI RunLua result");
    assert_eq!(standard_result, expected);
}

/// One registered FFI engine backed by a real strict System Plugin package layout.
/// 一个由真实严格 System Plugin 包布局支撑的已注册 FFI 引擎。
struct TestSystemFfiEngineHandle {
    /// Stable numeric FFI engine handle.
    /// 稳定数值 FFI 引擎句柄。
    engine_id: u64,
    /// Canonical runtime root configured on the engine.
    /// 配置到引擎上的规范运行时根目录。
    runtime_root: PathBuf,
    /// Canonical System Lua trust root configured on the engine.
    /// 配置到引擎上的规范 System Lua 信任根目录。
    system_lua_lib_dir: PathBuf,
    /// Stable System Plugin package identifier.
    /// 稳定的 System Plugin 包标识符。
    package_id: String,
    /// Canonical System Plugin package root.
    /// 规范 System Plugin 包根目录。
    package_root: PathBuf,
    /// Canonical dependency manifest file.
    /// 规范依赖清单文件。
    dependencies_file: PathBuf,
}

impl TestSystemFfiEngineHandle {
    /// Return the strict System package request object accepted by the high-level JSON FFI.
    /// 返回高层 JSON FFI 接受的严格 System 包请求对象。
    fn package_request_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.package_id.as_str(),
            "root": render_host_visible_path(&self.package_root),
            "dependencies_file": "dependencies.yaml"
        })
    }

    /// Return the canonical System package descriptor expected in create, status, and list responses.
    /// 返回 create、status 与 list 响应中预期的规范 System 包描述符。
    fn expected_package_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.package_id.as_str(),
            "root": render_host_visible_path(&self.package_root),
            "dependencies_file": render_host_visible_path(&self.dependencies_file)
        })
    }
}

impl Drop for TestSystemFfiEngineHandle {
    /// Remove the registered engine before deleting its real runtime layout.
    /// 在删除真实运行时布局前移除已注册引擎。
    fn drop(&mut self) {
        drop(super::remove_ffi_engine_slot(self.engine_id));
        let _ = std::fs::remove_dir_all(&self.runtime_root);
    }
}

/// Register one FFI engine with a real runtime root, System trust root, package, and manifest.
/// 注册一个具备真实运行时根、System 信任根、包与清单的 FFI 引擎。
///
/// The label parameter partitions temporary layouts created by independent tests.
/// label 参数用于隔离不同测试创建的临时布局。
///
/// Return the registered engine handle and its canonical package paths.
/// 返回已注册引擎句柄及其规范包路径。
fn register_system_test_engine(label: &str) -> TestSystemFfiEngineHandle {
    // Engine id allocated first so the temporary root remains unique within the current process.
    // 先分配引擎标识，确保临时根目录在当前进程内保持唯一。
    let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let runtime_root = std::env::temp_dir().join(format!(
        "luaskills-ffi-system-{label}-{}-{engine_id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&runtime_root);
    let system_lua_lib_dir = runtime_root.join("system_lua_lib");
    let package_id = "vulcan-debug".to_string();
    let package_root = system_lua_lib_dir.join(&package_id);
    let dependencies_file = package_root.join("dependencies.yaml");
    std::fs::create_dir_all(&package_root).expect("create strict System Plugin package root");
    std::fs::write(&dependencies_file, "{}\n")
        .expect("write strict System Plugin dependency manifest");
    // Canonical paths mirror the values returned by the trusted package context.
    // 规范路径与可信包上下文返回的值保持一致。
    let canonical_runtime_root =
        std::fs::canonicalize(&runtime_root).expect("canonicalize System FFI runtime root");
    let canonical_system_lua_lib_dir =
        std::fs::canonicalize(&system_lua_lib_dir).expect("canonicalize System FFI trust root");
    let canonical_package_root =
        std::fs::canonicalize(&package_root).expect("canonicalize System FFI package root");
    let canonical_dependencies_file = std::fs::canonicalize(&dependencies_file)
        .expect("canonicalize System FFI dependency manifest");
    let engine = LuaEngine::new(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        crate::LuaRuntimeHostOptions {
            runtime_root: Some(canonical_runtime_root.clone()),
            system_lua_lib_dir: Some(canonical_system_lua_lib_dir.clone()),
            ..Default::default()
        },
    ))
    .expect("create strict System FFI test engine");
    lock_ffi_engine_registry().insert(engine_id, FfiEngineSlot::new(engine));
    TestSystemFfiEngineHandle {
        engine_id,
        runtime_root: canonical_runtime_root,
        system_lua_lib_dir: canonical_system_lua_lib_dir,
        package_id,
        package_root: canonical_package_root,
        dependencies_file: canonical_dependencies_file,
    }
}

/// Invoke the strict System create JSON FFI and decode its owned response buffer.
/// 调用严格 System create JSON FFI 并解码其拥有型响应缓冲。
///
/// The request parameter is the complete high-level JSON request.
/// request 参数是完整的高层 JSON 请求。
///
/// Return the decoded stable FFI response envelope.
/// 返回解码后的稳定 FFI 响应包络。
fn invoke_system_create_json(request: serde_json::Value) -> serde_json::Value {
    let request = CString::new(request.to_string()).expect("encode strict System create request");
    unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_create_json(
            borrowed_json_buffer(&request),
        ))
    }
}

/// Standard C ABI signature shared by System lease create, eval, and close operations.
/// System 租约 create、eval 与 close 操作共享的标准 C ABI 签名。
type StandardSystemLeaseCall =
    unsafe extern "C" fn(u64, FfiBorrowedBuffer, *mut FfiOwnedBuffer, *mut FfiOwnedBuffer) -> i32;

/// Invoke one standard System lease ABI function and decode its direct JSON result.
/// 调用一个标准 System 租约 ABI 函数并解码其直接 JSON 结果。
///
/// `call` selects the ABI operation, `engine_id` identifies the registered engine, and `request`
/// is the strict operation body without high-level wrapper authority fields.
/// `call` 选择 ABI 操作，`engine_id` 标识已注册引擎，`request` 是不含高层包装 authority 字段的
/// 严格操作正文。
///
/// Returns the parsed result and fails the test after releasing any ABI-owned error buffer.
/// 返回解析后的结果；失败时会先释放 ABI 所有的错误缓冲再终止测试。
fn invoke_standard_system_lease_call(
    call: StandardSystemLeaseCall,
    engine_id: u64,
    request: serde_json::Value,
) -> serde_json::Value {
    let request = CString::new(request.to_string()).expect("encode standard System lease request");
    let mut result = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        call(
            engine_id,
            borrowed_json_buffer(&request),
            &mut result,
            &mut error,
        )
    };
    let error_text = unsafe { take_owned_buffer_text(error) };
    assert_eq!(status, 0, "standard System lease call failed: {error_text}");
    assert!(error_text.is_empty(), "unexpected ABI error: {error_text}");
    let result_text = unsafe { take_owned_buffer_text(result) };
    serde_json::from_str(&result_text).expect("standard System lease result must be JSON")
}

/// Run a real managed sidecar through standard C ABI create, cross-eval IO, and cleanup.
/// 通过标准 C ABI create、跨 eval IO 与清理运行真实受管 sidecar。
///
/// `runtime` selects the real Python or Node fixture prepared by the shared native test layout.
/// `runtime` 选择由共享原生测试布局准备的真实 Python 或 Node 夹具。
///
/// Returns only after the child protocol exits and both userdata and lease cleanup succeed.
/// 仅在子协议退出且 userdata 与租约清理均成功后返回。
fn run_standard_abi_managed_sidecar_integration(
    runtime: crate::runtime::engine::tests::ManagedSessionTestRuntime,
) {
    use crate::runtime::engine::tests::{
        ManagedSessionSystemLayout, discover_host_managed_runtime, managed_session_open_lua,
    };

    let Some(host) = discover_host_managed_runtime(runtime) else {
        return;
    };
    let runtime_label = match runtime {
        crate::runtime::engine::tests::ManagedSessionTestRuntime::Python => "python",
        crate::runtime::engine::tests::ManagedSessionTestRuntime::Node => "node",
    };
    let layout =
        ManagedSessionSystemLayout::new(&format!("ffi-standard-{runtime_label}"), runtime, &host);
    let engine = LuaEngine::new(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 30,
        },
        layout.host_options(),
    ))
    .expect("create standard ABI managed-session engine");
    let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    lock_ffi_engine_registry().insert(engine_id, FfiEngineSlot::new(engine));
    let _engine_handle = TestFfiEngineHandle { engine_id };

    let created = invoke_standard_system_lease_call(
        luaskills_ffi_system_runtime_lease_create,
        engine_id,
        layout.create_request(&format!("ffi-standard-{runtime_label}"), true),
    );
    assert_eq!(created["ok"], true);
    let lease_id = created["lease_id"].as_str().expect("standard lease id");
    let sid = created["sid"].as_str().expect("standard lease sid");
    let generation = created["generation"]
        .as_u64()
        .expect("standard lease generation");

    let opened = invoke_standard_system_lease_call(
        luaskills_ffi_system_runtime_lease_eval,
        engine_id,
        serde_json::json!({
            "lease_id": lease_id,
            "sid": sid,
            "generation": generation,
            "code": managed_session_open_lua(&layout, "ffi_sidecar", 64 * 1024, &[])
        }),
    );
    assert_eq!(opened["ok"], true);
    assert!(opened["result"]["status"]["managed_session_id"].is_number());

    let echoed = invoke_standard_system_lease_call(
        luaskills_ffi_system_runtime_lease_eval,
        engine_id,
        serde_json::json!({
            "lease_id": lease_id,
            "sid": sid,
            "generation": generation,
            "code": r#"ffi_sidecar:write('{"action":"echo","value":"ffi-standard"}\n'); return ffi_sidecar:read({ timeout_ms = 5000, max_bytes = 65536, until_text = 'ffi-standard' })"#
        }),
    );
    assert_eq!(echoed["ok"], true);
    assert!(
        echoed["result"]["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("ffi-standard"))
    );

    let exited = invoke_standard_system_lease_call(
        luaskills_ffi_system_runtime_lease_eval,
        engine_id,
        serde_json::json!({
            "lease_id": lease_id,
            "sid": sid,
            "generation": generation,
            "code": r#"ffi_sidecar:write('{"action":"exit"}\n'); local output = ffi_sidecar:read({ timeout_ms = 5000, max_bytes = 65536, until_text = '"event":"exit"' }); ffi_sidecar:close({ timeout_ms = 5000 }); return output"#
        }),
    );
    assert_eq!(exited["ok"], true);
    assert!(
        exited["result"]["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("\"event\":\"exit\""))
    );

    let closed = invoke_standard_system_lease_call(
        luaskills_ffi_system_runtime_lease_close,
        engine_id,
        serde_json::json!({
            "lease_id": lease_id,
            "sid": sid,
            "generation": generation
        }),
    );
    assert_eq!(closed["closed"], true);
}

/// Verify the standard C ABI reaches a real managed Python sidecar across lease evals.
/// 验证标准 C ABI 可跨租约 eval 到达真实受管 Python sidecar。
#[test]
fn ffi_standard_system_lease_executes_real_managed_python_sidecar() {
    let _guard = ffi_test_guard();
    run_standard_abi_managed_sidecar_integration(
        crate::runtime::engine::tests::ManagedSessionTestRuntime::Python,
    );
}

/// Verify the standard C ABI reaches a real managed Node sidecar across lease evals.
/// 验证标准 C ABI 可跨租约 eval 到达真实受管 Node sidecar。
#[test]
fn ffi_standard_system_lease_executes_real_managed_node_sidecar() {
    let _guard = ffi_test_guard();
    run_standard_abi_managed_sidecar_integration(
        crate::runtime::engine::tests::ManagedSessionTestRuntime::Node,
    );
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
    assert!(exported_names.contains(&"luaskills_ffi_managed_session_events_poll"));
    assert!(exported_names.contains(&"luaskills_ffi_managed_session_events_wait"));
    assert!(exported_names.contains(&"luaskills_ffi_set_managed_session_wake_callback"));
    assert!(exported_names.contains(&"luaskills_ffi_managed_session_events_poll_json"));
    assert!(exported_names.contains(&"luaskills_ffi_managed_session_events_wait_json"));
    assert!(
        exported_names
            .contains(&"luaskills_ffi_system_private_install_skill_from_url_manifest_json")
    );
    assert!(
        exported_names
            .contains(&"luaskills_ffi_system_private_update_skill_from_url_manifest_json")
    );
    assert!(exported_names.contains(&"luaskills_ffi_engine_new_v3"));
    assert!(exported_names.contains(&"luaskills_ffi_managed_runtime_resolve_json"));
}

/// Verify the read-only JSON FFI resolver returns a canonical managed runtime descriptor.
/// 验证只读 JSON FFI 解析器会返回规范受管运行时描述符。
#[test]
fn ffi_managed_runtime_resolve_json_returns_descriptor() {
    // DistributionRoot models one host-shared read-only application asset directory.
    // DistributionRoot 模拟宿主共享的只读应用资产目录。
    let root = std::env::temp_dir().join(format!(
        "luaskills_ffi_managed_runtime_resolve_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let distribution_root = root.join("application assets").join("runtimes");
    let install_root = distribution_root
        .join("node")
        .join("node-24.18.0-linux-x64");
    let executable = install_root.join("bin").join("node");
    std::fs::create_dir_all(executable.parent().expect("Node executable parent"))
        .expect("create JSON resolver install root");
    std::fs::write(&executable, b"node executable fixture")
        .expect("write JSON resolver executable");
    let manifest = serde_json::json!({
        "schema_version": 1,
        "runtime": "node",
        "version": "24.18.0",
        "platform": "linux-x64",
        "executable": "bin/node"
    });
    std::fs::write(
        install_root.join("runtime-manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("encode JSON resolver manifest"),
    )
    .expect("write JSON resolver manifest");
    // CanonicalDistributionRoot exercises the exact Windows verbatim spelling accepted by FFI.
    // CanonicalDistributionRoot 用于覆盖 FFI 接受的 Windows 逐字规范路径形式。
    let canonical_distribution_root =
        std::fs::canonicalize(&distribution_root).expect("canonical JSON resolver root");
    let request = CString::new(
        serde_json::json!({
            "distribution_root": canonical_distribution_root,
            "runtime": "node",
            "version": "24.18.0",
            "platform": "linux-x64"
        })
        .to_string(),
    )
    .expect("encode managed runtime resolver request");

    let response = unsafe {
        decode_response_json(luaskills_ffi_managed_runtime_resolve_json(
            borrowed_json_buffer(&request),
        ))
    };
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["runtime"], "node");
    assert_eq!(response["result"]["version"], "24.18.0");
    assert_eq!(response["result"]["platform"], "linux-x64");
    // CanonicalInstallRoot is retained internally while the JSON API returns host-visible spelling.
    // CanonicalInstallRoot 在内部保留，而 JSON API 返回宿主可见形式。
    let canonical_install_root =
        std::fs::canonicalize(&install_root).expect("canonical JSON resolver install root");
    assert_eq!(
        response["result"]["install_root"]
            .as_str()
            .expect("descriptor install_root string"),
        render_host_visible_path(&canonical_install_root)
    );
    #[cfg(windows)]
    assert!(
        canonical_distribution_root
            .to_string_lossy()
            .starts_with(r"\\?\")
    );
    #[cfg(windows)]
    assert!(
        !response["result"]["install_root"]
            .as_str()
            .expect("descriptor install_root string")
            .starts_with(r"\\?\")
    );
    #[cfg(windows)]
    assert!(
        !response["result"]["executable"]
            .as_str()
            .expect("descriptor executable string")
            .starts_with(r"\\?\")
    );
    assert_eq!(
        response["result"]["manifest_hash"].as_str().map(str::len),
        Some(64)
    );
    assert_eq!(
        response["result"]["executable_hash"].as_str().map(str::len),
        Some(64)
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Verify the managed-runtime resolver FFI rejects unsupported Windows verbatim namespaces.
/// 验证受管运行时解析器 FFI 会拒绝不受支持的 Windows verbatim 命名空间。
#[cfg(windows)]
#[test]
fn ffi_managed_runtime_resolve_json_rejects_unsupported_verbatim_namespace() {
    // Request containing a volume GUID namespace that cannot be exposed safely to Lua or JSON hosts.
    // 包含无法安全暴露给 Lua 或 JSON 宿主的卷 GUID 命名空间请求。
    let request = CString::new(
        serde_json::json!({
            "distribution_root": r"\\?\Volume{00000000-0000-0000-0000-000000000000}\runtimes",
            "runtime": "node",
            "version": "24.18.0",
            "platform": "windows-x64"
        })
        .to_string(),
    )
    .expect("encode unsupported managed runtime resolver request");
    // Stable FFI error envelope returned before any filesystem access.
    // 在任何文件系统访问前返回的稳定 FFI 错误包络。
    let response = unsafe {
        decode_response_json(luaskills_ffi_managed_runtime_resolve_json(
            borrowed_json_buffer(&request),
        ))
    };
    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .expect("resolver error string")
            .contains("unsupported Windows verbatim path namespace"),
        "unexpected resolver error: {response}"
    );
}

/// Verify JSON engine creation accepts independent managed distribution and environment roots.
/// 验证 JSON 引擎创建会接受独立受管发行根与环境根。
#[test]
fn ffi_engine_new_json_accepts_managed_runtime_roots() {
    let _guard = ffi_test_guard();
    // Root owns the distinct JSON-configured data, distribution, and environment boundaries.
    // Root 拥有 JSON 配置的独立数据、发行与环境边界。
    let root = std::env::temp_dir().join(format!(
        "luaskills_ffi_engine_managed_roots_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let runtime_root = root.join("runtime data");
    let distribution_root = root.join("application assets").join("runtimes");
    let environment_root = root.join("user data").join("managed envs");
    std::fs::create_dir_all(&runtime_root).expect("create JSON engine runtime root");
    std::fs::create_dir_all(&distribution_root).expect("create JSON engine distribution root");
    let mut host_options = LuaRuntimeHostOptions::with_runtime_root(&runtime_root);
    host_options.managed_runtime_distribution_root = Some(distribution_root.clone());
    host_options.managed_runtime_environment_root = Some(environment_root.clone());
    let request = EngineNewJsonRequest {
        options: LuaEngineOptions::new(
            LuaVmPoolConfig {
                min_size: 1,
                max_size: 1,
                idle_ttl_secs: 30,
            },
            host_options,
        ),
    };
    let request = CString::new(
        serde_json::to_string(&request).expect("encode JSON engine managed root request"),
    )
    .expect("build JSON engine managed root request");

    let response = unsafe {
        decode_response_json(luaskills_ffi_engine_new_json(borrowed_json_buffer(
            &request,
        )))
    };
    assert_eq!(response["ok"], true);
    let engine_id = response["result"]["engine_id"]
        .as_u64()
        .expect("JSON managed root engine id");
    assert!(environment_root.is_dir());
    let free_request = CString::new(serde_json::json!({ "engine_id": engine_id }).to_string())
        .expect("encode JSON managed root free request");
    let free_response = unsafe {
        decode_response_json(luaskills_ffi_engine_free_json(borrowed_json_buffer(
            &free_request,
        )))
    };
    assert_eq!(free_response["ok"], true);
    let _ = std::fs::remove_dir_all(root);
}

/// Verify strict JSON event requests preserve the bounded batch contract and explicit errors.
/// 验证严格 JSON 事件请求会保持有界批次契约与显式错误。
#[test]
fn ffi_managed_session_events_json_are_strict_bounded_and_explicit() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    // Registered event source used to publish two independently occupied logical slots.
    // 用于发布两个独立占用逻辑槽的已注册事件来源。
    let (event_center, token) = register_test_managed_session_event_source(engine.engine_id, 41);
    event_center
        .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
        .expect("publish stdout event");
    event_center
        .publish(&token, RuntimeManagedSessionEventKind::StderrReadable)
        .expect("publish stderr event");

    // Strict poll request whose single-event limit leaves one pending event.
    // 单事件上限会留下一个待处理事件的严格轮询请求。
    let poll_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "max_events": 1,
            "authority": SkillManagementAuthority::System
        })
        .to_string(),
    )
    .expect("managed session poll request");
    let polled = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_poll_json(
            borrowed_json_buffer(&poll_request),
        ))
    };
    assert_eq!(polled["ok"], true);
    assert_eq!(polled["result"]["remaining"], 1);
    assert_eq!(polled["result"]["timed_out"], false);
    assert_eq!(polled["result"]["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        polled["result"]["events"][0]["system_lease_id"],
        "ffi-system-lease"
    );
    assert_eq!(polled["result"]["events"][0]["sid"], "ffi-system-sid");
    assert_eq!(polled["result"]["events"][0]["generation"], 7);
    assert_eq!(polled["result"]["events"][0]["managed_session_id"], 41);
    assert_eq!(polled["result"]["events"][0]["kind"], "stdout_readable");
    assert_eq!(polled["result"]["events"][0]["sequence"], 1);

    // Zero-timeout wait that drains the remaining ready event without reporting timeout.
    // 以零超时排空剩余就绪事件且不报告超时的等待请求。
    let ready_wait_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "max_events": 4,
            "timeout_ms": 0,
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("managed session ready wait request");
    let ready_wait = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_wait_json(
            borrowed_json_buffer(&ready_wait_request),
        ))
    };
    assert_eq!(ready_wait["ok"], true);
    assert_eq!(ready_wait["result"]["remaining"], 0);
    assert_eq!(ready_wait["result"]["timed_out"], false);
    assert_eq!(ready_wait["result"]["events"][0]["kind"], "stderr_readable");
    assert_eq!(ready_wait["result"]["events"][0]["sequence"], 2);

    // Empty zero-timeout wait that must report an explicit timeout batch.
    // 必须报告显式超时批次的空队列零超时等待。
    let empty_wait = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_wait_json(
            borrowed_json_buffer(&ready_wait_request),
        ))
    };
    assert_eq!(empty_wait["ok"], true);
    assert_eq!(empty_wait["result"]["events"], serde_json::json!([]));
    assert_eq!(empty_wait["result"]["remaining"], 0);
    assert_eq!(empty_wait["result"]["timed_out"], true);

    // Request containing one unknown field to exercise deny_unknown_fields.
    // 包含一个未知字段以验证 deny_unknown_fields 的请求。
    let unknown_field_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "max_events": 1,
            "authority": SkillManagementAuthority::System,
            "unexpected": true
        })
        .to_string(),
    )
    .expect("managed session unknown-field request");
    let unknown_field_response = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_poll_json(
            borrowed_json_buffer(&unknown_field_request),
        ))
    };
    assert_eq!(unknown_field_response["ok"], false);
    assert!(
        unknown_field_response["error"]
            .as_str()
            .expect("unknown-field error")
            .contains("unknown field")
    );

    // Missing authority request rejected before event-center access.
    // 在访问事件中心前被拒绝的缺少权限请求。
    let missing_authority_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "max_events": 1
        })
        .to_string(),
    )
    .expect("managed session missing-authority request");
    let missing_authority_response = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_poll_json(
            borrowed_json_buffer(&missing_authority_request),
        ))
    };
    assert_eq!(missing_authority_response["ok"], false);
    assert!(
        missing_authority_response["error"]
            .as_str()
            .expect("missing-authority error")
            .contains("requires host-injected authority")
    );

    // Invalid authority spelling rejected by the strict enum decoder.
    // 由严格枚举解码器拒绝的非法权限拼写。
    let invalid_authority_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "max_events": 1,
            "authority": "invalid_authority"
        })
        .to_string(),
    )
    .expect("managed session invalid-authority request");
    let invalid_authority_response = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_poll_json(
            borrowed_json_buffer(&invalid_authority_request),
        ))
    };
    assert_eq!(invalid_authority_response["ok"], false);
    assert!(
        invalid_authority_response["error"]
            .as_str()
            .expect("invalid-authority error")
            .contains("invalid_authority")
    );

    // Closed empty center whose poll must return the stable explicit closure error.
    // 其轮询必须返回稳定显式关闭错误的已关闭空事件中心。
    event_center
        .close()
        .expect("close managed session event center");
    let closed_response = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_poll_json(
            borrowed_json_buffer(&poll_request),
        ))
    };
    assert_eq!(closed_response["ok"], false);
    assert!(
        closed_response["error"]
            .as_str()
            .expect("closed event-center error")
            .contains("managed session event center is closed")
    );
    // Closed wait request must fail explicitly rather than masquerading as a timeout.
    // 已关闭等待请求必须显式失败，而不能伪装成超时。
    let closed_wait_response = unsafe {
        decode_response_json(luaskills_ffi_managed_session_events_wait_json(
            borrowed_json_buffer(&ready_wait_request),
        ))
    };
    assert_eq!(closed_wait_response["ok"], false);
    assert!(
        closed_wait_response["error"]
            .as_str()
            .expect("closed wait event-center error")
            .contains("managed session event center is closed")
    );
}

/// Verify standard event polling, waiting, and per-engine wake callback ownership rules.
/// 验证标准事件轮询、等待与按引擎唤醒回调的所有权规则。
#[test]
fn ffi_managed_session_events_standard_abi_and_wake_callback_work() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    // Registered source and host-owned callback state kept alive through explicit clearing.
    // 通过显式清除保持存活的已注册来源与宿主拥有回调状态。
    let (event_center, token) = register_test_managed_session_event_source(engine.engine_id, 73);
    let callback_state = TestManagedSessionWakeState {
        callback_count: AtomicUsize::new(0),
        last_engine_id: AtomicU64::new(0),
    };
    let mut callback_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let set_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            Some(record_managed_session_wake),
            (&callback_state as *const TestManagedSessionWakeState)
                .cast_mut()
                .cast::<c_void>(),
            &mut callback_error,
        )
    };
    assert_eq!(set_status, 0);
    assert!(callback_error.ptr.is_null());

    event_center
        .publish(&token, RuntimeManagedSessionEventKind::Exited)
        .expect("publish exited event");
    let callback_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while callback_state.callback_count.load(Ordering::Acquire) < 1
        && std::time::Instant::now() < callback_deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(callback_state.callback_count.load(Ordering::Acquire), 1);
    assert_eq!(
        callback_state.last_engine_id.load(Ordering::Acquire),
        engine.engine_id
    );

    // Standard poll outputs direct batch JSON rather than the high-level JSON envelope.
    // 标准轮询输出直接批次 JSON，而不是高层 JSON 包络。
    let mut poll_result = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut poll_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let poll_status = unsafe {
        luaskills_ffi_managed_session_events_poll(
            engine.engine_id,
            1,
            &mut poll_result,
            &mut poll_error,
        )
    };
    assert_eq!(poll_status, 0);
    assert!(poll_error.ptr.is_null());
    let polled = unsafe { decode_response_json(poll_result) };
    assert_eq!(polled["events"][0]["kind"], "exited");
    assert_eq!(polled["remaining"], 0);
    assert_eq!(polled["timed_out"], false);

    // Quiescent callback clearing that authorizes immediate host-state release afterward.
    // 静默清除回调，使宿主状态可在返回后立即释放。
    let clear_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            None,
            ptr::null_mut(),
            &mut callback_error,
        )
    };
    assert_eq!(clear_status, 0);
    assert!(callback_error.ptr.is_null());
    event_center
        .publish(&token, RuntimeManagedSessionEventKind::Failed)
        .expect("publish failed event after callback clear");
    assert_eq!(callback_state.callback_count.load(Ordering::Acquire), 1);

    // Zero-timeout standard wait drains the ready event without timing out.
    // 零超时标准等待会排空就绪事件且不报告超时。
    let mut wait_result = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut wait_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let wait_status = unsafe {
        luaskills_ffi_managed_session_events_wait(
            engine.engine_id,
            4,
            0,
            &mut wait_result,
            &mut wait_error,
        )
    };
    assert_eq!(wait_status, 0);
    assert!(wait_error.ptr.is_null());
    let waited = unsafe { decode_response_json(wait_result) };
    assert_eq!(waited["events"][0]["kind"], "failed");
    assert_eq!(waited["timed_out"], false);

    // Invalid zero batch limit whose result slot stays empty and error is LuaSkills-owned.
    // 结果槽保持为空且错误由 LuaSkills 所有的非法零批次上限。
    let mut invalid_result = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut invalid_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let invalid_status = unsafe {
        luaskills_ffi_managed_session_events_poll(
            engine.engine_id,
            0,
            &mut invalid_result,
            &mut invalid_error,
        )
    };
    assert_eq!(invalid_status, 1);
    assert!(invalid_result.ptr.is_null());
    let invalid_error_text = unsafe { take_owned_buffer_text(invalid_error) };
    assert!(invalid_error_text.contains("max_events must be greater than 0"));
}

/// Verify a rejected standard wake request retries without another event publication.
/// 验证被拒绝的标准唤醒请求无需再次发布事件即可重试。
#[test]
fn ffi_managed_session_wake_callback_failure_retries_same_edge() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    let (event_center, token) = register_test_managed_session_event_source(engine.engine_id, 74);
    let callback_state = TestRetryingManagedSessionWakeState {
        callback_count: AtomicUsize::new(0),
    };
    let mut callback_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let set_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            Some(reject_first_managed_session_wake),
            (&callback_state as *const TestRetryingManagedSessionWakeState)
                .cast_mut()
                .cast::<c_void>(),
            &mut callback_error,
        )
    };
    assert_eq!(set_status, 0);
    unsafe { luaskills_ffi_buffer_free(callback_error) };

    event_center
        .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
        .expect("publish one retryable wake edge");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while callback_state.callback_count.load(Ordering::Acquire) < 2
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(callback_state.callback_count.load(Ordering::Acquire), 2);
    assert_eq!(
        event_center
            .poll(1)
            .expect("poll retried wake edge")
            .events
            .len(),
        1
    );

    let mut clear_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let clear_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            None,
            ptr::null_mut(),
            &mut clear_error,
        )
    };
    assert_eq!(clear_status, 0);
    unsafe { luaskills_ffi_buffer_free(clear_error) };
}

/// Verify a wake callback can poll through the detached event-center path while the engine is busy.
/// 验证引擎繁忙时唤醒回调仍可通过分离式事件中心路径执行轮询。
#[test]
fn ffi_managed_session_wake_callback_poll_bypasses_engine_mutex() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    // Registered source and stable host state used by the reentrant standard poll callback.
    // 嵌套标准轮询回调使用的已注册来源与稳定宿主状态。
    let (event_center, token) = register_test_managed_session_event_source(engine.engine_id, 121);
    let callback_state = Box::new(TestWakePollState {
        callback_count: AtomicUsize::new(0),
        poll_status: AtomicI32::new(-1),
    });
    let mut callback_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let set_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            Some(poll_managed_session_events_from_wake),
            (&*callback_state as *const TestWakePollState)
                .cast_mut()
                .cast::<c_void>(),
            &mut callback_error,
        )
    };
    assert_eq!(set_status, 0);
    assert!(callback_error.ptr.is_null());

    // Engine mutex deliberately retained while another thread publishes and runs the callback.
    // 在另一线程发布事件并运行回调期间故意保留的引擎互斥锁。
    let engine_handle = super::clone_engine_handle(engine.engine_id).expect("clone engine handle");
    let engine_guard = engine_handle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let publishing_center = Arc::clone(&event_center);
    let publishing_token = token.clone();
    let (publish_done_tx, publish_done_rx) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        let publish_result = publishing_center.publish(
            &publishing_token,
            RuntimeManagedSessionEventKind::StdoutReadable,
        );
        let _ = publish_done_tx.send(publish_result);
    });
    // Completion while the engine guard is held proves nested poll uses only the cached center.
    // 在持有引擎保护对象期间完成，可证明嵌套轮询仅使用缓存事件中心。
    let first_publish_result = publish_done_rx.recv_timeout(Duration::from_secs(2));
    // CallbackDeadline keeps the engine locked until the detached callback proves nested polling.
    // CallbackDeadline 会保持引擎锁，直到分离式回调证明嵌套轮询已经完成。
    let callback_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while callback_state.callback_count.load(Ordering::Acquire) == 0
        && std::time::Instant::now() < callback_deadline
    {
        std::thread::yield_now();
    }
    // CompletedWhileEngineLocked requires both publication and callback completion before unlock.
    // CompletedWhileEngineLocked 要求发布与回调均在解锁前完成。
    let completed_while_engine_locked =
        first_publish_result.is_ok() && callback_state.callback_count.load(Ordering::Acquire) == 1;
    drop(engine_guard);
    let publish_result = match first_publish_result {
        Ok(publish_result) => publish_result,
        Err(_) => publish_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("publisher should finish after engine mutex release"),
    };
    publisher.join().expect("join wake callback publisher");

    let clear_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            None,
            ptr::null_mut(),
            &mut callback_error,
        )
    };
    assert_eq!(clear_status, 0);
    assert!(callback_error.ptr.is_null());
    assert!(
        completed_while_engine_locked,
        "wake callback poll unexpectedly waited for the engine mutex"
    );
    assert_eq!(publish_result, Ok(true));
    assert_eq!(callback_state.callback_count.load(Ordering::Acquire), 1);
    assert_eq!(callback_state.poll_status.load(Ordering::Acquire), 0);
}

/// Verify engine free removes its slot before waiting for an in-flight wake callback.
/// 验证 engine free 会先移除槽，再等待在途唤醒回调。
#[test]
fn ffi_engine_free_releases_registry_before_callback_quiescence() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    // Registered event source whose callback remains in flight until the test releases it.
    // 其回调会保持在途直至测试释放的已注册事件来源。
    let (event_center, token) = register_test_managed_session_event_source(engine.engine_id, 122);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let callback_state = Box::new(TestBlockingWakeState {
        engine_id: engine.engine_id,
        entered_tx,
        release: Arc::clone(&release),
        should_poll: AtomicBool::new(false),
        poll_status: AtomicI32::new(-1),
    });
    let mut callback_error = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let set_status = unsafe {
        luaskills_ffi_set_managed_session_wake_callback(
            engine.engine_id,
            Some(block_managed_session_wake_during_engine_free),
            (&*callback_state as *const TestBlockingWakeState)
                .cast_mut()
                .cast::<c_void>(),
            &mut callback_error,
        )
    };
    assert_eq!(set_status, 0);
    assert!(callback_error.ptr.is_null());

    let publishing_center = Arc::clone(&event_center);
    let publishing_token = token.clone();
    let publisher = thread::spawn(move || {
        publishing_center.publish(
            &publishing_token,
            RuntimeManagedSessionEventKind::StderrReadable,
        )
    });
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("wake callback should enter before engine free");

    // Free thread consumes its owned error buffer before returning thread-safe values.
    // free 线程在返回线程安全值前消费其拥有型错误缓冲。
    let engine_id = engine.engine_id;
    let free_thread = thread::spawn(move || {
        let mut error_out = FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        let status = unsafe { luaskills_ffi_engine_free(engine_id, &mut error_out) };
        let error_text = unsafe { take_owned_buffer_text(error_out) };
        (status, error_text)
    });

    // Registry probe waits until removal is visible, but never blocks indefinitely on the mutex.
    // 注册表探测会等待移除可见，但绝不会在互斥锁上无限阻塞。
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut removed_while_callback_inflight = false;
    while Instant::now() < deadline {
        let entry_is_absent = match ffi_engine_registry().try_lock() {
            Ok(registry) => !registry.contains_key(&engine_id),
            Err(TryLockError::Poisoned(poisoned)) => {
                !poisoned.into_inner().contains_key(&engine_id)
            }
            Err(TryLockError::WouldBlock) => false,
        };
        if entry_is_absent {
            removed_while_callback_inflight = true;
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    // Nested callback poll runs only after lock release is proven, avoiding a hung regression test.
    // 仅在证明锁已释放后运行嵌套回调轮询，避免回归测试永久挂起。
    callback_state
        .should_poll
        .store(removed_while_callback_inflight, Ordering::Release);
    release.wait();

    let publish_result = publisher.join().expect("join blocking callback publisher");
    let (free_status, free_error_text) = free_thread.join().expect("join engine free thread");
    assert!(
        removed_while_callback_inflight,
        "engine free retained the registry lock while awaiting callback quiescence"
    );
    assert_eq!(publish_result, Ok(true));
    assert_eq!(
        free_status, 0,
        "unexpected engine free error: {free_error_text}"
    );
    assert!(free_error_text.is_empty());
    assert_eq!(
        callback_state.poll_status.load(Ordering::Acquire),
        1,
        "callback poll should observe the removed engine without blocking"
    );
}

/// Verify a blocking JSON wait retains neither the global registry lock nor the engine mutex.
/// 验证阻塞式 JSON 等待既不保留全局注册表锁，也不保留引擎互斥锁。
#[test]
fn ffi_managed_session_events_wait_releases_registry_and_engine_locks() {
    let _guard = ffi_test_guard();
    let engine = register_test_engine();
    // Event source published only after both lock checks complete.
    // 仅在两项锁检查完成后才发布的事件来源。
    let (event_center, token) = register_test_managed_session_event_source(engine.engine_id, 99);
    let engine_id = engine.engine_id;
    // Coordination channels proving the waiter remains blocked while locks are independently acquired.
    // 用于证明等待方保持阻塞且各锁仍可独立获取的协调通道。
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        let request = CString::new(
            serde_json::json!({
                "engine_id": engine_id,
                "max_events": 1,
                "timeout_ms": 2_000,
                "authority": SkillManagementAuthority::System
            })
            .to_string(),
        )
        .expect("blocking managed session wait request");
        started_tx.send(()).expect("signal wait start");
        let response = unsafe {
            decode_response_json(luaskills_ffi_managed_session_events_wait_json(
                borrowed_json_buffer(&request),
            ))
        };
        done_tx.send(response).expect("send wait response");
    });
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("wait thread should start");
    thread::sleep(Duration::from_millis(100));
    match done_rx.try_recv() {
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            panic!("event wait thread disconnected before lock verification")
        }
        Ok(response) => panic!("event wait returned before publication: {response}"),
    }

    // Registry guard acquired while the event wait is still pending.
    // 在事件等待仍处于挂起状态时获取的注册表保护对象。
    let registry_guard = match ffi_engine_registry().try_lock() {
        Ok(registry_guard) => registry_guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            panic!("blocking event wait must release registry lock")
        }
    };
    drop(registry_guard);
    // Engine guard acquired while the same event wait is still pending.
    // 在同一事件等待仍处于挂起状态时获取的引擎保护对象。
    let engine_handle = super::clone_engine_handle(engine_id).expect("clone engine handle");
    let engine_guard = match engine_handle.try_lock() {
        Ok(engine_guard) => engine_guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            panic!("blocking event wait must release engine lock")
        }
    };
    drop(engine_guard);

    event_center
        .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
        .expect("wake blocking event wait");
    let response = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("blocking event wait should return after publish");
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["timed_out"], false);
    assert_eq!(response["result"]["events"][0]["managed_session_id"], 99);
    waiter.join().expect("join blocking event waiter");
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
    let engine = register_system_test_engine("authority");
    let created = invoke_system_create_json(serde_json::json!({
        "engine_id": engine.engine_id,
        "sid": "ffi-system-session",
        "ttl_sec": 60,
        "system_package": engine.package_request_json()
    }));
    assert_eq!(created["ok"], false);
    assert!(
        created["error"]
            .as_str()
            .expect("system runtime session error")
            .contains("requires host-injected authority")
    );
}

/// Verify strict System create rejects a request without the required package descriptor.
/// 验证严格 System create 会拒绝缺少必需包描述符的请求。
#[test]
fn ffi_system_runtime_session_json_requires_system_package() {
    let _guard = ffi_test_guard();
    let engine = register_system_test_engine("missing-package");
    let created = invoke_system_create_json(serde_json::json!({
        "engine_id": engine.engine_id,
        "sid": "ffi-system-missing-package",
        "ttl_sec": 60,
        "authority": SkillManagementAuthority::System
    }));

    assert_eq!(created["ok"], false);
    let error = created["error"]
        .as_str()
        .expect("missing System package error");
    assert!(error.contains("missing field"), "unexpected error: {error}");
    assert!(
        error.contains("system_package"),
        "unexpected error: {error}"
    );
}

/// Verify strict System create rejects an incomplete package descriptor.
/// 验证严格 System create 会拒绝字段不完整的包描述符。
#[test]
fn ffi_system_runtime_session_json_rejects_missing_package_field() {
    let _guard = ffi_test_guard();
    let engine = register_system_test_engine("missing-package-field");
    let created = invoke_system_create_json(serde_json::json!({
        "engine_id": engine.engine_id,
        "sid": "ffi-system-missing-package-field",
        "ttl_sec": 60,
        "system_package": {
            "id": engine.package_id.as_str(),
            "root": render_host_visible_path(&engine.package_root)
        },
        "authority": SkillManagementAuthority::System
    }));

    assert_eq!(created["ok"], false);
    let error = created["error"]
        .as_str()
        .expect("missing System package field error");
    assert!(error.contains("missing field"), "unexpected error: {error}");
    assert!(
        error.contains("dependencies_file"),
        "unexpected error: {error}"
    );
}

/// Verify strict System create rejects public lua_roots and c_roots injection fields.
/// 验证严格 System create 会拒绝公开 lua_roots 与 c_roots 注入字段。
#[test]
fn ffi_system_runtime_session_json_rejects_public_module_roots() {
    let _guard = ffi_test_guard();
    let engine = register_system_test_engine("module-roots");
    let created = invoke_system_create_json(serde_json::json!({
        "engine_id": engine.engine_id,
        "sid": "ffi-system-module-roots",
        "ttl_sec": 60,
        "lua_roots": [render_host_visible_path(&engine.runtime_root)],
        "c_roots": [render_host_visible_path(&engine.runtime_root)],
        "system_package": engine.package_request_json(),
        "authority": SkillManagementAuthority::System
    }));

    assert_eq!(created["ok"], false);
    let error = created["error"]
        .as_str()
        .expect("unknown System module-root field error");
    assert!(error.contains("unknown field"), "unexpected error: {error}");
    assert!(
        error.contains("lua_roots") || error.contains("c_roots"),
        "unexpected error: {error}"
    );
}

/// Verify strict System create rejects a real package root outside the configured trust root.
/// 验证严格 System create 会拒绝位于已配置信任根外的真实包根目录。
#[test]
fn ffi_system_runtime_session_json_rejects_package_root_escape() {
    let _guard = ffi_test_guard();
    let engine = register_system_test_engine("package-root-escape");
    // Real external package proves containment is checked after canonicalization.
    // 真实外部包用于证明包含关系会在规范化后检查。
    let outside_package_root = engine.runtime_root.join("outside-system-package");
    std::fs::create_dir_all(&outside_package_root).expect("create outside System package root");
    std::fs::write(outside_package_root.join("dependencies.yaml"), "{}\n")
        .expect("write outside System package manifest");
    let created = invoke_system_create_json(serde_json::json!({
        "engine_id": engine.engine_id,
        "sid": "ffi-system-package-root-escape",
        "ttl_sec": 60,
        "system_package": {
            "id": "outside-system-package",
            "root": render_host_visible_path(&outside_package_root),
            "dependencies_file": "dependencies.yaml"
        },
        "authority": SkillManagementAuthority::System
    }));

    assert_eq!(created["ok"], false);
    let error = created["error"]
        .as_str()
        .expect("System package root containment error");
    assert!(
        error.contains("must be a strict descendant of system_lua_lib root"),
        "unexpected error: {error}"
    );
    assert!(
        !outside_package_root.starts_with(&engine.system_lua_lib_dir),
        "escape fixture must remain outside the trust root"
    );
}

/// Verify system runtime-session JSON FFI accepts delegated authority and preserves VM state.
/// 验证 system 运行时会话 JSON FFI 接受 delegated authority 并保留 VM 状态。
#[test]
fn ffi_system_runtime_session_json_supports_delegated_wrapper_flow() {
    let _guard = ffi_test_guard();
    let engine = register_system_test_engine("delegated-flow");
    let create_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "sid": "ffi-system-wrapper-session",
            "ttl_sec": 60,
            "replace": true,
            "system_package": engine.package_request_json(),
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
    assert_eq!(
        created["result"]["system_package"],
        engine.expected_package_json()
    );
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

    let status_request = CString::new(
        serde_json::json!({
            "engine_id": engine.engine_id,
            "lease_id": created["result"]["lease_id"],
            "sid": created["result"]["sid"],
            "generation": created["result"]["generation"],
            "authority": SkillManagementAuthority::DelegatedTool
        })
        .to_string(),
    )
    .expect("system status request");
    let status = unsafe {
        decode_response_json(luaskills_ffi_system_runtime_lease_status_json(
            borrowed_json_buffer(&status_request),
        ))
    };
    assert_eq!(status["ok"], true);
    assert_eq!(
        status["result"]["system_package"],
        engine.expected_package_json()
    );

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
    assert_eq!(
        listed["result"]["leases"][0]["system_package"],
        engine.expected_package_json()
    );

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

/// Write one enabled package with a declared sensitive string configuration for FFI tests.
/// 为 FFI 测试写入一个声明了敏感字符串配置的启用技能包。
fn write_config_test_skill(skill_root: &Path, skill_id: &str) -> PathBuf {
    let skill_dir = skill_root.join(skill_id);
    std::fs::create_dir_all(skill_dir.join("runtime")).expect("create config runtime dir");
    std::fs::write(
        skill_dir.join("skill.yaml"),
        format!(
            "name: {skill_id}\nversion: 0.1.0\nenable: true\ndebug: false\nconfig:\n  - key: api_token\n    type: string\n    required: true\n    sensitive: true\n    description: Service access token.\n    constraints:\n      min_length: 1\n      max_length: 4096\n  - key: retries\n    type: integer\n    description: Retry count.\n    constraints:\n      minimum: 0\n      maximum: 10\nentries:\n  - name: ping\n    description: Config ping entry.\n    lua_entry: runtime/ping.lua\n    lua_module: {skill_id}.ping\n"
        ),
    )
    .expect("write config skill yaml");
    std::fs::write(
        skill_dir.join("runtime").join("ping.lua"),
        "return function(args)\n  return vulcan.config.get('api_token')\nend\n",
    )
    .expect("write config runtime entry");
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

    drop(super::remove_ffi_engine_slot(engine_id));
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
                managed_runtime_distribution_root: None,
                managed_runtime_environment_root: None,
                managed_runtime_config: Default::default(),
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
                skill_config_root: None,
                skill_config_lock_timeout_ms: None,
                skill_config_watch_debounce_ms: None,
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

/// Verify JSON engine creation rejects an invalid managed-runtime policy with its stable field name.
/// 验证 JSON 引擎创建会使用稳定字段名拒绝非法受管运行时策略。
#[test]
fn ffi_engine_new_rejects_invalid_managed_runtime_config() {
    let _guard = ffi_test_guard();
    // HostOptions carries one invalid zero persistent-session capacity through serde.
    // HostOptions 通过 serde 携带一个非法的零持久会话容量。
    let mut host_options = crate::LuaRuntimeHostOptions::default();
    host_options
        .managed_runtime_config
        .persistent_session_limit_per_engine = 0;
    // Request uses the real JSON FFI engine-construction envelope.
    // Request 使用真实 JSON FFI 引擎构造包络。
    let request = EngineNewJsonRequest {
        options: LuaEngineOptions::new(
            LuaVmPoolConfig {
                min_size: 1,
                max_size: 1,
                idle_ttl_secs: 30,
            },
            host_options,
        ),
    };
    // Input retains the serialized request bytes for the complete borrowed call.
    // Input 在完整借用调用期间保留序列化请求字节。
    let input = CString::new(serde_json::to_string(&request).expect("request json"))
        .expect("request cstring");
    // Response is the structured error returned before engine resource allocation.
    // Response 是引擎资源分配前返回的结构化错误。
    let response = unsafe {
        decode_response_json(luaskills_ffi_engine_new_json(borrowed_json_buffer(&input)))
    };

    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("persistent_session_limit_per_engine"))
    );
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
                skill_config_root: Some(temp_root.join("config")),
                skill_config_lock_timeout_ms: None,
                skill_config_watch_debounce_ms: None,
                ..Default::default()
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
    let skill_root = temp_root.join("skills");
    write_config_test_skill(&skill_root, "demo-skill");
    with_engine_mut(result.engine_id, |engine| {
        engine
            .load_from_roots(&[RuntimeSkillRoot {
                name: "ROOT".to_string(),
                skills_dir: skill_root,
            }])
            .map_err(|error| error.to_string())
    })
    .expect("load declared config package into JSON FFI engine");

    let set_request = CString::new(
        serde_json::to_string(&SkillConfigSetJsonRequest {
            engine_id: result.engine_id,
            skill_id: "demo-skill".to_string(),
            values: BTreeMap::from([
                (
                    "api_token".to_string(),
                    SkillPackageConfigInputValue::String("sk-json-ffi".to_string()),
                ),
                (
                    "retries".to_string(),
                    SkillPackageConfigInputValue::Integer(3),
                ),
            ]),
            expected_revision: Some("0".to_string()),
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
    assert_eq!(set_response["result"]["revision"], "1");
    assert_eq!(set_response["result"]["changed"], true);
    assert_eq!(set_response["result"]["values"]["api_token"], "sk-json-ffi");
    assert_eq!(set_response["result"]["values"]["retries"], "3");

    let describe_hidden_request = CString::new(
        serde_json::to_string(&SkillPackageConfigDescribeJsonRequest {
            engine_id: result.engine_id,
            skill_id: Some("demo-skill".to_string()),
            include_values: false,
            mode: SkillPackageConfigDescribeMode::Effective,
            root_name: None,
        })
        .expect("hidden describe request json"),
    )
    .expect("hidden describe request cstring");
    let describe_hidden_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_describe_json(
            borrowed_json_buffer(&describe_hidden_request),
        ))
    };
    assert_eq!(describe_hidden_response["ok"], true);
    assert_eq!(
        describe_hidden_response["result"][0]["items"][0]["description"],
        "Service access token."
    );
    assert!(
        describe_hidden_response["result"][0]["items"][0]
            .get("value")
            .is_none()
    );

    let describe_visible_request = CString::new(
        serde_json::to_string(&SkillPackageConfigDescribeJsonRequest {
            engine_id: result.engine_id,
            skill_id: Some("demo-skill".to_string()),
            include_values: true,
            mode: SkillPackageConfigDescribeMode::Effective,
            root_name: None,
        })
        .expect("visible describe request json"),
    )
    .expect("visible describe request cstring");
    let describe_visible_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_describe_json(
            borrowed_json_buffer(&describe_visible_request),
        ))
    };
    assert_eq!(describe_visible_response["ok"], true);
    assert_eq!(
        describe_visible_response["result"][0]["items"][0]["value"],
        "sk-json-ffi"
    );

    let validate_request = CString::new(
        serde_json::to_string(&SkillPackageConfigValidateJsonRequest {
            engine_id: result.engine_id,
            skill_id: "demo-skill".to_string(),
        })
        .expect("validate request json"),
    )
    .expect("validate request cstring");
    let validate_response = unsafe {
        decode_response_json(luaskills_ffi_skill_config_validate_json(
            borrowed_json_buffer(&validate_request),
        ))
    };
    assert_eq!(validate_response["ok"], true);
    assert_eq!(validate_response["result"]["complete"], true);

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
    assert_eq!(list_response["result"].as_array().map(Vec::len), Some(2));
    assert_eq!(list_response["result"][0]["store_scope"], "system-skills");
    assert_eq!(list_response["result"][0]["skill_id"], "demo-skill");
    assert_eq!(list_response["result"][0]["key"], "api_token");
    assert_eq!(list_response["result"][0]["value"], "sk-json-ffi");
    assert_eq!(list_response["result"][1]["key"], "retries");
    assert_eq!(list_response["result"][1]["value"], "3");

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
