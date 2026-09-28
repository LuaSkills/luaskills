//! Explicit writer reconstruction must preserve storage truth, original receipts and permanent close intent.
//! 显式写入者重建必须保留存储事实、原回执及永久关闭意图。

use super::*;
use std::sync::Barrier;

/// Restart after each real failure boundary without releasing old quota or silently replaying any attempt.
/// 在每个真实故障边界后重启，不释放旧配额，也不静默重放任何尝试。
#[test]
fn embedded_journal_worker_recovery_preserves_receipts_and_exact_checkpoint() {
    for fault in [
        WriteFault::BeforeWrite,
        WriteFault::AfterWrite,
        WriteFault::AfterPublication,
    ] {
        // Isolate every failure boundary in a real database and actual writer thread.
        // 在真实数据库及实际写入线程中隔离每个故障边界。
        let directory = Directory::new();
        // Retain the same database object through actual thread replacement.
        // 跨实际线程替换保留同一个数据库对象。
        let journal = directory.journal();
        // One retained receipt consumes all admission capacity even after its worker is reconstructed.
        // 一个保留回执即使在工作线程重建后仍消耗全部入场容量。
        let worker = writer(&journal, 1);
        assert!(!worker.recover_worker().unwrap());
        *worker.center.fault.lock().unwrap() = Some(fault);
        // Keep the immutable original candidate independent of observed outcome.
        // 独立于观测结果保留不可变原候选。
        let candidate = snapshot("operation");
        // An internal checkpoint supports exact committed-successor acknowledgement after uncertain publication.
        // 内部检查点支持在发布不确定后确认精确已提交后继。
        let receipt = worker
            .submit_checkpoint("runtime", None, Arc::clone(&candidate))
            .unwrap();
        // Supervision finishes before reconstruction may publish a replacement thread.
        // 监督完成后，重建才可发布替代线程。
        let original_receipt = receipt.wait(OBSERVE).unwrap();
        until(
            || worker.status().unwrap().worker_exited,
            "original writer did not exit",
        );
        // Preserve exact actual charge rather than resetting count or recomputing a new byte cost.
        // 保留精确实际费用，不重置数量或重新计算新字节费用。
        let original_status = worker.status().unwrap();
        assert_eq!(original_status.pending_writes, 1);
        assert!(original_status.failure.is_some());
        assert_eq!(
            journal.get("runtime", "operation").unwrap().is_some(),
            fault != WriteFault::BeforeWrite
        );
        assert!(worker.recover_worker().unwrap());
        assert!(!worker.recover_worker().unwrap());
        // The new live thread does not alter completed old evidence or its retained budget.
        // 新活动线程不改变完成的旧证据或其保留预算。
        let recovered = worker.status().unwrap();
        assert!(!recovered.closing && !recovered.worker_exited && recovered.failure.is_none());
        assert_eq!(recovered.pending_writes, original_status.pending_writes);
        assert_eq!(recovered.pending_bytes, original_status.pending_bytes);
        // This native receipt type deliberately has no wire serialization; compare all actual evidence fields.
        // 此原生回执类型刻意没有线序列化；比较全部实际证据字段。
        let retained_receipt = receipt.snapshot().unwrap();
        assert_eq!(retained_receipt.phase, original_receipt.phase);
        assert_eq!(retained_receipt.revision, original_receipt.revision);
        assert_eq!(retained_receipt.error, original_receipt.error);
        assert_eq!(
            worker
                .submit("runtime", None, snapshot("other"))
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::CapacityExceeded
        );
        assert_eq!(
            journal.get("runtime", "operation").unwrap().is_some(),
            fault != WriteFault::BeforeWrite
        );
        drop(receipt);
        // Only an explicit retry of the original checkpoint may confirm or write that exact candidate.
        // 仅精确原检查点的显式重试才能确认或写入该候选。
        let retry = worker
            .submit_checkpoint("runtime", None, Arc::clone(&candidate))
            .unwrap();
        assert_eq!(revision(&retry), 1);
        drop(retry);
        // Public insertion remains strict even after a recovered internal checkpoint acknowledges existing storage.
        // 恢复的内部检查点确认已有存储后，公开插入仍保持严格。
        let duplicate = worker.submit("runtime", None, candidate).unwrap();
        assert_eq!(
            duplicate.wait(OBSERVE).unwrap().error.unwrap().code,
            EmbeddedErrorCode::AlreadyCompleted
        );
        drop(duplicate);
        assert_eq!(
            journal
                .get("runtime", "operation")
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        close(&worker);
        assert_eq!(
            worker.recover_worker().unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
    }
}

/// Failed supervision may be visible before its actual thread exits; recovery must reject that live owner.
/// 失败监督可能在实际线程退出前可见；恢复必须拒绝该活动所有者。
#[test]
fn embedded_journal_worker_recovery_waits_for_actual_supervisor_exit() {
    // A real SQLite write is held until its receipt mutex can block final supervisor publication.
    // 保持真实 SQLite 写入，直至回执互斥锁可以阻塞最终监督发布。
    let directory = Directory::new();
    // This exact database supplies both the write gate and durable commit proof.
    // 此精确数据库同时提供写入门禁及持久提交证明。
    let journal = directory.journal();
    // The isolated writer is interrupted only after storage actually returns.
    // 仅在存储实际返回后中断此独立写入者。
    let worker = writer(&journal, 1);
    *worker.center.fault.lock().unwrap() = Some(WriteFault::AfterWrite);
    // Do not hold writer metadata while blocking disk.
    // 阻塞磁盘时不持有写入者元数据。
    let disk = journal.block_for_test();
    // Actual write phase precedes taking the receipt lock.
    // 实际写入阶段先于取得回执锁。
    let receipt = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    until(
        || receipt.snapshot().unwrap().phase == JournalWritePhase::Writing,
        "writer did not enter disk phase",
    );
    // Block only receipt completion; the public control metadata stays observable.
    // 仅阻塞回执完成；公开控制元数据保持可观测。
    let completion = receipt.state.snapshot.lock().unwrap();
    drop(disk);
    until(
        || worker.status().unwrap().failure.is_some(),
        "supervision did not retain failure",
    );
    assert!(!worker.status().unwrap().worker_exited);
    assert_eq!(
        worker.recover_worker().unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    drop(completion);
    until(
        || worker.status().unwrap().worker_exited,
        "blocked supervisor did not exit",
    );
    assert!(worker.recover_worker().unwrap());
    assert!(journal.get("runtime", "operation").unwrap().is_some());
    drop(receipt);
    close(&worker);
}

/// Concurrent recovery and explicit closure linearize once; closure can never be undone by a later restart.
/// 并发恢复及显式关闭只线性化一次；后续重启绝不能撤销关闭。
#[test]
fn embedded_journal_worker_recovery_never_reopens_explicit_close() {
    // Actual worker failure precedes the competing administrative calls.
    // 实际工作线程故障先于竞争管理调用。
    let directory = Directory::new();
    // Keep the database alive through all contender joins.
    // 在所有竞争线程等待完成前保持数据库存活。
    let journal = directory.journal();
    // Both callers share one worker object and its unique thread-handle authority.
    // 两调用方共享一个写入者对象及其唯一线程句柄权威。
    let worker = Arc::new(writer(&journal, 1));
    *worker.center.fault.lock().unwrap() = Some(WriteFault::BeforeWrite);
    // Observe and release the original failed receipt without altering the failure latch.
    // 观测并释放原失败回执，不改变故障标记。
    let receipt = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    receipt.wait(OBSERVE).unwrap();
    drop(receipt);
    until(
        || worker.status().unwrap().worker_exited,
        "writer did not exit before close race",
    );
    // Simultaneously release the two contenders without timing-dependent sleeps.
    // 同时释放两个竞争者，不依赖时序睡眠。
    let barrier = Arc::new(Barrier::new(3));
    // Recovery retains its actual outcome for post-race validation.
    // 恢复保留其实际结果，供竞态后验证。
    let restarting = {
        // This task owns only a clone, never another native writer identity.
        // 此任务仅拥有克隆，绝非另一原生写入者身份。
        let worker = Arc::clone(&worker);
        // Both administrative calls use the same release gate.
        // 两个管理调用使用同一释放门禁。
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            worker.recover_worker()
        })
    };
    // Closure is permanent whether it linearizes before or after thread reconstruction.
    // 无论在线程重建之前或之后线性化，关闭均为永久。
    let closing = {
        // Retain the same actual writer through closure publication.
        // 在关闭发布期间保留同一实际写入者。
        let worker = Arc::clone(&worker);
        // The main thread releases both contenders together.
        // 主线程同时释放两个竞争者。
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            worker.request_close();
        })
    };
    barrier.wait();
    // Preserve the outcome until both administrative tasks have actually stopped.
    // 在两个管理任务实际停止前保留结果。
    let restarted = restarting.join().unwrap();
    closing.join().unwrap();
    assert!(
        matches!(restarted, Ok(true)) || restarted.unwrap_err().code == EmbeddedErrorCode::Closed
    );
    assert_eq!(
        worker.recover_worker().unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    close(&worker);
}

/// A consumed thread panic outside supervision stays an explicit permanent reconstruction rejection.
/// 监督之外已消费的线程 panic 始终保持为明确永久重建拒绝。
#[test]
fn embedded_journal_worker_recovery_retains_unproven_join() {
    // This actual OS thread deliberately panics outside the production supervisor.
    // 此实际操作系统线程有意在生产监督器之外 panic。
    let thread = std::thread::spawn(|| panic!("outside supervision fixture"));
    // Thread ownership retains failed-join evidence even after consuming the handle.
    // 即使消费句柄，线程所有权仍保留等待失败证据。
    let mut owner = super::super::recovery::ThreadOwner::new(thread);
    until(|| owner.exited(), "uncontrolled test thread did not exit");
    assert_eq!(
        owner.join_finished().unwrap_err().code,
        EmbeddedErrorCode::Internal
    );
    assert_eq!(
        owner.join_finished().unwrap_err().code,
        EmbeddedErrorCode::Internal
    );
}

/// Poisoned queue metadata is not silently cleared or reconstructed from guessed empty counters.
/// 中毒队列元数据不能被静默清除，也不能从猜测的空计数重建。
#[test]
fn embedded_journal_worker_recovery_rejects_poisoned_metadata() {
    // Finish the actual worker before injecting metadata corruption into this isolated owner.
    // 向此独立所有者注入元数据损坏前，结束实际工作线程。
    let directory = Directory::new();
    // Keep the real database alive while verifying failure rejection.
    // 验证故障拒绝时保持真实数据库存活。
    let journal = directory.journal();
    // No attempt is still writing when the test poisons metadata.
    // 测试毒化元数据时，没有仍在写入的尝试。
    let worker = writer(&journal, 1);
    *worker.center.fault.lock().unwrap() = Some(WriteFault::BeforeWrite);
    // Complete and release this original attempt normally through supervision.
    // 通过监督正常完成并释放此原始尝试。
    let receipt = worker
        .submit("runtime", None, snapshot("operation"))
        .unwrap();
    receipt.wait(OBSERVE).unwrap();
    drop(receipt);
    until(
        || worker.status().unwrap().worker_exited,
        "writer did not exit before poison injection",
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Holding this exact guard proves real mutex poison, not a fabricated error response.
            // 持有此精确守卫证明真实互斥锁中毒，而非伪造错误响应。
            let _guard = worker.center.state.lock().unwrap();
            panic!("metadata poison fixture");
        }))
        .is_err()
    );
    assert_eq!(
        worker.recover_worker().unwrap_err().code,
        EmbeddedErrorCode::Internal
    );
    assert_eq!(
        worker.recover_worker().unwrap_err().code,
        EmbeddedErrorCode::Internal
    );
    assert!(worker.center.state.is_poisoned());
}
