use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// One cooperative cancellation flag and monotonic budget for an entire operation.
/// 单次完整操作的协作取消标记与单调时钟预算。
#[derive(Debug)]
pub struct CallControl {
    /// Deadline created at admission, never renewed at a nested host call.
    /// 入场时创建的截止时间，嵌套宿主调用不得重新续期。
    deadline: Instant,
    /// Cancellation is a request, not proof that execution or effects stopped.
    /// 取消属于请求，不证明执行或副作用已经停止。
    cancelled: AtomicBool,
}

impl CallControl {
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
