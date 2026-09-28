//! Independent closing budgets with retained business and closing evidence.
//! 独立关闭预算及保留的业务与关闭证据。

use super::checkpoint::PendingCheckpoint;
use super::*;
use crate::runtime::embedded::effects::EffectAdmissionStage;
use std::sync::TryLockError;

/// One bounded execution outcome; explicit JSON null remains a successful value.
/// 一项有界执行结果；显式 JSON 空值仍为成功值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum OperationOutcome {
    /// The stage returned its validated application value.
    /// 该阶段返回经过校验的应用值。
    Succeeded {
        /// Exact successful JSON value, including explicit null.
        /// 精确成功 JSON 值，包含显式空值。
        value: Value,
    },
    /// The stage failed without discarding the other stage's result.
    /// 该阶段失败，但不丢弃另一阶段的结果。
    Failed {
        /// Structured stage error independent of side-effect evidence.
        /// 独立于副作用证据的结构化阶段错误。
        error: EmbeddedError,
    },
}

impl OperationOutcome {
    /// Bound a supplied stage result by the operation's existing value budget.
    /// 使用操作既有值预算限制提供的阶段结果。
    /// Return a fixed error when the result cannot be retained safely.
    /// 结果无法安全保留时返回固定错误。
    pub(super) fn bounded(result: EmbeddedResult<Value>, maximum: usize) -> Self {
        // Measure the original application value or diagnostic, without invoking user code.
        // 计量原应用值或诊断，不调用用户代码。
        let encoded = match &result {
            Ok(value) => json_size(value, maximum),
            Err(error) => json_size(error, maximum),
        };
        // Preserve the established fixed diagnostics for rejected oversized outcomes.
        // 对被拒绝的超大结果保留既有固定诊断。
        let result = match encoded {
            Ok(_) => result,
            Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => {
                Err(EmbeddedError::new(
                    EmbeddedErrorCode::CapacityExceeded,
                    "operation result exceeds the configured byte limit",
                ))
            }
            Err(_) => Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation result serialization failed",
            )),
        };
        match result {
            Ok(value) => Self::Succeeded { value },
            Err(error) => Self::Failed { error },
        }
    }

    /// Clone the exact stage value or error for ordinary Rust result consumers.
    /// 为普通 Rust 结果消费方克隆精确阶段值或错误。
    pub fn result(&self) -> EmbeddedResult<Value> {
        match self {
            Self::Succeeded { value } => Ok(value.clone()),
            Self::Failed { error } => Err(error.clone()),
        }
    }
}

/// Durable closing intent and separate outcomes within one original operation identity.
/// 同一原始操作身份中的持久关闭意图与独立结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct OperationFinalization {
    /// Exact host-selected declared export; absence of an outcome is not proof it never ran.
    /// 宿主选择的精确声明导出；缺少结果不能证明其从未运行。
    pub export: String,
    /// Bounded original business result retained even if closing fails.
    /// 即使关闭失败也保留的有界原始业务结果。
    pub business: OperationOutcome,
    /// Frozen number of ordered host effects admitted before the closing phase.
    /// 关闭阶段前接纳的有序宿主副作用的冻结数量。
    pub business_effect_count: usize,
    /// Actual closing result; omitted until the owner records a returned outcome.
    /// 实际关闭结果；所有者记录已返回结果前省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OperationOutcome>,
}

/// Sole owner of budget issuance; observing or retrying storage never creates a new budget.
/// 预算签发的唯一所有者；观察或重试存储绝不创建新预算。
pub(super) struct FinalizationOwnership {
    /// Validated finite duration; its clock starts only when execution claims the control.
    /// 经过校验的有限时长；仅在执行认领控制时开始计时。
    pub(super) timeout: Duration,
    /// Claimed before creating or exposing a closing control and never reset.
    /// 在创建或暴露关闭控制前认领，且绝不重置。
    pub(super) control_issued: bool,
    /// Returned outcome has been frozen for persistence, independently of disk acknowledgement.
    /// 已返回结果已经冻结以便持久化，与磁盘确认相互独立。
    pub(super) outcome_prepared: bool,
}

impl OperationSnapshot {
    /// Validate finalization shape and terminal projection before journal acceptance or restoration.
    /// 日志接纳或恢复前校验关闭形状及终态投影。
    /// Return an explicit error instead of treating contradictory history as authoritative evidence.
    /// 返回显式错误，不将矛盾历史视为权威证据。
    pub(in crate::runtime::embedded) fn validate_finalization(&self) -> EmbeddedResult<()> {
        let Some(finalization) = &self.finalization else {
            return Ok(());
        };
        if finalization.export.trim().is_empty()
            || finalization.export.contains('\0')
            || finalization.business_effect_count > self.host_effects.len()
            || (!self.phase.is_terminal() && self.phase != OperationPhase::Cleaning)
            || self
                .host_effects
                .iter()
                .take(finalization.business_effect_count)
                .any(|effect| effect.phase != super::super::HostEffectPhase::Completed)
        {
            return Err(EmbeddedError::invalid(
                "operation finalization evidence is inconsistent",
            ));
        }
        if finalization.outcome.is_some()
            && self
                .host_effects
                .iter()
                .any(|effect| effect.phase != super::super::HostEffectPhase::Completed)
        {
            return Err(EmbeddedError::invalid(
                "closing outcome precedes actual host completion",
            ));
        }
        if !self.phase.is_terminal() {
            if self.value.is_some() || self.error.is_some() {
                return Err(EmbeddedError::invalid(
                    "nonterminal finalization cannot carry a terminal result",
                ));
            }
            return Ok(());
        }
        let closing = finalization.outcome.as_ref().ok_or_else(|| {
            EmbeddedError::invalid("terminal finalization has no closing outcome")
        })?;
        // Business failure remains primary; a closing failure changes a successful business terminal status.
        // 业务失败仍为主错误；关闭失败会改变成功业务的终态状态。
        let result = finalization
            .business
            .result()
            .and_then(|value| closing.result().map(|_| value));
        let consistent = match result {
            Ok(value) => {
                self.phase == OperationPhase::Succeeded
                    && self.value.as_ref() == Some(&value)
                    && self.error.is_none()
            }
            Err(error) => {
                let phase = if error.code == EmbeddedErrorCode::Cancelled {
                    OperationPhase::Cancelled
                } else {
                    OperationPhase::Failed
                };
                self.phase == phase && self.value.is_none() && self.error.as_ref() == Some(&error)
            }
        };
        if !consistent {
            return Err(EmbeddedError::invalid(
                "terminal projection contradicts retained stage outcomes",
            ));
        }
        Ok(())
    }

    /// Keep original business evidence and any returned closing outcome immutable across journal successors.
    /// 在日志后继版本中保持原业务证据及任何已返回关闭结果不可变。
    pub(in crate::runtime::embedded) fn validate_finalization_successor(
        &self,
        previous: &Self,
    ) -> EmbeddedResult<()> {
        let Some(original) = &previous.finalization else {
            return Ok(());
        };
        let next = self.finalization.as_ref().ok_or_else(|| {
            EmbeddedError::invalid("operation finalization evidence cannot be removed")
        })?;
        if original.export != next.export
            || original.business != next.business
            || original.business_effect_count != next.business_effect_count
            || original
                .outcome
                .as_ref()
                .is_some_and(|outcome| next.outcome.as_ref() != Some(outcome))
        {
            return Err(EmbeddedError::invalid(
                "operation finalization evidence is immutable",
            ));
        }
        Ok(())
    }
}

impl OperationOwner {
    /// Freeze original business evidence and one closing intent using an explicit finite timeout.
    /// 使用显式有限超时冻结原业务证据及单次关闭意图。
    /// Poll or explicitly retry the retained checkpoint before claiming the closing control.
    /// 认领关闭控制前，轮询或显式重试保留的检查点。
    pub fn prepare_finalization(
        &mut self,
        export: String,
        business: EmbeddedResult<Value>,
        timeout: Duration,
    ) -> EmbeddedResult<()> {
        if self.finalization.is_some() || self.pending_completion.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation already owns finalization or terminal completion",
            ));
        }
        if export.trim().is_empty() || export.contains('\0') {
            return Err(EmbeddedError::invalid(
                "closing export must be nonempty and contain no NUL",
            ));
        }
        // Validate representability now; actual elapsed time starts at sole execution issuance.
        // 现在校验可表示性；真实计时在唯一执行签发时开始。
        CallControl::new(timeout)?;
        json_size(&export, self.operation.max_value_bytes)?;
        self.require_pollable_finalization_history()?;
        // One mutation gate orders lifecycle intent against any retained host checkpoint.
        // 一个变更门将生命周期意图与任何保留的宿主检查点排序。
        let mut transition = self
            .operation
            .transition
            .try_lock()
            .map_err(finalization_lock_error)?;
        if transition.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation checkpoint is pending",
            ));
        }
        let mut snapshot = self.operation.lock()?.clone();
        advance_snapshot(&mut snapshot, OperationPhase::Cleaning)?;
        let business = OperationOutcome::bounded(business, self.operation.max_value_bytes);
        let business_effect_count = self.operation.effects.begin_finalization()?;
        self.finalization = Some(FinalizationOwnership {
            timeout,
            control_issued: false,
            outcome_prepared: false,
        });
        snapshot = self.operation.project(snapshot)?;
        snapshot.finalization = Some(OperationFinalization {
            export,
            business,
            business_effect_count,
            outcome: None,
        });
        self.retain_finalization_checkpoint(&mut transition, snapshot)?;
        Ok(())
    }

    /// Observe the original closing checkpoint without issuing controls or replaying any callback.
    /// 观察原关闭检查点，不签发控制对象，也不重放任何回调。
    /// Return false while pending; storage failures require existing explicit retry_advance recovery.
    /// 待完成时返回 false；存储失败要求使用既有显式 retry_advance 恢复。
    pub fn poll_finalization(&self) -> EmbeddedResult<bool> {
        if self.finalization.is_none() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no prepared finalization",
            ));
        }
        let mut transition = match self.operation.transition.try_lock() {
            Ok(transition) => transition,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(_)) => return Err(checkpoint::poisoned()),
        };
        if let Some(checkpoint) = transition.as_ref() {
            if !checkpoint.is_lifecycle_phase(OperationPhase::Cleaning) {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "another operation checkpoint is pending",
                ));
            }
            self.drive_advance(&mut transition, OperationPhase::Cleaning, false, false)
        } else {
            Ok(self.operation.lock()?.finalization.is_some())
        }
    }

    /// Issue the sole finite closing control after its original intent is acknowledged.
    /// 原始意图确认后签发唯一有限关闭控制。
    /// It shares trusted identity and effect retention, while business cancellation cannot renew or cancel it.
    /// 它共享可信身份及副作用保留，而业务取消不能续期或取消它。
    pub fn take_finalization_control(&mut self) -> EmbeddedResult<Arc<CallControl>> {
        let transition = self
            .operation
            .transition
            .try_lock()
            .map_err(finalization_lock_error)?;
        if transition.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "finalization intent is not acknowledged",
            ));
        }
        let snapshot = self.operation.lock()?;
        if snapshot.phase != OperationPhase::Cleaning || snapshot.finalization.is_none() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "finalization intent is not published",
            ));
        }
        let ownership = self.finalization.as_mut().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no prepared finalization",
            )
        })?;
        if ownership.control_issued {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "closing control was already issued",
            ));
        }
        // Claim before fallible construction so recovery cannot reset a partially issued execution budget.
        // 在可失败构造前认领，确保恢复不能重置部分签发的执行预算。
        ownership.control_issued = true;
        let control = Arc::new(CallControl::new(ownership.timeout)?);
        control.attach_effects(
            Arc::clone(&self.operation.effects),
            EffectAdmissionStage::Finalization,
        )?;
        Ok(control)
    }

    /// Freeze the actual closing outcome after all closing handlers released their ownership.
    /// 全部关闭处理器释放所有权后冻结实际关闭结果。
    /// Poll its retained checkpoint before terminal completion; a failed write never allows another outcome.
    /// 终态完成前轮询其保留检查点；写入失败绝不允许替换结果。
    pub fn prepare_finalization_outcome(
        &mut self,
        result: EmbeddedResult<Value>,
    ) -> EmbeddedResult<()> {
        let ownership = self.finalization.as_ref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no prepared finalization",
            )
        })?;
        if !ownership.control_issued
            || ownership.outcome_prepared
            || self.pending_completion.is_some()
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "closing outcome is unavailable or already retained",
            ));
        }
        let mut transition = self
            .operation
            .transition
            .try_lock()
            .map_err(finalization_lock_error)?;
        if transition.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host checkpoint is pending",
            ));
        }
        let mut snapshot = self.operation.lock()?.clone();
        if snapshot.phase != OperationPhase::Cleaning {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation is outside finalization",
            ));
        }
        let outcome = OperationOutcome::bounded(result, self.operation.max_value_bytes);
        // Sealing shares admission's lock, so a late callback cannot appear beyond this returned outcome.
        // 封存共用入场锁，确保迟到回调不能出现在此已返回结果之后。
        snapshot.host_effects = self.operation.effects.seal()?;
        snapshot.effects = merge_effects(snapshot.effects, &snapshot.host_effects);
        snapshot.cancellation_requested = self.operation.control.is_cancelled();
        snapshot
            .finalization
            .as_mut()
            .expect("owned finalization is published")
            .outcome = Some(outcome);
        self.finalization
            .as_mut()
            .expect("owned finalization remains")
            .outcome_prepared = true;
        self.retain_finalization_checkpoint(&mut transition, snapshot)?;
        Ok(())
    }

    /// Reject synchronous disk backends before consuming any finalization authority.
    /// 消费任何关闭权威前拒绝同步磁盘后端。
    fn require_pollable_finalization_history(&self) -> EmbeddedResult<()> {
        if self
            .operation
            .history
            .as_ref()
            .is_some_and(|history| !history.is_queued())
        {
            return Err(EmbeddedError::invalid(
                "finalization checkpoints require a journal worker",
            ));
        }
        Ok(())
    }

    /// Publish memory evidence or retain the exact queued snapshot under the caller-held mutation gate.
    /// 在调用方持有的变更门下发布内存证据或保留精确排队快照。
    /// No filesystem access or callback occurs in this preparation step.
    /// 此准备步骤不访问文件系统，也不调用回调。
    fn retain_finalization_checkpoint(
        &self,
        transition: &mut Option<PendingCheckpoint>,
        snapshot: OperationSnapshot,
    ) -> EmbeddedResult<()> {
        if self.operation.history.is_none() {
            *self.operation.lock()? = snapshot;
            self.operation.changed.notify_all();
        } else {
            *transition = Some(PendingCheckpoint::new(Arc::new(snapshot)));
        }
        Ok(())
    }
}

/// Translate a nonblocking mutation-gate failure without waiting or guessing ownership.
/// 转换非阻塞变更门故障，不等待也不猜测所有权。
fn finalization_lock_error<T>(error: TryLockError<T>) -> EmbeddedError {
    match error {
        TryLockError::WouldBlock => {
            EmbeddedError::new(EmbeddedErrorCode::Busy, "operation checkpoint is owned")
        }
        TryLockError::Poisoned(_) => checkpoint::poisoned(),
    }
}
