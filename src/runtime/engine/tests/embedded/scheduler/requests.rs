//! Admission-bound request identity across Lua mutation, reused VMs and independent finalization.
//! 入场绑定请求身份跨 Lua 修改、复用 VM 及独立关闭的验证。

use super::*;
use crate::RuntimeRequestContext;

/// Build host context for exact optional `request_id`; never derive authority from business arguments.
/// 为精确可选 `request_id` 构造宿主上下文；绝不从业务参数派生权威。
fn context(request_id: &str) -> LuaInvocationContext {
    LuaInvocationContext {
        request_context: Some(RuntimeRequestContext {
            request_id: Some(request_id.into()),
            ..RuntimeRequestContext::default()
        }),
        ..LuaInvocationContext::default()
    }
}

/// Read request correlation from an admitted module `snapshot`, failing on an unbound operation.
/// 从已入场模块 `snapshot` 读取请求关联，未绑定操作令测试失败。
fn correlation(snapshot: &OperationSnapshot) -> Option<&str> {
    match &snapshot.context {
        OperationContext::Module(context) => context.caller.request_id.as_deref(),
        OperationContext::Unbound => panic!("formal operation must retain its original caller"),
    }
}

/// Initialization and business callbacks keep host correlation despite Lua mutation and VM reuse.
/// 初始化与业务回调在 Lua 修改和 VM 复用下仍保留宿主关联。
#[test]
fn embedded_request_identity_is_frozen_before_initialization_and_lua_mutation() {
    // One physical reusable VM must distinguish successive host requests without losing its state.
    // 同一个物理复用 VM 必须区分后续宿主请求，同时保留自身状态。
    let layout = SystemRuntimeTestLayout::new("trusted request identity");
    // Own one bounded scheduler for both reuse policies under test.
    // 为待测的两种复用策略持有一个有界调度器。
    let runtime = runtime(&layout, pool_config());
    // Retain actual native caller evidence outside Lua state.
    // 在 Lua 状态之外保留实际原生调用方证据。
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Give the native handler its own shared evidence owner.
    // 为原生处理器提供独立的共享证据所有者。
    let observed = Arc::clone(&seen);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.request",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(move |invocation| {
                observed
                    .lock()
                    .expect("record native identity")
                    .push(invocation.caller.clone());
                CapabilityOutcome {
                    result: Ok(invocation.arguments.clone()),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .expect("register native identity observer");
    // Bind initialization and business callbacks to the same reusable instance.
    // 将初始化与业务回调绑定到同一个可复用实例。
    let pool = runtime.register_pool(
        definition(&layout, "assert(vulcan.host.call('test.request','initialization').ok); local count=0; return {call=function(a) count=count+1; vulcan.context.request.request_id='lua-forged'; local r=vulcan.host.call('test.request',{request_id='argument-forged',count=count}); assert(r.ok); return r.value end}"),
        pool_policy(InstanceReuse::Reusable), permissions(), "request-revision".into(),
    ).expect("register reusable pool");
    for (index, request_id) in ["host-first", "host-second"].into_iter().enumerate() {
        // Admission freezes this request before initialization can invoke the host.
        // 入场在初始化能够调用宿主之前冻结此请求。
        let operation = runtime
            .submit(
                EmbeddedCall {
                    context: context(request_id),
                    ..call(&pool, Value::Null)
                },
                Duration::from_secs(5),
            )
            .expect("admit exact host request");
        assert_eq!(
            correlation(&operation.snapshot().expect("admitted context")),
            Some(request_id)
        );
        // Observe the original operation through actual callback completion.
        // 观察原操作直到实际回调完成。
        let result = operation
            .wait(Duration::from_secs(5))
            .expect("real callback completes");
        assert_eq!(result.phase, OperationPhase::Succeeded, "{result:?}");
        assert_eq!(
            result.value,
            Some(json!({"request_id":"argument-forged","count":index + 1}))
        );
        assert_eq!(correlation(&result), Some(request_id));
        assert!(
            result
                .host_effects
                .iter()
                .all(|effect| effect.caller.request_id.as_deref() == Some(request_id))
        );
    }
    // Compare the full native observation order including the first initialization.
    // 比较包含首次初始化在内的完整原生观测顺序。
    let callers = seen.lock().expect("original callback observations");
    // Project only request correlations after retaining the complete callers above.
    // 在上方保留完整调用方之后，仅投影请求关联。
    let ids: Vec<_> = callers
        .iter()
        .map(|caller| caller.request_id.as_deref())
        .collect();
    assert_eq!(
        ids,
        [Some("host-first"), Some("host-first"), Some("host-second")]
    );
    drop(callers);
    // Single-call closing is the same operation and must preserve its admitted request correlation.
    // 单次关闭属于同一操作，必须保留其入场请求关联。
    let mut single = definition(
        &layout,
        "return {call=function() return true end, shutdown=function() vulcan.context.request.request_id='closing-forged'; local r=vulcan.host.call('test.request','closing'); assert(r.ok); return r.value end}",
    );
    single.exports.push(ModuleExport {
        name: "shutdown".into(),
        input_schema: json!(true),
        output_schema: json!(true),
    });
    single.finalizer = Some(ModuleFinalizer {
        export: "shutdown".into(),
        arguments: Value::Null,
        timeout_ms: 5000,
    });
    // Register independent single-call ownership with explicit finalization.
    // 注册带显式关闭逻辑的独立单次调用所有权。
    let single_pool = runtime
        .register_pool(
            single,
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "single-request-revision".into(),
        )
        .expect("single-call finalizer");
    // Wait for both business execution and same-operation finalization.
    // 等待业务执行及同操作关闭全部结束。
    let closing = runtime
        .submit(
            EmbeddedCall {
                context: context("host-single"),
                ..call(&single_pool, Value::Null)
            },
            Duration::from_secs(5),
        )
        .expect("single-call admission")
        .wait(Duration::from_secs(5))
        .expect("same-operation closing");
    assert_eq!(closing.phase, OperationPhase::Succeeded, "{closing:?}");
    assert_eq!(correlation(&closing), Some("host-single"));
    assert_eq!(closing.host_effects.len(), 1);
    assert_eq!(
        closing
            .host_effects
            .first()
            .expect("actual closing callback")
            .caller
            .request_id
            .as_deref(),
        Some("host-single")
    );
    shutdown(&runtime);
}

/// Session business requests retain individual identity, while its independent closing operation has none.
/// 会话业务请求保留各自身份，而独立关闭操作不继承业务请求身份。
#[test]
fn embedded_request_identity_queued_session_and_independent_close_are_distinct() {
    // Isolate files and registry state for real queued callback delivery.
    // 为实际排队回调投递隔离文件与注册表状态。
    let layout = SystemRuntimeTestLayout::new("request identity queued session");
    // Own the scheduler through its actual independent finalization.
    // 持有调度器直至其实际独立关闭结束。
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.request",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .expect("register queued observer");
    // Deliberately leave forged Lua context behind before closing the same VM.
    // 在关闭同一 VM 之前刻意保留伪造的 Lua 上下文。
    let mut module = definition(
        &layout,
        "return {call=function(a) vulcan.context.request.request_id='lua-forged'; local r=vulcan.host.call('test.request',a); assert(r.ok); return r.value end, shutdown=function() local r=vulcan.host.call('test.request','closing'); assert(r.ok); return r.value end}",
    );
    module.exports.push(ModuleExport {
        name: "shutdown".into(),
        input_schema: json!(true),
        output_schema: json!(true),
    });
    module.finalizer = Some(ModuleFinalizer {
        export: "shutdown".into(),
        arguments: Value::Null,
        timeout_ms: 5000,
    });
    // Bind the stateful module to the fixed-session reuse policy.
    // 将有状态模块绑定到固定会话复用策略。
    let pool = runtime
        .register_pool(
            module,
            pool_policy(InstanceReuse::Session),
            permissions(),
            "request-revision".into(),
        )
        .expect("register session");
    // Complete opening before submitting either business request.
    // 在提交任何业务请求之前完成开启。
    let session = sessions::opened(&runtime, &pool);
    for request_id in ["host-first", "host-second"] {
        // Keep the native operation separate from its host correlation.
        // 将原生操作与其宿主关联保持分离。
        let operation = runtime
            .submit_session(
                &session,
                "call".into(),
                json!(request_id),
                context(request_id),
                Duration::from_secs(5),
            )
            .expect("submit session request");
        // Read the actual queued callback rather than reconstructing its caller.
        // 读取实际排队回调，不重建其调用方。
        let request = host_request(&runtime);
        assert_eq!(request.caller.request_id.as_deref(), Some(request_id));
        assert_ne!(request.request_id, request_id);
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
            .expect("complete exact callback");
        // Retain terminal evidence for the exact acknowledged callback.
        // 为精确已确认回调保留终态证据。
        let result = operation
            .wait(Duration::from_secs(5))
            .expect("complete original operation");
        assert_eq!(result.phase, OperationPhase::Succeeded, "{result:?}");
        assert_eq!(correlation(&result), Some(request_id));
    }
    runtime
        .close_session(&session)
        .expect("request independent close");
    // Independent close must omit business correlation even on the same VM.
    // 即使使用同一 VM，独立关闭也必须省略业务关联。
    let closing = host_request(&runtime);
    assert!(
        closing.caller.request_id.is_none(),
        "independent finalization cannot reuse business correlation"
    );
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &closing.request_id,
            CapabilityOutcome {
                result: Ok(Value::Null),
                effects: EffectState::NotApplicable,
            },
        )
        .expect("finish closing callback");
    // Observe the closing operation identified by the actual callback.
    // 观察由实际回调标识的关闭操作。
    let result = runtime
        .operation(&closing.caller.operation_id)
        .expect("original closing operation")
        .wait(Duration::from_secs(5))
        .expect("closing completed");
    assert_eq!(result.phase, OperationPhase::Succeeded, "{result:?}");
    assert!(correlation(&result).is_none());
    shutdown(&runtime);
}

/// Invalid or oversized host correlation is rejected before initialization and leaves no operation record.
/// 非法或超限宿主关联在初始化前被拒绝，不留下操作记录。
#[test]
fn embedded_request_identity_admission_rejects_invalid_or_oversized_values() {
    // Isolate a module whose initializer proves any accidental execution.
    // 隔离模块，其初始化器用于证明任何意外执行。
    let layout = SystemRuntimeTestLayout::new("request identity admission bounds");
    // Derive the invalid size from the authoritative request queue budget.
    // 从权威请求队列预算派生非法大小。
    let config = pool_config();
    // Exceed the existing context budget without inventing another size limit.
    // 超过既有上下文预算，不另设大小上限。
    let oversized = "x".repeat(config.max_queued_bytes + 1);
    // Retain scheduler ownership while all invalid requests are rejected.
    // 拒绝全部非法请求期间保留调度器所有权。
    let runtime = runtime(&layout, config);
    // Register metadata without executing the deliberately failing initializer.
    // 注册元数据，不执行刻意失败的初始化器。
    let pool = runtime
        .register_pool(
            definition(&layout, "error('must never initialize')"),
            pool_policy(InstanceReuse::SingleCall),
            permissions(),
            "request-revision".into(),
        )
        .expect("register unexecuted pool");
    for request_id in ["", " ", "host\0forged", oversized.as_str()] {
        assert!(
            runtime
                .submit(
                    EmbeddedCall {
                        context: context(request_id),
                        ..call(&pool, Value::Null)
                    },
                    Duration::from_secs(5)
                )
                .is_err()
        );
    }
    assert!(
        runtime
            .list_operations(Some(&pool), None, 10)
            .expect("no rejected records")
            .operation_ids
            .is_empty()
    );
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .expect("no physical allocation")
            .resources
            .resident,
        0
    );
    shutdown(&runtime);
}
