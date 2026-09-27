use super::*;
use crate::runtime::embedded::{ModuleRelease, ModuleRetirementPhase};

/// VM destruction completion is distinct from business success inside a global Lua finalizer.
/// VM 销毁完成与全局 Lua 终结器中的业务成功不同。
#[test]
fn embedded_pool_global_finalizer_failure_does_not_prove_worker_failure() {
    // A global root survives Lua's last live GC and reaches native state-close error handling.
    // 全局根在 Lua 最后一次存活垃圾回收后仍存活，进入原生状态关闭的错误处理。
    let layout = SystemRuntimeTestLayout::new("embedded global finalizer failure");
    let manager = pool_manager(&layout);
    let bad = manager.create_pool("failed-finalizer".into(), definition(&layout,
        "retained_finalizer=newproxy(true); getmetatable(retained_finalizer).__gc=function() io.open('late-native-open','w') end; return {call=function() return true end}"),
        pool_policy(InstanceReuse::SingleCall)).unwrap();
    let good = manager
        .create_pool(
            "healthy-finalizer".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::SingleCall),
        )
        .unwrap();
    // Both actual VMs use the same retirement thread.
    // 两个真实 VM 使用同一个退役线程。
    let bad_lease = bad.acquire_tracked(control()).unwrap();
    let good_lease = good.acquire_tracked(control()).unwrap();
    let ModuleRelease::Retiring(bad_receipt) = bad_lease.finish().unwrap() else {
        panic!("single-call instance must retire");
    };
    assert_eq!(
        bad_receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    assert!(
        !layout.package_root.join("late-native-open").exists(),
        "VM destruction cannot be presented as successful finalizer business execution"
    );
    // A panic hook log inside Lua finalization is not evidence that the Rust worker unwound.
    // Lua 终结中的 panic 钩子日志不能作为 Rust 工作线程栈展开的证据。
    let ModuleRelease::Retiring(good_receipt) = good_lease.finish().unwrap() else {
        panic!("healthy single-call instance must retire");
    };
    assert_eq!(
        good_receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    assert_eq!(manager.usage().unwrap().resident, 0);
    manager.request_close().unwrap();
    // Actual worker exit and join remain the final shutdown authority.
    // 实际工作线程退出与等待结束仍是最终关闭权威。
    let deadline = Instant::now() + Duration::from_secs(3);
    while !manager.poll_closed().unwrap() {
        assert!(
            Instant::now() < deadline,
            "retirement worker must exit after real destruction"
        );
        std::thread::yield_now();
    }
}

/// Always release the native finalizer probe, including when a test assertion unwinds.
/// 始终释放原生终结器探针，包含测试断言栈展开的情况。
struct FinalizerRelease(std::path::PathBuf);

impl Drop for FinalizerRelease {
    /// Publish the release marker without panicking during another failure.
    /// 发布释放标记，且不在其他失败期间触发 panic。
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

/// Completion must follow a blocked real Lua finalizer, not merely successful managed-resource close.
/// 完成必须晚于阻塞的真实 Lua 终结器，而不只是受管资源关闭成功。
#[test]
fn embedded_pool_retirement_receipt_waits_for_actual_lua_finalizer() {
    // Files provide an observable cross-thread barrier from the actual Lua VM destructor.
    // 文件提供来自真实 Lua VM 析构器的可观察跨线程屏障。
    let layout = SystemRuntimeTestLayout::new("embedded finalizer receipt");
    let manager = pool_manager(&layout);
    let release = FinalizerRelease(layout.package_root.join("release-finalizer"));
    // Captured IO and clock functions remain available while Lua closes its state.
    // Lua 关闭自身状态时，捕获的 IO 与时钟函数仍然可用。
    let source = r#"
        local open, clock = io.open, os.clock
        local finalizer = newproxy(true)
        getmetatable(finalizer).__gc = function()
            local entered = assert(open('entered-finalizer', 'w'))
            entered:write('entered'); entered:close()
            local deadline = clock() + 5
            repeat
                local ok, release = pcall(open, 'release-finalizer', 'r')
                if ok and release then release:close(); break end
            until clock() >= deadline
            local finished = assert(open('finished-finalizer', 'w'))
            finished:write('finished'); finished:close()
        end
        return {call=function() return finalizer ~= nil end}
    "#;
    let pool = manager
        .create_pool(
            "finalizer".into(),
            definition(&layout, source),
            pool_policy(InstanceReuse::SingleCall),
        )
        .unwrap();
    let lease = pool.acquire_tracked(control()).unwrap();
    let ModuleRelease::Retiring(receipt) = lease.finish().unwrap() else {
        panic!("single-call ownership must retire");
    };
    // Wait for the destructor itself, not a scheduler delay or aggregate capacity guess.
    // 等待析构器自身，而不依赖调度延迟或聚合容量猜测。
    let deadline = Instant::now() + Duration::from_secs(3);
    while !layout.package_root.join("entered-finalizer").exists() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        layout.package_root.join("entered-finalizer").exists(),
        "actual Lua finalizer must enter"
    );
    assert!(!layout.package_root.join("finished-finalizer").exists());
    assert_eq!(
        receipt.wait(Duration::ZERO).unwrap().phase,
        ModuleRetirementPhase::Running
    );
    assert_eq!(manager.usage().unwrap().resident, 1);
    manager.request_close().unwrap();
    assert!(!manager.poll_closed().unwrap());
    drop(release);
    assert_eq!(
        receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    assert_eq!(
        std::fs::read(layout.package_root.join("finished-finalizer")).unwrap(),
        b"finished"
    );
    assert_eq!(manager.usage().unwrap().resident, 0);
}

/// A release receipt observes only its instance, even while another instance keeps the pool occupied.
/// 即使另一个实例保持池占用，释放回执也只观察自身实例。
#[test]
fn embedded_pool_release_receipt_is_independent_of_other_residents() {
    // Both real VMs share a pool but have distinct ownership and completion.
    // 两个真实 VM 共享池，但具有不同所有权与完成状态。
    let layout = SystemRuntimeTestLayout::new("embedded exact cleanup receipt");
    let manager = pool_manager(&layout);
    let pool = manager
        .create_pool(
            "receipt".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::SingleCall),
        )
        .unwrap();
    let mut first = pool.acquire_tracked(control()).unwrap();
    let second = pool.acquire_tracked(control()).unwrap();
    let receipt = first.retirement_handle().unwrap();
    let second_receipt = second.retirement_handle().unwrap();
    assert_ne!(
        receipt.snapshot().unwrap().instance_id,
        second_receipt.snapshot().unwrap().instance_id
    );
    assert_eq!(
        receipt.wait(Duration::ZERO).unwrap().phase,
        ModuleRetirementPhase::Live
    );
    assert_eq!(count(&mut first).unwrap(), json!(true));
    // Repeated close keeps the same evidence after exclusive resident ownership moves away.
    // 独占常驻所有权转移后，重复关闭仍保留同一证据。
    first.close();
    first.close();
    let ModuleRelease::Retiring(finished) = first.finish().unwrap() else {
        panic!("a closed single-call VM must never report return to idle");
    };
    assert_eq!(
        finished.snapshot().unwrap().instance_id,
        receipt.snapshot().unwrap().instance_id
    );
    assert_eq!(
        receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    assert_eq!(manager.usage().unwrap().resident, 1);
    assert_eq!(
        second_receipt.snapshot().unwrap().phase,
        ModuleRetirementPhase::Live
    );
    // Completing one instance does not close admission or alter the other borrowed VM.
    // 完成一个实例不会关闭入场，也不会改变另一个已借用 VM。
    drop(second);
    assert_eq!(
        second_receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    pool.close().unwrap();
    manager.request_close().unwrap();
}

/// Returning reusable ownership is complete for the request without pretending the VM was destroyed.
/// 归还可复用所有权代表请求完成，但不假装 VM 已销毁。
#[test]
fn embedded_pool_finish_distinguishes_reuse_from_retirement() {
    // A persistent counter makes actual reuse and exact-instance lifetime observable.
    // 持久计数器使真实复用与精确实例生命周期可被观察。
    let layout = SystemRuntimeTestLayout::new("embedded release disposition");
    let manager = pool_manager(&layout);
    let pool = manager
        .create_pool(
            "reuse-receipt".into(),
            definition(
                &layout,
                "local n=0; return {call=function() n=n+1; return n end}",
            ),
            pool_policy(InstanceReuse::Reusable),
        )
        .unwrap();
    let mut lease = pool.acquire_tracked(control()).unwrap();
    let receipt = lease.retirement_handle().unwrap();
    assert_eq!(count(&mut lease).unwrap(), json!(1));
    assert!(matches!(
        lease.finish().unwrap(),
        ModuleRelease::ReturnedToPool
    ));
    assert_eq!(
        receipt.snapshot().unwrap().phase,
        ModuleRetirementPhase::Live
    );
    let mut reused = pool.acquire_tracked(control()).unwrap();
    assert_eq!(
        reused.instance_id().unwrap(),
        receipt.snapshot().unwrap().instance_id
    );
    assert_eq!(count(&mut reused).unwrap(), json!(2));
    pool.close().unwrap();
    assert!(matches!(
        reused.finish().unwrap(),
        ModuleRelease::Retiring(_)
    ));
    assert_eq!(
        receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    assert_eq!(manager.usage().unwrap().resident, 0);
}

/// Initialization failure returns real retirement evidence; rejection before allocation has none.
/// 初始化失败返回真实退役证据；分配前拒绝则没有证据。
#[test]
fn embedded_pool_failed_acquisition_preserves_cleanup_receipt() {
    // Source failure occurs after VM allocation, unlike policy rejection or closed admission.
    // 与策略拒绝或关闭入场不同，源码失败发生在 VM 分配后。
    let layout = SystemRuntimeTestLayout::new("embedded failed initialization receipt");
    let manager = pool_manager(&layout);
    let pool = manager
        .create_pool(
            "init-receipt".into(),
            definition(&layout, "error('fixture initialization failure')"),
            pool_policy(InstanceReuse::Reusable),
        )
        .unwrap();
    let failure = pool
        .acquire_tracked(control())
        .err()
        .expect("source must fail");
    assert_eq!(failure.error.code, EmbeddedErrorCode::ExecutionFailed);
    let receipt = failure
        .retirement
        .expect("allocated VM cleanup must remain queryable");
    assert_eq!(
        receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    pool.close().unwrap();
    let rejected = pool
        .acquire_tracked(control())
        .err()
        .expect("closed admission must fail");
    assert_eq!(rejected.error.code, EmbeddedErrorCode::Closed);
    assert!(rejected.retirement.is_none());
    assert_eq!(manager.usage().unwrap().resident, 0);
}

/// Pinned sessions release into retirement, never into ordinary mutable-state reuse.
/// 固定会话释放时进入退役，绝不进入普通可变状态复用。
#[test]
fn embedded_pool_session_release_retains_exact_cleanup_evidence() {
    // The same public completion contract applies to explicit session ownership.
    // 相同公开完成契约适用于显式会话所有权。
    let layout = SystemRuntimeTestLayout::new("embedded session receipt");
    let manager = pool_manager(&layout);
    let pool = manager
        .create_pool(
            "session-receipt".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::Session),
        )
        .unwrap();
    let wrong_entry = pool
        .acquire_tracked(control())
        .err()
        .expect("session policy must reject ordinary acquisition");
    assert!(wrong_entry.retirement.is_none());
    let session = pool.open_session_tracked(control()).unwrap();
    let ModuleRelease::Retiring(receipt) = session.finish().unwrap() else {
        panic!("session must retire on release");
    };
    assert_eq!(
        receipt.wait(Duration::from_secs(3)).unwrap().phase,
        ModuleRetirementPhase::Completed
    );
    assert_eq!(manager.usage().unwrap().resident, 0);
    pool.close().unwrap();
}
