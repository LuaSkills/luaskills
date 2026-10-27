use serde::Deserialize;

/// Official GitHub REST API host, the only host that ever receives the host's GitHub token.
/// 官方 GitHub REST API 主机，也是唯一会收到宿主 GitHub 令牌的主机。
pub const OFFICIAL_GITHUB_API_HOST: &str = "api.github.com";

/// Minimal GitHub release API asset record used to resolve browser download URLs.
/// 用于解析浏览器下载地址的最小 GitHub release API 资产记录。
#[derive(Debug, Clone, Deserialize)]
pub struct GithubReleaseAssetRecord {
    /// Exact asset file name exposed by GitHub release metadata.
    /// GitHub release 元数据中暴露的精确资产文件名。
    pub name: String,
    /// Browser download URL exposed by GitHub release metadata.
    /// GitHub release 元数据中暴露的浏览器下载地址。
    pub browser_download_url: String,
    /// Optional content digest such as `sha256:<hex>`, reported by GitHub and the LuaSkills Hub mirror.
    /// 可选内容摘要，例如 `sha256:<hex>`，由 GitHub 与 LuaSkills Hub 镜像提供。
    #[serde(default)]
    pub digest: Option<String>,
}

impl GithubReleaseAssetRecord {
    /// Return the lowercase SHA-256 hex of a `sha256:<hex>` digest, or `None` for other digests.
    /// 返回 `sha256:<hex>` 摘要的小写 SHA-256 十六进制值；其他摘要返回 `None`。
    pub fn sha256_digest(&self) -> Option<String> {
        let hex = self.digest.as_deref()?.trim().strip_prefix("sha256:")?;
        (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    }
}

/// Return the token to attach to one API request: only the official GitHub API ever receives it.
/// 返回单个 API 请求应附加的令牌：只有官方 GitHub API 会收到令牌。
pub fn github_api_token_for<'a>(
    api_url: &str,
    token: Option<&'a crate::host::options::LuaRuntimeSecretString>,
) -> Option<&'a crate::host::options::LuaRuntimeSecretString> {
    token.filter(|_| is_official_github_api_url(api_url))
}

/// Return whether one request URL targets the official GitHub API over HTTPS on the default port.
/// 判断单个请求 URL 是否通过 HTTPS 默认端口指向官方 GitHub API。
pub fn is_official_github_api_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|parsed| {
        parsed.scheme() == "https"
            && parsed.host_str() == Some(OFFICIAL_GITHUB_API_HOST)
            && parsed.port().is_none()
    })
}

/// Minimal GitHub release API response used by the downloader.
/// 下载器使用的最小 GitHub release API 响应。
#[derive(Debug, Clone, Deserialize)]
pub struct GithubReleaseApiResponse {
    /// Release tag name such as `v14.1.0`.
    /// 例如 `v14.1.0` 一类的 release 标签名。
    pub tag_name: String,
    /// Asset list carried by the release.
    /// 当前 release 携带的资产列表。
    #[serde(default)]
    pub assets: Vec<GithubReleaseAssetRecord>,
}

/// Rewrite one GitHub browser download URL through the host-provided site base URL override.
/// 通过宿主提供的站点基址覆盖重写单个 GitHub 浏览器下载地址。
pub fn rewrite_github_download_url(download_url: &str, github_base_url: Option<&str>) -> String {
    let Some(base_url) = github_base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return download_url.to_string();
    };
    let normalized_base = base_url.trim_end_matches('/');
    if download_url.starts_with("https://github.com/") {
        format!(
            "{}/{}",
            normalized_base,
            download_url.trim_start_matches("https://github.com/")
        )
    } else if download_url.starts_with("http://github.com/") {
        format!(
            "{}/{}",
            normalized_base,
            download_url.trim_start_matches("http://github.com/")
        )
    } else {
        download_url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{GithubReleaseAssetRecord, github_api_token_for, is_official_github_api_url};
    use crate::host::options::LuaRuntimeSecretString;

    /// Only HTTPS requests to api.github.com on the default port count as the official API.
    /// 只有发往 api.github.com 默认端口的 HTTPS 请求才视为官方 API。
    #[test]
    fn official_github_api_url_requires_exact_https_host() {
        assert!(is_official_github_api_url(
            "https://api.github.com/repos/a/b/releases/latest"
        ));
        for url in [
            "http://api.github.com/repos/a/b/releases/latest",
            "https://api.github.com:8443/repos/a/b",
            "https://api.github.com.evil.example/repos/a/b",
            "https://hub.luaskills.cn/github-api/repos/a/b/releases/latest",
            "https://gh-proxy.com/https://api.github.com/repos/a/b",
            "not a url",
        ] {
            assert!(
                !is_official_github_api_url(url),
                "{url} must not count as the official API"
            );
        }
    }

    /// The token is handed out for the official API and withheld from every other host.
    /// 令牌只交给官方 API，对其他任何主机都不提供。
    #[test]
    fn github_token_is_withheld_from_mirrors_and_stations() {
        let token = LuaRuntimeSecretString::new("ghp_fixture");
        assert!(
            github_api_token_for(
                "https://api.github.com/repos/a/b/releases/latest",
                Some(&token)
            )
            .is_some()
        );
        assert!(
            github_api_token_for(
                "https://hub.luaskills.com/github-api/repos/a/b",
                Some(&token)
            )
            .is_none()
        );
        assert!(github_api_token_for("https://api.github.com/repos/a/b", None).is_none());
    }

    /// Only well-formed sha256 digests are used; other algorithms and malformed values are ignored.
    /// 只采用格式正确的 sha256 摘要；其他算法与格式错误的值一律忽略。
    #[test]
    fn asset_digest_accepts_only_sha256_hex() {
        let asset = |digest: Option<&str>| GithubReleaseAssetRecord {
            name: "a.zip".to_string(),
            browser_download_url: "https://github.com/a/b/releases/download/v1/a.zip".to_string(),
            digest: digest.map(str::to_string),
        };
        let hex = "AB".repeat(32);
        assert_eq!(
            asset(Some(&format!("sha256:{hex}"))).sha256_digest(),
            Some("ab".repeat(32))
        );
        assert_eq!(asset(Some("sha512:abcd")).sha256_digest(), None);
        assert_eq!(asset(Some("sha256:zz")).sha256_digest(), None);
        assert_eq!(asset(None).sha256_digest(), None);
    }
}
