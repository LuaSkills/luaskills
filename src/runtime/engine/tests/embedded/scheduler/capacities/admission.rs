//! Capacity metadata and physical admission obey both plugin and parent limits before publication.
//! 容量元数据及物理入场在发布前同时遵守插件与父级限制。

use super::*;

/// Validate complete declarations and independently bound empty capacity registries at both ownership levels.
/// 校验完整声明，并在两个归属层级独立约束空容量注册表。
#[test]
fn embedded_capacity_metadata_limits_and_invalid_registration_are_atomic() {
    // One plugin may own one capacity while the parent and the second plugin can hold two.
    // 一个插件可持有一个容量，而父级及第二插件可持有两个。
    let layout = SystemRuntimeTestLayout::new("formal capacity metadata");
    // Parent fixture limits remain the single source for inherited plugin constraints.
    // 父级夹具限制保持为继承插件约束的唯一来源。
    let mut parent = pool_config();
    parent.max_registered_pools = 2;
    // Explicit plugin budget isolates plugin-level rejection from parent capacity.
    // 显式插件预算将插件级拒绝与父级容量区分。
    let mut plugin = plugin_policy(&parent);
    plugin.max_registered_pools = 1;
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime_with_plugin(&layout, parent.clone(), plugin);
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Shared, 0, 1);
    assert_eq!(
        runtime
            .register_capacity("missing", config.clone())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::NotFound
    );
    for invalid in [
        "queue-count",
        "queue-bytes",
        "shared-minimum",
        "plugin-running",
    ] {
        // Each failed declaration must leave the single available registration slot untouched.
        // 每个失败声明都必须保持唯一可用注册槽不被占用。
        let mut candidate = config.clone();
        match invalid {
            "queue-count" => candidate.max_queued_calls = 0,
            "queue-bytes" => candidate.max_queued_bytes = parent.max_queued_bytes + 1,
            "shared-minimum" => candidate.resources.min_resident_vms = 1,
            "plugin-running" => {
                candidate.resources.max_running_calls = parent.max_running_calls + 1
            }
            _ => unreachable!(),
        }
        assert_eq!(
            runtime
                .register_capacity(&layout.package_id, candidate)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
    }
    // Retain this capacity identity to verify metadata limits and explicit release.
    // 保留此容量身份以验证元数据限制及显式释放。
    let first = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    assert_eq!(
        runtime
            .register_capacity(&layout.package_id, config.clone())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    runtime
        .register_plugin("second".into(), plugin_policy(&parent))
        .unwrap();
    // Retain this capacity identity to verify metadata limits and explicit release.
    // 保留此容量身份以验证元数据限制及显式释放。
    let second = runtime.register_capacity("second", config.clone()).unwrap();
    assert_eq!(
        runtime
            .register_capacity("second", config.clone())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    runtime.close_capacity(&first).unwrap();
    assert_eq!(
        runtime
            .register_pool_in_capacity(
                &first,
                definition(&layout, "error('closed')"),
                member_policy(&config, InstanceReuse::Reusable),
                permissions(),
                "closed".into()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Closed
    );
    runtime.forget_capacity(&first).unwrap();
    // Replacement registration must receive a new opaque identity.
    // 替代注册必须获得新的不透明身份。
    let replacement = runtime.register_capacity("second", config).unwrap();
    assert_ne!(replacement, first);
    runtime.close_plugin("second").unwrap();
    assert!(runtime.capacity(&second).unwrap().closing);
    assert_eq!(
        runtime.forget_plugin("second").unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    runtime.forget_capacity(&second).unwrap();
    runtime.forget_capacity(&replacement).unwrap();
    runtime.forget_plugin("second").unwrap();
    shutdown(&runtime);
}

/// Concurrent fixed-session preparations cannot multiply one capacity's physical slot across modules.
/// 并发固定会话准备不能跨模块倍增同容量的物理槽位。
#[test]
fn embedded_capacity_concurrent_session_preparation_shares_one_physical_budget() {
    // Parent and plugin have headroom, isolating the capacity boundary from their own limits.
    // 父级及插件保留余量，将容量边界与它们自身上限区分。
    let layout = SystemRuntimeTestLayout::new("formal capacity concurrent sessions");
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime(&layout, pool_config());
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Dedicated, 1, 1);
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
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 1 end}
"#,
            ),
            member_policy(&config, InstanceReuse::Session),
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
return {call=function() return 2 end}
"#,
            ),
            member_policy(&config, InstanceReuse::Session),
            permissions(),
            "second".into(),
        )
        .unwrap();
    // Contenders intentionally target distinct modules within one physical capacity.
    // 竞争者刻意请求同一物理容量内的不同模块。
    let attempts = [&first, &second, &first, &second];
    // The barrier aligns competing submissions without altering admission serialization.
    // 屏障对齐竞争提交，不改变入场串行化。
    let gate = std::sync::Barrier::new(attempts.len());
    // Joined request outcomes prove the actual number of accepted physical owners.
    // 已合并请求结果证明实际接纳的物理所有者数量。
    let results = std::thread::scope(|scope| {
        // All request threads start together but actual allocation remains serialized by admission.
        // 全部请求线程共同开始，但实际分配仍由入场串行化。
        let workers = attempts
            .into_iter()
            .map(|pool| {
                // Real formal runtime retains plugin and physical ownership through cleanup.
                // 真实正式运行时跨清理保留插件及物理归属。
                let runtime = &runtime;
                // The barrier aligns competing submissions without altering admission serialization.
                // 屏障对齐竞争提交，不改变入场串行化。
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
    // Count only successfully admitted physical owners.
    // 仅统计成功接纳的物理所有者。
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
    assert_eq!(runtime.capacity(&capacity).unwrap().resources.resident, 1);
    assert_eq!(
        runtime.capacity(&capacity).unwrap().committed_resident_vms,
        1
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
