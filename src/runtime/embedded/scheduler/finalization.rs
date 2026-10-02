//! Same-VM closing continuations supervised independently from caller cancellation.
//! 独立于调用方取消进行监督的同 VM 关闭续行。

use super::*;

/// Exact VM and closing plan retained until its returned outcome is acknowledged.
/// 保留至返回结果确认的精确 VM 与关闭计划。
pub(super) struct PendingFinalization {
    /// Physical VM ownership retained across every checkpoint failure.
    /// 跨每个检查点故障保留的物理 VM 所有权。
    pub(super) lease: Box<ModuleLease>,
    /// Immutable declaration copied from this VM's module.
    /// 从此 VM 模块复制的不可变声明。
    pub(super) plan: ModuleFinalizer,
    /// Preparing intent is not repeatable even if its disk write fails.
    /// 即使磁盘写入失败，意图准备也不能重复。
    intent_prepared: bool,
    /// Actual returned result; none means execution has not returned.
    /// 实际返回结果；省略表示执行尚未返回。
    outcome: Option<EmbeddedResult<Value>>,
    /// Returned result was handed to the sole operation checkpoint owner.
    /// 已返回结果已经交给唯一操作检查点所有者。
    outcome_prepared: bool,
}

impl PendingFinalization {
    /// Retain the exact eligible lease and declaration without executing or allocating another VM.
    /// 保留精确符合条件的租借与声明，不执行，也不分配其他 VM。
    pub(super) fn new(lease: ModuleLease, plan: ModuleFinalizer) -> Self {
        Self {
            lease: Box::new(lease),
            plan,
            intent_prepared: false,
            outcome: None,
            outcome_prepared: false,
        }
    }
}

/// Supervisor decision; only Ready transfers execution to an existing worker.
/// 监督器决定；仅 Ready 将执行转交给既有工作线程。
pub(super) enum Progress {
    /// The original checkpoint or handler still owns unfinished work.
    /// 原检查点或处理器仍拥有未完成工作。
    Pending,
    /// Closing intent is acknowledged and this exact VM may be dispatched once.
    /// 关闭意图已经确认，此精确 VM 可以分发一次。
    Ready,
    /// Closing outcome is acknowledged and real resource retirement was scheduled.
    /// 关闭结果已经确认，且真实资源退役已经调度。
    Retiring,
}

/// Advance storage and ownership without running Lua or blocking the supervisor on disk.
/// 推进存储与所有权，不运行 Lua，也不让监督器阻塞于磁盘。
/// Return readiness, pending ownership, or actual retirement; retries only touch retained checkpoints.
/// 返回就绪、待完成所有权或真实退役；重试仅操作保留检查点。
/// center supervises completion's original checkpoints; retry authorizes one retained attempt, never a new closing execution.
/// center 监督 completion 原检查点；retry 授权一次保留尝试，绝不授权新的关闭执行。
pub(super) fn advance(
    center: &SchedulerCenter,
    completion: &mut PendingCompletion,
    retry: bool,
) -> EmbeddedResult<Progress> {
    // Consume recovery permission only for its retained checkpoint, so subsequent new checkpoints cannot auto-retry.
    // 仅为保留检查点消费恢复许可，避免后续新检查点自动重试。
    let mut retry_available = retry;
    // Each continuation consumes an acknowledged original checkpoint or sets one preparation flag exactly once.
    // 每次续行都消费已确认原检查点，或精确一次设置一个准备标记。
    // The exclusively owned predecessor, intent and outcome form a finite sequence; pending work always yields immediately.
    // 独占拥有的前驱、意图及结果构成有限序列；待完成工作始终立即让出。
    loop {
        // Re-read the real mutation gate even after preparation; successful preparation alone cannot prove disk acknowledgement.
        // 即使准备后也重读真实变更门；仅准备成功不能证明磁盘确认。
        let pending = match completion.call.owner.pending_phase() {
            Ok(phase) => phase,
            Err(error) if error.code == EmbeddedErrorCode::Busy => return Ok(Progress::Pending),
            Err(error) => return Err(error),
        };
        if let Some(phase) = pending {
            // Only this exact receipt may consume the one explicit recovery authorization.
            // 仅此精确回执可以消费单次明确恢复授权。
            let progress = if std::mem::take(&mut retry_available) {
                completion.call.owner.retry_advance()
            } else {
                completion.call.owner.poll_advance(phase)
            };
            match progress {
                Ok(true) => {
                    center.checkpoint_recovered(&completion.call.id)?;
                    continue;
                }
                Ok(false) => return Ok(Progress::Pending),
                Err(error) => {
                    center.checkpoint_failed(&completion.call.id, phase, error);
                    return Ok(Progress::Pending);
                }
            }
        }
        // An already-cleared predecessor cannot transfer its recovery authorization to a newly prepared checkpoint.
        // 已清除前驱不能把其恢复授权转交给新准备检查点。
        retry_available = false;
        // Borrow the sole original closing owner without constructing a second lease or control.
        // 借用唯一原关闭所有者，不构造第二个租借或控制。
        let closing = completion
            .finalization
            .as_mut()
            .expect("closing continuation exists");
        if !closing.intent_prepared {
            completion.call.owner.prepare_finalization(
                closing.plan.export.clone(),
                completion.result.clone(),
                Duration::from_millis(closing.plan.timeout_ms),
            )?;
            closing.intent_prepared = true;
            completion.cleaning_started = true;
            continue;
        }
        if closing.outcome.is_none() {
            return Ok(Progress::Ready);
        }
        if !closing.outcome_prepared {
            completion.call.owner.prepare_finalization_outcome(
                closing
                    .outcome
                    .as_ref()
                    .expect("returned closing outcome")
                    .clone(),
            )?;
            closing.outcome_prepared = true;
            continue;
        }
        // Take physical ownership only after the loop rechecked the acknowledged original outcome checkpoint.
        // 仅循环重新检查已确认原结果检查点之后，才取出物理所有权。
        let mut closing = completion
            .finalization
            .take()
            .expect("acknowledged closing owner");
        closing.lease.close();
        completion.retirement = match closing.lease.finish()? {
            ModuleRelease::Retiring(receipt) => Some(receipt),
            ModuleRelease::NoInstance => None,
            ModuleRelease::ReturnedToPool => {
                return Err(internal("finalized VM returned to reuse"));
            }
        };
        completion.call.request.release_values();
        return Ok(Progress::Retiring);
    }
}

/// Execute the already-acknowledged closing intent on one ordinary runtime worker.
/// 在一个普通运行时工作线程上执行已经确认的关闭意图。
/// Return the original completion with both business evidence and the actual closing outcome.
/// 返回保留原业务证据及实际关闭结果的原完成记录。
pub(super) fn execute(mut completion: PendingCompletion, maximum: usize) -> PendingCompletion {
    // The control is issued once here; cancellation of the original business control is independent.
    // 控制对象仅在此签发一次；原业务控制的取消相互独立。
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let control = completion.call.owner.take_finalization_control()?;
        let closing = completion
            .finalization
            .as_mut()
            .expect("worker owns closing VM");
        let context = match &completion.call.request {
            ScheduledRequest::Invoke(request) => &request.context,
            ScheduledRequest::CloseSession { context, .. }
            | ScheduledRequest::CloseInstance { context, .. } => context,
            _ => return Err(internal("unsupported automatic finalization request")),
        };
        closing.lease.finalize(ModuleInvocation {
            operation_id: &completion.call.id,
            session_id: completion.call.request.session_id(),
            export: &closing.plan.export,
            arguments: &closing.plan.arguments,
            context,
            control,
        })
    }));
    let mut result = match outcome {
        Ok(result) => result,
        Err(_) => Err(internal("embedded finalization panicked")),
    };
    let size = match &result {
        Ok(value) => json_size(value, maximum),
        Err(error) => json_size(error, maximum),
    };
    if let Err(error) = size {
        result = Err(error);
    }
    completion
        .finalization
        .as_mut()
        .expect("worker retains closing owner")
        .outcome = Some(result);
    completion
}
