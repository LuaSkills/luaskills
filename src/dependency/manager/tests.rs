use super::*;
use crate::runtime::path::render_host_visible_path;
use crate::skill::dependencies::{
    DependencyArchiveType, DependencyExportSpec, DependencyPackageSpec, DependencySourceSpec,
    GithubReleaseSourceSpec, PackageDependencyManifest, ToolDependencySpec, UrlSourceSpec,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Build one minimal dependency manager rooted under one unique temporary test directory.
/// 在唯一的临时测试目录下构造一个最小依赖管理器。
fn test_manager() -> (DependencyManager, PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("luaskills-dependency-test-{}", unique));
    let config = DependencyManagerConfig {
        tool_root: root.join("dependencies").join("tools"),
        host_tool_root: root.join("bin").join("tools"),
        lua_root: root.join("dependencies").join("lua"),
        host_lua_root: root.join("lua_packages"),
        ffi_root: root.join("dependencies").join("ffi"),
        host_ffi_root: root.join("libs"),
        download_cache_root: root.join("temp").join("downloads"),
        allow_network_download: false,
        github_base_url: None,
        github_api_base_url: None,
    };
    (DependencyManager::new(config), root)
}

/// Dependency path normalization preserves portable names and hashes every unsafe component.
/// 依赖路径规范化会保留可移植名称，并散列全部不安全片段。
#[test]
fn dependency_path_component_normalization_is_safe_and_collision_resistant() {
    assert_eq!(normalize_dependency_path_component("rg"), "rg");
    assert_eq!(normalize_dependency_path_component("v1.2.3"), "v1.2.3");
    // UnsafeComponents cover traversal, empty, separator, Windows device, and trailing-dot forms.
    // UnsafeComponents 覆盖穿越、空值、分隔符、Windows 设备名与末尾点号形式。
    let unsafe_components = ["", ".", "..", "a/b", r"a\b", "CON", "nul.txt", "name."];
    for unsafe_component in unsafe_components {
        // Normalized is one fixed digest component with no remaining path grammar.
        // Normalized 是不再包含路径语法的单个固定摘要片段。
        let normalized = normalize_dependency_path_component(unsafe_component);
        assert!(
            normalized.starts_with("sha256-"),
            "unexpected normalization for {unsafe_component:?}: {normalized}"
        );
        assert_eq!(
            Path::new(&normalized).components().count(),
            1,
            "normalized component must remain singular"
        );
    }
    assert_ne!(
        normalize_dependency_path_component("a/b"),
        normalize_dependency_path_component("a?b"),
        "distinct unsafe values must not collapse to one install root"
    );
}

/// Dependency install-root construction keeps special dependency names and versions beneath root.
/// 依赖安装根构造会使特殊依赖名称与版本始终位于根目录下。
#[test]
fn dependency_install_root_cannot_escape_through_special_components() {
    // Root is a lexical ownership boundary; no filesystem setup is required for this constructor test.
    // Root 是词法归属边界；该构造测试不需要文件系统准备。
    let root = Path::new("dependency-root");
    // InstallRoot uses traversal tokens in both raw caller-controlled components.
    // InstallRoot 在两个调用方可控原始片段中都使用穿越标记。
    let install_root = build_dependency_install_root(
        root,
        DependencyScope::Host,
        "valid-skill",
        "..",
        Some(".."),
        "windows-x64",
    );
    // RelativeComponents are every component appended after the trusted root.
    // RelativeComponents 是可信根目录之后追加的全部路径片段。
    let relative_components = install_root
        .strip_prefix(root)
        .expect("install root should remain beneath dependency root")
        .components()
        .collect::<Vec<_>>();

    assert_eq!(relative_components.len(), 3);
    assert!(
        relative_components
            .iter()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    );
}

/// Return one shared guard for tests that temporarily replace the global runtime log callback.
/// 返回一把共享保护锁，用于临时替换全局运行时日志回调的测试。
fn runtime_log_callback_test_guard() -> MutexGuard<'static, ()> {
    static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    match TEST_MUTEX.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Reset the global runtime log callback when one logging test exits.
/// 在单个日志测试退出时重置全局运行时日志回调。
struct RuntimeLogCallbackResetGuard;

impl Drop for RuntimeLogCallbackResetGuard {
    fn drop(&mut self) {
        crate::runtime_logging::set_log_callback(None);
    }
}

/// Directory creation errors should render paths through the host-visible formatter.
/// 目录创建错误应通过宿主可见路径渲染器输出路径。
#[test]
fn ensure_directory_create_error_uses_host_visible_path() {
    // Temporary root that isolates the dependency directory fixture.
    // 隔离依赖目录夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills-dependency-ensure-dir-test-{}",
        std::process::id()
    ));
    if temp_root.exists() {
        // Stale fixture cleanup result is intentionally ignored before recreation.
        // 重建前对陈旧夹具的清理结果有意忽略。
        let _ = fs::remove_dir_all(&temp_root);
    }
    fs::create_dir_all(&temp_root).expect("temp root should be created");
    // File path intentionally passed as a directory root so create_dir_all fails.
    // 有意作为目录根传入的文件路径，用于触发 create_dir_all 失败。
    let root_file = temp_root.join("root-file");
    fs::write(&root_file, b"not a directory").expect("root fixture file should be written");
    // Error returned by the shared dependency directory helper.
    // 共享依赖目录辅助函数返回的错误。
    let error = ensure_directory(&root_file).expect_err("file root should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!("Failed to create {}:", render_host_visible_path(&root_file));

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
    // 对临时测试产物的清理结果按最佳努力原则有意忽略。
    let _ = fs::remove_dir_all(&temp_root);
}

/// Build one minimal tool dependency spec for the current platform.
/// 为当前平台构造一个最小工具依赖声明。
fn tool_dependency(name: &str, version: &str, platform_key: &str) -> ToolDependencySpec {
    let mut packages = BTreeMap::new();
    packages.insert(
        platform_key.to_string(),
        DependencyPackageSpec {
            archive_type: DependencyArchiveType::Raw,
            asset_name: None,
            url: Some("https://example.invalid/package".to_string()),
            exports: vec![DependencyExportSpec {
                archive_path: "demo.bin".to_string(),
                target_path: "bin/demo.bin".to_string(),
                executable: false,
            }],
        },
    );
    ToolDependencySpec {
        name: name.to_string(),
        version: Some(version.to_string()),
        required: true,
        scope: DependencyScope::Skill,
        source: DependencySourceSpec {
            source_type: DependencySourceType::Url,
            github: None,
            url: Some(UrlSourceSpec::default()),
            skilllist: None,
        },
        packages,
    }
}

/// Build one GitHub-release tool dependency that must not touch GitHub when exports exist.
/// 构造一个在导出产物已存在时不应访问 GitHub 的 GitHub Release 工具依赖。
fn github_tool_dependency(
    name: &str,
    version: Option<&str>,
    platform_key: &str,
) -> ToolDependencySpec {
    let mut packages = BTreeMap::new();
    packages.insert(
        platform_key.to_string(),
        DependencyPackageSpec {
            archive_type: DependencyArchiveType::Zip,
            asset_name: Some("demo-{version}.zip".to_string()),
            url: None,
            exports: vec![DependencyExportSpec {
                archive_path: "demo-{version}.bin".to_string(),
                target_path: "bin/demo-{version}.bin".to_string(),
                executable: false,
            }],
        },
    );
    ToolDependencySpec {
        name: name.to_string(),
        version: version.map(str::to_string),
        required: true,
        scope: DependencyScope::Skill,
        source: DependencySourceSpec {
            source_type: DependencySourceType::GithubRelease,
            github: Some(GithubReleaseSourceSpec {
                repo: "OpenVulcan/demo-dependency".to_string(),
                tag_api: None,
            }),
            url: None,
            skilllist: None,
        },
        packages,
    }
}

/// Build one GitHub-release tool dependency whose export target uses the release tag.
/// 构造一个导出目标使用 release 标签的 GitHub Release 工具依赖。
fn github_tagged_tool_dependency(
    name: &str,
    version: Option<&str>,
    platform_key: &str,
) -> ToolDependencySpec {
    let mut dependency = github_tool_dependency(name, version, platform_key);
    let package = dependency
        .packages
        .get_mut(platform_key)
        .expect("test package should exist");
    package.exports = vec![DependencyExportSpec {
        archive_path: "demo-{tag}.bin".to_string(),
        target_path: "bin/demo-{tag}.bin".to_string(),
        executable: false,
    }];
    dependency
}

/// Existing GitHub-release exports should enable the skill without remote resolution.
/// 已存在的 GitHub Release 导出产物应直接启用 skill 而不进行远程解析。
#[test]
fn ensure_dependency_reuses_existing_github_release_exports_without_remote_resolution() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (mut manager, root) = test_manager();
    manager.config.allow_network_download = true;
    manager.config.github_api_base_url = Some("https://example.invalid/github-api".to_string());
    manager.downloader = DownloadManager::new(DownloadManagerConfig {
        cache_root: manager.config.download_cache_root.clone(),
        allow_network_download: manager.config.allow_network_download,
        github_base_url: manager.config.github_base_url.clone(),
        github_api_base_url: manager.config.github_api_base_url.clone(),
    });
    let skill_id = "demo-skill";
    let manifest = PackageDependencyManifest {
        tool_dependencies: vec![github_tool_dependency(
            "demo-tool",
            Some("1.2.3"),
            platform_key,
        )],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let dependency_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "demo-tool",
        Some("1.2.3"),
        platform_key,
    );
    fs::create_dir_all(dependency_root.join("bin")).unwrap();
    fs::write(dependency_root.join("bin").join("demo-1.2.3.bin"), b"ready").unwrap();

    manager
        .ensure_skill_dependencies(skill_id, &manifest)
        .expect("existing exports should bypass GitHub release lookup");

    let _ = fs::remove_dir_all(root);
}

/// Existing version directories should be reused when a GitHub dependency omits version.
/// 当 GitHub 依赖省略版本时，应复用已有版本目录中的导出产物。
#[test]
fn ensure_dependency_reuses_existing_unversioned_github_release_exports() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (mut manager, root) = test_manager();
    manager.config.allow_network_download = true;
    manager.config.github_api_base_url = Some("https://example.invalid/github-api".to_string());
    manager.downloader = DownloadManager::new(DownloadManagerConfig {
        cache_root: manager.config.download_cache_root.clone(),
        allow_network_download: manager.config.allow_network_download,
        github_base_url: manager.config.github_base_url.clone(),
        github_api_base_url: manager.config.github_api_base_url.clone(),
    });
    let skill_id = "demo-skill";
    let manifest = PackageDependencyManifest {
        tool_dependencies: vec![github_tool_dependency("demo-tool", None, platform_key)],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let dependency_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "demo-tool",
        Some("1.2.3"),
        platform_key,
    );
    fs::create_dir_all(dependency_root.join("bin")).unwrap();
    fs::write(dependency_root.join("bin").join("demo-1.2.3.bin"), b"ready").unwrap();

    manager
        .ensure_skill_dependencies(skill_id, &manifest)
        .expect("existing version directories should bypass GitHub release lookup");

    let _ = fs::remove_dir_all(root);
}

/// Local unversioned dependency scanning should warn when the version root cannot be read.
/// 本地无版本依赖扫描在版本根目录不可读取时应发出告警。
#[test]
fn local_unversioned_dependency_probe_requests_warns_when_version_root_is_not_directory() {
    let _log_guard = runtime_log_callback_test_guard();
    let (manager, root) = test_manager();
    let skill_id = "demo-skill";
    let dependency_name = "demo-tool";
    let platform_key = "test-platform";
    // File path intentionally occupies the dependency version root expected by the scanner.
    // 有意用文件占用扫描器期望的依赖版本根目录路径。
    let dependency_root = manager
        .config
        .tool_root
        .join(skill_id)
        .join(dependency_name);
    fs::create_dir_all(
        dependency_root
            .parent()
            .expect("dependency root should have parent"),
    )
    .expect("dependency parent should be created");
    fs::write(&dependency_root, b"not a directory").expect("dependency root fixture file");
    // Captured warning messages emitted through the runtime logging callback.
    // 通过运行时日志回调捕获到的告警消息。
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let captured_warnings = Arc::clone(&warnings);
    crate::runtime_logging::set_log_callback(Some(Arc::new(move |event| {
        if event.level == crate::runtime_logging::RuntimeLogLevel::Warn {
            captured_warnings
                .lock()
                .expect("capture dependency warning")
                .push(event.message.clone());
        }
    })));
    let _callback_reset = RuntimeLogCallbackResetGuard;
    let package = DependencyPackageSpec {
        archive_type: DependencyArchiveType::Raw,
        asset_name: None,
        url: None,
        exports: Vec::new(),
    };

    let requests = manager.local_unversioned_dependency_probe_requests(
        skill_id,
        SkillDependencyKind::Tool,
        dependency_name,
        DependencyScope::Skill,
        &package,
        platform_key,
        &manager.config.tool_root,
    );

    assert!(requests.is_empty());
    let warnings = warnings.lock().expect("read dependency warnings");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("Failed to scan local dependency versions under"));
    assert!(warnings[0].contains(&render_host_visible_path(&dependency_root)));
    let _ = fs::remove_dir_all(root);
}

/// Existing GitHub-release exports using `{tag}` should reuse the likely `v` tag variant.
/// 使用 `{tag}` 的 GitHub Release 已有导出产物应复用可能的 `v` 标签变体。
#[test]
fn ensure_dependency_reuses_existing_github_release_tag_exports_without_remote_resolution() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (mut manager, root) = test_manager();
    manager.config.allow_network_download = true;
    manager.config.github_api_base_url = Some("https://example.invalid/github-api".to_string());
    manager.downloader = DownloadManager::new(DownloadManagerConfig {
        cache_root: manager.config.download_cache_root.clone(),
        allow_network_download: manager.config.allow_network_download,
        github_base_url: manager.config.github_base_url.clone(),
        github_api_base_url: manager.config.github_api_base_url.clone(),
    });
    let skill_id = "demo-skill";
    let manifest = PackageDependencyManifest {
        tool_dependencies: vec![github_tagged_tool_dependency(
            "demo-tool",
            Some("1.2.3"),
            platform_key,
        )],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let dependency_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "demo-tool",
        Some("1.2.3"),
        platform_key,
    );
    fs::create_dir_all(dependency_root.join("bin")).unwrap();
    fs::write(
        dependency_root.join("bin").join("demo-v1.2.3.bin"),
        b"ready",
    )
    .unwrap();

    manager
        .ensure_skill_dependencies(skill_id, &manifest)
        .expect("existing tag exports should bypass GitHub release lookup");

    let _ = fs::remove_dir_all(root);
}

/// Existing unversioned GitHub exports using `{tag}` should reuse scanned version roots.
/// 未声明版本且使用 `{tag}` 的 GitHub 已有导出产物应复用扫描到的版本根目录。
#[test]
fn ensure_dependency_reuses_existing_unversioned_github_release_tag_exports() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (mut manager, root) = test_manager();
    manager.config.allow_network_download = true;
    manager.config.github_api_base_url = Some("https://example.invalid/github-api".to_string());
    manager.downloader = DownloadManager::new(DownloadManagerConfig {
        cache_root: manager.config.download_cache_root.clone(),
        allow_network_download: manager.config.allow_network_download,
        github_base_url: manager.config.github_base_url.clone(),
        github_api_base_url: manager.config.github_api_base_url.clone(),
    });
    let skill_id = "demo-skill";
    let manifest = PackageDependencyManifest {
        tool_dependencies: vec![github_tagged_tool_dependency(
            "demo-tool",
            None,
            platform_key,
        )],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let dependency_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "demo-tool",
        Some("1.2.3"),
        platform_key,
    );
    fs::create_dir_all(dependency_root.join("bin")).unwrap();
    fs::write(
        dependency_root.join("bin").join("demo-v1.2.3.bin"),
        b"ready",
    )
    .unwrap();

    manager
        .ensure_skill_dependencies(skill_id, &manifest)
        .expect("existing unversioned tag exports should bypass GitHub release lookup");

    let _ = fs::remove_dir_all(root);
}

/// Dependency export detection should report filesystem probe errors explicitly.
/// 依赖导出检测应显式报告文件系统探测错误。
#[test]
fn detect_dependency_reports_export_target_probe_errors() {
    let (manager, root) = test_manager();
    // Request whose export target path contains one embedded NUL rejected by metadata probing.
    // 导出目标路径包含一个会被元数据探测拒绝的内嵌 NUL。
    let request = ResolvedDependencyRequest {
        kind: SkillDependencyKind::Tool,
        name: "demo-tool".to_string(),
        scope: DependencyScope::Skill,
        platform_key: "test-platform".to_string(),
        download_url: String::new(),
        version: Some("1.2.3".to_string()),
        install_root: root.join("dependencies").join("tools"),
        archive_type: DependencyArchiveType::Raw,
        exports: vec![DependencyExportSpec {
            archive_path: "demo.bin".to_string(),
            target_path: "bin/invalid\0demo.bin".to_string(),
            executable: false,
        }],
    };

    // Error returned before an invalid export target can be treated as a missing dependency.
    // 在非法导出目标被当作依赖缺失前返回的错误。
    let error = manager
        .detect_dependency(&request)
        .expect_err("invalid export target probe should fail");

    assert!(
        error.contains("Failed to inspect dependency export target"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("demo-tool"), "unexpected error: {}", error);
    assert!(error.contains("invalid"), "unexpected error: {}", error);

    let _ = fs::remove_dir_all(root);
}

/// Dependency detection must treat a directory at one export path as missing rather than ready.
/// 依赖检测必须把占据导出路径的目录视为缺失，而不是就绪。
#[test]
fn detect_dependency_rejects_directory_export_target() {
    // Manager and Root isolate the concrete resolved request under a unique temporary directory.
    // Manager 与 Root 把具体解析请求隔离在唯一临时目录下。
    let (manager, root) = test_manager();
    // InstallRoot contains the directory that must not satisfy a regular-file export.
    // InstallRoot 包含不能满足普通文件导出的目录。
    let install_root = root.join("dependencies").join("tools");
    // TargetPath is deliberately created as a directory instead of the declared file.
    // TargetPath 被有意创建为目录，而不是声明的文件。
    let target_path = install_root.join("bin").join("demo.bin");
    fs::create_dir_all(&target_path).expect("directory target should be created");
    // Request declares the exact path as one ordinary file export.
    // Request 把该精确路径声明为普通文件导出。
    let request = ResolvedDependencyRequest {
        kind: SkillDependencyKind::Tool,
        name: "demo-tool".to_string(),
        scope: DependencyScope::Skill,
        platform_key: "test-platform".to_string(),
        download_url: String::new(),
        version: Some("1.2.3".to_string()),
        install_root,
        archive_type: DependencyArchiveType::Raw,
        exports: vec![DependencyExportSpec {
            archive_path: "demo.bin".to_string(),
            target_path: "bin/demo.bin".to_string(),
            executable: false,
        }],
    };

    // Status must enter the existing missing-dependency recovery path.
    // Status 必须进入既有依赖缺失恢复路径。
    let status = manager
        .detect_dependency(&request)
        .expect("directory type mismatch should be a recoverable detection result");

    assert_eq!(status, DependencyDetectionStatus::Missing);
    let _ = fs::remove_dir_all(root);
}

/// Public dependency installation rejects an invalid skill id even when no dependencies are declared.
/// 即使没有声明依赖，公开依赖安装入口也会拒绝非法技能标识符。
#[test]
fn ensure_skill_dependencies_validates_public_skill_id_boundary() {
    // Manager and Root isolate the public boundary call without requiring network access.
    // Manager 与 Root 隔离公开边界调用，且不需要网络访问。
    let (manager, root) = test_manager();
    // Manifest is empty so identifier validation is the only authorized first operation.
    // Manifest 为空，因此标识符校验是唯一获准的首个操作。
    let manifest = PackageDependencyManifest::default();

    // Error proves the public manager does not depend on an engine caller to validate ownership paths.
    // Error 证明公开管理器不依赖引擎调用方来验证归属路径。
    let error = manager
        .ensure_skill_dependencies("../outside", &manifest)
        .expect_err("traversal skill id should fail");

    assert!(
        error.contains("skill_id must match"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Unix dependency detection must require execute permission for executable exports.
/// Unix 依赖检测必须为可执行导出要求执行权限。
#[cfg(unix)]
#[test]
fn dependency_export_readiness_checks_unix_execute_bits() {
    use std::os::unix::fs::PermissionsExt;

    // Root isolates one ordinary export file whose mode is changed during the test.
    // Root 隔离一个在测试期间修改模式的普通导出文件。
    let root = std::env::temp_dir().join(format!(
        "luaskills_dependency_executable_test_{}",
        std::process::id()
    ));
    if root.exists() {
        // Stale fixture cleanup result is intentionally ignored before recreation.
        // 重建前对陈旧夹具的清理结果有意忽略。
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("executable test root should be created");
    // TargetPath begins as a non-executable regular file.
    // TargetPath 初始为没有执行位的普通文件。
    let target_path = root.join("tool");
    fs::write(&target_path, b"tool").expect("tool fixture should be written");
    fs::set_permissions(&target_path, fs::Permissions::from_mode(0o644))
        .expect("non-executable mode should be set");

    assert!(
        !dependency_export_target_is_ready("demo-tool", &target_path, true)
            .expect("non-executable file should be inspected"),
        "missing execute bits must make the export unready"
    );
    fs::set_permissions(&target_path, fs::Permissions::from_mode(0o755))
        .expect("executable mode should be set");
    assert!(
        dependency_export_target_is_ready("demo-tool", &target_path, true)
            .expect("executable file should be inspected"),
        "execute bits must make the export ready"
    );
    let _ = fs::remove_dir_all(root);
}

/// Updated-skill cleanup removes stale private dependency roots while preserving unchanged ones.
/// 更新后的清理流程会删除过期的私有依赖根，同时保留未变化的依赖。
#[test]
fn cleanup_updated_skill_dependencies_removes_stale_roots_and_keeps_reused_roots() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (manager, root) = test_manager();
    let skill_id = "demo-skill";
    let previous_manifest = PackageDependencyManifest {
        tool_dependencies: vec![
            tool_dependency("rg", "14.1.1", platform_key),
            tool_dependency("fd", "9.0.0", platform_key),
        ],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let current_manifest = PackageDependencyManifest {
        tool_dependencies: vec![
            tool_dependency("rg", "14.1.2", platform_key),
            tool_dependency("fd", "9.0.0", platform_key),
        ],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };

    let stale_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "rg",
        Some("14.1.1"),
        platform_key,
    );
    let kept_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "fd",
        Some("9.0.0"),
        platform_key,
    );
    let current_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "rg",
        Some("14.1.2"),
        platform_key,
    );

    fs::create_dir_all(stale_root.join("bin")).unwrap();
    fs::write(stale_root.join("bin").join("demo.bin"), b"old").unwrap();
    fs::create_dir_all(kept_root.join("bin")).unwrap();
    fs::write(kept_root.join("bin").join("demo.bin"), b"keep").unwrap();
    fs::create_dir_all(current_root.join("bin")).unwrap();
    fs::write(current_root.join("bin").join("demo.bin"), b"new").unwrap();

    manager
        .cleanup_updated_skill_dependencies(
            skill_id,
            Some(&previous_manifest),
            Some(&current_manifest),
        )
        .unwrap();

    assert!(
        !stale_root.exists(),
        "stale dependency root should be removed"
    );
    assert!(
        kept_root.exists(),
        "unchanged dependency root should be preserved"
    );
    assert!(
        current_root.exists(),
        "current dependency root should be preserved"
    );

    let _ = fs::remove_dir_all(root);
}

/// Updated-skill cleanup keeps identical dependency roots when the manifest does not change.
/// 当依赖清单没有变化时，更新清理流程会保留完全相同的依赖根目录。
#[test]
fn cleanup_updated_skill_dependencies_keeps_identical_roots() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (manager, root) = test_manager();
    let skill_id = "demo-skill";
    let manifest = PackageDependencyManifest {
        tool_dependencies: vec![tool_dependency("rg", "14.1.1", platform_key)],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };

    let dependency_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "rg",
        Some("14.1.1"),
        platform_key,
    );
    fs::create_dir_all(dependency_root.join("bin")).unwrap();
    fs::write(dependency_root.join("bin").join("demo.bin"), b"keep").unwrap();

    manager
        .cleanup_updated_skill_dependencies(skill_id, Some(&manifest), Some(&manifest))
        .unwrap();

    assert!(
        dependency_root.exists(),
        "unchanged dependency root should remain"
    );

    let _ = fs::remove_dir_all(root);
}

/// Updated-skill cleanup removes all old private dependency roots that disappear from the new manifest.
/// 当新清单移除了旧依赖时，更新清理流程会删除全部过期的私有依赖根目录。
#[test]
fn cleanup_updated_skill_dependencies_removes_deleted_dependencies() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (manager, root) = test_manager();
    let skill_id = "demo-skill";
    let previous_manifest = PackageDependencyManifest {
        tool_dependencies: vec![
            tool_dependency("rg", "14.1.1", platform_key),
            tool_dependency("fd", "9.0.0", platform_key),
        ],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let current_manifest = PackageDependencyManifest::default();

    let rg_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "rg",
        Some("14.1.1"),
        platform_key,
    );
    let fd_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "fd",
        Some("9.0.0"),
        platform_key,
    );
    fs::create_dir_all(rg_root.join("bin")).unwrap();
    fs::write(rg_root.join("bin").join("demo.bin"), b"old-rg").unwrap();
    fs::create_dir_all(fd_root.join("bin")).unwrap();
    fs::write(fd_root.join("bin").join("demo.bin"), b"old-fd").unwrap();

    manager
        .cleanup_updated_skill_dependencies(
            skill_id,
            Some(&previous_manifest),
            Some(&current_manifest),
        )
        .unwrap();

    assert!(
        !rg_root.exists(),
        "removed dependency root should be deleted"
    );
    assert!(
        !fd_root.exists(),
        "removed dependency root should be deleted"
    );

    let _ = fs::remove_dir_all(root);
}

/// Updated-skill cleanup does not remove current roots when the new manifest only adds dependencies.
/// 当新清单只是新增依赖时，更新清理流程不会误删当前仍然有效的依赖根目录。
#[test]
fn cleanup_updated_skill_dependencies_preserves_existing_roots_for_add_only_changes() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (manager, root) = test_manager();
    let skill_id = "demo-skill";
    let previous_manifest = PackageDependencyManifest {
        tool_dependencies: vec![tool_dependency("rg", "14.1.1", platform_key)],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let current_manifest = PackageDependencyManifest {
        tool_dependencies: vec![
            tool_dependency("rg", "14.1.1", platform_key),
            tool_dependency("fd", "9.0.0", platform_key),
        ],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };

    let rg_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "rg",
        Some("14.1.1"),
        platform_key,
    );
    let fd_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "fd",
        Some("9.0.0"),
        platform_key,
    );
    fs::create_dir_all(rg_root.join("bin")).unwrap();
    fs::write(rg_root.join("bin").join("demo.bin"), b"keep-rg").unwrap();
    fs::create_dir_all(fd_root.join("bin")).unwrap();
    fs::write(fd_root.join("bin").join("demo.bin"), b"new-fd").unwrap();

    manager
        .cleanup_updated_skill_dependencies(
            skill_id,
            Some(&previous_manifest),
            Some(&current_manifest),
        )
        .unwrap();

    assert!(
        rg_root.exists(),
        "existing dependency root should be preserved"
    );
    assert!(
        fd_root.exists(),
        "new dependency root should remain untouched"
    );

    let _ = fs::remove_dir_all(root);
}

/// Updated-skill cleanup should reject stale roots that are not directories before removal.
/// 更新后的清理流程应在删除前拒绝非目录的过期依赖根。
#[test]
fn cleanup_updated_skill_dependencies_rejects_file_stale_root() {
    // Current platform key used by dependency-root derivation.
    // 依赖根目录派生使用的当前平台键。
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    // Dependency manager and temporary root for this stale-root fixture.
    // 本次过期根目录夹具使用的依赖管理器和临时根目录。
    let (manager, root) = test_manager();
    // Skill identifier whose previous manifest contributes the stale root.
    // 提供过期根目录的旧清单所属技能标识符。
    let skill_id = "demo-skill";
    // Previous manifest that owns one skill-local tool dependency.
    // 拥有一个技能私有工具依赖的旧清单。
    let previous_manifest = PackageDependencyManifest {
        tool_dependencies: vec![tool_dependency("rg", "14.1.1", platform_key)],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    // Current empty manifest that makes the previous dependency root stale.
    // 当前空清单会使旧依赖根目录变为过期根目录。
    let current_manifest = PackageDependencyManifest::default();
    // Concrete stale root derived by the same helper used by production cleanup.
    // 使用生产清理流程同一辅助函数派生出的具体过期根目录。
    let stale_root = build_dependency_install_root(
        &manager.config.tool_root,
        DependencyScope::Skill,
        skill_id,
        "rg",
        Some("14.1.1"),
        platform_key,
    );
    // Parent directory that allows the stale root path itself to be occupied by a regular file.
    // 允许过期根路径本身被普通文件占用的父目录。
    let stale_parent = stale_root
        .parent()
        .expect("stale dependency root should have parent");
    fs::create_dir_all(stale_parent).expect("stale root parent should be created");
    fs::write(&stale_root, b"not-a-directory").expect("stale root file should be written");

    // Error returned before recursive removal is attempted.
    // 尝试递归删除前返回的错误。
    let error = manager
        .cleanup_updated_skill_dependencies(
            skill_id,
            Some(&previous_manifest),
            Some(&current_manifest),
        )
        .expect_err("file stale dependency root cleanup should fail");
    // Expected diagnostic rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断。
    let expected_error = format!(
        "Stale dependency root is not a directory before update cleanup: {}",
        render_host_visible_path(&stale_root)
    );

    assert_eq!(error, expected_error);
    let _ = fs::remove_dir_all(root);
}

/// Updated-skill cleanup should report stale-root removal errors explicitly.
/// 更新后的清理流程应显式报告过期根目录删除错误。
#[test]
fn cleanup_updated_skill_dependencies_reports_invalid_stale_root_path() {
    let platform_key = current_platform_key();
    if platform_key == "unknown" {
        return;
    }

    let (mut manager, root) = test_manager();
    manager.config.tool_root = PathBuf::from("invalid\0tool-root");
    let skill_id = "demo-skill";
    let previous_manifest = PackageDependencyManifest {
        tool_dependencies: vec![tool_dependency("rg", "14.1.1", platform_key)],
        lua_dependencies: Vec::new(),
        ffi_dependencies: Vec::new(),
        ..PackageDependencyManifest::default()
    };
    let current_manifest = PackageDependencyManifest::default();

    // Error returned before an invalid stale root can be removed or treated as already absent.
    // 在非法过期根目录被删除或当作已经不存在前返回的错误。
    let error = manager
        .cleanup_updated_skill_dependencies(
            skill_id,
            Some(&previous_manifest),
            Some(&current_manifest),
        )
        .expect_err("invalid stale dependency root removal should fail");

    assert!(
        error.contains("Failed to inspect stale dependency root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);

    let _ = fs::remove_dir_all(root);
}

/// Uninstalled-skill cleanup should reject private roots that are not directories before removal.
/// 卸载后的清理流程应在删除前拒绝非目录的私有依赖根。
#[test]
fn cleanup_uninstalled_skill_dependencies_rejects_file_private_root() {
    // Dependency manager and temporary root for this private-root cleanup fixture.
    // 本次私有根清理夹具使用的依赖管理器和临时根目录。
    let (manager, root) = test_manager();
    // Removed skill identifier used to derive the fixed private dependency roots.
    // 用于派生固定私有依赖根目录的已移除技能标识符。
    let removed_skill_id = "demo-skill";
    // Concrete tool private root that is visited first by uninstall cleanup.
    // 卸载清理最先访问的具体工具私有根目录。
    let private_root = manager.config.tool_root.join(removed_skill_id);
    // Parent directory that allows the private root path itself to be occupied by a regular file.
    // 允许私有根路径本身被普通文件占用的父目录。
    let private_parent = private_root
        .parent()
        .expect("private dependency root should have parent");
    fs::create_dir_all(private_parent).expect("private root parent should be created");
    fs::write(&private_root, b"not-a-directory").expect("private root file should be written");

    // Error returned before recursive removal is attempted.
    // 尝试递归删除前返回的错误。
    let error = manager
        .cleanup_uninstalled_skill_dependencies_from_roots(&[], removed_skill_id, None)
        .expect_err("file private dependency root cleanup should fail");
    // Expected diagnostic rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断。
    let expected_error = format!(
        "Skill-private dependency root is not a directory before uninstall cleanup: {}",
        render_host_visible_path(&private_root)
    );

    assert_eq!(error, expected_error);
    let _ = fs::remove_dir_all(root);
}

/// The private-root filesystem helper should report invalid path probe errors explicitly.
/// 私有根文件系统 helper 应显式报告非法路径探测错误。
#[test]
fn skill_private_dependency_root_probe_reports_invalid_path() {
    // InvalidRoot contains an embedded NUL rejected by metadata probing.
    // InvalidRoot 包含会被元数据探测拒绝的内嵌 NUL。
    let invalid_root = Path::new("invalid\0skill");

    // Error returned before an invalid private root can be removed or treated as already absent.
    // 在非法私有根目录被删除或当作已经不存在前返回的错误。
    let error = remove_skill_private_dependency_root(invalid_root)
        .expect_err("invalid private dependency root removal should fail");

    assert!(
        error.contains("Failed to inspect skill-private dependency root"),
        "unexpected error: {}",
        error
    );
    assert!(error.contains("invalid"), "unexpected error: {}", error);
}

/// Public uninstall cleanup rejects a traversal id before removing any path outside configured roots.
/// 公开卸载清理会在删除配置根之外的任何路径前拒绝穿越标识符。
#[test]
fn cleanup_uninstalled_skill_dependencies_rejects_traversal_skill_id_before_delete() {
    // Manager and Root provide the concrete tool root whose parent contains the protected fixture.
    // Manager 与 Root 提供具体工具根，其父目录包含受保护夹具。
    let (manager, root) = test_manager();
    // ProtectedRoot is exactly where tool_root.join("../protected") would resolve lexically.
    // ProtectedRoot 正是 tool_root.join("../protected") 在词法上会解析到的位置。
    let protected_root = manager
        .config
        .tool_root
        .parent()
        .expect("tool root should have a parent")
        .join("protected");
    fs::create_dir_all(&protected_root).expect("protected root should be created");
    // Marker proves no destructive cleanup occurred before validation.
    // Marker 证明校验前没有发生破坏性清理。
    let marker = protected_root.join("keep.txt");
    fs::write(&marker, b"keep").expect("protected marker should be written");

    // Error must precede remove_skill_private_dependency_roots for every configured dependency kind.
    // Error 必须先于所有配置依赖类型的 remove_skill_private_dependency_roots。
    let error = manager
        .cleanup_uninstalled_skill_dependencies_from_roots(&[], "../protected", None)
        .expect_err("traversal cleanup id should fail");

    assert!(
        error.contains("removed_skill_id must match"),
        "unexpected error: {error}"
    );
    assert!(marker.is_file(), "protected marker must not be removed");

    let _ = fs::remove_dir_all(root);
}

/// GitHub-release export templates should resolve `{version}` placeholders before archive extraction.
/// GitHub Release 导出模板应在归档解包前解析 `{version}` 占位符。
#[test]
fn resolve_export_templates_expands_version_placeholder() {
    let exports = vec![DependencyExportSpec {
        archive_path: "ripgrep-{version}-x86_64-pc-windows-msvc/rg.exe".to_string(),
        target_path: "bin/rg-{version}.exe".to_string(),
        executable: false,
    }];

    let resolved = resolve_export_templates(&exports, Some("14.1.1"), Some("14.1.1"));
    assert_eq!(
        resolved[0].archive_path,
        "ripgrep-14.1.1-x86_64-pc-windows-msvc/rg.exe"
    );
    assert_eq!(resolved[0].target_path, "bin/rg-14.1.1.exe");
}

/// Failed dependency download cleanup should reject non-file cache paths before redownload.
/// 失败依赖下载清理应在重新下载前拒绝非文件缓存路径。
#[test]
fn cleanup_failed_dependency_install_attempt_rejects_download_directory() {
    // Temporary root that isolates the failed-cleanup fixture.
    // 隔离失败清理夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills-dependency-cleanup-download-dir-{}",
        std::process::id()
    ));
    if temp_root.exists() {
        // Stale fixture cleanup result is intentionally ignored before recreation.
        // 重建前对陈旧夹具的清理结果有意忽略。
        let _ = fs::remove_dir_all(&temp_root);
    }
    fs::create_dir_all(&temp_root).expect("temp root should be created");
    // Download path that is a directory instead of a removable payload file.
    // 作为目录而不是可移除载荷文件的下载路径。
    let download_path = temp_root.join("cached-payload");
    fs::create_dir_all(&download_path).expect("download directory should be created");
    // Install root that should not be reached after download cleanup fails.
    // 下载清理失败后不应触达的安装根目录。
    let install_root = temp_root.join("install-root");
    fs::create_dir_all(&install_root).expect("install root should be created");

    // Error returned by failed dependency install cleanup.
    // 失败依赖安装清理返回的错误。
    let error = cleanup_failed_dependency_install_attempt(&download_path, &install_root)
        .expect_err("download directory cleanup should fail");
    // Expected diagnostic prefix rendered with the shared host-visible path formatter.
    // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
    let expected_prefix = format!(
        "Failed to remove failed dependency download {} before redownload:",
        render_host_visible_path(&download_path)
    );

    assert!(
        error.starts_with(&expected_prefix),
        "unexpected error: {}",
        error
    );
    assert!(
        install_root.exists(),
        "install root should remain untouched after download cleanup failure"
    );
    // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
    // 对临时测试产物的清理结果按最佳努力原则有意忽略。
    let _ = fs::remove_dir_all(&temp_root);
}

/// Failed dependency install-root cleanup should reject non-directory roots before reinstall.
/// 失败依赖安装根清理应在重新安装前拒绝非目录根路径。
#[test]
fn cleanup_failed_dependency_install_attempt_rejects_install_root_file() {
    // Temporary root that isolates the failed install-root cleanup fixture.
    // 隔离失败安装根清理夹具的临时根目录。
    let temp_root = std::env::temp_dir().join(format!(
        "luaskills-dependency-cleanup-install-file-{}",
        std::process::id()
    ));
    if temp_root.exists() {
        // Stale fixture cleanup result is intentionally ignored before recreation.
        // 重建前对陈旧夹具的清理结果有意忽略。
        let _ = fs::remove_dir_all(&temp_root);
    }
    fs::create_dir_all(&temp_root).expect("temp root should be created");
    // Regular cached payload file that should be removed before install-root cleanup.
    // 应在安装根清理前删除的普通缓存载荷文件。
    let download_path = temp_root.join("cached-payload.bin");
    fs::write(&download_path, b"bad-payload").expect("download payload should be written");
    // Install root path occupied by a file so recursive directory removal fails.
    // 被文件占用的安装根路径，用于触发递归目录删除失败。
    let install_root = temp_root.join("install-root");
    fs::write(&install_root, b"not-a-directory").expect("install root file should be written");

    // Error returned by failed dependency install cleanup.
    // 失败依赖安装清理返回的错误。
    let error = cleanup_failed_dependency_install_attempt(&download_path, &install_root)
        .expect_err("install root file cleanup should fail");
    // Expected diagnostic rendered before recursive directory removal is attempted.
    // 在尝试递归删除目录前生成的期望诊断。
    let expected_error = format!(
        "Failed dependency install root is not a directory before reinstall: {}",
        render_host_visible_path(&install_root)
    );

    assert_eq!(error, expected_error);
    assert!(
        !download_path.exists(),
        "download payload should be removed before install root cleanup fails"
    );
    // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
    // 对临时测试产物的清理结果按最佳努力原则有意忽略。
    let _ = fs::remove_dir_all(&temp_root);
}

/// UTF-8 dependency version directory names should be accepted as version components.
/// UTF-8 依赖版本目录名应被接受为版本片段。
#[test]
fn local_dependency_version_component_accepts_utf8_file_name() {
    let component =
        local_dependency_version_component_from_file_name(std::ffi::OsString::from("1.2.3"));

    assert_eq!(component.as_deref(), Some("1.2.3"));
}

/// Invalid Unix dependency version directory names should not become lossy versions.
/// 非法 Unix 依赖版本目录名不应被有损转换为版本号。
#[cfg(unix)]
#[test]
fn local_dependency_version_component_rejects_invalid_unicode_file_name() {
    use std::os::unix::ffi::OsStringExt;

    let component =
        local_dependency_version_component_from_file_name(std::ffi::OsString::from_vec(vec![0xff]));

    assert!(component.is_none());
}
