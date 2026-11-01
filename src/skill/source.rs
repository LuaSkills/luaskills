use serde::{Deserialize, Serialize};

/// Supported managed install source types recorded by LuaSkills.
/// LuaSkills 记录的受管安装来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillInstallSourceType {
    /// GitHub Release source tracked by repository and release tag.
    /// 通过仓库与发布标签追踪的 GitHub Release 来源。
    #[default]
    Github,
    /// Official LuaSkills Hub source resolved by skill id.
    /// 通过技能标识符解析的官方 LuaSkills Hub 来源。
    OfficialHub,
    /// Remote source YAML URL that describes one skill package.
    /// 描述单个技能包的远程 source YAML 地址。
    Url,
    /// Host-private URL manifest source available only to trusted system operations.
    /// 仅可信 system 操作可用的宿主私有 URL manifest 来源。
    PrivateUrlManifest,
}

/// Stable source descriptor persisted for one managed skill installation.
/// 为单个受管技能安装持久化的稳定来源描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkillSourceRecord {
    /// Stable source type used for future update checks.
    /// 用于后续更新检查的稳定来源类型。
    pub source_type: SkillInstallSourceType,
    /// Stable source locator such as `owner/repo` or one source YAML URL.
    /// 稳定来源定位值，例如 `owner/repo` 或某个 source YAML 地址。
    pub locator: String,
    /// Optional resolved release tag recorded during installation.
    /// 安装时记录的可选已解析发布标签。
    #[serde(default)]
    pub tag: Option<String>,
}

/// Persistent install record written only for managed skill installations.
/// 仅为受管技能安装写入的持久化安装记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkillRecord {
    /// Stable skill identifier currently managed by the install record.
    /// 当前安装记录所管理的稳定技能标识符。
    pub skill_id: String,
    /// Installed semantic version tracked for update checks.
    /// 用于更新检查的已安装语义化版本。
    pub version: String,
    /// Whether the current skill is managed by the install workflow.
    /// 当前技能是否由安装流程受管。
    pub managed: bool,
    /// Structured source descriptor of the current managed installation.
    /// 当前受管安装的结构化来源描述。
    pub source: InstalledSkillSourceRecord,
    /// Unix timestamp in milliseconds when the install record was written.
    /// 当前安装记录写入时的 Unix 毫秒时间戳。
    pub installed_at_unix_ms: u128,
    /// Platform of the installed platform package, or `None` for a universal package.
    /// 已安装平台包的平台；通用包为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_platform: Option<String>,
}
