//! Real formal-runtime capacity tests retain isolated module state under shared scheduling ownership.
//! 真实正式运行时容量测试在共享调度归属下保留隔离模块状态。

use super::*;

mod admission;
mod closing;

/// Build capacity resources with explicit kind, minimum and maximum; return bounded test queue policy.
/// 使用显式类别、最小值及最大值构造容量资源；返回有界测试队列策略。
fn capacity_policy(kind: PoolKind, minimum: usize, maximum: usize) -> EmbeddedCapacityConfig {
    // Queue limits originate in the same parent fixture used by this module.
    // 队列上限来自本模块使用的同一父级夹具。
    let parent = pool_config();
    EmbeddedCapacityConfig {
        resources: VmCapacityConfig {
            kind,
            min_resident_vms: minimum,
            max_resident_vms: maximum,
            max_running_calls: 1,
        },
        max_queued_calls: parent.max_queued_calls,
        max_queued_bytes: parent.max_queued_bytes,
    }
}

/// Derive a zero-reservation member from its exact capacity policy and explicit reuse mode.
/// 从精确容量策略及显式复用模式派生零预留成员。
fn member_policy(capacity: &EmbeddedCapacityConfig, reuse: InstanceReuse) -> PluginPoolConfig {
    // Physical ownership is reserved once by the capacity, never independently by each module.
    // 物理归属由容量预留一次，绝不由各模块独立预留。
    let mut policy = pool_policy(reuse);
    policy.kind = capacity.resources.kind;
    policy.min_resident_vms = 0;
    policy.max_resident_vms = capacity.resources.max_resident_vms;
    policy.max_running_calls = capacity.resources.max_running_calls;
    policy.max_queued_calls = capacity.max_queued_calls;
    policy
}

/// Register one queued host capability used as an externally controlled execution barrier.
/// 注册一个用作外部可控执行屏障的排队宿主能力。
fn register_wait(runtime: &EmbeddedRuntime) {
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
}

/// Acknowledge the exact retained request without changing its immutable caller or ownership.
/// 确认精确保留请求，不改变不可变调用方及归属。
fn acknowledge(runtime: &EmbeddedRuntime, request: HostRequest) {
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
}

/// Wait for a real invocation on pool and return its published value, failing with the original diagnostic.
/// 等待池上的真实调用并返回已发布值；失败时显示原诊断。
fn invoke(runtime: &EmbeddedRuntime, pool: &str) -> Value {
    // The operation deadline and bounded observation prevent a scheduling regression from hanging this test.
    // 操作截止时间及有界观测避免调度回归挂住此测试。
    let snapshot = runtime
        .submit(call(pool, Value::Null), Duration::from_secs(3))
        .unwrap()
        .wait(Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        snapshot.phase,
        OperationPhase::Succeeded,
        "{:?}",
        snapshot.error
    );
    snapshot.value.unwrap()
}

/// Empty capacity guarantees remain charged exactly once until explicit member and capacity retirement.
/// 空容量保证在显式成员及容量退役前始终精确计费一次。
#[test]
fn embedded_capacity_registration_retains_plugin_guarantees_and_exact_ownership() {
    // Leave parent headroom so rejected guarantees prove the plugin boundary independently.
    // 保留父级余量，使被拒绝保证独立证明插件边界。
    let layout = SystemRuntimeTestLayout::new("formal capacity ownership");
    // Parent fixture limits remain the single source for inherited plugin constraints.
    // 父级夹具限制保持为继承插件约束的唯一来源。
    let parent = pool_config();
    // Explicit plugin budget isolates plugin-level rejection from parent capacity.
    // 显式插件预算将插件级拒绝与父级容量区分。
    let mut plugin = plugin_policy(&parent);
    plugin.max_resident_vms = 2;
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime_with_plugin(&layout, parent.clone(), plugin);
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Dedicated, 2, 2);
    // Exact retained capacity ownership is never resolved through a fallback.
    // 精确保留容量归属绝不通过回退解析。
    let capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    assert_eq!(runtime.capacity(&capacity).unwrap().resources.resident, 0);
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .committed_resident_vms,
        2
    );
    assert_eq!(
        runtime
            .register_capacity(
                &layout.package_id,
                capacity_policy(PoolKind::Dedicated, 1, 1)
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    // Distinct generation registrations must not multiply the same physical promise.
    // 不同代次注册不得倍增同一物理承诺。
    let mut pools = Vec::new();
    for generation in ["old", "new"] {
        // Source cannot execute during registration; each definition keeps its own generation.
        // 注册期间不能执行源码；各定义保留自身代次。
        let mut module = definition(&layout, "error('registration must not execute source')");
        module.generation = generation.into();
        pools.push(
            runtime
                .register_pool_in_capacity(
                    &capacity,
                    module,
                    member_policy(&config, InstanceReuse::Reusable),
                    permissions(),
                    generation.into(),
                )
                .unwrap(),
        );
    }
    assert_eq!(runtime.capacity(&capacity).unwrap().retained_pools, 2);
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .committed_resident_vms,
        2
    );
    // A different registered plugin cannot attach a module to this capacity.
    // 另一已注册插件不能将模块加入此容量。
    runtime
        .register_plugin("foreign".into(), plugin_policy(&parent))
        .unwrap();
    // The requested module explicitly names a different plugin owner.
    // 请求模块显式指定不同插件所有者。
    let mut foreign = definition(&layout, "error('foreign source')");
    foreign.plugin_id = "foreign".into();
    assert_eq!(
        runtime
            .register_pool_in_capacity(
                &capacity,
                foreign,
                member_policy(&config, InstanceReuse::Reusable),
                permissions(),
                "foreign".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    // A member-local minimum would duplicate the capacity reservation.
    // 成员局部最小值会重复容量预留。
    let mut conflicting = member_policy(&config, InstanceReuse::Reusable);
    conflicting.min_resident_vms = 1;
    assert_eq!(
        runtime
            .register_pool_in_capacity(
                &capacity,
                definition(&layout, "error('conflict')"),
                conflicting,
                permissions(),
                "conflict".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert_eq!(runtime.capacity(&capacity).unwrap().retained_pools, 2);
    assert_eq!(
        runtime.forget_capacity(&capacity).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    runtime.close_capacity(&capacity).unwrap();
    assert!(runtime.capacity(&capacity).unwrap().closing);
    assert_eq!(
        runtime.forget_capacity(&capacity).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    for pool in pools {
        runtime.forget_pool(&pool).unwrap();
    }
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .committed_resident_vms,
        2
    );
    runtime.close_plugin(&layout.package_id).unwrap();
    assert_eq!(
        runtime.forget_plugin(&layout.package_id).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    runtime.forget_capacity(&capacity).unwrap();
    runtime.forget_plugin(&layout.package_id).unwrap();
    assert_eq!(
        runtime.capacity(&capacity).unwrap_err().code,
        EmbeddedErrorCode::NotFound
    );
    runtime
        .register_plugin(layout.package_id.clone(), plugin_policy(&parent))
        .unwrap();
    // Replacement registration must receive a new opaque identity.
    // 替代注册必须获得新的不透明身份。
    let replacement = runtime
        .register_capacity(&layout.package_id, config)
        .unwrap();
    assert_ne!(replacement, capacity);
    shutdown(&runtime);
}

/// Cross-generation queue limits retain exact bytes while another capacity continues on the same plugin.
/// 跨代次队列限制保留精确字节计费，同时同插件另一容量继续执行。
#[test]
fn embedded_capacity_queue_and_execution_limits_span_members_and_release_cancelled_bytes() {
    // Parent and plugin have two workers; only the tested capacity is limited to one.
    // 父级及插件拥有两个工作线程；仅被测容量限制为一个。
    let layout = SystemRuntimeTestLayout::new("formal capacity queues");
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime(&layout, pool_config());
    register_wait(&runtime);
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let mut config = capacity_policy(PoolKind::Shared, 0, 2);
    config.max_queued_calls = 2;
    config.max_queued_bytes = 2048;
    // Exact retained capacity ownership is never resolved through a fallback.
    // 精确保留容量归属绝不通过回退解析。
    let capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Retain this exact module identity to verify cross-member isolation and ordering.
    // 保留此精确模块身份以验证跨成员隔离及顺序。
    let first = runtime
        .register_pool_in_capacity(
            &capacity,
            definition(
                &layout,
                r#"
-- Await one authorized host request and return its structured result.
-- 等待一个已授权宿主请求并返回结构化结果。
return {call=function() return vulcan.host.call('test.wait',{}) end}
"#,
            ),
            member_policy(&config, InstanceReuse::Reusable),
            permissions(),
            "first".into(),
        )
        .unwrap();
    // Retain this exact module identity to verify cross-member isolation and ordering.
    // 保留此精确模块身份以验证跨成员隔离及顺序。
    let second = runtime
        .register_pool_in_capacity(
            &capacity,
            definition(
                &layout,
                r#"
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 42 end}
"#,
            ),
            member_policy(&config, InstanceReuse::Reusable),
            permissions(),
            "second".into(),
        )
        .unwrap();
    // The independent comparison path uses a bounded one-resident policy.
    // 独立对照路径使用有界单常驻策略。
    let other_config = capacity_policy(PoolKind::Shared, 0, 1);
    // A separate capacity owns its own reservation and execution allowance.
    // 独立容量拥有自身预留及执行额度。
    let other_capacity = runtime
        .register_capacity(&layout.package_id, other_config.clone())
        .unwrap();
    // The unrelated module provides observable progress outside the blocked capacity.
    // 无关模块提供被阻塞容量之外的可观察进展。
    let other = runtime
        .register_pool_in_capacity(
            &other_capacity,
            definition(
                &layout,
                r#"
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 7 end}
"#,
            ),
            member_policy(&other_config, InstanceReuse::Reusable),
            permissions(),
            "other".into(),
        )
        .unwrap();
    // The dispatched operation keeps its original identity through final cleanup.
    // 已分发操作跨最终清理保留原始身份。
    let running = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(5))
        .unwrap();
    // This exact host request holds execution until explicit acknowledgement.
    // 此精确宿主请求在显式确认前占用执行。
    let request = host_request(&runtime);
    // The first queued member consumes the same aggregate queue as the next member.
    // 首个排队成员与下个成员消费同一聚合队列。
    let a = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(5))
        .unwrap();
    // The second queued member tests aggregate admission across distinct modules.
    // 第二个排队成员测试跨不同模块的聚合入场。
    let b = runtime
        .submit(call(&second, Value::Null), Duration::from_secs(5))
        .unwrap();
    assert_eq!(invoke(&runtime, &other), json!(7));
    assert_eq!(b.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 1);
    assert_eq!(runtime.capacity(&capacity).unwrap().queued_calls, 2);
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
    // UTF-8 context exceeds half the capacity byte budget without exceeding one request's allowance.
    // UTF-8 上下文超过容量字节预算一半，但不超过单请求额度。
    let mut large = call(&second, Value::Null);
    large.context.client_budget = json!({"text":"中".repeat(350)});
    // Measure actual UTF-8 serialization rather than estimating argument length.
    // 测量实际 UTF-8 序列化，不估算参数长度。
    let bytes = serde_json::to_vec(&large).unwrap().len();
    assert!(bytes <= config.max_queued_bytes && bytes * 2 > config.max_queued_bytes);
    // Retain the waiting operation to observe cancellation and precise capacity recovery.
    // 保留等待操作，以观测取消及精确容量恢复。
    let queued = runtime
        .submit(large.clone(), Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        runtime
            .submit(large.clone(), Duration::from_secs(5))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(runtime.capacity(&capacity).unwrap().queued_bytes, bytes);
    queued.cancel().unwrap();
    assert_eq!(
        queued.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(runtime.capacity(&capacity).unwrap().queued_bytes, 0);
    // A fresh submission must succeed after cancelled queue ownership is released.
    // 取消的队列归属释放后，新提交必须成功。
    let accepted = runtime.submit(large, Duration::from_secs(5)).unwrap();
    acknowledge(&runtime, request);
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        accepted.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!(42))
    );
    shutdown(&runtime);
}

/// A full capacity replaces its own immutable module cache while another capacity keeps warm state.
/// 已满容量替换自身不可变模块缓存，同时另一容量保留热状态。
#[test]
fn embedded_capacity_pressure_transfers_own_guarantee_without_evicting_other_capacity() {
    // Register the unrelated cache first so an unscoped eviction would visibly destroy its counter.
    // 先注册无关缓存，使无归属驱逐会明显破坏其计数器。
    let layout = SystemRuntimeTestLayout::new("formal capacity pressure");
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime(&layout, pool_config());
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Dedicated, 1, 1);
    // A separate capacity owns its own reservation and execution allowance.
    // 独立容量拥有自身预留及执行额度。
    let other_capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Exact retained capacity ownership is never resolved through a fallback.
    // 精确保留容量归属绝不通过回退解析。
    let capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Real Lua code makes module state and lifetime boundaries observable.
    // 真实 Lua 代码使模块状态及寿命边界可观察。
    let source = r#"
        -- Retained module-local state detects accidental eviction or cross-module reuse.
        -- 保留的模块局部状态用于检测误驱逐或跨模块复用。
        local count=0
        -- Return the next counter value without external effects.
        -- 返回下一个计数值，不产生外部副作用。
        return {call=function() count=count+1; return count end}
    "#;
    // The unrelated module provides observable progress outside the blocked capacity.
    // 无关模块提供被阻塞容量之外的可观察进展。
    let other = runtime
        .register_pool_in_capacity(
            &other_capacity,
            definition(&layout, source),
            member_policy(&config, InstanceReuse::Reusable),
            permissions(),
            "other".into(),
        )
        .unwrap();
    // Retain this exact module identity to verify cross-member isolation and ordering.
    // 保留此精确模块身份以验证跨成员隔离及顺序。
    let first = runtime
        .register_pool_in_capacity(
            &capacity,
            definition(&layout, source),
            member_policy(&config, InstanceReuse::Reusable),
            permissions(),
            "first".into(),
        )
        .unwrap();
    // Retain this exact module identity to verify cross-member isolation and ordering.
    // 保留此精确模块身份以验证跨成员隔离及顺序。
    let second = runtime
        .register_pool_in_capacity(
            &capacity,
            definition(
                &layout,
                r#"
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 42 end}
"#,
            ),
            member_policy(&config, InstanceReuse::Reusable),
            permissions(),
            "second".into(),
        )
        .unwrap();
    for (pool, value) in [
        (&other, 1),
        (&first, 1),
        (&second, 42),
        (&first, 1),
        (&other, 2),
    ] {
        assert_eq!(invoke(&runtime, pool), json!(value));
        assert!(runtime.capacity(&capacity).unwrap().resources.resident <= 1);
    }
    assert_eq!(runtime.resources().unwrap().resident, 2);
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .committed_resident_vms,
        2
    );
    shutdown(&runtime);
}

/// Expiration retires surplus member caches but keeps the aggregate minimum once across generations.
/// 过期退役多余成员缓存，但跨代次仅保留一次聚合最小值。
#[test]
fn embedded_capacity_idle_expiration_preserves_aggregate_minimum() {
    // A short TTL deliberately expires both module caches under a one-instance aggregate floor.
    // 短 TTL 刻意使两个模块缓存过期，并受单实例聚合下限约束。
    let layout = SystemRuntimeTestLayout::new("formal capacity idle floor");
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime(&layout, pool_config());
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Dedicated, 1, 2);
    // Exact retained capacity ownership is never resolved through a fallback.
    // 精确保留容量归属绝不通过回退解析。
    let capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Member policy inherits the exact capacity while reserving no duplicate minimum.
    // 成员策略继承精确容量，不预留重复最小值。
    let mut policy = member_policy(&config, InstanceReuse::Reusable);
    policy.idle_ttl_ms = Some(1);
    for generation in ["first", "second"] {
        // Distinct revision identities prevent the second invocation from reusing the first module.
        // 不同修订身份防止第二次调用复用第一个模块。
        let pool = runtime
            .register_pool_in_capacity(
                &capacity,
                definition(
                    &layout,
                    r#"
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 1 end}
"#,
                ),
                policy.clone(),
                permissions(),
                generation.into(),
            )
            .unwrap();
        assert_eq!(invoke(&runtime, &pool), json!(1));
    }
    // Bound the wait for observable ownership transitions.
    // 为可观察归属转换设置等待边界。
    let deadline = Instant::now() + Duration::from_secs(3);
    while runtime.capacity(&capacity).unwrap().resources.resident != 1 {
        assert!(
            Instant::now() < deadline,
            "surplus capacity member must actually retire"
        );
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(runtime.capacity(&capacity).unwrap().resources.resident, 1);
    shutdown(&runtime);
}
