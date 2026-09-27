use super::*;

/// Register a real second package under the authoritative test trust root with explicit aggregate limits.
/// 在权威测试信任根下注册真实第二包，并显式指定聚合上限。
fn other_plugin(
    runtime: &EmbeddedRuntime,
    layout: &SystemRuntimeTestLayout,
    policy: EmbeddedPluginConfig,
) -> ModuleDefinition {
    let root = layout.system_lua_lib_dir.join("budget-other");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("dependencies.yaml"), "{}\n").unwrap();
    let mut module = definition(
        layout,
        "local n=0; return {call=function() n=n+1; return n end}",
    );
    module.plugin_id = "budget-other".into();
    module.package_root = render_host_visible_path(&fs::canonicalize(root).unwrap());
    runtime
        .register_plugin(module.plugin_id.clone(), policy)
        .unwrap();
    module
}

/// Return a one-instance domain policy compatible with constrained plugin fixtures.
/// 返回与受限插件夹具兼容的单实例域策略。
fn domain(reuse: InstanceReuse, queue_limit: usize) -> PluginPoolConfig {
    let mut policy = pool_policy(reuse);
    policy.max_resident_vms = 1;
    policy.max_running_calls = 1;
    policy.max_queued_calls = queue_limit;
    policy
}

/// Explicit registration and parent bounds cannot be bypassed by the first pool or an unregistered name.
/// 首个池或未注册名称不能绕过显式注册与父级边界。
#[test]
fn embedded_plugin_requires_explicit_bounded_registration() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin registration");
    let mut config = pool_config();
    config.max_registered_plugins = 1;
    let limits = plugin_policy(&config);
    let runtime = runtime(&layout, config);
    let mut undeclared = definition(&layout, "return {call=function() end}");
    undeclared.plugin_id = "unregistered".into();
    assert_eq!(
        runtime
            .register_pool(
                undeclared,
                domain(InstanceReuse::Reusable, 8),
                permissions(),
                "r1".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::NotFound
    );
    assert_eq!(
        runtime
            .register_plugin(layout.package_id.clone(), limits.clone())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        runtime
            .register_plugin("another".into(), limits.clone())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let mut invalid = limits.clone();
    invalid.max_resident_vms += 1;
    assert_eq!(
        runtime
            .register_plugin("invalid".into(), invalid)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    runtime.close_plugin(&layout.package_id).unwrap();
    runtime.forget_plugin(&layout.package_id).unwrap();
    runtime.register_plugin("another".into(), limits).unwrap();
    assert_eq!(
        runtime.plugin(&layout.package_id).unwrap_err().code,
        EmbeddedErrorCode::NotFound
    );
    shutdown(&runtime);
}

/// Two generations share one execution budget while another plugin can use remaining parent workers.
/// 两个代次共享同一个执行预算，同时另一个插件可以使用剩余父级工作线程。
#[test]
fn embedded_plugin_concurrency_spans_generations_without_blocking_other_plugins() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin concurrency");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_running_calls = 1;
    let runtime = runtime_with_plugin(&layout, config.clone(), limits);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let source = "return {call=function(a) return vulcan.host.call('test.wait',a) end}";
    let first = runtime
        .register_pool(
            definition(&layout, source),
            domain(InstanceReuse::Reusable, 8),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let mut next = definition(&layout, source);
    next.generation = "next-generation".into();
    let second = runtime
        .register_pool(
            next,
            domain(InstanceReuse::Reusable, 8),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    let other = runtime
        .register_pool(
            other_plugin(&runtime, &layout, plugin_policy(&config)),
            domain(InstanceReuse::Reusable, 8),
            permissions(),
            "other".into(),
        )
        .unwrap();
    let first_op = runtime
        .submit(call(&first, json!(1)), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    let second_op = runtime
        .submit(call(&second, json!(2)), Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&other, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(1))
    );
    assert_eq!(second_op.snapshot().unwrap().phase, OperationPhase::Queued);
    let usage = runtime.plugin(&layout.package_id).unwrap();
    assert_eq!(usage.active_operations, 1);
    assert_eq!(usage.queued_calls, 1);
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
        first_op.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    let second_request = host_request(&runtime);
    assert_eq!(second_request.caller.package_generation, "next-generation");
    let rejected = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(3))
        .unwrap();
    runtime.close_plugin(&layout.package_id).unwrap();
    assert_eq!(
        rejected
            .wait(Duration::from_secs(3))
            .unwrap()
            .error
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    assert!(runtime.plugin(&layout.package_id).unwrap().closing);
    assert_eq!(
        runtime
            .submit(call(&other, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(2))
    );
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &second_request.request_id,
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    assert_eq!(
        second_op.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    shutdown(&runtime);
}

/// Pool-local queues cannot multiply plugin-wide count or UTF-8 context byte budgets.
/// 池局部队列不能倍增插件级数量或 UTF-8 上下文字节预算。
#[test]
fn embedded_plugin_queue_limits_aggregate_domains_and_release_cancelled_bytes() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin queue accounting");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_running_calls = 1;
    limits.max_queued_calls = 2;
    limits.max_queued_bytes = 2048;
    let byte_limit = limits.max_queued_bytes;
    let runtime = runtime_with_plugin(&layout, config, limits);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let first = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function() return vulcan.host.call('test.wait',{}) end}",
            ),
            domain(InstanceReuse::Reusable, 2),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let second = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 2 end}"),
            domain(InstanceReuse::Reusable, 2),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    let running = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    let a = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(5))
        .unwrap();
    let b = runtime
        .submit(call(&second, Value::Null), Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&second, Value::Null), Duration::from_secs(5))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    a.cancel().unwrap();
    b.cancel().unwrap();
    assert_eq!(
        a.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(
        b.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
    );
    let mut large = call(&first, Value::Null);
    large.context.client_budget = json!({"text":"中".repeat(350)});
    let wire_bytes = serde_json::to_vec(&large).unwrap().len();
    assert!(
        wire_bytes <= byte_limit && wire_bytes * 2 > byte_limit,
        "fixture must isolate aggregate bytes"
    );
    let queued = runtime
        .submit(large.clone(), Duration::from_secs(5))
        .unwrap();
    large.pool_id = second.clone();
    assert_eq!(
        runtime
            .submit(large.clone(), Duration::from_secs(5))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        runtime.plugin(&layout.package_id).unwrap().queued_bytes,
        wire_bytes
    );
    queued.cancel().unwrap();
    assert_eq!(
        queued.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(runtime.plugin(&layout.package_id).unwrap().queued_bytes, 0);
    let accepted = runtime.submit(large, Duration::from_secs(5)).unwrap();
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
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        accepted.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!(2))
    );
    shutdown(&runtime);
}

/// Plugin-local pressure retires its own idle domain and preserves another plugin's warm state.
/// 插件局部压力退役其自身空闲域，并保留另一个插件的热状态。
#[test]
fn embedded_plugin_resident_limit_reclaims_only_its_own_idle_capacity() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin local pressure");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_resident_vms = 1;
    limits.max_running_calls = 1;
    let runtime = runtime_with_plugin(&layout, config.clone(), limits);
    // Register the unrelated cache first so an unfiltered scan would evict it before this plugin's idle VM.
    // 先注册无关缓存，使无过滤扫描会先驱逐它，再扫描此插件的空闲 VM。
    let other = runtime
        .register_pool(
            other_plugin(&runtime, &layout, plugin_policy(&config)),
            domain(InstanceReuse::Reusable, 8),
            permissions(),
            "other".into(),
        )
        .unwrap();
    let first = runtime
        .register_pool(
            definition(
                &layout,
                "local n=0; return {call=function() n=n+1; return n end}",
            ),
            domain(InstanceReuse::Reusable, 8),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let second = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 42 end}"),
            domain(InstanceReuse::Reusable, 8),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    for (pool, expected) in [
        (&other, 1),
        (&first, 1),
        (&second, 42),
        (&first, 1),
        (&other, 2),
    ] {
        assert_eq!(
            runtime
                .submit(call(pool, Value::Null), Duration::from_secs(3))
                .unwrap()
                .wait(Duration::from_secs(3))
                .unwrap()
                .value,
            Some(json!(expected))
        );
        assert!(
            runtime
                .plugin(&layout.package_id)
                .unwrap()
                .resources
                .resident
                <= 1
        );
    }
    assert_eq!(runtime.resources().unwrap().resident, 2);
    shutdown(&runtime);
}

/// Unused dedicated guarantees remain unavailable to another domain of the same plugin.
/// 未使用专用保证仍不可被同一插件的其他域占用。
#[test]
fn embedded_plugin_dedicated_guarantees_share_one_aggregate_budget() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin guarantees");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_resident_vms = 2;
    let runtime = runtime_with_plugin(&layout, config, limits);
    let mut policy = domain(InstanceReuse::Session, 8);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 1;
    policy.max_resident_vms = 2;
    let first = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 1 end}"),
            policy.clone(),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let second = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 2 end}"),
            policy.clone(),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .committed_resident_vms,
        2
    );
    assert_eq!(
        runtime
            .register_pool(
                definition(&layout, "return {call=function() end}"),
                policy,
                permissions(),
                "r3".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let first_session = runtime
        .open_session(&first, Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        first_session
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        runtime
            .open_session(&first, Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let second_session = runtime
        .open_session(&second, Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        second_session
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .resources
            .resident,
        2
    );
    shutdown(&runtime);
}

/// Retained sessions across distinct domains consume the same plugin metadata limit until forgotten.
/// 不同域的保留会话在遗忘前消费同一插件元数据上限。
#[test]
fn embedded_plugin_session_and_pool_metadata_limits_span_domains() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin retained metadata");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_sessions = 1;
    limits.max_registered_pools = 2;
    let runtime = runtime_with_plugin(&layout, config, limits);
    let first = runtime
        .register_pool(
            definition(&layout, "return {call=function() end}"),
            domain(InstanceReuse::Session, 8),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let second = runtime
        .register_pool(
            definition(&layout, "return {call=function() end}"),
            domain(InstanceReuse::Session, 8),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .register_pool(
                definition(&layout, "return {call=function() end}"),
                domain(InstanceReuse::Reusable, 8),
                permissions(),
                "r3".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let session = runtime
        .open_session(&first, Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        session
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        runtime
            .open_session(&second, Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    runtime.close_session(&session.session_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while runtime.session(&session.session_id).unwrap().phase != EmbeddedSessionPhase::Closed {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        runtime
            .open_session(&second, Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    runtime.forget_session(&session.session_id).unwrap();
    let replacement = runtime
        .open_session(&second, Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        replacement
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .retained_sessions,
        1
    );
    shutdown(&runtime);
}

/// Removing a pool or dropping a client handle cannot erase retained operation quota or plugin ownership.
/// 移除池或丢弃客户端句柄不能抹去保留操作配额及插件归属。
#[test]
fn embedded_plugin_operation_quota_survives_pool_removal() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin operation retention");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_running_calls = 1;
    limits.max_queued_calls = 2;
    limits.max_operations = 2;
    let runtime = runtime_with_plugin(&layout, config, limits);
    let pool = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 1 end}"),
            domain(InstanceReuse::SingleCall, 2),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let operation = runtime
            .submit(call(&pool, Value::Null), Duration::from_secs(3))
            .unwrap();
        let result = operation.wait(Duration::from_secs(3)).unwrap();
        assert_eq!(result.phase, OperationPhase::Succeeded);
        ids.push(result.operation_id);
    }
    assert_eq!(
        runtime
            .submit(call(&pool, Value::Null), Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    runtime.close_pool(&pool).unwrap();
    runtime.forget_pool(&pool).unwrap();
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .retained_operations,
        2
    );
    runtime.close_plugin(&layout.package_id).unwrap();
    assert_eq!(
        runtime.forget_plugin(&layout.package_id).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    for id in ids {
        runtime.forget_operation(&id).unwrap();
    }
    runtime.forget_plugin(&layout.package_id).unwrap();
    assert_eq!(
        runtime.plugin(&layout.package_id).unwrap_err().code,
        EmbeddedErrorCode::NotFound
    );
    shutdown(&runtime);
}

/// Old-generation teardown retains plugin guarantees until the real VM destructor and registration release finish.
/// 旧代次清理保留插件保证，直到真实 VM 析构和注册释放结束。
#[test]
fn embedded_plugin_generation_retirement_keeps_aggregate_reservation() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin retiring guarantee");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_resident_vms = 1;
    limits.max_running_calls = 1;
    let runtime = runtime_with_plugin(&layout, config, limits);
    let release = FinalizerRelease(layout.package_root.join("plugin-retirement-release"));
    let source = r#"
        local open, clock = io.open, os.clock
        local proxy = newproxy(true)
        getmetatable(proxy).__gc=function()
            local f=assert(open('plugin-retirement-entered','w')); f:close()
            local deadline=clock()+5
            repeat
                local ok, release=pcall(open,'plugin-retirement-release','r')
                if ok and release then release:close(); break end
            until clock() >= deadline
        end
        return {call=function() return proxy ~= nil end}
    "#;
    let mut policy = domain(InstanceReuse::Reusable, 8);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 1;
    let old = runtime
        .register_pool(
            definition(&layout, source),
            policy.clone(),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&old, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    runtime.close_pool(&old).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !layout
        .package_root
        .join("plugin-retirement-entered")
        .exists()
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let usage = runtime.plugin(&layout.package_id).unwrap();
    assert_eq!(usage.resources.retiring, 1);
    assert_eq!(usage.committed_resident_vms, 1);
    assert_eq!(
        runtime
            .register_pool(
                definition(&layout, "return {call=function() return 2 end}"),
                policy.clone(),
                permissions(),
                "r2".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    drop(release);
    while runtime
        .plugin(&layout.package_id)
        .unwrap()
        .committed_resident_vms
        != 0
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let next = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 2 end}"),
            policy,
            permissions(),
            "r2".into(),
        )
        .unwrap();
    assert_eq!(
        runtime.plugin(&layout.package_id).unwrap().retained_pools,
        2
    );
    assert_eq!(
        runtime
            .submit(call(&next, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(2))
    );
    shutdown(&runtime);
}

/// Concurrent session preparations in distinct domains cannot oversubscribe one aggregate resident slot.
/// 不同域中的并发会话准备不能超额占用同一个聚合常驻槽位。
#[test]
fn embedded_plugin_concurrent_session_preparation_is_atomic() {
    let layout = SystemRuntimeTestLayout::new("embedded plugin concurrent reservation");
    let config = pool_config();
    let mut limits = plugin_policy(&config);
    limits.max_resident_vms = 1;
    limits.max_running_calls = 1;
    let runtime = runtime_with_plugin(&layout, config, limits);
    let first = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 1 end}"),
            domain(InstanceReuse::Session, 8),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let second = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 2 end}"),
            domain(InstanceReuse::Session, 8),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    let attempts = [&first, &second, &first, &second];
    let gate = std::sync::Barrier::new(attempts.len());
    let results = std::thread::scope(|scope| {
        let workers = attempts
            .into_iter()
            .map(|pool| {
                let runtime = &runtime;
                let gate = &gate;
                scope.spawn(move || {
                    gate.wait();
                    runtime.open_session(pool, Duration::from_secs(3))
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut accepted = 0;
    for result in results {
        match result {
            Ok(opening) => {
                accepted += 1;
                assert_eq!(
                    opening
                        .operation
                        .wait(Duration::from_secs(3))
                        .unwrap()
                        .phase,
                    OperationPhase::Succeeded
                );
            }
            Err(error) => assert_eq!(error.code, EmbeddedErrorCode::CapacityExceeded),
        }
    }
    assert_eq!(accepted, 1);
    let snapshot = runtime.plugin(&layout.package_id).unwrap();
    assert_eq!(snapshot.resources.resident, 1);
    assert_eq!(snapshot.retained_sessions, 1);
    assert_eq!(snapshot.retained_operations, 1);
    shutdown(&runtime);
}
