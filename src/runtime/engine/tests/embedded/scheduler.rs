use super::pools::{pool_config, pool_policy};
use super::*;
use crate::runtime::embedded::capabilities::*;
use crate::runtime::embedded::*;
use std::collections::BTreeSet;

mod bindings;
mod capacities;
mod finalization;
mod persistence;
mod plugins;
mod requests;
mod resources;
mod sessions;

/// Build the real formal runtime with explicit fixture `config` and package trust roots.
/// 使用显式夹具 `config` 与包信任根构造真实正式运行时。
fn runtime(layout: &SystemRuntimeTestLayout, config: EmbeddedRuntimeConfig) -> EmbeddedRuntime {
    let plugin = plugin_policy(&config);
    runtime_with_plugin(layout, config, plugin)
}

/// Construct a real runtime with explicit parent `config` and aggregate `plugin` policy for the fixture package.
/// 使用显式父级 `config` 和聚合 `plugin` 策略，为夹具包构造真实运行时。
fn runtime_with_plugin(
    layout: &SystemRuntimeTestLayout,
    config: EmbeddedRuntimeConfig,
    plugin: EmbeddedPluginConfig,
) -> EmbeddedRuntime {
    let runtime = EmbeddedRuntime::new(
        Arc::new(make_runtime_test_engine_with_host_options(
            layout.host_options(),
        )),
        config,
    )
    .unwrap();
    runtime
        .register_plugin(layout.package_id.clone(), plugin)
        .unwrap();
    runtime
}

/// Resolve explicit fixture plugin budgets from this test's parent policy, without production defaults.
/// 从本测试的父策略解析显式夹具插件预算，不定义生产默认值。
fn plugin_policy(config: &EmbeddedRuntimeConfig) -> EmbeddedPluginConfig {
    EmbeddedPluginConfig {
        max_registered_pools: config.max_registered_pools,
        max_sessions: config.max_sessions,
        max_resident_vms: config.max_resident_vms,
        max_running_calls: config.max_running_calls,
        max_queued_calls: config.max_queued_calls,
        max_queued_bytes: config.max_queued_bytes,
        max_operations: config.max_operations,
    }
}

/// Return live fixture permission authority for explicitly registered host probes.
/// 返回用于显式注册宿主探针的实时夹具权限权威。
fn permissions() -> Arc<CapabilityPermissions> {
    CapabilityPermissions::new(BTreeSet::from(["test.host".into()])).unwrap()
}

/// Native closure destruction is real host work and must never block runtime control admission.
/// 原生闭包析构属于真实宿主工作，绝不能阻塞运行时控制入场。
struct CallbackDropBarrier {
    /// Reports the precise start of native closure destruction.
    /// 报告原生闭包析构的精确开始。
    entered: std::sync::mpsc::Sender<()>,
    /// Bounded release gate avoids a permanently stranded test after assertion failure.
    /// 有界释放门避免断言失败后测试永久滞留。
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

/// Release the real Lua finalizer file barrier even when a test assertion unwinds.
/// 即使测试断言栈展开，也释放真实 Lua 终结器文件屏障。
struct FinalizerRelease(std::path::PathBuf);

impl Drop for FinalizerRelease {
    /// Publish release without creating a second panic during unwinding.
    /// 发布释放，且不在栈展开期间创建第二次 panic。
    fn drop(&mut self) {
        let _ = fs::write(&self.0, b"release");
    }
}

/// Operation completion waits for its real VM finalizer while unrelated reusable execution continues.
/// 操作完成等待自身真实 VM 终结器，同时无关可复用执行继续推进。
#[test]
fn embedded_scheduler_operation_stays_cleaning_until_actual_vm_retirement() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler actual cleanup");
    let runtime = runtime(&layout, pool_config());
    let release = FinalizerRelease(layout.package_root.join("release-cleanup"));
    // The captured proxy stays reachable until the typed module releases its function roots.
    // 捕获代理保持可达，直到类型化模块释放其函数根。
    let source = r#"
        local open, clock = io.open, os.clock
        local proxy = newproxy(true)
        getmetatable(proxy).__gc = function()
            local entered = assert(open('entered-cleanup', 'w'))
            entered:write('entered'); entered:close()
            local deadline = clock() + 5
            repeat
                local ok, release = pcall(open, 'release-cleanup', 'r')
                if ok and release then release:close(); break end
            until clock() >= deadline
            local finished = assert(open('finished-cleanup', 'w'))
            finished:write('done'); finished:close()
        end
        return {call=function() return proxy ~= nil end}
    "#;
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let operation = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(5))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !layout.package_root.join("entered-cleanup").exists() {
        assert!(Instant::now() < deadline, "actual finalizer must enter");
        std::thread::yield_now();
    }
    loop {
        let phase = operation.snapshot().unwrap().phase;
        assert!(
            !phase.is_terminal(),
            "operation cannot complete before its real VM destructor"
        );
        if phase == OperationPhase::Cleaning {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker must transfer cleanup ownership"
        );
        std::thread::yield_now();
    }
    let unrelated = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 42 end}"),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "other-domain".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&unrelated, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(42))
    );
    assert_eq!(
        operation.wait(Duration::ZERO).unwrap().phase,
        OperationPhase::Cleaning
    );
    assert!(!layout.package_root.join("finished-cleanup").exists());
    drop(release);
    assert_eq!(
        operation.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        fs::read_to_string(layout.package_root.join("finished-cleanup")).unwrap(),
        "done"
    );
    shutdown(&runtime);
}

impl Drop for CallbackDropBarrier {
    /// Wait for the test's acknowledgement while the runtime remains queryable.
    /// 在运行时保持可查询期间等待测试确认。
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5));
    }
}

/// Runtime close requests return while native closure destructors are still owned by supervision.
/// 原生闭包析构器仍归监督器所有时，运行时关闭请求已经返回。
#[test]
fn embedded_scheduler_close_keeps_native_destructors_off_the_control_thread() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler callback destruction");
    let runtime = runtime(&layout, pool_config());
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, barrier) = std::sync::mpsc::channel();
    let guard = CallbackDropBarrier {
        entered,
        release: Mutex::new(barrier),
    };
    let registrations = runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor("test.drop", CapabilityExecution::Native),
            native: Some(Arc::new(move |_| {
                let _retained_guard = &guard;
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    runtime.request_close().unwrap();
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(!runtime.poll_closed().unwrap());
    assert!(
        !runtime
            .capabilities()
            .status(registrations.first().unwrap())
            .unwrap()
            .drained
    );
    release.send(()).unwrap();
    shutdown(&runtime);
    assert!(
        runtime
            .capabilities()
            .status(registrations.first().unwrap())
            .unwrap()
            .drained
    );
}

/// A synchronous host callback cannot recursively submit work to the same bounded executor set.
/// 同步宿主回调不能递归向同一有界执行器集合提交任务。
#[test]
fn embedded_scheduler_native_reentry_is_rejected_before_queue_admission() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler reentry");
    let runtime = Arc::new(runtime(&layout, pool_config()));
    let weak_runtime = Arc::downgrade(&runtime);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor(
                "test.reentry",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(move |_| {
                let error = weak_runtime
                    .upgrade()
                    .unwrap()
                    .submit(call("unresolved-pool", Value::Null), Duration::from_secs(1))
                    .err()
                    .expect("recursive submission must fail before identity lookup");
                CapabilityOutcome {
                    result: Ok(json!(error.code == EmbeddedErrorCode::Busy)),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    let pool = runtime.register_pool(definition(&layout,
        "return {call=function() local r=vulcan.host.call('test.reentry',{}); return r.value end}"),
        pool_policy(InstanceReuse::Reusable), permissions(), "r1".into()).unwrap();
    let result = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(3))
        .unwrap()
        .wait(Duration::from_secs(3))
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Succeeded);
    assert_eq!(result.value, Some(json!(true)));
    assert_eq!(runtime.usage().unwrap().queued_calls, 0);
    shutdown(&runtime);
}

/// Build a structured ordinary call to exact `pool` with application `arguments`.
/// 使用应用 `arguments` 构造对精确 `pool` 的结构化普通调用。
fn call(pool: &str, arguments: Value) -> EmbeddedCall {
    EmbeddedCall {
        pool_id: pool.into(),
        export: "call".into(),
        arguments,
        context: LuaInvocationContext::default(),
    }
}

/// Await a real SDK request while retaining an explicit test deadline.
/// 保留显式测试截止时间，等待真实 SDK 请求。
fn host_request(runtime: &EmbeddedRuntime) -> HostRequest {
    let broker = runtime.capabilities().host_requests();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut requests = broker.take(1).unwrap();
        if !requests.is_empty() {
            return requests.remove(0);
        }
        assert!(
            Instant::now() < deadline,
            "real Lua host request must arrive"
        );
        std::thread::yield_now();
    }
}

/// Require actual runtime worker and VM retirement shutdown before fixture paths disappear.
/// 在夹具路径消失前要求实际运行时工作线程及 VM 退役关闭。
fn shutdown(runtime: &EmbeddedRuntime) {
    runtime.request_close().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !runtime.poll_closed().unwrap() {
        assert!(
            Instant::now() < deadline,
            "runtime must close after actual ownership drains"
        );
        std::thread::yield_now();
    }
}

/// Cold plugins retain fair access even when one warm plugin could continuously reuse the only VM slot.
/// 即使已预热插件可以持续复用唯一 VM 槽，未预热插件仍保留公平访问。
#[test]
fn embedded_scheduler_rotates_plugins_and_reclaims_shared_idle_capacity() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler fair pressure");
    let mut config = pool_config();
    config.max_resident_vms = 1;
    config.max_running_calls = 1;
    let runtime = runtime(&layout, config);
    let observations = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&observations);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor("test.order", CapabilityExecution::Native),
            native: Some(Arc::new(move |invocation| {
                let label = invocation.arguments.as_str().unwrap().to_owned();
                observed.lock().unwrap().push(label.clone());
                if label == "hold" {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                CapabilityOutcome {
                    result: Ok(json!(label)),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    let source = "return {call=function(a) local r=vulcan.host.call('test.order',a); if not r.ok then error(r.error.message) end; return r.value end}";
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_resident_vms = 1;
    policy.max_running_calls = 1;
    policy.serial = true;
    let first = runtime
        .register_pool(
            definition(&layout, source),
            policy.clone(),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    // The second package is a real distinct plugin under the same authoritative trust root.
    // 第二个包是同一权威信任根下的真实不同插件。
    let second_root = layout.system_lua_lib_dir.join("fair-second");
    fs::create_dir_all(&second_root).unwrap();
    fs::write(second_root.join("dependencies.yaml"), "{}\n").unwrap();
    let mut second_definition = definition(&layout, source);
    second_definition.plugin_id = "fair-second".into();
    second_definition.package_root =
        render_host_visible_path(&fs::canonicalize(second_root).unwrap());
    runtime
        .register_plugin(
            second_definition.plugin_id.clone(),
            runtime.plugin(&layout.package_id).unwrap().config,
        )
        .unwrap();
    let second = runtime
        .register_pool(second_definition, policy, permissions(), "r1".into())
        .unwrap();
    let blocked = runtime
        .submit(call(&first, json!("hold")), Duration::from_secs(5))
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    // All four calls are queued before the single worker is released.
    // 单个工作线程被释放前，四个调用均已排队。
    let calls = [
        (&first, "a1"),
        (&first, "a2"),
        (&second, "b1"),
        (&second, "b2"),
    ]
    .into_iter()
    .map(|(pool, label)| {
        runtime
            .submit(call(pool, json!(label)), Duration::from_secs(5))
            .unwrap()
    })
    .collect::<Vec<_>>();
    release_tx.send(()).unwrap();
    assert_eq!(
        blocked.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    for operation in calls {
        let result = operation.wait(Duration::from_secs(3)).unwrap();
        assert_eq!(
            result.phase,
            OperationPhase::Succeeded,
            "{:?}",
            result.error
        );
    }
    let observed = observations.lock().unwrap().clone();
    assert_eq!(observed.first().unwrap(), "hold");
    let rest = observed
        .iter()
        .filter(|label| label.as_str() != "hold")
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        rest == ["a1", "b1", "a2", "b2"] || rest == ["b1", "a1", "b2", "a2"],
        "both pending plugins must rotate while domain FIFO is preserved: {rest:?}"
    );
    shutdown(&runtime);
}

/// Abandoning dispatch preparation releases its reservation without executing source or fabricating retirement.
/// 放弃分发准备释放预留，不执行源码，也不伪造退役。
#[test]
fn embedded_scheduler_preparation_never_executes_plugin_source() {
    let layout = SystemRuntimeTestLayout::new("embedded preparation ownership");
    let manager = super::pools::pool_manager(&layout);
    let pool = manager.create_pool("prepared".into(), definition(&layout,
        "vulcan.fs.write('must-not-execute','bad'); return {call=function() return true end}"),
        pool_policy(InstanceReuse::Reusable)).unwrap();
    let prepared = pool.prepare(&control()).unwrap();
    assert_eq!(manager.usage().unwrap().creating, 1);
    assert!(!layout.package_root.join("must-not-execute").exists());
    pool.close().unwrap();
    assert!(matches!(
        prepared.finish().unwrap(),
        ModuleRelease::NoInstance
    ));
    assert_eq!(manager.usage().unwrap().resident, 0);
    // Reusing the group name proves abandoned reservation teardown also released the closed registration.
    // 复用分组名证明放弃预留的清理也释放了已关闭注册。
    let replacement = manager
        .create_pool(
            "prepared".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::Reusable),
        )
        .unwrap();
    replacement.close().unwrap();
}

/// Actual dispatch reuses state and exposes independently retained operations with unique runtime identities.
/// 真实分发复用状态，并暴露独立保留的操作及唯一运行时身份。
#[test]
fn embedded_scheduler_executes_reused_modules_and_retains_queryable_operations() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler reuse");
    let runtime = runtime(&layout, pool_config());
    let other = super::scheduler::runtime(&layout, pool_config());
    assert_ne!(runtime.id(), other.id());
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
    for count in 1..=2 {
        let operation = runtime
            .submit(call(&pool, json!({"text":"中文"})), Duration::from_secs(3))
            .unwrap();
        let result = operation.wait(Duration::from_secs(3)).unwrap();
        assert_eq!(
            result.phase,
            OperationPhase::Succeeded,
            "{:?}",
            result.error
        );
        assert_eq!(
            result.value,
            Some(json!({"count":count,"value":{"text":"中文"}}))
        );
        assert_eq!(
            runtime
                .operation(&result.operation_id)
                .unwrap()
                .snapshot()
                .unwrap()
                .phase,
            OperationPhase::Succeeded
        );
        runtime.forget_operation(&result.operation_id).unwrap();
        assert!(runtime.operation(&result.operation_id).is_err());
    }
    shutdown(&runtime);
    shutdown(&other);
}

/// Queued cancellation and expiry complete while the only execution worker is blocked in a host request.
/// 唯一执行工作线程阻塞于宿主请求时，排队取消与过期仍能完成。
#[test]
fn embedded_scheduler_control_supervisor_remains_live_under_queue_pressure() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler control");
    let mut config = pool_config();
    config.max_running_calls = 1;
    config.max_queued_calls = 2;
    let runtime = runtime(&layout, config);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor("test.wait", CapabilityExecution::Queued),
            native: None,
        }])
        .unwrap();
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_running_calls = 1;
    policy.max_queued_calls = 2;
    policy.serial = true;
    let pool = runtime.register_pool(definition(&layout,
        "return {call=function(a) local r=vulcan.host.call('test.wait',a); if not r.ok then error(r.error.message) end; return r.value end}"),
        policy, permissions(), "r1".into()).unwrap();
    let first = runtime
        .submit(call(&pool, json!(1)), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    let cancelled = runtime
        .submit(call(&pool, json!(2)), Duration::from_secs(5))
        .unwrap();
    let expired = runtime
        .submit(call(&pool, json!(3)), Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&pool, json!(4)), Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    cancelled.cancel().unwrap();
    assert_eq!(
        cancelled.wait(Duration::from_secs(2)).unwrap().phase,
        OperationPhase::Cancelled
    );
    let expired_result = expired.wait(Duration::from_secs(2)).unwrap();
    assert_eq!(
        expired_result.error.unwrap().code,
        EmbeddedErrorCode::DeadlineExceeded
    );
    assert_eq!(expired_result.effects, EffectState::NotStarted);
    assert_eq!(
        first.snapshot().unwrap().phase,
        OperationPhase::WaitingForHost
    );
    assert_eq!(runtime.usage().unwrap().queued_calls, 0);
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!("released")),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    assert_eq!(
        first.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!("released"))
    );
    shutdown(&runtime);
}

/// Input rejection cannot execute initialization; invalid output retires while retaining a structured failure.
/// 输入拒绝不能执行初始化；无效输出在退役时保留结构化失败。
#[test]
fn embedded_scheduler_rejects_invalid_input_before_initialization() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler validation");
    let runtime = runtime(&layout, pool_config());
    let mut module = definition(
        &layout,
        "vulcan.fs.write('initialized.txt','yes'); return {call=function(a) return a end}",
    );
    module.exports[0].input_schema = json!({"type":"integer"});
    let pool = runtime
        .register_pool(
            module,
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&pool, json!("invalid")), Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert!(!layout.package_root.join("initialized.txt").exists());
    assert_eq!(
        runtime
            .submit(call(&pool, json!(7)), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(7))
    );
    assert!(layout.package_root.join("initialized.txt").exists());
    shutdown(&runtime);
}

/// Closing keeps a dispatched SDK callback live until its actual acknowledgement preserves commit evidence.
/// 关闭保持已分发 SDK 回调存活，直到实际确认保留提交证据。
#[test]
fn embedded_scheduler_shutdown_waits_for_host_ack_and_preserves_committed_effect() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler close acknowledgement");
    let runtime = runtime(&layout, pool_config());
    let mut descriptor =
        super::capabilities::descriptor("test.commit", CapabilityExecution::Queued);
    descriptor.effects = CapabilityEffects::Mutating;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: None,
        }])
        .unwrap();
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function(a) return vulcan.host.call('test.commit',a) end}",
            ),
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let operation = runtime
        .submit(call(&pool, json!(1)), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    assert!(!operation.snapshot().unwrap().phase.is_terminal());
    assert!(runtime.capabilities().snapshot().is_err());
    // A real host write precedes acknowledgement; cancellation must not erase it.
    // 真实宿主写入先于确认；取消不能抹去该写入。
    fs::write(layout.package_root.join("committed.txt"), "committed").unwrap();
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!(true)),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let result = operation.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(result.phase, OperationPhase::Cancelled);
    assert!(
        result
            .host_effects
            .iter()
            .any(|effect| effect.effects == EffectState::Committed)
    );
    assert_eq!(
        fs::read_to_string(layout.package_root.join("committed.txt")).unwrap(),
        "committed"
    );
    shutdown(&runtime);
}

/// Queue bytes include trusted context and exact UTF-8 serialization, independently of request count.
/// 队列字节包含可信上下文与精确 UTF-8 序列化，独立于请求数量。
#[test]
fn embedded_scheduler_queue_byte_budget_counts_context_and_releases_cancelled_ownership() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler byte accounting");
    let mut config = pool_config();
    config.max_running_calls = 1;
    config.max_queued_bytes = 2048;
    let byte_limit = config.max_queued_bytes;
    let runtime = runtime(&layout, config);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor("test.bytes", CapabilityExecution::Queued),
            native: None,
        }])
        .unwrap();
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_running_calls = 1;
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function(a) return vulcan.host.call('test.bytes',a) end}",
            ),
            policy,
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let first = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    let mut large = call(&pool, Value::Null);
    large.context.client_budget = json!({"text":"中".repeat(350)});
    let wire_bytes = serde_json::to_vec(&large).unwrap().len();
    assert!(
        wire_bytes <= byte_limit && wire_bytes * 2 > byte_limit,
        "fixture must isolate the aggregate queue byte limit"
    );
    let pending = runtime
        .submit(large.clone(), Duration::from_secs(3))
        .unwrap();
    assert_eq!(runtime.usage().unwrap().queued_bytes, wire_bytes);
    assert_eq!(
        runtime
            .submit(large, Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    pending.cancel().unwrap();
    assert_eq!(
        pending.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(runtime.usage().unwrap().queued_bytes, 0);
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
        .unwrap();
    assert_eq!(
        first.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    shutdown(&runtime);
}

/// Closed generations reject queued old work while a separately activated generation progresses.
/// 已关闭代次拒绝排队旧任务，同时单独激活的新代次继续推进。
#[test]
fn embedded_scheduler_pool_replacement_never_retargets_old_operations() {
    let layout = SystemRuntimeTestLayout::new("embedded scheduler generation replacement");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::capabilities::descriptor("test.old", CapabilityExecution::Queued),
            native: None,
        }])
        .unwrap();
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_running_calls = 1;
    policy.serial = true;
    let old = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function(a) return vulcan.host.call('test.old',a) end}",
            ),
            policy.clone(),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let running = runtime
        .submit(call(&old, json!("running")), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    let queued = runtime
        .submit(
            call(&old, json!("must-not-retarget")),
            Duration::from_secs(5),
        )
        .unwrap();
    // The still-queued operation already owns original module authority before any host effect exists.
    // 尚在排队的操作在任何宿主副作用出现前已经拥有原始模块权威。
    let admitted = queued.snapshot().unwrap();
    assert_eq!(admitted.phase, OperationPhase::Queued);
    assert!(admitted.host_effects.is_empty());
    match &admitted.context {
        OperationContext::Module(context) => {
            assert_eq!(context.pool_id, old);
            assert_eq!(context.caller.plugin_id, request.caller.plugin_id);
            assert_eq!(
                context.caller.package_generation,
                request.caller.package_generation
            );
            assert_eq!(context.caller.operation_id, queued.id());
        }
        OperationContext::Unbound => panic!("queued formal operation lost original module context"),
    }
    runtime.close_pool(&old).unwrap();
    assert_eq!(
        runtime.forget_pool(&old).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    let old_result = queued.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(old_result.context, admitted.context);
    assert_eq!(old_result.error.unwrap().code, EmbeddedErrorCode::Closed);
    assert_eq!(old_result.effects, EffectState::NotStarted);
    let mut replacement = definition(
        &layout,
        "return {call=function() return 'new-generation' end}",
    );
    replacement.generation = "generation-two".into();
    let new = runtime
        .register_pool(replacement, policy, permissions(), "r2".into())
        .unwrap();
    assert_ne!(old, new);
    let result = runtime
        .submit(call(&new, Value::Null), Duration::from_secs(3))
        .unwrap()
        .wait(Duration::from_secs(3))
        .unwrap();
    assert_eq!(result.value, Some(json!("new-generation")));
    assert!(!running.snapshot().unwrap().phase.is_terminal());
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!("old-generation")),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match runtime.forget_pool(&old) {
            Ok(()) => break,
            Err(error) if error.code == EmbeddedErrorCode::Busy => {
                assert!(
                    Instant::now() < deadline,
                    "closed old generation must drain"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("unexpected retirement failure: {error}"),
        }
    }
    assert_eq!(
        runtime
            .submit(call(&old, Value::Null), Duration::from_secs(1))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::NotFound
    );
    shutdown(&runtime);
}
