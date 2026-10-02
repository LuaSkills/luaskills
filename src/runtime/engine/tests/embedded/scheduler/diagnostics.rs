//! Real diagnostic behavior uses the existing logger, actual reusable VMs and physical retirement.
//! 真实诊断行为使用既有日志器、实际复用 VM 及物理退役。
//! These process-global logger tests require the existing serial shared-resource harness.
//! 这些进程全局日志测试要求使用既有串行共享资源测试配置。

use super::*;
use crate::runtime::logging::{
    RuntimeLogCallback, diagnostic_subscriber, send_diagnostic, set_log_callback,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Restore the original process logger after actual runtime shutdown, including assertion unwinds.
/// 在实际运行时关闭后恢复原进程日志器，包括断言展开时。
pub(super) struct RestoreLogger(Option<RuntimeLogCallback>);

impl RestoreLogger {
    /// Capture the exact original callback before registering a test observer; return restoration ownership.
    /// 在注册测试观察者前捕获精确原回调；返回恢复所有权。
    pub(super) fn capture() -> Self {
        Self(diagnostic_subscriber().and_then(|subscriber| subscriber.upgrade()))
    }
}

impl Drop for RestoreLogger {
    /// Restore the original callback without affecting operation or instance ownership.
    /// 恢复原回调，不影响操作或实例所有权。
    fn drop(&mut self) {
        set_log_callback(self.0.take());
    }
}

/// Real reused execution records exact operation identities, one initialization, true host calls and live heap samples.
/// 真实复用执行记录精确操作身份、一次初始化、真实宿主调用及存活堆采样。
/// No arguments or return value; a real Lua finalizer witness proves retirement emission follows actual destruction.
/// 无参数或返回值；真实 Lua 终结器见证证明退役发送晚于实际销毁。
#[test]
fn embedded_diagnostics_observe_real_reuse_host_wait_heap_and_retirement() {
    // Keep global logger restoration outside all fixture lifetime guards.
    // 使全局日志恢复位于全部夹具生命周期保护之外。
    let _restore = RestoreLogger::capture();
    // Existing fixture supplies real immutable package and filesystem authority.
    // 既有夹具提供真实不可变包及文件系统权威。
    let layout = SystemRuntimeTestLayout::new("embedded optional phase diagnostics");
    // One actual formal runtime owns queueing, capabilities and physical cleanup.
    // 单个实际正式运行时拥有排队、能力及物理清理。
    let runtime = runtime(&layout, pool_config());
    // Count actual handler entries rather than inferring host work from a configured sleep.
    // 计数实际处理器进入，而非根据配置休眠推断宿主工作。
    let host_calls = Arc::new(AtomicUsize::new(0));
    // Give the original registered native handler its own counter reference.
    // 为原注册原生处理器提供独立计数引用。
    let native_calls = Arc::clone(&host_calls);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.diagnostic",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(move |_| CapabilityOutcome {
                result: Ok(json!(native_calls.fetch_add(1, Ordering::Relaxed) + 1)),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap();
    // Capture JSON diagnostics while ordinary public log events retain their original free-form messages.
    // 捕获 JSON 诊断，同时普通公开日志事件保留原自由格式消息。
    let observations = Arc::new(Mutex::new(Vec::<Value>::new()));
    // Callback ownership contains only test observations and witness metadata.
    // 回调所有权仅包含测试观测及见证元数据。
    let observed = Arc::clone(&observations);
    // Actual GC writes this witness before instance destruction can be reported.
    // 实际 GC 在可报告实例销毁之前写入此见证。
    let witness = layout.package_root.join("diagnostic-gc-finished");
    // Publish whether the file existed at the actual retirement callback boundary.
    // 发布实际退役回调边界时文件是否存在。
    let gc_observed = Arc::new(AtomicBool::new(false));
    // Callback receives its own witness observation owner.
    // 回调接收独立见证观测所有者。
    let gc_callback = Arc::clone(&gc_observed);
    // Wait for actual retirement emission separately from earlier physical completion notification.
    // 与更早物理完成通知分开等待实际退役发送。
    let (retired_sender, retired_receiver) = std::sync::mpsc::channel();
    set_log_callback(Some(Arc::new(move |event| {
        if let Ok(observation) = serde_json::from_str::<Value>(&event.message)
            && observation["luaskills_embedded_diagnostic"] == json!(1)
        {
            // Publish the captured event before waking the test waiting for its complete observation.
            // 在唤醒等待完整观测的测试前发布捕获事件。
            let retired = observation["phase"] == "retired";
            if retired {
                gc_callback.store(witness.is_file(), Ordering::Relaxed);
            }
            observed.lock().unwrap().push(observation);
            if retired {
                let _ = retired_sender.send(());
            }
        }
    })));
    // Capture a real GC root in the declared export so only actual VM destruction releases it.
    // 在声明导出中捕获真实 GC 根，使其仅由实际 VM 销毁释放。
    let source = r#"
        local open = io.open
        local proxy = newproxy(true)
        getmetatable(proxy).__gc = function()
            local file = assert(open('diagnostic-gc-finished', 'w'))
            file:write('finished'); file:close()
        end
        assert(vulcan.host.call('test.diagnostic', {}).ok)
        local count = 0
        return {call=function(argument)
            count = count + 1
            local host = vulcan.host.call('test.diagnostic', {})
            assert(host.ok)
            if argument == "fail" then error("expected diagnostic business failure") end
            return {count=count, host=host.value, live=proxy ~= nil}
        end}
    "#;
    // A single reusable pool preserves the physical instance across two different original operations.
    // 单个复用池跨两个不同原操作保留物理实例。
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "diagnostic-revision".into(),
        )
        .unwrap();
    // Record actual operation identities from admission instead of inferring them from event order.
    // 从入场记录实际操作身份，而非从事件顺序推断。
    let mut operation_ids = Vec::new();
    for expected in [1, 2] {
        // Original business state must increment while the native handler returns its actual invocation count.
        // 原业务状态必须递增，同时原生处理器返回实际调用次数。
        let operation = runtime
            .submit(call(&pool, Value::Null), Duration::from_secs(5))
            .unwrap();
        operation_ids.push(operation.id().to_owned());
        // Original terminal outcome must remain independent of diagnostic delivery.
        // 原终态结果必须独立于诊断投递。
        let result = operation.wait(Duration::from_secs(5)).unwrap();
        assert_eq!(result.phase, OperationPhase::Succeeded);
        assert_eq!(
            result.value,
            Some(json!({"count":expected,"host":expected+1,"live":true}))
        );
    }
    assert_eq!(host_calls.load(Ordering::Relaxed), 3);
    assert_eq!(runtime.resources().unwrap().resident, 1);
    // A real failed reused invocation must retain its entered host and request-cleanup intervals, then retire the VM.
    // 真实失败复用调用必须保留已进入宿主及请求清理区间，然后退役 VM。
    let failed = runtime
        .submit(call(&pool, json!("fail")), Duration::from_secs(5))
        .unwrap();
    operation_ids.push(failed.id().to_owned());
    // Original Lua failure remains authoritative despite optional diagnostics.
    // 尽管存在可选诊断，原 Lua 失败仍为权威结果。
    let failed_result = failed.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(failed_result.phase, OperationPhase::Failed);
    assert_eq!(
        failed_result.error.unwrap().code,
        EmbeddedErrorCode::ExecutionFailed
    );
    assert_eq!(host_calls.load(Ordering::Relaxed), 4);
    shutdown(&runtime);
    retired_receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("actual post-destruction diagnostic must arrive");
    assert!(
        gc_observed.load(Ordering::Relaxed),
        "real Lua GC must precede retirement emission"
    );
    assert_eq!(runtime.resources().unwrap().resident, 0);
    // All selected observations use semantic identities, not evolving array positions.
    // 全部选取观测使用语义身份，而非会演进的数组位置。
    let observations = observations.lock().unwrap();
    // Exactly one initialization and allocation belong to the actual shared physical VM.
    // 恰好一次初始化及分配归属实际共享物理 VM。
    let initialization = observations
        .iter()
        .filter(|observation| observation["phase"] == "initialization")
        .collect::<Vec<_>>();
    assert_eq!(initialization.len(), 1);
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation["phase"] == "bundle_compile_and_evaluate")
            .count(),
        1
    );
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation["phase"] == "initialization_request_cleanup")
            .count(),
        1
    );
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation["phase"] == "business_request_cleanup")
            .count(),
        operation_ids.len()
    );
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation["phase"] == "allocation")
            .count(),
        1
    );
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation["phase"] == "initialization_skipped")
            .count(),
        operation_ids.len() - 1
    );
    // Locate actual allocated instance by event meaning and verify both business operations refer to it.
    // 按事件含义定位实际已分配实例，并验证两个业务操作均引用它。
    let allocated_instance = &observations
        .iter()
        .find(|observation| observation["phase"] == "allocation")
        .unwrap()["instance_id"];
    for operation_id in &operation_ids {
        // Queue residence never claims a VM before actual construction or checkout.
        // 队列驻留绝不在实际构造或借用前声称拥有 VM。
        let queue = observations
            .iter()
            .find(|observation| {
                observation["phase"] == "queue" && observation["operation_id"] == *operation_id
            })
            .unwrap();
        assert!(queue["instance_id"].is_null());
        assert!(queue["elapsed_ns"].as_u64().is_some());
        // Each original operation receives exactly its real host call and allocator sample.
        // 每个原操作恰好接收其真实宿主调用及分配器采样。
        let business = observations
            .iter()
            .find(|observation| {
                observation["phase"] == "business" && observation["operation_id"] == *operation_id
            })
            .unwrap();
        assert_eq!(&business["instance_id"], allocated_instance);
        assert_eq!(business["runtime_id"], runtime.id());
        assert_eq!(business["pool_id"], pool);
        assert_eq!(business["host_wait"]["calls"], 1);
        assert!(business["host_wait"]["elapsed_ns"].as_u64().unwrap() > 0);
        assert!(
            business["host_wait"]["elapsed_ns"].as_u64().unwrap()
                <= business["elapsed_ns"].as_u64().unwrap()
        );
        assert!(business["lua_heap_bytes"].as_u64().unwrap() > 0);
        assert_eq!(business["succeeded"], operation_id != failed.id());
        // Cleanup records remain local to this same entered operation, including its actual error path.
        // 清理记录保持归属于此同一已进入操作，包括其实际错误路径。
        let cleanup = observations
            .iter()
            .find(|observation| {
                observation["phase"] == "business_request_cleanup"
                    && observation["operation_id"] == *operation_id
            })
            .unwrap();
        assert!(cleanup["elapsed_ns"].as_u64().is_some());
    }
    // Retirement is attributed to the instance, not fabricated as the last business operation's work.
    // 退役归属实例，而非虚构为最后业务操作的工作。
    let retired = observations
        .iter()
        .find(|observation| observation["phase"] == "retired")
        .unwrap();
    assert!(retired["operation_id"].is_null());
    assert_eq!(&retired["instance_id"], allocated_instance);
    assert!(retired["lua_heap_before_close_bytes"].as_u64().unwrap() > 0);
    assert!(retired.get("lua_heap_bytes").is_none());
}

/// Diagnostic callback panic and later removal cannot alter real SingleCall success or ownership cleanup.
/// 诊断回调 panic 及后续移除不能改变真实 SingleCall 成功或所有权清理。
/// No arguments or return value; keeping an external callback Arc proves removal is checked against current registration.
/// 无参数或返回值；保留外部回调 Arc 证明移除根据当前注册检查。
#[test]
fn embedded_diagnostics_panic_and_removed_external_callback_preserve_execution() {
    // Preserve any preexisting process callback across this serial shared-resource test.
    // 在此串行共享资源测试期间保留任何既有进程回调。
    let _restore = RestoreLogger::capture();
    // Existing real fixture exercises allocation, execution and physical VM retirement.
    // 既有真实夹具验证分配、执行及物理 VM 退役。
    let layout = SystemRuntimeTestLayout::new("embedded diagnostic panic and removal");
    // Original SingleCall scheduler must terminate successfully despite every diagnostic callback panicking.
    // 即使每个诊断回调 panic，原 SingleCall 调度器也必须成功终止。
    let runtime = runtime(&layout, pool_config());
    // Record entered diagnostics separately from actual business outcomes.
    // 将已进入诊断与实际业务结果分开记录。
    let deliveries = Arc::new(AtomicUsize::new(0));
    // Callback retains only its actual delivery counter.
    // 回调仅保留实际投递计数器。
    let callback_deliveries = Arc::clone(&deliveries);
    // Observe the actual original post-destruction callback boundary before changing global registration.
    // 在改变全局注册前观测实际原销毁后回调边界。
    let (retired_sender, retired_receiver) = std::sync::mpsc::channel();
    // Keep external callback ownership alive after removing its global registration.
    // 在移除全局注册后保持外部回调所有权存活。
    let callback: RuntimeLogCallback = Arc::new(move |event| {
        if let Ok(observation) = serde_json::from_str::<Value>(&event.message)
            && observation["luaskills_embedded_diagnostic"] == json!(1)
        {
            callback_deliveries.fetch_add(1, Ordering::Relaxed);
            if observation["phase"] == "retired" {
                let _ = retired_sender.send(());
            }
            panic!("intentional private diagnostic callback failure");
        }
    });
    set_log_callback(Some(Arc::clone(&callback)));
    // Snapshot the actual registered callback identity before its removal.
    // 在移除前快照实际注册回调身份。
    let subscription = diagnostic_subscriber().unwrap();
    // Original function returns application input without requiring any diagnostic data.
    // 原函数返回应用输入，不需要任何诊断数据。
    let pool = runtime
        .register_pool(
            definition(&layout, "return {call=function(a) return a end}"),
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "panic-revision".into(),
        )
        .unwrap();
    // Every enabled diagnostic panics, while the original real operation still succeeds and destroys its VM.
    // 每个已启用诊断均 panic，同时原真实操作仍成功并销毁其 VM。
    let result = runtime
        .submit(call(&pool, json!(7)), Duration::from_secs(5))
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Succeeded);
    assert_eq!(result.value, Some(json!(7)));
    assert_eq!(runtime.resources().unwrap().resident, 0);
    retired_receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("original retirement diagnostic must arrive");
    assert!(deliveries.load(Ordering::Relaxed) > 0);
    set_log_callback(None);
    // Removed subscriptions must reject lazy construction even while the original external Arc still exists.
    // 即使原外部 Arc 仍存在，已移除订阅也必须拒绝惰性构造。
    let constructed = AtomicBool::new(false);
    send_diagnostic(&subscription, || {
        constructed.store(true, Ordering::Relaxed);
        "unexpected removed diagnostic".to_owned()
    });
    assert!(!constructed.load(Ordering::Relaxed));
    assert!(subscription.upgrade().is_some());
    // Capture the stable delivery count only after original callback removal has completed.
    // 仅原回调移除完成后捕获稳定投递次数。
    let before = deliveries.load(Ordering::Relaxed);
    // The same original pool remains operational with the logger disabled and performs another fresh VM lifecycle.
    // 原池在日志器关闭时仍可运行，并执行另一次新 VM 生命周期。
    let result = runtime
        .submit(call(&pool, json!(8)), Duration::from_secs(5))
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Succeeded);
    assert_eq!(result.value, Some(json!(8)));
    // Replacement also invalidates the old subscription even though its external Arc remains owned.
    // 替换也使旧订阅无效，即使其外部 Arc 仍被拥有。
    let replacement_deliveries = Arc::new(AtomicUsize::new(0));
    // New observations belong only to the actually registered replacement callback.
    // 新观测仅归属实际注册的替换回调。
    let replacement_observed = Arc::clone(&replacement_deliveries);
    set_log_callback(Some(Arc::new(move |event| {
        if let Ok(observation) = serde_json::from_str::<Value>(&event.message)
            && observation["luaskills_embedded_diagnostic"] == json!(1)
        {
            replacement_observed.fetch_add(1, Ordering::Relaxed);
        }
    })));
    send_diagnostic(&subscription, || {
        constructed.store(true, Ordering::Relaxed);
        "unexpected replaced diagnostic".to_owned()
    });
    assert!(!constructed.load(Ordering::Relaxed));
    // A new actual SingleCall operation uses the new registration while old observation ownership stays inactive.
    // 新实际 SingleCall 操作使用新注册，同时旧观测所有权保持不活动。
    let replacement_result = runtime
        .submit(call(&pool, json!(9)), Duration::from_secs(5))
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();
    assert_eq!(replacement_result.phase, OperationPhase::Succeeded);
    assert_eq!(replacement_result.value, Some(json!(9)));
    shutdown(&runtime);
    assert_eq!(runtime.resources().unwrap().resident, 0);
    assert_eq!(deliveries.load(Ordering::Relaxed), before);
    assert!(replacement_deliveries.load(Ordering::Relaxed) > 0);
    drop(callback);
}

/// Removing a callback releases captured real runtime ownership while its reusable VM is still idle.
/// 在复用 VM 仍空闲时移除回调会释放其捕获的真实运行时所有权。
/// No arguments or return value; weak callback observation catches resident-to-logger ownership cycles.
/// 无参数或返回值；弱回调观测捕获常驻对象到日志器的所有权循环。
#[test]
fn embedded_diagnostics_weak_subscription_cannot_retain_idle_vm_host_callback() {
    // Restore the process-global logger after the actual runtime shuts down.
    // 在实际运行时关闭后恢复进程全局日志器。
    let _restore = RestoreLogger::capture();
    // Real package and worker ownership remain live during callback release observation.
    // 回调释放观测期间，真实包及工作线程所有权保持存活。
    let layout = SystemRuntimeTestLayout::new("embedded diagnostic weak subscriber lifetime");
    // The callback deliberately captures this actual runtime, reproducing a potential idle VM ownership cycle.
    // 回调刻意捕获此实际运行时，复现潜在空闲 VM 所有权循环。
    let runtime = Arc::new(runtime(&layout, pool_config()));
    // Captured host ownership must disappear with the last public callback owner.
    // 捕获宿主所有权必须随最后公开回调所有者消失。
    let captured_runtime = Arc::clone(&runtime);
    // Keep observation weak so the test itself does not extend callback lifetime.
    // 保持观测为弱引用，避免测试自身延长回调生命周期。
    let callback: RuntimeLogCallback = Arc::new(move |_| {
        std::hint::black_box(captured_runtime.id());
    });
    // The exact callback trait-object allocation is observed, not a temporary Arc wrapper.
    // 观测精确回调特征对象分配，而非临时 Arc 包装。
    let callback_lifetime = Arc::downgrade(&callback);
    set_log_callback(Some(Arc::clone(&callback)));
    drop(callback);
    // Actual reusable state stays allocated when diagnostic subscription ownership is removed.
    // 在诊断订阅所有权移除时，实际复用状态仍已分配。
    let pool = runtime
        .register_pool(
            definition(&layout, "return {call=function(a) return a end}"),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "weak-revision".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&pool, json!(true)), Duration::from_secs(5))
            .unwrap()
            .wait(Duration::from_secs(5))
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    assert_eq!(runtime.resources().unwrap().resident, 1);
    set_log_callback(None);
    // A previously acquired callback snapshot may finish in flight; wait for its actual release with a failure deadline.
    // 先前获取的回调快照可以完成进行中发送；使用失败截止时间等待其实际释放。
    let deadline = Instant::now() + Duration::from_secs(3);
    while callback_lifetime.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "resident diagnostics must not retain the removed host callback"
        );
        std::thread::yield_now();
    }
    assert_eq!(runtime.resources().unwrap().resident, 1);
    shutdown(&runtime);
    assert_eq!(runtime.resources().unwrap().resident, 0);
}
