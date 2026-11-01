use super::*;

/// Create one empty temporary directory unique to the calling test.
/// 为调用测试创建一个唯一的空临时目录。
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "luaskills_package_{}_{}_{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temp root");
    root
}

/// Write one file and its parent directories.
/// 写入单个文件及其父目录。
fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

/// Create one skill source `demo` with bundled payloads for two platforms.
/// 创建一个带两个平台内置载荷的技能源 `demo`。
fn bundled_skill(root: &Path) -> PathBuf {
    let skill = root.join("demo");
    write(&skill.join("skill.yaml"), "name: Demo\nversion: 1.2.3\n");
    write(
        &skill.join("dependencies.yaml"),
        r#"ffi_dependencies:
  - name: demo-native
    version: "1.0.0"
    source:
      type: bundled
    packages:
      windows-x64:
        archive_type: raw
        path: native/windows-x64/demo.dll
        exports:
          - archive_path: demo.dll
            target_path: lib/demo.dll
      linux-x64:
        archive_type: raw
        path: native/linux-x64/libdemo.so
        exports:
          - archive_path: libdemo.so
            target_path: lib/libdemo.so
"#,
    );
    write(&skill.join("runtime/main.lua"), "return {}\n");
    write(
        &skill.join("native/windows-x64/demo.dll"),
        "windows payload",
    );
    write(&skill.join("native/linux-x64/libdemo.so"), "linux payload");
    write(&skill.join("notes/private.txt"), "not shipped");
    write(&skill.join("runtime/node_modules/x.js"), "generated");
    skill
}

/// Return the member names of one zip.
/// 返回单个 zip 的成员名。
fn members(path: &Path) -> BTreeSet<String> {
    let mut archive = ZipArchive::new(fs::File::open(path).expect("open zip")).expect("read zip");
    (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .expect("entry")
                .name()
                .expect("name")
                .into_owned()
        })
        .collect()
}

/// Build options for one skill source without extra entries.
/// 为单个技能源构建不带额外条目的选项。
fn source(skill_dir: &Path) -> SkillSourceOptions {
    SkillSourceOptions {
        skill_dir: skill_dir.to_path_buf(),
        skill_id: None,
        extra_root_names: Vec::new(),
    }
}

/// Build one index entry with a valid digest.
/// 构建一个带合法摘要的索引条目。
fn entry(asset: &str) -> PackageIndexEntry {
    PackageIndexEntry {
        asset: asset.to_string(),
        sha256: "a".repeat(64),
        size: 10,
    }
}

#[test]
fn index_selects_platform_then_universal_then_reports_unsupported() {
    let mut index = PackageIndex {
        schema_version: PACKAGE_INDEX_SCHEMA_VERSION,
        skill_id: "demo".to_string(),
        version: "1.2.3".to_string(),
        layout: PackageLayout::Platform,
        platforms: BTreeMap::from([(
            "linux-x64".to_string(),
            entry("demo-v1.2.3-linux-x64-skill.zip"),
        )]),
        universal: None,
    };
    index.validate("demo", "1.2.3").expect("valid index");
    assert_eq!(
        index
            .select("linux-x64")
            .expect("platform")
            .platform
            .as_deref(),
        Some("linux-x64")
    );
    let error = index.select("macos-arm64").expect_err("no package");
    assert!(error.starts_with(SKILL_PLATFORM_UNSUPPORTED), "{error}");

    index.universal = Some(entry("demo-v1.2.3-skill.zip"));
    let selected = index.select("macos-arm64").expect("universal");
    assert_eq!(selected.platform, None);
    assert_eq!(selected.entry.asset, "demo-v1.2.3-skill.zip");
}

#[test]
fn index_rejects_foreign_names_versions_and_platforms() {
    let base = PackageIndex {
        schema_version: PACKAGE_INDEX_SCHEMA_VERSION,
        skill_id: "demo".to_string(),
        version: "1.2.3".to_string(),
        layout: PackageLayout::Platform,
        platforms: BTreeMap::from([(
            "linux-x64".to_string(),
            entry("demo-v1.2.3-linux-x64-skill.zip"),
        )]),
        universal: None,
    };
    assert!(base.validate("demo", "1.2.4").is_err());
    assert!(base.validate("other", "1.2.3").is_err());

    let mut renamed = base.clone();
    renamed
        .platforms
        .insert("linux-x64".to_string(), entry("demo-linux.zip"));
    assert!(renamed.validate("demo", "1.2.3").is_err());

    let mut unknown = base.clone();
    unknown.platforms.insert(
        "solaris-sparc".to_string(),
        entry("demo-v1.2.3-solaris-sparc-skill.zip"),
    );
    assert!(unknown.validate("demo", "1.2.3").is_err());

    let mut future = base;
    future.schema_version = PACKAGE_INDEX_SCHEMA_VERSION + 1;
    assert!(future.validate("demo", "1.2.3").is_err());

    let parsed = PackageIndex::parse(
        r#"{"schema_version":1,"skill_id":"demo","version":"1.2.3","layout":"platform","platforms":{},"extra":1}"#,
        "demo",
        "1.2.3",
    );
    assert!(parsed.is_err());
}

#[test]
fn standard_build_ships_every_payload_without_marker() {
    let root = temp_root("standard");
    let skill = bundled_skill(&root);
    let report = build_skill_release(&PackageBuildRequest {
        source: source(&skill),
        out_dir: root.join("dist"),
        layout: PackageLayout::Standard,
        include_universal: false,
    })
    .expect("standard build");

    let names: Vec<&str> = report
        .assets
        .iter()
        .map(|asset| asset.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["demo-v1.2.3-skill.zip", "demo-v1.2.3-checksums.txt"]
    );
    let package = members(&report.assets[0].path);
    assert!(package.contains("demo/native/windows-x64/demo.dll"));
    assert!(package.contains("demo/native/linux-x64/libdemo.so"));
    assert!(!package.contains(&format!("demo/{PACKAGE_MARKER_FILE_NAME}")));
    assert!(
        !package
            .iter()
            .any(|name| name.contains("notes/") || name.contains("node_modules"))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn platform_build_splits_payloads_and_lists_every_asset() {
    let root = temp_root("platform");
    let skill = bundled_skill(&root);
    let report = build_skill_release(&PackageBuildRequest {
        source: source(&skill),
        out_dir: root.join("dist"),
        layout: PackageLayout::Platform,
        include_universal: true,
    })
    .expect("platform build");

    let linux = report
        .assets
        .iter()
        .find(|asset| asset.platform.as_deref() == Some("linux-x64"))
        .expect("linux package");
    let linux_members = members(&linux.path);
    assert!(linux_members.contains("demo/native/linux-x64/libdemo.so"));
    assert!(!linux_members.contains("demo/native/windows-x64/demo.dll"));
    assert!(linux_members.contains(&format!("demo/{PACKAGE_MARKER_FILE_NAME}")));

    let index_asset = report
        .assets
        .iter()
        .find(|asset| asset.kind == PackageAssetKind::Index)
        .expect("index");
    let index = PackageIndex::parse(
        &fs::read_to_string(&index_asset.path).expect("read index"),
        "demo",
        "1.2.3",
    )
    .expect("parse index");
    assert_eq!(
        index.platforms.keys().cloned().collect::<Vec<_>>(),
        ["linux-x64", "windows-x64"]
    );
    assert_eq!(index.platforms["linux-x64"].sha256, linux.sha256);
    assert!(index.universal.is_some());

    let checksums_asset = report.assets.last().expect("checksums");
    assert_eq!(checksums_asset.kind, PackageAssetKind::Checksums);
    let checksums = fs::read_to_string(&checksums_asset.path).expect("read checksums");
    for asset in &report.assets[..report.assets.len() - 1] {
        assert!(
            checksums.contains(&format!("{}  {}", asset.sha256, asset.name)),
            "{}",
            asset.name
        );
    }

    let extracted = root.join("extracted");
    let skill_dir =
        crate::download::archive::extract_skill_package_zip(&linux.path, &extracted, "demo")
            .expect("extract");
    assert_eq!(
        verify_package_platform(&skill_dir, "linux-x64", Some(Some("linux-x64")))
            .expect("matching platform"),
        Some("linux-x64".to_string())
    );
    let mismatch = verify_package_platform(&skill_dir, "windows-x64", None).expect_err("mismatch");
    assert!(
        mismatch.starts_with(SKILL_PACKAGE_PLATFORM_MISMATCH),
        "{mismatch}"
    );
    assert!(verify_package_platform(&skill_dir, "linux-x64", Some(None)).is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn platform_build_requires_bundled_payloads() {
    let root = temp_root("no_bundled");
    let skill = root.join("plain");
    write(&skill.join("skill.yaml"), "name: Plain\nversion: 0.1.0\n");
    write(&skill.join("runtime/main.lua"), "return {}\n");
    let error = build_skill_release(&PackageBuildRequest {
        source: source(&skill),
        out_dir: root.join("dist"),
        layout: PackageLayout::Platform,
        include_universal: true,
    })
    .expect_err("platform layout without payloads");
    assert!(error.contains("standard layout"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn validation_reports_missing_payloads_and_extra_entries() {
    let root = temp_root("validate");
    let skill = bundled_skill(&root);
    fs::remove_file(skill.join("native/linux-x64/libdemo.so")).expect("remove payload");
    let report = validate_skill_source(&source(&skill)).expect("validate");
    let codes: Vec<&str> = report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect();
    assert_eq!(codes, ["bundled_payload_missing"]);
    assert_eq!(report.bundled_platforms, ["windows-x64"]);

    let mut extra = source(&skill);
    extra.extra_root_names = vec!["notes".to_string(), "../escape".to_string()];
    write(&skill.join("native/linux-x64/libdemo.so"), "linux payload");
    let report = validate_skill_source(&extra).expect("validate extra");
    assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
    assert_eq!(report.issues[0].code, "root_name_invalid");
    let _ = fs::remove_dir_all(root);
}
