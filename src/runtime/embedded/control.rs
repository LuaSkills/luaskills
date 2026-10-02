use super::HostEffectRecord;
use super::capabilities::CapabilityCaller;
use super::effects::{EffectAdmissionStage, EffectAttempt, EffectLedger};
use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Private measurement enablement set outside runtime ownership locks, never at control construction.
    /// 在运行时所有权锁外设置的私有测量开关，绝不在控制构造时设置。
    diagnostic_host_wait_enabled: AtomicBool,
    /// Fixed aggregate elapsed nanoseconds of actual host invocations, with saturating arithmetic.
    /// 使用饱和运算聚合实际宿主调用耗时纳秒的固定标量。
    diagnostic_host_wait_ns: AtomicU64,
    /// Fixed count of actual host invocation attempts, without per-call records.
    /// 实际宿主调用尝试次数的固定标量，不记录逐调用条目。
    diagnostic_host_wait_calls: AtomicU64,
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

/// Fixed host interval counters are sampled only outside the Lua execution stack.
/// 固定宿主区间计数仅在 Lua 执行栈之外采样。
#[derive(Clone, Copy, Default)]
pub(crate) struct HostWaitSnapshot {
    /// Sum of real nested host-call intervals, not a configured delay.
    /// 真实嵌套宿主调用区间总和，并非配置的延迟。
    pub(crate) elapsed_ns: u64,
    /// Actual entered host-call count.
    /// 实际进入的宿主调用次数。
    pub(crate) calls: u64,
}

impl HostWaitSnapshot {
    /// Return the monotonic delta from earlier, preserving fixed scalar storage.
    /// 返回相对 earlier 的单调差值，保持固定标量存储。
    pub(crate) fn since(self, earlier: Self) -> Self {
        Self {
            elapsed_ns: self.elapsed_ns.saturating_sub(earlier.elapsed_ns),
            calls: self.calls.saturating_sub(earlier.calls),
        }
    }
}

/// RAII records true host transport return or unwind without calling the logger in the Lua stack.
/// RAII 记录真实宿主传输返回或展开，不在 Lua 栈内调用日志器。
pub(crate) struct HostWaitMeasurement {
    /// Original operation control, containing only fixed diagnostic counters.
    /// 原操作控制，仅含固定诊断计数。
    control: Arc<CallControl>,
    /// Actual start immediately before the declared transport executes.
    /// 声明传输执行前的实际起点。
    started: Instant,
}

impl Drop for HostWaitMeasurement {
    /// Add elapsed time and one attempt even on errors; never dispatch a callback.
    /// 即使错误也增加耗时及一次尝试；绝不分发回调。
    fn drop(&mut self) {
        // The integer's representable domain is the sole saturation bound.
        // 整数可表示域是唯一饱和边界。
        let elapsed = self.started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        let _ = self.control.diagnostic_host_wait_ns.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |previous| Some(previous.saturating_add(elapsed)),
        );
        let _ = self.control.diagnostic_host_wait_calls.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |previous| Some(previous.saturating_add(1)),
        );
    }
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
    /// Enable or disable fixed counters from a lock-free phase boundary, without consulting the logger.
    /// 在无锁阶段边界启用或关闭固定计数，不查询日志器。
    pub(crate) fn enable_diagnostic_host_wait(&self, enabled: bool) {
        self.diagnostic_host_wait_enabled
            .store(enabled, Ordering::Relaxed);
    }

    /// Return a real transport timer only when the surrounding subscribed phase enabled measurement.
    /// 仅外围订阅阶段启用测量时返回真实传输计时器。
    pub(crate) fn measure_diagnostic_host_wait(self: &Arc<Self>) -> Option<HostWaitMeasurement> {
        self.diagnostic_host_wait_enabled
            .load(Ordering::Relaxed)
            .then(|| HostWaitMeasurement {
                control: Arc::clone(self),
                started: Instant::now(),
            })
    }

    /// Read fixed totals after Lua returns; no callbacks, logging locks or per-host-call allocations occur.
    /// 在 Lua 返回后读取固定总值；不执行回调、不获取日志锁，也不进行逐宿主调用分配。
    pub(crate) fn diagnostic_host_wait(&self) -> HostWaitSnapshot {
        HostWaitSnapshot {
            elapsed_ns: self.diagnostic_host_wait_ns.load(Ordering::Relaxed),
            calls: self.diagnostic_host_wait_calls.load(Ordering::Relaxed),
        }
    }
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
            diagnostic_host_wait_enabled: AtomicBool::new(false),
            diagnostic_host_wait_ns: AtomicU64::new(0),
            diagnostic_host_wait_calls: AtomicU64::new(0),
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
