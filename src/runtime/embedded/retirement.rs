use super::pool::ResidentModule;
use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult, ModuleRetirementPhase};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Shared maintenance cadence for bounded retirement retries and live control refresh.
/// 有界退役重试与实时控制刷新共享的维护间隔。
pub(super) const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(20);

/// Retirement metadata; resident tokens bound all retained entries.
/// 退役元数据；常驻令牌约束全部保留条目。
struct RetirementState {
    /// Exclusive modules awaiting another real cleanup attempt.
    /// 等待下一次真实清理尝试的独占模块。
    pending: VecDeque<ResidentModule>,
    /// Detached cleanup remains visible until VM destruction and slot release finish.
    /// 摘除的清理在 VM 销毁及槽位释放结束前仍可见。
    running: bool,
    /// Shutdown rejects no already-owned resources and exits only after draining.
    /// 关闭不拒绝已拥有资源，且仅在排空后退出。
    closing: bool,
}

/// Independent center kept alive by the worker without retaining its public owner.
/// 由工作线程保持存活且不保留公开所有者的独立中心。
struct RetirementCenter {
    /// Short queue lock never held while Lua or native finalizers run.
    /// Lua 或原生终结器运行时绝不持有的短时队列锁。
    state: Mutex<RetirementState>,
    /// New work and shutdown notification.
    /// 新任务与关闭通知。
    changed: Condvar,
}

/// One bounded background cleanup worker per parent pool manager.
/// 每个父池管理器拥有一个有界后台清理工作线程。
pub(super) struct RetirementService {
    /// Queue ownership independent from the manager and Lua engine.
    /// 独立于管理器与 Lua 引擎的队列所有权。
    center: Arc<RetirementCenter>,
    /// Retained thread identity; explicit drain is observed through resident accounting.
    /// 保留的线程身份；显式排空通过常驻记账观察。
    worker: Mutex<Option<JoinHandle<()>>>,
    /// A joined panic remains observable instead of becoming success on the next poll.
    /// 已等待退出的 panic 持续可见，不会在下一次轮询变成成功。
    failed: AtomicBool,
}

impl RetirementService {
    /// Start one worker; return a spawn error before accepting any resource ownership.
    /// 启动一个工作线程；在接纳任何资源所有权前返回启动错误。
    pub(super) fn new() -> EmbeddedResult<Self> {
        // The worker owns only this center, avoiding a manager-reference cycle.
        // 工作线程仅拥有此中心，避免管理器引用环。
        let center = Arc::new(RetirementCenter {
            state: Mutex::new(RetirementState {
                pending: VecDeque::new(),
                running: false,
                closing: false,
            }),
            changed: Condvar::new(),
        });
        // A separate reference survives public manager destruction until real cleanup finishes.
        // 独立引用在公开管理器销毁后存活，直到真实清理结束。
        let worker_center = Arc::clone(&center);
        // Thread count is constant regardless of how many modules await teardown.
        // 无论有多少模块等待清理，线程数量保持固定。
        let worker = std::thread::Builder::new()
            .name("luaskills-pool-retirement".into())
            .spawn(move || run_retirement(worker_center))
            .map_err(|error| {
                EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    format!("start pool retirement worker: {error}"),
                )
            })?;
        Ok(Self {
            center,
            worker: Mutex::new(Some(worker)),
            failed: AtomicBool::new(false),
        })
    }

    /// Transfer exclusive `module` ownership without running plugin destructors on this thread.
    /// 转移独占 `module` 所有权，且不在当前线程运行插件析构器。
    pub(super) fn enqueue(&self, mut module: ResidentModule) {
        if let Err(error) = module.reservation.mark_retiring() {
            crate::runtime_logging::error(format!("pool retirement accounting failed: {error}"));
        }
        module
            .retirement
            .publish(ModuleRetirementPhase::Queued, None);
        self.center
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .push_back(module);
        self.center.changed.notify_one();
    }

    /// Stop an already-drained service and report true only after its thread was joined.
    /// 停止已排空服务，且仅在线程已等待退出后返回 true。
    /// The manager calls this only after closing admission and observing zero resident tokens.
    /// 管理器仅在关闭入场且观察到零常驻令牌后调用此方法。
    pub(super) fn try_shutdown(&self) -> EmbeddedResult<bool> {
        if self.failed.load(Ordering::Acquire) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "pool retirement worker panicked",
            ));
        }
        {
            // The transient detached cleanup marker prevents interpreting an empty queue as drained.
            // 临时摘除清理标记防止将空队列误认为已排空。
            let mut state = self
                .center
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.running || !state.pending.is_empty() {
                return Ok(false);
            }
            state.closing = true;
        }
        self.center.changed.notify_one();
        // Joining is nonblocking because the thread must already have finished.
        // 线程必须已经结束，因此等待退出不会阻塞。
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Ok(false);
        }
        if let Some(worker) = worker.take() {
            worker.join().map_err(|_| {
                self.failed.store(true, Ordering::Release);
                EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    "pool retirement worker panicked",
                )
            })?;
        }
        Ok(true)
    }
}

impl Drop for RetirementService {
    /// Request shutdown while retaining failed cleanup on the existing worker.
    /// 请求关闭，同时在既有工作线程上保留失败清理。
    /// Native cleanup is cooperative; destruction never claims a still-running thread was killed.
    /// 原生清理为协作式；析构绝不声称仍运行的线程已被终止。
    fn drop(&mut self) {
        // Only idle shutdown can safely join without waiting on native plugin cleanup.
        // 仅空闲关闭可以安全等待线程退出，而不等待原生插件清理。
        let idle = {
            // No producer can survive the final manager owner.
            // 最后一个管理器所有者消失后，不再有存活的生产者。
            let mut state = self
                .center
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closing = true;
            !state.running && state.pending.is_empty()
        };
        self.center.changed.notify_one();
        // Active cleanup retains its own VM, engine and governor until it truly finishes.
        // 活动清理在真正结束前保留自身 VM、引擎与治理器。
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if (idle || worker.as_ref().is_some_and(JoinHandle::is_finished))
            && let Some(worker) = worker.take()
        {
            // An in-flight callback may release the final owner on this worker; its handle must detach instead of self-joining.
            // 进行中回调可能在此工作线程释放最后所有者；其句柄必须脱离而非自等待。
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

/// Drain `center` fairly, retaining each VM and capacity token after any cleanup failure.
/// 公平排空 `center`，在任何清理失败后保留对应 VM 与容量令牌。
fn run_retirement(center: Arc<RetirementCenter>) {
    loop {
        // Detach ownership under a short lock; its resident token remains charged.
        // 在短时锁下摘除所有权；其常驻令牌仍计入容量。
        let mut module = {
            // The same mutex always accompanies this condition variable.
            // 此条件变量始终配合同一个互斥锁。
            let mut state = center
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while state.pending.is_empty() && !state.closing {
                state = center
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            match state.pending.pop_front() {
                Some(module) => {
                    state.running = true;
                    module
                }
                None => return,
            }
        };
        // A panic cannot release the resident token or silently terminate the sole cleanup worker.
        // panic 不能释放常驻令牌，也不能静默终止唯一清理线程。
        module
            .retirement
            .publish(ModuleRetirementPhase::Running, None);
        // Query the original weak subscriber only after all retirement and receipt locks are released.
        // 仅在全部退役及回执锁释放后查询原弱订阅者。
        let diagnostics = module
            .diagnostics
            .take()
            .filter(|diagnostics| diagnostics.enabled());
        // Read the real live heap before close, never fabricating memory after the VM disappears.
        // 在关闭前读取真实存活堆，绝不在 VM 消失后虚构内存。
        let heap_before_close = diagnostics
            .as_ref()
            .map(|_| module.module.diagnostic_lua_heap_bytes());
        // Unsubscribed retirement starts no diagnostic clock and performs no diagnostic heap read.
        // 未订阅退役不启动诊断时钟，也不进行诊断堆读取。
        let close_started = diagnostics.as_ref().map(|_| Instant::now());
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| module.module.close()));
        // Freeze the actual close interval before receipt publication or diagnostic callback work.
        // 在回执发布或诊断回调工作前冻结实际关闭区间。
        let close_elapsed = close_started.map(|started| started.elapsed());
        match result {
            Ok(Ok(())) => {
                // Completion must follow actual VM destruction, not only resource close.
                // 完成必须晚于真实 VM 销毁，而不只是资源关闭。
                let retirement = module.retirement.clone();
                // Resident destruction includes real Lua GC, VM destruction and original capacity release.
                // 常驻对象销毁包含真实 Lua GC、VM 销毁及原容量释放。
                let destruction_started = diagnostics.as_ref().map(|_| Instant::now());
                drop(module);
                // Retain only scalar timing and identity metadata after actual destruction.
                // 实际销毁后仅保留标量计时及身份元数据。
                let destruction_elapsed = destruction_started.map(|started| started.elapsed());
                retirement.publish(ModuleRetirementPhase::Completed, None);
                center
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .running = false;
                if let Some(diagnostics) = diagnostics
                    && let (Some(close), Some(destruction), Some(heap)) =
                        (close_elapsed, destruction_elapsed, heap_before_close)
                {
                    diagnostics.retired(close, destruction, heap);
                }
            }
            failure => {
                // Preserve optional original observation identity alongside the exact failed resident owner.
                // 与精确失败常驻所有者一并保留可选原观测身份。
                module.diagnostics = diagnostics;
                // Preserve a bounded diagnostic while retaining failed resource ownership.
                // 保留有界诊断，同时保留失败资源所有权。
                let code = match failure {
                    Ok(Err(error)) => error.code,
                    Err(_) => EmbeddedErrorCode::Internal,
                    Ok(Ok(())) => unreachable!("successful retirement is handled above"),
                };
                module
                    .retirement
                    .publish(ModuleRetirementPhase::Retrying, Some(code));
                // Round-robin reinsertion lets independent failed owners make progress.
                // 轮转重新入队，使独立的失败所有者仍能推进。
                let mut state = center
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.pending.push_back(module);
                state.running = false;
                // Backoff remains bounded even during shutdown; resources never disappear on timeout.
                // 关闭期间退避仍有界；资源绝不因超时而消失。
                let _ = center
                    .changed
                    .wait_timeout(state, MAINTENANCE_INTERVAL)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Last-owner destruction on the actual owned worker must return without self-joining or retaining metadata.
    /// 实际所属工作线程上的最后所有者析构必须返回，不自等待且不保留元数据。
    /// No parameters or return value; the real thread and idle service reproduce the diagnostic ownership boundary.
    /// 无参数或返回值；真实线程及空闲服务复现诊断所有权边界。
    #[test]
    fn embedded_retirement_last_owner_on_own_thread_drops_without_self_join() {
        // Transfer the actual service only after its worker identity is available.
        // 仅在工作线程身份可用后转移实际服务。
        let (ownership_tx, ownership_rx) = std::sync::mpsc::sync_channel::<RetirementService>(0);
        // Report successful destruction independently from the handle that the service owns.
        // 独立于服务拥有的句柄报告成功析构。
        let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
        // The service owns precisely this live thread, rather than a guessed worker identity.
        // 服务精确拥有此存活线程，而非猜测的工作线程身份。
        let worker = std::thread::spawn(move || {
            // Receiving establishes the same last-owner destructor location as the in-flight callback.
            // 接收建立与进行中回调相同的最后所有者析构位置。
            let service = ownership_rx.recv().unwrap();
            // Observe an actual destructor panic without converting it into a passed cleanup result.
            // 观测实际析构 panic，不将其转换为清理通过结果。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(service)));
            finished_tx.send(result.is_ok()).unwrap();
        });
        // Empty, nonrunning state is exactly the original idle join branch.
        // 空且未运行的状态精确对应原空闲等待分支。
        let center = Arc::new(RetirementCenter {
            state: Mutex::new(RetirementState {
                pending: VecDeque::new(),
                running: false,
                closing: false,
            }),
            changed: Condvar::new(),
        });
        // The weak witness must expire after real service destruction, without a residual owner.
        // 真实服务析构后弱见证必须失效，不残留所有者。
        let witness = Arc::downgrade(&center);
        ownership_tx
            .send(RetirementService {
                center,
                worker: Mutex::new(Some(worker)),
                failed: AtomicBool::new(false),
            })
            .unwrap_or_else(|_| panic!("owned retirement worker stopped before service transfer"));
        assert!(
            finished_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("owned retirement thread must finish destruction without self-join"),
            "owned retirement thread must not panic while destroying its service"
        );
        assert!(witness.upgrade().is_none());
    }
}
