//! Real scheduler accounting during synchronous asynchronous-observer wakeup.
//! 异步观察者同步唤醒期间的真实调度器记账。

use super::*;
use std::future::Future;
use std::sync::mpsc::{Sender, channel};
use std::task::{Context, Poll, Wake, Waker};

/// Query the actual scheduler synchronously when its original terminal observation is notified.
/// 原始终态观测获通知时，同步查询真实调度器。
struct SchedulerQueryWake {
    /// Real runtime kept alive without adding an observer-owned runtime ownership cycle.
    /// 保持真实运行时可访问，且不添加观察者拥有的运行时所有权环。
    runtime: std::sync::Weak<EmbeddedRuntime>,
    /// Exact original operation whose terminal result is queried during reentry.
    /// 在重入期间查询其终态结果的精确原始操作。
    operation_id: String,
    /// Report completed actual queries to the test's bounded receiving observer.
    /// 向测试的有界接收观察者报告已完成的真实查询。
    observed: Sender<(EmbeddedRuntimeUsage, OperationPhase)>,
}

impl Wake for SchedulerQueryWake {
    /// Consume this waker, reenter scheduler usage and operation queries, then send their results; returns no value.
    /// 消费此唤醒器，重入调度器用量及操作查询，再发送结果；不返回值。
    fn wake(self: Arc<Self>) {
        // These public methods acquire the real center and operation locks on the notifying thread.
        // 这些公开方法在通知线程取得真实中心及操作锁。
        let runtime = self.runtime.upgrade().unwrap();
        let usage = runtime.usage().unwrap();
        let phase = runtime
            .operation(&self.operation_id)
            .unwrap()
            .snapshot()
            .unwrap()
            .phase;
        self.observed.send((usage, phase)).unwrap();
    }
}

/// Async observation can reenter real scheduler queries only after original completion bookkeeping drains.
/// 异步观测仅在原始完成记账排空后，才可重入真实调度器查询。
#[test]
fn embedded_scheduler_async_terminal_waker_reenters_after_bookkeeping() {
    // The existing queued host fixture holds real business execution until the observer is registered.
    // 既有队列宿主夹具在观察者登记前保持真实业务执行。
    let layout = SystemRuntimeTestLayout::new("embedded scheduler async observer reentry");
    let runtime = Arc::new(runtime(&layout, pool_config()));
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.observe",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function() return vulcan.host.call('test.observe',{}) end}",
            ),
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "async-observation-r1".into(),
        )
        .unwrap();
    let operation = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(3))
        .unwrap();
    let request = host_request(&runtime);
    // Register the custom waker while the operation is demonstrably nonterminal.
    // 操作明确未达终态时，登记自定义唤醒器。
    let (observed_tx, observed_rx) = channel();
    let waker = Waker::from(Arc::new(SchedulerQueryWake {
        runtime: Arc::downgrade(&runtime),
        operation_id: operation.id().into(),
        observed: observed_tx,
    }));
    let mut waiter = std::pin::pin!(operation.wait_terminal());
    assert!(
        waiter
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(Value::Bool(true)),
                effects: EffectState::NotApplicable,
            },
        )
        .unwrap();
    // The notifying thread must finish actual query reentry, not merely mark an observer wake counter.
    // 通知线程必须完成真实查询重入，而非仅标记观察者唤醒计数。
    let (usage, phase) = observed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(phase, OperationPhase::Succeeded);
    assert_eq!(usage.active_operations, 0);
    assert_eq!(usage.cleaning_operations, 0);
    assert_eq!(usage.queued_calls, 0);
    let Poll::Ready(Ok(snapshot)) = waiter.as_mut().poll(&mut Context::from_waker(&waker)) else {
        panic!("the acknowledged real terminal result must resolve async observation");
    };
    assert_eq!(
        snapshot.value,
        Some(json!({"ok":true,"value":true,"effects":"not_applicable"}))
    );
    shutdown(&runtime);
}
