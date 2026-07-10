use crate::runtime::path::render_host_visible_path;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Database access mode used by one host-facing runtime backend.
/// 单个宿主侧运行时后端所使用的数据库访问模式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LuaRuntimeDatabaseProviderMode {
    /// The library loads and calls the local dynamic-library backend directly.
    /// 由库直接加载并调用本地动态库后端。
    #[default]
    DynamicLibrary,
    /// The library forwards database operations into one host-registered callback bridge.
    /// 由库把数据库操作转发给宿主已注册的回调桥接。
    HostCallback,
    /// The library forwards database operations into one external space controller.
    /// 由库把数据库操作转发给外部空间控制器。
    SpaceController,
}

/// Callback transport mode used when the database provider mode is `host_callback`.
/// 当数据库 provider 模式为 `host_callback` 时所使用的回调传输模式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LuaRuntimeDatabaseCallbackMode {
    /// The library uses the structured standard callback ABI.
    /// 由库使用结构化标准回调 ABI。
    #[default]
    Standard,
    /// The library uses the JSON callback ABI.
    /// 由库使用 JSON 回调 ABI。
    Json,
}

/// Return the stable display label for one database callback transport mode.
/// 返回数据库回调传输模式的稳定显示标签。
///
/// The mode parameter is the callback transport selected by host options.
/// mode 参数是宿主选项选择的回调传输模式。
///
/// Return a snake-case label used in diagnostics and host-facing error messages.
/// 返回用于诊断与宿主侧错误消息的 snake_case 标签。
pub(crate) fn database_callback_mode_name(mode: LuaRuntimeDatabaseCallbackMode) -> &'static str {
    match mode {
        LuaRuntimeDatabaseCallbackMode::Standard => "standard",
        LuaRuntimeDatabaseCallbackMode::Json => "json",
    }
}

/// Require one host-callback transport to have a registered provider callback before a host starts.
/// 要求宿主启动前指定的 host-callback 传输已注册 provider 回调。
///
/// The provider_label parameter is the stable provider name shown in diagnostics.
/// provider_label 参数是诊断信息中显示的稳定 provider 名称。
///
/// The callback_mode parameter identifies which callback transport is required.
/// callback_mode 参数标识当前需要的回调传输模式。
///
/// The has_callback parameter is the already-checked registry state for that exact provider and transport.
/// has_callback 参数是该 provider 与传输模式已经检查过的准确注册表状态。
///
/// Return Ok when the callback exists, or a host-facing startup error when it is missing.
/// 如果回调存在则返回 Ok；如果缺失则返回面向宿主的启动错误。
pub(crate) fn require_database_provider_callback_registration(
    provider_label: &str,
    callback_mode: LuaRuntimeDatabaseCallbackMode,
    has_callback: bool,
) -> Result<(), String> {
    if has_callback {
        return Ok(());
    }

    Err(format!(
        "{} host-callback mode is enabled but no {} callback is registered",
        provider_label,
        database_callback_mode_name(callback_mode)
    ))
}

/// Logical database kind resolved for one provider request.
/// 为单次 provider 请求解析出的逻辑数据库类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDatabaseKind {
    /// SQLite / FTS / BM25 backend operations.
    /// SQLite / FTS / BM25 后端操作。
    Sqlite,
    /// LanceDB vector backend operations.
    /// LanceDB 向量后端操作。
    LanceDb,
}

impl RuntimeDatabaseKind {
    /// Return the stable sidecar directory name owned by this database kind.
    /// 返回当前数据库类型拥有的稳定 sidecar 目录名称。
    ///
    /// Return the provider-specific directory segment used below the shared database root.
    /// 返回共享数据库根目录下使用的 provider 专属目录片段。
    fn sidecar_directory_name(self) -> &'static str {
        match self {
            RuntimeDatabaseKind::Sqlite => "sqlite",
            RuntimeDatabaseKind::LanceDb => "lancedb",
        }
    }

    /// Build the default database path for one provider storage directory and skill id.
    /// 基于 provider 存储目录与 skill 标识构造默认数据库路径。
    ///
    /// The provider_storage_dir parameter is the concrete directory dedicated to the skill and provider.
    /// provider_storage_dir 参数是专属于该 skill 与 provider 的具体目录。
    ///
    /// The skill_name parameter is the stable skill identifier used when a provider needs a file name.
    /// skill_name 参数是 provider 需要文件名时使用的稳定 skill 标识。
    ///
    /// Return the default database path used by diagnostics, callbacks, and embedded providers.
    /// 返回诊断、回调与内嵌 provider 共同使用的默认数据库路径。
    fn default_database_path(self, provider_storage_dir: &Path, skill_name: &str) -> PathBuf {
        match self {
            RuntimeDatabaseKind::Sqlite => {
                provider_storage_dir.join(format!("{}.sqlite3", skill_name))
            }
            RuntimeDatabaseKind::LanceDb => provider_storage_dir.to_path_buf(),
        }
    }
}

/// Complete construction input for one skill-scoped database binding context.
/// 单个 skill 级数据库绑定上下文的完整构造输入。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDatabaseBindingContextSpec {
    /// Stable host-provided space label such as ROOT, PROJECT, or USER.
    /// 由宿主提供的稳定空间标签，例如 ROOT、PROJECT 或 USER。
    pub space_label: String,
    /// Stable skill identifier currently owning the database binding.
    /// 当前拥有数据库绑定的稳定技能标识符。
    pub skill_id: String,
    /// Physical skill root label that resolved the effective skill instance.
    /// 解析出生效技能实例时命中的物理技能根标签。
    pub root_name: String,
    /// Runtime database sidecar root used as the host/controller space root.
    /// 作为宿主或控制器空间根使用的运行时数据库 sidecar 根目录。
    pub space_root: String,
    /// Physical skill directory path.
    /// 物理技能目录路径。
    pub skill_dir: String,
    /// Physical skill directory basename.
    /// 物理技能目录名称。
    pub skill_dir_name: String,
    /// Logical database kind requested by the current provider binding.
    /// 当前 provider 绑定请求的逻辑数据库类型。
    pub database_kind: RuntimeDatabaseKind,
    /// Default embedded database path resolved by the library for diagnostics and fallback.
    /// 由库按内嵌规则解析出的默认数据库路径，用于诊断和回退。
    pub default_database_path: String,
}

/// Resolved filesystem and context data for one skill-scoped database provider binding.
/// 单个 skill 级数据库 provider 绑定已经解析完成的文件系统与上下文数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDatabaseBindingPlan {
    /// Physical skill directory basename.
    /// 物理 skill 目录名称。
    pub(crate) skill_dir_name: String,
    /// Provider-specific storage directory dedicated to this skill.
    /// 专属于当前 skill 的 provider 存储目录。
    pub(crate) provider_storage_dir: PathBuf,
    /// Default database path exposed to diagnostics, callbacks, and embedded providers.
    /// 暴露给诊断、回调与内嵌 provider 的默认数据库路径。
    pub(crate) default_database_path: String,
    /// Stable host-facing binding context derived from the same resolved paths.
    /// 基于同一组已解析路径派生出的稳定宿主侧绑定上下文。
    pub(crate) context: RuntimeDatabaseBindingContext,
}

/// Stable host-facing binding context for one skill-scoped database backend.
/// 面向宿主的稳定 skill 级数据库后端绑定上下文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDatabaseBindingContext {
    /// Stable root label supplied by the host such as ROOT, PROJECT, or USER.
    /// 由宿主提供的稳定根标签，例如 ROOT、PROJECT 或 USER。
    pub space_label: String,
    /// Stable skill identifier currently owning the database binding.
    /// 当前拥有数据库绑定的稳定技能标识符。
    pub skill_id: String,
    /// Stable binding tag composed from the space label and skill id.
    /// 由空间标签与技能标识符组合得到的稳定绑定标签。
    pub binding_tag: String,
    /// Physical skill root label currently resolving the effective skill instance.
    /// 当前解析出生效技能实例时所命中的物理技能根标签。
    pub root_name: String,
    /// Runtime database sidecar root used as the host/controller space root.
    /// 作为宿主或控制器空间根使用的运行时数据库 sidecar 根目录。
    pub space_root: String,
    /// Physical skill directory path.
    /// 物理技能目录路径。
    pub skill_dir: String,
    /// Physical skill directory basename.
    /// 物理技能目录名称。
    pub skill_dir_name: String,
    /// Logical database kind requested by the current provider binding.
    /// 当前 provider 绑定请求的逻辑数据库类型。
    pub database_kind: RuntimeDatabaseKind,
    /// Default embedded database path resolved by the library for compatibility and diagnostics.
    /// 由库按内嵌规则解析出的默认数据库路径，用于兼容和诊断。
    pub default_database_path: String,
}

impl RuntimeDatabaseBindingContext {
    /// Build one stable binding context from a complete runtime database binding specification.
    /// 基于完整的运行时数据库绑定规格构造稳定绑定上下文。
    pub fn new(spec: RuntimeDatabaseBindingContextSpec) -> Self {
        let RuntimeDatabaseBindingContextSpec {
            space_label,
            skill_id,
            root_name,
            space_root,
            skill_dir,
            skill_dir_name,
            database_kind,
            default_database_path,
        } = spec;
        Self {
            binding_tag: format!("{}-{}", space_label, skill_id),
            space_label,
            skill_id,
            root_name,
            space_root,
            skill_dir,
            skill_dir_name,
            database_kind,
            default_database_path,
        }
    }
}

/// Build the complete binding plan for one skill-scoped database provider.
/// 为单个 skill 级数据库 provider 构造完整绑定计划。
///
/// The root_name parameter is the stable host root label that resolved the skill.
/// root_name 参数是解析出当前 skill 的稳定宿主根标签。
///
/// The skill_name parameter is the stable skill identifier being bound.
/// skill_name 参数是当前要绑定的稳定 skill 标识。
///
/// The skill_dir parameter is the physical directory of the effective skill instance.
/// skill_dir 参数是当前生效 skill 实例的物理目录。
///
/// The database_dir_name parameter is the host-configured sibling database directory name.
/// database_dir_name 参数是宿主配置的同级数据库目录名称。
///
/// The database_kind parameter selects the provider-specific storage and default path rules.
/// database_kind 参数选择 provider 专属存储与默认路径规则。
///
/// Return the resolved provider storage path, default database path, and binding context.
/// 返回解析后的 provider 存储路径、默认数据库路径与绑定上下文。
pub(crate) fn build_runtime_database_binding_plan(
    root_name: &str,
    skill_name: &str,
    skill_dir: &Path,
    database_dir_name: &str,
    database_kind: RuntimeDatabaseKind,
) -> Result<RuntimeDatabaseBindingPlan, String> {
    let skill_dir_name = skill_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "invalid skill directory name for {}: {}",
                skill_name,
                render_host_visible_path(skill_dir)
            )
        })?
        .to_string();
    let skills_root = skill_dir.parent().ok_or_else(|| {
        format!(
            "invalid skill root for {}: {}",
            skill_name,
            render_host_visible_path(skill_dir)
        )
    })?;
    let sidecar_root = skills_root
        .parent()
        .unwrap_or(skills_root)
        .join(database_dir_name);
    let provider_storage_dir = sidecar_root
        .join(database_kind.sidecar_directory_name())
        .join(skill_name);
    let default_database_path = render_host_visible_path(
        &database_kind.default_database_path(&provider_storage_dir, skill_name),
    );
    let context = RuntimeDatabaseBindingContext::new(RuntimeDatabaseBindingContextSpec {
        space_label: root_name.to_string(),
        skill_id: skill_name.to_string(),
        root_name: root_name.to_string(),
        space_root: render_host_visible_path(&sidecar_root),
        skill_dir: render_host_visible_path(skill_dir),
        skill_dir_name: skill_dir_name.clone(),
        database_kind,
        default_database_path: default_database_path.clone(),
    });

    Ok(RuntimeDatabaseBindingPlan {
        skill_dir_name,
        provider_storage_dir,
        default_database_path,
        context,
    })
}

/// Structured SQLite provider action routed through one host bridge.
/// 通过宿主桥接路由的结构化 SQLite provider 动作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSqliteProviderAction {
    /// Execute one SQL script or one single SQL statement.
    /// 执行一个 SQL 脚本或单条 SQL 语句。
    ExecuteScript,
    /// Execute one batch SQL write request.
    /// 执行一次批量 SQL 写入请求。
    ExecuteBatch,
    /// Execute one JSON row-set query.
    /// 执行一次 JSON 行集查询。
    QueryJson,
    /// Create one query-stream handle.
    /// 创建一个查询流句柄。
    QueryStream,
    /// Wait for query-stream metrics.
    /// 等待查询流统计信息。
    QueryStreamWaitMetrics,
    /// Read one query-stream chunk.
    /// 读取一个查询流分块。
    QueryStreamChunk,
    /// Close one query-stream handle.
    /// 关闭一个查询流句柄。
    QueryStreamClose,
    /// Execute text tokenization.
    /// 执行文本分词。
    TokenizeText,
    /// Upsert one custom dictionary word.
    /// 写入或更新一个自定义词。
    UpsertCustomWord,
    /// Remove one custom dictionary word.
    /// 删除一个自定义词。
    RemoveCustomWord,
    /// List current custom dictionary words.
    /// 列出当前自定义词。
    ListCustomWords,
    /// Ensure one FTS index exists.
    /// 确保一个 FTS 索引存在。
    EnsureFtsIndex,
    /// Rebuild one FTS index.
    /// 重建一个 FTS 索引。
    RebuildFtsIndex,
    /// Upsert one FTS document.
    /// 写入或更新一条 FTS 文档。
    UpsertFtsDocument,
    /// Delete one FTS document.
    /// 删除一条 FTS 文档。
    DeleteFtsDocument,
    /// Execute one standardized FTS/BM25 search.
    /// 执行一次标准化 FTS/BM25 检索。
    SearchFts,
}

/// Structured LanceDB provider action routed through one host bridge.
/// 通过宿主桥接路由的结构化 LanceDB provider 动作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLanceDbProviderAction {
    /// Create one table.
    /// 创建一张表。
    CreateTable,
    /// Upsert vectors into one table.
    /// 向一张表写入向量。
    VectorUpsert,
    /// Search vectors from one table.
    /// 从一张表检索向量。
    VectorSearch,
    /// Delete rows from one table.
    /// 从一张表删除行。
    Delete,
    /// Drop one table.
    /// 删除一张表。
    DropTable,
}

/// Structured SQLite provider request delivered to one host bridge.
/// 传递给宿主桥接的结构化 SQLite provider 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeSqliteProviderRequest {
    /// Requested SQLite provider action.
    /// 请求的 SQLite provider 动作。
    pub action: RuntimeSqliteProviderAction,
    /// Stable binding context of the current skill-scoped database.
    /// 当前 skill 级数据库的稳定绑定上下文。
    pub binding: RuntimeDatabaseBindingContext,
    /// Action-specific JSON input payload.
    /// 动作对应的 JSON 输入载荷。
    pub input: Value,
}

/// Structured LanceDB provider request delivered to one host bridge.
/// 传递给宿主桥接的结构化 LanceDB provider 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeLanceDbProviderRequest {
    /// Requested LanceDB provider action.
    /// 请求的 LanceDB provider 动作。
    pub action: RuntimeLanceDbProviderAction,
    /// Stable binding context of the current skill-scoped database.
    /// 当前 skill 级数据库的稳定绑定上下文。
    pub binding: RuntimeDatabaseBindingContext,
    /// Action-specific JSON input payload.
    /// 动作对应的 JSON 输入载荷。
    pub input: Value,
}

/// Standard host callback used for one structured SQLite provider request.
/// 用于处理结构化 SQLite provider 请求的标准宿主回调。
pub type RuntimeSqliteProviderCallback =
    Arc<dyn Fn(&RuntimeSqliteProviderRequest) -> Result<Value, String> + Send + Sync>;

/// Standard host callback used for one structured LanceDB provider request.
/// 用于处理结构化 LanceDB provider 请求的标准宿主回调。
pub type RuntimeLanceDbProviderCallback = Arc<
    dyn Fn(&RuntimeLanceDbProviderRequest) -> Result<RuntimeLanceDbProviderResult, String>
        + Send
        + Sync,
>;

/// JSON host callback used for one SQLite provider request.
/// 用于处理 SQLite provider 请求的 JSON 宿主回调。
pub type RuntimeSqliteProviderJsonCallback =
    Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// JSON host callback used for one LanceDB provider request.
/// 用于处理 LanceDB provider 请求的 JSON 宿主回调。
pub type RuntimeLanceDbProviderJsonCallback =
    Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// One engine-scoped snapshot of all database provider callbacks visible at creation time.
/// 一个在引擎创建时快照出的数据库 provider 回调集合，作用域限定为单个引擎实例。
#[derive(Clone, Default)]
pub(crate) struct RuntimeDatabaseProviderCallbacks {
    /// Structured SQLite callback captured for the current engine snapshot.
    /// 当前引擎快照捕获到的结构化 SQLite 回调。
    sqlite_standard: Option<RuntimeSqliteProviderCallback>,
    /// Structured LanceDB callback captured for the current engine snapshot.
    /// 当前引擎快照捕获到的结构化 LanceDB 回调。
    lancedb_standard: Option<RuntimeLanceDbProviderCallback>,
    /// JSON SQLite callback captured for the current engine snapshot.
    /// 当前引擎快照捕获到的 JSON SQLite 回调。
    sqlite_json: Option<RuntimeSqliteProviderJsonCallback>,
    /// JSON LanceDB callback captured for the current engine snapshot.
    /// 当前引擎快照捕获到的 JSON LanceDB 回调。
    lancedb_json: Option<RuntimeLanceDbProviderJsonCallback>,
}

impl RuntimeDatabaseProviderCallbacks {
    /// Snapshot the current process-wide callback defaults into one engine-private registry.
    /// 把当前进程级默认回调快照为一个引擎私有注册表。
    pub(crate) fn capture_process_defaults() -> Self {
        Self {
            sqlite_standard: clone_database_provider_callback_registry_value(
                sqlite_provider_callback_registry(),
            ),
            lancedb_standard: clone_database_provider_callback_registry_value(
                lancedb_provider_callback_registry(),
            ),
            sqlite_json: clone_database_provider_callback_registry_value(
                sqlite_provider_json_callback_registry(),
            ),
            lancedb_json: clone_database_provider_callback_registry_value(
                lancedb_provider_json_callback_registry(),
            ),
        }
    }

    /// Return whether the snapshot contains one SQLite callback for the requested transport mode.
    /// 返回当前快照是否包含指定传输模式的 SQLite 回调。
    pub(crate) fn has_sqlite_provider_callback_for_mode(
        &self,
        callback_mode: LuaRuntimeDatabaseCallbackMode,
    ) -> bool {
        match callback_mode {
            LuaRuntimeDatabaseCallbackMode::Standard => self.sqlite_standard.is_some(),
            LuaRuntimeDatabaseCallbackMode::Json => self.sqlite_json.is_some(),
        }
    }

    /// Return whether the snapshot contains one LanceDB callback for the requested transport mode.
    /// 返回当前快照是否包含指定传输模式的 LanceDB 回调。
    pub(crate) fn has_lancedb_provider_callback_for_mode(
        &self,
        callback_mode: LuaRuntimeDatabaseCallbackMode,
    ) -> bool {
        match callback_mode {
            LuaRuntimeDatabaseCallbackMode::Standard => self.lancedb_standard.is_some(),
            LuaRuntimeDatabaseCallbackMode::Json => self.lancedb_json.is_some(),
        }
    }

    /// Dispatch one SQLite provider request through the callbacks captured by this snapshot.
    /// 通过当前快照捕获的回调分发一次 SQLite provider 请求。
    pub(crate) fn dispatch_sqlite_provider_request(
        &self,
        request: &RuntimeSqliteProviderRequest,
        callback_mode: LuaRuntimeDatabaseCallbackMode,
    ) -> Result<Value, String> {
        match callback_mode {
            LuaRuntimeDatabaseCallbackMode::Standard => {
                let callback = self.sqlite_standard.clone().ok_or_else(|| {
                    "SQLite host-callback mode requires one registered standard callback"
                        .to_string()
                })?;
                callback(request)
            }
            LuaRuntimeDatabaseCallbackMode::Json => {
                let callback = self.sqlite_json.clone().ok_or_else(|| {
                    "SQLite host-callback JSON mode requires one registered JSON callback"
                        .to_string()
                })?;
                let request_json = serde_json::to_string(request).map_err(|error| {
                    format!("failed to encode sqlite provider request: {}", error)
                })?;
                let response_json = callback(&request_json)?;
                serde_json::from_str::<Value>(&response_json).map_err(|error| {
                    format!("failed to parse sqlite provider response json: {}", error)
                })
            }
        }
    }

    /// Dispatch one LanceDB provider request through the callbacks captured by this snapshot.
    /// 通过当前快照捕获的回调分发一次 LanceDB provider 请求。
    pub(crate) fn dispatch_lancedb_provider_request(
        &self,
        request: &RuntimeLanceDbProviderRequest,
        callback_mode: LuaRuntimeDatabaseCallbackMode,
    ) -> Result<RuntimeLanceDbProviderResult, String> {
        match callback_mode {
            LuaRuntimeDatabaseCallbackMode::Standard => {
                let callback = self.lancedb_standard.clone().ok_or_else(|| {
                    "LanceDB host-callback mode requires one registered standard callback"
                        .to_string()
                })?;
                callback(request)
            }
            LuaRuntimeDatabaseCallbackMode::Json => {
                let callback = self.lancedb_json.clone().ok_or_else(|| {
                    "LanceDB host-callback JSON mode requires one registered JSON callback"
                        .to_string()
                })?;
                let request_json = serde_json::to_string(request).map_err(|error| {
                    format!("failed to encode lancedb provider request: {}", error)
                })?;
                let response_json = callback(&request_json)?;
                let value: Value = serde_json::from_str(&response_json).map_err(|error| {
                    format!("failed to parse lancedb provider response json: {}", error)
                })?;
                let meta = value
                    .get("meta")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                let bytes = value
                    .get("data_base64")
                    .and_then(Value::as_str)
                    .map(|text| {
                        BASE64_STANDARD.decode(text.as_bytes()).map_err(|error| {
                            format!("failed to decode lancedb provider data_base64: {}", error)
                        })
                    })
                    .transpose()?
                    .unwrap_or_default();
                Ok(RuntimeLanceDbProviderResult::binary(meta, bytes))
            }
        }
    }
}

/// Structured LanceDB provider result returned by the standard host callback.
/// 标准宿主回调返回的结构化 LanceDB provider 结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeLanceDbProviderResult {
    /// Response metadata JSON.
    /// 响应元信息 JSON。
    pub meta: Value,
    /// Optional raw payload bytes such as vector search result data.
    /// 可选原始载荷字节，例如向量检索结果数据。
    pub bytes: Vec<u8>,
}

impl RuntimeLanceDbProviderResult {
    /// Build one result carrying only metadata JSON.
    /// 构造一个仅携带元信息 JSON 的结果。
    pub fn json(meta: Value) -> Self {
        Self {
            meta,
            bytes: Vec::new(),
        }
    }

    /// Build one result carrying metadata JSON plus raw bytes.
    /// 构造一个携带元信息 JSON 和原始字节的结果。
    pub fn binary(meta: Value, bytes: Vec<u8>) -> Self {
        Self { meta, bytes }
    }
}

/// Install or clear the process-wide standard SQLite provider callback.
/// 安装或清理进程级标准 SQLite provider 回调。
pub fn set_sqlite_provider_callback(callback: Option<RuntimeSqliteProviderCallback>) {
    set_database_provider_callback_registry_value(sqlite_provider_callback_registry(), callback);
}

/// Install or clear the process-wide standard LanceDB provider callback.
/// 安装或清理进程级标准 LanceDB provider 回调。
pub fn set_lancedb_provider_callback(callback: Option<RuntimeLanceDbProviderCallback>) {
    set_database_provider_callback_registry_value(lancedb_provider_callback_registry(), callback);
}

/// Install or clear the process-wide JSON SQLite provider callback.
/// 安装或清理进程级 JSON SQLite provider 回调。
pub fn set_sqlite_provider_json_callback(callback: Option<RuntimeSqliteProviderJsonCallback>) {
    set_database_provider_callback_registry_value(
        sqlite_provider_json_callback_registry(),
        callback,
    );
}

/// Install or clear the process-wide JSON LanceDB provider callback.
/// 安装或清理进程级 JSON LanceDB provider 回调。
pub fn set_lancedb_provider_json_callback(callback: Option<RuntimeLanceDbProviderJsonCallback>) {
    set_database_provider_callback_registry_value(
        lancedb_provider_json_callback_registry(),
        callback,
    );
}

/// Acquire one process-wide database provider callback registry lock and return its guard, recovering poisoned state.
/// 获取并返回单个进程级数据库 provider 回调注册表锁；如果状态已 poison，则恢复继续使用。
fn lock_database_provider_callback_registry<T>(
    registry: &'static Mutex<Option<T>>,
) -> MutexGuard<'static, Option<T>> {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Store one optional callback in a process-wide database provider registry.
/// 将一个可选回调写入进程级数据库 provider 注册表。
///
/// The registry parameter is the exact callback slot owned by one provider transport.
/// registry 参数是某个 provider 传输模式拥有的准确回调槽。
///
/// The callback parameter replaces the complete registered value, including clearing it with None.
/// callback 参数会替换完整注册值，也可以通过 None 清空该值。
///
fn set_database_provider_callback_registry_value<T>(
    registry: &'static Mutex<Option<T>>,
    callback: Option<T>,
) {
    // Hold the registry lock only while replacing the process default callback.
    // 仅在替换进程级默认回调期间持有注册表锁。
    let mut guard = lock_database_provider_callback_registry(registry);
    *guard = callback;
}

/// Clone one optional callback from a process-wide database provider registry.
/// 从进程级数据库 provider 注册表克隆一个可选回调。
///
/// The registry parameter is the exact callback slot owned by one provider transport.
/// registry 参数是某个 provider 传输模式拥有的准确回调槽。
///
/// Return the cloned callback option visible at capture time.
/// 返回捕获时可见的克隆后回调选项。
fn clone_database_provider_callback_registry_value<T: Clone>(
    registry: &'static Mutex<Option<T>>,
) -> Option<T> {
    // Hold the registry lock only long enough to clone the Arc-backed callback value.
    // 仅在克隆 Arc 封装的回调值所需的短时间内持有注册表锁。
    lock_database_provider_callback_registry(registry).clone()
}

/// Return the process-wide standard SQLite provider callback storage.
/// 返回进程级标准 SQLite provider 回调存储。
fn sqlite_provider_callback_registry() -> &'static Mutex<Option<RuntimeSqliteProviderCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeSqliteProviderCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide standard LanceDB provider callback storage.
/// 返回进程级标准 LanceDB provider 回调存储。
fn lancedb_provider_callback_registry() -> &'static Mutex<Option<RuntimeLanceDbProviderCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeLanceDbProviderCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide JSON SQLite provider callback storage.
/// 返回进程级 JSON SQLite provider 回调存储。
fn sqlite_provider_json_callback_registry()
-> &'static Mutex<Option<RuntimeSqliteProviderJsonCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeSqliteProviderJsonCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide JSON LanceDB provider callback storage.
/// 返回进程级 JSON LanceDB provider 回调存储。
fn lancedb_provider_json_callback_registry()
-> &'static Mutex<Option<RuntimeLanceDbProviderJsonCallback>> {
    static REGISTRY: OnceLock<Mutex<Option<RuntimeLanceDbProviderJsonCallback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::panic::{self, AssertUnwindSafe};
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    /// Return a process-wide test lock so callback-registry tests do not race in parallel.
    /// 返回一个进程级测试锁，避免回调注册表测试并发互相干扰。
    fn database_callback_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Restore the process-wide callback defaults captured before one test mutates them.
    /// 恢复某个测试修改前捕获到的进程级默认回调集合。
    struct ProcessCallbackRestoreGuard {
        snapshot: RuntimeDatabaseProviderCallbacks,
    }

    impl ProcessCallbackRestoreGuard {
        /// Capture the current process-wide callback defaults so they can be restored on drop.
        /// 捕获当前进程级默认回调，以便在释放时恢复。
        fn capture() -> Self {
            Self {
                snapshot: RuntimeDatabaseProviderCallbacks::capture_process_defaults(),
            }
        }
    }

    impl Drop for ProcessCallbackRestoreGuard {
        fn drop(&mut self) {
            set_sqlite_provider_callback(self.snapshot.sqlite_standard.clone());
            set_lancedb_provider_callback(self.snapshot.lancedb_standard.clone());
            set_sqlite_provider_json_callback(self.snapshot.sqlite_json.clone());
            set_lancedb_provider_json_callback(self.snapshot.lancedb_json.clone());
        }
    }

    /// Build one stable binding context used by snapshot-isolation tests.
    /// 构造供快照隔离测试使用的稳定绑定上下文。
    fn sample_binding_context(database_kind: RuntimeDatabaseKind) -> RuntimeDatabaseBindingContext {
        RuntimeDatabaseBindingContext::new(RuntimeDatabaseBindingContextSpec {
            space_label: "ROOT".to_string(),
            skill_id: "test-skill".to_string(),
            root_name: "ROOT".to_string(),
            space_root: "D:/runtime-test-root/__database".to_string(),
            skill_dir: "D:/runtime-test-root/skills/test-skill".to_string(),
            skill_dir_name: "test-skill".to_string(),
            database_kind,
            default_database_path: "D:/runtime-test-root/__database/default.db".to_string(),
        })
    }

    /// Verify that the shared binding planner resolves provider-specific storage paths.
    /// 验证共享绑定计划器会解析 provider 专属存储路径。
    #[test]
    fn database_binding_plan_resolves_provider_paths() {
        let skill_dir = Path::new("D:/runtime-test-root/skills/demo-skill");
        let runtime_root = skill_dir
            .parent()
            .and_then(Path::parent)
            .expect("runtime root path");
        let database_root = runtime_root.join("databases");

        let sqlite_plan = build_runtime_database_binding_plan(
            "ROOT",
            "demo-skill",
            skill_dir,
            "databases",
            RuntimeDatabaseKind::Sqlite,
        )
        .expect("sqlite binding plan");
        let sqlite_storage_dir = database_root.join("sqlite").join("demo-skill");
        let sqlite_database_path =
            render_host_visible_path(&sqlite_storage_dir.join("demo-skill.sqlite3"));
        assert_eq!(sqlite_plan.skill_dir_name, "demo-skill");
        assert_eq!(sqlite_plan.provider_storage_dir, sqlite_storage_dir);
        assert_eq!(sqlite_plan.default_database_path, sqlite_database_path);
        assert_eq!(
            sqlite_plan.context.space_root,
            render_host_visible_path(&database_root)
        );
        assert_eq!(
            sqlite_plan.context.default_database_path,
            sqlite_plan.default_database_path
        );
        assert_eq!(
            sqlite_plan.context.database_kind,
            RuntimeDatabaseKind::Sqlite
        );

        let lancedb_plan = build_runtime_database_binding_plan(
            "ROOT",
            "demo-skill",
            skill_dir,
            "databases",
            RuntimeDatabaseKind::LanceDb,
        )
        .expect("lancedb binding plan");
        let lancedb_storage_dir = database_root.join("lancedb").join("demo-skill");
        assert_eq!(lancedb_plan.skill_dir_name, "demo-skill");
        assert_eq!(lancedb_plan.provider_storage_dir, lancedb_storage_dir);
        assert_eq!(
            lancedb_plan.default_database_path,
            render_host_visible_path(&lancedb_plan.provider_storage_dir)
        );
        assert_eq!(
            lancedb_plan.context.default_database_path,
            lancedb_plan.default_database_path
        );
        assert_eq!(
            lancedb_plan.context.database_kind,
            RuntimeDatabaseKind::LanceDb
        );
    }

    /// Verify invalid skill-directory errors render paths through the host-visible formatter.
    /// 验证非法 skill 目录错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn database_binding_plan_invalid_skill_dir_error_uses_host_visible_path() {
        // Invalid empty skill directory path used to trigger directory-name validation.
        // 用于触发目录名校验失败的非法空 skill 目录路径。
        let skill_dir = Path::new("");
        // Error returned by the real shared database binding planner.
        // 真实共享数据库绑定计划器返回的错误。
        let error = build_runtime_database_binding_plan(
            "ROOT",
            "demo-skill",
            skill_dir,
            "databases",
            RuntimeDatabaseKind::Sqlite,
        )
        .expect_err("empty skill directory should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "invalid skill directory name for demo-skill: {}",
            render_host_visible_path(skill_dir)
        );

        assert_eq!(error, expected_prefix);
    }

    /// Verify that provider host startup errors name the exact missing callback transport.
    /// 验证 provider 宿主启动错误会指出准确缺失的回调传输模式。
    #[test]
    fn callback_registration_requirement_reports_transport_mode() {
        assert!(
            require_database_provider_callback_registration(
                "SQLite",
                LuaRuntimeDatabaseCallbackMode::Standard,
                true,
            )
            .is_ok()
        );

        assert_eq!(
            require_database_provider_callback_registration(
                "SQLite",
                LuaRuntimeDatabaseCallbackMode::Standard,
                false,
            )
            .expect_err("missing standard callback should fail"),
            "SQLite host-callback mode is enabled but no standard callback is registered"
        );
        assert_eq!(
            require_database_provider_callback_registration(
                "LanceDB",
                LuaRuntimeDatabaseCallbackMode::Json,
                false,
            )
            .expect_err("missing JSON callback should fail"),
            "LanceDB host-callback mode is enabled but no json callback is registered"
        );
    }

    /// Verify database provider callback registries remain writable and capturable after lock poisoning.
    /// 验证数据库 provider 回调注册表锁 poison 后仍可写入并被快照捕获。
    #[test]
    fn database_provider_callback_registry_recovers_after_poisoned_lock() {
        let _serial_guard = database_callback_test_lock()
            .lock()
            .expect("lock callback test guard");
        let _restore_guard = ProcessCallbackRestoreGuard::capture();
        set_sqlite_provider_callback(None);

        // Captured panic result from a registry writer that poisons the SQLite callback lock.
        // SQLite 回调注册表写入者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the process-wide SQLite provider callback registry.
            // 仅用于制造进程级 SQLite provider 回调注册表 poison 的保护对象。
            let _registry_guard = sqlite_provider_callback_registry()
                .lock()
                .expect("initial sqlite provider callback registry lock");
            panic!("poison sqlite provider callback registry for recovery test");
        }));

        assert!(poison_result.is_err());

        // SQLite callback installed after poisoning to prove setter recovery.
        // 在 poison 后安装的 SQLite 回调，用于证明 setter 已恢复。
        let callback: RuntimeSqliteProviderCallback =
            Arc::new(|_| Ok(json!({ "source": "sqlite-standard-recovered" })));
        set_sqlite_provider_callback(Some(callback));

        // Snapshot captured after poisoning to prove clone recovery.
        // 在 poison 后捕获的快照，用于证明 clone 已恢复。
        let snapshot = RuntimeDatabaseProviderCallbacks::capture_process_defaults();
        // SQLite provider request dispatched through the recovered snapshot.
        // 通过已恢复快照分发的 SQLite provider 请求。
        let sqlite_request = RuntimeSqliteProviderRequest {
            action: RuntimeSqliteProviderAction::QueryJson,
            binding: sample_binding_context(RuntimeDatabaseKind::Sqlite),
            input: json!({ "sql": "select 1" }),
        };

        assert_eq!(
            snapshot
                .dispatch_sqlite_provider_request(
                    &sqlite_request,
                    LuaRuntimeDatabaseCallbackMode::Standard,
                )
                .expect("dispatch recovered sqlite callback"),
            json!({ "source": "sqlite-standard-recovered" })
        );
    }

    /// Verify that each captured callback snapshot keeps routing to the callbacks visible at capture time.
    /// 验证每个捕获到的回调快照都会持续路由到捕获当时可见的回调实现。
    #[test]
    fn captured_callback_snapshots_stay_engine_scoped() {
        let _serial_guard = database_callback_test_lock()
            .lock()
            .expect("lock callback test guard");
        let _restore_guard = ProcessCallbackRestoreGuard::capture();

        set_sqlite_provider_callback(Some(Arc::new(|_| {
            Ok(json!({ "source": "sqlite-standard-a" }))
        })));
        set_sqlite_provider_json_callback(Some(Arc::new(|_| {
            Ok("{\"source\":\"sqlite-json-a\"}".to_string())
        })));
        set_lancedb_provider_callback(Some(Arc::new(|_| {
            Ok(RuntimeLanceDbProviderResult::json(
                json!({ "source": "lancedb-standard-a" }),
            ))
        })));
        set_lancedb_provider_json_callback(Some(Arc::new(|_| {
            Ok("{\"meta\":{\"source\":\"lancedb-json-a\"}}".to_string())
        })));
        let snapshot_a = RuntimeDatabaseProviderCallbacks::capture_process_defaults();

        set_sqlite_provider_callback(Some(Arc::new(|_| {
            Ok(json!({ "source": "sqlite-standard-b" }))
        })));
        set_sqlite_provider_json_callback(Some(Arc::new(|_| {
            Ok("{\"source\":\"sqlite-json-b\"}".to_string())
        })));
        set_lancedb_provider_callback(Some(Arc::new(|_| {
            Ok(RuntimeLanceDbProviderResult::json(
                json!({ "source": "lancedb-standard-b" }),
            ))
        })));
        set_lancedb_provider_json_callback(Some(Arc::new(|_| {
            Ok("{\"meta\":{\"source\":\"lancedb-json-b\"}}".to_string())
        })));
        let snapshot_b = RuntimeDatabaseProviderCallbacks::capture_process_defaults();

        let sqlite_request = RuntimeSqliteProviderRequest {
            action: RuntimeSqliteProviderAction::QueryJson,
            binding: sample_binding_context(RuntimeDatabaseKind::Sqlite),
            input: json!({ "sql": "select 1" }),
        };
        let lancedb_request = RuntimeLanceDbProviderRequest {
            action: RuntimeLanceDbProviderAction::VectorSearch,
            binding: sample_binding_context(RuntimeDatabaseKind::LanceDb),
            input: json!({ "table": "demo" }),
        };

        assert_eq!(
            snapshot_a
                .dispatch_sqlite_provider_request(
                    &sqlite_request,
                    LuaRuntimeDatabaseCallbackMode::Standard,
                )
                .expect("dispatch sqlite standard A"),
            json!({ "source": "sqlite-standard-a" })
        );
        assert_eq!(
            snapshot_a
                .dispatch_sqlite_provider_request(
                    &sqlite_request,
                    LuaRuntimeDatabaseCallbackMode::Json,
                )
                .expect("dispatch sqlite json A"),
            json!({ "source": "sqlite-json-a" })
        );
        assert_eq!(
            snapshot_b
                .dispatch_sqlite_provider_request(
                    &sqlite_request,
                    LuaRuntimeDatabaseCallbackMode::Standard,
                )
                .expect("dispatch sqlite standard B"),
            json!({ "source": "sqlite-standard-b" })
        );
        assert_eq!(
            snapshot_b
                .dispatch_sqlite_provider_request(
                    &sqlite_request,
                    LuaRuntimeDatabaseCallbackMode::Json,
                )
                .expect("dispatch sqlite json B"),
            json!({ "source": "sqlite-json-b" })
        );

        assert_eq!(
            snapshot_a
                .dispatch_lancedb_provider_request(
                    &lancedb_request,
                    LuaRuntimeDatabaseCallbackMode::Standard,
                )
                .expect("dispatch lancedb standard A")
                .meta,
            json!({ "source": "lancedb-standard-a" })
        );
        assert_eq!(
            snapshot_a
                .dispatch_lancedb_provider_request(
                    &lancedb_request,
                    LuaRuntimeDatabaseCallbackMode::Json,
                )
                .expect("dispatch lancedb json A")
                .meta,
            json!({ "source": "lancedb-json-a" })
        );
        assert_eq!(
            snapshot_b
                .dispatch_lancedb_provider_request(
                    &lancedb_request,
                    LuaRuntimeDatabaseCallbackMode::Standard,
                )
                .expect("dispatch lancedb standard B")
                .meta,
            json!({ "source": "lancedb-standard-b" })
        );
        assert_eq!(
            snapshot_b
                .dispatch_lancedb_provider_request(
                    &lancedb_request,
                    LuaRuntimeDatabaseCallbackMode::Json,
                )
                .expect("dispatch lancedb json B")
                .meta,
            json!({ "source": "lancedb-json-b" })
        );
    }
}
