use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use libloading::Library;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_uchar};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::{decode_non_null_ffi_c_string, decode_provider_last_error_message};
use crate::host::controller::{
    LuaRuntimeSpaceControllerBindingIds, LuaRuntimeSpaceControllerBridge,
};
use crate::host::database::{
    LuaRuntimeDatabaseCallbackMode, LuaRuntimeDatabaseProviderMode, RuntimeDatabaseBindingContext,
    RuntimeDatabaseKind, RuntimeDatabaseProviderCallbacks, RuntimeLanceDbProviderAction,
    RuntimeLanceDbProviderRequest, build_runtime_database_binding_plan,
    require_database_provider_callback_registration,
};
use crate::lua_skill::{SkillLanceDbLogLevel, SkillLanceDbMeta};
use crate::runtime::path::render_host_visible_path;
use crate::runtime_logging::{info as log_info, warn as log_warn};
use crate::runtime_options::LuaRuntimeHostOptions;
use vldb_controller_client::ControllerLanceDbEnableRequest;

/// Forward declaration of the FFI runtime handle used only for raw cross-library pointers.
/// FFI 运行时句柄前置声明，仅用于跨动态库传递裸指针。
#[repr(C)]
struct VldbLancedbRuntimeHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI engine handle used only for raw cross-library pointers.
/// FFI 引擎句柄前置声明，仅用于跨动态库传递裸指针。
#[repr(C)]
struct VldbLancedbEngineHandle {
    _private: [u8; 0],
}

/// Raw LanceDB FFI byte-buffer definition kept identical to the exported dynamic-library header.
/// LanceDB FFI 原始字节缓冲区定义，需与动态库头文件保持一致。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbLancedbByteBuffer {
    data: *mut c_uchar,
    len: usize,
    /// Original allocation capacity used only by the dynamic library when reconstructing the Vec during free.
    /// 原始分配容量，仅供动态库在释放时恢复 Vec 布局。
    cap: usize,
}

/// LanceDB FFI runtime options that must stay ABI-compatible with the exported header.
/// LanceDB FFI 运行时选项，需与导出的头文件严格对齐。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbLancedbRuntimeOptions {
    default_db_path: *const c_char,
    db_root: *const c_char,
    read_consistency_interval_ms: u64,
    has_read_consistency_interval: u8,
    max_upsert_payload: usize,
    max_search_limit: usize,
    max_concurrent_requests: usize,
}

type RuntimeOptionsDefaultFn = unsafe extern "C" fn() -> VldbLancedbRuntimeOptions;
type RuntimeCreateFn =
    unsafe extern "C" fn(VldbLancedbRuntimeOptions) -> *mut VldbLancedbRuntimeHandle;
type RuntimeDestroyFn = unsafe extern "C" fn(*mut VldbLancedbRuntimeHandle);
type RuntimeOpenDefaultEngineFn =
    unsafe extern "C" fn(*mut VldbLancedbRuntimeHandle) -> *mut VldbLancedbEngineHandle;
type RuntimeDatabasePathForNameFn =
    unsafe extern "C" fn(*mut VldbLancedbRuntimeHandle, *const c_char) -> *mut c_char;
type EngineCreateTableJsonFn =
    unsafe extern "C" fn(*mut VldbLancedbEngineHandle, *const c_char) -> *mut c_char;
type EngineVectorUpsertFn = unsafe extern "C" fn(
    *mut VldbLancedbEngineHandle,
    *const c_char,
    *const u8,
    usize,
) -> *mut c_char;
type EngineVectorSearchFn = unsafe extern "C" fn(
    *mut VldbLancedbEngineHandle,
    *const c_char,
    *mut VldbLancedbByteBuffer,
) -> *mut c_char;
type EngineDeleteJsonFn =
    unsafe extern "C" fn(*mut VldbLancedbEngineHandle, *const c_char) -> *mut c_char;
type EngineDropTableJsonFn =
    unsafe extern "C" fn(*mut VldbLancedbEngineHandle, *const c_char) -> *mut c_char;
type EngineDestroyFn = unsafe extern "C" fn(*mut VldbLancedbEngineHandle);
type BytesFreeFn = unsafe extern "C" fn(VldbLancedbByteBuffer);
type StringFreeFn = unsafe extern "C" fn(*mut c_char);
type LastErrorMessageFn = unsafe extern "C" fn() -> *const c_char;
type ClearLastErrorFn = unsafe extern "C" fn();

/// Loaded LanceDB FFI API table that owns the dynamic-library lifetime and exported function pointers.
/// 已加载的 LanceDB FFI API 表，负责持有动态库生命周期与导出函数指针。
struct LoadedLanceDbApi {
    _library: Library,
    library_path: PathBuf,
    runtime_options_default: RuntimeOptionsDefaultFn,
    runtime_create: RuntimeCreateFn,
    runtime_destroy: RuntimeDestroyFn,
    runtime_open_default_engine: RuntimeOpenDefaultEngineFn,
    runtime_database_path_for_name: RuntimeDatabasePathForNameFn,
    engine_create_table_json: EngineCreateTableJsonFn,
    engine_vector_upsert: EngineVectorUpsertFn,
    engine_vector_search: EngineVectorSearchFn,
    engine_delete_json: EngineDeleteJsonFn,
    engine_drop_table_json: EngineDropTableJsonFn,
    engine_destroy: EngineDestroyFn,
    bytes_free: BytesFreeFn,
    string_free: StringFreeFn,
    last_error_message: LastErrorMessageFn,
    clear_last_error: ClearLastErrorFn,
}

/// The loaded library handle and copied function pointers stay immutable after initialization, while callers serialize use via outer mutexes.
/// 动态库句柄与函数指针在初始化后只读，调用端通过外层互斥量串行化访问。
unsafe impl Send for LoadedLanceDbApi {}
unsafe impl Sync for LoadedLanceDbApi {}

impl LoadedLanceDbApi {
    /// Load the LanceDB dynamic library using host conventions, preferring an explicit environment variable and runtime libs directories.
    /// 按宿主约定加载 LanceDB 动态库，优先查找显式环境变量与运行时 libs 目录。
    fn load(library_path: &Path) -> Result<Self, String> {
        // Shared loader performs the only filesystem/load attempt and preserves the native error once.
        // 共享加载器只执行一次文件系统/加载尝试，并仅保留一次原生错误。
        let library =
            unsafe { crate::providers::load_provider_dynamic_library(library_path, "LanceDB") }?;
        unsafe { Self::from_library(library_path.to_path_buf(), library) }
    }

    /// Copy all required exported function pointers from an opened dynamic library and keep the library handle alive.
    /// 从已打开的动态库中复制需要的函数指针，并保留库句柄防止提前卸载。
    unsafe fn from_library(library_path: PathBuf, library: Library) -> Result<Self, String> {
        macro_rules! load_symbol {
            ($name:literal, $ty:ty) => {{
                unsafe {
                    *library
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|error| {
                            format!(
                                "failed to load symbol {} from {}: {}",
                                $name,
                                render_host_visible_path(&library_path),
                                error
                            )
                        })?
                }
            }};
        }

        Ok(Self {
            runtime_options_default: load_symbol!(
                "vldb_lancedb_runtime_options_default",
                RuntimeOptionsDefaultFn
            ),
            runtime_create: load_symbol!("vldb_lancedb_runtime_create", RuntimeCreateFn),
            runtime_destroy: load_symbol!("vldb_lancedb_runtime_destroy", RuntimeDestroyFn),
            runtime_open_default_engine: load_symbol!(
                "vldb_lancedb_runtime_open_default_engine",
                RuntimeOpenDefaultEngineFn
            ),
            runtime_database_path_for_name: load_symbol!(
                "vldb_lancedb_runtime_database_path_for_name",
                RuntimeDatabasePathForNameFn
            ),
            engine_create_table_json: load_symbol!(
                "vldb_lancedb_engine_create_table_json",
                EngineCreateTableJsonFn
            ),
            engine_vector_upsert: load_symbol!(
                "vldb_lancedb_engine_vector_upsert",
                EngineVectorUpsertFn
            ),
            engine_vector_search: load_symbol!(
                "vldb_lancedb_engine_vector_search",
                EngineVectorSearchFn
            ),
            engine_delete_json: load_symbol!("vldb_lancedb_engine_delete_json", EngineDeleteJsonFn),
            engine_drop_table_json: load_symbol!(
                "vldb_lancedb_engine_drop_table_json",
                EngineDropTableJsonFn
            ),
            engine_destroy: load_symbol!("vldb_lancedb_engine_destroy", EngineDestroyFn),
            bytes_free: load_symbol!("vldb_lancedb_bytes_free", BytesFreeFn),
            string_free: load_symbol!("vldb_lancedb_string_free", StringFreeFn),
            last_error_message: load_symbol!("vldb_lancedb_last_error_message", LastErrorMessageFn),
            clear_last_error: load_symbol!("vldb_lancedb_clear_last_error", ClearLastErrorFn),
            _library: library,
            library_path,
        })
    }

    /// Read the latest FFI error text and return it as a stable Rust string.
    /// 读取最近一次 FFI 调用错误文本，并返回稳定 Rust 字符串。
    fn take_last_error_message(&self) -> String {
        unsafe {
            let ptr = (self.last_error_message)();
            let text =
                decode_provider_last_error_message(ptr, "LanceDB", "unknown LanceDB host error");
            (self.clear_last_error)();
            text
        }
    }

    /// Convert a dynamic-library allocated string into a Rust `String` and free the original allocation.
    /// 释放由动态库分配的字符串并转成 Rust `String`。
    fn take_owned_string(&self, ptr: *mut c_char) -> Result<String, String> {
        if ptr.is_null() {
            return Err(self.take_last_error_message());
        }

        unsafe {
            let text = decode_non_null_ffi_c_string(ptr);
            (self.string_free)(ptr);
            text
        }
    }

    /// Convert a dynamic-library allocated byte buffer into a Rust `Vec<u8>` and free the original allocation.
    /// 释放由动态库分配的字节缓冲区并转成 Rust `Vec<u8>`。
    fn take_owned_bytes(&self, buffer: VldbLancedbByteBuffer) -> Vec<u8> {
        if buffer.data.is_null() || buffer.len == 0 {
            return Vec::new();
        }

        unsafe {
            let bytes = std::slice::from_raw_parts(buffer.data, buffer.len).to_vec();
            (self.bytes_free)(buffer);
            bytes
        }
    }
}

/// One skill-scoped LanceDB handle set whose lifetime is managed entirely by the host.
/// 单个 skill 的 LanceDB 句柄集合，由宿主管理其生命周期。
struct SkillHandleState {
    runtime: *mut VldbLancedbRuntimeHandle,
    engine: *mut VldbLancedbEngineHandle,
}

/// Stable provider integration mode used by one LanceDB skill binding.
/// 单个 LanceDB skill 绑定所使用的稳定 provider 集成模式。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LanceDbBindingMode {
    DynamicLibrary,
    HostCallback,
    SpaceController,
}

/// FFI handles are accessed only behind host-side mutexes, and cross-thread sharing is controlled centrally by the host.
/// FFI 句柄只通过宿主互斥量串行访问，跨线程共享由宿主统一控制。
unsafe impl Send for SkillHandleState {}

/// Database context bound to one LanceDB-enabled skill.
/// 某个启用 LanceDB 的 skill 所绑定的数据库上下文。
pub struct LanceDbSkillBinding {
    api: Option<Arc<LoadedLanceDbApi>>,
    skill_name: String,
    skill_dir_name: String,
    database_path: String,
    config: SkillLanceDbMeta,
    provider_mode: LanceDbBindingMode,
    callback_mode: LuaRuntimeDatabaseCallbackMode,
    handles: Option<Mutex<SkillHandleState>>,
    controller: Option<Arc<LuaRuntimeSpaceControllerBridge>>,
    provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    provider_binding: RuntimeDatabaseBindingContext,
}

impl LanceDbSkillBinding {
    /// Return the stable LanceDB status payload for the current skill; the shape stays stable whether enabled or disabled.
    /// 返回当前 skill 的稳定 LanceDB 状态信息；无论启用与否，返回结构都应稳定。
    pub fn status_json(&self) -> Result<Value, String> {
        if self.provider_mode == LanceDbBindingMode::DynamicLibrary && self.api.is_none() {
            return Err(self.missing_dynamic_api_error());
        }
        Ok(json!({
            "enabled": true,
            "initialized": true,
            "skill_name": self.skill_name,
            "skill_dir_name": self.skill_dir_name,
            "database_path": self.database_path,
            "integration_mode": self.integration_mode_name(),
            "library_path": lancedb_library_path_value(self.api.as_deref()),
            "space_label": self.provider_binding.space_label,
            "root_name": self.provider_binding.root_name,
            "binding_tag": self.provider_binding.binding_tag,
            "space_root": self.provider_binding.space_root,
            "default_database_path": self.provider_binding.default_database_path,
            "log_level": self.config.log_level.as_str(),
            "slow_log_enabled": self.config.slow_log_enabled,
            "slow_log_threshold_ms": self.config.slow_log_threshold_ms,
        }))
    }

    /// Return base information about the LanceDB instance bound to the current skill for Lua and diagnostics.
    /// 返回当前 skill 所绑定 LanceDB 的基础信息，供 Lua 或诊断输出使用。
    pub fn info_json(&self) -> Result<Value, String> {
        self.status_json()
    }

    /// Return the diagnostic used when a dynamic binding is missing its loaded API table.
    /// 返回 dynamic binding 缺失已加载 API 表时使用的诊断。
    fn missing_dynamic_api_error(&self) -> String {
        format!(
            "LanceDB dynamic-library API is unavailable for {} binding",
            self.integration_mode_name()
        )
    }

    /// Execute create-table using the host-defined JSON input shape.
    /// 执行建表操作，输入必须符合宿主约定的 JSON 结构。
    pub fn create_table_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let result =
                self.run_controller_binding_operation("create_table", None, |bridge, ids| {
                    let request_json =
                        serde_json::to_string(input).map_err(|error| error.to_string())?;
                    bridge.run(move |client| async move {
                        client
                            .create_lancedb_table(ids.space_id, ids.binding_id, request_json)
                            .await
                    })
                })?;
            return Ok(json!({ "message": result.message }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeLanceDbProviderAction::CreateTable, input)
                .map(|result| result.meta);
        }
        self.call_json_string("create_table", input, |api, state, input_ptr| unsafe {
            (api.engine_create_table_json)(state.engine, input_ptr)
        })
    }

    /// Execute vector upsert; callers must provide an already encoded raw payload.
    /// 执行向量写入；调用方负责提供已经编码好的原始载荷。
    pub fn vector_upsert_json(&self, input: &Value, data: &[u8]) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let result = self.run_controller_binding_operation(
                "vector_upsert",
                Some(format!("payload_bytes={}", data.len())),
                |bridge, ids| {
                    let request_json =
                        serde_json::to_string(input).map_err(|error| error.to_string())?;
                    let payload = data.to_vec();
                    bridge.run(move |client| async move {
                        client
                            .upsert_lancedb(ids.space_id, ids.binding_id, request_json, payload)
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "message": result.message,
                "version": result.version,
                "input_rows": result.input_rows,
                "inserted_rows": result.inserted_rows,
                "updated_rows": result.updated_rows,
                "deleted_rows": result.deleted_rows,
            }));
        }
        if self.is_host_provider_mode() {
            let mut host_input = input.clone();
            if let Some(object) = host_input.as_object_mut() {
                object.insert(
                    "data_base64".to_string(),
                    Value::String(BASE64_STANDARD.encode(data)),
                );
            }
            return self
                .dispatch_host_provider(RuntimeLanceDbProviderAction::VectorUpsert, &host_input)
                .map(|result| result.meta);
        }
        let api = self.api_ref()?;
        let input_text = serde_json::to_string(input).map_err(|error| error.to_string())?;
        let input_cstr = CString::new(input_text)
            .map_err(|_| "input json contains interior NUL bytes".to_string())?;
        self.log_info(
            "vector_upsert",
            Some(format!("payload_bytes={}", data.len())),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        unsafe {
            let response = (api.engine_vector_upsert)(
                guard.engine,
                input_cstr.as_ptr(),
                data.as_ptr(),
                data.len(),
            );
            let text = match api.take_owned_string(response) {
                Ok(text) => text,
                Err(error) => {
                    drop(guard);
                    self.log_warning("vector_upsert", &error);
                    return Err(error);
                }
            };
            // Release the native engine lock before parsing copied JSON in Rust.
            // 在 Rust 侧解析已复制的 JSON 前释放原生引擎锁。
            drop(guard);
            let value = serde_json::from_str(&text).map_err(|error| {
                format!("failed to parse LanceDB upsert response JSON: {}", error)
            })?;
            self.log_if_slow(
                "vector_upsert",
                started_at,
                Some(format!("payload_bytes={}", data.len())),
            );
            Ok(value)
        }
    }

    /// Execute vector search and return both metadata JSON and raw result bytes.
    /// 执行向量检索并返回元信息 JSON 与原始结果字节。
    pub fn vector_search_json(&self, input: &Value) -> Result<(Value, Vec<u8>), String> {
        if self.is_space_controller_mode() {
            self.log_info("vector_search", None);
            let started_at = Instant::now();
            let (bridge, controller_ids) = self.controller_call_context()?;
            let request_json = serde_json::to_string(input).map_err(|error| error.to_string())?;
            let result = bridge.run(move |client| async move {
                client
                    .search_lancedb(
                        controller_ids.space_id,
                        controller_ids.binding_id,
                        request_json,
                    )
                    .await
            })?;
            self.log_if_slow(
                "vector_search",
                started_at,
                Some(format!("result_bytes={}", result.data.len())),
            );
            return Ok((
                json!({
                    "message": result.message,
                    "format": result.format,
                    "rows": result.rows,
                }),
                result.data,
            ));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeLanceDbProviderAction::VectorSearch, input)
                .map(|result| (result.meta, result.bytes));
        }
        let api = self.api_ref()?;
        let input_text = serde_json::to_string(input).map_err(|error| error.to_string())?;
        let input_cstr = CString::new(input_text)
            .map_err(|_| "input json contains interior NUL bytes".to_string())?;
        self.log_info("vector_search", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let mut buffer = VldbLancedbByteBuffer {
            data: ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        unsafe {
            let response =
                (api.engine_vector_search)(guard.engine, input_cstr.as_ptr(), &mut buffer);
            let text = match api.take_owned_string(response) {
                Ok(text) => text,
                Err(error) => {
                    // Free a possible native out-buffer even when the metadata response reports failure.
                    // 即使元信息响应失败，也释放可能已经写入的原生 out-buffer。
                    let _ = api.take_owned_bytes(buffer);
                    drop(guard);
                    self.log_warning("vector_search", &error);
                    return Err(error);
                }
            };
            // Copy and free the native byte buffer before parsing metadata so parse failures cannot leak it.
            // 在解析元信息前复制并释放原生字节缓冲区，避免解析失败导致泄漏。
            let bytes = api.take_owned_bytes(buffer);
            drop(guard);
            let meta: Value = serde_json::from_str(&text).map_err(|error| {
                format!("failed to parse LanceDB search response JSON: {}", error)
            })?;
            self.log_if_slow(
                "vector_search",
                started_at,
                Some(format!("result_bytes={}", bytes.len())),
            );
            Ok((meta, bytes))
        }
    }

    /// Execute delete.
    /// 执行删除操作。
    pub fn delete_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let result = self.run_controller_binding_operation("delete", None, |bridge, ids| {
                let request_json =
                    serde_json::to_string(input).map_err(|error| error.to_string())?;
                bridge.run(move |client| async move {
                    client
                        .delete_lancedb(ids.space_id, ids.binding_id, request_json)
                        .await
                })
            })?;
            return Ok(json!({
                "message": result.message,
                "version": result.version,
                "deleted_rows": result.deleted_rows,
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeLanceDbProviderAction::Delete, input)
                .map(|result| result.meta);
        }
        self.call_json_string("delete", input, |api, state, input_ptr| unsafe {
            (api.engine_delete_json)(state.engine, input_ptr)
        })
    }

    /// Execute drop-table.
    /// 执行删表操作。
    pub fn drop_table_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let result =
                self.run_controller_binding_operation("drop_table", None, |bridge, ids| {
                    let table_name = require_string_field(input, "table_name")?.to_string();
                    bridge.run(move |client| async move {
                        client
                            .drop_lancedb_table(ids.space_id, ids.binding_id, table_name)
                            .await
                    })
                })?;
            return Ok(json!({ "message": result.message }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeLanceDbProviderAction::DropTable, input)
                .map(|result| result.meta);
        }
        self.call_json_string("drop_table", input, |api, state, input_ptr| unsafe {
            (api.engine_drop_table_json)(state.engine, input_ptr)
        })
    }

    /// Execute an FFI call that maps a JSON input into a JSON-string response.
    /// 统一执行“输入 JSON -> 返回 JSON 字符串”的 FFI 调用。
    fn call_json_string<F>(
        &self,
        operation: &str,
        input: &Value,
        invoke: F,
    ) -> Result<Value, String>
    where
        F: Fn(&LoadedLanceDbApi, &SkillHandleState, *const c_char) -> *mut c_char,
    {
        let input_text = serde_json::to_string(input).map_err(|error| error.to_string())?;
        let input_cstr = CString::new(input_text)
            .map_err(|_| "input json contains interior NUL bytes".to_string())?;
        self.log_info(operation, None);
        let started_at = Instant::now();
        let api = self.api_ref()?;
        let guard = self.lock_handles()?;
        let response = invoke(api, &guard, input_cstr.as_ptr());
        let text = match api.take_owned_string(response) {
            Ok(text) => text,
            Err(error) => {
                drop(guard);
                self.log_warning(operation, &error);
                return Err(error);
            }
        };
        // Release the native engine lock before parsing copied JSON in Rust.
        // 在 Rust 侧解析已复制的 JSON 前释放原生引擎锁。
        drop(guard);
        let value = serde_json::from_str(&text)
            .map_err(|error| format!("failed to parse LanceDB response JSON: {}", error))?;
        self.log_if_slow(operation, started_at, None);
        Ok(value)
    }

    /// Run one LanceDB controller operation with shared logging, timing, and binding identifiers.
    /// 使用共享日志、计时与绑定标识执行一次 LanceDB 控制器操作。
    ///
    /// The operation parameter is the stable operation name used by normal and slow logs.
    /// operation 参数是普通日志与慢日志使用的稳定操作名称。
    ///
    /// The slow_extra parameter is the optional detail emitted both before the call and in slow logs.
    /// slow_extra 参数是调用前与慢日志中共同输出的可选细节。
    ///
    /// The invoke parameter performs the provider-specific controller SDK call.
    /// invoke 参数执行 provider 专属的控制器 SDK 调用。
    ///
    /// Return the provider-specific controller response returned by the invoke closure.
    /// 返回 invoke 闭包产出的 provider 专属控制器响应。
    fn run_controller_binding_operation<T, F>(
        &self,
        operation: &str,
        slow_extra: Option<String>,
        invoke: F,
    ) -> Result<T, String>
    where
        F: FnOnce(
            &Arc<LuaRuntimeSpaceControllerBridge>,
            LuaRuntimeSpaceControllerBindingIds,
        ) -> Result<T, String>,
    {
        self.log_info(operation, slow_extra.clone());
        let started_at = Instant::now();
        let (bridge, controller_ids) = self.controller_call_context()?;
        let result = invoke(bridge, controller_ids)?;
        self.log_if_slow(operation, started_at, slow_extra);
        Ok(result)
    }

    /// Emit regular informational logs according to the skill-scoped log policy.
    /// 按 skill 配置输出普通信息级日志。
    fn log_info(&self, operation: &str, extra: Option<String>) {
        if self.config.log_level == SkillLanceDbLogLevel::Info {
            match extra {
                Some(extra) => log_info(format!(
                    "[LanceDb:info] skill={} db={} op={} {}",
                    self.skill_name, self.skill_dir_name, operation, extra
                )),
                None => log_info(format!(
                    "[LanceDb:info] skill={} db={} op={}",
                    self.skill_name, self.skill_dir_name, operation
                )),
            }
        }
    }

    /// Emit a slow-operation warning according to the slow-log policy; this is independent from regular log verbosity.
    /// 按慢日志配置输出耗时告警；该日志与普通日志开关独立。
    fn log_if_slow(&self, operation: &str, started_at: Instant, extra: Option<String>) {
        if !self.config.slow_log_enabled {
            return;
        }

        let elapsed_ms = started_at.elapsed().as_millis() as u64;
        if elapsed_ms < self.config.slow_log_threshold_ms {
            return;
        }

        match extra {
            Some(extra) => log_info(format!(
                "[LanceDb:slow] skill={} db={} op={} elapsed_ms={} {}",
                self.skill_name, self.skill_dir_name, operation, elapsed_ms, extra
            )),
            None => log_info(format!(
                "[LanceDb:slow] skill={} db={} op={} elapsed_ms={}",
                self.skill_name, self.skill_dir_name, operation, elapsed_ms
            )),
        }
    }

    /// Emit warning-level logs according to the skill policy, usually for FFI call failures or host-detected anomalies.
    /// 按 skill 配置输出告警级日志，通常用于 FFI 调用失败或宿主检测到的异常情况。
    fn log_warning(&self, operation: &str, message: &str) {
        if matches!(
            self.config.log_level,
            SkillLanceDbLogLevel::Info | SkillLanceDbLogLevel::Warning
        ) {
            log_warn(format!(
                "[LanceDb:warn] skill={} db={} op={} message={}",
                self.skill_name, self.skill_dir_name, operation, message
            ));
        }
    }

    /// Return whether the current binding dispatches requests into one host provider.
    /// 返回当前绑定是否把请求转发给宿主 provider。
    fn is_host_provider_mode(&self) -> bool {
        self.provider_mode == LanceDbBindingMode::HostCallback
    }

    /// Return whether the current binding dispatches requests into one external space controller.
    /// 返回当前绑定是否把请求转发给外部空间控制器。
    fn is_space_controller_mode(&self) -> bool {
        self.provider_mode == LanceDbBindingMode::SpaceController
    }

    /// Return the loaded dynamic-library API for dynamic mode bindings, or an explicit binding-state error.
    /// 返回动态模式绑定所对应的已加载动态库 API；若绑定状态不一致则返回显式错误。
    fn api_ref(&self) -> Result<&LoadedLanceDbApi, String> {
        self.api.as_deref().ok_or_else(|| {
            format!(
                "LanceDB dynamic-library API is unavailable for {} binding",
                self.integration_mode_name()
            )
        })
    }

    /// Return the stable integration mode name for diagnostics and Lua status payloads.
    /// 返回用于诊断和 Lua 状态输出的稳定集成模式名称。
    fn integration_mode_name(&self) -> &'static str {
        match self.provider_mode {
            LanceDbBindingMode::DynamicLibrary => "dynamic_library",
            LanceDbBindingMode::HostCallback => "host_callback",
            LanceDbBindingMode::SpaceController => "space_controller",
        }
    }

    /// Dispatch one LanceDB operation through the host-registered provider contract.
    /// 通过宿主已注册的 provider 协议分发一次 LanceDB 操作。
    fn dispatch_host_provider(
        &self,
        action: RuntimeLanceDbProviderAction,
        input: &Value,
    ) -> Result<crate::host::database::RuntimeLanceDbProviderResult, String> {
        let request = RuntimeLanceDbProviderRequest {
            action,
            binding: self.provider_binding.clone(),
            input: input.clone(),
        };
        self.provider_callbacks
            .dispatch_lancedb_provider_request(&request, self.callback_mode)
    }

    /// Acquire the handle lock so LanceDB FFI calls for the same skill execute serially.
    /// 获取句柄锁，确保同一个 skill 的 LanceDB FFI 调用按顺序串行执行。
    fn lock_handles(&self) -> Result<std::sync::MutexGuard<'_, SkillHandleState>, String> {
        self.handles
            .as_ref()
            .ok_or_else(|| {
                "LanceDB dynamic-library handles are unavailable in host provider mode".to_string()
            })?
            .lock()
            .map_err(|_| "failed to acquire LanceDB handle lock".to_string())
    }

    /// Return the shared controller bridge for one space-controller binding.
    /// 返回 space-controller 绑定所使用的共享控制器桥接。
    fn controller_bridge(&self) -> Result<&Arc<LuaRuntimeSpaceControllerBridge>, String> {
        self.controller
            .as_ref()
            .ok_or_else(|| "LanceDB space-controller bridge is unavailable".to_string())
    }

    /// Return the controller bridge and identifiers required by one controller operation.
    /// 返回一次控制器操作所需的控制器桥接与标识集合。
    fn controller_call_context(
        &self,
    ) -> Result<
        (
            &Arc<LuaRuntimeSpaceControllerBridge>,
            LuaRuntimeSpaceControllerBindingIds,
        ),
        String,
    > {
        let bridge = self.controller_bridge()?;
        let ids = bridge.binding_ids_for_binding(&self.provider_binding);
        Ok((bridge, ids))
    }
}

impl Drop for LanceDbSkillBinding {
    /// The host releases engine and runtime handles together when the skill binding is dropped.
    /// 由宿主在 skill 生命周期结束时统一释放引擎与运行时句柄。
    fn drop(&mut self) {
        let Some(handles) = self.handles.as_ref() else {
            return;
        };
        let Some(api) = self.api.as_ref() else {
            return;
        };
        if let Ok(mut guard) = handles.lock() {
            unsafe {
                if !guard.engine.is_null() {
                    (api.engine_destroy)(guard.engine);
                    guard.engine = ptr::null_mut();
                }
                if !guard.runtime.is_null() {
                    (api.runtime_destroy)(guard.runtime);
                    guard.runtime = ptr::null_mut();
                }
            }
        }
    }
}

/// Maintain skill-scoped LanceDB bindings, auto-creating and reusing them for enabled skills.
/// 按 skill 维度维护 LanceDB 绑定，负责技能启用后的自动创建与长期复用。
pub struct LanceDbSkillHost {
    api: Option<Arc<LoadedLanceDbApi>>,
    controller: Option<Arc<LuaRuntimeSpaceControllerBridge>>,
    skills: Mutex<HashMap<String, Arc<LanceDbSkillBinding>>>,
    provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    host_options: LuaRuntimeHostOptions,
}

/// Resolve the optional LanceDB dynamic-library API for the selected provider mode.
/// 根据选定的 provider 模式解析可选 LanceDB 动态库 API。
///
/// The host_options parameter carries the provider mode and dynamic-library path selected by the host.
/// host_options 参数携带宿主选择的 provider 模式与动态库路径。
///
/// The provider_callbacks parameter is the engine-captured provider callback snapshot used by host-callback mode.
/// provider_callbacks 参数是 host-callback 模式使用的引擎级 provider 回调快照。
///
/// Return a loaded dynamic-library API only when LanceDB runs in dynamic-library mode.
/// 仅当 LanceDB 运行在 dynamic-library 模式时返回已加载的动态库 API。
fn resolve_lancedb_skill_host_api(
    host_options: &LuaRuntimeHostOptions,
    provider_callbacks: &RuntimeDatabaseProviderCallbacks,
) -> Result<Option<Arc<LoadedLanceDbApi>>, String> {
    match host_options.lancedb_provider_mode {
        LuaRuntimeDatabaseProviderMode::DynamicLibrary => {
            let library_path = host_options.lancedb_library_path.clone().ok_or_else(|| {
                "LanceDB dynamic-library mode requires host_options.lancedb_library_path"
                    .to_string()
            })?;
            Ok(Some(Arc::new(LoadedLanceDbApi::load(&library_path)?)))
        }
        LuaRuntimeDatabaseProviderMode::HostCallback => {
            require_database_provider_callback_registration(
                "LanceDB",
                host_options.lancedb_callback_mode,
                provider_callbacks
                    .has_lancedb_provider_callback_for_mode(host_options.lancedb_callback_mode),
            )?;
            Ok(None)
        }
        LuaRuntimeDatabaseProviderMode::SpaceController => Ok(None),
    }
}

/// Resolve the optional LanceDB space-controller bridge for the selected provider mode.
/// 根据选定的 provider 模式解析可选 LanceDB space-controller 桥接。
///
/// The host_options parameter carries the provider mode and controller connection settings.
/// host_options 参数携带 provider 模式与控制器连接设置。
///
/// Return a controller bridge only when LanceDB runs in space-controller mode.
/// 仅当 LanceDB 运行在 space-controller 模式时返回控制器桥接。
fn resolve_lancedb_skill_host_controller(
    host_options: &LuaRuntimeHostOptions,
) -> Result<Option<Arc<LuaRuntimeSpaceControllerBridge>>, String> {
    match host_options.lancedb_provider_mode {
        LuaRuntimeDatabaseProviderMode::SpaceController => Ok(Some(
            LuaRuntimeSpaceControllerBridge::new(host_options, "lancedb")?,
        )),
        _ => Ok(None),
    }
}

impl LanceDbSkillHost {
    /// Create the host-side LanceDB skill manager and resolve resources for the selected provider mode.
    /// 创建宿主级 LanceDB 技能管理器，并解析所选 provider 模式需要的资源。
    pub fn new(
        host_options: LuaRuntimeHostOptions,
        provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    ) -> Result<Self, String> {
        let api = resolve_lancedb_skill_host_api(&host_options, provider_callbacks.as_ref())?;
        let controller = resolve_lancedb_skill_host_controller(&host_options)?;
        Ok(Self {
            api,
            controller,
            skills: Mutex::new(HashMap::new()),
            provider_callbacks,
            host_options,
        })
    }

    /// Register the fixed database binding for a LanceDB-enabled skill; each skill is created only once.
    /// 为启用 LanceDB 的 skill 注册固定数据库绑定；同一个 skill 只会创建一次。
    pub fn register_skill(
        &self,
        root_name: &str,
        skill_name: &str,
        skill_dir: &Path,
        config: SkillLanceDbMeta,
    ) -> Result<Arc<LanceDbSkillBinding>, String> {
        let mut guard = self.lock_skills();
        if let Some(existing) = guard.get(skill_name) {
            return Ok(existing.clone());
        }

        let binding_plan = build_runtime_database_binding_plan(
            root_name,
            skill_name,
            skill_dir,
            self.host_options.database_dir_name.as_str(),
            RuntimeDatabaseKind::LanceDb,
        )?;
        let skill_dir_name = binding_plan.skill_dir_name;
        let db_path = binding_plan.provider_storage_dir;
        let database_path = binding_plan.default_database_path;
        let binding_context = binding_plan.context;
        let (resolved_path, handles, provider_mode, controller) = if let Some(api) =
            self.api.as_ref()
        {
            std::fs::create_dir_all(&db_path).map_err(|error| {
                format!(
                    "failed to create LanceDB directory {}: {}",
                    render_host_visible_path(&db_path),
                    error
                )
            })?;
            let default_path = CString::new(database_path.clone())
                .map_err(|_| "database path contains interior NUL bytes".to_string())?;
            let mut options = unsafe { (api.runtime_options_default)() };
            options.default_db_path = default_path.as_ptr();
            options.db_root = ptr::null();
            let runtime = unsafe { (api.runtime_create)(options) };
            if runtime.is_null() {
                return Err(api.take_last_error_message());
            }

            let engine = unsafe { (api.runtime_open_default_engine)(runtime) };
            if engine.is_null() {
                unsafe {
                    (api.runtime_destroy)(runtime);
                }
                return Err(api.take_last_error_message());
            }

            let resolved_path = unsafe {
                api.take_owned_string((api.runtime_database_path_for_name)(runtime, ptr::null()))
            }
            .unwrap_or(database_path.clone());
            (
                resolved_path,
                Some(Mutex::new(SkillHandleState { runtime, engine })),
                LanceDbBindingMode::DynamicLibrary,
                None,
            )
        } else if matches!(
            self.host_options.lancedb_provider_mode,
            LuaRuntimeDatabaseProviderMode::SpaceController
        ) {
            let controller = self
                .controller
                .as_ref()
                .ok_or_else(|| "LanceDB space-controller bridge is unavailable".to_string())?
                .clone();
            let controller_ids = controller.attach_binding_with_ids(&binding_context)?;
            let controller_database_path = database_path.clone();
            controller.run(move |client| async move {
                client
                    .enable_lancedb(ControllerLanceDbEnableRequest {
                        space_id: controller_ids.space_id,
                        binding_id: controller_ids.binding_id,
                        default_db_path: controller_database_path,
                        ..ControllerLanceDbEnableRequest::default()
                    })
                    .await
            })?;
            (
                database_path.clone(),
                None,
                LanceDbBindingMode::SpaceController,
                Some(controller),
            )
        } else {
            (
                database_path.clone(),
                None,
                LanceDbBindingMode::HostCallback,
                None,
            )
        };

        let binding = Arc::new(LanceDbSkillBinding {
            api: self.api.clone(),
            skill_name: skill_name.to_string(),
            skill_dir_name,
            database_path: resolved_path,
            config,
            provider_mode,
            callback_mode: self.host_options.lancedb_callback_mode,
            handles,
            controller,
            provider_callbacks: self.provider_callbacks.clone(),
            provider_binding: binding_context,
        });
        guard.insert(skill_name.to_string(), binding.clone());
        Ok(binding)
    }

    /// Fetch a registered binding by skill name so Lua injection and cross-skill calls can restore context.
    /// 按 skill 名称获取已注册绑定，供 Lua 注入与跨 skill 调用恢复上下文使用。
    pub fn binding_for_skill(
        &self,
        skill_name: &str,
    ) -> Result<Option<Arc<LanceDbSkillBinding>>, String> {
        let skills = self.lock_skills();
        Ok(skills.get(skill_name).cloned())
    }

    /// Acquire the LanceDB skill binding registry and return its guard, recovering after registry lock poisoning.
    /// 获取并返回 LanceDB skill binding 注册表保护对象；如果注册表锁已 poison，则恢复继续使用。
    fn lock_skills(&self) -> MutexGuard<'_, HashMap<String, Arc<LanceDbSkillBinding>>> {
        self.skills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Build a stable status object for skills that do not enable LanceDB so Lua can check before calling.
/// 为未启用 LanceDB 的 skill 生成稳定状态对象，便于 Lua 侧先判断再调用。
pub fn disabled_skill_status_json(skill_name: Option<&str>) -> Value {
    json!({
        "enabled": false,
        "initialized": false,
        "skill_name": skill_name.unwrap_or(""),
        "integration_mode": "dynamic_library",
        "reason": "current skill has not enabled lancedb"
    })
}

/// Return the host-visible LanceDB library path when a dynamic API is loaded.
/// 在动态 API 已加载时返回宿主可见的 LanceDB 动态库路径。
fn lancedb_library_path_value(api: Option<&LoadedLanceDbApi>) -> Value {
    api.map(|api| json!(render_host_visible_path(&api.library_path)))
        .unwrap_or(Value::Null)
}

/// Ensure that a required string field exists in the JSON request payload.
/// 确保 JSON 请求载荷中存在指定的必填字符串字段。
fn require_string_field<'a>(input: &'a Value, field_name: &str) -> Result<&'a str, String> {
    input
        .get(field_name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing or empty field `{}`", field_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::database::{
        LuaRuntimeDatabaseProviderMode, RuntimeDatabaseBindingContext,
        RuntimeDatabaseBindingContextSpec, RuntimeDatabaseKind,
    };
    use crate::runtime::path::render_host_visible_path;
    use std::panic::{self, AssertUnwindSafe};

    /// Verify missing LanceDB libraries render paths through the host-visible formatter.
    /// 验证缺失 LanceDB 动态库会通过宿主可见路径渲染器输出路径。
    #[test]
    fn lancedb_missing_library_error_uses_host_visible_path() {
        // Missing library path used to exercise the shared real loader operation.
        // 用于触发共享真实加载操作的缺失动态库路径。
        let library_path = std::env::temp_dir().join(format!(
            "luaskills-missing-lancedb-provider-{}.dll",
            std::process::id()
        ));
        // Error returned by the real LanceDB dynamic-library loader.
        // 真实 LanceDB 动态库加载器返回的错误。
        let error = match LoadedLanceDbApi::load(&library_path) {
            Ok(_) => panic!("missing LanceDB library should fail"),
            Err(error) => error,
        };
        // Stable diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的稳定诊断前缀。
        let expected_prefix = format!(
            "failed to load LanceDB dynamic library {}:",
            render_host_visible_path(&library_path)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {error}"
        );
    }

    /// Build one deterministic LanceDB binding context for provider binding-state tests.
    /// 为 provider 绑定状态测试构造一个确定性的 LanceDB 绑定上下文。
    fn sample_lancedb_binding_context() -> RuntimeDatabaseBindingContext {
        RuntimeDatabaseBindingContext::new(RuntimeDatabaseBindingContextSpec {
            space_label: "ROOT".to_string(),
            skill_id: "lancedb-api-state-skill".to_string(),
            root_name: "ROOT".to_string(),
            space_root: "D:/runtime-test-root/databases".to_string(),
            skill_dir: "D:/runtime-test-root/skills/lancedb-api-state-skill".to_string(),
            skill_dir_name: "lancedb-api-state-skill".to_string(),
            database_kind: RuntimeDatabaseKind::LanceDb,
            default_database_path: "D:/runtime-test-root/databases/lancedb-api-state-skill"
                .to_string(),
        })
    }

    /// Build one intentionally inconsistent dynamic LanceDB binding without a loaded API.
    /// 构造一个有意失配的动态 LanceDB 绑定，其中没有已加载 API。
    fn dynamic_lancedb_binding_without_api() -> LanceDbSkillBinding {
        LanceDbSkillBinding {
            api: None,
            skill_name: "lancedb-api-state-skill".to_string(),
            skill_dir_name: "lancedb-api-state-skill".to_string(),
            database_path: "D:/runtime-test-root/databases/lancedb-api-state-skill".to_string(),
            config: SkillLanceDbMeta::default(),
            provider_mode: LanceDbBindingMode::DynamicLibrary,
            callback_mode: LuaRuntimeDatabaseCallbackMode::Standard,
            handles: None,
            controller: None,
            provider_callbacks: Arc::new(RuntimeDatabaseProviderCallbacks::default()),
            provider_binding: sample_lancedb_binding_context(),
        }
    }

    /// Verify a dynamic LanceDB binding missing its API returns an error instead of panicking.
    /// 验证缺失 API 的动态 LanceDB 绑定会返回错误，而不是 panic。
    #[test]
    fn lancedb_dynamic_binding_without_api_returns_error() {
        // Binding state that violates the dynamic-library construction invariant.
        // 违反动态库构造不变量的绑定状态。
        let binding = dynamic_lancedb_binding_without_api();
        // Error returned by the real create-table dispatch path before native handle access.
        // 真实建表分发路径在访问原生句柄前返回的错误。
        let error = binding
            .create_table_json(&json!({ "table_name": "docs" }))
            .expect_err("missing dynamic API should return an error");

        assert_eq!(
            error,
            "LanceDB dynamic-library API is unavailable for dynamic_library binding"
        );
    }

    /// Verify dynamic LanceDB status refuses a missing API instead of rendering placeholder metadata.
    /// 验证动态 LanceDB 状态拒绝缺失 API，而不是渲染占位元数据。
    #[test]
    fn lancedb_dynamic_status_without_api_returns_error() {
        // Binding state that violates the dynamic-library construction invariant.
        // 违反动态库构造不变量的绑定状态。
        let binding = dynamic_lancedb_binding_without_api();
        // Status diagnostic surfaced before presenting an invalid provider status payload.
        // 在展示无效 provider 状态载荷前暴露的状态诊断。
        let error = binding
            .status_json()
            .expect_err("missing dynamic API should make status fail");

        assert_eq!(
            error,
            "LanceDB dynamic-library API is unavailable for dynamic_library binding"
        );
    }

    /// Verify the LanceDB skill binding registry can register and read bindings after lock poisoning.
    /// 验证 LanceDB skill binding 注册表锁 poison 后仍可注册并读取绑定。
    #[test]
    fn lancedb_skill_binding_registry_recovers_after_poisoned_lock() {
        // Host options selecting host-callback mode so the test does not load a dynamic provider library.
        // 选择 host-callback 模式的宿主选项，避免测试加载动态 provider 库。
        let host_options = LuaRuntimeHostOptions {
            lancedb_provider_mode: LuaRuntimeDatabaseProviderMode::HostCallback,
            database_dir_name: "databases".to_string(),
            ..LuaRuntimeHostOptions::default()
        };
        // LanceDB host with an empty registry used by this poison recovery test.
        // 本 poison 恢复测试使用的空注册表 LanceDB host。
        let host = LanceDbSkillHost {
            api: None,
            controller: None,
            skills: Mutex::new(HashMap::new()),
            provider_callbacks: Arc::new(RuntimeDatabaseProviderCallbacks::default()),
            host_options,
        };

        // Captured panic result from a writer that poisons the LanceDB binding registry.
        // LanceDB binding 注册表写入者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the LanceDB skill binding registry.
            // 仅用于制造 LanceDB skill binding 注册表 poison 的保护对象。
            let _registry_guard = host
                .skills
                .lock()
                .expect("initial lancedb skill binding registry lock");
            panic!("poison lancedb skill binding registry for recovery test");
        }));

        assert!(poison_result.is_err());

        // Skill directory path used only to derive the deterministic database binding context.
        // 仅用于派生确定性数据库绑定上下文的 skill 目录路径。
        let skill_dir = PathBuf::from("D:/runtime-test-root/skills/lancedb-poison-skill");
        // Binding registered after poisoning to prove write-path recovery.
        // poison 后注册的绑定，用于证明写路径已恢复。
        let binding = host
            .register_skill(
                "ROOT",
                "lancedb-poison-skill",
                &skill_dir,
                SkillLanceDbMeta::default(),
            )
            .expect("register lancedb binding after poison");
        // Binding fetched after poisoning to prove read-path recovery.
        // poison 后读取的绑定，用于证明读路径已恢复。
        let fetched = host
            .binding_for_skill("lancedb-poison-skill")
            .expect("fetch lancedb binding after poison")
            .expect("lancedb binding should exist");

        assert!(Arc::ptr_eq(&binding, &fetched));
    }
}
