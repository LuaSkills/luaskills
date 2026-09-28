//! Real journal integration at native and queued capability dispatch boundaries.
//! 原生及队列能力分发边界的真实日志集成。

use super::*;
use crate::runtime::embedded::capabilities::*;
use crate::runtime::embedded::*;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Direct synchronous history must reject persistent host dispatch rather than block an SDK control caller.
/// 直接同步历史必须拒绝持久宿主分发，不能阻塞 SDK 控制调用方。
#[test]
fn embedded_effect_intent_direct_backend_rejects_both_transports() {
    // Test both declared transports independently with fresh databases and identities.
    // 使用全新数据库及身份独立测试两种声明传输。
    for execution in [CapabilityExecution::Native, CapabilityExecution::Queued] {
        // Direct history remains a phase-only low-level constructor.
        // 直接历史仍是仅支持阶段的低层构造入口。
        let directory = Directory::new();
        // The journal is real even though dispatch will reject before attempting intent I/O.
        // 即使分发在尝试意图 I/O 前被拒绝，日志仍然真实。
        let journal = directory.journal(journal_config());
        // Preserve the original direct storage selection; do not silently create a worker.
        // 保留原始直接存储选择，不静默创建写入者。
        let registry = super::super::registry(&journal);
        // A native implementation must remain unentered after the explicit rejection.
        // 显式拒绝后原生实现必须保持未进入。
        let count = Arc::new(AtomicUsize::new(0));
        // Callback ownership shares only this concrete execution counter.
        // 回调所有权仅共享此具体执行计数。
        let called = Arc::clone(&count);
        // Transport declaration uniquely determines whether a native closure is supplied.
        // 传输声明唯一决定是否提供原生闭包。
        let native: Option<NativeCapability> = match execution {
            CapabilityExecution::Native => Some(Arc::new(move |_| {
                called.fetch_add(1, Ordering::SeqCst);
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::Committed,
                }
            })),
            CapabilityExecution::Queued => None,
        };
        // The capability and operation share the same exact trusted namespace.
        // 能力及操作共享同一精确可信命名空间。
        let capabilities = capabilities(&registry.runtime_id, execution, native);
        // Persist a real operation phase before attempting unsupported dispatch.
        // 尝试不支持的分发之前持久化真实操作阶段。
        let (operation, mut owner) = admit(&registry);
        owner.advance(OperationPhase::Running).unwrap();
        match execution {
            CapabilityExecution::Native => assert_eq!(
                native_call(&capabilities, &registry, &operation, &owner)
                    .unwrap_err()
                    .code,
                EmbeddedErrorCode::Unsupported
            ),
            CapabilityExecution::Queued => {
                // The request remains a normal retained business rejection rather than a delivered handler.
                // 请求保持正常保留业务拒绝，不成为已交付处理器。
                let request = queued_call(&capabilities, &registry, &operation, &owner);
                assert!(capabilities.host_requests().take(1).unwrap().is_empty());
                assert_eq!(
                    request.wait().unwrap().result.unwrap_err().code,
                    EmbeddedErrorCode::Unsupported
                );
            }
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            journal
                .get(&registry.runtime_id, operation.id())
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        owner.advance(OperationPhase::Cleaning).unwrap();
        owner
            .complete(Ok(Value::Null), EffectState::NotStarted)
            .unwrap();
    }
}

/// Register one mutating capability with exact `runtime_id`, `execution` and optional native implementation.
/// 按精确 `runtime_id`、`execution` 及可选原生实现注册一项可变更能力。
fn capabilities(
    runtime_id: &str,
    execution: CapabilityExecution,
    native: Option<NativeCapability>,
) -> Arc<CapabilityRegistry> {
    // Admission limits match the operation registry fixture.
    // 入场上限与操作注册表夹具一致。
    let capabilities =
        CapabilityRegistry::new(runtime_id.into(), crate::runtime::embedded::tests::config())
            .unwrap();
    capabilities
        .register(vec![CapabilityRegistrationRequest {
            descriptor: CapabilityDescriptor {
                name: "test.persisted_write".into(),
                version: "1.0.0".into(),
                description: "Persisted host mutation fixture".into(),
                input_schema: json!(true),
                output_schema: json!(true),
                execution,
                permissions: BTreeSet::new(),
                scope: CapabilityScope::Invocation,
                max_concurrent: 1,
                max_call_ms: OBSERVE.as_millis() as u64,
                max_input_bytes: 1024,
                max_output_bytes: 1024,
                effects: CapabilityEffects::Mutating,
                idempotency: CapabilityIdempotency::None,
            },
            native,
        }])
        .unwrap();
    capabilities
}

/// Construct trusted fixture context from the exact registry and admitted operation.
/// 根据精确注册表及已入场操作构造可信夹具上下文。
fn identity(registry: &OperationRegistry, operation: &OperationHandle) -> CapabilityCaller {
    CapabilityCaller {
        runtime_id: registry.runtime_id.clone(),
        plugin_id: "trusted-plugin".into(),
        package_generation: "package-a".into(),
        execution_revision: "revision-a".into(),
        security_partition: "workspace-a".into(),
        operation_id: operation.id().into(),
        session_id: None,
        workspace_root: None,
    }
}

/// Submit a queued call using the original owner control and exact admitted identity.
/// 使用原始所有者控制及精确入场身份提交队列调用。
fn queued_call(
    capabilities: &CapabilityRegistry,
    registry: &OperationRegistry,
    operation: &OperationHandle,
    owner: &OperationOwner,
) -> HostRequestHandle {
    capabilities
        .snapshot()
        .unwrap()
        .submit_queued(
            "test.persisted_write",
            identity(registry, operation),
            CapabilityPermissions::new(BTreeSet::new()).unwrap(),
            Value::Null,
            owner.control(),
        )
        .unwrap()
}

/// Invoke a native fixture with the exact original operation and no additional permission grants.
/// 使用精确原始操作且不添加额外授权调用原生夹具。
fn native_call(
    capabilities: &CapabilityRegistry,
    registry: &OperationRegistry,
    operation: &OperationHandle,
    owner: &OperationOwner,
) -> EmbeddedResult<CapabilityOutcome> {
    capabilities.snapshot()?.invoke_native(
        "test.persisted_write",
        identity(registry, operation),
        CapabilityPermissions::new(BTreeSet::new())?,
        Value::Null,
        owner.control(),
    )
}

/// Find one exact effect record without relying on a mutable vector position.
/// 查找单项精确副作用记录，不依赖会变化的向量位置。
fn effect<'a>(snapshot: &'a OperationSnapshot, id: &str) -> &'a HostEffectRecord {
    snapshot
        .host_effects
        .iter()
        .find(|effect| effect.effect_id == id)
        .expect("expected exact effect identity; fixture ownership may have changed")
}

/// Native code must observe its already committed intent before producing any business effect.
/// 原生代码必须在产生任何业务副作用之前观测到自身已提交意图。
#[test]
fn embedded_effect_intent_native_precedes_execution() {
    // Use the real queued backend and independent SQLite database.
    // 使用真实队列后端及独立 SQLite 数据库。
    let directory = Directory::new();
    // The callback reads this same database after the storage worker acknowledged its write.
    // 回调在存储线程确认写入后读取同一数据库。
    let journal = directory.journal(journal_config());
    // The owner is the only phase and revision authority.
    // 所有者是唯一阶段与修订权威。
    let (writer, registry) = queued_registry(&journal);
    // Record actual invocations, not attempted dispatch or submission.
    // 记录真实调用，不记录尝试分发或提交。
    let count = Arc::new(AtomicUsize::new(0));
    // The handler retains exact database ownership for its observation.
    // 处理器为其观测保留精确数据库所有权。
    let database = Arc::clone(&journal);
    // Share the actual execution counter with the trusted handler.
    // 与可信处理器共享真实执行计数。
    let called = Arc::clone(&count);
    // Each callback validates the disk intent before its own mutation counter advances.
    // 每次回调在自身变更计数前进之前校验磁盘意图。
    let capabilities = capabilities(
        &registry.runtime_id,
        CapabilityExecution::Native,
        Some(Arc::new(move |call| {
            // Derive every lookup identity from the core-bound invocation, not its arguments.
            // 全部查询身份来自核心绑定调用，不来自其参数。
            let stored = database
                .get(&call.caller.runtime_id, &call.caller.operation_id)
                .unwrap()
                .unwrap();
            // Native calls have a stable effect identity even though they have no SDK request identity.
            // 原生调用拥有稳定副作用身份，即使没有 SDK 请求身份。
            let evidence = effect(&stored.snapshot, call.effect_id.as_deref().unwrap());
            assert_eq!(stored.snapshot.phase, OperationPhase::Running);
            assert_eq!(evidence.phase, HostEffectPhase::Running);
            assert_eq!(evidence.effects, EffectState::Unknown);
            assert!(evidence.request_id.is_none());
            called.fetch_add(1, Ordering::SeqCst);
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            }
        })),
    );
    // Repeated calls have different retained effect identities in the same operation.
    // 同一操作中的重复调用拥有不同保留副作用身份。
    let (operation, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    for _ in 0..2 {
        assert!(
            native_call(&capabilities, &registry, &operation, &owner)
                .unwrap()
                .result
                .is_ok()
        );
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(
        journal
            .get(&registry.runtime_id, operation.id())
            .unwrap()
            .unwrap()
            .revision,
        3
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotStarted)
        .unwrap();
    // Terminal publication persists confirmed evidence from both real handlers.
    // 终态发布持久化两个真实处理器的已确认记录。
    let stored = journal
        .get(&registry.runtime_id, operation.id())
        .unwrap()
        .unwrap();
    assert_eq!(stored.revision, 5);
    assert_eq!(stored.snapshot.host_effects.len(), 2);
    assert!(
        stored
            .snapshot
            .host_effects
            .iter()
            .all(|record| record.effects == EffectState::Committed
                && record.phase == HostEffectPhase::Completed)
    );
    close(&writer);
}

/// A blocked real disk write must keep JSON delivery empty and the control channel responsive.
/// 真实磁盘写入阻塞必须使 JSON 交付保持为空，且控制通道继续响应。
#[test]
fn embedded_effect_intent_queued_poll_does_not_wait_for_disk() {
    // Separate operation execution from the thread that polls the SDK queue.
    // 将操作执行与轮询 SDK 队列的线程分开。
    let directory = Directory::new();
    // Block only actual SQLite work after the operation has entered Running.
    // 仅在操作已进入执行阶段后阻塞真实 SQLite 工作。
    let journal = directory.journal(journal_config());
    // Keep the writer alive until exact request ownership has drained.
    // 在精确请求所有权排空前保持写入者存活。
    let (writer, registry) = queued_registry(&journal);
    // Queued capabilities never install a native handler.
    // 队列能力不安装原生处理器。
    let capabilities = capabilities(&registry.runtime_id, CapabilityExecution::Queued, None);
    // Phase admission succeeds before applying the storage gate.
    // 施加存储门禁之前阶段入场成功。
    let (operation, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    // One original request carries its identity through every pending observation.
    // 单个原始请求跨全部待完成观测携带其身份。
    let request = queued_call(&capabilities, &registry, &operation, &owner);
    // The public broker remains independently queryable while disk work waits.
    // 磁盘工作等待期间，公开代理仍可独立查询。
    let broker = capabilities.host_requests();
    // Prevent the writer from completing the intent until the first queue observation returns.
    // 在首次队列观测返回前阻止写入者完成意图。
    let gate = journal.block_for_test();
    // Capture a timed observation before releasing the real storage gate.
    // 释放真实存储门禁前捕获限时观测。
    let (sent, received) = std::sync::mpsc::channel();
    // Use an independent consumer so a regression cannot hang the test behind its own storage lock.
    // 使用独立消费者，避免回归使测试挂在自身存储锁之后。
    let consumer = Arc::clone(&broker);
    // Thread ownership is joined even when the pre-release observation fails.
    // 即使释放前观测失败，也等待线程所有权退出。
    let worker = std::thread::spawn(move || sent.send(consumer.take_json(1, 4096)).unwrap());
    // The real blocking boundary is observed, rather than inferred from elapsed sleep.
    // 观测真实阻塞边界，不根据睡眠时长推断。
    let first = received.recv_timeout(OBSERVE);
    drop(gate);
    worker.join().unwrap();
    assert_eq!(first.unwrap().unwrap(), b"[]");
    assert_eq!(
        broker.status(request.id()).unwrap().phase,
        HostRequestPhase::Queued
    );
    // Poll the same request until its write has actually acknowledged.
    // 轮询同一请求，直到其写入确实确认。
    let mut delivered = None;
    poll(|| {
        delivered = broker.take(1)?.pop();
        Ok(delivered.is_some())
    })
    .unwrap();
    // The delivered request identity must match the persisted intent exactly.
    // 已交付请求身份必须与持久意图精确匹配。
    let delivered = delivered.unwrap();
    assert_eq!(delivered.request_id, request.id());
    // Read actual durable state after delivery, not a synthetic completion receipt.
    // 交付后读取真实持久状态，不读取合成完成回执。
    let stored = journal
        .get(&registry.runtime_id, operation.id())
        .unwrap()
        .unwrap();
    // Every wire request is linked to the sole owning operation effect.
    // 每个线协议请求都关联唯一所属操作副作用。
    let evidence = effect(&stored.snapshot, delivered.effect_id.as_deref().unwrap());
    assert_eq!(evidence.request_id.as_deref(), Some(request.id()));
    assert_eq!(evidence.phase, HostEffectPhase::Running);
    assert_eq!(evidence.effects, EffectState::Unknown);
    broker
        .complete(
            request.id(),
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    assert!(request.wait().unwrap().result.is_ok());
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotStarted)
        .unwrap();
    close(&writer);
}

/// Cancelling an intent before acknowledgement never publishes a handler or revives it during recovery.
/// 确认之前取消意图绝不发布处理器，也不在恢复期间将其复活。
#[test]
fn embedded_effect_intent_cancelled_pending_request_preserves_actual_lifecycle() {
    // Exercise cancellation against a real queued disk intent.
    // 针对真实队列磁盘意图执行取消。
    let directory = Directory::new();
    // Keep exact disk and operation namespace ownership throughout recovery.
    // 恢复全程保留精确磁盘及操作命名空间所有权。
    let journal = directory.journal(journal_config());
    // Both owner and broker share the same immutable persistence binding.
    // 所有者与代理共享同一不可变持久绑定。
    let (writer, registry) = queued_registry(&journal);
    // No actual SDK callback is delivered in this case.
    // 此场景不交付真实 SDK 回调。
    let capabilities = capabilities(&registry.runtime_id, CapabilityExecution::Queued, None);
    // The operation remains retained after its host request has been cancelled.
    // 宿主请求取消后，操作仍然保留。
    let (operation, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    // Bind request identity before the start-intent snapshot is captured.
    // 在捕获开始意图快照之前绑定请求身份。
    let request = queued_call(&capabilities, &registry, &operation, &owner);
    // The broker observes only metadata while this storage guard is held.
    // 持有此存储守卫期间代理只观测元数据。
    let broker = capabilities.host_requests();
    // Admission queues a real write but cannot complete it yet.
    // 入场排队真实写入，但此时无法完成。
    let gate = journal.block_for_test();
    assert!(broker.take(1).unwrap().is_empty());
    request.cancel().unwrap();
    assert_eq!(request.wait().unwrap().effects, EffectState::NotStarted);
    assert!(broker.take(1).unwrap().is_empty());
    drop(gate);
    // Owner recovery acknowledges intent without changing actual completed-before-start evidence.
    // 所有者恢复确认意图，不改变真实的开始前已完成证据。
    poll(|| owner.poll_advance(OperationPhase::Running)).unwrap();
    assert!(
        operation
            .snapshot()
            .unwrap()
            .host_effects
            .iter()
            .all(|record| record.phase == HostEffectPhase::Completed
                && record.effects == EffectState::NotStarted)
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotStarted)
        .unwrap();
    assert_eq!(
        journal
            .get(&registry.runtime_id, operation.id())
            .unwrap()
            .unwrap()
            .snapshot
            .effects,
        EffectState::NotStarted
    );
    close(&writer);
}

/// An abandoned in-flight intent must not starve a later request in the same operation.
/// 已放弃的在途意图不能使同一操作的后续请求无法推进。
#[test]
fn embedded_effect_intent_cancelled_predecessor_allows_next_request() {
    // Use two sequential request owners and one operation mutation gate.
    // 使用两个顺序请求所有者及一个操作变更门禁。
    let directory = Directory::new();
    // Database history retains both request identities.
    // 数据库历史保留两个请求身份。
    let journal = directory.journal(journal_config());
    // The queue backend bounds each immutable write attempt.
    // 队列后端限制每个不可变写入尝试。
    let (writer, registry) = queued_registry(&journal);
    // Cancellation releases the first actual admission before admitting the second.
    // 取消先释放第一个真实入场，再接纳第二个。
    let capabilities = capabilities(&registry.runtime_id, CapabilityExecution::Queued, None);
    // The same control remains valid; request cancellation does not cancel the operation.
    // 同一控制保持有效；请求取消不取消操作。
    let (operation, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    // This first request leaves a real, still-owned disk receipt behind.
    // 此第一个请求留下真实且仍被拥有的磁盘回执。
    let first = queued_call(&capabilities, &registry, &operation, &owner);
    // Delivery is observed independently from disk completion.
    // 交付观测独立于磁盘完成。
    let broker = capabilities.host_requests();
    // The first intent cannot finish before the first request is cancelled.
    // 第一个请求取消前，第一个意图无法完成。
    let gate = journal.block_for_test();
    assert!(broker.take(1).unwrap().is_empty());
    first.cancel().unwrap();
    assert!(first.wait().unwrap().result.is_err());
    drop(gate);
    // Later polling must drain the old receipt before capturing the next intent.
    // 后续轮询必须先排空旧回执，再捕获下一意图。
    let second = queued_call(&capabilities, &registry, &operation, &owner);
    // Only the second request may become deliverable.
    // 只有第二个请求可以变为可交付。
    let mut delivered = None;
    poll(|| {
        delivered = broker.take(1)?.pop();
        Ok(delivered.is_some())
    })
    .unwrap();
    assert_eq!(delivered.unwrap().request_id, second.id());
    // Disk captures actual cancellation of the predecessor and permission for the second handler.
    // 磁盘捕获前驱真实取消及第二个处理器执行许可。
    let stored = journal
        .get(&registry.runtime_id, operation.id())
        .unwrap()
        .unwrap();
    assert_eq!(stored.snapshot.host_effects.len(), 2);
    assert!(
        stored
            .snapshot
            .host_effects
            .iter()
            .any(|record| record.request_id.as_deref() == Some(first.id())
                && record.phase == HostEffectPhase::Completed
                && record.effects == EffectState::NotStarted)
    );
    broker
        .complete(
            second.id(),
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    assert!(second.wait().unwrap().result.is_ok());
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotStarted)
        .unwrap();
    close(&writer);
}

/// Exhausted write admission must prevent execution and require an explicit retry of the original intent.
/// 写入入场耗尽必须阻止执行，并要求显式重试原始意图。
#[test]
fn embedded_effect_intent_failure_recovery_never_replays_handler() {
    // A real receipt quota failure leaves a retained original candidate.
    // 真实回执配额失败留下保留的原始候选。
    let directory = Directory::new();
    // Other records occupy writer receipts, not this operation's ledger budget.
    // 其他记录占用写入者回执，不占用此操作账本预算。
    let journal = directory.journal(OperationJournalConfig {
        max_database_bytes: 128 * 1024,
        ..journal_config()
    });
    // The writer's configured quota is the sole authority for receipt saturation.
    // 写入者已配置配额是回执饱和的唯一权威。
    let (writer, registry) = queued_registry(&journal);
    // Every actual callback would increment this counter.
    // 每个真实回调都会递增此计数。
    let count = Arc::new(AtomicUsize::new(0));
    // Share counter ownership independently of the registry lifetime.
    // 独立于注册表生命周期共享计数所有权。
    let called = Arc::clone(&count);
    // Business mutation must never occur during either failure or recovery.
    // 失败及恢复期间均不得发生业务变更。
    let capabilities = capabilities(
        &registry.runtime_id,
        CapabilityExecution::Native,
        Some(Arc::new(move |_| {
            called.fetch_add(1, Ordering::SeqCst);
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            }
        })),
    );
    // Persist the normal execution phase before consuming write receipts.
    // 消耗写入回执之前持久化正常执行阶段。
    let (operation, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    // Retained completed receipts consume the exact configured queue quota.
    // 保留的已完成回执消耗精确已配置队列配额。
    let mut held = Vec::new();
    loop {
        // Each unrelated row owns a unique identity and one receipt.
        // 每个无关行拥有唯一身份及一个回执。
        let mut row = filler();
        row.operation_id = format!("filler-{}", held.len());
        // Wait for disk completion while deliberately retaining the completed receipt's capacity.
        // 等待磁盘完成，同时有意保留已完成回执的容量。
        let receipt = match writer.submit("fill-runtime", None, Arc::new(row)) {
            Ok(receipt) => receipt,
            Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => break,
            Err(error) => panic!("unexpected write admission failure: {error:?}"),
        };
        // A timed observation is not completion unless it carries the real committed revision.
        // 限时观测只有携带真实已提交修订时才证明完成。
        let acknowledged = receipt.wait(OBSERVE).unwrap();
        assert_eq!(acknowledged.phase, JournalWritePhase::Completed);
        assert!(acknowledged.error.is_none());
        assert!(acknowledged.revision.is_some());
        held.push(receipt);
    }
    assert_eq!(
        native_call(&capabilities, &registry, &operation, &owner)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    drop(held);
    // Restored capacity must not turn a new callback invocation into an implicit retry of old work.
    // 恢复容量不能将新回调调用变成旧工作的隐式重试。
    assert_eq!(
        native_call(&capabilities, &registry, &operation, &owner)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        journal
            .get(&registry.runtime_id, operation.id())
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    if !owner.retry_advance().unwrap() {
        poll(|| owner.poll_advance(OperationPhase::Running)).unwrap();
    }
    // Recovery writes the original one-effect candidate, not the newer two-effect live projection.
    // 恢复写入原始单副作用候选，不写入较新的双副作用实时投影。
    let stored = journal
        .get(&registry.runtime_id, operation.id())
        .unwrap()
        .unwrap();
    assert_eq!(stored.snapshot.host_effects.len(), 1);
    assert_eq!(operation.snapshot().unwrap().host_effects.len(), 2);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotStarted)
        .unwrap();
    assert_eq!(
        operation.snapshot().unwrap().effects,
        EffectState::NotStarted
    );
    close(&writer);
}

/// Cancellation and registration revocation during disk waiting must be rechecked before native execution.
/// 磁盘等待期间的取消及注册撤销必须在原生执行前重新检查。
#[test]
fn embedded_effect_intent_native_rechecks_authority_after_disk() {
    // Exercise revocation both alone and together with operation cancellation.
    // 分别执行单独撤销及同时取消操作的场景。
    for cancel_operation in [false, true] {
        // Keep all observation outside the blocked journal mutex.
        // 将全部观测保持在被阻塞日志互斥锁之外。
        let directory = Directory::new();
        // Only the real writer is paused by this fixture's storage gate.
        // 此夹具存储门禁只暂停真实写入者。
        let journal = directory.journal(journal_config());
        // Fixed native execution waits without owning broker or scheduler metadata.
        // 固定原生执行等待时不拥有代理或调度元数据。
        let (writer, registry) = queued_registry(&journal);
        // Record actual business execution independently of intent admission.
        // 独立于意图入场记录真实业务执行。
        let count = Arc::new(AtomicUsize::new(0));
        // Callback ownership may drain only after the waiting native invocation returns.
        // 等待中的原生调用返回后，回调所有权才可以排空。
        let called = Arc::clone(&count);
        // The handler should never be entered in this scenario.
        // 此场景绝不应进入处理器。
        let capabilities = capabilities(
            &registry.runtime_id,
            CapabilityExecution::Native,
            Some(Arc::new(move |_| {
                called.fetch_add(1, Ordering::SeqCst);
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::Committed,
                }
            })),
        );
        // Enter Running before deliberately stalling dispatch persistence.
        // 有意暂停分发持久化前进入执行阶段。
        let (operation, mut owner) = admit(&registry);
        owner.advance(OperationPhase::Running).unwrap();
        // Freeze exact caller identity before sending the callback to its execution thread.
        // 将回调发送到执行线程前冻结精确调用方身份。
        let caller = identity(&registry, &operation);
        // Snapshot selection remains fixed despite later registry revocation.
        // 即使注册表稍后撤销，快照选择仍固定。
        let snapshot = capabilities.snapshot().unwrap();
        // Preserve the original control for concurrent cancellation.
        // 为并发取消保留原始控制对象。
        let control = owner.control();
        // Actual storage is released only after cancellation has been requested.
        // 只有请求取消后才释放真实存储。
        let gate = journal.block_for_test();
        // Join the actual execution thread after releasing its storage dependency.
        // 释放执行线程的存储依赖后等待其真实退出。
        let callback = std::thread::spawn(move || {
            snapshot.invoke_native(
                "test.persisted_write",
                caller,
                CapabilityPermissions::new(BTreeSet::new()).unwrap(),
                Value::Null,
                control,
            )
        });
        poll(|| Ok(writer.status()?.writing)).unwrap();
        if cancel_operation {
            operation.cancel().unwrap();
        }
        // Derive the exact registration from its retained effect rather than guessing a sequence number.
        // 从保留副作用派生精确注册身份，不猜测序号。
        let registration = operation
            .snapshot()
            .unwrap()
            .host_effects
            .into_iter()
            .find(|record| record.capability_name == "test.persisted_write")
            .expect("expected retained fixture capability; its registration may have changed")
            .registration_id;
        assert!(!capabilities.unregister(&registration).unwrap().drained);
        drop(gate);
        assert_eq!(
            callback.join().unwrap().unwrap_err().code,
            if cancel_operation {
                EmbeddedErrorCode::Cancelled
            } else {
                EmbeddedErrorCode::Closed
            }
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        owner.advance(OperationPhase::Cleaning).unwrap();
        owner
            .complete(
                Err(EmbeddedError::new(
                    EmbeddedErrorCode::Cancelled,
                    "cancelled before native dispatch",
                )),
                EffectState::NotStarted,
            )
            .unwrap();
        assert_eq!(
            operation.snapshot().unwrap().effects,
            EffectState::NotStarted
        );
        assert!(capabilities.status(&registration).unwrap().drained);
        close(&writer);
    }
}
