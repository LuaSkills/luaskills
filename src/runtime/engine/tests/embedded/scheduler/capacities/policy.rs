//! Capacity revisions are exercised through real Lua, pinned state and queued host barriers.
//! 通过真实 Lua、固定状态及排队宿主屏障验证容量修订。

use super::*;

/// Queue edits and growth retain warm Lua state; resident shrink retires it through the original owner.
/// 队列编辑及扩容保留预热 Lua 状态；常驻缩容经原所有者退役该状态。
#[test]
fn embedded_capacity_policy_nonshrinking_edits_preserve_warm_state() {
    // A real per-instance counter detects unnecessary cache invalidation without timing assumptions.
    // 真实逐实例计数器无需时序假设即可检测不必要的缓存失效。
    let layout = SystemRuntimeTestLayout::new("capacity revision warm preservation");
    // Use the actual scheduler and physical governor.
    // 使用实际调度器及物理治理器。
    let runtime = runtime(&layout, pool_config());
    // The original ceiling allows a later resident shrink.
    // 原上限允许后续常驻缩容。
    let original = capacity_policy(PoolKind::Shared, 0, 2);
    // Exact ownership survives every revision.
    // 精确归属跨各修订存续。
    let id = runtime
        .register_capacity(&layout.package_id, original.clone())
        .unwrap();
    // The private counter identifies continuity inside one real Lua instance.
    // 私有计数器识别同一真实 Lua 实例内的连续性。
    let pool = member(
        &runtime,
        &layout,
        &id,
        &original,
        InstanceReuse::Reusable,
        r#"
-- Retain only instance-local state.
-- 仅保留实例局部状态。
local count=0
return {
-- Advance this instance's counter.
-- 递增此实例的计数器。
call=function() count=count+1; return count end}
"#,
    );
    assert_eq!(invoke(&runtime, &pool), json!(1));
    // A queue-only change keeps the current warm instance.
    // 仅队列变更保留当前预热实例。
    let mut target = original;
    target.max_queued_calls -= 1;
    // Each mutation echoes the token from the immediately preceding atomic snapshot.
    // 各变更回传紧邻前一次原子快照中的令牌。
    let mut revision = runtime.capacity_policy(&id).unwrap().revision;
    revision = runtime
        .revise_capacity(&id, &revision, target.clone())
        .unwrap();
    assert_eq!(invoke(&runtime, &pool), json!(2));
    target.resources.kind = PoolKind::Dedicated;
    target.resources.min_resident_vms = 1;
    revision = runtime
        .revise_capacity(&id, &revision, target.clone())
        .unwrap();
    assert_eq!(invoke(&runtime, &pool), json!(3));
    target.resources.max_resident_vms -= 1;
    revision = runtime
        .revise_capacity(&id, &revision, target.clone())
        .unwrap();
    residents(&runtime, &id, 0);
    assert_eq!(invoke(&runtime, &pool), json!(1));
    target.resources.max_resident_vms += 1;
    runtime.revise_capacity(&id, &revision, target).unwrap();
    assert_eq!(invoke(&runtime, &pool), json!(2));
    shutdown(&runtime);
}

/// Register a real module in capacity using config and reuse; return its exact retained pool identity.
/// 使用 config 和 reuse 在 capacity 中注册真实模块；返回其精确保留池身份。
fn member(
    runtime: &EmbeddedRuntime,
    layout: &SystemRuntimeTestLayout,
    capacity: &str,
    config: &EmbeddedCapacityConfig,
    reuse: InstanceReuse,
    source: &str,
) -> String {
    runtime
        .register_pool_in_capacity(
            capacity,
            definition(layout, source),
            member_policy(config, reuse),
            permissions(),
            "policy-test".into(),
        )
        .expect("valid test module must register")
}

/// Observe actual resident convergence for capacity within a bounded interval, without changing ownership.
/// 在有界时间内观测 capacity 的实际常驻收敛，不改变所有权。
fn residents(runtime: &EmbeddedRuntime, capacity: &str, expected: usize) {
    // Use the same deadline for the complete observation rather than renewing each polling attempt.
    // 完整观测使用同一截止时间，不逐次续期。
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // Query real physical ownership instead of interpreting a closing flag as release.
        // 查询真实物理归属，不把关闭标记解释为释放。
        let actual = runtime
            .capacity(capacity)
            .expect("retained capacity")
            .resources
            .resident;
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected} residents, actual {actual}"
        );
        std::thread::yield_now();
    }
}

/// Revisions reject impossible guarantees and stale tokens atomically, including simultaneous writers.
/// 修订原子拒绝不可能保证及过期令牌，包含同时写入者。
#[test]
fn embedded_capacity_policy_atomic_reservations_and_compare_exchange() {
    // Run separate ownership layouts so parent and plugin failures are independently proven.
    // 分别运行归属布局，独立证明父级及插件失败。
    for parent_limited in [false, true] {
        // Each runtime owns isolated real resources and explicit limits.
        // 各运行时拥有隔离的真实资源及显式限制。
        let layout = SystemRuntimeTestLayout::new("capacity revision guarantees");
        // The parent fixture is the source of all inherited limits.
        // 父级夹具是全部继承限制的来源。
        let parent = pool_config();
        // Restrict only the plugin in the plugin-boundary case.
        // 仅在插件边界情形中限制插件。
        let mut plugin = plugin_policy(&parent);
        if !parent_limited {
            plugin.max_resident_vms = 2;
        }
        // Explicit plugin authority is installed before capacity registration.
        // 容量注册前安装显式插件权威。
        let runtime = runtime_with_plugin(&layout, parent.clone(), plugin);
        // The revision target starts without a reserved guarantee.
        // 修订目标初始不预留保证。
        let original = capacity_policy(PoolKind::Shared, 0, 2);
        // Keep exact identity throughout all successful and rejected revisions.
        // 在全部成功及被拒修订中保持精确身份。
        let id = runtime
            .register_capacity(&layout.package_id, original.clone())
            .unwrap();
        // A second owner consumes the budget that an invalid guarantee would steal.
        // 第二个所有者消费非法保证可能抢占的预算。
        let other_owner = if parent_limited {
            runtime
                .register_plugin("other".into(), plugin_policy(&parent))
                .unwrap();
            "other"
        } else {
            &layout.package_id
        };
        runtime
            .register_capacity(other_owner, capacity_policy(PoolKind::Dedicated, 2, 2))
            .unwrap();
        // Snapshot ties the compare token to its actual original policy.
        // 快照将比较令牌与实际原策略绑定。
        let before = runtime.capacity_policy(&id).unwrap();
        // This is valid structurally but impossible under the occupied aggregate budget.
        // 此声明结构合法，但在已占用聚合预算下不可能满足。
        let candidate = capacity_policy(PoolKind::Dedicated, 2, 2);
        // Rejection must leave both physical commitment and the token unchanged.
        // 拒绝必须保持物理承诺与令牌不变。
        let error = runtime
            .revise_capacity(&id, &before.revision, candidate)
            .unwrap_err();
        assert_eq!(error.code, EmbeddedErrorCode::CapacityExceeded);
        assert!(
            error
                .message
                .contains(if parent_limited { "parent" } else { "plugin" })
        );
        assert_eq!(
            runtime.capacity_policy(&id).unwrap().revision,
            before.revision
        );
        assert_eq!(runtime.capacity(&id).unwrap().config, original);
        assert_eq!(runtime.capacity(&id).unwrap().committed_resident_vms, 0);
        assert_eq!(
            runtime
                .revise_capacity(&id, &before.revision, original.clone())
                .unwrap(),
            before.revision
        );
        // One explicit queue change is enough to create a new revision without consuming VM capacity.
        // 一项显式队列变更足以创建新修订，且不消费 VM 容量。
        let mut updated = original.clone();
        updated.max_queued_calls -= 1;
        // Two writers deliberately use the same predecessor and identical requested values.
        // 两个写入者刻意使用同一前驱及相同请求值。
        let barrier = std::sync::Barrier::new(2);
        // Both threads finish before any fixture owner is released.
        // 任何夹具所有者释放前，两个线程均完成。
        let outcomes = std::thread::scope(|scope| {
            // Each contender echoes the same token without numeric conversion.
            // 各竞争者原样回传相同令牌，不进行数值转换。
            let workers = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        runtime.revise_capacity(&id, &before.revision, updated.clone())
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().err())
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            vec![EmbeddedErrorCode::Busy]
        );
        // Restoring the original values must still issue a different token, preventing ABA acceptance.
        // 恢复原始值仍必须签发不同令牌，防止 ABA 接纳。
        let current = runtime.capacity_policy(&id).unwrap();
        runtime
            .revise_capacity(&id, &current.revision, original.clone())
            .unwrap();
        assert_ne!(
            runtime.capacity_policy(&id).unwrap().revision,
            before.revision
        );
        assert_eq!(
            runtime
                .revise_capacity(&id, &before.revision, original)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::Busy
        );
        runtime.close_capacity(&id).unwrap();
        // A current token cannot reopen a permanently closed capacity.
        // 当前令牌不能重新打开永久关闭的容量。
        let closed = runtime.capacity_policy(&id).unwrap();
        assert_eq!(
            runtime
                .revise_capacity(&id, &closed.revision, updated)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::Closed
        );
        shutdown(&runtime);
    }
}

/// Shrinking below pinned occupancy preserves original state while preventing new physical admission.
/// 缩容低于固定占用时保留原状态，同时阻止新物理入场。
#[test]
fn embedded_capacity_policy_shrink_preserves_fixed_sessions_and_kind_transition() {
    // The actual VM counter proves fixed-session continuity across a policy change.
    // 真实 VM 计数器证明策略变化前后的固定会话连续性。
    let layout = SystemRuntimeTestLayout::new("capacity revision pinned state");
    // Use the production scheduler and physical governor together.
    // 一并使用生产调度器及物理治理器。
    let runtime = runtime(&layout, pool_config());
    // Start with room for two fixed instances and no reservation.
    // 初始容纳两个固定实例，不预留保证。
    let config = capacity_policy(PoolKind::Shared, 0, 2);
    // The original capacity identity is retained after converting ownership kind.
    // 转换归属类别后保留原容量身份。
    let id = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Per-instance Lua state must survive the explicit quota revision.
    // 逐实例 Lua 状态必须在显式额度修订后保留。
    let pool = member(
        &runtime,
        &layout,
        &id,
        &config,
        InstanceReuse::Session,
        r#"
-- Count invocations inside this exact instance.
-- 在此精确实例内统计调用。
local count=0
return {
-- Advance and return the private count.
-- 递增并返回私有计数。
call=function() count=count+1; return count end}
"#,
    );
    // Original pinned identities are never retargeted to a replacement module.
    // 原固定身份绝不改投替代模块。
    let mut sessions = Vec::new();
    for _ in 0..2 {
        // Initialization completes before the next exact session is created.
        // 下一个精确会话创建前完成初始化。
        let opening = runtime.open_session(&pool, Duration::from_secs(3)).unwrap();
        assert_eq!(
            opening
                .operation
                .wait(Duration::from_secs(3))
                .unwrap()
                .phase,
            OperationPhase::Succeeded
        );
        sessions.push(opening.session_id);
    }
    // Atomic snapshot supplies the required predecessor.
    // 原子快照提供必需前驱。
    let before = runtime.capacity_policy(&id).unwrap();
    // A new dedicated guarantee shares the same physical identity across old and new members.
    // 新专用保证跨新旧成员共享同一物理身份。
    let target = capacity_policy(PoolKind::Dedicated, 1, 1);
    runtime
        .revise_capacity(&id, &before.revision, target.clone())
        .unwrap();
    // Actual occupancy is not reduced merely because the target was lowered.
    // 不因目标降低而减少实际占用。
    let pending = runtime.capacity_policy(&id).unwrap();
    assert!(pending.pending_convergence);
    assert_eq!(pending.capacity.resources.resident, 2);
    assert_eq!(pending.capacity.config, target);
    assert!(
        matches!(runtime.open_session(&pool, Duration::from_secs(3)), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    for session in &sessions {
        for expected in 1..=2 {
            assert_eq!(
                sessions::session_call(&runtime, session, Value::Null)
                    .wait(Duration::from_secs(3))
                    .unwrap()
                    .value,
                Some(json!(expected))
            );
        }
    }
    // New members must explicitly use the new kind and limits; old declarations are not rewritten.
    // 新成员必须显式使用新类别及限制；旧声明不被重写。
    assert_eq!(
        runtime
            .register_pool_in_capacity(
                &id,
                definition(&layout, "error('must not execute')"),
                member_policy(&config, InstanceReuse::Session),
                permissions(),
                "stale".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    for session in &sessions {
        runtime.close_session(session).unwrap();
    }
    residents(&runtime, &id, 0);
    assert!(!runtime.capacity_policy(&id).unwrap().pending_convergence);
    assert_eq!(runtime.capacity(&id).unwrap().committed_resident_vms, 1);
    // A replacement member is accepted only under the exact new capacity policy.
    // 替代成员仅在精确新容量策略下被接纳。
    let replacement = member(
        &runtime,
        &layout,
        &id,
        &target,
        InstanceReuse::Session,
        r#"
return {
-- Return a marker from the newly admitted module.
-- 从新入场模块返回标记。
call=function() return 99 end}
"#,
    );
    // Creating one new instance spends the preserved dedicated guarantee.
    // 创建一个新实例消费保留的专用保证。
    let opening = runtime
        .open_session(&replacement, Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        opening
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        sessions::session_call(&runtime, &opening.session_id, Value::Null)
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(99))
    );
    shutdown(&runtime);
}

/// A smaller queue retains accepted requests and their original deadlines while rejecting new overload.
/// 更小队列保留已接纳请求及原截止时间，同时拒绝新过载。
#[test]
fn embedded_capacity_policy_shrink_preserves_queued_work() {
    // A queued capability provides a deterministic barrier without replacing real Lua execution.
    // 排队能力提供确定性屏障，不替代真实 Lua 执行。
    let layout = SystemRuntimeTestLayout::new("capacity revision queued work");
    // One actual capacity execution slot keeps the second and third requests queued.
    // 一个实际容量执行槽使第二及第三个请求保持排队。
    let runtime = runtime(&layout, pool_config());
    register_wait(&runtime);
    // Complete original queue limits come from the shared fixture.
    // 完整原队列限制来自共享夹具。
    let config = capacity_policy(PoolKind::Shared, 0, 2);
    // Keep the same identity through the queue shrink.
    // 队列缩减期间保持同一身份。
    let id = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Every call must reach its own exact external acknowledgement.
    // 每个调用必须到达其自身精确外部确认。
    let pool = member(
        &runtime,
        &layout,
        &id,
        &config,
        InstanceReuse::SingleCall,
        r#"
return {
-- Wait for one acknowledged host callback.
-- 等待一个已确认宿主回调。
call=function() return vulcan.host.call('test.wait',{}) end}
"#,
    );
    // The first dispatched operation owns its original execution slot throughout the revision.
    // 首个已分发操作在修订全过程中拥有原执行槽。
    let first = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(8))
        .unwrap();
    // Retain the exact host request before enqueuing the next operations.
    // 后续操作入队前保留精确宿主请求。
    let request = host_request(&runtime);
    // These accepted operations must survive a queue size smaller than their combined occupancy.
    // 这些已接纳操作必须在队列缩至小于合计占用时保留。
    let queued = (0..2)
        .map(|_| {
            runtime
                .submit(call(&pool, Value::Null), Duration::from_secs(8))
                .unwrap()
        })
        .collect::<Vec<_>>();
    // Snapshot includes actual bytes of both queued requests.
    // 快照包含两个排队请求的实际字节。
    let before = runtime.capacity_policy(&id).unwrap();
    // Shrink both count and bytes below existing occupancy, without modifying accepted requests.
    // 将数量及字节同时缩至低于既有占用，不修改已接纳请求。
    let mut target = config;
    target.max_queued_calls = 1;
    target.max_queued_bytes = before.capacity.queued_bytes / 2;
    runtime
        .revise_capacity(&id, &before.revision, target)
        .unwrap();
    assert!(runtime.capacity_policy(&id).unwrap().pending_convergence);
    assert_eq!(runtime.capacity(&id).unwrap().queued_calls, queued.len());
    assert_eq!(
        runtime
            .submit(call(&pool, Value::Null), Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    acknowledge(&runtime, request);
    assert_eq!(
        first.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    for operation in queued {
        // Each original queued request is dispatched once and acknowledged independently.
        // 各原排队请求仅分发一次，并独立确认。
        let request = host_request(&runtime);
        acknowledge(&runtime, request);
        assert_eq!(
            operation.wait(Duration::from_secs(3)).unwrap().phase,
            OperationPhase::Succeeded
        );
    }
    assert!(!runtime.capacity_policy(&id).unwrap().pending_convergence);
    shutdown(&runtime);
}

/// Cache invalidation closes the original VM only after its active call and preserves independent cleanup evidence.
/// 缓存失效仅在活动调用结束后关闭原 VM，并保留独立清理证据。
#[test]
fn embedded_capacity_policy_revision_drains_reusable_finalizer() {
    // Real closing callbacks demonstrate that a revision does not bypass normal resource cleanup.
    // 真实关闭回调证明修订不绕过正常资源清理。
    let layout = SystemRuntimeTestLayout::new("capacity revision reusable closing");
    // The broker controls both business and later independent closing work.
    // 代理一并控制业务及后续独立关闭工作。
    let runtime = runtime(&layout, pool_config());
    register_wait(&runtime);
    // Initial capacity permits two residents but only one active operation.
    // 初始容量允许两个常驻，但仅允许一个活动操作。
    let config = capacity_policy(PoolKind::Shared, 0, 2);
    // Retain exact ownership while shrinking below the original declaration.
    // 缩至低于原声明时保留精确归属。
    let id = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Business and closing execute on the same real VM under the original module declaration.
    // 业务与关闭在原模块声明下使用同一真实 VM 执行。
    let pool = runtime.register_pool_in_capacity(&id, finalization::closing_definition(&layout, r#"
return {
-- Keep the original business operation active until acknowledged.
-- 在确认前保持原业务操作活动。
call=function() return vulcan.host.call('test.wait','business') end,
-- Prove that the exact original instance closes normally.
-- 证明精确原实例正常关闭。
shutdown=function() local result=vulcan.host.call('test.wait','closing'); assert(result.ok); return true end}
"#, 5000), member_policy(&config, InstanceReuse::Reusable), permissions(), "closing-policy".into()).unwrap();
    // This operation must succeed despite revision-driven cache retirement.
    // 即使修订要求缓存退役，此操作也必须成功。
    let operation = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(8))
        .unwrap();
    // Wait for a real business callback before applying the revision.
    // 应用修订前等待真实业务回调。
    let business = host_request(&runtime);
    assert_eq!(business.arguments, json!("business"));
    // Exact predecessor prevents a stale control plane from replacing another writer's policy.
    // 精确前驱防止过期控制面替换其他写入者的策略。
    let before = runtime.capacity_policy(&id).unwrap();
    runtime
        .revise_capacity(
            &id,
            &before.revision,
            capacity_policy(PoolKind::Dedicated, 1, 1),
        )
        .unwrap();
    assert_eq!(runtime.capacity(&id).unwrap().active_operations, 1);
    acknowledge(&runtime, business);
    assert_eq!(
        operation.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    // Independent finalization remains a real capacity owner until explicitly acknowledged.
    // 独立关闭在明确确认前仍为真实容量所有者。
    let closing = host_request(&runtime);
    assert_eq!(closing.arguments, json!("closing"));
    assert_eq!(runtime.capacity(&id).unwrap().resources.resident, 1);
    assert_eq!(runtime.capacity(&id).unwrap().active_operations, 1);
    acknowledge(&runtime, closing);
    residents(&runtime, &id, 0);
    assert_eq!(runtime.capacity(&id).unwrap().committed_resident_vms, 1);
    shutdown(&runtime);
}

/// Lowering physical execution cannot invalidate already dispatched workers; retry after they drain is explicit.
/// 降低物理执行额度不能使已分发工作失效；排空后的重试必须显式执行。
#[test]
fn embedded_capacity_policy_execution_shrink_requires_dispatched_drain() {
    // Separate pools allow two real calls to hold execution permits concurrently.
    // 独立池允许两个真实调用同时持有执行许可。
    let layout = SystemRuntimeTestLayout::new("capacity revision dispatched drain");
    // Parent fixture has exactly two physical execution permits.
    // 父级夹具恰好拥有两个物理执行许可。
    let runtime = runtime(&layout, pool_config());
    register_wait(&runtime);
    // Both member domains share the original two-permit capacity.
    // 两个成员域共享原双许可容量。
    let mut config = capacity_policy(PoolKind::Shared, 0, 2);
    config.resources.max_running_calls = 2;
    // Preserve this identity and revision when a busy change is rejected.
    // 忙碌变更被拒绝时保留此身份及修订。
    let id = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Accepted operations and broker requests are retained until independent acknowledgement.
    // 已接纳操作及代理请求保留至独立确认。
    let mut operations = Vec::new();
    // Exact broker receipts serve as deterministic evidence of concurrent physical execution.
    // 精确代理回执作为并发物理执行的确定性证据。
    let mut requests = Vec::new();
    for _ in 0..2 {
        // One serial domain per call avoids hiding the aggregate execution boundary.
        // 每调用一个串行域，避免掩盖聚合执行边界。
        let mut policy = member_policy(&config, InstanceReuse::SingleCall);
        policy.max_running_calls = 1;
        // This real module blocks inside the authorized host callback.
        // 此真实模块阻塞于已授权宿主回调内。
        let pool = runtime
            .register_pool_in_capacity(
                &id,
                definition(
                    &layout,
                    r#"
return {
-- Hold one real execution permit until acknowledged.
-- 在确认前持有一个真实执行许可。
call=function() return vulcan.host.call('test.wait',{}) end}
"#,
                ),
                policy,
                permissions(),
                "parallel-policy".into(),
            )
            .unwrap();
        operations.push(
            runtime
                .submit(call(&pool, Value::Null), Duration::from_secs(8))
                .unwrap(),
        );
        requests.push(host_request(&runtime));
    }
    // Read the exact policy before attempting to shrink under two already dispatched calls.
    // 在两个已分发调用下尝试缩容前读取精确策略。
    let before = runtime.capacity_policy(&id).unwrap();
    // One-permit target requires the admitted pair to release before committing.
    // 单许可目标要求已入场的两个调用释放后才提交。
    let target = capacity_policy(PoolKind::Shared, 0, 1);
    assert_eq!(
        runtime
            .revise_capacity(&id, &before.revision, target.clone())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        runtime.capacity_policy(&id).unwrap().revision,
        before.revision
    );
    assert_eq!(runtime.capacity(&id).unwrap().config, config);
    for request in requests {
        acknowledge(&runtime, request);
    }
    for operation in operations {
        assert_eq!(
            operation.wait(Duration::from_secs(3)).unwrap().phase,
            OperationPhase::Succeeded
        );
    }
    runtime
        .revise_capacity(&id, &before.revision, target.clone())
        .unwrap();
    assert_eq!(runtime.capacity(&id).unwrap().config, target);
    shutdown(&runtime);
}
