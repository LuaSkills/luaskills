//! Real storage and deterministic ownership tests for the bounded writer.
//! 有界写入者的真实存储与确定性所有权测试。

use super::*;
use crate::runtime::embedded::{
    EffectState, OperationContext, OperationJournalConfig, OperationPhase,
};
use std::path::PathBuf;
use std::sync::mpsc;

/// One deadline for all bounded test observations, independent of production timing.
/// 全部有界测试观测共用的截止时长，独立于生产时序。
const OBSERVE: Duration = Duration::from_secs(8);

/// Private fault locations exercise supervision without exposing application-controlled hooks.
/// 私有故障位置验证监督，不暴露应用可控钩子。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteFault {
    /// Fail after dispatch but before touching storage.
    /// 分发后、接触存储前失败。
    BeforeWrite,
    /// Fail after publishing the acknowledged revision but before releasing active ownership.
    /// 发布确认修订号后、释放活动所有权前失败。
    AfterPublication,
}

/// Panic at `position` only when armed on this exact `center`; release the hook lock first.
/// 仅在此精确 `center` 已设置故障时于 `position` 触发 panic；先释放钩子锁。
pub(super) fn inject_fault(center: &WriterCenter, position: WriteFault) {
    // Consume the selected hook before unwinding so it cannot poison its own configuration.
    // 展开前消耗选中的钩子，避免毒化其配置。
    let armed = {
        // Per-worker state prevents interference between independent tests.
        // 逐工作线程状态防止独立测试互相干扰。
        let mut fault = center.fault.lock().unwrap();
        if *fault == Some(position) {
            fault.take();
            true
        } else {
            false
        }
    };
    assert!(!armed, "injected journal worker failure");
}

/// Private directory whose lifetime encloses all actual journal and worker owners.
/// 生命周期包围所有真实日志与工作线程所有者的私有目录。
struct Directory(PathBuf);

impl Directory {
    /// Create a collision-resistant temporary root, returning its sole cleanup owner.
    /// 创建抗碰撞临时根目录，返回唯一清理所有者。
    fn new() -> Self {
        // Independent entropy avoids process-global counters or shared fixture paths.
        // 独立熵避免进程全局计数器或共享夹具路径。
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).unwrap();
        // Hexadecimal names remain portable across supported filesystems.
        // 十六进制名称可跨支持的文件系统使用。
        let name = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // The exact created directory alone is removed by this fixture.
        // 此夹具只删除实际创建的精确目录。
        let directory = std::env::temp_dir().join(format!("luaskills-writer-{name}"));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    /// Open this fixture's database with enough disk capacity to isolate writer admission tests.
    /// 以充足磁盘容量打开此夹具数据库，隔离验证写入者入场行为。
    fn journal(&self) -> Arc<OperationJournal> {
        Arc::new(
            OperationJournal::open(
                &self.0.join("operations.db"),
                OperationJournalConfig {
                    max_records: 16,
                    max_record_bytes: 16 * 1024,
                    max_database_bytes: 128 * 1024,
                },
            )
            .unwrap(),
        )
    }
}

impl Drop for Directory {
    /// Remove only the test-owned directory after its database handles have closed.
    /// 数据库句柄关闭后只移除此测试拥有的目录。
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Return one immutable unresolved checkpoint identified by `id`.
/// 返回一个由 `id` 标识的不可变未决检查点。
fn snapshot(id: &str) -> Arc<OperationSnapshot> {
    Arc::new(OperationSnapshot {
        context: OperationContext::Unbound,
        operation_id: id.into(),
        phase: OperationPhase::Running,
        cancellation_requested: false,
        effects: EffectState::Unknown,
        value: None,
        error: None,
        host_effects: Vec::new(),
    })
}

/// Build a writer with caller-selected count capacity and a non-binding byte allowance.
/// 创建数量容量由调用方指定且字节预算宽裕的写入者。
fn writer(journal: &Arc<OperationJournal>, count: usize) -> OperationJournalWorker {
    OperationJournalWorker::new(
        Arc::clone(journal),
        OperationJournalWorkerConfig {
            max_pending_writes: count,
            max_pending_bytes: 64 * 1024,
        },
    )
    .unwrap()
}

/// Wait for an observed `condition`, failing with `reason` instead of assuming scheduling from sleep.
/// 等待观测到 `condition`；失败时报告 `reason`，不从睡眠推测调度进度。
fn until(mut condition: impl FnMut() -> bool, reason: &str) {
    // The deadline bounds a broken implementation without imposing a production timeout.
    // 截止时间约束错误实现，不引入生产超时。
    let deadline = Instant::now() + OBSERVE;
    while !condition() {
        assert!(Instant::now() < deadline, "{reason}");
        std::thread::yield_now();
    }
}

/// Observe an actual successful receipt and return its acknowledged revision.
/// 观测真实成功回执并返回其确认修订号。
fn revision(receipt: &JournalWriteReceipt) -> u64 {
    // Waiting observes completion; it never transfers or cancels storage ownership.
    // 等待观测完成；绝不转移或取消存储所有权。
    let result = receipt.wait(OBSERVE).unwrap();
    assert_eq!(result.phase, JournalWritePhase::Completed);
    assert!(
        result.error.is_none(),
        "unexpected write error: {:?}",
        result.error
    );
    result.revision.unwrap()
}

/// Close and join a fully released writer; tests must drop their retained receipts first.
/// 关闭并等待已完全释放的写入者；测试必须先释放保留回执。
fn close(writer: &OperationJournalWorker) {
    writer.request_close();
    until(
        || writer.poll_closed().unwrap(),
        "writer did not drain its actual owners",
    );
}

/// Real inserts and revision conflicts keep their own outcomes without stopping unrelated writes.
/// 真实插入与修订冲突保留各自结果，不停止无关写入。
#[test]
fn embedded_journal_worker_persists_and_isolates_storage_rejections() {
    // Retain the fixture outside database and worker lifetimes.
    // 在数据库及工作线程生命周期外保留夹具。
    let directory = Directory::new();
    // Both the reader and writer address the same real SQLite file.
    // 读取者和写入者访问同一个真实 SQLite 文件。
    let journal = directory.journal();
    // Capacity covers all receipts intentionally retained by this test.
    // 容量覆盖本测试故意保留的全部回执。
    let worker = writer(&journal, 5);
    // Original insertion establishes revision one.
    // 原始插入建立修订一。
    let first = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    assert_eq!(first.runtime_id(), "runtime");
    assert_eq!(first.operation_id(), "operation");
    assert_eq!(first.expected_revision(), None);
    assert_eq!(revision(&first), 1);
    // Duplicate insertion must report the existing identity rather than overwrite it.
    // 重复插入必须报告既有身份，而非覆盖它。
    let duplicate = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    assert_eq!(
        duplicate.wait(OBSERVE).unwrap().error.unwrap().code,
        EmbeddedErrorCode::AlreadyCompleted
    );
    // The next exact revision succeeds on the same worker.
    // 下一精确修订在同一个工作线程上成功。
    let second = worker
        .submit("runtime", Some(1), snapshot("operation"))
        .unwrap();
    assert_eq!(second.expected_revision(), Some(1));
    assert_eq!(revision(&second), 2);
    // An obsolete revision cannot erase the new checkpoint.
    // 过期修订不能抹除新检查点。
    let stale = worker
        .submit("runtime", Some(1), snapshot("operation"))
        .unwrap();
    assert_eq!(
        stale.wait(OBSERVE).unwrap().error.unwrap().code,
        EmbeddedErrorCode::StaleGeneration
    );
    // A distinct original identity continues after both expected storage rejections.
    // 不同的原始身份在两次预期存储拒绝后继续完成。
    let other = worker.submit("runtime", None, snapshot("other")).unwrap();
    assert_eq!(revision(&other), 1);
    assert!(worker.status().unwrap().failure.is_none());
    assert_eq!(
        journal
            .get("runtime", "operation")
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    drop((first, duplicate, second, stale, other));
    close(&worker);
    drop(worker);
    drop(journal);
    // Reopening proves evidence outlives the thread and original database object.
    // 重新打开证明证据寿命超过线程及原数据库对象。
    let reopened = directory.journal();
    assert_eq!(
        reopened
            .get("runtime", "operation")
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        reopened.get("runtime", "other").unwrap().unwrap().revision,
        1
    );
}

/// Disk blockage cannot block observation/closure or let abandoned receipts free live write capacity.
/// 磁盘阻塞不能阻塞观测与关闭，也不能让放弃的回执释放真实写入容量。
#[test]
fn embedded_journal_worker_blocked_disk_preserves_control_and_ownership() {
    // Each test owns an independent file and worker.
    // 每个测试拥有独立文件和工作线程。
    let directory = Directory::new();
    // Hold the actual storage mutex rather than simulate latency with sleep.
    // 持有真实存储互斥锁，而非用睡眠模拟延迟。
    let journal = directory.journal();
    // Two slots allow one executing and one queued attempt.
    // 两个槽位允许一个执行中尝试和一个排队尝试。
    let worker = Arc::new(writer(&journal, 2));
    // The real transaction cannot enter SQLite until this guard is released.
    // 此守卫释放前，真实事务不能进入 SQLite。
    let gate = journal.block_for_test();
    // Observe Writing before exercising concurrent control APIs.
    // 先观测写入阶段，再验证并发控制接口。
    let first = worker.submit("runtime", None, snapshot("first")).unwrap();
    until(
        || first.snapshot().unwrap().phase == JournalWritePhase::Writing,
        "writer did not claim the first attempt",
    );
    // The second attempt remains owned by the bounded queue.
    // 第二个尝试继续由有界队列拥有。
    let second = worker.submit("runtime", None, snapshot("second")).unwrap();
    assert_eq!(second.snapshot().unwrap().phase, JournalWritePhase::Queued);
    assert_eq!(
        first.wait(Duration::from_millis(1)).unwrap().phase,
        JournalWritePhase::Writing
    );
    assert_eq!(
        second.wait(Duration::ZERO).unwrap().phase,
        JournalWritePhase::Queued
    );
    drop((first, second));
    assert_eq!(worker.status().unwrap().pending_writes, 2);
    assert_eq!(
        worker
            .submit("runtime", None, snapshot("third"))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    // This observer must complete before the main thread releases the actual storage gate.
    // 此观测者必须在主线程释放真实存储门禁之前完成。
    let observer = Arc::clone(&worker);
    // Channel timeout produces a bounded failure even if a regression holds control behind disk I/O.
    // 即使回退导致控制被磁盘 I/O 阻塞，通道超时仍产生有界失败。
    let (send, receive) = mpsc::channel();
    // Preserve native thread ownership for an explicit join after opening the gate.
    // 保留原生线程所有权，以便打开门禁后显式等待。
    let control = std::thread::spawn(move || {
        observer.request_close();
        send.send((observer.status(), observer.poll_closed()))
            .unwrap();
    });
    // Capture the pre-release result before allowing storage progress.
    // 在允许存储推进之前捕获释放前结果。
    let observed = receive.recv_timeout(OBSERVE);
    drop(gate);
    control.join().unwrap();
    // Both control operations were already done while the real storage mutex was unavailable.
    // 两个控制操作都已在真实存储互斥锁不可用时完成。
    let (status, closed) = observed.expect("disk I/O blocked journal control");
    assert!(!closed.unwrap());
    assert!(status.as_ref().unwrap().closing);
    assert!(status.as_ref().unwrap().writing);
    assert!(!status.unwrap().worker_exited);
    assert_eq!(
        worker
            .submit("runtime", None, snapshot("third"))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    close(&worker);
    assert_eq!(
        journal.get("runtime", "first").unwrap().unwrap().revision,
        1
    );
    assert_eq!(
        journal.get("runtime", "second").unwrap().unwrap().revision,
        1
    );
    assert!(journal.get("runtime", "third").unwrap().is_none());
}

/// Last actual receipt ownership, including clones, bounds retention even after thread exit.
/// 包含克隆在内的最后真实回执所有权，在工作线程退出后仍约束保留量。
#[test]
fn embedded_journal_worker_completed_receipts_retain_capacity() {
    // Isolated real storage backs this lifetime test.
    // 独立真实存储支撑此生命周期测试。
    let directory = Directory::new();
    // Keep storage alive independently of writer completion.
    // 独立于写入者完成而保持存储存活。
    let journal = directory.journal();
    // One slot makes premature capacity return immediately observable.
    // 单槽使提前归还容量立即可见。
    let worker = writer(&journal, 1);
    // The successful observer remains live after disk acknowledgement.
    // 成功观测者在磁盘确认后继续存活。
    let receipt = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    assert_eq!(revision(&receipt), 1);
    // Cloning must not double-charge or allow the original drop to release shared ownership.
    // 克隆不得重复计费，也不能让原对象释放时归还共享所有权。
    let retained = receipt.clone();
    drop(receipt);
    assert_eq!(worker.status().unwrap().pending_writes, 1);
    assert_eq!(
        worker
            .submit("runtime", None, snapshot("other"))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    worker.request_close();
    until(
        || worker.status().unwrap().worker_exited,
        "worker did not exit after draining writes",
    );
    assert!(!worker.poll_closed().unwrap());
    assert_eq!(revision(&retained), 1);
    drop(retained);
    assert!(worker.poll_closed().unwrap());
    assert_eq!(worker.status().unwrap().pending_bytes, 0);
}

/// Exact serialized admission bytes bound queued and retained requests without assuming heap size.
/// 精确序列化入场字节约束排队与保留请求，不假定堆大小。
#[test]
fn embedded_journal_worker_enforces_total_byte_budget() {
    // All attempts use a real database while byte rejection occurs before dispatch.
    // 全部尝试使用真实数据库，而字节拒绝在分发之前发生。
    let directory = Directory::new();
    // The journal's separate record limit is intentionally larger than these requests.
    // 日志独立的记录上限故意大于这些请求。
    let journal = directory.journal();
    // Equal-length identities produce equal charges, including the request envelope.
    // 等长身份产生相同费用，包含请求封装。
    let first_snapshot = snapshot("a");
    // Calculate the actual serialized byte representation rather than duplicating a size formula.
    // 计算实际序列化字节表示，不复制大小公式。
    let bytes = serde_json::to_vec(&WriteRequest {
        reconcile: false,
        runtime_id: "runtime".into(),
        expected_revision: None,
        snapshot: Arc::clone(&first_snapshot),
    })
    .unwrap()
    .len();
    // Exactly two requests fit by bytes while the count limit would allow three.
    // 字节恰好容纳两个请求，而数量限制允许三个。
    let worker = OperationJournalWorker::new(
        Arc::clone(&journal),
        OperationJournalWorkerConfig {
            max_pending_writes: 3,
            max_pending_bytes: bytes * 2,
        },
    )
    .unwrap();
    // These completed receipts intentionally retain their original request charge.
    // 这些已完成回执故意保留原始请求费用。
    let first = worker.submit("runtime", None, first_snapshot).unwrap();
    // The second request reaches the exact inclusive byte bound.
    // 第二个请求达到包含边界值的精确字节上限。
    let second = worker.submit("runtime", None, snapshot("b")).unwrap();
    assert_eq!(revision(&first), 1);
    assert_eq!(revision(&second), 1);
    assert_eq!(worker.status().unwrap().pending_bytes, bytes * 2);
    assert_eq!(
        worker
            .submit("runtime", None, snapshot("c"))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    drop(first);
    until(
        || worker.status().unwrap().pending_writes == 1,
        "completed receipt capacity was not released",
    );
    // Reusing released quota creates a distinct record without changing the retained one.
    // 重用已释放配额创建不同记录，不改变保留记录。
    let third = worker.submit("runtime", None, snapshot("c")).unwrap();
    assert_eq!(revision(&third), 1);
    drop((second, third));
    close(&worker);
    assert_eq!(worker.status().unwrap().pending_bytes, 0);
}

/// Invalid limits, revisions and oversized requests never enter the storage queue.
/// 非法上限、修订号和超大请求绝不进入存储队列。
#[test]
fn embedded_journal_worker_rejects_invalid_admission() {
    // Keep one real database for all rejected admissions.
    // 对全部被拒绝入场保留同一个真实数据库。
    let directory = Directory::new();
    // Rejected configurations must not claim journal ownership through a live worker.
    // 被拒绝的配置不能通过存活工作线程占有日志。
    let journal = directory.journal();
    for config in [
        OperationJournalWorkerConfig {
            max_pending_writes: 0,
            max_pending_bytes: 1,
        },
        OperationJournalWorkerConfig {
            max_pending_writes: 1,
            max_pending_bytes: 0,
        },
    ] {
        assert_eq!(
            OperationJournalWorker::new(Arc::clone(&journal), config)
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
    }
    // A one-byte limit rejects even the request envelope before touching SQLite.
    // 单字节上限会在接触 SQLite 前拒绝请求封装。
    let worker = OperationJournalWorker::new(
        Arc::clone(&journal),
        OperationJournalWorkerConfig {
            max_pending_writes: 1,
            max_pending_bytes: 1,
        },
    )
    .unwrap();
    for revision in [0, i64::MAX as u64, u64::MAX] {
        assert_eq!(
            worker
                .submit("runtime", Some(revision), snapshot("operation"))
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        worker
            .submit("runtime", None, snapshot("operation"))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(worker.status().unwrap().pending_writes, 0);
    assert_eq!(worker.status().unwrap().pending_bytes, 0);
    assert!(journal.next(None).unwrap().is_none());
    close(&worker);
}

/// Supervision preserves an acknowledged revision while failing still-queued writes explicitly.
/// 监督保留已确认修订，同时明确报告仍在排队的写入失败。
#[test]
fn embedded_journal_worker_panic_preserves_published_evidence() {
    // Both the committed and unattempted records are inspected in the real database.
    // 在真实数据库内检查已提交及未尝试记录。
    let directory = Directory::new();
    // The storage gate allows deterministic queuing before the injected worker failure.
    // 存储门禁允许在注入工作线程故障前确定性排队。
    let journal = directory.journal();
    // Retained receipts must survive supervision instead of disappearing with the failed thread.
    // 保留回执必须在监督后存活，不能随失败线程消失。
    let worker = writer(&journal, 2);
    *worker.center.fault.lock().unwrap() = Some(WriteFault::AfterPublication);
    // Block only disk entry; worker metadata stays available.
    // 仅阻塞磁盘入口；工作线程元数据保持可用。
    let gate = journal.block_for_test();
    // The first write will succeed before the post-publication panic.
    // 首次写入将在发布后 panic 之前成功。
    let committed = worker
        .submit("runtime", None, snapshot("committed"))
        .unwrap();
    until(
        || committed.snapshot().unwrap().phase == JournalWritePhase::Writing,
        "first attempt was not dispatched",
    );
    // The second write must be failed by supervision without entering storage.
    // 第二次写入必须被监督标记失败，不能进入存储。
    let queued = worker.submit("runtime", None, snapshot("queued")).unwrap();
    drop(gate);
    until(
        || worker.status().unwrap().worker_exited,
        "panicked worker did not terminate",
    );
    assert_eq!(revision(&committed), 1);
    // Completion after a worker failure is an explicit error, never a successful revision.
    // 工作线程故障后的完成是明确错误，绝非成功修订。
    let rejected = queued.wait(OBSERVE).unwrap();
    assert_eq!(rejected.phase, JournalWritePhase::Completed);
    assert!(rejected.revision.is_none());
    assert_eq!(rejected.error.unwrap().code, EmbeddedErrorCode::Internal);
    assert_eq!(
        worker.status().unwrap().failure.unwrap().code,
        EmbeddedErrorCode::Internal
    );
    assert!(!worker.poll_closed().unwrap());
    assert!(journal.get("runtime", "committed").unwrap().is_some());
    assert!(journal.get("runtime", "queued").unwrap().is_none());
    drop((committed, queued));
    assert!(worker.poll_closed().unwrap());
    assert!(worker.status().unwrap().failure.is_some());
}

/// An active attempt receives explicit uncertainty when its thread fails before storage acknowledgement.
/// 活动尝试的线程在存储确认前失败时，收到明确的不确定结果。
#[test]
fn embedded_journal_worker_panic_retains_active_failure() {
    // Real storage confirms this particular injected failure occurred before insertion.
    // 真实存储确认此特定注入故障发生于插入前。
    let directory = Directory::new();
    // Production callers still receive conservative failure, not an inferred rollback promise.
    // 生产调用方仍收到保守故障，而非推断的回滚保证。
    let journal = directory.journal();
    // The fault is local to the only worker in this test.
    // 故障仅属于本测试唯一工作线程。
    let worker = writer(&journal, 1);
    *worker.center.fault.lock().unwrap() = Some(WriteFault::BeforeWrite);
    // The original attempt remains observable after its worker panics.
    // 工作线程 panic 后，原始尝试仍可观测。
    let receipt = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    // No revision may be fabricated on the failure path.
    // 失败路径不得伪造修订号。
    let result = receipt.wait(OBSERVE).unwrap();
    assert_eq!(result.phase, JournalWritePhase::Completed);
    assert!(result.revision.is_none());
    assert_eq!(result.error.unwrap().code, EmbeddedErrorCode::Internal);
    assert!(journal.get("runtime", "operation").unwrap().is_none());
    drop(receipt);
    close(&worker);
    assert!(worker.status().unwrap().failure.is_some());
}

/// Dropping the public worker requests drainage without abandoning active disk execution.
/// 释放公开工作线程对象会请求排空，不放弃活动磁盘执行。
#[test]
fn embedded_journal_worker_drop_drains_owned_write() {
    // Keep the actual database alive to inspect work after the public worker has disappeared.
    // 保持真实数据库存活，以便公开工作线程对象消失后检查工作。
    let directory = Directory::new();
    // Block storage until after the public worker and receipt have both been released.
    // 阻塞存储，直至公开工作线程对象和回执均被释放。
    let journal = directory.journal();
    // Exactly one real write must retain its execution owner.
    // 唯一真实写入必须保留执行所有者。
    let worker = writer(&journal, 1);
    // Weak observation cannot itself extend execution ownership.
    // 弱观测本身不能延长执行所有权。
    let center = Arc::downgrade(&worker.center);
    // The actual storage guard makes the worker's retained ownership observable.
    // 真实存储守卫使工作线程保留的所有权可被观测。
    let gate = journal.block_for_test();
    // Wait until the worker has claimed the actual attempt.
    // 等待工作线程认领真实尝试。
    let receipt = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    until(
        || receipt.snapshot().unwrap().phase == JournalWritePhase::Writing,
        "write was not claimed",
    );
    drop(receipt);
    drop(worker);
    assert!(center.upgrade().is_some());
    drop(gate);
    until(
        || center.upgrade().is_none(),
        "detached worker did not release its center after draining",
    );
    assert_eq!(
        journal
            .get("runtime", "operation")
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}
