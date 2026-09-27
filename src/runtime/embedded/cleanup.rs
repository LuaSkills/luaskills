use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use serde::Serialize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Observable stages of one exact VM lifetime, independent of pool-wide usage.
/// 单个精确 VM 生命周期的可观察阶段，独立于整个池的用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleRetirementPhase {
    /// The instance remains leased or idle and has not entered retirement.
    /// 实例仍被租借或处于空闲状态，尚未进入退役。
    Live,
    /// Ownership has been transferred to the retirement queue.
    /// 所有权已转移至退役队列。
    Queued,
    /// The worker is executing cleanup or destroying the actual VM.
    /// 工作线程正在执行清理或销毁真实 VM。
    Running,
    /// Cleanup failed; the worker retains the VM and its capacity for a retry.
    /// 清理失败；工作线程保留 VM 及其容量以供重试。
    Retrying,
    /// VM destruction, capacity release and registration cleanup have returned; business finalizers may have failed.
    /// VM 销毁、容量释放及注册清理均已返回；业务终结器仍可能失败。
    Completed,
}

/// Bounded diagnostic snapshot; no application values or raw native diagnostics are retained.
/// 有界诊断快照；不保留应用值或原生诊断原文。
#[derive(Debug, Clone, Serialize)]
pub struct ModuleRetirementSnapshot {
    /// Immutable identity of the exact instance, never the pool name.
    /// 精确实例的不可变身份，绝不是池名。
    pub instance_id: String,
    /// Actual lifetime stage; a caller wait timeout cannot advance this value.
    /// 实际生命周期阶段；调用方等待超时不能推进此值。
    pub phase: ModuleRetirementPhase,
    /// Cleanup attempts, saturating at the representable maximum without identity reuse.
    /// 清理尝试次数，到达可表示最大值后饱和，且不复用身份。
    pub attempts: u64,
    /// Most recent failed attempt's code, preserved even after eventual success.
    /// 最近失败尝试的错误码，即使最终成功也保留。
    pub last_error_code: Option<EmbeddedErrorCode>,
}

/// Shared metadata outlives a destroyed VM without retaining its engine or dynamic libraries.
/// 共享元数据比已销毁 VM 存活更久，但不保留其引擎或动态库。
struct RetirementEvidence {
    /// Short metadata lock never held during native cleanup or VM destruction.
    /// 原生清理或 VM 销毁期间绝不持有的短时元数据锁。
    snapshot: Mutex<ModuleRetirementSnapshot>,
    /// Notification of lifecycle changes, paired only with the snapshot mutex.
    /// 生命周期变更通知，仅配合快照互斥锁使用。
    changed: Condvar,
}

/// Read-only completion receipt for one VM; cloning does not retain executable ownership.
/// 单个 VM 的只读完成回执；克隆不会保留可执行所有权。
#[derive(Clone)]
pub struct ModuleRetirement {
    /// Exact lifetime evidence shared with the sole retirement worker.
    /// 与唯一退役工作线程共享的精确生命周期证据。
    evidence: Arc<RetirementEvidence>,
}

impl ModuleRetirement {
    /// Construct live evidence for trusted `instance_id` before plugin initialization.
    /// 在插件初始化前，为可信 `instance_id` 构造活跃证据。
    pub(super) fn new(instance_id: String) -> Self {
        Self {
            evidence: Arc::new(RetirementEvidence {
                snapshot: Mutex::new(ModuleRetirementSnapshot {
                    instance_id,
                    phase: ModuleRetirementPhase::Live,
                    attempts: 0,
                    last_error_code: None,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    /// Return the current exact-instance snapshot, reporting poisoned metadata explicitly.
    /// 返回当前精确实例快照，明确报告元数据中毒。
    pub fn snapshot(&self) -> EmbeddedResult<ModuleRetirementSnapshot> {
        Ok(self.lock()?.clone())
    }

    /// Wait up to `timeout` for actual completion and return the current snapshot.
    /// 最多等待 `timeout` 以观察真实完成，并返回当前快照。
    /// Timeout never cancels cleanup, releases capacity or turns pending work into success.
    /// 超时绝不取消清理、释放容量或将待完成任务转换为成功。
    pub fn wait(&self, timeout: Duration) -> EmbeddedResult<ModuleRetirementSnapshot> {
        // This is only the observer's deadline, independent of the original operation budget.
        // 此处仅为观察者截止时间，独立于原始操作预算。
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            EmbeddedError::invalid("retirement wait deadline cannot be represented")
        })?;
        // Spurious notifications always consume the original remaining observer budget.
        // 虚假通知始终消耗原始观察者预算的剩余部分。
        let mut snapshot = self.lock()?;
        while snapshot.phase != ModuleRetirementPhase::Completed {
            // A timed-out observer receives pending evidence rather than an invented terminal error.
            // 超时观察者收到待完成证据，而不是编造的终态错误。
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            snapshot = self
                .evidence
                .changed
                .wait_timeout(snapshot, remaining)
                .map_err(|_| poisoned())?
                .0;
        }
        Ok(snapshot.clone())
    }

    /// Publish worker-owned `phase` and optional failed-attempt `error` after the actual transition.
    /// 在实际转换后发布工作线程拥有的 `phase` 与可选失败尝试 `error`。
    /// Metadata poisoning cannot discard actual ownership; readers still report the poison.
    /// 元数据中毒不能丢弃实际所有权；读取方仍报告中毒。
    pub(super) fn publish(&self, phase: ModuleRetirementPhase, error: Option<EmbeddedErrorCode>) {
        // Only retirement code can mutate evidence; external receipts cannot manufacture completion.
        // 仅退役代码可以修改证据；外部回执无法制造完成状态。
        let mut snapshot = self
            .evidence
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if phase == ModuleRetirementPhase::Running {
            snapshot.attempts = snapshot.attempts.saturating_add(1);
        }
        if let Some(error) = error {
            snapshot.last_error_code = Some(error);
        }
        snapshot.phase = phase;
        self.evidence.changed.notify_all();
    }

    /// Acquire the receipt's metadata without inferring progress after poisoning.
    /// 获取回执元数据，且不在中毒后推断进度。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, ModuleRetirementSnapshot>> {
        self.evidence.snapshot.lock().map_err(|_| poisoned())
    }
}

/// Return a fixed metadata failure that cannot include plugin-provided data.
/// 返回不可能包含插件数据的固定元数据错误。
fn poisoned() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "module retirement evidence lock is poisoned",
    )
}

/// Explicit ownership disposition after an ordinary call or session lease is released.
/// 普通调用或会话租借释放后的显式所有权去向。
pub enum ModuleRelease {
    /// Preparation reserved capacity but no actual VM was allocated; the reservation is released.
    /// 准备阶段预留了容量，但未分配真实 VM；预留已释放。
    NoInstance,
    /// Safe state has been returned to the pool; this request owns no pending retirement.
    /// 安全状态已归还池；此请求不再拥有待完成退役。
    ReturnedToPool,
    /// This exact instance must finish retirement before its owner can claim cleanup completion.
    /// 所有者必须等待此精确实例退役完成后，才能声称清理完成。
    Retiring(ModuleRetirement),
}

/// Failed acquisition plus evidence for an allocated VM that still requires real cleanup.
/// 获取失败以及仍需真实清理的已分配 VM 证据。
pub struct ModuleAcquireFailure {
    /// Original admission, allocation or initialization failure.
    /// 原始入场、分配或初始化错误。
    pub error: EmbeddedError,
    /// Present once actual VM ownership existed, including failed source initialization.
    /// 真实 VM 所有权建立后即存在，包含源码初始化失败。
    pub retirement: Option<ModuleRetirement>,
}

impl std::fmt::Debug for ModuleAcquireFailure {
    /// Format the original failure without acquiring mutable cleanup metadata.
    /// 格式化原始错误，且不获取可变清理元数据。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleAcquireFailure")
            .field("error", &self.error)
            .field("has_retirement", &self.retirement.is_some())
            .finish()
    }
}

impl std::fmt::Display for ModuleAcquireFailure {
    /// Format the original `error` into `formatter`, returning its formatting result.
    /// 将原始 `error` 格式化到 `formatter`，返回格式化结果。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ModuleAcquireFailure {
    /// Return the original failure as the standard error source without discarding the receipt.
    /// 将原始失败作为标准错误来源返回，且不丢弃回执。
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
