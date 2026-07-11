use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::dependency::platform::current_platform_key;
use crate::dependency::types::{
    DependencyDetectionStatus, DependencyScope, DependencySourceType, ResolvedDependencyRequest,
    SkillDependencyKind,
};
use crate::download::archive::install_downloaded_payload;
use crate::download::manager::{DownloadManager, DownloadManagerConfig, DownloadRequest};
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
}

/// High-level dependency manager owned by the LuaSkills runtime.
/// 由 LuaSkills 运行时拥有的高层依赖管理器。
pub struct DependencyManager {
    config: DependencyManagerConfig,
    downloader: DownloadManager,
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
        });
        Self { config, downloader }
    }

    /// Ensure all declared dependencies for one skill are installed and ready.
    /// 确保单个 skill 声明的全部依赖已安装且可用。
    pub fn ensure_skill_dependencies(
        &self,
        skill_id: &str,
        manifest: &PackageDependencyManifest,
    ) -> Result<(), String> {
        let platform_key = current_platform_key();
        if platform_key == "unknown" {
            return Err(
                "current platform is not supported by LuaSkills dependency manager".to_string(),
            );
        }

        for dependency in &manifest.tool_dependencies {
            self.ensure_tool_dependency(skill_id, dependency, platform_key)?;
        }
        for dependency in &manifest.lua_dependencies {
            self.ensure_lua_dependency(skill_id, dependency, platform_key)?;
        }
        for dependency in &manifest.ffi_dependencies {
            self.ensure_ffi_dependency(skill_id, dependency, platform_key)?;
        }
        Ok(())
    }

    /// Ensure one tool dependency is installed for the current platform.
    /// 确保单个工具依赖在当前平台上已安装完成。
    fn ensure_tool_dependency(
        &self,
        skill_id: &str,
        spec: &ToolDependencySpec,
        platform_key: &str,
    ) -> Result<(), String> {
        self.ensure_dependency(
            skill_id,
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

    /// Ensure one Lua dependency is installed for the current platform.
    /// 确保单个 Lua 依赖在当前平台上已安装完成。
    fn ensure_lua_dependency(
        &self,
        skill_id: &str,
        spec: &LuaDependencySpec,
        platform_key: &str,
    ) -> Result<(), String> {
        self.ensure_dependency(
            skill_id,
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

    /// Ensure one FFI dependency is installed for the current platform.
    /// 确保单个 FFI 依赖在当前平台上已安装完成。
    fn ensure_ffi_dependency(
        &self,
        skill_id: &str,
        spec: &FfiDependencySpec,
        platform_key: &str,
    ) -> Result<(), String> {
        self.ensure_dependency(
            skill_id,
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

    /// Shared dependency ensure pipeline used by tool/lua/ffi dependency kinds.
    /// tool
    /// lua
    /// ffi 三类依赖共用的统一安装确保流程。
    #[allow(clippy::too_many_arguments)]
    fn ensure_dependency(
        &self,
        skill_id: &str,
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
    ) -> Result<(), String> {
        if self
            .find_existing_local_dependency_request(
                skill_id,
                kind,
                dependency_name,
                version.as_deref(),
                scope,
                package,
                platform_key,
                install_root,
            )?
            .is_some()
        {
            log_info(format!(
                "[LuaSkills:dependency] Skill '{}' reuses existing dependency '{}' on {}",
                skill_id, dependency_name, platform_key
            ));
            return Ok(());
        }

        if scope == DependencyScope::Host {
            if required {
                return Err(format!(
                    "required host dependency '{}' is missing",
                    dependency_name
                ));
            }
            log_warn(format!(
                "[LuaSkills:dependency] Optional host dependency '{}' is missing",
                dependency_name
            ));
            return Ok(());
        }
        if !self.config.allow_network_download {
            if required {
                return Err(format!(
                    "required dependency '{}' is missing and network download is disabled",
                    dependency_name
                ));
            }
            log_warn(format!(
                "[LuaSkills:dependency] Optional dependency '{}' is missing and download is disabled",
                dependency_name
            ));
            return Ok(());
        }

        let resolved_request = self.resolve_dependency_request(
            skill_id,
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
                Ok(())
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
                let download_path = self.downloader.download(&DownloadRequest {
                    source_type,
                    source_locator: resolved_request.download_url.clone(),
                    cache_key: cache_key.clone(),
                })?;
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
                    let redownloaded_path = self.downloader.download(&DownloadRequest {
                        source_type,
                        source_locator: resolved_request.download_url.clone(),
                        cache_key,
                    })?;
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
                Ok(())
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
            DependencySourceType::GithubRelease | DependencySourceType::Url => {
                package.cloned().ok_or_else(|| {
                    format!(
                        "dependency '{}' does not declare package metadata for platform '{}'",
                        dependency_name, platform_key
                    )
                })?
            }
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
        };

        Ok(ResolvedDependencyRequest {
            kind,
            name: dependency_name.to_string(),
            scope,
            platform_key: platform_key.to_string(),
            download_url,
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
        self.remove_skill_private_dependency_roots(removed_skill_id)?;
        let _ = (skill_roots, removed_manifest);
        Ok(())
    }

    /// Remove stale skill-private dependency roots after one successful skill update.
    /// 在单个技能更新成功后移除已经过期的技能私有依赖根目录。
    pub fn cleanup_updated_skill_dependencies(
        &self,
        skill_id: &str,
        previous_manifest: Option<&PackageDependencyManifest>,
        current_manifest: Option<&PackageDependencyManifest>,
    ) -> Result<(), String> {
        let platform_key = current_platform_key();
        if platform_key == "unknown" {
            return Ok(());
        }

        let previous_roots =
            self.collect_skill_local_dependency_roots(skill_id, previous_manifest, platform_key)?;
        let current_roots =
            self.collect_skill_local_dependency_roots(skill_id, current_manifest, platform_key)?;

        for stale_root in previous_roots.difference(&current_roots) {
            remove_stale_dependency_root(stale_root)?;
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

    /// Collect all skill-local dependency install roots that are relevant for the current platform.
    /// 收集当前平台下与单个技能相关的全部技能私有依赖安装根目录。
    fn collect_skill_local_dependency_roots(
        &self,
        skill_id: &str,
        manifest: Option<&PackageDependencyManifest>,
        platform_key: &str,
    ) -> Result<BTreeSet<PathBuf>, String> {
        let mut roots = BTreeSet::new();
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
                spec.package_for_platform(platform_key).is_some(),
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
                spec.package_for_platform(platform_key).is_some(),
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
                spec.package_for_platform(platform_key).is_some(),
                platform_key,
            );
        }

        Ok(roots)
    }

    /// Insert one dependency install root when the dependency is skill-local and applicable to the current platform.
    /// 当依赖属于技能私有且适用于当前平台时，把其安装根目录加入集合。
    #[allow(clippy::too_many_arguments)]
    fn push_dependency_root_if_applicable(
        &self,
        roots: &mut BTreeSet<PathBuf>,
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

        let root = build_dependency_install_root(
            self.install_root_for_kind(kind, scope),
            scope,
            skill_id,
            dependency_name,
            version,
            platform_key,
        );
        roots.insert(root);
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
            let target_path = request.install_root.join(
                export
                    .target_path
                    .replace('/', std::path::MAIN_SEPARATOR_STR),
            );
            if !dependency_export_target_exists(request.name.as_str(), &target_path)? {
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
        let cached_text = self.downloader.fetch_text(url, &cache_key)?;
        serde_yaml::from_str::<SkillListIndexFile>(&cached_text)
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

/// Inspect whether one declared dependency export target exists without hiding probe errors.
/// 检查单个依赖声明导出目标是否存在，同时不隐藏探测错误。
///
/// The dependency_name parameter identifies the dependency whose export target is being checked.
/// dependency_name 参数标识当前正在检查导出目标的依赖。
///
/// The target_path parameter is the concrete installed export path resolved from the request.
/// target_path 参数是从请求中解析出的具体已安装导出路径。
///
/// Return true for an existing export, false for a confirmed missing export, or an explicit probe error.
/// 导出已存在返回 true，确认缺失返回 false；探测失败时返回显式错误。
fn dependency_export_target_exists(
    dependency_name: &str,
    target_path: &Path,
) -> Result<bool, String> {
    target_path.try_exists().map_err(|error| {
        format!(
            "Failed to inspect dependency export target '{}' at {}: {}",
            dependency_name,
            render_dependency_manager_path(target_path),
            error
        )
    })
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
    let mut output = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "unnamed".to_string()
    } else {
        output
    }
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
