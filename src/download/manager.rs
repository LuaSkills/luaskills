use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dependency::types::DependencySourceType;
use crate::download::github::{GithubReleaseApiResponse, rewrite_github_download_url};
use crate::runtime::path::render_host_visible_path;
use crate::runtime_logging::info as log_info;
use crate::skill::dependencies::GithubReleaseSourceSpec;

/// Render one downloader filesystem path for user-facing error messages.
/// 为面向用户的下载器错误消息渲染单个文件系统路径。
fn render_download_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Inspect whether one cached download target path is a file without hiding filesystem probe errors.
/// 检查单个下载缓存目标路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The target_path parameter is the deterministic cache path derived from one download request.
/// target_path 参数是从单个下载请求派生出的确定性缓存路径。
///
/// Return true for an existing cache file, false for a confirmed missing cache file, or an explicit probe/type error.
/// 已存在缓存文件返回 true，确认缺失缓存文件返回 false；探测或类型异常时返回显式错误。
fn cached_download_target_is_file(target_path: &Path) -> Result<bool, String> {
    match fs::metadata(target_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Cached download target is not a file: {}",
            render_download_path(target_path)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect cached download target {}: {}",
            render_download_path(target_path),
            error
        )),
    }
}

/// Callback type used by callers to observe byte-level download progress.
/// 调用方用于观察字节级下载进度的回调类型。
pub type DownloadProgressCallback = Arc<dyn Fn(&DownloadProgress) + Send + Sync>;

/// One resolved GitHub release asset with the tag/version metadata needed by install flows.
/// 安装流程需要的单个已解析 GitHub release 资产及其标签/版本元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedGithubReleaseAsset {
    /// Exact GitHub release tag name returned by the upstream API.
    /// 上游 API 返回的精确 GitHub release 标签名。
    pub tag_name: String,
    /// Normalized semantic version string derived from the release tag.
    /// 从 release 标签派生出的标准化语义化版本字符串。
    pub version: String,
    /// Exact asset file name selected from the release payload.
    /// 从 release 载荷中选中的精确资产文件名。
    pub asset_name: String,
    /// Exact browser download URL after optional host-side GitHub URL rewriting.
    /// 经过可选宿主侧 GitHub URL 重写后的精确浏览器下载地址。
    pub download_url: String,
    /// Expected SHA-256 checksum for the selected asset when one checksum manifest is available.
    /// 当存在校验清单时，所选资产对应的期望 SHA-256 校验值。
    pub sha256: Option<String>,
}

/// Download-manager configuration that describes cache roots and upstream policy.
/// 描述缓存根目录与上游策略的下载管理配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadManagerConfig {
    /// Root directory used to cache downloaded archives and remote manifests.
    /// 用于缓存下载归档与远程清单的根目录。
    pub cache_root: PathBuf,
    /// Whether network downloads are allowed.
    /// 是否允许网络下载。
    pub allow_network_download: bool,
    /// Optional GitHub site base URL override.
    /// 可选的 GitHub 站点基址覆盖。
    pub github_base_url: Option<String>,
    /// Optional GitHub API base URL override.
    /// 可选的 GitHub API 基址覆盖。
    pub github_api_base_url: Option<String>,
}

/// One byte-level progress sample emitted by the shared downloader.
/// 共享下载器发出的单个字节级进度样本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Exact source locator currently being downloaded or served from cache.
    /// 当前正在下载或从缓存命中的精确来源定位值。
    pub source_locator: String,
    /// Number of bytes read so far.
    /// 当前已经读取的字节数。
    pub bytes_done: u64,
    /// Optional total byte count reported by the remote server or cache metadata.
    /// 远端服务器或缓存元数据报告的可选总字节数。
    pub bytes_total: Option<u64>,
    /// Whether this progress sample represents a cache hit.
    /// 当前进度样本是否表示缓存命中。
    pub cached: bool,
}

/// One normalized download request consumed by the shared download layer.
/// 由共享下载层消费的单次标准化下载请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    /// Source type of the current download request.
    /// 当前下载请求的来源类型。
    pub source_type: DependencySourceType,
    /// Exact source locator, usually one URL.
    /// 精确来源定位值，通常为一个 URL。
    pub source_locator: String,
    /// Stable cache key used to derive one cache file path.
    /// 用于派生缓存文件路径的稳定缓存键。
    pub cache_key: String,
}

/// Shared downloader used by dependency resolution and install flows.
/// 供依赖解析与安装流程共用的共享下载器。
pub struct DownloadManager {
    config: DownloadManagerConfig,
    progress_callback: Option<DownloadProgressCallback>,
}

impl DownloadManager {
    /// Create one shared downloader from configuration.
    /// 基于配置创建一个共享下载器。
    pub fn new(config: DownloadManagerConfig) -> Self {
        Self {
            config,
            progress_callback: None,
        }
    }

    /// Create one shared downloader with an optional progress callback.
    /// 基于配置与可选进度回调创建一个共享下载器。
    pub fn new_with_progress(
        config: DownloadManagerConfig,
        progress_callback: Option<DownloadProgressCallback>,
    ) -> Self {
        Self {
            config,
            progress_callback,
        }
    }

    /// Download one binary payload into the cache directory and return the cached file path.
    /// 把单个二进制载荷下载到缓存目录并返回缓存文件路径。
    pub fn download(&self, request: &DownloadRequest) -> Result<PathBuf, String> {
        self.ensure_network_allowed()?;
        fs::create_dir_all(&self.config.cache_root).map_err(|error| {
            format!(
                "Failed to create download cache root {}: {}",
                render_download_path(&self.config.cache_root),
                error
            )
        })?;

        let target_path = self.cached_path_for_request(request);
        if cached_download_target_is_file(&target_path)? {
            let metadata = fs::metadata(&target_path).map_err(|error| {
                format!(
                    "Failed to read cached download metadata {}: {}",
                    render_download_path(&target_path),
                    error
                )
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "Download cache path {} exists but is not a regular file",
                    render_download_path(&target_path)
                ));
            }
            if let Some(callback) = self.progress_callback.as_ref() {
                let bytes_done = metadata.len();
                callback(&DownloadProgress {
                    source_locator: request.source_locator.clone(),
                    bytes_done,
                    bytes_total: Some(bytes_done),
                    cached: true,
                });
            }
            return Ok(target_path);
        }

        log_info(format!(
            "[LuaSkills:download] Fetching {} from {}",
            request.cache_key, request.source_locator
        ));
        let source_locator = request.source_locator.clone();
        let progress_callback = self.progress_callback.clone();
        let bytes = self.run_http_task(move |client| {
            let mut response = client
                .get(&source_locator)
                .send()
                .map_err(|error| format!("Failed to download {}: {}", source_locator, error))?
                .error_for_status()
                .map_err(|error| format!("Failed to download {}: {}", source_locator, error))?;
            let bytes_total = response.content_length();
            let mut buffer = [0_u8; 64 * 1024];
            let mut bytes = Vec::new();
            let mut bytes_done = 0_u64;
            loop {
                let read = response
                    .read(&mut buffer)
                    .map_err(|error| format!("Failed to read {}: {}", source_locator, error))?;
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                bytes_done = bytes_done.saturating_add(read as u64);
                if let Some(callback) = progress_callback.as_ref() {
                    callback(&DownloadProgress {
                        source_locator: source_locator.clone(),
                        bytes_done,
                        bytes_total,
                        cached: false,
                    });
                }
            }
            Ok(bytes)
        })?;
        fs::write(&target_path, &bytes).map_err(|error| {
            format!(
                "Failed to write {}: {}",
                render_download_path(&target_path),
                error
            )
        })?;
        Ok(target_path)
    }

    /// Fetch one UTF-8 text resource over HTTP after dropping any stale cached copy.
    /// 删除可能陈旧的缓存副本后通过 HTTP 获取单个 UTF-8 文本资源。
    pub fn fetch_text_fresh(&self, url: &str, cache_key: &str) -> Result<String, String> {
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: url.to_string(),
            cache_key: cache_key.to_string(),
        };
        let cached_path = self.cached_path_for_request(&request);
        remove_stale_text_cache_before_fresh_download(&cached_path)?;
        let downloaded_path = self.download(&request)?;
        fs::read_to_string(&downloaded_path).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_download_path(&downloaded_path),
                error
            )
        })
    }

    /// Download one binary payload and verify one expected SHA-256 checksum.
    /// 下载单个二进制载荷，并验证其期望的 SHA-256 校验值。
    pub fn download_with_sha256(
        &self,
        request: &DownloadRequest,
        expected_sha256: &str,
    ) -> Result<PathBuf, String> {
        let target_path = self.download(request)?;
        if let Err(error) = verify_file_sha256(&target_path, expected_sha256) {
            remove_checksum_mismatched_download(&target_path, "before automatic redownload")
                .map_err(|cleanup_error| format!("{}. {}", error, cleanup_error))?;
            let redownloaded_path = self.download(request)?;
            if let Err(redownload_error) = verify_file_sha256(&redownloaded_path, expected_sha256) {
                remove_checksum_mismatched_download(
                    &redownloaded_path,
                    "after failed automatic redownload",
                )
                .map_err(|cleanup_error| {
                    format!(
                        "{}. Automatic redownload also failed checksum verification: {}. {}",
                        error, redownload_error, cleanup_error
                    )
                })?;
                return Err(format!(
                    "{}. Automatic redownload also failed checksum verification: {}",
                    error, redownload_error
                ));
            }
            return Ok(redownloaded_path);
        }
        Ok(target_path)
    }

    /// Fetch one UTF-8 text resource over HTTP.
    /// 通过 HTTP 获取单个 UTF-8 文本资源。
    pub fn fetch_text(&self, url: &str, cache_key: &str) -> Result<String, String> {
        let cached_path = self.download(&DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: url.to_string(),
            cache_key: cache_key.to_string(),
        })?;
        fs::read_to_string(&cached_path).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_download_path(&cached_path),
                error
            )
        })
    }

    /// Resolve one GitHub release asset into an exact browser download URL.
    /// 把单个 GitHub Release 资产解析为精确浏览器下载地址。
    pub fn resolve_github_release_asset_url(
        &self,
        source: &GithubReleaseSourceSpec,
        asset_name_template: &str,
        expected_version: Option<&str>,
    ) -> Result<String, String> {
        Ok(self
            .resolve_github_release_asset(source, asset_name_template, expected_version)?
            .download_url)
    }

    /// Resolve one GitHub latest-release asset together with its tag and normalized version.
    /// 解析单个 GitHub 最新 release 资产，并返回其标签与标准化版本。
    pub fn resolve_github_release_asset(
        &self,
        source: &GithubReleaseSourceSpec,
        asset_name_template: &str,
        expected_version: Option<&str>,
    ) -> Result<ResolvedGithubReleaseAsset, String> {
        self.ensure_network_allowed()?;
        let release = self.fetch_github_release(source, expected_version)?;
        let normalized_version = normalize_release_version(
            expected_version.unwrap_or(release.tag_name.as_str()),
            release.tag_name.as_str(),
        );
        let expected_asset_name = asset_name_template
            .replace("{version}", normalized_version.as_str())
            .replace("{tag}", release.tag_name.as_str());
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == expected_asset_name)
            .ok_or_else(|| {
                format!(
                    "GitHub release {} does not contain asset '{}'",
                    release.tag_name, expected_asset_name
                )
            })?;
        Ok(ResolvedGithubReleaseAsset {
            tag_name: release.tag_name.clone(),
            version: normalized_version,
            asset_name: asset.name.clone(),
            download_url: rewrite_github_download_url(
                asset.browser_download_url.as_str(),
                self.config.github_base_url.as_deref(),
            ),
            sha256: None,
        })
    }

    /// Resolve one managed GitHub skill release asset together with its checksum metadata.
    /// 解析单个受管 GitHub 技能 release 资产及其校验和元数据。
    pub fn resolve_github_managed_skill_release_asset(
        &self,
        source: &GithubReleaseSourceSpec,
        skill_id: &str,
        expected_version: Option<&str>,
    ) -> Result<ResolvedGithubReleaseAsset, String> {
        let mut resolved = self.resolve_github_release_asset(
            source,
            &format!("{}-v{{version}}-skill.zip", skill_id),
            expected_version,
        )?;
        let release = self.fetch_github_release(source, Some(resolved.version.as_str()))?;
        let checksum_asset_name = format!("{}-v{}-checksums.txt", skill_id, resolved.version);
        let checksum_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == checksum_asset_name)
            .ok_or_else(|| {
                format!(
                    "GitHub release {} does not contain checksum asset '{}'",
                    release.tag_name, checksum_asset_name
                )
            })?;
        let checksum_url = rewrite_github_download_url(
            checksum_asset.browser_download_url.as_str(),
            self.config.github_base_url.as_deref(),
        );
        let checksum_text = self.fetch_text(
            checksum_url.as_str(),
            &format!(
                "github-checksums-{}-{}",
                sanitize_cache_key_fragment(source.repo.as_str()),
                sanitize_cache_key_fragment(release.tag_name.as_str())
            ),
        )?;
        resolved.sha256 = Some(parse_checksum_manifest_for_asset(
            checksum_text.as_str(),
            resolved.asset_name.as_str(),
        )?);
        Ok(resolved)
    }

    /// Ensure the downloader is allowed to hit the network.
    /// 确保当前下载器允许访问网络。
    fn ensure_network_allowed(&self) -> Result<(), String> {
        if self.config.allow_network_download {
            Ok(())
        } else {
            Err("network download is disabled by host policy".to_string())
        }
    }

    /// Fetch one GitHub release payload, preferring an explicit version/tag when provided.
    /// 获取单个 GitHub release 载荷；若提供版本号则优先按显式版本标签解析。
    fn fetch_github_release(
        &self,
        source: &GithubReleaseSourceSpec,
        expected_version: Option<&str>,
    ) -> Result<GithubReleaseApiResponse, String> {
        if let Some(tag_api) = source.tag_api.as_ref() {
            return self.fetch_github_release_from_url(tag_api);
        }

        if let Some(expected_version) = expected_version {
            let trimmed_version = expected_version.trim().trim_start_matches('v');
            if !trimmed_version.is_empty() {
                let candidate_tags = [trimmed_version.to_string(), format!("v{}", trimmed_version)];
                let mut attempted_tag_urls = Vec::new();
                for candidate_tag in candidate_tags {
                    let api_url = build_github_release_tag_api_url(
                        &self.config,
                        source.repo.as_str(),
                        &candidate_tag,
                    );
                    match self.try_fetch_github_release_from_url(&api_url)? {
                        Some(release) => return Ok(release),
                        None => attempted_tag_urls.push(api_url),
                    }
                }
                return Err(format_github_release_tag_not_found_error(
                    source.repo.as_str(),
                    trimmed_version,
                    &attempted_tag_urls,
                ));
            }
        }

        let api_url = build_github_release_api_url(&self.config, source.repo.as_str());
        self.fetch_github_release_from_url(&api_url)
    }

    /// Fetch one GitHub release payload from one exact API URL and fail on any non-success status.
    /// 从精确 API URL 获取单个 GitHub release 载荷，并在非成功状态时直接失败。
    fn fetch_github_release_from_url(
        &self,
        api_url: &str,
    ) -> Result<GithubReleaseApiResponse, String> {
        let api_url = api_url.to_string();
        let request_url = api_url.clone();
        let response_text = self.run_http_task(move |client| {
            client
                .get(&request_url)
                .send()
                .map_err(|error| format!("Failed to query {}: {}", request_url, error))?
                .error_for_status()
                .map_err(|error| format!("Failed to query {}: {}", request_url, error))?
                .text()
                .map_err(|error| format!("Failed to read {}: {}", request_url, error))
        })?;
        serde_json::from_str(&response_text)
            .map_err(|error| format!("Failed to parse {}: {}", api_url, error))
    }

    /// Try to fetch one GitHub release payload from one exact API URL, returning `None` on 404.
    /// 尝试从精确 API URL 获取单个 GitHub release 载荷；遇到 404 时返回 `None`。
    fn try_fetch_github_release_from_url(
        &self,
        api_url: &str,
    ) -> Result<Option<GithubReleaseApiResponse>, String> {
        let api_url = api_url.to_string();
        self.run_http_task(move |client| {
            let response = client
                .get(&api_url)
                .send()
                .map_err(|error| format!("Failed to query {}: {}", api_url, error))?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let response = response
                .error_for_status()
                .map_err(|error| format!("Failed to query {}: {}", api_url, error))?;
            let response_text = response
                .text()
                .map_err(|error| format!("Failed to read {}: {}", api_url, error))?;
            let release = serde_json::from_str(&response_text)
                .map_err(|error| format!("Failed to parse {}: {}", api_url, error))?;
            Ok(Some(release))
        })
    }

    /// Run one blocking HTTP task inside a dedicated OS thread to stay independent from Tokio contexts.
    /// 在专用操作系统线程中运行单个阻塞式 HTTP 任务，以避免依赖 Tokio 上下文。
    fn run_http_task<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(reqwest::blocking::Client) -> Result<T, String> + Send + 'static,
    {
        thread::spawn(move || {
            let client = Self::build_http_client()?;
            operation(client)
        })
        .join()
        .map_err(|_| "Blocking HTTP worker thread panicked".to_string())?
    }

    /// Return the deterministic cache path for one download request.
    /// 返回单个下载请求对应的确定性缓存路径。
    fn cached_path_for_request(&self, request: &DownloadRequest) -> PathBuf {
        let file_extension = infer_download_extension(&request.source_locator);
        self.config
            .cache_root
            .join(format!("{}{}", request.cache_key, file_extension))
    }

    /// Build one blocking HTTP client only when a network operation is actually needed.
    /// 仅在真正需要网络操作时构建一个阻塞式 HTTP 客户端。
    fn build_http_client() -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .user_agent("luaskills/0.1.0")
            .build()
            .map_err(|error| format!("Failed to build HTTP client: {}", error))
    }
}

/// Build the GitHub latest-release API URL for one repository.
/// 为单个仓库构造 GitHub 最新 release API 地址。
fn build_github_release_api_url(config: &DownloadManagerConfig, repo: &str) -> String {
    let normalized_repo = repo
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_matches('/');
    let api_base = config
        .github_api_base_url
        .as_deref()
        .unwrap_or("https://api.github.com")
        .trim_end_matches('/');
    format!("{}/repos/{}/releases/latest", api_base, normalized_repo)
}

/// Build the GitHub release-by-tag API URL for one repository and tag.
/// 为单个仓库和标签构造 GitHub 按标签查询 release 的 API 地址。
fn build_github_release_tag_api_url(
    config: &DownloadManagerConfig,
    repo: &str,
    tag: &str,
) -> String {
    let normalized_repo = repo
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_matches('/');
    let api_base = config
        .github_api_base_url
        .as_deref()
        .unwrap_or("https://api.github.com")
        .trim_end_matches('/');
    format!(
        "{}/repos/{}/releases/tags/{}",
        api_base,
        normalized_repo,
        tag.trim()
    )
}

/// Format a GitHub release lookup failure with every tag endpoint that was actually attempted.
/// 使用所有真实尝试过的标签端点格式化 GitHub release 查询失败信息。
fn format_github_release_tag_not_found_error(
    repo: &str,
    version: &str,
    attempted_tag_urls: &[String],
) -> String {
    if attempted_tag_urls.is_empty() {
        return format!(
            "Failed to resolve GitHub release for repo '{}' and version '{}'; no tag endpoints were attempted",
            repo, version
        );
    }
    format!(
        "Failed to resolve GitHub release for repo '{}' and version '{}'; attempted tag endpoints: {}",
        repo,
        version,
        attempted_tag_urls.join(", ")
    )
}

/// Normalize the effective release version used for asset name interpolation.
/// 归一化用于资产名插值的生效 release 版本字符串。
fn normalize_release_version(expected_version: &str, tag_name: &str) -> String {
    let trimmed_expected = expected_version.trim();
    if !trimmed_expected.is_empty() {
        return trimmed_expected.trim_start_matches('v').to_string();
    }
    tag_name.trim().trim_start_matches('v').to_string()
}

/// Infer a cache file extension from one download URL.
/// 根据下载 URL 推断缓存文件扩展名。
fn infer_download_extension(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") {
        ".tar.gz"
    } else if lower.ends_with(".zip") {
        ".zip"
    } else if let Some(extension) = Path::new(url)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    {
        if extension.eq_ignore_ascii_case("gz") && lower.ends_with(".tar.gz") {
            ".tar.gz"
        } else {
            match extension {
                "txt" => ".txt",
                "yaml" => ".yaml",
                "yml" => ".yml",
                "json" => ".json",
                "dll" => ".dll",
                "so" => ".so",
                "dylib" => ".dylib",
                "lua" => ".lua",
                _ => "",
            }
        }
    } else {
        ""
    }
}

/// Parse one checksum manifest and return the SHA-256 value matching one asset name.
/// 解析单个校验清单，并返回与某个资产名称匹配的 SHA-256 值。
fn parse_checksum_manifest_for_asset(content: &str, asset_name: &str) -> Result<String, String> {
    for (line_index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let line_number = line_index + 1;
        let mut parts = trimmed.split_whitespace();
        let checksum = parts.next().ok_or_else(|| {
            format!(
                "Checksum manifest line {} must contain one SHA-256 value and one asset file name",
                line_number
            )
        })?;
        let file_name = parts
            .next()
            .ok_or_else(|| {
                format!(
                    "Checksum manifest line {} must contain one SHA-256 value and one asset file name",
                    line_number
                )
            })?
            .trim_start_matches('*')
            .trim();
        if parts.next().is_some() {
            return Err(format!(
                "Checksum manifest line {} must contain exactly one SHA-256 value and one asset file name",
                line_number
            ));
        }
        if file_name.is_empty() {
            return Err(format!(
                "Checksum manifest line {} asset file name must not be empty",
                line_number
            ));
        }
        if file_name == asset_name {
            if checksum.len() == 64 && checksum.chars().all(|value| value.is_ascii_hexdigit()) {
                return Ok(checksum.to_ascii_lowercase());
            }
            return Err(format!(
                "Checksum entry for '{}' is not one valid SHA-256 value",
                asset_name
            ));
        }
    }
    Err(format!(
        "Checksum manifest does not contain an entry for '{}'",
        asset_name
    ))
}

/// Verify one downloaded file against one expected SHA-256 checksum.
/// 使用单个期望的 SHA-256 校验值验证一个已下载文件。
fn verify_file_sha256(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err(format!(
            "Expected checksum for {} is not one valid SHA-256 value",
            render_download_path(path)
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("Failed to read {}: {}", render_download_path(path), error))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "Checksum mismatch for {}: expected {}, got {}",
            render_download_path(path),
            expected,
            actual
        ));
    }
    Ok(())
}

/// Remove one checksum-mismatched download before continuing checksum recovery.
/// 在继续 checksum 恢复流程前移除单个 checksum 不匹配的下载文件。
fn remove_checksum_mismatched_download(path: &Path, cleanup_context: &str) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to remove checksum-mismatched download {} {}: {}",
            render_download_path(path),
            cleanup_context,
            error
        )),
    }
}

/// Remove one stale text cache entry before forcing a fresh text download.
/// 强制重新下载文本前移除单个陈旧文本缓存条目。
fn remove_stale_text_cache_before_fresh_download(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to remove stale text cache {} before fresh download: {}",
            render_download_path(path),
            error
        )),
    }
}

/// Sanitize one cache-key fragment so it can safely participate in cache file names.
/// 规范化单个缓存键片段，使其可以安全参与缓存文件名构造。
fn sanitize_cache_key_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DownloadManager, DownloadManagerConfig, DownloadRequest, cached_download_target_is_file,
        format_github_release_tag_not_found_error, parse_checksum_manifest_for_asset,
        remove_checksum_mismatched_download, remove_stale_text_cache_before_fresh_download,
        verify_file_sha256,
    };
    use crate::dependency::types::DependencySourceType;
    use crate::runtime::path::render_host_visible_path;
    use std::path::PathBuf;

    /// Verify that the checksum manifest parser resolves one matching SHA-256 entry.
    /// 验证校验清单解析器能够解析出匹配的 SHA-256 条目。
    #[test]
    fn checksum_manifest_parser_returns_matching_sha256() {
        let checksum = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let manifest = format!(
            "{}  demo-v0.1.0-skill.zip\n{}  other-file.zip\n",
            checksum, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let parsed = parse_checksum_manifest_for_asset(&manifest, "demo-v0.1.0-skill.zip")
            .expect("checksum should be parsed");
        assert_eq!(parsed, checksum);
    }

    /// Verify cached download target probes report invalid path errors.
    /// 验证下载缓存目标探测会报告非法路径错误。
    #[test]
    fn cached_download_target_probe_errors_are_reported() {
        // Cached download target containing one embedded NUL that metadata cannot inspect.
        // 包含内嵌 NUL 且元数据无法探测的下载缓存目标。
        let invalid_target_path = PathBuf::from("invalid\0cache");

        // Error returned before the invalid cache target can behave like a cache miss.
        // 在非法缓存目标表现得像缓存未命中之前返回的错误。
        let error = cached_download_target_is_file(&invalid_target_path)
            .expect_err("invalid cached download target probe should fail");

        assert!(
            error.contains("Failed to inspect cached download target"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
    }

    /// Verify corrupted cache directories are rejected before callers receive a payload path.
    /// 验证损坏的缓存目录会在调用方收到载荷路径前被拒绝。
    #[test]
    fn download_rejects_cached_directory_instead_of_returning_it() {
        // Temporary cache root that isolates the corrupted cache fixture.
        // 隔离损坏缓存夹具的临时缓存根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_download_cached_directory_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp cache root should be created");
        // Download manager configured to allow the cache hit path before any network request.
        // 配置为允许缓存命中路径先于任何网络请求执行的下载管理器。
        let manager = DownloadManager::new(DownloadManagerConfig {
            cache_root: temp_root.clone(),
            allow_network_download: true,
            github_base_url: None,
            github_api_base_url: None,
        });
        // Download request whose deterministic cache path is occupied by a directory.
        // 其确定性缓存路径被目录占用的下载请求。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: "https://example.invalid/payload.txt".to_string(),
            cache_key: "cached-directory".to_string(),
        };
        // Corrupted cache path that must not be returned as a valid payload file.
        // 不应作为有效载荷文件返回的损坏缓存路径。
        let cached_path = manager.cached_path_for_request(&request);
        std::fs::create_dir_all(&cached_path).expect("cached directory should be created");

        let error = manager
            .download(&request)
            .expect_err("cached directory should be rejected");

        assert_eq!(
            error,
            format!(
                "Cached download target is not a file: {}",
                render_host_visible_path(&cached_path)
            )
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify fresh text cache cleanup treats an already-missing cache file as clean.
    /// 验证 fresh 文本缓存清理会把已不存在的缓存文件视为已清理。
    #[test]
    fn fresh_text_cache_cleanup_accepts_missing_file() {
        // Missing cache path used to verify idempotent fresh-download cleanup.
        // 用于验证 fresh 下载清理幂等性的缺失缓存路径。
        let missing_path = std::env::temp_dir().join(format!(
            "luaskills_download_fresh_text_missing_test_{}",
            std::process::id()
        ));
        if missing_path.exists() {
            // Stale fixture cleanup result is intentionally ignored before the missing-path assertion.
            // 缺失路径断言前对陈旧夹具的清理结果有意忽略。
            if missing_path.is_dir() {
                // Directory cleanup is needed only for stale fixtures from interrupted test runs.
                // 目录清理仅用于处理中断测试运行留下的陈旧夹具。
                let _ = std::fs::remove_dir_all(&missing_path);
            } else {
                // File cleanup is needed only for stale fixtures from interrupted test runs.
                // 文件清理仅用于处理中断测试运行留下的陈旧夹具。
                let _ = std::fs::remove_file(&missing_path);
            }
        }

        remove_stale_text_cache_before_fresh_download(&missing_path)
            .expect("missing fresh text cache file should already be clean");
    }

    /// Verify fresh text cache cleanup rejects directories before a fresh download starts.
    /// 验证 fresh 文本缓存清理会在重新下载前拒绝目录路径。
    #[test]
    fn fresh_text_cache_cleanup_rejects_directory() {
        // Temporary root that isolates the fresh text cleanup fixture.
        // 隔离 fresh 文本清理夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_download_fresh_text_directory_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // Directory occupying the cache path that fresh text download wants to remove as a file.
        // 占用 fresh 文本下载期望按文件删除路径的缓存目录。
        let cache_path = temp_root.join("fresh-text-cache");
        std::fs::create_dir_all(&cache_path).expect("cache directory should be created");

        // Error returned before any fresh text download can start.
        // 任何 fresh 文本下载开始前返回的错误。
        let error = remove_stale_text_cache_before_fresh_download(&cache_path)
            .expect_err("directory cache cleanup should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "Failed to remove stale text cache {} before fresh download:",
            render_host_visible_path(&cache_path)
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

    /// Verify malformed checksum manifest rows fail instead of being reported as a missing asset.
    /// 验证格式错误的校验清单行会失败，而不是被误报为缺少资产。
    #[test]
    fn checksum_manifest_parser_rejects_row_without_asset_name() {
        // Manifest row with a checksum but no asset file name.
        // 只有 checksum 而没有资产文件名的清单行。
        let manifest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
        // Parser diagnostic that points to the corrupted manifest row.
        // 指向损坏清单行的解析器诊断。
        let error = parse_checksum_manifest_for_asset(manifest, "demo-v0.1.0-skill.zip")
            .expect_err("malformed checksum row should fail");

        assert_eq!(
            error,
            "Checksum manifest line 1 must contain one SHA-256 value and one asset file name"
        );
    }

    /// Verify checksum manifest rows reject extra fields that cannot belong to generated asset names.
    /// 验证校验清单行会拒绝无法属于生成资产名的多余字段。
    #[test]
    fn checksum_manifest_parser_rejects_row_with_extra_fields() {
        // Manifest row with the expected two fields plus one unexpected token.
        // 包含期望两列以及一个额外字段的清单行。
        let manifest =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef demo.zip extra\n";
        // Parser diagnostic that prevents ambiguous checksum row interpretation.
        // 防止歧义解释校验清单行的解析器诊断。
        let error = parse_checksum_manifest_for_asset(manifest, "demo.zip")
            .expect_err("checksum row with extra fields should fail");

        assert_eq!(
            error,
            "Checksum manifest line 1 must contain exactly one SHA-256 value and one asset file name"
        );
    }

    /// Verify GitHub release version failures report every tag endpoint that was attempted.
    /// 验证 GitHub release 版本解析失败会报告所有已尝试的标签端点。
    #[test]
    fn github_release_tag_not_found_error_reports_all_attempted_endpoints() {
        // Attempted tag endpoints generated by the explicit-version lookup path.
        // 显式版本查询路径生成的已尝试标签端点。
        let attempted_urls = vec![
            "https://api.example.test/repos/acme/tool/releases/tags/1.2.3".to_string(),
            "https://api.example.test/repos/acme/tool/releases/tags/v1.2.3".to_string(),
        ];

        // Error text rendered for the not-found release tag lookup.
        // 未找到 release 标签查询时渲染出的错误文本。
        let error =
            format_github_release_tag_not_found_error("acme/tool", "1.2.3", &attempted_urls);

        assert_eq!(
            error,
            "Failed to resolve GitHub release for repo 'acme/tool' and version '1.2.3'; attempted tag endpoints: https://api.example.test/repos/acme/tool/releases/tags/1.2.3, https://api.example.test/repos/acme/tool/releases/tags/v1.2.3"
        );
    }

    /// Verify that file SHA-256 verification succeeds for one matching payload.
    /// 验证当文件内容匹配时，文件 SHA-256 校验会成功。
    #[test]
    fn file_sha256_verification_succeeds_for_matching_payload() {
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_download_checksum_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        let file_path = temp_root.join("payload.txt");
        std::fs::write(&file_path, b"hello world").expect("payload should be written");
        let checksum = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify_file_sha256(&file_path, checksum).expect("checksum should match");
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify that checksum mismatch errors render paths through the host-visible formatter.
    /// 验证校验不匹配错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn file_sha256_verification_mismatch_error_uses_host_visible_path() {
        // Temporary root that isolates the checksum mismatch fixture.
        // 隔离校验不匹配夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_download_checksum_mismatch_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // Payload path used to trigger a real SHA-256 mismatch.
        // 用于触发真实 SHA-256 不匹配的载荷路径。
        let file_path = temp_root.join("payload.txt");
        std::fs::write(&file_path, b"hello world").expect("payload should be written");
        // Wrong but syntactically valid checksum used to reach the mismatch branch.
        // 用于进入不匹配分支的语法有效但内容错误的校验值。
        let expected_checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        // Expected diagnostic text rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断文本。
        let expected_error = format!(
            "Checksum mismatch for {}: expected {}, got {}",
            render_host_visible_path(&file_path),
            expected_checksum,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        assert_eq!(
            verify_file_sha256(&file_path, expected_checksum)
                .expect_err("checksum mismatch should be reported"),
            expected_error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify checksum recovery cleanup rejects directories before automatic redownload.
    /// 验证 checksum 恢复清理会在自动重新下载前拒绝目录路径。
    #[test]
    fn checksum_mismatch_cleanup_rejects_directory_before_redownload() {
        // Temporary root that isolates the checksum cleanup fixture.
        // 隔离 checksum 清理夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_download_checksum_cleanup_directory_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // Directory occupying the path that checksum recovery wants to remove as a file.
        // 占用 checksum 恢复期望按文件删除路径的目录。
        let download_path = temp_root.join("bad-cache.bin");
        std::fs::create_dir_all(&download_path).expect("download directory should be created");

        // Error returned by checksum cleanup before automatic redownload.
        // 自动重新下载前 checksum 清理返回的错误。
        let error =
            remove_checksum_mismatched_download(&download_path, "before automatic redownload")
                .expect_err("directory cleanup should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "Failed to remove checksum-mismatched download {} before automatic redownload:",
            render_host_visible_path(&download_path)
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

    /// Verify checksum recovery cleanup treats an already-missing file as cleaned.
    /// 验证 checksum 恢复清理会把已不存在的文件视为已清理。
    #[test]
    fn checksum_mismatch_cleanup_accepts_missing_file() {
        // Missing path used to verify idempotent checksum cleanup.
        // 用于验证 checksum 清理幂等性的缺失路径。
        let missing_path = std::env::temp_dir().join(format!(
            "luaskills_download_checksum_cleanup_missing_test_{}",
            std::process::id()
        ));
        if missing_path.exists() {
            // Stale fixture cleanup result is intentionally ignored before the missing-path assertion.
            // 缺失路径断言前对陈旧夹具的清理结果有意忽略。
            if missing_path.is_dir() {
                // Directory cleanup is needed only for stale fixtures from interrupted test runs.
                // 目录清理仅用于处理中断测试运行留下的陈旧夹具。
                let _ = std::fs::remove_dir_all(&missing_path);
            } else {
                // File cleanup is needed only for stale fixtures from interrupted test runs.
                // 文件清理仅用于处理中断测试运行留下的陈旧夹具。
                let _ = std::fs::remove_file(&missing_path);
            }
        }

        remove_checksum_mismatched_download(&missing_path, "before automatic redownload")
            .expect("missing checksum cache file should already be clean");
    }
}
