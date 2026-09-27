//! Bounded disk execution outside runtime control and scheduling locks.
//! 在运行时控制与调度锁之外执行有界磁盘工作。

#[cfg(test)]
mod tests;

use super::OperationJournal;
use crate::runtime::embedded::value_size::json_size;
use crate::runtime::embedded::{
    EmbeddedError, EmbeddedErrorCode, EmbeddedResult, OperationSnapshot,
};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Explicit budgets include queued, executing and caller-retained completed write receipts.
/// 显式预算包含排队、执行中及调用方仍保留的已完成写入回执。
#[derive(Debug, Clone, Copy)]
pub struct OperationJournalWorkerConfig {
    /// Maximum admitted write attempts until their last actual receipt owner releases them.
    /// 最后一个真实回执所有者释放之前，最多接纳的写入尝试数。
    pub max_pending_writes: usize,
    /// Cumulative JSON request bytes retained across all admitted write attempts.
    /// 所有已接纳写入尝试合计保留的 JSON 请求字节数。
    pub max_pending_bytes: usize,
}

/// Phase of the storage attempt, independent of plugin operation success or cancellation.
/// 存储尝试的阶段，独立于插件操作成功或取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalWritePhase {
    /// Owned by the worker queue and not yet submitted to SQLite.
    /// 由工作队列拥有，尚未提交给 SQLite。
    Queued,
    /// The one storage worker owns execution; observer timeout cannot cancel it.
    /// 唯一存储工作线程拥有执行；观测超时不能取消它。
    Writing,
    /// A durable revision or explicit failure has been retained; this does not itself mean success.
    /// 已保留持久修订号或明确失败；此阶段本身不表示成功。
    Completed,
}

/// Small observable write receipt; only completed successful attempts contain a revision.
/// 小型可观察写入回执；仅成功完成的尝试包含修订号。
#[derive(Debug, Clone)]
pub struct JournalWriteSnapshot {
    /// Actual queue/execution phase, never inferred from the caller's wait budget.
    /// 真实排队及执行阶段，绝不从调用方等待预算推断。
    pub phase: JournalWritePhase,
    /// Exact acknowledged stored revision, absent on failure or before completion.
    /// 精确确认的存储修订号；失败或完成前省略。
    pub revision: Option<u64>,
    /// Retained fixed diagnostic; unknown commit remains an error requiring reconciliation.
    /// 保留的固定诊断；未知提交仍为需要对账的错误。
    pub error: Option<EmbeddedError>,
}

/// Live worker observations; retained receipts keep quota even after the thread has finished.
/// 实时工作线程观测；线程结束后，保留的回执仍占有配额。
#[derive(Debug, Clone)]
pub struct OperationJournalWorkerStatus {
    /// Total owned attempts including caller-retained completed receipts.
    /// 拥有的尝试总数，包含调用方保留的已完成回执。
    pub pending_writes: usize,
    /// Encoded request bytes reserved until each attempt's last owner disappears.
    /// 每次尝试最后一个所有者消失前预留的请求编码字节数。
    pub pending_bytes: usize,
    /// Attempts still owned by the queue.
    /// 仍由队列拥有的尝试数。
    pub queued_writes: usize,
    /// Whether a real write is currently owned by the storage thread.
    /// 存储线程当前是否拥有真实写入。
    pub writing: bool,
    /// New attempts are permanently refused while admitted attempts finish normally.
    /// 永久拒绝新尝试，而已接纳尝试正常完成。
    pub closing: bool,
    /// Actual thread termination, observed from its join handle rather than a provisional flag.
    /// 从等待句柄观测到的真实线程终止，而非临时标记。
    pub worker_exited: bool,
    /// First infrastructure failure; individual database rejection remains on its own receipt.
    /// 首个基础设施故障；单独的数据库拒绝仍位于各自回执。
    pub failure: Option<EmbeddedError>,
}

/// Immutable bounded work passed to the storage thread; application snapshots are shared, not recopied.
/// 传给存储线程的不可变有界工作；共享应用快照，不再次复制。
#[derive(Serialize)]
struct WriteRequest {
    /// Original host runtime namespace.
    /// 原始宿主运行时命名空间。
    runtime_id: String,
    /// None means insert; a positive value requires exact compare-and-swap replacement.
    /// 空表示插入；正值要求精确比较交换替换。
    expected_revision: Option<u64>,
    /// Original immutable checkpoint kept alive through actual storage completion.
    /// 保留至真实存储完成的原始不可变检查点。
    #[serde(serialize_with = "serialize_snapshot")]
    snapshot: Arc<OperationSnapshot>,
}

/// Serialize the exact borrowed `snapshot` for byte accounting, without requiring serde's global rc feature.
/// 为字节计费序列化精确借用的 `snapshot`，不要求 serde 全局 rc 特性。
fn serialize_snapshot<S: serde::Serializer>(
    snapshot: &Arc<OperationSnapshot>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    snapshot.as_ref().serialize(serializer)
}

/// Shared receipt identity and publication state; dropping the last owner releases actual reserved quota.
/// 共享回执身份与发布状态；最后一个所有者释放时归还真实预留配额。
struct ReceiptState {
    /// Original request values provide the identity and charged byte ownership.
    /// 原始请求值提供身份及计费字节所有权。
    request: WriteRequest,
    /// Queue admission's exact byte charge.
    /// 队列入场的精确字节费用。
    bytes: usize,
    /// Weak accounting reference avoids a queue-to-receipt ownership cycle.
    /// 弱计费引用避免队列到回执的所有权引用环。
    center: Weak<WriterCenter>,
    /// Short local receipt publication lock; never held across SQLite I/O.
    /// 短期本地回执发布锁；绝不跨 SQLite I/O 持有。
    snapshot: Mutex<JournalWriteSnapshot>,
    /// Notify observers without transferring execution ownership to them.
    /// 通知观测者，不向其转移执行所有权。
    changed: Condvar,
}

impl ReceiptState {
    /// Publish `result` after storage returns; retain precise error instead of guessing a revision.
    /// 存储返回后发布 `result`；保留精确错误，不猜测修订号。
    fn complete(&self, result: EmbeddedResult<u64>) {
        // Owned evidence survives internal poisoning; public observation still reports the poisoned lock.
        // 已拥有证据在内部锁中毒后存活；公开观测仍报告锁中毒。
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A later supervisor failure must never replace an already acknowledged storage outcome.
        // 后续监督故障绝不能替换已确认的存储结果。
        if snapshot.phase == JournalWritePhase::Completed {
            return;
        }
        snapshot.phase = JournalWritePhase::Completed;
        match result {
            Ok(revision) => snapshot.revision = Some(revision),
            Err(error) => snapshot.error = Some(error),
        }
        self.changed.notify_all();
    }
}

impl Drop for ReceiptState {
    /// Release quota only when neither a client nor the actual queue/execution thread owns this attempt.
    /// 仅在客户端及真实队列或执行线程均不再拥有此尝试时释放配额。
    fn drop(&mut self) {
        if let Some(center) = self.center.upgrade() {
            // Receipt destruction performs only bounded accounting, never database work or thread joins.
            // 回执析构只执行有界计费，绝不操作数据库或等待线程。
            let mut state = center
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending_writes -= 1;
            state.pending_bytes -= self.bytes;
            center.changed.notify_all();
        }
    }
}

/// Cloneable observation of one original disk attempt; dropping observers never cancels admitted work.
/// 对单个原始磁盘尝试的可克隆观测；丢弃观测者绝不取消已接纳工作。
#[derive(Clone)]
pub struct JournalWriteReceipt {
    /// Shared original attempt remains owned by the worker through actual completion.
    /// 工作线程在真实完成之前保持拥有的共享原始尝试。
    state: Arc<ReceiptState>,
}

impl JournalWriteReceipt {
    /// Borrow the original runtime identity without reading disk or parsing opaque identifiers.
    /// 借用原始运行时身份，不读取磁盘或解析不透明标识符。
    pub fn runtime_id(&self) -> &str {
        &self.state.request.runtime_id
    }

    /// Borrow the exact operation identity whose checkpoint was admitted.
    /// 借用检查点已被接纳的精确操作身份。
    pub fn operation_id(&self) -> &str {
        &self.state.request.snapshot.operation_id
    }

    /// Return the original compare-and-swap revision; absence means this attempt was an insertion.
    /// 返回原始比较交换修订号；省略表示此尝试为插入。
    pub fn expected_revision(&self) -> Option<u64> {
        self.state.request.expected_revision
    }

    /// Return current local evidence without waiting for SQLite or releasing ownership.
    /// 返回当前本地证据，不等待 SQLite 或释放所有权。
    pub fn snapshot(&self) -> EmbeddedResult<JournalWriteSnapshot> {
        self.state
            .snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| poisoned())
    }

    /// Observe up to `timeout`, returning current state on expiration without cancelling the write.
    /// 最多观测 `timeout`，到期返回当前状态，不取消写入。
    pub fn wait(&self, timeout: Duration) -> EmbeddedResult<JournalWriteSnapshot> {
        // Check duration overflow before waiting instead of silently extending the caller's deadline.
        // 等待前检查时长溢出，不静默延长调用方截止时间。
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            EmbeddedError::invalid("journal write observation timeout is too large")
        })?;
        // Only the local receipt mutex is held; the worker owns all filesystem work independently.
        // 仅持有本地回执互斥锁；工作线程独立拥有全部文件系统工作。
        let mut snapshot = self.state.snapshot.lock().map_err(|_| poisoned())?;
        while snapshot.phase != JournalWritePhase::Completed {
            // A zero observation budget returns immediately and never changes the original attempt.
            // 零观测预算立即返回，绝不改变原始尝试。
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            snapshot = self
                .state
                .changed
                .wait_timeout(snapshot, remaining)
                .map_err(|_| poisoned())?
                .0;
        }
        Ok(snapshot.clone())
    }
}

/// Queue metadata; no serialization, disk access or user callback executes under this lock.
/// 队列元数据；此锁下不执行序列化、磁盘访问或用户回调。
struct WriterState {
    /// Permanent admission closure.
    /// 永久入场关闭。
    closing: bool,
    /// Attempts not yet handed to the single writer.
    /// 尚未交给唯一写入者的尝试。
    queued: VecDeque<Arc<ReceiptState>>,
    /// Actual writer ownership, retained for panic supervision.
    /// 为 panic 监督保留的真实写入者所有权。
    active: Option<Arc<ReceiptState>>,
    /// All actual receipt owners, including completed observers.
    /// 全部真实回执所有者，包含已完成观测者。
    pending_writes: usize,
    /// Total reserved serialized request bytes.
    /// 合计预留的序列化请求字节数。
    pending_bytes: usize,
    /// First infrastructure failure for explicit diagnostic inspection.
    /// 可供显式诊断检查的首个基础设施故障。
    failure: Option<EmbeddedError>,
}

/// Thread-owned control center independent of the public join-handle owner.
/// 由线程拥有的控制中心，独立于公开等待句柄所有者。
struct WriterCenter {
    /// Exact local database shared with historical readers.
    /// 与历史读取者共享的精确本地数据库。
    journal: Arc<OperationJournal>,
    /// Immutable parent bounds for all admitted write receipts.
    /// 所有已接纳写入回执的不可变父级上限。
    config: OperationJournalWorkerConfig,
    /// Sole queue metadata authority, never held while writing SQLite.
    /// 唯一队列元数据权威；写 SQLite 时绝不持有。
    state: Mutex<WriterState>,
    /// Wakes the one worker for new work or explicit close.
    /// 因新任务或显式关闭唤醒唯一工作线程。
    changed: Condvar,
    /// Test-only panic position; application input can never configure this hook.
    /// 仅测试使用的 panic 位置；应用输入绝不能配置此钩子。
    #[cfg(test)]
    fault: Mutex<Option<tests::WriteFault>>,
}

impl WriterCenter {
    /// Lock live metadata or expose poisoning without fabricating a successful worker state.
    /// 锁定实时元数据，或暴露锁中毒，不编造成功工作状态。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, WriterState>> {
        self.state.lock().map_err(|_| poisoned())
    }

    /// Stop admission and settle retained attempts after infrastructure `error`; never replay the active write.
    /// 基础设施 `error` 后停止入场并收束保留尝试；绝不重放活动写入。
    fn fail(&self, error: EmbeddedError) {
        // Move ownership out before dropping references whose destructor updates queue accounting.
        // 在丢弃会更新队列计费的引用前，先移出其所有权。
        let (active, queued) = {
            // Poison recovery preserves owned attempts instead of abandoning them in an inaccessible queue.
            // 中毒恢复保留已拥有尝试，而非将其遗弃在不可访问队列中。
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closing = true;
            state.failure.get_or_insert_with(|| error.clone());
            (state.active.take(), std::mem::take(&mut state.queued))
        };
        if let Some(active) = active {
            active.complete(Err(error.clone()));
        }
        for receipt in queued {
            receipt.complete(Err(error.clone()));
        }
        self.changed.notify_all();
    }

    /// Drain each owned write exactly once; return only after close and queue exhaustion.
    /// 对每次拥有的写入精确排空一次；仅在关闭且队列耗尽后返回。
    fn run(&self) -> EmbeddedResult<()> {
        loop {
            // Reserve real execution before releasing metadata ownership.
            // 释放元数据所有权前预留真实执行。
            let receipt = {
                // Waiting releases the metadata lock so admission, status and closure remain available.
                // 等待会释放元数据锁，使入场、状态及关闭保持可用。
                let mut state = self.lock()?;
                loop {
                    if let Some(receipt) = state.queued.pop_front() {
                        state.active = Some(Arc::clone(&receipt));
                        break receipt;
                    }
                    if state.closing {
                        return Ok(());
                    }
                    state = self.changed.wait(state).map_err(|_| poisoned())?;
                }
            };
            receipt.snapshot.lock().map_err(|_| poisoned())?.phase = JournalWritePhase::Writing;
            receipt.changed.notify_all();
            #[cfg(test)]
            tests::inject_fault(self, tests::WriteFault::BeforeWrite);
            // No writer or receipt metadata lock is held during the SQLite transaction.
            // SQLite 事务期间不持有写入者或回执元数据锁。
            let request = &receipt.request;
            // Record the exact acknowledged revision; failures remain attached to this original attempt.
            // 记录精确确认修订号；失败继续附着于此次原始尝试。
            let result = match request.expected_revision {
                None => self.journal.insert(&request.runtime_id, &request.snapshot),
                Some(revision) => {
                    self.journal
                        .replace(&request.runtime_id, revision, &request.snapshot)
                }
            }
            .map(|record| record.revision);
            receipt.complete(result);
            #[cfg(test)]
            tests::inject_fault(self, tests::WriteFault::AfterPublication);
            // The local receipt still owns the quota while the active slot is released.
            // 释放活动槽位时，本地回执仍拥有配额。
            let active = self.lock()?.active.take();
            drop(active);
            drop(receipt);
        }
    }
}

/// One fixed disk worker with bounded receipts and explicit close/join ownership.
/// 具有有界回执及显式关闭、等待所有权的单个固定磁盘工作线程。
/// The trusted host must observe closed before unloading the library; Drop only requests close.
/// 可信宿主必须在卸载库前观测已关闭；Drop 仅请求关闭。
pub struct OperationJournalWorker {
    /// Shared queue and storage owned independently by the actual running thread.
    /// 由真实运行线程独立拥有的共享队列及存储。
    center: Arc<WriterCenter>,
    /// Join ownership is serialized independently from disk or queue execution.
    /// 等待所有权独立于磁盘或队列执行进行串行化。
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl OperationJournalWorker {
    /// Start one worker for exact `journal` with explicit positive `config` limits.
    /// 使用显式正值 `config` 上限，为精确 `journal` 启动一个工作线程。
    pub fn new(
        journal: Arc<OperationJournal>,
        config: OperationJournalWorkerConfig,
    ) -> EmbeddedResult<Self> {
        if config.max_pending_writes == 0 || config.max_pending_bytes == 0 {
            return Err(EmbeddedError::invalid(
                "journal worker limits must be positive",
            ));
        }
        // The center has no join-handle reference, so its thread cannot form an ownership cycle.
        // 中心不引用等待句柄，因此其线程不能形成所有权引用环。
        let center = Arc::new(WriterCenter {
            journal,
            config,
            state: Mutex::new(WriterState {
                closing: false,
                queued: VecDeque::new(),
                active: None,
                pending_writes: 0,
                pending_bytes: 0,
                failure: None,
            }),
            changed: Condvar::new(),
            #[cfg(test)]
            fault: Mutex::new(None),
        });
        // The thread retains real work even if the public owner only requests shutdown and drops.
        // 即使公开所有者仅请求关闭后释放，线程仍保留真实工作。
        let worker_center = Arc::clone(&center);
        // Construction fails before any work can be admitted if thread creation is unavailable.
        // 线程创建不可用时，构造在任何工作入场之前失败。
        let worker = std::thread::Builder::new()
            .name("luaskills-journal".into())
            .spawn(move || {
                // Supervision retains active and queued receipts after an unexpected worker panic.
                // 监督在意外工作线程 panic 后保留活动及排队回执。
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker_center.run()));
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => worker_center.fail(error),
                    Err(_) => worker_center.fail(EmbeddedError::new(
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
            })?;
        Ok(Self {
            center,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Admit one immutable `snapshot` for exact `runtime_id` and optional compare-and-swap revision.
    /// 为精确 `runtime_id` 及可选比较交换修订号接纳一个不可变 `snapshot`。
    /// Return a local receipt immediately after bounded admission, without waiting for disk execution.
    /// 有界入场后立即返回本地回执，不等待磁盘执行。
    pub fn submit(
        &self,
        runtime_id: &str,
        expected_revision: Option<u64>,
        snapshot: Arc<OperationSnapshot>,
    ) -> EmbeddedResult<JournalWriteReceipt> {
        self.center
            .journal
            .validate_key(runtime_id, &snapshot.operation_id)?;
        if expected_revision.is_some_and(|revision| revision == 0 || revision >= i64::MAX as u64) {
            return Err(EmbeddedError::invalid(
                "journal write revision is invalid or exhausted",
            ));
        }
        // Serialize only into the counting sink before acquiring queue metadata.
        // 在获取队列元数据前，仅向计数接收器序列化。
        let request = WriteRequest {
            runtime_id: runtime_id.to_owned(),
            expected_revision,
            snapshot,
        };
        // Count the retained request, including identities and explicit revision presence.
        // 计数保留请求，包含身份及显式修订号存在性。
        let bytes = json_size(&request, self.center.config.max_pending_bytes)?;
        // Count and byte reservation publish atomically with actual queue ownership.
        // 数量与字节预留随真实队列所有权原子发布。
        let mut state = self.center.lock()?;
        if state.closing {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "journal worker is closing",
            ));
        }
        if state.pending_writes >= self.center.config.max_pending_writes
            || bytes
                > self
                    .center
                    .config
                    .max_pending_bytes
                    .saturating_sub(state.pending_bytes)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "journal worker receipt capacity reached",
            ));
        }
        state.pending_writes += 1;
        state.pending_bytes += bytes;
        // Both the caller and worker share one charged object; only its final destructor returns capacity.
        // 调用方及工作线程共享一个计费对象；仅最后一次析构归还容量。
        let receipt = Arc::new(ReceiptState {
            request,
            bytes,
            center: Arc::downgrade(&self.center),
            snapshot: Mutex::new(JournalWriteSnapshot {
                phase: JournalWritePhase::Queued,
                revision: None,
                error: None,
            }),
            changed: Condvar::new(),
        });
        state.queued.push_back(Arc::clone(&receipt));
        self.center.changed.notify_one();
        Ok(JournalWriteReceipt { state: receipt })
    }

    /// Stop admission without cancelling or duplicating previously accepted disk attempts.
    /// 停止入场，不取消或重复之前已接纳的磁盘尝试。
    pub fn request_close(&self) {
        // Closing must preserve owned work even if metadata was poisoned by an internal panic.
        // 即使元数据被内部 panic 毒化，关闭也必须保留已拥有工作。
        let mut state = self
            .center
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closing = true;
        self.center.changed.notify_all();
    }

    /// Return queue ownership and actual thread exit observations without waiting on disk.
    /// 返回队列所有权及真实线程退出观测，不等待磁盘。
    pub fn status(&self) -> EmbeddedResult<OperationJournalWorkerStatus> {
        // Join-handle then queue metadata is the sole order used by status and final closure.
        // 等待句柄后再取队列元数据，是状态与最终关闭共用的唯一顺序。
        let worker = self.worker.lock().map_err(|_| poisoned())?;
        // Receipt and queue bookkeeping are bounded and contain no filesystem calls.
        // 回执及队列记账有界，且不包含文件系统调用。
        let state = self.center.lock()?;
        Ok(OperationJournalWorkerStatus {
            pending_writes: state.pending_writes,
            pending_bytes: state.pending_bytes,
            queued_writes: state.queued.len(),
            writing: state.active.is_some(),
            closing: state.closing,
            worker_exited: worker.as_ref().is_none_or(JoinHandle::is_finished),
            failure: state.failure.clone(),
        })
    }

    /// Join only an actually finished thread; return true only after close and release of every receipt.
    /// 仅等待真实已结束的线程；仅在关闭且全部回执释放后返回真。
    /// Closure proves resource drainage, not success of the individual stored operations.
    /// 关闭证明资源排空，而非各个存储操作成功。
    pub fn poll_closed(&self) -> EmbeddedResult<bool> {
        // Keep join ownership unique even when several observers poll closure concurrently.
        // 即使多个观测者并发轮询关闭，也保持等待所有权唯一。
        let mut worker = self.worker.lock().map_err(|_| poisoned())?;
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Ok(false);
        }
        if let Some(worker) = worker.take()
            && worker.join().is_err()
        {
            // Retain failure before removing final thread ownership; draining and success are distinct.
            // 移除最终线程所有权前保留故障；排空和成功分别判断。
            self.center.fail(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "journal worker terminated outside its supervisor",
            ));
        }
        // Completed receipts still own native bookkeeping until their final actual reference is released.
        // 已完成回执在最后真实引用释放之前仍拥有原生记账。
        let state = self.center.lock()?;
        Ok(state.closing
            && state.pending_writes == 0
            && state.active.is_none()
            && state.queued.is_empty())
    }
}

impl Drop for OperationJournalWorker {
    /// Request drainage without blocking a host control thread in a destructor.
    /// 请求排空，不在析构中阻塞宿主控制线程。
    fn drop(&mut self) {
        self.request_close();
    }
}

/// Expose broken internal ownership explicitly instead of inventing an empty or completed queue.
/// 显式暴露损坏的内部所有权，不编造空队列或已完成队列。
fn poisoned() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "journal worker ownership lock is poisoned",
    )
}
