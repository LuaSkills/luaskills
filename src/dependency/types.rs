use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Dependency categories managed by LuaSkills for one skill package.
/// LuaSkills 为单个技能包管理的依赖分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDependencyKind {
    Tool,
    Lua,
    Ffi,
}

/// Install scope that decides whether one dependency is skill-private or host-provided.
/// 决定单个依赖是技能私有还是宿主提供的安装作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyScope {
    Skill,
    Host,
}

/// Supported upstream source types for dependency downloads.
/// 依赖下载支持的上游来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencySourceType {
    #[serde(rename = "github_release")]
    GithubRelease,
    #[serde(rename = "url")]
    Url,
    #[serde(rename = "skilllist", alias = "skill_list")]
    SkillList,
    /// Payload shipped inside the skill package itself; installing it never touches the network.
    /// 随技能包自身分发的载荷；安装时从不访问网络。
    #[serde(rename = "bundled")]
    Bundled,
}

/// One dependency detection result returned before install or skip decisions.
/// 在决定安装或跳过之前返回的单个依赖检测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyDetectionStatus {
    Present,
    Missing,
}

/// One normalized dependency install request after platform resolution.
/// 平台解析之后得到的标准化依赖安装请求。
#[derive(Debug, Clone)]
pub struct ResolvedDependencyRequest {
    /// Dependency category being installed.
    /// 当前正在安装的依赖分类。
    pub kind: SkillDependencyKind,
    /// Stable dependency name.
    /// 稳定依赖名称。
    pub name: String,
    /// Install scope used by the current dependency.
    /// 当前依赖使用的安装作用域。
    pub scope: DependencyScope,
    /// Normalized platform key used to resolve the package.
    /// 用于解析安装包的标准平台键。
    pub platform_key: String,
    /// Resolved download URL used to fetch the package.
    /// 用于获取依赖包的已解析下载地址。
    pub download_url: String,
    /// Expected SHA-256 of the downloaded payload, from the release digest or the package declaration.
    /// 下载载荷的期望 SHA-256，来自 Release 摘要或依赖包声明。
    pub expected_sha256: Option<String>,
    /// Optional expected display version.
    /// 可选的预期展示版本号。
    pub version: Option<String>,
    /// Root directory used to install this dependency kind.
    /// 当前依赖分类对应的安装根目录。
    pub install_root: PathBuf,
    /// Archive type used to unpack the downloaded payload.
    /// 解压下载载荷时使用的归档类型。
    pub archive_type: crate::skill::dependencies::DependencyArchiveType,
    /// Export file rules used for extraction and existence detection.
    /// 用于提取文件和存在性检测的导出规则。
    pub exports: Vec<crate::skill::dependencies::DependencyExportSpec>,
}
