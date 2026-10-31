//! SQLite host registration, nested identity and retained-data regression tests.
//! SQLite 宿主注册、嵌套身份及数据保留回归测试。

use super::*;
use crate::{LuaRuntimeDatabaseCallbackMode, RuntimeSqliteProviderAction};

/// Write a SQLite-enabled `name` under `root` with exact `source`; return its package directory.
/// 在 `root` 下以精确 `source` 写入启用 SQLite 的 `name`；返回包目录。
fn sqlite_skill(root: &Path, name: &str, source: &str) -> PathBuf {
    // Reuse the canonical minimal package fixture before enabling its host capability.
    // 复用规范最小包夹具，再启用其宿主能力。
    let directory = write_minimal_skill_to_root(root, name);
    // The manifest receives only the supported SQLite declaration.
    // 清单仅添加受支持的 SQLite 声明。
    let manifest = fs::read_to_string(directory.join("skill.yaml")).unwrap();
    fs::write(
        directory.join("skill.yaml"),
        format!("{manifest}sqlite:\n  enable: true\n"),
    )
    .unwrap();
    fs::write(directory.join("runtime/ping.lua"), source).unwrap();
    directory
}

/// Build an isolated callback collection reporting `owner`; return either supported transport.
/// 构造报告 `owner` 的隔离回调集合；返回任一受支持传输。
fn callbacks(owner: &'static str, json_transport: bool) -> RuntimeDatabaseProviderCallbacks {
    if json_transport {
        RuntimeDatabaseProviderCallbacks::default().with_sqlite_json(Arc::new(move |request| {
            // JSON transport must preserve the same authoritative binding and input envelope.
            // JSON 传输必须保留相同的权威绑定和输入包络。
            let request: crate::RuntimeSqliteProviderRequest =
                serde_json::from_str(request).unwrap();
            assert_eq!(request.action, RuntimeSqliteProviderAction::QueryJson);
            Ok(json!({"owner": owner, "skill": request.binding.skill_id}).to_string())
        }))
    } else {
        RuntimeDatabaseProviderCallbacks::default().with_sqlite(Arc::new(move |request| {
            assert_eq!(request.action, RuntimeSqliteProviderAction::QueryJson);
            assert!(request.binding.default_database_path.ends_with(".sqlite3"));
            Ok(json!({"owner": owner, "skill": request.binding.skill_id}))
        }))
    }
}

/// Verify distinct engines, reloads and nested restoration retain exact host/skill identities.
/// 验证不同引擎、重载及嵌套恢复保留精确宿主与技能身份。
#[test]
fn sqlite_host_registration_isolates_engines_and_survives_reload() {
    // The test owns this unique runtime root and all files created underneath it.
    // 测试拥有此唯一区域及其下创建的全部文件。
    let root = make_temp_runtime_root("sqlite-host-registration");
    create_runtime_test_layout(&root);
    // Two packages exercise both the nested destination and restored source binding.
    // 两个包同时检验嵌套目标绑定和恢复后的源绑定。
    let skill_root = RuntimeSkillRoot {
        name: "ROOT".into(),
        skills_dir: root.join("skills"),
    };
    sqlite_skill(
        &skill_root.skills_dir,
        "inner",
        "return function() return vulcan.json.encode(vulcan.sqlite.query_json({sql='select 1'})) end",
    );
    sqlite_skill(
        &skill_root.skills_dir,
        "outer",
        r#"return function()
        assert(vulcan.lancedb == nil)
        -- The nested request uses the destination binding.
        -- 嵌套请求使用目标绑定。
        local nested = vulcan.json.decode(vulcan.call('inner-ping', {}))
        -- The following request must restore the source binding.
        -- 后续请求必须恢复源绑定。
        local own = vulcan.sqlite.query_json({sql='select 1'})
        return vulcan.json.encode({nested=nested, own=own})
    end"#,
    );
    // Legacy data is an opaque sentinel; registration, calls and reloads must leave it intact.
    // 旧数据是不可解释的哨兵；注册、调用及重载必须保持其完整。
    let retained = root.join("databases/lancedb/outer/retained.bin");
    fs::create_dir_all(retained.parent().unwrap()).unwrap();
    fs::write(&retained, b"unchanged historical database").unwrap();
    // Each engine uses one immutable collection even when both operate on the same package root.
    // 即使两个引擎操作同一个包根，每个引擎也使用单个不可变集合。
    let mut engines = Vec::new();
    for (owner, transport) in [("first", false), ("second", true)] {
        // Transport is explicit, with no fallback to a different registered callback kind.
        // 传输显式指定，不回退到其他已注册回调种类。
        let mut host = LuaRuntimeHostOptions::with_runtime_root(&root);
        host.sqlite_callback_mode = if transport {
            LuaRuntimeDatabaseCallbackMode::Json
        } else {
            LuaRuntimeDatabaseCallbackMode::Standard
        };
        engines.push((
            owner,
            LuaEngine::new_with_database_providers(
                runtime_test_engine_options(host),
                callbacks(owner, transport),
            )
            .unwrap(),
        ));
    }
    for (owner, engine) in &mut engines {
        engine
            .load_from_roots(std::slice::from_ref(&skill_root))
            .unwrap();
        for reloading in [false, true] {
            if reloading {
                engine
                    .reload_from_roots(std::slice::from_ref(&skill_root))
                    .unwrap();
            }
            // The nested call returns its own identity, and the outer SQL call sees the restored identity.
            // 嵌套调用返回自身身份，外层 SQL 调用看到恢复后的身份。
            let result = engine.call_skill("outer-ping", &json!({}), None).unwrap();
            // The test decodes the actual Lua result, rather than inspecting private callback fields.
            // 测试解码实际 Lua 结果，而非检查私有回调字段。
            let result: Value = serde_json::from_str(&result.content).unwrap();
            assert_eq!(
                result,
                json!({"nested":{"owner":owner,"skill":"inner"},"own":{"owner":owner,"skill":"outer"}})
            );
        }
    }
    assert!(
        !root.join("databases/sqlite").exists(),
        "only the host may create SQLite storage"
    );
    // A host-created SQLite file must also survive ordinary skill package removal.
    // 宿主创建的 SQLite 文件也必须在普通技能包移除后保留。
    let sqlite_retained = root.join("databases/sqlite/outer/outer.sqlite3");
    fs::create_dir_all(sqlite_retained.parent().unwrap()).unwrap();
    fs::write(&sqlite_retained, b"host-owned SQLite data").unwrap();
    engines[0]
        .1
        .system_uninstall_skill(
            std::slice::from_ref(&skill_root),
            crate::SkillManagementAuthority::System,
            "outer",
        )
        .unwrap();
    assert_eq!(
        fs::read(retained).unwrap(),
        b"unchanged historical database"
    );
    assert_eq!(
        fs::read(sqlite_retained).unwrap(),
        b"host-owned SQLite data"
    );
    drop(engines);
    fs::remove_dir_all(root).unwrap();
}

/// Reject a missing selected transport and removed configuration without touching historical data.
/// 拒绝缺失的指定传输及已删除配置，不接触历史数据。
#[test]
fn sqlite_host_registration_rejects_missing_callback_and_retired_config() {
    // The fixture has no callback and therefore cannot obtain database capability.
    // 夹具没有回调，因此无法取得数据库能力。
    let root = make_temp_runtime_root("sqlite-missing-host");
    create_runtime_test_layout(&root);
    // Root and skill are separate values to exercise the public loader API.
    // 根与技能使用独立值，以检验公开加载 API。
    let skill_root = RuntimeSkillRoot {
        name: "ROOT".into(),
        skills_dir: root.join("skills"),
    };
    sqlite_skill(
        &skill_root.skills_dir,
        "requires-host",
        "return function() return 'unused' end",
    );
    // Explicit empty registration must not inherit process-global callbacks.
    // 显式空注册不得继承进程全局回调。
    let mut engine = LuaEngine::new_with_database_providers(
        runtime_test_engine_options(LuaRuntimeHostOptions::with_runtime_root(&root)),
        RuntimeDatabaseProviderCallbacks::default(),
    )
    .unwrap();
    engine.load_from_roots(&[skill_root]).unwrap();
    assert!(
        engine.skills.is_empty(),
        "loader must reject the skill without a host callback"
    );
    // The loader keeps its existing skip-and-report contract; the provider carries the precise failure.
    // 加载器保留既有跳过并报告契约；provider 提供精确失败原因。
    let failure = crate::sqlite_host::SqliteSkillHost::new(
        LuaRuntimeHostOptions::default(),
        Arc::new(RuntimeDatabaseProviderCallbacks::default()),
    )
    .err()
    .unwrap();
    assert!(failure.contains("no standard callback is registered"));
    for key in [
        "sqlite_library_path",
        "sqlite_provider_mode",
        "space_controller",
        "lancedb_callback_mode",
    ] {
        // Start from a complete valid value so only the retired key accounts for rejection.
        // 从完整有效值开始，确保拒绝仅由退役键引起。
        let mut value = serde_json::to_value(LuaRuntimeHostOptions::default()).unwrap();
        value[key] = Value::Null;
        assert!(
            serde_json::from_value::<LuaRuntimeHostOptions>(value)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }
    assert!(
        serde_norway::from_str::<SkillMeta>(
            "name: retired\nversion: 0.1.0\nlancedb:\n  enable: true\n"
        )
        .unwrap_err()
        .to_string()
        .contains("unknown field")
    );
    for field in [
        "log_level: info",
        "slow_log_enabled: true",
        "slow_log_threshold_ms: 500",
        "unknown_option: true",
    ] {
        // SQLite options belong to the binding contract; ignored host-policy declarations are forbidden.
        // SQLite 选项属于绑定契约；禁止被忽略的宿主策略声明。
        let manifest =
            format!("name: retired\nversion: 0.1.0\nsqlite:\n  enable: true\n  {field}\n");
        assert!(
            serde_norway::from_str::<SkillMeta>(&manifest)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }
    drop(engine);
    fs::remove_dir_all(root).unwrap();
}

/// Exercise every Lua SQLite action through both transports; preserve input shapes and host binding authority.
/// 通过两种传输检验全部 Lua SQLite 动作；保留输入结构及宿主绑定权威性。
#[test]
fn sqlite_host_parameters_round_trip_all_actions() {
    // Only the fixture owns this root; callbacks never create database files underneath it.
    // 仅夹具拥有此根目录；回调不在其中创建数据库文件。
    let root = make_temp_runtime_root("sqlite-parameters");
    create_runtime_test_layout(&root);
    // The directory identity must remain authoritative even when the display name differs.
    // 即使显示名称不同，目录身份仍必须保持权威。
    let directory = sqlite_skill(
        &root.join("skills"),
        "parameter-owner",
        r#"return function(args)
        if args.action == 'status' then
            return vulcan.json.encode({status=vulcan.sqlite.status(), info=vulcan.sqlite.info()})
        end
        if args.action == 'list_custom_words' then
            return vulcan.json.encode(vulcan.sqlite.list_custom_words())
        end
        return vulcan.json.encode(vulcan.sqlite[args.action](args.input))
    end"#,
    );
    // Replace the exact fixture declaration rather than deriving a second runtime identity.
    // 替换精确夹具声明，而不派生第二套运行时身份。
    let manifest = fs::read_to_string(directory.join("skill.yaml")).unwrap();
    assert!(manifest.contains("name: parameter-owner"));
    fs::write(
        directory.join("skill.yaml"),
        manifest.replacen("name: parameter-owner", "name: display-only", 1),
    )
    .unwrap();
    // Complete expected binding comes from the fixture's physical paths and declared root.
    // 完整预期绑定来自夹具物理路径及已声明根。
    let binding = json!({
        "space_label":"ROOT", "skill_id":"parameter-owner", "root_name":"ROOT",
        "binding_tag":"ROOT-parameter-owner", "database_kind":"sqlite",
        "space_root":render_host_visible_path(&root.join("private-store")),
        "skill_dir":render_host_visible_path(&directory), "skill_dir_name":"parameter-owner",
        "default_database_path":render_host_visible_path(&root.join("private-store").join("sqlite").join("parameter-owner").join("parameter-owner.sqlite3"))
    });
    // User input deliberately contains misleading identity keys; none may replace the outer binding.
    // 用户输入故意包含误导性身份键；它们均不得替换外层绑定。
    let input = json!({"sql":"select ?", "params":[null,false,0,1.25,"中文\u{0}参数"],
        "empty_array":[], "empty_object":{}, "nested":{"value":null,"enabled":false},
        "binding":{"skill_id":"forged"}, "skill_id":"forged", "database_path":"forged"});
    for (json_transport, canonical_layout) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        // Canonical runtime_root intentionally owns the fixed directory names; legacy paths remain configurable.
        // 规范 runtime_root 有意拥有固定目录名；旧有路径仍可配置。
        let mut expected_binding = binding.clone();
        if canonical_layout {
            expected_binding["space_root"] =
                json!(render_host_visible_path(&root.join("databases")));
            expected_binding["default_database_path"] = json!(render_host_visible_path(
                &root
                    .join("databases")
                    .join("sqlite")
                    .join("parameter-owner")
                    .join("parameter-owner.sqlite3")
            ));
        }
        // Both providers return the exact received envelope, exposing any transport conversion loss.
        // 两种提供者返回精确接收包络，使传输转换丢失可被观察。
        let providers = if json_transport {
            RuntimeDatabaseProviderCallbacks::default().with_sqlite_json(Arc::new(|text| {
                // The transport contract requires a structured request with an explicit input field.
                // 传输契约要求结构化请求及显式输入字段。
                let request: crate::RuntimeSqliteProviderRequest =
                    serde_json::from_str(text).unwrap();
                if request.input["fail"] == true {
                    return Err("host rejected requested operation".into());
                }
                Ok(text.to_owned())
            }))
        } else {
            RuntimeDatabaseProviderCallbacks::default().with_sqlite(Arc::new(|request| {
                if request.input["fail"] == true {
                    return Err("host rejected requested operation".into());
                }
                Ok(serde_json::to_value(request).unwrap())
            }))
        };
        // The host supplies a non-default storage suggestion and selects exactly one callback transport.
        // 宿主提供非默认存储建议，并选择精确一种回调传输。
        let mut host = LuaRuntimeHostOptions::with_runtime_root(&root);
        if !canonical_layout {
            host.runtime_root = None;
        }
        host.database_dir_name = "private-store".into();
        host.sqlite_callback_mode = if json_transport {
            LuaRuntimeDatabaseCallbackMode::Json
        } else {
            LuaRuntimeDatabaseCallbackMode::Standard
        };
        // A distinct engine owns the immutable callback set for each transport.
        // 每种传输由独立引擎拥有不可变回调集合。
        let mut engine =
            LuaEngine::new_with_database_providers(runtime_test_engine_options(host), providers)
                .unwrap();
        engine
            .load_from_roots(&[RuntimeSkillRoot {
                name: "ROOT".into(),
                skills_dir: root.join("skills"),
            }])
            .unwrap();
        for action in [
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
        ] {
            // Observe the actual Lua call result, including host-to-Lua response conversion.
            // 观察真实 Lua 调用结果，包括宿主到 Lua 的响应转换。
            let result = engine
                .call_skill(
                    "parameter-owner-ping",
                    &json!({"action":action,"input":input}),
                    None,
                )
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&result.content).unwrap(),
                json!({"action":action,"binding":expected_binding,
                "input":if action == "list_custom_words" { json!({}) } else { input.clone() }})
            );
        }
        // Status reports registration metadata only, without claiming a particular host implementation.
        // 状态仅报告注册元信息，不声称宿主采用特定实现。
        let result = engine
            .call_skill("parameter-owner-ping", &json!({"action":"status"}), None)
            .unwrap();
        // Both supported metadata methods expose the same binding readiness.
        // 两种受支持元信息方法均暴露相同绑定就绪状态。
        let metadata: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(metadata["status"], metadata["info"]);
        assert_eq!(metadata["status"]["binding_tag"], "ROOT-parameter-owner");
        for removed in [
            "log_level",
            "slow_log_enabled",
            "slow_log_threshold_ms",
            "library_path",
            "library_name",
            "library_version",
            "library_info",
            "ffi_stage",
            "capabilities",
        ] {
            assert!(metadata["status"].get(removed).is_none(), "{removed}");
        }
        // Native host failures remain visible instead of being replaced by an empty successful result.
        // 原生宿主失败保持可见，不被替换为空的成功结果。
        let error = engine
            .call_skill(
                "parameter-owner-ping",
                &json!({"action":"query_json","input":{"fail":true}}),
                None,
            )
            .unwrap_err();
        assert!(
            error.contains("host rejected requested operation"),
            "{error}"
        );
    }
    assert!(!root.join("private-store").exists());
    assert!(!root.join("databases/sqlite").exists());
    fs::remove_dir_all(root).unwrap();
}
