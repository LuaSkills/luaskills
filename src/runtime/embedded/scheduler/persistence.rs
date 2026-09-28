//! Explicit observation and recovery requests for retained scheduler checkpoints.
//! 保留调度检查点的显式观测及恢复请求。

use super::*;

/// Recovery state is independent of the operation's business phase and cancellation intent.
/// 恢复状态独立于操作业务阶段及取消意愿。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum CheckpointRetryState {
    /// No retry will run until the host explicitly requests one.
    /// 宿主显式请求之前不会执行重试。
    Waiting,
    /// A single retry request is retained for the original checkpoint owner.
    /// 为原始检查点所有者保留了单次重试请求。
    Requested,
    /// The requested retry is being driven; repeated observations cannot create another attempt.
    /// 正在推进已请求重试；重复观测不能创建另一次尝试。
    Retrying,
}

/// A failed checkpoint remains queryable by exact operation ID until that original checkpoint is acknowledged.
/// 失败检查点可按精确操作 ID 查询，直至原检查点得到确认。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct OperationPersistenceFailure {
    /// Stable original operation identity, never a replacement execution.
    /// 稳定的原始操作身份，绝非替代执行。
    pub operation_id: String,
    /// Exact candidate phase whose write failed; terminal candidates are not yet publicly terminal.
    /// 写入失败的精确候选阶段；终态候选尚不是公开终态。
    pub phase: OperationPhase,
    /// Retained persistence error; this does not rewrite the original business result.
    /// 保留的持久化错误；不改写原始业务结果。
    pub error: EmbeddedError,
    /// Explicit host retry coordination, separate from ordinary polling.
    /// 显式宿主重试协调，独立于普通轮询。
    pub retry: CheckpointRetryState,
}

impl SchedulerState {
    /// Reject new work while unresolved checkpoints pause dispatch, without disabling cancellation or recovery.
    /// 未决检查点暂停分发时拒绝新工作，不禁用取消或恢复。
    pub(super) fn check_persistence_admission(&self) -> EmbeddedResult<()> {
        if self.persistence_failures.is_empty() && !self.shared_checkpoint_failed {
            Ok(())
        } else {
            Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "runtime persistence requires explicit checkpoint recovery",
            ))
        }
    }
}

impl SchedulerCenter {
    /// Inspect live checkpoint metadata without waiting for transition ownership; never retry or consume results.
    /// 检查活动检查点元数据而不等待变更所有权；绝不重试或消费结果。
    pub(super) fn observe_shared_checkpoint_failures(&self) -> EmbeddedResult<()> {
        if !self.operations.has_queued_history() {
            return Ok(());
        }
        // Serialize this short metadata observation with direct fault queries so an older scan cannot erase a newer pause.
        // 将此短时元数据观测与直接故障查询串行化，避免较旧遍历清除较新暂停。
        let mut state = self.lock()?;
        // Every operation lookup and checkpoint inspection is metadata-only; inspection never waits for its mutation gate.
        // 每个操作查询及检查点检查均仅访问元数据；检查绝不等待其变更门禁。
        let mut failed = false;
        // An incomplete observation cannot prove a previously observed failure was repaired.
        // 不完整观测不能证明此前观测到的故障已经修复。
        let mut incomplete = false;
        for identity in state.live.keys() {
            match self.operations.get(identity)?.checkpoint_failure() {
                Ok(Some(_)) => {
                    failed = true;
                    break;
                }
                Ok(None) => {}
                Err(error) if error.code == EmbeddedErrorCode::Busy => incomplete = true,
                Err(error) => return Err(error),
            }
        }
        if failed {
            state.shared_checkpoint_failed = true;
        } else if !incomplete {
            state.shared_checkpoint_failed = false;
        }
        Ok(())
    }

    /// Retain `error` for exact `operation_id` and `phase`, pausing new dispatch until explicit recovery.
    /// 为精确 `operation_id` 及 `phase` 保留 `error`，暂停新分发直至显式恢复。
    pub(super) fn checkpoint_failed(
        &self,
        operation_id: &str,
        phase: OperationPhase,
        error: EmbeddedError,
    ) {
        if !self.operations.has_queued_history() {
            self.fail(error);
            return;
        }
        // This map is bounded by original live operation admission, not by retry count.
        // 此映射受原始活动操作入场约束，而非受重试次数影响。
        match self.lock() {
            Ok(mut state) => {
                state.persistence_failures.insert(
                    operation_id.to_owned(),
                    OperationPersistenceFailure {
                        operation_id: operation_id.to_owned(),
                        phase,
                        error,
                        retry: CheckpointRetryState::Waiting,
                    },
                );
                self.changed.notify_all();
            }
            Err(error) => self.fail(error),
        }
    }

    /// Clear a fault only after the exact retained checkpoint was acknowledged; wake paused dispatch.
    /// 仅在精确保留检查点得到确认后清除故障；唤醒暂停的分发。
    pub(super) fn checkpoint_recovered(&self, operation_id: &str) -> EmbeddedResult<()> {
        self.lock()?.persistence_failures.remove(operation_id);
        self.changed.notify_all();
        Ok(())
    }
}

impl EmbeddedRuntime {
    /// Query the currently observed checkpoint failure for retained `operation_id`, without reading disk.
    /// 查询保留 `operation_id` 当前已观测到的检查点故障，不读取磁盘。
    /// None means no failure is currently retained, not proof that an in-flight write has succeeded.
    /// 空表示当前没有保留故障，不证明在途写入已经成功。
    /// Busy reports an unavailable shared-gate observation immediately; it never waits for an execution-stage write.
    /// Busy 立即报告共享门禁观测暂不可用；绝不等待执行阶段写入。
    pub fn persistence_failure(
        &self,
        operation_id: &str,
    ) -> EmbeddedResult<Option<OperationPersistenceFailure>> {
        // Validate exact retained ownership before consulting either original checkpoint owner.
        // 查询任何原始检查点所有者前校验精确保留归属。
        let mut state = self.center.lock()?;
        if !state.operation_plugins.contains_key(operation_id) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::NotFound,
                "embedded operation is not retained",
            ));
        }
        if let Some(failure) = state.persistence_failures.get(operation_id) {
            return Ok(Some(failure.clone()));
        }
        // A directly observed live failure closes admission immediately, before the next maintenance pass.
        // 直接观测到活动故障时立即关闭入场，不等待下一次维护遍历。
        let failure = self
            .center
            .operations
            .get(operation_id)?
            .checkpoint_failure()?;
        if failure.is_some() {
            state.shared_checkpoint_failed = true;
        }
        Ok(failure)
    }

    /// Request one explicit checkpoint retry for `operation_id`; never rerun Lua or replace the business result.
    /// 为 `operation_id` 请求一次显式检查点重试；绝不重新运行 Lua 或替换业务结果。
    /// Return false when a retry is already requested or running; closing runtimes still permit persistence repair.
    /// 已请求或正在重试时返回假；关闭中的运行时仍允许持久化修复。
    pub fn retry_checkpoint(&self, operation_id: &str) -> EmbeddedResult<bool> {
        // A closing runtime retains the same recovery rights and original result ownership.
        // 关闭中的运行时保留相同恢复权限及原始结果所有权。
        let mut state = self.center.lock()?;
        if !state.operation_plugins.contains_key(operation_id) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::NotFound,
                "embedded operation is not retained",
            ));
        }
        if let Some(failure) = state.persistence_failures.get_mut(operation_id) {
            if failure.retry != CheckpointRetryState::Waiting {
                return Ok(false);
            }
            failure.retry = CheckpointRetryState::Requested;
            self.center.changed.notify_all();
            return Ok(true);
        }
        // Active returned handlers retain their own immutable confirmation; request only that exact retry.
        // 活动已返回处理器保留其自身不可变确认；仅请求该精确重试。
        self.center
            .operations
            .get(operation_id)?
            .request_checkpoint_retry()?
            .ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "operation has no failed checkpoint",
                )
            })
    }
}
