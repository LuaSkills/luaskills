//! Immutable operation authority captured before scheduling, including calls with no host effects.
//! 调度前捕获的不可变操作权威，包含没有宿主副作用的调用。

use super::*;
use crate::runtime::embedded::capabilities::CapabilityCaller;

#[cfg(test)]
mod tests;

/// Explicit operation origin; unbound low-level work is never inferred to belong to a current plugin.
/// 明确的操作来源；未绑定的低层工作绝不被推断归属于当前插件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum OperationContext {
    /// The low-level host admitted this operation without a module binding.
    /// 低层宿主接纳此操作时没有模块绑定。
    Unbound,
    /// The formal scheduler froze this module context before publishing the operation.
    /// 正式调度器在发布操作前冻结了此模块上下文。
    Module(Box<ModuleOperationContext>),
}

/// Original module identity used for execution and historical reconciliation, never a new execution authorization.
/// 用于执行及历史对账的原始模块身份，绝非新的执行授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct ModuleOperationContext {
    /// Exact retained pool identity, not a lookup of the plugin's newest pool.
    /// 精确保留池身份，不查询插件最新的池。
    pub pool_id: String,
    /// Original trusted caller shared by initialization and this operation's host callbacks.
    /// 初始化及此操作宿主回调共同使用的原始可信调用方。
    pub caller: CapabilityCaller,
    /// Exact capability membership snapshot frozen when the pool was registered.
    /// 注册池时冻结的精确能力成员快照。
    pub capability_revision: String,
    /// Requested declared export; absent only for a fixed-session opening operation.
    /// 请求的已声明导出；仅固定会话开启操作省略。
    pub export: Option<String>,
}

impl OperationContext {
    /// Validate context against `runtime_id` and `operation_id`; reject contradictory or incomplete authority.
    /// 对照 `runtime_id` 与 `operation_id` 校验上下文；拒绝矛盾或不完整权威。
    /// Return success for explicitly unbound operations without guessing a module or session.
    /// 对明确未绑定操作返回成功，不猜测模块或会话。
    pub(in crate::runtime::embedded) fn validate(
        &self,
        runtime_id: &str,
        operation_id: &str,
    ) -> EmbeddedResult<()> {
        // Only the module variant carries authority that can be checked against execution identity.
        // 仅模块分支携带可对照执行身份检查的权威。
        let Self::Module(context) = self else {
            return Ok(());
        };
        context.caller.validate(runtime_id)?;
        if context.caller.operation_id != operation_id
            || [&context.pool_id, &context.capability_revision]
                .iter()
                .any(|value| value.trim().is_empty() || value.contains('\0'))
            || context
                .export
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.contains('\0'))
            || (context.export.is_none() && context.caller.session_id.is_none())
        {
            return Err(EmbeddedError::invalid(
                "operation module context is invalid",
            ));
        }
        Ok(())
    }

    /// Borrow the exact admitted module caller, or report the explicit low-level unbound variant.
    /// 借用精确入场模块调用方，或报告明确的低层未绑定分支。
    pub(in crate::runtime::embedded) fn caller(&self) -> Option<&CapabilityCaller> {
        match self {
            Self::Unbound => None,
            Self::Module(context) => Some(&context.caller),
        }
    }
}
