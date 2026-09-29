//! Real Lua scheduler persistence, explicit recovery, and session-close ordering tests.
//! 真实 Lua 调度器持久化、显式恢复和会话关闭顺序测试。

use super::*;

mod context;
mod finalization;
mod intent;
mod outcome;
mod prewarm;
mod reusable;
mod reusable_finalization;

/// One bounded fixture observation budget; production storage has its own ownership lifecycle.
/// 单一有界夹具观测预算；生产存储具有自己的所有权生命周期。
const OBSERVE: Duration = Duration::from_secs(8);

/// A terminal payload large enough to require more space than one accepted filler record.
/// 足够大的终态载荷，其所需空间超过单个已接纳填充记录。
const TERMINAL_RESULT_BYTES: usize = 12 * 1024;

/// Return storage limits with caller-selected `max_records` and `max_database_bytes`.
/// 返回由调用方指定 `max_records` 和 `max_database_bytes` 的存储上限。
fn journal_config(max_records: usize, max_database_bytes: u64) -> OperationJournalConfig {
    OperationJournalConfig {
        max_records,
        max_record_bytes: 16 * 1024,
        max_database_bytes,
    }
}

/// Build a real scheduler with explicit `config`, durable `limits`, and a separately host-owned writer.
/// 按显式 `config`、持久化 `limits` 和独立宿主自有写入者构造真实调度器。
fn durable_runtime(
    layout: &SystemRuntimeTestLayout,
    config: EmbeddedRuntimeConfig,
    limits: OperationJournalConfig,
) -> (
    Arc<OperationJournal>,
    Arc<OperationJournalWorker>,
    EmbeddedRuntime,
) {
    // Store host history outside the plugin package directory.
    // 将宿主历史存放在插件包目录之外。
    let journal = Arc::new(
        OperationJournal::open(&layout.runtime_root.join("operations.db"), limits).unwrap(),
    );
    // Pending write count derives from the already validated runtime operation budget for this fixture.
    // 此夹具待写数量派生自已校验的运行时操作预算。
    let writer = Arc::new(
        OperationJournalWorker::new(
            Arc::clone(&journal),
            OperationJournalWorkerConfig {
                max_pending_writes: config.max_operations,
                max_pending_bytes: 64 * 1024,
            },
        )
        .unwrap(),
    );
    // Preserve the same plugin aggregate limits used by ordinary scheduler tests.
    // 保留普通调度器测试使用的同一插件聚合上限。
    let plugin = plugin_policy(&config);
    // The explicit constructor is the only switch that enables disk phase checkpoints.
    // 显式构造入口是启用磁盘阶段检查点的唯一开关。
    let runtime = EmbeddedRuntime::with_journal_worker(
        Arc::new(make_runtime_test_engine_with_host_options(
            layout.host_options(),
        )),
        config,
        Arc::clone(&writer),
    )
    .unwrap();
    runtime
        .register_plugin(layout.package_id.clone(), plugin)
        .unwrap();
    (journal, writer, runtime)
}

/// Observe `condition` within the fixture budget without guessing progress from a sleep.
/// 在夹具预算内观测 `condition`，不从睡眠猜测进度。
fn until(mut condition: impl FnMut() -> bool, reason: &str) {
    // This deadline only bounds regression-test coordination.
    // 此截止时间只约束回归测试协调。
    let deadline = Instant::now() + OBSERVE;
    while !condition() {
        assert!(Instant::now() < deadline, "{reason}");
        std::thread::yield_now();
    }
}

/// Wait for the scheduler to retain a real checkpoint fault for exact `operation`.
/// 等待调度器为精确 `operation` 保留真实检查点故障。
fn failure(runtime: &EmbeddedRuntime, operation: &OperationHandle) -> OperationPersistenceFailure {
    // Keep the exact successful observation; a second query could race a currently owned checkpoint gate.
    // 保留精确成功观测；第二次查询可能与当前被拥有的检查点门禁竞争。
    let mut observed = None;
    until(
        || match runtime.persistence_failure(operation.id()) {
            Ok(failure) => {
                observed = failure;
                observed.is_some()
            }
            Err(error) if error.code == EmbeddedErrorCode::Busy => false,
            Err(error) => panic!("unexpected checkpoint observation error: {error:?}"),
        },
        "scheduler never retained the expected checkpoint failure",
    );
    observed.expect("successful fault observation was retained")
}

/// Close actual scheduler ownership first, then separately close its host-owned disk writer.
/// 先关闭真实调度器所有权，再单独关闭宿主自有磁盘写入者。
fn shutdown_durable(runtime: &EmbeddedRuntime, writer: &OperationJournalWorker) {
    shutdown(runtime);
    writer.request_close();
    until(
        || writer.poll_closed().unwrap(),
        "disk writer did not release its real receipts and thread",
    );
}

/// Return a reconciled terminal filler whose `bytes` payload deliberately consumes real storage.
/// 返回已对账终态填充项，其 `bytes` 载荷故意消费真实存储。
fn filler(bytes: usize) -> OperationSnapshot {
    OperationSnapshot {
        finalization: None,
        context: OperationContext::Unbound,
        operation_id: "filler".into(),
        phase: OperationPhase::Succeeded,
        cancellation_requested: false,
        effects: EffectState::Committed,
        value: Some(json!("x".repeat(bytes))),
        error: None,
        host_effects: Vec::new(),
    }
}

/// Fill `journal` to an actual capacity refusal, release one filler for Cleaning, and return the retained IDs.
/// 填充 `journal` 至真实容量拒绝，为清理阶段释放一项填充，并返回仍保留的身份。
/// `limits` are the journal's configured bounds; larger terminal growth must still fail in SQLite.
/// `limits` 为日志配置上限；较大的终态增长仍须在 SQLite 中失败。
fn fill_database(journal: &OperationJournal, limits: OperationJournalConfig) -> Vec<String> {
    // Each filler stays below the document budget while terminal growth exceeds a filler's footprint.
    // 每项填充均低于文档预算，同时终态增长超过填充项占用。
    let filler_bytes = limits.max_record_bytes / 4;
    assert!(TERMINAL_RESULT_BYTES > filler_bytes);
    // Only successfully committed fixtures are removed before retrying the original checkpoint.
    // 重试原检查点前仅删除成功提交的夹具。
    let mut committed = Vec::new();
    for index in 0..limits.max_records {
        // Unique historical identities leave the active runtime's exact revision untouched.
        // 唯一历史身份保持活动运行时的精确修订不变。
        let mut record = filler(filler_bytes);
        record.operation_id = format!("filler-{index}");
        match journal.insert("old-runtime", &record) {
            Ok(_) => committed.push(record.operation_id),
            Err(error) => {
                assert_eq!(error.code, EmbeddedErrorCode::CapacityExceeded);
                // Record-count or JSON-budget failures would not prove the intended SQLite transaction boundary.
                // 记录数量或 JSON 预算失败无法证明预期 SQLite 事务边界。
                assert_eq!(error.message, "operation history database operation failed");
                assert!(
                    !committed.is_empty(),
                    "no disk-pressure fixture was accepted"
                );
                // Free one proven footprint for the small Cleaning rewrite while retaining terminal pressure.
                // 释放一份已证实占用供小型清理重写使用，同时保留终态压力。
                let spare = committed.pop().unwrap();
                journal.forget("old-runtime", &spare, 1).unwrap();
                assert!(!committed.is_empty(), "no terminal disk pressure remains");
                return committed;
            }
        }
    }
    panic!("database pressure was not reached before the fixture record limit");
}

/// Real Lua results are persisted before observation, and immediate forgetting remains atomic with scheduler release.
/// 真实 Lua 结果在观测前持久化，立即遗忘继续与调度释放保持原子性。
#[test]
fn embedded_scheduler_persistence_preserves_results_and_immediate_forget() {
    // Use a real trusted plugin and independent host database.
    // 使用真实可信插件及独立宿主数据库。
    let layout = SystemRuntimeTestLayout::new("embedded durable ordinary");
    // Plenty of disk capacity isolates successful completion and reuse from storage exhaustion.
    // 充足磁盘容量将成功完成和复用与存储耗尽隔离。
    let (journal, writer, runtime) =
        durable_runtime(&layout, pool_config(), journal_config(16, 128 * 1024));
    // State persists in the actual reused Lua VM; this source has no host side effects.
    // 状态保留于真实复用 Lua VM；此源码没有宿主副作用。
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "local n=0; return {call=function(a) n=n+1; return {count=n,value=a} end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    for expected in 1..=2 {
        // Every call receives its own exact operation ID and original durable result.
        // 每个调用取得各自精确操作 ID 和原始持久结果。
        let operation = runtime.submit(call(&pool, json!(null)), OBSERVE).unwrap();
        // Public success cannot precede a real terminal journal acknowledgement.
        // 公开成功不能早于真实终态日志确认。
        let result = operation.wait(OBSERVE).unwrap();
        assert_eq!(
            result.phase,
            OperationPhase::Succeeded,
            "{:?}",
            result.error
        );
        assert_eq!(
            result.value,
            Some(json!({"count": expected, "value": null}))
        );
        assert!(
            runtime
                .persistence_failure(operation.id())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            runtime.retry_checkpoint(operation.id()).unwrap_err().code,
            EmbeddedErrorCode::Busy
        );
        // The exact stored operation remains available after its live handle is explicitly forgotten.
        // 活动句柄被显式遗忘后，精确存储操作仍然可用。
        let stored = journal.get(runtime.id(), operation.id()).unwrap().unwrap();
        assert_eq!(stored.revision, 4);
        assert_eq!(stored.snapshot.value, result.value);
        runtime.forget_operation(operation.id()).unwrap();
        assert_eq!(
            runtime
                .persistence_failure(operation.id())
                .unwrap_err()
                .code,
            EmbeddedErrorCode::NotFound
        );
        assert_eq!(
            journal
                .get(runtime.id(), operation.id())
                .unwrap()
                .unwrap()
                .snapshot
                .phase,
            OperationPhase::Succeeded
        );
    }
    assert_eq!(
        runtime
            .retry_checkpoint("foreign-operation")
            .unwrap_err()
            .code,
        EmbeddedErrorCode::NotFound
    );
    shutdown_durable(&runtime, &writer);
}

/// Failure before initialization keeps the original owner through close and explicit recovery without running Lua.
/// 初始化前失败时跨关闭及显式恢复保留原始所有者，不运行 Lua。
#[test]
fn embedded_scheduler_persistence_initial_failure_retains_cleanup_and_identity() {
    // Saturate the persistent record count before admitting real scheduler work.
    // 接纳真实调度任务前耗尽持久记录数量。
    let layout = SystemRuntimeTestLayout::new("embedded durable preinitialization failure");
    // A one-record limit is independently repairable by forgetting a reconciled old record.
    // 单记录上限可通过遗忘已对账旧记录独立修复。
    let (journal, writer, runtime) =
        durable_runtime(&layout, pool_config(), journal_config(1, 128 * 1024));
    journal.insert("old-runtime", &filler(0)).unwrap();
    // A real file would prove source initialization ran; the failed initial checkpoint must prevent it.
    // 真实文件可证明源码初始化已运行；失败的初始检查点必须阻止它。
    let pool = runtime.register_pool(definition(&layout, "local f=assert(io.open('unexpected-initialization','w')); f:write('ran'); f:close(); return {call=function() return 5 end}"), pool_policy(InstanceReuse::Reusable), permissions(), "r1".into()).unwrap();
    // Keep the original queryable operation rather than replacing it after storage repair.
    // 保留原始可查询操作，不在存储修复后替换它。
    let operation = runtime.submit(call(&pool, json!(null)), OBSERVE).unwrap();
    // Fault phase describes the unpublished candidate, not a public execution claim.
    // 故障阶段描述尚未发布的候选，不是公开执行声明。
    let failed = failure(&runtime, &operation);
    assert_eq!(failed.phase, OperationPhase::Initializing);
    assert_eq!(failed.error.code, EmbeddedErrorCode::CapacityExceeded);
    assert_eq!(failed.retry, CheckpointRetryState::Waiting);
    assert_eq!(operation.snapshot().unwrap().phase, OperationPhase::Queued);
    assert!(
        !layout
            .package_root
            .join("unexpected-initialization")
            .exists()
    );
    assert_eq!(runtime.usage().unwrap().cleaning_operations, 1);
    assert_eq!(
        runtime
            .submit(call(&pool, json!(null)), OBSERVE)
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Busy
    );
    journal.forget("old-runtime", "filler", 1).unwrap();
    assert!(journal.get(runtime.id(), operation.id()).unwrap().is_none());
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    // Block the actual retry transaction to make duplicate retry requests and live control deterministic.
    // 阻塞真实重试事务，使重复重试请求及实时控制具有确定性。
    let blocked = journal.block_for_test();
    assert!(runtime.retry_checkpoint(operation.id()).unwrap());
    assert!(!runtime.retry_checkpoint(operation.id()).unwrap());
    assert!(!runtime.poll_closed().unwrap());
    assert!(operation.snapshot().unwrap().cancellation_requested);
    drop(blocked);
    // Recovery persists the original failure; it cannot execute initialization after a failed admission intent.
    // 恢复持久化原始失败；不能在入场意图失败后执行初始化。
    let result = operation.wait(OBSERVE).unwrap();
    assert_eq!(result.phase, OperationPhase::Failed);
    assert_eq!(
        result.error.unwrap().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(result.effects, EffectState::NotStarted);
    assert!(
        !layout
            .package_root
            .join("unexpected-initialization")
            .exists()
    );
    assert_eq!(
        journal
            .get(runtime.id(), operation.id())
            .unwrap()
            .unwrap()
            .snapshot
            .phase,
        OperationPhase::Failed
    );
    assert!(
        runtime
            .persistence_failure(operation.id())
            .unwrap()
            .is_none()
    );
    shutdown_durable(&runtime, &writer);
}

/// A blocked execution write cannot stop the supervisor from removing cancelled queued work or responding to close.
/// 执行写入被阻塞不能阻止监督器移除已取消排队任务或响应关闭。
#[test]
fn embedded_scheduler_persistence_disk_wait_keeps_supervisor_live() {
    // One execution worker makes its blocked disk wait explicit while the supervisor must continue independently.
    // 单个执行工作线程使其磁盘阻塞明确，而监督器必须独立继续运行。
    let layout = SystemRuntimeTestLayout::new("embedded durable blocked disk");
    // Runtime and plugin concurrency use the same fixture source of truth.
    // 运行时及插件并发使用同一夹具权威来源。
    let mut config = pool_config();
    config.max_running_calls = 1;
    // A shared writer with adequate queue space admits both blocked checkpoints without additional threads.
    // 队列空间充足的共享写入者接纳两个被阻塞检查点，不增加线程。
    let (journal, writer, runtime) =
        durable_runtime(&layout, config, journal_config(16, 128 * 1024));
    // Pool policy is bounded by the single runtime executor.
    // 池策略受单个运行时执行者限制。
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_running_calls = 1;
    // Any executed source would leave a real marker, including initialization before the export.
    // 任何执行源码都会留下真实标记，包含导出前的初始化。
    let pool = runtime.register_pool(definition(&layout, "local f=assert(io.open('unexpected-source','w')); f:write('ran'); f:close(); return {call=function() return 1 end}"), policy, permissions(), "r1".into()).unwrap();
    // Freeze the actual database before the first execution intent can be acknowledged.
    // 在首个执行意图得到确认前冻结真实数据库。
    let blocked = journal.block_for_test();
    // The first operation occupies the one execution worker while waiting for its durable intent.
    // 第一个操作在等待持久意图时占有唯一执行工作线程。
    let first = runtime.submit(call(&pool, json!(null)), OBSERVE).unwrap();
    until(
        || writer.status().unwrap().writing,
        "execution did not reach its real disk attempt",
    );
    // The second request is admitted to the scheduler queue, then cancelled without executing Lua.
    // 第二个请求进入调度队列后被取消，不执行 Lua。
    let second = runtime.submit(call(&pool, json!(null)), OBSERVE).unwrap();
    second.cancel().unwrap();
    until(
        || runtime.usage().unwrap().queued_calls == 0,
        "blocked disk prevented supervisor cancellation maintenance",
    );
    assert_eq!(runtime.usage().unwrap().cleaning_operations, 1);
    assert_eq!(first.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(second.snapshot().unwrap().phase, OperationPhase::Queued);
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    assert!(first.snapshot().unwrap().cancellation_requested);
    assert!(!layout.package_root.join("unexpected-source").exists());
    drop(blocked);
    assert_eq!(
        first.wait(OBSERVE).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(
        second.wait(OBSERVE).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert!(!layout.package_root.join("unexpected-source").exists());
    shutdown_durable(&runtime, &writer);
}

/// Actual page exhaustion after one Lua call retains its original result and never replays business execution.
/// 一次 Lua 调用后的真实页耗尽保留原始结果，绝不重放业务执行。
#[test]
fn embedded_scheduler_persistence_terminal_failure_retries_only_storage() {
    // Initial capacity must accommodate real admission metadata before deliberately applying disk pressure.
    // 初始容量必须容纳真实入场元数据，随后才有意施加磁盘压力。
    let layout = SystemRuntimeTestLayout::new("embedded durable terminal capacity");
    // Allow the deliberately large result through value validation so SQLite supplies the actual failure.
    // 允许故意扩大的结果通过值校验，使 SQLite 产生真实失败。
    let mut config = pool_config();
    config.max_value_bytes = TERMINAL_RESULT_BYTES + 2;
    config.max_queued_bytes = config.max_value_bytes;
    config.max_host_request_bytes = config.max_value_bytes;
    // Keep record and database budgets explicit for the bounded fill procedure.
    // 为有界填充过程保留明确的记录及数据库预算。
    let limits = journal_config(16, 32 * 1024);
    // Page capacity, rather than a mocked writer error, forces the actual transaction failure.
    // 使用页容量而非模拟写入错误迫使真实事务失败。
    let (journal, writer, runtime) = durable_runtime(&layout, config, limits);
    // The fixture releases the Lua business gate even during assertion unwinding.
    // 即使断言展开，夹具仍释放 Lua 业务门禁。
    let release = FinalizerRelease(layout.package_root.join("business-release"));
    // A real append records how often the business function actually runs.
    // 真实追加记录业务函数实际运行次数。
    let source = r#"
        -- Retain controlled file and clock functions for deterministic fixture coordination.
        -- 保留受控文件及计时函数以进行确定性夹具协调。
        local open, clock = io.open, os.clock
        return {call=function(result_bytes)
            -- The append is the observable business effect whose duplication is forbidden.
            -- 此追加是禁止重复的可观测业务副作用。
            local count=assert(open('business-count','a')); count:write('x'); count:close()
            -- Enter only after the Running checkpoint was acknowledged.
            -- 仅在 Running 检查点确认后进入。
            local entered=assert(open('business-entered','w')); entered:write('yes'); entered:close()
            -- Bound the fixture gate independently from runtime operation deadlines.
            -- 独立于运行时操作截止时间约束夹具门禁。
            local deadline=clock()+8
            repeat
                -- Host release controls real Lua progress without a guessed sleep.
                -- 宿主释放控制真实 Lua 推进，不使用推测性睡眠。
                local ok, released=pcall(open,'business-release','r')
                if ok and released then released:close(); break end
            until clock() >= deadline
            return string.rep('v',result_bytes)
        end}
    "#;
    // Ordinary reusable execution permits inspection after failure without migrating the original call.
    // 普通复用执行允许失败后检查，不迁移原始调用。
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    // Keep one original operation identity across the real failed disk transaction.
    // 跨真实失败磁盘事务保留一个原始操作身份。
    let operation = runtime
        .submit(call(&pool, json!(TERMINAL_RESULT_BYTES)), OBSERVE)
        .unwrap();
    until(
        || layout.package_root.join("business-entered").exists(),
        "Lua business never reached its gate",
    );
    // Derive pressure from actual accepted records, independent of evolving context field lengths.
    // 按实际接纳记录形成压力，不依赖持续演进的上下文字段长度。
    let fillers = fill_database(&journal, limits);
    drop(release);
    // Public Cleaning is distinct from the successful but unacknowledged terminal candidate.
    // 公开清理状态区别于成功但尚未确认的终态候选。
    let failed = failure(&runtime, &operation);
    assert_eq!(failed.phase, OperationPhase::Succeeded);
    assert_eq!(failed.error.code, EmbeddedErrorCode::CapacityExceeded);
    assert_eq!(
        operation.snapshot().unwrap().phase,
        OperationPhase::Cleaning
    );
    assert!(operation.snapshot().unwrap().value.is_none());
    assert_eq!(
        fs::read_to_string(layout.package_root.join("business-count")).unwrap(),
        "x"
    );
    for id in fillers {
        journal.forget("old-runtime", &id, 1).unwrap();
    }
    assert_eq!(
        runtime
            .persistence_failure(operation.id())
            .unwrap()
            .unwrap()
            .retry,
        CheckpointRetryState::Waiting
    );
    assert_eq!(
        journal
            .get(runtime.id(), operation.id())
            .unwrap()
            .unwrap()
            .snapshot
            .phase,
        OperationPhase::Cleaning
    );
    assert!(runtime.retry_checkpoint(operation.id()).unwrap());
    // Only storage retries; the Lua counter must remain exactly one.
    // 仅重试存储；Lua 计数必须精确保持一次。
    let result = operation.wait(OBSERVE).unwrap();
    assert_eq!(
        result.phase,
        OperationPhase::Succeeded,
        "{:?}",
        result.error
    );
    assert_eq!(result.value, Some(json!("v".repeat(TERMINAL_RESULT_BYTES))));
    assert_eq!(
        fs::read_to_string(layout.package_root.join("business-count")).unwrap(),
        "x"
    );
    runtime.forget_operation(operation.id()).unwrap();
    shutdown_durable(&runtime, &writer);
}

/// Session close after terminal preparation cannot rewrite the original result and still waits for actual VM retirement.
/// 终态准备后的会话关闭不能改写原始结果，且仍等待真实 VM 退役。
#[test]
fn embedded_scheduler_persistence_late_session_close_preserves_prepared_result() {
    // The open session and its later call share one retained VM and exact runtime namespace.
    // 打开的会话及其后续调用共享一个保留 VM 和精确运行时命名空间。
    let layout = SystemRuntimeTestLayout::new("embedded durable late session close");
    // Admit the large terminal value before applying actual database pressure.
    // 施加真实数据库压力前，允许较大终态值入场。
    let mut config = pool_config();
    config.max_value_bytes = TERMINAL_RESULT_BYTES + 2;
    config.max_queued_bytes = config.max_value_bytes;
    config.max_host_request_bytes = config.max_value_bytes;
    // Both opening and invocation checkpoints fit before filling the remaining database capacity.
    // 填充剩余数据库容量前，开启及调用检查点均能容纳。
    let limits = journal_config(16, 32 * 1024);
    // A real terminal write failure provides a deterministic pause after preparation.
    // 真实终态写入失败在准备之后提供确定性暂停。
    let (journal, writer, runtime) = durable_runtime(&layout, config, limits);
    // Business progress and VM destruction have separately owned release markers.
    // 业务推进和 VM 销毁具有独立拥有的释放标记。
    let business_release = FinalizerRelease(layout.package_root.join("business-release"));
    // Retain actual VM destruction until terminal success has already been observed.
    // 保留真实 VM 销毁，直至已经观测到终态成功。
    let finalizer_release = FinalizerRelease(layout.package_root.join("finalizer-release"));
    // Both gates use real Lua execution; the terminal preparation point comes from an actual SQLite failure.
    // 两个门禁均使用真实 Lua 执行；终态准备时点来自实际 SQLite 失败。
    let source = r#"
        -- Keep controlled host file and clock functions for both real lifecycle gates.
        -- 为两个真实生命周期门禁保留受控宿主文件和计时函数。
        local open, clock = io.open, os.clock
        -- The proxy remains reachable through the session export until the VM is actually destroyed.
        -- 代理通过会话导出保持可达，直到 VM 实际销毁。
        local proxy = newproxy(true)
        getmetatable(proxy).__gc = function()
            -- Signal actual VM finalization rather than a provisional scheduler flag.
            -- 标记真实 VM 析构，而非调度器临时标记。
            local entered=assert(open('finalizer-entered','w')); entered:write('yes'); entered:close()
            -- The test releases this gate after observing terminal publication.
            -- 测试在观测终态发布后释放此门禁。
            local deadline=clock()+8
            repeat
                -- Poll the explicit fixture release file without changing process cwd.
                -- 轮询显式夹具释放文件，不改变进程工作目录。
                local ok, released=pcall(open,'finalizer-release','r')
                if ok and released then released:close(); break end
            until clock() >= deadline
        end
        return {call=function(result_bytes)
            -- Keep finalization pinned to the session's actual VM lifetime.
            -- 将析构固定到会话的真实 VM 生命周期。
            assert(proxy ~= nil)
            -- Count actual execution independently from persistence retries.
            -- 独立于持久化重试计数真实执行。
            local count=assert(open('business-count','a')); count:write('x'); count:close()
            -- Tell the host that Running was acknowledged before it fills the disk budget.
            -- 在宿主填满磁盘预算前通知其 Running 已确认。
            local entered=assert(open('business-entered','w')); entered:write('yes'); entered:close()
            -- Bound this fixture's rendezvous independently of the finalizer gate.
            -- 独立于析构门禁约束此夹具会合。
            local deadline=clock()+8
            repeat
                -- The host releases business only after inserting the real disk filler.
                -- 宿主仅在插入真实磁盘填充项后释放业务。
                local ok, released=pcall(open,'business-release','r')
                if ok and released then released:close(); break end
            until clock() >= deadline
            return string.rep('v',result_bytes)
        end}
    "#;
    // Session reuse retains this precise VM after the business function succeeds.
    // 会话复用在业务函数成功后保留此精确 VM。
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    // Opening itself must finish durably before the business request is accepted.
    // 接纳业务请求前，打开操作自身必须持久完成。
    let session = super::sessions::opened(&runtime, &pool);
    // The business operation remains the same through failure, late close and disk-only retry.
    // 业务操作在失败、较晚关闭及仅磁盘重试过程中保持相同。
    let operation = super::sessions::session_call(&runtime, &session, json!(TERMINAL_RESULT_BYTES));
    until(
        || {
            if let Ok(Some(failed)) = runtime.persistence_failure(operation.id()) {
                panic!("session checkpoint failed before business entry: {failed:?}");
            }
            layout.package_root.join("business-entered").exists()
        },
        "session business never reached its gate",
    );
    // Accepted historical records supply real storage pressure without guessing a fixed filler length.
    // 已接纳历史记录提供真实存储压力，不推测固定填充长度。
    let fillers = fill_database(&journal, limits);
    drop(business_release);
    assert_eq!(
        failure(&runtime, &operation).phase,
        OperationPhase::Succeeded
    );
    runtime.close_session(&session).unwrap();
    assert_eq!(
        runtime.session(&session).unwrap().phase,
        EmbeddedSessionPhase::Closing
    );
    assert_eq!(
        operation.snapshot().unwrap().phase,
        OperationPhase::Cleaning
    );
    assert!(!layout.package_root.join("finalizer-entered").exists());
    for id in fillers {
        journal.forget("old-runtime", &id, 1).unwrap();
    }
    runtime.retry_checkpoint(operation.id()).unwrap();
    // The close linearized after preparation, so the original successful business value remains successful.
    // 关闭在线性顺序中晚于准备，因此原成功业务值保持成功。
    let result = operation.wait(OBSERVE).unwrap();
    assert_eq!(
        result.phase,
        OperationPhase::Succeeded,
        "{:?}",
        result.error
    );
    assert_eq!(result.value, Some(json!("v".repeat(TERMINAL_RESULT_BYTES))));
    until(
        || layout.package_root.join("finalizer-entered").exists(),
        "late close never started actual session retirement",
    );
    assert_eq!(
        runtime.session(&session).unwrap().phase,
        EmbeddedSessionPhase::Closing
    );
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    assert_eq!(
        fs::read_to_string(layout.package_root.join("business-count")).unwrap(),
        "x"
    );
    drop(finalizer_release);
    super::sessions::closed_session(&runtime, &session);
    shutdown_durable(&runtime, &writer);
}
