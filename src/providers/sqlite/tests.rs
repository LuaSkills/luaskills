use super::{
    LoadedSqliteApi, SkillSqliteMeta, SqliteBindingMode, SqliteSkillBinding, SqliteSkillHost,
    parse_single_sql_params, require_present_string_field, require_sqlite_dynamic_string,
    validate_sqlite_library_info,
};
use crate::LuaRuntimeHostOptions;
use crate::host::database::{
    LuaRuntimeDatabaseCallbackMode, LuaRuntimeDatabaseProviderMode, RuntimeDatabaseBindingContext,
    RuntimeDatabaseBindingContextSpec, RuntimeDatabaseKind, RuntimeDatabaseProviderCallbacks,
};
use crate::runtime::path::render_host_visible_path;
use serde_json::json;
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Verify legacy params_json payloads are rejected in favor of formal params arrays.
/// 验证旧版 params_json 载荷会被拒绝，并要求改用正式 params 数组。
#[test]
fn parse_single_sql_params_rejects_legacy_params_json() {
    let error = match parse_single_sql_params(&json!({
        "params_json": "[1, 2, 3]"
    })) {
        Ok(_) => panic!("legacy params_json input should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error, "params_json is no longer supported; use params");
}

/// Verify mandatory dynamic SQLite string fields are not silently defaulted to empty text.
/// 验证必需的动态 SQLite 字符串字段不会被静默默认为空文本。
#[test]
fn require_sqlite_dynamic_string_rejects_missing_value() {
    // Present dynamic string value returned by the provider.
    // provider 返回的存在的动态字符串值。
    let present = require_sqlite_dynamic_string(Some("jieba".to_string()), "custom_word.word")
        .expect("present dynamic string should pass");
    // Missing dynamic string diagnostic returned for a corrupt provider record.
    // provider 记录损坏导致动态字符串缺失时返回的诊断。
    let missing_error = require_sqlite_dynamic_string(None, "custom_word.word")
        .expect_err("missing dynamic string should fail");

    assert_eq!(present, "jieba");
    assert_eq!(
        missing_error,
        "SQLite dynamic result field `custom_word.word` is missing"
    );
}

/// Verify missing dynamic SQLite FTS hit fields report their owning result shape.
/// 验证缺失的动态 SQLite FTS 命中字段会报告所属结果结构。
#[test]
fn require_sqlite_dynamic_string_labels_missing_fts_hit_field() {
    // Missing highlighted title returned by a corrupt dynamic search hit.
    // 损坏的动态检索命中返回的缺失高亮标题。
    let missing_error = require_sqlite_dynamic_string(None, "fts_hit.title_highlight")
        .expect_err("missing FTS hit string should fail");

    assert_eq!(
        missing_error,
        "SQLite dynamic result field `fts_hit.title_highlight` is missing"
    );
}

/// Verify missing dynamic SQLite execute messages report their result shape.
/// 验证缺失的动态 SQLite 执行消息会报告所属结果结构。
#[test]
fn require_sqlite_dynamic_string_labels_missing_execute_message() {
    // Missing script execution message returned by a corrupt dynamic execute result.
    // 损坏的动态执行结果返回的缺失脚本执行消息。
    let missing_error = require_sqlite_dynamic_string(None, "execute_result.message")
        .expect_err("missing execute message should fail");

    assert_eq!(
        missing_error,
        "SQLite dynamic result field `execute_result.message` is missing"
    );
}

/// Verify explicitly present SQLite string fields may carry empty text.
/// 验证显式存在的 SQLite 字符串字段可以承载空文本。
#[test]
fn require_present_string_field_preserves_empty_text() {
    // Input payload with an explicit empty title value.
    // 带有显式空标题值的输入载荷。
    let input = json!({ "title": "" });
    // Title extracted through the required-present string helper.
    // 通过必需存在字符串辅助函数提取的标题。
    let title =
        require_present_string_field(&input, "title").expect("explicit empty title should pass");
    // Missing content diagnostic returned when the field is absent.
    // 字段缺失时返回的 content 诊断。
    let missing_error = require_present_string_field(&input, "content")
        .expect_err("missing content field should fail");

    assert_eq!(title, "");
    assert_eq!(missing_error, "missing or non-string field `content`");
}

/// Verify missing SQLite libraries render paths through the host-visible formatter.
/// 验证缺失 SQLite 动态库会通过宿主可见路径渲染器输出路径。
#[test]
fn sqlite_missing_library_error_uses_host_visible_path() {
    // Missing library path used to exercise the shared real loader operation.
    // 用于触发共享真实加载操作的缺失动态库路径。
    let library_path = std::env::temp_dir().join(format!(
        "luaskills-missing-sqlite-provider-{}.dll",
        std::process::id()
    ));
    // Error returned by the real SQLite dynamic-library loader.
    // 真实 SQLite 动态库加载器返回的错误。
    let error = match LoadedSqliteApi::load(&library_path) {
        Ok(_) => panic!("missing SQLite library should fail"),
        Err(error) => error,
    };
    // Stable diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的稳定诊断前缀。
    let expected_prefix = format!(
        "failed to load SQLite dynamic library {}:",
        render_host_visible_path(&library_path)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {error}"
    );
}

/// Build one deterministic SQLite binding context for provider binding-state tests.
/// 为 provider 绑定状态测试构造一个确定性的 SQLite 绑定上下文。
fn sample_sqlite_binding_context() -> RuntimeDatabaseBindingContext {
    RuntimeDatabaseBindingContext::new(RuntimeDatabaseBindingContextSpec {
        space_label: "ROOT".to_string(),
        skill_id: "sqlite-api-state-skill".to_string(),
        root_name: "ROOT".to_string(),
        space_root: "D:/runtime-test-root/databases".to_string(),
        skill_dir: "D:/runtime-test-root/skills/sqlite-api-state-skill".to_string(),
        skill_dir_name: "sqlite-api-state-skill".to_string(),
        database_kind: RuntimeDatabaseKind::Sqlite,
        default_database_path: "D:/runtime-test-root/databases/sqlite-api-state-skill.sqlite3"
            .to_string(),
    })
}

/// Build one intentionally inconsistent dynamic SQLite binding without a loaded API.
/// 构造一个有意失配的动态 SQLite 绑定，其中没有已加载 API。
fn dynamic_sqlite_binding_without_api() -> SqliteSkillBinding {
    SqliteSkillBinding {
        api: None,
        skill_name: "sqlite-api-state-skill".to_string(),
        skill_dir_name: "sqlite-api-state-skill".to_string(),
        database_path: "D:/runtime-test-root/databases/sqlite-api-state-skill.sqlite3".to_string(),
        config: SkillSqliteMeta::default(),
        provider_mode: SqliteBindingMode::DynamicLibrary,
        callback_mode: LuaRuntimeDatabaseCallbackMode::Standard,
        handles: None,
        controller: None,
        provider_callbacks: Arc::new(RuntimeDatabaseProviderCallbacks::default()),
        provider_binding: sample_sqlite_binding_context(),
    }
}

/// Build one intentionally incomplete space-controller SQLite binding for input validation tests.
/// 为输入校验测试构造一个有意不完整的 space-controller SQLite 绑定。
fn space_controller_sqlite_binding_without_controller() -> SqliteSkillBinding {
    SqliteSkillBinding {
        api: None,
        skill_name: "sqlite-space-controller-validation-skill".to_string(),
        skill_dir_name: "sqlite-space-controller-validation-skill".to_string(),
        database_path:
            "D:/runtime-test-root/databases/sqlite-space-controller-validation-skill.sqlite3"
                .to_string(),
        config: SkillSqliteMeta::default(),
        provider_mode: SqliteBindingMode::SpaceController,
        callback_mode: LuaRuntimeDatabaseCallbackMode::Standard,
        handles: None,
        controller: None,
        provider_callbacks: Arc::new(RuntimeDatabaseProviderCallbacks::default()),
        provider_binding: sample_sqlite_binding_context(),
    }
}

/// Verify a dynamic SQLite binding missing its API returns an error instead of panicking.
/// 验证缺失 API 的动态 SQLite 绑定会返回错误，而不是 panic。
#[test]
fn sqlite_dynamic_binding_without_api_returns_error() {
    // Binding state that violates the dynamic-library construction invariant.
    // 违反动态库构造不变量的绑定状态。
    let binding = dynamic_sqlite_binding_without_api();
    // Error returned by the real execute-script dispatch path before native handle access.
    // 真实脚本执行分发路径在访问原生句柄前返回的错误。
    let error = binding
        .execute_script(&json!({ "sql": "select 1", "params": [] }))
        .expect_err("missing dynamic API should return an error");

    assert_eq!(
        error,
        "SQLite dynamic-library API is unavailable for dynamic_library binding"
    );
}

/// Verify dynamic SQLite status refuses a missing API instead of rendering placeholder metadata.
/// 验证动态 SQLite 状态拒绝缺失 API，而不是渲染占位元数据。
#[test]
fn sqlite_dynamic_status_without_api_returns_error() {
    // Binding state that violates the dynamic-library construction invariant.
    // 违反动态库构造不变量的绑定状态。
    let binding = dynamic_sqlite_binding_without_api();
    // Status diagnostic surfaced before presenting an invalid provider status payload.
    // 在展示无效 provider 状态载荷前暴露的状态诊断。
    let error = binding
        .status_json()
        .expect_err("missing dynamic API should make status fail");

    assert_eq!(
        error,
        "SQLite dynamic-library API is unavailable for dynamic_library binding"
    );
}

/// Verify dynamic SQLite library metadata must provide the formal protocol fields.
/// 验证动态 SQLite 库元数据必须提供正式协议字段。
#[test]
fn sqlite_library_info_requires_protocol_fields() {
    // Non-object metadata returned by a corrupt dynamic provider.
    // 损坏的动态 provider 返回的非对象元数据。
    let non_object_error = validate_sqlite_library_info(&json!("invalid"), "library_info_json")
        .expect_err("non-object metadata should fail metadata validation");
    // Metadata missing the mandatory dynamic library version field.
    // 缺少动态库强制 version 字段的元数据。
    let missing_version_error = validate_sqlite_library_info(
        &json!({
            "name": "vldb-sqlite",
            "ffi_stage": "stable",
            "capabilities": []
        }),
        "library_info_json",
    )
    .expect_err("missing version should fail metadata validation");
    // Metadata with a capability scalar where the protocol requires an array.
    // 在协议要求数组的位置提供标量 capabilities 的元数据。
    let invalid_capabilities_error = validate_sqlite_library_info(
        &json!({
            "name": "vldb-sqlite",
            "version": "1",
            "ffi_stage": "stable",
            "capabilities": "fts"
        }),
        "library_info_json",
    )
    .expect_err("scalar capabilities should fail metadata validation");

    assert_eq!(
        non_object_error,
        "SQLite dynamic library_info_json must be a JSON object"
    );
    assert_eq!(
        missing_version_error,
        "SQLite dynamic library_info_json field `version` is missing or empty"
    );
    assert_eq!(
        invalid_capabilities_error,
        "SQLite dynamic library_info_json field `capabilities` must be an array"
    );
}

/// Verify FTS document upsert requires title and content fields before controller access.
/// 验证 FTS 文档写入会在访问 controller 前要求 title 与 content 字段。
#[test]
fn upsert_fts_document_requires_explicit_title_and_content() {
    // Space-controller binding whose controller is intentionally absent after input validation.
    // 输入校验之后有意缺少 controller 的 space-controller 绑定。
    let binding = space_controller_sqlite_binding_without_controller();
    // Error returned when the title field is absent.
    // 缺失 title 字段时返回的错误。
    let missing_title_error = binding
        .upsert_fts_document_json(&json!({
            "index_name": "documents",
            "id": "doc-1",
            "file_path": "docs/doc-1.md",
            "content": "body"
        }))
        .expect_err("missing title should fail before controller access");
    // Error returned when the content field is absent but an empty title is explicit.
    // 显式空 title 存在但缺失 content 字段时返回的错误。
    let missing_content_error = binding
        .upsert_fts_document_json(&json!({
            "index_name": "documents",
            "id": "doc-1",
            "file_path": "docs/doc-1.md",
            "title": ""
        }))
        .expect_err("missing content should fail before controller access");

    assert_eq!(missing_title_error, "missing or non-string field `title`");
    assert_eq!(
        missing_content_error,
        "missing or non-string field `content`"
    );
}

/// Verify the SQLite skill binding registry can register and read bindings after lock poisoning.
/// 验证 SQLite skill binding 注册表锁 poison 后仍可注册并读取绑定。
#[test]
fn sqlite_skill_binding_registry_recovers_after_poisoned_lock() {
    // Host options selecting host-callback mode so the test does not load a dynamic provider library.
    // 选择 host-callback 模式的宿主选项，避免测试加载动态 provider 库。
    let host_options = LuaRuntimeHostOptions {
        sqlite_provider_mode: LuaRuntimeDatabaseProviderMode::HostCallback,
        database_dir_name: "databases".to_string(),
        ..LuaRuntimeHostOptions::default()
    };
    // SQLite host with an empty registry used by this poison recovery test.
    // 本 poison 恢复测试使用的空注册表 SQLite host。
    let host = SqliteSkillHost {
        api: None,
        controller: None,
        skills: Mutex::new(HashMap::new()),
        provider_callbacks: Arc::new(RuntimeDatabaseProviderCallbacks::default()),
        host_options,
    };

    // Captured panic result from a writer that poisons the SQLite binding registry.
    // SQLite binding 注册表写入者制造 poison 后被捕获的 panic 结果。
    let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard used only to poison the SQLite skill binding registry.
        // 仅用于制造 SQLite skill binding 注册表 poison 的保护对象。
        let _registry_guard = host
            .skills
            .lock()
            .expect("initial sqlite skill binding registry lock");
        panic!("poison sqlite skill binding registry for recovery test");
    }));

    assert!(poison_result.is_err());

    // Skill directory path used only to derive the deterministic database binding context.
    // 仅用于派生确定性数据库绑定上下文的 skill 目录路径。
    let skill_dir = PathBuf::from("D:/runtime-test-root/skills/sqlite-poison-skill");
    // Binding registered after poisoning to prove write-path recovery.
    // poison 后注册的绑定，用于证明写路径已恢复。
    let binding = host
        .register_skill(
            "ROOT",
            "sqlite-poison-skill",
            &skill_dir,
            SkillSqliteMeta::default(),
        )
        .expect("register sqlite binding after poison");
    // Binding fetched after poisoning to prove read-path recovery.
    // poison 后读取的绑定，用于证明读路径已恢复。
    let fetched = host
        .binding_for_skill("sqlite-poison-skill")
        .expect("fetch sqlite binding after poison")
        .expect("sqlite binding should exist");

    assert!(Arc::ptr_eq(&binding, &fetched));
}
