use serde::{Deserialize, Serialize};

use crate::lua_skill::{validate_luaskills_identifier, validate_luaskills_version};
use crate::skill::source::SkillInstallSourceType;

/// Shared source manifest returned by official Hub resolve APIs or private host URL manifests.
/// 官方 Hub resolve API 或宿主私有 URL manifest 返回的共享来源清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSourceManifest {
    /// Optional schema version used by hosts and Hubs to evolve the manifest format.
    /// 宿主与 Hub 用于演进 manifest 格式的可选 schema 版本。
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Stable skill identifier described by the source manifest.
    /// 来源清单描述的稳定技能标识符。
    pub skill_id: String,
    /// Semantic version of the skill package archive.
    /// 技能包归档的语义化版本。
    pub version: String,
    /// Downloadable archive metadata for the resolved skill package.
    /// 已解析技能包的可下载归档元数据。
    pub archive: SkillSourceArchive,
    /// Layout of the resolved package (`platform` or `universal`), present from schema version 2.
    /// 已解析包的布局（`platform` 或 `universal`），自结构版本 2 起提供。
    #[serde(default)]
    pub layout: Option<String>,
    /// Platform of a resolved platform package.
    /// 已解析平台包的平台。
    #[serde(default)]
    pub platform: Option<String>,
    /// Optional update source recorded after installation.
    /// 安装后记录的可选更新来源。
    #[serde(default)]
    pub update: Option<SkillSourceUpdate>,
}

/// Downloadable archive metadata inside one source manifest.
/// 单个来源清单中的可下载归档元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSourceArchive {
    /// Archive type, currently only `zip` is accepted by the managed install path.
    /// 归档类型，当前受管安装路径仅接受 `zip`。
    #[serde(rename = "type", alias = "archive_type")]
    pub archive_type: String,
    /// Exact archive download URL returned by the trusted source.
    /// 可信来源返回的精确归档下载 URL。
    pub url: String,
    /// Expected SHA-256 checksum for the archive payload.
    /// 归档载荷的期望 SHA-256 校验值。
    pub sha256: String,
}

/// Optional source descriptor that can provide diagnostic update metadata.
/// 可提供诊断性更新元数据的可选来源描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSourceUpdate {
    /// Managed source type used by future update checks.
    /// 后续更新检查使用的受管来源类型。
    pub source_type: SkillInstallSourceType,
    /// Stable source locator used by future update checks.
    /// 后续更新检查使用的稳定来源定位值。
    pub locator: String,
    /// Optional source tag recorded for diagnostics.
    /// 为诊断记录的可选来源标签。
    #[serde(default)]
    pub tag: Option<String>,
}

impl SkillSourceManifest {
    /// Validate this manifest against an expected skill id and return the normalized SHA-256 checksum.
    /// 根据期望 skill id 校验当前 manifest，并返回标准化后的 SHA-256 校验值。
    pub fn validate_for_skill(&self, expected_skill_id: &str) -> Result<String, String> {
        validate_luaskills_identifier(&self.skill_id, "source manifest skill_id")?;
        validate_luaskills_identifier(expected_skill_id, "expected skill_id")?;
        if self.skill_id != expected_skill_id {
            return Err(format!(
                "source manifest resolves to skill_id '{}' instead of '{}'",
                self.skill_id, expected_skill_id
            ));
        }
        validate_luaskills_version(&self.version, "source manifest version")?;
        if !self.archive.archive_type.trim().eq_ignore_ascii_case("zip") {
            return Err(format!(
                "source manifest archive type '{}' is not supported; expected zip",
                self.archive.archive_type
            ));
        }
        if self.archive.url.trim().is_empty() {
            return Err("source manifest archive.url must not be empty".to_string());
        }
        normalize_sha256(self.archive.sha256.as_str())
    }
}

impl SkillSourceManifest {
    /// Return the package platform the archive must carry (`Some(None)` for universal), or `None` without a layout.
    /// 返回归档必须携带的包平台（通用包为 `Some(None)`）；未声明布局时返回 `None`。
    pub fn expected_package_platform(
        &self,
        runtime_platform: &str,
    ) -> Result<Option<Option<String>>, String> {
        match self.layout.as_deref() {
            None => {
                if self.platform.is_some() {
                    return Err("source manifest declares platform without layout".to_string());
                }
                Ok(None)
            }
            Some("universal") => {
                if self.platform.is_some() {
                    return Err("source manifest universal layout must not declare platform".to_string());
                }
                Ok(Some(None))
            }
            Some("platform") => {
                let platform = self
                    .platform
                    .as_deref()
                    .ok_or_else(|| "source manifest platform layout requires platform".to_string())?;
                if platform != runtime_platform {
                    return Err(format!(
                        "{}: source manifest resolved the '{}' package but this runtime is '{}'",
                        crate::package::SKILL_PACKAGE_PLATFORM_MISMATCH,
                        platform,
                        runtime_platform
                    ));
                }
                Ok(Some(Some(platform.to_string())))
            }
            Some(other) => Err(format!(
                "source manifest layout '{}' is not supported; expected platform or universal",
                other
            )),
        }
    }
}

/// Parse one source manifest from YAML or JSON text.
/// 从 YAML 或 JSON 文本解析单个来源清单。
pub fn parse_skill_source_manifest(
    content: &str,
    origin: &str,
) -> Result<SkillSourceManifest, String> {
    serde_norway::from_str::<SkillSourceManifest>(content).map_err(|error| {
        format!(
            "Failed to parse skill source manifest from {}: {}",
            origin, error
        )
    })
}

/// Normalize and validate one SHA-256 checksum string.
/// 规范化并校验单个 SHA-256 校验字符串。
pub fn normalize_sha256(value: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() == 64 && normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Ok(normalized);
    }
    Err("source manifest archive.sha256 must be one valid SHA-256 value".to_string())
}
