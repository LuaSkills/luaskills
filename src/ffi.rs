use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CString, c_char};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::ffi_standard::{FfiBorrowedBuffer, FfiOwnedBuffer};
use crate::runtime_help::{RuntimeHelpDetail, RuntimeSkillHelpDescriptor};

use crate::{
    LuaEngine, RuntimeEntryDescriptor, RuntimeInvocationResult, SkillApplyResult,
    SkillInstallRequest, SkillInstallSourceType, SkillManagementAuthority, SkillUninstallResult,
};

mod requests;

use self::requests::*;

/// Stable FFI protocol version derived from the crate package version.
/// 从 crate 包版本派生出的稳定 FFI 协议版本。
pub(crate) const FFI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One stable JSON response envelope returned by every LuaSkills FFI entrypoint.
/// 每个 LuaSkills FFI 入口统一返回的稳定 JSON 响应包络。
#[derive(Debug, Serialize)]
struct FfiJsonEnvelope<T: Serialize> {
    /// Whether the requested operation completed successfully.
    /// 当前请求操作是否执行成功。
    ok: bool,
    /// Structured result payload when the operation succeeds.
    /// 操作成功时返回的结构化结果载荷。
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    /// Human-readable English error message when the operation fails.
    /// 操作失败时返回的人类可读英文错误消息。
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// One engine registry entry stored behind one stable numeric FFI handle id.
/// 通过稳定数值 FFI 句柄标识存放的单个引擎注册表条目。
pub(crate) struct FfiEngineSlot {
    /// Independently locked runtime engine instance owned by the current FFI handle.
    /// 由当前 FFI 句柄拥有并独立加锁的运行时引擎实例。
    pub(crate) engine: Arc<Mutex<LuaEngine>>,
}

impl FfiEngineSlot {
    /// Wrap one runtime engine into one independently locked shared FFI handle slot.
    /// 将单个运行时引擎封装为一个可独立加锁的共享 FFI 句柄槽位。
    pub(crate) fn new(engine: LuaEngine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
        }
    }
}

pub(crate) static FFI_ENGINE_REGISTRY: OnceLock<Mutex<HashMap<u64, FfiEngineSlot>>> =
    OnceLock::new();
pub(crate) static FFI_ENGINE_COUNTER: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Active engine ids currently executing on the calling thread.
    /// 当前调用线程上正在执行中的引擎标识列表。
    static ACTIVE_FFI_ENGINE_IDS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// One thread-local engine activity guard used to reject same-thread reentrant access.
/// 用于拒绝同线程重入访问的线程局部引擎活动守卫。
struct ActiveFfiEngineGuard {
    engine_id: u64,
}

impl ActiveFfiEngineGuard {
    /// Enter one engine activity scope on the current thread.
    /// 在当前线程上进入单个引擎活动作用域。
    fn enter(engine_id: u64) -> Result<Self, String> {
        ACTIVE_FFI_ENGINE_IDS.with(|active_ids| {
            let mut active_ids = active_ids.borrow_mut();
            if active_ids.contains(&engine_id) {
                return Err(format!(
                    "FFI engine {} reentrant access is not allowed on the same thread",
                    engine_id
                ));
            }
            active_ids.push(engine_id);
            Ok(Self { engine_id })
        })
    }
}

impl Drop for ActiveFfiEngineGuard {
    fn drop(&mut self) {
        ACTIVE_FFI_ENGINE_IDS.with(|active_ids| {
            let mut active_ids = active_ids.borrow_mut();
            if let Some(position) = active_ids
                .iter()
                .rposition(|active| *active == self.engine_id)
            {
                active_ids.remove(position);
            }
        });
    }
}

/// Return the default empty JSON object payload.
/// 返回默认的空 JSON 对象载荷。
fn default_json_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Return the default persistent runtime session eval timeout in milliseconds.
/// 返回默认的持久运行时会话执行超时时长（毫秒）。
fn default_ffi_runtime_session_timeout_ms() -> u64 {
    60_000
}

/// Parse one runtime session JSON payload returned by the engine.
/// 解析引擎返回的单个运行时会话 JSON 载荷。
fn parse_runtime_session_engine_payload(
    payload: String,
    function_name: &str,
) -> Result<Value, String> {
    serde_json::from_str(&payload)
        .map_err(|error| format!("{function_name} received invalid engine JSON: {error}"))
}

/// Require one host-injected authority value for an authority-gated JSON FFI entrypoint.
/// 为单个受权限保护的 JSON FFI 入口要求宿主注入权限值。
fn require_json_authority(
    authority: Option<SkillManagementAuthority>,
    function_name: &str,
) -> Result<SkillManagementAuthority, String> {
    authority.ok_or_else(|| {
        format!(
            "{} requires host-injected authority: use 'system' or 'delegated_tool'",
            function_name
        )
    })
}

/// Require full system authority for one host-private JSON FFI entrypoint.
/// 为单个宿主私有 JSON FFI 入口要求完整 system 权限。
fn require_system_json_authority(
    authority: Option<SkillManagementAuthority>,
    function_name: &str,
) -> Result<SkillManagementAuthority, String> {
    let authority = require_json_authority(authority, function_name)?;
    if authority == SkillManagementAuthority::System {
        return Ok(authority);
    }
    Err(format!(
        "{} requires system authority for private URL manifest operations",
        function_name
    ))
}

/// Return the global engine registry used by the FFI layer.
/// 返回 FFI 层使用的全局引擎注册表。
pub(crate) fn ffi_engine_registry() -> &'static Mutex<HashMap<u64, FfiEngineSlot>> {
    FFI_ENGINE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Acquire the global FFI engine registry and return its guard, recovering after registry lock poisoning.
/// 获取并返回全局 FFI 引擎注册表保护对象；如果注册表锁已 poison，则恢复继续使用。
pub(crate) fn lock_ffi_engine_registry() -> MutexGuard<'static, HashMap<u64, FfiEngineSlot>> {
    ffi_engine_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Clone one shared engine handle out of the global registry without holding the registry lock during execution.
/// 从全局注册表中克隆一个共享引擎句柄，并确保执行期间不再持有注册表锁。
fn clone_engine_handle(engine_id: u64) -> Result<Arc<Mutex<LuaEngine>>, String> {
    let registry = lock_ffi_engine_registry();
    registry
        .get(&engine_id)
        .map(|slot| Arc::clone(&slot.engine))
        .ok_or_else(|| format!("FFI engine {} not found", engine_id))
}

/// Acquire one registered FFI engine handle and return its guard, recovering after engine lock poisoning.
/// 获取并返回单个已注册 FFI 引擎句柄的保护对象；如果引擎锁已 poison，则恢复继续使用。
fn lock_engine_handle(engine_handle: &Arc<Mutex<LuaEngine>>) -> MutexGuard<'_, LuaEngine> {
    engine_handle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Convert one owned byte slice into one LuaSkills-owned FFI buffer container.
/// 将一段拥有型字节切片转换为一个由 LuaSkills 管理的 FFI 缓冲容器。
fn owned_buffer_from_bytes(bytes: &[u8]) -> FfiOwnedBuffer {
    if bytes.is_empty() {
        return FfiOwnedBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
    }
    let mut owned = bytes.to_vec();
    let pointer = owned.as_mut_ptr();
    let len = owned.len();
    std::mem::forget(owned);
    FfiOwnedBuffer { ptr: pointer, len }
}

/// Convert one Rust value into one LuaSkills-owned UTF-8 JSON response buffer.
/// 将单个 Rust 值转换为一个由 LuaSkills 管理的 UTF-8 JSON 响应缓冲。
fn encode_json_buffer<T: Serialize>(value: &T) -> FfiOwnedBuffer {
    let json_text = match serde_json::to_string(value) {
        Ok(json_text) => json_text,
        Err(error) => encode_json_serialization_error_text(error),
    };
    owned_buffer_from_bytes(json_text.as_bytes())
}

/// Build escaped fallback JSON text for a response serialization failure.
/// 为响应序列化失败构造已转义的兜底 JSON 文本。
///
/// The error parameter is the serializer error raised while encoding the original response.
/// error 参数是编码原始响应时产生的序列化错误。
///
/// Return a valid JSON error envelope string that can be sent over the FFI boundary.
/// 返回可通过 FFI 边界发送的合法 JSON 错误包络字符串。
fn encode_json_serialization_error_text(error: serde_json::Error) -> String {
    json!({
        "ok": false,
        "error": format!("Failed to serialize FFI response: {error}")
    })
    .to_string()
}

/// Build one successful FFI JSON envelope.
/// 构造一个成功的 FFI JSON 响应包络。
fn ffi_ok<T: Serialize>(result: T) -> FfiOwnedBuffer {
    encode_json_buffer(&FfiJsonEnvelope {
        ok: true,
        result: Some(result),
        error: None::<String>,
    })
}

/// Build one failed FFI JSON envelope.
/// 构造一个失败的 FFI JSON 响应包络。
fn ffi_error(message: impl Into<String>) -> FfiOwnedBuffer {
    encode_json_buffer(&FfiJsonEnvelope::<Value> {
        ok: false,
        result: None,
        error: Some(message.into()),
    })
}

/// Parse one UTF-8 JSON request from one foreign string pointer.
/// 从外部字符串指针解析一段 UTF-8 JSON 请求。
fn decode_json_request<T: DeserializeOwned>(
    input_json: FfiBorrowedBuffer,
    function_name: &str,
) -> Result<T, String> {
    if input_json.ptr.is_null() {
        if input_json.len == 0 {
            return Err(format!(
                "{} requires one non-null JSON buffer",
                function_name
            ));
        }
        return Err(format!(
            "{} received null JSON buffer with non-zero len",
            function_name
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(input_json.ptr, input_json.len) };
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("{} received invalid UTF-8 input: {}", function_name, error))?;
    serde_json::from_str(text)
        .map_err(|error| format!("{} received invalid JSON input: {}", function_name, error))
}

/// Execute one read-only engine operation by engine id.
/// 按引擎标识执行一次只读引擎操作。
pub(crate) fn with_engine<T, F>(engine_id: u64, operation: F) -> Result<T, String>
where
    F: FnOnce(&LuaEngine) -> Result<T, String>,
{
    let engine_handle = clone_engine_handle(engine_id)?;
    let _active_guard = ActiveFfiEngineGuard::enter(engine_id)?;
    let engine = lock_engine_handle(&engine_handle);
    operation(&engine)
}

/// Execute one mutable engine operation by engine id.
/// 按引擎标识执行一次可变引擎操作。
pub(crate) fn with_engine_mut<T, F>(engine_id: u64, operation: F) -> Result<T, String>
where
    F: FnOnce(&mut LuaEngine) -> Result<T, String>,
{
    let engine_handle = clone_engine_handle(engine_id)?;
    let _active_guard = ActiveFfiEngineGuard::enter(engine_id)?;
    let mut engine = lock_engine_handle(&engine_handle);
    operation(&mut engine)
}

/// Return one stable list of all exported FFI entrypoints.
/// 返回一份稳定的全部已导出 FFI 入口点列表。
pub(crate) fn exported_ffi_function_names() -> Vec<String> {
    vec![
        "luaskills_ffi_version",
        "luaskills_ffi_describe",
        "luaskills_ffi_engine_new",
        "luaskills_ffi_engine_new_v2",
        "luaskills_ffi_engine_free",
        "luaskills_ffi_load_from_roots",
        "luaskills_ffi_reload_from_roots",
        "luaskills_ffi_list_entries",
        "luaskills_ffi_list_skill_help",
        "luaskills_ffi_render_skill_help_detail",
        "luaskills_ffi_prompt_argument_completions",
        "luaskills_ffi_is_skill",
        "luaskills_ffi_skill_name_for_tool",
        "luaskills_ffi_skill_config_list",
        "luaskills_ffi_skill_config_get",
        "luaskills_ffi_skill_config_set",
        "luaskills_ffi_skill_config_delete",
        "luaskills_ffi_call_skill",
        "luaskills_ffi_run_lua",
        "luaskills_ffi_disable_skill",
        "luaskills_ffi_system_disable_skill",
        "luaskills_ffi_enable_skill",
        "luaskills_ffi_system_enable_skill",
        "luaskills_ffi_uninstall_skill",
        "luaskills_ffi_system_uninstall_skill",
        "luaskills_ffi_install_skill",
        "luaskills_ffi_system_install_skill",
        "luaskills_ffi_update_skill",
        "luaskills_ffi_system_update_skill",
        "luaskills_ffi_set_sqlite_provider_callback",
        "luaskills_ffi_set_lancedb_provider_callback",
        "luaskills_ffi_set_sqlite_provider_json_callback",
        "luaskills_ffi_set_lancedb_provider_json_callback",
        "luaskills_ffi_set_host_tool_json_callback",
        "luaskills_ffi_set_skill_operation_progress_json_callback",
        "luaskills_ffi_set_model_embed_json_callback",
        "luaskills_ffi_set_model_llm_json_callback",
        "luaskills_ffi_string_clone",
        "luaskills_ffi_version_json",
        "luaskills_ffi_describe_json",
        "luaskills_ffi_engine_new_json",
        "luaskills_ffi_engine_free_json",
        "luaskills_ffi_load_from_roots_json",
        "luaskills_ffi_reload_from_roots_json",
        "luaskills_ffi_list_entries_json",
        "luaskills_ffi_list_skill_help_json",
        "luaskills_ffi_render_skill_help_detail_json",
        "luaskills_ffi_prompt_argument_completions_json",
        "luaskills_ffi_is_skill_json",
        "luaskills_ffi_skill_name_for_tool_json",
        "luaskills_ffi_skill_config_list_json",
        "luaskills_ffi_skill_config_get_json",
        "luaskills_ffi_skill_config_set_json",
        "luaskills_ffi_skill_config_delete_json",
        "luaskills_ffi_call_skill_json",
        "luaskills_ffi_run_lua_json",
        "luaskills_ffi_runtime_lease_create_json",
        "luaskills_ffi_runtime_lease_eval_json",
        "luaskills_ffi_runtime_lease_status_json",
        "luaskills_ffi_runtime_lease_list_json",
        "luaskills_ffi_runtime_lease_close_json",
        "luaskills_ffi_system_runtime_lease_create_json",
        "luaskills_ffi_system_runtime_lease_eval_json",
        "luaskills_ffi_system_runtime_lease_status_json",
        "luaskills_ffi_system_runtime_lease_list_json",
        "luaskills_ffi_system_runtime_lease_close_json",
        "luaskills_ffi_disable_skill_json",
        "luaskills_ffi_system_disable_skill_json",
        "luaskills_ffi_enable_skill_json",
        "luaskills_ffi_system_enable_skill_json",
        "luaskills_ffi_uninstall_skill_json",
        "luaskills_ffi_system_uninstall_skill_json",
        "luaskills_ffi_install_skill_json",
        "luaskills_ffi_system_install_skill_json",
        "luaskills_ffi_system_private_install_skill_from_url_manifest_json",
        "luaskills_ffi_update_skill_json",
        "luaskills_ffi_system_update_skill_json",
        "luaskills_ffi_system_private_update_skill_from_url_manifest_json",
        "luaskills_ffi_string_free",
        "luaskills_ffi_bytes_clone",
        "luaskills_ffi_bytes_free",
        "luaskills_ffi_buffer_clone",
        "luaskills_ffi_buffer_free",
        "luaskills_ffi_string_array_free",
        "luaskills_ffi_entry_list_free",
        "luaskills_ffi_help_list_free",
        "luaskills_ffi_help_detail_free",
        "luaskills_ffi_invocation_result_free",
        "luaskills_ffi_skill_apply_result_free",
        "luaskills_ffi_skill_uninstall_result_free",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Free one heap-allocated JSON string returned by the FFI layer.
/// 释放一段由 FFI 层返回并在堆上分配的 JSON 字符串。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_string_free(value: *mut c_char) {
    if !value.is_null() {
        let _ = unsafe { CString::from_raw(value) };
    }
}

/// Return the stable FFI version descriptor as one JSON envelope.
/// 以 JSON 响应包络形式返回稳定的 FFI 版本描述。
#[unsafe(no_mangle)]
pub extern "C" fn luaskills_ffi_version_json() -> FfiOwnedBuffer {
    ffi_ok(json!({
        "ffi_version": FFI_VERSION,
        "protocol": "json-cabi"
    }))
}

/// Return the exported JSON FFI entrypoint list as one JSON envelope.
/// 以 JSON 响应包络形式返回已导出的 JSON FFI 入口列表。
#[unsafe(no_mangle)]
pub extern "C" fn luaskills_ffi_describe_json() -> FfiOwnedBuffer {
    ffi_ok(FfiDescribeJsonResult {
        ffi_version: FFI_VERSION.to_string(),
        exported_functions: exported_ffi_function_names(),
    })
}

/// Create one new LuaSkills engine instance and return its stable FFI handle id.
/// 创建一个新的 LuaSkills 引擎实例，并返回其稳定的 FFI 句柄标识。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_engine_new_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EngineNewJsonRequest>(
        input_json,
        "luaskills_ffi_engine_new_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match LuaEngine::new(request.options) {
        Ok(engine) => {
            let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut registry = lock_ffi_engine_registry();
            registry.insert(engine_id, FfiEngineSlot::new(engine));
            ffi_ok(EngineHandleJsonResult { engine_id })
        }
        Err(error) => ffi_error(error.to_string()),
    }
}

/// Free one existing LuaSkills engine handle.
/// 释放一个现有的 LuaSkills 引擎句柄。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_engine_free_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EngineIdJsonRequest>(
        input_json,
        "luaskills_ffi_engine_free_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let mut registry = lock_ffi_engine_registry();
    if registry.remove(&request.engine_id).is_none() {
        return ffi_error(format!("FFI engine {} not found", request.engine_id));
    }
    ffi_ok(json!({ "freed": true }))
}

/// Load skills from one ordered root chain through the JSON FFI surface.
/// 通过 JSON FFI 入口按有序根链加载技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_load_from_roots_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EngineRootsJsonRequest>(
        input_json,
        "luaskills_ffi_load_from_roots_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine
            .load_from_roots(&request.skill_roots)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok(json!({ "loaded": true })),
        Err(error) => ffi_error(error),
    }
}

/// Reload skills from one ordered root chain through the JSON FFI surface.
/// 通过 JSON FFI 入口按有序根链重载技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_reload_from_roots_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EngineRootsJsonRequest>(
        input_json,
        "luaskills_ffi_reload_from_roots_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine
            .reload_from_roots(&request.skill_roots)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok(json!({ "reloaded": true })),
        Err(error) => ffi_error(error),
    }
}

/// List runtime entry descriptors visible to one host-injected authority through the JSON FFI surface.
/// 通过 JSON FFI 入口列出单个宿主注入权限可见的运行时入口描述。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_list_entries_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EngineAuthorityJsonRequest>(
        input_json,
        "luaskills_ffi_list_entries_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority =
        match require_json_authority(request.authority, "luaskills_ffi_list_entries_json") {
            Ok(authority) => authority,
            Err(error) => return ffi_error(error),
        };
    match with_engine(request.engine_id, |engine| {
        engine.list_entries_for_authority(authority)
    }) {
        Ok(result) => ffi_ok::<Vec<RuntimeEntryDescriptor>>(result),
        Err(error) => ffi_error(error),
    }
}

/// List structured help trees visible to one host-injected authority through the JSON FFI surface.
/// 通过 JSON FFI 入口列出单个宿主注入权限可见的结构化帮助树。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_list_skill_help_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EngineAuthorityJsonRequest>(
        input_json,
        "luaskills_ffi_list_skill_help_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority =
        match require_json_authority(request.authority, "luaskills_ffi_list_skill_help_json") {
            Ok(authority) => authority,
            Err(error) => return ffi_error(error),
        };
    match with_engine(request.engine_id, |engine| {
        engine.list_skill_help_for_authority(authority)
    }) {
        Ok(result) => ffi_ok::<Vec<RuntimeSkillHelpDescriptor>>(result),
        Err(error) => ffi_error(error),
    }
}

/// Render one structured help detail payload visible to one host-injected authority through the JSON FFI surface.
/// 通过 JSON FFI 入口渲染单个宿主注入权限可见的结构化帮助详情载荷。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_render_skill_help_detail_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RenderHelpJsonRequest>(
        input_json,
        "luaskills_ffi_render_skill_help_detail_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_render_skill_help_detail_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        engine.render_skill_help_detail_for_authority(
            authority,
            &request.skill_id,
            &request.flow_name,
            request.request_context.as_ref(),
        )
    }) {
        Ok(result) => ffi_ok::<Option<RuntimeHelpDetail>>(result),
        Err(error) => ffi_error(error),
    }
}

/// Resolve prompt argument completion candidates through the JSON FFI surface.
/// 通过 JSON FFI 入口解析提示词参数补全候选项。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_prompt_argument_completions_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<PromptCompletionJsonRequest>(
        input_json,
        "luaskills_ffi_prompt_argument_completions_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_prompt_argument_completions_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        Ok(engine.prompt_argument_completions_for_authority(
            authority,
            &request.prompt_name,
            &request.argument_name,
        ))
    }) {
        Ok(result) => ffi_ok::<Option<Vec<String>>>(result),
        Err(error) => ffi_error(error),
    }
}

/// Check whether one canonical tool name belongs to one visible Lua skill entry.
/// 检查某个 canonical 工具名是否属于一个可见 Lua 技能入口。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_is_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<IsSkillJsonRequest>(
        input_json,
        "luaskills_ffi_is_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_json_authority(request.authority, "luaskills_ffi_is_skill_json") {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        engine.is_skill_for_authority(authority, &request.tool_name)
    }) {
        Ok(value) => ffi_ok(BoolJsonResult { value }),
        Err(error) => ffi_error(error),
    }
}

/// Resolve the visible owning skill id of one canonical tool name through the JSON FFI surface.
/// 通过 JSON FFI 入口解析某个 canonical 工具名可见的所属技能标识符。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_name_for_tool_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<SkillNameForToolJsonRequest>(
        input_json,
        "luaskills_ffi_skill_name_for_tool_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority =
        match require_json_authority(request.authority, "luaskills_ffi_skill_name_for_tool_json") {
            Ok(authority) => authority,
            Err(error) => return ffi_error(error),
        };
    match with_engine(request.engine_id, |engine| {
        engine.skill_name_for_tool_for_authority(authority, &request.tool_name)
    }) {
        Ok(skill_id) => ffi_ok(OptionalSkillNameJsonResult { skill_id }),
        Err(error) => ffi_error(error),
    }
}

/// List flattened skill config records through the JSON FFI surface.
/// 通过 JSON FFI 入口列出扁平化技能配置记录。
/// Skill config is addressed by skill id and is intentionally outside root visibility filtering.
/// skill 配置按 skill id 寻址，并有意不进入 root 可见性过滤。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_list_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<SkillConfigListJsonRequest>(
        input_json,
        "luaskills_ffi_skill_config_list_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        engine.list_skill_config_entries(request.skill_id.as_deref())
    }) {
        Ok(entries) => ffi_ok(entries),
        Err(error) => ffi_error(error),
    }
}

/// Read one optional skill config value through the JSON FFI surface.
/// 通过 JSON FFI 入口读取单个可选技能配置值。
/// Skill config only affects behavior when Lua skill code reads it explicitly.
/// skill 配置只有在 Lua skill 代码显式读取时才影响行为。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_get_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<SkillConfigGetJsonRequest>(
        input_json,
        "luaskills_ffi_skill_config_get_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        engine.get_skill_config_value(&request.skill_id, &request.key)
    }) {
        Ok(value) => ffi_ok(SkillConfigGetJsonResult {
            found: value.is_some(),
            skill_id: request.skill_id,
            key: request.key,
            value,
        }),
        Err(error) => ffi_error(error),
    }
}

/// Insert or replace one skill config value through the JSON FFI surface.
/// 通过 JSON FFI 入口插入或替换单个技能配置值。
/// Hosts that do not want user-level config mutation should not expose this endpoint.
/// 不希望用户级修改配置的宿主不应暴露该入口。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_set_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<SkillConfigSetJsonRequest>(
        input_json,
        "luaskills_ffi_skill_config_set_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine.set_skill_config_value(&request.skill_id, &request.key, &request.value)
    }) {
        Ok(()) => ffi_ok(SkillConfigMutationJsonResult {
            action: "set".to_string(),
            skill_id: request.skill_id,
            key: request.key,
            value: Some(request.value),
            deleted: None,
        }),
        Err(error) => ffi_error(error),
    }
}

/// Delete one skill config key through the JSON FFI surface.
/// 通过 JSON FFI 入口删除单个技能配置键。
/// Hosts that do not want user-level config mutation should not expose this endpoint.
/// 不希望用户级修改配置的宿主不应暴露该入口。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_delete_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<SkillConfigGetJsonRequest>(
        input_json,
        "luaskills_ffi_skill_config_delete_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine.delete_skill_config_value(&request.skill_id, &request.key)
    }) {
        Ok(deleted) => ffi_ok(SkillConfigMutationJsonResult {
            action: "delete".to_string(),
            skill_id: request.skill_id,
            key: request.key,
            value: None,
            deleted: Some(deleted),
        }),
        Err(error) => ffi_error(error),
    }
}

/// Call one loaded skill entry through the JSON FFI surface.
/// 通过 JSON FFI 入口调用单个已加载技能入口。
/// Calls target the active runtime execution surface and do not apply root visibility filtering.
/// 调用面向当前已激活运行时执行面，不应用 root 可见性过滤。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_call_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<CallSkillJsonRequest>(
        input_json,
        "luaskills_ffi_call_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        engine.call_skill(
            &request.tool_name,
            &request.args,
            request.invocation_context.as_ref(),
        )
    }) {
        Ok(result) => ffi_ok::<RuntimeInvocationResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Execute arbitrary Lua code through the JSON FFI surface.
/// 通过 JSON FFI 入口执行任意 Lua 代码。
/// Hosts should wrap or hide this endpoint when arbitrary Lua execution is not intended.
/// 不希望开放任意 Lua 执行的宿主应封装或隐藏该入口。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_run_lua_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request =
        match decode_json_request::<RunLuaJsonRequest>(input_json, "luaskills_ffi_run_lua_json") {
            Ok(request) => request,
            Err(error) => return ffi_error(error),
        };
    match with_engine(request.engine_id, |engine| {
        engine.run_lua(
            &request.code,
            &request.args,
            request.invocation_context.as_ref(),
        )
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Create one persistent public runtime lease through the JSON FFI surface.
/// 通过 JSON FFI 入口创建单个公开持久运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_create_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionCreateJsonRequest>(
        input_json,
        "luaskills_ffi_runtime_lease_create_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({
            "sid": request.sid,
            "ttl_sec": request.ttl_sec,
            "replace": request.replace,
            "cwd": request.cwd,
            "workspace_root": request.workspace_root,
            "lua_roots": request.lua_roots,
            "c_roots": request.c_roots,
            "mounts": request.mounts
        });
        let response = engine.create_runtime_lease_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(response, "luaskills_ffi_runtime_lease_create_json")
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Evaluate code inside one persistent public runtime lease through the JSON FFI surface.
/// 通过 JSON FFI 入口在单个公开持久运行时租约中执行代码。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_eval_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionEvalJsonRequest>(
        input_json,
        "luaskills_ffi_runtime_lease_eval_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let request_context = request
            .invocation_context
            .as_ref()
            .and_then(|context| context.request_context.clone());
        let client_budget = request
            .invocation_context
            .as_ref()
            .map(|context| context.client_budget.clone())
            .unwrap_or_else(default_json_object);
        let tool_config = request
            .invocation_context
            .as_ref()
            .map(|context| context.tool_config.clone())
            .unwrap_or_else(default_json_object);
        let payload = json!({
            "lease_id": request.lease_id,
            "sid": request.sid,
            "generation": request.generation,
            "code": request.code,
            "args": request.args,
            "timeout_ms": request.timeout_ms,
            "request_context": request_context,
            "client_budget": client_budget,
            "tool_config": tool_config
        });
        let response = engine.eval_runtime_lease_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(response, "luaskills_ffi_runtime_lease_eval_json")
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Return one persistent public runtime lease status through the JSON FFI surface.
/// 通过 JSON FFI 入口返回单个公开持久运行时租约状态。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_status_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionLeaseJsonRequest>(
        input_json,
        "luaskills_ffi_runtime_lease_status_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({
            "lease_id": request.lease_id,
            "sid": request.sid,
            "generation": request.generation
        });
        let response = engine.runtime_lease_status_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(response, "luaskills_ffi_runtime_lease_status_json")
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// List active persistent public runtime leases through the JSON FFI surface.
/// 通过 JSON FFI 入口列出活跃公开持久运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_list_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionListJsonRequest>(
        input_json,
        "luaskills_ffi_runtime_lease_list_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({ "sid": request.sid });
        let response = engine.list_runtime_leases_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(response, "luaskills_ffi_runtime_lease_list_json")
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Close one persistent public runtime lease through the JSON FFI surface.
/// 通过 JSON FFI 入口关闭单个公开持久运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_close_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionLeaseJsonRequest>(
        input_json,
        "luaskills_ffi_runtime_lease_close_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({
            "lease_id": request.lease_id,
            "sid": request.sid,
            "generation": request.generation
        });
        let response = engine.close_runtime_lease_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(response, "luaskills_ffi_runtime_lease_close_json")
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Create one persistent `system_lua_lib` runtime lease through the system JSON FFI surface.
/// 通过 system JSON FFI 入口创建单个持久 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_create_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionCreateJsonRequest>(
        input_json,
        "luaskills_ffi_system_runtime_lease_create_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let _authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_runtime_lease_create_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({
            "sid": request.sid,
            "ttl_sec": request.ttl_sec,
            "replace": request.replace,
            "cwd": request.cwd,
            "workspace_root": request.workspace_root,
            "lua_roots": request.lua_roots,
            "c_roots": request.c_roots,
            "mounts": request.mounts
        });
        let response = engine.create_system_runtime_lease_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(
            response,
            "luaskills_ffi_system_runtime_lease_create_json",
        )
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Evaluate code inside one persistent `system_lua_lib` runtime lease through the system JSON FFI surface.
/// 通过 system JSON FFI 入口在单个持久 `system_lua_lib` 运行时租约中执行代码。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_eval_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionEvalJsonRequest>(
        input_json,
        "luaskills_ffi_system_runtime_lease_eval_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let _authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_runtime_lease_eval_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let request_context = request
            .invocation_context
            .as_ref()
            .and_then(|context| context.request_context.clone());
        let client_budget = request
            .invocation_context
            .as_ref()
            .map(|context| context.client_budget.clone())
            .unwrap_or_else(default_json_object);
        let tool_config = request
            .invocation_context
            .as_ref()
            .map(|context| context.tool_config.clone())
            .unwrap_or_else(default_json_object);
        let payload = json!({
            "lease_id": request.lease_id,
            "sid": request.sid,
            "generation": request.generation,
            "code": request.code,
            "args": request.args,
            "timeout_ms": request.timeout_ms,
            "request_context": request_context,
            "client_budget": client_budget,
            "tool_config": tool_config
        });
        let response = engine.eval_system_runtime_lease_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(
            response,
            "luaskills_ffi_system_runtime_lease_eval_json",
        )
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Return one persistent `system_lua_lib` runtime lease status through the system JSON FFI surface.
/// 通过 system JSON FFI 入口返回单个持久 `system_lua_lib` 运行时租约状态。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_status_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionLeaseJsonRequest>(
        input_json,
        "luaskills_ffi_system_runtime_lease_status_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let _authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_runtime_lease_status_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({
            "lease_id": request.lease_id,
            "sid": request.sid,
            "generation": request.generation
        });
        let response = engine.system_runtime_lease_status_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(
            response,
            "luaskills_ffi_system_runtime_lease_status_json",
        )
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// List active persistent `system_lua_lib` runtime leases through the system JSON FFI surface.
/// 通过 system JSON FFI 入口列出活跃持久 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_list_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionListJsonRequest>(
        input_json,
        "luaskills_ffi_system_runtime_lease_list_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let _authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_runtime_lease_list_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({ "sid": request.sid });
        let response = engine.list_system_runtime_leases_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(
            response,
            "luaskills_ffi_system_runtime_lease_list_json",
        )
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Close one persistent `system_lua_lib` runtime lease through the system JSON FFI surface.
/// 通过 system JSON FFI 入口关闭单个持久 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_close_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<RuntimeSessionLeaseJsonRequest>(
        input_json,
        "luaskills_ffi_system_runtime_lease_close_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let _authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_runtime_lease_close_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine(request.engine_id, |engine| {
        let payload = json!({
            "lease_id": request.lease_id,
            "sid": request.sid,
            "generation": request.generation
        });
        let response = engine.close_system_runtime_lease_json(&payload.to_string())?;
        parse_runtime_session_engine_payload(
            response,
            "luaskills_ffi_system_runtime_lease_close_json",
        )
    }) {
        Ok(result) => ffi_ok::<Value>(result),
        Err(error) => ffi_error(error),
    }
}

/// Disable one skill through the ordinary skills plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在普通 skills 平面停用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_disable_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<DisableSkillJsonRequest>(
        input_json,
        "luaskills_ffi_disable_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine
            .disable_skill_in_roots(
                &request.skill_roots,
                &request.skill_id,
                request.reason.as_deref(),
            )
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok(json!({ "disabled": true })),
        Err(error) => ffi_error(error),
    }
}

/// Disable one skill through the system plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在 system 平面停用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_disable_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<DisableSkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_disable_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_disable_skill_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine
            .system_disable_skill_in_roots(
                &request.skill_roots,
                authority,
                &request.skill_id,
                request.reason.as_deref(),
            )
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok(json!({ "disabled": true })),
        Err(error) => ffi_error(error),
    }
}

/// Enable one skill through the ordinary skills plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在普通 skills 平面启用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_enable_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EnableSkillJsonRequest>(
        input_json,
        "luaskills_ffi_enable_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        engine
            .enable_skill(&request.skill_roots, &request.skill_id)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok(json!({ "enabled": true })),
        Err(error) => ffi_error(error),
    }
}

/// Enable one skill through the system plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在 system 平面启用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_enable_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<EnableSkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_enable_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority =
        match require_json_authority(request.authority, "luaskills_ffi_system_enable_skill_json") {
            Ok(authority) => authority,
            Err(error) => return ffi_error(error),
        };
    match with_engine_mut(request.engine_id, |engine| {
        engine
            .system_enable_skill(&request.skill_roots, authority, &request.skill_id)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok(json!({ "enabled": true })),
        Err(error) => ffi_error(error),
    }
}

/// Uninstall one skill through the ordinary skills plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在普通 skills 平面卸载单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_uninstall_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<UninstallSkillJsonRequest>(
        input_json,
        "luaskills_ffi_uninstall_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .uninstall_skill_in_root(
                    &request.skill_roots,
                    target_root,
                    &request.skill_id,
                    &request.options,
                )
                .map_err(|error| error.to_string())
        } else {
            engine
                .uninstall_skill(&request.skill_roots, &request.skill_id, &request.options)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillUninstallResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Uninstall one skill through the system plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在 system 平面卸载单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_uninstall_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<UninstallSkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_uninstall_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_uninstall_skill_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .system_uninstall_skill_in_root(
                    &request.skill_roots,
                    target_root,
                    authority,
                    &request.skill_id,
                    &request.options,
                )
                .map_err(|error| error.to_string())
        } else {
            engine
                .system_uninstall_skill(
                    &request.skill_roots,
                    authority,
                    &request.skill_id,
                    &request.options,
                )
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillUninstallResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Install one managed skill through the ordinary skills plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在普通 skills 平面安装单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_install_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<ApplySkillJsonRequest>(
        input_json,
        "luaskills_ffi_install_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .install_skill_in_root(&request.skill_roots, target_root, &request.request)
                .map_err(|error| error.to_string())
        } else {
            engine
                .install_skill(&request.skill_roots, &request.request)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillApplyResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Install one managed skill through the system plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在 system 平面安装单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_install_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<ApplySkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_install_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_json_authority(
        request.authority,
        "luaskills_ffi_system_install_skill_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .system_install_skill_in_root(
                    &request.skill_roots,
                    target_root,
                    authority,
                    &request.request,
                )
                .map_err(|error| error.to_string())
        } else {
            engine
                .system_install_skill(&request.skill_roots, authority, &request.request)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillApplyResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Install one private URL-manifest skill through a host-private system JSON FFI surface.
/// 通过宿主私有 system JSON FFI 入口安装单个私有 URL manifest 技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_private_install_skill_from_url_manifest_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<PrivateUrlManifestSkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_private_install_skill_from_url_manifest_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_system_json_authority(
        request.authority,
        "luaskills_ffi_system_private_install_skill_from_url_manifest_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    let install_request = SkillInstallRequest {
        skill_id: Some(request.skill_id),
        source: Some(request.manifest_url),
        source_type: SkillInstallSourceType::PrivateUrlManifest,
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .system_install_skill_in_root(
                    &request.skill_roots,
                    target_root,
                    authority,
                    &install_request,
                )
                .map_err(|error| error.to_string())
        } else {
            engine
                .system_install_skill(&request.skill_roots, authority, &install_request)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillApplyResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Update one managed skill through the ordinary skills plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在普通 skills 平面更新单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_update_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<ApplySkillJsonRequest>(
        input_json,
        "luaskills_ffi_update_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .update_skill_in_root(&request.skill_roots, target_root, &request.request)
                .map_err(|error| error.to_string())
        } else {
            engine
                .update_skill(&request.skill_roots, &request.request)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillApplyResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Update one managed skill through the system plane via the JSON FFI surface.
/// 通过 JSON FFI 入口在 system 平面更新单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_update_skill_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<ApplySkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_update_skill_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority =
        match require_json_authority(request.authority, "luaskills_ffi_system_update_skill_json") {
            Ok(authority) => authority,
            Err(error) => return ffi_error(error),
        };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .system_update_skill_in_root(
                    &request.skill_roots,
                    target_root,
                    authority,
                    &request.request,
                )
                .map_err(|error| error.to_string())
        } else {
            engine
                .system_update_skill(&request.skill_roots, authority, &request.request)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillApplyResult>(result),
        Err(error) => ffi_error(error),
    }
}

/// Update one private URL-manifest skill through a host-private system JSON FFI surface.
/// 通过宿主私有 system JSON FFI 入口更新单个私有 URL manifest 技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_private_update_skill_from_url_manifest_json(
    input_json: FfiBorrowedBuffer,
) -> FfiOwnedBuffer {
    let request = match decode_json_request::<PrivateUrlManifestSkillJsonRequest>(
        input_json,
        "luaskills_ffi_system_private_update_skill_from_url_manifest_json",
    ) {
        Ok(request) => request,
        Err(error) => return ffi_error(error),
    };
    let authority = match require_system_json_authority(
        request.authority,
        "luaskills_ffi_system_private_update_skill_from_url_manifest_json",
    ) {
        Ok(authority) => authority,
        Err(error) => return ffi_error(error),
    };
    let update_request = SkillInstallRequest {
        skill_id: Some(request.skill_id),
        source: Some(request.manifest_url),
        source_type: SkillInstallSourceType::PrivateUrlManifest,
    };
    match with_engine_mut(request.engine_id, |engine| {
        if let Some(target_root) = request.target_root.as_ref() {
            engine
                .system_update_skill_in_root(
                    &request.skill_roots,
                    target_root,
                    authority,
                    &update_request,
                )
                .map_err(|error| error.to_string())
        } else {
            engine
                .system_update_skill(&request.skill_roots, authority, &update_request)
                .map_err(|error| error.to_string())
        }
    }) {
        Ok(result) => ffi_ok::<SkillApplyResult>(result),
        Err(error) => ffi_error(error),
    }
}

#[cfg(test)]
mod tests;
