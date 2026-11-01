//! Release package layouts: the standard single package and per-platform packages with a package index.
//! 发布包布局：标准单包，以及带包索引的按平台分包。
//!
//! The runtime selection rule, the in-package marker, and the build rules used by packaging tools all live here,
//! so installers and packagers never disagree about one release.
//! 运行时选包规则、包内标记与打包工具使用的构建规则都集中在此，使安装器与打包器对同一发布的理解一致。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use crate::dependency::platform::{SUPPORTED_PLATFORM_KEYS, is_supported_platform_key};
use crate::dependency::types::DependencySourceType;
use crate::lua_skill::{validate_luaskills_identifier, validate_luaskills_version};
use crate::runtime::path::render_host_visible_path;
use crate::skill::dependencies::{DependencyPackageSpec, PackageDependencyManifest};
use crate::skill::resolver::normalize_sha256;

/// Schema version of the release package index file.
/// 发布包索引文件的结构版本。
pub const PACKAGE_INDEX_SCHEMA_VERSION: u32 = 1;

/// Schema version of the in-package platform marker.
/// 包内平台标记的结构版本。
pub const PACKAGE_MARKER_SCHEMA_VERSION: u32 = 1;

/// File name of the platform marker at the root of one platform package.
/// 平台包根目录下平台标记的文件名。
pub const PACKAGE_MARKER_FILE_NAME: &str = "luaskills-package.json";

/// Top-level entries every package may contain; tools add skill-specific entries explicitly.
/// 每个技能包都可以包含的顶层条目；工具需显式追加技能专属条目。
pub const DEFAULT_PACKAGE_ROOT_NAMES: [&str; 16] = [
    "skill.yaml",
    "dependencies.yaml",
    "README.md",
    "README.zh-CN.md",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "THIRD_PARTY_LICENSES.md",
    "runtime",
    "node",
    "python",
    "native",
    "help",
    "overflow_templates",
    "resources",
    "licenses",
    "schemas",
];

/// Directory names that are generated locally and never ship.
/// 本地生成、绝不进入发布包的目录名。
const GENERATED_DIRECTORY_NAMES: [&str; 3] = ["node_modules", ".pnpm-store", "__pycache__"];

/// Stable error code returned when a release has no package for the current platform.
/// 发布中没有当前平台可用包时返回的稳定错误码。
pub const SKILL_PLATFORM_UNSUPPORTED: &str = "skill_platform_unsupported";

/// Stable error code returned when a platform package does not match the running platform.
/// 平台包与当前运行平台不符时返回的稳定错误码。
pub const SKILL_PACKAGE_PLATFORM_MISMATCH: &str = "skill_package_platform_mismatch";

/// Release package layout selected when building one version.
/// 构建单个版本时选择的发布包布局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageLayout {
    /// One package that carries every platform's bundled payloads.
    /// 携带全部平台内置载荷的单个包。
    Standard,
    /// One package per platform plus a package index.
    /// 每个平台一个包，另加包索引。
    Platform,
}

/// Return the standard (universal) package asset name.
/// 返回标准（通用）包的资产名。
pub fn standard_package_asset_name(skill_id: &str, version: &str) -> String {
    format!("{skill_id}-v{version}-skill.zip")
}

/// Return one platform package asset name.
/// 返回单个平台包的资产名。
pub fn platform_package_asset_name(skill_id: &str, version: &str, platform: &str) -> String {
    format!("{skill_id}-v{version}-{platform}-skill.zip")
}

/// Return the package index asset name.
/// 返回包索引的资产名。
pub fn package_index_asset_name(skill_id: &str, version: &str) -> String {
    format!("{skill_id}-v{version}-packages.json")
}

/// Return the checksum manifest asset name.
/// 返回校验文件的资产名。
pub fn checksums_asset_name(skill_id: &str, version: &str) -> String {
    format!("{skill_id}-v{version}-checksums.txt")
}

/// One package listed by a package index.
/// 包索引列出的单个包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIndexEntry {
    /// Exact release asset name.
    /// 精确的发布资产名。
    pub asset: String,
    /// Lowercase SHA-256 of the asset bytes.
    /// 资产字节的小写 SHA-256。
    pub sha256: String,
    /// Asset size in bytes.
    /// 资产字节大小。
    pub size: u64,
}

/// Release package index published as `{skill_id}-v{version}-packages.json`.
/// 以 `{skill_id}-v{version}-packages.json` 发布的发布包索引。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIndex {
    /// Index schema version; only [`PACKAGE_INDEX_SCHEMA_VERSION`] is accepted.
    /// 索引结构版本；只接受 [`PACKAGE_INDEX_SCHEMA_VERSION`]。
    pub schema_version: u32,
    /// Skill identifier.
    /// 技能标识符。
    pub skill_id: String,
    /// Skill version.
    /// 技能版本。
    pub version: String,
    /// Layout; an index always describes the platform layout.
    /// 布局；索引总是描述按平台布局。
    pub layout: PackageLayout,
    /// Platform packages keyed by platform key.
    /// 按平台键索引的平台包。
    pub platforms: BTreeMap<String, PackageIndexEntry>,
    /// Optional universal package for platforms without a dedicated package and for older runtimes.
    /// 可选的通用包，供没有专属包的平台与旧运行时使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universal: Option<PackageIndexEntry>,
}

/// Package chosen from one index for one platform.
/// 从单个索引为单个平台选中的包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPackage {
    /// Platform key of a platform package, or `None` for the universal package.
    /// 平台包的平台键；通用包为 `None`。
    pub platform: Option<String>,
    /// Selected index entry.
    /// 选中的索引条目。
    pub entry: PackageIndexEntry,
}

impl PackageIndex {
    /// Parse and validate one package index for an expected skill and version.
    /// 针对期望的技能与版本解析并校验单个包索引。
    pub fn parse(
        text: &str,
        expected_skill_id: &str,
        expected_version: &str,
    ) -> Result<Self, String> {
        let index: PackageIndex = serde_json::from_str(text)
            .map_err(|error| format!("package index is not valid JSON: {error}"))?;
        index.validate(expected_skill_id, expected_version)?;
        Ok(index)
    }

    /// Validate this index for an expected skill and version and normalize nothing in place.
    /// 针对期望的技能与版本校验当前索引，不做原地规范化。
    pub fn validate(&self, expected_skill_id: &str, expected_version: &str) -> Result<(), String> {
        if self.schema_version != PACKAGE_INDEX_SCHEMA_VERSION {
            return Err(format!(
                "package index schema_version {} is not supported; expected {}",
                self.schema_version, PACKAGE_INDEX_SCHEMA_VERSION
            ));
        }
        if self.skill_id != expected_skill_id {
            return Err(format!(
                "package index describes skill '{}' instead of '{}'",
                self.skill_id, expected_skill_id
            ));
        }
        if self.version != expected_version {
            return Err(format!(
                "package index describes version '{}' instead of '{}'",
                self.version, expected_version
            ));
        }
        if self.layout != PackageLayout::Platform {
            return Err("package index layout must be 'platform'".to_string());
        }
        if self.platforms.is_empty() {
            return Err("package index must list at least one platform package".to_string());
        }
        let mut assets = BTreeSet::new();
        for (platform, entry) in &self.platforms {
            if !is_supported_platform_key(platform) {
                return Err(format!(
                    "package index platform '{}' is not one of {}",
                    platform,
                    SUPPORTED_PLATFORM_KEYS.join(", ")
                ));
            }
            let expected = platform_package_asset_name(&self.skill_id, &self.version, platform);
            validate_index_entry(entry, &expected)?;
            assets.insert(entry.asset.as_str());
        }
        if let Some(universal) = &self.universal {
            let expected = standard_package_asset_name(&self.skill_id, &self.version);
            validate_index_entry(universal, &expected)?;
            assets.insert(universal.asset.as_str());
        }
        Ok(())
    }

    /// Select the package for one platform: its platform package, else the universal package.
    /// 为单个平台选包：优先其平台包，否则用通用包。
    pub fn select(&self, platform: &str) -> Result<SelectedPackage, String> {
        if let Some(entry) = self.platforms.get(platform) {
            return Ok(SelectedPackage {
                platform: Some(platform.to_string()),
                entry: entry.clone(),
            });
        }
        if let Some(entry) = &self.universal {
            return Ok(SelectedPackage {
                platform: None,
                entry: entry.clone(),
            });
        }
        Err(format!(
            "{SKILL_PLATFORM_UNSUPPORTED}: skill '{}' version {} has no package for platform '{}'; available: {}",
            self.skill_id,
            self.version,
            platform,
            self.platforms
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Render this index as the canonical JSON text written to releases.
    /// 将当前索引渲染为写入发布的规范 JSON 文本。
    pub fn to_canonical_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map(|text| format!("{text}\n"))
            .map_err(|error| format!("failed to render package index: {error}"))
    }
}

/// Validate one index entry against its conventional asset name.
/// 按约定资产名校验单个索引条目。
fn validate_index_entry(entry: &PackageIndexEntry, expected_asset: &str) -> Result<(), String> {
    if entry.asset != expected_asset {
        return Err(format!(
            "package index asset '{}' must be named '{}'",
            entry.asset, expected_asset
        ));
    }
    let normalized = normalize_sha256(&entry.sha256).map_err(|_| {
        format!(
            "package index asset '{}' has an invalid sha256",
            entry.asset
        )
    })?;
    if normalized != entry.sha256 {
        return Err(format!(
            "package index asset '{}' sha256 must be lowercase hexadecimal",
            entry.asset
        ));
    }
    if entry.size == 0 {
        return Err(format!(
            "package index asset '{}' size must be positive",
            entry.asset
        ));
    }
    Ok(())
}

/// Platform marker stored as `luaskills-package.json` at the root of a platform package.
/// 以 `luaskills-package.json` 存放在平台包根目录的平台标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMarker {
    /// Marker schema version; only [`PACKAGE_MARKER_SCHEMA_VERSION`] is accepted.
    /// 标记结构版本；只接受 [`PACKAGE_MARKER_SCHEMA_VERSION`]。
    pub schema_version: u32,
    /// Layout; a marker always describes the platform layout.
    /// 布局；标记总是描述按平台布局。
    pub layout: PackageLayout,
    /// Platform key the package was built for.
    /// 技能包所针对的平台键。
    pub platform: String,
}

impl PackageMarker {
    /// Build the marker for one platform package.
    /// 为单个平台包构建标记。
    pub fn for_platform(platform: &str) -> Self {
        Self {
            schema_version: PACKAGE_MARKER_SCHEMA_VERSION,
            layout: PackageLayout::Platform,
            platform: platform.to_string(),
        }
    }

    /// Render the canonical one-line marker text.
    /// 渲染规范的单行标记文本。
    pub fn to_canonical_json(&self) -> Result<String, String> {
        serde_json::to_string(self)
            .map(|text| format!("{text}\n"))
            .map_err(|error| format!("failed to render package marker: {error}"))
    }
}

/// Read and validate the platform marker of one extracted skill directory; `None` means a universal package.
/// 读取并校验单个已解压技能目录的平台标记；`None` 表示通用包。
pub fn read_package_marker(skill_dir: &Path) -> Result<Option<PackageMarker>, String> {
    let path = skill_dir.join(PACKAGE_MARKER_FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Failed to read {}: {}",
                render_host_visible_path(&path),
                error
            ));
        }
    };
    let marker: PackageMarker = serde_json::from_str(&text).map_err(|error| {
        format!(
            "package marker {} is invalid: {}",
            render_host_visible_path(&path),
            error
        )
    })?;
    if marker.schema_version != PACKAGE_MARKER_SCHEMA_VERSION {
        return Err(format!(
            "package marker schema_version {} is not supported; expected {}",
            marker.schema_version, PACKAGE_MARKER_SCHEMA_VERSION
        ));
    }
    if marker.layout != PackageLayout::Platform {
        return Err("package marker layout must be 'platform'".to_string());
    }
    if !is_supported_platform_key(&marker.platform) {
        return Err(format!(
            "package marker platform '{}' is not supported",
            marker.platform
        ));
    }
    Ok(Some(marker))
}

/// Check that one extracted package may be installed on `platform` and return its package platform.
/// 检查单个已解压包能否安装在 `platform` 上，并返回其包平台。
///
/// `expected` is the platform the source selected (`Some(None)` for a universal package); `None` skips that check.
/// `expected` 是来源选中的平台（通用包为 `Some(None)`）；为 `None` 时跳过该检查。
pub fn verify_package_platform(
    skill_dir: &Path,
    platform: &str,
    expected: Option<Option<&str>>,
) -> Result<Option<String>, String> {
    let marker_platform = read_package_marker(skill_dir)?.map(|marker| marker.platform);
    if let Some(package_platform) = marker_platform.as_deref()
        && package_platform != platform
    {
        return Err(format!(
            "{SKILL_PACKAGE_PLATFORM_MISMATCH}: package was built for '{package_platform}' but this runtime is '{platform}'"
        ));
    }
    if let Some(expected) = expected
        && expected != marker_platform.as_deref()
    {
        return Err(format!(
            "{SKILL_PACKAGE_PLATFORM_MISMATCH}: source selected the {} package but the archive is the {} package",
            expected.unwrap_or("universal"),
            marker_platform.as_deref().unwrap_or("universal")
        ));
    }
    Ok(marker_platform)
}

/// One problem found while validating a skill source directory.
/// 校验技能源目录时发现的单个问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillValidationIssue {
    /// Stable issue code.
    /// 稳定问题代码。
    pub code: String,
    /// English description with the fix.
    /// 带修复建议的英文描述。
    pub message: String,
    /// Package-relative path the issue refers to, when any.
    /// 问题所指的包内相对路径（如有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Validation result of one skill source directory.
/// 单个技能源目录的校验结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillValidationReport {
    /// Skill identifier used for the package root.
    /// 用作包根目录的技能标识符。
    pub skill_id: String,
    /// Version declared by `skill.yaml`, when readable.
    /// `skill.yaml` 声明的版本（可读取时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Platforms with at least one bundled payload, which are the platforms a platform layout can build.
    /// 至少有一个内置载荷的平台，即按平台布局可以构建的平台。
    pub bundled_platforms: Vec<String>,
    /// Number of files the standard package would contain.
    /// 标准包将包含的文件数。
    pub file_count: usize,
    /// Uncompressed bytes the standard package would contain.
    /// 标准包将包含的未压缩字节数。
    pub total_bytes: u64,
    /// Problems found; an empty list means the directory can be packaged.
    /// 发现的问题；为空表示目录可以打包。
    pub issues: Vec<SkillValidationIssue>,
}

impl SkillValidationReport {
    /// Return whether no issue was found.
    /// 返回是否没有发现问题。
    pub fn is_valid(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Inputs shared by validation and packaging.
/// 校验与打包共用的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSourceOptions {
    /// Skill source directory, usually the repository root.
    /// 技能源目录，通常是仓库根目录。
    pub skill_dir: PathBuf,
    /// Explicit skill id; defaults to the directory name.
    /// 显式技能标识符；默认取目录名。
    pub skill_id: Option<String>,
    /// Extra top-level entries this skill ships besides [`DEFAULT_PACKAGE_ROOT_NAMES`].
    /// 除 [`DEFAULT_PACKAGE_ROOT_NAMES`] 外，本技能额外发布的顶层条目。
    pub extra_root_names: Vec<String>,
}

/// One collected package file.
/// 单个已收集的包文件。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageFile {
    /// POSIX path relative to the skill root.
    /// 相对技能根目录的 POSIX 路径。
    relative: String,
    /// Source file on disk.
    /// 磁盘上的源文件。
    source: PathBuf,
    /// File size in bytes.
    /// 文件字节大小。
    size: u64,
}

/// Fully inspected skill source ready for packaging.
/// 已完整检查、可以打包的技能源。
struct InspectedSource {
    report: SkillValidationReport,
    files: Vec<PackageFile>,
    /// Bundled payload paths per platform key.
    /// 按平台键列出的内置载荷路径。
    bundled: BTreeMap<String, BTreeSet<String>>,
}

/// Validate one skill source directory without writing anything.
/// 校验单个技能源目录，不写入任何内容。
pub fn validate_skill_source(
    options: &SkillSourceOptions,
) -> Result<SkillValidationReport, String> {
    Ok(inspect_skill_source(options)?.report)
}

/// Inspect one skill source directory; `Err` only for unusable inputs such as a missing directory.
/// 检查单个技能源目录；只有目录缺失等不可用输入才返回 `Err`。
fn inspect_skill_source(options: &SkillSourceOptions) -> Result<InspectedSource, String> {
    let skill_dir = &options.skill_dir;
    let metadata = fs::metadata(skill_dir).map_err(|error| {
        format!(
            "skill directory {} cannot be read: {}",
            render_host_visible_path(skill_dir),
            error
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "skill path {} is not a directory",
            render_host_visible_path(skill_dir)
        ));
    }
    let skill_id = match &options.skill_id {
        Some(skill_id) => skill_id.trim().to_string(),
        None => fs::canonicalize(skill_dir)
            .ok()
            .and_then(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                format!(
                    "Failed to resolve skill id from directory {}",
                    render_host_visible_path(skill_dir)
                )
            })?,
    };
    validate_luaskills_identifier(&skill_id, "skill_id")?;

    let mut issues = Vec::new();
    let mut version = None;
    match crate::skill::manager::read_skill_manifest_with_skill_id(skill_dir, skill_id.clone()) {
        Ok(meta) => match validate_luaskills_version(meta.version(), "skill.yaml version") {
            Ok(()) => version = Some(meta.version().to_string()),
            Err(error) => issues.push(issue("manifest_invalid", error, Some("skill.yaml"))),
        },
        Err(error) => issues.push(issue("manifest_invalid", error, Some("skill.yaml"))),
    }

    let mut root_names: BTreeSet<String> = DEFAULT_PACKAGE_ROOT_NAMES
        .iter()
        .map(|name| name.to_string())
        .collect();
    for name in &options.extra_root_names {
        let name = name.trim();
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            issues.push(issue(
                "root_name_invalid",
                format!(
                    "extra package entry '{name}' must be one top-level file or directory name"
                ),
                None,
            ));
            continue;
        }
        if name == PACKAGE_MARKER_FILE_NAME {
            issues.push(issue(
                "root_name_reserved",
                format!("'{PACKAGE_MARKER_FILE_NAME}' is written by the packager and cannot be shipped from source"),
                Some(name),
            ));
            continue;
        }
        root_names.insert(name.to_string());
    }
    if skill_dir.join(PACKAGE_MARKER_FILE_NAME).exists() {
        issues.push(issue(
            "marker_in_source",
            format!("remove '{PACKAGE_MARKER_FILE_NAME}' from the source; only built platform packages carry it"),
            Some(PACKAGE_MARKER_FILE_NAME),
        ));
    }

    let files = collect_package_files(skill_dir, &root_names, &mut issues)?;
    let packaged: BTreeSet<&str> = files.iter().map(|file| file.relative.as_str()).collect();

    let mut bundled: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let dependencies_path = skill_dir.join("dependencies.yaml");
    if dependencies_path.is_file() {
        match PackageDependencyManifest::load_from_path(&dependencies_path) {
            Ok(manifest) => {
                for (platform, path) in bundled_payloads(&manifest) {
                    if !is_supported_platform_key(&platform) {
                        issues.push(issue(
                            "platform_key_unsupported",
                            format!(
                                "bundled payload platform '{}' is not one of {}",
                                platform,
                                SUPPORTED_PLATFORM_KEYS.join(", ")
                            ),
                            Some("dependencies.yaml"),
                        ));
                        continue;
                    }
                    let Some(path) = path else {
                        issues.push(issue(
                            "bundled_path_missing",
                            format!("bundled package for platform '{platform}' must declare path"),
                            Some("dependencies.yaml"),
                        ));
                        continue;
                    };
                    let Some(normalized) = normalize_relative_path(&path) else {
                        issues.push(issue(
                            "bundled_path_invalid",
                            format!("bundled payload path '{path}' must be relative and stay inside the skill"),
                            Some("dependencies.yaml"),
                        ));
                        continue;
                    };
                    if !packaged.contains(normalized.as_str()) {
                        let code = if skill_dir.join(&normalized).is_file() {
                            "bundled_payload_not_packaged"
                        } else {
                            "bundled_payload_missing"
                        };
                        let message = if code == "bundled_payload_missing" {
                            format!(
                                "bundled payload '{normalized}' for platform '{platform}' does not exist; build or stage it before packaging"
                            )
                        } else {
                            format!(
                                "bundled payload '{normalized}' for platform '{platform}' is outside the package entries; move it under 'native/' or add its top-level entry"
                            )
                        };
                        issues.push(issue(code, message, Some(&normalized)));
                        continue;
                    }
                    bundled.entry(platform).or_default().insert(normalized);
                }
            }
            Err(error) => issues.push(issue(
                "dependencies_invalid",
                error,
                Some("dependencies.yaml"),
            )),
        }
    }

    let report = SkillValidationReport {
        skill_id,
        version,
        bundled_platforms: bundled.keys().cloned().collect(),
        file_count: files.len(),
        total_bytes: files.iter().map(|file| file.size).sum(),
        issues,
    };
    Ok(InspectedSource {
        report,
        files,
        bundled,
    })
}

/// Build one validation issue.
/// 构建单个校验问题。
fn issue(code: &str, message: String, path: Option<&str>) -> SkillValidationIssue {
    SkillValidationIssue {
        code: code.to_string(),
        message,
        path: path.map(str::to_string),
    }
}

/// List every bundled payload as `(platform, path)` across tool, Lua and FFI dependencies.
/// 列出工具、Lua 与 FFI 依赖中的全部内置载荷 `(平台, 路径)`。
fn bundled_payloads(manifest: &PackageDependencyManifest) -> Vec<(String, Option<String>)> {
    let mut payloads = Vec::new();
    let mut push = |source_type: DependencySourceType,
                    packages: &BTreeMap<String, DependencyPackageSpec>| {
        if source_type == DependencySourceType::Bundled {
            for (platform, package) in packages {
                payloads.push((platform.clone(), package.path.clone()));
            }
        }
    };
    for dependency in &manifest.tool_dependencies {
        push(dependency.source.source_type, &dependency.packages);
    }
    for dependency in &manifest.lua_dependencies {
        push(dependency.source.source_type, &dependency.packages);
    }
    for dependency in &manifest.ffi_dependencies {
        push(dependency.source.source_type, &dependency.packages);
    }
    payloads
}

/// Normalize one package-relative path to POSIX form, rejecting absolute and escaping paths.
/// 将单个包内相对路径规范化为 POSIX 形式，拒绝绝对路径与越界路径。
fn normalize_relative_path(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in Path::new(&path.replace('\\', "/")).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Collect every regular file under the allowed top-level entries, sorted by relative path.
/// 收集允许的顶层条目下的全部普通文件，按相对路径排序。
fn collect_package_files(
    skill_dir: &Path,
    root_names: &BTreeSet<String>,
    issues: &mut Vec<SkillValidationIssue>,
) -> Result<Vec<PackageFile>, String> {
    let mut files = Vec::new();
    for name in root_names {
        let path = skill_dir.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to inspect {}: {}",
                    render_host_visible_path(&path),
                    error
                ));
            }
        };
        walk_package_entry(&path, name.clone(), metadata, &mut files, issues)?;
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(files)
}

/// Walk one package entry recursively, skipping generated files and reporting symbolic links.
/// 递归遍历单个包条目，跳过生成文件并报告符号链接。
fn walk_package_entry(
    path: &Path,
    relative: String,
    metadata: fs::Metadata,
    files: &mut Vec<PackageFile>,
    issues: &mut Vec<SkillValidationIssue>,
) -> Result<(), String> {
    let name = relative.rsplit('/').next().unwrap_or(relative.as_str());
    if metadata.file_type().is_symlink() {
        issues.push(issue(
            "symlink_not_allowed",
            format!("'{relative}' is a symbolic link; packages may contain only regular files"),
            Some(&relative),
        ));
        return Ok(());
    }
    if metadata.is_dir() {
        if GENERATED_DIRECTORY_NAMES.contains(&name) {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)
            .map_err(|error| {
                format!(
                    "Failed to read {}: {}",
                    render_host_visible_path(path),
                    error
                )
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                format!(
                    "Failed to read {}: {}",
                    render_host_visible_path(path),
                    error
                )
            })?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let child_name = entry.file_name();
            let Some(child_name) = child_name.to_str() else {
                issues.push(issue(
                    "path_not_utf8",
                    format!("an entry under '{relative}' has a name that is not valid UTF-8"),
                    Some(&relative),
                ));
                continue;
            };
            let child_metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                format!(
                    "Failed to inspect {}: {}",
                    render_host_visible_path(&entry.path()),
                    error
                )
            })?;
            walk_package_entry(
                &entry.path(),
                format!("{relative}/{child_name}"),
                child_metadata,
                files,
                issues,
            )?;
        }
        return Ok(());
    }
    if !metadata.is_file() || name == ".DS_Store" || name.ends_with(".pyc") {
        return Ok(());
    }
    files.push(PackageFile {
        relative,
        source: path.to_path_buf(),
        size: metadata.len(),
    });
    Ok(())
}

/// Request to build the release assets of one skill version.
/// 构建单个技能版本发布资产的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageBuildRequest {
    /// Skill source to package.
    /// 待打包的技能源。
    pub source: SkillSourceOptions,
    /// Output directory for the release assets.
    /// 发布资产的输出目录。
    pub out_dir: PathBuf,
    /// Layout to build.
    /// 要构建的布局。
    pub layout: PackageLayout,
    /// For the platform layout, also build the universal package listed as `universal`.
    /// 按平台布局时，同时构建列为 `universal` 的通用包。
    pub include_universal: bool,
}

/// Kind of one built release asset.
/// 单个已构建发布资产的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAssetKind {
    /// The standard (universal) package.
    /// 标准（通用）包。
    Universal,
    /// One platform package.
    /// 单个平台包。
    Platform,
    /// The package index.
    /// 包索引。
    Index,
    /// The checksum manifest.
    /// 校验文件。
    Checksums,
}

/// One built release asset.
/// 单个已构建的发布资产。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageAsset {
    /// Asset file name.
    /// 资产文件名。
    pub name: String,
    /// Asset kind.
    /// 资产种类。
    pub kind: PackageAssetKind,
    /// Platform key of a platform package.
    /// 平台包的平台键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Lowercase SHA-256 of the asset.
    /// 资产的小写 SHA-256。
    pub sha256: String,
    /// Asset size in bytes.
    /// 资产字节大小。
    pub size: u64,
    /// Full path of the written asset.
    /// 已写入资产的完整路径。
    pub path: PathBuf,
}

/// Result of one package build.
/// 单次打包的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageBuildReport {
    /// Skill identifier.
    /// 技能标识符。
    pub skill_id: String,
    /// Skill version.
    /// 技能版本。
    pub version: String,
    /// Built layout.
    /// 已构建的布局。
    pub layout: PackageLayout,
    /// Every written asset, packages first, then the index and the checksum manifest.
    /// 全部已写入资产：先是各包，再是索引与校验文件。
    pub assets: Vec<PackageAsset>,
}

/// Validate one skill source and build its release assets.
/// 校验单个技能源并构建其发布资产。
///
/// Every package shares the same files except other platforms' bundled payloads and the platform marker.
/// 除其他平台的内置载荷与平台标记外，各包的文件完全相同。
pub fn build_skill_release(request: &PackageBuildRequest) -> Result<PackageBuildReport, String> {
    let inspected = inspect_skill_source(&request.source)?;
    if !inspected.report.is_valid() {
        let details = inspected
            .report
            .issues
            .iter()
            .map(|issue| format!("[{}] {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("skill source is not valid: {details}"));
    }
    let skill_id = inspected.report.skill_id.clone();
    let version = inspected
        .report
        .version
        .clone()
        .ok_or_else(|| "skill.yaml version is missing".to_string())?;
    if request.layout == PackageLayout::Platform && inspected.bundled.is_empty() {
        return Err(
            "platform layout requires at least one bundled payload in dependencies.yaml; use the standard layout".to_string(),
        );
    }
    fs::create_dir_all(&request.out_dir).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_host_visible_path(&request.out_dir),
            error
        )
    })?;

    let mut assets = Vec::new();
    let build_universal = request.layout == PackageLayout::Standard || request.include_universal;
    if build_universal {
        let name = standard_package_asset_name(&skill_id, &version);
        let files: Vec<&PackageFile> = inspected.files.iter().collect();
        assets.push(write_package(
            &request.out_dir,
            &name,
            &skill_id,
            &files,
            None,
            PackageAssetKind::Universal,
        )?);
    }
    if request.layout == PackageLayout::Platform {
        let all_payloads: BTreeSet<&str> = inspected
            .bundled
            .values()
            .flat_map(|paths| paths.iter().map(String::as_str))
            .collect();
        for (platform, own) in &inspected.bundled {
            let excluded: BTreeSet<&str> = all_payloads
                .iter()
                .copied()
                .filter(|path| !own.contains(*path))
                .collect();
            let files: Vec<&PackageFile> = inspected
                .files
                .iter()
                .filter(|file| !excluded.contains(file.relative.as_str()))
                .collect();
            let name = platform_package_asset_name(&skill_id, &version, platform);
            let marker = PackageMarker::for_platform(platform).to_canonical_json()?;
            let mut asset = write_package(
                &request.out_dir,
                &name,
                &skill_id,
                &files,
                Some(&marker),
                PackageAssetKind::Platform,
            )?;
            asset.platform = Some(platform.clone());
            assets.push(asset);
        }
        let index = PackageIndex {
            schema_version: PACKAGE_INDEX_SCHEMA_VERSION,
            skill_id: skill_id.clone(),
            version: version.clone(),
            layout: PackageLayout::Platform,
            platforms: assets
                .iter()
                .filter_map(|asset| {
                    asset
                        .platform
                        .clone()
                        .map(|platform| (platform, index_entry(asset)))
                })
                .collect(),
            universal: assets
                .iter()
                .find(|asset| asset.kind == PackageAssetKind::Universal)
                .map(index_entry),
        };
        index.validate(&skill_id, &version)?;
        let name = package_index_asset_name(&skill_id, &version);
        assets.push(write_text_asset(
            &request.out_dir,
            &name,
            &index.to_canonical_json()?,
            PackageAssetKind::Index,
        )?);
    }
    let mut checksum_lines: Vec<String> = assets
        .iter()
        .map(|asset| format!("{}  {}", asset.sha256, asset.name))
        .collect();
    checksum_lines.sort_by(|left, right| left[66..].cmp(&right[66..]));
    let checksums = format!("{}\n", checksum_lines.join("\n"));
    let name = checksums_asset_name(&skill_id, &version);
    assets.push(write_text_asset(
        &request.out_dir,
        &name,
        &checksums,
        PackageAssetKind::Checksums,
    )?);

    Ok(PackageBuildReport {
        skill_id,
        version,
        layout: request.layout,
        assets,
    })
}

/// Convert one built asset into an index entry.
/// 将单个已构建资产转换为索引条目。
fn index_entry(asset: &PackageAsset) -> PackageIndexEntry {
    PackageIndexEntry {
        asset: asset.name.clone(),
        sha256: asset.sha256.clone(),
        size: asset.size,
    }
}

/// Write one deterministic package zip under `{skill_id}/` and verify its member list.
/// 在 `{skill_id}/` 下写入单个确定性包 zip，并校验其成员列表。
fn write_package(
    out_dir: &Path,
    name: &str,
    skill_id: &str,
    files: &[&PackageFile],
    marker: Option<&str>,
    kind: PackageAssetKind,
) -> Result<PackageAsset, String> {
    let path = out_dir.join(name);
    let temp_path = out_dir.join(format!(".{name}.partial"));
    let file = fs::File::create(&temp_path).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_host_visible_path(&temp_path),
            error
        )
    })?;
    let mut writer = ZipWriter::new(file);
    let mut expected = BTreeSet::new();
    let mut entries: Vec<(String, Option<&PackageFile>)> = files
        .iter()
        .map(|file| (format!("{skill_id}/{}", file.relative), Some(*file)))
        .collect();
    if marker.is_some() {
        entries.push((format!("{skill_id}/{PACKAGE_MARKER_FILE_NAME}"), None));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (member, file) in &entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(DateTime::default())
            .unix_permissions(file.map(|file| file_mode(&file.source)).unwrap_or(0o644))
            .large_file(file.is_some_and(|file| file.size >= u32::MAX as u64));
        writer
            .start_file(member.as_str(), options)
            .map_err(|error| format!("Failed to add '{member}' to {name}: {error}"))?;
        match file {
            Some(file) => {
                let mut source = fs::File::open(&file.source).map_err(|error| {
                    format!(
                        "Failed to open {}: {}",
                        render_host_visible_path(&file.source),
                        error
                    )
                })?;
                std::io::copy(&mut source, &mut writer)
                    .map_err(|error| format!("Failed to add '{member}' to {name}: {error}"))?;
            }
            None => writer
                .write_all(marker.unwrap_or_default().as_bytes())
                .map_err(|error| format!("Failed to add '{member}' to {name}: {error}"))?,
        }
        expected.insert(member.clone());
    }
    writer
        .finish()
        .map_err(|error| format!("Failed to finish {name}: {error}"))?;
    verify_package_members(&temp_path, name, &expected)?;
    fs::rename(&temp_path, &path).map_err(|error| {
        format!(
            "Failed to publish {}: {}",
            render_host_visible_path(&path),
            error
        )
    })?;
    let (sha256, size) = file_digest(&path)?;
    Ok(PackageAsset {
        name: name.to_string(),
        kind,
        platform: None,
        sha256,
        size,
        path,
    })
}

/// Return the Unix permission bits to record for one file.
/// 返回单个文件应记录的 Unix 权限位。
#[cfg(unix)]
fn file_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .unwrap_or(0o644)
}

/// Return the Unix permission bits to record for one file.
/// 返回单个文件应记录的 Unix 权限位。
#[cfg(not(unix))]
fn file_mode(_path: &Path) -> u32 {
    0o644
}

/// Reopen one written zip and require exactly the expected unique members.
/// 重新打开单个已写入 zip，要求成员恰好是期望的唯一集合。
fn verify_package_members(
    path: &Path,
    name: &str,
    expected: &BTreeSet<String>,
) -> Result<(), String> {
    let file = fs::File::open(path).map_err(|error| {
        format!(
            "Failed to open {}: {}",
            render_host_visible_path(path),
            error
        )
    })?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("Failed to reopen {name}: {error}"))?;
    let mut actual = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read {name} entry #{index}: {error}"))?;
        let member = entry
            .name()
            .map_err(|error| format!("Failed to decode {name} entry #{index}: {error}"))?
            .into_owned();
        if !actual.insert(member.clone()) {
            return Err(format!("{name} contains duplicate member '{member}'"));
        }
    }
    if &actual != expected {
        return Err(format!(
            "{name} member list does not match the package files"
        ));
    }
    Ok(())
}

/// Write one text asset atomically.
/// 原子写入单个文本资产。
fn write_text_asset(
    out_dir: &Path,
    name: &str,
    text: &str,
    kind: PackageAssetKind,
) -> Result<PackageAsset, String> {
    let path = out_dir.join(name);
    let temp_path = out_dir.join(format!(".{name}.partial"));
    fs::write(&temp_path, text.as_bytes()).map_err(|error| {
        format!(
            "Failed to write {}: {}",
            render_host_visible_path(&temp_path),
            error
        )
    })?;
    fs::rename(&temp_path, &path).map_err(|error| {
        format!(
            "Failed to publish {}: {}",
            render_host_visible_path(&path),
            error
        )
    })?;
    let (sha256, size) = file_digest(&path)?;
    Ok(PackageAsset {
        name: name.to_string(),
        kind,
        platform: None,
        sha256,
        size,
        path,
    })
}

/// Return the lowercase SHA-256 and size of one file.
/// 返回单个文件的小写 SHA-256 与大小。
pub fn file_digest(path: &Path) -> Result<(String, u64), String> {
    let mut file = fs::File::open(path).map_err(|error| {
        format!(
            "Failed to open {}: {}",
            render_host_visible_path(path),
            error
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_host_visible_path(path),
                error
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((digest, size))
}

#[cfg(test)]
mod tests;
