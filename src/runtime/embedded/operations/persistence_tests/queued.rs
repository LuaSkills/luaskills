//! Queued checkpoint integration against real storage and original operation owners.
//! 对真实存储及原始操作所有者进行队列检查点集成验证。

use super::*;

mod finalization;

mod intent;
mod outcome;
use crate::runtime::embedded::{OperationJournalWorker, OperationJournalWorkerConfig};

/// One fixture observation deadline, independent from production storage or operation deadlines.
/// 单一夹具观测截止时长，独立于生产存储和操作截止时间。
const OBSERVE: Duration = Duration::from_secs(8);

/// Build a queued registry and its host-owned writer for exact `journal`.
/// 为精确 `journal` 创建队列注册表及宿主拥有的写入者。
fn queued_registry(
    journal: &Arc<OperationJournal>,
) -> (Arc<OperationJournalWorker>, OperationRegistry) {
    // The host explicitly owns closure independently of retained operation observations.
    // 宿主显式拥有关闭责任，独立于保留的操作观测。
    let writer = Arc::new(
        OperationJournalWorker::new(
            Arc::clone(journal),
            OperationJournalWorkerConfig {
                max_pending_writes: 4,
                max_pending_bytes: 64 * 1024,
            },
        )
        .unwrap(),
    );
    // The new namespace cannot adopt an old process's operation history.
    // 新命名空间不能接管旧进程的操作历史。
    let registry = OperationRegistry::with_journal_worker(
        "queued-checkpoint-runtime".into(),
        &crate::runtime::embedded::tests::config(),
        Arc::clone(&writer),
    )
    .unwrap();
    (writer, registry)
}

/// Poll `observe` to completion or explicit failure without treating timeout as operation success.
/// 轮询 `observe` 至完成或明确失败，不把超时当作操作成功。
fn poll(mut observe: impl FnMut() -> EmbeddedResult<bool>) -> EmbeddedResult<()> {
    // Bound broken test coordination without adding production retry or sleep behavior.
    // 限制错误测试协调，不增加生产重试或睡眠行为。
    let deadline = Instant::now() + OBSERVE;
    loop {
        if observe()? {
            return Ok(());
        }
        assert!(
            Instant::now() < deadline,
            "checkpoint never reached its expected observation"
        );
        std::thread::yield_now();
    }
}

/// Request and prove real writer closure after every owned receipt has been released.
/// 每个自有回执释放后请求并证明真实写入者关闭。
fn close(writer: &OperationJournalWorker) {
    writer.request_close();
    poll(|| writer.poll_closed()).unwrap();
}

/// Queue admission failure retains the original candidate, and blocking retry cannot invent another identity.
/// 队列入场失败保留原始候选，阻塞重试不能编造另一个身份。
#[test]
fn embedded_operation_queued_admission_failure_keeps_original_candidate() {
    // The database has space; only the writer's retained receipt budget is exhausted.
    // 数据库尚有空间；仅写入者保留回执预算耗尽。
    let directory = Directory::new();
    // Both the deliberately retained receipt and operation use the exact same storage.
    // 故意保留的回执和操作使用同一精确存储。
    let journal = directory.journal(journal_config());
    // A single retained attempt rejects operation checkpoints before disk execution begins.
    // 单个保留尝试在磁盘执行开始前拒绝操作检查点。
    let writer = Arc::new(
        OperationJournalWorker::new(
            Arc::clone(&journal),
            OperationJournalWorkerConfig {
                max_pending_writes: 1,
                max_pending_bytes: 64 * 1024,
            },
        )
        .unwrap(),
    );
    // Retain one successful unrelated write so its actual receipt still owns the only queue slot.
    // 保留一次成功无关写入，使其真实回执仍拥有唯一队列槽。
    let retained = writer
        .submit("other-runtime", None, Arc::new(filler()))
        .unwrap();
    assert_eq!(retained.wait(OBSERVE).unwrap().revision, Some(1));
    // Registry admission is independent of the worker's durable-write capacity.
    // 注册表入场独立于写入者持久写入容量。
    let registry = OperationRegistry::with_journal_worker(
        "admission-runtime".into(),
        &crate::runtime::embedded::tests::config(),
        Arc::clone(&writer),
    )
    .unwrap();
    // The failed phase candidate must remain tied to this original client handle.
    // 失败阶段候选必须继续绑定此原始客户端句柄。
    let (handle, owner) = admit(&registry);
    assert_eq!(
        owner
            .poll_advance(OperationPhase::Running)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Queued);
    drop(retained);
    poll(|| Ok(writer.status()?.pending_writes == 0)).unwrap();
    assert_eq!(
        owner
            .poll_advance(OperationPhase::Running)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert!(
        journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .is_none()
    );
    assert!(handle.cancel().unwrap());
    owner.advance(OperationPhase::Running).unwrap();
    // Explicit retry preserves captured checkpoint bytes; live cancellation remains independently observable.
    // 显式重试保留捕获的检查点字节；实时取消仍可独立观测。
    let record = journal
        .get(&registry.runtime_id, handle.id())
        .unwrap()
        .unwrap();
    assert_eq!(record.revision, 1);
    assert!(!record.snapshot.cancellation_requested);
    assert!(handle.snapshot().unwrap().cancellation_requested);
    close(&writer);
}

/// Phase polling stays responsive with the real database locked and rejects overtaking transitions.
/// 真实数据库锁定时阶段轮询保持响应，并拒绝超越原阶段的变更。
#[test]
fn embedded_operation_queued_phase_keeps_control_and_original_snapshot() {
    // Real storage is deliberately unavailable until the observer has already returned.
    // 真实存储故意保持不可用，直至观测者已经返回。
    let directory = Directory::new();
    // The actual SQLite gate supplies the causal boundary.
    // 真实 SQLite 门禁提供因果边界。
    let journal = directory.journal(journal_config());
    // Registry and writer share the exact database without a second persistence authority.
    // 注册表和写入者共享精确数据库，没有第二套持久化权威。
    let (writer, registry) = queued_registry(&journal);
    // Only this owner may publish the pending transition.
    // 只有此所有者可以发布待完成变更。
    let (handle, owner) = admit(&registry);
    // Freeze disk before any owner checkpoint can start.
    // 在任何所有者检查点能够开始前冻结磁盘。
    let blocked = journal.block_for_test();
    // Receipt delivery is bounded so a regression can release storage before joining.
    // 回执交付有界，使发生回退时能够先释放存储再等待线程。
    let (send, receive) = mpsc::sync_channel(1);
    // The observer exercises both first submission and polling while disk remains unavailable.
    // 观测者在磁盘不可用时验证首次提交和轮询。
    let observer = std::thread::spawn(move || {
        // Both pending observations must refer to the same queued write.
        // 两次待完成观测必须指向同一排队写入。
        let first = owner.poll_advance(OperationPhase::Initializing);
        // Polling an existing attempt cannot perform another insertion.
        // 轮询既有尝试不能执行另一次插入。
        let second = owner.poll_advance(OperationPhase::Initializing);
        // A different phase cannot replace the in-flight candidate.
        // 不同阶段不能替换在途候选。
        let overtaking = owner.poll_advance(OperationPhase::Running);
        send.send((first, second, overtaking)).unwrap();
        owner
    });
    // Capture responsiveness before allowing disk progress.
    // 在允许磁盘推进前捕获响应性。
    let observed = receive.recv_timeout(OBSERVE);
    assert_eq!(
        handle.wait(Duration::ZERO).unwrap().phase,
        OperationPhase::Queued
    );
    assert!(handle.cancel().unwrap());
    drop(blocked);
    // Join after releasing the real gate even when response observation failed.
    // 即使响应观测失败，也在释放真实门禁后等待线程。
    let owner = observer.join().unwrap();
    // Successful receipt admission is still not a durable phase acknowledgement.
    // 成功的回执入场仍不是持久阶段确认。
    let (first, second, overtaking) = observed.unwrap();
    assert!(!first.unwrap());
    assert!(!second.unwrap());
    assert_eq!(overtaking.unwrap_err().code, EmbeddedErrorCode::Busy);
    poll(|| owner.poll_advance(OperationPhase::Initializing)).unwrap();
    assert_eq!(
        handle.snapshot().unwrap().phase,
        OperationPhase::Initializing
    );
    assert!(handle.snapshot().unwrap().cancellation_requested);
    // The stored checkpoint is the originally submitted snapshot; later cancellation stays a live projection.
    // 存储检查点为最初提交快照；后续取消保持实时投影。
    let record = journal
        .get(&registry.runtime_id, handle.id())
        .unwrap()
        .unwrap();
    assert_eq!(record.revision, 1);
    assert!(!record.snapshot.cancellation_requested);
    close(&writer);
}

/// Nonblocking observation never waits behind a concurrent blocking owner checkpoint.
/// 非阻塞观测绝不在并发阻塞所有者检查点之后等待。
#[test]
fn embedded_operation_queued_poll_does_not_wait_for_phase_gate() {
    // A held database lock keeps the blocking execution transition provably live.
    // 持有的数据库锁使阻塞执行变更可被证明仍存活。
    let directory = Directory::new();
    // The writer's actual execution status supplies the rendezvous.
    // 写入者的真实执行状态提供会合依据。
    let journal = directory.journal(journal_config());
    // The registry supports both execution-thread waits and control-thread polls.
    // 注册表同时支持执行线程等待及控制线程轮询。
    let (writer, registry) = queued_registry(&journal);
    // Shared references exercise the publicly permitted concurrent phase interface.
    // 共享引用验证公开接口允许的并发阶段调用。
    let (handle, owner) = admit(&registry);
    // Only one non-cloneable owner exists, shared through an Arc for concurrent immutable calls.
    // 仅存在一个不可克隆所有者，通过 Arc 共享以并发执行不可变调用。
    let owner = Arc::new(owner);
    // Retain this exact gate until both observation threads have reported.
    // 保留此精确门禁，直到观测线程报告。
    let blocked = journal.block_for_test();
    // This worker owns the phase mutation mutex throughout its synchronous wait.
    // 此工作线程在同步等待期间始终拥有阶段变更互斥锁。
    let execution = Arc::clone(&owner);
    // Join actual execution after storage is opened.
    // 存储开放后等待真实执行退出。
    let advancing = std::thread::spawn(move || execution.advance(OperationPhase::Running));
    poll(|| Ok(writer.status()?.writing)).unwrap();
    // A second owner reference must return pending, rather than wait for the phase mutex.
    // 第二个所有者引用必须返回待完成，而非等待阶段互斥锁。
    let observing = Arc::clone(&owner);
    // Capture before release to prove this is nonblocking with respect to storage.
    // 在释放前捕获，以证明相对于存储非阻塞。
    let (send, receive) = mpsc::sync_channel(1);
    // This explicit observer catches accidental blocking mutex acquisition.
    // 此显式观测者捕获意外阻塞互斥锁获取。
    let observer = std::thread::spawn(move || {
        send.send(observing.poll_advance(OperationPhase::Running))
            .unwrap()
    });
    // Preserve the result observed while execution was still blocked.
    // 保留执行仍被阻塞时观测的结果。
    let observed = receive.recv_timeout(OBSERVE);
    assert!(handle.cancel().unwrap());
    drop(blocked);
    advancing.join().unwrap().unwrap();
    observer.join().unwrap();
    assert!(!observed.unwrap().unwrap());
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Running);
    close(&writer);
}

/// A capacity failure stays failed under polling even after repair until the original phase is explicitly retried.
/// 容量故障即使修复后也在轮询中保持失败，直至显式重试原始阶段。
#[test]
fn embedded_operation_queued_phase_failure_requires_explicit_retry() {
    // One unrelated reconciled record fills the exact disk record budget.
    // 一条无关且已对账记录填满精确磁盘记录预算。
    let directory = Directory::new();
    // The database limit is independent of live operation and writer queue capacity.
    // 数据库上限独立于活动操作及写入队列容量。
    let journal = directory.journal(OperationJournalConfig {
        max_records: 1,
        ..journal_config()
    });
    journal.insert("other-runtime", &filler()).unwrap();
    // All retries must preserve this registry's original namespace and identity.
    // 全部重试必须保留此注册表原始命名空间及身份。
    let (writer, registry) = queued_registry(&journal);
    // The original owner remains authoritative after rejected persistence.
    // 持久化被拒绝后，原始所有者仍为权威。
    let (handle, mut owner) = admit(&registry);
    assert_eq!(
        poll(|| owner.poll_advance(OperationPhase::Initializing))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(
        owner
            .prepare_completion(Ok(json!(null)), EffectState::NotStarted)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    journal.forget("other-runtime", "filler", 1).unwrap();
    for _ in 0..3 {
        assert_eq!(
            owner
                .poll_advance(OperationPhase::Initializing)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::CapacityExceeded
        );
    }
    assert!(
        journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .is_none()
    );
    // One explicit retry is sufficient; subsequent polling only observes that new original-candidate attempt.
    // 一次显式重试即可；后续轮询仅观测原候选的新尝试。
    if !owner.retry_advance().unwrap() {
        poll(|| owner.poll_advance(OperationPhase::Initializing)).unwrap();
    }
    assert_eq!(
        journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(json!(null)), EffectState::NotStarted)
        .unwrap();
    assert_eq!(handle.snapshot().unwrap().value, Some(json!(null)));
    close(&writer);
}

/// Terminal preparation and polling preserve an original result while real disk I/O is blocked.
/// 真实磁盘 I/O 阻塞时，终态准备及轮询保留原始结果。
#[test]
fn embedded_operation_queued_terminal_wait_never_publishes_early() {
    // Use actual durable phase records before blocking the terminal checkpoint.
    // 阻塞终态检查点前使用真实持久阶段记录。
    let directory = Directory::new();
    // Shared storage exposes the acknowledged phase before terminal publication.
    // 共享存储暴露终态发布前的已确认阶段。
    let journal = directory.journal(journal_config());
    // Writer closure stays explicitly host-owned.
    // 写入者关闭继续显式归宿主拥有。
    let (writer, registry) = queued_registry(&journal);
    // The client cannot access the private pending terminal value as a completed operation.
    // 客户端不能把私有待完成终态值当作已完成操作访问。
    let (handle, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .prepare_completion(Ok(json!({"original": null})), EffectState::Committed)
        .unwrap();
    assert_eq!(
        owner
            .prepare_completion(Ok(json!("replacement")), EffectState::RolledBack)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    // The real storage lock remains held while terminal polling must return.
    // 终态轮询必须返回时，仍持有真实存储锁。
    let blocked = journal.block_for_test();
    // Capture bounded observations from the actual sole owner.
    // 从真实唯一所有者捕获有界观测。
    let (send, receive) = mpsc::sync_channel(1);
    // Return the owner through its join handle to preserve its pending attempt across threads.
    // 通过等待句柄返回所有者，跨线程保留其待完成尝试。
    let observer = std::thread::spawn(move || {
        send.send((owner.poll_completion(), owner.poll_completion()))
            .unwrap();
        owner
    });
    // Waiting on the client and requesting cancellation never acquire the pending terminal receipt gate.
    // 等待客户端及请求取消绝不获取待完成终态回执门禁。
    let observed = receive.recv_timeout(OBSERVE);
    assert_eq!(
        handle.wait(Duration::ZERO).unwrap().phase,
        OperationPhase::Cleaning
    );
    assert!(handle.snapshot().unwrap().value.is_none());
    assert!(handle.cancel().unwrap());
    drop(blocked);
    // Rejoin only after storage is allowed to finish.
    // 仅在允许存储完成后重新等待线程。
    let mut owner = observer.join().unwrap();
    // Both observations happened before the terminal checkpoint could be acknowledged.
    // 两个观测均发生于终态检查点可能被确认之前。
    let (first, second) = observed.unwrap();
    assert!(!first.unwrap());
    assert!(!second.unwrap());
    poll(|| owner.poll_completion()).unwrap();
    assert!(owner.pending_completion().is_none());
    assert_eq!(
        handle.snapshot().unwrap().value,
        Some(json!({"original": null}))
    );
    assert!(handle.snapshot().unwrap().cancellation_requested);
    // There was one exact terminal write after two phase writes, despite repeated polling.
    // 尽管重复轮询，两次阶段写入后仅发生一次精确终态写入。
    let record = journal
        .get(&registry.runtime_id, handle.id())
        .unwrap()
        .unwrap();
    assert_eq!(record.revision, 3);
    assert_eq!(record.snapshot.value, Some(json!({"original": null})));
    assert_eq!(record.snapshot.effects, EffectState::Committed);
    assert!(!record.snapshot.cancellation_requested);
    close(&writer);
}

/// Actual SQLite exhaustion keeps terminal failure stable until an explicit retry of the same outcome.
/// 真实 SQLite 耗尽使终态故障稳定保留，直至显式重试相同结果。
#[test]
fn embedded_operation_queued_terminal_failure_does_not_retry_on_poll() {
    // The four-page database can hold prior phases but cannot grow for the terminal value.
    // 四页数据库能容纳之前阶段，但不能为终态值增长。
    let directory = Directory::new();
    // Keep direct access only for the explicit host repair of unrelated reconciled history.
    // 仅为宿主显式修复无关且已对账历史而保留直接访问。
    let journal = directory.journal(journal_config());
    // This writer is shared by the original operation throughout failure and repair.
    // 原始操作在故障及修复全过程共享此写入者。
    let (writer, registry) = queued_registry(&journal);
    // Only the original owner may retry persistence of the original result.
    // 仅原始所有者可以重试原始结果的持久化。
    let (handle, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    journal.insert("other-runtime", &filler()).unwrap();
    // A valid runtime value requires an unavailable SQLite overflow page.
    // 合法运行时值需要一个不可用 SQLite 溢出页。
    let original = json!("v".repeat(900));
    owner
        .prepare_completion(Ok(original.clone()), EffectState::Committed)
        .unwrap();
    assert_eq!(
        poll(|| owner.poll_completion()).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    journal.forget("other-runtime", "filler", 1).unwrap();
    for _ in 0..3 {
        assert_eq!(
            owner.poll_completion().unwrap_err().code,
            EmbeddedErrorCode::CapacityExceeded
        );
    }
    assert_eq!(
        journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        owner.pending_completion().unwrap().value.as_ref(),
        Some(&original)
    );
    assert_eq!(
        owner
            .complete(Ok(json!("replacement")), EffectState::RolledBack)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Cleaning);
    if !owner.retry_completion_nonblocking().unwrap() {
        poll(|| owner.poll_completion()).unwrap();
    }
    assert_eq!(handle.snapshot().unwrap().value, Some(original.clone()));
    assert_eq!(
        journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .unwrap()
            .snapshot
            .value,
        Some(original)
    );
    close(&writer);
}

/// Direct persistence rejects nonblocking APIs explicitly, while memory completion retains its old semantics.
/// 直接持久化显式拒绝非阻塞接口，而内存完成保留原语义。
#[test]
fn embedded_operation_queued_api_does_not_hide_direct_storage() {
    // A direct journal is deliberately selected through the original constructor.
    // 通过原构造入口刻意选择直接日志。
    let directory = Directory::new();
    // A control-thread poll must never silently access this file.
    // 控制线程轮询绝不能静默访问此文件。
    let journal = directory.journal(journal_config());
    // Explicit backend selection, rather than probing, determines supported wait behavior.
    // 显式后端选择而非探测决定受支持的等待行为。
    let registry = registry(&journal);
    // Retain the owner after rejected nonblocking requests for normal synchronous completion.
    // 非阻塞请求被拒绝后保留所有者，以正常同步完成。
    let (handle, mut owner) = admit(&registry);
    assert_eq!(
        owner
            .poll_advance(OperationPhase::Running)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Queued);
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .prepare_completion(Ok(json!(null)), EffectState::NotStarted)
        .unwrap();
    assert_eq!(
        owner.poll_completion().unwrap_err().code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Cleaning);
    owner.retry_completion().unwrap();
    assert_eq!(handle.snapshot().unwrap().value, Some(json!(null)));
    // A memory-only registry needs no worker and can use the same preparation/polling interface.
    // 仅内存注册表不需要工作线程，并可使用同一准备及轮询接口。
    let memory =
        OperationRegistry::new("memory".into(), &crate::runtime::embedded::tests::config())
            .unwrap();
    // Memory publication completes immediately after explicit cleanup.
    // 显式清理后立即完成内存发布。
    let (handle, mut owner) = admit(&memory);
    assert!(owner.poll_advance(OperationPhase::Cleaning).unwrap());
    owner
        .prepare_completion(Ok(json!(null)), EffectState::NotStarted)
        .unwrap();
    assert!(owner.poll_completion().unwrap());
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Succeeded);
}
