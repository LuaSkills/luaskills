//! Real scheduler context retention before callbacks, across generations and after disk reopen.
//! 真实调度器在回调前、跨代次及磁盘重新打开后的上下文保留。

use super::*;

/// Borrow the formal module context from `snapshot`; fail when a scheduler operation loses its binding.
/// 从 `snapshot` 借用正式模块上下文；调度操作丢失绑定时失败。
fn module_context(snapshot: &OperationSnapshot) -> &ModuleOperationContext {
    match &snapshot.context {
        OperationContext::Module(context) => context,
        OperationContext::Unbound => panic!("formal operation lost its module binding"),
    }
}

/// Calls without host effects retain their original generations and capability membership after the entire runtime exits.
/// 没有宿主副作用的调用在整个运行时退出后仍保留原始代次及能力成员身份。
#[test]
fn embedded_operation_context_survives_generation_change_and_restart() {
    // Use a real package but no callback in either executed Lua source.
    // 使用真实包，但两份执行的 Lua 源码都不调用回调。
    let layout = SystemRuntimeTestLayout::new("embedded durable operation context");
    // Only owned snapshots escape this scope; no live operation or writer can hold the old database open.
    // 仅自有快照离开此作用域；没有活动操作或写入者可继续占用旧数据库。
    let (runtime_id, first, second) = {
        // Share the actual journal with the formal scheduler's fixed writer.
        // 与正式调度器的固定写入者共享真实日志。
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(8, 128 * 1024));
        // Capture the exact original membership before registering a later unused capability.
        // 在稍后注册未使用能力前捕获精确原始成员身份。
        let original_membership = runtime.capabilities().snapshot().unwrap().revision();
        // Frozen source authority is independent of the deliberately forged input object.
        // 冻结源码权威独立于有意伪造的输入对象。
        let original = definition(&layout, "return {call=function(a) return a end}");
        // Preserve the first registered pool independently from its replacement.
        // 独立于替换池保留首个已注册池。
        let old = runtime
            .register_pool(
                original.clone(),
                pool_policy(InstanceReuse::Reusable),
                permissions(),
                "original-revision".into(),
            )
            .unwrap();
        // This is business data, not a module identity override.
        // 这是业务数据，不是模块身份覆盖。
        let forged = json!({"plugin_id":"forged", "package_generation":"forged", "execution_revision":"forged", "session_id":"forged"});
        // The real scheduler binds the first operation before consuming business input.
        // 真实调度器在消费业务输入前绑定首个操作。
        let first_operation = runtime.submit(call(&old, forged.clone()), OBSERVE).unwrap();
        // Retain the acknowledged result for comparison after the old pool is gone.
        // 保留已确认结果，以便旧池消失后进行比较。
        let first = first_operation.wait(OBSERVE).unwrap();
        assert_eq!(first.phase, OperationPhase::Succeeded, "{:?}", first.error);
        assert_eq!(first.value, Some(forged));
        assert!(first.host_effects.is_empty());
        assert_eq!(module_context(&first).caller.plugin_id, original.plugin_id);
        assert_eq!(
            module_context(&first).caller.package_generation,
            original.generation
        );
        assert_eq!(
            module_context(&first).caller.execution_revision,
            "original-revision"
        );
        assert_eq!(
            module_context(&first).caller.security_partition,
            original.security_partition
        );
        assert_eq!(
            module_context(&first).caller.workspace_root,
            original.workspace_root
        );
        assert_eq!(
            module_context(&first).caller.operation_id,
            first.operation_id
        );
        assert_eq!(module_context(&first).pool_id, old);
        assert_eq!(
            module_context(&first).capability_revision,
            original_membership
        );
        assert_eq!(module_context(&first).export.as_deref(), Some("call"));
        assert!(module_context(&first).caller.session_id.is_none());
        runtime.close_pool(&old).unwrap();
        until(
            || match runtime.forget_pool(&old) {
                Ok(()) => true,
                Err(error) if error.code == EmbeddedErrorCode::Busy => false,
                Err(error) => panic!("old pool retirement failed: {error}"),
            },
            "old pool did not finish retirement",
        );
        runtime
            .capabilities()
            .register(vec![CapabilityRegistrationRequest {
                descriptor: super::super::super::capabilities::descriptor(
                    "test.unused_context",
                    CapabilityExecution::Native,
                ),
                native: Some(Arc::new(|_| {
                    panic!("context test unexpectedly ran a host callback")
                })),
            }])
            .unwrap();
        // The replacement changes both package identity and the captured capability membership.
        // 替换同时改变包身份及捕获的能力成员身份。
        let mut replacement = original;
        replacement.generation = "replacement-generation".into();
        replacement.security_partition = "replacement-partition".into();
        // Register replacement authority without mutating the first operation's context.
        // 注册替换权威，不修改首个操作上下文。
        let new = runtime
            .register_pool(
                replacement,
                pool_policy(InstanceReuse::Reusable),
                permissions(),
                "replacement-revision".into(),
            )
            .unwrap();
        // Admit a separate operation against the replacement pool.
        // 针对替换池接纳独立操作。
        let second_operation = runtime.submit(call(&new, Value::Null), OBSERVE).unwrap();
        // Observe the replacement's own durable result and identity.
        // 观测替换代次自身的持久结果及身份。
        let second = second_operation.wait(OBSERVE).unwrap();
        assert_eq!(
            second.phase,
            OperationPhase::Succeeded,
            "{:?}",
            second.error
        );
        assert!(second.host_effects.is_empty());
        assert_eq!(
            module_context(&second).caller.package_generation,
            "replacement-generation"
        );
        assert_eq!(
            module_context(&second).caller.execution_revision,
            "replacement-revision"
        );
        assert_eq!(
            module_context(&second).caller.security_partition,
            "replacement-partition"
        );
        assert_ne!(
            module_context(&second).capability_revision,
            original_membership
        );
        assert_ne!(
            module_context(&first).pool_id,
            module_context(&second).pool_id
        );
        assert_eq!(first_operation.snapshot().unwrap().context, first.context);
        assert_eq!(
            journal
                .get(runtime.id(), first_operation.id())
                .unwrap()
                .unwrap()
                .snapshot
                .context,
            first.context
        );
        shutdown_durable(&runtime, &writer);
        (runtime.id().to_owned(), first, second)
    };
    // Read history after all live registries, module pools and handlers have been destroyed.
    // 在全部活动注册表、模块池及处理器销毁后读取历史。
    let reopened = OperationJournal::open(
        &layout.runtime_root.join("operations.db"),
        journal_config(8, 128 * 1024),
    )
    .unwrap();
    for snapshot in [&first, &second] {
        // Resolve each historical operation using its original stable identity.
        // 使用每个历史操作的原始稳定身份读取记录。
        let restored = reopened
            .get(&runtime_id, &snapshot.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(restored.snapshot.context, snapshot.context);
        assert!(restored.snapshot.host_effects.is_empty());
        assert_eq!(restored.snapshot.phase, OperationPhase::Succeeded);
    }
}

/// Session opening and invocation carry distinct operation identities while retaining one exact pinned session.
/// 会话开启及调用携带不同操作身份，同时保留同一个精确固定会话。
#[test]
fn embedded_operation_context_session_open_and_call_are_durable() {
    // Exercise actual session source initialization and a later export with no host callbacks.
    // 检验真实会话源码初始化及稍后导出调用，二者都没有宿主回调。
    let layout = SystemRuntimeTestLayout::new("embedded durable session context");
    // Keep disk ownership explicit throughout both session operations.
    // 在两个会话操作期间保持明确的磁盘所有权。
    let (journal, writer, runtime) =
        durable_runtime(&layout, pool_config(), journal_config(8, 128 * 1024));
    // The pool's immutable binding supplies both operations' authority.
    // 池的不可变绑定提供两个操作的权威。
    let pool = runtime
        .register_pool(
            definition(&layout, "return {call=function(a) return a end}"),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "session-revision".into(),
        )
        .unwrap();
    // Capture opening identity before its initialization finishes.
    // 在初始化完成前捕获开启身份。
    let opening = runtime.open_session(&pool, OBSERVE).unwrap();
    // The opening snapshot must already retain context without a host callback.
    // 开启快照必须在没有宿主回调时也已保留上下文。
    let opened = opening.operation.wait(OBSERVE).unwrap();
    assert_eq!(
        opened.phase,
        OperationPhase::Succeeded,
        "{:?}",
        opened.error
    );
    assert!(module_context(&opened).export.is_none());
    assert_eq!(
        module_context(&opened).caller.session_id.as_deref(),
        Some(opening.session_id.as_str())
    );
    // A forged session value remains business data while the formal call retains the admitted session.
    // 伪造会话值仍是业务数据，正式调用保留入场会话。
    let invoked = runtime
        .submit_session(
            &opening.session_id,
            "call".into(),
            json!({"session_id":"forged"}),
            crate::LuaInvocationContext::default(),
            OBSERVE,
        )
        .unwrap()
        .wait(OBSERVE)
        .unwrap();
    assert_eq!(
        invoked.phase,
        OperationPhase::Succeeded,
        "{:?}",
        invoked.error
    );
    assert_eq!(module_context(&invoked).export.as_deref(), Some("call"));
    assert_eq!(
        module_context(&invoked).caller.session_id,
        module_context(&opened).caller.session_id
    );
    assert_ne!(
        module_context(&invoked).caller.operation_id,
        module_context(&opened).caller.operation_id
    );
    for snapshot in [&opened, &invoked] {
        assert_eq!(module_context(snapshot).pool_id, pool);
        assert!(snapshot.host_effects.is_empty());
        assert_eq!(
            journal
                .get(runtime.id(), &snapshot.operation_id)
                .unwrap()
                .unwrap()
                .snapshot
                .context,
            snapshot.context
        );
    }
    shutdown_durable(&runtime, &writer);
}

/// Context retention refusal publishes no operation or VM and executes no package initialization.
/// 上下文保留拒绝不发布操作或 VM，也不执行包初始化。
#[test]
fn embedded_operation_context_budget_rejects_before_initialization() {
    // Give dynamic context insufficient metadata capacity without changing any execution or queue budget.
    // 仅使动态上下文元数据容量不足，不更改执行或队列预算。
    let layout = SystemRuntimeTestLayout::new("embedded context budget");
    // Change only the existing per-operation metadata authority.
    // 仅更改既有逐操作元数据权威。
    let mut config = pool_config();
    config.max_effect_bytes_per_operation = 1;
    // Use the formal scheduler so rejected context cannot bypass normal admission.
    // 使用正式调度器，使被拒上下文无法绕过正常入场。
    let runtime = runtime(&layout, config);
    // A visible source effect proves that rejection precedes VM initialization.
    // 可见源码副作用证明拒绝早于 VM 初始化。
    let pool = runtime.register_pool(definition(&layout, "local f=assert(io.open('unexpected-context-start','w')); f:write('ran'); f:close(); return {call=function() return 1 end}"), pool_policy(InstanceReuse::Reusable), permissions(), "revision".into()).unwrap();
    // Compare actual public accounting before and after the rejected admission.
    // 比较被拒入场前后的真实公开记账。
    let before = serde_json::to_value(runtime.usage().unwrap()).unwrap();
    // Retain the admission error without ever publishing an operation handle.
    // 保留入场错误，始终不发布操作句柄。
    let rejected = runtime
        .submit(call(&pool, Value::Null), OBSERVE)
        .err()
        .unwrap();
    assert_eq!(rejected.code, EmbeddedErrorCode::CapacityExceeded);
    assert_eq!(
        serde_json::to_value(runtime.usage().unwrap()).unwrap(),
        before
    );
    assert!(
        !layout
            .package_root
            .join("unexpected-context-start")
            .exists()
    );
    shutdown(&runtime);
}
