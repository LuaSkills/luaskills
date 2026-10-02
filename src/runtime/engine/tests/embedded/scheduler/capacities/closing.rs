//! Capacity execution charges cover finalization and physical retirement rather than only business Lua.
//! 容量执行计费覆盖关闭执行及物理退役，而不仅是业务 Lua。

use super::*;

/// Single-call cleanup holds its capacity while unrelated capacity work can still complete.
/// 单次调用清理占用其容量，同时无关容量工作仍可完成。
#[test]
fn embedded_capacity_single_call_finalization_retains_execution_allowance() {
    // Two parent workers distinguish aggregate capacity pressure from global worker starvation.
    // 两个父级工作线程区分聚合容量压力及全局线程饥饿。
    let layout = SystemRuntimeTestLayout::new("formal capacity single closing");
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime(&layout, pool_config());
    register_wait(&runtime);
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Shared, 0, 2);
    // Exact retained capacity ownership is never resolved through a fallback.
    // 精确保留容量归属绝不通过回退解析。
    let capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Real Lua code makes module state and lifetime boundaries observable.
    // 真实 Lua 代码使模块状态及寿命边界可观察。
    let source = r#"
        -- Capture the authorized host entry for business-independent cleanup.
        -- 捕获已授权宿主入口，用于独立于业务的清理。
        local host=vulcan.host.call
        return {
            -- Return a business value without waiting on the host.
            -- 返回业务值，不等待宿主。
            call=function() return 1 end,
            -- Await the host acknowledgement before the VM may retire.
            -- 等待宿主确认后，才允许 VM 退役。
            shutdown=function()
                -- Preserve the exact host acknowledgement before returning cleanup success.
                -- 返回清理成功前保留精确宿主确认。
                local result=host('test.wait',{}); assert(result.ok); return true end
        }
    "#;
    // Retain this exact module identity to verify cross-member isolation and ordering.
    // 保留此精确模块身份以验证跨成员隔离及顺序。
    let first = runtime
        .register_pool_in_capacity(
            &capacity,
            finalization::closing_definition(&layout, source, 5000),
            member_policy(&config, InstanceReuse::SingleCall),
            permissions(),
            "closing".into(),
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
            member_policy(&config, InstanceReuse::SingleCall),
            permissions(),
            "next".into(),
        )
        .unwrap();
    // The independent comparison path uses a bounded one-resident policy.
    // 独立对照路径使用有界单常驻策略。
    let other_config = capacity_policy(PoolKind::Shared, 0, 1);
    // The unrelated module provides observable progress outside the blocked capacity.
    // 无关模块提供被阻塞容量之外的可观察进展。
    let other = runtime
        .register_pool(
            definition(
                &layout,
                r#"
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 7 end}
"#,
            ),
            member_policy(&other_config, InstanceReuse::SingleCall),
            permissions(),
            "other".into(),
        )
        .unwrap();
    // The dispatched operation keeps its original identity through final cleanup.
    // 已分发操作跨最终清理保留原始身份。
    let running = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(3))
        .unwrap();
    // This exact host request holds execution until explicit acknowledgement.
    // 此精确宿主请求在显式确认前占用执行。
    let request = host_request(&runtime);
    // Retain the waiting operation to observe cancellation and precise capacity recovery.
    // 保留等待操作，以观测取消及精确容量恢复。
    let queued = runtime
        .submit(call(&second, Value::Null), Duration::from_secs(3))
        .unwrap();
    assert_eq!(invoke(&runtime, &other), json!(7));
    assert_eq!(running.snapshot().unwrap().phase, OperationPhase::Cleaning);
    assert_eq!(queued.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 1);
    acknowledge(&runtime, request);
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!(1))
    );
    assert_eq!(
        queued.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!(2))
    );
    assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 0);
    shutdown(&runtime);
}

/// Session and reusable finalizers obey the same capacity even after capacity or parent closure.
/// 会话及可复用实例关闭器在容量或父级关闭后仍遵守同一容量。
#[test]
fn embedded_capacity_closing_serializes_session_and_reusable_finalizers() {
    for reuse in [InstanceReuse::Session, InstanceReuse::Reusable] {
        // Two initialized instances must close serially within one capacity, without replay or a new VM.
        // 两个已初始化实例必须在同容量内串行关闭，不重放，也不创建新 VM。
        let layout = SystemRuntimeTestLayout::new("formal capacity independent finalizers");
        // Real formal runtime retains plugin and physical ownership through cleanup.
        // 真实正式运行时跨清理保留插件及物理归属。
        let runtime = runtime(&layout, pool_config());
        register_wait(&runtime);
        // Capacity policy is frozen before any member or operation is admitted.
        // 在接纳任何成员或操作前冻结容量策略。
        let config = capacity_policy(PoolKind::Dedicated, 1, 2);
        // Exact retained capacity ownership is never resolved through a fallback.
        // 精确保留容量归属绝不通过回退解析。
        let capacity = runtime
            .register_capacity(&layout.package_id, config.clone())
            .unwrap();
        // Retain exact member identities for ordered explicit forgetting after shutdown.
        // 保留精确成员身份，用于关闭后按序显式遗忘。
        let mut pools = Vec::new();
        // Pinned identities remain available until their original VMs and operations drain.
        // 固定身份保留至其原始 VM 及操作排空。
        let mut sessions = Vec::new();
        for label in ["first", "second"] {
            // Each exact original VM reports its module label from the closing export.
            // 各精确原始 VM 从关闭导出报告模块标签。
            let source = format!(
                r#"
                -- Capture the authorized host entry in the original instance.
                -- 在原实例中捕获已授权宿主入口。
                local host=vulcan.host.call
                return {{
                    -- Return this module's immutable label.
                    -- 返回此模块的不可变标签。
                    call=function() return '{label}' end,
                    -- Report and await cleanup for this exact module.
                    -- 报告并等待此精确模块的清理。
                    shutdown=function()
                -- Preserve the exact host acknowledgement before returning cleanup success.
                -- 返回清理成功前保留精确宿主确认。
                local result=host('test.wait','{label}'); assert(result.ok); return true end
                }}
            "#
            );
            // The exact module registration fixes policy, generation and cleanup ownership.
            // 精确模块注册固定策略、代次及清理归属。
            let pool = runtime
                .register_pool_in_capacity(
                    &capacity,
                    finalization::closing_definition(&layout, &source, 5000),
                    member_policy(&config, reuse),
                    permissions(),
                    label.into(),
                )
                .unwrap();
            if reuse == InstanceReuse::Session {
                // Session opening initializes without invoking business.
                // 会话开启执行初始化，不调用业务。
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
            } else {
                assert_eq!(invoke(&runtime, &pool), json!(label));
            }
            pools.push(pool);
        }
        assert_eq!(runtime.capacity(&capacity).unwrap().resources.resident, 2);
        runtime.close_capacity(&capacity).unwrap();
        // The exact closing request retains its original VM identity.
        // 精确关闭请求保留原始 VM 身份。
        let first = host_request(&runtime);
        // The independent comparison path uses a bounded one-resident policy.
        // 独立对照路径使用有界单常驻策略。
        let other_config = capacity_policy(PoolKind::Shared, 0, 1);
        // The unrelated module provides observable progress outside the blocked capacity.
        // 无关模块提供被阻塞容量之外的可观察进展。
        let other = runtime
            .register_pool(
                definition(
                    &layout,
                    r#"
-- Return the fixed marker without arguments or external effects.
-- 返回固定标记，不使用参数或产生外部副作用。
return {call=function() return 7 end}
"#,
                ),
                member_policy(&other_config, InstanceReuse::SingleCall),
                permissions(),
                "other".into(),
            )
            .unwrap();
        assert_eq!(invoke(&runtime, &other), json!(7));
        assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 1);
        assert!(
            runtime
                .capabilities()
                .host_requests()
                .take(1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            runtime.forget_capacity(&capacity).unwrap_err().code,
            EmbeddedErrorCode::Busy
        );
        // Preserve the first cleanup label to reject duplicate dispatch of one VM.
        // 保留首次清理标签，以拒绝同 VM 的重复分发。
        let first_label = first.arguments.clone();
        acknowledge(&runtime, first);
        // The exact closing request retains its original VM identity.
        // 精确关闭请求保留原始 VM 身份。
        let second = host_request(&runtime);
        assert_ne!(first_label, second.arguments);
        assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 1);
        runtime.request_close().unwrap();
        assert!(!runtime.poll_closed().unwrap());
        acknowledge(&runtime, second);
        shutdown(&runtime);
        assert_eq!(runtime.capacity(&capacity).unwrap().resources.resident, 0);
        assert_eq!(
            runtime.capacity(&capacity).unwrap().committed_resident_vms,
            1
        );
        assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 0);
        for session in sessions {
            runtime.forget_session(&session).unwrap();
        }
        for pool in pools {
            runtime.forget_pool(&pool).unwrap();
        }
        runtime.forget_capacity(&capacity).unwrap();
        assert_eq!(
            runtime.capacity(&capacity).unwrap_err().code,
            EmbeddedErrorCode::NotFound
        );
    }
}

/// A blocked real Lua destructor retains aggregate execution and physical residency until it returns.
/// 阻塞的真实 Lua 析构在返回前保留聚合执行及物理常驻计费。
#[test]
fn embedded_capacity_physical_retirement_keeps_execution_charge() {
    // The release guard unblocks native finalization even if a later assertion fails.
    // 释放守卫即使遇到后续断言失败，也会解除原生关闭阻塞。
    let layout = SystemRuntimeTestLayout::new("formal capacity retirement");
    // Real formal runtime retains plugin and physical ownership through cleanup.
    // 真实正式运行时跨清理保留插件及物理归属。
    let runtime = runtime(&layout, pool_config());
    // Scope cleanup releases the real destructor even on assertion unwinding.
    // 作用域清理即使在断言展开时也释放真实析构。
    let release = FinalizerRelease(layout.package_root.join("capacity-retirement-release"));
    // Capacity policy is frozen before any member or operation is admitted.
    // 在接纳任何成员或操作前冻结容量策略。
    let config = capacity_policy(PoolKind::Shared, 0, 2);
    // Exact retained capacity ownership is never resolved through a fallback.
    // 精确保留容量归属绝不通过回退解析。
    let capacity = runtime
        .register_capacity(&layout.package_id, config.clone())
        .unwrap();
    // Real Lua code makes module state and lifetime boundaries observable.
    // 真实 Lua 代码使模块状态及寿命边界可观察。
    let source = r#"
        -- Preserve controlled filesystem access and a finite destructor observation deadline.
        -- 保留受控文件访问及有限析构观测截止时间。
        local open, clock = io.open, os.clock
        -- Retain the finalizable object until actual physical VM destruction.
        -- 保留可关闭对象，直到真实物理 VM 销毁。
        local proxy = newproxy(true)
        -- Expose actual destructor entry and await external release.
        -- 暴露真实析构入口并等待外部释放。
        getmetatable(proxy).__gc=function()
            -- A file marker proves that physical destruction has started.
            -- 文件标记证明物理销毁已开始。
            local entered=assert(open('capacity-retirement-entered','w')); entered:close()
            -- Bound the fixture even if external release never arrives.
            -- 即使外部释放未到达，也约束夹具运行时间。
            local deadline=clock()+5
            repeat
                -- Missing release files are expected until the test acknowledges destruction.
                -- 测试确认销毁前，释放文件缺失属于预期状态。
                local ok, release=pcall(open,'capacity-retirement-release','r')
                if ok and release then release:close(); break end
            until clock() >= deadline
        end
        -- Returning business success must not release physical ownership early.
        -- 返回业务成功不得提前释放物理归属。
        return {call=function() return proxy ~= nil end}
    "#;
    // Retain this exact module identity to verify cross-member isolation and ordering.
    // 保留此精确模块身份以验证跨成员隔离及顺序。
    let first = runtime
        .register_pool_in_capacity(
            &capacity,
            definition(&layout, source),
            member_policy(&config, InstanceReuse::SingleCall),
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
            member_policy(&config, InstanceReuse::SingleCall),
            permissions(),
            "second".into(),
        )
        .unwrap();
    // The dispatched operation keeps its original identity through final cleanup.
    // 已分发操作跨最终清理保留原始身份。
    let running = runtime
        .submit(call(&first, Value::Null), Duration::from_secs(5))
        .unwrap();
    // Physical retirement and public cleanup are separate transitions; observe both within the original budget.
    // 物理退役与公开清理是独立转换；在原预算内同时观测二者。
    let deadline = Instant::now() + Duration::from_secs(3);
    while !layout
        .package_root
        .join("capacity-retirement-entered")
        .exists()
        || running.snapshot().unwrap().phase != OperationPhase::Cleaning
    {
        assert!(
            Instant::now() < deadline,
            "real destructor and scheduler cleanup must both be observed"
        );
        std::thread::yield_now();
    }
    // Retain the waiting operation to observe cancellation and precise capacity recovery.
    // 保留等待操作，以观测取消及精确容量恢复。
    let queued = runtime
        .submit(call(&second, Value::Null), Duration::from_secs(5))
        .unwrap();
    assert_eq!(runtime.capacity(&capacity).unwrap().resources.retiring, 1);
    assert_eq!(runtime.capacity(&capacity).unwrap().active_operations, 1);
    assert_eq!(running.snapshot().unwrap().phase, OperationPhase::Cleaning);
    // Another worker has time to attempt dispatch; the original capacity charge must still prevent it.
    // 另一工作线程有时间尝试分发；原容量计费必须继续阻止它。
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(queued.snapshot().unwrap().phase, OperationPhase::Queued);
    runtime.close_capacity(&capacity).unwrap();
    assert_eq!(
        runtime.forget_capacity(&capacity).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    drop(release);
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        queued
            .wait(Duration::from_secs(3))
            .unwrap()
            .error
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    shutdown(&runtime);
}
