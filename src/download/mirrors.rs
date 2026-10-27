//! Built-in GitHub release download mirrors and the speed probe hosts use to compare them.
//! 内置 GitHub Release 下载镜像，以及宿主用来比较镜像的测速探针。
//!
//! A mirror only replaces the `https://github.com` prefix of release download URLs. Release
//! metadata keeps coming from the GitHub API, and integrity is enforced by the release digest
//! and checksum files rather than by trusting the mirror. The LuaSkills Hub is a separate
//! publishing system with its own download addresses, not a GitHub mirror.
//! 镜像只替换 Release 下载地址中的 `https://github.com` 前缀。Release 元数据仍来自 GitHub API，
//! 完整性由 Release 摘要与校验文件保证，而不是依赖对镜像的信任。LuaSkills Hub 是拥有自身下载地址的
//! 独立发布体系，不是 GitHub 镜像。

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

/// Release asset path probed on every mirror, relative to the GitHub web root.
/// 在每个镜像上探测的 Release 资产路径，相对于 GitHub 网页根。
///
/// The same path works for official GitHub and prefix proxies because both keep the GitHub
/// release layout below their base URL.
/// 官方 GitHub 与前缀代理都在基址下保持 GitHub Release 布局，因此同一路径通用。
pub const MIRROR_PROBE_DEFAULT_RELEASE_PATH: &str =
    "LuaSkills/luaskills/releases/download/v0.6.1/luaskills-debug-tool-windows-x64.tar.gz";

/// Default number of payload bytes read from each mirror.
/// 每个镜像默认读取的载荷字节数。
pub const MIRROR_PROBE_DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

/// Upper bound accepted for `max_bytes`, so a probe never turns into a full download.
/// `max_bytes` 可接受的上限，避免测速变成完整下载。
pub const MIRROR_PROBE_MAX_BYTES_LIMIT: u64 = 8 * 1024 * 1024;

/// Default deadline for one mirror, covering connect, headers and the sampled body.
/// 单个镜像的默认截止时间，涵盖连接、响应头与采样正文。
pub const MIRROR_PROBE_DEFAULT_TIMEOUT_MS: u64 = 15_000;

/// Upper bound accepted for `timeout_ms`.
/// `timeout_ms` 可接受的上限。
pub const MIRROR_PROBE_TIMEOUT_LIMIT_MS: u64 = 60_000;

/// Maximum number of mirrors probed in one request; each runs on its own thread.
/// 单次请求最多探测的镜像数量；每个镜像在独立线程中运行。
pub const MIRROR_PROBE_MAX_TARGETS: usize = 32;

/// Official GitHub web root that mirror base URLs replace.
/// 镜像基址所替换的官方 GitHub 网页根。
pub const OFFICIAL_GITHUB_BASE_URL: &str = "https://github.com";

/// How one mirror maps release download URLs.
/// 单个镜像映射 Release 下载地址的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LuaRuntimeGithubMirrorKind {
    /// Official GitHub without any rewrite.
    /// 官方 GitHub，不做任何改写。
    Direct,
    /// Public proxy that takes the full GitHub URL after its own origin.
    /// 在自身源地址后接收完整 GitHub 地址的公共代理。
    Prefix,
}

/// Network region a mirror is meant to serve.
/// 镜像主要服务的网络区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LuaRuntimeGithubMirrorRegion {
    /// Mainland China networks.
    /// 中国大陆网络。
    China,
    /// Networks outside mainland China.
    /// 中国大陆以外的网络。
    Global,
}

/// One built-in GitHub release download mirror.
/// 单个内置 GitHub Release 下载镜像。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct LuaRuntimeGithubMirror {
    /// Stable mirror identifier stored by hosts.
    /// 宿主持久保存的稳定镜像标识。
    pub id: String,
    /// English display label; hosts may localize it by `id`.
    /// 英文显示名称；宿主可按 `id` 本地化。
    pub label: String,
    /// URL mapping used by this mirror.
    /// 此镜像使用的地址映射方式。
    pub kind: LuaRuntimeGithubMirrorKind,
    /// Network region this mirror is meant to serve.
    /// 此镜像主要服务的网络区域。
    pub region: LuaRuntimeGithubMirrorRegion,
    /// Base that replaces `https://github.com` in release download URLs; `None` keeps GitHub.
    /// 替换 Release 下载地址中 `https://github.com` 的基址；`None` 表示保持 GitHub。
    pub github_base_url: Option<String>,
}

/// One mirror to probe; built-in and user-defined mirrors share this shape.
/// 单个待探测镜像；内置镜像与用户自定义镜像共用此结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct LuaRuntimeGithubMirrorProbeTarget {
    /// Caller-chosen identifier echoed in the result.
    /// 调用方选择、在结果中原样返回的标识。
    pub id: String,
    /// Base that replaces `https://github.com`; `None` probes official GitHub.
    /// 替换 `https://github.com` 的基址；`None` 表示探测官方 GitHub。
    #[serde(default)]
    pub github_base_url: Option<String>,
}

/// Request for probing several mirrors in parallel.
/// 并行探测多个镜像的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct LuaRuntimeGithubMirrorProbeRequest {
    /// Mirrors to probe, at most `MIRROR_PROBE_MAX_TARGETS`.
    /// 待探测镜像，最多 `MIRROR_PROBE_MAX_TARGETS` 个。
    pub targets: Vec<LuaRuntimeGithubMirrorProbeTarget>,
    /// Release asset path relative to the GitHub web root; defaults to the built-in probe asset.
    /// 相对 GitHub 网页根的 Release 资产路径；默认使用内置探测资产。
    #[serde(default)]
    pub release_path: Option<String>,
    /// Payload bytes to sample from each mirror.
    /// 每个镜像采样的载荷字节数。
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Deadline for each mirror in milliseconds.
    /// 每个镜像的截止时间（毫秒）。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Measured outcome for one mirror.
/// 单个镜像的测量结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct LuaRuntimeGithubMirrorProbeResult {
    /// Identifier copied from the target.
    /// 从探测目标复制的标识。
    pub id: String,
    /// Exact URL that was requested.
    /// 实际请求的地址。
    pub url: String,
    /// Whether the mirror served release bytes.
    /// 镜像是否返回了 Release 字节。
    pub ok: bool,
    /// Whether the full sample (or the whole asset) arrived before the deadline.
    /// 截止前是否收到完整样本（或整个资产）。
    pub complete: bool,
    /// HTTP status of the final response, when one arrived.
    /// 收到最终响应时的 HTTP 状态码。
    pub http_status: Option<u16>,
    /// Milliseconds until response headers arrived.
    /// 收到响应头所用的毫秒数。
    pub ttfb_ms: Option<u64>,
    /// Total milliseconds spent on this mirror.
    /// 此镜像总共花费的毫秒数。
    pub elapsed_ms: u64,
    /// Payload bytes read.
    /// 已读取的载荷字节数。
    pub bytes_read: u64,
    /// Body throughput measured after the headers arrived.
    /// 收到响应头之后测得的正文吞吐量。
    pub bytes_per_second: Option<u64>,
    /// Failure reason when `ok` is false.
    /// `ok` 为 false 时的失败原因。
    pub error: Option<String>,
}

/// Return the built-in mirror catalog in display order.
/// 按显示顺序返回内置镜像目录。
///
/// The public proxies were selected by measured reachability from mainland China; they are
/// third-party services, so integrity always comes from release digests and checksum files.
/// 公共代理按中国大陆实测可达性挑选；它们是第三方服务，因此完整性始终来自 Release 摘要与校验文件。
pub fn builtin_github_mirrors() -> Vec<LuaRuntimeGithubMirror> {
    use LuaRuntimeGithubMirrorKind::{Direct, Prefix};
    use LuaRuntimeGithubMirrorRegion::{China, Global};
    let prefix = |id: &str, label: &str, region, origin: &str| LuaRuntimeGithubMirror {
        id: id.to_string(),
        label: label.to_string(),
        kind: Prefix,
        region,
        github_base_url: Some(format!("{origin}/{OFFICIAL_GITHUB_BASE_URL}")),
    };
    vec![
        LuaRuntimeGithubMirror {
            id: "github".to_string(),
            label: "GitHub".to_string(),
            kind: Direct,
            region: Global,
            github_base_url: None,
        },
        prefix(
            "gh-proxy-com",
            "gh-proxy.com",
            China,
            "https://gh-proxy.com",
        ),
        prefix("ghfast-top", "ghfast.top", China, "https://ghfast.top"),
        prefix("gh-llkk-cc", "gh.llkk.cc", China, "https://gh.llkk.cc"),
        prefix("ghproxy-net", "ghproxy.net", Global, "https://ghproxy.net"),
    ]
}

/// Normalize a user-entered mirror into a base URL that replaces `https://github.com`.
/// 将用户输入的镜像规范化为替换 `https://github.com` 的基址。
///
/// A bare origin such as `https://ghfast.top` is a prefix proxy and gets the GitHub URL
/// appended; any other path is kept as a GitHub-compatible layout root.
/// 形如 `https://ghfast.top` 的裸源地址视为前缀代理并追加 GitHub 地址；其他路径按 GitHub 兼容布局根保留。
pub fn normalize_github_mirror_base_url(input: &str) -> Result<String, String> {
    let trimmed = input.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|error| format!("Invalid mirror URL '{}': {}", trimmed, error))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("Mirror URL '{}' must use http or https", trimmed));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!(
            "Mirror URL '{}' must not contain a query or fragment",
            trimmed
        ));
    }
    if parsed.path().trim_matches('/').is_empty() {
        return Ok(format!("{trimmed}/{OFFICIAL_GITHUB_BASE_URL}"));
    }
    Ok(trimmed.to_string())
}

/// Probe every target in parallel and return results in request order.
/// 并行探测全部目标，并按请求顺序返回结果。
pub fn probe_github_mirrors(
    request: &LuaRuntimeGithubMirrorProbeRequest,
) -> Result<Vec<LuaRuntimeGithubMirrorProbeResult>, String> {
    if request.targets.len() > MIRROR_PROBE_MAX_TARGETS {
        return Err(format!(
            "At most {} mirrors can be probed at once, got {}",
            MIRROR_PROBE_MAX_TARGETS,
            request.targets.len()
        ));
    }
    let release_path = request
        .release_path
        .as_deref()
        .unwrap_or(MIRROR_PROBE_DEFAULT_RELEASE_PATH)
        .trim()
        .trim_start_matches('/')
        .to_string();
    if release_path.is_empty() {
        return Err("Mirror probe release_path must not be empty".to_string());
    }
    let max_bytes = request
        .max_bytes
        .unwrap_or(MIRROR_PROBE_DEFAULT_MAX_BYTES)
        .clamp(1, MIRROR_PROBE_MAX_BYTES_LIMIT);
    let timeout = Duration::from_millis(
        request
            .timeout_ms
            .unwrap_or(MIRROR_PROBE_DEFAULT_TIMEOUT_MS)
            .clamp(1, MIRROR_PROBE_TIMEOUT_LIMIT_MS),
    );
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("luaskills/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
        .map_err(|error| format!("Failed to build HTTP client: {}", error))?;
    let handles: Vec<_> = request
        .targets
        .iter()
        .map(|target| {
            let target = target.clone();
            let client = client.clone();
            let release_path = release_path.clone();
            thread::spawn(move || probe_one_mirror(&client, &target, &release_path, max_bytes))
        })
        .collect();
    Ok(handles
        .into_iter()
        .zip(&request.targets)
        .map(|(handle, target)| {
            handle
                .join()
                .unwrap_or_else(|_| LuaRuntimeGithubMirrorProbeResult {
                    id: target.id.clone(),
                    url: String::new(),
                    ok: false,
                    complete: false,
                    http_status: None,
                    ttfb_ms: None,
                    elapsed_ms: 0,
                    bytes_read: 0,
                    bytes_per_second: None,
                    error: Some("Mirror probe thread panicked".to_string()),
                })
        })
        .collect())
}

/// Probe one mirror by sampling the beginning of the probe asset.
/// 通过采样探测资产开头部分来探测单个镜像。
fn probe_one_mirror(
    client: &reqwest::blocking::Client,
    target: &LuaRuntimeGithubMirrorProbeTarget,
    release_path: &str,
    max_bytes: u64,
) -> LuaRuntimeGithubMirrorProbeResult {
    let base = match target.github_base_url.as_deref() {
        Some(base) => match normalize_github_mirror_base_url(base) {
            Ok(base) => base,
            Err(error) => return failed_probe(target, String::new(), None, None, 0, error),
        },
        None => OFFICIAL_GITHUB_BASE_URL.to_string(),
    };
    let url = format!("{base}/{release_path}");
    let started = Instant::now();
    let response = match client.get(&url).send() {
        Ok(response) => response,
        Err(error) => {
            let elapsed = elapsed_ms(started);
            return failed_probe(
                target,
                url,
                None,
                None,
                elapsed,
                error.without_url().to_string(),
            );
        }
    };
    let ttfb = elapsed_ms(started);
    let status = response.status();
    if !status.is_success() {
        let error = format!("HTTP {}", status.as_u16());
        return failed_probe(target, url, Some(status.as_u16()), Some(ttfb), ttfb, error);
    }
    // Several dead proxies answer 200 with a landing page instead of the asset.
    // 一些失效代理会以 200 返回落地页而不是资产本身。
    let is_html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"));
    if is_html {
        let error = "Mirror returned an HTML page instead of the release asset".to_string();
        return failed_probe(target, url, Some(status.as_u16()), Some(ttfb), ttfb, error);
    }
    let body_started = Instant::now();
    let mut reader = response.take(max_bytes);
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes_read = 0_u64;
    let mut complete = true;
    let mut read_error = None;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => bytes_read += read as u64,
            Err(error) => {
                complete = false;
                read_error = Some(error.to_string());
                break;
            }
        }
    }
    let body_ms = body_started.elapsed().as_millis().max(1) as u64;
    let elapsed = elapsed_ms(started);
    if bytes_read == 0 {
        let error = read_error.unwrap_or_else(|| "Mirror returned an empty body".to_string());
        return failed_probe(
            target,
            url,
            Some(status.as_u16()),
            Some(ttfb),
            elapsed,
            error,
        );
    }
    // A deadline hit after some bytes still yields a usable, if partial, throughput sample.
    // 读到部分字节后才到达截止时间时，仍可得到可用（但不完整）的吞吐样本。
    LuaRuntimeGithubMirrorProbeResult {
        id: target.id.clone(),
        url,
        ok: true,
        complete,
        http_status: Some(status.as_u16()),
        ttfb_ms: Some(ttfb),
        elapsed_ms: elapsed,
        bytes_read,
        bytes_per_second: Some(bytes_read.saturating_mul(1000) / body_ms),
        error: None,
    }
}

/// Build one failed probe result.
/// 构造单个失败的探测结果。
fn failed_probe(
    target: &LuaRuntimeGithubMirrorProbeTarget,
    url: String,
    http_status: Option<u16>,
    ttfb_ms: Option<u64>,
    elapsed_ms: u64,
    error: String,
) -> LuaRuntimeGithubMirrorProbeResult {
    LuaRuntimeGithubMirrorProbeResult {
        id: target.id.clone(),
        url,
        ok: false,
        complete: false,
        http_status,
        ttfb_ms,
        elapsed_ms,
        bytes_read: 0,
        bytes_per_second: None,
        error: Some(error),
    }
}

/// Return whole milliseconds elapsed since `started`.
/// 返回自 `started` 起经过的整毫秒数。
fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{
        LuaRuntimeGithubMirrorKind, LuaRuntimeGithubMirrorProbeRequest,
        LuaRuntimeGithubMirrorProbeTarget, MIRROR_PROBE_MAX_TARGETS, builtin_github_mirrors,
        normalize_github_mirror_base_url, probe_github_mirrors,
    };
    use std::collections::HashSet;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Serve one raw response per planned connection on a loopback port and return its base URL.
    /// 在回环端口上按计划每个连接回答一份原始响应，并返回基址。
    fn serve(responses: Vec<Vec<u8>>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let url = format!("http://{}", listener.local_addr().expect("probe address"));
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept probe request");
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).expect("read probe request") == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                let _ = stream.write_all(&response);
            }
        });
        (url, handle)
    }

    /// Build one raw HTTP/1.1 response with the given content type.
    /// 构造带指定内容类型的原始 HTTP/1.1 响应。
    fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    /// Built-in mirrors have unique ids, and every rewriting mirror normalizes to itself.
    /// 内置镜像标识唯一，且每个改写型镜像规范化后保持不变。
    #[test]
    fn builtin_catalog_is_unique_and_normalized() {
        let mirrors = builtin_github_mirrors();
        let ids: HashSet<_> = mirrors.iter().map(|mirror| mirror.id.as_str()).collect();
        assert_eq!(ids.len(), mirrors.len(), "mirror ids must be unique");
        assert!(mirrors.len() <= MIRROR_PROBE_MAX_TARGETS);
        for mirror in &mirrors {
            match mirror.kind {
                LuaRuntimeGithubMirrorKind::Direct => assert!(mirror.github_base_url.is_none()),
                _ => {
                    let base = mirror.github_base_url.as_deref().expect("mirror base url");
                    assert_eq!(normalize_github_mirror_base_url(base).as_deref(), Ok(base));
                }
            }
        }
    }

    /// A bare origin becomes a prefix proxy; layout roots stay; unsafe input is rejected.
    /// 裸源地址成为前缀代理；布局根保持不变；不安全的输入被拒绝。
    #[test]
    fn custom_mirror_urls_are_normalized() {
        assert_eq!(
            normalize_github_mirror_base_url(" https://example.cn/ ").as_deref(),
            Ok("https://example.cn/https://github.com")
        );
        assert_eq!(
            normalize_github_mirror_base_url("https://res.example.cn/github/").as_deref(),
            Ok("https://res.example.cn/github")
        );
        for invalid in [
            "ftp://example.cn",
            "example.cn",
            "https://example.cn/?a=1",
            "",
        ] {
            assert!(
                normalize_github_mirror_base_url(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    /// Asset bytes count as success, while landing pages and HTTP errors are reported as failures.
    /// 资产字节视为成功，落地页与 HTTP 错误报告为失败。
    #[test]
    fn probe_measures_assets_and_rejects_landing_pages() {
        let payload = vec![7_u8; 4096];
        let (asset_base, asset_server) = serve(vec![response(
            "200 OK",
            "application/octet-stream",
            &payload,
        )]);
        let (html_base, html_server) = serve(vec![response(
            "200 OK",
            "text/html; charset=utf-8",
            b"<html></html>",
        )]);
        let (missing_base, missing_server) =
            serve(vec![response("404 Not Found", "text/plain", b"missing")]);
        let target = |id: &str, base: &str| LuaRuntimeGithubMirrorProbeTarget {
            id: id.to_string(),
            github_base_url: Some(format!("{base}/mirror")),
        };
        let results = probe_github_mirrors(&LuaRuntimeGithubMirrorProbeRequest {
            targets: vec![
                target("asset", &asset_base),
                target("html", &html_base),
                target("missing", &missing_base),
            ],
            release_path: Some("/demo/demo/releases/download/v1/a.bin".to_string()),
            max_bytes: Some(1024),
            timeout_ms: Some(5_000),
        })
        .expect("probe request");
        for server in [asset_server, html_server, missing_server] {
            server.join().expect("probe server");
        }
        let ids: Vec<_> = results.iter().map(|result| result.id.as_str()).collect();
        assert_eq!(ids, ["asset", "html", "missing"]);
        assert!(results[0].ok && results[0].complete);
        assert_eq!(results[0].bytes_read, 1024);
        assert!(
            results[0]
                .url
                .ends_with("/mirror/demo/demo/releases/download/v1/a.bin")
        );
        assert!(!results[1].ok);
        assert!(
            results[1]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("HTML"))
        );
        assert!(!results[2].ok);
        assert_eq!(results[2].http_status, Some(404));
    }

    /// Requests over the target limit are rejected before any network work.
    /// 超过目标上限的请求在任何网络操作之前被拒绝。
    #[test]
    fn probe_rejects_too_many_targets() {
        let targets = (0..=MIRROR_PROBE_MAX_TARGETS)
            .map(|index| LuaRuntimeGithubMirrorProbeTarget {
                id: index.to_string(),
                github_base_url: None,
            })
            .collect();
        let request = LuaRuntimeGithubMirrorProbeRequest {
            targets,
            release_path: None,
            max_bytes: None,
            timeout_ms: None,
        };
        assert!(probe_github_mirrors(&request).is_err());
    }
}
