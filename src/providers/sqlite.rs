use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use libloading::Library;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::{CString, c_char};
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
    RuntimeDatabaseKind, RuntimeDatabaseProviderCallbacks, RuntimeSqliteProviderAction,
    RuntimeSqliteProviderRequest, build_runtime_database_binding_plan,
    require_database_provider_callback_registration,
};
use crate::lua_skill::{SkillSqliteLogLevel, SkillSqliteMeta};
use crate::runtime::path::render_host_visible_path;
use crate::runtime_logging::{info as log_info, warn as log_warn};
use crate::runtime_options::LuaRuntimeHostOptions;
use vldb_controller_client::{
    ControllerSqliteEnableRequest, ControllerSqliteTokenizerMode, ControllerSqliteValue,
};

/// Forward declaration of the FFI runtime handle used only for raw cross-library pointers.
/// FFI runtime 句柄前置声明，仅用于跨动态库传递裸指针。
#[repr(C)]
struct VldbSqliteRuntimeHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI database handle used only for raw cross-library pointers.
/// FFI 数据库句柄前置声明，仅用于跨动态库传递裸指针。
#[repr(C)]
struct VldbSqliteDatabaseHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI tokenize-result handle.
/// FFI 分词结果句柄前置声明。
#[repr(C)]
struct VldbSqliteTokenizeResultHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI custom-word list handle.
/// FFI 自定义词列表句柄前置声明。
#[repr(C)]
struct VldbSqliteCustomWordListHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI search-result handle.
/// FFI 检索结果句柄前置声明。
#[repr(C)]
struct VldbSqliteSearchResultHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI shared SQL execute-result handle.
/// FFI 通用 SQL 执行结果句柄前置声明。
#[repr(C)]
struct VldbSqliteExecuteResultHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI JSON-query result handle.
/// FFI JSON 查询结果句柄前置声明。
#[repr(C)]
struct VldbSqliteQueryJsonResultHandle {
    _private: [u8; 0],
}

/// Forward declaration of the FFI QueryStream result handle.
/// FFI QueryStream 结果句柄前置声明。
#[repr(C)]
struct VldbSqliteQueryStreamHandle {
    _private: [u8; 0],
}

/// SQLite FFI status code.
/// SQLite FFI 返回状态码。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VldbSqliteStatusCode {
    Success = 0,
}

/// SQLite FFI tokenizer-mode enum kept ABI-compatible with the exported header.
/// SQLite FFI 分词模式枚举，需与导出头文件严格保持一致。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VldbSqliteFfiTokenizerMode {
    None = 0,
    Jieba = 1,
}

/// POD result structure for custom-word mutations.
/// 自定义词修改结果 POD 结构。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbSqliteDictionaryMutationResultPod {
    success: u8,
    affected_rows: u64,
}

/// POD result structure for FTS ensure-index operations.
/// FTS 索引创建结果 POD 结构。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbSqliteEnsureFtsIndexResultPod {
    success: u8,
    tokenizer_mode: u32,
}

/// POD result structure for FTS rebuild-index operations.
/// FTS 索引重建结果 POD 结构。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbSqliteRebuildFtsIndexResultPod {
    success: u8,
    tokenizer_mode: u32,
    reindexed_rows: u64,
}

/// POD result structure for FTS document mutations.
/// FTS 文档写入/删除结果 POD 结构。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbSqliteFtsMutationResultPod {
    success: u8,
    affected_rows: u64,
}

/// FFI byte-view structure used for bytes parameters.
/// FFI 字节视图结构，供 bytes 参数使用。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct VldbSqliteByteView {
    data: *const u8,
    len: u64,
}

/// FFI releasable byte buffer returned by QueryStream chunk getters.
/// FFI 可释放字节缓冲区，供 QueryStream chunk getter 返回。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct VldbSqliteByteBuffer {
    data: *mut u8,
    len: u64,
    cap: u64,
}

/// FFI SQL value-kind enum kept ABI-compatible with the exported header.
/// FFI SQL 值类型枚举，必须与头文件定义保持一致。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VldbSqliteFfiValueKind {
    Null = 0,
    Int64 = 1,
    Float64 = 2,
    String = 3,
    Bytes = 4,
    Bool = 5,
}

/// FFI SQL parameter value structure.
/// FFI SQL 参数值结构。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbSqliteFfiValue {
    kind: VldbSqliteFfiValueKind,
    int64_value: i64,
    float64_value: f64,
    string_value: *const c_char,
    bytes_value: VldbSqliteByteView,
    bool_value: u8,
}

/// FFI SQL parameter-slice structure used by batch execution.
/// FFI SQL 参数切片结构，用于批量执行。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VldbSqliteFfiValueSlice {
    values: *const VldbSqliteFfiValue,
    len: u64,
}

type RuntimeCreateDefaultFn = unsafe extern "C" fn() -> *mut VldbSqliteRuntimeHandle;
type RuntimeDestroyFn = unsafe extern "C" fn(*mut VldbSqliteRuntimeHandle);
type RuntimeOpenDatabaseFn = unsafe extern "C" fn(
    *mut VldbSqliteRuntimeHandle,
    *const c_char,
) -> *mut VldbSqliteDatabaseHandle;
type DatabaseDestroyFn = unsafe extern "C" fn(*mut VldbSqliteDatabaseHandle);
type DatabaseDbPathFn = unsafe extern "C" fn(*mut VldbSqliteDatabaseHandle) -> *mut c_char;
type StringFreeFn = unsafe extern "C" fn(*mut c_char);
type LastErrorMessageFn = unsafe extern "C" fn() -> *const c_char;
type ClearLastErrorFn = unsafe extern "C" fn();
type LibraryInfoJsonFn = unsafe extern "C" fn() -> *mut c_char;
type DatabaseExecuteScriptFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    *const VldbSqliteFfiValue,
    u64,
    *const c_char,
) -> *mut VldbSqliteExecuteResultHandle;
type DatabaseExecuteBatchFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    *const VldbSqliteFfiValueSlice,
    u64,
) -> *mut VldbSqliteExecuteResultHandle;
type DatabaseQueryJsonFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    *const VldbSqliteFfiValue,
    u64,
    *const c_char,
) -> *mut VldbSqliteQueryJsonResultHandle;
type DatabaseQueryStreamFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    *const VldbSqliteFfiValue,
    u64,
    *const c_char,
    u64,
) -> *mut VldbSqliteQueryStreamHandle;
type ExecuteResultDestroyFn = unsafe extern "C" fn(*mut VldbSqliteExecuteResultHandle);
type ExecuteResultSuccessFn = unsafe extern "C" fn(*mut VldbSqliteExecuteResultHandle) -> u8;
type ExecuteResultMessageFn =
    unsafe extern "C" fn(*mut VldbSqliteExecuteResultHandle) -> *mut c_char;
type ExecuteResultRowsChangedFn = unsafe extern "C" fn(*mut VldbSqliteExecuteResultHandle) -> i64;
type ExecuteResultLastInsertRowIdFn =
    unsafe extern "C" fn(*mut VldbSqliteExecuteResultHandle) -> i64;
type ExecuteResultStatementsExecutedFn =
    unsafe extern "C" fn(*mut VldbSqliteExecuteResultHandle) -> i64;
type QueryJsonResultDestroyFn = unsafe extern "C" fn(*mut VldbSqliteQueryJsonResultHandle);
type QueryJsonResultJsonDataFn =
    unsafe extern "C" fn(*mut VldbSqliteQueryJsonResultHandle) -> *mut c_char;
type QueryJsonResultRowCountFn = unsafe extern "C" fn(*mut VldbSqliteQueryJsonResultHandle) -> u64;
type QueryStreamDestroyFn = unsafe extern "C" fn(*mut VldbSqliteQueryStreamHandle);
type QueryStreamChunkCountFn = unsafe extern "C" fn(*mut VldbSqliteQueryStreamHandle) -> u64;
type QueryStreamRowCountFn = unsafe extern "C" fn(*mut VldbSqliteQueryStreamHandle) -> u64;
type QueryStreamTotalBytesFn = unsafe extern "C" fn(*mut VldbSqliteQueryStreamHandle) -> u64;
type QueryStreamGetChunkFn =
    unsafe extern "C" fn(*mut VldbSqliteQueryStreamHandle, u64) -> VldbSqliteByteBuffer;
type BytesFreeFn = unsafe extern "C" fn(VldbSqliteByteBuffer);
type DatabaseTokenizeTextFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    VldbSqliteFfiTokenizerMode,
    *const c_char,
    u8,
) -> *mut VldbSqliteTokenizeResultHandle;
type TokenizeResultDestroyFn = unsafe extern "C" fn(*mut VldbSqliteTokenizeResultHandle);
type TokenizeResultNormalizedTextFn =
    unsafe extern "C" fn(*mut VldbSqliteTokenizeResultHandle) -> *mut c_char;
type TokenizeResultFtsQueryFn =
    unsafe extern "C" fn(*mut VldbSqliteTokenizeResultHandle) -> *mut c_char;
type TokenizeResultTokenCountFn = unsafe extern "C" fn(*mut VldbSqliteTokenizeResultHandle) -> u64;
type TokenizeResultGetTokenFn =
    unsafe extern "C" fn(*mut VldbSqliteTokenizeResultHandle, u64) -> *mut c_char;
type DatabaseUpsertCustomWordFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    u64,
    *mut VldbSqliteDictionaryMutationResultPod,
) -> i32;
type DatabaseRemoveCustomWordFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    *mut VldbSqliteDictionaryMutationResultPod,
) -> i32;
type DatabaseListCustomWordsFn =
    unsafe extern "C" fn(*mut VldbSqliteDatabaseHandle) -> *mut VldbSqliteCustomWordListHandle;
type CustomWordListDestroyFn = unsafe extern "C" fn(*mut VldbSqliteCustomWordListHandle);
type CustomWordListLenFn = unsafe extern "C" fn(*mut VldbSqliteCustomWordListHandle) -> u64;
type CustomWordListGetWordFn =
    unsafe extern "C" fn(*mut VldbSqliteCustomWordListHandle, u64) -> *mut c_char;
type CustomWordListGetWeightFn =
    unsafe extern "C" fn(*mut VldbSqliteCustomWordListHandle, u64) -> u64;
type DatabaseEnsureFtsIndexFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    VldbSqliteFfiTokenizerMode,
    *mut VldbSqliteEnsureFtsIndexResultPod,
) -> i32;
type DatabaseRebuildFtsIndexFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    VldbSqliteFfiTokenizerMode,
    *mut VldbSqliteRebuildFtsIndexResultPod,
) -> i32;
type DatabaseUpsertFtsDocumentFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    VldbSqliteFfiTokenizerMode,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut VldbSqliteFtsMutationResultPod,
) -> i32;
type DatabaseDeleteFtsDocumentFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    *const c_char,
    *mut VldbSqliteFtsMutationResultPod,
) -> i32;
type DatabaseSearchFtsFn = unsafe extern "C" fn(
    *mut VldbSqliteDatabaseHandle,
    *const c_char,
    VldbSqliteFfiTokenizerMode,
    *const c_char,
    u32,
    u32,
) -> *mut VldbSqliteSearchResultHandle;
type SearchResultDestroyFn = unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle);
type SearchResultTotalFn = unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle) -> u64;
type SearchResultLenFn = unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle) -> u64;
type SearchResultSourceFn = unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle) -> *mut c_char;
type SearchResultQueryModeFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle) -> *mut c_char;
type SearchResultGetIdFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> *mut c_char;
type SearchResultGetFilePathFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> *mut c_char;
type SearchResultGetTitleFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> *mut c_char;
type SearchResultGetTitleHighlightFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> *mut c_char;
type SearchResultGetContentSnippetFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> *mut c_char;
type SearchResultGetScoreFn = unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> f64;
type SearchResultGetRankFn = unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> u64;
type SearchResultGetRawScoreFn =
    unsafe extern "C" fn(*mut VldbSqliteSearchResultHandle, u64) -> f64;

/// Loaded SQLite FFI API table that owns the dynamic-library lifetime and all exported function pointers.
/// 已加载的 SQLite FFI API 表，持有动态库生命周期与全部导出函数指针。
struct LoadedSqliteApi {
    _library: Library,
    library_path: PathBuf,
    runtime_create_default: RuntimeCreateDefaultFn,
    runtime_destroy: RuntimeDestroyFn,
    runtime_open_database: RuntimeOpenDatabaseFn,
    database_destroy: DatabaseDestroyFn,
    database_db_path: DatabaseDbPathFn,
    string_free: StringFreeFn,
    last_error_message: LastErrorMessageFn,
    clear_last_error: ClearLastErrorFn,
    library_info_json: LibraryInfoJsonFn,
    database_execute_script: DatabaseExecuteScriptFn,
    database_execute_batch: DatabaseExecuteBatchFn,
    database_query_json: DatabaseQueryJsonFn,
    database_query_stream: DatabaseQueryStreamFn,
    execute_result_destroy: ExecuteResultDestroyFn,
    execute_result_success: ExecuteResultSuccessFn,
    execute_result_message: ExecuteResultMessageFn,
    execute_result_rows_changed: ExecuteResultRowsChangedFn,
    execute_result_last_insert_rowid: ExecuteResultLastInsertRowIdFn,
    execute_result_statements_executed: ExecuteResultStatementsExecutedFn,
    query_json_result_destroy: QueryJsonResultDestroyFn,
    query_json_result_json_data: QueryJsonResultJsonDataFn,
    query_json_result_row_count: QueryJsonResultRowCountFn,
    query_stream_destroy: QueryStreamDestroyFn,
    query_stream_chunk_count: QueryStreamChunkCountFn,
    query_stream_row_count: QueryStreamRowCountFn,
    query_stream_total_bytes: QueryStreamTotalBytesFn,
    query_stream_get_chunk: QueryStreamGetChunkFn,
    bytes_free: BytesFreeFn,
    database_tokenize_text: DatabaseTokenizeTextFn,
    tokenize_result_destroy: TokenizeResultDestroyFn,
    tokenize_result_normalized_text: TokenizeResultNormalizedTextFn,
    tokenize_result_fts_query: TokenizeResultFtsQueryFn,
    tokenize_result_token_count: TokenizeResultTokenCountFn,
    tokenize_result_get_token: TokenizeResultGetTokenFn,
    database_upsert_custom_word: DatabaseUpsertCustomWordFn,
    database_remove_custom_word: DatabaseRemoveCustomWordFn,
    database_list_custom_words: DatabaseListCustomWordsFn,
    custom_word_list_destroy: CustomWordListDestroyFn,
    custom_word_list_len: CustomWordListLenFn,
    custom_word_list_get_word: CustomWordListGetWordFn,
    custom_word_list_get_weight: CustomWordListGetWeightFn,
    database_ensure_fts_index: DatabaseEnsureFtsIndexFn,
    database_rebuild_fts_index: DatabaseRebuildFtsIndexFn,
    database_upsert_fts_document: DatabaseUpsertFtsDocumentFn,
    database_delete_fts_document: DatabaseDeleteFtsDocumentFn,
    database_search_fts: DatabaseSearchFtsFn,
    search_result_destroy: SearchResultDestroyFn,
    search_result_total: SearchResultTotalFn,
    search_result_len: SearchResultLenFn,
    search_result_source: SearchResultSourceFn,
    search_result_query_mode: SearchResultQueryModeFn,
    search_result_get_id: SearchResultGetIdFn,
    search_result_get_file_path: SearchResultGetFilePathFn,
    search_result_get_title: SearchResultGetTitleFn,
    search_result_get_title_highlight: SearchResultGetTitleHighlightFn,
    search_result_get_content_snippet: SearchResultGetContentSnippetFn,
    search_result_get_score: SearchResultGetScoreFn,
    search_result_get_rank: SearchResultGetRankFn,
    search_result_get_raw_score: SearchResultGetRawScoreFn,
}

/// The loaded library and function table stay immutable after initialization, while outer locks protect shared access.
/// 动态库句柄与函数表初始化后只读，跨线程共享由外层锁负责保护。
unsafe impl Send for LoadedSqliteApi {}
unsafe impl Sync for LoadedSqliteApi {}

impl LoadedSqliteApi {
    /// Load the SQLite dynamic library using host conventions, preferring an explicit environment variable and runtime directories.
    /// 按宿主约定加载 SQLite 动态库，优先查找显式环境变量和运行时目录。
    fn load(library_path: &Path) -> Result<Self, String> {
        if !library_path.exists() {
            return Err(format!(
                "SQLite dynamic library path does not exist: {}",
                render_host_visible_path(library_path)
            ));
        }

        let library = unsafe { Library::new(library_path) }.map_err(|error| {
            format!(
                "failed to load {}: {}: {}",
                render_host_visible_path(library_path),
                error,
                error
            )
        })?;
        unsafe { Self::from_library(library_path.to_path_buf(), library) }
    }

    /// Copy required exported function pointers from the opened dynamic library while retaining the library handle.
    /// 从已打开的动态库中复制所需函数指针，并保留库句柄防止提前卸载。
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
            runtime_create_default: load_symbol!(
                "vldb_sqlite_runtime_create_default",
                RuntimeCreateDefaultFn
            ),
            runtime_destroy: load_symbol!("vldb_sqlite_runtime_destroy", RuntimeDestroyFn),
            runtime_open_database: load_symbol!(
                "vldb_sqlite_runtime_open_database",
                RuntimeOpenDatabaseFn
            ),
            database_destroy: load_symbol!("vldb_sqlite_database_destroy", DatabaseDestroyFn),
            database_db_path: load_symbol!("vldb_sqlite_database_db_path", DatabaseDbPathFn),
            string_free: load_symbol!("vldb_sqlite_string_free", StringFreeFn),
            last_error_message: load_symbol!("vldb_sqlite_last_error_message", LastErrorMessageFn),
            clear_last_error: load_symbol!("vldb_sqlite_clear_last_error", ClearLastErrorFn),
            library_info_json: load_symbol!("vldb_sqlite_library_info_json", LibraryInfoJsonFn),
            database_execute_script: load_symbol!(
                "vldb_sqlite_database_execute_script",
                DatabaseExecuteScriptFn
            ),
            database_execute_batch: load_symbol!(
                "vldb_sqlite_database_execute_batch",
                DatabaseExecuteBatchFn
            ),
            database_query_json: load_symbol!(
                "vldb_sqlite_database_query_json",
                DatabaseQueryJsonFn
            ),
            database_query_stream: load_symbol!(
                "vldb_sqlite_database_query_stream",
                DatabaseQueryStreamFn
            ),
            execute_result_destroy: load_symbol!(
                "vldb_sqlite_execute_result_destroy",
                ExecuteResultDestroyFn
            ),
            execute_result_success: load_symbol!(
                "vldb_sqlite_execute_result_success",
                ExecuteResultSuccessFn
            ),
            execute_result_message: load_symbol!(
                "vldb_sqlite_execute_result_message",
                ExecuteResultMessageFn
            ),
            execute_result_rows_changed: load_symbol!(
                "vldb_sqlite_execute_result_rows_changed",
                ExecuteResultRowsChangedFn
            ),
            execute_result_last_insert_rowid: load_symbol!(
                "vldb_sqlite_execute_result_last_insert_rowid",
                ExecuteResultLastInsertRowIdFn
            ),
            execute_result_statements_executed: load_symbol!(
                "vldb_sqlite_execute_result_statements_executed",
                ExecuteResultStatementsExecutedFn
            ),
            query_json_result_destroy: load_symbol!(
                "vldb_sqlite_query_json_result_destroy",
                QueryJsonResultDestroyFn
            ),
            query_json_result_json_data: load_symbol!(
                "vldb_sqlite_query_json_result_json_data",
                QueryJsonResultJsonDataFn
            ),
            query_json_result_row_count: load_symbol!(
                "vldb_sqlite_query_json_result_row_count",
                QueryJsonResultRowCountFn
            ),
            query_stream_destroy: load_symbol!(
                "vldb_sqlite_query_stream_destroy",
                QueryStreamDestroyFn
            ),
            query_stream_chunk_count: load_symbol!(
                "vldb_sqlite_query_stream_chunk_count",
                QueryStreamChunkCountFn
            ),
            query_stream_row_count: load_symbol!(
                "vldb_sqlite_query_stream_row_count",
                QueryStreamRowCountFn
            ),
            query_stream_total_bytes: load_symbol!(
                "vldb_sqlite_query_stream_total_bytes",
                QueryStreamTotalBytesFn
            ),
            query_stream_get_chunk: load_symbol!(
                "vldb_sqlite_query_stream_get_chunk",
                QueryStreamGetChunkFn
            ),
            bytes_free: load_symbol!("vldb_sqlite_bytes_free", BytesFreeFn),
            database_tokenize_text: load_symbol!(
                "vldb_sqlite_database_tokenize_text",
                DatabaseTokenizeTextFn
            ),
            tokenize_result_destroy: load_symbol!(
                "vldb_sqlite_tokenize_result_destroy",
                TokenizeResultDestroyFn
            ),
            tokenize_result_normalized_text: load_symbol!(
                "vldb_sqlite_tokenize_result_normalized_text",
                TokenizeResultNormalizedTextFn
            ),
            tokenize_result_fts_query: load_symbol!(
                "vldb_sqlite_tokenize_result_fts_query",
                TokenizeResultFtsQueryFn
            ),
            tokenize_result_token_count: load_symbol!(
                "vldb_sqlite_tokenize_result_token_count",
                TokenizeResultTokenCountFn
            ),
            tokenize_result_get_token: load_symbol!(
                "vldb_sqlite_tokenize_result_get_token",
                TokenizeResultGetTokenFn
            ),
            database_upsert_custom_word: load_symbol!(
                "vldb_sqlite_database_upsert_custom_word",
                DatabaseUpsertCustomWordFn
            ),
            database_remove_custom_word: load_symbol!(
                "vldb_sqlite_database_remove_custom_word",
                DatabaseRemoveCustomWordFn
            ),
            database_list_custom_words: load_symbol!(
                "vldb_sqlite_database_list_custom_words",
                DatabaseListCustomWordsFn
            ),
            custom_word_list_destroy: load_symbol!(
                "vldb_sqlite_custom_word_list_destroy",
                CustomWordListDestroyFn
            ),
            custom_word_list_len: load_symbol!(
                "vldb_sqlite_custom_word_list_len",
                CustomWordListLenFn
            ),
            custom_word_list_get_word: load_symbol!(
                "vldb_sqlite_custom_word_list_get_word",
                CustomWordListGetWordFn
            ),
            custom_word_list_get_weight: load_symbol!(
                "vldb_sqlite_custom_word_list_get_weight",
                CustomWordListGetWeightFn
            ),
            database_ensure_fts_index: load_symbol!(
                "vldb_sqlite_database_ensure_fts_index",
                DatabaseEnsureFtsIndexFn
            ),
            database_rebuild_fts_index: load_symbol!(
                "vldb_sqlite_database_rebuild_fts_index",
                DatabaseRebuildFtsIndexFn
            ),
            database_upsert_fts_document: load_symbol!(
                "vldb_sqlite_database_upsert_fts_document",
                DatabaseUpsertFtsDocumentFn
            ),
            database_delete_fts_document: load_symbol!(
                "vldb_sqlite_database_delete_fts_document",
                DatabaseDeleteFtsDocumentFn
            ),
            database_search_fts: load_symbol!(
                "vldb_sqlite_database_search_fts",
                DatabaseSearchFtsFn
            ),
            search_result_destroy: load_symbol!(
                "vldb_sqlite_search_result_destroy",
                SearchResultDestroyFn
            ),
            search_result_total: load_symbol!(
                "vldb_sqlite_search_result_total",
                SearchResultTotalFn
            ),
            search_result_len: load_symbol!("vldb_sqlite_search_result_len", SearchResultLenFn),
            search_result_source: load_symbol!(
                "vldb_sqlite_search_result_source",
                SearchResultSourceFn
            ),
            search_result_query_mode: load_symbol!(
                "vldb_sqlite_search_result_query_mode",
                SearchResultQueryModeFn
            ),
            search_result_get_id: load_symbol!(
                "vldb_sqlite_search_result_get_id",
                SearchResultGetIdFn
            ),
            search_result_get_file_path: load_symbol!(
                "vldb_sqlite_search_result_get_file_path",
                SearchResultGetFilePathFn
            ),
            search_result_get_title: load_symbol!(
                "vldb_sqlite_search_result_get_title",
                SearchResultGetTitleFn
            ),
            search_result_get_title_highlight: load_symbol!(
                "vldb_sqlite_search_result_get_title_highlight",
                SearchResultGetTitleHighlightFn
            ),
            search_result_get_content_snippet: load_symbol!(
                "vldb_sqlite_search_result_get_content_snippet",
                SearchResultGetContentSnippetFn
            ),
            search_result_get_score: load_symbol!(
                "vldb_sqlite_search_result_get_score",
                SearchResultGetScoreFn
            ),
            search_result_get_rank: load_symbol!(
                "vldb_sqlite_search_result_get_rank",
                SearchResultGetRankFn
            ),
            search_result_get_raw_score: load_symbol!(
                "vldb_sqlite_search_result_get_raw_score",
                SearchResultGetRawScoreFn
            ),
            _library: library,
            library_path,
        })
    }

    /// Read the latest FFI error and convert it into a stable Rust string.
    /// 读取最近一次 FFI 调用错误并转换成稳定 Rust 字符串。
    fn take_last_error_message(&self) -> String {
        unsafe {
            let ptr = (self.last_error_message)();
            let text =
                decode_provider_last_error_message(ptr, "SQLite", "unknown SQLite host error");
            (self.clear_last_error)();
            text
        }
    }

    /// Convert a dynamic-library allocated string into a Rust `String` and free the original allocation.
    /// 释放动态库分配的字符串并转换成 Rust `String`。
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

    /// Convert a dynamic-library allocated optional string into Rust `Option<String>`.
    /// 将动态库分配的可选字符串转换成 Rust `Option<String>`。
    fn take_optional_string(&self, ptr: *mut c_char) -> Result<Option<String>, String> {
        if ptr.is_null() {
            return Ok(None);
        }
        unsafe {
            let text = decode_non_null_ffi_c_string(ptr);
            (self.string_free)(ptr);
            text.map(Some)
        }
    }

    /// Invoke a zero-argument JSON FFI entrypoint and parse the response into `serde_json::Value`.
    /// 调用无参 JSON FFI 接口并解析成 `serde_json::Value`。
    fn call_json_noarg(
        &self,
        function: LibraryInfoJsonFn,
        operation: &str,
    ) -> Result<Value, String> {
        unsafe {
            let response_ptr = function();
            let response_text = self.take_owned_string(response_ptr)?;
            serde_json::from_str(&response_text)
                .map_err(|error| format!("{} returned invalid JSON: {}", operation, error))
        }
    }

    /// Copy a QueryStream byte buffer into a host-owned `Vec<u8>` and free the underlying allocation.
    /// 把 QueryStream 返回的字节缓冲区复制成宿主拥有的 `Vec<u8>`，并回收底层分配。
    fn take_chunk_bytes(&self, buffer: VldbSqliteByteBuffer) -> Result<Vec<u8>, String> {
        if buffer.data.is_null() {
            if buffer.len == 0 {
                return Ok(Vec::new());
            }
            return Err(self.take_last_error_message());
        }

        let len =
            usize::try_from(buffer.len).map_err(|_| "chunk length exceeds usize".to_string())?;
        unsafe {
            let bytes = std::slice::from_raw_parts(buffer.data, len).to_vec();
            (self.bytes_free)(buffer);
            Ok(bytes)
        }
    }
}

/// SQLite handle set for a single skill, with lifetime managed centrally by the host.
/// 单个 skill 的 SQLite 句柄集合，由宿主统一管理生命周期。
struct SkillHandleState {
    runtime: *mut VldbSqliteRuntimeHandle,
    database: *mut VldbSqliteDatabaseHandle,
    query_streams: HashMap<u64, *mut VldbSqliteQueryStreamHandle>,
    next_stream_id: u64,
}

/// Stable provider integration mode used by one SQLite skill binding.
/// 单个 SQLite skill 绑定所使用的稳定 provider 集成模式。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SqliteBindingMode {
    DynamicLibrary,
    HostCallback,
    SpaceController,
}

/// FFI handles are accessed only behind a host-side mutex, with all cross-thread sharing managed by the host.
/// FFI 句柄仅通过宿主互斥量串行访问，跨线程共享由宿主统一控制。
unsafe impl Send for SkillHandleState {}

/// Database context bound to one SQLite-enabled skill.
/// 启用 SQLite 的 skill 所绑定的数据库上下文。
pub struct SqliteSkillBinding {
    api: Option<Arc<LoadedSqliteApi>>,
    skill_name: String,
    skill_dir_name: String,
    database_path: String,
    config: SkillSqliteMeta,
    provider_mode: SqliteBindingMode,
    callback_mode: LuaRuntimeDatabaseCallbackMode,
    handles: Option<Mutex<SkillHandleState>>,
    controller: Option<Arc<LuaRuntimeSpaceControllerBridge>>,
    provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    provider_binding: RuntimeDatabaseBindingContext,
}

impl SqliteSkillBinding {
    /// Return the stable SQLite status payload for the current skill; the response shape stays stable whether enabled or disabled.
    /// 返回当前 skill 的稳定 SQLite 状态信息；无论启用与否，结构都保持稳定。
    pub fn status_json(&self) -> Result<Value, String> {
        let integration_mode = self.integration_mode_name();
        let controller_capabilities = if self.is_space_controller_mode() {
            vec![
                "execute_script",
                "execute_batch",
                "query_json",
                "tokenize_text",
                "upsert_custom_word",
                "remove_custom_word",
                "list_custom_words",
                "ensure_fts_index",
                "rebuild_fts_index",
                "upsert_fts_document",
                "delete_fts_document",
                "search_fts",
            ]
        } else {
            vec![
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
            ]
        };
        let library_info = self.library_info_json(controller_capabilities)?;
        Ok(json!({
            "enabled": true,
            "initialized": true,
            "skill_name": self.skill_name,
            "skill_dir_name": self.skill_dir_name,
            "database_path": self.database_path,
            "integration_mode": integration_mode,
            "library_path": sqlite_library_path_value(self.api.as_deref()),
            "library_name": required_sqlite_library_info_string(&library_info, "name")?,
            "library_version": required_sqlite_library_info_string(&library_info, "version")?,
            "ffi_stage": required_sqlite_library_info_string(&library_info, "ffi_stage")?,
            "capabilities": required_sqlite_library_info_array(&library_info, "capabilities")?,
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

    /// Return basic information about the SQLite binding for the current skill.
    /// 返回当前 skill 所绑定 SQLite 的基础信息。
    pub fn info_json(&self) -> Result<Value, String> {
        let mut status = self.status_json()?;
        if let Some(status_object) = status.as_object_mut() {
            let library_info = self.library_info_json(Vec::new())?;
            status_object.insert("library_info".to_string(), library_info);
        }
        Ok(status)
    }

    /// Return validated provider metadata for the active SQLite integration mode.
    /// 返回当前 SQLite 集成模式对应的已校验 provider 元数据。
    fn library_info_json(
        &self,
        controller_capabilities: Vec<&'static str>,
    ) -> Result<Value, String> {
        if let Some(api) = self.api.as_ref() {
            let library_info = api.call_json_noarg(api.library_info_json, "library_info_json")?;
            validate_sqlite_library_info(&library_info, "library_info_json")?;
            return Ok(library_info);
        }
        if self.provider_mode == SqliteBindingMode::DynamicLibrary {
            return Err(self.missing_dynamic_api_error());
        }
        let provider_name = match self.provider_mode {
            SqliteBindingMode::HostCallback => "host_callback",
            SqliteBindingMode::SpaceController => "space_controller",
            SqliteBindingMode::DynamicLibrary => "vldb-sqlite",
        };
        let provider_version = match self.provider_mode {
            SqliteBindingMode::SpaceController => "controller_managed",
            _ => "host_managed",
        };
        Ok(json!({
            "name": provider_name,
            "version": provider_version,
            "ffi_stage": provider_version,
            "capabilities": controller_capabilities,
        }))
    }

    /// Return the diagnostic used when a dynamic binding is missing its loaded API table.
    /// 返回 dynamic binding 缺失已加载 API 表时使用的诊断。
    fn missing_dynamic_api_error(&self) -> String {
        format!(
            "SQLite dynamic-library API is unavailable for {} binding",
            self.integration_mode_name()
        )
    }

    /// Execute a script or single SQL statement through the non-JSON primary interface.
    /// 通过非 JSON 主接口执行脚本或单条 SQL。
    pub fn execute_script(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let sql = require_string_field(input, "sql")?;
            let params = parse_single_sql_params(input)?;
            let sql_text = sql.to_string();
            let mapped_params = map_controller_sqlite_params(&params);
            let result = self.run_controller_binding_operation(
                "execute_script",
                None,
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .execute_sqlite_script_typed(
                                ids.space_id,
                                ids.binding_id,
                                sql_text,
                                mapped_params,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "rows_changed": result.rows_changed,
                "last_insert_rowid": result.last_insert_rowid,
                "statements_executed": 1,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::ExecuteScript, input);
        }
        let api = self.api_ref()?;
        let sql = require_string_field(input, "sql")?;
        let params = parse_single_sql_params(input)?;
        let owned_params = build_owned_ffi_values(&params)?;
        self.log_info("execute_script", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let sql_cstr = to_cstring(sql, "sql")?;
        unsafe {
            let result_handle = (api.database_execute_script)(
                guard.database,
                sql_cstr.as_ptr(),
                if owned_params.values.is_empty() {
                    ptr::null()
                } else {
                    owned_params.as_ptr()
                },
                owned_params.len_u64(),
                ptr::null(),
            );
            if result_handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "execute_script", guard));
            }

            let result = (|| -> Result<Value, String> {
                // Required execution message returned by the dynamic execute-result handle.
                // 动态执行结果句柄返回的必需执行消息。
                let message = require_sqlite_dynamic_string(
                    api.take_optional_string((api.execute_result_message)(result_handle))?,
                    "execute_result.message",
                )?;
                Ok(json!({
                    "success": u8_to_bool((api.execute_result_success)(result_handle)),
                    "message": message,
                    "rows_changed": (api.execute_result_rows_changed)(result_handle),
                    "last_insert_rowid": (api.execute_result_last_insert_rowid)(result_handle),
                    "statements_executed": (api.execute_result_statements_executed)(result_handle),
                }))
            })();
            (api.execute_result_destroy)(result_handle);
            drop(guard);
            let result = result?;
            self.log_if_slow("execute_script", started_at, None);
            Ok(result)
        }
    }

    /// Execute batch SQL through the non-JSON primary interface.
    /// 通过非 JSON 主接口批量执行 SQL。
    pub fn execute_batch(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let sql = require_string_field(input, "sql")?;
            let rows = parse_batch_sql_params(input)?;
            let sql_text = sql.to_string();
            let mapped_rows = rows
                .iter()
                .map(|row| map_controller_sqlite_params(row))
                .collect();
            let result = self.run_controller_binding_operation(
                "execute_batch",
                None,
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .execute_sqlite_batch_typed(
                                ids.space_id,
                                ids.binding_id,
                                sql_text,
                                mapped_rows,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "rows_changed": result.rows_changed,
                "last_insert_rowid": result.last_insert_rowid,
                "statements_executed": result.statements_executed,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::ExecuteBatch, input);
        }
        let api = self.api_ref()?;
        let sql = require_string_field(input, "sql")?;
        let rows = parse_batch_sql_params(input)?;
        let owned_rows = build_owned_ffi_value_matrix(&rows)?;
        self.log_info("execute_batch", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let sql_cstr = to_cstring(sql, "sql")?;
        unsafe {
            let result_handle = (api.database_execute_batch)(
                guard.database,
                sql_cstr.as_ptr(),
                owned_rows.as_ptr(),
                owned_rows.len_u64(),
            );
            if result_handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "execute_batch", guard));
            }

            let result = (|| -> Result<Value, String> {
                // Required batch execution message returned by the dynamic execute-result handle.
                // 动态执行结果句柄返回的必需批处理执行消息。
                let message = require_sqlite_dynamic_string(
                    api.take_optional_string((api.execute_result_message)(result_handle))?,
                    "execute_batch_result.message",
                )?;
                Ok(json!({
                    "success": u8_to_bool((api.execute_result_success)(result_handle)),
                    "message": message,
                    "rows_changed": (api.execute_result_rows_changed)(result_handle),
                    "last_insert_rowid": (api.execute_result_last_insert_rowid)(result_handle),
                    "statements_executed": (api.execute_result_statements_executed)(result_handle),
                }))
            })();
            (api.execute_result_destroy)(result_handle);
            drop(guard);
            let result = result?;
            self.log_if_slow("execute_batch", started_at, None);
            Ok(result)
        }
    }

    /// Execute a JSON row-set query through the non-JSON primary interface.
    /// 通过非 JSON 主接口执行 JSON 行集查询。
    pub fn query_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let sql = require_string_field(input, "sql")?;
            let params = parse_single_sql_params(input)?;
            self.log_info("query_json", None);
            let started_at = Instant::now();
            let (bridge, controller_ids) = self.controller_call_context()?;
            let sql_text = sql.to_string();
            let mapped_params = map_controller_sqlite_params(&params);
            let result = bridge.run(move |client| async move {
                client
                    .query_sqlite_json_typed(
                        controller_ids.space_id,
                        controller_ids.binding_id,
                        sql_text,
                        mapped_params,
                    )
                    .await
            })?;
            let rows = serde_json::from_str::<Value>(&result.json_data).map_err(|error| {
                format!("space_controller returned invalid json_data: {}", error)
            })?;
            self.log_if_slow(
                "query_json",
                started_at,
                Some(format!("rows={}", result.row_count)),
            );
            return Ok(json!({
                "success": true,
                "row_count": result.row_count,
                "json_data": result.json_data,
                "rows": rows,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryJson, input);
        }
        let api = self.api_ref()?;
        let sql = require_string_field(input, "sql")?;
        let params = parse_single_sql_params(input)?;
        let owned_params = build_owned_ffi_values(&params)?;
        self.log_info("query_json", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let sql_cstr = to_cstring(sql, "sql")?;
        unsafe {
            let result_handle = (api.database_query_json)(
                guard.database,
                sql_cstr.as_ptr(),
                if owned_params.values.is_empty() {
                    ptr::null()
                } else {
                    owned_params.as_ptr()
                },
                owned_params.len_u64(),
                ptr::null(),
            );
            if result_handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "query_json", guard));
            }

            let row_count = (api.query_json_result_row_count)(result_handle);
            // Store the string extraction result so the native result handle is always destroyed first.
            // 先保存字符串提取结果，确保原生结果句柄总会先被销毁。
            let json_data = api.take_owned_string((api.query_json_result_json_data)(result_handle));
            (api.query_json_result_destroy)(result_handle);
            drop(guard);
            let json_data = json_data?;
            let rows = serde_json::from_str::<Value>(&json_data)
                .map_err(|error| format!("query_json returned invalid json_data: {}", error))?;
            self.log_if_slow(
                "query_json",
                started_at,
                Some(format!("rows={}", row_count)),
            );
            Ok(json!({
                "success": true,
                "row_count": row_count,
                "json_data": json_data,
                "rows": rows,
            }))
        }
    }

    /// Create a QueryStream handle through the non-JSON primary interface.
    /// 通过非 JSON 主接口创建 QueryStream 句柄。
    pub fn query_stream(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let sql = require_string_field(input, "sql")?;
            let params = parse_single_sql_params(input)?;
            let chunk_bytes = input
                .get("chunk_bytes")
                .or_else(|| input.get("chunk_size"))
                .and_then(Value::as_u64);
            self.log_info("query_stream", None);
            let started_at = Instant::now();
            let (bridge, controller_ids) = self.controller_call_context()?;
            let sql_text = sql.to_string();
            let mapped_params = map_controller_sqlite_params(&params);
            let result = bridge.run(move |client| async move {
                client
                    .open_sqlite_query_stream_typed(
                        controller_ids.space_id,
                        controller_ids.binding_id,
                        sql_text,
                        mapped_params,
                        chunk_bytes,
                    )
                    .await
            })?;
            self.log_if_slow(
                "query_stream",
                started_at,
                Some(format!(
                    "stream_id={} metrics_ready={}",
                    result.stream_id, result.metrics_ready
                )),
            );
            return Ok(json!({
                "success": true,
                "stream_id": result.stream_id,
                "metrics_ready": result.metrics_ready,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryStream, input);
        }
        let api = self.api_ref()?;
        let sql = require_string_field(input, "sql")?;
        let params = parse_single_sql_params(input)?;
        let owned_params = build_owned_ffi_values(&params)?;
        let chunk_bytes = input
            .get("chunk_bytes")
            .or_else(|| input.get("chunk_size"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.log_info("query_stream", None);
        let started_at = Instant::now();
        let mut guard = self.lock_handles()?;
        let sql_cstr = to_cstring(sql, "sql")?;
        unsafe {
            let result_handle = (api.database_query_stream)(
                guard.database,
                sql_cstr.as_ptr(),
                if owned_params.values.is_empty() {
                    ptr::null()
                } else {
                    owned_params.as_ptr()
                },
                owned_params.len_u64(),
                ptr::null(),
                chunk_bytes,
            );
            if result_handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "query_stream", guard));
            }

            let stream_id = guard.next_stream_id;
            guard.next_stream_id = guard.next_stream_id.saturating_add(1).max(1);
            guard.query_streams.insert(stream_id, result_handle);
            drop(guard);
            self.log_if_slow(
                "query_stream",
                started_at,
                Some(format!("stream_id={} metrics_ready=false", stream_id)),
            );
            Ok(json!({
                "success": true,
                "stream_id": stream_id,
                "metrics_ready": false,
            }))
        }
    }

    /// Wait for final QueryStream metrics and return terminal statistics.
    /// 等待 QueryStream 最终统计信息就绪，并返回终态指标。
    pub fn query_stream_wait_metrics(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let stream_id = input
                .get("stream_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| "stream_id is required".to_string())?;
            self.log_info("query_stream_wait_metrics", None);
            let started_at = Instant::now();
            let bridge = self.controller_bridge()?;
            let metrics = bridge.run(move |client| async move {
                client.wait_sqlite_query_stream_metrics(stream_id).await
            })?;
            self.log_if_slow(
                "query_stream_wait_metrics",
                started_at,
                Some(format!(
                    "stream_id={} chunks={} rows={} bytes={}",
                    stream_id, metrics.chunk_count, metrics.row_count, metrics.total_bytes
                )),
            );
            return Ok(json!({
                "success": true,
                "stream_id": stream_id,
                "metrics_ready": true,
                "row_count": metrics.row_count,
                "chunk_count": metrics.chunk_count,
                "total_bytes": metrics.total_bytes,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(
                RuntimeSqliteProviderAction::QueryStreamWaitMetrics,
                input,
            );
        }
        let api = self.api_ref()?;
        let stream_id = input
            .get("stream_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "stream_id is required".to_string())?;
        self.log_info("query_stream_wait_metrics", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let stream_handle = *guard
            .query_streams
            .get(&stream_id)
            .ok_or_else(|| format!("query stream handle not found: {}", stream_id))?;
        unsafe {
            let row_count = (api.query_stream_row_count)(stream_handle);
            let chunk_count = (api.query_stream_chunk_count)(stream_handle);
            let total_bytes = (api.query_stream_total_bytes)(stream_handle);
            drop(guard);
            self.log_if_slow(
                "query_stream_wait_metrics",
                started_at,
                Some(format!(
                    "stream_id={} chunks={} rows={} bytes={}",
                    stream_id, chunk_count, row_count, total_bytes
                )),
            );
            Ok(json!({
                "success": true,
                "stream_id": stream_id,
                "metrics_ready": true,
                "row_count": row_count,
                "chunk_count": chunk_count,
                "total_bytes": total_bytes,
            }))
        }
    }

    /// Read a single QueryStream chunk and return it as base64 text.
    /// 读取单个 QueryStream chunk，并以 base64 形式返回。
    pub fn query_stream_chunk(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let stream_id = input
                .get("stream_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| "stream_id is required".to_string())?;
            let index = input
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "index is required".to_string())?;
            self.log_info("query_stream_chunk", None);
            let started_at = Instant::now();
            let bridge = self.controller_bridge()?;
            let chunk = bridge.run(move |client| async move {
                client
                    .read_sqlite_query_stream_chunk(stream_id, index)
                    .await
            })?;
            self.log_if_slow(
                "query_stream_chunk",
                started_at,
                Some(format!(
                    "stream_id={} index={} bytes={}",
                    stream_id,
                    index,
                    chunk.len()
                )),
            );
            return Ok(json!({
                "success": true,
                "stream_id": stream_id,
                "index": index,
                "byte_count": u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                "chunk_base64": BASE64_STANDARD.encode(chunk),
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::QueryStreamChunk, input);
        }
        let api = self.api_ref()?;
        let stream_id = input
            .get("stream_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "stream_id is required".to_string())?;
        let index = input
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "index is required".to_string())?;
        self.log_info("query_stream_chunk", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let stream_handle = *guard
            .query_streams
            .get(&stream_id)
            .ok_or_else(|| format!("query stream handle not found: {}", stream_id))?;
        unsafe {
            let buffer = (api.query_stream_get_chunk)(stream_handle, index);
            let chunk = api.take_chunk_bytes(buffer)?;
            drop(guard);
            self.log_if_slow(
                "query_stream_chunk",
                started_at,
                Some(format!(
                    "stream_id={} index={} bytes={}",
                    stream_id,
                    index,
                    chunk.len()
                )),
            );
            Ok(json!({
                "success": true,
                "stream_id": stream_id,
                "index": index,
                "byte_count": u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                "chunk_base64": BASE64_STANDARD.encode(chunk),
            }))
        }
    }

    /// Close a QueryStream handle and release the host-cached stream result.
    /// 关闭 QueryStream 句柄并释放宿主缓存的流结果。
    pub fn query_stream_close(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let stream_id = input
                .get("stream_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| "stream_id is required".to_string())?;
            self.log_info("query_stream_close", None);
            let started_at = Instant::now();
            let bridge = self.controller_bridge()?;
            let closed = bridge.run(move |client| async move {
                client.close_sqlite_query_stream(stream_id).await
            })?;
            self.log_if_slow(
                "query_stream_close",
                started_at,
                Some(format!("stream_id={}", stream_id)),
            );
            return Ok(json!({
                "success": closed,
                "stream_id": stream_id,
                "message": if closed {
                    format!("query_stream handle {} closed successfully", stream_id)
                } else {
                    format!("query_stream handle {} was not found or was already closed", stream_id)
                },
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::QueryStreamClose, input);
        }
        let api = self.api_ref()?;
        let stream_id = input
            .get("stream_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "stream_id is required".to_string())?;
        self.log_info("query_stream_close", None);
        let started_at = Instant::now();
        let mut guard = self.lock_handles()?;
        let stream_handle = guard
            .query_streams
            .remove(&stream_id)
            .ok_or_else(|| format!("query stream handle not found: {}", stream_id))?;
        unsafe {
            (api.query_stream_destroy)(stream_handle);
            drop(guard);
            self.log_if_slow(
                "query_stream_close",
                started_at,
                Some(format!("stream_id={}", stream_id)),
            );
            Ok(json!({
                "success": true,
                "stream_id": stream_id,
                "message": format!("query_stream handle {} closed successfully", stream_id),
            }))
        }
    }

    /// Execute text tokenization and return a normalized result payload.
    /// 执行文本分词，并返回标准化结果。
    pub fn tokenize_text_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let tokenizer_mode = map_controller_tokenizer_mode(parse_tokenizer_mode(
                input
                    .get("tokenizer_mode")
                    .or_else(|| input.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
            )?);
            let text = require_string_field(input, "text")?;
            let search_mode = input
                .get("search_mode")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text_value = text.to_string();
            let result = self.run_controller_binding_operation(
                "tokenize_text",
                Some(format!(
                    "tokenizer_mode={} search_mode={}",
                    controller_tokenizer_mode_name(tokenizer_mode),
                    search_mode
                )),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .tokenize_sqlite_text(
                                ids.space_id,
                                ids.binding_id,
                                tokenizer_mode,
                                text_value,
                                search_mode,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": true,
                "tokenizer_mode": result.tokenizer_mode,
                "normalized_text": result.normalized_text,
                "fts_query": result.fts_query,
                "tokens": result.tokens,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::TokenizeText, input);
        }
        let api = self.api_ref()?;
        let tokenizer_mode = parse_tokenizer_mode(
            input
                .get("tokenizer_mode")
                .or_else(|| input.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        let text = require_string_field(input, "text")?;
        let search_mode = input
            .get("search_mode")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        self.log_info(
            "tokenize_text",
            Some(format!(
                "tokenizer_mode={} search_mode={}",
                tokenizer_mode_name(tokenizer_mode),
                search_mode
            )),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let text_cstr = to_cstring(text, "text")?;
        unsafe {
            let handle = (api.database_tokenize_text)(
                guard.database,
                tokenizer_mode,
                text_cstr.as_ptr(),
                bool_to_u8(search_mode),
            );
            if handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "tokenize_text", guard));
            }

            // Extract tokenization fields as one fallible payload so the native handle is destroyed on every path.
            // 将分词字段作为一个可失败载荷提取，确保所有路径都会销毁原生句柄。
            let tokenized = (|| -> Result<(String, String, Vec<Value>), String> {
                let normalized_text =
                    api.take_owned_string((api.tokenize_result_normalized_text)(handle))?;
                let fts_query = api.take_owned_string((api.tokenize_result_fts_query)(handle))?;
                let token_count = (api.tokenize_result_token_count)(handle);
                let mut tokens = Vec::with_capacity(token_count as usize);
                for index in 0..token_count {
                    if let Some(token) =
                        api.take_optional_string((api.tokenize_result_get_token)(handle, index))?
                    {
                        tokens.push(Value::String(token));
                    }
                }
                Ok((normalized_text, fts_query, tokens))
            })();
            (api.tokenize_result_destroy)(handle);
            drop(guard);
            let (normalized_text, fts_query, tokens) = tokenized?;
            self.log_if_slow("tokenize_text", started_at, None);
            Ok(json!({
                "success": true,
                "tokenizer_mode": tokenizer_mode_name(tokenizer_mode),
                "normalized_text": normalized_text,
                "fts_query": fts_query,
                "tokens": tokens,
            }))
        }
    }

    /// Insert or update a custom dictionary word.
    /// 写入或更新自定义词。
    pub fn upsert_custom_word_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let word = require_string_field(input, "word")?;
            let weight = input.get("weight").and_then(Value::as_u64).unwrap_or(1);
            let word_value = word.to_string();
            let weight_value = u32::try_from(weight).unwrap_or(u32::MAX);
            let result = self.run_controller_binding_operation(
                "upsert_custom_word",
                Some(format!("word={}", word)),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .upsert_sqlite_custom_word(
                                ids.space_id,
                                ids.binding_id,
                                word_value,
                                weight_value,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "affected_rows": result.affected_rows,
                "word": word,
                "weight": weight,
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::UpsertCustomWord, input);
        }
        let api = self.api_ref()?;
        let word = require_string_field(input, "word")?;
        let weight = input.get("weight").and_then(Value::as_u64).unwrap_or(1);
        self.log_info("upsert_custom_word", Some(format!("word={}", word)));
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let word_cstr = to_cstring(word, "word")?;
        let mut result = VldbSqliteDictionaryMutationResultPod {
            success: 0,
            affected_rows: 0,
        };
        let status = unsafe {
            (api.database_upsert_custom_word)(
                guard.database,
                word_cstr.as_ptr(),
                weight,
                &mut result,
            )
        };
        drop(guard);
        self.log_if_slow("upsert_custom_word", started_at, None);
        ensure_status(api, status, "upsert_custom_word")?;
        Ok(json!({
            "success": u8_to_bool(result.success),
            "affected_rows": result.affected_rows,
            "word": word,
            "weight": weight,
        }))
    }

    /// Remove a custom dictionary word.
    /// 删除自定义词。
    pub fn remove_custom_word_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let word = require_string_field(input, "word")?;
            let word_value = word.to_string();
            let result = self.run_controller_binding_operation(
                "remove_custom_word",
                Some(format!("word={}", word)),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .remove_sqlite_custom_word(ids.space_id, ids.binding_id, word_value)
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "affected_rows": result.affected_rows,
                "word": word,
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::RemoveCustomWord, input);
        }
        let api = self.api_ref()?;
        let word = require_string_field(input, "word")?;
        self.log_info("remove_custom_word", Some(format!("word={}", word)));
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let word_cstr = to_cstring(word, "word")?;
        let mut result = VldbSqliteDictionaryMutationResultPod {
            success: 0,
            affected_rows: 0,
        };
        let status = unsafe {
            (api.database_remove_custom_word)(guard.database, word_cstr.as_ptr(), &mut result)
        };
        drop(guard);
        self.log_if_slow("remove_custom_word", started_at, None);
        ensure_status(api, status, "remove_custom_word")?;
        Ok(json!({
            "success": u8_to_bool(result.success),
            "affected_rows": result.affected_rows,
            "word": word,
        }))
    }

    /// List enabled custom dictionary words from the current database.
    /// 列出当前数据库中启用的自定义词。
    pub fn list_custom_words_json(&self) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            self.log_info("list_custom_words", None);
            let started_at = Instant::now();
            let (bridge, controller_ids) = self.controller_call_context()?;
            let result = bridge.run(move |client| async move {
                client
                    .list_sqlite_custom_words(controller_ids.space_id, controller_ids.binding_id)
                    .await
            })?;
            self.log_if_slow(
                "list_custom_words",
                started_at,
                Some(format!("count={}", result.words.len())),
            );
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "total": result.words.len(),
                "words": result.words.into_iter().map(|entry| json!({
                    "word": entry.word,
                    "weight": entry.weight,
                })).collect::<Vec<_>>(),
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(
                RuntimeSqliteProviderAction::ListCustomWords,
                &Value::Object(Default::default()),
            );
        }
        let api = self.api_ref()?;
        self.log_info("list_custom_words", None);
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        unsafe {
            let list_handle = (api.database_list_custom_words)(guard.database);
            if list_handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "list_custom_words", guard));
            }

            let len = (api.custom_word_list_len)(list_handle);
            let words = (|| -> Result<Vec<Value>, String> {
                let mut words = Vec::with_capacity(len as usize);
                for index in 0..len {
                    let word = require_sqlite_dynamic_string(
                        api.take_optional_string((api.custom_word_list_get_word)(
                            list_handle,
                            index,
                        ))?,
                        "custom_word.word",
                    )?;
                    let weight = (api.custom_word_list_get_weight)(list_handle, index);
                    words.push(json!({
                        "word": word,
                        "weight": weight,
                    }));
                }
                Ok(words)
            })();
            (api.custom_word_list_destroy)(list_handle);
            drop(guard);
            let words = words?;
            self.log_if_slow(
                "list_custom_words",
                started_at,
                Some(format!("count={}", len)),
            );
            Ok(json!({
                "success": true,
                "total": len,
                "words": words,
            }))
        }
    }

    /// Ensure the specified FTS index exists.
    /// 确保指定 FTS 索引存在。
    pub fn ensure_fts_index_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let index_name = require_string_field(input, "index_name")?;
            let tokenizer_mode = map_controller_tokenizer_mode(parse_tokenizer_mode(
                input
                    .get("tokenizer_mode")
                    .or_else(|| input.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
            )?);
            let index_name_value = index_name.to_string();
            let result = self.run_controller_binding_operation(
                "ensure_fts_index",
                Some(format!("index_name={}", index_name)),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .ensure_sqlite_fts_index(
                                ids.space_id,
                                ids.binding_id,
                                index_name_value,
                                tokenizer_mode,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "index_name": result.index_name,
                "tokenizer_mode": result.tokenizer_mode,
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::EnsureFtsIndex, input);
        }
        let api = self.api_ref()?;
        let index_name = require_string_field(input, "index_name")?;
        let tokenizer_mode = parse_tokenizer_mode(
            input
                .get("tokenizer_mode")
                .or_else(|| input.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        self.log_info(
            "ensure_fts_index",
            Some(format!("index_name={}", index_name)),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let index_cstr = to_cstring(index_name, "index_name")?;
        let mut result = VldbSqliteEnsureFtsIndexResultPod {
            success: 0,
            tokenizer_mode: tokenizer_mode as u32,
        };
        let status = unsafe {
            (api.database_ensure_fts_index)(
                guard.database,
                index_cstr.as_ptr(),
                tokenizer_mode,
                &mut result,
            )
        };
        drop(guard);
        self.log_if_slow("ensure_fts_index", started_at, None);
        ensure_status(api, status, "ensure_fts_index")?;
        Ok(json!({
            "success": u8_to_bool(result.success),
            "index_name": index_name,
            "tokenizer_mode": tokenizer_mode_name_from_u32(result.tokenizer_mode),
        }))
    }

    /// Rebuild an FTS index using the current dictionary and tokenizer mode.
    /// 使用当前词典和分词模式重建 FTS 索引。
    pub fn rebuild_fts_index_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let index_name = require_string_field(input, "index_name")?;
            let tokenizer_mode = map_controller_tokenizer_mode(parse_tokenizer_mode(
                input
                    .get("tokenizer_mode")
                    .or_else(|| input.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
            )?);
            let index_name_value = index_name.to_string();
            let result = self.run_controller_binding_operation(
                "rebuild_fts_index",
                Some(format!("index_name={}", index_name)),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .rebuild_sqlite_fts_index(
                                ids.space_id,
                                ids.binding_id,
                                index_name_value,
                                tokenizer_mode,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "index_name": result.index_name,
                "tokenizer_mode": result.tokenizer_mode,
                "reindexed_rows": result.reindexed_rows,
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::RebuildFtsIndex, input);
        }
        let api = self.api_ref()?;
        let index_name = require_string_field(input, "index_name")?;
        let tokenizer_mode = parse_tokenizer_mode(
            input
                .get("tokenizer_mode")
                .or_else(|| input.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        self.log_info(
            "rebuild_fts_index",
            Some(format!("index_name={}", index_name)),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let index_cstr = to_cstring(index_name, "index_name")?;
        let mut result = VldbSqliteRebuildFtsIndexResultPod {
            success: 0,
            tokenizer_mode: tokenizer_mode as u32,
            reindexed_rows: 0,
        };
        let status = unsafe {
            (api.database_rebuild_fts_index)(
                guard.database,
                index_cstr.as_ptr(),
                tokenizer_mode,
                &mut result,
            )
        };
        drop(guard);
        self.log_if_slow("rebuild_fts_index", started_at, None);
        ensure_status(api, status, "rebuild_fts_index")?;
        Ok(json!({
            "success": u8_to_bool(result.success),
            "index_name": index_name,
            "tokenizer_mode": tokenizer_mode_name_from_u32(result.tokenizer_mode),
            "reindexed_rows": result.reindexed_rows,
        }))
    }

    /// Insert or update a single FTS document.
    /// 写入或更新一条 FTS 文档。
    pub fn upsert_fts_document_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let index_name = require_string_field(input, "index_name")?;
            let tokenizer_mode = map_controller_tokenizer_mode(parse_tokenizer_mode(
                input
                    .get("tokenizer_mode")
                    .or_else(|| input.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
            )?);
            let id = require_string_field(input, "id")?;
            let file_path = require_string_field(input, "file_path")?;
            let title = require_present_string_field(input, "title")?;
            let content = require_present_string_field(input, "content")?;
            let index_name_value = index_name.to_string();
            let id_value = id.to_string();
            let file_path_value = file_path.to_string();
            let title_value = title.to_string();
            let content_value = content.to_string();
            let result = self.run_controller_binding_operation(
                "upsert_fts_document",
                Some(format!("index_name={} id={}", index_name, id)),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .upsert_sqlite_fts_document(
                                ids.space_id,
                                ids.binding_id,
                                index_name_value,
                                tokenizer_mode,
                                id_value,
                                file_path_value,
                                title_value,
                                content_value,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "affected_rows": result.affected_rows,
                "index_name": result.index_name,
                "id": id,
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::UpsertFtsDocument, input);
        }
        let api = self.api_ref()?;
        let index_name = require_string_field(input, "index_name")?;
        let tokenizer_mode = parse_tokenizer_mode(
            input
                .get("tokenizer_mode")
                .or_else(|| input.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        let id = require_string_field(input, "id")?;
        let file_path = require_string_field(input, "file_path")?;
        let title = require_present_string_field(input, "title")?;
        let content = require_present_string_field(input, "content")?;
        self.log_info(
            "upsert_fts_document",
            Some(format!("index_name={} id={}", index_name, id)),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let index_cstr = to_cstring(index_name, "index_name")?;
        let id_cstr = to_cstring(id, "id")?;
        let file_path_cstr = to_cstring(file_path, "file_path")?;
        let title_cstr = to_cstring(title, "title")?;
        let content_cstr = to_cstring(content, "content")?;
        let mut result = VldbSqliteFtsMutationResultPod {
            success: 0,
            affected_rows: 0,
        };
        let status = unsafe {
            (api.database_upsert_fts_document)(
                guard.database,
                index_cstr.as_ptr(),
                tokenizer_mode,
                id_cstr.as_ptr(),
                file_path_cstr.as_ptr(),
                title_cstr.as_ptr(),
                content_cstr.as_ptr(),
                &mut result,
            )
        };
        drop(guard);
        self.log_if_slow("upsert_fts_document", started_at, None);
        ensure_status(api, status, "upsert_fts_document")?;
        Ok(json!({
            "success": u8_to_bool(result.success),
            "affected_rows": result.affected_rows,
            "index_name": index_name,
            "id": id,
        }))
    }

    /// Delete a single FTS document.
    /// 删除一条 FTS 文档。
    pub fn delete_fts_document_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let index_name = require_string_field(input, "index_name")?;
            let id = require_string_field(input, "id")?;
            let index_name_value = index_name.to_string();
            let id_value = id.to_string();
            let result = self.run_controller_binding_operation(
                "delete_fts_document",
                Some(format!("index_name={} id={}", index_name, id)),
                None,
                |bridge, ids| {
                    bridge.run(move |client| async move {
                        client
                            .delete_sqlite_fts_document(
                                ids.space_id,
                                ids.binding_id,
                                index_name_value,
                                id_value,
                            )
                            .await
                    })
                },
            )?;
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "affected_rows": result.affected_rows,
                "index_name": result.index_name,
                "id": id,
            }));
        }
        if self.is_host_provider_mode() {
            return self
                .dispatch_host_provider(RuntimeSqliteProviderAction::DeleteFtsDocument, input);
        }
        let api = self.api_ref()?;
        let index_name = require_string_field(input, "index_name")?;
        let id = require_string_field(input, "id")?;
        self.log_info(
            "delete_fts_document",
            Some(format!("index_name={} id={}", index_name, id)),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let index_cstr = to_cstring(index_name, "index_name")?;
        let id_cstr = to_cstring(id, "id")?;
        let mut result = VldbSqliteFtsMutationResultPod {
            success: 0,
            affected_rows: 0,
        };
        let status = unsafe {
            (api.database_delete_fts_document)(
                guard.database,
                index_cstr.as_ptr(),
                id_cstr.as_ptr(),
                &mut result,
            )
        };
        drop(guard);
        self.log_if_slow("delete_fts_document", started_at, None);
        ensure_status(api, status, "delete_fts_document")?;
        Ok(json!({
            "success": u8_to_bool(result.success),
            "affected_rows": result.affected_rows,
            "index_name": index_name,
            "id": id,
        }))
    }

    /// Execute FTS search and return a rich result payload.
    /// 执行 FTS 检索并返回富结果结构。
    pub fn search_fts_json(&self, input: &Value) -> Result<Value, String> {
        if self.is_space_controller_mode() {
            let index_name = require_string_field(input, "index_name")?;
            let tokenizer_mode = map_controller_tokenizer_mode(parse_tokenizer_mode(
                input
                    .get("tokenizer_mode")
                    .or_else(|| input.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
            )?);
            let query = require_string_field(input, "query")?;
            let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(10);
            let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
            self.log_info(
                "search_fts",
                Some(format!(
                    "index_name={} tokenizer_mode={} limit={} offset={}",
                    index_name,
                    controller_tokenizer_mode_name(tokenizer_mode),
                    limit,
                    offset
                )),
            );
            let started_at = Instant::now();
            let (bridge, controller_ids) = self.controller_call_context()?;
            let index_name_value = index_name.to_string();
            let query_value = query.to_string();
            let limit_value = u32::try_from(limit).unwrap_or(u32::MAX);
            let offset_value = u32::try_from(offset).unwrap_or(u32::MAX);
            let result = bridge.run(move |client| async move {
                client
                    .search_sqlite_fts(
                        controller_ids.space_id,
                        controller_ids.binding_id,
                        index_name_value,
                        tokenizer_mode,
                        query_value,
                        limit_value,
                        offset_value,
                    )
                    .await
            })?;
            self.log_if_slow(
                "search_fts",
                started_at,
                Some(format!("hits={}", result.total)),
            );
            return Ok(json!({
                "success": result.success,
                "message": result.message,
                "index_name": result.index_name,
                "tokenizer_mode": result.tokenizer_mode,
                "normalized_query": result.normalized_query,
                "fts_query": result.fts_query,
                "source": result.source,
                "query_mode": result.query_mode,
                "total": result.total,
                "hits": result.hits.into_iter().map(|hit| json!({
                    "id": hit.id,
                    "file_path": hit.file_path,
                    "title": hit.title,
                    "title_highlight": hit.title_highlight,
                    "content_snippet": hit.content_snippet,
                    "score": hit.score,
                    "rank": hit.rank,
                    "raw_score": hit.raw_score,
                })).collect::<Vec<_>>(),
            }));
        }
        if self.is_host_provider_mode() {
            return self.dispatch_host_provider(RuntimeSqliteProviderAction::SearchFts, input);
        }
        let api = self.api_ref()?;
        let index_name = require_string_field(input, "index_name")?;
        let tokenizer_mode = parse_tokenizer_mode(
            input
                .get("tokenizer_mode")
                .or_else(|| input.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("none"),
        )?;
        let query = require_string_field(input, "query")?;
        let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32;
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0) as u32;
        self.log_info(
            "search_fts",
            Some(format!(
                "index_name={} tokenizer_mode={} limit={} offset={}",
                index_name,
                tokenizer_mode_name(tokenizer_mode),
                limit,
                offset
            )),
        );
        let started_at = Instant::now();
        let guard = self.lock_handles()?;
        let index_cstr = to_cstring(index_name, "index_name")?;
        let query_cstr = to_cstring(query, "query")?;
        unsafe {
            let result_handle = (api.database_search_fts)(
                guard.database,
                index_cstr.as_ptr(),
                tokenizer_mode,
                query_cstr.as_ptr(),
                limit,
                offset,
            );
            if result_handle.is_null() {
                return Err(self.take_ffi_null_handle_error(api, "search_fts", guard));
            }

            let result = (|| -> Result<Value, String> {
                let total = (api.search_result_total)(result_handle);
                let len = (api.search_result_len)(result_handle);
                let source = api
                    .take_optional_string((api.search_result_source)(result_handle))?
                    .unwrap_or_else(|| "sqlite_fts".to_string());
                let query_mode = api
                    .take_optional_string((api.search_result_query_mode)(result_handle))?
                    .unwrap_or_else(|| "fts".to_string());
                let mut hits = Vec::with_capacity(len as usize);
                for index in 0..len {
                    // Required hit identifier returned by the dynamic search result handle.
                    // 动态检索结果句柄返回的必需命中标识。
                    let id = require_sqlite_dynamic_string(
                        api.take_optional_string((api.search_result_get_id)(result_handle, index))?,
                        "fts_hit.id",
                    )?;
                    // Required hit file or logical path returned by the dynamic search result handle.
                    // 动态检索结果句柄返回的必需命中文件路径或逻辑路径。
                    let file_path = require_sqlite_dynamic_string(
                        api.take_optional_string((api.search_result_get_file_path)(
                            result_handle,
                            index,
                        ))?,
                        "fts_hit.file_path",
                    )?;
                    // Required raw title returned by the dynamic search result handle.
                    // 动态检索结果句柄返回的必需原始标题。
                    let title = require_sqlite_dynamic_string(
                        api.take_optional_string((api.search_result_get_title)(
                            result_handle,
                            index,
                        ))?,
                        "fts_hit.title",
                    )?;
                    // Required highlighted title returned by the dynamic search result handle.
                    // 动态检索结果句柄返回的必需高亮标题。
                    let title_highlight = require_sqlite_dynamic_string(
                        api.take_optional_string((api.search_result_get_title_highlight)(
                            result_handle,
                            index,
                        ))?,
                        "fts_hit.title_highlight",
                    )?;
                    // Required highlighted content snippet returned by the dynamic search result handle.
                    // 动态检索结果句柄返回的必需高亮正文片段。
                    let content_snippet = require_sqlite_dynamic_string(
                        api.take_optional_string((api.search_result_get_content_snippet)(
                            result_handle,
                            index,
                        ))?,
                        "fts_hit.content_snippet",
                    )?;
                    hits.push(json!({
                        "id": id,
                        "file_path": file_path,
                        "title": title,
                        "title_highlight": title_highlight,
                        "content_snippet": content_snippet,
                        "score": (api.search_result_get_score)(result_handle, index),
                        "rank": (api.search_result_get_rank)(result_handle, index),
                        "raw_score": (api.search_result_get_raw_score)(result_handle, index),
                    }));
                }
                Ok(json!({
                    "success": true,
                    "index_name": index_name,
                    "tokenizer_mode": tokenizer_mode_name(tokenizer_mode),
                    "source": source,
                    "query_mode": query_mode,
                    "total": total,
                    "hits": hits,
                }))
            })();
            (api.search_result_destroy)(result_handle);
            drop(guard);
            let result = result?;
            let hit_count = result
                .get("hits")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            self.log_if_slow("search_fts", started_at, Some(format!("hits={hit_count}")));
            Ok(result)
        }
    }

    /// Emit informational logs according to the configured skill policy.
    /// 按配置输出普通信息级日志。
    fn log_info(&self, operation: &str, extra: Option<String>) {
        if self.config.log_level == SkillSqliteLogLevel::Info {
            match extra {
                Some(extra) => log_info(format!(
                    "[Sqlite:info] skill={} db={} op={} {}",
                    self.skill_name, self.skill_dir_name, operation, extra
                )),
                None => log_info(format!(
                    "[Sqlite:info] skill={} db={} op={}",
                    self.skill_name, self.skill_dir_name, operation
                )),
            }
        }
    }

    /// Emit slow-operation warnings according to the slow-log configuration.
    /// 按慢日志配置输出慢操作告警。
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
                "[Sqlite:slow] skill={} db={} op={} elapsed_ms={} {}",
                self.skill_name, self.skill_dir_name, operation, elapsed_ms, extra
            )),
            None => log_info(format!(
                "[Sqlite:slow] skill={} db={} op={} elapsed_ms={}",
                self.skill_name, self.skill_dir_name, operation, elapsed_ms
            )),
        }
    }

    /// Emit warning-level logs according to configuration, usually for FFI call failures.
    /// 按配置输出告警级日志，通常用于 FFI 调用失败。
    fn log_warning(&self, operation: &str, message: &str) {
        if matches!(
            self.config.log_level,
            SkillSqliteLogLevel::Info | SkillSqliteLogLevel::Warning
        ) {
            log_warn(format!(
                "[Sqlite:warn] skill={} db={} op={} message={}",
                self.skill_name, self.skill_dir_name, operation, message
            ));
        }
    }

    /// Release one held SQLite handle lock, read the native last error, and log it for a failed FFI handle allocation.
    /// 释放一个已持有的 SQLite 句柄锁，读取原生 last error，并为失败的 FFI handle 分配记录告警。
    ///
    /// The api parameter owns the loaded dynamic-library symbols used to read the native last error.
    /// api 参数持有读取原生 last error 所需的动态库符号。
    ///
    /// The operation parameter is the stable operation name used by warning logs.
    /// operation 参数是告警日志使用的稳定操作名称。
    ///
    /// The guard parameter is consumed so the native handle lock is released before the last-error call.
    /// guard 参数会被消费，确保在读取 last-error 前释放原生句柄锁。
    ///
    /// Return the native error message that should be returned to the caller.
    /// 返回应传递给调用方的原生错误消息。
    fn take_ffi_null_handle_error(
        &self,
        api: &LoadedSqliteApi,
        operation: &str,
        guard: std::sync::MutexGuard<'_, SkillHandleState>,
    ) -> String {
        drop(guard);
        // Capture the native last-error message after the handle lock has been released.
        // 在句柄锁释放后捕获原生 last-error 消息。
        let error = api.take_last_error_message();
        self.log_warning(operation, &error);
        error
    }

    /// Acquire the handle lock so SQLite FFI calls for the same skill execute serially.
    /// 获取句柄锁，确保同一个 skill 的 SQLite FFI 调用按顺序串行执行。
    fn lock_handles(&self) -> Result<std::sync::MutexGuard<'_, SkillHandleState>, String> {
        self.handles
            .as_ref()
            .ok_or_else(|| {
                "SQLite dynamic-library handles are unavailable in host provider mode".to_string()
            })?
            .lock()
            .map_err(|_| "failed to acquire SQLite handle lock".to_string())
    }

    /// Return whether the current binding dispatches requests into one host provider.
    /// 返回当前绑定是否把请求转发给宿主 provider。
    fn is_host_provider_mode(&self) -> bool {
        self.provider_mode == SqliteBindingMode::HostCallback
    }

    /// Return whether the current binding dispatches requests into one external space controller.
    /// 返回当前绑定是否把请求转发给外部空间控制器。
    fn is_space_controller_mode(&self) -> bool {
        self.provider_mode == SqliteBindingMode::SpaceController
    }

    /// Return the loaded dynamic-library API for dynamic mode bindings, or an explicit binding-state error.
    /// 返回动态模式绑定所对应的已加载动态库 API；若绑定状态不一致则返回显式错误。
    fn api_ref(&self) -> Result<&LoadedSqliteApi, String> {
        self.api.as_deref().ok_or_else(|| {
            format!(
                "SQLite dynamic-library API is unavailable for {} binding",
                self.integration_mode_name()
            )
        })
    }

    /// Return the stable integration mode name for diagnostics and Lua status payloads.
    /// 返回用于诊断和 Lua 状态输出的稳定集成模式名称。
    fn integration_mode_name(&self) -> &'static str {
        match self.provider_mode {
            SqliteBindingMode::DynamicLibrary => "dynamic_library",
            SqliteBindingMode::HostCallback => "host_callback",
            SqliteBindingMode::SpaceController => "space_controller",
        }
    }

    /// Dispatch one SQLite operation through the host-registered provider contract.
    /// 通过宿主已注册的 provider 协议分发一次 SQLite 操作。
    fn dispatch_host_provider(
        &self,
        action: RuntimeSqliteProviderAction,
        input: &Value,
    ) -> Result<Value, String> {
        let request = RuntimeSqliteProviderRequest {
            action,
            binding: self.provider_binding.clone(),
            input: input.clone(),
        };
        self.provider_callbacks
            .dispatch_sqlite_provider_request(&request, self.host_callback_mode())
    }

    /// Return the configured callback transport mode for the current host-managed binding.
    /// 返回当前宿主管理绑定所配置的回调传输模式。
    fn host_callback_mode(&self) -> LuaRuntimeDatabaseCallbackMode {
        self.callback_mode
    }

    /// Return the shared controller bridge for one space-controller binding.
    /// 返回 space-controller 绑定所使用的共享控制器桥接。
    fn controller_bridge(&self) -> Result<&Arc<LuaRuntimeSpaceControllerBridge>, String> {
        self.controller
            .as_ref()
            .ok_or_else(|| "SQLite space-controller bridge is unavailable".to_string())
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

    /// Run one SQLite controller operation with shared logging, timing, and binding identifiers.
    /// 使用共享日志、计时与绑定标识执行一次 SQLite 控制器操作。
    ///
    /// The operation parameter is the stable operation name used by normal and slow logs.
    /// operation 参数是普通日志与慢日志使用的稳定操作名称。
    ///
    /// The log_extra parameter is the optional detail emitted before the controller call.
    /// log_extra 参数是控制器调用前输出的可选细节。
    ///
    /// The slow_extra parameter is the optional detail emitted when the slow-log threshold is reached.
    /// slow_extra 参数是达到慢日志阈值时输出的可选细节。
    ///
    /// The invoke parameter performs the provider-specific controller SDK call.
    /// invoke 参数执行 provider 专属的控制器 SDK 调用。
    ///
    /// Return the provider-specific controller response returned by the invoke closure.
    /// 返回 invoke 闭包产出的 provider 专属控制器响应。
    fn run_controller_binding_operation<T, F>(
        &self,
        operation: &str,
        log_extra: Option<String>,
        slow_extra: Option<String>,
        invoke: F,
    ) -> Result<T, String>
    where
        F: FnOnce(
            &Arc<LuaRuntimeSpaceControllerBridge>,
            LuaRuntimeSpaceControllerBindingIds,
        ) -> Result<T, String>,
    {
        self.log_info(operation, log_extra);
        let started_at = Instant::now();
        let (bridge, controller_ids) = self.controller_call_context()?;
        let result = invoke(bridge, controller_ids)?;
        self.log_if_slow(operation, started_at, slow_extra);
        Ok(result)
    }
}

impl Drop for SqliteSkillBinding {
    /// Release the database handle and runtime together when the skill binding is dropped.
    /// 在 skill 生命周期结束时统一释放数据库句柄与 runtime。
    fn drop(&mut self) {
        let Some(handles) = self.handles.as_ref() else {
            return;
        };
        let Some(api) = self.api.as_ref() else {
            return;
        };
        if let Ok(mut guard) = handles.lock() {
            unsafe {
                for (_, stream_handle) in guard.query_streams.drain() {
                    if !stream_handle.is_null() {
                        (api.query_stream_destroy)(stream_handle);
                    }
                }
                if !guard.database.is_null() {
                    (api.database_destroy)(guard.database);
                    guard.database = ptr::null_mut();
                }
                if !guard.runtime.is_null() {
                    (api.runtime_destroy)(guard.runtime);
                    guard.runtime = ptr::null_mut();
                }
            }
        }
    }
}

/// Maintain SQLite bindings per skill, auto-creating and reusing them for enabled skills.
/// 按 skill 维度维护 SQLite 绑定，负责启用后的自动创建与长期复用。
pub struct SqliteSkillHost {
    api: Option<Arc<LoadedSqliteApi>>,
    controller: Option<Arc<LuaRuntimeSpaceControllerBridge>>,
    skills: Mutex<HashMap<String, Arc<SqliteSkillBinding>>>,
    provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    host_options: LuaRuntimeHostOptions,
}

/// Resolve the optional SQLite dynamic-library API for the selected provider mode.
/// 根据选定的 provider 模式解析可选 SQLite 动态库 API。
///
/// The host_options parameter carries the provider mode and dynamic-library path selected by the host.
/// host_options 参数携带宿主选择的 provider 模式与动态库路径。
///
/// The provider_callbacks parameter is the engine-captured provider callback snapshot used by host-callback mode.
/// provider_callbacks 参数是 host-callback 模式使用的引擎级 provider 回调快照。
///
/// Return a loaded dynamic-library API only when SQLite runs in dynamic-library mode.
/// 仅当 SQLite 运行在 dynamic-library 模式时返回已加载的动态库 API。
fn resolve_sqlite_skill_host_api(
    host_options: &LuaRuntimeHostOptions,
    provider_callbacks: &RuntimeDatabaseProviderCallbacks,
) -> Result<Option<Arc<LoadedSqliteApi>>, String> {
    match host_options.sqlite_provider_mode {
        LuaRuntimeDatabaseProviderMode::DynamicLibrary => {
            let library_path = host_options.sqlite_library_path.clone().ok_or_else(|| {
                "SQLite dynamic-library mode requires host_options.sqlite_library_path".to_string()
            })?;
            Ok(Some(Arc::new(LoadedSqliteApi::load(&library_path)?)))
        }
        LuaRuntimeDatabaseProviderMode::HostCallback => {
            require_database_provider_callback_registration(
                "SQLite",
                host_options.sqlite_callback_mode,
                provider_callbacks
                    .has_sqlite_provider_callback_for_mode(host_options.sqlite_callback_mode),
            )?;
            Ok(None)
        }
        LuaRuntimeDatabaseProviderMode::SpaceController => Ok(None),
    }
}

/// Resolve the optional SQLite space-controller bridge for the selected provider mode.
/// 根据选定的 provider 模式解析可选 SQLite space-controller 桥接。
///
/// The host_options parameter carries the provider mode and controller connection settings.
/// host_options 参数携带 provider 模式与控制器连接设置。
///
/// Return a controller bridge only when SQLite runs in space-controller mode.
/// 仅当 SQLite 运行在 space-controller 模式时返回控制器桥接。
fn resolve_sqlite_skill_host_controller(
    host_options: &LuaRuntimeHostOptions,
) -> Result<Option<Arc<LuaRuntimeSpaceControllerBridge>>, String> {
    match host_options.sqlite_provider_mode {
        LuaRuntimeDatabaseProviderMode::SpaceController => Ok(Some(
            LuaRuntimeSpaceControllerBridge::new(host_options, "sqlite")?,
        )),
        _ => Ok(None),
    }
}

impl SqliteSkillHost {
    /// Create the host-side SQLite skill manager and resolve resources for the selected provider mode.
    /// 创建宿主级 SQLite 技能管理器，并解析所选 provider 模式需要的资源。
    pub fn new(
        host_options: LuaRuntimeHostOptions,
        provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    ) -> Result<Self, String> {
        let api = resolve_sqlite_skill_host_api(&host_options, provider_callbacks.as_ref())?;
        let controller = resolve_sqlite_skill_host_controller(&host_options)?;
        Ok(Self {
            api,
            controller,
            skills: Mutex::new(HashMap::new()),
            provider_callbacks,
            host_options,
        })
    }

    /// Register a fixed database binding for an SQLite-enabled skill; each skill is created only once.
    /// 为启用 SQLite 的 skill 注册固定数据库绑定；同一个 skill 只会创建一次。
    pub fn register_skill(
        &self,
        root_name: &str,
        skill_name: &str,
        skill_dir: &Path,
        config: SkillSqliteMeta,
    ) -> Result<Arc<SqliteSkillBinding>, String> {
        let mut guard = self.lock_skills();
        if let Some(existing) = guard.get(skill_name) {
            return Ok(existing.clone());
        }

        let binding_plan = build_runtime_database_binding_plan(
            root_name,
            skill_name,
            skill_dir,
            self.host_options.database_dir_name.as_str(),
            RuntimeDatabaseKind::Sqlite,
        )?;
        let skill_dir_name = binding_plan.skill_dir_name;
        let db_dir = binding_plan.provider_storage_dir;
        let database_path = binding_plan.default_database_path;
        let binding_context = binding_plan.context;

        let (resolved_path, handles, provider_mode, controller) = if let Some(api) =
            self.api.as_ref()
        {
            std::fs::create_dir_all(&db_dir).map_err(|error| {
                format!(
                    "failed to create SQLite directory {}: {}",
                    render_host_visible_path(&db_dir),
                    error
                )
            })?;
            let database_cstr = CString::new(database_path.clone())
                .map_err(|_| "database path contains interior NUL bytes".to_string())?;
            let runtime = unsafe { (api.runtime_create_default)() };
            if runtime.is_null() {
                return Err(api.take_last_error_message());
            }

            let database = unsafe { (api.runtime_open_database)(runtime, database_cstr.as_ptr()) };
            if database.is_null() {
                unsafe {
                    (api.runtime_destroy)(runtime);
                }
                return Err(api.take_last_error_message());
            }

            let resolved_path = unsafe { api.take_owned_string((api.database_db_path)(database)) }
                .unwrap_or(database_path.clone());
            (
                resolved_path,
                Some(Mutex::new(SkillHandleState {
                    runtime,
                    database,
                    query_streams: HashMap::new(),
                    next_stream_id: 1,
                })),
                SqliteBindingMode::DynamicLibrary,
                None,
            )
        } else if matches!(
            self.host_options.sqlite_provider_mode,
            LuaRuntimeDatabaseProviderMode::SpaceController
        ) {
            let controller = self
                .controller
                .as_ref()
                .ok_or_else(|| "SQLite space-controller bridge is unavailable".to_string())?
                .clone();
            let controller_ids = controller.attach_binding_with_ids(&binding_context)?;
            let controller_database_path = database_path.clone();
            controller.run(move |client| async move {
                client
                    .enable_sqlite(ControllerSqliteEnableRequest {
                        space_id: controller_ids.space_id,
                        binding_id: controller_ids.binding_id,
                        db_path: controller_database_path,
                        // Controller mode already centralizes ownership in one dedicated process.
                        // 控制器模式已经把数据库所有权集中到单独控制进程，不再额外启用文件锁。
                        enforce_db_file_lock: false,
                        ..ControllerSqliteEnableRequest::default()
                    })
                    .await
            })?;
            (
                database_path.clone(),
                None,
                SqliteBindingMode::SpaceController,
                Some(controller),
            )
        } else {
            (
                database_path.clone(),
                None,
                SqliteBindingMode::HostCallback,
                None,
            )
        };

        let binding = Arc::new(SqliteSkillBinding {
            api: self.api.clone(),
            skill_name: skill_name.to_string(),
            skill_dir_name,
            database_path: resolved_path,
            config,
            provider_mode,
            callback_mode: self.host_options.sqlite_callback_mode,
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
    ) -> Result<Option<Arc<SqliteSkillBinding>>, String> {
        let skills = self.lock_skills();
        Ok(skills.get(skill_name).cloned())
    }

    /// Acquire the SQLite skill binding registry and return its guard, recovering after registry lock poisoning.
    /// 获取并返回 SQLite skill binding 注册表保护对象；如果注册表锁已 poison，则恢复继续使用。
    fn lock_skills(&self) -> MutexGuard<'_, HashMap<String, Arc<SqliteSkillBinding>>> {
        self.skills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Build a stable status object for skills without SQLite enabled so Lua can check before calling.
/// 为未启用 SQLite 的 skill 生成稳定状态对象，便于 Lua 侧先判断再调用。
pub fn disabled_skill_status_json(skill_name: Option<&str>) -> Value {
    json!({
        "enabled": false,
        "initialized": false,
        "skill_name": skill_name.unwrap_or(""),
        "integration_mode": "dynamic_library",
        "reason": "current skill has not enabled sqlite"
    })
}

/// Return the Lua-facing SQLite library path value without inventing an empty placeholder.
/// 返回面向 Lua 的 SQLite library path 值，不制造空字符串占位。
fn sqlite_library_path_value(api: Option<&LoadedSqliteApi>) -> Value {
    api.map(|api| Value::String(render_host_visible_path(&api.library_path)))
        .unwrap_or(Value::Null)
}

/// Validate the dynamic SQLite library metadata shape returned by `vldb_sqlite_library_info_json`.
/// 校验 `vldb_sqlite_library_info_json` 返回的动态 SQLite 库元数据结构。
fn validate_sqlite_library_info(value: &Value, context: &str) -> Result<(), String> {
    if !value.is_object() {
        return Err(format!("SQLite dynamic {context} must be a JSON object"));
    }
    required_sqlite_library_info_string(value, "name").map(|_| ())?;
    required_sqlite_library_info_string(value, "version").map(|_| ())?;
    required_sqlite_library_info_string(value, "ffi_stage").map(|_| ())?;
    required_sqlite_library_info_array(value, "capabilities").map(|_| ())?;
    Ok(())
}

/// Return one required non-empty string field from dynamic SQLite library metadata.
/// 从动态 SQLite 库元数据返回一个必填非空字符串字段。
fn required_sqlite_library_info_string(value: &Value, field_name: &str) -> Result<Value, String> {
    value
        .get(field_name)
        .and_then(Value::as_str)
        .filter(|field| !field.trim().is_empty())
        .map(|field| Value::String(field.to_string()))
        .ok_or_else(|| {
            format!("SQLite dynamic library_info_json field `{field_name}` is missing or empty")
        })
}

/// Return one required array field from dynamic SQLite library metadata.
/// 从动态 SQLite 库元数据返回一个必填数组字段。
fn required_sqlite_library_info_array(value: &Value, field_name: &str) -> Result<Value, String> {
    value
        .get(field_name)
        .filter(|field| field.is_array())
        .cloned()
        .ok_or_else(|| {
            format!("SQLite dynamic library_info_json field `{field_name}` must be an array")
        })
}

/// Parse a tokenizer-mode text label into the FFI enum.
/// 将文本分词模式字符串解析为 FFI 枚举。
fn parse_tokenizer_mode(text: &str) -> Result<VldbSqliteFfiTokenizerMode, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "none" => Ok(VldbSqliteFfiTokenizerMode::None),
        "jieba" => Ok(VldbSqliteFfiTokenizerMode::Jieba),
        other => Err(format!("unsupported sqlite tokenizer mode: {}", other)),
    }
}

/// Convert the FFI tokenizer mode into a stable string label.
/// 将 FFI 分词模式转换成稳定字符串。
fn tokenizer_mode_name(mode: VldbSqliteFfiTokenizerMode) -> &'static str {
    match mode {
        VldbSqliteFfiTokenizerMode::None => "none",
        VldbSqliteFfiTokenizerMode::Jieba => "jieba",
    }
}

/// Convert the local tokenizer-mode enum into the controller client tokenizer-mode enum.
/// 将本地分词模式枚举转换为控制器客户端分词模式枚举。
fn map_controller_tokenizer_mode(
    mode: VldbSqliteFfiTokenizerMode,
) -> ControllerSqliteTokenizerMode {
    match mode {
        VldbSqliteFfiTokenizerMode::None => ControllerSqliteTokenizerMode::None,
        VldbSqliteFfiTokenizerMode::Jieba => ControllerSqliteTokenizerMode::Jieba,
    }
}

/// Convert the controller tokenizer mode into one stable display label.
/// 将控制器分词模式转换为稳定显示标签。
fn controller_tokenizer_mode_name(mode: ControllerSqliteTokenizerMode) -> &'static str {
    match mode {
        ControllerSqliteTokenizerMode::None => "none",
        ControllerSqliteTokenizerMode::Jieba => "jieba",
    }
}

/// Convert the tokenizer-mode integer returned by FFI into a stable string label.
/// 将 FFI 返回的分词模式数值转换成稳定字符串。
fn tokenizer_mode_name_from_u32(mode: u32) -> &'static str {
    match mode {
        1 => "jieba",
        _ => "none",
    }
}

/// Encode a boolean value as the `u8` representation used by the FFI.
/// 将布尔值编码为 FFI 所使用的 `u8`。
fn bool_to_u8(value: bool) -> u8 {
    if value { 1 } else { 0 }
}

/// Convert an FFI `u8` boolean into a Rust boolean.
/// 将 FFI `u8` 布尔值转换为 Rust 布尔值。
fn u8_to_bool(value: u8) -> bool {
    value != 0
}

/// Host-side SQLite parameter representation used as a stable bridge between Lua/JSON and the FFI ABI.
/// 宿主内部使用的 SQLite 参数值表示，负责在 Lua/JSON 与 FFI ABI 之间做稳定过渡。
enum HostSqliteParamValue {
    Null,
    Int64(i64),
    Float64(f64),
    String(String),
    Bytes(Vec<u8>),
    Bool(bool),
}

/// Convert one host-side SQLite parameter list into controller client typed values.
/// 把一组宿主侧 SQLite 参数列表转换为控制器客户端类型化值。
fn map_controller_sqlite_params(values: &[HostSqliteParamValue]) -> Vec<ControllerSqliteValue> {
    values
        .iter()
        .map(|value| match value {
            HostSqliteParamValue::Null => ControllerSqliteValue::Null,
            HostSqliteParamValue::Int64(value) => ControllerSqliteValue::Int64(*value),
            HostSqliteParamValue::Float64(value) => ControllerSqliteValue::Float64(*value),
            HostSqliteParamValue::String(value) => ControllerSqliteValue::String(value.clone()),
            HostSqliteParamValue::Bytes(value) => ControllerSqliteValue::Bytes(value.clone()),
            HostSqliteParamValue::Bool(value) => ControllerSqliteValue::Bool(*value),
        })
        .collect()
}

/// One owned FFI parameter array that keeps strings and byte buffers alive for the entire call.
/// 一组已拥有生命周期的 FFI 参数数组，确保字符串和字节缓冲在调用期间保持有效。
struct OwnedSqliteFfiValues {
    values: Vec<VldbSqliteFfiValue>,
    _strings: Vec<CString>,
    _bytes: Vec<Vec<u8>>,
}

impl OwnedSqliteFfiValues {
    /// Return the pointer to the first FFI parameter value.
    /// 返回 FFI 参数数组首指针。
    fn as_ptr(&self) -> *const VldbSqliteFfiValue {
        self.values.as_ptr()
    }

    /// Return the length of the FFI parameter array.
    /// 返回 FFI 参数数组长度。
    fn len_u64(&self) -> u64 {
        u64::try_from(self.values.len()).unwrap_or(u64::MAX)
    }
}

/// Owned two-dimensional parameter matrix used by batch SQL execution.
/// 批量 SQL 所使用的已拥有生命周期的二维参数矩阵。
struct OwnedSqliteFfiValueMatrix {
    _rows: Vec<OwnedSqliteFfiValues>,
    slices: Vec<VldbSqliteFfiValueSlice>,
}

impl OwnedSqliteFfiValueMatrix {
    /// Return the pointer to the first batch-parameter slice.
    /// 返回批量参数切片首指针。
    fn as_ptr(&self) -> *const VldbSqliteFfiValueSlice {
        self.slices.as_ptr()
    }

    /// Return the number of batch-parameter slices.
    /// 返回批量参数切片数量。
    fn len_u64(&self) -> u64 {
        u64::try_from(self.slices.len()).unwrap_or(u64::MAX)
    }
}

/// Convert host parameter values into an owned FFI parameter array.
/// 把宿主参数值数组转换成拥有生命周期的 FFI 参数数组。
fn build_owned_ffi_values(values: &[HostSqliteParamValue]) -> Result<OwnedSqliteFfiValues, String> {
    let mut ffi_values = Vec::with_capacity(values.len());
    let mut strings = Vec::new();
    let mut bytes = Vec::new();

    for value in values {
        match value {
            HostSqliteParamValue::Null => ffi_values.push(VldbSqliteFfiValue {
                kind: VldbSqliteFfiValueKind::Null,
                int64_value: 0,
                float64_value: 0.0,
                string_value: ptr::null(),
                bytes_value: VldbSqliteByteView::default(),
                bool_value: 0,
            }),
            HostSqliteParamValue::Int64(number) => ffi_values.push(VldbSqliteFfiValue {
                kind: VldbSqliteFfiValueKind::Int64,
                int64_value: *number,
                float64_value: 0.0,
                string_value: ptr::null(),
                bytes_value: VldbSqliteByteView::default(),
                bool_value: 0,
            }),
            HostSqliteParamValue::Float64(number) => ffi_values.push(VldbSqliteFfiValue {
                kind: VldbSqliteFfiValueKind::Float64,
                int64_value: 0,
                float64_value: *number,
                string_value: ptr::null(),
                bytes_value: VldbSqliteByteView::default(),
                bool_value: 0,
            }),
            HostSqliteParamValue::String(text) => {
                let c_text = to_cstring(text, "params[*].string")?;
                let ptr = c_text.as_ptr();
                strings.push(c_text);
                ffi_values.push(VldbSqliteFfiValue {
                    kind: VldbSqliteFfiValueKind::String,
                    int64_value: 0,
                    float64_value: 0.0,
                    string_value: ptr,
                    bytes_value: VldbSqliteByteView::default(),
                    bool_value: 0,
                });
            }
            HostSqliteParamValue::Bytes(blob) => {
                let owned = blob.clone();
                let view = if owned.is_empty() {
                    VldbSqliteByteView::default()
                } else {
                    VldbSqliteByteView {
                        data: owned.as_ptr(),
                        len: u64::try_from(owned.len()).unwrap_or(u64::MAX),
                    }
                };
                bytes.push(owned);
                ffi_values.push(VldbSqliteFfiValue {
                    kind: VldbSqliteFfiValueKind::Bytes,
                    int64_value: 0,
                    float64_value: 0.0,
                    string_value: ptr::null(),
                    bytes_value: view,
                    bool_value: 0,
                });
            }
            HostSqliteParamValue::Bool(flag) => ffi_values.push(VldbSqliteFfiValue {
                kind: VldbSqliteFfiValueKind::Bool,
                int64_value: 0,
                float64_value: 0.0,
                string_value: ptr::null(),
                bytes_value: VldbSqliteByteView::default(),
                bool_value: bool_to_u8(*flag),
            }),
        }
    }

    Ok(OwnedSqliteFfiValues {
        values: ffi_values,
        _strings: strings,
        _bytes: bytes,
    })
}

/// Convert a batch parameter matrix into owned FFI batch-parameter slices.
/// 把批量参数矩阵转换成拥有生命周期的 FFI 批量参数切片。
fn build_owned_ffi_value_matrix(
    rows: &[Vec<HostSqliteParamValue>],
) -> Result<OwnedSqliteFfiValueMatrix, String> {
    let owned_rows = rows
        .iter()
        .map(|row| build_owned_ffi_values(row))
        .collect::<Result<Vec<_>, _>>()?;
    let slices = owned_rows
        .iter()
        .map(|row| VldbSqliteFfiValueSlice {
            values: row.as_ptr(),
            len: row.len_u64(),
        })
        .collect::<Vec<_>>();
    Ok(OwnedSqliteFfiValueMatrix {
        _rows: owned_rows,
        slices,
    })
}

/// Convert a JSON/Lua scalar parameter into the host-side SQLite parameter representation.
/// 把 JSON/ Lua 标量参数转换为宿主内部 SQLite 参数值。
fn parse_scalar_sqlite_param(
    value: &Value,
    field_name: &str,
) -> Result<HostSqliteParamValue, String> {
    match value {
        Value::Null => Ok(HostSqliteParamValue::Null),
        Value::Bool(flag) => Ok(HostSqliteParamValue::Bool(*flag)),
        Value::Number(number) => {
            if let Some(int_value) = number.as_i64() {
                Ok(HostSqliteParamValue::Int64(int_value))
            } else if let Some(unsigned) = number.as_u64() {
                let converted = i64::try_from(unsigned).map_err(|_| {
                    format!(
                        "{} contains an unsigned integer larger than i64",
                        field_name
                    )
                })?;
                Ok(HostSqliteParamValue::Int64(converted))
            } else if let Some(float_value) = number.as_f64() {
                Ok(HostSqliteParamValue::Float64(float_value))
            } else {
                Err(format!(
                    "{} contains an unsupported numeric value",
                    field_name
                ))
            }
        }
        Value::String(text) => Ok(HostSqliteParamValue::String(text.clone())),
        _ => Err(format!("{} must contain only scalar values", field_name)),
    }
}

/// Convert a typed parameter object into the host-side SQLite parameter representation.
/// 把 typed 参数对象转换为宿主内部 SQLite 参数值。
fn parse_typed_sqlite_param(
    object: &serde_json::Map<String, Value>,
    field_name: &str,
) -> Result<HostSqliteParamValue, String> {
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{}.kind is required for typed parameters", field_name))?;
    match kind.trim().to_ascii_lowercase().as_str() {
        "null" => Ok(HostSqliteParamValue::Null),
        "bool" => object
            .get("value")
            .and_then(Value::as_bool)
            .map(HostSqliteParamValue::Bool)
            .ok_or_else(|| format!("{}.value must be a bool", field_name)),
        "int64" => object
            .get("value")
            .and_then(Value::as_i64)
            .map(HostSqliteParamValue::Int64)
            .ok_or_else(|| format!("{}.value must be an int64", field_name)),
        "float64" => object
            .get("value")
            .and_then(Value::as_f64)
            .map(HostSqliteParamValue::Float64)
            .ok_or_else(|| format!("{}.value must be a float64", field_name)),
        "string" => object
            .get("value")
            .and_then(Value::as_str)
            .map(|value| HostSqliteParamValue::String(value.to_string()))
            .ok_or_else(|| format!("{}.value must be a string", field_name)),
        "bytes" => {
            if let Some(base64_value) = object.get("base64").and_then(Value::as_str) {
                let decoded = BASE64_STANDARD
                    .decode(base64_value)
                    .map_err(|error| format!("{}.base64 is invalid: {}", field_name, error))?;
                return Ok(HostSqliteParamValue::Bytes(decoded));
            }
            let array = object
                .get("value")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!(
                        "{}.value must be a byte array or provide base64",
                        field_name
                    )
                })?;
            let mut bytes = Vec::with_capacity(array.len());
            for (index, item) in array.iter().enumerate() {
                let byte = item.as_u64().ok_or_else(|| {
                    format!(
                        "{}.value[{}] must be an unsigned integer",
                        field_name, index
                    )
                })?;
                let converted = u8::try_from(byte)
                    .map_err(|_| format!("{}.value[{}] exceeds u8", field_name, index))?;
                bytes.push(converted);
            }
            Ok(HostSqliteParamValue::Bytes(bytes))
        }
        other => Err(format!("{}.kind={} is unsupported", field_name, other)),
    }
}

/// Normalize a JSON parameter value into the host-side SQLite parameter representation.
/// 把 JSON 参数值统一转换为宿主内部 SQLite 参数值。
fn parse_sqlite_param(value: &Value, field_name: &str) -> Result<HostSqliteParamValue, String> {
    match value {
        Value::Object(object) if object.contains_key("kind") => {
            parse_typed_sqlite_param(object, field_name)
        }
        other => parse_scalar_sqlite_param(other, field_name),
    }
}

/// Parse the parameter list for a single SQL request from the unified input object.
/// 从统一输入对象中解析单条 SQL 的参数列表。
fn parse_single_sql_params(input: &Value) -> Result<Vec<HostSqliteParamValue>, String> {
    if input.get("params_json").is_some() {
        return Err("params_json is no longer supported; use params".to_string());
    }
    if let Some(params_value) = input.get("params") {
        let params_array = params_value
            .as_array()
            .ok_or_else(|| "params must be an array".to_string())?;
        return params_array
            .iter()
            .enumerate()
            .map(|(index, item)| parse_sqlite_param(item, &format!("params[{}]", index)))
            .collect();
    }
    Ok(Vec::new())
}

/// Parse the parameter matrix for batch SQL from the unified input object.
/// 从统一输入对象中解析批量 SQL 的参数矩阵。
fn parse_batch_sql_params(input: &Value) -> Result<Vec<Vec<HostSqliteParamValue>>, String> {
    let items = input
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "items must be an array of arrays".to_string())?;
    if items.is_empty() {
        return Err("items must not be empty".to_string());
    }
    items
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            let row_items = row
                .as_array()
                .ok_or_else(|| format!("items[{}] must be an array", row_index))?;
            row_items
                .iter()
                .enumerate()
                .map(|(col_index, item)| {
                    parse_sqlite_param(item, &format!("items[{}][{}]", row_index, col_index))
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests;

/// Ensure that a required string field exists in the JSON request.
/// 确保 JSON 请求中存在指定字符串字段。
fn require_string_field<'a>(input: &'a Value, field_name: &str) -> Result<&'a str, String> {
    input
        .get(field_name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing or empty field `{}`", field_name))
}

/// Ensure that a required JSON string field exists while preserving explicit empty text.
/// 确保必需 JSON 字符串字段存在，同时保留显式空文本。
///
/// The input parameter is the JSON object that owns the requested field.
/// input 参数是持有所请求字段的 JSON 对象。
///
/// The field_name parameter identifies the required field in diagnostics.
/// field_name 参数用于在诊断中标识必需字段。
///
/// Return the string value when the field is present and typed as a JSON string.
/// 字段存在且类型为 JSON 字符串时返回该字符串值。
fn require_present_string_field<'a>(input: &'a Value, field_name: &str) -> Result<&'a str, String> {
    input
        .get(field_name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or non-string field `{}`", field_name))
}

/// Require one dynamic-library string field that is mandatory in a returned SQLite record.
/// 要求动态库返回的单条 SQLite 记录中必须存在的字符串字段。
///
/// The value parameter is the optional string decoded from the dynamic-library pointer.
/// value 参数是从动态库指针解码得到的可选字符串。
///
/// The field_name parameter identifies the returned field in diagnostics.
/// field_name 参数用于在诊断中标识返回字段。
///
/// Return the string when present, or an explicit missing-field error.
/// 字符串存在时返回该字符串；否则返回显式字段缺失错误。
fn require_sqlite_dynamic_string(
    value: Option<String>,
    field_name: &str,
) -> Result<String, String> {
    value.ok_or_else(|| format!("SQLite dynamic result field `{field_name}` is missing"))
}

/// Convert a Rust string into a C string while uniformly validating interior NUL bytes.
/// 将 Rust 字符串转换为 C 字符串，统一校验 NUL 字节。
fn to_cstring(text: &str, field_name: &str) -> Result<CString, String> {
    CString::new(text).map_err(|_| format!("field `{}` contains interior NUL bytes", field_name))
}

/// Check the FFI return status code and convert failures into a host-level error string.
/// 检查 FFI 返回状态码，并在失败时转换成宿主级错误文本。
fn ensure_status(api: &LoadedSqliteApi, status: i32, operation: &str) -> Result<(), String> {
    if status == VldbSqliteStatusCode::Success as i32 {
        return Ok(());
    }
    let error = api.take_last_error_message();
    Err(format!("{} failed: {}", operation, error))
}
