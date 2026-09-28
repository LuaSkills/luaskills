//! Actual isolated Lua instances consume one persistent capacity guarantee.
//! 实际隔离 Lua 实例消费单个持久容量保证。

use super::*;
use crate::runtime::embedded::EffectState;
use crate::runtime::embedded::capabilities::{
    CapabilityExecution, CapabilityOutcome, CapabilityRegistrationRequest, CapabilityRegistry,
};
use crate::runtime::embedded::{ModulePoolPlacement, VmCapacityConfig};

/// Bound physical ownership observations without inferring completion from request return.
/// 限制物理所有权观测时长，不从请求返回推断完成。
const WAIT: Duration = Duration::from_secs(5);

/// Return a dedicated two-VM capacity with one physical execution permit.
/// 返回包含两个 VM 及单个物理执行许可的专用容量。
fn capacity_policy() -> VmCapacityConfig {
    VmCapacityConfig {
        kind: PoolKind::Dedicated,
        min_resident_vms: 2,
        max_resident_vms: 2,
        max_running_calls: 1,
    }
}

/// Build a reusable member policy; its minimum stays on the capacity owner, never on each module.
/// 构造可复用成员策略；最小值保留在容量所有者上，绝不分配给每个模块。
fn member_policy() -> PluginPoolConfig {
    // Derive all physical limits from the exact capacity policy under test.
    // 从待测精确容量策略派生全部物理限制。
    let capacity = capacity_policy();
    PluginPoolConfig {
        kind: capacity.kind,
        max_resident_vms: capacity.max_resident_vms,
        max_running_calls: capacity.max_running_calls,
        ..pool_policy(InstanceReuse::Reusable)
    }
}

/// Observe member unregistration and real worker termination before releasing the fixture.
/// 释放夹具前观测成员注销及真实工作线程结束。
fn close_manager(manager: &EmbeddedPoolManager) {
    manager.request_close().expect("close physical parent");
    // Keep a finite observation deadline independent of business invocation control.
    // 保持独立于业务调用控制的有限观测截止时间。
    let deadline = Instant::now() + WAIT;
    while !manager.poll_closed().expect("actual worker state") {
        assert!(Instant::now() < deadline, "physical parent did not close");
        std::thread::yield_now();
    }
}

/// Real prewarming uses one reservation across package generations while Lua counters remain independent.
/// 真实预热跨包代次使用同一预留，而 Lua 计数状态保持独立。
#[test]
fn embedded_capacity_real_modules_preserve_state_isolation_and_empty_guarantees() {
    // Existing runtime fixtures provide real package and dependency boundaries.
    // 既有运行时夹具提供真实包与依赖边界。
    let layout = SystemRuntimeTestLayout::new("capacity module generations");
    // One real engine owns all independent module pools in this test.
    // 单个真实引擎拥有此测试全部独立模块池。
    let manager = pool_manager(&layout);
    manager
        .register_capacity("plugin-tools", capacity_policy())
        .expect("reserved before module creation");
    // Unrelated work can consume only the unreserved parent slot.
    // 无关工作仅可消费父级未预留槽位。
    let shared = manager
        .create_pool(
            "other".into(),
            definition(&layout, "-- Return the unrelated module marker without arguments.\n-- 无参数返回无关模块标记。\nreturn {call=function() return 'other' end}"),
            pool_policy(InstanceReuse::Reusable),
        )
        .expect("shared module");
    // Keep the one unreserved actual VM occupied throughout the reservation proof.
    // 在预留证明期间保持唯一未预留实际 VM 被占用。
    let mut other = shared.acquire(control()).expect("unreserved VM");
    assert!(
        matches!(shared.acquire(control()), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    // Each generation's Lua counter is private even though physical capacity is shared.
    // 每代 Lua 计数器均私有，即使物理容量共享。
    let source = r#"
-- Keep one private counter inside this exact module instance.
-- 在此精确模块实例内部保留私有计数器。
local n = 0
-- Increment private state without arguments and return the updated count.
-- 无参数递增私有状态并返回更新后计数。
return {call=function() n=n+1; return n end}
"#;
    // Different package generations and security partitions retain separate module ownership.
    // 不同包代次及安全分区保留独立模块所有权。
    let pools = ["old", "new"]
        .into_iter()
        .map(|generation| {
            // Freeze generation and security identity before creating the module pool.
            // 创建模块池前冻结代次及安全身份。
            let mut module = definition(&layout, source);
            module.generation = generation.into();
            module.security_partition = format!("workspace-{generation}");
            manager
                .create_pool_with_placement(
                    ModulePoolPlacement::Capacity {
                        group: generation.into(),
                        capacity_id: "plugin-tools".into(),
                    },
                    module,
                    member_policy(),
                    None,
                    None,
                )
                .expect("isolated capacity member")
        })
        .collect::<Vec<_>>();
    for pool in &pools {
        pool.prewarm(1, control()).expect("real module prewarm");
    }
    assert_eq!(
        manager
            .capacity("plugin-tools")
            .expect("warm capacity")
            .resources
            .idle,
        2
    );
    assert_eq!(manager.usage().expect("parent actual usage").resident, 3);
    for expected in [1, 2] {
        for pool in &pools {
            // Reacquire each pool's real VM; no other member may provide its cached state.
            // 重新获取每个池的真实 VM；其他成员不能提供其缓存状态。
            let mut lease = pool.acquire(control()).expect("same exact member VM");
            assert_eq!(count(&mut lease).expect("isolated count"), json!(expected));
        }
    }
    assert_eq!(count(&mut other).expect("other continues"), json!("other"));
    for pool in &pools {
        pool.close().expect("close member");
        drained(pool);
    }
    // Registration release may follow the last physical slot; inspect its own actual checkpoint.
    // 注册释放可能晚于最后物理槽位；检查其自身实际检查点。
    let deadline = Instant::now() + WAIT;
    while manager
        .capacity("plugin-tools")
        .expect("retained capacity")
        .registered_groups
        != 0
    {
        assert!(
            Instant::now() < deadline,
            "member registration did not retire"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        manager
            .capacity("plugin-tools")
            .expect("empty capacity")
            .committed_resident_vms,
        2
    );
    assert!(
        matches!(shared.acquire(control()), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    manager
        .unregister_capacity("plugin-tools")
        .expect("explicit reservation release");
    // Closed old pool handles still exist, but cannot retain or remove a later capacity identity.
    // 已关闭旧池句柄仍存在，但不能保留或移除后续容量身份。
    let extra = shared
        .acquire(control())
        .expect("released capacity becomes usable");
    drop(extra);
    drop(other);
    close_manager(&manager);
    assert_eq!(
        manager
            .register_capacity("late", capacity_policy())
            .expect_err("no publication after close")
            .code,
        EmbeddedErrorCode::Closed
    );
}

/// A real native callback holds the shared execution limit while another capacity still runs.
/// 真实原生回调持有共同执行上限，同时另一容量仍可执行。
#[test]
fn embedded_capacity_native_wait_enforces_cross_module_execution_limit() {
    // Controlled channels identify actual callback entry and release without timing guesses.
    // 受控通道标识实际回调进入与释放，不猜测时序。
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    // An independent release channel keeps the original callback alive until explicitly released.
    // 独立释放通道保持原回调存活，直至显式释放。
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    // Keep physical package files alive through callback and VM retirement.
    // 跨回调及 VM 退役保持物理包文件存活。
    let layout = SystemRuntimeTestLayout::new("capacity callback ownership");
    // All three physical VMs share this parent, but only two share the restricted capacity.
    // 三个物理 VM 共享此父级，但只有两个共享受限容量。
    let manager = pool_manager(&layout);
    manager
        .register_capacity("plugin-ui", capacity_policy())
        .expect("capacity");
    // One immutable native capability snapshot is shared by independently keyed module pools.
    // 单个不可变原生能力快照由独立键模块池共享。
    let registry = CapabilityRegistry::new("capacity-native".into(), manager.config().clone())
        .expect("registry");
    // Native registration requires a synchronized receiver that outlives the callback.
    // 原生注册需要比回调存活更久的同步接收器。
    let release = Mutex::new(release_rx);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "pool.probe",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(move |_| {
                entered_tx.send(()).expect("callback evidence");
                release
                    .lock()
                    .expect("release channel")
                    .recv_timeout(WAIT)
                    .expect("bounded release");
                CapabilityOutcome {
                    result: Ok(json!(true)),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .expect("native handler");
    // Bind the actual immutable registry snapshot and exact permission grant.
    // 绑定实际不可变注册表快照及精确权限授予。
    let (_, binding) = super::super::capabilities::binding(&registry);
    // Both declared members use a real host wait, so accidental admission is observable.
    // 两个已声明成员均使用真实宿主等待，因此意外入场可观察。
    let source = r#"
-- Wait in the native test handler without arguments; return its confirmed result.
-- 无参数在原生测试处理器中等待；返回其已确认结果。
return {call=function()
    -- Preserve the actual capability result before checking its declared status.
    -- 在检查已声明状态前保留实际能力结果。
    local r = vulcan.host.call('pool.probe', {index=0})
    if not r.ok then error(r.error.message) end
    return r.value
end}
"#;
    // The first member will hold the capacity's actual execution permit.
    // 第一成员将持有容量的实际执行许可。
    let first = manager
        .create_pool_with_placement(
            ModulePoolPlacement::Capacity {
                group: "first".into(),
                capacity_id: "plugin-ui".into(),
            },
            definition(&layout, source),
            member_policy(),
            Some(binding.clone()),
            None,
        )
        .expect("first member");
    // A separate member must obey the same aggregate permit without sharing Lua state.
    // 独立成员必须遵守同一聚合许可，且不共享 Lua 状态。
    let second = manager
        .create_pool_with_placement(
            ModulePoolPlacement::Capacity {
                group: "second".into(),
                capacity_id: "plugin-ui".into(),
            },
            definition(&layout, source),
            member_policy(),
            Some(binding),
            None,
        )
        .expect("second member");
    // Complete initialization before deliberately blocking the first real invocation.
    // 刻意阻塞首个真实调用前完成初始化。
    let mut first_lease = first.acquire(control()).expect("first VM");
    // Prepare the other member before any running permit is deliberately held.
    // 刻意持有任何运行许可前，准备另一成员。
    let mut second_lease = second.acquire(control()).expect("second VM");
    // Unrelated work uses independent physical capacity in the same parent.
    // 无关工作在同一父级中使用独立物理容量。
    let shared = manager
        .create_pool(
            "other".into(),
            definition(&layout, "-- Return an unrelated result without entering the native wait.\n-- 不进入原生等待，返回无关结果。\nreturn {call=function() return 7 end}"),
            pool_policy(InstanceReuse::Reusable),
        )
        .expect("other capacity");
    // Keep the independent VM ready so its invocation tests execution capacity alone.
    // 保持独立 VM 就绪，使其调用仅测试执行容量。
    let mut shared_lease = shared.acquire(control()).expect("other VM");
    // Only the original call enters a real blocking native handler.
    // 只有原调用进入真实阻塞原生处理器。
    let worker = std::thread::spawn(move || count(&mut first_lease));
    entered_rx
        .recv_timeout(WAIT)
        .expect("real callback entered");
    // Capture results before releasing the callback; defer assertions until the worker has joined.
    // 释放回调前捕获结果；断言延后至工作线程已结束。
    let denied = count(&mut second_lease);
    // A different capacity can consume the parent's remaining physical execution permit.
    // 不同容量可以消费父级剩余物理执行许可。
    let unrelated = count(&mut shared_lease);
    first.close().expect("close while native work remains");
    // Closing a pool cannot remove membership while its native callback still owns the VM.
    // 原生回调仍拥有 VM 时，关闭池不能移除成员关系。
    let removal = manager.unregister_capacity("plugin-ui");
    // Logical close leaves the original running permit charged until the real callback returns.
    // 逻辑关闭保留原运行许可计费，直至真实回调返回。
    let occupied = manager
        .capacity("plugin-ui")
        .expect("real usage")
        .resources
        .running;
    release_tx.send(()).expect("release actual work");
    // Joining proves the original callback actually left the native execution boundary.
    // 等待线程证明原回调实际离开原生执行边界。
    let completed = worker.join().expect("callback worker");
    assert_eq!(
        denied.expect_err("aggregate physical execution limit").code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(unrelated.expect("other capacity still executes"), json!(7));
    assert_eq!(
        removal.expect_err("physical owner still exists").code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(occupied, 1);
    assert_eq!(completed.expect("original callback result"), json!(true));
    assert!(
        entered_rx.try_recv().is_err(),
        "rejected member must not enter callback"
    );
    drop((second_lease, shared_lease));
    close_manager(&manager);
    manager
        .unregister_capacity("plugin-ui")
        .expect("all physical members retired");
}
