//! Retained host outcome checkpoints share phase ordering and explicit recovery.
//! 保留宿主结果检查点共享阶段排序及显式恢复。

use super::*;
use crate::runtime::embedded::HostEffectPhase;
use crate::runtime::embedded::retirement::MAINTENANCE_INTERVAL;

impl Operation {
    /// Submit or observe the returned result evidence of exact `effect_id`; never wait for disk or retry implicitly.
    /// 提交或观测精确 `effect_id` 的已返回结果证据；绝不等待磁盘或隐式重试。
    /// False preserves both the immutable candidate and the caller's actual result/admission ownership.
    /// 返回假时同时保留不可变候选及调用方真实结果和入场所有权。
    pub(in crate::runtime::embedded) fn poll_effect_outcome(
        &self,
        effect_id: &str,
    ) -> EmbeddedResult<bool> {
        // Persistent binding is fixed before control publication; absent history is not a fallback mode here.
        // 持久绑定在控制发布前固定；此处不存在历史不代表可回退模式。
        let history = self.history.as_ref().ok_or_else(poisoned)?;
        if !history.is_queued() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "persistent host completion requires a journal worker",
            ));
        }
        // Completion observers never wait for an execution-stage mutation that currently owns this gate.
        // 完成观察者绝不等待当前拥有此门禁的执行阶段变更。
        let mut transition = match self.transition.try_lock() {
            Ok(transition) => transition,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
        };
        if let Some(checkpoint) = transition.as_mut() {
            // Another live completion or an owner phase must be consumed by its original owner.
            // 另一活动完成或所有者阶段必须由其原始所有者消费。
            let same_effect = match &checkpoint.effect {
                Some(CheckpointEffect::Outcome(id)) if id == effect_id => true,
                Some(CheckpointEffect::Start(_)) => false,
                _ => return Ok(false),
            };
            // Failed storage remains observable on this exact candidate until an explicit retry request.
            // 在显式重试请求前，失败存储继续在此精确候选上可观测。
            match checkpoint.drive(history, false, false) {
                Ok(true) => {}
                Ok(false) | Err(_) => return Ok(false),
            }
            transition.take();
            if same_effect {
                return Ok(true);
            }
        }
        // Capture trusted live evidence only after acquiring the common mutation ordering gate.
        // 仅在取得公共变更排序门禁后捕获可信实时证据。
        let mut snapshot = self.lock()?.clone();
        if snapshot.phase.is_terminal() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "operation is terminal",
            ));
        }
        snapshot.cancellation_requested = self.control.is_cancelled();
        snapshot.host_effects = self.effects.snapshot()?;
        // The handler has returned, but its actual permit remains held until acknowledgement.
        // 处理器已返回，但其真实许可在确认前仍被持有。
        let record = snapshot
            .host_effects
            .iter()
            .find(|record| record.effect_id == effect_id)
            .ok_or_else(poisoned)?;
        if record.phase != HostEffectPhase::Running {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host outcome requires retained handler admission",
            ));
        }
        snapshot.effects = merge_effects(snapshot.effects, &snapshot.host_effects);
        // Known commit evidence belongs to this immutable candidate even if cancellation changes later.
        // 即使稍后取消变化，已知提交证据仍归属于此不可变候选。
        let mut checkpoint = PendingCheckpoint::new(Arc::new(snapshot));
        checkpoint.effect = Some(CheckpointEffect::Outcome(effect_id.to_owned()));
        *transition = Some(checkpoint);
        match transition
            .as_mut()
            .expect("retained host outcome")
            .drive(history, false, false)
        {
            Ok(true) => {
                transition.take();
                Ok(true)
            }
            // Storage rejection retains the original result on the actual callback stack or broker record.
            // 存储拒绝使原始结果保留于真实回调栈或代理记录。
            Ok(false) | Err(_) => Ok(false),
        }
    }

    /// Wait for one shared metadata notification or maintenance interval without surrendering native result ownership.
    /// 等待一次共享元数据通知或维护间隔，不放弃原生结果所有权。
    pub(in crate::runtime::embedded) fn wait_checkpoint_change(&self) -> EmbeddedResult<()> {
        // This condition variable always uses the same public snapshot mutex and releases it while waiting.
        // 此条件变量始终使用同一公开快照互斥锁，并在等待期间释放它。
        let snapshot = self.lock()?;
        drop(
            self.changed
                .wait_timeout(snapshot, MAINTENANCE_INTERVAL)
                .map_err(|_| poisoned())?,
        );
        Ok(())
    }
}

impl OperationHandle {
    /// Inspect this operation's currently retained shared checkpoint failure without waiting for disk or its gate.
    /// 检查此操作当前保留的共享检查点故障，不等待磁盘或其门禁。
    pub(crate) fn checkpoint_failure(&self) -> EmbeddedResult<Option<OperationPersistenceFailure>> {
        if self.operation.history.is_none() {
            return Ok(None);
        }
        // A busy gate reports an incomplete observation rather than pretending the last failure disappeared.
        // 门禁忙碌报告不完整观测，不伪装上次故障已经消失。
        let transition = self
            .operation
            .transition
            .try_lock()
            .map_err(|error| match error {
                TryLockError::WouldBlock => {
                    EmbeddedError::new(EmbeddedErrorCode::Busy, "checkpoint observation is busy")
                }
                TryLockError::Poisoned(_) => poisoned(),
            })?;
        Ok(transition.as_ref().and_then(|checkpoint| {
            checkpoint
                .failure
                .as_ref()
                .map(|error| OperationPersistenceFailure {
                    operation_id: self.operation.id.clone(),
                    phase: checkpoint.snapshot.phase,
                    error: error.clone(),
                    retry: checkpoint.retry,
                })
        }))
    }

    /// Request one retry on the exact shared failed candidate, returning None when no shared candidate exists.
    /// 为精确共享失败候选请求一次重试；不存在共享候选时返回空。
    /// This changes only retained intent; the actual owner submits and observes the same immutable write.
    /// 此方法仅改变保留意愿；真实所有者提交并观测同一不可变写入。
    pub(crate) fn request_checkpoint_retry(&self) -> EmbeddedResult<Option<bool>> {
        if self.operation.history.is_none() {
            return Ok(None);
        }
        // Never wait for a blocking stage owner while servicing a host control command.
        // 处理宿主控制命令期间绝不等待阻塞阶段所有者。
        let mut transition = self
            .operation
            .transition
            .try_lock()
            .map_err(|error| match error {
                TryLockError::WouldBlock => {
                    EmbeddedError::new(EmbeddedErrorCode::Busy, "checkpoint recovery is busy")
                }
                TryLockError::Poisoned(_) => poisoned(),
            })?;
        let Some(checkpoint) = transition.as_mut() else {
            return Ok(None);
        };
        if checkpoint.failure.is_none() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation checkpoint has not failed",
            ));
        }
        if matches!(checkpoint.attempt, Attempt::Unobservable { .. }) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "checkpoint receipt requires reconciliation before retry",
            ));
        }
        if checkpoint.retry != CheckpointRetryState::Waiting {
            return Ok(Some(false));
        }
        checkpoint.retry = CheckpointRetryState::Requested;
        self.operation.changed.notify_all();
        Ok(Some(true))
    }
}
