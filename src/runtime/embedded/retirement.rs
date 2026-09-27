use super::pool::ResidentModule;
use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Shared maintenance cadence for bounded retirement retries.
/// 有界退役重试共享的维护间隔。
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
            let _ = worker.join();
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
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| module.module.close()));
        match result {
            Ok(Ok(())) => {
                drop(module);
                center
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .running = false;
            }
            _ => {
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
