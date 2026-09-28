//! Same-VM, at-most-once closing calls and their actual resource accounting.
//! 同 VM、至多一次的关闭调用及其实际资源记账。

use super::*;
use crate::runtime::embedded::capabilities::{
    CapabilityExecution, CapabilityOutcome, CapabilityRegistrationRequest, CapabilityRegistry,
};
use crate::runtime::embedded::{
    EffectState, EmbeddedResult, InstanceReuse, ModuleLease, ModulePool, ModuleRelease,
};
use capabilities::{binding, descriptor};
use pools::{drained, pool_manager, pool_policy};

/// Declare the fixture business and closing exports for the exact source and package.
/// 为精确源码和包声明夹具的业务与关闭导出。
/// Return a validated-shape definition; runtime construction still performs actual validation.
/// 返回具有合法形状的定义；运行时构造仍执行实际校验。
fn closing_definition(layout: &SystemRuntimeTestLayout, source: &str) -> ModuleDefinition {
    // Keep both exports under the same immutable module declaration.
    // 将两个导出保留在同一个不可变模块声明下。
    let mut declared = definition(layout, source);
    declared.exports.push(ModuleExport {
        name: "shutdown".into(),
        input_schema: json!(true),
        output_schema: json!(true),
    });
    declared
}

/// Invoke the fixture business export with the supplied control and return its original result.
/// 使用提供的控制调用夹具业务导出，并返回原始结果。
fn business(lease: &mut ModuleLease, budget: Arc<CallControl>) -> EmbeddedResult<Value> {
    lease.invoke(ModuleInvocation {
        operation_id: "business-operation",
        session_id: None,
        export: "call",
        arguments: &Value::Null,
        context: &LuaInvocationContext::default(),
        control: budget,
    })
}

/// Attempt the fixture closing export with its separate control and return only its result.
/// 使用独立控制尝试夹具关闭导出，并只返回其结果。
fn finalize(lease: &mut ModuleLease, budget: Arc<CallControl>) -> EmbeddedResult<Value> {
    lease.finalize(ModuleInvocation {
        operation_id: "closing-operation",
        session_id: None,
        export: "shutdown",
        arguments: &Value::Null,
        context: &LuaInvocationContext::default(),
        control: budget,
    })
}

/// Prove that a finalized lease retires instead of returning a reusable VM to the pool.
/// 证明已关闭的租借会退役，而非将可复用 VM 归还池中。
fn retire(lease: ModuleLease, pool: &ModulePool) {
    assert!(matches!(
        lease.finish().unwrap(),
        ModuleRelease::Retiring(_)
    ));
    drained(pool);
}

/// Single-call and exhausted reusable policies still close the exact business VM once.
/// 单次调用与耗尽的复用策略仍会在精确业务 VM 中执行一次关闭。
#[test]
fn embedded_finalization_preserves_state_and_bypasses_only_business_use_limits() {
    for reuse in [InstanceReuse::SingleCall, InstanceReuse::Reusable] {
        // Each policy owns an independent real package and pool.
        // 每种策略拥有独立真实包与池。
        let layout = SystemRuntimeTestLayout::new("embedded closing state");
        let manager = pool_manager(&layout);
        let mut policy = pool_policy(reuse);
        policy.max_uses = Some(1);
        let pool = manager
            .create_pool(
                "closing-state".into(),
                closing_definition(
                    &layout,
                    r#"
                    local calls = 0
                    return {
                        call = function() calls = calls + 1; return calls end,
                        shutdown = function()
                            calls = calls + 1
                            vulcan.fs.write('closing-count.txt', tostring(calls))
                            return calls
                        end
                    }
                "#,
                ),
                policy,
            )
            .unwrap();
        let mut lease = pool.acquire(control()).unwrap();
        let instance = lease.instance_id().unwrap().to_owned();
        let outcome = business(&mut lease, control()).unwrap();
        assert_eq!(
            business(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        assert_eq!(finalize(&mut lease, control()).unwrap(), json!(2));
        assert_eq!(lease.instance_id().unwrap(), instance);
        assert_eq!(outcome, json!(1));
        assert_eq!(
            finalize(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        assert_eq!(
            business(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        retire(lease, &pool);
        assert_eq!(
            std::fs::read_to_string(layout.package_root.join("closing-count.txt")).unwrap(),
            "2"
        );
    }
}

/// Failed business calls retain Lua state while closing uses a fresh finite budget.
/// 业务调用失败时保留 Lua 状态，关闭使用新的有限预算。
#[test]
fn embedded_finalization_survives_business_error_and_deadline() {
    for (body, expected) in [
        (
            "error('business failed')",
            EmbeddedErrorCode::ExecutionFailed,
        ),
        ("while true do end", EmbeddedErrorCode::DeadlineExceeded),
    ] {
        // The timeout case loops in real Lua rather than relying on a mocked clock.
        // 超时分支在真实 Lua 中循环，不依赖模拟时钟。
        let layout = SystemRuntimeTestLayout::new("embedded closing failed business");
        let manager = pool_manager(&layout);
        let source = format!(
            "local calls=0; return {{call=function() calls=calls+1; {body} end, shutdown=function() return calls end}}"
        );
        let pool = manager
            .create_pool(
                "closing-failure".into(),
                closing_definition(&layout, &source),
                pool_policy(InstanceReuse::SingleCall),
            )
            .unwrap();
        let mut lease = pool.acquire(control()).unwrap();
        let budget = Arc::new(CallControl::new(Duration::from_millis(100)).unwrap());
        let outcome = business(&mut lease, budget).unwrap_err();
        assert_eq!(outcome.code, expected);
        assert_eq!(finalize(&mut lease, control()).unwrap(), json!(1));
        assert_eq!(outcome.code, expected);
        retire(lease, &pool);
    }
}

/// Cancellation after a real host callback must not cancel the independent closing call.
/// 真实宿主回调后的取消不得取消独立关闭调用。
#[test]
fn embedded_finalization_survives_business_cancellation() {
    // The handler cancels the exact business control only after Lua state changed.
    // 处理器仅在 Lua 状态变化后取消精确业务控制。
    let layout = SystemRuntimeTestLayout::new("embedded closing cancellation");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("closing-cancel".into(), manager.config().clone()).unwrap();
    // The low-level caller owns the exact business control, independently from native capability budgets.
    // 底层调用方拥有精确业务控制，与原生能力预算相互独立。
    let budget = control();
    let cancelled_by_host = Arc::clone(&budget);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.cancel", CapabilityExecution::Native),
            native: Some(Arc::new(move |_| {
                cancelled_by_host.cancel();
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    let (_, capabilities) = binding(&registry);
    let pool = manager.create_pool_with_capabilities(
        "closing-cancel".into(),
        closing_definition(&layout, "local calls=0; return {call=function() calls=calls+1; vulcan.host.call('test.cancel',{}); return calls end, shutdown=function() return calls end}"),
        pool_policy(InstanceReuse::SingleCall), capabilities,
    ).unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    let outcome = business(&mut lease, budget).unwrap_err();
    assert_eq!(outcome.code, EmbeddedErrorCode::Cancelled);
    assert_eq!(finalize(&mut lease, control()).unwrap(), json!(1));
    assert_eq!(outcome.code, EmbeddedErrorCode::Cancelled);
    retire(lease, &pool);
}

/// Closing errors and deadlines are terminal and never replay through retirement.
/// 关闭错误与超时均为终态，绝不经退役重放。
#[test]
fn embedded_finalization_error_and_deadline_do_not_replay() {
    for (body, expected) in [
        (
            "error('closing failed')",
            EmbeddedErrorCode::ExecutionFailed,
        ),
        ("while true do end", EmbeddedErrorCode::DeadlineExceeded),
    ] {
        // A file records actual attempts even when Lua returns no value.
        // 即使 Lua 未返回值，文件仍记录实际尝试次数。
        let layout = SystemRuntimeTestLayout::new("embedded closing attempt");
        let manager = pool_manager(&layout);
        let source = format!(
            "local attempts=0; return {{call=function() return 'business' end, shutdown=function() attempts=attempts+1; vulcan.fs.write('attempts.txt',tostring(attempts)); {body} end}}"
        );
        let pool = manager
            .create_pool(
                "closing-once".into(),
                closing_definition(&layout, &source),
                pool_policy(InstanceReuse::Reusable),
            )
            .unwrap();
        let mut lease = pool.acquire(control()).unwrap();
        let outcome = business(&mut lease, control()).unwrap();
        let budget = Arc::new(CallControl::new(Duration::from_millis(100)).unwrap());
        assert_eq!(finalize(&mut lease, budget).unwrap_err().code, expected);
        assert_eq!(
            finalize(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        assert_eq!(
            business(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        assert_eq!(outcome, json!("business"));
        retire(lease, &pool);
        assert_eq!(
            std::fs::read_to_string(layout.package_root.join("attempts.txt")).unwrap(),
            "1"
        );
    }
}

/// A cancelled closing attempt is consumed even when no user function was entered.
/// 已取消的关闭尝试即使未进入用户函数也会被消费。
#[test]
fn embedded_finalization_cancelled_attempt_is_terminal() {
    // The write must remain absent after rejection, explicit retry and VM retirement.
    // 拒绝、显式重试和 VM 退役后该写入都必须不存在。
    let layout = SystemRuntimeTestLayout::new("embedded closing cancelled attempt");
    let manager = pool_manager(&layout);
    let pool = manager.create_pool("closing-cancelled".into(), closing_definition(&layout,
        "return {call=function() return true end, shutdown=function() vulcan.fs.write('unexpected.txt','bad'); return true end}"),
        pool_policy(InstanceReuse::Reusable)).unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    let cancelled = control();
    cancelled.cancel();
    assert_eq!(
        finalize(&mut lease, cancelled).unwrap_err().code,
        EmbeddedErrorCode::Cancelled
    );
    assert_eq!(
        finalize(&mut lease, control()).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    retire(lease, &pool);
    assert!(!layout.package_root.join("unexpected.txt").exists());
}

/// Export resolution and schema rejection consume a closing attempt without executing its body.
/// 导出解析与 Schema 拒绝会消费关闭尝试，但不执行函数体。
#[test]
fn embedded_finalization_validation_failure_is_terminal() {
    for (export, expected) in [
        ("missing", EmbeddedErrorCode::NotFound),
        ("shutdown", EmbeddedErrorCode::InvalidArgument),
    ] {
        // The fixture closing input requires an object while the attempted value is null.
        // 夹具关闭入参要求对象，但尝试传入的值为空值。
        let layout = SystemRuntimeTestLayout::new("embedded closing invalid input");
        let manager = pool_manager(&layout);
        let mut declared = closing_definition(
            &layout,
            "return {call=function() return true end, shutdown=function() vulcan.fs.write('unexpected.txt','bad'); return true end}",
        );
        declared
            .exports
            .iter_mut()
            .find(|entry| entry.name == "shutdown")
            .unwrap()
            .input_schema = json!({"type":"object"});
        let pool = manager
            .create_pool(
                "closing-validation".into(),
                declared,
                pool_policy(InstanceReuse::Reusable),
            )
            .unwrap();
        let mut lease = pool.acquire(control()).unwrap();
        let outcome = lease
            .finalize(ModuleInvocation {
                operation_id: "closing-invalid",
                session_id: None,
                export,
                arguments: &Value::Null,
                context: &LuaInvocationContext::default(),
                control: control(),
            })
            .unwrap_err();
        assert_eq!(outcome.code, expected);
        assert_eq!(
            finalize(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        assert_eq!(
            business(&mut lease, control()).unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        retire(lease, &pool);
        assert!(!layout.package_root.join("unexpected.txt").exists());
    }
}

/// Failed initialization cannot be replayed or treated as an initialized closing target.
/// 初始化失败后不能重放，也不能被视为已初始化的关闭目标。
#[test]
fn embedded_finalization_rejects_partial_initialization() {
    // Initialization writes once before failing; no initialized export table is captured.
    // 初始化在失败前写入一次；没有捕获已初始化导出表。
    let layout = SystemRuntimeTestLayout::new("embedded closing partial init");
    let manager = pool_manager(&layout);
    let pool = manager.create_pool("closing-init".into(), closing_definition(&layout,
        "local n=assert(tonumber(vulcan.fs.read('init-count.txt'))); vulcan.fs.write('init-count.txt',tostring(n+1)); error('init failed')"),
        pool_policy(InstanceReuse::Reusable)).unwrap();
    std::fs::write(layout.package_root.join("init-count.txt"), "0").unwrap();
    let mut lease = pool.prepare(&control()).unwrap();
    assert_eq!(
        lease.initialize(control()).unwrap_err().code,
        EmbeddedErrorCode::ExecutionFailed
    );
    assert_eq!(
        finalize(&mut lease, control()).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(
        lease.initialize(control()).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    retire(lease, &pool);
    assert_eq!(
        std::fs::read_to_string(layout.package_root.join("init-count.txt")).unwrap(),
        "1"
    );
}

/// Closing a generation does not prevent its borrowed VM from finalizing with live permissions.
/// 关闭代次不会阻止借出 VM 使用实时权限完成关闭。
#[test]
fn embedded_finalization_after_pool_close_keeps_revocation() {
    // Count native dispatches to prove closing cannot restore revoked authority.
    // 统计原生分发次数，证明关闭不能恢复已撤销权威。
    let layout = SystemRuntimeTestLayout::new("embedded closing revoked");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("closing-revoked".into(), manager.config().clone()).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.revoke", CapabilityExecution::Native),
            native: Some(Arc::new(move |_| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                CapabilityOutcome {
                    result: Ok(json!(true)),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    let (permissions, capabilities) = binding(&registry);
    let pool = manager.create_pool_with_capabilities("closing-revoked".into(), closing_definition(&layout,
        "return {call=function() return vulcan.host.call('test.revoke',{}) end, shutdown=function() return vulcan.host.call('test.revoke',{}) end}"),
        pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    assert_eq!(business(&mut lease, control()).unwrap()["ok"], json!(true));
    pool.close().unwrap();
    permissions.revoke("test.host").unwrap();
    assert_eq!(
        business(&mut lease, control()).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(
        finalize(&mut lease, control()).unwrap()["error"]["code"],
        json!("permission_denied")
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    retire(lease, &pool);
}

/// Panicking native closing handlers execute once and cannot revive the module.
/// 发生 panic 的原生关闭处理器只执行一次，不能复活模块。
#[test]
fn embedded_finalization_native_panic_is_not_replayed() {
    // Native panic containment returns a Lua-visible failure without exposing the panic text.
    // 原生 panic 隔离返回 Lua 可见失败，不暴露 panic 文本。
    let layout = SystemRuntimeTestLayout::new("embedded closing panic");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("closing-panic".into(), manager.config().clone()).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.panic", CapabilityExecution::Native),
            native: Some(Arc::new(move |_| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                panic!("private closing panic")
            })),
        }])
        .unwrap();
    let (_, capabilities) = binding(&registry);
    let pool = manager.create_pool_with_capabilities("closing-panic".into(), closing_definition(&layout,
        "return {call=function() return true end, shutdown=function() local result=vulcan.host.call('test.panic',{}); assert(result.ok,result.error.message); return true end}"),
        pool_policy(InstanceReuse::Reusable), capabilities).unwrap();
    let mut lease = pool.acquire(control()).unwrap();
    assert_eq!(
        finalize(&mut lease, control()).unwrap_err().code,
        EmbeddedErrorCode::ExecutionFailed
    );
    assert_eq!(
        finalize(&mut lease, control()).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    retire(lease, &pool);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// A blocked closing handler retains quotas after cancellation; capacity rejection permits a later first attempt.
/// 阻塞关闭处理器在取消后仍保留配额；容量拒绝允许稍后进行首次尝试。
#[test]
fn embedded_finalization_retains_capacity_until_native_return() {
    // Channel barriers observe real execution without guessing scheduler timing.
    // 通道屏障观察真实执行，不猜测调度时序。
    let layout = SystemRuntimeTestLayout::new("embedded closing capacity");
    let manager = pool_manager(&layout);
    let registry =
        CapabilityRegistry::new("closing-capacity".into(), manager.config().clone()).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release = Mutex::new(release_rx);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor("test.wait", CapabilityExecution::Native),
            native: Some(Arc::new(move |request| {
                // Only the first actual call blocks; rejected attempts must not reach this counter.
                // 只有首次实际调用阻塞；被拒绝的尝试不得到达该计数器。
                let index = observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if index == 0 {
                    entered_tx.send(request.caller.clone()).unwrap();
                    release
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                CapabilityOutcome {
                    result: Ok(json!(index)),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    let (_, capabilities) = binding(&registry);
    let mut policy = pool_policy(InstanceReuse::Reusable);
    policy.max_running_calls = 1;
    let pool = manager.create_pool_with_capabilities("closing-capacity".into(), closing_definition(&layout,
        "return {call=function() return true end, shutdown=function() local result=vulcan.host.call('test.wait',{}); assert(result.ok); return result.value end}"),
        policy, capabilities).unwrap();
    // Both VMs exist before the first handler consumes this group's only execution permit.
    // 首个处理器消耗此分组唯一执行许可前，两个 VM 均已存在。
    let mut first = pool.acquire(control()).unwrap();
    let mut second = pool.acquire(control()).unwrap();
    let budget = control();
    let worker_budget = Arc::clone(&budget);
    let worker = std::thread::spawn(move || {
        // Return actual ownership together with the result, avoiding implicit release by thread exit.
        // 将真实所有权与结果一同返回，避免线程退出隐式释放。
        let result = finalize(&mut first, worker_budget);
        (first, result)
    });
    let caller = entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(caller.operation_id, "closing-operation");
    assert_eq!(caller.plugin_id, layout.package_id);
    assert_eq!(pool.usage().unwrap().running, 1);
    assert_eq!(
        finalize(&mut second, control()).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    budget.cancel();
    pool.close().unwrap();
    assert_eq!(pool.usage().unwrap().resident, 2);
    assert_eq!(pool.usage().unwrap().running, 1);
    // Another group can use the remaining global permit while the first group remains occupied.
    // 首个分组仍被占用时，另一个分组可以使用剩余全局许可。
    let other = manager
        .create_pool(
            "closing-independent".into(),
            definition(&layout, "return {call=function() return 'independent' end}"),
            pool_policy(InstanceReuse::SingleCall),
        )
        .unwrap();
    let mut other_lease = other.acquire(control()).unwrap();
    assert_eq!(
        business(&mut other_lease, control()).unwrap(),
        json!("independent")
    );
    retire(other_lease, &other);
    release_tx.send(()).unwrap();
    let (mut first, outcome) = worker.join().unwrap();
    assert_eq!(outcome.unwrap_err().code, EmbeddedErrorCode::Cancelled);
    assert_eq!(
        finalize(&mut first, control()).unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(finalize(&mut second, control()).unwrap(), json!(1));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(matches!(
        first.finish().unwrap(),
        ModuleRelease::Retiring(_)
    ));
    retire(second, &pool);
}
