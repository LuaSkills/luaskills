//! One broken skill is skipped with a diagnostic; every other skill still loads.
//! 单个损坏的技能会被跳过并记录诊断，其余技能照常加载。

use super::*;
use crate::skill::manager::collect_effective_skill_instances_from_roots;

/// Write a skill under `skill_root` whose one entry has `lua_source`.
/// 在 `skill_root` 下写入一个技能，其唯一入口的源码为 `lua_source`。
fn write_skill_with_entry_source(skill_root: &Path, skill_id: &str, lua_source: &str) {
    write_model_test_skill_to_root(skill_root, skill_id, lua_source);
}

/// Write a skill under `skill_root` with a valid entry and the given `skill.yaml` text.
/// 在 `skill_root` 下写入一个入口有效、`skill.yaml` 为给定文本的技能。
fn write_skill_with_manifest(skill_root: &Path, skill_id: &str, manifest: &str) {
    let skill_dir = write_minimal_skill_to_root(skill_root, skill_id);
    fs::write(skill_dir.join("skill.yaml"), manifest).expect("overwrite skill manifest");
}

/// Sorted canonical names of the entries an engine currently exposes.
/// 引擎当前暴露的入口 canonical 名称（已排序）。
fn entry_names(engine: &LuaEngine) -> Vec<String> {
    let mut names: Vec<String> = engine
        .list_entries()
        .expect("list entries")
        .into_iter()
        .map(|entry| entry.canonical_name)
        .collect();
    names.sort();
    names
}

/// An entry that fails to compile, fails to initialize, or fails only in the hardened runLua VM
/// drops its own skill; the other skills load and both VM pools still build.
/// 编译失败、初始化失败或只在加固后的 runLua VM 中失败的入口只会移除其所属技能；其余技能照常加载，
/// 两个 VM 池都能构建。
#[test]
fn load_from_roots_drops_only_skills_whose_entries_fail_in_a_vm() {
    let runtime_root = make_temp_runtime_root("load-isolation-entry-failures");
    let _ = fs::remove_dir_all(&runtime_root);
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    write_minimal_skill_to_root_with_response(&root.skills_dir, "healthy", "still here");
    // A broken ordinary initializer runs first and must leave no state for later skills.
    // 损坏的普通初始化入口先运行，其状态不得留给后续技能。
    write_skill_with_entry_source(
        &root.skills_dir,
        "a-init-error",
        "probe_marker = true; error('initialization failed')",
    );
    write_skill_with_entry_source(
        &root.skills_dir,
        "c-isolated",
        "assert(probe_marker == nil, 'previous initializer leaked'); return function() return 'ok' end",
    );
    write_skill_with_entry_source(&root.skills_dir, "compile-error", "return function(\n");
    // The same isolation must hold when only the hardened initializer fails.
    // 仅加固 VM 的初始化失败时也必须保持同样的隔离。
    write_skill_with_entry_source(
        &root.skills_dir,
        "b-runlua-only-error",
        "if not debug.sethook then runlua_probe_marker = true; error('sethook is gone') end; return function() return 'x' end",
    );
    write_skill_with_entry_source(
        &root.skills_dir,
        "d-runlua-isolated",
        "assert(runlua_probe_marker == nil, 'previous runLua initializer leaked'); return function() return 'ok' end",
    );

    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(std::slice::from_ref(&root))
        .expect("broken skills must not fail the whole load");

    assert_eq!(
        entry_names(&engine),
        vec!["c-isolated-ping", "d-runlua-isolated-ping", "healthy-ping"]
    );
    let result = engine
        .call_skill("healthy-ping", &json!({}), None)
        .expect("call healthy skill");
    assert_eq!(result.content, "still here");
    let runlua = engine
        .execute_runlua_request_json_inline(
            &json!({ "code": "return vulcan.call('healthy-ping', {})" }).to_string(),
        )
        .expect("runLua VMs build without the broken skills");
    assert!(runlua.contains("still here"), "{runlua}");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// A debug skill edited into a broken state after loading fails only its own calls; new VMs for
/// every other skill still build.
/// 加载后被改坏的调试技能只让对它自己的调用失败；其他技能的新 VM 照常构建。
#[test]
fn broken_edit_to_debug_skill_does_not_fail_new_vms() {
    let runtime_root = make_temp_runtime_root("load-isolation-debug-edit");
    let _ = fs::remove_dir_all(&runtime_root);
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    write_minimal_skill_to_root(&root.skills_dir, "healthy");
    let debug_dir = write_minimal_skill_to_root(&root.skills_dir, "debugged");
    fs::write(
        debug_dir.join("skill.yaml"),
        "name: debugged\nversion: 0.1.0\nenable: true\ndebug: true\nentries:\n  - name: ping\n    description: Ping.\n    lua_entry: runtime/ping.lua\n    lua_module: debugged.ping\n",
    )
    .expect("mark skill as debug");

    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(std::slice::from_ref(&root))
        .expect("load skills");
    assert_eq!(
        entry_names(&engine),
        vec!["debugged-ping".to_string(), "healthy-ping".to_string()]
    );

    fs::write(
        debug_dir.join("runtime").join("ping.lua"),
        "return function(\n",
    )
    .expect("break the debug entry");
    engine
        .create_vm()
        .expect("a broken debug entry must not fail building a VM");
    let error = engine
        .call_skill("debugged-ping", &json!({}), None)
        .expect_err("the broken debug entry fails its own call");
    assert!(error.contains("Failed to compile skill"), "{error}");
    let result = engine
        .call_skill("healthy-ping", &json!({}), None)
        .expect("call healthy skill");
    assert_eq!(result.content, "ok");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Enabling a skill that then fails to load reports the failure and leaves the active state alone.
/// 启用后无法加载的技能会报告失败，且不改动活动状态。
#[test]
fn enabling_a_skill_that_fails_to_load_reports_the_failure() {
    let runtime_root = make_temp_runtime_root("load-isolation-enable-broken");
    let _ = fs::remove_dir_all(&runtime_root);
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    write_minimal_skill_to_root(&root.skills_dir, "root-healthy");
    let toggled_dir = write_minimal_skill_to_root(&user.skills_dir, "toggled");
    let roots = [root, user];

    let mut engine = make_runtime_test_engine();
    engine.load_from_roots(&roots).expect("load skills");
    engine
        .disable_skill_in_roots(&roots, "toggled", Some("Disabled by user"))
        .expect("disable the user skill");
    // Read the exact on-disk record so the failure must preserve reason and timestamp as well.
    // 读取磁盘上的完整记录，以确认失败时同时保留停用原因和时间戳。
    let manager = engine.skill_manager_for(&roots[1]).expect("user manager");
    // Preserve the original disabled state for the comparison after the failed enable.
    // 保留原有停用状态，供启用失败后对照。
    let previous_marker = manager
        .disabled_record("toggled")
        .expect("disabled record")
        .expect("skill is disabled");
    fs::write(
        toggled_dir.join("runtime").join("ping.lua"),
        "return function(\n",
    )
    .expect("break the disabled skill");

    let error = engine
        .enable_skill(&roots, "toggled")
        .expect_err("enabling a skill that fails to load must report it");
    assert!(
        error.to_string().contains("'toggled' failed to load"),
        "unexpected error: {error}"
    );
    assert_eq!(entry_names(&engine), vec!["root-healthy-ping".to_string()]);
    // Repairing the source must not silently enable the skill on a normal reload.
    // 修复源码后进行普通重载，不应悄悄启用该技能。
    let current_marker = manager
        .disabled_record("toggled")
        .expect("read disabled record")
        .expect("failed enable keeps the marker");
    assert_eq!(current_marker.reason, previous_marker.reason);
    assert_eq!(
        current_marker.disabled_at_unix_ms,
        previous_marker.disabled_at_unix_ms
    );
    fs::write(
        toggled_dir.join("runtime/ping.lua"),
        "return function() return 'repaired' end",
    )
    .expect("repair skill");
    engine.reload_from_roots(&roots).expect("ordinary reload");
    assert_eq!(entry_names(&engine), vec!["root-healthy-ping"]);
    engine
        .enable_skill(&roots, "toggled")
        .expect("explicitly enable repaired skill");
    assert!(
        manager
            .disabled_record("toggled")
            .expect("read disabled record")
            .is_none()
    );
    assert_eq!(
        entry_names(&engine),
        vec!["root-healthy-ping", "toggled-ping"]
    );

    let _ = fs::remove_dir_all(&runtime_root);
}

/// A malformed `skill.yaml` in a USER root skips that skill alone; ROOT skills still load.
/// USER 根中写错的 `skill.yaml` 只跳过该技能；ROOT 技能照常加载。
#[test]
fn load_from_roots_skips_only_skills_with_malformed_user_manifests() {
    let runtime_root = make_temp_runtime_root("load-isolation-malformed-manifests");
    let _ = fs::remove_dir_all(&runtime_root);
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    write_minimal_skill_to_root(&root.skills_dir, "root-healthy");
    write_minimal_skill_to_root(&user.skills_dir, "user-healthy");
    let entries =
        "entries:\n  - name: ping\n    description: Ping.\n    lua_entry: runtime/ping.lua\n";
    write_skill_with_manifest(
        &user.skills_dir,
        "syntax-error",
        "name: [unterminated\nversion: 0.1.0\n",
    );
    write_skill_with_manifest(
        &user.skills_dir,
        "declares-id",
        &format!(
            "skill_id: other\nname: declares-id\nversion: 0.1.0\n{entries}    lua_module: declares-id.ping\n"
        ),
    );
    write_skill_with_manifest(
        &user.skills_dir,
        "enable-maybe",
        &format!(
            "name: enable-maybe\nversion: 0.1.0\nenable: maybe\n{entries}    lua_module: enable-maybe.ping\n"
        ),
    );
    let roots = [root, user];

    // Resolution keeps the broken directories, so loading reports each one by itself.
    // 解析阶段保留损坏的目录，由加载阶段逐个报告。
    let mut resolved: Vec<String> = collect_effective_skill_instances_from_roots(&roots)
        .expect("malformed manifests do not fail resolution")
        .into_iter()
        .map(|instance| instance.skill_id)
        .collect();
    resolved.sort();
    assert_eq!(
        resolved,
        [
            "declares-id",
            "enable-maybe",
            "root-healthy",
            "syntax-error",
            "user-healthy"
        ]
    );

    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&roots)
        .expect("malformed USER manifests must not fail the whole load");
    assert_eq!(
        entry_names(&engine),
        vec![
            "root-healthy-ping".to_string(),
            "user-healthy-ping".to_string()
        ]
    );

    let _ = fs::remove_dir_all(&runtime_root);
}
