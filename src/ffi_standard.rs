use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde_json::Value;

use crate::ffi::{
    FFI_ENGINE_COUNTER, clone_managed_session_event_center, destroy_removed_ffi_engine_slot,
    lock_ffi_engine_registry, remove_ffi_engine_slot, with_engine, with_engine_mut,
};
use crate::host::callbacks::{
    RuntimeHostToolCallback, RuntimeHostToolRequest, RuntimeModelEmbedCallback,
    RuntimeModelEmbedRequest, RuntimeModelEmbedResponse, RuntimeModelError, RuntimeModelErrorCode,
    RuntimeModelLlmCallback, RuntimeModelLlmRequest, RuntimeModelLlmResponse,
    RuntimeSkillOperationProgressCallback, RuntimeSkillOperationProgressEvent,
    set_host_tool_callback, set_model_embed_callback, set_model_llm_callback,
    set_skill_operation_progress_callback,
};
use crate::host::database::{
    LuaRuntimeDatabaseCallbackMode, LuaRuntimeDatabaseProviderMode, RuntimeDatabaseBindingContext,
    RuntimeDatabaseKind, RuntimeLanceDbProviderAction, RuntimeLanceDbProviderCallback,
    RuntimeLanceDbProviderRequest, RuntimeLanceDbProviderResult, RuntimeSqliteProviderAction,
    RuntimeSqliteProviderCallback, RuntimeSqliteProviderRequest, set_lancedb_provider_callback,
    set_lancedb_provider_json_callback, set_sqlite_provider_callback,
    set_sqlite_provider_json_callback,
};
use crate::runtime::config_tool::deserialize_unique_config_values;
use crate::runtime::logging;
use crate::runtime::managed_session_events::{
    FallibleRuntimeManagedSessionWakeCallback, RuntimeManagedSessionEventBatch,
};
use crate::runtime_context::RuntimeRequestContext;
use crate::runtime_help::{
    RuntimeHelpDetail, RuntimeHelpNodeDescriptor, RuntimeSkillHelpDescriptor,
};
use crate::runtime_options::{
    LuaInvocationContext, LuaRuntimeCapabilityOptions, LuaRuntimeHostOptions,
    LuaRuntimeManagedRuntimeConfig, LuaRuntimeRunLuaPoolConfig, LuaRuntimeSpaceControllerOptions,
    LuaRuntimeSpaceControllerProcessMode, RuntimeSkillRoot,
};
use crate::runtime_result::RuntimeHostResult;
use crate::skill::manager::{SkillInstallRequest, SkillManagementAuthority, SkillUninstallOptions};
use crate::skill::source::SkillInstallSourceType;
use crate::tool_cache::ToolCacheConfig;
use crate::{
    LuaEngine, LuaEngineOptions, LuaVmPoolConfig, RuntimeEntryDescriptor,
    RuntimeEntryParameterDescriptor, RuntimeInvocationResult, SkillApplyResult,
    SkillPackageConfigInputValue, SkillUninstallResult,
};

const FFI_STATUS_OK: i32 = 0;
const FFI_STATUS_ERROR: i32 = 1;
const FFI_SOURCE_TYPE_ABSENT: i32 = -1;
const FFI_SOURCE_TYPE_GITHUB: i32 = 0;

/// Strict standard-ABI batch wrapper that rejects duplicate JSON object keys.
/// 拒绝重复 JSON 对象键的严格标准 ABI 批次封装。
#[derive(Deserialize)]
#[serde(transparent)]
struct StrictSkillConfigInputValues(
    #[serde(deserialize_with = "deserialize_unique_config_values")]
    BTreeMap<String, SkillPackageConfigInputValue>,
);
const FFI_SOURCE_TYPE_URL: i32 = 1;
const FFI_SOURCE_TYPE_OFFICIAL_HUB: i32 = 2;
const FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST: i32 = 3;
const FFI_PROVIDER_MODE_DYNAMIC_LIBRARY: i32 = 0;
const FFI_PROVIDER_MODE_HOST_CALLBACK: i32 = 1;
const FFI_PROVIDER_MODE_SPACE_CONTROLLER: i32 = 2;
const FFI_CALLBACK_MODE_STANDARD: i32 = 0;
const FFI_CALLBACK_MODE_JSON: i32 = 1;
const FFI_SPACE_CONTROLLER_PROCESS_MODE_SERVICE: i32 = 0;
const FFI_SPACE_CONTROLLER_PROCESS_MODE_MANAGED: i32 = 1;
const FFI_DATABASE_KIND_SQLITE: i32 = 0;
const FFI_DATABASE_KIND_LANCEDB: i32 = 1;
const FFI_SQLITE_PROVIDER_ACTION_EXECUTE_SCRIPT: i32 = 0;
const FFI_SQLITE_PROVIDER_ACTION_EXECUTE_BATCH: i32 = 1;
const FFI_SQLITE_PROVIDER_ACTION_QUERY_JSON: i32 = 2;
const FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM: i32 = 3;
const FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_WAIT_METRICS: i32 = 4;
const FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_CHUNK: i32 = 5;
const FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_CLOSE: i32 = 6;
const FFI_SQLITE_PROVIDER_ACTION_TOKENIZE_TEXT: i32 = 7;
const FFI_SQLITE_PROVIDER_ACTION_UPSERT_CUSTOM_WORD: i32 = 8;
const FFI_SQLITE_PROVIDER_ACTION_REMOVE_CUSTOM_WORD: i32 = 9;
const FFI_SQLITE_PROVIDER_ACTION_LIST_CUSTOM_WORDS: i32 = 10;
const FFI_SQLITE_PROVIDER_ACTION_ENSURE_FTS_INDEX: i32 = 11;
const FFI_SQLITE_PROVIDER_ACTION_REBUILD_FTS_INDEX: i32 = 12;
const FFI_SQLITE_PROVIDER_ACTION_UPSERT_FTS_DOCUMENT: i32 = 13;
const FFI_SQLITE_PROVIDER_ACTION_DELETE_FTS_DOCUMENT: i32 = 14;
const FFI_SQLITE_PROVIDER_ACTION_SEARCH_FTS: i32 = 15;
const FFI_LANCEDB_PROVIDER_ACTION_CREATE_TABLE: i32 = 0;
const FFI_LANCEDB_PROVIDER_ACTION_VECTOR_UPSERT: i32 = 1;
const FFI_LANCEDB_PROVIDER_ACTION_VECTOR_SEARCH: i32 = 2;
const FFI_LANCEDB_PROVIDER_ACTION_DELETE: i32 = 3;
const FFI_LANCEDB_PROVIDER_ACTION_DROP_TABLE: i32 = 4;
/// Stable integer value for full host-system skill-management authority.
/// 完整宿主系统技能管理权限的稳定整数值。
const FFI_SKILL_AUTHORITY_SYSTEM: i32 = 0;
/// Stable integer value for delegated-tool skill-management authority.
/// 委托工具技能管理权限的稳定整数值。
const FFI_SKILL_AUTHORITY_DELEGATED_TOOL: i32 = 1;

mod types;

pub use self::types::*;

/// Write one owned UTF-8 error buffer into the caller-provided error output slot.
/// 将一段拥有型 UTF-8 错误缓冲写入调用方提供的错误输出槽位。
fn set_error_out(error_out: *mut FfiOwnedBuffer, message: impl Into<String>) {
    if error_out.is_null() {
        return;
    }
    let text = message.into();
    unsafe {
        *error_out = alloc_owned_buffer_from_bytes(text.as_bytes());
    }
}

/// Clear one caller-provided error output slot to an empty buffer.
/// 将调用方提供的错误输出槽位清空为空缓冲。
fn clear_error_out(error_out: *mut FfiOwnedBuffer) {
    clear_out_buffer(error_out);
}

/// Clear one caller-provided pointer output slot to null.
/// 将调用方提供的指针输出槽位清空为 null。
fn clear_out_ptr<T>(value_out: *mut *mut T) {
    if !value_out.is_null() {
        unsafe { *value_out = std::ptr::null_mut() };
    }
}

/// Clear one caller-provided owned-buffer output slot to an empty buffer.
/// 将调用方提供的拥有型缓冲输出槽位清空为空缓冲。
fn clear_out_buffer(value_out: *mut FfiOwnedBuffer) {
    if !value_out.is_null() {
        unsafe {
            *value_out = FfiOwnedBuffer {
                ptr: ptr::null_mut(),
                len: 0,
            }
        };
    }
}

/// Clear one caller-provided unsigned 64-bit output slot to zero.
/// 将调用方提供的无符号 64 位输出槽位清空为零。
fn clear_out_u64(value_out: *mut u64) {
    if !value_out.is_null() {
        unsafe { *value_out = 0 };
    }
}

/// Clear one caller-provided unsigned 8-bit output slot to zero.
/// 将调用方提供的无符号 8 位输出槽位清空为零。
fn clear_out_u8(value_out: *mut u8) {
    if !value_out.is_null() {
        unsafe { *value_out = 0 };
    }
}

/// Clone one validated C string into one owned raw C string pointer.
/// 将单个已验证的 C 字符串克隆为一个拥有所有权的原生 C 字符串指针。
///
/// The value parameter is a NUL-terminated C string already parsed by the FFI boundary.
/// value 参数是已经由 FFI 边界解析过的 NUL 结尾 C 字符串。
///
/// Return a LuaSkills-owned raw C string pointer that must be freed by the matching FFI free function.
/// 返回一个 LuaSkills 拥有的原生 C 字符串指针，必须由匹配的 FFI 释放函数释放。
fn alloc_c_string(value: &CStr) -> *mut c_char {
    value.to_owned().into_raw()
}

/// Convert one byte slice into one owned FFI buffer.
/// 将单个字节切片转换为一个拥有所有权的 FFI 缓冲。
fn alloc_owned_buffer_from_bytes(value: &[u8]) -> FfiOwnedBuffer {
    alloc_owned_buffer_from_vec(value.to_vec())
}

/// Transfer one owned byte vector into the exact-length LuaSkills FFI allocation contract.
/// 将一个拥有所有权的字节向量移交给精确长度的 LuaSkills FFI 分配契约。
///
/// `value` supplies the complete byte payload and relinquishes Rust vector ownership.
/// `value` 提供完整字节载荷并交出 Rust 向量所有权。
///
/// Returns a pointer-length pair freed only by `luaskills_ffi_buffer_free` or `luaskills_ffi_bytes_free`.
/// 返回只能由 `luaskills_ffi_buffer_free` 或 `luaskills_ffi_bytes_free` 释放的指针长度对。
pub(crate) fn alloc_owned_buffer_from_vec(value: Vec<u8>) -> FfiOwnedBuffer {
    // Exact boxed allocation shared by JSON, standard buffers, callback clones, and nested fields.
    // 由 JSON、标准缓冲、回调克隆及嵌套字段共享的精确 boxed 分配。
    let (pointer, len) = alloc_ffi_boxed_slice(value);
    FfiOwnedBuffer { ptr: pointer, len }
}

/// Transfer one vector into an exact-length boxed slice for an ABI pointer-length pair.
/// 将一个向量移交为精确长度 boxed slice，供 ABI 指针长度对使用。
///
/// `values` contains every element whose ownership crosses the FFI boundary.
/// `values` 包含所有跨越 FFI 边界的元素。
///
/// Returns a null-zero pair for empty input or a raw pointer plus exact allocation length.
/// 空输入返回空指针零长度，否则返回原始指针及精确分配长度。
fn alloc_ffi_boxed_slice<T>(values: Vec<T>) -> (*mut T, usize) {
    if values.is_empty() {
        return (ptr::null_mut(), 0);
    }
    // Exact boxed slice whose allocation layout is fully described by its element count.
    // 分配布局完全由元素数量描述的精确 boxed slice。
    let values = values.into_boxed_slice();
    let len = values.len();
    let pointer = Box::into_raw(values) as *mut T;
    (pointer, len)
}

/// Reclaim one exact-length boxed slice previously transferred across the FFI boundary.
/// 回收此前跨越 FFI 边界移交的精确长度 boxed slice。
///
/// `value` and `len` must be the unchanged pointer-length pair returned by `alloc_ffi_boxed_slice`.
/// `value` 与 `len` 必须是 `alloc_ffi_boxed_slice` 返回且未经修改的指针长度对。
///
/// Returns ownership as `Box<[T]>`; null-zero input returns `None`.
/// 以 `Box<[T]>` 返回所有权；空指针零长度输入返回 `None`。
unsafe fn take_ffi_boxed_slice<T>(value: *mut T, len: usize) -> Option<Box<[T]>> {
    if value.is_null() || len == 0 {
        return None;
    }
    let slice = ptr::slice_from_raw_parts_mut(value, len);
    Some(unsafe { Box::from_raw(slice) })
}

/// Convert one Rust string into one owned UTF-8 FFI buffer.
/// 将单个 Rust 字符串转换为一个拥有所有权的 UTF-8 FFI 缓冲。
fn alloc_owned_buffer_from_string(value: impl AsRef<str>) -> FfiOwnedBuffer {
    alloc_owned_buffer_from_bytes(value.as_ref().as_bytes())
}

/// Convert one Rust string into one owned C string while rejecting interior NUL bytes.
/// 将一个 Rust 字符串转换为拥有所有权的 C 字符串，并拒绝内部 NUL 字节。
fn to_cstring(value: impl AsRef<str>, field_name: &str) -> Result<CString, String> {
    CString::new(value.as_ref()).map_err(|_| format!("{} contains interior NUL bytes", field_name))
}

/// Convert one optional Rust string into one optional owned UTF-8 FFI buffer.
/// 将单个可选 Rust 字符串转换为一个可选拥有所有权的 UTF-8 FFI 缓冲。
fn alloc_optional_owned_buffer_from_string(value: Option<&str>) -> FfiOwnedBuffer {
    value.map_or(
        FfiOwnedBuffer {
            ptr: ptr::null_mut(),
            len: 0,
        },
        alloc_owned_buffer_from_string,
    )
}

/// Parse one required UTF-8 string pointer.
/// 解析单个必填 UTF-8 字符串指针。
fn parse_required_string(value: *const c_char, field_name: &str) -> Result<String, String> {
    if value.is_null() {
        return Err(format!("{} must not be null", field_name));
    }
    let text = unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|error| format!("{} contains invalid UTF-8: {}", field_name, error))?;
    if text.is_empty() {
        return Err(format!("{} must not be empty", field_name));
    }
    Ok(text.to_string())
}

/// Parse one required UTF-8 string pointer while allowing one empty string payload.
/// 解析单个必填 UTF-8 字符串指针，并允许空字符串载荷。
/// Parse one optional UTF-8 string pointer.
/// 解析单个可选 UTF-8 字符串指针。
fn parse_optional_string(value: *const c_char, field_name: &str) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let text = unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|error| format!("{} contains invalid UTF-8: {}", field_name, error))?;
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text.to_string()))
}

/// Parse one legacy directory-name field with a runtime-root default fallback.
/// 解析单个旧目录名字段，并在使用 runtime-root 时回落到默认值。
fn parse_runtime_layout_name(
    value: *const c_char,
    field_name: &str,
    default_value: &str,
    has_runtime_root: bool,
) -> Result<String, String> {
    if has_runtime_root {
        return Ok(
            parse_optional_string(value, field_name)?.unwrap_or_else(|| default_value.to_string())
        );
    }
    parse_required_string(value, field_name)
}

/// Parse one array of UTF-8 string pointers.
/// 解析一组 UTF-8 字符串指针数组。
fn parse_string_array(
    items: *const *const c_char,
    len: usize,
    field_name: &str,
) -> Result<Vec<String>, String> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if items.is_null() {
        return Err(format!(
            "{} items pointer must not be null when len > 0",
            field_name
        ));
    }
    let slice = unsafe { std::slice::from_raw_parts(items, len) };
    slice
        .iter()
        .enumerate()
        .map(|(index, item)| parse_required_string(*item, &format!("{}[{}]", field_name, index)))
        .collect()
}

/// Parse one optional borrowed UTF-8 buffer into one owned Rust string.
/// 将单个可选借用 UTF-8 缓冲解析为一个 Rust 自有字符串。
fn parse_optional_borrowed_text(
    value: &FfiBorrowedBuffer,
    field_name: &str,
) -> Result<Option<String>, String> {
    if value.len == 0 {
        return Ok(None);
    }
    if value.ptr.is_null() {
        return Err(format!(
            "{} pointer must not be null when len > 0",
            field_name
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.ptr, value.len) };
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("{} contains invalid UTF-8: {}", field_name, error))?;
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text.to_string()))
}

/// Parse one optional borrowed JSON buffer into one serde_json value object.
/// 将单个可选借用 JSON 缓冲解析为一个 serde_json 值对象。
fn parse_json_value_or_empty_object_buffer(
    value: &FfiBorrowedBuffer,
    field_name: &str,
) -> Result<Value, String> {
    match parse_optional_borrowed_text(value, field_name)? {
        Some(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} contains invalid JSON: {}", field_name, error)),
        None => Ok(Value::Object(serde_json::Map::new())),
    }
}

/// Parse one optional borrowed request-context JSON buffer into one structured request context.
/// 将单个可选借用请求上下文 JSON 缓冲解析为一个结构化请求上下文。
fn parse_request_context_buffer(
    value: &FfiBorrowedBuffer,
    field_name: &str,
) -> Result<Option<RuntimeRequestContext>, String> {
    match parse_optional_borrowed_text(value, field_name)? {
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("{} contains invalid JSON: {}", field_name, error)),
        None => Ok(None),
    }
}

/// Execute one engine JSON-text method and write its returned UTF-8 JSON text into one owned buffer output.
/// 执行单个引擎 JSON 文本方法，并把返回的 UTF-8 JSON 文本写入拥有型缓冲输出。
fn run_engine_json_text_call<F>(
    engine_id: u64,
    request_json: &FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
    field_name: &str,
    callback: F,
) -> i32
where
    F: FnOnce(&LuaEngine, &str) -> Result<String, String>,
{
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let request_json = match parse_optional_borrowed_text(request_json, field_name) {
        Ok(Some(text)) => text,
        Ok(None) => return ffi_error_status(error_out, format!("{field_name} must not be empty")),
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| callback(engine, &request_json)) {
        Ok(result_json) => {
            unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Convert one C ABI cache config pointer into one Rust cache config.
/// 将单个 C ABI 缓存配置指针转换为一个 Rust 缓存配置。
fn parse_cache_config(value: *const FfiToolCacheConfig) -> Option<ToolCacheConfig> {
    if value.is_null() {
        None
    } else {
        let config = unsafe { &*value };
        Some(ToolCacheConfig {
            max_entries: config.max_entries,
            default_ttl_secs: config.default_ttl_secs,
            max_ttl_secs: config.max_ttl_secs,
        })
    }
}

/// Convert one optional C ABI runlua pool config pointer into one Rust runlua pool config.
/// 将一个可选的 C ABI runlua 池配置指针转换为一个 Rust runlua 池配置。
fn parse_runlua_pool_config(
    value: *const FfiLuaVmPoolConfig,
) -> Option<LuaRuntimeRunLuaPoolConfig> {
    if value.is_null() {
        None
    } else {
        let config = unsafe { &*value };
        Some(LuaRuntimeRunLuaPoolConfig {
            min_size: config.min_size,
            max_size: config.max_size,
            idle_ttl_secs: config.idle_ttl_secs,
        })
    }
}

/// Convert one optional C ABI managed-runtime policy pointer into a validated Rust value.
/// 将一个可选 C ABI 受管运行时策略指针转换为已校验 Rust 值。
///
/// `value` is null for stable defaults or points to a complete v3 policy for the duration of the call.
/// `value` 为空时使用稳定默认值，否则在调用期间指向完整 v3 策略。
///
/// Returns the effective policy or a stable presence-flag/capacity validation error.
/// 返回生效策略，或稳定的存在标记及容量校验错误。
fn parse_managed_runtime_config(
    value: *const FfiLuaRuntimeManagedRuntimeConfig,
) -> Result<LuaRuntimeManagedRuntimeConfig, String> {
    if value.is_null() {
        return Ok(LuaRuntimeManagedRuntimeConfig::default());
    }
    // Config is borrowed only for the synchronous engine-construction parse.
    // Config 仅在同步引擎构造解析期间借用。
    let config = unsafe { &*value };
    // InvokeDefaultTimeout preserves absence separately from a concrete positive timeout.
    // InvokeDefaultTimeout 将缺失状态与具体正数超时分开保留。
    let invoke_default_timeout_ms = match config.has_invoke_default_timeout_ms {
        0 => None,
        1 => Some(config.invoke_default_timeout_ms),
        other => {
            return Err(format!(
                "managed_runtime_config.has_invoke_default_timeout_ms must be 0 or 1, got {other}"
            ));
        }
    };
    // ParsedConfig owns primitive values and cannot retain any caller pointer.
    // ParsedConfig 拥有全部基础值，不会保留任何调用方指针。
    let parsed = LuaRuntimeManagedRuntimeConfig {
        worker_pool_max_size_per_environment: config.worker_pool_max_size_per_environment,
        worker_idle_ttl_secs: config.worker_idle_ttl_secs,
        persistent_session_limit_per_engine: config.persistent_session_limit_per_engine,
        persistent_session_default_buffer_limit_bytes_per_stream: config
            .persistent_session_default_buffer_limit_bytes_per_stream,
        invoke_default_timeout_ms,
    };
    parsed.validate()?;
    Ok(parsed)
}

/// Convert one C ABI host options struct into one Rust host options value.
/// 将单个 C ABI 宿主选项结构转换为一个 Rust 宿主选项值。
fn parse_host_options(value: &FfiLuaRuntimeHostOptions) -> Result<LuaRuntimeHostOptions, String> {
    parse_host_options_with_runtime_root(value, None)
}

/// Convert one C ABI host options struct plus optional v2 runtime_root into Rust host options.
/// 将单个 C ABI 宿主选项结构和可选 v2 runtime_root 转换为 Rust 宿主选项值。
fn parse_host_options_with_runtime_root(
    value: &FfiLuaRuntimeHostOptions,
    runtime_root: Option<PathBuf>,
) -> Result<LuaRuntimeHostOptions, String> {
    parse_host_options_with_managed_roots(
        value,
        runtime_root,
        None,
        None,
        LuaRuntimeManagedRuntimeConfig::default(),
    )
}

/// Convert one stable v1 host-options base plus v3 root and B3-B7 policy extensions into Rust host options.
/// 将稳定 v1 宿主选项基础结构与 v3 根及 B3-B7 策略扩展转换为 Rust 宿主选项。
///
/// `value` owns the published base fields while the three optional paths and managed policy are
/// parsed only from the matching versioned ABI wrapper supplied by the caller.
/// `value` 承载已发布基础字段，三个可选路径与受管策略仅从调用方提供的匹配版本 ABI 包装结构解析。
///
/// Returns one Rust host-options value or an explicit string, enum, or layout validation error.
/// 返回 Rust 宿主选项值，或显式字符串、枚举及布局校验错误。
fn parse_host_options_with_managed_roots(
    value: &FfiLuaRuntimeHostOptions,
    runtime_root: Option<PathBuf>,
    managed_runtime_distribution_root: Option<PathBuf>,
    managed_runtime_environment_root: Option<PathBuf>,
    managed_runtime_config: LuaRuntimeManagedRuntimeConfig,
) -> Result<LuaRuntimeHostOptions, String> {
    let has_runtime_root = runtime_root.is_some();
    Ok(LuaRuntimeHostOptions {
        runtime_root,
        managed_runtime_distribution_root,
        managed_runtime_environment_root,
        managed_runtime_config,
        temp_dir: parse_optional_string(value.temp_dir, "temp_dir")?.map(PathBuf::from),
        resources_dir: parse_optional_string(value.resources_dir, "resources_dir")?
            .map(PathBuf::from),
        lua_packages_dir: parse_optional_string(value.lua_packages_dir, "lua_packages_dir")?
            .map(PathBuf::from),
        host_provided_tool_root: parse_optional_string(
            value.host_provided_tool_root,
            "host_provided_tool_root",
        )?
        .map(PathBuf::from),
        host_provided_lua_root: parse_optional_string(
            value.host_provided_lua_root,
            "host_provided_lua_root",
        )?
        .map(PathBuf::from),
        host_provided_ffi_root: parse_optional_string(
            value.host_provided_ffi_root,
            "host_provided_ffi_root",
        )?
        .map(PathBuf::from),
        system_lua_lib_dir: parse_optional_string(value.system_lua_lib_dir, "system_lua_lib_dir")?
            .map(PathBuf::from),
        download_cache_root: parse_optional_string(
            value.download_cache_root,
            "download_cache_root",
        )?
        .map(PathBuf::from),
        dependency_dir_name: parse_runtime_layout_name(
            value.dependency_dir_name,
            "dependency_dir_name",
            "dependencies",
            has_runtime_root,
        )?,
        state_dir_name: parse_runtime_layout_name(
            value.state_dir_name,
            "state_dir_name",
            "state",
            has_runtime_root,
        )?,
        database_dir_name: parse_runtime_layout_name(
            value.database_dir_name,
            "database_dir_name",
            "databases",
            has_runtime_root,
        )?,
        skill_config_root: parse_optional_string(value.skill_config_root, "skill_config_root")?
            .map(PathBuf::from),
        skill_config_lock_timeout_ms: (value.skill_config_lock_timeout_ms != 0)
            .then_some(value.skill_config_lock_timeout_ms),
        skill_config_watch_debounce_ms: (value.skill_config_watch_debounce_ms != 0)
            .then_some(value.skill_config_watch_debounce_ms),
        allow_network_download: value.allow_network_download != 0,
        github_base_url: parse_optional_string(value.github_base_url, "github_base_url")?,
        github_api_base_url: parse_optional_string(
            value.github_api_base_url,
            "github_api_base_url",
        )?,
        official_skill_hub_base_url: parse_optional_string(
            value.official_skill_hub_base_url,
            "official_skill_hub_base_url",
        )?,
        enable_private_url_skill_install: value.enable_private_url_skill_install != 0,
        private_skill_source_allowlist: parse_string_array(
            value.private_skill_source_allowlist,
            value.private_skill_source_allowlist_len,
            "private_skill_source_allowlist",
        )?,
        default_text_encoding: parse_optional_string(
            value.default_text_encoding,
            "default_text_encoding",
        )?,
        sqlite_library_path: parse_optional_string(
            value.sqlite_library_path,
            "sqlite_library_path",
        )?
        .map(PathBuf::from),
        sqlite_provider_mode: parse_provider_mode(
            value.sqlite_provider_mode,
            "sqlite_provider_mode",
        )?,
        sqlite_callback_mode: parse_callback_mode(
            value.sqlite_callback_mode,
            "sqlite_callback_mode",
        )?,
        lancedb_library_path: parse_optional_string(
            value.lancedb_library_path,
            "lancedb_library_path",
        )?
        .map(PathBuf::from),
        lancedb_provider_mode: parse_provider_mode(
            value.lancedb_provider_mode,
            "lancedb_provider_mode",
        )?,
        lancedb_callback_mode: parse_callback_mode(
            value.lancedb_callback_mode,
            "lancedb_callback_mode",
        )?,
        space_controller: LuaRuntimeSpaceControllerOptions {
            endpoint: parse_optional_string(
                value.space_controller_endpoint,
                "space_controller_endpoint",
            )?,
            auto_spawn: value.space_controller_auto_spawn != 0,
            executable_path: parse_optional_string(
                value.space_controller_executable_path,
                "space_controller_executable_path",
            )?
            .map(PathBuf::from),
            process_mode: parse_space_controller_process_mode(
                value.space_controller_process_mode,
                "space_controller_process_mode",
            )?,
            ..LuaRuntimeSpaceControllerOptions::default()
        },
        cache_config: parse_cache_config(value.cache_config),
        runlua_pool_config: parse_runlua_pool_config(value.runlua_pool_config),
        reserved_entry_names: parse_string_array(
            value.reserved_entry_names,
            value.reserved_entry_names_len,
            "reserved_entry_names",
        )?,
        ignored_skill_ids: parse_string_array(
            value.ignored_skill_ids,
            value.ignored_skill_ids_len,
            "ignored_skill_ids",
        )?,
        capabilities: LuaRuntimeCapabilityOptions {
            enable_skill_management_bridge: value.enable_skill_management_bridge != 0,
            enable_managed_io_compat: value.disable_managed_io_compat == 0,
        },
    })
}

/// Convert one stable integer provider-mode value into the Rust runtime enum.
/// 将一个稳定整数 provider 模式值转换为 Rust 运行时枚举。
fn parse_provider_mode(
    value: i32,
    field_name: &str,
) -> Result<LuaRuntimeDatabaseProviderMode, String> {
    match value {
        FFI_PROVIDER_MODE_DYNAMIC_LIBRARY => Ok(LuaRuntimeDatabaseProviderMode::DynamicLibrary),
        FFI_PROVIDER_MODE_HOST_CALLBACK => Ok(LuaRuntimeDatabaseProviderMode::HostCallback),
        FFI_PROVIDER_MODE_SPACE_CONTROLLER => Ok(LuaRuntimeDatabaseProviderMode::SpaceController),
        _ => Err(format!("Unsupported {} value '{}'", field_name, value)),
    }
}

/// Convert one stable integer authority value into the Rust skill-management authority enum.
/// 将一个稳定整数权限值转换为 Rust 技能管理权限枚举。
fn parse_skill_management_authority(
    value: i32,
    field_name: &str,
) -> Result<SkillManagementAuthority, String> {
    match value {
        FFI_SKILL_AUTHORITY_SYSTEM => Ok(SkillManagementAuthority::System),
        FFI_SKILL_AUTHORITY_DELEGATED_TOOL => Ok(SkillManagementAuthority::DelegatedTool),
        other => Err(format!(
            "{} must be 0 (system) or 1 (delegated_tool); got {}",
            field_name, other
        )),
    }
}

/// Convert one stable integer callback-mode value into the Rust runtime enum.
/// 将一个稳定整数回调模式值转换为 Rust 运行时枚举。
fn parse_callback_mode(
    value: i32,
    field_name: &str,
) -> Result<LuaRuntimeDatabaseCallbackMode, String> {
    match value {
        FFI_CALLBACK_MODE_STANDARD => Ok(LuaRuntimeDatabaseCallbackMode::Standard),
        FFI_CALLBACK_MODE_JSON => Ok(LuaRuntimeDatabaseCallbackMode::Json),
        _ => Err(format!("Unsupported {} value '{}'", field_name, value)),
    }
}

/// Convert one stable integer space-controller process-mode value into the Rust runtime enum.
/// 将一个稳定整数空间控制器进程模式值转换为 Rust 运行时枚举。
fn parse_space_controller_process_mode(
    value: i32,
    field_name: &str,
) -> Result<LuaRuntimeSpaceControllerProcessMode, String> {
    match value {
        FFI_SPACE_CONTROLLER_PROCESS_MODE_SERVICE => {
            Ok(LuaRuntimeSpaceControllerProcessMode::Service)
        }
        FFI_SPACE_CONTROLLER_PROCESS_MODE_MANAGED => {
            Ok(LuaRuntimeSpaceControllerProcessMode::Managed)
        }
        _ => Err(format!("Unsupported {} value '{}'", field_name, value)),
    }
}

/// Convert one C ABI engine options struct into one Rust engine options value.
/// 将单个 C ABI 引擎选项结构转换为一个 Rust 引擎选项值。
fn parse_engine_options(value: &FfiLuaEngineOptions) -> Result<LuaEngineOptions, String> {
    Ok(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: value.pool.min_size,
            max_size: value.pool.max_size,
            idle_ttl_secs: value.pool.idle_ttl_secs,
        },
        parse_host_options(&value.host)?,
    ))
}

/// Convert one C ABI v2 engine options struct into one Rust engine options value.
/// 将单个 C ABI v2 引擎选项结构转换为一个 Rust 引擎选项值。
fn parse_engine_options_v2(value: &FfiLuaEngineOptionsV2) -> Result<LuaEngineOptions, String> {
    let runtime_root =
        parse_optional_string(value.host.runtime_root, "runtime_root")?.map(PathBuf::from);
    Ok(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: value.pool.min_size,
            max_size: value.pool.max_size,
            idle_ttl_secs: value.pool.idle_ttl_secs,
        },
        parse_host_options_with_runtime_root(&value.host.base, runtime_root)?,
    ))
}

/// Convert one C ABI v3 engine options struct into one Rust engine options value.
/// 将单个 C ABI v3 引擎选项结构转换为 Rust 引擎选项值。
///
/// `value` preserves the complete v2 prefix and adds two managed roots plus one optional B3-B7 policy.
/// `value` 保留完整 v2 前缀，并新增两个受管根及一份可选 B3-B7 策略。
///
/// Returns validated Rust options or an explicit UTF-8/path-layout parsing error.
/// 返回已校验 Rust 选项，或显式 UTF-8 及路径布局解析错误。
fn parse_engine_options_v3(value: &FfiLuaEngineOptionsV3) -> Result<LuaEngineOptions, String> {
    // RuntimeRoot and managed roots are parsed from their exact versioned owning fields.
    // RuntimeRoot 与受管根均从其精确版本所有字段解析。
    let runtime_root =
        parse_optional_string(value.host.base.runtime_root, "runtime_root")?.map(PathBuf::from);
    let managed_runtime_distribution_root = parse_optional_string(
        value.host.managed_runtime_distribution_root,
        "managed_runtime_distribution_root",
    )?
    .map(PathBuf::from);
    let managed_runtime_environment_root = parse_optional_string(
        value.host.managed_runtime_environment_root,
        "managed_runtime_environment_root",
    )?
    .map(PathBuf::from);
    // ManagedRuntimeConfig is borrowed from the optional v3 pointer and copied before engine creation.
    // ManagedRuntimeConfig 从可选 v3 指针借用，并在引擎创建前完成复制。
    let managed_runtime_config = parse_managed_runtime_config(value.host.managed_runtime_config)?;
    Ok(LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: value.pool.min_size,
            max_size: value.pool.max_size,
            idle_ttl_secs: value.pool.idle_ttl_secs,
        },
        parse_host_options_with_managed_roots(
            &value.host.base.base,
            runtime_root,
            managed_runtime_distribution_root,
            managed_runtime_environment_root,
            managed_runtime_config,
        )?,
    ))
}

/// Convert one C ABI root slice into one Rust runtime root vector.
/// 将单个 C ABI 根切片转换为一个 Rust 运行时根向量。
fn parse_skill_roots(
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
) -> Result<Vec<RuntimeSkillRoot>, String> {
    if skill_roots_len == 0 {
        return Ok(Vec::new());
    }
    if skill_roots.is_null() {
        return Err("skill_roots pointer must not be null when len > 0".to_string());
    }
    let roots = unsafe { std::slice::from_raw_parts(skill_roots, skill_roots_len) };
    roots
        .iter()
        .enumerate()
        .map(|(index, root)| {
            Ok(RuntimeSkillRoot {
                name: parse_required_string(root.name, &format!("skill_roots[{}].name", index))?,
                skills_dir: PathBuf::from(parse_required_string(
                    root.skills_dir,
                    &format!("skill_roots[{}].skills_dir", index),
                )?),
            })
        })
        .collect()
}

/// Convert one optional C ABI invocation context pointer into one Rust invocation context.
/// 将单个可选 C ABI 调用上下文指针转换为一个 Rust 调用上下文。
fn parse_invocation_context(
    value: *const FfiLuaInvocationContext,
) -> Result<Option<LuaInvocationContext>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let context = unsafe { &*value };
    Ok(Some(LuaInvocationContext::new(
        parse_request_context_buffer(&context.request_context_json, "request_context_json")?,
        parse_json_value_or_empty_object_buffer(&context.client_budget_json, "client_budget_json")?,
        parse_json_value_or_empty_object_buffer(&context.tool_config_json, "tool_config_json")?,
    )))
}

/// Convert one C ABI source type integer into one Rust source type value.
/// 将单个 C ABI 来源类型整数转换为一个 Rust 来源类型值。
fn parse_source_type(value: i32) -> Result<SkillInstallSourceType, String> {
    match value {
        FFI_SOURCE_TYPE_GITHUB => Ok(SkillInstallSourceType::Github),
        FFI_SOURCE_TYPE_URL => Ok(SkillInstallSourceType::Url),
        FFI_SOURCE_TYPE_OFFICIAL_HUB => Ok(SkillInstallSourceType::OfficialHub),
        FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST => Ok(SkillInstallSourceType::PrivateUrlManifest),
        _ => Err(format!("Unsupported source_type '{}'", value)),
    }
}

/// Convert one C ABI install request into one Rust install request value.
/// 将单个 C ABI 安装请求转换为一个 Rust 安装请求值。
fn parse_install_request(value: &FfiSkillInstallRequest) -> Result<SkillInstallRequest, String> {
    Ok(SkillInstallRequest {
        skill_id: parse_optional_string(value.skill_id, "skill_id")?,
        source: parse_optional_string(value.source, "source")?,
        source_type: parse_source_type(value.source_type)?,
    })
}

/// Convert one C ABI uninstall options struct into one Rust uninstall options value.
/// 将单个 C ABI 卸载选项结构转换为一个 Rust 卸载选项值。
fn parse_uninstall_options(value: Option<&FfiSkillUninstallOptions>) -> SkillUninstallOptions {
    match value {
        Some(value) => SkillUninstallOptions {
            remove_sqlite: value.remove_sqlite != 0,
            remove_lancedb: value.remove_lancedb != 0,
        },
        None => SkillUninstallOptions::default(),
    }
}

/// Convert one string vector into one owned C string array.
/// 将一个字符串向量转换为一个拥有所有权的 C 字符串数组。
fn alloc_string_array(values: &[String]) -> FfiStringArray {
    // Exact owned item array whose length completely describes its allocation layout.
    // 长度可完整描述其分配布局的精确拥有型条目数组。
    let items: Vec<FfiOwnedBuffer> = values.iter().map(alloc_owned_buffer_from_string).collect();
    let (items, len) = alloc_ffi_boxed_slice(items);
    FfiStringArray { items, len }
}

/// Convert one runtime entry parameter descriptor into one C ABI descriptor.
/// 将单个运行时入口参数描述转换为一个 C ABI 描述结构。
fn alloc_entry_parameter_descriptor(
    value: &RuntimeEntryParameterDescriptor,
) -> FfiRuntimeEntryParameterDescriptor {
    FfiRuntimeEntryParameterDescriptor {
        name: alloc_owned_buffer_from_string(&value.name),
        param_type: alloc_owned_buffer_from_string(&value.param_type),
        description: alloc_owned_buffer_from_string(&value.description),
        required: u8::from(value.required),
    }
}

/// Serialize one runtime entry input schema for the C ABI descriptor.
/// 为 C ABI 描述结构序列化单个运行时入口输入 schema。
///
/// The value parameter is the resolved JSON schema stored on the runtime entry descriptor.
/// value 参数是运行时入口描述中保存的已解析 JSON schema。
///
/// Return the compact JSON string, or an explicit serialization error for the FFI status path.
/// 返回紧凑 JSON 字符串，或返回供 FFI 状态路径使用的显式序列化错误。
fn serialize_entry_input_schema_json(value: &Value) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|error| format!("runtime entry input schema failed to serialize: {error}"))
}

/// Convert one runtime entry descriptor into one C ABI descriptor.
/// 将单个运行时入口描述转换为一个 C ABI 描述结构。
fn alloc_entry_descriptor(
    value: &RuntimeEntryDescriptor,
) -> Result<FfiRuntimeEntryDescriptor, String> {
    // Serialize before allocating nested FFI buffers so a schema error cannot leave partial ownership behind.
    // 先完成序列化再分配嵌套 FFI 缓冲，避免 schema 错误留下部分所有权。
    let input_schema_json = serialize_entry_input_schema_json(&value.input_schema)?;
    // Exact parameter array transferred under the common boxed-slice ownership contract.
    // 按公共 boxed-slice 所有权契约移交的精确参数数组。
    let parameters: Vec<FfiRuntimeEntryParameterDescriptor> = value
        .parameters
        .iter()
        .map(alloc_entry_parameter_descriptor)
        .collect();
    let (parameters_ptr, parameters_len) = alloc_ffi_boxed_slice(parameters);
    Ok(FfiRuntimeEntryDescriptor {
        canonical_name: alloc_owned_buffer_from_string(&value.canonical_name),
        skill_id: alloc_owned_buffer_from_string(&value.skill_id),
        local_name: alloc_owned_buffer_from_string(&value.local_name),
        root_name: alloc_owned_buffer_from_string(&value.root_name),
        skill_dir: alloc_owned_buffer_from_string(&value.skill_dir),
        description: alloc_owned_buffer_from_string(&value.description),
        input_schema_json: alloc_owned_buffer_from_string(input_schema_json),
        parameters: parameters_ptr,
        parameters_len,
    })
}

/// Convert runtime entry descriptors into one owned C ABI descriptor list.
/// 将运行时入口描述集合转换为一个拥有所有权的 C ABI 描述列表。
///
/// The values parameter is the list of runtime entries returned by the engine for one authority.
/// values 参数是引擎为单个权限返回的运行时入口列表。
///
/// Return an owned descriptor list, or an explicit error after freeing any already allocated descriptors.
/// 返回拥有所有权的描述列表；如失败则释放已分配描述后返回显式错误。
fn alloc_entry_descriptor_list(
    values: &[RuntimeEntryDescriptor],
) -> Result<FfiRuntimeEntryDescriptorList, String> {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        match alloc_entry_descriptor(value) {
            Ok(descriptor) => items.push(descriptor),
            Err(error) => {
                for descriptor in items {
                    unsafe { free_entry_descriptor(descriptor) };
                }
                return Err(error);
            }
        }
    }
    // Exact descriptor array transferred only after every nested allocation succeeds.
    // 仅在全部嵌套分配成功后移交的精确描述符数组。
    let (items, len) = alloc_ffi_boxed_slice(items);
    Ok(FfiRuntimeEntryDescriptorList { items, len })
}

/// Convert one help node descriptor into one C ABI descriptor.
/// 将单个帮助节点描述转换为一个 C ABI 描述结构。
fn alloc_help_node_descriptor(value: &RuntimeHelpNodeDescriptor) -> FfiRuntimeHelpNodeDescriptor {
    let related_entries = alloc_string_array(&value.related_entries);
    FfiRuntimeHelpNodeDescriptor {
        flow_name: alloc_owned_buffer_from_string(&value.flow_name),
        description: alloc_owned_buffer_from_string(&value.description),
        related_entries: related_entries.items,
        related_entries_len: related_entries.len,
        is_main: u8::from(value.is_main),
    }
}

/// Convert one runtime help tree descriptor into one C ABI descriptor.
/// 将单个运行时帮助树描述转换为一个 C ABI 描述结构。
fn alloc_help_descriptor(value: &RuntimeSkillHelpDescriptor) -> FfiRuntimeSkillHelpDescriptor {
    // Exact flow array using the same pointer-length allocation contract as every other FFI list.
    // 与其他全部 FFI 列表使用相同指针长度分配契约的精确流程数组。
    let flows: Vec<FfiRuntimeHelpNodeDescriptor> =
        value.flows.iter().map(alloc_help_node_descriptor).collect();
    let (flows_ptr, flows_len) = alloc_ffi_boxed_slice(flows);
    FfiRuntimeSkillHelpDescriptor {
        skill_id: alloc_owned_buffer_from_string(&value.skill_id),
        skill_name: alloc_owned_buffer_from_string(&value.skill_name),
        skill_version: alloc_owned_buffer_from_string(&value.skill_version),
        root_name: alloc_owned_buffer_from_string(&value.root_name),
        skill_dir: alloc_owned_buffer_from_string(&value.skill_dir),
        main: alloc_help_node_descriptor(&value.main),
        flows: flows_ptr,
        flows_len,
    }
}

/// Convert one runtime help detail into one C ABI descriptor.
/// 将单个运行时帮助详情转换为一个 C ABI 描述结构。
fn alloc_help_detail(value: &RuntimeHelpDetail) -> FfiRuntimeHelpDetail {
    let related_entries = alloc_string_array(&value.related_entries);
    FfiRuntimeHelpDetail {
        skill_id: alloc_owned_buffer_from_string(&value.skill_id),
        skill_name: alloc_owned_buffer_from_string(&value.skill_name),
        skill_version: alloc_owned_buffer_from_string(&value.skill_version),
        root_name: alloc_owned_buffer_from_string(&value.root_name),
        skill_dir: alloc_owned_buffer_from_string(&value.skill_dir),
        flow_name: alloc_owned_buffer_from_string(&value.flow_name),
        description: alloc_owned_buffer_from_string(&value.description),
        related_entries: related_entries.items,
        related_entries_len: related_entries.len,
        is_main: u8::from(value.is_main),
        content_type: alloc_owned_buffer_from_string(&value.content_type),
        content: alloc_owned_buffer_from_string(&value.content),
    }
}

/// Convert one runtime host result into one C ABI host result.
/// 将单个运行时宿主结果转换为一个 C ABI 宿主结果结构。
///
/// The value parameter is the structured host result produced by the runtime result pipeline.
/// value 参数是运行时结果链路产出的结构化宿主结果。
///
/// Return the allocated C ABI host result, or an explicit serialization error for the caller.
/// 返回已分配的 C ABI 宿主结果；如失败则向调用方返回显式序列化错误。
fn alloc_host_result(value: &RuntimeHostResult) -> Result<FfiRuntimeHostResult, String> {
    let payload_json = serde_json::to_string(&value.payload)
        .map_err(|error| format!("Failed to serialize host_result payload: {}", error))?;
    Ok(FfiRuntimeHostResult {
        kind: alloc_owned_buffer_from_string(&value.kind),
        payload_json: alloc_owned_buffer_from_string(&payload_json),
        payload_bytes: payload_json.len(),
    })
}

/// Convert one runtime invocation result into one C ABI result.
/// 将单个运行时调用结果转换为一个 C ABI 结果结构。
///
/// The value parameter is the complete invocation result returned by the runtime engine.
/// value 参数是运行时引擎返回的完整调用结果。
///
/// Return the allocated C ABI invocation result, or an explicit error from nested result allocation.
/// 返回已分配的 C ABI 调用结果；如嵌套结果分配失败则返回显式错误。
fn alloc_invocation_result(
    value: &RuntimeInvocationResult,
) -> Result<FfiRuntimeInvocationResult, String> {
    let overflow_mode = match value.overflow_mode {
        None => 0,
        Some(crate::ToolOverflowMode::Truncate) => 1,
        Some(crate::ToolOverflowMode::Page) => 2,
    };
    // Optional structured host result allocated only after successful JSON serialization.
    // 只有在 JSON 序列化成功后才分配的可选结构化宿主结果。
    let host_result = match value.host_result.as_ref() {
        Some(host_result) => Box::into_raw(Box::new(alloc_host_result(host_result)?)),
        None => ptr::null_mut(),
    };
    Ok(FfiRuntimeInvocationResult {
        content: alloc_owned_buffer_from_string(&value.content),
        overflow_mode,
        template_hint: alloc_optional_owned_buffer_from_string(value.template_hint.as_deref()),
        content_bytes: value.content_bytes,
        content_lines: value.content_lines,
        host_result,
    })
}

/// Convert one install or update result into one C ABI result.
/// 将单个安装或更新结果转换为一个 C ABI 结果结构。
fn alloc_skill_apply_result(value: &SkillApplyResult) -> FfiSkillApplyResult {
    let source_type = match value.source_type {
        None => FFI_SOURCE_TYPE_ABSENT,
        Some(SkillInstallSourceType::Github) => FFI_SOURCE_TYPE_GITHUB,
        Some(SkillInstallSourceType::Url) => FFI_SOURCE_TYPE_URL,
        Some(SkillInstallSourceType::OfficialHub) => FFI_SOURCE_TYPE_OFFICIAL_HUB,
        Some(SkillInstallSourceType::PrivateUrlManifest) => FFI_SOURCE_TYPE_PRIVATE_URL_MANIFEST,
    };
    FfiSkillApplyResult {
        skill_id: alloc_owned_buffer_from_string(&value.skill_id),
        status: alloc_owned_buffer_from_string(&value.status),
        message: alloc_owned_buffer_from_string(&value.message),
        version: alloc_optional_owned_buffer_from_string(value.version.as_deref()),
        source_type,
        source_locator: alloc_optional_owned_buffer_from_string(value.source_locator.as_deref()),
    }
}

/// Convert one uninstall result into one C ABI result.
/// 将单个卸载结果转换为一个 C ABI 结果结构。
fn alloc_skill_uninstall_result(value: &SkillUninstallResult) -> FfiSkillUninstallResult {
    FfiSkillUninstallResult {
        skill_id: alloc_owned_buffer_from_string(&value.skill_id),
        skill_removed: u8::from(value.skill_removed),
        sqlite_removed: u8::from(value.sqlite_removed),
        lancedb_removed: u8::from(value.lancedb_removed),
        sqlite_retained: u8::from(value.sqlite_retained),
        lancedb_retained: u8::from(value.lancedb_retained),
        message: alloc_owned_buffer_from_string(&value.message),
    }
}

/// Owned C-string storage used to keep one provider binding context alive during one callback invocation.
/// 用于在单次回调调用期间保持 provider 绑定上下文存活的拥有型 C 字符串存储。
struct OwnedFfiRuntimeDatabaseBindingContext {
    /// Stable host-provided space label.
    /// 宿主提供的稳定空间标签。
    space_label: CString,
    /// Stable skill identifier.
    /// 稳定技能标识符。
    skill_id: CString,
    /// Stable database binding tag.
    /// 稳定数据库绑定标签。
    binding_tag: CString,
    /// Effective physical root label.
    /// 生效物理根标签。
    root_name: CString,
    /// Physical space root path.
    /// 物理空间根路径。
    space_root: CString,
    /// Physical skill directory path.
    /// 物理技能目录路径。
    skill_dir: CString,
    /// Physical skill directory basename.
    /// 物理技能目录名称。
    skill_dir_name: CString,
    /// Default embedded database path.
    /// 默认内嵌数据库路径。
    default_database_path: CString,
    /// Borrowed C ABI view built on top of the owned strings.
    /// 构建在拥有型字符串之上的借用式 C ABI 视图。
    ffi: FfiRuntimeDatabaseBindingContext,
}

impl OwnedFfiRuntimeDatabaseBindingContext {
    /// Build one owned C ABI binding context from one runtime binding context.
    /// 基于运行时绑定上下文构造一个拥有型 C ABI 绑定上下文。
    fn from_runtime(value: &RuntimeDatabaseBindingContext) -> Result<Self, String> {
        let space_label = to_cstring(&value.space_label, "space_label")?;
        let skill_id = to_cstring(&value.skill_id, "skill_id")?;
        let binding_tag = to_cstring(&value.binding_tag, "binding_tag")?;
        let root_name = to_cstring(&value.root_name, "root_name")?;
        let space_root = to_cstring(&value.space_root, "space_root")?;
        let skill_dir = to_cstring(&value.skill_dir, "skill_dir")?;
        let skill_dir_name = to_cstring(&value.skill_dir_name, "skill_dir_name")?;
        let default_database_path =
            to_cstring(&value.default_database_path, "default_database_path")?;
        let database_kind = ffi_database_kind_code(value.database_kind);
        let ffi = FfiRuntimeDatabaseBindingContext {
            space_label: space_label.as_ptr(),
            skill_id: skill_id.as_ptr(),
            binding_tag: binding_tag.as_ptr(),
            root_name: root_name.as_ptr(),
            space_root: space_root.as_ptr(),
            skill_dir: skill_dir.as_ptr(),
            skill_dir_name: skill_dir_name.as_ptr(),
            database_kind,
            default_database_path: default_database_path.as_ptr(),
        };
        Ok(Self {
            space_label,
            skill_id,
            binding_tag,
            root_name,
            space_root,
            skill_dir,
            skill_dir_name,
            default_database_path,
            ffi,
        })
    }

    /// Borrow the underlying C ABI binding context.
    /// 借用底层 C ABI 绑定上下文。
    fn as_ffi(&self) -> FfiRuntimeDatabaseBindingContext {
        FfiRuntimeDatabaseBindingContext {
            space_label: self.space_label.as_ptr(),
            skill_id: self.skill_id.as_ptr(),
            binding_tag: self.binding_tag.as_ptr(),
            root_name: self.root_name.as_ptr(),
            space_root: self.space_root.as_ptr(),
            skill_dir: self.skill_dir.as_ptr(),
            skill_dir_name: self.skill_dir_name.as_ptr(),
            database_kind: self.ffi.database_kind,
            default_database_path: self.default_database_path.as_ptr(),
        }
    }
}

/// Build one borrowed buffer view over one owned byte slice kept alive by the caller.
/// 基于调用方持有存活期的拥有型字节切片构造一个借用缓冲视图。
fn borrowed_buffer_from_bytes(bytes: &[u8]) -> FfiBorrowedBuffer {
    if bytes.is_empty() {
        return FfiBorrowedBuffer {
            ptr: ptr::null(),
            len: 0,
        };
    }
    FfiBorrowedBuffer {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    }
}

/// Owned SQLite provider request wrapper used during one standard callback invocation.
/// 在单次标准回调调用期间使用的拥有型 SQLite provider 请求包装器。
struct OwnedFfiSqliteProviderRequest {
    /// Owned binding context backing the request.
    /// 为请求提供支撑的拥有型绑定上下文。
    _binding: OwnedFfiRuntimeDatabaseBindingContext,
    /// JSON-encoded action input payload bytes.
    /// 以 JSON 编码的动作输入载荷字节。
    _input_json: Vec<u8>,
    /// Borrowed C ABI request view.
    /// 借用式 C ABI 请求视图。
    ffi: FfiSqliteProviderRequest,
}

impl OwnedFfiSqliteProviderRequest {
    /// Build one owned SQLite provider request wrapper from one runtime request.
    /// 基于运行时请求构造一个拥有型 SQLite provider 请求包装器。
    fn from_runtime(value: &RuntimeSqliteProviderRequest) -> Result<Self, String> {
        let binding = OwnedFfiRuntimeDatabaseBindingContext::from_runtime(&value.binding)?;
        let input_json = serde_json::to_vec(&value.input)
            .map_err(|error| format!("failed to encode sqlite input json: {}", error))?;
        let ffi = FfiSqliteProviderRequest {
            action: ffi_sqlite_provider_action_code(&value.action),
            binding: binding.as_ffi(),
            input_json: borrowed_buffer_from_bytes(&input_json),
        };
        Ok(Self {
            _binding: binding,
            _input_json: input_json,
            ffi,
        })
    }

    /// Borrow the underlying C ABI request pointer.
    /// 借用底层 C ABI 请求指针。
    fn as_ptr(&self) -> *const FfiSqliteProviderRequest {
        &self.ffi
    }
}

/// Owned LanceDB provider request wrapper used during one standard callback invocation.
/// 在单次标准回调调用期间使用的拥有型 LanceDB provider 请求包装器。
struct OwnedFfiLanceDbProviderRequest {
    /// Owned binding context backing the request.
    /// 为请求提供支撑的拥有型绑定上下文。
    _binding: OwnedFfiRuntimeDatabaseBindingContext,
    /// JSON-encoded action input payload bytes.
    /// 以 JSON 编码的动作输入载荷字节。
    _input_json: Vec<u8>,
    /// Borrowed C ABI request view.
    /// 借用式 C ABI 请求视图。
    ffi: FfiLanceDbProviderRequest,
}

impl OwnedFfiLanceDbProviderRequest {
    /// Build one owned LanceDB provider request wrapper from one runtime request.
    /// 基于运行时请求构造一个拥有型 LanceDB provider 请求包装器。
    fn from_runtime(value: &RuntimeLanceDbProviderRequest) -> Result<Self, String> {
        let binding = OwnedFfiRuntimeDatabaseBindingContext::from_runtime(&value.binding)?;
        let input_json = serde_json::to_vec(&value.input)
            .map_err(|error| format!("failed to encode lancedb input json: {}", error))?;
        let ffi = FfiLanceDbProviderRequest {
            action: ffi_lancedb_provider_action_code(&value.action),
            binding: binding.as_ffi(),
            input_json: borrowed_buffer_from_bytes(&input_json),
        };
        Ok(Self {
            _binding: binding,
            _input_json: input_json,
            ffi,
        })
    }

    /// Borrow the underlying C ABI request pointer.
    /// 借用底层 C ABI 请求指针。
    fn as_ptr(&self) -> *const FfiLanceDbProviderRequest {
        &self.ffi
    }
}

/// Convert one runtime database kind into one stable FFI integer code.
/// 将运行时数据库类型转换为稳定 FFI 整数编码。
fn ffi_database_kind_code(value: RuntimeDatabaseKind) -> i32 {
    match value {
        RuntimeDatabaseKind::Sqlite => FFI_DATABASE_KIND_SQLITE,
        RuntimeDatabaseKind::LanceDb => FFI_DATABASE_KIND_LANCEDB,
    }
}

/// Convert one runtime SQLite provider action into one stable FFI integer code.
/// 将运行时 SQLite provider 动作转换为稳定 FFI 整数编码。
fn ffi_sqlite_provider_action_code(value: &RuntimeSqliteProviderAction) -> i32 {
    match value {
        RuntimeSqliteProviderAction::ExecuteScript => FFI_SQLITE_PROVIDER_ACTION_EXECUTE_SCRIPT,
        RuntimeSqliteProviderAction::ExecuteBatch => FFI_SQLITE_PROVIDER_ACTION_EXECUTE_BATCH,
        RuntimeSqliteProviderAction::QueryJson => FFI_SQLITE_PROVIDER_ACTION_QUERY_JSON,
        RuntimeSqliteProviderAction::QueryStream => FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM,
        RuntimeSqliteProviderAction::QueryStreamWaitMetrics => {
            FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_WAIT_METRICS
        }
        RuntimeSqliteProviderAction::QueryStreamChunk => {
            FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_CHUNK
        }
        RuntimeSqliteProviderAction::QueryStreamClose => {
            FFI_SQLITE_PROVIDER_ACTION_QUERY_STREAM_CLOSE
        }
        RuntimeSqliteProviderAction::TokenizeText => FFI_SQLITE_PROVIDER_ACTION_TOKENIZE_TEXT,
        RuntimeSqliteProviderAction::UpsertCustomWord => {
            FFI_SQLITE_PROVIDER_ACTION_UPSERT_CUSTOM_WORD
        }
        RuntimeSqliteProviderAction::RemoveCustomWord => {
            FFI_SQLITE_PROVIDER_ACTION_REMOVE_CUSTOM_WORD
        }
        RuntimeSqliteProviderAction::ListCustomWords => {
            FFI_SQLITE_PROVIDER_ACTION_LIST_CUSTOM_WORDS
        }
        RuntimeSqliteProviderAction::EnsureFtsIndex => FFI_SQLITE_PROVIDER_ACTION_ENSURE_FTS_INDEX,
        RuntimeSqliteProviderAction::RebuildFtsIndex => {
            FFI_SQLITE_PROVIDER_ACTION_REBUILD_FTS_INDEX
        }
        RuntimeSqliteProviderAction::UpsertFtsDocument => {
            FFI_SQLITE_PROVIDER_ACTION_UPSERT_FTS_DOCUMENT
        }
        RuntimeSqliteProviderAction::DeleteFtsDocument => {
            FFI_SQLITE_PROVIDER_ACTION_DELETE_FTS_DOCUMENT
        }
        RuntimeSqliteProviderAction::SearchFts => FFI_SQLITE_PROVIDER_ACTION_SEARCH_FTS,
    }
}

/// Convert one runtime LanceDB provider action into one stable FFI integer code.
/// 将运行时 LanceDB provider 动作转换为稳定 FFI 整数编码。
fn ffi_lancedb_provider_action_code(value: &RuntimeLanceDbProviderAction) -> i32 {
    match value {
        RuntimeLanceDbProviderAction::CreateTable => FFI_LANCEDB_PROVIDER_ACTION_CREATE_TABLE,
        RuntimeLanceDbProviderAction::VectorUpsert => FFI_LANCEDB_PROVIDER_ACTION_VECTOR_UPSERT,
        RuntimeLanceDbProviderAction::VectorSearch => FFI_LANCEDB_PROVIDER_ACTION_VECTOR_SEARCH,
        RuntimeLanceDbProviderAction::Delete => FFI_LANCEDB_PROVIDER_ACTION_DELETE,
        RuntimeLanceDbProviderAction::DropTable => FFI_LANCEDB_PROVIDER_ACTION_DROP_TABLE,
    }
}

/// Invoke one host-supplied JSON provider callback and copy the returned string into Rust ownership.
/// 调用宿主提供的 JSON provider 回调，并把返回字符串复制到 Rust 所有权下。
fn invoke_json_provider_callback(
    callback: FfiJsonProviderCallback,
    user_data: usize,
    request_json: &str,
) -> Result<String, String> {
    let request_bytes = request_json.as_bytes();
    let mut response_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        callback(
            FfiBorrowedBuffer {
                ptr: request_bytes.as_ptr(),
                len: request_bytes.len(),
            },
            user_data as *mut c_void,
            &mut response_out,
            &mut error_out,
        )
    };
    let callback_error =
        take_optional_owned_ffi_string_buffer(error_out, "json host provider callback error_out")?;
    if status != FFI_STATUS_OK {
        unsafe { free_ffi_bytes(response_out.ptr, response_out.len) };
        return Err(callback_error.unwrap_or_else(|| {
            "json host provider callback returned failure without error message".to_string()
        }));
    }
    let response = take_optional_owned_ffi_string_buffer(
        response_out,
        "json host provider callback response_out",
    )?
    .ok_or_else(|| "json host provider callback returned empty response_out".to_string())?;
    if let Some(message) = callback_error
        && !message.is_empty()
    {
        return Err(format!(
            "json host provider callback returned unexpected error text on success: {}",
            message
        ));
    }
    Ok(response)
}

/// Build an internal model callback bridge error.
/// 构造一个模型 callback 桥接内部错误。
fn runtime_model_callback_internal_error(message: impl Into<String>) -> RuntimeModelError {
    RuntimeModelError {
        code: RuntimeModelErrorCode::InternalError,
        message: message.into(),
        provider_message: None,
        provider_code: None,
        provider_status: None,
    }
}

/// Extract one string field from a JSON model error object.
/// 从 JSON 模型错误对象中提取单个字符串字段。
fn runtime_model_error_string_field(
    object: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Option<String> {
    object
        .get(field_name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Extract one provider status field from a JSON model error object.
/// 从 JSON 模型错误对象中提取单个 provider 状态字段。
fn runtime_model_error_status_field(
    object: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Option<u16> {
    object
        .get(field_name)
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

/// Locate the model error object inside either an envelope or a direct error payload.
/// 在错误包络或直接错误载荷中定位模型错误对象。
fn runtime_model_error_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.get("error").and_then(Value::as_object).or_else(|| {
        value
            .as_object()
            .filter(|object| object.contains_key("code") || object.contains_key("message"))
    })
}

/// Convert one JSON model error payload into the internal runtime error type.
/// 将单个 JSON 模型错误载荷转换为内部运行时错误类型。
fn runtime_model_error_from_json_value(value: &Value) -> Option<RuntimeModelError> {
    let object = runtime_model_error_object(value)?;
    let code = runtime_model_error_string_field(object, "code")
        .map(|value| RuntimeModelErrorCode::from_code_str(&value))
        .unwrap_or(RuntimeModelErrorCode::InternalError);
    let message = runtime_model_error_string_field(object, "message")
        .unwrap_or_else(|| "model callback returned an error".to_string());
    Some(RuntimeModelError {
        code,
        message,
        provider_message: runtime_model_error_string_field(object, "provider_message"),
        provider_code: runtime_model_error_string_field(object, "provider_code"),
        provider_status: runtime_model_error_status_field(object, "provider_status"),
    })
}

/// Convert one failed JSON callback bridge message into a model error.
/// 将单个失败的 JSON callback 桥接消息转换为模型错误。
fn runtime_model_error_from_callback_failure(message: String) -> RuntimeModelError {
    if let Ok(value) = serde_json::from_str::<Value>(&message)
        && let Some(error) = runtime_model_error_from_json_value(&value)
    {
        return error;
    }
    runtime_model_callback_internal_error(message)
}

/// Decode and normalize one JSON callback model response.
/// 解码并归一化单个 JSON callback 模型响应。
fn runtime_model_callback_response_value(
    response_json: &str,
    capability: &str,
) -> Result<Value, RuntimeModelError> {
    let value = serde_json::from_str::<Value>(response_json).map_err(|error| {
        runtime_model_callback_internal_error(format!(
            "model {} response JSON decode failed: {}",
            capability, error
        ))
    })?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(
            runtime_model_error_from_json_value(&value).unwrap_or_else(|| {
                runtime_model_callback_internal_error(format!(
                    "model {} callback returned ok=false without a valid error object",
                    capability
                ))
            }),
        );
    }
    if value.get("ok").and_then(Value::as_bool) == Some(true)
        && let Some(inner) = value.get("value").or_else(|| value.get("result"))
    {
        return Ok(inner.clone());
    }
    Ok(value)
}

/// Decode one JSON callback embedding response into the typed runtime response.
/// 将单个 JSON callback embedding 响应解码为类型化运行时响应。
fn runtime_model_embed_response_from_json(
    response_json: &str,
) -> Result<RuntimeModelEmbedResponse, RuntimeModelError> {
    let value = runtime_model_callback_response_value(response_json, "embed")?;
    serde_json::from_value::<RuntimeModelEmbedResponse>(value).map_err(|error| {
        runtime_model_callback_internal_error(format!(
            "model embed response JSON decode failed: {}",
            error
        ))
    })
}

/// Decode one JSON callback LLM response into the typed runtime response.
/// 将单个 JSON callback LLM 响应解码为类型化运行时响应。
fn runtime_model_llm_response_from_json(
    response_json: &str,
) -> Result<RuntimeModelLlmResponse, RuntimeModelError> {
    let value = runtime_model_callback_response_value(response_json, "llm")?;
    serde_json::from_value::<RuntimeModelLlmResponse>(value).map_err(|error| {
        runtime_model_callback_internal_error(format!(
            "model llm response JSON decode failed: {}",
            error
        ))
    })
}

/// Invoke one host-supplied standard SQLite provider callback and decode the returned JSON payload.
/// 调用宿主提供的标准 SQLite provider 回调，并解码返回的 JSON 载荷。
fn invoke_standard_sqlite_provider_callback(
    callback: FfiSqliteProviderCallback,
    user_data: usize,
    request: &RuntimeSqliteProviderRequest,
) -> Result<Value, String> {
    let request = OwnedFfiSqliteProviderRequest::from_runtime(request)?;
    let mut response_json_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        callback(
            request.as_ptr(),
            user_data as *mut c_void,
            &mut response_json_out,
            &mut error_out,
        )
    };
    let callback_error = take_optional_owned_ffi_string_buffer(
        error_out,
        "sqlite host provider callback error_out",
    )?;
    if status != FFI_STATUS_OK {
        unsafe { free_ffi_bytes(response_json_out.ptr, response_json_out.len) };
        return Err(callback_error.unwrap_or_else(|| {
            "sqlite host provider callback returned failure without error message".to_string()
        }));
    }
    let response_json = take_optional_owned_ffi_string_buffer(
        response_json_out,
        "sqlite host provider callback response_json_out",
    )?
    .ok_or_else(|| "sqlite host provider callback returned empty response_json_out".to_string())?;
    if let Some(message) = callback_error
        && !message.is_empty()
    {
        return Err(format!(
            "sqlite host provider callback returned unexpected error text on success: {}",
            message
        ));
    }
    serde_json::from_str(&response_json).map_err(|error| {
        format!(
            "failed to parse sqlite provider callback response json: {}",
            error
        )
    })
}

/// Invoke one host-supplied standard LanceDB provider callback and decode the returned payload.
/// 调用宿主提供的标准 LanceDB provider 回调，并解码返回的载荷。
fn invoke_standard_lancedb_provider_callback(
    callback: FfiLanceDbProviderCallback,
    user_data: usize,
    request: &RuntimeLanceDbProviderRequest,
) -> Result<RuntimeLanceDbProviderResult, String> {
    let request = OwnedFfiLanceDbProviderRequest::from_runtime(request)?;
    let mut meta_json_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut data_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        callback(
            request.as_ptr(),
            user_data as *mut c_void,
            &mut meta_json_out,
            &mut data_out,
            &mut error_out,
        )
    };
    let callback_error = take_optional_owned_ffi_string_buffer(
        error_out,
        "lancedb host provider callback error_out",
    )?;
    if status != FFI_STATUS_OK {
        unsafe {
            free_ffi_bytes(meta_json_out.ptr, meta_json_out.len);
            free_ffi_bytes(data_out.ptr, data_out.len);
        }
        return Err(callback_error.unwrap_or_else(|| {
            "lancedb host provider callback returned failure without error message".to_string()
        }));
    }
    let meta_json = take_optional_owned_ffi_string_buffer(
        meta_json_out,
        "lancedb host provider callback meta_json_out",
    )?
    .unwrap_or_else(|| "{}".to_string());
    let meta = serde_json::from_str::<Value>(&meta_json).map_err(|error| {
        format!(
            "failed to parse lancedb provider callback meta json: {}",
            error
        )
    })?;
    let bytes =
        take_optional_owned_ffi_buffer(data_out, "lancedb host provider callback data_out")?
            .unwrap_or_default();
    if let Some(message) = callback_error
        && !message.is_empty()
    {
        return Err(format!(
            "lancedb host provider callback returned unexpected error text on success: {}",
            message
        ));
    }
    Ok(RuntimeLanceDbProviderResult::binary(meta, bytes))
}

/// Copy one optional owned FFI buffer into Rust ownership and free the original allocation.
/// 将单个可选拥有型 FFI 缓冲复制到 Rust 所有权，并释放原始分配。
fn take_optional_owned_ffi_buffer(
    value: FfiOwnedBuffer,
    field_name: &str,
) -> Result<Option<Vec<u8>>, String> {
    if value.ptr.is_null() {
        if value.len == 0 {
            return Ok(None);
        }
        return Err(format!(
            "{} returned null ptr with non-zero len",
            field_name
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.ptr, value.len) }.to_vec();
    unsafe { free_ffi_bytes(value.ptr, value.len) };
    Ok(Some(bytes))
}

/// Copy one optional owned UTF-8 buffer into Rust string ownership and free the original allocation.
/// 将单个可选拥有型 UTF-8 缓冲复制到 Rust 字符串所有权，并释放原始分配。
fn take_optional_owned_ffi_string_buffer(
    value: FfiOwnedBuffer,
    field_name: &str,
) -> Result<Option<String>, String> {
    let bytes = match take_optional_owned_ffi_buffer(value, field_name)? {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("{} returned non-utf8 bytes: {}", field_name, error))
}

/// Free one owned byte buffer allocated by one FFI callback helper.
/// 释放由某个 FFI 回调辅助函数分配的拥有型字节缓冲。
unsafe fn free_ffi_bytes(value: *mut u8, len: usize) {
    drop(unsafe { take_ffi_boxed_slice(value, len) });
}

/// Free one owned string array and all nested string items.
/// 释放单个拥有所有权的字符串数组以及其嵌套字符串条目。
unsafe fn free_string_array_parts(items: *mut FfiOwnedBuffer, len: usize) {
    if let Some(values) = unsafe { take_ffi_boxed_slice(items, len) } {
        for value in values {
            unsafe { luaskills_ffi_buffer_free(value) };
        }
    }
}

/// Free one owned entry parameter descriptor.
/// 释放单个拥有所有权的入口参数描述结构。
unsafe fn free_entry_parameter_descriptor(value: FfiRuntimeEntryParameterDescriptor) {
    unsafe { luaskills_ffi_buffer_free(value.name) };
    unsafe { luaskills_ffi_buffer_free(value.param_type) };
    unsafe { luaskills_ffi_buffer_free(value.description) };
}

/// Free one owned entry descriptor.
/// 释放单个拥有所有权的入口描述结构。
unsafe fn free_entry_descriptor(value: FfiRuntimeEntryDescriptor) {
    unsafe { luaskills_ffi_buffer_free(value.canonical_name) };
    unsafe { luaskills_ffi_buffer_free(value.skill_id) };
    unsafe { luaskills_ffi_buffer_free(value.local_name) };
    unsafe { luaskills_ffi_buffer_free(value.root_name) };
    unsafe { luaskills_ffi_buffer_free(value.skill_dir) };
    unsafe { luaskills_ffi_buffer_free(value.description) };
    unsafe { luaskills_ffi_buffer_free(value.input_schema_json) };
    if let Some(parameters) =
        unsafe { take_ffi_boxed_slice(value.parameters, value.parameters_len) }
    {
        for parameter in parameters {
            unsafe { free_entry_parameter_descriptor(parameter) };
        }
    }
}

/// Free one owned help node descriptor.
/// 释放单个拥有所有权的帮助节点描述结构。
unsafe fn free_help_node_descriptor(value: FfiRuntimeHelpNodeDescriptor) {
    unsafe { luaskills_ffi_buffer_free(value.flow_name) };
    unsafe { luaskills_ffi_buffer_free(value.description) };
    unsafe { free_string_array_parts(value.related_entries, value.related_entries_len) };
}

/// Write one successful status code.
/// 写入一个成功状态码。
fn ffi_ok_status(error_out: *mut FfiOwnedBuffer) -> i32 {
    clear_error_out(error_out);
    FFI_STATUS_OK
}

/// Write one failed status code and error text.
/// 写入一个失败状态码与错误文本。
fn ffi_error_status(error_out: *mut FfiOwnedBuffer, message: impl Into<String>) -> i32 {
    set_error_out(error_out, message);
    FFI_STATUS_ERROR
}

/// Serialize one managed-session event operation result into the caller-owned output slots.
/// 将单次受管会话事件操作结果序列化到调用方拥有的输出槽位。
///
/// `operation_result` contains either the stable event batch or its explicit runtime error.
/// `operation_result` 包含稳定事件批次或对应的显式运行时错误。
///
/// `result_json_out` must be non-null and writable; `error_out` may be null or writable.
/// `result_json_out` 必须非空且可写；`error_out` 可以为空或可写。
///
/// Return `FFI_STATUS_OK` after writing direct batch JSON, otherwise `FFI_STATUS_ERROR`.
/// 写入直接批次 JSON 后返回 `FFI_STATUS_OK`，否则返回 `FFI_STATUS_ERROR`。
fn write_managed_session_event_batch(
    operation_result: Result<RuntimeManagedSessionEventBatch, String>,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    // Successful event batch selected before any output allocation occurs.
    // 在发生任何输出分配前选出的成功事件批次。
    let batch = match operation_result {
        Ok(batch) => batch,
        Err(error) => return ffi_error_status(error_out, error),
    };
    // Stable direct JSON payload shared by poll and wait standard ABI calls.
    // poll 与 wait 标准 ABI 调用共享的稳定直接 JSON 载荷。
    let result_json = match serde_json::to_string(&batch) {
        Ok(result_json) => result_json,
        Err(error) => {
            return ffi_error_status(
                error_out,
                format!("managed session event batch JSON encode failed: {error}"),
            );
        }
    };
    unsafe {
        *result_json_out = alloc_owned_buffer_from_string(result_json);
    }
    ffi_ok_status(error_out)
}

/// Invoke one host wake callback and consume its optional LuaSkills-owned diagnostic buffer.
/// 调用单个宿主唤醒回调并消费其可选的 LuaSkills 所有诊断缓冲。
///
/// `callback` is the registered C ABI function and `user_data` is its opaque host value.
/// `callback` 是已注册的 C ABI 函数，`user_data` 是其不透明宿主值。
///
/// `engine_id` identifies the event source delivered to the callback.
/// `engine_id` 标识传递给回调的事件来源。
///
/// Return success only when the host accepted the wake scheduling request.
/// 仅在宿主接受唤醒调度请求时返回成功。
fn invoke_managed_session_wake_callback(
    callback: FfiManagedSessionWakeCallback,
    user_data: usize,
    engine_id: u64,
) -> Result<(), String> {
    // Callback-owned error output initialized to the required empty representation.
    // 初始化为规定空表示形式的回调错误输出。
    let mut error_out = FfiOwnedBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    // Callback status captured without holding any LuaSkills registry, engine, or event-center lock.
    // 在不持有 LuaSkills 注册表锁、引擎锁或事件中心锁时捕获的回调状态。
    let status = unsafe { callback(engine_id, user_data as *mut c_void, &mut error_out) };
    // Optional callback diagnostic copied into Rust ownership and released immediately.
    // 立即复制到 Rust 所有权并释放的可选回调诊断信息。
    let callback_error = match take_optional_owned_ffi_string_buffer(
        error_out,
        "managed session wake callback error_out",
    ) {
        Ok(callback_error) => callback_error,
        Err(error) => {
            return Err(format!(
                "managed session wake callback for engine {engine_id} returned invalid error_out: {error}"
            ));
        }
    };
    if status != FFI_STATUS_OK {
        // Normalized host diagnostic retaining the engine source in runtime logs.
        // 在运行时日志中保留引擎来源的规范化宿主诊断信息。
        let message = callback_error
            .unwrap_or_else(|| "callback returned failure without error message".to_string());
        return Err(format!(
            "managed session wake callback for engine {engine_id} failed: {message}"
        ));
    }
    if let Some(message) = callback_error
        && !message.is_empty()
    {
        logging::error(format!(
            "managed session wake callback for engine {engine_id} returned unexpected error text on success: {message}"
        ));
    }
    Ok(())
}

/// Clone one host string into one LuaSkills-owned heap string so callbacks can return safely across FFI.
/// 将宿主字符串克隆到 LuaSkills 管理的堆字符串，便于回调安全跨 FFI 返回。
///
/// The value parameter must point to a NUL-terminated UTF-8 string, or be null for an empty clone.
/// value 参数必须指向 NUL 结尾的 UTF-8 字符串；传入 null 时克隆为空字符串。
///
/// Return one LuaSkills-owned C string pointer, or null when the input is not valid UTF-8.
/// 返回一个 LuaSkills 拥有的 C 字符串指针；当输入不是有效 UTF-8 时返回 null。
///
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_string_clone(value: *const c_char) -> *mut c_char {
    if value.is_null() {
        return alloc_c_string(c"");
    }
    let source = unsafe { CStr::from_ptr(value) };
    match source.to_str() {
        Ok(_) => alloc_c_string(source),
        Err(_) => ptr::null_mut(),
    }
}

/// Clone one host buffer into one LuaSkills-owned buffer container for callback returns.
/// 将宿主缓冲克隆到 LuaSkills 管理的缓冲容器，便于 callback 返回。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_buffer_clone(
    value: *const u8,
    len: usize,
    buffer_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(buffer_out);
    if buffer_out.is_null() {
        return ffi_error_status(error_out, "buffer_out must not be null");
    }
    if value.is_null() && len != 0 {
        return ffi_error_status(error_out, "value must not be null when len > 0");
    }
    let slice = if len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(value, len) }
    };
    unsafe {
        *buffer_out = alloc_owned_buffer_from_bytes(slice);
    }
    ffi_ok_status(error_out)
}

/// Clone one host byte buffer into one LuaSkills-owned heap buffer for standard callback returns.
/// 将宿主字节缓冲克隆到 LuaSkills 管理的堆缓冲，用于标准回调返回。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_bytes_clone(value: *const u8, len: usize) -> *mut u8 {
    if value.is_null() || len == 0 {
        return ptr::null_mut();
    }
    let slice = unsafe { std::slice::from_raw_parts(value, len) };
    alloc_owned_buffer_from_bytes(slice).ptr
}

/// Free one LuaSkills-owned heap byte buffer created by `luaskills_ffi_bytes_clone`.
/// 释放由 `luaskills_ffi_bytes_clone` 创建的 LuaSkills 自主管理堆字节缓冲。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_bytes_free(value: *mut u8, len: usize) {
    unsafe { free_ffi_bytes(value, len) };
}

/// Free one LuaSkills-owned buffer container created by `luaskills_ffi_buffer_clone`.
/// 释放由 `luaskills_ffi_buffer_clone` 创建的 LuaSkills 自主管理缓冲容器。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_buffer_free(value: FfiOwnedBuffer) {
    unsafe { free_ffi_bytes(value.ptr, value.len) };
}

/// Register or clear one SQLite standard provider callback for host-managed database integration.
/// 为宿主管理数据库集成注册或清理一个 SQLite 标准 provider 回调。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_sqlite_provider_callback(
    callback: Option<FfiSqliteProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request: &RuntimeSqliteProviderRequest| {
            invoke_standard_sqlite_provider_callback(callback_fn, user_data, request)
        }) as RuntimeSqliteProviderCallback
    });
    set_sqlite_provider_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one LanceDB standard provider callback for host-managed database integration.
/// 为宿主管理数据库集成注册或清理一个 LanceDB 标准 provider 回调。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_lancedb_provider_callback(
    callback: Option<FfiLanceDbProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request: &RuntimeLanceDbProviderRequest| {
            invoke_standard_lancedb_provider_callback(callback_fn, user_data, request)
        }) as RuntimeLanceDbProviderCallback
    });
    set_lancedb_provider_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one SQLite JSON provider callback for cross-language host integration.
/// 为跨语言宿主集成注册或清理一个 SQLite JSON provider 回调。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_sqlite_provider_json_callback(
    callback: Option<FfiJsonProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request_json: &str| {
            invoke_json_provider_callback(callback_fn, user_data, request_json)
        }) as crate::host::database::RuntimeSqliteProviderJsonCallback
    });
    set_sqlite_provider_json_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one LanceDB JSON provider callback for cross-language host integration.
/// 为跨语言宿主集成注册或清理一个 LanceDB JSON provider 回调。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_lancedb_provider_json_callback(
    callback: Option<FfiJsonProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request_json: &str| {
            invoke_json_provider_callback(callback_fn, user_data, request_json)
        }) as crate::host::database::RuntimeLanceDbProviderJsonCallback
    });
    set_lancedb_provider_json_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one host-tool JSON callback for Lua `vulcan.host.*` integration.
/// 为 Lua `vulcan.host.*` 集成注册或清理一个宿主工具 JSON 回调。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_host_tool_json_callback(
    callback: Option<FfiJsonProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request: &RuntimeHostToolRequest| {
            let request_json = serde_json::to_string(request)
                .map_err(|error| format!("host tool request JSON encode failed: {}", error))?;
            let response_json =
                invoke_json_provider_callback(callback_fn, user_data, &request_json)?;
            serde_json::from_str::<Value>(&response_json)
                .map_err(|error| format!("host tool response JSON decode failed: {}", error))
        }) as RuntimeHostToolCallback
    });
    set_host_tool_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one skill-operation progress JSON callback for host UI integration.
/// 为宿主 UI 集成注册或清理一个技能操作进度 JSON 回调。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_skill_operation_progress_json_callback(
    callback: Option<FfiJsonProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |event: &RuntimeSkillOperationProgressEvent| {
            if let Ok(request_json) = serde_json::to_string(event) {
                let _ = invoke_json_provider_callback(callback_fn, user_data, &request_json);
            }
        }) as RuntimeSkillOperationProgressCallback
    });
    set_skill_operation_progress_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one model embedding JSON callback for Lua `vulcan.models.embed`.
/// 为 Lua `vulcan.models.embed` 注册或清理一个模型 embedding JSON callback。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_model_embed_json_callback(
    callback: Option<FfiJsonProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request: &RuntimeModelEmbedRequest| {
            let request_json = serde_json::to_string(request).map_err(|error| {
                runtime_model_callback_internal_error(format!(
                    "model embed request JSON encode failed: {}",
                    error
                ))
            })?;
            let response_json =
                invoke_json_provider_callback(callback_fn, user_data, &request_json)
                    .map_err(runtime_model_error_from_callback_failure)?;
            runtime_model_embed_response_from_json(&response_json)
        }) as RuntimeModelEmbedCallback
    });
    set_model_embed_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Register or clear one model LLM JSON callback for Lua `vulcan.models.llm`.
/// 为 Lua `vulcan.models.llm` 注册或清理一个模型 LLM JSON callback。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_model_llm_json_callback(
    callback: Option<FfiJsonProviderCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let wrapped = callback.map(|callback_fn| {
        let user_data = user_data as usize;
        std::sync::Arc::new(move |request: &RuntimeModelLlmRequest| {
            let request_json = serde_json::to_string(request).map_err(|error| {
                runtime_model_callback_internal_error(format!(
                    "model llm request JSON encode failed: {}",
                    error
                ))
            })?;
            let response_json =
                invoke_json_provider_callback(callback_fn, user_data, &request_json)
                    .map_err(runtime_model_error_from_callback_failure)?;
            runtime_model_llm_response_from_json(&response_json)
        }) as RuntimeModelLlmCallback
    });
    set_model_llm_callback(wrapped);
    ffi_ok_status(error_out)
}

/// Free one string array result allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个字符串数组结果。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_string_array_free(value: *mut FfiStringArray) {
    if value.is_null() {
        return;
    }
    let value = unsafe { Box::from_raw(value) };
    unsafe { free_string_array_parts(value.items, value.len) };
}

/// Free one entry descriptor list allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个入口描述列表。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_entry_list_free(value: *mut FfiRuntimeEntryDescriptorList) {
    if value.is_null() {
        return;
    }
    let value = unsafe { Box::from_raw(value) };
    if let Some(items) = unsafe { take_ffi_boxed_slice(value.items, value.len) } {
        for item in items {
            unsafe { free_entry_descriptor(item) };
        }
    }
}

/// Free one help descriptor list allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个帮助描述列表。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_help_list_free(
    value: *mut FfiRuntimeSkillHelpDescriptorList,
) {
    if value.is_null() {
        return;
    }
    let value = unsafe { Box::from_raw(value) };
    if let Some(items) = unsafe { take_ffi_boxed_slice(value.items, value.len) } {
        for item in items {
            unsafe { luaskills_ffi_buffer_free(item.skill_id) };
            unsafe { luaskills_ffi_buffer_free(item.skill_name) };
            unsafe { luaskills_ffi_buffer_free(item.skill_version) };
            unsafe { luaskills_ffi_buffer_free(item.root_name) };
            unsafe { luaskills_ffi_buffer_free(item.skill_dir) };
            unsafe { free_help_node_descriptor(item.main) };
            if let Some(flows) = unsafe { take_ffi_boxed_slice(item.flows, item.flows_len) } {
                for flow in flows {
                    unsafe { free_help_node_descriptor(flow) };
                }
            }
        }
    }
}

/// Free one help detail allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个帮助详情。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_help_detail_free(value: *mut FfiRuntimeHelpDetail) {
    if value.is_null() {
        return;
    }
    let value = unsafe { *Box::from_raw(value) };
    unsafe { luaskills_ffi_buffer_free(value.skill_id) };
    unsafe { luaskills_ffi_buffer_free(value.skill_name) };
    unsafe { luaskills_ffi_buffer_free(value.skill_version) };
    unsafe { luaskills_ffi_buffer_free(value.root_name) };
    unsafe { luaskills_ffi_buffer_free(value.skill_dir) };
    unsafe { luaskills_ffi_buffer_free(value.flow_name) };
    unsafe { luaskills_ffi_buffer_free(value.description) };
    unsafe { free_string_array_parts(value.related_entries, value.related_entries_len) };
    unsafe { luaskills_ffi_buffer_free(value.content_type) };
    unsafe { luaskills_ffi_buffer_free(value.content) };
}

/// Free one invocation result allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个调用结果。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_invocation_result_free(
    value: *mut FfiRuntimeInvocationResult,
) {
    if value.is_null() {
        return;
    }
    let value = unsafe { *Box::from_raw(value) };
    unsafe { luaskills_ffi_buffer_free(value.content) };
    unsafe { luaskills_ffi_buffer_free(value.template_hint) };
    if !value.host_result.is_null() {
        let host_result = unsafe { *Box::from_raw(value.host_result) };
        unsafe { luaskills_ffi_buffer_free(host_result.kind) };
        unsafe { luaskills_ffi_buffer_free(host_result.payload_json) };
    }
}

/// Free one install or update result allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个安装或更新结果。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_apply_result_free(value: *mut FfiSkillApplyResult) {
    if value.is_null() {
        return;
    }
    let value = unsafe { *Box::from_raw(value) };
    unsafe { luaskills_ffi_buffer_free(value.skill_id) };
    unsafe { luaskills_ffi_buffer_free(value.status) };
    unsafe { luaskills_ffi_buffer_free(value.message) };
    unsafe { luaskills_ffi_buffer_free(value.version) };
    unsafe { luaskills_ffi_buffer_free(value.source_locator) };
}

/// Free one uninstall result allocated by the standard FFI layer.
/// 释放由标准 FFI 层分配的单个卸载结果。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_uninstall_result_free(
    value: *mut FfiSkillUninstallResult,
) {
    if value.is_null() {
        return;
    }
    let value = unsafe { *Box::from_raw(value) };
    unsafe { luaskills_ffi_buffer_free(value.skill_id) };
    unsafe { luaskills_ffi_buffer_free(value.message) };
}

/// Return the stable FFI version string through the standard C ABI surface.
/// 通过标准 C ABI 接口返回稳定的 FFI 版本字符串。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_version(
    version_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(version_out);
    if version_out.is_null() {
        return ffi_error_status(error_out, "version_out must not be null");
    }
    unsafe { *version_out = alloc_owned_buffer_from_string(crate::ffi::FFI_VERSION) };
    ffi_ok_status(error_out)
}

/// Return the exported FFI entrypoint names through the standard C ABI surface.
/// 通过标准 C ABI 接口返回已导出 FFI 入口点名称。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_describe(
    functions_out: *mut *mut FfiStringArray,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(functions_out);
    if functions_out.is_null() {
        return ffi_error_status(error_out, "functions_out must not be null");
    }
    let values = crate::ffi::exported_ffi_function_names();
    unsafe { *functions_out = Box::into_raw(Box::new(alloc_string_array(&values))) };
    ffi_ok_status(error_out)
}

/// Create one runtime engine through the standard C ABI surface.
/// 通过标准 C ABI 接口创建单个运行时引擎。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_engine_new(
    options: *const FfiLuaEngineOptions,
    engine_id_out: *mut u64,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_u64(engine_id_out);
    if options.is_null() {
        return ffi_error_status(error_out, "options must not be null");
    }
    if engine_id_out.is_null() {
        return ffi_error_status(error_out, "engine_id_out must not be null");
    }
    let options = match parse_engine_options(unsafe { &*options }) {
        Ok(options) => options,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match LuaEngine::new(options) {
        Ok(engine) => {
            let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut registry = lock_ffi_engine_registry();
            registry.insert(engine_id, crate::ffi::FfiEngineSlot::new(engine));
            unsafe { *engine_id_out = engine_id };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error.to_string()),
    }
}

/// Create one runtime engine through the standard C ABI v2 surface.
/// 通过标准 C ABI v2 接口创建单个运行时引擎。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_engine_new_v2(
    options: *const FfiLuaEngineOptionsV2,
    engine_id_out: *mut u64,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_u64(engine_id_out);
    if options.is_null() {
        return ffi_error_status(error_out, "options must not be null");
    }
    if engine_id_out.is_null() {
        return ffi_error_status(error_out, "engine_id_out must not be null");
    }
    let options = match parse_engine_options_v2(unsafe { &*options }) {
        Ok(options) => options,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match LuaEngine::new(options) {
        Ok(engine) => {
            let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut registry = lock_ffi_engine_registry();
            registry.insert(engine_id, crate::ffi::FfiEngineSlot::new(engine));
            unsafe { *engine_id_out = engine_id };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error.to_string()),
    }
}

/// Create one runtime engine through the standard C ABI v3 surface.
/// 通过标准 C ABI v3 接口创建单个运行时引擎。
/// # Safety
/// # 安全性
/// `options`, `engine_id_out`, and `error_out` must satisfy the LuaSkills C ABI pointer contracts;
/// every nested string pointer must remain readable for the complete call.
/// `options`、`engine_id_out` 与 `error_out` 必须满足 LuaSkills C ABI 指针契约；全部嵌套字符串
/// 指针必须在完整调用期间保持可读。
///
/// Returns zero and writes a nonzero engine id on success, or returns nonzero with a LuaSkills-owned
/// UTF-8 error buffer that the caller must free.
/// 成功时返回零并写入非零引擎标识；失败时返回非零及一段调用方必须释放的 LuaSkills 所有 UTF-8
/// 错误缓冲。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_engine_new_v3(
    options: *const FfiLuaEngineOptionsV3,
    engine_id_out: *mut u64,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_u64(engine_id_out);
    if options.is_null() {
        return ffi_error_status(error_out, "options must not be null");
    }
    if engine_id_out.is_null() {
        return ffi_error_status(error_out, "engine_id_out must not be null");
    }
    let options = match parse_engine_options_v3(unsafe { &*options }) {
        Ok(options) => options,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match LuaEngine::new(options) {
        Ok(engine) => {
            let engine_id = FFI_ENGINE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut registry = lock_ffi_engine_registry();
            registry.insert(engine_id, crate::ffi::FfiEngineSlot::new(engine));
            unsafe { *engine_id_out = engine_id };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error.to_string()),
    }
}

/// Free one runtime engine through the standard C ABI surface.
/// 通过标准 C ABI 接口释放单个运行时引擎。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_engine_free(
    engine_id: u64,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    // Removed slot whose callback-quiescing engine teardown must run outside the registry lock.
    // 已移除的槽；其等待回调收敛的引擎清理必须在注册表锁外运行。
    let removed_slot = match remove_ffi_engine_slot(engine_id) {
        Some(removed_slot) => removed_slot,
        None => return ffi_error_status(error_out, format!("FFI engine {} not found", engine_id)),
    };
    destroy_removed_ffi_engine_slot(removed_slot);
    ffi_ok_status(error_out)
}

/// Load skills from one ordered root chain through the standard C ABI surface.
/// 通过标准 C ABI 接口从一条有序根链加载技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_load_from_roots(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .load_from_roots(&skill_roots)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Reload skills from one ordered root chain through the standard C ABI surface.
/// 通过标准 C ABI 接口从一条有序根链重载技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_reload_from_roots(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .reload_from_roots(&skill_roots)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// List runtime entries visible to one host-injected authority through the standard C ABI surface.
/// 通过标准 C ABI 接口列出单个宿主注入权限可见的运行时入口。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_list_entries(
    engine_id: u64,
    authority: i32,
    entries_out: *mut *mut FfiRuntimeEntryDescriptorList,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(entries_out);
    if entries_out.is_null() {
        return ffi_error_status(error_out, "entries_out must not be null");
    }
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.list_entries_for_authority(authority)
    }) {
        Ok(entries) => match alloc_entry_descriptor_list(&entries) {
            Ok(list) => {
                unsafe { *entries_out = Box::into_raw(Box::new(list)) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(error_out, error),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// List runtime help trees visible to one host-injected authority through the standard C ABI surface.
/// 通过标准 C ABI 接口列出单个宿主注入权限可见的运行时帮助树。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_list_skill_help(
    engine_id: u64,
    authority: i32,
    help_out: *mut *mut FfiRuntimeSkillHelpDescriptorList,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(help_out);
    if help_out.is_null() {
        return ffi_error_status(error_out, "help_out must not be null");
    }
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.list_skill_help_for_authority(authority)
    }) {
        Ok(help_descriptors) => {
            // Exact top-level help array matching the shared boxed-slice free contract.
            // 与共享 boxed-slice 释放契约匹配的精确顶层帮助数组。
            let items: Vec<FfiRuntimeSkillHelpDescriptor> =
                help_descriptors.iter().map(alloc_help_descriptor).collect();
            let (items, len) = alloc_ffi_boxed_slice(items);
            let list = FfiRuntimeSkillHelpDescriptorList { items, len };
            unsafe { *help_out = Box::into_raw(Box::new(list)) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Render one help detail visible to one host-injected authority through the standard C ABI surface.
/// 通过标准 C ABI 接口渲染单个宿主注入权限可见的帮助详情。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_render_skill_help_detail(
    engine_id: u64,
    authority: i32,
    skill_id: *const c_char,
    flow_name: *const c_char,
    request_context_json: FfiBorrowedBuffer,
    detail_out: *mut *mut FfiRuntimeHelpDetail,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(detail_out);
    if detail_out.is_null() {
        return ffi_error_status(error_out, "detail_out must not be null");
    }
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let flow_name = match parse_required_string(flow_name, "flow_name") {
        Ok(flow_name) => flow_name,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let request_context =
        match parse_request_context_buffer(&request_context_json, "request_context_json") {
            Ok(request_context) => request_context,
            Err(error) => return ffi_error_status(error_out, error),
        };
    match with_engine(engine_id, |engine| {
        engine.render_skill_help_detail_for_authority(
            authority,
            &skill_id,
            &flow_name,
            request_context.as_ref(),
        )
    }) {
        Ok(Some(detail)) => {
            unsafe { *detail_out = Box::into_raw(Box::new(alloc_help_detail(&detail))) };
            ffi_ok_status(error_out)
        }
        Ok(None) => ffi_error_status(error_out, "Requested help detail was not found"),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Resolve prompt argument completions through the standard C ABI surface.
/// 通过标准 C ABI 接口解析提示词参数补全项。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_prompt_argument_completions(
    engine_id: u64,
    authority: i32,
    prompt_name: *const c_char,
    argument_name: *const c_char,
    values_out: *mut *mut FfiStringArray,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(values_out);
    if values_out.is_null() {
        return ffi_error_status(error_out, "values_out must not be null");
    }
    let prompt_name = match parse_required_string(prompt_name, "prompt_name") {
        Ok(prompt_name) => prompt_name,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let argument_name = match parse_required_string(argument_name, "argument_name") {
        Ok(argument_name) => argument_name,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        Ok(engine.prompt_argument_completions_for_authority(
            authority,
            &prompt_name,
            &argument_name,
        ))
    }) {
        Ok(Some(values)) => {
            unsafe { *values_out = Box::into_raw(Box::new(alloc_string_array(&values))) };
            ffi_ok_status(error_out)
        }
        Ok(None) => {
            unsafe { *values_out = Box::into_raw(Box::new(alloc_string_array(&[]))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Check whether one tool belongs to a visible Lua skill through the standard C ABI surface.
/// 通过标准 C ABI 接口检查单个工具是否属于可见 Lua 技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_is_skill(
    engine_id: u64,
    authority: i32,
    tool_name: *const c_char,
    value_out: *mut u8,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_u8(value_out);
    if value_out.is_null() {
        return ffi_error_status(error_out, "value_out must not be null");
    }
    let tool_name = match parse_required_string(tool_name, "tool_name") {
        Ok(tool_name) => tool_name,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.is_skill_for_authority(authority, &tool_name)
    }) {
        Ok(value) => {
            unsafe { *value_out = u8::from(value) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Resolve the visible owning skill id of one tool through the standard C ABI surface.
/// 通过标准 C ABI 接口解析单个工具可见的所属技能标识符。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_name_for_tool(
    engine_id: u64,
    authority: i32,
    tool_name: *const c_char,
    skill_id_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(skill_id_out);
    if skill_id_out.is_null() {
        return ffi_error_status(error_out, "skill_id_out must not be null");
    }
    let tool_name = match parse_required_string(tool_name, "tool_name") {
        Ok(tool_name) => tool_name,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.skill_name_for_tool_for_authority(authority, &tool_name)
    }) {
        Ok(skill_id) => {
            unsafe { *skill_id_out = alloc_optional_owned_buffer_from_string(skill_id.as_deref()) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// List flattened skill config records through the standard C ABI surface.
/// 通过标准 C ABI 接口列出扁平化技能配置记录。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_list(
    engine_id: u64,
    skill_id: *const c_char,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let skill_id = match parse_optional_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.list_skill_config_entries(skill_id.as_deref())
    }) {
        Ok(entries) => match serde_json::to_string(&entries) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill config entries: {}",
                    error
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Describe effective package configuration declarations through the standard C ABI.
/// 通过标准 C ABI 描述有效技能包配置声明。
///
/// `skill_id` may be null. `include_values` must be exactly zero or one.
/// `skill_id` 可以为空指针；`include_values` 必须严格为零或一。
///
/// The host owns authorization for value disclosure; requested values are never masked here.
/// 宿主负责配置值披露授权；此处绝不遮罩已请求的值。
///
/// # Safety
/// # 安全性
///
/// Optional strings must be null or valid UTF-8 C strings, and output pointers must be writable.
/// 可选字符串必须为空指针或合法 UTF-8 C 字符串，输出指针必须可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_describe(
    engine_id: u64,
    skill_id: *const c_char,
    include_values: u8,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    if include_values > 1 {
        return ffi_error_status(error_out, "include_values must be 0 or 1");
    }
    let skill_id = match parse_optional_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.describe_skill_package_config(skill_id.as_deref(), include_values == 1)
    }) {
        Ok(descriptors) => match serde_json::to_string(&descriptors) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill package configuration descriptors: {}",
                    error
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Validate one effective package configuration through the standard C ABI.
/// 通过标准 C ABI 校验单个有效技能包配置。
///
/// The operation is read-only and returns one JSON status object.
/// 当前操作只读并返回单个 JSON 状态对象。
///
/// # Safety
/// # 安全性
///
/// `skill_id` must be a valid UTF-8 C string and output pointers must be writable.
/// `skill_id` 必须是合法 UTF-8 C 字符串，输出指针必须可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_validate(
    engine_id: u64,
    skill_id: *const c_char,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.validate_skill_package_config(&skill_id)
    }) {
        Ok(status) => match serde_json::to_string(&status) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill package configuration status: {}",
                    error
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Read one optional skill config value through the standard C ABI surface.
/// 通过标准 C ABI 接口读取单个可选技能配置值。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_get(
    engine_id: u64,
    skill_id: *const c_char,
    key: *const c_char,
    value_out: *mut FfiOwnedBuffer,
    found_out: *mut u8,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(value_out);
    clear_out_u8(found_out);
    if value_out.is_null() {
        return ffi_error_status(error_out, "value_out must not be null");
    }
    if found_out.is_null() {
        return ffi_error_status(error_out, "found_out must not be null");
    }
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let key = match parse_required_string(key, "key") {
        Ok(key) => key,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.get_skill_config_value(&skill_id, &key)
    }) {
        Ok(value) => {
            unsafe {
                *found_out = u8::from(value.is_some());
                *value_out = alloc_optional_owned_buffer_from_string(value.as_deref());
            }
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Atomically insert or replace one package configuration batch through the standard C ABI.
/// 通过标准 C ABI 原子插入或替换单个技能包配置批次。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_set_values(
    engine_id: u64,
    skill_id: *const c_char,
    values_json: *const c_char,
    expected_revision: *const c_char,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let values_json = match parse_required_string(values_json, "values_json") {
        Ok(values_json) => values_json,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let values = match serde_json::from_str::<StrictSkillConfigInputValues>(&values_json) {
        Ok(values) => values.0,
        Err(error) => {
            return ffi_error_status(
                error_out,
                format!("values_json must be one typed JSON object: {}", error),
            );
        }
    };
    let expected_revision = match parse_optional_string(expected_revision, "expected_revision") {
        Ok(expected_revision) => expected_revision,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine.set_skill_config_values(&skill_id, values, expected_revision.as_deref())
    }) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill config write result: {}",
                    error
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Delete one skill config key through the standard C ABI surface.
/// 通过标准 C ABI 接口删除单个技能配置键。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_delete(
    engine_id: u64,
    skill_id: *const c_char,
    key: *const c_char,
    expected_revision: *const c_char,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let key = match parse_required_string(key, "key") {
        Ok(key) => key,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let expected_revision = match parse_optional_string(expected_revision, "expected_revision") {
        Ok(expected_revision) => expected_revision,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine.delete_skill_config_value(&skill_id, &key, expected_revision.as_deref())
    }) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill config delete result: {}",
                    error
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Explicitly refresh one selected skill configuration store or both stores.
/// 显式刷新一个选定技能配置存储或两个存储。
/// # Safety
/// # 安全性
/// Optional strings must be null or valid UTF-8 C strings, and output pointers must be writable.
/// 可选字符串必须为空指针或合法 UTF-8 C 字符串，输出指针必须可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_refresh(
    engine_id: u64,
    store_scope: *const c_char,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let store_scope = match parse_optional_string(store_scope, "store_scope") {
        Ok(store_scope) => store_scope,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.refresh_skill_config(store_scope.as_deref())
    }) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill config refresh result: {error}"
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Poll ordered skill configuration events through the standard C ABI.
/// 通过标准 C ABI 轮询有序技能配置事件。
/// # Safety
/// # 安全性
/// Optional strings must be null or valid UTF-8 C strings, and output pointers must be writable.
/// 可选字符串必须为空指针或合法 UTF-8 C 字符串，输出指针必须可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_skill_config_events_poll(
    engine_id: u64,
    after_sequence: *const c_char,
    limit: u64,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let after_sequence = match parse_optional_string(after_sequence, "after_sequence") {
        Ok(after_sequence) => after_sequence,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let limit = match usize::try_from(limit) {
        Ok(limit) => limit,
        Err(_) => {
            return ffi_error_status(
                error_out,
                "event poll limit is outside the platform usize range",
            );
        }
    };
    match with_engine(engine_id, |engine| {
        engine.poll_skill_config_events(after_sequence.as_deref(), limit)
    }) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!(
                    "CONFIG_DECLARATION_INVALID: failed to serialize skill config event batch: {error}"
                ),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Call one loaded skill entry through the standard C ABI surface.
/// 通过标准 C ABI 接口调用单个已加载技能入口。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_call_skill(
    engine_id: u64,
    tool_name: *const c_char,
    args_json: FfiBorrowedBuffer,
    invocation_context: *const FfiLuaInvocationContext,
    result_out: *mut *mut FfiRuntimeInvocationResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    let tool_name = match parse_required_string(tool_name, "tool_name") {
        Ok(tool_name) => tool_name,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let args = match parse_json_value_or_empty_object_buffer(&args_json, "args_json") {
        Ok(args) => args,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let invocation_context = match parse_invocation_context(invocation_context) {
        Ok(invocation_context) => invocation_context,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.call_skill(&tool_name, &args, invocation_context.as_ref())
    }) {
        Ok(result) => match alloc_invocation_result(&result) {
            Ok(ffi_result) => {
                unsafe { *result_out = Box::into_raw(Box::new(ffi_result)) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(error_out, error),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Execute arbitrary Lua code through the standard C ABI surface.
/// 通过标准 C ABI 接口执行任意 Lua 代码。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_run_lua(
    engine_id: u64,
    code: *const c_char,
    args_json: FfiBorrowedBuffer,
    invocation_context: *const FfiLuaInvocationContext,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    let code = match parse_required_string(code, "code") {
        Ok(code) => code,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let args = match parse_json_value_or_empty_object_buffer(&args_json, "args_json") {
        Ok(args) => args,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let invocation_context = match parse_invocation_context(invocation_context) {
        Ok(invocation_context) => invocation_context,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine(engine_id, |engine| {
        engine.run_lua(&code, &args, invocation_context.as_ref())
    }) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(result_json) => {
                unsafe { *result_json_out = alloc_owned_buffer_from_string(result_json) };
                ffi_ok_status(error_out)
            }
            Err(error) => ffi_error_status(
                error_out,
                format!("Failed to serialize Lua result: {}", error),
            ),
        },
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Create one public runtime lease through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷创建一个公开运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_create(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.create_runtime_lease_json(request_json),
    )
}

/// Evaluate one public runtime lease through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷执行一个公开运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_eval(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.eval_runtime_lease_json(request_json),
    )
}

/// Return one public runtime lease status through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷返回一个公开运行时租约状态。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_status(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.runtime_lease_status_json(request_json),
    )
}

/// List public runtime leases through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷列出公开运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_list(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.list_runtime_leases_json(request_json),
    )
}

/// Close one public runtime lease through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷关闭一个公开运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_runtime_lease_close(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.close_runtime_lease_json(request_json),
    )
}

/// Create one `system_lua_lib` runtime lease through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷创建一个 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_create(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.create_system_runtime_lease_json(request_json),
    )
}

/// Evaluate one `system_lua_lib` runtime lease through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷执行一个 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_eval(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.eval_system_runtime_lease_json(request_json),
    )
}

/// Return one `system_lua_lib` runtime lease status through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷返回一个 `system_lua_lib` 运行时租约状态。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_status(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.system_runtime_lease_status_json(request_json),
    )
}

/// List `system_lua_lib` runtime leases through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷列出 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_list(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.list_system_runtime_leases_json(request_json),
    )
}

/// Close one `system_lua_lib` runtime lease through the standard C ABI surface using one JSON request payload.
/// 通过标准 C ABI 接口使用一段 JSON 请求载荷关闭一个 `system_lua_lib` 运行时租约。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_runtime_lease_close(
    engine_id: u64,
    request_json: FfiBorrowedBuffer,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    run_engine_json_text_call(
        engine_id,
        &request_json,
        result_json_out,
        error_out,
        "request_json",
        |engine, request_json| engine.close_system_runtime_lease_json(request_json),
    )
}

/// Poll one bounded batch of engine-level managed-session events through the standard C ABI.
/// 通过标准 C ABI 轮询一批有界的引擎级受管会话事件。
///
/// `engine_id` identifies the engine and `max_events` is the positive destructive-drain limit.
/// `engine_id` 标识目标引擎，`max_events` 是破坏性排空的正数上限。
///
/// `result_json_out` receives direct batch JSON; `error_out` receives an owned diagnostic on failure.
/// `result_json_out` 接收直接批次 JSON；失败时 `error_out` 接收拥有型诊断信息。
///
/// Return zero on success or one on validation, lookup, center-state, or serialization failure.
/// 成功返回零；校验、查找、事件中心状态或序列化失败返回一。
///
/// # Safety
/// # 安全性
/// Writable output pointers must remain valid for the duration of this call.
/// 可写输出指针必须在本次调用期间保持有效。
/// Returned LuaSkills-owned buffers must be released with `luaskills_ffi_buffer_free`.
/// 返回的 LuaSkills 所有缓冲必须使用 `luaskills_ffi_buffer_free` 释放。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_managed_session_events_poll(
    engine_id: u64,
    max_events: usize,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    // Detached event center acquired before the destructive nonblocking drain.
    // 在破坏性非阻塞排空前获取的分离式事件中心。
    let event_center = match clone_managed_session_event_center(engine_id) {
        Ok(event_center) => event_center,
        Err(error) => return ffi_error_status(error_out, error),
    };
    write_managed_session_event_batch(event_center.poll(max_events), result_json_out, error_out)
}

/// Wait for one bounded batch of engine-level managed-session events through the standard C ABI.
/// 通过标准 C ABI 等待一批有界的引擎级受管会话事件。
///
/// `engine_id` identifies the engine, `max_events` bounds the drain, and `timeout_ms` is finite.
/// `engine_id` 标识目标引擎，`max_events` 限制排空数量，`timeout_ms` 是有限超时。
///
/// `result_json_out` receives direct batch JSON; `error_out` receives an owned diagnostic on failure.
/// `result_json_out` 接收直接批次 JSON；失败时 `error_out` 接收拥有型诊断信息。
///
/// Return zero for an event or timeout batch, or one for an explicit error.
/// 返回事件或超时批次时为零；发生显式错误时为一。
///
/// # Safety
/// # 安全性
/// Writable output pointers must remain valid until this potentially blocking call returns.
/// 可写输出指针必须保持有效，直至这个潜在阻塞调用返回。
/// Returned LuaSkills-owned buffers must be released with `luaskills_ffi_buffer_free`.
/// 返回的 LuaSkills 所有缓冲必须使用 `luaskills_ffi_buffer_free` 释放。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_managed_session_events_wait(
    engine_id: u64,
    max_events: usize,
    timeout_ms: u64,
    result_json_out: *mut FfiOwnedBuffer,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_buffer(result_json_out);
    if result_json_out.is_null() {
        return ffi_error_status(error_out, "result_json_out must not be null");
    }
    // Detached center cloned before blocking so the global registry and engine locks are released.
    // 在阻塞前克隆分离式事件中心，确保全局注册表锁与引擎锁已经释放。
    let event_center = match clone_managed_session_event_center(engine_id) {
        Ok(event_center) => event_center,
        Err(error) => return ffi_error_status(error_out, error),
    };
    write_managed_session_event_batch(
        event_center.wait(max_events, timeout_ms),
        result_json_out,
        error_out,
    )
}

/// Register, replace, or clear one per-engine managed-session wake callback.
/// 注册、替换或清除单个引擎级受管会话唤醒回调。
///
/// `engine_id` identifies the engine, `callback` may be null to clear, and `user_data` is opaque.
/// `engine_id` 标识目标引擎，`callback` 可为空以清除回调，`user_data` 为不透明值。
///
/// `error_out` receives an owned diagnostic when lookup or quiescent replacement fails.
/// 查找或静默替换失败时，`error_out` 接收拥有型诊断信息。
///
/// Return only after the retired callback and its `user_data` are no longer in flight.
/// 仅在退役回调及其 `user_data` 不再处于在途状态后返回。
///
/// # Safety
/// # 安全性
/// The callback and `user_data` must remain valid until this function later clears or replaces them.
/// 回调与 `user_data` 必须保持有效，直至本函数后续清除或替换它们。
/// Both must support safe access from arbitrary managed-session background threads.
/// 两者都必须支持从任意受管会话后台线程安全访问。
/// They must already be valid on entry because a pending queue may trigger catch-up before return.
/// 它们在进入函数时就必须有效，因为待处理队列可能在返回前触发补偿调用。
/// The callback must not unwind or synchronously reenter Lua execution across the C ABI boundary.
/// 回调不得跨 C ABI 边界展开异常，也不得同步重入 Lua 执行。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_set_managed_session_wake_callback(
    engine_id: u64,
    callback: Option<FfiManagedSessionWakeCallback>,
    user_data: *mut c_void,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    // Detached center ensures catch-up invocation and quiescent replacement do not hold engine locks.
    // 分离式事件中心确保补偿调用与静默替换不会持有引擎锁。
    let event_center = match clone_managed_session_event_center(engine_id) {
        Ok(event_center) => event_center,
        Err(error) => return ffi_error_status(error_out, error),
    };
    // Rust callback bridge retaining only the ABI function, engine id, and opaque pointer value.
    // 仅保留 ABI 函数、引擎标识与不透明指针值的 Rust 回调桥接。
    let wrapped = callback.map(|callback_fn| {
        // Pointer bits stored as a Send-compatible integer until callback invocation.
        // 在回调调用前以 Send 兼容整数形式保存的指针位。
        let user_data = user_data as usize;
        std::sync::Arc::new(move || {
            invoke_managed_session_wake_callback(callback_fn, user_data, engine_id)
        }) as FallibleRuntimeManagedSessionWakeCallback
    });
    match event_center.set_fallible_wake_callback(wrapped) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Disable one skill through one ordered root chain via the standard C ABI surface.
/// 通过标准 C ABI 接口按一条有序根链停用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_disable_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    skill_id: *const c_char,
    reason: *const c_char,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let reason = match parse_optional_string(reason, "reason") {
        Ok(reason) => reason,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .disable_skill_in_roots(&skill_roots, &skill_id, reason.as_deref())
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Disable one skill on the system plane through one ordered root chain.
/// 通过标准 C ABI 接口按一条有序根链在 system 平面停用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_disable_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    authority: i32,
    skill_id: *const c_char,
    reason: *const c_char,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let reason = match parse_optional_string(reason, "reason") {
        Ok(reason) => reason,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .system_disable_skill_in_roots(&skill_roots, authority, &skill_id, reason.as_deref())
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Enable one skill through one ordered root chain via the standard C ABI surface.
/// 通过标准 C ABI 接口按一条有序根链启用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_enable_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    skill_id: *const c_char,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .enable_skill(&skill_roots, &skill_id)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Enable one skill on the system plane through one ordered root chain.
/// 通过标准 C ABI 接口按一条有序根链在 system 平面启用单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_enable_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    authority: i32,
    skill_id: *const c_char,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .system_enable_skill(&skill_roots, authority, &skill_id)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => ffi_ok_status(error_out),
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Uninstall one skill through one ordered root chain via the standard C ABI surface.
/// 通过标准 C ABI 接口按一条有序根链卸载单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_uninstall_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    skill_id: *const c_char,
    options: *const FfiSkillUninstallOptions,
    result_out: *mut *mut FfiSkillUninstallResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let options = parse_uninstall_options(unsafe { options.as_ref() });
    match with_engine_mut(engine_id, |engine| {
        engine
            .uninstall_skill(&skill_roots, &skill_id, &options)
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => {
            unsafe { *result_out = Box::into_raw(Box::new(alloc_skill_uninstall_result(&result))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Uninstall one skill on the system plane through one ordered root chain.
/// 通过标准 C ABI 接口按一条有序根链在 system 平面卸载单个技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_uninstall_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    authority: i32,
    skill_id: *const c_char,
    options: *const FfiSkillUninstallOptions,
    result_out: *mut *mut FfiSkillUninstallResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let skill_id = match parse_required_string(skill_id, "skill_id") {
        Ok(skill_id) => skill_id,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let options = parse_uninstall_options(unsafe { options.as_ref() });
    match with_engine_mut(engine_id, |engine| {
        engine
            .system_uninstall_skill(&skill_roots, authority, &skill_id, &options)
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => {
            unsafe { *result_out = Box::into_raw(Box::new(alloc_skill_uninstall_result(&result))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Install one managed skill through one ordered root chain via the standard C ABI surface.
/// 通过标准 C ABI 接口按一条有序根链安装单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_install_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    request: *const FfiSkillInstallRequest,
    result_out: *mut *mut FfiSkillApplyResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    if request.is_null() {
        return ffi_error_status(error_out, "request must not be null");
    }
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let request = match parse_install_request(unsafe { &*request }) {
        Ok(request) => request,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .install_skill(&skill_roots, &request)
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => {
            unsafe { *result_out = Box::into_raw(Box::new(alloc_skill_apply_result(&result))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Install one managed skill on the system plane through one ordered root chain.
/// 通过标准 C ABI 接口按一条有序根链在 system 平面安装单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_install_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    authority: i32,
    request: *const FfiSkillInstallRequest,
    result_out: *mut *mut FfiSkillApplyResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    if request.is_null() {
        return ffi_error_status(error_out, "request must not be null");
    }
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let request = match parse_install_request(unsafe { &*request }) {
        Ok(request) => request,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .system_install_skill(&skill_roots, authority, &request)
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => {
            unsafe { *result_out = Box::into_raw(Box::new(alloc_skill_apply_result(&result))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Update one managed skill through one ordered root chain via the standard C ABI surface.
/// 通过标准 C ABI 接口按一条有序根链更新单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_update_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    request: *const FfiSkillInstallRequest,
    result_out: *mut *mut FfiSkillApplyResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    if request.is_null() {
        return ffi_error_status(error_out, "request must not be null");
    }
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let request = match parse_install_request(unsafe { &*request }) {
        Ok(request) => request,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .update_skill(&skill_roots, &request)
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => {
            unsafe { *result_out = Box::into_raw(Box::new(alloc_skill_apply_result(&result))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

/// Update one managed skill on the system plane through one ordered root chain.
/// 通过标准 C ABI 接口按一条有序根链在 system 平面更新单个受管技能。
/// # Safety
/// # 安全性
/// The caller must uphold the LuaSkills C ABI contract for every pointer and borrowed buffer used by this function.
/// 调用方必须遵守本函数所用每个指针与借用缓冲的 LuaSkills C ABI 契约。
/// Output slots must be writable, returned LuaSkills-owned allocations must be freed with the matching free function, registered callbacks must remain callable, and callbacks must not unwind across the FFI boundary.
/// 输出槽位必须可写，返回的 LuaSkills 所有分配必须用匹配的释放函数处理，已注册 callback 必须保持可调用，且 callback 不得跨 FFI 边界展开异常。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_system_update_skill(
    engine_id: u64,
    skill_roots: *const FfiRuntimeSkillRoot,
    skill_roots_len: usize,
    authority: i32,
    request: *const FfiSkillInstallRequest,
    result_out: *mut *mut FfiSkillApplyResult,
    error_out: *mut FfiOwnedBuffer,
) -> i32 {
    clear_error_out(error_out);
    clear_out_ptr(result_out);
    if result_out.is_null() {
        return ffi_error_status(error_out, "result_out must not be null");
    }
    if request.is_null() {
        return ffi_error_status(error_out, "request must not be null");
    }
    let skill_roots = match parse_skill_roots(skill_roots, skill_roots_len) {
        Ok(skill_roots) => skill_roots,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let request = match parse_install_request(unsafe { &*request }) {
        Ok(request) => request,
        Err(error) => return ffi_error_status(error_out, error),
    };
    let authority = match parse_skill_management_authority(authority, "authority") {
        Ok(authority) => authority,
        Err(error) => return ffi_error_status(error_out, error),
    };
    match with_engine_mut(engine_id, |engine| {
        engine
            .system_update_skill(&skill_roots, authority, &request)
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => {
            unsafe { *result_out = Box::into_raw(Box::new(alloc_skill_apply_result(&result))) };
            ffi_ok_status(error_out)
        }
        Err(error) => ffi_error_status(error_out, error),
    }
}

#[cfg(test)]
mod tests;
