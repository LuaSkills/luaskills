use super::HostEffectRecord;
use super::capabilities::CapabilityCaller;
use super::effects::{EffectAdmissionStage, EffectAttempt, EffectLedger};
use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Report poisoned identity ownership instead of silently treating it as untracked execution.
/// 报告中毒身份所有权，不静默将其视作未跟踪执行。
fn evidence_poisoned() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "operation evidence binding is poisoned",
    )
}

/// One cooperative cancellation flag and monotonic budget for an entire operation.
/// 单次完整操作的协作取消标记与单调时钟预算。
#[derive(Debug)]
pub struct CallControl {
    /// Registered evidence is attached once and survives cancellation and Lua errors.
    /// 注册证据仅附加一次，并在取消与 Lua 错误后存活。
    effects: Mutex<ControlEvidence>,
    /// Deadline created at admission, never renewed at a nested host call.
    /// 入场时创建的截止时间，嵌套宿主调用不得重新续期。
    deadline: Instant,
    /// Cancellation is a request, not proof that execution or effects stopped.
    /// 取消属于请求，不证明执行或副作用已经停止。
    cancelled: AtomicBool,
}

/// Evidence mode freezes atomically at registration or the first untracked capability call.
/// 证据模式在注册或首次未跟踪能力调用时原子冻结。
#[derive(Debug)]
enum ControlEvidence {
    /// Fresh control may still be admitted into an operation journal.
    /// 新控制对象仍可入场到操作日志。
    Fresh,
    /// Explicit low-level execution has begun and cannot be retroactively journaled.
    /// 显式低层执行已开始，不能追溯补记为完整日志。
    Untracked,
    /// All subsequent calls belong to this exact registered operation.
    /// 全部后续调用归属于此精确注册操作。
    Registered(Arc<EffectLedger>, EffectAdmissionStage),
}

impl CallControl {
    /// Return whether this control was issued for a registered closing stage.
    /// 返回此控制对象是否为已注册关闭阶段签发。
    pub(crate) fn is_finalization(&self) -> EmbeddedResult<bool> {
        let evidence = self.effects.lock().map_err(|_| evidence_poisoned())?;
        Ok(matches!(
            &*evidence,
            ControlEvidence::Registered(_, EffectAdmissionStage::Finalization)
        ))
    }

    /// Attach `ledger` once; reusing a control across registered operations is rejected.
    /// 仅附加一次 `ledger`；拒绝跨注册操作复用控制对象。
    pub(super) fn attach_effects(
        &self,
        ledger: Arc<EffectLedger>,
        stage: EffectAdmissionStage,
    ) -> EmbeddedResult<()> {
        // Registration cannot race past an untracked callback and lose its evidence.
        // 注册不能越过并发未跟踪回调并丢失其证据。
        let mut evidence = self.effects.lock().map_err(|_| evidence_poisoned())?;
        if !matches!(*evidence, ControlEvidence::Fresh) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation control is already registered or used",
            ));
        }
        *evidence = ControlEvidence::Registered(ledger, stage);
        Ok(())
    }

    /// Return the registered operation identity, absent for explicitly unregistered low-level execution.
    /// 返回注册操作身份，显式未注册低层执行省略。
    pub fn operation_id(&self) -> EmbeddedResult<Option<String>> {
        // Return owned identity so no control-state lock crosses module initialization.
        // 返回拥有所有权的身份，避免控制状态锁跨越模块初始化。
        let evidence = self.effects.lock().map_err(|_| evidence_poisoned())?;
        Ok(match &*evidence {
            ControlEvidence::Registered(ledger, _) => Some(ledger.operation_id().to_owned()),
            ControlEvidence::Fresh | ControlEvidence::Untracked => None,
        })
    }

    /// Clone the request correlation frozen by the original operation admission.
    /// 克隆原操作入场时冻结的请求关联。
    /// Returns no correlation for explicit untracked controls and reports poisoned ownership.
    /// 明确未跟踪控制返回无关联，并报告所有权中毒故障。
    pub(crate) fn request_id(&self) -> EmbeddedResult<Option<String>> {
        // Read the original ledger while holding its ownership lock.
        // 持有所有权锁期间读取原始账本。
        let evidence = self.effects.lock().map_err(|_| evidence_poisoned())?;
        Ok(match &*evidence {
            ControlEvidence::Registered(ledger, _) => ledger.request_id().map(str::to_owned),
            ControlEvidence::Fresh | ControlEvidence::Untracked => None,
        })
    }

    /// Read retained host evidence; none means no journal was attached, not proof of no effects.
    /// 读取保留宿主证据；省略表示未附加日志，不证明没有副作用。
    pub fn host_effects(&self) -> EmbeddedResult<Option<Vec<HostEffectRecord>>> {
        // Clone the journal without retaining the evidence-mode lock during its snapshot read.
        // 克隆日志，读取其快照时不保留证据模式锁。
        let ledger = {
            let evidence = self.effects.lock().map_err(|_| evidence_poisoned())?;
            match &*evidence {
                ControlEvidence::Registered(ledger, _) => Some(Arc::clone(ledger)),
                ControlEvidence::Fresh | ControlEvidence::Untracked => None,
            }
        };
        ledger.map(|ledger| ledger.snapshot()).transpose()
    }

    /// Reserve one exact capability's evidence under original trusted caller identity.
    /// 在原始可信调用方身份下预留单个精确能力的证据。
    pub(crate) fn reserve_effect(
        &self,
        caller: &CapabilityCaller,
        registration_id: &str,
        name: &str,
        version: &str,
    ) -> EmbeddedResult<EffectAttempt> {
        // Freeze either registered or explicitly untracked ownership before handler admission.
        // 在处理器入场前冻结已注册或显式未跟踪所有权。
        let ledger = {
            let mut evidence = self.effects.lock().map_err(|_| evidence_poisoned())?;
            match &*evidence {
                ControlEvidence::Registered(ledger, stage) => Some((Arc::clone(ledger), *stage)),
                ControlEvidence::Fresh | ControlEvidence::Untracked => {
                    *evidence = ControlEvidence::Untracked;
                    None
                }
            }
        };
        match ledger {
            Some((ledger, stage)) => ledger.prepare(stage, caller, registration_id, name, version),
            None => Ok(EffectAttempt::untracked()),
        }
    }

    /// Start a finite budget of `timeout`; reject zero or unrepresentable deadlines.
    /// 启动长度为 `timeout` 的有限预算；拒绝零或无法表示的截止时间。
    pub fn new(timeout: Duration) -> EmbeddedResult<Self> {
        if timeout.is_zero() {
            return Err(EmbeddedError::invalid("execution timeout must be positive"));
        }
        // Checked arithmetic rejects oversized public duration inputs before execution.
        // 在执行前用受检运算拒绝过大的公开时长输入。
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| EmbeddedError::invalid("execution deadline cannot be represented"))?;
        Ok(Self {
            effects: Mutex::new(ControlEvidence::Fresh),
            deadline,
            cancelled: AtomicBool::new(false),
        })
    }

    /// Request cancellation; return whether this call first set the request flag.
    /// 请求取消；返回本次调用是否首次设置请求标记。
    pub fn cancel(&self) -> bool {
        !self.cancelled.swap(true, Ordering::AcqRel)
    }

    /// Report cancellation intent without claiming that actual execution has stopped.
    /// 报告取消意图，不声称实际执行已经停止。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Check this operation's original budget and cooperative cancellation state.
    /// 检查当前操作的原始预算与协作取消状态。
    /// Return the corresponding structured failure, or success while still runnable.
    /// 返回相应的结构化错误，仍可执行时返回成功。
    pub fn check(&self) -> EmbeddedResult<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Cancelled,
                "operation cancellation requested",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::DeadlineExceeded,
                "operation deadline exceeded",
            ));
        }
        Ok(())
    }

    /// Return the original deadline for managed I/O propagation.
    /// 返回原始截止时间，供受管 I/O 传播使用。
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Return the remaining budget without granting a fresh nested timeout.
    /// 返回剩余预算，不为嵌套调用重新授予超时时间。
    pub fn remaining(&self) -> EmbeddedResult<Duration> {
        self.check()?;
        Ok(self.deadline.saturating_duration_since(Instant::now()))
    }
}
