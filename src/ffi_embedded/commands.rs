use crate::LuaInvocationContext;
use crate::runtime::embedded::{
    EffectState, EmbeddedCall, EmbeddedError, EmbeddedPluginConfig, EmbeddedResult,
    ModuleDefinition, PluginPoolConfig,
    capabilities::{CapabilityDescriptor, CapabilityOutcome},
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

/// Typed commands for one already initialized runtime; every identity stays bound to that runtime.
/// 一个已初始化运行时的类型化命令；每个身份始终绑定该运行时。
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) enum RuntimeCommand {
    /// Register aggregate plugin budgets.
    /// 注册插件聚合预算。
    PluginRegister {
        /// Exact host-assigned plugin identity.
        /// 精确宿主分配的插件身份。
        plugin_id: String,
        /// Explicit aggregate plugin budgets.
        /// 显式插件聚合预算。
        config: EmbeddedPluginConfig,
    },
    /// Query live aggregate plugin ownership.
    /// 查询实时插件聚合所有权。
    PluginStatus {
        /// Exact host-assigned plugin identity.
        /// 精确宿主分配的插件身份。
        plugin_id: String,
    },
    /// Close one plugin's admission and pools.
    /// 关闭一个插件的入场及池。
    PluginClose {
        /// Exact host-assigned plugin identity.
        /// 精确宿主分配的插件身份。
        plugin_id: String,
    },
    /// Forget only a fully released plugin registration.
    /// 仅遗忘完全释放的插件注册。
    PluginForget {
        /// Exact host-assigned plugin identity.
        /// 精确宿主分配的插件身份。
        plugin_id: String,
    },
    /// Register immutable source and capability authority without executing Lua.
    /// 注册不可变源码及能力权威，不执行 Lua。
    PoolRegister {
        /// Immutable package and module declaration.
        /// 不可变包与模块声明。
        definition: Box<ModuleDefinition>,
        /// Explicit immutable VM pool policy.
        /// 显式不可变 VM 池策略。
        policy: PluginPoolConfig,
        /// Explicit host grants for this binding or discovery request.
        /// 此绑定或发现请求的显式宿主授权。
        permissions: BTreeSet<String>,
        /// Immutable host initialization and configuration revision.
        /// 不可变宿主初始化及配置修订。
        execution_revision: String,
    },
    /// Read actual resource accounting for an exact pool.
    /// 读取精确池的实际资源计数。
    PoolStatus {
        /// Exact immutable pool identity.
        /// 精确不可变池身份。
        pool_id: String,
    },
    /// Close an exact pool and begin actual retirement.
    /// 关闭精确池并开始实际退役。
    PoolClose {
        /// Exact immutable pool identity.
        /// 精确不可变池身份。
        pool_id: String,
    },
    /// Forget only a pool whose ownership has drained.
    /// 仅遗忘所有权已排空的池。
    PoolForget {
        /// Exact immutable pool identity.
        /// 精确不可变池身份。
        pool_id: String,
    },
    /// Revoke a grant on the existing live binding.
    /// 撤销既有实时绑定上的授权。
    PoolRevokePermission {
        /// Exact immutable pool identity.
        /// 精确不可变池身份。
        pool_id: String,
        /// Exact live permission to revoke.
        /// 需要撤销的精确实时权限。
        permission: String,
    },
    /// Admit an asynchronous ordinary invocation.
    /// 接纳异步普通调用。
    CallSubmit {
        /// Typed ordinary call bound to one exact pool.
        /// 绑定一个精确池的类型化普通调用。
        call: Box<EmbeddedCall>,
        /// Original end-to-end execution budget in milliseconds.
        /// 原始端到端执行预算毫秒数。
        timeout_ms: u64,
    },
    /// Reserve a fixed instance and submit initialization.
    /// 预留固定实例并提交初始化。
    SessionOpen {
        /// Exact immutable pool identity.
        /// 精确不可变池身份。
        pool_id: String,
        /// Original end-to-end execution budget in milliseconds.
        /// 原始端到端执行预算毫秒数。
        timeout_ms: u64,
    },
    /// Submit work to an exact fixed session.
    /// 向精确固定会话提交工作。
    SessionSubmit {
        /// Exact fixed-instance session identity.
        /// 精确固定实例会话身份。
        session_id: String,
        /// Declared module export name.
        /// 已声明模块导出名称。
        export: String,
        /// Structured application arguments.
        /// 结构化应用参数。
        arguments: Value,
        /// Trusted host invocation context.
        /// 可信宿主调用上下文。
        context: Box<LuaInvocationContext>,
        /// Original end-to-end execution budget in milliseconds.
        /// 原始端到端执行预算毫秒数。
        timeout_ms: u64,
    },
    /// Read actual session ownership and closure.
    /// 读取实际会话所有权及关闭状态。
    SessionStatus {
        /// Exact fixed-instance session identity.
        /// 精确固定实例会话身份。
        session_id: String,
    },
    /// Close and cancel a fixed session without migrating its state.
    /// 关闭并取消固定会话，不迁移其状态。
    SessionClose {
        /// Exact fixed-instance session identity.
        /// 精确固定实例会话身份。
        session_id: String,
    },
    /// Forget only a closed session with no live ownership.
    /// 仅遗忘没有活动所有权的已关闭会话。
    SessionForget {
        /// Exact fixed-instance session identity.
        /// 精确固定实例会话身份。
        session_id: String,
    },
    /// Read current operation outcome and effect evidence.
    /// 读取当前操作结果及副作用证据。
    OperationStatus {
        /// Exact retained operation identity.
        /// 精确保留操作身份。
        operation_id: String,
    },
    /// Wait for terminal state within an independent observer budget.
    /// 在独立观察者预算内等待终态。
    OperationWait {
        /// Exact retained operation identity.
        /// 精确保留操作身份。
        operation_id: String,
        /// Finite observer wait in milliseconds, independent of execution cancellation.
        /// 有限观察者等待毫秒数，独立于执行取消。
        wait_ms: u64,
    },
    /// Request cooperative cancellation without declaring completion.
    /// 请求协作取消，不宣称完成。
    OperationCancel {
        /// Exact retained operation identity.
        /// 精确保留操作身份。
        operation_id: String,
    },
    /// Forget retained terminal evidence explicitly.
    /// 显式遗忘保留的终态证据。
    OperationForget {
        /// Exact retained operation identity.
        /// 精确保留操作身份。
        operation_id: String,
    },
    /// Publish a queued capability batch atomically.
    /// 原子发布队列能力批次。
    CapabilitiesRegister {
        /// Batch of explicit queued capability declarations.
        /// 显式队列能力声明批次。
        descriptors: Vec<CapabilityDescriptor>,
    },
    /// List declarations authorized by explicit host grants.
    /// 列出显式宿主授权允许的声明。
    CapabilitiesList {
        /// Explicit host grants for this binding or discovery request.
        /// 此绑定或发现请求的显式宿主授权。
        permissions: BTreeSet<String>,
    },
    /// Read actual callback registration lifetime.
    /// 读取实际回调注册寿命。
    CapabilityStatus {
        /// Exact capability registration identity.
        /// 精确能力注册身份。
        registration_id: String,
    },
    /// Retire one exact registration without rerouting existing calls.
    /// 退役一个精确注册，不重定向既有调用。
    CapabilityUnregister {
        /// Exact capability registration identity.
        /// 精确能力注册身份。
        registration_id: String,
    },
    /// Forget only a drained registration.
    /// 仅遗忘已排空注册。
    CapabilityForget {
        /// Exact capability registration identity.
        /// 精确能力注册身份。
        registration_id: String,
    },
    /// Deliver one bounded callback batch with pre-dispatch encoding.
    /// 通过分发前编码投递一个有界回调批次。
    HostRequestsTake {
        /// Maximum host requests in this one bounded batch.
        /// 此单个有界批次的宿主请求数量上限。
        limit: usize,
    },
    /// Read cancellation while retaining actual handler ownership.
    /// 读取取消状态，同时保留实际处理器所有权。
    HostRequestStatus {
        /// Exact host request identity to query or acknowledge.
        /// 用于查询或确认的精确宿主请求身份。
        request_id: String,
    },
    /// Acknowledge actual host completion and preserve effect evidence.
    /// 确认实际宿主完成并保留副作用证据。
    HostRequestComplete {
        /// Exact host request identity to query or acknowledge.
        /// 用于查询或确认的精确宿主请求身份。
        request_id: String,
        /// Actual host result and effect evidence, including late completion.
        /// 实际宿主结果与副作用证据，包含迟到完成。
        outcome: HostCompletion,
    },
}

/// Names are advertised only for the commands wired by the exhaustive runtime dispatcher.
/// 仅为穷尽运行时分发器已接通的命令公布名称。
pub(super) const RUNTIME_COMMAND_NAMES: &[&str] = &[
    "plugin_register",
    "plugin_status",
    "plugin_close",
    "plugin_forget",
    "pool_register",
    "pool_status",
    "pool_close",
    "pool_forget",
    "pool_revoke_permission",
    "call_submit",
    "session_open",
    "session_submit",
    "session_status",
    "session_close",
    "session_forget",
    "operation_status",
    "operation_wait",
    "operation_cancel",
    "operation_forget",
    "capabilities_register",
    "capabilities_list",
    "capability_status",
    "capability_unregister",
    "capability_forget",
    "host_requests_take",
    "host_request_status",
    "host_request_complete",
];

impl RuntimeCommand {
    /// Return whether this command creates new business work and must respect the slot's close fence.
    /// 返回此命令是否创建新业务工作且必须遵守槽关闭屏障。
    pub(super) fn admits_work(&self) -> bool {
        matches!(
            self,
            Self::PluginRegister { .. }
                | Self::PoolRegister { .. }
                | Self::CallSubmit { .. }
                | Self::SessionOpen { .. }
                | Self::SessionSubmit { .. }
                | Self::CapabilitiesRegister { .. }
        )
    }
}

/// Strict host completion shapes match CapabilityOutcome::to_json, preserving successful JSON null.
/// 严格宿主完成形状匹配 CapabilityOutcome::to_json，保留成功 JSON 空值。
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) enum HostCompletion {
    /// Exactly the success shape; ok must be true and value remains required even when null.
    /// 精确成功形状；ok 必须为真，value 即使为空值也必须存在。
    Success {
        /// Explicit success discriminator.
        /// 显式成功判别。
        ok: bool,
        /// Actual application result.
        /// 实际应用结果。
        value: Value,
        /// Actual host effect evidence.
        /// 实际宿主副作用证据。
        effects: EffectState,
    },
    /// Exactly the failure shape; ok must be false.
    /// 精确失败形状；ok 必须为假。
    Failure {
        /// Explicit failure discriminator.
        /// 显式失败判别。
        ok: bool,
        /// Actual structured host error.
        /// 实际结构化宿主错误。
        error: EmbeddedError,
        /// Actual host effect evidence, even if a commit preceded the error.
        /// 实际宿主副作用证据，即使错误前已发生提交。
        effects: EffectState,
    },
}

impl HostCompletion {
    /// Convert only a consistent discriminator and shape into the core's actual result contract.
    /// 仅将一致的判别与形状转换为核心实际结果契约。
    pub(super) fn into_outcome(self) -> EmbeddedResult<CapabilityOutcome> {
        match self {
            Self::Success {
                ok: true,
                value,
                effects,
            } => Ok(CapabilityOutcome {
                result: Ok(value),
                effects,
            }),
            Self::Failure {
                ok: false,
                error,
                effects,
            } => Ok(CapabilityOutcome {
                result: Err(error),
                effects,
            }),
            _ => Err(EmbeddedError::invalid(
                "host completion discriminator conflicts with its result shape",
            )),
        }
    }
}
