use super::capabilities::CapabilityCaller;
use super::value_size::json_size;
use super::{EffectState, EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

/// Actual handler lifecycle, separate from its reported business effect.
/// 真实处理器生命周期，独立于其报告的业务副作用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum HostEffectPhase {
    /// Retention reserved before execution.
    /// 执行前已预留保留容量。
    Prepared,
    /// Handler may execute; cancellation is not completion.
    /// 处理器可能执行；取消不等于完成。
    Running,
    /// Actual handler and admission ownership have been released.
    /// 真实处理器及入场所有权已释放。
    Completed,
}

/// Bounded evidence retained independently from values returned to Lua.
/// 独立于返回 Lua 的值保留的有界证据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct HostEffectRecord {
    /// Original host-bound caller identity, retained for reconciliation without consulting a newer plugin generation.
    /// 原始宿主绑定调用身份；对账保留该身份，不查询较新的插件代次。
    pub caller: CapabilityCaller,
    /// Never-reused identity within the original operation.
    /// 原始操作内绝不复用的身份。
    pub effect_id: String,
    /// Exact immutable registration identity.
    /// 精确不可变注册身份。
    pub registration_id: String,
    /// Public capability name, excluding business arguments and credentials.
    /// 公开能力名称，不包含业务参数与凭证。
    pub capability_name: String,
    /// Interface version of the exact registration.
    /// 精确注册的接口版本。
    pub capability_version: String,
    /// SDK request identity for queued execution.
    /// 队列执行的 SDK 请求身份。
    pub request_id: Option<String>,
    /// Actual handler ownership lifecycle.
    /// 真实处理器所有权生命周期。
    pub phase: HostEffectPhase,
    /// Trusted host evidence, independent from caller cancellation.
    /// 可信宿主证据，独立于调用方取消。
    pub effects: EffectState,
}

/// Internal admission stage bound to a control, never supplied by Lua or capability arguments.
/// 绑定到控制对象的内部入场阶段，绝不由 Lua 或能力参数提供。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EffectAdmissionStage {
    /// Original initialization and business execution.
    /// 原始初始化与业务执行。
    Business,
    /// The sole separately budgeted closing execution.
    /// 唯一使用独立预算的关闭执行。
    Finalization,
}

/// Private retention metadata; no user implementation runs under this lock.
/// 私有保留元数据；此锁内不运行用户实现。
#[derive(Debug)]
struct LedgerState {
    /// Only controls for this owner-selected stage may admit new effects.
    /// 只有匹配所有者所选阶段的控制对象可以接纳新副作用。
    stage: EffectAdmissionStage,
    /// Permanent terminal admission gate.
    /// 永久终态入场门。
    sealed: bool,
    /// Monotonic identity counter.
    /// 单调身份计数器。
    sequence: u64,
    /// Reserved serialized module context and record bytes.
    /// 预留序列化模块上下文及记录字节数。
    bytes: usize,
    /// Records retained until the operation is released.
    /// 保留至操作释放的记录。
    records: BTreeMap<u64, HostEffectRecord>,
}

/// One operation's bounded journal shared by control and read-only observers.
/// 控制对象与只读观察者共享的单操作有界日志。
#[derive(Debug)]
pub(super) struct EffectLedger {
    /// Exact admitted module caller; absent only for explicitly unbound low-level operations.
    /// 精确入场模块调用方；仅明确未绑定的低层操作省略。
    caller: Option<CapabilityCaller>,
    /// Persistent operations share their exact mutation gate without creating an ownership cycle.
    /// 持久操作共享其精确变更门禁，不创建所有权循环。
    operation: Option<Weak<super::operations::Operation>>,
    /// Trusted runtime namespace.
    /// 可信运行时命名空间。
    runtime_id: String,
    /// Exact owning operation identity.
    /// 精确所属操作身份。
    operation_id: String,
    /// Parent-authorized record count.
    /// 父级授权记录数量。
    max_records: usize,
    /// Parent-authorized serialized metadata bytes.
    /// 父级授权序列化元数据字节数。
    max_bytes: usize,
    /// Admission and evidence authority.
    /// 入场与证据权威。
    state: Mutex<LedgerState>,
}

impl EffectLedger {
    /// Close business admission only after all original handlers returned and release their ownership.
    /// 仅在全部原处理器返回并释放所有权后关闭业务入场。
    /// Return the frozen business-effect count; only owner-issued closing controls may enter afterward.
    /// 返回冻结的业务副作用数量；之后仅所有者签发的关闭控制可以入场。
    pub(super) fn begin_finalization(&self) -> EmbeddedResult<usize> {
        // Stage publication and callback admission share this one metadata transaction.
        // 阶段发布和回调入场共用这一次元数据事务。
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.sealed || state.stage != EffectAdmissionStage::Business {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "operation finalization was already prepared or sealed",
            ));
        }
        if state
            .records
            .values()
            .any(|record| record.phase != HostEffectPhase::Completed)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "business host handlers have not finished execution and cleanup",
            ));
        }
        state.stage = EffectAdmissionStage::Finalization;
        Ok(state.records.len())
    }

    /// Create empty evidence for exact runtime/operation identities and explicit retention budgets.
    /// 为精确运行时及操作身份和显式保留预算创建空证据。
    /// `operation` binds persistent dispatch to its exact owner; None explicitly selects memory-only evidence.
    /// `operation` 将持久分发绑定到精确所有者；None 显式选择纯内存证据。
    /// `caller` freezes module authority and `context_bytes` reserves its already-validated share of `max_bytes`.
    /// `caller` 冻结模块权威，`context_bytes` 预留其已经校验的 `max_bytes` 份额。
    /// Return the shared ledger whose dynamic context and records consume one authoritative byte budget.
    /// 返回共享账本；其动态上下文及记录共同消耗一个权威字节预算。
    pub(super) fn new(
        runtime_id: String,
        operation_id: String,
        max_records: usize,
        max_bytes: usize,
        operation: Option<Weak<super::operations::Operation>>,
        caller: Option<CapabilityCaller>,
        context_bytes: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            caller,
            operation,
            runtime_id,
            operation_id,
            max_records,
            max_bytes,
            state: Mutex::new(LedgerState {
                stage: EffectAdmissionStage::Business,
                sealed: false,
                sequence: 0,
                bytes: context_bytes,
                records: BTreeMap::new(),
            }),
        })
    }

    /// Return original operation authority for initialization and nested calls.
    /// 返回初始化与嵌套调用所用的原始操作权威。
    pub(super) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Borrow the admitted host request correlation; unbound operations have no request authority.
    /// 借用入场宿主请求关联；未绑定操作没有请求权威。
    /// Returns the original optional value without reading mutable Lua context.
    /// 返回原始可选值，不读取可变 Lua 上下文。
    pub(super) fn request_id(&self) -> Option<&str> {
        self.caller
            .as_ref()
            .and_then(|caller| caller.request_id.as_deref())
    }

    /// Reserve exact registration evidence before host execution; reject wrong callers or exhausted capacity.
    /// 宿主执行前预留精确注册证据；拒绝错误调用方或耗尽容量。
    pub(super) fn prepare(
        self: &Arc<Self>,
        stage: EffectAdmissionStage,
        caller: &CapabilityCaller,
        registration_id: &str,
        name: &str,
        version: &str,
    ) -> EmbeddedResult<EffectAttempt> {
        if caller.runtime_id != self.runtime_id || caller.operation_id != self.operation_id {
            return Err(EmbeddedError::invalid(
                "capability caller does not match its operation journal",
            ));
        }
        if self.caller.as_ref().is_some_and(|bound| bound != caller) {
            return Err(EmbeddedError::invalid(
                "capability caller does not match admitted module context",
            ));
        }
        // Reserve count, bytes and identity atomically before any handler can start.
        // 在任何处理器可以开始前原子预留数量、字节与身份。
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.sealed || state.stage != stage {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "operation effect journal is sealed for this execution stage",
            ));
        }
        if state.records.len() >= self.max_records {
            return Err(capacity());
        }
        let sequence = state.sequence.checked_add(1).ok_or_else(capacity)?;
        let id = format!("{}:effect:{sequence}", self.operation_id);
        let record = HostEffectRecord {
            caller: caller.clone(),
            effect_id: id.clone(),
            registration_id: registration_id.into(),
            capability_name: name.into(),
            capability_version: version.into(),
            request_id: None,
            phase: HostEffectPhase::Prepared,
            effects: EffectState::NotStarted,
        };
        let bytes = reserved_bytes(&record)?;
        let total = state
            .bytes
            .checked_add(bytes)
            .filter(|bytes| *bytes <= self.max_bytes)
            .ok_or_else(capacity)?;
        state.sequence = sequence;
        state.bytes = total;
        state.records.insert(sequence, record);
        Ok(EffectAttempt {
            owner: Some((Arc::clone(self), sequence, id)),
        })
    }

    /// Read actual effects even while cancellation and handler cleanup remain in progress.
    /// 即使取消与处理器清理仍在进行，也读取真实副作用。
    pub(super) fn snapshot(&self) -> EmbeddedResult<Vec<HostEffectRecord>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| poisoned())?
            .records
            .values()
            .cloned()
            .collect())
    }

    /// Seal only after every handler releases its actual ownership; reject premature completion.
    /// 仅在每个处理器释放真实所有权后封存；拒绝提前完成。
    pub(super) fn seal(&self) -> EmbeddedResult<Vec<HostEffectRecord>> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state
            .records
            .values()
            .any(|record| record.phase != HostEffectPhase::Completed)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host handlers have not finished execution and cleanup",
            ));
        }
        state.sealed = true;
        Ok(state.records.values().cloned().collect())
    }
}

/// Unique effect owner, destroyed after the callback clone and its admission permit.
/// 唯一副作用所有者，在回调克隆及其入场许可之后销毁。
pub(crate) struct EffectAttempt {
    /// Explicit journal binding; unregistered low-level controls make no retention promise.
    /// 显式日志绑定；未注册低层控制对象不承诺保留。
    owner: Option<(Arc<EffectLedger>, u64, String)>,
}

impl EffectAttempt {
    /// Poll returned handler evidence without waiting, preserving the caller's result and permit until true.
    /// 不等待地轮询已返回处理器证据，在返回真前保留调用方结果及许可。
    pub(crate) fn poll_outcome(&self) -> EmbeddedResult<bool> {
        // Explicit memory-only and untracked ledgers have no storage acknowledgement requirement.
        // 显式纯内存及未跟踪账本没有存储确认要求。
        let Some((ledger, _, effect_id)) = &self.owner else {
            return Ok(true);
        };
        let Some(operation) = &ledger.operation else {
            return Ok(true);
        };
        // Keep the exact persistent operation alive for this observation without retaining a strong cycle.
        // 为本次观测保持精确持久操作存活，不保留强引用循环。
        let operation = operation.upgrade().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "persistent operation owner was released",
            )
        })?;
        operation.poll_effect_outcome(effect_id)
    }

    /// Keep the native result and permit on the caller's stack until exact outcome evidence reaches durable storage.
    /// 在精确结果证据到达持久存储前，将原生结果及许可保留在调用方栈中。
    /// Storage failures require explicit operation recovery; cancellation never discards already returned evidence.
    /// 存储失败要求显式操作恢复；取消绝不丢弃已返回证据。
    pub(crate) fn confirm_native_outcome(&self) -> EmbeddedResult<()> {
        // Untracked and memory-only calls need no disk confirmation.
        // 未跟踪及纯内存调用不需要磁盘确认。
        let Some((ledger, _, effect_id)) = &self.owner else {
            return Ok(());
        };
        let Some(operation) = &ledger.operation else {
            return Ok(());
        };
        // A strong reference keeps the exact operation alive during native result retention.
        // 强引用在原生结果保留期间使精确操作保持存活。
        let operation = operation.upgrade().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "persistent operation owner was released",
            )
        })?;
        while !operation.poll_effect_outcome(effect_id)? {
            operation.wait_checkpoint_change()?;
        }
        Ok(())
    }

    /// Persist this attempt's original dispatch intent; `wait` is allowed only on a native execution thread.
    /// 持久化此尝试的原始分发意图；仅原生执行线程允许设置 `wait`。
    /// Return false for an in-flight write without changing actual handler ownership or running user code.
    /// 写入在途时返回假，不改变真实处理器所有权，也不运行用户代码。
    pub(crate) fn checkpoint_start(&self, wait: bool) -> EmbeddedResult<bool> {
        // Untracked calls and explicitly memory-only ledgers preserve their existing dispatch contract.
        // 未跟踪调用及显式纯内存账本保留其既有分发契约。
        let Some((ledger, _, effect_id)) = &self.owner else {
            return Ok(true);
        };
        let Some(operation) = &ledger.operation else {
            return Ok(true);
        };
        // Losing the exact persistent owner cannot silently turn this invocation into an untracked one.
        // 精确持久所有者丢失不能将本次调用静默转为未跟踪调用。
        let operation = operation.upgrade().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "persistent operation owner was released",
            )
        })?;
        operation.checkpoint_effect_start(effect_id, wait)
    }

    /// Construct an untracked low-level attempt without silently allocating an implicit journal.
    /// 构造未跟踪低层尝试，不静默分配隐式日志。
    pub(super) fn untracked() -> Self {
        Self { owner: None }
    }

    /// Return the stable journal identity, or none for explicit low-level execution.
    /// 返回稳定日志身份，显式低层执行则省略。
    pub(crate) fn id(&self) -> Option<&str> {
        self.owner.as_ref().map(|(_, _, id)| id.as_str())
    }

    /// Attach queued `request_id` before delivery, charging its metadata to the original operation budget.
    /// 投递前附加队列 `request_id`，将其元数据计入原始操作预算。
    pub(crate) fn bind_request(&self, request_id: &str) -> EmbeddedResult<()> {
        let Some((ledger, sequence, _)) = &self.owner else {
            return Ok(());
        };
        let mut state = ledger.state.lock().map_err(|_| poisoned())?;
        let record = state.records.get(sequence).ok_or_else(poisoned)?;
        if record.phase != HostEffectPhase::Prepared || record.request_id.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host request identity is already frozen",
            ));
        }
        let old_bytes = reserved_bytes(record)?;
        let mut updated = record.clone();
        updated.request_id = Some(request_id.into());
        let new_bytes = reserved_bytes(&updated)?;
        let total = state
            .bytes
            .checked_sub(old_bytes)
            .and_then(|bytes| bytes.checked_add(new_bytes))
            .filter(|bytes| *bytes <= ledger.max_bytes)
            .ok_or_else(capacity)?;
        state.records.insert(*sequence, updated);
        state.bytes = total;
        Ok(())
    }

    /// Record dispatch before external execution; unknown effects remain visible until acknowledged.
    /// 在外部执行前记录分发；未知副作用在确认前持续可见。
    pub(crate) fn begin(&self) -> EmbeddedResult<()> {
        let Some((ledger, sequence, _)) = &self.owner else {
            return Ok(());
        };
        let mut state = ledger.state.lock().map_err(|_| poisoned())?;
        let record = state.records.get_mut(sequence).ok_or_else(poisoned)?;
        if record.phase != HostEffectPhase::Prepared {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host effect was already dispatched",
            ));
        }
        record.phase = HostEffectPhase::Running;
        record.effects = EffectState::Unknown;
        Ok(())
    }

    /// Preserve actual `effects` without declaring handler cleanup complete or rewriting on cancellation.
    /// 保留真实 `effects`，不声明处理器清理已完成，也不因取消改写。
    pub(crate) fn observe(&self, effects: EffectState) {
        if let Some((ledger, sequence, _)) = &self.owner {
            // Owned evidence survives poison recovery; public queries still expose the poisoned state.
            // 已拥有证据在中毒恢复后存活；公开查询仍报告中毒状态。
            let mut state = ledger
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(record) = state.records.get_mut(sequence) {
                record.effects = effects;
            }
        }
    }
}

impl Drop for EffectAttempt {
    /// Finalize after admission release; unacknowledged dispatched effects remain unknown.
    /// 入场释放后完成；未确认的已分发副作用保持未知。
    fn drop(&mut self) {
        if let Some((ledger, sequence, _)) = &self.owner {
            let mut state = ledger
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(record) = state.records.get_mut(sequence) {
                record.phase = HostEffectPhase::Completed;
            }
        }
    }
}

/// Merge owner evidence with host observations; per-call records retain mixed outcomes explicitly.
/// 合并所有者证据与宿主观察；逐调用记录显式保留混合结果。
pub(super) fn merge_effects(base: EffectState, records: &[HostEffectRecord]) -> EffectState {
    if base == EffectState::Unknown
        || records.iter().any(|record| {
            record.phase != HostEffectPhase::Completed || record.effects == EffectState::Unknown
        })
    {
        return EffectState::Unknown;
    }
    if base == EffectState::Committed
        || records
            .iter()
            .any(|record| record.effects == EffectState::Committed)
    {
        return EffectState::Committed;
    }
    if base == EffectState::RolledBack
        || records
            .iter()
            .any(|record| record.effects == EffectState::RolledBack)
    {
        return EffectState::RolledBack;
    }
    if base == EffectState::NotApplicable
        || records
            .iter()
            .any(|record| record.effects == EffectState::NotApplicable)
    {
        return EffectState::NotApplicable;
    }
    EffectState::NotStarted
}

/// Reserve the longest phase/effect spellings so later transitions cannot exhaust retention.
/// 预留最长阶段与副作用拼写，使后续转换不会耗尽保留容量。
fn reserved_bytes(record: &HostEffectRecord) -> EmbeddedResult<usize> {
    let mut maximum = record.clone();
    maximum.phase = HostEffectPhase::Completed;
    maximum.effects = EffectState::NotApplicable;
    json_size(&maximum, usize::MAX)
}

/// Reject retention growth before host execution.
/// 在宿主执行前拒绝保留增长。
fn capacity() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::CapacityExceeded,
        "operation effect retention capacity reached",
    )
}

/// Report inconsistency rather than invent effect certainty.
/// 报告不一致，不编造副作用确定性。
fn poisoned() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "operation effect journal is inconsistent",
    )
}
