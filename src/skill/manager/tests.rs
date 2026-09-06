use super::{
    PreparedSkillApply, PreparedSkillInstall, PreparedSkillUninstall, PreparedSkillUpdate,
    SkillApplyResult, SkillInstallRequest, SkillInstallSourceType, SkillManager,
    SkillManagerConfig, SkillOperationPlane, SkillUninstallResult, TempDirGuard,
    collect_effective_skill_instances, collect_effective_skill_instances_from_roots,
    format_uninstall_finalization_error, github_repo_skill_id, is_allowed_private_source_url,
    is_skill_manifest_enabled, normalize_github_repo_locator, publish_staged_skill_update,
    remove_staging_dir_if_present, resolve_effective_skill_instance, resolve_requested_skill_id,
    staging_temp_root_is_directory, unix_millis_from_system_time,
};
use crate::runtime::path::render_host_visible_path;
use crate::runtime_options::RuntimeSkillRoot;
use crate::skill::source::{InstalledSkillRecord, InstalledSkillSourceRecord};

/// Build one test skill-manager configuration rooted under the provided temporary directory.
/// 基于给定临时目录构造单个测试用技能管理器配置。
fn test_manager_config(
    temp_root: &std::path::Path,
    skill_root: RuntimeSkillRoot,
) -> SkillManagerConfig {
    SkillManagerConfig {
        skill_root,
        lifecycle_root: temp_root.join("state"),
        download_cache_root: temp_root.join("downloads"),
        allow_network_download: false,
        github_base_url: None,
        github_api_base_url: None,
        official_skill_hub_base_url: None,
        enable_private_url_skill_install: false,
        private_skill_source_allowlist: Vec::new(),
    }
}

/// Build one deterministic source record for prepared install/update lifecycle tests.
/// 构造一个用于预备安装/更新生命周期测试的确定性来源记录。
///
/// Returns a GitHub source descriptor that avoids network access while keeping record shape realistic.
/// 返回一个 GitHub 来源描述符，避免网络访问，同时保持记录结构真实。
fn test_install_source_record() -> InstalledSkillSourceRecord {
    InstalledSkillSourceRecord {
        source_type: SkillInstallSourceType::Github,
        locator: "vulcan-ai/vulcan-codekit".to_string(),
        tag: Some("v0.1.0".to_string()),
    }
}

/// Build one deterministic install record for prepared lifecycle tests.
/// 构造一个用于预备生命周期测试的确定性安装记录。
///
/// The version parameter is the semantic version string stored in the returned record.
/// version 参数是写入返回记录的语义化版本字符串。
///
/// Returns an installed-skill record for the shared vulcan-codekit fixture.
/// 返回共享 vulcan-codekit 夹具使用的已安装技能记录。
fn test_installed_record(version: &str) -> InstalledSkillRecord {
    InstalledSkillRecord {
        skill_id: "vulcan-codekit".to_string(),
        version: version.to_string(),
        managed: true,
        source: test_install_source_record(),
        installed_at_unix_ms: 1,
    }
}

/// Build one deterministic apply result for prepared lifecycle tests.
/// 构造一个用于预备生命周期测试的确定性应用结果。
///
/// The status parameter becomes the returned high-level lifecycle status.
/// status 参数会成为返回值中的高层生命周期状态。
///
/// Returns an apply result for the shared vulcan-codekit fixture.
/// 返回共享 vulcan-codekit 夹具使用的应用结果。
fn test_apply_result(status: &str) -> SkillApplyResult {
    SkillApplyResult {
        skill_id: "vulcan-codekit".to_string(),
        status: status.to_string(),
        message: format!("skill was {}", status),
        version: Some("0.1.0".to_string()),
        source_type: Some(SkillInstallSourceType::Github),
        source_locator: Some("vulcan-ai/vulcan-codekit".to_string()),
    }
}

/// Build one deterministic uninstall result for prepared lifecycle tests.
/// 构造一个用于预备生命周期测试的确定性卸载结果。
///
/// The skill_removed parameter is copied into the returned result and controls its message.
/// skill_removed 参数会复制到返回结果中，并控制返回结果的消息。
///
/// Returns an uninstall result for the shared vulcan-codekit fixture.
/// 返回共享 vulcan-codekit 夹具使用的卸载结果。
fn test_uninstall_result(skill_removed: bool) -> SkillUninstallResult {
    SkillUninstallResult {
        skill_id: "vulcan-codekit".to_string(),
        skill_removed,
        sqlite_removed: false,
        lancedb_removed: false,
        sqlite_retained: false,
        lancedb_retained: false,
        message: if skill_removed {
            "skill package removed".to_string()
        } else {
            "skill package directory not found".to_string()
        },
    }
}

/// Verify uninstall finalization errors keep the primary failure unchanged when rollback succeeds.
/// 验证回滚成功时卸载收尾错误会保持主失败信息不变。
#[test]
fn uninstall_finalization_error_keeps_base_message_when_rollback_succeeds() {
    // Formatted uninstall finalization failure when rollback succeeds.
    // 回滚成功时格式化得到的卸载收尾失败信息。
    let message = format_uninstall_finalization_error(
        "Failed to finalize uninstall: commit failed".to_string(),
        Ok::<(), String>(()),
    );

    assert_eq!(message, "Failed to finalize uninstall: commit failed");
}

/// Verify uninstall finalization errors append rollback failures when rollback fails.
/// 验证回滚失败时卸载收尾错误会追加回滚失败诊断。
#[test]
fn uninstall_finalization_error_appends_rollback_failure() {
    // Formatted uninstall finalization failure when rollback also fails.
    // 回滚也失败时格式化得到的卸载收尾失败信息。
    let message = format_uninstall_finalization_error(
        "Failed to finalize uninstall: commit failed".to_string(),
        Err("backup restore failed".to_string()),
    );

    assert_eq!(
        message,
        "Failed to finalize uninstall: commit failed. rollback failed: backup restore failed"
    );
}

/// Verify a failed staged update publication restores the previous package into the canonical target.
/// 验证已暂存更新发布失败时会把旧版本包恢复到规范目标目录。
#[test]
fn staged_update_publish_failure_restores_previous_package() {
    // Isolated filesystem root for the deterministic publication-failure fixture.
    // 确定性发布失败夹具使用的隔离文件系统根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_update_publish_rollback_success_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        // Stale fixture cleanup is intentionally ignored before deterministic recreation.
        // 确定性重建前对陈旧夹具的清理结果有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    std::fs::create_dir_all(&temp_root).expect("temporary root should be created");
    // Missing staged directory that deterministically makes the primary publication rename fail.
    // 确定性触发主要发布重命名失败的缺失暂存目录。
    let staged_dir = temp_root.join("missing-staged");
    // Canonical target that must receive the restored previous package.
    // 必须接收已恢复旧版本包的规范目标目录。
    let target_dir = temp_root.join("installed");
    // Existing backup directory that makes the rollback rename succeed.
    // 使回滚重命名成功的现有备份目录。
    let backup_dir = temp_root.join("backup");
    std::fs::create_dir_all(&backup_dir).expect("backup directory should be created");
    std::fs::write(backup_dir.join("old.txt"), "old")
        .expect("previous package marker should be written");

    // Publication error returned after the previous package has been restored successfully.
    // 旧版本包成功恢复后返回的发布错误。
    let error = publish_staged_skill_update(&staged_dir, &target_dir, &backup_dir)
        .expect_err("missing staged directory should fail publication");

    assert!(error.contains("Failed to move updated skill"));
    assert!(!error.contains("rollback failed"));
    assert_eq!(
        std::fs::read_to_string(target_dir.join("old.txt"))
            .expect("restored previous package marker should be readable"),
        "old"
    );
    assert!(!backup_dir.exists());
    std::fs::remove_dir_all(&temp_root).expect("temporary root should be removed");
}

/// Verify a failed staged update rollback is reported together with uncertain disk state.
/// 验证已暂存更新回滚失败时会同时报告失败并标明磁盘状态不确定。
#[test]
fn staged_update_publish_failure_reports_rollback_failure() {
    // Isolated filesystem root for the deterministic double-failure fixture.
    // 确定性双重失败夹具使用的隔离文件系统根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_update_publish_rollback_failure_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        // Stale fixture cleanup is intentionally ignored before deterministic recreation.
        // 确定性重建前对陈旧夹具的清理结果有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    std::fs::create_dir_all(&temp_root).expect("temporary root should be created");
    // Missing staged directory that deterministically makes primary publication fail.
    // 确定性触发主要发布失败的缺失暂存目录。
    let staged_dir = temp_root.join("missing-staged");
    // Missing canonical target retained to prove no package was silently restored.
    // 保持缺失以证明没有静默恢复任何包的规范目标目录。
    let target_dir = temp_root.join("installed");
    // Missing backup directory that deterministically makes rollback fail too.
    // 确定性触发回滚也失败的缺失备份目录。
    let backup_dir = temp_root.join("missing-backup");

    // Combined failure that must preserve both the publication and rollback diagnostics.
    // 必须同时保留发布与回滚诊断的组合失败。
    let error = publish_staged_skill_update(&staged_dir, &target_dir, &backup_dir)
        .expect_err("missing staged and backup directories should fail publication and rollback");

    assert!(error.contains("Failed to move updated skill"));
    assert!(error.contains("rollback failed to restore"));
    assert!(error.contains("skill disk state is uncertain"));
    assert!(error.contains(&render_host_visible_path(&backup_dir)));
    assert!(!target_dir.exists());
    std::fs::remove_dir_all(&temp_root).expect("temporary root should be removed");
}

/// Verify that the staging-directory guard cleans temp roots on drop.
/// 验证暂存目录守卫会在析构时清理临时根目录。
#[test]
fn temp_dir_guard_removes_staging_root_on_drop() {
    let temp_root =
        std::env::temp_dir().join(format!("luaskills_temp_guard_test_{}", std::process::id()));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    std::fs::create_dir_all(&temp_root).expect("temp root should be created");
    {
        let _guard = TempDirGuard::new(temp_root.clone());
        std::fs::write(temp_root.join("staged.txt"), "staged")
            .expect("staged marker should be written");
    }
    assert!(!temp_root.exists());
}

/// Verify staging-directory cleanup reports a confirmed missing directory without probing first.
/// 验证暂存目录清理会报告已确认缺失的目录，而不先进行存在性探测。
#[test]
fn remove_staging_dir_reports_missing_directory() {
    // Temporary root that is intentionally absent before cleanup.
    // 清理前有意保持缺失状态的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_missing_staging_cleanup_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    // Missing staging directory reported as false after direct removal returns NotFound.
    // 直接删除返回 NotFound 后，缺失暂存目录被报告为 false。
    let removed = remove_staging_dir_if_present(&temp_root)
        .expect("missing staging directory cleanup should succeed");

    assert!(!removed);
}

/// Verify staging-directory cleanup reports invalid path cleanup errors explicitly.
/// 验证暂存目录清理会显式报告非法路径清理错误。
#[test]
fn remove_staging_dir_reports_invalid_path_errors() {
    // Staging directory path containing one embedded NUL that remove_dir_all cannot process.
    // 包含内嵌 NUL 且 remove_dir_all 无法处理的暂存目录路径。
    let invalid_path = std::path::PathBuf::from("invalid\0staging");

    // Error returned by the cleanup helper instead of being folded through an existence check.
    // 清理 helper 返回的错误，而不是通过存在性检查折叠。
    let error = remove_staging_dir_if_present(&invalid_path)
        .expect_err("invalid staging directory cleanup should fail");

    assert!(
        error.contains("Failed to remove staging directory"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
}

/// Verify staging temp-root probes reject files before cleanup or extraction.
/// 验证暂存临时根探测会在清理或解压前拒绝普通文件。
#[test]
fn staging_temp_root_rejects_file_path() {
    // Temporary root that isolates the file-backed staging temp fixture.
    // 隔离普通文件占位暂存临时根夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_file_staging_temp_root_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Staging temp path that mirrors install/update lifecycle temp-root naming.
    // 模拟安装/更新生命周期临时根命名的暂存临时路径。
    let staging_temp_root = temp_root
        .join("state")
        .join("install_tmp")
        .join("vulcan-codekit-1");
    std::fs::create_dir_all(
        staging_temp_root
            .parent()
            .expect("staging temp path should have a parent"),
    )
    .expect("staging temp parent should be created");
    std::fs::write(&staging_temp_root, "not a staging directory")
        .expect("file staging temp root should be written");

    // Error returned before a file can be treated as a stale staging directory.
    // 在普通文件被当作陈旧暂存目录之前返回的错误。
    let error = staging_temp_root_is_directory(&staging_temp_root)
        .expect_err("file staging temp root should fail");

    assert!(
        error.contains("Skill staging temp root is not a directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&staging_temp_root)),
        "unexpected error: {}",
        error
    );
    assert!(
        staging_temp_root.is_file(),
        "file staging temp root should remain in place after rejection"
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify effective skill collection reports directory-open errors instead of treating invalid roots as empty.
/// 验证生效技能收集会报告目录打开错误，而不是把非法根目录当作空目录。
#[test]
fn collect_effective_skill_instances_rejects_skill_root_probe_errors() {
    // Runtime skill root containing one embedded NUL that the filesystem cannot open.
    // 包含内嵌 NUL 且文件系统无法打开的运行时技能根。
    let invalid_root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: std::path::PathBuf::from("invalid\0skills"),
    };

    // Error returned by the single real directory-open operation.
    // 单次真实目录打开操作返回的错误。
    let error = collect_effective_skill_instances_from_roots(&[invalid_root])
        .expect_err("invalid skill root metadata probe should fail");

    assert!(
        error.contains("Failed to read skill root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
}

/// Verify effective skill collection rejects skill roots that exist as non-directory files.
/// 验证生效技能收集会拒绝以非目录文件形式存在的技能根。
#[test]
fn collect_effective_skill_instances_rejects_file_skill_root() {
    // Temporary root that isolates the file skill-root fixture.
    // 隔离文件型技能根夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_file_skill_root_test_{}",
        std::process::id()
    ));
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = std::fs::remove_dir_all(&temp_root);
    std::fs::create_dir_all(&temp_root).expect("create file skill-root test dir");
    // File deliberately occupying the configured skill-root path.
    // 故意占用已配置技能根路径的文件。
    let file_root = temp_root.join("skills");
    std::fs::write(&file_root, "not a directory\n").expect("write file skill root");
    // Runtime skill root that points at the file fixture.
    // 指向文件夹具的运行时技能根。
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: file_root.clone(),
    };

    // Error returned directly by opening the file path as a directory.
    // 直接把文件路径作为目录打开时返回的错误。
    let error = collect_effective_skill_instances_from_roots(&[root])
        .expect_err("file skill root should fail");

    assert!(
        error.contains("Failed to read skill root"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&file_root)),
        "unexpected error: {}",
        error
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify missing skill manifests still default to enabled.
/// 验证缺失技能清单仍然默认启用。
#[test]
fn is_skill_manifest_enabled_defaults_missing_manifest_to_enabled() {
    // Temporary root that isolates the missing-manifest enable probe.
    // 隔离缺失清单启用状态探针的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_missing_manifest_enabled_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Existing skill directory intentionally missing skill.yaml.
    // 有意缺失 skill.yaml 的已存在技能目录。
    let skill_dir = temp_root.join("skills").join("vulcan-codekit");
    std::fs::create_dir_all(&skill_dir).expect("create missing manifest skill dir");

    assert!(
        is_skill_manifest_enabled(&skill_dir).expect("missing manifest probe should succeed"),
        "missing manifest should default to enabled"
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify skill manifest enable probes report filesystem probe errors.
/// 验证技能清单启用状态探针会报告文件系统探测错误。
#[test]
fn is_skill_manifest_enabled_rejects_manifest_probe_errors() {
    // Skill directory containing one embedded NUL that makes skill.yaml impossible to inspect.
    // 包含内嵌 NUL 的技能目录，使 skill.yaml 无法被探测。
    let invalid_skill_dir = std::path::PathBuf::from("invalid\0skill");

    // Error returned before the invalid manifest can behave like a missing enabled-by-default manifest.
    // 在非法清单表现得像缺失且默认启用的清单之前返回的错误。
    let error = is_skill_manifest_enabled(&invalid_skill_dir)
        .expect_err("invalid skill manifest probe should fail");

    assert!(
        error.contains("Failed to inspect skill manifest"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("skill.yaml"), "unexpected error: {}", error);
}

/// Verify skill manifest enable probes reject directory manifests before reading YAML.
/// 验证技能清单启用状态探针会在读取 YAML 前拒绝目录型清单。
#[test]
fn is_skill_manifest_enabled_rejects_directory_manifest() {
    // Temporary root that isolates the directory-manifest enable probe.
    // 隔离目录型清单启用状态探针的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_directory_manifest_enabled_test_{}",
        std::process::id()
    ));
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = std::fs::remove_dir_all(&temp_root);
    // Existing skill directory whose skill.yaml path is deliberately a directory.
    // skill.yaml 路径被有意创建为目录的已存在技能目录。
    let skill_dir = temp_root.join("skills").join("vulcan-codekit");
    let manifest_dir = skill_dir.join("skill.yaml");
    std::fs::create_dir_all(&manifest_dir).expect("create directory skill manifest");

    // Error returned before the directory manifest can be treated as readable YAML.
    // 在目录型清单被当作可读 YAML 之前返回的错误。
    let error =
        is_skill_manifest_enabled(&skill_dir).expect_err("directory skill manifest should fail");

    assert!(
        error.contains("Skill manifest is not a file"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&manifest_dir)),
        "unexpected error: {}",
        error
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify that disable/enable operations persist and clear state markers correctly.
/// 验证停用/启用操作会正确持久化并清理状态标记。
#[test]
fn skill_manager_persists_disabled_state() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_skill_manager_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: skill_root,
            },
        )
    });

    assert!(manager.is_skill_enabled("vulcan-codekit").unwrap());
    manager
        .disable_skill("vulcan-codekit", Some("manual test"))
        .expect("disable should succeed");
    assert!(!manager.is_skill_enabled("vulcan-codekit").unwrap());
    assert_eq!(
        manager
            .disabled_record("vulcan-codekit")
            .unwrap()
            .expect("record should exist")
            .reason
            .as_deref(),
        Some("manual test")
    );

    manager
        .enable_skill("vulcan-codekit")
        .expect("enable should succeed");
    assert!(manager.is_skill_enabled("vulcan-codekit").unwrap());

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify missing disabled-state records still return none.
/// 验证缺失停用状态记录仍返回 none。
#[test]
fn disabled_record_returns_none_when_missing() {
    // Temporary root that isolates the missing disabled-record probe.
    // 隔离缺失停用状态记录探针的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_missing_disabled_record_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill manager whose disabled record path is confirmed missing.
    // 停用状态记录路径确认缺失的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });

    assert!(
        manager
            .disabled_record("vulcan-codekit")
            .expect("missing disabled record probe should succeed")
            .is_none(),
        "missing disabled record should return none"
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify disabled-state record reads report filesystem probe errors.
/// 验证读取停用状态记录会报告文件系统探测错误。
#[test]
fn disabled_record_rejects_probe_errors() {
    // Lifecycle root containing one embedded NUL that makes disabled record paths impossible to inspect.
    // 包含内嵌 NUL 的生命周期根目录，使停用状态记录路径无法被探测。
    let invalid_lifecycle_root = std::path::PathBuf::from("invalid\0state");
    // Skill manager whose disabled record path derives from the invalid lifecycle root.
    // 停用状态记录路径派生自非法生命周期根目录的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &invalid_lifecycle_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: std::path::PathBuf::from("skills"),
            },
        )
    });

    // Error returned before the invalid disabled record can behave like a missing record.
    // 在非法停用状态记录表现得像缺失记录之前返回的错误。
    let error = manager
        .disabled_record("vulcan-codekit")
        .expect_err("invalid disabled record probe should fail");

    assert!(
        error.contains("Failed to inspect disabled record"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("vulcan-codekit.json"),
        "unexpected error: {}",
        error
    );
}

/// Verify disabled-state record reads reject directory records before JSON parsing.
/// 验证读取停用状态记录会在 JSON 解析前拒绝目录型记录。
#[test]
fn disabled_record_rejects_directory_record() {
    // Temporary root that isolates the directory disabled-record fixture.
    // 隔离目录型停用状态记录夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_directory_disabled_record_test_{}",
        std::process::id()
    ));
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = std::fs::remove_dir_all(&temp_root);
    // Skill manager whose disabled record path will be occupied by a directory.
    // 停用状态记录路径将被目录占用的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });
    // Directory deliberately occupying the JSON disabled-record path.
    // 故意占用 JSON 停用状态记录路径的目录。
    let record_dir = manager.disabled_record_path("vulcan-codekit");
    std::fs::create_dir_all(&record_dir).expect("create directory disabled record");

    // Error returned before the directory record can be treated as readable JSON.
    // 在目录型记录被当作可读 JSON 之前返回的错误。
    let error = manager
        .disabled_record("vulcan-codekit")
        .expect_err("directory disabled record should fail");

    assert!(
        error.contains("Disabled record is not a file"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&record_dir)),
        "unexpected error: {}",
        error
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify missing managed install records still return none.
/// 验证缺失受管安装记录仍返回 none。
#[test]
fn install_record_returns_none_when_missing() {
    // Temporary root that isolates the missing install-record probe.
    // 隔离缺失安装记录探针的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_missing_install_record_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill manager whose install record path is confirmed missing.
    // 安装记录路径确认缺失的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });

    assert!(
        manager
            .install_record("vulcan-codekit")
            .expect("missing install record probe should succeed")
            .is_none(),
        "missing install record should return none"
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify managed install-record reads report filesystem probe errors.
/// 验证读取受管安装记录会报告文件系统探测错误。
#[test]
fn install_record_rejects_probe_errors() {
    // Lifecycle root containing one embedded NUL that makes install record paths impossible to inspect.
    // 包含内嵌 NUL 的生命周期根目录，使安装记录路径无法被探测。
    let invalid_lifecycle_root = std::path::PathBuf::from("invalid\0state");
    // Skill manager whose install record path derives from the invalid lifecycle root.
    // 安装记录路径派生自非法生命周期根目录的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &invalid_lifecycle_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: std::path::PathBuf::from("skills"),
            },
        )
    });

    // Error returned before the invalid install record can behave like a missing record.
    // 在非法安装记录表现得像缺失记录之前返回的错误。
    let error = manager
        .install_record("vulcan-codekit")
        .expect_err("invalid install record probe should fail");

    assert!(
        error.contains("Failed to inspect install record"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("vulcan-codekit.yaml"),
        "unexpected error: {}",
        error
    );
}

/// Verify managed install-record reads reject directory records before YAML parsing.
/// 验证读取受管安装记录会在 YAML 解析前拒绝目录型记录。
#[test]
fn install_record_rejects_directory_record() {
    // Temporary root that isolates the directory install-record fixture.
    // 隔离目录型安装记录夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_directory_install_record_test_{}",
        std::process::id()
    ));
    // Best-effort cleanup for stale state from an earlier run of this same test.
    // 清理同一测试早先运行可能留下的残留状态。
    let _ = std::fs::remove_dir_all(&temp_root);
    // Skill manager whose install record path will be occupied by a directory.
    // 安装记录路径将被目录占用的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });
    // Directory deliberately occupying the YAML install-record path.
    // 故意占用 YAML 安装记录路径的目录。
    let record_dir = manager.install_record_path("vulcan-codekit");
    std::fs::create_dir_all(&record_dir).expect("create directory install record");

    // Error returned before the directory record can be treated as readable YAML.
    // 在目录型记录被当作可读 YAML 之前返回的错误。
    let error = manager
        .install_record("vulcan-codekit")
        .expect_err("directory install record should fail");

    assert!(
        error.contains("Install record is not a file"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&record_dir)),
        "unexpected error: {}",
        error
    );

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify uninstall keeps the not-removed result when a skill package directory is missing.
/// 验证技能包目录缺失时卸载仍保持未删除结果。
#[test]
fn uninstall_skill_reports_not_removed_when_package_dir_is_missing() {
    // Temporary root that isolates the missing package uninstall fixture.
    // 隔离缺失包目录卸载夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_missing_uninstall_package_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill manager whose configured skill root contains no target package directory.
    // 已配置技能根中不存在目标包目录的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });

    let result = manager
        .uninstall_skill("vulcan-codekit")
        .expect("missing package uninstall should succeed");

    assert!(!result.skill_removed);
    assert_eq!(result.message, "skill package directory not found");
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify the path-aware uninstall API rejects a directory outside the manager-owned skill root.
/// 验证带路径卸载 API 会拒绝管理器所拥有技能根之外的目录。
#[test]
fn uninstall_skill_at_path_rejects_unmanaged_directory_without_side_effects() {
    // Temporary root that isolates the manager root and the protected external directory.
    // 隔离管理器根目录与受保护外部目录的临时根。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_unmanaged_uninstall_path_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill root exclusively owned by the manager under test.
    // 被测管理器唯一拥有的技能根目录。
    let skill_root = temp_root.join("skills");
    // External directory that must never be staged or deleted by this manager.
    // 绝不能被当前管理器暂存或删除的外部目录。
    let protected_dir = temp_root.join("protected");
    // Marker proving that rejection happens without moving or deleting external contents.
    // 用于证明拒绝过程不会移动或删除外部内容的标记文件。
    let marker_path = protected_dir.join("keep.txt");
    std::fs::create_dir_all(&protected_dir).expect("protected directory should be created");
    std::fs::write(&marker_path, "keep").expect("protected marker should be written");
    // Manager whose valid uninstall target is skills/vulcan-codekit, not protected/.
    // 合法卸载目标是 skills/vulcan-codekit 而非 protected/ 的管理器。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: skill_root,
        },
    ));

    // Error returned before lifecycle state creation or any external-directory mutation.
    // 在创建生命周期状态或修改外部目录之前返回的错误。
    let error = manager
        .prepare_uninstall_skill_at_path_in_plane(
            SkillOperationPlane::Skills,
            "vulcan-codekit",
            &protected_dir,
        )
        .expect_err("unmanaged uninstall directory should be rejected");

    assert!(
        error.contains("unmanaged directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        marker_path.is_file(),
        "protected marker should remain after rejection"
    );
    assert!(
        !temp_root.join("state").exists(),
        "lifecycle state should not be created before target ownership validation"
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify uninstall rejects a file where the skill package directory must be.
/// 验证卸载会拒绝占据技能包目录位置的普通文件。
#[test]
fn uninstall_skill_rejects_file_package_path() {
    // Temporary root that isolates the file-backed package path fixture.
    // 隔离普通文件占位包路径夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_file_uninstall_package_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill root containing a plain file at the package path that must be a directory.
    // 技能根目录中在必须为目录的包路径位置放置一个普通文件。
    let skill_root = temp_root.join("skills");
    std::fs::create_dir_all(&skill_root).expect("skill root should be created");
    // Package path that the uninstall lifecycle would otherwise move into a backup location.
    // 卸载生命周期原本可能会移动到备份位置的包路径。
    let package_path = skill_root.join("vulcan-codekit");
    std::fs::write(&package_path, "not a skill directory")
        .expect("file package path should be written");
    // Skill manager configured to uninstall from the file-backed package path.
    // 配置为从普通文件占位包路径执行卸载的技能管理器。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: skill_root,
        },
    ));

    // Error returned before the file can be treated as a removable package directory.
    // 在普通文件被当作可移除包目录之前返回的错误。
    let error = manager
        .uninstall_skill("vulcan-codekit")
        .expect_err("file package path should fail uninstall");

    assert!(
        error.contains("Skill package path is not a directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains(&render_host_visible_path(&package_path)),
        "unexpected error: {}",
        error
    );
    assert!(
        package_path.is_file(),
        "file package path should remain in place after rejection"
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify uninstall reports skill package directory probe errors.
/// 验证卸载会报告技能包目录探测错误。
#[test]
fn uninstall_skill_rejects_package_dir_probe_errors() {
    // Temporary state root that keeps manager lifecycle state valid.
    // 保持管理器生命周期状态有效的临时状态根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_uninstall_package_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill root containing one embedded NUL that makes the target package path impossible to inspect.
    // 包含内嵌 NUL 的技能根目录，使目标包路径无法被探测。
    let invalid_skill_root = std::path::PathBuf::from("invalid\0skills");
    // Skill manager whose state root is valid but package root is invalid.
    // 状态根有效但包根非法的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: invalid_skill_root,
            },
        )
    });

    // Error returned before the invalid package directory can behave like a missing package.
    // 在非法包目录表现得像缺失包目录之前返回的错误。
    let error = manager
        .uninstall_skill("vulcan-codekit")
        .expect_err("invalid uninstall package directory probe should fail");

    assert!(
        error.contains("Failed to inspect skill package directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("vulcan-codekit"),
        "unexpected error: {}",
        error
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify that disabled-record parse errors render paths through the host-visible formatter.
/// 验证停用状态记录解析错误会通过宿主可见路径渲染器输出路径。
#[test]
fn disabled_record_parse_error_uses_host_visible_path() {
    // Temporary root that isolates the disabled-record parse fixture.
    // 隔离停用状态记录解析夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_disabled_record_parse_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        // Stale fixture cleanup result is intentionally ignored before recreation.
        // 重建前对陈旧夹具的清理结果有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill root configured for the test manager.
    // 测试管理器配置使用的技能根目录。
    let skill_root = temp_root.join("skills");
    // Skill manager whose lifecycle state lives under the temporary root.
    // 生命周期状态位于临时根目录下的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: skill_root,
            },
        )
    });
    manager
        .ensure_state_layout()
        .expect("state layout should be created");
    // Disabled record path that mirrors SkillManager's lifecycle layout.
    // 与 SkillManager 生命周期布局一致的停用状态记录路径。
    let disabled_record_path = temp_root
        .join("state")
        .join("skills")
        .join("disabled")
        .join("vulcan-codekit.json");
    std::fs::write(&disabled_record_path, "{not-json")
        .expect("invalid disabled record should be written");

    // Error returned by the real disabled-record reader.
    // 真实停用状态记录读取器返回的错误。
    let error = manager
        .disabled_record("vulcan-codekit")
        .expect_err("invalid disabled record should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to parse {}:",
        render_host_visible_path(&disabled_record_path)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
    // 对临时测试产物的清理结果按最佳努力原则有意忽略。
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify public URL installs are rejected by the managed source policy.
/// 验证公开 URL 安装会被受管来源策略拒绝。
#[test]
fn public_url_install_is_rejected_by_source_policy() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_public_url_policy_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    std::fs::create_dir_all(&skill_root).expect("skill root should be created");
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: skill_root,
        },
    ));
    let error = manager
        .prepare_install_skill(
            SkillOperationPlane::Skills,
            &[RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            }],
            &SkillInstallRequest {
                skill_id: Some("demo-skill".to_string()),
                source: Some("https://example.com/demo.yaml".to_string()),
                source_type: SkillInstallSourceType::Url,
            },
        )
        .expect_err("public URL install should be rejected");
    assert!(error.contains("public URL skill install is disabled"));
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify official Hub requests can derive the skill id from the Hub locator.
/// 验证官方 Hub 请求可以从 Hub 定位值派生 skill id。
#[test]
fn official_hub_request_derives_skill_id_from_source() {
    let skill_id = resolve_requested_skill_id(&SkillInstallRequest {
        skill_id: None,
        source: Some("skill-search".to_string()),
        source_type: SkillInstallSourceType::OfficialHub,
    })
    .expect("official Hub skill id should derive from source");
    assert_eq!(skill_id, "skill-search");
}

/// Verify GitHub requests derive the skill id from one normalized repository locator.
/// 验证 GitHub 请求会从单个规范化仓库定位值派生技能标识符。
#[test]
fn github_request_derives_skill_id_from_repository_source() {
    // Skill id derived from the repository segment of one GitHub URL.
    // 从单个 GitHub URL 的仓库段派生出的技能标识符。
    let skill_id = resolve_requested_skill_id(&SkillInstallRequest {
        skill_id: None,
        source: Some("https://github.com/vulcan-ai/vulcan-codekit".to_string()),
        source_type: SkillInstallSourceType::Github,
    })
    .expect("GitHub skill id should derive from repository source");

    assert_eq!(skill_id, "vulcan-codekit");
}

/// Verify GitHub repository normalization accepts exactly owner and repo segments.
/// 验证 GitHub 仓库规范化只接受准确的 owner 与 repo 两段。
#[test]
fn github_repo_locator_normalization_requires_exact_owner_repo_segments() {
    assert_eq!(
        normalize_github_repo_locator(" https://github.com/vulcan-ai/vulcan-codekit/ ")
            .expect("GitHub URL locator should normalize"),
        "vulcan-ai/vulcan-codekit"
    );
    assert_eq!(
        normalize_github_repo_locator("vulcan-ai/vulcan-codekit")
            .expect("owner/repo locator should normalize"),
        "vulcan-ai/vulcan-codekit"
    );

    // Error returned when the owner segment is missing.
    // owner 段缺失时返回的错误。
    let missing_owner =
        normalize_github_repo_locator("/vulcan-codekit").expect_err("missing owner should fail");
    assert!(missing_owner.contains("owner/repo form"));

    // Error returned when the repository segment is missing.
    // repo 段缺失时返回的错误。
    let missing_repo =
        normalize_github_repo_locator("vulcan-ai/").expect_err("missing repo should fail");
    assert!(missing_repo.contains("owner/repo form"));

    // Error returned when extra path segments are supplied.
    // 提供额外路径段时返回的错误。
    let extra_path = normalize_github_repo_locator("vulcan-ai/vulcan-codekit/tree/main")
        .expect_err("extra GitHub path segments should fail");
    assert!(extra_path.contains("owner/repo form"));
}

/// Verify GitHub repository skill-id derivation rejects non-repository locators.
/// 验证 GitHub 仓库技能标识符派生会拒绝非仓库定位值。
#[test]
fn github_repo_skill_id_rejects_locator_without_repo_segment() {
    // Error returned when the locator does not contain the required owner/repo separator.
    // 定位值不包含必需的 owner/repo 分隔符时返回的错误。
    let error =
        github_repo_skill_id("vulcan-codekit").expect_err("repo-only GitHub locator should fail");

    assert!(error.contains("owner/repo form"));
}

/// Verify official Hub install rejects mismatched explicit ids before remote resolution.
/// 验证官方 Hub 安装会在远程解析前拒绝显式 id 不一致的请求。
#[test]
fn official_hub_install_rejects_mismatched_source_skill_id() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_official_hub_mismatch_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    std::fs::create_dir_all(&skill_root).expect("skill root should be created");
    let manager = SkillManager::new(SkillManagerConfig {
        official_skill_hub_base_url: Some("https://hub.example.invalid".to_string()),
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: skill_root,
            },
        )
    });
    let error = manager
        .prepare_install_skill(
            SkillOperationPlane::Skills,
            &[RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            }],
            &SkillInstallRequest {
                skill_id: Some("expected-skill".to_string()),
                source: Some("other-skill".to_string()),
                source_type: SkillInstallSourceType::OfficialHub,
            },
        )
        .expect_err("mismatched official Hub locator should fail before network");
    assert!(error.contains("does not match requested skill_id"));
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify private manifest allowlists accept exact prefixes and reject sibling prefixes.
/// 验证私有 manifest allowlist 接受精确前缀并拒绝相邻伪前缀。
#[test]
fn private_source_allowlist_uses_directory_prefix_matching() {
    let allowlist = vec!["https://internal.example.com/luaskills/manifests".to_string()];
    assert!(is_allowed_private_source_url(
        "https://internal.example.com/luaskills/manifests/demo.json",
        &allowlist,
    ));
    assert!(!is_allowed_private_source_url(
        "https://internal.example.com/luaskills/manifests-extra/demo.json",
        &allowlist,
    ));
}

/// Verify that install/update entrypoints return strict structured states before networking succeeds.
/// 验证 install/update 入口在真正下载前会返回严格的结构化状态。
#[test]
fn install_update_entrypoints_return_strict_structured_results() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_install_update_test_{}",
        std::process::id()
    ));
    let skill_root = temp_root.join("skills");
    let skill_roots = vec![RuntimeSkillRoot {
        name: "USER".to_string(),
        skills_dir: skill_root.clone(),
    }];
    let _ = std::fs::create_dir_all(&skill_root);
    let manager = SkillManager::new(test_manager_config(&temp_root, skill_roots[0].clone()));

    let install_result = manager
        .prepare_install_skill(
            SkillOperationPlane::Skills,
            &skill_roots,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("install without source should fail strictly");
    assert!(install_result.contains("github install requires source repository"));

    let _ = std::fs::create_dir_all(skill_root.join("vulcan-codekit"));
    let update_result = manager
        .prepare_update_skill(
            SkillOperationPlane::Skills,
            &skill_roots,
            &SkillInstallRequest {
                skill_id: Some("vulcan-codekit".to_string()),
                source: None,
                source_type: SkillInstallSourceType::Github,
            },
        )
        .expect_err("update without install record should fail strictly");
    assert!(update_result.contains("is not managed by the install workflow"));

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify install staging reports temp-root probe errors before archive extraction.
/// 验证安装暂存会在归档解压前报告临时根探测错误。
#[test]
fn stage_skill_install_rejects_temp_root_probe_errors() {
    // Temporary root that keeps unrelated skill paths valid for the direct staging call.
    // 为直接暂存调用保持无关技能路径有效的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_install_stage_temp_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Lifecycle root containing one embedded NUL that makes the install temp root impossible to inspect.
    // 包含内嵌 NUL 的生命周期根目录，使安装临时根无法被探测。
    let invalid_lifecycle_root = std::path::PathBuf::from("invalid\0state");
    // Skill manager with a valid skill root and invalid lifecycle temp root.
    // 技能根有效但生命周期临时根无效的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        lifecycle_root: invalid_lifecycle_root,
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });

    // Error returned before the unused archive path can be opened or extracted.
    // 在未使用的归档路径被打开或解压前返回的错误。
    let error = manager
        .stage_skill_install_from_archive(
            "vulcan-codekit",
            &temp_root.join("unused.zip"),
            "0.1.0",
            test_install_source_record(),
            "install message".to_string(),
        )
        .expect_err("invalid install temp root probe should fail");

    assert!(
        error.contains("Failed to inspect skill staging temp root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("install_tmp"), "unexpected error: {}", error);
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify update staging reports temp-root probe errors before archive extraction.
/// 验证更新暂存会在归档解压前报告临时根探测错误。
#[test]
fn stage_skill_update_rejects_temp_root_probe_errors() {
    // Temporary root that keeps unrelated skill paths valid for the direct staging call.
    // 为直接暂存调用保持无关技能路径有效的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_update_stage_temp_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Lifecycle root containing one embedded NUL that makes the update temp root impossible to inspect.
    // 包含内嵌 NUL 的生命周期根目录，使更新临时根无法被探测。
    let invalid_lifecycle_root = std::path::PathBuf::from("invalid\0state");
    // Skill manager with a valid skill root and invalid lifecycle temp root.
    // 技能根有效但生命周期临时根无效的技能管理器。
    let manager = SkillManager::new(SkillManagerConfig {
        lifecycle_root: invalid_lifecycle_root,
        ..test_manager_config(
            &temp_root,
            RuntimeSkillRoot {
                name: "USER".to_string(),
                skills_dir: temp_root.join("skills"),
            },
        )
    });

    // Error returned before the unused archive path can be opened or extracted.
    // 在未使用的归档路径被打开或解压前返回的错误。
    let error = manager
        .stage_skill_update_from_archive(
            "vulcan-codekit",
            &temp_root.join("unused.zip"),
            "0.2.0",
            test_installed_record("0.1.0"),
            test_install_source_record(),
        )
        .expect_err("invalid update temp root probe should fail");

    assert!(
        error.contains("Failed to inspect skill staging temp root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("update_tmp"), "unexpected error: {}", error);
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify staged install rollback reports target directory probe errors.
/// 验证暂存安装回滚会报告目标目录探测错误。
#[test]
fn rollback_staged_install_rejects_target_dir_probe_errors() {
    // Temporary root that isolates the rollback probe fixture state.
    // 隔离回滚探测夹具状态的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_install_rollback_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill manager with valid lifecycle state and an invalid prepared target directory.
    // 生命周期状态有效但预备目标目录无效的技能管理器。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: temp_root.join("skills"),
        },
    ));
    // Prepared install whose staged target contains an embedded NUL and cannot be inspected.
    // 暂存目标包含内嵌 NUL 且无法被探测的预备安装。
    let prepared = PreparedSkillApply::Install(PreparedSkillInstall {
        result: test_apply_result("installed"),
        target_dir: std::path::PathBuf::from("invalid\0target"),
        install_record: test_installed_record("0.1.0"),
    });

    // Error returned before the invalid target can behave like a missing rollback directory.
    // 在无效目标表现得像缺失回滚目录之前返回的错误。
    let error = manager
        .rollback_prepared_skill_apply(&prepared)
        .expect_err("invalid staged install target probe should fail");

    assert!(
        error.contains("Failed to inspect skill package directory"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify staged update rollback probes backup paths before deleting the staged target.
/// 验证暂存更新回滚会先探测备份路径，再删除暂存目标。
#[test]
fn rollback_staged_update_rejects_backup_probe_errors_before_removing_target() {
    // Temporary root that isolates the update rollback fixture.
    // 隔离更新回滚夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_update_rollback_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Valid staged target directory that must survive a backup probe failure.
    // 必须在备份探测失败后仍然保留的有效暂存目标目录。
    let target_dir = temp_root.join("skills").join("vulcan-codekit");
    std::fs::create_dir_all(&target_dir).expect("staged target should be created");
    std::fs::write(target_dir.join("marker.txt"), "staged")
        .expect("staged marker should be written");
    // Skill manager with a valid lifecycle root for the rollback operation.
    // 回滚操作使用有效生命周期根的技能管理器。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: temp_root.join("skills"),
        },
    ));
    // Prepared update whose backup path contains an embedded NUL and cannot be inspected.
    // 备份路径包含内嵌 NUL 且无法被探测的预备更新。
    let prepared = PreparedSkillApply::Update(PreparedSkillUpdate {
        result: test_apply_result("updated"),
        target_dir: target_dir.clone(),
        backup_dir: std::path::PathBuf::from("invalid\0backup"),
        install_record: test_installed_record("0.2.0"),
        previous_install_record: test_installed_record("0.1.0"),
    });

    // Error returned before rollback removes the valid staged target directory.
    // 在回滚删除有效暂存目标目录之前返回的错误。
    let error = manager
        .rollback_prepared_skill_apply(&prepared)
        .expect_err("invalid update backup probe should fail");

    assert!(
        error.contains("Failed to inspect skill package directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        target_dir.join("marker.txt").exists(),
        "staged target should remain when backup probe fails"
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify staged update commit restores the previous install record when backup probing fails.
/// 验证暂存更新提交在备份探测失败时会恢复旧安装记录。
#[test]
fn commit_staged_update_restores_previous_record_on_backup_probe_error() {
    // Temporary root that isolates the update commit fixture.
    // 隔离更新提交夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_update_commit_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Skill manager with valid lifecycle state so record restoration can be observed.
    // 生命周期状态有效的技能管理器，便于观察记录恢复结果。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: temp_root.join("skills"),
        },
    ));
    // Previous install record expected after the failed commit restores state.
    // 失败提交恢复状态后预期保留的旧安装记录。
    let previous_record = test_installed_record("0.1.0");
    // Prepared update whose backup path cannot be inspected during commit cleanup.
    // 提交清理期间备份路径无法被探测的预备更新。
    let prepared = PreparedSkillApply::Update(PreparedSkillUpdate {
        result: test_apply_result("updated"),
        target_dir: temp_root.join("skills").join("vulcan-codekit"),
        backup_dir: std::path::PathBuf::from("invalid\0backup"),
        install_record: test_installed_record("0.2.0"),
        previous_install_record: previous_record.clone(),
    });

    // Error returned after the new record write is compensated by restoring the previous record.
    // 新记录写入后通过恢复旧记录完成补偿，然后返回的错误。
    let error = manager
        .commit_prepared_skill_apply(&prepared)
        .expect_err("invalid update backup probe should fail during commit");

    assert!(
        error.contains("Failed to inspect skill package directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        error.contains("previous install record was restored"),
        "unexpected error: {}",
        error
    );
    assert_eq!(
        manager
            .install_record("vulcan-codekit")
            .expect("restored install record should be readable")
            .expect("restored install record should exist"),
        previous_record
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify staged uninstall rollback reports target directory probe errors.
/// 验证暂存卸载回滚会报告目标目录探测错误。
#[test]
fn rollback_staged_uninstall_rejects_target_dir_probe_errors() {
    // Temporary root that isolates the uninstall rollback target probe fixture.
    // 隔离卸载回滚目标探测夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_uninstall_rollback_target_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Valid backup directory proves the failure comes from the invalid target path.
    // 有效备份目录证明失败来自无效目标路径。
    let backup_dir = temp_root
        .join("state")
        .join("uninstall_backup")
        .join("backup");
    std::fs::create_dir_all(&backup_dir).expect("backup directory should be created");
    // Skill manager with valid lifecycle state for rollback record restoration.
    // 回滚记录恢复使用有效生命周期状态的技能管理器。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: temp_root.join("skills"),
        },
    ));
    // Prepared uninstall whose target path contains an embedded NUL and cannot be inspected.
    // 目标路径包含内嵌 NUL 且无法被探测的预备卸载。
    let prepared = PreparedSkillUninstall {
        result: test_uninstall_result(true),
        target_dir: std::path::PathBuf::from("invalid\0target"),
        backup_dir: Some(backup_dir),
        previous_disabled_record: None,
        previous_install_record: None,
    };

    // Error returned before the invalid target can behave like a missing rollback target.
    // 在无效目标表现得像缺失回滚目标之前返回的错误。
    let error = manager
        .rollback_prepared_skill_uninstall(&prepared)
        .expect_err("invalid staged uninstall target probe should fail");

    assert!(
        error.contains("Failed to inspect skill package directory"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify staged uninstall rollback probes backup paths before deleting the staged target.
/// 验证暂存卸载回滚会先探测备份路径，再删除暂存目标。
#[test]
fn rollback_staged_uninstall_rejects_backup_probe_errors_before_removing_target() {
    // Temporary root that isolates the uninstall rollback backup probe fixture.
    // 隔离卸载回滚备份探测夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_uninstall_rollback_backup_probe_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    // Valid staged target directory that must survive a backup probe failure.
    // 必须在备份探测失败后仍然保留的有效暂存目标目录。
    let target_dir = temp_root.join("skills").join("vulcan-codekit");
    std::fs::create_dir_all(&target_dir).expect("staged target should be created");
    std::fs::write(target_dir.join("marker.txt"), "staged")
        .expect("staged marker should be written");
    // Skill manager with valid lifecycle state for rollback record restoration.
    // 回滚记录恢复使用有效生命周期状态的技能管理器。
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: temp_root.join("skills"),
        },
    ));
    // Prepared uninstall whose backup path contains an embedded NUL and cannot be inspected.
    // 备份路径包含内嵌 NUL 且无法被探测的预备卸载。
    let prepared = PreparedSkillUninstall {
        result: test_uninstall_result(true),
        target_dir: target_dir.clone(),
        backup_dir: Some(std::path::PathBuf::from("invalid\0backup")),
        previous_disabled_record: None,
        previous_install_record: None,
    };

    // Error returned before rollback removes the valid staged target directory.
    // 在回滚删除有效暂存目标目录之前返回的错误。
    let error = manager
        .rollback_prepared_skill_uninstall(&prepared)
        .expect_err("invalid uninstall backup probe should fail");

    assert!(
        error.contains("Failed to inspect skill package directory"),
        "unexpected error: {}",
        error
    );
    assert!(
        target_dir.join("marker.txt").exists(),
        "staged target should remain when backup probe fails"
    );
    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify that uninstall removes the skill directory but keeps database flags unset by default.
/// 验证卸载会删除技能目录，同时默认不声明数据库已删除。
#[test]
fn uninstall_returns_safe_default_database_flags() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_uninstall_result_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let skill_root = temp_root.join("skills");
    let manager = SkillManager::new(test_manager_config(
        &temp_root,
        RuntimeSkillRoot {
            name: "USER".to_string(),
            skills_dir: skill_root.clone(),
        },
    ));
    let _ = std::fs::create_dir_all(skill_root.join("vulcan-codekit"));

    let result = manager
        .uninstall_skill("vulcan-codekit")
        .expect("uninstall should succeed");
    assert!(result.skill_removed);
    assert!(!result.sqlite_removed);
    assert!(!result.lancedb_removed);
    assert!(!skill_root.join("vulcan-codekit").exists());

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify that PROJECT roots can contribute standalone skills without shadowing ROOT skills.
/// 验证 PROJECT 根目录可以独立提供技能，但不能覆盖 ROOT 技能。
#[test]
fn collect_effective_skill_instances_keeps_root_priority_over_project() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_collect_effective_instances_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let base_dir = temp_root.join("base");
    let override_dir = temp_root.join("override");
    let _ = std::fs::create_dir_all(base_dir.join("vulcan-codekit"));
    let _ = std::fs::create_dir_all(override_dir.join("vulcan-codekit"));
    let _ = std::fs::create_dir_all(override_dir.join("vulcan-runtime"));
    let _ = std::fs::write(
        base_dir.join("vulcan-codekit").join("skill.yaml"),
        "name: vulcan-codekit\nversion: 0.1.0\n",
    );
    let _ = std::fs::write(
        override_dir.join("vulcan-codekit").join("skill.yaml"),
        "name: vulcan-codekit\nversion: 0.2.0\n",
    );
    let _ = std::fs::write(
        override_dir.join("vulcan-runtime").join("skill.yaml"),
        "name: vulcan-runtime\nversion: 0.1.0\n",
    );

    let resolved = collect_effective_skill_instances(&base_dir, Some(&override_dir))
        .expect("effective skill collection should succeed");
    assert_eq!(resolved.len(), 2);
    let codekit = resolved
        .iter()
        .find(|value| value.skill_id == "vulcan-codekit")
        .expect("vulcan-codekit should exist");
    assert!(codekit.actual_dir.starts_with(&base_dir));
    let runtime = resolved
        .iter()
        .find(|value| value.skill_id == "vulcan-runtime")
        .expect("project-only vulcan-runtime should exist");
    assert!(runtime.actual_dir.starts_with(&override_dir));

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify that resolving one effective skill instance keeps ROOT ahead of PROJECT.
/// 验证解析单个生效技能实例时会保持 ROOT 高于 PROJECT。
#[test]
fn resolve_effective_skill_instance_prefers_root_directory() {
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills_resolve_effective_instance_test_{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    let base_dir = temp_root.join("base");
    let override_dir = temp_root.join("override");
    let _ = std::fs::create_dir_all(base_dir.join("vulcan-codekit"));
    let _ = std::fs::create_dir_all(override_dir.join("vulcan-codekit"));
    let _ = std::fs::write(
        base_dir.join("vulcan-codekit").join("skill.yaml"),
        "name: vulcan-codekit\nversion: 0.1.0\n",
    );
    let _ = std::fs::write(
        override_dir.join("vulcan-codekit").join("skill.yaml"),
        "name: vulcan-codekit\nversion: 0.2.0\n",
    );

    let resolved =
        resolve_effective_skill_instance(&base_dir, Some(&override_dir), "vulcan-codekit")
            .expect("resolution should succeed")
            .expect("instance should exist");
    assert!(resolved.actual_dir.starts_with(&base_dir));

    let _ = std::fs::remove_dir_all(&temp_root);
}

/// Verify Unix millisecond conversion reports normal post-epoch timestamps.
/// 验证 Unix 毫秒转换会报告正常的 epoch 之后时间戳。
#[test]
fn unix_millis_from_system_time_accepts_post_epoch_time() {
    // Timestamp one millisecond after the Unix epoch.
    // Unix epoch 之后一毫秒的时间戳。
    let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1);

    assert_eq!(
        unix_millis_from_system_time(timestamp, "test timestamp")
            .expect("post-epoch timestamp should convert"),
        1
    );
}

/// Verify Unix millisecond conversion rejects pre-epoch timestamps instead of returning zero.
/// 验证 Unix 毫秒转换会拒绝早于 epoch 的时间戳，而不是返回零。
#[test]
fn unix_millis_from_system_time_rejects_pre_epoch_time() {
    // Timestamp one millisecond before the Unix epoch.
    // Unix epoch 之前一毫秒的时间戳。
    let timestamp = std::time::UNIX_EPOCH - std::time::Duration::from_millis(1);

    let error = unix_millis_from_system_time(timestamp, "test timestamp")
        .expect_err("pre-epoch timestamp should fail");

    assert!(
        error.starts_with("System clock is before Unix epoch while computing test timestamp:"),
        "unexpected error: {}",
        error
    );
}
