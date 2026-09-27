use super::{CallControl, EmbeddedError, EmbeddedResult, JsonContract};
use crate::LuaInvocationContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Immutable source and trusted path declaration supplied during module activation.
/// 模块激活时提供的不可变源码与可信路径声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct ModuleDefinition {
    /// Host-assigned stable plugin identity.
    /// 宿主分配的稳定插件身份。
    pub plugin_id: String,
    /// Host-assigned immutable code and dependency generation.
    /// 宿主分配的不可变代码与依赖代次。
    pub generation: String,
    /// Absolute plugin root inside the configured System trust root.
    /// 位于已配置 System 信任根内的绝对插件根目录。
    pub package_root: String,
    /// Package-relative dependency manifest, validated by the existing package loader.
    /// 由既有包加载器校验的包相对依赖清单。
    pub dependencies_file: String,
    /// Explicitly authorized workspace root, absent for package-only execution.
    /// 显式授权的工作区根目录；仅在包内执行时省略。
    pub workspace_root: Option<String>,
    /// Logical working directory; absent selects the existing package-root rule.
    /// 逻辑工作目录；省略时使用既有包根目录规则。
    pub cwd: Option<String>,
    /// Trusted mount metadata; must be a JSON object.
    /// 可信挂载元数据，必须为 JSON 对象。
    pub mounts: Value,
    /// Host-authenticated security partition used for instance matching.
    /// 用于实例匹配且由宿主认证的安全分区。
    pub security_partition: String,
    /// Source evaluated once; it must return a table of declared functions.
    /// 仅求值一次的源码；必须返回已声明函数的表。
    pub source: String,
    /// Exact public exports and value contracts validated before invocation.
    /// 调用前校验的精确公开导出及值契约。
    pub exports: Vec<ModuleExport>,
}

/// One named export with explicit input and output schemas.
/// 具有显式输入及输出 Schema 的单个具名导出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct ModuleExport {
    /// Exact Lua table key captured when the module is loaded.
    /// 模块加载时捕获的精确 Lua 表键。
    pub name: String,
    /// Offline Draft 2020-12 schema for structured invocation arguments.
    /// 结构化调用参数的离线 Draft 2020-12 Schema。
    pub input_schema: Value,
    /// Offline Draft 2020-12 schema for structured return values.
    /// 结构化返回值的离线 Draft 2020-12 Schema。
    pub output_schema: Value,
}

impl ModuleExport {
    /// Compile both value contracts; return validation errors before executing source.
    /// 编译两种值契约；在执行源码前返回校验错误。
    pub fn compile(&self) -> EmbeddedResult<(JsonContract, JsonContract)> {
        Ok((
            JsonContract::compile(&self.input_schema)?,
            JsonContract::compile(&self.output_schema)?,
        ))
    }
}

impl ModuleDefinition {
    /// Check identities and export declarations before allocating a VM.
    /// 在分配 VM 前检查身份与导出声明。
    /// Return a structured argument error; filesystem checks run in the package loader.
    /// 返回结构化参数错误；文件系统检查在包加载器中执行。
    pub fn validate(&self) -> EmbeddedResult<()> {
        if self.plugin_id.trim().is_empty()
            || self.generation.trim().is_empty()
            || self.security_partition.trim().is_empty()
        {
            return Err(EmbeddedError::invalid(
                "plugin, generation, and security partition must be nonempty",
            ));
        }
        if self.source.is_empty() || self.exports.is_empty() {
            return Err(EmbeddedError::invalid(
                "module source and export list must be nonempty",
            ));
        }
        if !self.mounts.is_object() {
            return Err(EmbeddedError::invalid(
                "module mounts must be a JSON object",
            ));
        }
        // Reject duplicate declarations instead of silently shadowing functions.
        // 拒绝重复声明，避免静默遮蔽函数。
        let mut names = BTreeSet::new();
        for export in &self.exports {
            if export.name.trim().is_empty()
                || export.name.contains('\0')
                || !names.insert(&export.name)
            {
                return Err(EmbeddedError::invalid(
                    "module exports must be distinct nonempty names without NUL",
                ));
            }
        }
        Ok(())
    }
}

/// Host-owned invocation values; Lua receives no writable identity authority.
/// 宿主拥有的调用值；Lua 不会获得可写的身份权威。
pub struct ModuleInvocation<'a> {
    /// Host-generated operation identity, separate from plugin-controlled arguments.
    /// 宿主生成的操作身份，独立于插件可控参数。
    pub operation_id: &'a str,
    /// Trusted session identity for this invocation, absent for ordinary calls.
    /// 此次调用的可信会话身份，普通调用省略。
    pub session_id: Option<&'a str>,
    /// Exact export name selected from the validated module declaration.
    /// 从已校验模块声明中选定的精确导出名称。
    pub export: &'a str,
    /// Structured argument copied into Lua for this invocation only.
    /// 仅为本次调用复制到 Lua 中的结构化参数。
    pub arguments: &'a Value,
    /// Existing trusted host context consumed by managed APIs.
    /// 受管 API 使用的既有可信宿主上下文。
    pub context: &'a LuaInvocationContext,
    /// Shared cancellation and original deadline, also used during host calls.
    /// 在宿主调用期间同样使用的共享取消控制与原始截止时间。
    pub control: Arc<CallControl>,
}
