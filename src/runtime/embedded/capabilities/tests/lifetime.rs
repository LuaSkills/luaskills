use super::*;

/// A native closure capture whose actual destruction is controlled by channel barriers.
/// 实际析构由通道屏障控制的原生闭包捕获对象。
struct BlockingDestructor {
    /// Signals that destruction has begun but is not yet complete.
    /// 通知析构已开始但尚未完成。
    entered: std::sync::mpsc::Sender<()>,
    /// Release authority stays with the test rather than the callback.
    /// 释放权威由测试而非回调持有。
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Drop for BlockingDestructor {
    /// Block after ownership is removed from the entry and before native resources finish destruction.
    /// 在所有权从条目移除后、原生资源析构结束前阻塞。
    fn drop(&mut self) {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }
}

/// Removing a callback from its slot is not sufficient evidence that native cleanup has finished.
/// 从槽位移除回调不足以证明原生清理已结束。
#[test]
fn embedded_capabilities_native_destructor_must_finish_before_drained() {
    // The worker unregisters while the main thread inspects real destructor progress.
    // 工作线程注销时主线程观察真实析构进度。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    // Capture the whole resource so closure destruction owns its final cleanup.
    // 捕获完整资源，使闭包析构拥有其最终清理。
    let resource = BlockingDestructor {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    };
    let id = register(
        &registry,
        "test.cleanup",
        Arc::new(move |_| {
            std::hint::black_box(&resource);
            value(Value::Null)
        }),
    );
    // The worker keeps the exact registration and registry alive until unregister returns.
    // 工作线程保持精确注册与注册表存活，直到注销返回。
    let worker_registry = Arc::clone(&registry);
    let worker_id = id.clone();
    let worker = std::thread::spawn(move || worker_registry.unregister(&worker_id));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!registry.status(&id).unwrap().drained);
    assert_eq!(
        registry.forget(&id).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap().unwrap().drained);
    registry.forget(&id).unwrap();
}

/// Identical names in distinct runtimes never resolve to each other's callback implementation.
/// 不同运行时中的相同名称绝不解析到彼此的回调实现。
#[test]
fn embedded_capabilities_instances_remain_independent() {
    // Each registry has its own callback authority and trusted caller namespace.
    // 每个注册表具有独立回调权威与可信调用方命名空间。
    let first = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let second = CapabilityRegistry::new("runtime-b".into(), config()).unwrap();
    register(&first, "test.echo", Arc::new(|_| value(json!("first"))));
    register(&second, "test.echo", Arc::new(|_| value(json!("second"))));
    // Only the authenticated runtime field differs; plugin business arguments remain unchanged.
    // 仅已认证运行时字段不同；插件业务参数保持不变。
    let mut second_caller = caller();
    second_caller.runtime_id = "runtime-b".into();
    assert_eq!(
        first
            .snapshot()
            .unwrap()
            .invoke_native("test.echo", caller(), grants(), Value::Null, control())
            .unwrap()
            .result
            .unwrap(),
        json!("first")
    );
    assert_eq!(
        second
            .snapshot()
            .unwrap()
            .invoke_native("test.echo", second_caller, grants(), Value::Null, control())
            .unwrap()
            .result
            .unwrap(),
        json!("second")
    );
}

/// Retired registrations count against capacity until explicit drained-record removal.
/// 已退役注册计入容量，直到显式移除已排空记录。
#[test]
fn embedded_capabilities_registration_retention_is_bounded() {
    // A single retained slot makes accidental early reclamation observable.
    // 单个保留槽位使意外提前回收可被观察。
    let mut limits = config();
    limits.max_registered_capabilities = 1;
    let registry = CapabilityRegistry::new("runtime-a".into(), limits).unwrap();
    let id = register(&registry, "test.old", Arc::new(|_| value(Value::Null)));
    registry.unregister(&id).unwrap();
    assert_eq!(
        registry
            .register(vec![CapabilityRegistrationRequest {
                descriptor: descriptor("test.new"),
                native: Some(Arc::new(|_| value(Value::Null)))
            }])
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    registry.forget(&id).unwrap();
    register(&registry, "test.new", Arc::new(|_| value(Value::Null)));
}

/// Host-request deduplication cannot be advertised by a transport that supplies no request identity.
/// 未提供请求身份的传输不能声明宿主请求去重。
#[test]
fn embedded_capabilities_native_rejects_unavailable_idempotency_contract() {
    // Native callbacks currently receive operation context, not queued-request identities.
    // 原生回调当前接收操作上下文，而非队列请求身份。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let mut contract = descriptor("test.dedupe");
    contract.idempotency = CapabilityIdempotency::HostRequest;
    assert_eq!(
        registry
            .register(vec![CapabilityRegistrationRequest {
                descriptor: contract,
                native: Some(Arc::new(|_| value(Value::Null)))
            }])
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Unsupported
    );
    assert!(
        registry
            .snapshot()
            .unwrap()
            .list(&grants())
            .unwrap()
            .is_empty()
    );
}

/// Output schema failure must not erase an actual external commit.
/// 输出 Schema 失败不得抹去真实外部提交。
#[test]
fn embedded_capabilities_invalid_output_preserves_commit_without_cancellation() {
    // No cancellation masks the exact schema-validation result in this regression.
    // 此回归中没有取消掩盖精确 Schema 校验结果。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let mut contract = descriptor("test.schema");
    contract.effects = CapabilityEffects::Mutating;
    contract.output_schema = json!({"type":"integer"});
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: Some(Arc::new(|_| CapabilityOutcome {
                result: Ok(json!("invalid")),
                effects: EffectState::Committed,
            })),
        }])
        .unwrap();
    let outcome = registry
        .snapshot()
        .unwrap()
        .invoke_native("test.schema", caller(), grants(), Value::Null, control())
        .unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert_eq!(outcome.effects, EffectState::Committed);
}
