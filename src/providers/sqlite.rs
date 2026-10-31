//! Host-registered SQLite dispatch without a database implementation.
//! 仅通过宿主注册分发 SQLite 请求，不包含数据库实现。
use crate::host::database::{
    LuaRuntimeDatabaseCallbackMode, RuntimeDatabaseBindingContext, RuntimeDatabaseKind,
    RuntimeDatabaseProviderCallbacks, RuntimeSqliteProviderAction, RuntimeSqliteProviderRequest,
    build_runtime_database_binding_plan, require_database_provider_callback_registration,
};
use crate::host::options::LuaRuntimeHostOptions;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// One skill binding routing requests to its host; it owns no database resources.
/// 向宿主转发请求的单个技能绑定，不持有数据库资源。
pub struct SqliteSkillBinding {
    /// Stable skill identifier used in the Lua status contract.
    /// Lua 状态契约使用的稳定技能标识。
    skill_name: String,
    /// Existing skill directory name retained without moving stored data.
    /// 保留现有技能目录名称，不移动存储数据。
    skill_dir_name: String,
    /// Existing derived path supplied to the host as binding context.
    /// 作为绑定上下文交给宿主的既有派生路径。
    database_path: String,
    /// Explicit callback transport chosen by this engine.
    /// 当前引擎明确选择的回调传输模式。
    callback_mode: LuaRuntimeDatabaseCallbackMode,
    /// Immutable callbacks shared with engine reload candidates.
    /// 与引擎重载候选共享的不可变回调。
    provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    /// Exact skill identity and storage context forwarded on every request.
    /// 每次请求转发的精确技能身份和存储上下文。
    provider_binding: RuntimeDatabaseBindingContext,
}

impl SqliteSkillBinding {
    /// Return the registered binding metadata; initialization confirms the binding, not an open database.
    /// 返回已注册绑定元信息；初始化状态仅确认绑定，不表示数据库已经打开。
    pub fn status_json(&self) -> Result<Value, String> {
        Ok(json!({
            "enabled": true, "initialized": true,
            "skill_name": self.skill_name, "skill_dir_name": self.skill_dir_name,
            "database_path": self.database_path, "integration_mode": "host_callback",
            "space_label": self.provider_binding.space_label,
            "root_name": self.provider_binding.root_name,
            "binding_tag": self.provider_binding.binding_tag,
            "space_root": self.provider_binding.space_root,
            "default_database_path": self.provider_binding.default_database_path,
        }))
    }

    /// Return the same binding information as status; host internals and supported actions are not inferred.
    /// 返回与状态相同的绑定信息；不推断宿主内部实现或支持的动作。
    pub fn info_json(&self) -> Result<Value, String> {
        // Registration establishes a binding, not knowledge of the host's database implementation.
        // 注册仅建立绑定，不代表了解宿主的数据库实现。
        self.status_json()
    }

    /// Forward `input` for ExecuteScript; return the exact host response or error.
    /// 为 ExecuteScript 转发 `input`；返回精确宿主响应或错误。
    pub fn execute_script(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::ExecuteScript, input)
    }

    /// Forward `input` for ExecuteBatch; return the exact host response or error.
    /// 为 ExecuteBatch 转发 `input`；返回精确宿主响应或错误。
    pub fn execute_batch(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::ExecuteBatch, input)
    }

    /// Forward `input` for QueryJson; return the exact host response or error.
    /// 为 QueryJson 转发 `input`；返回精确宿主响应或错误。
    pub fn query_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryJson, input)
    }

    /// Forward `input` for QueryStream; return the exact host response or error.
    /// 为 QueryStream 转发 `input`；返回精确宿主响应或错误。
    pub fn query_stream(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryStream, input)
    }

    /// Forward `input` for QueryStreamWaitMetrics; return the exact host response or error.
    /// 为 QueryStreamWaitMetrics 转发 `input`；返回精确宿主响应或错误。
    pub fn query_stream_wait_metrics(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryStreamWaitMetrics, input)
    }

    /// Forward `input` for QueryStreamChunk; return the exact host response or error.
    /// 为 QueryStreamChunk 转发 `input`；返回精确宿主响应或错误。
    pub fn query_stream_chunk(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryStreamChunk, input)
    }

    /// Forward `input` for QueryStreamClose; return the exact host response or error.
    /// 为 QueryStreamClose 转发 `input`；返回精确宿主响应或错误。
    pub fn query_stream_close(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::QueryStreamClose, input)
    }

    /// Forward `input` for TokenizeText; return the exact host response or error.
    /// 为 TokenizeText 转发 `input`；返回精确宿主响应或错误。
    pub fn tokenize_text_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::TokenizeText, input)
    }

    /// Forward `input` for UpsertCustomWord; return the exact host response or error.
    /// 为 UpsertCustomWord 转发 `input`；返回精确宿主响应或错误。
    pub fn upsert_custom_word_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::UpsertCustomWord, input)
    }

    /// Forward `input` for RemoveCustomWord; return the exact host response or error.
    /// 为 RemoveCustomWord 转发 `input`；返回精确宿主响应或错误。
    pub fn remove_custom_word_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::RemoveCustomWord, input)
    }

    /// Forward `input` for EnsureFtsIndex; return the exact host response or error.
    /// 为 EnsureFtsIndex 转发 `input`；返回精确宿主响应或错误。
    pub fn ensure_fts_index_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::EnsureFtsIndex, input)
    }

    /// Forward `input` for RebuildFtsIndex; return the exact host response or error.
    /// 为 RebuildFtsIndex 转发 `input`；返回精确宿主响应或错误。
    pub fn rebuild_fts_index_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::RebuildFtsIndex, input)
    }

    /// Forward `input` for UpsertFtsDocument; return the exact host response or error.
    /// 为 UpsertFtsDocument 转发 `input`；返回精确宿主响应或错误。
    pub fn upsert_fts_document_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::UpsertFtsDocument, input)
    }

    /// Forward `input` for DeleteFtsDocument; return the exact host response or error.
    /// 为 DeleteFtsDocument 转发 `input`；返回精确宿主响应或错误。
    pub fn delete_fts_document_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::DeleteFtsDocument, input)
    }

    /// Forward `input` for SearchFts; return the exact host response or error.
    /// 为 SearchFts 转发 `input`；返回精确宿主响应或错误。
    pub fn search_fts_json(&self, input: &Value) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::SearchFts, input)
    }

    /// Request the custom dictionary without input fields; return the host response or error.
    /// 在不携带输入字段的情况下请求自定义词典；返回宿主响应或错误。
    pub fn list_custom_words_json(&self) -> Result<Value, String> {
        self.dispatch_host_provider(RuntimeSqliteProviderAction::ListCustomWords, &json!({}))
    }

    /// Dispatch `action` and `input` with this binding; return the host response or error.
    /// 携带当前绑定分发 `action` 和 `input`；返回宿主响应或错误。
    fn dispatch_host_provider(
        &self,
        action: RuntimeSqliteProviderAction,
        input: &Value,
    ) -> Result<Value, String> {
        // Identity is outside the input payload so the skill cannot select another owner.
        // 身份在输入载荷之外，避免技能选择其他所有者。
        let request = RuntimeSqliteProviderRequest {
            action,
            binding: self.provider_binding.clone(),
            input: input.clone(),
        };
        self.provider_callbacks
            .dispatch_sqlite_provider_request(&request, self.callback_mode)
    }
}

/// Engine-local SQLite bindings backed exclusively by host callbacks.
/// 完全由宿主回调提供能力的引擎内 SQLite 绑定。
pub struct SqliteSkillHost {
    /// Bindings indexed by the existing engine skill identifier.
    /// 按既有引擎技能标识索引的绑定。
    skills: Mutex<HashMap<String, Arc<SqliteSkillBinding>>>,
    /// Immutable callbacks supplied by this engine's host.
    /// 当前引擎宿主提供的不可变回调。
    provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    /// Existing path and callback settings retained for compatibility.
    /// 为兼容保留的既有路径和回调设置。
    host_options: LuaRuntimeHostOptions,
}

impl SqliteSkillHost {
    /// Validate `host_options` against `provider_callbacks`; return a host without opening storage.
    /// 根据 `provider_callbacks` 验证 `host_options`；返回不打开存储的宿主。
    pub fn new(
        host_options: LuaRuntimeHostOptions,
        provider_callbacks: Arc<RuntimeDatabaseProviderCallbacks>,
    ) -> Result<Self, String> {
        require_database_provider_callback_registration(
            "SQLite",
            host_options.sqlite_callback_mode,
            provider_callbacks
                .has_sqlite_provider_callback_for_mode(host_options.sqlite_callback_mode),
        )?;
        Ok(Self {
            skills: Mutex::new(HashMap::new()),
            provider_callbacks,
            host_options,
        })
    }

    /// Bind `root_name`, `skill_name`, `skill_dir` and `config`; return the reused or new skill binding.
    /// 绑定 `root_name`、`skill_name`、`skill_dir` 和 `config`；返回复用或新增技能绑定。
    pub fn register_skill(
        &self,
        root_name: &str,
        skill_name: &str,
        skill_dir: &Path,
    ) -> Result<Arc<SqliteSkillBinding>, String> {
        // Registration changes only metadata; storage creation and cleanup belong to the host.
        // 注册仅改变元信息，存储创建和清理由宿主负责。
        let mut guard = self
            .skills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = guard.get(skill_name) {
            return Ok(Arc::clone(existing));
        }
        // Preserve the existing path derivation so databases stay in place.
        // 保留既有路径派生，使数据库保持原位。
        let plan = build_runtime_database_binding_plan(
            root_name,
            skill_name,
            skill_dir,
            &self.host_options.database_dir_name,
            RuntimeDatabaseKind::Sqlite,
        )?;
        // Each binding retains callbacks independently of process-global defaults.
        // 每个绑定保留回调，不依赖进程全局默认值。
        let binding = Arc::new(SqliteSkillBinding {
            skill_name: skill_name.to_string(),
            skill_dir_name: plan.skill_dir_name,
            database_path: plan.default_database_path,
            callback_mode: self.host_options.sqlite_callback_mode,
            provider_callbacks: Arc::clone(&self.provider_callbacks),
            provider_binding: plan.context,
        });
        guard.insert(skill_name.to_string(), Arc::clone(&binding));
        Ok(binding)
    }

    /// Look up `skill_name`; return its binding or absence without allocating a database.
    /// 查找 `skill_name`；返回其绑定或不存在状态，不分配数据库。
    pub fn binding_for_skill(
        &self,
        skill_name: &str,
    ) -> Result<Option<Arc<SqliteSkillBinding>>, String> {
        Ok(self
            .skills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(skill_name)
            .cloned())
    }
}

/// Return disabled status for optional `skill_name` without registering a backend.
/// 为可选 `skill_name` 返回禁用状态，不注册后端。
pub fn disabled_skill_status_json(skill_name: Option<&str>) -> Value {
    json!({"enabled": false, "initialized": false, "skill_name": skill_name.unwrap_or(""),
        "integration_mode": "host_callback", "reason": "current skill has not enabled sqlite"})
}
