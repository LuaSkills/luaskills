//! Integer ingress rejection before scheduler admission, Lua initialization and native dispatch.
//! 调度器入场、Lua 初始化及原生分发前的整数入口拒绝。

use super::*;

/// Invalid JSON integers never initialize Lua or dispatch a host handler during cold submission.
/// 冷提交中的无效 JSON 整数绝不初始化 Lua，也不分发宿主处理器。
#[test]
fn embedded_integer_scheduler_rejects_before_admission_and_initialization() {
    // Initialization would both write a file and enter the explicit native probe.
    // 初始化本应同时写入文件并进入显式原生探针。
    let layout = SystemRuntimeTestLayout::new("embedded integer cold admission");
    // The real scheduler starts with no allocated VM or retained operation.
    // 真实调度器起初未分配 VM，也未保留操作。
    let runtime = runtime(&layout, pool_config());
    // A shared counter proves neither initialization nor business dispatch reaches the handler.
    // 共享计数器证明初始化及业务分发均未到达处理器。
    let calls = Arc::new(Mutex::new(0));
    // The registered native callback retains the same observable count.
    // 已注册原生回调持有同一可观察计数。
    let captured = Arc::clone(&calls);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.input",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(move |_| {
                *captured.lock().unwrap() += 1;
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    // Source side effects make late rejection observable before any business call could run.
    // 源码副作用使过晚拒绝在任何业务调用运行前可观察。
    let pool = runtime.register_pool(definition(&layout,
        "vulcan.fs.write('integer-initialized.txt','bad'); vulcan.host.call('test.input',{}); return {call=function(a) return vulcan.host.call('test.input',a) end}"),
        pool_policy(InstanceReuse::Reusable), permissions(), "integer-r1".into()).unwrap();
    // Capture production usage rather than inferring absence of admission from one file.
    // 捕获生产用量，不仅通过单个文件推断尚未入场。
    let before = runtime.usage().unwrap();
    // Exact pool resource accounting distinguishes a retained VM from an empty request queue.
    // 精确池资源计数区分保留 VM 与空请求队列。
    let resident_before = runtime.pool_resources(&pool).unwrap().resident;
    // Reject every unsafe integer variant through the formal scheduler's actual enqueue boundary.
    // 通过正式调度器真实排队边界拒绝每种不安全整数变体。
    let maximum = LuaEngine::EMBEDDED_MAX_SAFE_INTEGER;
    for arguments in [
        json!(maximum + 1),
        json!(-(maximum + 2)),
        json!(maximum + 3),
        json!(u64::MAX),
        json!({"nested":[null,{"value":i64::MAX}]}),
    ] {
        // Submission itself fails, so no operation identity or cold initialization may be published.
        // 提交本身失败，因此不得发布操作身份或冷初始化。
        let error = runtime
            .submit(call(&pool, arguments), Duration::from_secs(3))
            .err()
            .unwrap();
        assert_eq!(error.code, EmbeddedErrorCode::InvalidArgument);
    }
    // Context ingress must obey the same early scheduler rejection.
    // 上下文入口必须遵循相同调度器提前拒绝。
    let mut request = call(&pool, Value::Null);
    request.context = LuaInvocationContext::new(None, json!({"bytes":u64::MAX}), json!({}));
    assert_eq!(
        runtime
            .submit(request, Duration::from_secs(3))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(!layout.package_root.join("integer-initialized.txt").exists());
    // Queue and resident usage remain unchanged because every failure precedes admission.
    // 每次失败均先于入场，因此排队及常驻用量保持不变。
    let after = runtime.usage().unwrap();
    assert_eq!(after.queued_calls, before.queued_calls);
    assert_eq!(after.active_operations, before.active_operations);
    assert_eq!(
        runtime.pool_resources(&pool).unwrap().resident,
        resident_before
    );
    shutdown(&runtime);
}

/// Queued unsafe host output with a wide key returns a bounded error envelope and retains its commit.
/// 带长键的不安全队列宿主输出返回有界错误信封，并保留其提交。
#[test]
fn embedded_integer_queued_wide_host_key_preserves_bounded_error_and_commit() {
    // Real Lua execution waits for this runtime's actual host request broker.
    // 真实 Lua 执行等待此运行时的实际宿主请求代理。
    let layout = SystemRuntimeTestLayout::new("embedded integer bounded queued error");
    // Capture the original production value budget used by operation publication.
    // 捕获操作发布使用的原生产值预算。
    let config = pool_config();
    // The original host value and returned error envelope must both fit this same value limit.
    // 原宿主值及返回错误信封均须满足此相同值上限。
    let max_value_bytes = config.max_value_bytes;
    // Own one formal scheduler with real callbacks and operation evidence.
    // 持有一个具备真实回调及操作证据的正式调度器。
    let runtime = runtime(&layout, config);
    // Mutating classification causes the queued completion to retain confirmed commit evidence.
    // 变更分类使队列完成保留已确认提交证据。
    let mut contract =
        super::super::capabilities::descriptor("test.wide", CapabilityExecution::Queued);
    contract.effects = CapabilityEffects::Mutating;
    // Check the encoded host value against its independently declared callback output limit.
    // 针对其独立声明的回调输出上限检查已编码宿主值。
    let max_host_output_bytes = contract.max_output_bytes;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: None,
        }])
        .unwrap();
    // Returning the complete callback envelope exposes any later publication-size failure.
    // 返回完整回调信封，使任何后续发布大小失败可观察。
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function() return vulcan.host.call('test.wide',{}) end}",
            ),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "integer-wide-r1".into(),
        )
        .unwrap();
    // Submission reaches actual Lua and queues exactly one host handler request.
    // 提交到达真实 Lua，并且只排队一个宿主处理器请求。
    let operation = runtime
        .submit(call(&pool, Value::Null), Duration::from_secs(3))
        .unwrap();
    // Stable request and effect identities come from the actual broker dispatch.
    // 稳定请求及副作用身份来自实际代理分发。
    let request = host_request(&runtime);
    // Escaping this application key into a diagnostic previously doubled its 800 separators.
    // 将此应用键转义到诊断曾使其 800 个分隔符长度翻倍。
    let host_value = Value::Object(serde_json::Map::from_iter([(
        "/".repeat(800),
        json!(u64::MAX),
    )]));
    // Encoded original output is valid under both existing byte budgets before Lua representation.
    // 原输出编码在 Lua 表示前满足两个既有字节预算。
    let host_bytes = serde_json::to_vec(&host_value).unwrap().len();
    assert!(host_bytes <= max_value_bytes);
    assert!(host_bytes <= max_host_output_bytes);
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(host_value),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    // Completion acknowledgement remains successful; Lua consumes a value error, not a replayed request.
    // 完成确认保持成功；Lua 消费值错误，而非重放请求。
    let snapshot = operation.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Succeeded);
    // The scheduler marks executed Lua effects unknown; one confirmed host commit cannot prove all Lua effects.
    // 调度器将已执行 Lua 的副作用标记为未知；单个已确认宿主提交无法证明全部 Lua 副作用。
    // The callback envelope and exact host ledger below independently retain the confirmed committed state.
    // 下方回调信封及精确宿主账本独立保留已确认提交状态。
    assert_eq!(snapshot.effects, EffectState::Unknown);
    // The exact operation value is the callback's bounded InvalidArgument envelope.
    // 精确操作值为回调的有界 InvalidArgument 信封。
    let envelope = snapshot.value.as_ref().unwrap();
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], json!("invalid_argument"));
    assert_eq!(envelope["effects"], json!("committed"));
    assert!(envelope.get("value").is_none());
    assert!(
        !envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&"/".repeat(800))
    );
    assert!(serde_json::to_vec(envelope).unwrap().len() <= max_value_bytes);
    assert!(serde_json::to_vec(envelope).unwrap().len() <= host_bytes);
    assert!(
        snapshot
            .host_effects
            .iter()
            .any(
                |effect| Some(effect.effect_id.as_str()) == request.effect_id.as_deref()
                    && effect.request_id.as_deref() == Some(request.request_id.as_str())
                    && effect.phase == HostEffectPhase::Completed
                    && effect.effects == EffectState::Committed
            )
    );
    assert!(
        runtime
            .capabilities()
            .host_requests()
            .take(1)
            .unwrap()
            .is_empty()
    );
    shutdown(&runtime);
}
