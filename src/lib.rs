pub mod dependency;
pub mod download;
pub mod ffi;
pub mod ffi_standard;
pub mod host;
mod providers;
pub mod runtime;
pub mod skill;

pub use host::callbacks::{
    RuntimeEntryRegistryCallback, RuntimeEntryRegistryDelta, RuntimeHostToolAction,
    RuntimeHostToolCallback, RuntimeHostToolRequest, RuntimeModelCaller, RuntimeModelEmbedCallback,
    RuntimeModelEmbedRequest, RuntimeModelEmbedResponse, RuntimeModelError, RuntimeModelErrorCode,
    RuntimeModelLlmCallback, RuntimeModelLlmRequest, RuntimeModelLlmResponse, RuntimeModelUsage,
    RuntimeSkillLifecycleCallback, RuntimeSkillLifecycleEvent, RuntimeSkillManagementAction,
    RuntimeSkillManagementCallback, RuntimeSkillManagementRequest,
    RuntimeSkillOperationProgressCallback, RuntimeSkillOperationProgressEvent,
    set_entry_registry_callback, set_host_tool_callback, set_model_embed_callback,
    set_model_llm_callback, set_skill_lifecycle_callback, set_skill_management_callback,
    set_skill_operation_progress_callback,
};
pub use host::database::{
    LuaRuntimeDatabaseCallbackMode, LuaRuntimeDatabaseProviderMode, RuntimeDatabaseBindingContext,
    RuntimeDatabaseBindingContextSpec, RuntimeDatabaseKind, RuntimeLanceDbProviderAction,
    RuntimeLanceDbProviderCallback, RuntimeLanceDbProviderJsonCallback,
    RuntimeLanceDbProviderRequest, RuntimeLanceDbProviderResult, RuntimeSqliteProviderAction,
    RuntimeSqliteProviderCallback, RuntimeSqliteProviderJsonCallback, RuntimeSqliteProviderRequest,
    set_lancedb_provider_callback, set_lancedb_provider_json_callback,
    set_sqlite_provider_callback, set_sqlite_provider_json_callback,
};
pub use host::options::{
    LuaInvocationContext, LuaRuntimeCapabilityOptions, LuaRuntimeHostOptions,
    LuaRuntimeSpaceControllerOptions, LuaRuntimeSpaceControllerProcessMode, RuntimeSkillRoot,
};
pub use runtime::cache::{
    DEFAULT_TOOL_CACHE_DEFAULT_TTL_SECS, DEFAULT_TOOL_CACHE_MAX_ENTRIES,
    DEFAULT_TOOL_CACHE_MAX_TTL_SECS, ToolCacheConfig,
};
pub use runtime::config::SkillConfigEntry;
pub use runtime::context::{RuntimeClientInfo, RuntimeRequestContext};
pub use runtime::engine::{LuaEngine, LuaEngineOptions, LuaVmPoolConfig};
pub use runtime::entry::{RuntimeEntryDescriptor, RuntimeEntryParameterDescriptor};
pub use runtime::help::{RuntimeHelpDetail, RuntimeHelpNodeDescriptor, RuntimeSkillHelpDescriptor};
pub use runtime::logging::{
    RuntimeLogCallback, RuntimeLogEvent, RuntimeLogLevel, set_log_callback,
};
pub use runtime::managed_runtime::{
    MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION, ManagedRuntimeEnvHashInput, ManagedRuntimeEnvMarker,
    ManagedRuntimeEnvPlan, ManagedRuntimeInstallManifest, ManagedRuntimeKind,
    ManagedRuntimePersistentSessionCapability, WINDOWS_ARM_PERSISTENT_SESSION_UNSUPPORTED_REASON,
    compute_managed_runtime_env_hash, current_managed_runtime_persistent_session_capability,
    current_managed_runtime_platform_key, ensure_managed_env, managed_env_dir,
    managed_env_is_ready, managed_env_marker_matches, managed_env_marker_path,
    read_install_manifest, read_managed_env_marker, sha256_file, sha256_hex,
};
pub use runtime::managed_session_events::{
    RuntimeManagedSessionEvent, RuntimeManagedSessionEventBatch, RuntimeManagedSessionEventKind,
    RuntimeManagedSessionWakeCallback,
};
pub use runtime::result::{
    NON_STRING_TOOL_RESULT_ERROR, RuntimeInvocationResult, ToolOverflowMode,
};
pub use skill::dependencies::{
    DependencyArchiveType, DependencyExportSpec, DependencyPackageSpec, DependencySourceSpec,
    FfiDependencySpec, GithubReleaseSourceSpec, LuaDependencySpec, NodeRuntimeDependencySpec,
    NodeRuntimePackageManager, PackageDependencyManifest, PythonRuntimeDependencySpec,
    PythonRuntimePackageManager, SkillListPackageManifest, SkillListSourceSpec, ToolDependencySpec,
    UrlSourceSpec,
};
pub use skill::manager::{
    DisabledSkillRecord, ResolvedSkillInstance, SkillApplyResult, SkillInstallRequest,
    SkillLifecycleAction, SkillManagementAuthority, SkillManager, SkillManagerConfig,
    SkillOperationPlane, SkillUninstallOptions, SkillUninstallResult,
    collect_effective_skill_instances, resolve_declared_skill_instance_from_roots,
    resolve_effective_skill_instance,
};
pub use skill::manifest::{SkillHelpMeta, SkillHelpNodeMeta, SkillMeta, SkillToolMeta};
pub use skill::source::{InstalledSkillRecord, InstalledSkillSourceRecord, SkillInstallSourceType};

pub use host::options as runtime_options;
pub use runtime::cache as tool_cache;
pub use runtime::config as runtime_config_store;
pub use runtime::context as runtime_context;
pub use runtime::engine as lua_engine;
pub use runtime::entry as entry_descriptor;
pub use runtime::help as runtime_help;
pub use runtime::logging as runtime_logging;
pub use runtime::result as runtime_result;
pub use skill::manifest as lua_skill;

pub(crate) use providers::lancedb as lancedb_host;
pub(crate) use providers::sqlite as sqlite_host;
