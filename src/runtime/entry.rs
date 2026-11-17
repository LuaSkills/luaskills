use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One parameter descriptor exposed by a LuaSkills runtime entry.
/// LuaSkills 运行时入口对外暴露的单个参数描述。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEntryParameterDescriptor {
    /// Stable local parameter name.
    /// 稳定的局部参数名称。
    pub name: String,
    /// Runtime parameter type string.
    /// 运行时参数类型字符串。
    pub param_type: String,
    /// Human-readable parameter description.
    /// 人类可读的参数说明。
    pub description: String,
    /// Whether the parameter is required.
    /// 当前参数是否必填。
    pub required: bool,
}

/// Generic runtime entry descriptor that stays independent from MCP tool/resource concepts.
/// 独立于 MCP tool/resource 概念的通用运行时入口描述对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEntryDescriptor {
    /// Canonical runtime entry identifier in `skill_id-entry_name[-N]` format.
    /// 采用 `skill_id-entry_name[-N]` 形式的 canonical 运行时入口标识。
    pub canonical_name: String,
    /// Stable skill namespace that owns the entry.
    /// 拥有该入口的稳定 skill 命名空间。
    pub skill_id: String,
    /// Stable local entry name declared by the skill.
    /// 由 skill 声明的稳定局部入口名称。
    pub local_name: String,
    /// Named skill root that currently owns the effective skill instance.
    /// 当前生效技能实例所属的命名技能根。
    pub root_name: String,
    /// Physical skill directory of the current effective skill instance.
    /// 当前生效技能实例对应的物理技能目录。
    pub skill_dir: String,
    /// Human-readable entry description.
    /// 人类可读的入口描述。
    pub description: String,
    /// Parameter descriptors of the current entry.
    /// 当前入口的参数描述列表。
    pub parameters: Vec<RuntimeEntryParameterDescriptor>,
    /// Final AI-facing input schema resolved from entry metadata.
    /// 从入口元数据解析得到的最终面向 AI 输入 schema。
    pub input_schema: Value,
}

impl RuntimeEntryDescriptor {
    /// Return the portable public tool name derived from this entry's canonical identity.
    /// 返回根据此入口的规范标识生成的通用公开工具名。
    pub fn public_tool_name(&self) -> String {
        crate::public_tool_name(&self.canonical_name)
    }
}
