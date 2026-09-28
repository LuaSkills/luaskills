use super::*;
use crate::runtime::embedded::OperationJournalConfig;
use serde_json::json;
use std::path::PathBuf;
use std::sync::mpsc;

/// Own an isolated real database directory for operation checkpoint tests.
/// 为操作检查点测试拥有隔离的真实数据库目录。
struct Directory(PathBuf);

impl Directory {
    /// Allocate a new random directory and return its sole cleanup owner.
    /// 分配新的随机目录并返回其唯一清理所有者。
    fn new() -> Self {
        // Operating-system randomness prevents reuse across processes and parallel runs.
        // 操作系统随机源防止跨进程及并行运行复用。
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).unwrap();
        // The prefix identifies only directories owned by this fixture.
        // 前缀仅标识此夹具拥有的目录。
        let suffix = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // Create rather than reuse a candidate directory.
        // 新建而非复用候选目录。
        let path = std::env::temp_dir().join(format!("luaskills-checkpoint-{suffix}"));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// Open the same exact database using explicit test budgets.
    /// 使用显式测试预算打开同一精确数据库。
    fn journal(&self, limits: OperationJournalConfig) -> Arc<OperationJournal> {
        Arc::new(OperationJournal::open(&self.0.join("operations.db"), limits).unwrap())
    }
}

impl Drop for Directory {
    /// Remove the uniquely created fixture after all operation and SQLite owners leave scope.
    /// 所有操作及 SQLite 所有者离开作用域后，删除唯一创建的夹具。
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Return deliberately small history budgets independent of runtime execution quotas.
/// 返回刻意设置较小且独立于运行时执行配额的历史预算。
fn journal_config() -> OperationJournalConfig {
    OperationJournalConfig {
        max_records: 8,
        max_record_bytes: 16 * 1024,
        max_database_bytes: 16 * 1024,
    }
}

/// Create one explicitly durable registry with the existing authoritative runtime fixture budgets.
/// 使用既有权威运行时夹具预算创建一个显式持久注册表。
fn registry(journal: &Arc<OperationJournal>) -> OperationRegistry {
    OperationRegistry::with_journal(
        "checkpoint-runtime".into(),
        &crate::runtime::embedded::tests::config(),
        Arc::clone(journal),
    )
    .unwrap()
}

/// Admit one operation with its original finite control and unique execution owner.
/// 使用原始有限控制及唯一执行所有者接纳一个操作。
fn admit(registry: &OperationRegistry) -> (OperationHandle, OperationOwner) {
    registry
        .admit(Arc::new(CallControl::new(Duration::from_secs(30)).unwrap()))
        .unwrap()
}

/// Construct trusted terminal filler evidence used to exercise actual SQLite page exhaustion.
/// 构造可信终态填充证据，以验证真实 SQLite 页耗尽。
fn filler() -> OperationSnapshot {
    OperationSnapshot {
        context: OperationContext::Unbound,
        operation_id: "filler".into(),
        phase: OperationPhase::Succeeded,
        cancellation_requested: false,
        effects: EffectState::Committed,
        value: Some(json!("x".repeat(8192))),
        error: None,
        host_effects: Vec::new(),
    }
}

/// Execution and terminal publication follow durable checkpoints; live forgetting preserves history.
/// 执行及终态发布跟随持久检查点；活动记录遗忘保留历史。
#[test]
fn embedded_operation_history_checkpoints_and_reopen() {
    // Keep the fixture alive beyond every registry, operation and journal owner.
    // 使夹具寿命长于所有注册表、操作及日志所有者。
    let directory = Directory::new();
    // Retain only the opaque original operation identity across complete shutdown.
    // 完全关闭时仅跨越保留原始不透明操作身份。
    let id = {
        // Each history is explicitly selected rather than inferred from a sibling path.
        // 每份历史均显式选择，不从相邻路径推断。
        let journal = directory.journal(journal_config());
        // Registry admission owns memory; execution admission owns the first disk checkpoint.
        // 注册表入场拥有内存；执行入场拥有首个磁盘检查点。
        let registry = registry(&journal);
        // The owner remains unique even while a client observes and cancels.
        // 即使客户端观测及取消，所有者仍唯一。
        let (handle, mut owner) = admit(&registry);
        assert!(
            journal
                .get("checkpoint-runtime", handle.id())
                .unwrap()
                .is_none()
        );
        owner.advance(OperationPhase::Initializing).unwrap();
        assert_eq!(
            journal
                .get("checkpoint-runtime", handle.id())
                .unwrap()
                .unwrap()
                .snapshot
                .phase,
            OperationPhase::Initializing
        );
        owner.advance(OperationPhase::Running).unwrap();
        assert!(handle.cancel().unwrap());
        owner.advance(OperationPhase::Cleaning).unwrap();
        owner
            .complete(Ok(Value::Null), EffectState::Committed)
            .unwrap();
        assert!(owner.pending_completion().is_none());
        assert_eq!(handle.snapshot().unwrap().value, Some(Value::Null));
        registry.forget(handle.id()).unwrap();
        assert!(registry.get(handle.id()).is_err());
        assert!(
            journal
                .get("checkpoint-runtime", handle.id())
                .unwrap()
                .is_some()
        );
        handle.id().to_owned()
    };
    // Reopening proves the acknowledged result came from disk rather than a surviving client handle.
    // 重新打开证明已确认结果来自磁盘，而非存活的客户端句柄。
    let journal = directory.journal(journal_config());
    // Preserve explicit null, committed effects and the independent cancellation flag.
    // 保留显式空值、已提交副作用及独立取消标记。
    let retained = journal.get("checkpoint-runtime", &id).unwrap().unwrap();
    assert_eq!(retained.snapshot.phase, OperationPhase::Succeeded);
    assert_eq!(retained.snapshot.value, Some(Value::Null));
    assert_eq!(retained.snapshot.effects, EffectState::Committed);
    assert!(retained.snapshot.cancellation_requested);
}

/// Failed initial checkpoint cannot advance the live phase, and a proven capacity failure is recoverable.
/// 初始检查点失败不能推进实时阶段，已证实的容量失败可以恢复。
#[test]
fn embedded_operation_history_rejects_execution_without_capacity() {
    // One retained record saturates disk history independently from live operation quota.
    // 一条保留记录使磁盘历史饱和，独立于活动操作配额。
    let directory = Directory::new();
    // Restrict the record count without preventing the complete filler value from fitting.
    // 限制记录数量，但允许完整填充值装入。
    let journal = directory.journal(OperationJournalConfig {
        max_records: 1,
        ..journal_config()
    });
    journal.insert("other-runtime", &filler()).unwrap();
    // Admission itself remains available without doing I/O under a registry lock.
    // 入场本身保持可用，不在注册表锁内执行 I/O。
    let registry = registry(&journal);
    // A failed transition retains the same execution owner and identity.
    // 失败变更保留相同执行所有者及身份。
    let (handle, owner) = admit(&registry);
    assert_eq!(
        owner
            .advance(OperationPhase::Initializing)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(handle.snapshot().unwrap().effects, EffectState::NotStarted);
    assert!(
        journal
            .get("checkpoint-runtime", handle.id())
            .unwrap()
            .is_none()
    );
    journal.forget("other-runtime", "filler", 1).unwrap();
    owner.advance(OperationPhase::Initializing).unwrap();
    assert_eq!(
        handle.snapshot().unwrap().phase,
        OperationPhase::Initializing
    );
    assert_eq!(
        journal
            .get("checkpoint-runtime", handle.id())
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

/// Real disk fullness retains the original terminal value and forbids replacing or publishing it early.
/// 真实磁盘空间耗尽保留原始终态值，禁止替换或提前发布。
#[test]
fn embedded_operation_history_terminal_failure_retains_original_outcome() {
    // Four SQLite pages suffice for the old checkpoint and filler but not the enlarged terminal result.
    // 四个 SQLite 页足以装下旧检查点及填充项，但容不下增大的终态结果。
    let directory = Directory::new();
    // Keep the external journal handle so recovery can explicitly release unrelated reconciled evidence.
    // 保留外部日志句柄，使恢复能够显式释放无关且已对账的证据。
    let journal = directory.journal(journal_config());
    // Registry lifetime owns pending results independently from client wait timeouts.
    // 注册表寿命独立于客户端等待超时而拥有待完成结果。
    let registry = registry(&journal);
    // The same owner retries persistence only; business execution is not replayed.
    // 同一所有者仅重试持久化；不重放业务执行。
    let (handle, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    journal.insert("other-runtime", &filler()).unwrap();
    // Stay within the runtime result bound while forcing a new SQLite overflow page.
    // 保持在运行时结果上限内，同时迫使 SQLite 使用新的溢出页。
    let actual_value = json!("v".repeat(900));
    assert_eq!(
        owner
            .complete(Ok(actual_value.clone()), EffectState::Committed)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        handle.wait(Duration::ZERO).unwrap().phase,
        OperationPhase::Cleaning
    );
    assert_eq!(handle.snapshot().unwrap().value, None);
    assert_eq!(
        journal
            .get("checkpoint-runtime", handle.id())
            .unwrap()
            .unwrap()
            .snapshot
            .phase,
        OperationPhase::Cleaning
    );
    assert_eq!(
        owner.pending_completion().unwrap().value.as_ref(),
        Some(&actual_value)
    );
    assert_eq!(
        owner
            .complete(Ok(json!("replacement")), EffectState::RolledBack)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        owner.advance(OperationPhase::Running).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        registry.forget(handle.id()).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    assert!(handle.cancel().unwrap());
    journal.forget("other-runtime", "filler", 1).unwrap();
    owner.retry_completion().unwrap();
    assert!(owner.pending_completion().is_none());
    assert_eq!(handle.snapshot().unwrap().value, Some(actual_value.clone()));
    assert_eq!(handle.snapshot().unwrap().effects, EffectState::Committed);
    // Durable terminal evidence is the original result, with current cancellation intent preserved.
    // 持久终态证据为原始结果，并保留当前取消意愿。
    let retained = journal
        .get("checkpoint-runtime", handle.id())
        .unwrap()
        .unwrap();
    assert_eq!(retained.snapshot.phase, OperationPhase::Succeeded);
    assert_eq!(retained.snapshot.value, Some(actual_value));
    assert!(retained.snapshot.cancellation_requested);
}

/// A checkpoint blocked at the actual disk gate never holds the public snapshot/cancellation lock.
/// 在真实磁盘门禁阻塞的检查点绝不持有公开快照及取消锁。
#[test]
fn embedded_operation_history_disk_wait_keeps_observation_and_cancel_available() {
    // Real SQLite storage is held at its own gate instead of relying on scheduler timing or sleeps.
    // 在真实 SQLite 自身门禁处阻塞，不依赖调度时序或睡眠。
    let directory = Directory::new();
    // The journal lock supplies the deterministic I/O boundary.
    // 日志锁提供确定性的 I/O 边界。
    let journal = directory.journal(journal_config());
    // The registry still uses its normal memory and identity ownership.
    // 注册表仍使用正常内存及身份所有权。
    let registry = registry(&journal);
    // Client handle and execution owner are deliberately used from different threads.
    // 客户端句柄及执行所有者被刻意用于不同线程。
    let (handle, owner) = admit(&registry);
    // Hold storage before starting the checkpoint so no write can win a race with observation.
    // 在开始检查点前持有存储，使写入无法与观测竞态获胜。
    let blocked = journal.block_for_test();
    // Advancing acquires its real per-operation receipt gate before reaching the blocked storage.
    // 阶段推进在到达已阻塞存储前取得真实逐操作回执门禁。
    let writer = std::thread::spawn(move || {
        owner.advance(OperationPhase::Running).unwrap();
        owner
    });
    // Bound the fixture's rendezvous without imposing a production timeout.
    // 限制夹具会合时间，不施加生产超时。
    let deadline = Instant::now() + Duration::from_secs(2);
    // Observe the actual checkpoint owner, not a message sent before the transition starts.
    // 观测真实检查点所有者，而非变更开始前发送的消息。
    let entered = loop {
        if matches!(
            handle
                .operation
                .history
                .as_ref()
                .unwrap()
                .revision
                .try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };
    // The observer must finish while the journal gate is still held.
    // 观测者必须在日志门禁仍被持有时完成。
    let observed = handle.clone();
    // A bounded channel reports whether public operations were responsive.
    // 有界通道报告公开操作是否及时响应。
    let (sender, receiver) = mpsc::sync_channel(1);
    // Query and cancellation only acquire their normal public operation state.
    // 查询及取消仅获取其正常公开操作状态。
    let observer = std::thread::spawn(move || {
        sender
            .send((
                observed.snapshot().unwrap().phase,
                observed.cancel().unwrap(),
            ))
            .unwrap();
    });
    // Release storage even on a failed observation before joining either test thread.
    // 即使观测失败，也先释放存储再等待任一测试线程。
    let result = receiver.recv_timeout(Duration::from_secs(2));
    drop(blocked);
    // Joining proves no gated test writer or observer leaks into subsequent cases.
    // 等待退出证明没有被门禁阻塞的测试写入者或观测者泄漏到后续用例。
    let owner = writer.join().unwrap();
    observer.join().unwrap();
    assert!(
        entered,
        "checkpoint writer never reached its actual receipt gate"
    );
    assert_eq!(result.unwrap(), (OperationPhase::Queued, true));
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Running);
    assert!(owner.control().is_cancelled());
}

/// Reusing an old runtime namespace never rebinds its stored operation to a fresh execution owner.
/// 复用旧运行时命名空间绝不将其已存操作重新绑定到新的执行所有者。
#[test]
fn embedded_operation_history_rejects_reused_namespace_identity() {
    // Reopen the exact file after dropping every live registry and operation reference.
    // 丢弃全部活动注册表及操作引用后重新打开精确文件。
    let directory = Directory::new();
    // Carry only the historical identity, without a handle capable of querying live execution.
    // 仅携带历史身份，不携带能够查询活动执行的句柄。
    let previous_id = {
        // The first runtime durably started but never published terminal evidence.
        // 第一个运行时已持久记录开始，但未发布终态证据。
        let journal = directory.journal(journal_config());
        // A deliberately fixed namespace reproduces an incorrect host reuse attempt.
        // 刻意固定命名空间，复现错误的宿主复用尝试。
        let registry = registry(&journal);
        // Execution authority is dropped instead of transferred to the next registry.
        // 执行权威被丢弃，而非转移到下一个注册表。
        let (handle, owner) = admit(&registry);
        owner.advance(OperationPhase::Running).unwrap();
        handle.id().to_owned()
    };
    // New owners can inspect history but cannot overwrite its original identity.
    // 新所有者可以查看历史，但不能覆盖其原始身份。
    let journal = directory.journal(journal_config());
    // This intentionally repeats the old namespace to prove rejection before business execution.
    // 此处故意重复旧命名空间，证明在业务执行前拒绝。
    let registry = registry(&journal);
    assert!(registry.get(&previous_id).is_err());
    // Memory admission does not automatically adopt the stored historical operation.
    // 内存入场不会自动接管已存历史操作。
    let (handle, owner) = admit(&registry);
    assert_eq!(handle.id(), previous_id);
    assert_eq!(
        owner
            .advance(OperationPhase::Initializing)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::AlreadyCompleted
    );
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Queued);
    // The original record remains running and unknown, never a fresh successful initialization.
    // 原始记录保持运行中及未知，绝不是一次新的成功初始化。
    let previous = journal
        .get("checkpoint-runtime", &previous_id)
        .unwrap()
        .unwrap();
    assert_eq!(previous.revision, 1);
    assert_eq!(previous.snapshot.phase, OperationPhase::Running);
    assert_eq!(previous.snapshot.effects, EffectState::Unknown);
}
mod queued;
