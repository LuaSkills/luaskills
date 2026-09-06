use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::{self, JoinHandle};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dependency::types::DependencySourceType;
use crate::download::github::{GithubReleaseApiResponse, rewrite_github_download_url};
use crate::runtime::file_system::replace_file_atomically;
use crate::runtime::path::render_host_visible_path;
use crate::runtime_logging::info as log_info;
use crate::skill::dependencies::GithubReleaseSourceSpec;

/// Number of reusable blocking HTTP workers shared by the current process.
/// 当前进程共享的可复用阻塞式 HTTP 工作线程数量。
const DOWNLOAD_HTTP_WORKER_COUNT: usize = 4;

/// Maximum number of pending HTTP tasks retained before synchronous callers apply backpressure.
/// 同步调用方开始施加背压前允许保留的最大待处理 HTTP 任务数。
const DOWNLOAD_HTTP_QUEUE_CAPACITY: usize = 64;

/// Maximum collision retries while creating one same-directory unique download temp file.
/// 创建同目录唯一下载临时文件时允许的最大冲突重试次数。
const DOWNLOAD_TEMP_FILE_CREATE_ATTEMPTS: usize = 128;

/// Process-wide reusable HTTP executor initialized only by the first network operation.
/// 仅由首次网络操作初始化的进程级可复用 HTTP 执行器。
static DOWNLOAD_HTTP_EXECUTOR: OnceLock<Result<DownloadHttpExecutor, String>> = OnceLock::new();

/// Process-wide weak lock registry serializing publication for identical cache target paths.
/// 对相同缓存目标路径的发布进行串行化的进程级弱锁注册表。
static DOWNLOAD_TARGET_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

/// Monotonic suffix used to create collision-resistant temp file names without random dependencies.
/// 用于创建抗冲突临时文件名且不引入随机依赖的单调后缀。
static DOWNLOAD_TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Reusable HTTP client installed on each fixed download worker for deadlock-free nested tasks.
    /// 安装在每个固定下载工作线程上的可复用 HTTP Client，用于无死锁地执行嵌套任务。
    static DOWNLOAD_HTTP_WORKER_CLIENT: RefCell<Option<reqwest::blocking::Client>> = const { RefCell::new(None) };
    /// Cache targets whose synchronous progress callbacks are active on the current thread.
    /// 当前线程正在同步执行进度回调的缓存目标栈。
    static DOWNLOAD_PROGRESS_TARGET_STACK: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
/// Number of process-wide blocking HTTP clients successfully constructed in the current test process.
/// 当前测试进程中成功构造的进程级阻塞 HTTP Client 数量。
static DOWNLOAD_HTTP_CLIENT_BUILDS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
/// Number of reusable HTTP worker threads successfully started in the current test process.
/// 当前测试进程中成功启动的可复用 HTTP 工作线程数量。
static DOWNLOAD_HTTP_WORKER_STARTS: AtomicU64 = AtomicU64::new(0);

/// Type-erased blocking HTTP operation executed by one reusable worker.
/// 由一个可复用工作线程执行的类型擦除阻塞式 HTTP 操作。
type DownloadHttpTask = Box<dyn FnOnce(reqwest::blocking::Client) + Send + 'static>;

/// Fixed process-wide executor that owns one bounded queue and reusable HTTP client clones.
/// 拥有一个有界队列与可复用 HTTP Client 克隆的固定进程级执行器。
struct DownloadHttpExecutor {
    /// Bounded task sender shared by synchronous download callers.
    /// 由同步下载调用方共享的有界任务发送端。
    sender: SyncSender<DownloadHttpTask>,
}

/// Cache lookup policy selected by one public download operation.
/// 由一次公共下载操作选择的缓存查找策略。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DownloadCachePolicy {
    /// Reuse an existing compatible cache file before accessing the network.
    /// 在访问网络前复用已有兼容缓存文件。
    Reuse,
    /// Always request a new payload while preserving the previous file until publication succeeds.
    /// 始终请求新载荷，同时在发布成功前保留旧文件。
    Refresh,
}

/// Content validation required before one cache path may be returned or published.
/// 在返回或发布一个缓存路径前必须执行的内容验证。
#[derive(Clone, Copy)]
enum DownloadValidation<'a> {
    /// Accept any complete response bytes.
    /// 接受任何完整响应字节。
    None,
    /// Require one caller-supplied SHA-256 checksum.
    /// 要求一个调用方提供的 SHA-256 checksum。
    Sha256(&'a str),
    /// Require well-formed UTF-8 text without buffering the whole file for validation.
    /// 要求格式正确的 UTF-8 文本，且验证时不缓冲整个文件。
    Utf8,
}

/// One streamed download failure classified for checksum retry decisions.
/// 为校验重试决策分类的一次流式下载失败。
enum DownloadAttemptError {
    /// Network, response, temp-file, or disk-write failure that must not be retried implicitly.
    /// 不应隐式重试的网络、响应、临时文件或磁盘写入失败。
    Failed(String),
    /// Completed payload whose incremental SHA-256 differs from the expected value.
    /// 已完成载荷的增量 SHA-256 与期望值不一致。
    ChecksumMismatch(String),
}

/// Armed same-directory temp file removed automatically unless atomic publication succeeds.
/// 除非原子发布成功，否则会自动删除的同目录临时文件守卫。
struct PendingDownloadFile {
    /// Unique temporary path located beside the final cache target.
    /// 位于最终缓存目标旁的唯一临时路径。
    path: PathBuf,
    /// Writable file handle retained until flush and durable synchronization complete.
    /// 保留到刷新与持久同步完成的可写文件句柄。
    file: Option<File>,
    /// Whether the temp path has already been consumed by successful publication.
    /// 临时路径是否已经被成功发布操作消费。
    published: bool,
}

/// RAII scope that removes one active progress-callback target on every callback exit path.
/// 在进度回调的每条退出路径上移除一个活动目标的 RAII 作用域。
struct DownloadProgressTargetScope {
    /// Exact cache target pushed by the matching callback invocation.
    /// 由对应回调调用压入的精确缓存目标。
    target_path: PathBuf,
}

impl DownloadProgressTargetScope {
    /// Enter one progress-callback target scope on the current thread.
    /// 在当前线程进入一个进度回调目标作用域。
    ///
    /// The target_path parameter is the cache destination whose lock is owned by the outer operation.
    /// target_path 参数是外层操作持有其锁的缓存目标。
    ///
    /// Returns a guard that restores the previous callback stack when dropped.
    /// 返回一个在析构时恢复先前回调栈的守卫。
    fn enter(target_path: &Path) -> Self {
        // OwnedTarget is retained by both the callback stack and its matching guard.
        // OwnedTarget 同时由回调栈与对应守卫持有。
        let owned_target = target_path.to_path_buf();
        DOWNLOAD_PROGRESS_TARGET_STACK.with(|targets| {
            targets.borrow_mut().push(owned_target.clone());
        });
        Self {
            target_path: owned_target,
        }
    }
}

impl Drop for DownloadProgressTargetScope {
    /// Restore the previous active progress-target stack.
    /// 恢复先前的活动进度目标栈。
    fn drop(&mut self) {
        DOWNLOAD_PROGRESS_TARGET_STACK.with(|targets| {
            // PoppedTarget must match this guard because callback scopes are strictly nested.
            // PoppedTarget 必须匹配当前守卫，因为回调作用域严格嵌套。
            let popped_target = targets.borrow_mut().pop();
            debug_assert_eq!(popped_target.as_deref(), Some(self.target_path.as_path()));
        });
    }
}

/// Return the active progress-callback cache target on the current thread.
/// 返回当前线程上活动进度回调的缓存目标。
///
/// Returns the innermost locked target while a synchronous callback is running, or None outside callbacks.
/// 同步回调运行时返回最内层已锁定目标；回调外返回 None。
fn active_download_progress_target() -> Option<PathBuf> {
    DOWNLOAD_PROGRESS_TARGET_STACK.with(|targets| targets.borrow().last().cloned())
}

/// Invoke one progress callback while exposing its locked target to nested downloader calls.
/// 执行一个进度回调，同时向嵌套下载调用公开其已锁定目标。
///
/// The callback and progress parameters carry the public notification contract.
/// callback 与 progress 参数承载公共通知契约。
///
/// The target_path parameter identifies the outer operation's locked cache destination.
/// target_path 参数标识外层操作已经锁定的缓存目标。
fn invoke_download_progress_callback(
    callback: &DownloadProgressCallback,
    progress: &DownloadProgress,
    target_path: &Path,
) {
    // TargetScope prevents same-target callback re-entry from waiting on its own outer lock.
    // TargetScope 防止同目标回调重入等待自身外层锁。
    let _target_scope = DownloadProgressTargetScope::enter(target_path);
    callback(progress);
}

/// Clone the HTTP client installed on the current fixed worker, when called from a worker task.
/// 当调用发生在固定工作线程任务中时，克隆安装在当前线程上的 HTTP Client。
///
/// Returns None for ordinary caller threads and one shared-pool clone for worker re-entry.
/// 普通调用线程返回 None；工作线程重入时返回共享连接池的克隆。
fn current_download_http_worker_client() -> Option<reqwest::blocking::Client> {
    DOWNLOAD_HTTP_WORKER_CLIENT.with(|client| client.borrow().clone())
}

/// Render one downloader filesystem path for user-facing error messages.
/// 为面向用户的下载器错误消息渲染单个文件系统路径。
fn render_download_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Inspect one cached download target once and return reusable file metadata when present.
/// 单次检查一个下载缓存目标，并在存在时返回可复用的文件元数据。
///
/// The target_path parameter is the deterministic cache path derived from one download request.
/// target_path 参数是从单个下载请求派生出的确定性缓存路径。
///
/// Returns metadata for an existing regular file, None for a confirmed miss, or an explicit probe/type error.
/// 已存在普通文件时返回元数据，确认未命中时返回 None；探测或类型异常时返回显式错误。
fn cached_download_file_metadata(target_path: &Path) -> Result<Option<Metadata>, String> {
    match fs::metadata(target_path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(format!(
            "Cached download target is not a file: {}",
            render_download_path(target_path)
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
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
#[derive(Clone)]
pub struct DownloadManager {
    /// Immutable configuration shared by operation-scoped downloader clones.
    /// 由操作级下载器克隆共享的不可变配置。
    config: Arc<DownloadManagerConfig>,
    /// Optional operation-scoped progress callback.
    /// 可选的操作级进度回调。
    progress_callback: Option<DownloadProgressCallback>,
}

impl DownloadManager {
    /// Create one shared downloader from configuration.
    /// 基于配置创建一个共享下载器。
    pub fn new(config: DownloadManagerConfig) -> Self {
        Self {
            config: Arc::new(config),
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
            config: Arc::new(config),
            progress_callback,
        }
    }

    /// Clone this downloader's shared configuration and replace only its progress callback.
    /// 克隆当前下载器的共享配置，并仅替换其进度回调。
    ///
    /// The progress_callback parameter is scoped to one caller operation.
    /// progress_callback 参数限定于单次调用方操作。
    ///
    /// Returns a cheap downloader clone that reuses process-wide HTTP resources.
    /// 返回一个复用进程级 HTTP 资源的低成本下载器克隆。
    pub(crate) fn with_progress_callback(
        &self,
        progress_callback: Option<DownloadProgressCallback>,
    ) -> Self {
        Self {
            config: self.config.clone(),
            progress_callback,
        }
    }

    /// Download one binary payload into the cache directory and return the cached file path.
    /// 把单个二进制载荷下载到缓存目录并返回缓存文件路径。
    pub fn download(&self, request: &DownloadRequest) -> Result<PathBuf, String> {
        self.download_internal(
            request,
            DownloadValidation::None,
            DownloadCachePolicy::Reuse,
        )
    }

    /// Fetch one fresh UTF-8 text resource while preserving the old cache until validation succeeds.
    /// 获取一份新的 UTF-8 文本资源，并在验证成功前保留旧缓存。
    pub fn fetch_text_fresh(&self, url: &str, cache_key: &str) -> Result<String, String> {
        // Request identifies the fresh text resource and its stable cache destination.
        // Request 标识 fresh 文本资源及其稳定缓存目标。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: url.to_string(),
            cache_key: cache_key.to_string(),
        };
        // DownloadedPath is atomically replaced only after the new response is fully written.
        // DownloadedPath 仅在新响应完整写入后才会被原子替换。
        let downloaded_path = self.download_internal(
            &request,
            DownloadValidation::Utf8,
            DownloadCachePolicy::Refresh,
        )?;
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
        self.download_internal(
            request,
            DownloadValidation::Sha256(expected_sha256),
            DownloadCachePolicy::Reuse,
        )
    }

    /// Fetch one UTF-8 text resource over HTTP.
    /// 通过 HTTP 获取单个 UTF-8 文本资源。
    pub fn fetch_text(&self, url: &str, cache_key: &str) -> Result<String, String> {
        // Request identifies the UTF-8 resource and deterministic cache destination.
        // Request 标识 UTF-8 资源与确定性缓存目标。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: url.to_string(),
            cache_key: cache_key.to_string(),
        };
        // CachedPath is returned only after cached or streamed bytes pass UTF-8 validation.
        // CachedPath 仅在缓存或流式字节通过 UTF-8 验证后返回。
        let cached_path = self.download_internal(
            &request,
            DownloadValidation::Utf8,
            DownloadCachePolicy::Reuse,
        )?;
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

    /// Resolve cache state, stream any required network payload, and atomically publish it.
    /// 解析缓存状态、流式处理所需网络载荷并原子发布。
    ///
    /// The request parameter identifies the source and deterministic cache target.
    /// request 参数标识来源与确定性缓存目标。
    ///
    /// The validation parameter selects binary, SHA-256, or UTF-8 admission checks.
    /// validation 参数选择二进制、SHA-256 或 UTF-8 准入检查。
    ///
    /// The cache_policy parameter selects cache reuse or forced refresh semantics.
    /// cache_policy 参数选择缓存复用或强制刷新语义。
    ///
    /// Returns the final cache path only after a cache hit or successful atomic publication.
    /// 仅在缓存命中或成功原子发布后返回最终缓存路径。
    fn download_internal(
        &self,
        request: &DownloadRequest,
        validation: DownloadValidation<'_>,
        cache_policy: DownloadCachePolicy,
    ) -> Result<PathBuf, String> {
        self.ensure_network_allowed()?;
        fs::create_dir_all(&self.config.cache_root).map_err(|error| {
            format!(
                "Failed to create download cache root {}: {}",
                render_download_path(&self.config.cache_root),
                error
            )
        })?;

        // TargetPath is the only stable cache path exposed to callers.
        // TargetPath 是唯一向调用方暴露的稳定缓存路径。
        let target_path = self.cached_path_for_request(request);
        if let Some(active_target) = active_download_progress_target() {
            return Err(format!(
                "Download progress callback for active cache target {} cannot start nested download for {}",
                render_download_path(&active_target),
                render_download_path(&target_path)
            ));
        }
        // ExpectedSha256 is normalized before cache lookup or network work begins.
        // ExpectedSha256 在缓存查找或网络工作开始前完成归一化。
        let expected_sha256 = match validation {
            DownloadValidation::Sha256(value) => {
                Some(normalize_expected_sha256(&target_path, value)?)
            }
            DownloadValidation::None | DownloadValidation::Utf8 => None,
        };
        // RequiresUtf8 records the text-specific pre-publication validation contract.
        // RequiresUtf8 记录文本专属的发布前验证契约。
        let requires_utf8 = matches!(validation, DownloadValidation::Utf8);
        // TargetLock serializes cache inspection and publication for this exact target.
        // TargetLock 将当前精确目标的缓存检查与发布串行化。
        let target_lock = shared_download_target_lock(&target_path);
        // TargetGuard remains held until the selected cache result is stable.
        // TargetGuard 持有到所选缓存结果稳定为止。
        let _target_guard = lock_download_target(&target_lock);
        // CachedMetadata performs the target type validation exactly once under the lock.
        // CachedMetadata 在锁内只执行一次目标类型验证。
        let cached_metadata = cached_download_file_metadata(&target_path)?;
        // InitialChecksumError retains a mismatched cached-file diagnostic across one redownload.
        // InitialChecksumError 在一次重新下载期间保留缓存文件不匹配诊断。
        let mut initial_checksum_error = None;

        if cache_policy == DownloadCachePolicy::Reuse
            && let Some(metadata) = cached_metadata.as_ref()
        {
            if let Some(expected_sha256) = expected_sha256.as_deref() {
                match verify_file_sha256_normalized(&target_path, expected_sha256) {
                    Ok(()) => {
                        self.emit_cached_progress(request, metadata, &target_path);
                        return Ok(target_path);
                    }
                    Err(error) => initial_checksum_error = Some(error),
                }
            } else if requires_utf8 {
                validate_file_utf8(&target_path)?;
                self.emit_cached_progress(request, metadata, &target_path);
                return Ok(target_path);
            } else {
                self.emit_cached_progress(request, metadata, &target_path);
                return Ok(target_path);
            }
        }

        // AttemptCount preserves one automatic checksum retry for a newly downloaded payload.
        // AttemptCount 为新下载载荷保留一次自动 checksum 重试。
        let attempt_count = if expected_sha256.is_some() && initial_checksum_error.is_none() {
            2
        } else {
            1
        };
        // FirstDownloadChecksumError retains the first network checksum failure for final context.
        // FirstDownloadChecksumError 保留首次网络 checksum 失败，供最终错误提供上下文。
        let mut first_download_checksum_error = None;

        for attempt_index in 0..attempt_count {
            log_info(format!(
                "[LuaSkills:download] Fetching {} from {}",
                request.cache_key, request.source_locator
            ));
            // SourceLocator transfers one immutable request source into the blocking task.
            // SourceLocator 将一个不可变请求来源转移到阻塞任务中。
            let source_locator = request.source_locator.clone();
            // AttemptTarget identifies both the temp-file parent and final checksum diagnostic path.
            // AttemptTarget 同时标识临时文件父目录与最终 checksum 诊断路径。
            let attempt_target = target_path.clone();
            // AttemptExpected carries the normalized expected checksum into incremental hashing.
            // AttemptExpected 将归一化期望 checksum 传入增量哈希流程。
            let attempt_expected = expected_sha256.clone();
            // ProgressCallback is cloned once per request attempt, never once per response chunk.
            // ProgressCallback 每次请求尝试仅克隆一次，不会按响应块重复克隆。
            let progress_callback = self.progress_callback.clone();
            // AttemptResult keeps checksum mismatch classification outside generic executor errors.
            // AttemptResult 在通用执行器错误之外保留 checksum 不匹配分类。
            let attempt_result = self.run_http_task(move |client| {
                Ok(stream_download_to_temp_file(
                    client,
                    source_locator,
                    attempt_target,
                    attempt_expected,
                    progress_callback,
                ))
            })?;

            match attempt_result {
                Ok(pending_file) => {
                    if requires_utf8 {
                        validate_file_utf8(pending_file.path())?;
                    }
                    pending_file.publish(&target_path)?;
                    return Ok(target_path);
                }
                Err(DownloadAttemptError::Failed(error)) => return Err(error),
                Err(DownloadAttemptError::ChecksumMismatch(error)) => {
                    if attempt_index + 1 < attempt_count {
                        first_download_checksum_error = Some(error);
                        continue;
                    }
                    // FirstError is either the rejected cache or the first rejected network payload.
                    // FirstError 是被拒绝的缓存或第一个被拒绝的网络载荷。
                    let first_error = initial_checksum_error
                        .as_ref()
                        .or(first_download_checksum_error.as_ref());
                    return Err(match first_error {
                        Some(first_error) => format!(
                            "{}. Automatic redownload also failed checksum verification: {}",
                            first_error, error
                        ),
                        None => error,
                    });
                }
            }
        }

        Err("Download attempt loop completed without a result".to_string())
    }

    /// Emit one cache-hit progress sample without re-reading target metadata.
    /// 在不重复读取目标元数据的情况下发出一个缓存命中进度样本。
    ///
    /// The request parameter supplies the source locator reported to the caller.
    /// request 参数提供向调用方报告的来源定位值。
    ///
    /// The metadata parameter is the single cache probe result reused for byte counts.
    /// metadata 参数是用于复用字节数的单次缓存探测结果。
    fn emit_cached_progress(
        &self,
        request: &DownloadRequest,
        metadata: &Metadata,
        target_path: &Path,
    ) {
        if let Some(callback) = self.progress_callback.as_ref() {
            // BytesDone is the cached file length already available in metadata.
            // BytesDone 是元数据中已经可用的缓存文件长度。
            let bytes_done = metadata.len();
            // Progress is built once before entering the target-aware callback scope.
            // Progress 在进入目标感知的回调作用域前只构造一次。
            let progress = DownloadProgress {
                source_locator: request.source_locator.clone(),
                bytes_done,
                bytes_total: Some(bytes_done),
                cached: true,
            };
            invoke_download_progress_callback(callback, &progress, target_path);
        }
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

    /// Run one blocking HTTP task on the bounded process-wide executor.
    /// 在有界进程级执行器上运行单个阻塞式 HTTP 任务。
    ///
    /// The operation parameter receives a cheap clone of the shared blocking client.
    /// operation 参数接收共享阻塞式 Client 的低成本克隆。
    ///
    /// Returns the operation result, executor startup/queue error, or a contained panic error.
    /// 返回操作结果、执行器启动/队列错误或被隔离的 panic 错误。
    fn run_http_task<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(reqwest::blocking::Client) -> Result<T, String> + Send + 'static,
    {
        if let Some(worker_client) = current_download_http_worker_client() {
            return panic::catch_unwind(AssertUnwindSafe(|| operation(worker_client)))
                .map_err(|_| "Blocking HTTP worker task panicked".to_string())
                .and_then(|result| result);
        }
        // Executor is initialized lazily so cache-only and network-disabled flows build no client.
        // Executor 采用惰性初始化，使纯缓存与禁用网络流程不会构造 Client。
        let executor = download_http_executor()?;
        // Result channel carries one type-specific reply back to this synchronous caller.
        // Result 通道把一个具体类型的回复传回当前同步调用方。
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        // Task contains the operation panic so one faulty task cannot shrink the fixed worker pool.
        // Task 隔离操作 panic，防止单个错误任务缩减固定工作池。
        let task: DownloadHttpTask = Box::new(move |client| {
            // OperationResult converts a panic into the same explicit string error surface.
            // OperationResult 将 panic 转换为相同的显式字符串错误界面。
            let operation_result = panic::catch_unwind(AssertUnwindSafe(|| operation(client)))
                .map_err(|_| "Blocking HTTP worker task panicked".to_string())
                .and_then(|result| result);
            // A dropped receiver means the synchronous caller has already abandoned this result.
            // 接收端已释放只表示同步调用方已经放弃当前结果。
            let _ = result_sender.send(operation_result);
        });
        executor.submit(task)?;
        result_receiver
            .recv()
            .map_err(|_| "Blocking HTTP worker result channel disconnected".to_string())?
    }

    /// Return the deterministic cache path for one download request.
    /// 返回单个下载请求对应的确定性缓存路径。
    fn cached_path_for_request(&self, request: &DownloadRequest) -> PathBuf {
        // FileExtension preserves the bounded allow-listed suffix inferred from the source URL.
        // FileExtension 保留从来源 URL 推导出的有界白名单后缀。
        let file_extension = infer_download_extension(&request.source_locator);
        // CacheKeyDigest prevents caller-controlled separators or absolute prefixes from escaping the cache root.
        // CacheKeyDigest 防止调用方控制的分隔符或绝对路径前缀逃逸缓存根目录。
        let cache_key_digest = format!("{:x}", Sha256::digest(request.cache_key.as_bytes()));
        self.config
            .cache_root
            .join(format!("{cache_key_digest}{file_extension}"))
    }

    /// Build one blocking HTTP client only when a network operation is actually needed.
    /// 仅在真正需要网络操作时构建一个阻塞式 HTTP 客户端。
    fn build_http_client() -> Result<reqwest::blocking::Client, String> {
        // Client is constructed once and cloned by workers while preserving one connection pool.
        // Client 仅构造一次并由工作线程克隆，同时保留同一个连接池。
        let client = reqwest::blocking::Client::builder()
            .user_agent("luaskills/0.1.0")
            .build()
            .map_err(|error| format!("Failed to build HTTP client: {}", error))?;
        #[cfg(test)]
        DOWNLOAD_HTTP_CLIENT_BUILDS.fetch_add(1, Ordering::SeqCst);
        Ok(client)
    }
}

impl DownloadHttpExecutor {
    /// Start the fixed worker set and one shared blocking HTTP client transactionally.
    /// 以事务方式启动固定工作线程集合与一个共享阻塞式 HTTP Client。
    ///
    /// Returns a usable executor or a startup error after joining any partially started workers.
    /// 返回可用执行器；若启动失败，则等待已部分启动的工作线程退出后返回错误。
    fn start() -> Result<Self, String> {
        // Client owns the reusable connection, DNS, and TLS session pools.
        // Client 拥有可复用的连接、DNS 与 TLS 会话池。
        let client = DownloadManager::build_http_client()?;
        // Sender and Receiver form the bounded queue applying backpressure at capacity.
        // Sender 与 Receiver 构成达到容量后施加背压的有界队列。
        let (sender, receiver) = mpsc::sync_channel(DOWNLOAD_HTTP_QUEUE_CAPACITY);
        // SharedReceiver serializes the brief receive operation, not task execution.
        // SharedReceiver 仅串行化短暂的接收操作，不串行化任务执行。
        let shared_receiver = Arc::new(Mutex::new(receiver));
        // Handles retain partially started workers until startup is known to be successful.
        // Handles 保留已部分启动的工作线程，直到确认启动成功。
        let mut handles: Vec<JoinHandle<()>> = Vec::with_capacity(DOWNLOAD_HTTP_WORKER_COUNT);

        for worker_index in 0..DOWNLOAD_HTTP_WORKER_COUNT {
            // WorkerReceiver shares the bounded queue with every fixed worker.
            // WorkerReceiver 与所有固定工作线程共享有界队列。
            let worker_receiver = shared_receiver.clone();
            // WorkerClient is a cheap clone backed by the same reqwest connection pool.
            // WorkerClient 是由同一 reqwest 连接池支持的低成本克隆。
            let worker_client = client.clone();
            // WorkerName provides a stable diagnostic identity without per-request thread churn.
            // WorkerName 提供稳定诊断标识，同时避免逐请求线程抖动。
            let worker_name = format!("luaskills-download-http-{worker_index}");
            // SpawnResult is handled transactionally so partial pools cannot remain detached.
            // SpawnResult 以事务方式处理，防止部分工作池被遗留为游离线程。
            let spawn_result = thread::Builder::new()
                .name(worker_name)
                .spawn(move || run_download_http_worker(worker_receiver, worker_client));
            match spawn_result {
                Ok(handle) => {
                    #[cfg(test)]
                    DOWNLOAD_HTTP_WORKER_STARTS.fetch_add(1, Ordering::SeqCst);
                    handles.push(handle);
                }
                Err(error) => {
                    drop(sender);
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(format!("Failed to start blocking HTTP worker: {error}"));
                }
            }
        }

        // Join handles are intentionally detached after all fixed workers start successfully.
        // 所有固定工作线程成功启动后，有意释放 join 句柄使其后台运行。
        drop(handles);
        Ok(Self { sender })
    }

    /// Submit one task to the bounded worker queue.
    /// 向有界工作队列提交一个任务。
    ///
    /// The task parameter owns one complete blocking HTTP operation.
    /// task 参数拥有一个完整的阻塞式 HTTP 操作。
    ///
    /// Returns success after enqueueing or an explicit disconnected-pool error.
    /// 入队成功后返回成功，否则返回显式工作池断连错误。
    fn submit(&self, task: DownloadHttpTask) -> Result<(), String> {
        self.sender
            .send(task)
            .map_err(|_| "Blocking HTTP worker queue disconnected".to_string())
    }
}

/// Run tasks from the bounded shared queue until all executor senders disconnect.
/// 从有界共享队列运行任务，直到所有执行器发送端断开。
///
/// The receiver parameter is shared only around each receive operation.
/// receiver 参数仅在每次接收操作期间共享。
///
/// The client parameter is one clone backed by the process-wide connection pool.
/// client 参数是由进程级连接池支持的一个克隆。
fn run_download_http_worker(
    receiver: Arc<Mutex<Receiver<DownloadHttpTask>>>,
    client: reqwest::blocking::Client,
) {
    DOWNLOAD_HTTP_WORKER_CLIENT.with(|worker_client| {
        // InstalledClient keeps nested worker-originated HTTP tasks on the current worker.
        // InstalledClient 让工作线程发起的嵌套 HTTP 任务继续在当前工作线程执行。
        *worker_client.borrow_mut() = Some(client.clone());
    });
    loop {
        // TaskResult releases the receiver lock before any network operation executes.
        // TaskResult 会在执行任何网络操作前释放接收器锁。
        let task_result = {
            let receiver_guard = lock_download_http_receiver(&receiver);
            receiver_guard.recv()
        };
        match task_result {
            Ok(task) => task(client.clone()),
            Err(_) => return,
        }
    }
}

/// Acquire the shared HTTP task receiver and recover after a worker-side poison event.
/// 获取共享 HTTP 任务接收器，并在工作线程侧发生 poison 后恢复。
///
/// The receiver parameter owns the single bounded-queue receiving endpoint.
/// receiver 参数拥有有界队列的唯一接收端。
///
/// Returns the protected receiving endpoint.
/// 返回受保护的接收端。
fn lock_download_http_receiver(
    receiver: &Mutex<Receiver<DownloadHttpTask>>,
) -> MutexGuard<'_, Receiver<DownloadHttpTask>> {
    receiver
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Return the lazily initialized process-wide blocking HTTP executor.
/// 返回惰性初始化的进程级阻塞式 HTTP 执行器。
///
/// Returns the shared executor or the stable startup failure from its first initialization.
/// 返回共享执行器，或返回首次初始化产生的稳定启动失败。
fn download_http_executor() -> Result<&'static DownloadHttpExecutor, String> {
    match DOWNLOAD_HTTP_EXECUTOR.get_or_init(DownloadHttpExecutor::start) {
        Ok(executor) => Ok(executor),
        Err(error) => Err(error.clone()),
    }
}

/// Return the process-wide weak lock registry for deterministic cache targets.
/// 返回确定性缓存目标使用的进程级弱锁注册表。
///
/// Returns the lazily initialized registry mutex.
/// 返回惰性初始化的注册表互斥量。
fn download_target_lock_registry() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<()>>>> {
    DOWNLOAD_TARGET_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Acquire the cache-target lock registry and recover it after poisoning.
/// 获取缓存目标锁注册表，并在 poison 后恢复。
///
/// Returns the protected weak-lock map.
/// 返回受保护的弱锁映射。
fn lock_download_target_registry() -> MutexGuard<'static, HashMap<PathBuf, Weak<Mutex<()>>>> {
    download_target_lock_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Return one shared mutex for the exact deterministic cache target path.
/// 为精确的确定性缓存目标路径返回一个共享互斥量。
///
/// The target_path parameter is the final cache path used by all contenders.
/// target_path 参数是所有竞争方使用的最终缓存路径。
///
/// Returns one strong lock owner while inactive historical entries are discarded.
/// 返回一个强锁所有者，同时清理已失活的历史注册项。
fn shared_download_target_lock(target_path: &Path) -> Arc<Mutex<()>> {
    // Registry serializes weak-entry cleanup, upgrade, and insertion.
    // Registry 将弱条目清理、升级与插入串行化。
    let mut registry = lock_download_target_registry();
    registry.retain(|_, weak_lock| weak_lock.strong_count() > 0);
    if let Some(target_lock) = registry.get(target_path).and_then(Weak::upgrade) {
        return target_lock;
    }
    // TargetLock is the unique live mutex for this exact path generation.
    // TargetLock 是当前精确路径代的唯一活动互斥量。
    let target_lock = Arc::new(Mutex::new(()));
    registry.insert(target_path.to_path_buf(), Arc::downgrade(&target_lock));
    target_lock
}

/// Acquire one cache-target mutex and recover it after poisoning.
/// 获取一个缓存目标互斥量，并在 poison 后恢复。
///
/// The target_lock parameter serializes one exact path's inspection and publication.
/// target_lock 参数将一个精确路径的检查与发布串行化。
///
/// Returns the exclusive target guard.
/// 返回独占目标保护对象。
fn lock_download_target(target_lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    target_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PendingDownloadFile {
    /// Create one unique same-directory temp file without overwriting an existing path.
    /// 创建一个不会覆盖已有路径的同目录唯一临时文件。
    ///
    /// The target_path parameter determines the parent directory and diagnostic file stem.
    /// target_path 参数决定父目录与诊断文件名主体。
    ///
    /// Returns an armed cleanup guard or the exact creation error.
    /// 返回已激活的清理守卫，或返回精确创建错误。
    fn create(target_path: &Path) -> Result<Self, String> {
        // Parent is required because atomic publication must remain on the same filesystem.
        // Parent 是必需的，因为原子发布必须位于同一文件系统。
        let parent = target_path.parent().ok_or_else(|| {
            format!(
                "Download target has no parent directory: {}",
                render_download_path(target_path)
            )
        })?;
        // FileName preserves non-Unicode target names without lossy conversion.
        // FileName 在不进行有损转换的情况下保留非 Unicode 目标文件名。
        let file_name = target_path.file_name().ok_or_else(|| {
            format!(
                "Download target has no file name: {}",
                render_download_path(target_path)
            )
        })?;

        for _ in 0..DOWNLOAD_TEMP_FILE_CREATE_ATTEMPTS {
            // Sequence separates concurrent temp files created for different target locks.
            // Sequence 隔离为不同目标锁并发创建的临时文件。
            let sequence = DOWNLOAD_TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            // TempName combines the exact target name with process and sequence identity.
            // TempName 把精确目标名与进程及序号标识组合起来。
            let mut temp_name = OsString::from(".");
            temp_name.push(file_name);
            temp_name.push(format!(".{}.{}.part", std::process::id(), sequence));
            // TempPath remains beside the final target for atomic publication.
            // TempPath 保持在最终目标旁，以支持原子发布。
            let temp_path = parent.join(temp_name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path: temp_path,
                        file: Some(file),
                        published: false,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Failed to create download temp file beside {}: {}",
                        render_download_path(target_path),
                        error
                    ));
                }
            }
        }

        Err(format!(
            "Failed to allocate a unique download temp file beside {} after {} attempts",
            render_download_path(target_path),
            DOWNLOAD_TEMP_FILE_CREATE_ATTEMPTS
        ))
    }

    /// Append one response chunk to the pending file.
    /// 向待发布文件追加一个响应块。
    ///
    /// The bytes parameter contains exactly one successfully read response chunk.
    /// bytes 参数包含一个成功读取的精确响应块。
    ///
    /// Returns success after all bytes are written or a path-specific disk error.
    /// 全部字节写入后返回成功，否则返回包含路径的磁盘错误。
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        let file = self.file.as_mut().ok_or_else(|| {
            format!(
                "Download temp file is already closed: {}",
                render_download_path(&self.path)
            )
        })?;
        file.write_all(bytes).map_err(|error| {
            format!(
                "Failed to write download temp file {}: {}",
                render_download_path(&self.path),
                error
            )
        })
    }

    /// Return the private temp path for bounded pre-publication validation.
    /// 返回用于有界发布前验证的私有临时路径。
    ///
    /// Returns the same-directory temp path while this guard remains armed.
    /// 在当前守卫保持激活期间返回同目录临时路径。
    fn path(&self) -> &Path {
        &self.path
    }

    /// Flush, synchronize, and close the writable temp file before content validation.
    /// 在内容验证前刷新、同步并关闭可写临时文件。
    ///
    /// Returns success when the temp path contains the complete durable payload.
    /// 当临时路径包含完整持久载荷时返回成功。
    fn finish_writing(&mut self) -> Result<(), String> {
        // An absent handle means a previous successful call already finished the file.
        // 句柄为空表示之前的成功调用已经完成文件写入。
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        file.flush().map_err(|error| {
            format!(
                "Failed to flush download temp file {}: {}",
                render_download_path(&self.path),
                error
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "Failed to synchronize download temp file {}: {}",
                render_download_path(&self.path),
                error
            )
        })?;
        Ok(())
    }

    /// Atomically publish this completed pending file.
    /// 原子发布当前已完成待发布文件。
    ///
    /// The target_path parameter is the stable cache path visible to readers.
    /// target_path 参数是读取方可见的稳定缓存路径。
    ///
    /// Returns success only after publication; failure leaves any old target untouched.
    /// 仅在发布完成后返回成功；失败时保持任何旧目标不变。
    fn publish(mut self, target_path: &Path) -> Result<(), String> {
        self.finish_writing()?;
        replace_file_atomically(&self.path, target_path).map_err(|error| {
            format!(
                "Failed to publish download cache {}: {}",
                render_download_path(target_path),
                error
            )
        })?;
        self.published = true;
        Ok(())
    }
}

impl Drop for PendingDownloadFile {
    /// Close and remove an unpublished temp file best-effort.
    /// 尽力关闭并删除尚未发布的临时文件。
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Stream one HTTP response directly into an armed temp file while hashing and reporting progress.
/// 将一个 HTTP 响应直接流式写入已激活临时文件，同时计算哈希并报告进度。
///
/// The client parameter reuses the process-wide reqwest connection pool.
/// client 参数复用进程级 reqwest 连接池。
///
/// The source_locator parameter is the exact request URL and progress identity.
/// source_locator 参数是精确请求 URL 与进度标识。
///
/// The target_path parameter supplies the final diagnostic and temp-file location.
/// target_path 参数提供最终诊断与临时文件位置。
///
/// The expected_sha256 parameter contains a prevalidated normalized checksum when required.
/// expected_sha256 参数在需要时包含预先验证并归一化的 checksum。
///
/// The progress_callback parameter receives samples without per-chunk source string cloning.
/// progress_callback 参数接收不会逐块克隆来源字符串的进度样本。
///
/// Returns an unpublished completed file or a classified attempt error.
/// 返回尚未发布的已完成文件，或返回分类后的尝试错误。
fn stream_download_to_temp_file(
    client: reqwest::blocking::Client,
    source_locator: String,
    target_path: PathBuf,
    expected_sha256: Option<String>,
    progress_callback: Option<DownloadProgressCallback>,
) -> Result<PendingDownloadFile, DownloadAttemptError> {
    // Response is validated before any temp file is created or old cache is touched.
    // Response 在创建任何临时文件或触碰旧缓存前完成验证。
    let mut response = client
        .get(&source_locator)
        .send()
        .map_err(|error| {
            DownloadAttemptError::Failed(format!(
                "Failed to download {}: {}",
                source_locator, error
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            DownloadAttemptError::Failed(format!(
                "Failed to download {}: {}",
                source_locator, error
            ))
        })?;
    // BytesTotal preserves an absent Content-Length instead of inventing one.
    // BytesTotal 在缺少 Content-Length 时保持为空，不会虚构总长度。
    let bytes_total = response.content_length();
    // PendingFile owns cleanup until atomic publication succeeds in the caller.
    // PendingFile 在调用方原子发布成功前拥有清理责任。
    let mut pending_file =
        PendingDownloadFile::create(&target_path).map_err(DownloadAttemptError::Failed)?;
    // Hasher exists only for a checksum-constrained request.
    // Hasher 仅在带 checksum 约束的请求中存在。
    let mut hasher = expected_sha256.as_ref().map(|_| Sha256::new());
    // Buffer bounds response memory independently from total payload size.
    // Buffer 使响应内存占用不依赖载荷总大小并保持有界。
    let mut buffer = [0_u8; 64 * 1024];
    // BytesDone tracks only chunks that were read and successfully written.
    // BytesDone 只统计已经读取且成功写入的响应块。
    let mut bytes_done = 0_u64;
    // ProgressSample owns the source string once and mutates only numeric fields per chunk.
    // ProgressSample 只拥有一次来源字符串，并在每个块上仅修改数值字段。
    let mut progress_sample = progress_callback.as_ref().map(|_| DownloadProgress {
        source_locator: source_locator.clone(),
        bytes_done: 0,
        bytes_total,
        cached: false,
    });

    loop {
        // ReadCount is the next bounded response chunk size.
        // ReadCount 是下一个有界响应块的大小。
        let read_count = response.read(&mut buffer).map_err(|error| {
            DownloadAttemptError::Failed(format!("Failed to read {}: {}", source_locator, error))
        })?;
        if read_count == 0 {
            break;
        }
        pending_file
            .write_all(&buffer[..read_count])
            .map_err(DownloadAttemptError::Failed)?;
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(&buffer[..read_count]);
        }
        bytes_done = bytes_done.saturating_add(read_count as u64);
        if let (Some(callback), Some(progress_sample)) =
            (progress_callback.as_ref(), progress_sample.as_mut())
        {
            progress_sample.bytes_done = bytes_done;
            invoke_download_progress_callback(callback, progress_sample, &target_path);
        }
    }

    pending_file
        .finish_writing()
        .map_err(DownloadAttemptError::Failed)?;

    if let (Some(expected_sha256), Some(hasher)) = (expected_sha256.as_deref(), hasher) {
        // ActualSha256 is finalized directly from streamed chunks without rereading the file.
        // ActualSha256 直接从流式块完成计算，无需重新读取文件。
        let actual_sha256 = format!("{:x}", hasher.finalize());
        if actual_sha256 != expected_sha256 {
            return Err(DownloadAttemptError::ChecksumMismatch(format!(
                "Checksum mismatch for {}: expected {}, got {}",
                render_download_path(&target_path),
                expected_sha256,
                actual_sha256
            )));
        }
    }

    Ok(pending_file)
}

/// Normalize and validate one expected SHA-256 value before cache or network work.
/// 在缓存或网络工作前归一化并验证一个期望 SHA-256 值。
///
/// The path parameter identifies the cache target in diagnostics.
/// path 参数在诊断中标识缓存目标。
///
/// The expected_sha256 parameter is the caller-supplied checksum text.
/// expected_sha256 参数是调用方提供的 checksum 文本。
///
/// Returns one lowercase 64-digit hexadecimal checksum or a validation error.
/// 返回一个小写 64 位十六进制 checksum，或返回验证错误。
fn normalize_expected_sha256(path: &Path, expected_sha256: &str) -> Result<String, String> {
    // Expected is the canonical value shared by file and streamed verification.
    // Expected 是文件校验与流式校验共享的规范值。
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err(format!(
            "Expected checksum for {} is not one valid SHA-256 value",
            render_download_path(path)
        ));
    }
    Ok(expected)
}

/// Verify one existing cache file with bounded reads against a normalized checksum.
/// 使用有界读取校验一个已有缓存文件与归一化 checksum 是否一致。
///
/// The path parameter identifies the existing cache file.
/// path 参数标识已有缓存文件。
///
/// The expected_sha256 parameter must already be lowercase validated hexadecimal text.
/// expected_sha256 参数必须已经是经过验证的小写十六进制文本。
///
/// Returns success for a matching file or a path-specific read/checksum error.
/// 文件匹配时返回成功，否则返回包含路径的读取或 checksum 错误。
fn verify_file_sha256_normalized(path: &Path, expected_sha256: &str) -> Result<(), String> {
    // File is streamed so cache verification never allocates payload-sized memory.
    // File 采用流式读取，使缓存校验不会分配载荷大小级内存。
    let mut file = File::open(path)
        .map_err(|error| format!("Failed to read {}: {}", render_download_path(path), error))?;
    // Buffer is the fixed verification working set.
    // Buffer 是固定大小的校验工作集。
    let mut buffer = [0_u8; 64 * 1024];
    // Hasher accumulates bytes incrementally across bounded reads.
    // Hasher 跨有界读取增量累计字节。
    let mut hasher = Sha256::new();
    loop {
        // ReadCount is the number of bytes available in this verification chunk.
        // ReadCount 是当前校验块中的可用字节数。
        let read_count = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read {}: {}", render_download_path(path), error))?;
        if read_count == 0 {
            break;
        }
        hasher.update(&buffer[..read_count]);
    }
    // Actual is the lowercase hexadecimal digest for the existing file.
    // Actual 是已有文件的小写十六进制摘要。
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        return Err(format!(
            "Checksum mismatch for {}: expected {}, got {}",
            render_download_path(path),
            expected_sha256,
            actual
        ));
    }
    Ok(())
}

/// Validate one file as UTF-8 with a fixed buffer while preserving split code-point boundaries.
/// 使用固定缓冲区验证一个文件为 UTF-8，同时保留被拆分的码点边界。
///
/// The path parameter identifies a cached or unpublished text payload.
/// path 参数标识缓存文本或尚未发布的文本载荷。
///
/// Returns success for complete valid UTF-8 or a path and byte-offset diagnostic.
/// 对完整有效 UTF-8 返回成功，否则返回包含路径与字节偏移的诊断。
fn validate_file_utf8(path: &Path) -> Result<(), String> {
    // File is read independently from the final String allocation required by the public API.
    // File 独立于公共 API 最终所需的 String 分配进行读取。
    let mut file = File::open(path)
        .map_err(|error| format!("Failed to read {}: {}", render_download_path(path), error))?;
    // Buffer reserves three prefix bytes for one UTF-8 sequence split across reads.
    // Buffer 预留三个前缀字节，用于跨读取拆分的一个 UTF-8 序列。
    let mut buffer = [0_u8; 64 * 1024 + 3];
    // CarryLen counts the incomplete suffix retained at the front of the next read.
    // CarryLen 统计保留到下一次读取前部的不完整后缀长度。
    let mut carry_len = 0_usize;
    // ValidatedBytes counts complete bytes before the current buffer.
    // ValidatedBytes 统计当前缓冲区之前已经完成验证的字节数。
    let mut validated_bytes = 0_u64;

    loop {
        // ReadCount fills the buffer after any retained incomplete UTF-8 suffix.
        // ReadCount 在保留的不完整 UTF-8 后缀之后填充缓冲区。
        let read_count = file
            .read(&mut buffer[carry_len..])
            .map_err(|error| format!("Failed to read {}: {}", render_download_path(path), error))?;
        if read_count == 0 {
            if carry_len == 0 {
                return Ok(());
            }
            return Err(format!(
                "Failed to decode {} as UTF-8: incomplete sequence at byte offset {}",
                render_download_path(path),
                validated_bytes
            ));
        }
        // AvailableLen includes both the retained prefix and newly read bytes.
        // AvailableLen 同时包含保留前缀与新读取字节。
        let available_len = carry_len + read_count;
        match std::str::from_utf8(&buffer[..available_len]) {
            Ok(_) => {
                validated_bytes = validated_bytes.saturating_add(available_len as u64);
                carry_len = 0;
            }
            Err(error) if error.error_len().is_some() => {
                return Err(format!(
                    "Failed to decode {} as UTF-8: invalid sequence at byte offset {}",
                    render_download_path(path),
                    validated_bytes.saturating_add(error.valid_up_to() as u64)
                ));
            }
            Err(error) => {
                // ValidPrefixLen excludes the incomplete suffix that must cross the read boundary.
                // ValidPrefixLen 排除必须跨读取边界保留的不完整后缀。
                let valid_prefix_len = error.valid_up_to();
                // IncompleteLen is bounded by UTF-8's maximum three-byte unfinished prefix.
                // IncompleteLen 受 UTF-8 最多三字节未完成前缀约束。
                let incomplete_len = available_len - valid_prefix_len;
                if incomplete_len > 3 {
                    return Err(format!(
                        "Failed to decode {} as UTF-8: invalid incomplete sequence at byte offset {}",
                        render_download_path(path),
                        validated_bytes.saturating_add(valid_prefix_len as u64)
                    ));
                }
                buffer.copy_within(valid_prefix_len..available_len, 0);
                validated_bytes = validated_bytes.saturating_add(valid_prefix_len as u64);
                carry_len = incomplete_len;
            }
        }
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
#[cfg(test)]
fn verify_file_sha256(path: &Path, expected_sha256: &str) -> Result<(), String> {
    // Expected is validated once before the bounded file scan begins.
    // Expected 在有界文件扫描开始前只验证一次。
    let expected = normalize_expected_sha256(path, expected_sha256)?;
    verify_file_sha256_normalized(path, &expected)
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
        DOWNLOAD_HTTP_CLIENT_BUILDS, DOWNLOAD_HTTP_WORKER_COUNT, DOWNLOAD_HTTP_WORKER_STARTS,
        DownloadManager, DownloadManagerConfig, DownloadRequest, PendingDownloadFile,
        cached_download_file_metadata, format_github_release_tag_not_found_error,
        lock_download_target_registry, parse_checksum_manifest_for_asset,
        shared_download_target_lock, validate_file_utf8, verify_file_sha256,
    };
    use crate::dependency::types::DependencySourceType;
    use crate::runtime::path::render_host_visible_path;
    use sha2::{Digest, Sha256};
    use std::io::{ErrorKind, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};
    #[cfg(windows)]
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    #[cfg(windows)]
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// Monotonic suffix isolating download integration fixtures inside one test process.
    /// 在单个测试进程内隔离下载集成夹具的单调后缀。
    static DOWNLOAD_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    /// Local HTTP server fixture that returns one predetermined raw response per accepted request.
    /// 每个已接受请求返回一份预定原始响应的本地 HTTP 服务夹具。
    struct TestHttpServer {
        /// Exact loopback URL requested by the download manager.
        /// 下载管理器请求的精确回环 URL。
        url: String,
        /// Number of connections that received a response.
        /// 已收到响应的连接数量。
        request_count: Arc<AtomicUsize>,
        /// Background server thread joined explicitly by each test.
        /// 由每个测试显式等待的后台服务线程。
        handle: Option<JoinHandle<()>>,
    }

    /// Accept one loopback test request within the full-suite scheduling allowance.
    /// 在全量套件调度允许时长内接受一个回环测试请求。
    ///
    /// The listener parameter is one nonblocking ephemeral-port listener.
    /// listener 参数是一个非阻塞临时端口监听器。
    ///
    /// Returns the accepted stream or panics with a bounded fixture diagnostic.
    /// 返回已接受连接，否则以有界夹具诊断触发 panic。
    fn accept_test_http_connection(listener: &TcpListener) -> TcpStream {
        // Deadline prevents a failed client path from hanging the test process indefinitely.
        // Deadline 防止失败的客户端路径无限挂起测试进程。
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Accepted streams use blocking writes so large deterministic bodies apply backpressure.
                    // 已接受连接使用阻塞写入，使大型确定性正文能够施加背压。
                    stream
                        .set_nonblocking(false)
                        .expect("set accepted test HTTP stream blocking");
                    return stream;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "test HTTP request did not arrive before timeout"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept test HTTP request: {error}"),
            }
        }
    }

    /// Drain one small deterministic HTTP request header before writing its response.
    /// 在写入响应前排空一个小型确定性 HTTP 请求头。
    ///
    /// The stream parameter is the accepted loopback connection.
    /// stream 参数是已接受的回环连接。
    fn read_test_http_request(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set test HTTP read timeout");
        // RequestBuffer is sufficient for every fixed request emitted by these tests.
        // RequestBuffer 足以容纳这些测试发出的每个固定请求。
        let mut request_buffer = [0_u8; 4096];
        let _ = stream.read(&mut request_buffer);
    }

    impl TestHttpServer {
        /// Start one nonblocking loopback server for a finite response sequence.
        /// 为有限响应序列启动一个非阻塞回环服务。
        ///
        /// The responses parameter contains complete raw HTTP responses in acceptance order.
        /// responses 参数按接受顺序包含完整原始 HTTP 响应。
        ///
        /// Returns the running server fixture or panics when the local listener cannot start.
        /// 返回运行中的服务夹具；本地监听器无法启动时触发 panic。
        fn start(responses: Vec<Vec<u8>>) -> Self {
            // Listener binds an ephemeral loopback port so tests require no external network.
            // Listener 绑定临时回环端口，使测试不需要外部网络。
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP listener");
            listener
                .set_nonblocking(true)
                .expect("set test HTTP listener nonblocking");
            // Address becomes the exact local request endpoint.
            // Address 成为精确的本地请求端点。
            let address = listener.local_addr().expect("read test HTTP address");
            // RequestCount is shared with assertions after all responses are delivered.
            // RequestCount 与全部响应投递后的断言共享。
            let request_count = Arc::new(AtomicUsize::new(0));
            // ServerRequestCount transfers the shared counter into the background thread.
            // ServerRequestCount 将共享计数器转移到后台线程。
            let server_request_count = request_count.clone();
            // Handle owns the finite response loop.
            // Handle 拥有有限响应循环。
            let handle = thread::spawn(move || {
                for response in responses {
                    // Stream is accepted only for the current ordered response.
                    // Stream 仅为当前有序响应接受连接。
                    let mut stream = accept_test_http_connection(&listener);
                    read_test_http_request(&mut stream);
                    stream
                        .write_all(&response)
                        .expect("write test HTTP response");
                    stream.flush().expect("flush test HTTP response");
                    server_request_count.fetch_add(1, Ordering::SeqCst);
                }
            });
            Self {
                url: format!("http://{address}/payload.bin"),
                request_count,
                handle: Some(handle),
            }
        }

        /// Start one server that produces a large body from a fixed-size reusable chunk.
        /// 启动一个使用固定大小可复用块生成大正文的服务。
        ///
        /// The total_bytes parameter is the exact advertised and emitted response size.
        /// total_bytes 参数是精确声明并发出的响应大小。
        ///
        /// Returns a finite one-request fixture without allocating payload-sized server memory.
        /// 返回一个有限单请求夹具，且不会在服务端分配载荷大小级内存。
        fn start_streaming_body(total_bytes: usize) -> Self {
            // Listener binds an isolated ephemeral loopback endpoint.
            // Listener 绑定隔离的临时回环端点。
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind streaming HTTP listener");
            listener
                .set_nonblocking(true)
                .expect("set streaming HTTP listener nonblocking");
            // Address becomes the large-download request URL.
            // Address 成为大下载请求 URL。
            let address = listener.local_addr().expect("read streaming HTTP address");
            // RequestCount records the single delivered response.
            // RequestCount 记录单个已投递响应。
            let request_count = Arc::new(AtomicUsize::new(0));
            // ServerRequestCount transfers counting into the streaming thread.
            // ServerRequestCount 将计数能力转移到流式线程。
            let server_request_count = request_count.clone();
            // Handle owns the bounded-memory response producer.
            // Handle 拥有有界内存响应生产器。
            let handle = thread::spawn(move || {
                // Stream is the only accepted large-payload connection.
                // Stream 是唯一接受的大载荷连接。
                let mut stream = accept_test_http_connection(&listener);
                read_test_http_request(&mut stream);
                // Headers advertise the exact payload size and disable connection reuse.
                // Headers 声明精确载荷大小并禁用连接复用。
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {total_bytes}\r\n\r\n"
                );
                stream
                    .write_all(headers.as_bytes())
                    .expect("write streaming HTTP headers");
                // Chunk is reused for the entire response and keeps server memory bounded.
                // Chunk 在整个响应中重复使用，使服务端内存保持有界。
                let chunk = [0x7b_u8; 64 * 1024];
                // Remaining tracks exact bytes still owed to the client.
                // Remaining 跟踪仍需发送给客户端的精确字节数。
                let mut remaining = total_bytes;
                while remaining > 0 {
                    // WriteCount caps the final write without allocating a tail buffer.
                    // WriteCount 限制最终写入大小，且不分配尾部缓冲区。
                    let write_count = remaining.min(chunk.len());
                    stream
                        .write_all(&chunk[..write_count])
                        .expect("write streaming HTTP body chunk");
                    remaining -= write_count;
                }
                stream.flush().expect("flush streaming HTTP response");
                server_request_count.fetch_add(1, Ordering::SeqCst);
            });
            Self {
                url: format!("http://{address}/large.bin"),
                request_count,
                handle: Some(handle),
            }
        }

        /// Join the finite server and return its delivered response count.
        /// 等待有限服务结束并返回其已投递响应数量。
        ///
        /// Returns the exact count after the server thread completes.
        /// 在服务线程完成后返回精确计数。
        fn finish(mut self) -> usize {
            // Handle is present exactly once because finish consumes the fixture.
            // Handle 只存在一次，因为 finish 会消费该夹具。
            let handle = self.handle.take().expect("test HTTP server handle");
            handle.join().expect("join test HTTP server");
            self.request_count.load(Ordering::SeqCst)
        }
    }

    impl Drop for TestHttpServer {
        /// Join a server that a failing assertion did not finish explicitly.
        /// 等待因断言失败而未显式结束的服务。
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Build one complete connection-closing HTTP 200 response.
    /// 构造一个完整的关闭连接式 HTTP 200 响应。
    ///
    /// The body parameter contains the exact response payload.
    /// body 参数包含精确响应载荷。
    ///
    /// The declared_length parameter controls Content-Length presence and mismatch fixtures.
    /// declared_length 参数控制 Content-Length 是否存在及不匹配夹具。
    ///
    /// Returns raw response bytes accepted by reqwest's HTTP/1 parser.
    /// 返回 reqwest HTTP/1 解析器可接受的原始响应字节。
    fn test_http_response(body: &[u8], declared_length: Option<usize>) -> Vec<u8> {
        // Headers always close the connection so absent lengths are delimited by EOF.
        // Headers 始终关闭连接，使缺少长度时由 EOF 划分响应边界。
        let mut headers = String::from("HTTP/1.1 200 OK\r\nConnection: close\r\n");
        if let Some(declared_length) = declared_length {
            headers.push_str(&format!("Content-Length: {declared_length}\r\n"));
        }
        headers.push_str("\r\n");
        // Response owns headers and body in one deterministic socket write.
        // Response 在一次确定性套接字写入中拥有响应头与正文。
        let mut response = headers.into_bytes();
        response.extend_from_slice(body);
        response
    }

    /// Create one unique temporary cache root for a download integration test.
    /// 为一个下载集成测试创建唯一临时缓存根目录。
    ///
    /// The label parameter identifies the owning scenario in filesystem diagnostics.
    /// label 参数在文件系统诊断中标识所属场景。
    ///
    /// Returns an existing empty directory path.
    /// 返回一个已经存在的空目录路径。
    fn make_download_test_root(label: &str) -> PathBuf {
        // Sequence prevents parallel scenarios from sharing one cache target.
        // Sequence 防止并行场景共享同一个缓存目标。
        let sequence = DOWNLOAD_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        // TempRoot remains outside the repository and is cleaned by each test.
        // TempRoot 位于仓库之外，并由每个测试清理。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_download_{label}_{}_{}",
            std::process::id(),
            sequence
        ));
        if temp_root.exists() {
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("create download test root");
        temp_root
    }

    /// Build one network-enabled manager rooted at an isolated test cache directory.
    /// 构建一个以隔离测试缓存目录为根的启用网络下载管理器。
    ///
    /// Returns a manager with default GitHub endpoint policy.
    /// 返回采用默认 GitHub 端点策略的管理器。
    fn make_test_download_manager(cache_root: PathBuf) -> DownloadManager {
        DownloadManager::new(DownloadManagerConfig {
            cache_root,
            allow_network_download: true,
            github_base_url: None,
            github_api_base_url: None,
        })
    }

    /// Assert that no armed download temp files remain below one cache root.
    /// 断言一个缓存根目录下没有遗留已激活下载临时文件。
    ///
    /// The cache_root parameter is the directory inspected after success or failure.
    /// cache_root 参数是成功或失败后接受检查的目录。
    fn assert_no_download_temp_files(cache_root: &std::path::Path) {
        // TempFiles collects only the private `.part` publication artifacts.
        // TempFiles 只收集私有 `.part` 发布产物。
        let temp_files = std::fs::read_dir(cache_root)
            .expect("read download cache root")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert!(
            temp_files.is_empty(),
            "download temp files should be cleaned: {temp_files:?}"
        );
    }

    /// Return the Windows process peak working-set byte count for bounded-memory acceptance.
    /// 返回用于有界内存验收的 Windows 进程峰值工作集字节数。
    ///
    /// Returns the operating-system counter or a native probe error.
    /// 返回操作系统计数器，或返回原生探测错误。
    #[cfg(windows)]
    fn windows_peak_working_set_bytes() -> Result<u64, String> {
        // Counters receives the native process memory snapshot.
        // Counters 接收原生进程内存快照。
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..PROCESS_MEMORY_COUNTERS::default()
        };
        // Status reports whether the native snapshot was populated successfully.
        // Status 表示原生快照是否成功填充。
        let status = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        if status == 0 {
            return Err(format!(
                "GetProcessMemoryInfo failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(counters.PeakWorkingSetSize as u64)
    }

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
        let error = cached_download_file_metadata(&invalid_target_path)
            .expect_err("invalid cached download target probe should fail");

        assert!(
            error.contains("Failed to inspect cached download target"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
    }

    /// Verify caller-controlled cache keys cannot escape the configured cache root.
    /// 验证调用方控制的缓存键无法逃逸配置的缓存根目录。
    #[test]
    fn caller_controlled_cache_key_cannot_escape_cache_root() {
        // CacheRoot is an arbitrary absolute root used only for deterministic path derivation.
        // CacheRoot 是仅用于确定性路径派生的任意绝对根目录。
        let cache_root = std::env::temp_dir().join("luaskills_download_cache_key_boundary");
        // Manager owns the root that every derived cache path must remain beneath.
        // Manager 持有每个派生缓存路径都必须位于其下的根目录。
        let manager = DownloadManager::new(DownloadManagerConfig {
            cache_root: cache_root.clone(),
            allow_network_download: true,
            github_base_url: None,
            github_api_base_url: None,
        });
        // Request uses parent traversal plus separators that previously escaped the cache root.
        // Request 使用此前可逃逸缓存根目录的父级遍历与分隔符。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: "https://example.invalid/payload.txt".to_string(),
            cache_key: "../outside\\attacker-controlled".to_string(),
        };
        // CachedPath must contain only the SHA-256 key digest and allow-listed URL suffix.
        // CachedPath 必须只包含缓存键 SHA-256 摘要与 URL 白名单后缀。
        let cached_path = manager.cached_path_for_request(&request);
        // FileName captures the untrusted-key projection for exact character validation.
        // FileName 捕获不可信缓存键投影，以便精确验证字符范围。
        let file_name = cached_path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("cache file name should be valid UTF-8");

        assert_eq!(cached_path.parent(), Some(cache_root.as_path()));
        assert!(cached_path.starts_with(&cache_root));
        assert_eq!(file_name.len(), 64 + ".txt".len());
        assert!(file_name.ends_with(".txt"));
        assert!(
            file_name[..64]
                .chars()
                .all(|value| value.is_ascii_hexdigit())
        );
        assert!(!file_name.contains(".."));
        assert!(!file_name.contains(['/', '\\']));
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

    /// Verify bounded UTF-8 validation preserves a code point split across its read-buffer boundary.
    /// 验证有界 UTF-8 校验会保留跨读取缓冲区边界拆分的码点。
    #[test]
    fn utf8_file_validation_preserves_cross_buffer_code_points() {
        // CacheRoot isolates the direct validation fixture.
        // CacheRoot 隔离直接验证夹具。
        let cache_root = make_download_test_root("utf8_boundary");
        // FilePath contains a multibyte code point split after the first byte of one validator read.
        // FilePath 包含一个在校验器首次读取后仅保留首字节的多字节码点。
        let file_path = cache_root.join("boundary.txt");
        // Payload places the first UTF-8 byte at the final position of the 64 KiB plus prefix buffer.
        // Payload 把首个 UTF-8 字节放在 64 KiB 加前缀缓冲区的最后位置。
        let mut payload = vec![b'a'; 64 * 1024 + 2];
        payload.extend_from_slice("中".as_bytes());
        std::fs::write(&file_path, &payload).expect("write UTF-8 boundary fixture");

        validate_file_utf8(&file_path).expect("cross-buffer UTF-8 should validate");
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify inactive per-target locks are discarded on the next registry access.
    /// 验证失活的逐目标锁会在下次访问注册表时被清理。
    #[test]
    fn download_target_lock_registry_discards_inactive_paths() {
        // Paths identify only entries owned by this test so parallel downloads do not affect assertions.
        // Paths 只标识当前测试拥有的条目，使并行下载不会影响断言。
        let paths = (0..64)
            .map(|index| {
                PathBuf::from(format!(
                    "download-lock-registry-{}-{index}",
                    DOWNLOAD_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ))
            })
            .collect::<Vec<_>>();
        // Locks keep every test entry active until the explicit drop boundary.
        // Locks 在显式 drop 边界前保持每个测试条目活动。
        let locks = paths
            .iter()
            .map(|path| shared_download_target_lock(path))
            .collect::<Vec<_>>();
        drop(locks);
        // CleanupTrigger causes the production retain pass after all test entries become weak-only.
        // CleanupTrigger 在全部测试条目只剩弱引用后触发生产 retain 清理。
        let cleanup_trigger = PathBuf::from(format!(
            "download-lock-registry-trigger-{}",
            DOWNLOAD_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        drop(shared_download_target_lock(&cleanup_trigger));
        // RegistryGuard inspects only the exact historical paths created above.
        // RegistryGuard 只检查上面创建的精确历史路径。
        let registry_guard = lock_download_target_registry();
        assert!(paths.iter().all(|path| !registry_guard.contains_key(path)));
    }

    /// Verify a real write error keeps the pending-file guard armed and removes its partial path.
    /// 验证真实写入错误会保持待发布文件守卫激活并删除其部分路径。
    #[test]
    fn pending_download_write_failure_removes_partial_file() {
        // CacheRoot isolates the intentionally unwritable file handle fixture.
        // CacheRoot 隔离刻意不可写文件句柄夹具。
        let cache_root = make_download_test_root("write_failure");
        // TempPath is opened read-only before being transferred into the production guard.
        // TempPath 在转移到生产守卫前以只读方式打开。
        let temp_path = cache_root.join("read-only.part");
        std::fs::write(&temp_path, b"partial").expect("write partial fixture");
        // ReadOnlyFile deterministically rejects write_all on every supported platform.
        // ReadOnlyFile 在每个受支持平台上都会确定性拒绝 write_all。
        let read_only_file = std::fs::File::open(&temp_path).expect("open read-only fixture");
        // PendingFile uses the same armed cleanup state as a production temp download.
        // PendingFile 使用与生产临时下载相同的已激活清理状态。
        let mut pending_file = PendingDownloadFile {
            path: temp_path.clone(),
            file: Some(read_only_file),
            published: false,
        };

        // Error proves the production chunk write surfaces the disk failure.
        // Error 证明生产块写入会暴露磁盘失败。
        let error = pending_file
            .write_all(b"new-bytes")
            .expect_err("read-only temp file should reject writes");
        assert!(error.contains("Failed to write download temp file"));
        drop(pending_file);
        assert!(!temp_path.exists());
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify unknown-length responses stream to disk and initialize one reusable HTTP executor.
    /// 验证未知长度响应会流式写盘，并且只初始化一个可复用 HTTP 执行器。
    #[test]
    fn streamed_download_supports_unknown_content_length_and_reuses_http_resources() {
        // Payload spans multiple fixed download buffers without being retained as one response Vec.
        // Payload 跨越多个固定下载缓冲区，不会作为一个完整响应 Vec 保留。
        let payload = vec![0x5a_u8; 192 * 1024 + 17];
        // Server omits Content-Length so EOF terminates the successful response.
        // Server 省略 Content-Length，使 EOF 终止成功响应。
        let server = TestHttpServer::start(vec![test_http_response(&payload, None)]);
        // CacheRoot isolates the streamed publication result.
        // CacheRoot 隔离流式发布结果。
        let cache_root = make_download_test_root("unknown_length");
        // ProgressSamples records byte totals reported by the streaming loop.
        // ProgressSamples 记录流式循环报告的字节总量。
        let progress_samples = Arc::new(Mutex::new(Vec::new()));
        // CallbackSamples transfers the shared progress log into the callback.
        // CallbackSamples 将共享进度日志转移到回调中。
        let callback_samples = progress_samples.clone();
        // Manager emits operation-scoped progress while sharing global HTTP resources.
        // Manager 发出操作级进度，同时共享全局 HTTP 资源。
        let manager = DownloadManager::new_with_progress(
            DownloadManagerConfig {
                cache_root: cache_root.clone(),
                allow_network_download: true,
                github_base_url: None,
                github_api_base_url: None,
            },
            Some(Arc::new(move |progress| {
                callback_samples.lock().expect("record progress").push((
                    progress.bytes_done,
                    progress.bytes_total,
                    progress.source_locator.clone(),
                ));
            })),
        );
        // Request selects one deterministic binary cache target.
        // Request 选择一个确定性二进制缓存目标。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "unknown-length".to_string(),
        };

        // DownloadedPath is returned only after the streamed temp file is published.
        // DownloadedPath 仅在流式临时文件发布后返回。
        let downloaded_path = manager
            .download(&request)
            .expect("stream unknown-length body");
        assert_eq!(
            std::fs::read(&downloaded_path).expect("read streamed payload"),
            payload
        );
        // Samples contain an absent total and end exactly at the payload length.
        // Samples 保持总长度为空，并精确结束于载荷长度。
        let samples = progress_samples.lock().expect("read progress samples");
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|(_, total, _)| total.is_none()));
        assert_eq!(
            samples.last().map(|sample| sample.0),
            Some(payload.len() as u64)
        );
        assert!(
            samples
                .iter()
                .all(|(_, _, source_locator)| source_locator == &server.url)
        );
        drop(samples);
        assert_eq!(server.finish(), 1);
        assert_eq!(DOWNLOAD_HTTP_CLIENT_BUILDS.load(Ordering::SeqCst), 1);
        assert_eq!(
            DOWNLOAD_HTTP_WORKER_STARTS.load(Ordering::SeqCst),
            DOWNLOAD_HTTP_WORKER_COUNT as u64
        );
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify a progress callback cannot form a cross-target lock cycle by starting any nested download.
    /// 验证进度回调无法通过启动任意嵌套下载形成跨目标锁环。
    #[test]
    fn progress_callback_rejects_cross_target_reentry_without_blocking_outer_download() {
        // Payload guarantees one non-empty progress notification from the streaming loop.
        // Payload 保证流式循环至少发出一次非空进度通知。
        let payload = b"outer-download-payload";
        // Server needs only the outer request because cross-target re-entry is rejected before networking.
        // Server 只需处理外层请求，因为跨目标重入会在联网前被拒绝。
        let server = TestHttpServer::start(vec![test_http_response(payload, Some(payload.len()))]);
        // CacheRoot owns two distinct deterministic targets used to prove target inequality cannot bypass the guard.
        // CacheRoot 持有两个不同的确定性目标，用于证明目标不相等也无法绕过保护。
        let cache_root = make_download_test_root("progress_cross_target_reentry");
        // BaseManager has no callback and is safe to capture for the nested request.
        // BaseManager 不带回调，可安全捕获用于嵌套请求。
        let base_manager = make_test_download_manager(cache_root.clone());
        // NestedRequest intentionally resolves to a different cache path from the outer request.
        // NestedRequest 被刻意解析到与外层请求不同的缓存路径。
        let nested_request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "progress-nested-target".to_string(),
        };
        // OuterRequest owns the target lock held while its synchronous progress callback runs.
        // OuterRequest 拥有同步进度回调运行期间持有的目标锁。
        let outer_request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "progress-outer-target".to_string(),
        };
        // CallbackErrors records the explicit nested result without panicking inside the worker.
        // CallbackErrors 记录显式嵌套结果，避免在工作线程回调中触发 panic。
        let callback_errors = Arc::new(Mutex::new(Vec::new()));
        // CallbackManager owns the callback-free downloader used for re-entry.
        // CallbackManager 持有用于重入且不带回调的下载器。
        let callback_manager = base_manager.clone();
        // CallbackRequest owns the distinct nested request used by the callback.
        // CallbackRequest 持有回调使用的不同嵌套请求。
        let callback_request = nested_request.clone();
        // CallbackErrorsSlot transfers the shared diagnostic vector into the callback.
        // CallbackErrorsSlot 将共享诊断向量转移到回调中。
        let callback_errors_slot = callback_errors.clone();
        // OuterManager adds the re-entrant callback while sharing every other downloader resource.
        // OuterManager 添加重入回调，同时共享下载器的其他全部资源。
        let outer_manager = base_manager.with_progress_callback(Some(Arc::new(move |_| {
            // NestedError must be returned immediately instead of waiting on the outer target lock.
            // NestedError 必须立即返回，而不是等待外层目标锁。
            let nested_error = callback_manager
                .download(&callback_request)
                .expect_err("cross-target callback re-entry must be rejected");
            callback_errors_slot
                .lock()
                .expect("record cross-target re-entry error")
                .push(nested_error);
        })));

        // DownloadedPath proves rejecting nested re-entry does not abort the valid outer transfer.
        // DownloadedPath 证明拒绝嵌套重入不会中止有效的外层传输。
        let downloaded_path = outer_manager
            .download(&outer_request)
            .expect("outer download should complete after nested rejection");
        assert_eq!(
            std::fs::read(&downloaded_path).expect("read outer download result"),
            payload
        );
        // Errors must contain at least one stable nested-download diagnostic and no lock wait.
        // Errors 必须至少包含一条稳定的嵌套下载诊断，且没有锁等待。
        let errors = callback_errors
            .lock()
            .expect("read cross-target callback errors");
        assert!(!errors.is_empty());
        assert!(
            errors
                .iter()
                .all(|error| { error.contains("cannot start nested download") })
        );
        drop(errors);
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify a saturated worker set executes nested different-target HTTP tasks inline.
    /// 验证饱和工作线程集合会内联执行嵌套的不同目标 HTTP 任务。
    #[test]
    fn saturated_http_workers_complete_nested_tasks_without_pool_starvation() {
        // CacheRoot supplies a valid manager configuration without requiring external networking.
        // CacheRoot 提供有效管理器配置，且不需要外部网络。
        let cache_root = make_download_test_root("nested_worker_tasks");
        // Manager is shared by every synchronous caller and nested worker operation.
        // Manager 由每个同步调用方与嵌套工作线程操作共享。
        let manager = Arc::new(make_test_download_manager(cache_root.clone()));
        // WorkerBarrier first occupies every fixed worker before any nested task begins.
        // WorkerBarrier 在任何嵌套任务开始前先占满全部固定工作线程。
        let worker_barrier = Arc::new(Barrier::new(DOWNLOAD_HTTP_WORKER_COUNT));
        // ResultChannel reports each bounded nested result to the test thread.
        // ResultChannel 向测试线程报告每个有界嵌套结果。
        let (result_sender, result_receiver) = mpsc::channel();
        // Callers own the synchronous threads waiting on the fixed worker set.
        // Callers 持有等待固定工作线程集合的同步线程。
        let mut callers = Vec::with_capacity(DOWNLOAD_HTTP_WORKER_COUNT);

        for _ in 0..DOWNLOAD_HTTP_WORKER_COUNT {
            // CallerManager submits one outer task from this synchronous thread.
            // CallerManager 从当前同步线程提交一个外层任务。
            let caller_manager = manager.clone();
            // NestedManager is captured by the worker-side outer operation.
            // NestedManager 由工作线程侧外层操作捕获。
            let nested_manager = manager.clone();
            // TaskBarrier synchronizes all fixed workers at the nested-call boundary.
            // TaskBarrier 在嵌套调用边界同步全部固定工作线程。
            let task_barrier = worker_barrier.clone();
            // ThreadSender returns the outer operation result without shared mutation.
            // ThreadSender 在不共享可变状态的情况下返回外层操作结果。
            let thread_sender = result_sender.clone();
            callers.push(thread::spawn(move || {
                // Result completes only if the nested task does not wait behind its own saturated pool.
                // Result 仅在嵌套任务不等待自身已饱和工作池时完成。
                let result = caller_manager.run_http_task(move |_| {
                    task_barrier.wait();
                    nested_manager.run_http_task(|_| Ok(7_u8))
                });
                thread_sender
                    .send(result)
                    .expect("send nested worker result");
            }));
        }
        drop(result_sender);

        for _ in 0..DOWNLOAD_HTTP_WORKER_COUNT {
            // Result must arrive within a bounded interval instead of pool-starving forever.
            // Result 必须在有界时间内到达，而不是因工作池饥饿永久等待。
            let result = result_receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("nested worker result must not time out")
                .expect("nested worker task must succeed");
            assert_eq!(result, 7);
        }
        for caller in callers {
            caller.join().expect("join nested HTTP caller");
        }
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify a failed checksum refresh preserves the previous cache file and removes its temp file.
    /// 验证 checksum 刷新失败会保留旧缓存文件并删除临时文件。
    #[test]
    fn checksum_mismatch_preserves_existing_cache_until_valid_publication() {
        // TrustedPayload defines the expected checksum but is never served by this failure fixture.
        // TrustedPayload 定义期望 checksum，但当前失败夹具不会提供它。
        let trusted_payload = b"trusted-payload";
        // ExpectedSha256 is the exact digest required by the request.
        // ExpectedSha256 是请求要求的精确摘要。
        let expected_sha256 = format!("{:x}", Sha256::digest(trusted_payload));
        // WrongPayload forces both cached and network content to fail validation.
        // WrongPayload 强制缓存与网络内容均校验失败。
        let wrong_payload = b"wrong-network-payload";
        // Server supplies the one automatic redownload allowed after a bad cached file.
        // Server 提供缓存文件损坏后允许的一次自动重新下载。
        let server = TestHttpServer::start(vec![test_http_response(
            wrong_payload,
            Some(wrong_payload.len()),
        )]);
        // CacheRoot isolates the preserved old cache assertion.
        // CacheRoot 隔离旧缓存保留断言。
        let cache_root = make_download_test_root("checksum_preserve");
        // Manager owns the deterministic target policy.
        // Manager 拥有确定性目标策略。
        let manager = make_test_download_manager(cache_root.clone());
        // Request maps the local server to one cache file.
        // Request 把本地服务映射到一个缓存文件。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "checksum-preserve".to_string(),
        };
        // TargetPath is populated with the old cache before refresh begins.
        // TargetPath 在刷新开始前写入旧缓存。
        let target_path = manager.cached_path_for_request(&request);
        std::fs::write(&target_path, b"old-cache").expect("write old cache");

        // Error proves both the old cache and redownload were rejected.
        // Error 证明旧缓存与重新下载均被拒绝。
        let error = manager
            .download_with_sha256(&request, &expected_sha256)
            .expect_err("wrong redownload checksum should fail");
        assert!(error.contains("Automatic redownload also failed checksum verification"));
        assert_eq!(
            std::fs::read(&target_path).expect("read preserved cache"),
            b"old-cache"
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify a valid checksum payload atomically replaces an invalid existing cache file.
    /// 验证有效 checksum 载荷会原子替换已有的无效缓存文件。
    #[test]
    fn valid_checksum_download_replaces_invalid_existing_cache() {
        // ValidPayload is both served and used to derive the expected digest.
        // ValidPayload 既由服务返回，也用于派生期望摘要。
        let valid_payload = b"validated-network-payload";
        // ExpectedSha256 is the exact digest accepted before publication.
        // ExpectedSha256 是发布前接受的精确摘要。
        let expected_sha256 = format!("{:x}", Sha256::digest(valid_payload));
        // Server returns one valid replacement body.
        // Server 返回一个有效替换正文。
        let server = TestHttpServer::start(vec![test_http_response(
            valid_payload,
            Some(valid_payload.len()),
        )]);
        // CacheRoot isolates the existing-target replacement branch.
        // CacheRoot 隔离已有目标替换分支。
        let cache_root = make_download_test_root("checksum_replace");
        // Manager applies the shared streaming and atomic replacement pipeline.
        // Manager 应用共享流式与原子替换流水线。
        let manager = make_test_download_manager(cache_root.clone());
        // Request selects the pre-populated target.
        // Request 选择预先填充的目标。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "checksum-replace".to_string(),
        };
        // TargetPath starts with content rejected by the expected checksum.
        // TargetPath 初始包含会被期望 checksum 拒绝的内容。
        let target_path = manager.cached_path_for_request(&request);
        std::fs::write(&target_path, b"invalid-old-cache").expect("write invalid old cache");

        // DownloadedPath is the same stable target after validated replacement.
        // DownloadedPath 是验证替换后的同一稳定目标。
        let downloaded_path = manager
            .download_with_sha256(&request, &expected_sha256)
            .expect("publish valid checksum replacement");
        assert_eq!(downloaded_path, target_path);
        assert_eq!(
            std::fs::read(&target_path).expect("read checksum replacement"),
            valid_payload
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify complete but invalid UTF-8 fresh content cannot replace the previous valid text cache.
    /// 验证完整但非法的 UTF-8 fresh 内容无法替换之前的有效文本缓存。
    #[test]
    fn invalid_utf8_fresh_text_download_preserves_existing_cache() {
        // InvalidUtf8 is a complete binary response that fails text admission.
        // InvalidUtf8 是会在文本准入阶段失败的完整二进制响应。
        let invalid_utf8 = [0xf0_u8, 0x28, 0x8c, 0x28];
        // Server advertises the exact length so failure is exclusively UTF-8 validation.
        // Server 声明精确长度，使失败只来自 UTF-8 验证。
        let server = TestHttpServer::start(vec![test_http_response(
            &invalid_utf8,
            Some(invalid_utf8.len()),
        )]);
        // CacheRoot isolates text admission from other scenarios.
        // CacheRoot 将文本准入与其他场景隔离。
        let cache_root = make_download_test_root("fresh_invalid_utf8");
        // Manager performs a forced fresh text request.
        // Manager 执行强制 fresh 文本请求。
        let manager = make_test_download_manager(cache_root.clone());
        // Request mirrors the public fresh-text target derivation.
        // Request 复现公共 fresh 文本目标派生。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "fresh-invalid-utf8".to_string(),
        };
        // TargetPath contains valid text that must remain visible after rejection.
        // TargetPath 包含拒绝后必须继续可见的有效文本。
        let target_path = manager.cached_path_for_request(&request);
        std::fs::write(&target_path, b"old-valid-text").expect("write valid old text cache");

        // Error is raised before the pending file reaches atomic publication.
        // Error 在待发布文件进入原子发布前返回。
        let error = manager
            .fetch_text_fresh(&server.url, "fresh-invalid-utf8")
            .expect_err("invalid UTF-8 fresh text should fail");
        assert!(
            error.contains("Failed to decode") && error.contains("UTF-8"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read(&target_path).expect("read preserved valid text"),
            b"old-valid-text"
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify an interrupted fresh text response cannot delete or partially replace the old cache.
    /// 验证中断的 fresh 文本响应无法删除或部分替换旧缓存。
    #[test]
    fn interrupted_fresh_text_download_preserves_existing_cache() {
        // PartialBody is shorter than the declared response length and forces a read error.
        // PartialBody 短于声明的响应长度，并强制触发读取错误。
        let partial_body = b"partial-new-text";
        // Server closes the socket before the advertised body length is reached.
        // Server 在达到声明正文长度前关闭套接字。
        let server = TestHttpServer::start(vec![test_http_response(
            partial_body,
            Some(partial_body.len() + 128),
        )]);
        // CacheRoot isolates the fresh-text replacement contract.
        // CacheRoot 隔离 fresh 文本替换契约。
        let cache_root = make_download_test_root("fresh_interrupted");
        // Manager performs the refresh through the shared streaming path.
        // Manager 通过共享流式路径执行刷新。
        let manager = make_test_download_manager(cache_root.clone());
        // Request mirrors fetch_text_fresh path derivation for the same URL and key.
        // Request 复现相同 URL 与 key 的 fetch_text_fresh 路径派生。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "fresh-interrupted".to_string(),
        };
        // TargetPath contains the valid old text that must survive failure.
        // TargetPath 包含失败后必须保留的有效旧文本。
        let target_path = manager.cached_path_for_request(&request);
        std::fs::write(&target_path, b"old-valid-text").expect("write old text cache");

        // Error originates before any atomic publication can consume the old target.
        // Error 产生于任何原子发布消费旧目标之前。
        let error = manager
            .fetch_text_fresh(&server.url, "fresh-interrupted")
            .expect_err("truncated fresh text response should fail");
        assert!(
            error.contains("Failed to read"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read(&target_path).expect("read old text cache"),
            b"old-valid-text"
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify concurrent callers for one cache key issue one request and all observe a complete file.
    /// 验证同一缓存键的并发调用方只发起一次请求，且全部观察到完整文件。
    #[test]
    fn concurrent_same_cache_key_downloads_once_and_publishes_complete_file() {
        // CallerCount creates enough contention to exercise the process-wide target lock.
        // CallerCount 创建足够竞争以验证进程级目标锁。
        let caller_count = 16_usize;
        // Payload spans multiple chunks so no caller can accept an intermediate file.
        // Payload 跨越多个块，使任何调用方都不能接受中间文件。
        let payload = vec![0x33_u8; 512 * 1024 + 9];
        // Server intentionally owns only one response; duplicate network requests fail the test.
        // Server 有意只拥有一个响应；重复网络请求会使测试失败。
        let server = TestHttpServer::start(vec![test_http_response(&payload, Some(payload.len()))]);
        // CacheRoot is shared by every concurrent manager clone.
        // CacheRoot 由每个并发管理器克隆共享。
        let cache_root = make_download_test_root("same_key_concurrent");
        // Manager clones share immutable configuration and process-wide HTTP resources.
        // Manager 克隆共享不可变配置与进程级 HTTP 资源。
        let manager = make_test_download_manager(cache_root.clone());
        // Request is cloned into each competing caller.
        // Request 被克隆到每个竞争调用方中。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "same-key".to_string(),
        };
        // StartBarrier releases all callers together after their threads are ready.
        // StartBarrier 在线程全部就绪后同时释放所有调用方。
        let start_barrier = Arc::new(Barrier::new(caller_count + 1));
        // Handles own every concurrent download result.
        // Handles 拥有每个并发下载结果。
        let mut handles = Vec::with_capacity(caller_count);
        for _ in 0..caller_count {
            // ThreadManager is the cheap operation-safe downloader clone.
            // ThreadManager 是低成本且操作安全的下载器克隆。
            let thread_manager = manager.clone();
            // ThreadRequest preserves the exact same target identity.
            // ThreadRequest 保持完全相同的目标标识。
            let thread_request = request.clone();
            // ThreadBarrier coordinates simultaneous entry.
            // ThreadBarrier 协调同时进入。
            let thread_barrier = start_barrier.clone();
            handles.push(thread::spawn(move || {
                thread_barrier.wait();
                thread_manager.download(&thread_request)
            }));
        }
        start_barrier.wait();
        // Paths collects every successful stable cache result.
        // Paths 收集每个成功的稳定缓存结果。
        let paths = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("join concurrent downloader")
                    .expect("concurrent download should succeed")
            })
            .collect::<Vec<_>>();
        assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            std::fs::read(&paths[0]).expect("read concurrent cache result"),
            payload
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify distinct cache keys can use every fixed HTTP worker without queue starvation.
    /// 验证不同缓存键能够使用全部固定 HTTP 工作线程且不会发生队列饥饿。
    #[test]
    fn parallel_distinct_downloads_complete_on_fixed_http_pool() {
        // RequestCount exceeds the worker count so the bounded queue must advance between waves.
        // RequestCount 超过工作线程数量，使有界队列必须跨批次推进。
        let request_count = DOWNLOAD_HTTP_WORKER_COUNT * 2;
        // Payload is small because this test isolates scheduling rather than streaming volume.
        // Payload 保持较小，因为当前测试隔离调度而非流式体积。
        let payload = b"parallel-worker-payload";
        // Servers provide one independent loopback endpoint per concurrent request.
        // Servers 为每个并发请求提供一个独立回环端点。
        let servers = (0..request_count)
            .map(|_| TestHttpServer::start(vec![test_http_response(payload, Some(payload.len()))]))
            .collect::<Vec<_>>();
        // CacheRoot is shared while every request uses a distinct key.
        // CacheRoot 被共享，同时每个请求使用不同键。
        let cache_root = make_download_test_root("parallel_distinct");
        // Manager is cloned into all callers.
        // Manager 被克隆到所有调用方中。
        let manager = make_test_download_manager(cache_root.clone());
        // StartBarrier releases all distinct requests in one scheduling wave.
        // StartBarrier 在同一调度批次释放全部不同请求。
        let start_barrier = Arc::new(Barrier::new(request_count + 1));
        // Handles own every distinct network result.
        // Handles 拥有每个不同网络结果。
        let handles = servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                // ThreadManager reuses the same process-wide executor.
                // ThreadManager 复用同一个进程级执行器。
                let thread_manager = manager.clone();
                // ThreadBarrier coordinates simultaneous submission.
                // ThreadBarrier 协调同时提交。
                let thread_barrier = start_barrier.clone();
                // ThreadUrl identifies this caller's independent listener.
                // ThreadUrl 标识当前调用方的独立监听器。
                let thread_url = server.url.clone();
                thread::spawn(move || {
                    thread_barrier.wait();
                    thread_manager.download(&DownloadRequest {
                        source_type: DependencySourceType::Url,
                        source_locator: thread_url,
                        cache_key: format!("parallel-{index}"),
                    })
                })
            })
            .collect::<Vec<_>>();
        start_barrier.wait();
        for handle in handles {
            handle
                .join()
                .expect("join distinct downloader")
                .expect("distinct download should succeed");
        }
        // DeliveredCounts proves every endpoint received exactly one request.
        // DeliveredCounts 证明每个端点都精确收到一个请求。
        let delivered_counts = servers
            .into_iter()
            .map(TestHttpServer::finish)
            .collect::<Vec<_>>();
        assert!(delivered_counts.iter().all(|count| *count == 1));
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify a new checksum-constrained download retries once and publishes nothing after two mismatches.
    /// 验证新的 checksum 约束下载会重试一次，并在两次不匹配后不发布任何内容。
    #[test]
    fn new_checksum_download_retries_once_without_publishing_mismatches() {
        // TrustedPayload defines an expected checksum absent from both responses.
        // TrustedPayload 定义两个响应都不具备的期望 checksum。
        let trusted_payload = b"trusted";
        // ExpectedSha256 is the normalized target digest.
        // ExpectedSha256 是归一化目标摘要。
        let expected_sha256 = format!("{:x}", Sha256::digest(trusted_payload));
        // FirstWrong and SecondWrong exercise the automatic redownload path.
        // FirstWrong 与 SecondWrong 验证自动重新下载路径。
        let first_wrong = b"first-wrong";
        let second_wrong = b"second-wrong";
        // Server provides exactly the initial attempt and one retry.
        // Server 精确提供首次尝试与一次重试。
        let server = TestHttpServer::start(vec![
            test_http_response(first_wrong, Some(first_wrong.len())),
            test_http_response(second_wrong, Some(second_wrong.len())),
        ]);
        // CacheRoot starts without the target file.
        // CacheRoot 初始不包含目标文件。
        let cache_root = make_download_test_root("checksum_retry");
        // Manager performs both attempts on the reusable executor.
        // Manager 在可复用执行器上执行两次尝试。
        let manager = make_test_download_manager(cache_root.clone());
        // Request selects the target checked after both failures.
        // Request 选择两次失败后接受检查的目标。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "checksum-retry".to_string(),
        };
        // TargetPath must remain absent because neither temp file passed validation.
        // TargetPath 必须保持缺失，因为两个临时文件都未通过校验。
        let target_path = manager.cached_path_for_request(&request);

        // Error retains both the first failure and automatic retry context.
        // Error 保留首次失败与自动重试上下文。
        let error = manager
            .download_with_sha256(&request, &expected_sha256)
            .expect_err("two checksum mismatches should fail");
        assert!(error.contains("Automatic redownload also failed checksum verification"));
        assert!(!target_path.exists());
        assert_eq!(server.finish(), 2);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Verify a zero-byte response publishes one empty cache file without progress fabrication.
    /// 验证零字节响应会发布一个空缓存文件，且不会虚构进度。
    #[test]
    fn streamed_download_publishes_empty_response() {
        // Server declares and sends an empty successful body.
        // Server 声明并发送空的成功正文。
        let server = TestHttpServer::start(vec![test_http_response(b"", Some(0))]);
        // CacheRoot isolates the empty-file publication.
        // CacheRoot 隔离空文件发布结果。
        let cache_root = make_download_test_root("empty");
        // Manager uses the same production streaming path as non-empty payloads.
        // Manager 使用与非空载荷相同的生产流式路径。
        let manager = make_test_download_manager(cache_root.clone());
        // Request selects one empty binary cache file.
        // Request 选择一个空二进制缓存文件。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "empty".to_string(),
        };

        // DownloadedPath must exist as a regular zero-length file.
        // DownloadedPath 必须作为普通零长度文件存在。
        let downloaded_path = manager.download(&request).expect("download empty body");
        assert_eq!(
            std::fs::metadata(&downloaded_path)
                .expect("read empty cache metadata")
                .len(),
            0
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }

    /// Measure and enforce bounded process memory while streaming and hashing a 100 MiB response.
    /// 测量并约束流式写入与哈希 100 MiB 响应时的进程内存。
    #[cfg(windows)]
    #[test]
    #[ignore = "explicit 100 MiB download performance acceptance"]
    fn large_streamed_checksum_download_has_bounded_peak_working_set() {
        // PayloadBytes is the priority-plan large-asset acceptance size.
        // PayloadBytes 是优先修复方案规定的大资产验收大小。
        let payload_bytes = 100_usize * 1024 * 1024;
        // HashChunk matches the server's reusable byte pattern without allocating the payload.
        // HashChunk 匹配服务端可复用字节模式，且不分配完整载荷。
        let hash_chunk = [0x7b_u8; 64 * 1024];
        // Hasher derives the expected digest incrementally from the same fixed chunk.
        // Hasher 使用同一固定块增量派生期望摘要。
        let mut hasher = Sha256::new();
        // RemainingHashBytes tracks exact bytes still needed by the expected digest.
        // RemainingHashBytes 跟踪期望摘要仍需处理的精确字节数。
        let mut remaining_hash_bytes = payload_bytes;
        while remaining_hash_bytes > 0 {
            // HashCount caps the final digest update without a tail allocation.
            // HashCount 限制最终摘要更新大小，且不分配尾部缓冲区。
            let hash_count = remaining_hash_bytes.min(hash_chunk.len());
            hasher.update(&hash_chunk[..hash_count]);
            remaining_hash_bytes -= hash_count;
        }
        // ExpectedSha256 is validated during the single network-to-disk pass.
        // ExpectedSha256 在单次网络到磁盘流程中完成验证。
        let expected_sha256 = format!("{:x}", hasher.finalize());
        // Server emits the full payload from one fixed-size buffer.
        // Server 使用一个固定大小缓冲区发出完整载荷。
        let server = TestHttpServer::start_streaming_body(payload_bytes);
        // CacheRoot isolates the 100 MiB artifact.
        // CacheRoot 隔离 100 MiB 产物。
        let cache_root = make_download_test_root("large_streamed");
        // Manager runs the production reusable-client streaming path.
        // Manager 运行生产可复用 Client 流式路径。
        let manager = make_test_download_manager(cache_root.clone());
        // Request selects one large deterministic cache target.
        // Request 选择一个大型确定性缓存目标。
        let request = DownloadRequest {
            source_type: DependencySourceType::Url,
            source_locator: server.url.clone(),
            cache_key: "large-streamed".to_string(),
        };
        // PeakBefore snapshots cumulative process peak immediately before the measured operation.
        // PeakBefore 在被测操作前立即记录累计进程峰值。
        let peak_before = windows_peak_working_set_bytes().expect("read pre-download peak memory");
        // StartedAt measures end-to-end streaming, hashing, synchronization, and publication time.
        // StartedAt 测量流式写入、哈希、同步与发布的端到端时间。
        let started_at = Instant::now();

        // DownloadedPath becomes visible only after the checksum-constrained publication succeeds.
        // DownloadedPath 仅在 checksum 约束发布成功后可见。
        let downloaded_path = manager
            .download_with_sha256(&request, &expected_sha256)
            .expect("stream and verify 100 MiB payload");
        // Elapsed records the measured end-to-end duration.
        // Elapsed 记录被测端到端耗时。
        let elapsed = started_at.elapsed();
        // PeakAfter captures the process lifetime maximum including the large operation.
        // PeakAfter 捕获包含大型操作的进程生命周期最大值。
        let peak_after = windows_peak_working_set_bytes().expect("read post-download peak memory");
        // PeakDelta is the additional measured working set attributable to this isolated test.
        // PeakDelta 是当前隔离测试可归因的额外实测工作集。
        let peak_delta = peak_after.saturating_sub(peak_before);

        assert_eq!(
            std::fs::metadata(&downloaded_path)
                .expect("read large cache metadata")
                .len(),
            payload_bytes as u64
        );
        assert!(
            peak_delta < 64 * 1024 * 1024,
            "100 MiB streaming download peak delta must stay below 64 MiB, measured {peak_delta} bytes"
        );
        println!(
            "DOWNLOAD_PERF payload_bytes={payload_bytes} peak_before={peak_before} peak_after={peak_after} peak_delta={peak_delta} elapsed_ms={}",
            elapsed.as_millis()
        );
        assert_eq!(server.finish(), 1);
        assert_no_download_temp_files(&cache_root);
        let _ = std::fs::remove_dir_all(&cache_root);
    }
}
