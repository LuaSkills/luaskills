use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Registration mode declared by the owning package.
/// 由所属包声明的注册方式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRegistrationMode {
    /// Publish individual tool definitions.
    /// 发布独立的工具定义。
    #[default]
    Explicit,
    /// Publish a guide and resolve tools through a shared dispatcher.
    /// 发布指南，并经共享调度器解析工具。
    Implicit,
}

/// Localized package or guide text, indexed separately from stable identifiers.
/// 包或指南的本地化文案，与稳定标识分别索引。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalizedSkillIntroduction {
    /// Display name.
    /// 显示名称。
    pub title: String,
    /// Compact package summary or guide discovery description.
    /// 简短的包概述或指南发现说明。
    pub description: String,
}

/// A named Agent Skill guide shipped with a LuaSkills package.
/// 随 LuaSkills 包分发的具名提示词技能指南。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundledPromptSkill {
    /// Stable local prompt skill identifier.
    /// 稳定的包内提示词技能标识。
    pub name: String,
    /// Package-relative path to the primary Agent Skill Markdown file.
    /// 主提示词技能 Markdown 文件的包内相对路径。
    pub file: String,
    /// Localized guide introductions keyed by language tag.
    /// 按语言标签索引的指南介绍。
    pub locales: BTreeMap<String, LocalizedSkillIntroduction>,
}

/// Package-owned discovery and registration declaration.
/// 由包定义的发现与注册声明。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRegistration {
    /// Default registration mode for the package's entries.
    /// 包内入口的默认注册方式。
    pub mode: SkillRegistrationMode,
    /// Required language tag used when a translation is missing.
    /// 缺少译文时使用的必填语言标签。
    pub default_locale: String,
    /// Package introductions keyed by language tag.
    /// 按语言标签索引的包介绍。
    pub locales: BTreeMap<String, LocalizedSkillIntroduction>,
    /// Guides exported from this package. Implicit packages require at least one.
    /// 此包导出的指南；隐式包至少需要一份。
    #[serde(default)]
    pub prompt_skills: Vec<BundledPromptSkill>,
}

/// Registration metadata for one loaded package instance.
/// 单个已加载包实例的注册元数据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSkillRegistration {
    /// Stable package identifier.
    /// 稳定的包标识。
    pub skill_id: String,
    /// Name of the runtime root that owns the package.
    /// 所属运行时根目录名称。
    pub root_name: String,
    /// Installed package path used to locate declared prompt skills.
    /// 用于定位已声明提示词技能的安装路径。
    pub skill_dir: PathBuf,
    /// Validated package declaration.
    /// 已验证的包声明。
    pub declaration: SkillRegistration,
}

impl SkillRegistration {
    /// Returns whether this declaration supplies a guide for implicit dispatch.
    /// 返回此声明是否为隐式调用提供随包指南。
    pub fn supports_implicit(&self) -> bool {
        !self.prompt_skills.is_empty()
    }

    /// Validate localized introductions and guide paths in the given package directory.
    /// 校验指定包目录中的本地化介绍和指南路径。
    pub(crate) fn validate(&self, skill_dir: &Path) -> Result<(), String> {
        validate_introductions(&self.locales, &self.default_locale)?;
        if self.mode == SkillRegistrationMode::Implicit && !self.supports_implicit() {
            return Err("implicit tool registration requires a prompt skill".into());
        }
        // Names must be unique because they are published together in one package namespace.
        // 指南名称在同一包命名空间内发布，因此必须唯一。
        let mut names = std::collections::BTreeSet::new();
        for guide in &self.prompt_skills {
            super::manifest::validate_luaskills_identifier(&guide.name, "prompt skill name")?;
            if guide.name.trim() != guide.name || !names.insert(&guide.name) {
                return Err(format!(
                    "invalid or duplicate prompt skill name: {}",
                    guide.name
                ));
            }
            validate_introductions(&guide.locales, &self.default_locale)?;
            guide.path_in(skill_dir)?;
        }
        Ok(())
    }
}

impl BundledPromptSkill {
    /// Resolve the declared Markdown file under the package's skills directory.
    /// 在包的 skills 目录下解析声明的 Markdown 文件。
    pub fn path_in(&self, skill_dir: &Path) -> Result<PathBuf, String> {
        super::manifest::validate_skill_relative_path(&self.file, "skills", "prompt skill file")?;
        // Require an ordinary Skill document and reject links leaving the registered package.
        // 要求标准 Skill 文档，并拒绝指向已注册包外部的链接。
        let path = skill_dir.join(&self.file);
        if path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
            return Err(format!(
                "prompt skill file must be named SKILL.md: {}",
                self.file
            ));
        }
        // Canonicalize both sides before comparing directory ancestry.
        // 比较目录归属前规范化两侧路径。
        let root = skill_dir
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve prompt skill file {}: {error}", self.file))?;
        if !canonical_path.starts_with(root.join("skills")) || !canonical_path.is_file() {
            return Err(format!(
                "prompt skill file is outside its package: {}",
                self.file
            ));
        }
        Ok(canonical_path)
    }
}

/// Validate nonempty localized introductions and the required default translation.
/// 校验本地化介绍非空且包含必填的默认语言译文。
fn validate_introductions(
    locales: &BTreeMap<String, LocalizedSkillIntroduction>,
    default_locale: &str,
) -> Result<(), String> {
    if !locales.contains_key(default_locale) {
        return Err(format!(
            "missing introduction for default locale {default_locale}"
        ));
    }
    for (locale, introduction) in locales {
        // Language tags are data from packages, independent of the host's UI locale inventory.
        // 语言标签由包提供，独立于宿主的界面语言清单。
        let mut parts = locale.split('-');
        if !parts.next().is_some_and(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphabetic())
        }) || !parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) || introduction.title.trim().is_empty()
            || introduction.description.trim().is_empty()
        {
            return Err(format!("invalid localized introduction for {locale}"));
        }
    }
    Ok(())
}

/// Return the shared MCP and model identifier for a validated canonical entry.
/// 根据已验证的规范入口生成 MCP 与模型共用的工具标识。
pub fn public_tool_name(canonical_name: &str) -> String {
    // A bounded hash suffix preserves the identity of names with the same long prefix.
    // 固定长度的哈希后缀保留具有相同长前缀的入口身份。
    let name = canonical_name.replace('-', "_");
    if name.len() <= 64 {
        return name;
    }
    // Canonical entry names only use ASCII, so this byte offset always starts a character.
    // 规范入口名只使用 ASCII，因此字节偏移总在字符边界上。
    let digest = format!("{:x}", Sha256::digest(name.as_bytes()));
    format!("{}_{}", &name[..47], &digest[..16])
}
