//! Exercise scheduler-owned reusable state through expiry, use limits, and failed storage confirmation.
//! 验证调度器拥有的可复用状态跨过期、使用额度和失败存储确认的行为。

use super::*;

/// Idle pool closure retains real destructor ownership without blocking unrelated execution or control.
/// 空闲池关闭保留真实析构所有权，同时不阻塞无关执行或控制操作。
#[test]
fn embedded_scheduler_reusable_idle_close_waits_for_actual_retirement() {
    // A real Lua finalizer exposes the interval between requesting close and actual destruction.
    // 真实 Lua 析构器暴露请求关闭到实际销毁之间的区间。
    let layout = SystemRuntimeTestLayout::new("scheduler reusable idle retirement");
    let runtime = runtime(&layout, pool_config());
    let release = FinalizerRelease(layout.package_root.join("retirement-release"));
    let source = r#"
        -- Keep the resource proxy reachable until the owning VM is really destroyed.
        -- 保持资源代理可达，直到所属 VM 真实销毁。
        local open, clock = io.open, os.clock
        local proxy = newproxy(true)
        getmetatable(proxy).__gc = function()
            local entered=assert(open('retirement-entered','w')); entered:write('yes'); entered:close()
            -- The host releases this destructor after verifying capacity and independent execution.
            -- 宿主验证容量和独立执行后释放此析构器。
            local deadline=clock()+8
            repeat
                local ok, released=pcall(open,'retirement-release','r')
                if ok and released then released:close(); break end
            until clock() >= deadline
            local finished=assert(open('retirement-finished','w')); finished:write('yes'); finished:close()
        end
        return {call=function() return proxy ~= nil end}
    "#;
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
    assert_eq!(operation.wait(OBSERVE).unwrap().value, Some(json!(true)));
    runtime.forget_operation(operation.id()).unwrap();
    runtime.close_pool(&pool).unwrap();
    until(
        || layout.package_root.join("retirement-entered").exists(),
        "real idle VM destructor never entered",
    );
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
    assert_eq!(
        runtime.forget_pool(&pool).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    // Another execution domain can progress while the retirement service owns the blocked VM.
    // 退役服务拥有被阻塞 VM 期间，另一执行域能够推进。
    let other = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 23 end}"),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    let next = runtime.submit(call(&other, Value::Null), OBSERVE).unwrap();
    assert_eq!(next.wait(OBSERVE).unwrap().value, Some(json!(23)));
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    drop(release);
    until(
        || match runtime.forget_pool(&pool) {
            Ok(()) => true,
            Err(error) if error.code == EmbeddedErrorCode::Busy => false,
            Err(error) => panic!("unexpected pool forgetting failure: {error}"),
        },
        "real retirement never released the closed pool identity",
    );
    assert_eq!(
        fs::read_to_string(layout.package_root.join("retirement-finished")).unwrap(),
        "yes"
    );
    shutdown(&runtime);
}

/// Real concurrent VMs expire down to the dedicated minimum, then retire at their declared use limit.
/// 真实并发 VM 过期回收到专用最小数量，再在声明的使用额度耗尽时退役。
#[test]
fn embedded_scheduler_reusable_idle_minimum_and_use_limit() {
    // One runtime supplies both actual execution workers and the parent resident budget.
    // 一个运行时提供实际执行工作线程及父级常驻预算。
    let layout = SystemRuntimeTestLayout::new("scheduler reusable warm minimum");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::super::capabilities::descriptor(
                "test.cache",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    // Expiry retains one initialized VM; a use limit remains independent of that warm minimum.
    // 过期保留一个已初始化 VM；使用额度独立于该预热最小值。
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.kind = PoolKind::Dedicated;
    policy.min_resident_vms = 1;
    policy.idle_ttl_ms = Some(1);
    policy.max_uses = Some(2);
    // Local state distinguishes reuse from a fresh allocation without relying on internal metadata.
    // 局部状态区分复用和新分配，不依赖内部元数据。
    let source = r#"
        -- The counter belongs to one real VM and remains reachable through its export.
        -- 计数器属于一个真实 VM，并通过其导出保持可达。
        local count = 0
        return {call=function(wait)
            count=count+1
            if wait then
                -- Host acknowledgement keeps both calls in flight until two VMs exist.
                -- 宿主确认在两个 VM 均存在前保持两个调用执行中。
                local reply=vulcan.host.call('test.cache', count)
                assert(reply.ok)
            end
            return count
        end}
    "#;
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            policy,
            permissions(),
            "r1".into(),
        )
        .unwrap();
    // Dispatch and observe both real host requests before acknowledging either one.
    // 在确认任何请求前，分发并观察两个真实宿主请求。
    let first = runtime.submit(call(&pool, json!(true)), OBSERVE).unwrap();
    let first_request = host_request(&runtime);
    let second = runtime.submit(call(&pool, json!(true)), OBSERVE).unwrap();
    let second_request = host_request(&runtime);
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 2);
    for request in [first_request, second_request] {
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
    for operation in [first, second] {
        assert_eq!(operation.wait(OBSERVE).unwrap().value, Some(json!(1)));
    }
    until(
        || runtime.pool_resources(&pool).unwrap().resident == 1,
        "idle expiry did not retain exactly the dedicated minimum",
    );
    // The protected warm VM must survive further scans and retain its first call's state.
    // 受保护的预热 VM 必须跨后续扫描存活，并保留首次调用状态。
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
    let reused = runtime.submit(call(&pool, json!(false)), OBSERVE).unwrap();
    assert_eq!(reused.wait(OBSERVE).unwrap().value, Some(json!(2)));
    assert_eq!(
        runtime.pool_resources(&pool).unwrap().resident,
        0,
        "exhausted state must retire despite the idle minimum"
    );
    let replacement = runtime.submit(call(&pool, json!(false)), OBSERVE).unwrap();
    assert_eq!(replacement.wait(OBSERVE).unwrap().value, Some(json!(1)));
    shutdown(&runtime);
}

/// Failed durable acknowledgement keeps the exact VM out of idle expiry until business evidence is confirmed.
/// 持久确认失败时，精确 VM 在业务证据确认前不能进入空闲过期回收。
#[test]
fn embedded_scheduler_reusable_failed_checkpoint_retains_vm_past_idle_ttl() {
    for committed in [false, true] {
        // Exercise both transaction rollback and lost acknowledgement after actual commit.
        // 同时验证事务回滚和实际提交后的确认丢失。
        let layout = SystemRuntimeTestLayout::new("scheduler reusable durable ownership");
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(16, 1024 * 1024));
        // Release the Lua gate during assertion unwinding as well as the successful test path.
        // 断言展开和测试成功路径均释放 Lua 门禁。
        let release = FinalizerRelease(layout.package_root.join("business-release"));
        let source = r#"
            -- Host files coordinate real execution before the next disk checkpoint.
            -- 宿主文件在下一磁盘检查点前协调真实执行。
            local open, clock = io.open, os.clock
            return {call=function()
                local count=assert(open('business-count','a')); count:write('x'); count:close()
                local entered=assert(open('business-entered','w')); entered:write('yes'); entered:close()
                -- The explicit release controls progress; timeout only bounds fixture failure.
                -- 显式释放控制推进；超时仅约束夹具失败。
                local deadline=clock()+8
                repeat
                    local ok, released=pcall(open,'business-release','r')
                    if ok and released then released:close(); return 1 end
                until clock() >= deadline
                error('fixture gate expired')
            end}
        "#;
        let mut policy = pool_policy(InstanceReuse::Reusable);
        policy.idle_ttl_ms = Some(1);
        let pool = runtime
            .register_pool(
                definition(&layout, source),
                policy,
                permissions(),
                "r1".into(),
            )
            .unwrap();
        let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        until(
            || layout.package_root.join("business-entered").exists(),
            "business never reached the real gate",
        );
        journal.lose_next_confirmation_for_test(committed);
        drop(release);
        assert_eq!(
            failure(&runtime, &operation).phase,
            OperationPhase::Cleaning
        );
        // Multiple idle TTLs pass while the retained checkpoint has no confirmed successor.
        // 在保留检查点没有已确认后继状态期间，经过多个空闲期限。
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(
            runtime.pool_resources(&pool).unwrap().resident,
            1,
            "unconfirmed business ownership cannot become evictable idle state"
        );
        assert!(!operation.snapshot().unwrap().phase.is_terminal());
        assert!(journal.recover_storage().unwrap());
        assert!(runtime.retry_checkpoint(operation.id()).unwrap());
        assert_eq!(operation.wait(OBSERVE).unwrap().value, Some(json!(1)));
        until(
            || runtime.pool_resources(&pool).unwrap().resident == 0,
            "confirmed idle VM never expired",
        );
        assert_eq!(
            fs::read_to_string(layout.package_root.join("business-count")).unwrap(),
            "x"
        );
        shutdown_durable(&runtime, &writer);
    }
}
