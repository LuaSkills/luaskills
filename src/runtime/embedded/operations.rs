use super::HostEffectRecord;
use super::effects::{EffectLedger, merge_effects};
use super::value_size::json_size;
use super::{
    CallControl, EmbeddedError, EmbeddedErrorCode, EmbeddedResult, EmbeddedRuntimeConfig,
    OperationJournal,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

mod admission;
mod finalization;
use finalization::FinalizationOwnership;
pub use finalization::{OperationFinalization, OperationOutcome};
mod context;
pub use context::{ModuleOperationContext, OperationContext};
mod checkpoint;
use checkpoint::{HistoryBackend, PendingCheckpoint};

#[cfg(test)]
mod persistence_tests;

/// Execution phase; cancellation intent is reported separately from actual termination.
/// 执行阶段；取消意图与实际终止分开报告。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum OperationPhase {
    /// Accepted and waiting for resource admission.
    /// 已接纳且正在等待资源入场。
    Queued,
    /// Creating or loading the selected VM instance.
    /// 正在创建或加载选定的 VM 实例。
    Initializing,
    /// Executing the requested exported function.
    /// 正在执行请求的导出函数。
    Running,
    /// Still owns its VM while awaiting a host capability result.
    /// 等待宿主能力结果期间仍拥有其 VM。
    WaitingForHost,
    /// Execution returned but request-owned cleanup is not yet complete.
    /// 执行已返回，但请求所属清理尚未完成。
    Cleaning,
    /// Execution and required cleanup completed successfully.
    /// 执行及必要清理已成功完成。
    Succeeded,
    /// Execution or cleanup failed and the actual result is available.
    /// 执行或清理失败，且实际结果已经可用。
    Failed,
    /// Cooperative cancellation completed; this does not imply effect rollback.
    /// 协作取消已完成；这不表示副作用已回滚。
    Cancelled,
}

impl OperationPhase {
    /// Return whether this phase represents a completed operation record.
    /// 返回此阶段是否表示已完成的操作记录。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Host-reported effect outcome, independent from execution success or cancellation.
/// 宿主报告的副作用结果，独立于执行成功或取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum EffectState {
    /// No business execution has started.
    /// 尚未开始业务执行。
    NotStarted,
    /// The declared operation has no externally visible mutations.
    /// 声明的操作不包含外部可见变更。
    NotApplicable,
    /// A trusted host transaction explicitly confirmed its commit.
    /// 可信宿主事务明确确认其提交。
    Committed,
    /// A trusted host transaction explicitly confirmed its rollback.
    /// 可信宿主事务明确确认其回滚。
    RolledBack,
    /// Effects may have occurred; retries require host-specific reconciliation.
    /// 副作用可能已发生；重试需要宿主特定的对账。
    Unknown,
}

/// Bounded operation snapshot suitable for direct serialization to every SDK.
/// 适合直接序列化给各 SDK 的有界操作快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct OperationSnapshot {
    /// Closing intent and separate stage results, absent when no explicit finalization was prepared.
    /// 关闭意图及独立阶段结果；未准备显式关闭时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalization: Option<OperationFinalization>,
    /// Admission-time module authority, or an explicit unbound low-level origin; never reconstructed from effects.
    /// 入场时模块权威，或明确未绑定的低层来源；绝不从副作用重建。
    pub context: OperationContext,
    /// Exact host callback evidence retained even after Lua failure, cancellation or output rejection.
    /// 即使 Lua 失败、取消或输出被拒绝也保留的精确宿主回调证据。
    pub host_effects: Vec<HostEffectRecord>,
    /// Opaque identifier, never a JavaScript floating-point integer.
    /// 不透明标识符，绝不使用 JavaScript 浮点整数。
    pub operation_id: String,
    /// Current execution phase, including still-running cancelled requests.
    /// 当前执行阶段，包含已请求取消但仍在运行的请求。
    pub phase: OperationPhase,
    /// Whether cooperative cancellation has been requested.
    /// 是否已请求协作取消。
    pub cancellation_requested: bool,
    /// Explicit effect status; successful execution does not automatically imply commit.
    /// 显式副作用状态；执行成功不自动表示提交。
    pub effects: EffectState,
    /// Successful value; absent before completion or on failure, distinct from JSON null.
    /// 成功值；完成前或失败时省略，与 JSON 空值不同。
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<Value>,
    /// Structured terminal error; absent while execution is still in progress.
    /// 结构化终态错误；执行仍在进行时省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<EmbeddedError>,
}

/// Preserve an explicitly present JSON null as a successful value; missing fields use the serde default.
/// 将显式出现的 JSON 空值保留为成功值；缺失字段使用 serde 默认值。
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// Apply the single legal nonterminal transition table to owned `snapshot`, preserving effect uncertainty.
/// 将唯一合法非终态转换表应用于拥有所有权的 `snapshot`，保留副作用不确定性。
/// Return an error before mutation when requested `phase` would bypass lifecycle ownership.
/// 请求的 `phase` 绕过生命周期所有权时，在变更前返回错误。
fn advance_snapshot(snapshot: &mut OperationSnapshot, phase: OperationPhase) -> EmbeddedResult<()> {
    if !matches!(
        (snapshot.phase, phase),
        (
            OperationPhase::Queued,
            OperationPhase::Initializing | OperationPhase::Running | OperationPhase::Cleaning
        ) | (
            OperationPhase::Initializing,
            OperationPhase::Running | OperationPhase::WaitingForHost | OperationPhase::Cleaning
        ) | (
            OperationPhase::Running,
            OperationPhase::WaitingForHost | OperationPhase::Cleaning
        ) | (
            OperationPhase::WaitingForHost,
            OperationPhase::Running | OperationPhase::Initializing | OperationPhase::Cleaning
        )
    ) {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::Busy,
            "operation lifecycle transition is invalid",
        ));
    }
    snapshot.phase = phase;
    if matches!(
        phase,
        OperationPhase::Initializing | OperationPhase::Running | OperationPhase::WaitingForHost
    ) {
        snapshot.effects = EffectState::Unknown;
    }
    Ok(())
}

/// State shared by one read/cancel handle and its sole execution owner.
/// 单个读取与取消句柄和其唯一执行所有者共享的状态。
pub(super) struct Operation {
    /// Serializes phase and host-intent checkpoints separately from client observation and cancellation.
    /// 将阶段及宿主意图检查点与客户端观测、取消分开串行化。
    transition: Mutex<Option<PendingCheckpoint>>,
    /// Explicit durable checkpoints for this operation; absence preserves the existing memory-only API.
    /// 此操作的显式持久检查点；省略时保留既有仅内存 API。
    history: Option<OperationHistory>,
    /// Immutable identity remains available even if mutable lifecycle observations fail.
    /// 即使可变生命周期观测失败，不可变身份仍可取得。
    id: String,
    /// Bounded evidence shared with the original execution control.
    /// 与原始执行控制共享的有界证据。
    effects: Arc<EffectLedger>,
    /// Stable original deadline and cooperative cancellation flag.
    /// 稳定的原始截止时间与协作取消标记。
    control: Arc<CallControl>,
    /// Serialized lifecycle and terminal-value mutation.
    /// 串行化生命周期与终态值的变更。
    snapshot: Mutex<OperationSnapshot>,
    /// Completion notification; query state remains authoritative.
    /// 完成通知；查询状态仍为权威。
    changed: Condvar,
    /// Result-size budget copied from the runtime's single validated configuration.
    /// 从运行时唯一已校验配置复制的结果大小预算。
    max_value_bytes: usize,
}

/// One operation's exact disk revision, serialized independently from public snapshot observations.
/// 单个操作的精确磁盘修订号，独立于公开快照观测串行化。
struct OperationHistory {
    /// Host-owned storage, shared across operation namespaces without becoming a live registry.
    /// 宿主拥有的存储，可跨操作命名空间共享，但不成为活动注册表。
    backend: HistoryBackend,
    /// Original namespace supplied by the registry, never taken from application arguments.
    /// 注册表提供的原始命名空间，绝不从应用参数取得。
    runtime_id: String,
    /// Last acknowledged disk revision; an uncertain write never advances this receipt.
    /// 最后确认的磁盘修订号；不确定写入绝不推进此回执。
    revision: Mutex<Option<u64>>,
}

impl OperationHistory {
    /// Persist exact `snapshot` before its corresponding execution or terminal publication proceeds.
    /// 在对应执行或终态发布继续之前，持久化精确 `snapshot`。
    /// Return errors without advancing the acknowledged revision; the journal blocks uncertain retries.
    /// 返回错误而不推进已确认修订号；日志阻止不确定重试。
    fn checkpoint(&self, snapshot: &OperationSnapshot) -> EmbeddedResult<()> {
        // This per-operation gate never blocks queries or cancellation on the public operation lock.
        // 此逐操作门禁绝不在公开操作锁上阻塞查询或取消。
        let mut revision = self.revision.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation checkpoint lock is poisoned",
            )
        })?;
        // The first acknowledged checkpoint creates history; later ones compare the exact prior receipt.
        // 首个确认检查点创建历史；后续检查点比较精确的前次回执。
        *revision = Some(
            self.backend
                .checkpoint(&self.runtime_id, *revision, snapshot)?,
        );
        Ok(())
    }
}

impl Operation {
    /// Commit `snapshot` to explicitly configured storage, without holding this operation's state lock.
    /// 将 `snapshot` 提交到显式配置的存储，不持有此操作的状态锁。
    fn checkpoint(&self, snapshot: &OperationSnapshot) -> EmbeddedResult<()> {
        match &self.history {
            Some(history) => history.checkpoint(snapshot),
            None => Ok(()),
        }
    }
    /// Project live cancellation and host waiting onto owned `snapshot` without changing execution authority.
    /// 将实时取消与宿主等待投影到拥有所有权的 `snapshot`，不改变执行权威。
    /// Preserve terminal phases; only in-progress host records refine initializing or running observations.
    /// 保留终态；仅进行中的宿主记录细化初始化或运行观测。
    fn project(&self, mut snapshot: OperationSnapshot) -> EmbeddedResult<OperationSnapshot> {
        snapshot.cancellation_requested = self.control.is_cancelled();
        snapshot.host_effects = self.effects.snapshot()?;
        snapshot.effects = merge_effects(snapshot.effects, &snapshot.host_effects);
        if matches!(
            snapshot.phase,
            OperationPhase::Initializing | OperationPhase::Running
        ) && snapshot
            .host_effects
            .iter()
            .any(|effect| effect.phase != super::HostEffectPhase::Completed)
        {
            snapshot.phase = OperationPhase::WaitingForHost;
        }
        Ok(snapshot)
    }

    /// Acquire current state, rejecting poisoning rather than returning invented status.
    /// 获取当前状态；中毒时报错，不返回编造状态。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, OperationSnapshot>> {
        self.snapshot.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation record lock is poisoned",
            )
        })
    }
}

/// Cloneable client view that can query or request cancellation, but cannot complete work.
/// 可克隆的客户端视图，可查询或请求取消，但不能完成工作。
#[derive(Clone)]
pub struct OperationHandle {
    /// Shared state whose execution authority lives only in the owner token.
    /// 执行权威仅位于所有者令牌中的共享状态。
    operation: Arc<Operation>,
}

impl OperationHandle {
    /// Borrow the exact immutable identity without querying execution or effect state.
    /// 借用精确不可变身份，不查询执行或副作用状态。
    pub fn id(&self) -> &str {
        &self.operation.id
    }

    /// Return a fresh snapshot with the actual cooperative cancellation request flag.
    /// 返回包含真实协作取消请求标记的最新快照。
    pub fn snapshot(&self) -> EmbeddedResult<OperationSnapshot> {
        // Clone phase ownership before reading independent live host evidence.
        // 在读取独立实时宿主证据前克隆阶段所有权。
        let snapshot = self.operation.lock()?.clone();
        self.operation.project(snapshot)
    }

    /// Request cancellation if still active; return whether this request changed intent.
    /// 仍处于活动状态时请求取消；返回本次请求是否改变意图。
    /// This never marks execution terminal or releases resources on the caller's behalf.
    /// 此操作绝不代表调用方将执行标为终态或释放资源。
    pub fn cancel(&self) -> EmbeddedResult<bool> {
        // Serialize intent with completion so cancellation after success is a no-op.
        // 将意图与完成串行化，使成功后的取消不产生变更。
        let snapshot = self.operation.lock()?;
        if snapshot.phase.is_terminal() {
            return Ok(false);
        }
        Ok(self.operation.control.cancel())
    }

    /// Wait at most `timeout` for completion and return the actual current snapshot.
    /// 最多等待 `timeout` 以观察完成，并返回真实当前快照。
    /// A caller wait timeout does not cancel work or replace its original execution budget.
    /// 调用方等待超时不会取消工作，也不会替换原始执行预算。
    pub fn wait(&self, timeout: Duration) -> EmbeddedResult<OperationSnapshot> {
        // Checked local wait deadline is independent from the operation deadline.
        // 受检的本地等待截止时间独立于操作截止时间。
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| EmbeddedError::invalid("wait deadline cannot be represented"))?;
        // The same mutex is always paired with this operation's condition variable.
        // 此操作的条件变量始终配合同一个互斥锁使用。
        let mut snapshot = self.operation.lock()?;
        while !snapshot.phase.is_terminal() {
            // Spurious wakes consume only the remaining caller wait budget.
            // 虚假唤醒仅消耗调用方剩余等待预算。
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            snapshot = self
                .operation
                .changed
                .wait_timeout(snapshot, remaining)
                .map_err(|_| {
                    EmbeddedError::new(
                        EmbeddedErrorCode::Internal,
                        "operation wait lock is poisoned",
                    )
                })?
                .0;
        }
        // Cancellation intent may change independently of the phase snapshot.
        // 取消意图可能独立于阶段快照变化。
        let result = snapshot.clone();
        drop(snapshot);
        self.operation.project(result)
    }
}

/// Non-cloneable execution authority retained until real execution and cleanup finish.
/// 保留到真实执行与清理结束的不可克隆执行权威。
pub struct OperationOwner {
    /// Exclusive closing-budget and outcome ownership across persistence retries.
    /// 跨持久化重试的独占关闭预算与结果所有权。
    finalization: Option<FinalizationOwnership>,
    /// Exact state shared with client views and the bounded registry.
    /// 与客户端视图及有界注册表共享的精确状态。
    operation: Arc<Operation>,
    /// The original bounded terminal outcome survives a failed checkpoint and can only be retried explicitly.
    /// 原始有界终态结果在检查点失败后存活，且只能显式重试。
    pending_completion: Option<Arc<OperationSnapshot>>,
    /// Queued terminal write retained across observation and explicit retry, never reconstructed by polling.
    /// 跨观测及显式重试保留的排队终态写入，绝不由轮询重新构造。
    completion_checkpoint: Option<PendingCheckpoint>,
}

impl OperationOwner {
    /// Borrow the original prepared terminal outcome before acknowledgement, without claiming completion.
    /// 确认前借用原始已准备终态结果，不声称已经完成。
    pub fn pending_completion(&self) -> Option<&OperationSnapshot> {
        self.pending_completion.as_deref()
    }

    /// Retry only the retained terminal checkpoint; never execute plugin code or accept a replacement outcome.
    /// 仅重试保留的终态检查点；绝不执行插件代码或接收替代结果。
    /// A journal with uncertain commit evidence rejects this retry until explicit storage recovery.
    /// 日志的提交证据不确定时，在显式存储恢复前拒绝此重试。
    pub fn retry_completion(&mut self) -> EmbeddedResult<()> {
        if self
            .operation
            .history
            .as_ref()
            .is_some_and(OperationHistory::is_queued)
        {
            self.drive_completion(true, true)?;
            return Ok(());
        }
        if self.operation.lock()?.phase != OperationPhase::Cleaning {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation completion requires finished execution and cleanup",
            ));
        }
        // Direct persistence may refresh cancellation only before a new explicit attempt.
        // 直接持久化只在新的显式尝试之前刷新取消观测。
        let snapshot = self.pending_completion.as_mut().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no retained terminal checkpoint",
            )
        })?;
        Arc::make_mut(snapshot).cancellation_requested = self.operation.control.is_cancelled();
        self.operation.checkpoint(snapshot)?;
        // Publication follows the durable receipt and never exposes an unacknowledged terminal result.
        // 发布跟随持久回执，绝不暴露尚未确认的终态结果。
        let mut current = self.operation.lock()?;
        *current = Arc::unwrap_or_clone(
            self.pending_completion
                .take()
                .expect("owned completion retained through checkpoint"),
        );
        self.operation.changed.notify_all();
        Ok(())
    }

    /// Return the original operation control for VM and host-capability execution.
    /// 返回 VM 与宿主能力执行使用的原始操作控制。
    pub fn control(&self) -> Arc<CallControl> {
        Arc::clone(&self.operation.control)
    }

    /// Move to nonterminal `phase`; reject transitions that would bypass lifecycle rules.
    /// 转入非终态 `phase`；拒绝绕过生命周期规则的转换。
    pub fn advance(&self, phase: OperationPhase) -> EmbeddedResult<()> {
        if self.pending_completion.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has a retained terminal checkpoint",
            ));
        }
        // Queue-backed execution waits only on its original write; control-thread users call poll_advance.
        // 队列后端的执行仅等待原写入；控制线程用户调用 poll_advance。
        let mut transition = self
            .operation
            .transition
            .lock()
            .map_err(|_| checkpoint::poisoned())?;
        if self
            .operation
            .history
            .as_ref()
            .is_some_and(OperationHistory::is_queued)
        {
            self.drive_advance(&mut transition, phase, true, true)?;
            return Ok(());
        }
        if self.operation.history.is_none() {
            return advance_snapshot(&mut *self.operation.lock()?, phase);
        }
        // The public lock is released before any synchronous storage work starts.
        // 任何同步存储开始前均释放公开锁。
        let mut snapshot = self.operation.lock()?.clone();
        advance_snapshot(&mut snapshot, phase)?;
        snapshot = self.operation.project(snapshot)?;
        snapshot.phase = phase;
        self.operation.checkpoint(&snapshot)?;
        *self.operation.lock()? = snapshot;
        Ok(())
    }

    /// Complete after cleanup with actual `result` and trusted `effects` evidence.
    /// 清理后使用实际 `result` 与可信 `effects` 证据完成操作。
    /// Oversized results fail explicitly without rewriting effect history.
    /// 超大结果明确失败，且不改写副作用历史。
    pub fn complete(
        &mut self,
        result: EmbeddedResult<Value>,
        effects: EffectState,
    ) -> EmbeddedResult<()> {
        self.prepare_completion(result, effects)?;
        self.retry_completion()
    }
}

/// Private registry state; result retention is explicit and bounded.
/// 私有注册表状态；结果保留显式且有界。
struct OperationRegistryState {
    /// Monotonic sequence never reused after explicit record removal.
    /// 显式移除记录后也不会复用的单调序号。
    sequence: u64,
    /// Pending and terminal records under one capacity limit.
    /// 受同一容量上限约束的待完成记录与终态记录。
    records: BTreeMap<String, Arc<Operation>>,
}

/// In-memory operation journal; missing records never imply that effects did not occur.
/// 内存操作日志；记录缺失绝不表示副作用未发生。
pub struct OperationRegistry {
    /// Optional explicit history; admission itself performs no disk I/O while holding registry metadata.
    /// 可选显式历史；入场本身持有注册表元数据时不执行磁盘 I/O。
    journal: Option<HistoryBackend>,
    /// Retained host effect count per operation, copied from the authoritative configuration.
    /// 从权威配置复制的逐操作宿主副作用保留数量。
    max_effect_records: usize,
    /// Retained module context and host effect metadata bytes per operation.
    /// 逐操作模块上下文及宿主副作用元数据保留字节数。
    max_effect_bytes: usize,
    /// Trusted runtime namespace used only to format opaque operation IDs.
    /// 仅用于生成不透明操作 ID 的可信运行时命名空间。
    runtime_id: String,
    /// Maximum live records from the runtime's authoritative configuration.
    /// 来自运行时权威配置的活跃记录数量上限。
    max_operations: usize,
    /// Maximum retained result bytes from the same authoritative configuration.
    /// 来自同一权威配置的保留结果字节上限。
    max_value_bytes: usize,
    /// Admission and explicit retention changes are atomic.
    /// 入场与显式保留变更为原子操作。
    state: Mutex<OperationRegistryState>,
}

impl OperationRegistry {
    /// Create a journal for trusted `runtime_id` using validated `config` limits.
    /// 使用已校验的 `config` 上限，为可信 `runtime_id` 创建日志。
    pub fn new(runtime_id: String, config: &EmbeddedRuntimeConfig) -> EmbeddedResult<Self> {
        config.validate()?;
        if runtime_id.trim().is_empty() {
            return Err(EmbeddedError::invalid("runtime identity must be nonempty"));
        }
        Ok(Self {
            journal: None,
            max_effect_records: config.max_effect_records_per_operation,
            max_effect_bytes: config.max_effect_bytes_per_operation,
            runtime_id,
            max_operations: config.max_operations,
            max_value_bytes: config.max_value_bytes,
            state: Mutex::new(OperationRegistryState {
                sequence: 0,
                records: BTreeMap::new(),
            }),
        })
    }

    /// Create a registry whose owner transitions checkpoint into the explicitly supplied `journal`.
    /// 创建所有者阶段变更会向显式提供的 `journal` 写入检查点的注册表。
    /// Admission is memory-only; initialization/execution and terminal publication require durable receipt.
    /// 入场仅在内存中进行；初始化、执行及终态发布要求持久回执。
    /// Owners must run outside scheduler/registry locks; forgetting a live handle never erases disk history.
    /// 所有者必须在调度器及注册表锁外运行；遗忘活动句柄绝不删除磁盘历史。
    /// Use a fresh runtime namespace; existing historical identities cannot be adopted by new owners.
    /// 使用全新运行时命名空间；新的所有者不能接管已有历史身份。
    pub fn with_journal(
        runtime_id: String,
        config: &EmbeddedRuntimeConfig,
        journal: Arc<OperationJournal>,
    ) -> EmbeddedResult<Self> {
        // Reuse the sole validation and budget authority before enabling explicit durable checkpoints.
        // 启用显式持久检查点前复用唯一校验与预算权威。
        let mut registry = Self::new(runtime_id, config)?;
        registry.journal = Some(HistoryBackend::Direct(journal));
        Ok(registry)
    }

    /// Admit explicitly unbound low-level work with original `control`, returning a client handle and unique owner.
    /// 使用原始 `control` 接纳明确未绑定的低层工作，返回客户端句柄与唯一所有者。
    /// Reject exhausted retention capacity; never evict unfinished work.
    /// 保留容量耗尽时拒绝接纳；绝不驱逐未完成工作。
    pub fn admit(
        &self,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<(OperationHandle, OperationOwner)> {
        self.admit_context(control, |_| Ok(OperationContext::Unbound))
    }

    /// Query the exact `id`; unknown or explicitly expired identities return not-found.
    /// 查询精确 `id`；未知或显式过期的身份返回未找到。
    pub fn get(&self, id: &str) -> EmbeddedResult<OperationHandle> {
        self.lock()?
            .records
            .get(id)
            .map(|operation| OperationHandle {
                operation: Arc::clone(operation),
            })
            .ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::NotFound,
                    "operation record is unknown or expired",
                )
            })
    }

    /// Explicitly forget terminal `id`; reject attempts to discard live execution evidence.
    /// 显式遗忘终态 `id`；拒绝丢弃活跃执行证据。
    pub fn forget(&self, id: &str) -> EmbeddedResult<()> {
        // Keep terminal-state validation and record removal atomic with registry admission.
        // 将终态校验与记录移除相对注册表入场保持原子性。
        let mut state = self.lock()?;
        // An exact known record is required; absence cannot mean successful removal.
        // 必须存在精确已知记录；缺失不代表移除成功。
        let operation = state.records.get(id).ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::NotFound,
                "operation record is unknown or expired",
            )
        })?;
        if !operation.lock()?.phase.is_terminal() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has not finished execution and cleanup",
            ));
        }
        state.records.remove(id);
        Ok(())
    }

    /// Acquire registry metadata or report its poisoned state explicitly.
    /// 获取注册表元数据，或明确报告其中毒状态。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, OperationRegistryState>> {
        self.state.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation registry lock is poisoned",
            )
        })
    }
}
