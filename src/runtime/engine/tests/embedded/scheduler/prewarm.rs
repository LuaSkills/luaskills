//! Real initialization, cancellation and ownership evidence for explicit additional-instance prewarming.
//! 明确额外实例预热的真实初始化、取消及归属证据。

use super::*;

/// Ready snapshots apply expiration without consuming the original declared warm floor.
/// 就绪快照应用过期规则，且不消耗原声明预热下限。
#[test]
fn embedded_prewarm_readiness_expires_surplus_and_preserves_floor() {
    // Real module state is retained by the formal scheduler, with one protected idle instance.
    // 正式调度器保留真实模块状态，其中一个空闲实例受保护。
    let layout = SystemRuntimeTestLayout::new("formal readiness expiration");
    let runtime = runtime(&layout, pool_config());
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 1;
    policy.max_resident_vms = 2;
    policy.idle_ttl_ms = Some(50);
    let pool = runtime
        .register_pool(
            definition(&layout, "return {call=function() return true end}"),
            policy,
            permissions(),
            "readiness-expiration".into(),
        )
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            runtime
                .prewarm_instance(request(&pool), OBSERVE)
                .unwrap()
                .wait(OBSERVE)
                .unwrap()
                .phase,
            OperationPhase::Succeeded
        );
    }
    // The existing test observation budget bounds physical retirement independently of the short idle TTL.
    // 既有测试观测预算独立于短空闲时长，约束物理退役。
    let deadline = std::time::Instant::now() + OBSERVE;
    while runtime.reusable_pool_status(&pool).unwrap().ready != 1
        || runtime.pool_resources(&pool).unwrap().resident != 1
    {
        assert!(
            std::time::Instant::now() < deadline,
            "surplus did not retire"
        );
        std::thread::yield_now();
    }
    assert_eq!(runtime.reusable_pool_status(&pool).unwrap().ready, 1);
    runtime.close_pool(&pool).unwrap();
    assert_eq!(runtime.reusable_pool_status(&pool).unwrap().ready, 0);
    assert_eq!(
        runtime
            .reusable_pool_status("unknown-pool")
            .unwrap_err()
            .code,
        EmbeddedErrorCode::NotFound
    );
    shutdown(&runtime);
}
use crate::RuntimeRequestContext;

/// Bound fixture observation and execution; production budgets remain runtime-owned.
/// 约束夹具观测及执行；生产预算仍归运行时所有。
const OBSERVE: Duration = Duration::from_secs(5);

/// Build one explicit additional-instance request for pool; return trusted host correlation without a fake export.
/// 为 pool 构造一个明确额外实例请求；返回可信宿主关联，不伪造导出。
fn request(pool: &str) -> EmbeddedPrewarm {
    EmbeddedPrewarm {
        pool_id: pool.into(),
        context: LuaInvocationContext {
            request_context: Some(RuntimeRequestContext {
                request_id: Some("host-prewarm".into()),
                ..RuntimeRequestContext::default()
            }),
            ..LuaInvocationContext::default()
        },
    }
}

/// Register a queued initialization observer in runtime; tests explicitly acknowledge every actual callback.
/// 在 runtime 注册排队初始化观察器；测试明确确认每个真实回调。
fn register_observer(runtime: &EmbeddedRuntime) {
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.init",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .expect("register initialization observer");
}

/// Complete the exact observed request in runtime; return no value and preserve read-only effect semantics.
/// 在 runtime 完成精确观察请求；不返回值，保留只读副作用语义。
fn acknowledge(runtime: &EmbeddedRuntime, request: &HostRequest) {
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::NotApplicable,
            },
        )
        .expect("acknowledge actual initialization callback");
}

/// Each successful prewarm creates a distinct real VM; full-pool rejection leaves ordinary reuse live.
/// 每次成功预热创建不同真实 VM；满池拒绝后普通复用仍可推进。
#[test]
fn embedded_prewarm_creates_distinct_instances_without_business_execution() {
    // Own the real scheduler and its explicitly authorized initialization callback.
    // 持有真实调度器及其明确授权的初始化回调。
    let layout = SystemRuntimeTestLayout::new("formal prewarm distinct instances");
    let runtime = runtime(&layout, pool_config());
    register_observer(&runtime);
    // Leave parent headroom so the rejected third request proves the exact pool limit.
    // 保留父级余量，使第三次拒绝证明精确池上限。
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_resident_vms = 2;
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                r#"
        -- Initialization emits one real host observation per VM.
        -- 每个 VM 的初始化发出一次真实宿主观察。
        assert(vulcan.host.call('test.init', 'initialization').ok)
        -- Only the exported function changes this business counter.
        -- 仅导出函数修改此业务计数器。
        local count = 0
        return {call=function() count=count+1; return count end}
    "#,
            ),
            policy,
            permissions(),
            "prewarm-revision".into(),
        )
        .unwrap();
    // Compare native allocation identities rather than inferring VM count from successful calls.
    // 比较原生分配身份，不根据成功调用次数推断 VM 数量。
    let mut instances = BTreeSet::new();
    for _ in 0..2 {
        // Preserve this operation's immutable admission before the callback is acknowledged.
        // 在确认回调前保留此操作的不可变入场信息。
        let operation = runtime.prewarm_instance(request(&pool), OBSERVE).unwrap();
        let callback = host_request(&runtime);
        let snapshot = operation.snapshot().unwrap();
        // A real initialization callback owns one unavailable instance while prior confirmed instances remain ready.
        // 真实初始化回调拥有一个不可用实例，同时先前已确认实例保持就绪。
        let readiness = runtime.reusable_pool_status(&pool).unwrap();
        assert_eq!(readiness.ready, instances.len());
        assert_eq!(readiness.unavailable, 1);
        assert_eq!(snapshot.phase, OperationPhase::WaitingForHost);
        let OperationContext::Module(context) = &snapshot.context else {
            panic!("prewarm must retain a bound module context");
        };
        assert!(context.prewarm);
        assert!(context.export.is_none());
        assert!(context.caller.session_id.is_none());
        assert_eq!(context.caller.request_id.as_deref(), Some("host-prewarm"));
        assert_eq!(context.pool_id, pool);
        assert_eq!(callback.caller, context.caller);
        acknowledge(&runtime, &callback);
        // Success becomes observable only after initialized ownership is retained by the formal scheduler.
        // 仅正式调度器保留已初始化归属后，成功才可被观察。
        let completed = operation.wait(OBSERVE).unwrap();
        assert_eq!(
            completed.phase,
            OperationPhase::Succeeded,
            "{:?}",
            completed.error
        );
        assert_eq!(completed.context, snapshot.context);
        let identity = completed.value.unwrap()["instance_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            instances.insert(identity),
            "each prewarm must create a new VM"
        );
    }
    assert_eq!(
        runtime.pool_resources(&pool).unwrap().resident,
        instances.len()
    );
    assert_eq!(
        runtime.reusable_pool_status(&pool).unwrap().ready,
        instances.len()
    );
    // An impossible additional allocation must fail without blocking a later ordinary borrower.
    // 不可能的额外分配必须失败，不能阻塞后续普通借用者。
    let rejected = runtime
        .prewarm_instance(request(&pool), OBSERVE)
        .unwrap()
        .wait(OBSERVE)
        .unwrap();
    assert_eq!(rejected.phase, OperationPhase::Failed);
    assert_eq!(
        rejected.error.unwrap().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    for count in 1..=2 {
        let business = runtime
            .submit(call(&pool, Value::Null), OBSERVE)
            .unwrap()
            .wait(OBSERVE)
            .unwrap();
        assert_eq!(business.value, Some(json!(count)));
    }
    assert!(
        runtime
            .capabilities()
            .host_requests()
            .take(1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        runtime.pool_resources(&pool).unwrap().resident,
        instances.len()
    );
    shutdown(&runtime);
    assert_eq!(runtime.resources().unwrap().resident, 0);
}

/// Queue cancellation and deadlines release bytes; runtime close still waits for actual in-flight host acknowledgement.
/// 队列取消及截止时间释放字节；运行时关闭仍等待真实在途宿主确认。
#[test]
fn embedded_prewarm_queue_cancel_timeout_and_close_preserve_ownership() {
    // Restore the process logger after this real queue cancellation and deadline fixture finishes.
    // 此真实排队取消及截止夹具结束后恢复进程日志器。
    let _logger = super::diagnostics::RestoreLogger::capture();
    // Keep exact phase evidence for admitted calls, including requests that never receive a VM.
    // 保留已入场调用的精确阶段证据，包含从未取得 VM 的请求。
    let observations = Arc::new(Mutex::new(Vec::<Value>::new()));
    // Capture only this existing private event protocol; ordinary runtime log messages are unrelated.
    // 仅捕获此既有私有事件协议；普通运行时日志消息无关。
    let captured = Arc::clone(&observations);
    crate::runtime::logging::set_log_callback(Some(Arc::new(move |event| {
        if let Ok(value) = serde_json::from_str::<Value>(&event.message)
            && value["luaskills_embedded_diagnostic"] == 1
        {
            captured.lock().unwrap().push(value);
        }
    })));
    // One execution slot makes the remaining prewarm requests deterministically queued.
    // 单个执行槽使剩余预热请求确定地排队。
    let layout = SystemRuntimeTestLayout::new("formal prewarm queue and close");
    let mut config = pool_config();
    config.max_running_calls = 1;
    let runtime = runtime(&layout, config);
    register_observer(&runtime);
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_running_calls = 1;
    let pool = runtime.register_pool(definition(&layout,
        "assert(vulcan.host.call('test.init','initialization').ok); return {call=function() return true end}"),
        policy, permissions(), "prewarm-close".into()).unwrap();
    // The delivered callback proves actual initialization and stays owned through cancellation.
    // 已交付回调证明真实初始化，且跨取消持续被拥有。
    let running = runtime.prewarm_instance(request(&pool), OBSERVE).unwrap();
    let callback = host_request(&runtime);
    let queued_request = request(&pool);
    let bytes = serde_json::to_vec(&queued_request).unwrap().len();
    let cancelled = runtime.prewarm_instance(queued_request, OBSERVE).unwrap();
    assert_eq!(cancelled.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(runtime.usage().unwrap().queued_bytes, bytes);
    assert!(cancelled.cancel().unwrap());
    assert_eq!(
        cancelled.wait(OBSERVE).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(runtime.usage().unwrap().queued_bytes, 0);
    // A separate short execution deadline expires while the same real callback occupies the only slot.
    // 同一真实回调占用唯一执行槽时，另一独立短执行截止时间到期。
    let expired = runtime
        .prewarm_instance(request(&pool), Duration::from_millis(50))
        .unwrap();
    // Deadline failure uses the existing failed phase and its explicit error code.
    // 截止失败使用既有失败阶段及其明确错误码。
    let expired_result = expired.wait(OBSERVE).unwrap();
    assert_eq!(expired_result.phase, OperationPhase::Failed);
    assert_eq!(
        expired_result.error.unwrap().code,
        EmbeddedErrorCode::DeadlineExceeded
    );
    assert_eq!(runtime.usage().unwrap().queued_bytes, 0);
    assert_eq!(runtime.resources().unwrap().resident, 1);
    assert!(
        runtime
            .capabilities()
            .host_requests()
            .take(1)
            .unwrap()
            .is_empty()
    );
    runtime.request_close().unwrap();
    let readiness = runtime.reusable_pool_status(&pool).unwrap();
    assert!(readiness.closing);
    assert_eq!(readiness.ready, 0);
    assert!(!runtime.poll_closed().unwrap());
    assert!(!running.snapshot().unwrap().phase.is_terminal());
    acknowledge(&runtime, &callback);
    assert_eq!(
        running.wait(OBSERVE).unwrap().phase,
        OperationPhase::Cancelled
    );
    shutdown(&runtime);
    assert_eq!(runtime.resources().unwrap().resident, 0);
    // Every actual queue removal is emitted once; cancelled and expired requests claim no allocated VM.
    // 每次真实队列移除仅发送一次；取消及过期请求不声称已分配 VM。
    let observed = observations.lock().unwrap();
    for operation in [&running, &cancelled, &expired] {
        // Select immutable identity and semantic phase rather than an evolving observation position.
        // 选择不可变身份及语义阶段，而非演进中的观测位置。
        let queue = observed
            .iter()
            .filter(|value| value["operation_id"] == operation.id() && value["phase"] == "queue")
            .collect::<Vec<_>>();
        assert_eq!(
            queue.len(),
            1,
            "exactly one real queue interval is required"
        );
        assert!(queue[0]["elapsed_ns"].as_u64().is_some());
        assert!(queue[0]["instance_id"].is_null());
    }
    // Requests rejected while queued must never produce allocation, initialization or business observations.
    // 排队时被拒绝的请求绝不产生分配、初始化或业务观测。
    for operation in [&cancelled, &expired] {
        assert!(
            observed
                .iter()
                .filter(|value| value["operation_id"] == operation.id())
                .all(|value| value["phase"] == "queue")
        );
    }
}

/// Single-use and fixed-session pools cannot be implicitly converted into reusable prewarm pools.
/// 单次及固定会话池不能隐式转换为可复用预热池。
#[test]
fn embedded_prewarm_rejects_other_reuse_modes_before_initialization() {
    // Deliberately invalid Lua proves rejection happens before source execution.
    // 故意非法的 Lua 证明拒绝发生在源码执行之前。
    let layout = SystemRuntimeTestLayout::new("formal prewarm reuse policy");
    let runtime = runtime(&layout, pool_config());
    for reuse in [InstanceReuse::SingleCall, InstanceReuse::Session] {
        let pool = runtime
            .register_pool(
                definition(&layout, "error('must never execute')"),
                pool_policy(reuse),
                permissions(),
                "ineligible".into(),
            )
            .unwrap();
        assert_eq!(
            runtime.reusable_pool_status(&pool).unwrap_err().code,
            EmbeddedErrorCode::InvalidArgument
        );
        assert_eq!(
            runtime
                .prewarm_instance(request(&pool), OBSERVE)
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
    }
    assert_eq!(runtime.resources().unwrap().resident, 0);
    assert!(
        runtime
            .list_operations(None, None, 1)
            .unwrap()
            .operation_ids
            .is_empty()
    );
    shutdown(&runtime);
}

/// A failed initializer retires the actual allocation before publishing failure and can be explicitly retried.
/// 初始化器失败后先退役实际分配再发布失败，并允许明确重试。
#[test]
fn embedded_prewarm_failed_initialization_releases_real_capacity() {
    // A real Lua exception exercises allocation cleanup without a simulated worker result.
    // 真实 Lua 异常覆盖分配清理，不模拟工作线程结果。
    let layout = SystemRuntimeTestLayout::new("formal prewarm failed initialization");
    let runtime = runtime(&layout, pool_config());
    let pool = runtime
        .register_pool(
            definition(&layout, "error('prewarm initialization failed')"),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "failed-init".into(),
        )
        .unwrap();
    for _ in 0..2 {
        let failed = runtime
            .prewarm_instance(request(&pool), OBSERVE)
            .unwrap()
            .wait(OBSERVE)
            .unwrap();
        assert_eq!(failed.phase, OperationPhase::Failed);
        assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 0);
    }
    shutdown(&runtime);
}

/// Failed prewarming releases the independent finalization reservation without publishing a spurious close operation.
/// 预热失败释放独立关闭预留，不发布虚假的关闭操作。
#[test]
fn embedded_prewarm_failed_initialization_releases_finalization_reservation() {
    // The declared finalizer reserves future operation capacity before any source executes.
    // 声明的终结器在任何源码执行前预留未来操作容量。
    let layout = SystemRuntimeTestLayout::new("formal prewarm reserved finalization");
    let runtime = runtime(&layout, pool_config());
    let pool = runtime
        .register_pool(
            super::finalization::closing_definition(
                &layout,
                "error('prewarm initializer fails before exports exist')",
                OBSERVE.as_millis().try_into().unwrap(),
            ),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "failed-reserved-init".into(),
        )
        .unwrap();
    let failed = runtime.prewarm_instance(request(&pool), OBSERVE).unwrap();
    assert_eq!(failed.wait(OBSERVE).unwrap().phase, OperationPhase::Failed);
    // Reservation release is supervisor-owned and may follow terminal publication without running Lua again.
    // 预留释放归监督器所有，可在终态发布之后发生，但不会再次运行 Lua。
    let deadline = Instant::now() + OBSERVE;
    while runtime
        .plugin(&layout.package_id)
        .unwrap()
        .reserved_operations
        != 0
    {
        assert!(
            Instant::now() < deadline,
            "unused prewarm finalization reservation leaked"
        );
        std::thread::yield_now();
    }
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 0);
    let page = runtime.list_operations(Some(&pool), None, 1).unwrap();
    assert_eq!(page.operation_ids, vec![failed.id().to_owned()]);
    assert!(!page.has_more);
    shutdown(&runtime);
}

/// A never-invoked warmed VM still runs its declared finalizer with original module and allocation identities.
/// 从未调用业务的已预热 VM 仍使用原模块及分配身份执行声明终结器。
#[test]
fn embedded_prewarm_close_preserves_independent_finalization_authority() {
    // Only shutdown enters the queued observer; prewarm initialization itself needs no callback.
    // 仅关闭进入排队观察器；预热初始化本身不需要回调。
    let layout = SystemRuntimeTestLayout::new("formal prewarm independent finalization");
    let runtime = runtime(&layout, pool_config());
    register_observer(&runtime);
    let pool = runtime.register_pool(super::finalization::closing_definition(&layout,
        "return {call=function() error('business must not execute') end, shutdown=function() assert(vulcan.host.call('test.init','shutdown').ok); return true end}",
        OBSERVE.as_millis().try_into().unwrap()),
        pool_policy(InstanceReuse::Reusable), permissions(), "prewarm-finalizer".into()).unwrap();
    let opening = runtime.prewarm_instance(request(&pool), OBSERVE).unwrap();
    let opened = opening.wait(OBSERVE).unwrap();
    assert_eq!(opened.phase, OperationPhase::Succeeded);
    let instance_id = opened.value.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.close_pool(&pool).unwrap();
    // The independent operation is observable through the actual dispatched host callback identity.
    // 可通过实际分发宿主回调身份观察独立操作。
    let callback = host_request(&runtime);
    assert_ne!(callback.caller.operation_id, opening.id());
    // Independent finalization must not inherit the prewarm operation's host request correlation.
    // 独立终结不得继承预热操作的宿主请求关联。
    assert!(callback.caller.request_id.is_none());
    let closing = runtime.operation(&callback.caller.operation_id).unwrap();
    let snapshot = closing.snapshot().unwrap();
    let OperationContext::Module(context) = snapshot.context else {
        panic!("prewarmed VM finalizer must retain a bound module context");
    };
    assert!(!context.prewarm);
    assert_eq!(
        context.finalization_instance_id.as_deref(),
        Some(instance_id.as_str())
    );
    assert_eq!(context.export.as_deref(), Some("shutdown"));
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
    assert_eq!(opening.snapshot().unwrap().phase, OperationPhase::Succeeded);
    acknowledge(&runtime, &callback);
    assert_eq!(
        closing.wait(OBSERVE).unwrap().phase,
        OperationPhase::Succeeded
    );
    shutdown(&runtime);
    assert_eq!(runtime.resources().unwrap().resident, 0);
}
