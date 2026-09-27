use super::*;

/// Register queued `name` with room for a batch and retain exact grants and schema semantics.
/// 注册允许批次的队列 `name`，保留精确授权及 Schema 语义。
fn registry() -> Arc<CapabilityRegistry> {
    let mut limits = config();
    // Keep admission large enough for the three retained requests and their reserved output values.
    // 使入场容量足以容纳三个保留请求及其预留输出值。
    limits.max_host_request_bytes = 8192;
    let registry = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let mut contract = descriptor("test.delivery");
    contract.execution = CapabilityExecution::Queued;
    contract.max_concurrent = 4;
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: None,
        }])
        .unwrap();
    registry
}

/// Submit actual queued `arguments` through the public snapshot with its original parent control.
/// 通过公开快照和原始父控制提交实际排队 `arguments`。
fn submit(registry: &CapabilityRegistry, arguments: Value) -> HostRequestHandle {
    registry
        .snapshot()
        .unwrap()
        .submit_queued("test.delivery", caller(), grants(), arguments, control())
        .unwrap()
}

/// Decode a real bounded JSON batch; any malformed delimiter or lost field fails the test.
/// 解码实际有界 JSON 批次；任何分隔符错误或字段丢失都会使测试失败。
fn batch(broker: &HostRequestBroker, limit: usize, bytes: usize) -> Vec<HostRequest> {
    let encoded = broker.take_json(limit, bytes).unwrap();
    assert!(encoded.len() <= bytes);
    serde_json::from_slice(&encoded).unwrap()
}

/// Oversized delivery retains the exact queued identity and can later be delivered and acknowledged once.
/// 超大交付保留精确排队身份，随后可投递并确认一次。
#[test]
fn embedded_capabilities_json_delivery_failure_does_not_claim_execution() {
    let registry = registry();
    let handle = submit(&registry, json!("中文\0🦀"));
    let broker = registry.host_requests();
    assert_eq!(
        broker.take_json(1, 2).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        broker.status(handle.id()).unwrap().phase,
        HostRequestPhase::Queued
    );
    assert_eq!(
        broker
            .complete(handle.id(), value(json!(1)))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    let requests = batch(&broker, 1, 2048);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request_id, handle.id());
    assert_eq!(requests[0].arguments, json!("中文\0🦀"));
    assert!(batch(&broker, 1, 2048).is_empty());
    broker.complete(handle.id(), value(json!(2))).unwrap();
    assert_eq!(handle.wait().unwrap().result.unwrap(), json!(2));
}

/// A bounded prefix is delivered while the oversized next request remains the authoritative FIFO head.
/// 投递有界前缀，同时超大的下一请求仍为权威先进先出队首。
#[test]
fn embedded_capabilities_json_batch_preserves_delivered_prefix_and_oversized_head() {
    let registry = registry();
    let first = submit(&registry, Value::Null);
    let second = submit(&registry, json!("中".repeat(260)));
    let third = submit(&registry, Value::Null);
    let broker = registry.host_requests();
    let requests = batch(&broker, 3, 1000);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request_id, first.id());
    assert_eq!(
        broker.status(first.id()).unwrap().phase,
        HostRequestPhase::Dispatched
    );
    assert_eq!(
        broker.status(second.id()).unwrap().phase,
        HostRequestPhase::Queued
    );
    assert_eq!(
        broker.status(third.id()).unwrap().phase,
        HostRequestPhase::Queued
    );
    assert_eq!(
        broker.take_json(3, 1000).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let requests = batch(&broker, 3, 4096);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec![second.id(), third.id()]
    );
    for handle in [&first, &second, &third] {
        broker.complete(handle.id(), value(Value::Null)).unwrap();
        assert!(handle.wait().unwrap().result.is_ok());
    }
}

/// Cancellation after failed encoding remains a pre-dispatch cancellation with no possible host effects.
/// 编码失败后的取消仍为分发前取消，不可能存在宿主副作用。
#[test]
fn embedded_capabilities_json_delivery_rejection_remains_cancellable_without_ack() {
    let registry = registry();
    let handle = submit(&registry, Value::Null);
    let broker = registry.host_requests();
    assert_eq!(
        broker.take_json(1, 2).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    handle.cancel().unwrap();
    let outcome = handle.wait().unwrap();
    assert_eq!(outcome.effects, EffectState::NotStarted);
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::Cancelled
    );
    assert!(batch(&broker, 1, 2).is_empty());
    drop(handle);
    assert!(broker.is_drained().unwrap());
}

/// Competing Rust and JSON consumers share one actual dispatch transition and cannot duplicate a handler.
/// 竞争的 Rust 与 JSON 消费者共享一个实际分发变更，不能重复处理器。
#[test]
fn embedded_capabilities_json_and_rust_consumers_share_single_delivery() {
    let registry = registry();
    let handle = submit(&registry, Value::Null);
    let broker = registry.host_requests();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = [true, false]
        .into_iter()
        .map(|json| {
            let broker = Arc::clone(&broker);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                if json {
                    batch(&broker, 1, 2048)
                } else {
                    broker.take(1).unwrap()
                }
            })
        })
        .collect();
    barrier.wait();
    let requests: Vec<_> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request_id, handle.id());
    broker.complete(handle.id(), value(Value::Null)).unwrap();
    assert!(handle.wait().unwrap().result.is_ok());
}

/// Revoked queued requests drain before encoding, even when their serialized payload would exceed the budget.
/// 被撤销排队请求在编码前排空，即使其序列化内容会超出预算。
#[test]
fn embedded_capabilities_json_delivery_rechecks_authority_before_encoding() {
    let registry = registry();
    let permissions = grants();
    let handle = registry
        .snapshot()
        .unwrap()
        .submit_queued(
            "test.delivery",
            caller(),
            Arc::clone(&permissions),
            Value::Null,
            control(),
        )
        .unwrap();
    permissions.revoke("test.read").unwrap();
    assert!(batch(&registry.host_requests(), 1, 2).is_empty());
    let outcome = handle.wait().unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::PermissionDenied
    );
    assert_eq!(outcome.effects, EffectState::NotStarted);
}

/// Empty and closed queues return valid minimal JSON; invalid budgets cannot consume a pending request.
/// 空队列与已关闭队列返回合法最小 JSON；无效预算不能消费待处理请求。
#[test]
fn embedded_capabilities_json_batch_limits_are_checked_before_dispatch() {
    let registry = registry();
    let broker = registry.host_requests();
    assert_eq!(broker.take_json(1, 2).unwrap(), b"[]");
    let handle = submit(&registry, Value::Null);
    for (count, bytes) in [
        (0, 2048),
        (usize::MAX, 2048),
        (1, 0),
        (1, 1),
        (1, usize::MAX),
    ] {
        assert_eq!(
            broker.take_json(count, bytes).unwrap_err().code,
            EmbeddedErrorCode::InvalidArgument
        );
        assert_eq!(
            broker.status(handle.id()).unwrap().phase,
            HostRequestPhase::Queued
        );
    }
    broker.close();
    assert_eq!(broker.take_json(1, 2).unwrap(), b"[]");
    assert_eq!(handle.wait().unwrap().effects, EffectState::NotStarted);
}
