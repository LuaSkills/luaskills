use super::*;
use crate::runtime::embedded::EmbeddedResult;

/// Independent VMs on one engine must overlap in a real host wait while keeping both permits charged.
/// 同一引擎上的独立 VM 必须在真实宿主等待中重叠，同时保持两个许可记账。
#[test]
fn embedded_pool_parallel_host_wait_keeps_actual_capacity() {
    use crate::runtime::embedded::EffectState;
    use crate::runtime::embedded::capabilities::{
        CapabilityExecution, CapabilityOutcome, CapabilityRegistrationRequest, CapabilityRegistry,
    };
    // Channels prove both actual native handlers overlap while the parent counts both VMs.
    // 通道证明两个真实原生处理器重叠，同时父级统计两个 VM。
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (first_tx, first_rx) = std::sync::mpsc::channel();
    let (second_tx, second_rx) = std::sync::mpsc::channel();
    // Each callback has an independent release barrier.
    // 每个回调具有独立释放屏障。
    let releases = [Mutex::new(first_rx), Mutex::new(second_rx)];
    let layout = SystemRuntimeTestLayout::new("embedded concurrent pool");
    let manager = pool_manager(&layout);
    // The instance registry replaces the previous process-global probe.
    // 实例注册表替代此前的进程全局探针。
    let registry =
        CapabilityRegistry::new("parallel-runtime".into(), manager.config().clone()).unwrap();
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor("pool.probe", CapabilityExecution::Native),
            native: Some(Arc::new(move |request| {
                // Fixture input is validated before selecting an exact synchronization slot.
                // 选择精确同步槽前校验夹具输入。
                let index = request.arguments["index"]
                    .as_u64()
                    .filter(|index| *index < 2)
                    .unwrap() as usize;
                entered_tx.send(index).unwrap();
                releases[index]
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                CapabilityOutcome {
                    result: Ok(json!(index)),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    // Every VM binds the same immutable snapshot while retaining independent Lua state.
    // 每个 VM 绑定同一个不可变快照，同时保留独立 Lua 状态。
    let (_, capabilities) = super::capabilities::binding(&registry);
    let pool = manager.create_pool_with_capabilities("parallel".into(), definition(&layout,
        "return {call=function(a) local r=vulcan.host.call('pool.probe',a); if not r.ok then error(r.error.message) end; return r.value end}"),
        pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    // Prepare both instances before either worker blocks inside a real host callback.
    // 在任一工作线程阻塞于真实宿主回调前准备两个实例。
    let leases = [
        pool.acquire(control()).unwrap(),
        pool.acquire(control()).unwrap(),
    ];
    let workers = leases
        .into_iter()
        .enumerate()
        .map(|(index, mut lease)| {
            std::thread::spawn(move || {
                lease.invoke(ModuleInvocation {
                    operation_id: "parallel-operation",
                    session_id: None,
                    export: "call",
                    arguments: &json!({"index":index}),
                    context: &LuaInvocationContext::default(),
                    control: control(),
                })
            })
        })
        .collect::<Vec<_>>();
    assert_ne!(
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap()
    );
    assert_eq!(manager.usage().unwrap().running, 2);
    pool.close().unwrap();
    assert_eq!(
        pool.usage().unwrap().resident,
        2,
        "close must retain both blocked native calls"
    );
    first_tx.send(()).unwrap();
    second_tx.send(()).unwrap();
    for (index, worker) in workers.into_iter().enumerate() {
        assert_eq!(worker.join().unwrap().unwrap(), json!(index));
    }
    drained(&pool);
}
use crate::runtime::embedded::{
    EmbeddedPoolManager, EmbeddedRuntimeConfig, ExecutionBackend, InstanceReuse, ModuleLease,
    ModulePool, PluginPoolConfig, PoolKind,
};

/// Small explicit limits expose parent capacity, reservation and retirement behavior.
/// 小规模显式上限暴露父级容量、预留与退役行为。
pub(super) fn pool_manager(layout: &SystemRuntimeTestLayout) -> Arc<EmbeddedPoolManager> {
    // One engine is deliberately shared by every independently allocated VM.
    // 所有独立分配的 VM 刻意共享同一个引擎。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    EmbeddedPoolManager::new(
        engine,
        EmbeddedRuntimeConfig {
            max_registered_pools: 8,
            max_registered_capabilities: 8,
            max_resident_vms: 3,
            max_running_calls: 2,
            max_queued_calls: 8,
            max_queued_bytes: 4096,
            max_operations: 16,
            max_host_requests: 4,
            max_host_request_bytes: 4096,
            max_value_bytes: 1024,
        },
    )
    .unwrap()
}

/// Return explicit `reuse` policy without implicit reuse or unbounded options.
/// 返回显式 `reuse` 策略，不隐式复用或使用无界选项。
pub(super) fn pool_policy(reuse: InstanceReuse) -> PluginPoolConfig {
    PluginPoolConfig {
        kind: PoolKind::Shared,
        min_resident_vms: 0,
        max_resident_vms: 3,
        max_running_calls: 2,
        max_queued_calls: 8,
        reuse,
        serial: false,
        backend: ExecutionBackend::InProcess,
        idle_ttl_ms: None,
        max_uses: None,
    }
}

/// Invoke the exact counter export in `lease` and return its JSON value.
/// 调用 `lease` 中精确计数导出并返回 JSON 值。
fn count(lease: &mut ModuleLease) -> EmbeddedResult<Value> {
    lease.invoke(ModuleInvocation {
        operation_id: "test-operation",
        session_id: None,
        export: "call",
        arguments: &Value::Null,
        context: &LuaInvocationContext::default(),
        control: control(),
    })
}

/// Wait for true resident destruction without treating an empty retirement queue as completion.
/// 等待真实常驻实例销毁，不将空退役队列视为完成。
pub(super) fn drained(pool: &ModulePool) {
    // Bounded deadline detects ownership leaks without requiring exact worker scheduling.
    // 有界截止时间检测所有权泄漏，不要求精确工作线程调度。
    let deadline = Instant::now() + Duration::from_secs(3);
    while pool.usage().unwrap().resident != 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        pool.usage().unwrap().resident,
        0,
        "actual VM ownership must drain"
    );
}

/// Reuse retains exactly one module's state; closing rejects new work and eventually frees capacity.
/// 复用保留精确单个模块的状态；关闭拒绝新任务并最终释放容量。
#[test]
fn embedded_pool_reuses_real_vm_and_closes_borrowed_generation() {
    // Counter state makes accidental source reloading observable.
    // 计数器状态使意外重新加载源码可被观察。
    let layout = SystemRuntimeTestLayout::new("embedded reusable pool");
    // Manager provides one aggregate governor and cleanup worker.
    // 管理器提供单个聚合治理器与清理线程。
    let manager = pool_manager(&layout);
    // Immutable pool captures code and the complete trusted context.
    // 不可变池捕获代码及完整可信上下文。
    let pool = manager
        .create_pool(
            "reusable".into(),
            definition(
                &layout,
                "local n=0; return {call=function() n=n+1; return n end}",
            ),
            pool_policy(InstanceReuse::Reusable),
        )
        .unwrap();
    // First exclusive checkout initializes once.
    // 首次独占借用仅初始化一次。
    let mut first = pool.acquire(control()).unwrap();
    // Identity must survive a real return and checkout cycle.
    // 身份必须跨越真实归还与借用周期保留。
    let identity = first.instance_id().unwrap().to_owned();
    assert_eq!(count(&mut first).unwrap(), json!(1));
    drop(first);
    assert_eq!(pool.usage().unwrap().idle, 1);
    // Second checkout must retain the original closure state.
    // 第二次借用必须保留原闭包状态。
    let mut second = pool.acquire(control()).unwrap();
    assert_eq!(second.instance_id().unwrap(), identity);
    assert_eq!(count(&mut second).unwrap(), json!(2));
    pool.close().unwrap();
    assert!(
        matches!(pool.acquire(control()), Err(error) if error.code == EmbeddedErrorCode::Closed)
    );
    assert_eq!(
        count(&mut second).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(pool.usage().unwrap().resident, 1);
    drop(second);
    drained(&pool);
    assert_eq!(manager.usage().unwrap().resident, 0);
}

/// Single-call instances cannot be reused through the same lease or a later checkout.
/// 单次实例不能通过同一租借或后续借用复用。
#[test]
fn embedded_pool_enforces_single_call_and_initialization_failure_cleanup() {
    // Package fixture uses production module and resource paths.
    // 包夹具使用生产模块与资源路径。
    let layout = SystemRuntimeTestLayout::new("embedded single pool");
    // Real manager, not a mocked pool counter.
    // 真实管理器，不使用模拟池计数器。
    let manager = pool_manager(&layout);
    // Stateless policy must remain stateless even if the Lua source itself has state.
    // 即使 Lua 源码自身有状态，无状态策略仍必须保持无状态。
    let pool = manager
        .create_pool(
            "single".into(),
            definition(
                &layout,
                "local n=0; return {call=function() n=n+1; return n end}",
            ),
            pool_policy(InstanceReuse::SingleCall),
        )
        .unwrap();
    for _ in 0..2 {
        // Each actual instance can execute exactly once.
        // 每个真实实例仅能执行一次。
        let mut lease = pool.acquire(control()).unwrap();
        assert_eq!(count(&mut lease).unwrap(), json!(1));
        assert_eq!(
            count(&mut lease).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        drop(lease);
        drained(&pool);
    }
    // Initialization failure still transfers the allocated VM to real retirement.
    // 初始化失败仍将已分配 VM 转交真实退役。
    let failed = manager
        .create_pool(
            "failed".into(),
            definition(&layout, "error('initialization failed')"),
            pool_policy(InstanceReuse::Reusable),
        )
        .unwrap();
    assert!(failed.acquire(control()).is_err());
    drained(&failed);
    assert_eq!(manager.usage().unwrap().running, 0);
}

/// Pinned sessions preserve private state and resident accounting while idle execution permits are free.
/// 固定会话保留私有状态与常驻记账，空闲时执行许可保持空闲。
#[test]
fn embedded_pool_sessions_pin_independent_instances() {
    // Session state is isolated even within one immutable package generation.
    // 即使属于同一不可变包代次，会话状态仍相互隔离。
    let layout = SystemRuntimeTestLayout::new("embedded session pool");
    // Shared manager verifies aggregate capacity without globally serializing calls.
    // 共享管理器验证聚合容量，且不对调用进行全局串行化。
    let manager = pool_manager(&layout);
    // Session policy forbids ordinary acquire and general idle reuse.
    // 会话策略禁止普通借用与通用空闲复用。
    let pool = manager
        .create_pool(
            "sessions".into(),
            definition(
                &layout,
                "local n=0; return {call=function() n=n+1; return n end}",
            ),
            pool_policy(InstanceReuse::Session),
        )
        .unwrap();
    assert!(pool.acquire(control()).is_err());
    // Separate pins must retain distinct identities and counters.
    // 不同固定实例必须保留不同身份与计数器。
    let mut first = pool.open_session(control()).unwrap();
    // Second session occupies resident capacity without holding a running permit.
    // 第二个会话占用常驻容量，且不持有运行许可。
    let mut second = pool.open_session(control()).unwrap();
    assert_ne!(first.instance_id().unwrap(), second.instance_id().unwrap());
    assert_eq!(pool.usage().unwrap().resident, 2);
    assert_eq!(pool.usage().unwrap().running, 0);
    assert_eq!(count(&mut first).unwrap(), json!(1));
    assert_eq!(count(&mut first).unwrap(), json!(2));
    assert_eq!(count(&mut second).unwrap(), json!(1));
    drop((first, second));
    drained(&pool);
}

/// Parent shutdown must retain pinned owners and reject all newly published generations.
/// 父级关闭必须保留固定所有者，并拒绝全部新发布代次。
#[test]
fn embedded_pool_manager_shutdown_waits_for_pins_and_worker_exit() {
    // Real pinned state keeps one resident alive past the closing request.
    // 真实固定状态使一个常驻实例在请求关闭后仍存活。
    let layout = SystemRuntimeTestLayout::new("embedded manager shutdown");
    // Manager owns the actual cleanup thread whose join is part of completion.
    // 管理器拥有真实清理线程，等待该线程退出属于完成条件。
    let manager = pool_manager(&layout);
    // A session must be explicitly released before native teardown can complete.
    // 会话必须显式释放，原生清理才能完成。
    let pool = manager
        .create_pool(
            "pinned".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::Session),
        )
        .unwrap();
    // Keep this pin beyond request_close to expose premature completion.
    // 让此固定实例跨越 request_close 存活，以暴露过早完成。
    let mut pin = pool.open_session(control()).unwrap();
    assert!(!manager.poll_closed().unwrap());
    manager.request_close().unwrap();
    assert!(!manager.poll_closed().unwrap());
    assert!(
        manager
            .create_pool(
                "late".into(),
                definition(&layout, "return {call=function() return true end}"),
                pool_policy(InstanceReuse::Reusable)
            )
            .is_err()
    );
    assert_eq!(count(&mut pin).unwrap_err().code, EmbeddedErrorCode::Closed);
    pin.close();
    pin.close();
    // Completion observes both destruction and thread exit, rather than only queue emptiness.
    // 完成同时观察销毁与线程退出，而非仅观察队列为空。
    let deadline = Instant::now() + Duration::from_secs(3);
    while !manager.poll_closed().unwrap() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(manager.poll_closed().unwrap());
    manager.request_close().unwrap();
    assert!(manager.poll_closed().unwrap());
}

/// Closed handles must neither retain dedicated guarantees nor alias a new same-named pool.
/// 已关闭句柄不得保留专用保证，也不得成为同名新池的别名。
#[test]
fn embedded_pool_replacement_releases_old_registration_without_handle_drop() {
    // Both generations intentionally reuse the host's logical pool name.
    // 两个代次刻意复用宿主的逻辑池名。
    let layout = SystemRuntimeTestLayout::new("embedded pool replacement");
    // Dedicated reservation consumes the entire parent capacity.
    // 专用预留消耗整个父级容量。
    let manager = pool_manager(&layout);
    // Keeping an old pool Arc must not prevent activation after it has truly drained.
    // 保留旧池 Arc 不能阻止在实际排空后激活。
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 3;
    // Old generation has one actual warm resident and three guaranteed slots.
    // 旧代次拥有一个真实预热实例与三个保证槽位。
    let old = manager
        .create_pool(
            "plugin".into(),
            definition(&layout, "return {call=function() return 'old' end}"),
            policy.clone(),
        )
        .unwrap();
    old.prewarm(1, control()).unwrap();
    old.close().unwrap();
    // The governor slot may become empty just before registration-release publication.
    // 治理器槽位可能恰好在注册释放发布前变为空。
    let deadline = Instant::now() + Duration::from_secs(3);
    // Retry only explicit old-generation busy status; no source or protocol fallback is allowed.
    // 仅重试明确的旧代次忙碌状态；不允许源码或协议降级。
    let new = loop {
        match manager.create_pool(
            "plugin".into(),
            definition(&layout, "return {call=function() return 'new' end}"),
            policy.clone(),
        ) {
            Ok(pool) => break pool,
            Err(error) if error.code == EmbeddedErrorCode::Busy && Instant::now() < deadline => {
                std::thread::yield_now()
            }
            Err(error) => panic!("old registration did not release after real teardown: {error}"),
        }
    };
    // New state belongs exclusively to the new generation.
    // 新状态仅属于新代次。
    let mut lease = new.acquire(control()).unwrap();
    assert_eq!(count(&mut lease).unwrap(), json!("new"));
    assert_eq!(old.usage().unwrap().resident, 0);
    assert_eq!(new.usage().unwrap().resident, 1);
    drop(old);
    assert_eq!(
        new.usage().unwrap().resident,
        1,
        "dropping the stale handle cannot unregister the new lifetime"
    );
    drop(lease);
    new.close().unwrap();
    drained(&new);
}

/// Idle eviction preserves dedicated warm minimums and use limits cannot be bypassed by holding a lease.
/// 空闲驱逐保留专用预热最小值，持有租借不能绕过使用次数上限。
#[test]
fn embedded_pool_enforces_idle_minimum_and_use_limit() {
    // Explicit short TTL is used only after waiting beyond it in this deterministic fixture.
    // 此确定性夹具仅在等待超过显式短 TTL 后使用该阈值。
    let layout = SystemRuntimeTestLayout::new("embedded reuse retirement");
    // Shared manager retains real capacity while asynchronous retirement finishes.
    // 异步退役完成前，共享管理器保留真实容量。
    let manager = pool_manager(&layout);
    // Dedicated warm minimum and finite use limit apply independently.
    // 专用预热最小值与有限使用次数上限独立生效。
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 1;
    policy.idle_ttl_ms = Some(1);
    policy.max_uses = Some(1);
    // Three VMs exercise both eviction and the protected minimum.
    // 三个 VM 同时验证驱逐与受保护最小值。
    let pool = manager
        .create_pool(
            "limited".into(),
            definition(&layout, "return {call=function() return true end}"),
            policy,
        )
        .unwrap();
    pool.prewarm(3, control()).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(pool.retire_expired().unwrap(), 2);
    assert_eq!(
        pool.retire_expired().unwrap(),
        0,
        "retiring VMs must not let a second pass evict the warm minimum"
    );
    // The retained warm instance has exactly one allowed successful use.
    // 保留的预热实例恰好允许一次成功使用。
    let mut lease = pool.acquire(control()).unwrap();
    assert_eq!(count(&mut lease).unwrap(), json!(true));
    assert_eq!(
        count(&mut lease).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    drop(lease);
    drained(&pool);
}

/// Real VMs obey dedicated guarantees and warm preparation does not count one instance twice.
/// 真实 VM 遵守专用保证，预热准备不对同一实例重复计数。
#[test]
fn embedded_pool_dedicated_preparation_and_partition_isolation() {
    // Both pools deliberately share a package path; their security contexts remain distinct.
    // 两个池刻意共享包路径；其安全上下文仍不同。
    let layout = SystemRuntimeTestLayout::new("embedded dedicated pool");
    // Three parent residents leave exactly one unreserved slot.
    // 父级三个常驻槽位恰好剩余一个非预留槽位。
    let manager = pool_manager(&layout);
    // Dedicated minimum is registered before shared work starts.
    // 公共任务开始前先注册专用最小值。
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 2;
    // First domain owns protected capacity.
    // 第一个域拥有受保护容量。
    let dedicated = manager
        .create_pool(
            "dedicated".into(),
            definition(&layout, "return {call=function() return 'dedicated' end}"),
            policy,
        )
        .unwrap();
    // A different context cannot receive the dedicated pool's Lua state.
    // 不同上下文不能获得专用池的 Lua 状态。
    let mut shared_definition = definition(&layout, "return {call=function() return 'shared' end}");
    shared_definition.security_partition = "workspace-b".into();
    // Shared ownership has no reserved minimum.
    // 公共所有权不设预留最小值。
    let shared = manager
        .create_pool(
            "shared".into(),
            shared_definition,
            pool_policy(InstanceReuse::Reusable),
        )
        .unwrap();
    // Shared pool cannot take either reserved slot.
    // 公共池不能占用任一个预留槽位。
    let mut lease = shared.acquire(control()).unwrap();
    assert!(
        matches!(shared.acquire(control()), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    dedicated.prewarm(2, control()).unwrap();
    assert_eq!(dedicated.usage().unwrap().idle, 2);
    assert_eq!(manager.usage().unwrap().resident, 3);
    assert_eq!(count(&mut lease).unwrap(), json!("shared"));
    // Captured functions and state stay within their own immutable pool.
    // 捕获的函数与状态留在各自不可变池内。
    let mut dedicated_lease = dedicated.acquire(control()).unwrap();
    assert_eq!(count(&mut dedicated_lease).unwrap(), json!("dedicated"));
    drop((lease, dedicated_lease));
    shared.close().unwrap();
    dedicated.close().unwrap();
    drained(&shared);
    drained(&dedicated);
}
