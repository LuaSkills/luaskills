use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::dependency::platform::current_platform_key;
use crate::dependency::types::{
    DependencyDetectionStatus, DependencyScope, DependencySourceType, ResolvedDependencyRequest,
    SkillDependencyKind,
};
use crate::download::archive::{install_downloaded_payload, resolve_dependency_export_target};
use crate::download::manager::{DownloadManager, DownloadManagerConfig, DownloadRequest};
use crate::lua_skill::validate_luaskills_identifier;
use crate::runtime::path::render_host_visible_path;
use crate::runtime_logging::{info as log_info, warn as log_warn};
use crate::runtime_options::RuntimeSkillRoot;
use crate::skill::dependencies::{
    DependencyExportSpec, FfiDependencySpec, LuaDependencySpec, PackageDependencyManifest,
    SkillListIndexFile, ToolDependencySpec,
};

/// Render one dependency-manager filesystem path for user-facing error messages.
/// 为面向用户的依赖管理器错误消息渲染单个文件系统路径。
fn render_dependency_manager_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Dependency-manager configuration shared by dependency resolution and installation phases.
/// 供依赖解析与安装阶段共享使用的依赖管理配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyManagerConfig {
    /// Root directory used to install skill-local executable/tool dependencies.
    /// 用于安装技能本地可执行工具依赖的根目录。
    pub tool_root: PathBuf,
    /// Root directory used to probe host-provided executable/tool dependencies.
    /// 用于探测宿主提供可执行工具依赖的根目录。
    pub host_tool_root: PathBuf,
    /// Root directory used to install Lua package dependencies.
    /// 用于安装 Lua 包依赖的根目录。
    pub lua_root: PathBuf,
    /// Root directory used to probe host-provided Lua package dependencies.
    /// 用于探测宿主提供 Lua 包依赖的根目录。
    pub host_lua_root: PathBuf,
    /// Root directory used to install FFI/native library dependencies.
    /// 用于安装 FFI/原生库依赖的根目录。
    pub ffi_root: PathBuf,
    /// Root directory used to probe host-provided FFI/native dependencies.
    /// 用于探测宿主提供 FFI/原生依赖的根目录。
    pub host_ffi_root: PathBuf,
    /// Root directory used for cached downloads and fetched remote manifests.
    /// 用于缓存下载结果和远程清单的根目录。
    pub download_cache_root: PathBuf,
    /// Whether network downloads are allowed during dependency resolution.
    /// 依赖解析过程中是否允许网络下载。
    pub allow_network_download: bool,
    /// Optional GitHub browser base URL override.
    /// 可选的 GitHub 浏览器下载基址覆盖。
    pub github_base_url: Option<String>,
    /// Optional GitHub API base URL override.
    /// 可选的 GitHub API 基址覆盖。
    pub github_api_base_url: Option<String>,
    /// Optional GitHub token, attached only to requests for the official GitHub API host.
    /// 可选的 GitHub 令牌，只附加到发往官方 GitHub API 主机的请求。
    #[serde(default)]
    pub github_token: Option<crate::host::options::LuaRuntimeSecretString>,
}

/// High-level dependency manager owned by the LuaSkills runtime.
/// 由 LuaSkills 运行时拥有的高层依赖管理器。
pub struct DependencyManager {
    config: DependencyManagerConfig,
    downloader: DownloadManager,
}

/// Version paths allowed by the manifest for one private dependency on this platform.
/// 清单允许单个技能私有依赖在当前平台使用的版本路径。
#[derive(Default)]
struct DependencyCleanupCandidates {
    /// Paths derived directly from the declared version.
    /// 直接由声明版本派生的路径。
    paths: BTreeSet<PathBuf>,
    /// Whether the dependency may reuse an installed version absent from those paths.
    /// 依赖是否可复用这些路径之外的已安装版本。
    unpinned: bool,
}

impl DependencyManager {
    /// Create one dependency manager from a shared configuration object.
    /// 基于共享配置对象创建一个依赖管理器实例。
    pub fn new(config: DependencyManagerConfig) -> Self {
        let downloader = DownloadManager::new(DownloadManagerConfig {
            cache_root: config.download_cache_root.clone(),
            allow_network_download: config.allow_network_download,
            github_base_url: config.github_base_url.clone(),
            github_api_base_url: config.github_api_base_url.clone(),
            github_token: config.github_token.clone(),
        });
        Self { config, downloader }
    }

    /// Ensure all declared dependencies for one skill are installed and ready.
    /// 确保单个 skill 声明的全部依赖已安装且可用。
    ///
    /// `skill_dir` is the installed skill directory that `bundled` payload paths resolve against.
    /// `skill_dir` 是 `bundled` 载荷路径据以解析的已安装技能目录。
    pub fn ensure_skill_dependencies(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        manifest: &PackageDependencyManifest,
    ) -> Result<(), String> {
        self.prepare_skill_dependency_roots(skill_id, skill_dir, manifest)
            .map(|_| ())
    }

    /// Prepare the declared dependencies and return the ready private install roots for this load.
    /// 准备清单声明的依赖并返回本次加载已就绪的技能私有安装根目录。
    /// The package directory resolves bundled paths. Missing optional dependencies have no root.
    /// 技能包目录用于解析内置载荷路径。缺失的可选依赖没有对应根目录。
    pub(crate) fn prepare_skill_dependency_roots(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        manifest: &PackageDependencyManifest,
    ) -> Result<BTreeSet<PathBuf>, String> {
        validate_luaskills_identifier(skill_id, "skill_id")?;
        let platform_key = current_platform_key();
        if platform_key == "unknown" {
            return Err(
                "current platform is not supported by LuaSkills dependency manager".to_string(),
            );
        }

        // Only ready private roots authorize version pruning after this package is loaded.
        // 只有已就绪的技能私有根目录才能作为本次加载后清理版本的依据。
        let mut ready_roots = BTreeSet::new();
        for dependency in &manifest.tool_dependencies {
            ready_roots.extend(self.ensure_tool_dependency(
                skill_id,
                skill_dir,
                dependency,
                platform_key,
            )?);
        }
        for dependency in &manifest.lua_dependencies {
            ready_roots.extend(self.ensure_lua_dependency(
                skill_id,
                skill_dir,
                dependency,
                platform_key,
            )?);
        }
        for dependency in &manifest.ffi_dependencies {
            ready_roots.extend(self.ensure_ffi_dependency(
                skill_id,
                skill_dir,
                dependency,
                platform_key,
            )?);
        }
        Ok(ready_roots)
    }

    /// Ensure the tool declared by `spec` and return its private install root when ready.
    /// 确保 `spec` 声明的工具依赖已就绪，并返回其技能私有安装根目录。
    fn ensure_tool_dependency(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        spec: &ToolDependencySpec,
        platform_key: &str,
    ) -> Result<Option<PathBuf>, String> {
        self.ensure_dependency(
            skill_id,
            skill_dir,
            SkillDependencyKind::Tool,
            spec.name.as_str(),
            spec.version.clone(),
            spec.scope,
            spec.required,
            spec.source.source_type,
            &spec.source,
            spec.package_for_platform(platform_key),
            platform_key,
            self.install_root_for_kind(SkillDependencyKind::Tool, spec.scope),
        )
    }

    /// Ensure the Lua dependency declared by `spec` and return its private install root when ready.
    /// 确保 `spec` 声明的 Lua 依赖已就绪，并返回其技能私有安装根目录。
    fn ensure_lua_dependency(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        spec: &LuaDependencySpec,
        platform_key: &str,
    ) -> Result<Option<PathBuf>, String> {
        self.ensure_dependency(
            skill_id,
            skill_dir,
            SkillDependencyKind::Lua,
            spec.name.as_str(),
            spec.version.clone(),
            spec.scope,
            spec.required,
            spec.source.source_type,
            &spec.source,
            spec.package_for_platform(platform_key),
            platform_key,
            self.install_root_for_kind(SkillDependencyKind::Lua, spec.scope),
        )
    }

    /// Ensure the FFI dependency declared by `spec` and return its private install root when ready.
    /// 确保 `spec` 声明的 FFI 依赖已就绪，并返回其技能私有安装根目录。
    fn ensure_ffi_dependency(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        spec: &FfiDependencySpec,
        platform_key: &str,
    ) -> Result<Option<PathBuf>, String> {
        self.ensure_dependency(
            skill_id,
            skill_dir,
            SkillDependencyKind::Ffi,
            spec.name.as_str(),
            spec.version.clone(),
            spec.scope,
            spec.required,
            spec.source.source_type,
            &spec.source,
            spec.package_for_platform(platform_key),
            platform_key,
            self.install_root_for_kind(SkillDependencyKind::Ffi, spec.scope),
        )
    }

    /// Resolve the concrete install/probe root for one dependency kind and scope pair.
    /// 根据依赖类型与作用域解析实际安装/探测根目录。
    fn install_root_for_kind(&self, kind: SkillDependencyKind, scope: DependencyScope) -> &Path {
        match (kind, scope) {
            (SkillDependencyKind::Tool, DependencyScope::Host) => &self.config.host_tool_root,
            (SkillDependencyKind::Tool, _) => &self.config.tool_root,
            (SkillDependencyKind::Lua, DependencyScope::Host) => &self.config.host_lua_root,
            (SkillDependencyKind::Lua, _) => &self.config.lua_root,
            (SkillDependencyKind::Ffi, DependencyScope::Host) => &self.config.host_ffi_root,
            (SkillDependencyKind::Ffi, _) => &self.config.ffi_root,
        }
    }

    /// Prepare a dependency from its source, package and install root for the requested platform.
    /// 按指定平台、来源、载荷和安装根目录准备单个依赖。
    /// Return its private root when ready. Propagate required failures and log optional failures.
    /// 就绪时返回技能私有根目录。必需依赖失败时返回错误，可选依赖失败时记录警告。
    #[allow(clippy::too_many_arguments)]
    fn ensure_dependency(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        kind: SkillDependencyKind,
        dependency_name: &str,
        version: Option<String>,
        scope: DependencyScope,
        required: bool,
        source_type: DependencySourceType,
        source: &crate::skill::dependencies::DependencySourceSpec,
        package: Option<&crate::skill::dependencies::DependencyPackageSpec>,
        platform_key: &str,
        install_root: &Path,
    ) -> Result<Option<PathBuf>, String> {
        // Readiness comes from the install pipeline, which also handles local reuse.
        // 就绪路径由安装流程返回，该流程也处理本地复用。
        let result = self.make_dependency_ready(
            skill_id,
            skill_dir,
            kind,
            dependency_name,
            version,
            scope,
            source_type,
            source,
            package,
            platform_key,
            install_root,
        );
        match result {
            Ok(ready_root) if scope == DependencyScope::Skill => {
                // Keep the persisted hint for removing dropped dependencies across upgrades.
                // 保留磁盘提示，以便跨版本更新时清理已移除的依赖。
                record_active_dependency_root(&ready_root);
                Ok(Some(ready_root))
            }
            Err(error) if !required => {
                log_warn(format!(
                    "[LuaSkills:dependency] Skill '{}' loads without optional dependency '{}' on {}: {}",
                    skill_id, dependency_name, platform_key, error
                ));
                Ok(None)
            }
            result => result.map(|_| None),
        }
    }

    /// Reuse, install or download one dependency and return the install root that is now ready.
    /// 复用、安装或下载单个依赖，并返回已经就绪的安装根目录。
    #[allow(clippy::too_many_arguments)]
    fn make_dependency_ready(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        kind: SkillDependencyKind,
        dependency_name: &str,
        version: Option<String>,
        scope: DependencyScope,
        source_type: DependencySourceType,
        source: &crate::skill::dependencies::DependencySourceSpec,
        package: Option<&crate::skill::dependencies::DependencyPackageSpec>,
        platform_key: &str,
        install_root: &Path,
    ) -> Result<PathBuf, String> {
        if let Some(existing) = self.find_existing_local_dependency_request(
            skill_id,
            kind,
            dependency_name,
            version.as_deref(),
            scope,
            package,
            platform_key,
            install_root,
        )? {
            log_info(format!(
                "[LuaSkills:dependency] Skill '{}' reuses existing dependency '{}' on {}",
                skill_id, dependency_name, platform_key
            ));
            return Ok(existing.install_root);
        }

        if scope == DependencyScope::Host {
            return Err(format!("host dependency '{}' is missing", dependency_name));
        }
        // Bundled payloads are local files, so only remote sources depend on the network switch.
        // 内置载荷是本地文件，因此只有远程来源受网络开关约束。
        if !self.config.allow_network_download && source_type != DependencySourceType::Bundled {
            return Err(format!(
                "dependency '{}' is missing and network download is disabled",
                dependency_name
            ));
        }

        let resolved_request = self.resolve_dependency_request(
            skill_id,
            skill_dir,
            kind,
            dependency_name,
            version,
            scope,
            source_type,
            source,
            package,
            platform_key,
            install_root,
        )?;

        match self.detect_dependency(&resolved_request)? {
            DependencyDetectionStatus::Present => {
                log_info(format!(
                    "[LuaSkills:dependency] Skill '{}' reuses existing dependency '{}' on {}",
                    skill_id, dependency_name, platform_key
                ));
                Ok(resolved_request.install_root)
            }
            DependencyDetectionStatus::Missing if source_type == DependencySourceType::Bundled => {
                self.install_bundled_dependency(&resolved_request)?;
                log_info(format!(
                    "[LuaSkills:dependency] Installed bundled dependency '{}' for skill '{}' on {}",
                    dependency_name, skill_id, platform_key
                ));
                Ok(resolved_request.install_root)
            }
            DependencyDetectionStatus::Missing => {
                let cache_key = format!(
                    "{}-{}-{}",
                    match kind {
                        SkillDependencyKind::Tool => "tool",
                        SkillDependencyKind::Lua => "lua",
                        SkillDependencyKind::Ffi => "ffi",
                    },
                    dependency_name,
                    platform_key
                );
                let download_path = self.download_dependency_payload(
                    &resolved_request,
                    source_type,
                    cache_key.clone(),
                )?;
                if let Err(first_error) = install_downloaded_payload(
                    &download_path,
                    resolved_request.archive_type,
                    &resolved_request.install_root,
                    &resolved_request.exports,
                ) {
                    log_warn(format!(
                        "[LuaSkills:dependency] Dependency '{}' install from cached archive failed once, retrying after cache cleanup: {}",
                        dependency_name, first_error
                    ));
                    cleanup_failed_dependency_install_attempt(
                        &download_path,
                        &resolved_request.install_root,
                    )?;
                    let redownloaded_path = self.download_dependency_payload(
                        &resolved_request,
                        source_type,
                        cache_key,
                    )?;
                    install_downloaded_payload(
                        &redownloaded_path,
                        resolved_request.archive_type,
                        &resolved_request.install_root,
                        &resolved_request.exports,
                    )
                    .map_err(|retry_error| {
                        format!(
                            "{}. Automatic redownload and reinstall also failed: {}",
                            first_error, retry_error
                        )
                    })?;
                }
                if matches!(
                    self.detect_dependency(&resolved_request)?,
                    DependencyDetectionStatus::Missing
                ) {
                    return Err(format!(
                        "dependency '{}' was downloaded but exported files are still missing",
                        dependency_name
                    ));
                }

                log_info(format!(
                    "[LuaSkills:dependency] Installed dependency '{}' for skill '{}' on {}",
                    dependency_name, skill_id, platform_key
                ));
                Ok(resolved_request.install_root)
            }
        }
    }

    /// Find one already-installed dependency request without resolving any remote source.
    /// 在不解析任何远程来源的前提下查找一个已安装依赖请求。
    #[allow(clippy::too_many_arguments)]
    fn find_existing_local_dependency_request(
        &self,
        skill_id: &str,
        kind: SkillDependencyKind,
        dependency_name: &str,
        version: Option<&str>,
        scope: DependencyScope,
        package: Option<&crate::skill::dependencies::DependencyPackageSpec>,
        platform_key: &str,
        install_root: &Path,
    ) -> Result<Option<ResolvedDependencyRequest>, String> {
        let Some(package) = package else {
            return Ok(None);
        };
        for request in self.local_dependency_probe_requests(
            skill_id,
            kind,
            dependency_name,
            version,
            scope,
            package,
            platform_key,
            install_root,
        ) {
            if matches!(
                self.detect_dependency(&request)?,
                DependencyDetectionStatus::Present
            ) {
                return Ok(Some(request));
            }
        }
        Ok(None)
    }

    /// Build local-only dependency probe requests before network-backed resolution is needed.
    /// 在需要网络解析前构造仅用于本地探测的依赖请求。
    #[allow(clippy::too_many_arguments)]
    fn local_dependency_probe_requests(
        &self,
        skill_id: &str,
        kind: SkillDependencyKind,
        dependency_name: &str,
        version: Option<&str>,
        scope: DependencyScope,
        package: &crate::skill::dependencies::DependencyPackageSpec,
        platform_key: &str,
        install_root: &Path,
    ) -> Vec<ResolvedDependencyRequest> {
        let mut requests = Vec::new();
        for version_candidate in local_dependency_version_candidates(version) {
            requests.extend(local_dependency_probe_request_variants(
                skill_id,
                kind,
                dependency_name,
                version_candidate.as_deref(),
                scope,
                package,
                platform_key,
                install_root,
            ));
        }

        if version.is_none() {
            requests.extend(self.local_unversioned_dependency_probe_requests(
                skill_id,
                kind,
                dependency_name,
                scope,
                package,
                platform_key,
                install_root,
            ));
        }

        requests
    }

    /// Build probe requests for existing version directories when the manifest omits a version.
    /// 当清单省略版本时，根据已有版本目录构造探测请求。
    #[allow(clippy::too_many_arguments)]
    fn local_unversioned_dependency_probe_requests(
        &self,
        skill_id: &str,
        kind: SkillDependencyKind,
        dependency_name: &str,
        scope: DependencyScope,
        package: &crate::skill::dependencies::DependencyPackageSpec,
        platform_key: &str,
        install_root: &Path,
    ) -> Vec<ResolvedDependencyRequest> {
        let normalized_name = normalize_dependency_path_component(dependency_name);
        let dependency_root = match scope {
            DependencyScope::Host => install_root.join(normalized_name),
            DependencyScope::Skill => install_root.join(skill_id).join(normalized_name),
        };
        let version_entries = match fs::read_dir(&dependency_root) {
            Ok(version_entries) => version_entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                log_warn(format!(
                    "[LuaSkills:dependency] Failed to scan local dependency versions under {}: {}",
                    render_dependency_manager_path(&dependency_root),
                    error
                ));
                return Vec::new();
            }
        };

        let normalized_platform = normalize_dependency_path_component(platform_key);
        let mut requests = Vec::new();
        for entry in version_entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log_warn(format!(
                        "[LuaSkills:dependency] Failed to read local dependency version entry under {}: {}",
                        render_dependency_manager_path(&dependency_root),
                        error
                    ));
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    log_warn(format!(
                        "[LuaSkills:dependency] Failed to inspect local dependency version entry {}: {}",
                        render_dependency_manager_path(&entry.path()),
                        error
                    ));
                    continue;
                }
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(version_component) =
                local_dependency_version_component_from_file_name(entry.file_name())
            else {
                continue;
            };
            let candidate_root = entry.path().join(&normalized_platform);
            if !candidate_root.is_dir() {
                continue;
            }
            requests.extend(local_dependency_probe_request_variants_for_root(
                kind,
                dependency_name,
                Some(version_component.as_str()),
                scope,
                platform_key,
                candidate_root,
                package,
            ));
        }
        requests
    }

    /// Resolve one dependency declaration into a concrete install request.
    /// 把单个依赖声明解析成具体可执行的安装请求。
    #[allow(clippy::too_many_arguments)]
    fn resolve_dependency_request(
        &self,
        skill_id: &str,
        skill_dir: &Path,
        kind: SkillDependencyKind,
        dependency_name: &str,
        version: Option<String>,
        scope: DependencyScope,
        source_type: DependencySourceType,
        source: &crate::skill::dependencies::DependencySourceSpec,
        package: Option<&crate::skill::dependencies::DependencyPackageSpec>,
        platform_key: &str,
        install_root: &Path,
    ) -> Result<ResolvedDependencyRequest, String> {
        let resolved_package = match source_type {
            DependencySourceType::GithubRelease
            | DependencySourceType::Url
            | DependencySourceType::Bundled => package.cloned().ok_or_else(|| {
                format!(
                    "dependency '{}' does not declare package metadata for platform '{}'",
                    dependency_name, platform_key
                )
            })?,
            DependencySourceType::SkillList => {
                let skilllist = source.skilllist.as_ref().ok_or_else(|| {
                    format!(
                        "dependency '{}' declares skilllist source but misses skilllist config",
                        dependency_name
                    )
                })?;
                let index_file = self.fetch_skilllist_index(&skilllist.url)?;
                let package_manifest =
                    index_file.packages.get(&skilllist.package).ok_or_else(|| {
                        format!(
                            "dependency '{}' cannot find package '{}' in skilllist",
                            dependency_name, skilllist.package
                        )
                    })?;
                package_manifest
                    .packages
                    .get(platform_key)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "dependency '{}' skilllist package '{}' does not support platform '{}'",
                            dependency_name, skilllist.package, platform_key
                        )
                    })?
            }
        };

        let mut resolved_version = version.clone();
        let mut resolved_tag_name: Option<String> = None;
        let mut expected_sha256: Option<String> = None;
        let download_url = match source_type {
            DependencySourceType::GithubRelease => {
                let github_source = source.github.as_ref().ok_or_else(|| {
                    format!(
                        "dependency '{}' declares github_release source but misses github config",
                        dependency_name
                    )
                })?;
                let asset_name = resolved_package.asset_name.as_ref().ok_or_else(|| {
                    format!(
                        "dependency '{}' github package for platform '{}' must declare asset_name",
                        dependency_name, platform_key
                    )
                })?;
                let resolved_asset = self.downloader.resolve_github_release_asset(
                    github_source,
                    asset_name,
                    version.as_deref(),
                )?;
                resolved_version = Some(resolved_asset.version.clone());
                resolved_tag_name = Some(resolved_asset.tag_name.clone());
                expected_sha256 = resolved_asset.sha256;
                resolved_asset.download_url
            }
            DependencySourceType::Url | DependencySourceType::SkillList => {
                resolved_package.url.clone().ok_or_else(|| {
                    format!(
                        "dependency '{}' platform '{}' must declare url",
                        dependency_name, platform_key
                    )
                })?
            }
            DependencySourceType::Bundled => resolve_bundled_payload_path(
                skill_dir,
                dependency_name,
                platform_key,
                &resolved_package,
            )?,
        };
        // A digest declared by the skill must agree with the one reported by the release.
        // 技能声明的摘要必须与 Release 报告的摘要一致。
        if let Some(declared) = resolved_package.sha256.as_deref() {
            let declared = declared.trim().to_ascii_lowercase();
            if let Some(reported) = expected_sha256.as_deref()
                && reported != declared
            {
                return Err(format!(
                    "dependency '{}' platform '{}' declares sha256 {} but the release reports {}",
                    dependency_name, platform_key, declared, reported
                ));
            }
            expected_sha256 = Some(declared);
        }

        Ok(ResolvedDependencyRequest {
            kind,
            name: dependency_name.to_string(),
            scope,
            platform_key: platform_key.to_string(),
            download_url,
            expected_sha256,
            version: resolved_version.clone(),
            install_root: build_dependency_install_root(
                install_root,
                scope,
                skill_id,
                dependency_name,
                resolved_version.as_deref(),
                platform_key,
            ),
            archive_type: resolved_package.archive_type,
            exports: resolve_export_templates(
                &resolved_package.exports,
                resolved_version.as_deref(),
                resolved_tag_name.as_deref(),
            ),
        })
    }

    /// Install one bundled payload straight from the skill directory without any cache copy.
    /// 直接从技能目录安装单个内置载荷，不经过任何缓存副本。
    fn install_bundled_dependency(
        &self,
        resolved_request: &ResolvedDependencyRequest,
    ) -> Result<(), String> {
        let payload_path = Path::new(resolved_request.download_url.as_str());
        if let Some(expected_sha256) = resolved_request.expected_sha256.as_deref() {
            crate::download::manager::verify_file_sha256(payload_path, expected_sha256)?;
        }
        // The payload belongs to the skill package, so a failed install only removes the target.
        // 载荷属于技能包，因此安装失败时只清理目标目录。
        if let Err(error) = install_downloaded_payload(
            payload_path,
            resolved_request.archive_type,
            &resolved_request.install_root,
            &resolved_request.exports,
        ) {
            remove_failed_dependency_install_root(&resolved_request.install_root)?;
            return Err(error);
        }
        if matches!(
            self.detect_dependency(resolved_request)?,
            DependencyDetectionStatus::Missing
        ) {
            return Err(format!(
                "bundled dependency '{}' was installed but exported files are still missing",
                resolved_request.name
            ));
        }
        Ok(())
    }

    /// Download one dependency payload, verifying its SHA-256 whenever one is known.
    /// 下载单个依赖载荷，只要已知 SHA-256 就进行校验。
    fn download_dependency_payload(
        &self,
        resolved_request: &ResolvedDependencyRequest,
        source_type: DependencySourceType,
        cache_key: String,
    ) -> Result<PathBuf, String> {
        let request = DownloadRequest {
            source_type,
            source_locator: resolved_request.download_url.clone(),
            cache_key,
        };
        match resolved_request.expected_sha256.as_deref() {
            Some(expected_sha256) => self
                .downloader
                .download_with_sha256(&request, expected_sha256),
            None => self.downloader.download(&request),
        }
    }

    /// Remove dependencies owned by one uninstalled skill.
    /// 清理一个已卸载技能拥有的依赖。
    pub fn cleanup_uninstalled_skill_dependencies(
        &self,
        base_dir: &Path,
        override_dir: Option<&Path>,
        removed_skill_id: &str,
        removed_manifest: Option<&PackageDependencyManifest>,
    ) -> Result<(), String> {
        let mut roots = vec![RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: base_dir.to_path_buf(),
        }];
        if let Some(override_dir) = override_dir {
            roots.push(RuntimeSkillRoot {
                name: "PROJECT".to_string(),
                skills_dir: override_dir.to_path_buf(),
            });
        }
        self.cleanup_uninstalled_skill_dependencies_from_roots(
            &roots,
            removed_skill_id,
            removed_manifest,
        )
    }

    /// Remove all private dependency roots for one uninstalled skill using an ordered root chain.
    /// 使用有序根目录链为单个已卸载技能清理全部私有依赖根。
    pub fn cleanup_uninstalled_skill_dependencies_from_roots(
        &self,
        skill_roots: &[RuntimeSkillRoot],
        removed_skill_id: &str,
        removed_manifest: Option<&PackageDependencyManifest>,
    ) -> Result<(), String> {
        validate_luaskills_identifier(removed_skill_id, "removed_skill_id")?;
        self.remove_skill_private_dependency_roots(removed_skill_id)?;
        let _ = (skill_roots, removed_manifest);
        Ok(())
    }

    /// Remove stale skill-private dependency roots after one successful skill update.
    /// 在单个技能更新成功后移除已经过期的技能私有依赖根目录。
    ///
    /// The manifests identify stale pinned versions and removed dependencies. Without a current
    /// load result, retained unpinned dependencies are left intact.
    /// 前后清单用于识别过期的锁定版本和已移除的依赖。没有本次加载结果时，保留仍声明的未锁版本依赖。
    pub fn cleanup_updated_skill_dependencies(
        &self,
        skill_id: &str,
        previous_manifest: Option<&PackageDependencyManifest>,
        current_manifest: Option<&PackageDependencyManifest>,
    ) -> Result<(), String> {
        self.cleanup_updated_skill_dependencies_using_roots(
            skill_id,
            previous_manifest,
            current_manifest,
            None,
        )
    }

    /// Clean up after an update using the private install roots from the same package load.
    /// 用本次技能包加载返回的私有安装根目录执行更新清理。
    /// A supplied set can be empty for a package with no ready private dependencies.
    /// 提供的集合可以为空，表示该包没有已就绪的技能私有依赖。
    pub(crate) fn cleanup_updated_skill_dependencies_using_roots(
        &self,
        skill_id: &str,
        previous_manifest: Option<&PackageDependencyManifest>,
        current_manifest: Option<&PackageDependencyManifest>,
        ready_roots: Option<&BTreeSet<PathBuf>>,
    ) -> Result<(), String> {
        validate_luaskills_identifier(skill_id, "skill_id")?;
        let platform_key = current_platform_key();
        if platform_key == "unknown" {
            return Ok(());
        }
        let normalized_platform = normalize_dependency_path_component(platform_key);

        let previous_roots =
            self.collect_skill_local_dependency_roots(skill_id, previous_manifest, platform_key)?;
        let current_roots =
            self.collect_skill_local_dependency_roots(skill_id, current_manifest, platform_key)?;

        for (dependency_dir, previous_candidates) in &previous_roots {
            // Only the previous declarations are proven stale without a current load result.
            // 没有本次加载结果时，只有旧清单声明的路径可以确定已过期。
            let mut stale = previous_candidates.paths.clone();
            // Keep paths allowed by the new declaration and exact paths prepared for this load.
            // 保留新清单允许的路径以及本次加载实际准备的路径。
            let mut keep = BTreeSet::new();
            if let Some(current_candidates) = current_roots.get(dependency_dir) {
                keep.extend(current_candidates.paths.iter().cloned());
                if let Some(ready_roots) = ready_roots {
                    // Match <dependency>/<version>/<platform> from the prepare result.
                    // 匹配准备结果中的 <依赖>/<版本>/<平台> 路径。
                    let prepared = ready_roots
                        .iter()
                        .filter(|root| {
                            root.parent().and_then(Path::parent) == Some(dependency_dir.as_path())
                        })
                        .collect::<Vec<_>>();
                    if prepared.is_empty() {
                        continue;
                    }
                    keep.extend(prepared.into_iter().cloned());
                    stale.extend(collect_dependency_platform_roots(
                        dependency_dir,
                        &normalized_platform,
                    )?);
                } else if current_candidates.unpinned {
                    // An unpinned package can reuse any installed version, even if the disk hint
                    // still names the previous version. Only a same-load result permits pruning.
                    // 未锁版本的包可以复用任一已安装版本，即使磁盘提示仍指向旧版本。只有同次加载结果
                    // 才能用于清理。
                    continue;
                }
            } else if let Some(recorded_root) =
                read_active_dependency_root(dependency_dir, &normalized_platform)
            {
                stale.insert(recorded_root);
                remove_active_dependency_record(dependency_dir, &normalized_platform);
            }
            for stale_root in &stale {
                // A ready root can also appear in the scan with the casing stored on disk.
                // 扫描结果也可能以磁盘保存的大小写形式指向一个已就绪的根目录。
                if keep
                    .iter()
                    .chain(ready_roots.into_iter().flatten())
                    .any(|root| cleanup_paths_may_alias(root, stale_root))
                {
                    continue;
                }
                remove_stale_dependency_root(stale_root)?;
            }
        }
        Ok(())
    }

    /// Remove all skill-private dependency roots of one skill identifier.
    /// 删除单个技能标识符对应的全部技能私有依赖根目录。
    fn remove_skill_private_dependency_roots(&self, skill_id: &str) -> Result<(), String> {
        for root in [
            self.config.tool_root.join(skill_id),
            self.config.lua_root.join(skill_id),
            self.config.ffi_root.join(skill_id),
        ] {
            remove_skill_private_dependency_root(&root)?;
        }
        Ok(())
    }

    /// Collect the candidate install roots of every skill-local dependency for the current platform.
    /// 收集当前平台下每个技能私有依赖的候选安装根目录。
    ///
    /// Keys are the per-dependency directories that hold one directory per version; values are
    /// the roots a declared version can be installed under (with and without a leading `v`).
    /// 键是每个依赖下按版本分目录的目录；值是声明版本可能安装到的根目录（带与不带前导 `v`）。
    fn collect_skill_local_dependency_roots(
        &self,
        skill_id: &str,
        manifest: Option<&PackageDependencyManifest>,
        platform_key: &str,
    ) -> Result<BTreeMap<PathBuf, DependencyCleanupCandidates>, String> {
        let mut roots = BTreeMap::new();
        let Some(manifest) = manifest else {
            return Ok(roots);
        };

        for spec in &manifest.tool_dependencies {
            self.push_dependency_root_if_applicable(
                &mut roots,
                skill_id,
                SkillDependencyKind::Tool,
                spec.name.as_str(),
                spec.version.as_deref(),
                spec.scope,
                spec.package_for_platform(platform_key).is_some()
                    || spec.source.source_type == DependencySourceType::SkillList,
                platform_key,
            );
        }
        for spec in &manifest.lua_dependencies {
            self.push_dependency_root_if_applicable(
                &mut roots,
                skill_id,
                SkillDependencyKind::Lua,
                spec.name.as_str(),
                spec.version.as_deref(),
                spec.scope,
                spec.package_for_platform(platform_key).is_some()
                    || spec.source.source_type == DependencySourceType::SkillList,
                platform_key,
            );
        }
        for spec in &manifest.ffi_dependencies {
            self.push_dependency_root_if_applicable(
                &mut roots,
                skill_id,
                SkillDependencyKind::Ffi,
                spec.name.as_str(),
                spec.version.as_deref(),
                spec.scope,
                spec.package_for_platform(platform_key).is_some()
                    || spec.source.source_type == DependencySourceType::SkillList,
                platform_key,
            );
        }

        Ok(roots)
    }

    /// Insert the candidate install roots of one dependency that is skill-local and applies to the current platform.
    /// 当依赖属于技能私有且适用于当前平台时，把其候选安装根目录加入集合。
    #[allow(clippy::too_many_arguments)]
    fn push_dependency_root_if_applicable(
        &self,
        roots: &mut BTreeMap<PathBuf, DependencyCleanupCandidates>,
        skill_id: &str,
        kind: SkillDependencyKind,
        dependency_name: &str,
        version: Option<&str>,
        scope: DependencyScope,
        has_platform_package: bool,
        platform_key: &str,
    ) {
        if scope != DependencyScope::Skill || !has_platform_package {
            return;
        }

        let kind_root = self.install_root_for_kind(kind, scope);
        let dependency_dir = kind_root
            .join(skill_id)
            .join(normalize_dependency_path_component(dependency_name));
        let candidates = roots.entry(dependency_dir).or_default();
        candidates.unpinned |= version.is_none();
        // The installer strips a leading `v` from resolved release versions, so both spellings can exist.
        // 安装器会去掉解析出的 Release 版本的前导 `v`，因此两种写法都可能存在。
        for version_candidate in local_dependency_version_candidates(version) {
            candidates.paths.insert(build_dependency_install_root(
                kind_root,
                scope,
                skill_id,
                dependency_name,
                version_candidate.as_deref(),
                platform_key,
            ));
        }
    }

    /// Detect whether one dependency is already installed by checking all declared exports.
    /// 通过检查全部声明的导出文件来判断单个依赖是否已经安装。
    fn detect_dependency(
        &self,
        request: &ResolvedDependencyRequest,
    ) -> Result<DependencyDetectionStatus, String> {
        if request.exports.is_empty() {
            return Err(format!(
                "dependency '{}' must declare at least one export",
                request.name
            ));
        }

        for export in &request.exports {
            // TargetPath reuses the installer boundary so detection cannot inspect outside the dependency root.
            // TargetPath 复用安装器边界，确保存在性检测不会探测依赖根目录之外的路径。
            let target_path =
                resolve_dependency_export_target(&request.install_root, &export.target_path)?;
            if !dependency_export_target_is_ready(
                request.name.as_str(),
                &target_path,
                export.executable,
            )? {
                return Ok(DependencyDetectionStatus::Missing);
            }
        }

        Ok(DependencyDetectionStatus::Present)
    }

    /// Fetch and parse one remote skilllist index file.
    /// 获取并解析单个远程 skilllist 索引文件。
    fn fetch_skilllist_index(&self, url: &str) -> Result<SkillListIndexFile, String> {
        let cache_key = format!(
            "skilllist-{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url)
        );
        // The index is fetched fresh, so one bad response is never served again from the cache.
        // 索引每次都重新获取，一次坏响应不会再从缓存中返回。
        let cached_text = self.downloader.fetch_text_fresh(url, &cache_key)?;
        serde_norway::from_str::<SkillListIndexFile>(&cached_text)
            .map_err(|error| format!("failed to parse skilllist index {}: {}", url, error))
    }
}

/// Ensure one root directory exists before it is used by the dependency manager.
/// 在依赖管理器使用某个根目录前确保其已经存在。
pub fn ensure_directory(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_dependency_manager_path(root),
            error
        )
    })
}

/// Build the final install root of one dependency according to its scope, name, version, and platform.
/// 根据依赖的作用域、名称、版本和平台构造最终安装根目录。
fn build_dependency_install_root(
    root: &Path,
    scope: DependencyScope,
    skill_id: &str,
    dependency_name: &str,
    version: Option<&str>,
    platform_key: &str,
) -> PathBuf {
    let normalized_version = normalize_dependency_path_component(version.unwrap_or("unversioned"));
    let normalized_name = normalize_dependency_path_component(dependency_name);
    let normalized_platform = normalize_dependency_path_component(platform_key);
    match scope {
        DependencyScope::Host => root
            .join(normalized_name)
            .join(normalized_version)
            .join(normalized_platform),
        DependencyScope::Skill => root
            .join(skill_id)
            .join(normalized_name)
            .join(normalized_version)
            .join(normalized_platform),
    }
}

/// Resolve one bundled payload path inside the skill directory, rejecting escapes and links out.
/// 解析技能目录内单个内置载荷路径，拒绝路径逃逸与指向目录外的链接。
fn resolve_bundled_payload_path(
    skill_dir: &Path,
    dependency_name: &str,
    platform_key: &str,
    package: &crate::skill::dependencies::DependencyPackageSpec,
) -> Result<String, String> {
    let relative_path = package.path.as_deref().ok_or_else(|| {
        format!(
            "dependency '{}' bundled package for platform '{}' must declare path",
            dependency_name, platform_key
        )
    })?;
    let canonical_skill_dir = fs::canonicalize(skill_dir).map_err(|error| {
        format!(
            "Failed to resolve skill directory {}: {}",
            render_dependency_manager_path(skill_dir),
            error
        )
    })?;
    let payload_path = crate::runtime::managed_package::resolve_existing_package_file(
        &canonical_skill_dir,
        relative_path,
        &format!("dependency '{}' bundled path", dependency_name),
    )?;
    Ok(payload_path.to_string_lossy().into_owned())
}

/// Build local version candidates that may map to one already-installed dependency root.
/// 构造可能映射到已安装依赖根目录的本地版本候选值。
fn local_dependency_version_candidates(version: Option<&str>) -> Vec<Option<String>> {
    let Some(raw_version) = version else {
        return vec![None];
    };
    let trimmed_version = raw_version.trim();
    let normalized_version = trimmed_version.trim_start_matches('v');
    let mut candidates = Vec::new();
    push_unique_optional_candidate(&mut candidates, Some(trimmed_version.to_string()));
    if normalized_version != trimmed_version {
        push_unique_optional_candidate(&mut candidates, Some(normalized_version.to_string()));
    }
    candidates
}

/// Build local tag candidates used for export-template probing without GitHub access.
/// 构造不访问 GitHub 时用于导出模板探测的本地标签候选值。
fn local_dependency_tag_candidates(version: Option<&str>) -> Vec<Option<String>> {
    let Some(raw_version) = version else {
        return vec![None];
    };
    let trimmed_version = raw_version.trim();
    let normalized_version = trimmed_version.trim_start_matches('v');
    let mut candidates = Vec::new();
    push_unique_optional_candidate(&mut candidates, Some(trimmed_version.to_string()));
    if normalized_version != trimmed_version {
        push_unique_optional_candidate(&mut candidates, Some(normalized_version.to_string()));
    }
    if !normalized_version.is_empty() {
        push_unique_optional_candidate(&mut candidates, Some(format!("v{}", normalized_version)));
    }
    candidates
}

/// Append one optional string candidate while preserving insertion order.
/// 在保留插入顺序的同时追加一个可选字符串候选值。
fn push_unique_optional_candidate(candidates: &mut Vec<Option<String>>, candidate: Option<String>) {
    if !candidates.iter().any(|value| value == &candidate) {
        candidates.push(candidate);
    }
}

/// Parse one local dependency version directory name into a valid version component.
/// 将单个本地依赖版本目录名解析为有效版本片段。
///
/// The file_name parameter is the raw OS directory name returned by directory scanning.
/// file_name 参数是目录扫描返回的原始 OS 目录名。
///
/// Return a UTF-8 version component, or None when the directory name cannot be represented safely.
/// 返回 UTF-8 版本片段；当目录名无法安全表示时返回 None。
fn local_dependency_version_component_from_file_name(file_name: OsString) -> Option<String> {
    file_name.into_string().ok()
}

/// Inspect whether one declared dependency export target is a ready regular file.
/// 检查单个依赖声明导出目标是否为已经就绪的普通文件。
///
/// The dependency_name parameter identifies the dependency whose export target is being checked.
/// dependency_name 参数标识当前正在检查导出目标的依赖。
///
/// The target_path parameter is the concrete installed export path resolved from the request.
/// target_path 参数是从请求中解析出的具体已安装导出路径。
///
/// The executable parameter requires at least one Unix execute bit when the declaration requests it.
/// executable 参数在声明要求时使 Unix 文件必须至少具有一个执行位。
///
/// Return true only for a ready file, false for missing/type/mode mismatches, or an explicit probe error.
/// 仅就绪文件返回 true；缺失、类型或模式不匹配返回 false，探测失败返回显式错误。
fn dependency_export_target_is_ready(
    dependency_name: &str,
    target_path: &Path,
    executable: bool,
) -> Result<bool, String> {
    match fs::symlink_metadata(target_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Ok(false),
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                if executable && metadata.permissions().mode() & 0o111 == 0 {
                    return Ok(false);
                }
            }
            #[cfg(not(unix))]
            {
                // Executable is a declaration-only hint on hosts without Unix mode bits.
                // Executable 在没有 Unix 模式位的宿主上仅作为声明提示。
                let _ = (&metadata, executable);
            }
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect dependency export target '{}' at {}: {}",
            dependency_name,
            render_dependency_manager_path(target_path),
            error
        )),
    }
}

/// File name prefix of the record naming the version directory a skill-private dependency uses.
/// 记录技能私有依赖当前使用的版本目录的文件名前缀。
const ACTIVE_DEPENDENCY_RECORD_PREFIX: &str = ".luaskills-active-";

/// Return the record path for one install root `<dependency>/<version>/<platform>`.
/// 返回安装根目录 `<dependency>/<version>/<platform>` 对应的记录路径。
fn active_dependency_record_path(dependency_dir: &Path, normalized_platform: &str) -> PathBuf {
    dependency_dir.join(format!(
        "{ACTIVE_DEPENDENCY_RECORD_PREFIX}{normalized_platform}"
    ))
}

/// Record that a skill-private dependency now uses `install_root`; failures only log a warning.
/// 记录技能私有依赖当前使用 `install_root`；失败时只记录警告。
fn record_active_dependency_root(install_root: &Path) {
    let (Some(platform), Some(version_dir)) = (install_root.file_name(), install_root.parent())
    else {
        return;
    };
    let (Some(platform), Some(version), Some(dependency_dir)) = (
        platform.to_str(),
        version_dir.file_name().and_then(|value| value.to_str()),
        version_dir.parent(),
    ) else {
        return;
    };
    let record_path = active_dependency_record_path(dependency_dir, platform);
    if fs::read_to_string(&record_path).ok().as_deref() == Some(version) {
        return;
    }
    // Sequence keeps concurrent writers of one process from sharing a temporary file.
    // Sequence 让同一进程内并发的写入方不会共用同一个临时文件。
    static RECORD_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp_path = dependency_dir.join(format!(
        "{ACTIVE_DEPENDENCY_RECORD_PREFIX}{platform}.tmp-{}-{}",
        std::process::id(),
        RECORD_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = fs::write(&temp_path, version).and_then(|()| {
        crate::runtime::file_system::replace_file_atomically(&temp_path, &record_path)
    });
    if let Err(error) = result {
        let _ = fs::remove_file(&temp_path);
        log_warn(format!(
            "[LuaSkills:dependency] Failed to record active dependency root {}: {}",
            render_dependency_manager_path(install_root),
            error
        ));
    }
}

/// Read the install root a skill-private dependency recorded for one platform, if any.
/// 读取技能私有依赖为某个平台记录的安装根目录（如有）。
fn read_active_dependency_root(
    dependency_dir: &Path,
    normalized_platform: &str,
) -> Option<PathBuf> {
    let version = fs::read_to_string(active_dependency_record_path(
        dependency_dir,
        normalized_platform,
    ))
    .ok()?;
    // A record that is not one portable path component is ignored rather than trusted.
    // 不是单个可移植路径片段的记录会被忽略，而不是被信任。
    if !dependency_path_component_is_portable(&version) {
        return None;
    }
    Some(dependency_dir.join(version).join(normalized_platform))
}

/// Remove the active-root record of one dependency that is no longer declared; best effort.
/// 删除不再声明的依赖的活动根记录；按最佳努力进行。
fn remove_active_dependency_record(dependency_dir: &Path, normalized_platform: &str) {
    let _ = fs::remove_file(active_dependency_record_path(
        dependency_dir,
        normalized_platform,
    ));
}

/// Collect `<dependency>/<version>/<platform>` for every version directory of one dependency.
/// 收集单个依赖每个版本目录下的 `<dependency>/<version>/<platform>`。
fn collect_dependency_platform_roots(
    dependency_dir: &Path,
    normalized_platform: &str,
) -> Result<BTreeSet<PathBuf>, String> {
    let entries = match fs::read_dir(dependency_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(format!(
                "Failed to scan dependency versions under {} before update cleanup: {}",
                render_dependency_manager_path(dependency_dir),
                error
            ));
        }
    };
    let mut roots = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "Failed to scan dependency versions under {} before update cleanup: {}",
                render_dependency_manager_path(dependency_dir),
                error
            )
        })?;
        let is_dir = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false);
        let platform_root = entry.path().join(normalized_platform);
        if is_dir && platform_root.is_dir() {
            roots.insert(platform_root);
        }
    }
    Ok(roots)
}

/// Compare cleanup paths conservatively on platforms with case-insensitive installations.
/// 在可能使用不区分大小写的文件系统的平台上，保守比较清理路径。
/// The dependency-specific path components are normalized to ASCII.
/// 依赖专用路径片段已规范为 ASCII。
fn cleanup_paths_may_alias(left: &Path, right: &Path) -> bool {
    left == right
        || (cfg!(any(windows, target_os = "macos"))
            && left.as_os_str().eq_ignore_ascii_case(right.as_os_str()))
}

/// Remove one stale dependency root while treating an already-missing root as cleaned.
/// 删除单个过期依赖根目录；根目录已不存在时视为已经清理完成。
fn remove_stale_dependency_root(stale_root: &Path) -> Result<(), String> {
    if !stale_dependency_root_is_directory(stale_root)? {
        return Ok(());
    }
    fs::remove_dir_all(stale_root).map_err(|error| {
        format!(
            "Failed to remove stale dependency root {}: {}",
            render_dependency_manager_path(stale_root),
            error
        )
    })
}

/// Inspect whether one stale dependency root is a directory before update cleanup.
/// 在更新清理前检查单个过期依赖根是否为目录。
///
/// The stale_root parameter is the concrete outdated dependency root derived from the previous manifest.
/// stale_root 参数是从旧清单派生出的具体过期依赖根目录。
///
/// Return true for an existing directory, false for a confirmed missing root, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失根目录返回 false；探测或类型异常时返回显式错误。
fn stale_dependency_root_is_directory(stale_root: &Path) -> Result<bool, String> {
    match fs::metadata(stale_root) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Stale dependency root is not a directory before update cleanup: {}",
            render_dependency_manager_path(stale_root)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect stale dependency root {} before update cleanup: {}",
            render_dependency_manager_path(stale_root),
            error
        )),
    }
}

/// Remove one skill-private dependency root while treating an already-missing root as cleaned.
/// 删除单个技能私有依赖根目录；根目录已不存在时视为已经清理完成。
fn remove_skill_private_dependency_root(root: &Path) -> Result<(), String> {
    if !skill_private_dependency_root_is_directory(root)? {
        return Ok(());
    }
    fs::remove_dir_all(root).map_err(|error| {
        format!(
            "Failed to remove skill-private dependency root {} before uninstall cleanup: {}",
            render_dependency_manager_path(root),
            error
        )
    })
}

/// Inspect whether one skill-private dependency root is a directory before uninstall cleanup.
/// 在卸载清理前检查单个技能私有依赖根是否为目录。
///
/// The root parameter is one concrete private dependency root derived from the removed skill identifier.
/// root 参数是从已移除技能标识符派生出的单个具体私有依赖根目录。
///
/// Return true for an existing directory, false for a confirmed missing root, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失根目录返回 false；探测或类型异常时返回显式错误。
fn skill_private_dependency_root_is_directory(root: &Path) -> Result<bool, String> {
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Skill-private dependency root is not a directory before uninstall cleanup: {}",
            render_dependency_manager_path(root)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect skill-private dependency root {} before uninstall cleanup: {}",
            render_dependency_manager_path(root),
            error
        )),
    }
}

/// Remove failed dependency-install artifacts before one redownload and reinstall attempt.
/// 在重新下载与重新安装前移除失败依赖安装留下的产物。
fn cleanup_failed_dependency_install_attempt(
    download_path: &Path,
    install_root: &Path,
) -> Result<(), String> {
    remove_failed_dependency_download(download_path)?;
    remove_failed_dependency_install_root(install_root)?;
    Ok(())
}

/// Remove one failed dependency download payload, treating an already-missing file as cleaned.
/// 移除单个失败的依赖下载载荷；文件已不存在时视为已经清理完成。
fn remove_failed_dependency_download(download_path: &Path) -> Result<(), String> {
    match fs::remove_file(download_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to remove failed dependency download {} before redownload: {}",
            render_dependency_manager_path(download_path),
            error
        )),
    }
}

/// Remove one failed dependency install root, treating an already-missing root as cleaned.
/// 移除单个失败的依赖安装根目录；根目录已不存在时视为已经清理完成。
fn remove_failed_dependency_install_root(install_root: &Path) -> Result<(), String> {
    if !failed_dependency_install_root_is_directory(install_root)? {
        return Ok(());
    }
    fs::remove_dir_all(install_root).map_err(|error| {
        format!(
            "Failed to remove failed dependency install root {} before reinstall: {}",
            render_dependency_manager_path(install_root),
            error
        )
    })
}

/// Inspect whether one failed dependency install root is a directory before cleanup.
/// 在清理前检查单个失败依赖安装根是否为目录。
///
/// The install_root parameter is the concrete dependency install root that may be removed before reinstall.
/// install_root 参数是重新安装前可能被删除的具体依赖安装根目录。
///
/// Return true for an existing directory, false for a confirmed missing root, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失根目录返回 false；探测或类型异常时返回显式错误。
fn failed_dependency_install_root_is_directory(install_root: &Path) -> Result<bool, String> {
    match fs::metadata(install_root) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Failed dependency install root is not a directory before reinstall: {}",
            render_dependency_manager_path(install_root)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect failed dependency install root {} before reinstall: {}",
            render_dependency_manager_path(install_root),
            error
        )),
    }
}

/// Build local probe request variants for one dependency root derived from a version candidate.
/// 为一个由版本候选值派生的依赖根目录构造本地探测请求变体。
#[allow(clippy::too_many_arguments)]
fn local_dependency_probe_request_variants(
    skill_id: &str,
    kind: SkillDependencyKind,
    dependency_name: &str,
    version: Option<&str>,
    scope: DependencyScope,
    package: &crate::skill::dependencies::DependencyPackageSpec,
    platform_key: &str,
    install_root: &Path,
) -> Vec<ResolvedDependencyRequest> {
    let candidate_root = build_dependency_install_root(
        install_root,
        scope,
        skill_id,
        dependency_name,
        version,
        platform_key,
    );
    local_dependency_probe_request_variants_for_root(
        kind,
        dependency_name,
        version,
        scope,
        platform_key,
        candidate_root,
        package,
    )
}

/// Build local probe request variants for one concrete dependency root and tag candidate set.
/// 为一个具体依赖根目录和标签候选集合构造本地探测请求变体。
#[allow(clippy::too_many_arguments)]
fn local_dependency_probe_request_variants_for_root(
    kind: SkillDependencyKind,
    dependency_name: &str,
    version: Option<&str>,
    scope: DependencyScope,
    platform_key: &str,
    install_root: PathBuf,
    package: &crate::skill::dependencies::DependencyPackageSpec,
) -> Vec<ResolvedDependencyRequest> {
    local_dependency_tag_candidates(version)
        .into_iter()
        .map(|tag| ResolvedDependencyRequest {
            kind,
            name: dependency_name.to_string(),
            scope,
            platform_key: platform_key.to_string(),
            download_url: String::new(),
            expected_sha256: None,
            version: version.map(str::to_string),
            install_root: install_root.clone(),
            archive_type: package.archive_type,
            exports: resolve_export_templates(&package.exports, version, tag.as_deref()),
        })
        .collect()
}

/// Normalize one dependency path component for stable cross-platform directory generation.
/// 归一化单个依赖路径片段，以生成稳定的跨平台目录结构。
fn normalize_dependency_path_component(raw: &str) -> String {
    if dependency_path_component_is_portable(raw) {
        return raw.to_string();
    }
    // Digest preserves a one-to-one practical identity for unsafe raw values without path syntax.
    // Digest 在不保留路径语法的前提下，为不安全原始值保持实际唯一的身份。
    let digest = Sha256::digest(raw.as_bytes());
    format!("sha256-{digest:x}")
}

/// Return whether one raw dependency component is portable and cannot change path ownership.
/// 返回单个原始依赖片段是否可移植且无法改变路径归属。
///
/// The raw parameter is a dependency name, version, or normalized platform identifier.
/// raw 参数是依赖名称、版本或标准化平台标识符。
///
/// Return true only for non-special ASCII components safe on both Unix and Windows.
/// 仅对 Unix 与 Windows 均安全的非特殊 ASCII 路径片段返回 true。
fn dependency_path_component_is_portable(raw: &str) -> bool {
    if raw.is_empty()
        || raw == "."
        || raw == ".."
        || raw.ends_with('.')
        || !raw
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return false;
    }
    // WindowsStem determines device-name reservation even when a normal-looking extension follows.
    // WindowsStem 用于判断即使带有普通扩展名仍然生效的 Windows 设备名保留规则。
    let windows_stem = raw.split('.').next().unwrap_or(raw);
    !dependency_path_component_is_windows_reserved(windows_stem)
}

/// Return whether one case-insensitive Windows filename stem names a reserved device.
/// 返回单个不区分大小写的 Windows 文件名主干是否为保留设备名。
///
/// The stem parameter excludes any extension because Windows reserves device stems with extensions too.
/// stem 参数不含扩展名，因为 Windows 对带扩展名的设备主干同样保留。
///
/// Return true for CON, PRN, AUX, NUL, COM1-COM9, or LPT1-LPT9.
/// 对 CON、PRN、AUX、NUL、COM1-COM9 或 LPT1-LPT9 返回 true。
fn dependency_path_component_is_windows_reserved(stem: &str) -> bool {
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return true;
    }
    // UpperStem enables compact numeric COM/LPT device matching without locale-sensitive rules.
    // UpperStem 使用不受区域设置影响的规则紧凑匹配数字 COM/LPT 设备名。
    let upper_stem = stem.to_ascii_uppercase();
    // Bytes exposes the fixed ASCII prefix and final device digit.
    // Bytes 暴露固定 ASCII 前缀与末尾设备数字。
    let bytes = upper_stem.as_bytes();
    bytes.len() == 4
        && (bytes.starts_with(b"COM") || bytes.starts_with(b"LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

/// Resolve export-path templates using the final normalized version and tag values.
/// 使用最终标准化版本号与标签值解析导出路径模板。
fn resolve_export_templates(
    exports: &[DependencyExportSpec],
    version: Option<&str>,
    tag: Option<&str>,
) -> Vec<DependencyExportSpec> {
    exports
        .iter()
        .cloned()
        .map(|mut export| {
            export.archive_path = resolve_export_template_field(&export.archive_path, version, tag);
            export.target_path = resolve_export_template_field(&export.target_path, version, tag);
            export
        })
        .collect()
}

/// Resolve one export template field by replacing `{version}` and `{tag}` when values are available.
/// 在值可用时解析单个导出模板字段中的 `{version}` 与 `{tag}`。
fn resolve_export_template_field(raw: &str, version: Option<&str>, tag: Option<&str>) -> String {
    let mut value = raw.to_string();
    if let Some(version) = version {
        value = value.replace("{version}", version);
    }
    if let Some(tag) = tag {
        value = value.replace("{tag}", tag);
    }
    value
}

#[cfg(test)]
mod tests;
