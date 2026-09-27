use super::*;

/// Register a queued implementation and return its exact identity.
/// 注册队列实现并返回其精确身份。
fn queued(registry: &CapabilityRegistry, name: &str) -> String {
    let mut contract = descriptor(name);
    contract.execution = CapabilityExecution::Queued;
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: None,
        }])
        .unwrap()
        .remove(0)
}

/// Submit a real queued invocation through the same public snapshot API used by modules.
/// 通过模块使用的相同公开快照 API 提交真实队列调用。
fn submit(registry: &CapabilityRegistry, name: &str) -> HostRequestHandle {
    registry
        .snapshot()
        .unwrap()
        .submit_queued(name, caller(), grants(), Value::Null, control())
        .unwrap()
}

/// A callback can be delivered and completed only once, with terminal records retained until release.
/// 回调只能投递与完成一次，终态记录保留到释放。
#[test]
fn embedded_capabilities_queue_single_delivery_and_completion() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    queued(&registry, "test.queue");
    let broker = registry.host_requests();
    let handle = submit(&registry, "test.queue");
    assert_eq!(
        broker
            .complete(handle.id(), value(json!(1)))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    let request = broker.take(1).unwrap().remove(0);
    assert_eq!(request.caller, caller());
    assert_eq!(request.request_id, handle.id());
    assert!(broker.take(1).unwrap().is_empty());
    broker.complete(handle.id(), value(json!(2))).unwrap();
    assert_eq!(handle.wait().unwrap().result.unwrap(), json!(2));
    assert!(!broker.is_drained().unwrap());
    assert_eq!(
        broker
            .complete(handle.id(), value(json!(3)))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::AlreadyCompleted
    );
    drop(handle);
    assert!(broker.is_drained().unwrap());
    assert_eq!(
        broker
            .complete(&request.request_id, value(json!(3)))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::AlreadyCompleted
    );
}

/// Cancellation before delivery releases admission without running a host handler.
/// 投递前取消释放入场许可，不运行宿主处理器。
#[test]
fn embedded_capabilities_queue_cancel_before_dispatch_has_no_effects() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let registration = queued(&registry, "test.queue");
    let handle = submit(&registry, "test.queue");
    handle.cancel().unwrap();
    let outcome = handle.wait().unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::Cancelled
    );
    assert_eq!(outcome.effects, EffectState::NotStarted);
    assert!(registry.host_requests().take(1).unwrap().is_empty());
    assert_eq!(registry.status(&registration).unwrap().in_flight, 0);
}

/// Unregistering a queued-only registration drains it without requiring a still-running SDK pump.
/// 注销仅有排队任务的注册无需仍在运行的 SDK 事件泵即可排空。
#[test]
fn embedded_capabilities_queue_unregister_drains_without_pump() {
    // Keep the result handle alive to prove draining does not depend on dropping it.
    // 保持结果句柄存活，证明排空不依赖于丢弃该句柄。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let registration = queued(&registry, "test.queue");
    let handle = submit(&registry, "test.queue");
    let status = registry.unregister(&registration).unwrap();
    assert!(status.drained);
    assert_eq!(status.in_flight, 0);
    assert_eq!(
        handle.wait().unwrap().result.unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    registry.forget(&registration).unwrap();
    assert!(registry.host_requests().take(1).unwrap().is_empty());
}

/// Delivered handlers keep admission during cancellation and unregistration until actual acknowledgement.
/// 已投递处理器在取消与注销期间保留入场许可，直到真实确认。
#[test]
fn embedded_capabilities_queue_cancel_after_dispatch_preserves_commit_and_capacity() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let registration = queued(&registry, "test.queue");
    let handle = submit(&registry, "test.queue");
    let broker = registry.host_requests();
    broker.take(1).unwrap();
    handle.cancel().unwrap();
    assert!(handle.poll().unwrap().is_none());
    let status = registry.unregister(&registration).unwrap();
    assert_eq!(status.in_flight, 1);
    assert!(!status.drained);
    assert_eq!(
        broker.status(handle.id()).unwrap().phase,
        HostRequestPhase::Dispatched
    );
    broker
        .complete(
            handle.id(),
            CapabilityOutcome {
                result: Ok(json!("written")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let outcome = handle.wait().unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::Cancelled
    );
    assert_eq!(outcome.effects, EffectState::Committed);
    assert!(registry.status(&registration).unwrap().drained);
}

/// Abandoned waiters do not free the real SDK handler or lose its eventual completion acknowledgement.
/// 放弃等待不会释放真实 SDK 处理器，也不会丢失其后续完成确认。
#[test]
fn embedded_capabilities_queue_orphan_retains_capacity_until_sdk_completion() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let registration = queued(&registry, "test.queue");
    let handle = submit(&registry, "test.queue");
    let broker = registry.host_requests();
    let request = broker.take(1).unwrap().remove(0);
    drop(handle);
    assert!(!broker.is_drained().unwrap());
    assert_eq!(registry.status(&registration).unwrap().in_flight, 1);
    broker
        .complete(&request.request_id, value(Value::Null))
        .unwrap();
    assert!(broker.is_drained().unwrap());
    assert_eq!(registry.status(&registration).unwrap().in_flight, 0);
}

/// Live revocation and unregister gates stop requests that have not yet reached a handler.
/// 实时撤权与注销门阻止尚未到达处理器的请求。
#[test]
fn embedded_capabilities_queue_rechecks_authority_at_dispatch() {
    for revoke in [false, true] {
        let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
        let registration = queued(&registry, "test.queue");
        let permissions = grants();
        let handle = registry
            .snapshot()
            .unwrap()
            .submit_queued(
                "test.queue",
                caller(),
                Arc::clone(&permissions),
                Value::Null,
                control(),
            )
            .unwrap();
        if revoke {
            permissions.revoke("test.read").unwrap();
        } else {
            registry.unregister(&registration).unwrap();
        }
        assert!(registry.host_requests().take(1).unwrap().is_empty());
        let outcome = handle.wait().unwrap();
        assert_eq!(
            outcome.result.unwrap_err().code,
            if revoke {
                EmbeddedErrorCode::PermissionDenied
            } else {
                EmbeddedErrorCode::Closed
            }
        );
        assert_eq!(outcome.effects, EffectState::NotStarted);
    }
}

/// Closing the broker prevents further delivery but preserves ownership of already delivered work.
/// 关闭代理阻止后续投递，但保留已投递任务的所有权。
#[test]
fn embedded_capabilities_queue_close_drains_dispatched_and_queued_independently() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    queued(&registry, "test.first");
    queued(&registry, "test.second");
    let first = submit(&registry, "test.first");
    let broker = registry.host_requests();
    broker.take(1).unwrap();
    let second = submit(&registry, "test.second");
    broker.close();
    assert!(broker.take(1).unwrap().is_empty());
    assert!(first.poll().unwrap().is_none());
    assert_eq!(second.wait().unwrap().effects, EffectState::NotStarted);
    broker.complete(first.id(), value(Value::Null)).unwrap();
    assert_eq!(
        first.wait().unwrap().result.unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
    drop(first);
    drop(second);
    assert!(broker.is_drained().unwrap());
}

/// Request metadata and output reservations consume the byte bound even before SDK delivery.
/// 请求元数据与输出预留在 SDK 投递前即消耗字节上限。
#[test]
fn embedded_capabilities_queue_byte_bound_rejects_without_leaking_admission() {
    let mut limits = config();
    limits.max_host_request_bytes = limits.max_value_bytes;
    let registry = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let registration = queued(&registry, "test.queue");
    let result = registry.snapshot().unwrap().submit_queued(
        "test.queue",
        caller(),
        grants(),
        Value::Null,
        control(),
    );
    assert_eq!(
        result.err().unwrap().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(registry.status(&registration).unwrap().in_flight, 0);
    assert!(registry.host_requests().is_drained().unwrap());
}

/// Real concurrent consumers cannot both acquire or complete the same callback.
/// 真实并发消费者不能同时取得或完成同一个回调。
#[test]
fn embedded_capabilities_queue_concurrent_consumers_have_one_completion_owner() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    queued(&registry, "test.queue");
    let handle = submit(&registry, "test.queue");
    let broker = registry.host_requests();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let broker = Arc::clone(&broker);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                broker.take(1).unwrap().len()
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum::<usize>(),
        1
    );
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let broker = Arc::clone(&broker);
            let barrier = Arc::clone(&barrier);
            let id = handle.id().to_owned();
            std::thread::spawn(move || {
                barrier.wait();
                broker.complete(&id, value(json!(1)))
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes.into_iter().find_map(Result::err).unwrap().code,
        EmbeddedErrorCode::AlreadyCompleted
    );
}

/// Deadlines cancel interest while preserving a late acknowledged external commit.
/// 截止时间取消等待意愿，同时保留随后确认的外部提交。
#[test]
fn embedded_capabilities_queue_expired_parent_keeps_actual_handler_until_ack() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let registration = queued(&registry, "test.queue");
    let parent = Arc::new(CallControl::new(Duration::from_millis(100)).unwrap());
    let handle = registry
        .snapshot()
        .unwrap()
        .submit_queued(
            "test.queue",
            caller(),
            grants(),
            Value::Null,
            Arc::clone(&parent),
        )
        .unwrap();
    let broker = registry.host_requests();
    broker.take(1).unwrap();
    std::thread::sleep(
        parent
            .deadline()
            .saturating_duration_since(std::time::Instant::now()),
    );
    assert!(handle.poll().unwrap().is_none());
    assert_eq!(
        broker
            .status(handle.id())
            .unwrap()
            .cancellation
            .unwrap()
            .code,
        EmbeddedErrorCode::DeadlineExceeded
    );
    assert_eq!(registry.status(&registration).unwrap().in_flight, 1);
    broker
        .complete(
            handle.id(),
            CapabilityOutcome {
                result: Ok(json!(1)),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let outcome = handle.wait().unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::DeadlineExceeded
    );
    assert_eq!(outcome.effects, EffectState::Committed);
    assert_eq!(registry.status(&registration).unwrap().in_flight, 0);
}

/// A callback cannot submit a same-runtime SDK wait and deadlock its own event loop.
/// 回调不能提交同运行时 SDK 等待并死锁自身事件循环。
#[test]
fn embedded_capabilities_queue_rejects_submission_from_native_callback() {
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    queued(&registry, "test.queue");
    let weak = Arc::downgrade(&registry);
    register(
        &registry,
        "test.native",
        Arc::new(move |_| {
            let result = weak.upgrade().unwrap().snapshot().unwrap().submit_queued(
                "test.queue",
                caller(),
                grants(),
                Value::Null,
                control(),
            );
            value(json!(result.err().unwrap().code))
        }),
    );
    let outcome = registry
        .snapshot()
        .unwrap()
        .invoke_native("test.native", caller(), grants(), Value::Null, control())
        .unwrap();
    assert_eq!(outcome.result.unwrap(), json!("busy"));
    assert!(registry.host_requests().is_drained().unwrap());
}

/// Native and queued handlers consume one common parent admission budget.
/// 原生与队列处理器消耗同一个父级入场预算。
#[test]
fn embedded_capabilities_queue_and_native_share_parent_capacity() {
    // The single parent slot must remain occupied even after queued work is cancelled.
    // 即使队列任务已取消，唯一父级槽位也必须继续占用。
    let mut limits = config();
    limits.max_host_requests = 1;
    let registry = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    queued(&registry, "test.queue");
    register(
        &registry,
        "test.native",
        Arc::new(|_| value(json!("available"))),
    );
    // Deliver the request and leave its real handler waiting for an acknowledgement.
    // 投递请求，并使其真实处理器等待确认。
    let handle = submit(&registry, "test.queue");
    let broker = registry.host_requests();
    broker.take(1).unwrap();
    handle.cancel().unwrap();
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .invoke_native("test.native", caller(), grants(), Value::Null, control())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    broker.complete(handle.id(), value(Value::Null)).unwrap();
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .invoke_native("test.native", caller(), grants(), Value::Null, control())
            .unwrap()
            .result
            .unwrap(),
        json!("available")
    );
}

/// Revoking a grant after dispatch remains visible to the SDK and preserves actual commit evidence.
/// 分发后撤销授权仍对 SDK 可见，并保留真实提交证据。
#[test]
fn embedded_capabilities_queue_late_revocation_keeps_committed_effects() {
    // Shared permission ownership lets the host revoke a live callback without replacing its snapshot.
    // 共享权限所有权使宿主无需替换快照就能撤销活动回调授权。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    queued(&registry, "test.queue");
    let permissions = grants();
    let handle = registry
        .snapshot()
        .unwrap()
        .submit_queued(
            "test.queue",
            caller(),
            Arc::clone(&permissions),
            Value::Null,
            control(),
        )
        .unwrap();
    let broker = registry.host_requests();
    broker.take(1).unwrap();
    permissions.revoke("test.read").unwrap();
    assert_eq!(
        broker
            .status(handle.id())
            .unwrap()
            .cancellation
            .unwrap()
            .code,
        EmbeddedErrorCode::PermissionDenied
    );
    assert!(handle.poll().unwrap().is_none());
    broker
        .complete(
            handle.id(),
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let outcome = handle.wait().unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::PermissionDenied
    );
    assert_eq!(outcome.effects, EffectState::Committed);
}
