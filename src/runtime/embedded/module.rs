use super::{CallControl, EmbeddedError, EmbeddedResult, JsonContract};
use crate::LuaInvocationContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Immutable source and trusted path declaration supplied during module activation.
/// 模块激活时提供的不可变源码与可信路径声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct ModuleDefinition {
    /// Optional host-owned closing declaration for supported scheduled lifecycles.
    /// 可选的宿主所有关闭声明，用于受支持的调度生命周期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalizer: Option<ModuleFinalizer>,
    /// Host-assigned stable plugin identity.
    /// 宿主分配的稳定插件身份。
    pub plugin_id: String,
    /// Host-assigned immutable code and dependency generation.
    /// 宿主分配的不可变代码与依赖代次。
    pub generation: String,
    /// Exact absolute plugin root authorized by the trusted host, independent of legacy System roots.
    /// 可信宿主授权的精确绝对插件根目录，独立于旧 System 根。
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

/// Immutable closing export, arguments and independent finite execution budget.
/// 不可变关闭导出、参数及独立有限执行预算。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct ModuleFinalizer {
    /// Exact name already present in the module's declared exports.
    /// 已存在于模块声明导出中的精确名称。
    pub export: String,
    /// Structured closing input validated at registration and again before execution.
    /// 在注册时及执行前再次校验的结构化关闭输入。
    pub arguments: Value,
    /// Finite milliseconds starting at closing execution admission, independent from business cancellation.
    /// 从关闭执行入场开始计时的有限毫秒数，独立于业务取消。
    pub timeout_ms: u64,
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
        self.validate_declarations()?;
        if let Some(finalizer) = &self.finalizer {
            // Public declaration validation preserves its original closing-contract checks.
            // 公开声明校验保留原有关闭契约检查。
            let export = self
                .exports
                .iter()
                .find(|export| export.name == finalizer.export)
                .ok_or_else(|| EmbeddedError::invalid("closing export must be declared"))?;
            validate_finalizer_arguments(finalizer, &export.compile()?.0)?;
        }
        Ok(())
    }

    /// Check this definition's immutable names, source, mount values and closing declaration without compiling contracts.
    /// 检查此定义的不可变名称、源码、挂载值及关闭声明，不编译契约。
    /// Return a declaration error; filesystem and live authority remain separate admission checks.
    /// 返回声明错误；文件系统及实时权威保持为独立入场检查。
    fn validate_declarations(&self) -> EmbeddedResult<()> {
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
        // Mount primitives enter Lua through the shared readonly context projector.
        // 挂载基础值通过共享只读上下文投影器进入 Lua。
        crate::runtime::engine::LuaEngine::validate_embedded_json_value(&self.mounts, "mounts")?;
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
        if let Some(finalizer) = &self.finalizer {
            // Preserve the original declared-export and timeout checks before schema compilation.
            // 在 Schema 编译前保留原有已声明导出及超时检查。
            if !self
                .exports
                .iter()
                .any(|export| export.name == finalizer.export)
            {
                return Err(EmbeddedError::invalid("closing export must be declared"));
            }
            CallControl::new(std::time::Duration::from_millis(finalizer.timeout_ms))?;
        }
        Ok(())
    }
}

/// Immutable original declaration and the contracts compiled exactly once before pool publication.
/// 池发布前恰好编译一次的契约与不可变原声明。
/// Private fields prevent a caller from pairing compiled validators with a different declaration.
/// 私有字段防止调用方将已编译校验器与不同声明配对。
pub(crate) struct PreparedModuleDefinition {
    /// Exact source, paths, exports and finalizer supplying these compiled contracts.
    /// 提供这些已编译契约的精确源码、路径、导出及关闭器。
    definition: ModuleDefinition,
    /// Original export names mapped to shared immutable input and output validators.
    /// 原导出名称映射到共享不可变输入及输出校验器。
    contracts: BTreeMap<String, (JsonContract, JsonContract)>,
}

impl PreparedModuleDefinition {
    /// Prepare definition without allocating a VM, reading paths or executing source.
    /// 准备 definition，不分配 VM、不读取路径、不执行源码。
    /// Return one shared owner only after every contract and the exact finalizer input are valid.
    /// 仅全部契约及精确关闭器输入有效后，返回单个共享所有者。
    pub(crate) fn new(definition: ModuleDefinition) -> EmbeddedResult<Arc<Self>> {
        definition.validate_declarations()?;
        // This single compilation serves scheduled admission and every fresh VM from this pool.
        // 此单次编译供调度入场及此池每个新 VM 使用。
        let contracts = definition
            .exports
            .iter()
            .map(|export| export.compile().map(|pair| (export.name.clone(), pair)))
            .collect::<EmbeddedResult<BTreeMap<_, _>>>()?;
        if let Some(finalizer) = &definition.finalizer {
            // Match the exact already-compiled export, without compiling closing schemas again.
            // 匹配精确已编译导出，不再次编译关闭 Schema。
            let (input, _) = contracts
                .get(&finalizer.export)
                .ok_or_else(|| EmbeddedError::invalid("closing export must be declared"))?;
            validate_finalizer_arguments(finalizer, input)?;
        }
        Ok(Arc::new(Self {
            definition,
            contracts,
        }))
    }

    /// Borrow the original declaration; callers cannot mutate it or replace its authority.
    /// 借用原声明；调用方不能修改它或替换其权威。
    pub(crate) fn definition(&self) -> &ModuleDefinition {
        &self.definition
    }

    /// Borrow original compiled pairs; cloning a JsonContract shares its existing validator Arc.
    /// 借用原已编译契约对；克隆 JsonContract 共享其既有校验器 Arc。
    pub(crate) fn contracts(&self) -> &BTreeMap<String, (JsonContract, JsonContract)> {
        &self.contracts
    }
}

/// Validate finalizer's structured arguments and Lua representation with its exact input after declaration checks.
/// 声明检查后使用精确 input 校验 finalizer 的结构化参数及 Lua 表示。
/// Return the existing argument error without allocating a VM or executing shutdown.
/// 返回既有参数错误，不分配 VM 或执行关闭。
fn validate_finalizer_arguments(
    finalizer: &ModuleFinalizer,
    input: &JsonContract,
) -> EmbeddedResult<()> {
    input.validate(&finalizer.arguments)?;
    // Closing arguments must be representable before activation, not first at teardown.
    // 关闭参数必须在激活前可表示，不能等到清理时才发现。
    crate::runtime::engine::LuaEngine::validate_embedded_json_value(
        &finalizer.arguments,
        "finalizer/arguments",
    )
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
