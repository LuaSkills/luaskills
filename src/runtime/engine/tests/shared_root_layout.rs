//! Roots under one parent directory share derived state and dependency directories.
//! 同一父目录下的根共用派生出的状态目录和依赖目录。

use super::*;

/// Uninstalling a shadowed USER copy keeps the ROOT copy's disabled marker, install record and
/// private dependencies when both roots derive the same state and dependency directories.
/// 两个根派生出相同的状态目录和依赖目录时，卸载被遮蔽的 USER 副本会保留 ROOT 副本的停用标记、
/// 安装记录和私有依赖。
#[test]
fn uninstalling_shadowed_copy_keeps_records_of_root_copy() {
    let runtime_root = make_temp_runtime_root("shared-root-layout-uninstall");
    let _ = fs::remove_dir_all(&runtime_root);
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("root_skills"),
    };
    let user = RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: runtime_root.join("user_skills"),
    };
    write_minimal_skill_to_root_with_response(&root.skills_dir, "foo", "root");
    write_minimal_skill_to_root_with_response(&user.skills_dir, "foo", "user");
    let chain = vec![root.clone(), user.clone()];
    let mut engine = make_runtime_test_engine();
    engine.load_from_roots(&chain).expect("initial load");

    engine
        .system_disable_skill_in_roots(&chain, SkillManagementAuthority::System, "foo", None)
        .expect("system disable of the ROOT copy");
    let state_root = runtime_root.join(engine.host_options.state_dir_name.as_str());
    let disabled_marker = state_root.join("skills").join("disabled").join("foo.json");
    assert!(disabled_marker.is_file(), "the ROOT copy is disabled");
    let install_record = state_root.join("installs").join("foo.yaml");
    fs::create_dir_all(install_record.parent().expect("install record parent"))
        .expect("create install record dir");
    fs::write(
        &install_record,
        "skill_id: foo\nversion: 0.1.0\nmanaged: true\nsource:\n  source_type: github\n  locator: acme/foo\ninstalled_at_unix_ms: 1\n",
    )
    .expect("write ROOT install record");
    let root_dependency = runtime_root
        .join(engine.host_options.dependency_dir_name.as_str())
        .join("tools")
        .join("foo");
    fs::create_dir_all(&root_dependency).expect("create ROOT dependency dir");
    fs::write(root_dependency.join("rg.bin"), "root dependency").expect("write ROOT dependency");

    engine
        .uninstall_skill_in_root(&chain, &user, "foo")
        .expect("uninstall the shadowed USER copy");

    assert!(!user.skills_dir.join("foo").exists());
    assert!(root.skills_dir.join("foo").exists());
    assert!(disabled_marker.is_file(), "ROOT disabled marker kept");
    assert!(install_record.is_file(), "ROOT install record kept");
    assert!(
        root_dependency.join("rg.bin").is_file(),
        "ROOT dependency kept"
    );
    engine.reload_from_roots(&chain).expect("reload");
    assert!(
        engine.call_skill("foo-ping", &json!({}), None).is_err(),
        "the ROOT copy stays disabled"
    );

    // With no higher-priority copy left, uninstalling removes the records as before.
    // 没有高优先级副本时，卸载照常删除记录。
    write_minimal_skill_to_root_with_response(&user.skills_dir, "bar", "user");
    engine.reload_from_roots(&chain).expect("reload with bar");
    engine
        .disable_skill_in_roots(&chain, "bar", None)
        .expect("disable bar");
    let bar_marker = state_root.join("skills").join("disabled").join("bar.json");
    assert!(bar_marker.is_file());
    engine
        .uninstall_skill_in_root(&chain, &user, "bar")
        .expect("uninstall bar");
    assert!(!bar_marker.exists(), "bar's own disabled marker is removed");

    let _ = fs::remove_dir_all(&runtime_root);
}
