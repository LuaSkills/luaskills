use crate::LuaInvocationContext;
use crate::runtime::embedded::{
    EffectState, EmbeddedCall, EmbeddedCapacityConfig, EmbeddedError, EmbeddedPluginConfig,
    EmbeddedPrewarm, EmbeddedResult, ModuleDefinition, OperationReconciliation, PluginPoolConfig,
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
    /// Read retained checkpoint failure without disk I/O or retry.
    /// 读取保留检查点故障，不进行磁盘 I/O 或重试。
    OperationPersistenceFailure {
        /// Exact live operation identity in this runtime.
        /// 此运行时中的精确活动操作身份。
        operation_id: String,
    },
    /// Request one retry of the original immutable checkpoint, never another business execution.
    /// 请求重试原不可变检查点一次，绝不再次执行业务。
    OperationRetryCheckpoint {
        /// Exact live operation identity retaining the failed candidate.
        /// 保留失败候选的精确活动操作身份。
        operation_id: String,
    },
    /// Read actual bounded writer ownership without waiting for disk.
    /// 读取真实有界写入者所有权，不等待磁盘。
    StorageStatus {},
    /// Reopen and validate failed storage; this synchronous disk command belongs on a work lane.
    /// 重新打开并校验失败存储；此同步磁盘命令归入工作通道。
    StorageRecover {},
    /// Rebuild a failed, actually exited writer without retrying original checkpoints or reopening explicit closure.
    /// 重建已失败且实际退出的写入者，不重试原检查点，也不重新打开显式关闭。
    StorageWorkerRecover {},
    /// Read historical evidence by its original namespace, without adopting it as a live operation.
    /// 按原命名空间读取历史证据，不将其接管为活动操作。
    HistoryGet {
        /// Original core runtime namespace, distinct from the containing FFI slot identity.
        /// 原核心运行时命名空间，区别于外层 FFI 槽身份。
        history_runtime_id: String,
        /// Exact original operation identity.
        /// 精确原始操作身份。
        operation_id: String,
    },
    /// Read at most one historical row after an explicit cursor; absence starts enumeration.
    /// 在显式游标后至多读取一条历史；缺失表示开始枚举。
    HistoryNext {
        /// Original history key returned by a prior row, with no inferred current-runtime substitution.
        /// 前一行返回的原始历史键，不推断替换为当前运行时。
        after: Option<HistoryCursor>,
    },
    /// Attach final trusted-host evidence after all original execution owners have stopped; never replay execution.
    /// 全部原执行所有者停止后附加最终可信宿主证据；绝不重放执行。
    HistoryReconcile {
        /// Original historical runtime namespace.
        /// 原始历史运行时命名空间。
        history_runtime_id: String,
        /// Exact original operation identity.
        /// 精确原始操作身份。
        operation_id: String,
        /// Positive original revision; exact retries must retain this predecessor and all resolution fields.
        /// 原始正修订号；精确重试必须保留此前驱及全部对账字段。
        expected_revision: u64,
        /// Complete host-authorized evidence; this API does not authenticate supplied resolver names.
        /// 完整宿主授权证据；此 API 不认证所提供的对账者名称。
        resolution: Box<OperationReconciliation>,
    },
    /// Forget reconciled history only after any matching live runtime operation has been explicitly forgotten.
    /// 仅在显式遗忘任何匹配的活动运行时操作后，遗忘已对账历史。
    HistoryForget {
        /// Original historical runtime namespace.
        /// 原始历史运行时命名空间。
        history_runtime_id: String,
        /// Exact original operation identity.
        /// 精确原始操作身份。
        operation_id: String,
        /// Positive original revision required for atomic compare-and-swap removal.
        /// 原子比较交换删除所需的原始正修订号。
        expected_revision: u64,
    },
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
    /// Register immutable capacity owned by one existing plugin without creating a VM.
    /// 注册单个既有插件拥有的不可变容量，不创建 VM。
    CapacityRegister {
        /// Exact previously registered plugin owner.
        /// 精确先前已注册插件所有者。
        plugin_id: String,
        /// Complete physical and queued-work budgets; no implicit defaults are inserted.
        /// 完整物理及排队工作预算；不插入隐式默认值。
        config: EmbeddedCapacityConfig,
    },
    /// Read actual capacity ownership, including cleanup and unused physical guarantees.
    /// 读取实际容量归属，包含清理及未使用物理保证。
    CapacityStatus {
        /// Exact runtime-issued capacity identity.
        /// 精确运行时签发容量身份。
        capacity_id: String,
    },
    /// Read the current policy token and actual convergence state atomically.
    /// 原子读取当前策略令牌及实际收敛状态。
    CapacityPolicy {
        /// Exact runtime-issued capacity identity.
        /// 精确运行时签发容量身份。
        capacity_id: String,
    },
    /// Replace complete capacity policy only when the supplied opaque predecessor still matches.
    /// 仅在提供的不透明前驱仍匹配时替换完整容量策略。
    CapacityRevise {
        /// Exact runtime-issued capacity identity; never inferred from plugin name.
        /// 精确运行时签发容量身份；绝不从插件名推断。
        capacity_id: String,
        /// Exact string from capacity_policy; clients must not convert it into a numeric value.
        /// 来自 capacity_policy 的精确字符串；客户端不得将其转成数值。
        expected_revision: String,
        /// Complete replacement policy validated atomically by the original scheduler and governor.
        /// 由原调度器及治理器原子校验的完整替换策略。
        config: EmbeddedCapacityConfig,
    },
    /// Close capacity admission and all exact members without claiming actual resource completion.
    /// 关闭容量入场及全部精确成员，不宣称实际资源已完成。
    CapacityClose {
        /// Exact runtime-issued capacity identity.
        /// 精确运行时签发容量身份。
        capacity_id: String,
    },
    /// Forget a closed capacity only after every member and physical owner has drained.
    /// 仅在全部成员及物理所有者排空后遗忘已关闭容量。
    CapacityForget {
        /// Exact runtime-issued capacity identity.
        /// 精确运行时签发容量身份。
        capacity_id: String,
    },
    /// Register immutable source and capability authority without executing Lua.
    /// 注册不可变源码及能力权威，不执行 Lua。
    PoolRegister {
        /// Optional exact capacity owner; omission or null explicitly selects independent placement.
        /// 可选精确容量所有者；省略或空值显式选择独立归属。
        capacity_id: Option<String>,
        /// Immutable package and module declaration.
        /// 不可变包与模块声明。
        definition: Box<ModuleDefinition>,
        /// Explicit immutable VM pool policy.
        /// 显式不可变 VM 池策略。
        policy: PluginPoolConfig,
        /// Explicit host grants for this binding or discovery request.
        /// 此绑定或发现请求的显式宿主授权。
        permissions: BTreeSet<String>,
        /// Exact initialization callback subset; absent or null inherits grants, while an empty set denies all.
        /// 精确初始化回调子集；省略或空值继承授权，空集合则全部拒绝。
        /// Names only narrow existing authority and are frozen before any VM is allocated.
        /// 名称仅收窄既有权威，并在分配任何 VM 前冻结。
        initialization_capabilities: Option<BTreeSet<String>>,
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
    /// Observe confirmed reusable readiness for one exact pool without admitting new work.
    /// 观测单个精确池的已确认可复用就绪状态，不接纳新工作。
    PoolReusableStatus {
        /// Exact immutable reusable pool identity.
        /// 精确不可变可复用池身份。
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
    /// Initialize one additional reusable VM without invoking a business export.
    /// 初始化一个额外可复用 VM，不调用业务导出。
    InstancePrewarm {
        /// Exact pool and trusted initialization/finalization context.
        /// 精确池及可信初始化／关闭上下文。
        request: Box<EmbeddedPrewarm>,
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
    /// Discover retained operation identities in actual publication order with bounded pagination.
    /// 按实际发布顺序，通过有界分页发现保留操作身份。
    OperationList {
        /// Optional original pool filter, valid even after that pool was forgotten.
        /// 可选原始池过滤条件，即使该池已遗忘仍有效。
        pool_id: Option<String>,
        /// Exact retained cursor from the previous page; omitted to restart enumeration.
        /// 上一页的精确保留游标；省略则重新开始枚举。
        after_operation_id: Option<String>,
        /// Positive maximum number of IDs, bounded by the runtime operation retention limit.
        /// 正的身份数量上限，受运行时操作保留上限约束。
        limit: usize,
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

/// Exact historical cursor; its fields come from the original durable record, not a newly opened runtime.
/// 精确历史游标；字段来自原持久记录，不来自新打开的运行时。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct HistoryCursor {
    /// Original core runtime namespace from the returned history record.
    /// 返回历史记录中的原始核心运行时命名空间。
    pub(super) runtime_id: String,
    /// Original operation identity within that namespace.
    /// 该命名空间中的原始操作身份。
    pub(super) operation_id: String,
}

/// Names are advertised only for the commands wired by the exhaustive runtime dispatcher.
/// 仅为穷尽运行时分发器已接通的命令公布名称。
pub(super) const RUNTIME_COMMAND_NAMES: &[&str] = &[
    "operation_persistence_failure",
    "operation_retry_checkpoint",
    "storage_status",
    "storage_recover",
    "storage_worker_recover",
    "history_get",
    "history_next",
    "history_reconcile",
    "history_forget",
    "plugin_register",
    "plugin_status",
    "plugin_close",
    "plugin_forget",
    "capacity_register",
    "capacity_status",
    "capacity_policy",
    "capacity_revise",
    "capacity_close",
    "capacity_forget",
    "pool_register",
    "pool_status",
    "pool_reusable_status",
    "pool_close",
    "pool_forget",
    "pool_revoke_permission",
    "call_submit",
    "instance_prewarm",
    "session_open",
    "session_submit",
    "session_status",
    "session_close",
    "session_forget",
    "operation_status",
    "operation_list",
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
                | Self::CapacityRegister { .. }
                | Self::CapacityRevise { .. }
                | Self::PoolRegister { .. }
                | Self::CallSubmit { .. }
                | Self::InstancePrewarm { .. }
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
