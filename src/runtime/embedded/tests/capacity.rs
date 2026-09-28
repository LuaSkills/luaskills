//! Physical capacity groups preserve aggregate guarantees independently of module registration churn.
//! 物理容量组保持聚合保证，独立于模块注册更替。

use super::*;
use std::sync::Arc;

/// Build one dedicated capacity with two resident slots and one physical execution permit.
/// 构造包含两个常驻槽位及一个物理执行许可的专用容量。
fn capacity_policy() -> VmCapacityConfig {
    VmCapacityConfig {
        kind: PoolKind::Dedicated,
        min_resident_vms: 2,
        max_resident_vms: 2,
        max_running_calls: 1,
    }
}

/// Derive a legal member policy from its actual capacity owner without copying reservations.
/// 从实际容量所有者派生合法成员策略，不复制预留。
fn member_policy() -> PluginPoolConfig {
    // Member state stays independently reusable while its physical bounds share one capacity owner.
    // 成员状态保持独立可复用，而其物理边界共享单个容量所有者。
    let capacity = capacity_policy();
    PluginPoolConfig {
        kind: capacity.kind,
        max_resident_vms: capacity.max_resident_vms,
        max_running_calls: capacity.max_running_calls,
        ..policy(capacity.kind, 0)
    }
}

/// Empty capacity reservations survive member churn and are charged once across all members.
/// 空容量预留跨成员更替存活，并且全部成员共同仅计费一次。
#[test]
fn embedded_capacity_reservations_are_not_duplicated_or_lent_between_modules() {
    // Three parent slots leave one for unrelated work even before capacity members exist.
    // 父级三个槽位在容量成员尚不存在时，也为无关工作留下一个。
    let governor = PoolGovernor::new(config()).expect("governor");
    governor
        .register_capacity("tools", capacity_policy())
        .expect("capacity guarantee");
    governor
        .register_group("other", policy(PoolKind::Shared, 0))
        .expect("independent module");
    // The unused reservation is already protected before the first plugin call.
    // 首次插件调用前，未使用预留已经受到保护。
    let other = governor.reserve("other").expect("unreserved slot");
    assert!(
        matches!(governor.reserve("other"), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    for group in ["old-generation", "new-generation"] {
        governor
            .register_group_in_capacity(group, member_policy(), "tools")
            .expect("same capacity membership");
    }
    // Each generation gets its own physical slot without reserving two additional minimums.
    // 每代获得独立物理槽位，无需再预留两份最小值。
    let old = governor.reserve("old-generation").expect("old module");
    // A separate generation consumes another slot from the same original guarantee.
    // 独立代次从同一原保证消费另一个槽位。
    let new = governor.reserve("new-generation").expect("new module");
    assert_eq!(
        governor
            .capacity("tools")
            .expect("capacity")
            .resources
            .resident,
        2
    );
    assert_eq!(
        governor
            .capacity("tools")
            .expect("capacity")
            .committed_resident_vms,
        2
    );
    drop((old, new));
    for group in ["old-generation", "new-generation"] {
        governor
            .unregister_group(group)
            .expect("removed module identity");
    }
    assert_eq!(
        governor
            .capacity("tools")
            .expect("retained guarantee")
            .resources
            .resident,
        0
    );
    assert!(
        matches!(governor.reserve("other"), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    governor
        .unregister_capacity("tools")
        .expect("explicit capacity removal");
    // Only explicit removal returns the unused guarantee to unrelated work.
    // 只有显式移除才将未使用保证归还无关工作。
    let released = governor.reserve("other").expect("returned reserved slot");
    drop((other, released));
}

/// Actual execution and failed retirement consume the aggregate in the same transaction as leaf and parent.
/// 实际执行及失败退役在与叶级和父级相同的事务中消费聚合预算。
#[test]
fn embedded_capacity_execution_and_retirement_remain_atomic_across_members() {
    // Keep one spare parent execution permit to distinguish aggregate rejection from parent saturation.
    // 保留一个父级空闲执行许可，以区分聚合拒绝和父级饱和。
    let governor = PoolGovernor::new(config()).expect("governor");
    governor
        .register_capacity("actions", capacity_policy())
        .expect("capacity");
    for group in ["first", "second"] {
        governor
            .register_group_in_capacity(group, member_policy(), "actions")
            .expect("member");
    }
    // Distinct allocations share one execution limit without sharing state.
    // 独立分配共享单个执行限制，不共享状态。
    let mut first = governor.reserve("first").expect("first allocation");
    // The second member retains its own construction state on aggregate admission failure.
    // 聚合入场失败时，第二成员保留自身构造状态。
    let mut second = governor.reserve("second").expect("second allocation");
    // This actual permit saturates only the common capacity's running limit.
    // 此实际许可仅占满共同容量的运行上限。
    let running = first.begin_execution().expect("first physical permit");
    assert!(
        matches!(second.begin_execution(), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    assert_eq!(
        governor
            .capacity("actions")
            .expect("actual counters")
            .resources
            .running,
        1
    );
    assert_eq!(
        governor
            .usage(Some("second"))
            .expect("unchanged phase")
            .creating,
        1
    );
    drop(running);
    second.begin_execution().expect("released aggregate permit");
    first
        .mark_retiring()
        .expect("failed cleanup remains charged");
    assert!(
        matches!(governor.reserve("second"), Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
    );
    assert_eq!(
        governor
            .unregister_group("first")
            .expect_err("actual retiring ownership")
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        governor
            .unregister_capacity("actions")
            .expect_err("member ownership")
            .code,
        EmbeddedErrorCode::Busy
    );
    drop(first);
    governor
        .unregister_group("first")
        .expect("physical retirement");
    drop(second);
    governor
        .unregister_group("second")
        .expect("second retirement");
    governor
        .unregister_capacity("actions")
        .expect("released ownership");
}

/// Invalid membership and failed reservations leave original ownership unchanged.
/// 非法成员关系和失败预留保持原所有权不变。
#[test]
fn embedded_capacity_rejects_conflicting_membership_without_mutation() {
    // Existing dedicated capacity must not be reduced when another registration fails.
    // 另一注册失败时不能降低既有专用容量。
    let governor = PoolGovernor::new(config()).expect("governor");
    governor
        .register_capacity("tools", capacity_policy())
        .expect("initial capacity");
    assert_eq!(
        governor
            .register_capacity("extra", capacity_policy())
            .expect_err("parent guarantee exceeded")
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        governor
            .register_capacity("tools", capacity_policy())
            .expect_err("duplicate")
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        governor
            .register_group_in_capacity("missing", member_policy(), "unknown")
            .expect_err("exact owner required")
            .code,
        EmbeddedErrorCode::NotFound
    );
    for case in ["kind", "reservation", "resident", "running"] {
        // Change only one otherwise valid member field.
        // 仅改变一个原本有效的成员字段。
        let mut invalid = member_policy();
        match case {
            "kind" => invalid.kind = PoolKind::Shared,
            "reservation" => invalid.min_resident_vms = 1,
            "resident" => invalid.max_resident_vms += 1,
            "running" => invalid.max_running_calls += 1,
            _ => unreachable!("fixed mutation case"),
        }
        assert_eq!(
            governor
                .register_group_in_capacity(case, invalid, "tools")
                .expect_err("conflicting member")
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        governor
            .capacity("tools")
            .expect("original capacity")
            .registered_groups,
        0
    );
    assert_eq!(
        governor.capacity("tools").expect("original policy").config,
        capacity_policy()
    );
    governor
        .register_group_in_capacity("valid", member_policy(), "tools")
        .expect("valid member");
    assert_eq!(
        governor
            .unregister_capacity("tools")
            .expect_err("even empty member retains its policy")
            .code,
        EmbeddedErrorCode::Busy
    );
    governor.unregister_group("valid").expect("member removed");
    governor
        .unregister_capacity("tools")
        .expect("capacity removed");
}

/// All registration dimensions are bounded even when no physical VM has been created.
/// 即使没有创建物理 VM，全部注册维度也受限。
#[test]
fn embedded_capacity_metadata_is_bounded_and_failed_registration_is_absent() {
    // One shared capacity consumes no minimum, exposing only metadata limits.
    // 单个公共容量不消耗最小值，仅暴露元数据限制。
    let governor = PoolGovernor::new(config()).expect("governor");
    // No unused physical guarantee obscures the registration-count boundary.
    // 不用未使用物理保证掩盖注册数量边界。
    let shared = policy(PoolKind::Shared, 0).capacity();
    for index in 0..governor.config().max_registered_pools {
        governor
            .register_capacity(&format!("capacity-{index}"), shared.clone())
            .expect("bounded capacity");
        governor
            .register_group_in_capacity(
                &format!("module-{index}"),
                policy(PoolKind::Shared, 0),
                "capacity-0",
            )
            .expect("bounded member");
    }
    assert_eq!(
        governor
            .register_capacity("overflow", shared)
            .expect_err("capacity table full")
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        governor
            .capacity("overflow")
            .expect_err("no partial owner")
            .code,
        EmbeddedErrorCode::NotFound
    );
    assert_eq!(
        governor
            .register_group_in_capacity("overflow", policy(PoolKind::Shared, 0), "capacity-0")
            .expect_err("member table full")
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        governor
            .usage(Some("overflow"))
            .expect_err("no partial member")
            .code,
        EmbeddedErrorCode::NotFound
    );
}

/// Concurrent module allocations cannot race through their shared aggregate limit.
/// 并发模块分配不能竞争穿越共同聚合上限。
#[test]
fn embedded_capacity_parallel_reservations_never_overcommit() {
    // Capacity membership is established before any competing allocation begins.
    // 任何竞争分配开始前已建立容量成员关系。
    let governor = PoolGovernor::new(config()).expect("governor");
    governor
        .register_capacity("parallel", capacity_policy())
        .expect("capacity");
    // The existing parent registration limit bounds the number of contenders.
    // 既有父级注册上限限制竞争者数量。
    let count = governor.config().max_registered_pools;
    // Two phases retain successful slots until the parent captures the actual aggregate.
    // 两个阶段保留成功槽位，直至父线程捕获实际聚合状态。
    let barrier = Arc::new(std::sync::Barrier::new(count + 1));
    // Each bounded contender shares the one authoritative governor.
    // 每个有界竞争者共享同一权威治理器。
    let workers = (0..count)
        .map(|index| {
            // Each worker reserves from an independent module identity.
            // 每个工作线程从独立模块身份预留。
            let group = format!("parallel-{index}");
            governor
                .register_group_in_capacity(&group, member_policy(), "parallel")
                .expect("member");
            // Strong owners retain the one ledger and the two-phase observation barrier.
            // 强所有者保留同一账本及两阶段观测屏障。
            let owner = governor.clone();
            // Hold the same observation barrier in each worker.
            // 在每个工作线程持有同一观测屏障。
            let gate = barrier.clone();
            std::thread::spawn(move || {
                // Keep successful reservations alive until the parent has inspected all contenders.
                // 父线程检查全部竞争者前，保持成功预留存活。
                let allocation = owner.reserve(&group);
                gate.wait();
                gate.wait();
                allocation.map(drop).map_err(|error| error.code)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    // Capture evidence before releasing workers, but assert only after joining every thread.
    // 释放工作线程前捕获证据，但仅在等待全部线程后断言。
    let observed = governor
        .capacity("parallel")
        .expect("atomic view")
        .resources
        .resident;
    barrier.wait();
    // Failed reservations must leave no partially charged physical slot.
    // 失败预留不能留下部分计费的物理槽位。
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect::<Vec<_>>();
    assert_eq!(observed, capacity_policy().max_resident_vms);
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        observed
    );
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|code| *code == EmbeddedErrorCode::CapacityExceeded)
    );
    assert_eq!(
        governor
            .usage(None)
            .expect("all actual owners released")
            .resident,
        0
    );
}
