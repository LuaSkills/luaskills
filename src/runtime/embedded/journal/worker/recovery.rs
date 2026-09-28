//! Explicit thread recovery preserves the original center, receipts, budgets and permanent host closure.
//! 显式线程恢复保留原中心、回执、预算及永久宿主关闭。

use super::*;
use std::thread::JoinHandle;

/// Unique thread ownership also remembers a failed join after its consumed handle is gone.
/// 唯一线程所有权在已消费句柄消失后仍记住等待失败。
pub(super) struct ThreadOwner {
    /// Actual current thread handle, absent only after an observed join.
    /// 当前实际线程句柄，仅在已观测等待后缺失。
    handle: Option<JoinHandle<()>>,
    /// A panic outside supervision permanently prevents reconstruction from potentially incomplete bookkeeping.
    /// 监督之外的 panic 永久阻止从可能不完整的记账重建。
    unproven_exit: bool,
}

impl ThreadOwner {
    /// Retain the newly started `handle` with no prior unproven exit.
    /// 保留新启动的 `handle`，此前没有未证实退出。
    pub(super) fn new(handle: JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
            unproven_exit: false,
        }
    }

    /// Return actual OS-thread termination evidence without waiting for its execution.
    /// 返回实际操作系统线程终止证据，不等待其执行。
    pub(super) fn exited(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Join only a finished thread; return false while alive and retain any unproven exit across future calls.
    /// 仅等待已完成线程；存活时返回假，并跨后续调用保留任何未证实退出。
    pub(super) fn join_finished(&mut self) -> EmbeddedResult<bool> {
        if !self.exited() {
            return Ok(false);
        }
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            self.unproven_exit = true;
        }
        if self.unproven_exit {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "journal worker terminated outside its supervisor; recovery is unavailable",
            ));
        }
        Ok(true)
    }
}

/// Start exactly one supervised thread for `center`; return its actual join owner or a construction error.
/// 为 `center` 启动恰好一个受监督线程；返回其实际等待所有者或构造错误。
pub(super) fn spawn(center: Arc<WriterCenter>) -> EmbeddedResult<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("luaskills-journal".into())
        .spawn(move || {
            // Both original construction and recovery use identical failure publication and retained ownership.
            // 原始构造及恢复使用相同的故障发布与保留所有权。
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| center.run()));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => center.fail(error),
                Err(_) => center.fail(EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    "journal worker panicked; reconcile the active write before retrying",
                )),
            }
        })
        .map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "journal worker creation failed",
            )
        })
}

impl OperationJournalWorker {
    /// Arm one test-only panic before storage or after its actual return as `after_storage` selects.
    /// 按 `after_storage` 选择，在存储之前或实际返回后设置一次仅测试 panic。
    #[cfg(test)]
    pub(crate) fn panic_next_write_for_test(&self, after_storage: bool) {
        *self.center.fault.lock().unwrap() = Some(if after_storage {
            tests::WriteFault::AfterWrite
        } else {
            tests::WriteFault::BeforeWrite
        });
    }

    /// Rebuild one failed, actually exited thread on this same writer; return false for a healthy running thread.
    /// 在同一写入者上重建一个已失败且实际退出的线程；健康运行线程返回假。
    /// Keep original completed receipts and their budgets; storage recovery and checkpoint retries remain separate host actions.
    /// 保留原完成回执及其预算；存储恢复与检查点重试仍为独立宿主操作。
    /// Explicit host closure, poisoned metadata or an unproven supervisor exit cannot be undone by this method.
    /// 此方法不能撤销显式宿主关闭、中毒元数据或未证实监督器退出。
    pub fn recover_worker(&self) -> EmbeddedResult<bool> {
        // Serialize recovery with status and join; this path performs no database I/O or business execution.
        // 使恢复与状态和等待串行；此路径不执行数据库 I/O 或业务。
        let mut worker = self.worker.lock().map_err(|_| poisoned())?;
        // Hold the metadata fence through thread publication so concurrent close cannot be overwritten.
        // 在线程发布全程持有元数据屏障，避免覆盖并发关闭。
        let mut state = self.center.lock()?;
        if state.close_requested {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "journal worker was explicitly closed",
            ));
        }
        if state.failure.is_none() {
            if worker.exited() {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    "journal worker exited without retained supervised failure",
                ));
            }
            return Ok(false);
        }
        if !worker.join_finished()? {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "journal worker has not actually exited",
            ));
        }
        if state.active.is_some() || !state.queued.is_empty() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "journal worker recovery requires supervised completion of every admitted attempt",
            ));
        }
        // Thread creation failure leaves the original failure and all quota reservations unchanged.
        // 线程创建失败使原故障及全部配额预留保持不变。
        let handle = spawn(Arc::clone(&self.center))?;
        worker.handle = Some(handle);
        state.failure = None;
        Ok(true)
    }
}
