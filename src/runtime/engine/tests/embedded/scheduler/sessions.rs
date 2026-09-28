use super::*;
mod finalization;

/// Open the exact session pool and require completed initialization before returning its identity.
/// 打开精确会话池，要求初始化完成后才返回其身份。
pub(super) fn opened(runtime: &EmbeddedRuntime, pool: &str) -> String {
    let opening = runtime.open_session(pool, Duration::from_secs(5)).unwrap();
    let result = opening.operation.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(result.phase, OperationPhase::Succeeded, "{result:?}");
    assert_eq!(
        runtime.session(&opening.session_id).unwrap().phase,
        EmbeddedSessionPhase::Ready
    );
    opening.session_id
}

/// Submit one real session export with fixture context and a bounded execution deadline.
/// 使用夹具上下文和有界执行截止时间提交一个真实会话导出。
pub(super) fn session_call(
    runtime: &EmbeddedRuntime,
    session: &str,
    arguments: Value,
) -> OperationHandle {
    runtime
        .submit_session(
            session,
            "call".into(),
            arguments,
            LuaInvocationContext::default(),
            Duration::from_secs(5),
        )
        .unwrap()
}

/// Wait only for observed session closure, never infer it from a close request or operation result.
/// 仅等待观察到的会话关闭，绝不从关闭请求或操作结果推断关闭。
pub(super) fn closed_session(runtime: &EmbeddedRuntime, session: &str) -> EmbeddedSessionSnapshot {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = runtime.session(session).unwrap();
        if snapshot.phase == EmbeddedSessionPhase::Closed {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "session ownership must drain: {snapshot:?}"
        );
        std::thread::yield_now();
    }
}

/// Fixed state survives calls, idle VMs retain capacity, and forgotten session identities never alias replacements.
/// 固定状态跨调用保留，空闲 VM 保留容量，遗忘会话身份绝不指向替代会话。
#[test]
fn embedded_session_pins_state_and_bounds_retained_identities() {
    let layout = SystemRuntimeTestLayout::new("embedded pinned state");
    let mut config = pool_config();
    config.max_resident_vms = 2;
    config.max_sessions = 2;
    let runtime = runtime(&layout, config);
    let mut policy = pool_policy(InstanceReuse::Session);
    policy.max_resident_vms = 2;
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "local n=0; return {call=function() n=n+1; return n end}",
            ),
            policy,
            permissions(),
            "r1".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&pool, Value::Null), Duration::from_secs(1))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
    let first = opened(&runtime, &pool);
    let second = opened(&runtime, &pool);
    for expected in 1..=2 {
        assert_eq!(
            session_call(&runtime, &first, Value::Null)
                .wait(Duration::from_secs(3))
                .unwrap()
                .value,
            Some(json!(expected))
        );
    }
    assert_eq!(
        session_call(&runtime, &second, Value::Null)
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(1))
    );
    assert_eq!(runtime.resources().unwrap().resident, 2);
    assert_eq!(runtime.resources().unwrap().running, 0);
    assert_eq!(
        runtime
            .open_session(&pool, Duration::from_secs(1))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        runtime.forget_session(&first).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    runtime.close_session(&first).unwrap();
    closed_session(&runtime, &first);
    assert_eq!(
        runtime
            .open_session(&pool, Duration::from_secs(1))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    runtime.forget_session(&first).unwrap();
    let replacement = opened(&runtime, &pool);
    assert_ne!(replacement, first);
    assert_eq!(
        runtime.session(&first).unwrap_err().code,
        EmbeddedErrorCode::NotFound
    );
    assert_eq!(
        session_call(&runtime, &replacement, Value::Null)
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(1))
    );
    shutdown(&runtime);
    closed_session(&runtime, &second);
    closed_session(&runtime, &replacement);
}

/// One blocked session remains FIFO while another session progresses; cancelling a queued call preserves its VM.
/// 一个阻塞会话保持先进先出，另一个会话仍能推进；取消排队调用保留其 VM。
#[test]
fn embedded_session_serializes_its_calls_without_blocking_other_sessions() {
    let layout = SystemRuntimeTestLayout::new("embedded session ordering");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let pool = runtime.register_pool(definition(&layout,
        "local n=0; return {call=function(a) if a then local r=vulcan.host.call('test.wait',a); if not r.ok then error(r.error.message) end end; n=n+1; return n end}"),
        pool_policy(InstanceReuse::Session), permissions(), "r1".into()).unwrap();
    let first = opened(&runtime, &pool);
    let second = opened(&runtime, &pool);
    let running = session_call(&runtime, &first, json!(true));
    let request = host_request(&runtime);
    let queued = session_call(&runtime, &first, json!(false));
    let cancelled = session_call(&runtime, &first, json!(false));
    cancelled.cancel().unwrap();
    assert_eq!(
        cancelled.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert_eq!(
        session_call(&runtime, &second, json!(false))
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(1))
    );
    assert_eq!(queued.snapshot().unwrap().phase, OperationPhase::Queued);
    assert_eq!(request.caller.session_id.as_deref(), Some(first.as_str()));
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
        .unwrap();
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!(1))
    );
    assert_eq!(
        queued.wait(Duration::from_secs(3)).unwrap().value,
        Some(json!(2))
    );
    assert_eq!(
        runtime.session(&first).unwrap().phase,
        EmbeddedSessionPhase::Ready
    );
    shutdown(&runtime);
}

/// Session-scoped capabilities receive the host identity during both initialization and invocation.
/// 会话作用域能力在初始化和调用期间均接收宿主身份。
#[test]
fn embedded_session_binds_identity_before_source_initialization() {
    let layout = SystemRuntimeTestLayout::new("embedded session init identity");
    let runtime = runtime(&layout, pool_config());
    let mut descriptor =
        super::super::capabilities::descriptor("test.identity", CapabilityExecution::Native);
    descriptor.scope = CapabilityScope::Session;
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: Some(Arc::new(|call| CapabilityOutcome {
                result: Ok(json!(call.caller.session_id)),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap();
    let pool = runtime.register_pool(definition(&layout,
        "local initial=vulcan.host.call('test.identity',{}); assert(initial.ok); return {call=function(a) local r=vulcan.host.call('test.identity',a); assert(r.ok); return {initial=initial.value,current=r.value} end}"),
        pool_policy(InstanceReuse::Session), permissions(), "r1".into()).unwrap();
    let session = opened(&runtime, &pool);
    let result = session_call(&runtime, &session, json!({"session_id":"forged"}))
        .wait(Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        result.value,
        Some(json!({"initial":session,"current":session}))
    );
    shutdown(&runtime);
}

/// Close keeps real capacity and committed effects until an already dispatched host request acknowledges completion.
/// 已分发宿主请求确认完成前，关闭保留真实容量与已提交副作用。
#[test]
fn embedded_session_close_waits_for_actual_host_acknowledgement() {
    let layout = SystemRuntimeTestLayout::new("embedded session close acknowledgement");
    let mut config = pool_config();
    config.max_resident_vms = 1;
    config.max_running_calls = 1;
    let runtime = runtime(&layout, config);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let mut policy = pool_policy(InstanceReuse::Session);
    policy.max_resident_vms = 1;
    policy.max_running_calls = 1;
    let pool = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function(a) return vulcan.host.call('test.wait',a) end}",
            ),
            policy,
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let session = opened(&runtime, &pool);
    let running = session_call(&runtime, &session, Value::Null);
    let request = host_request(&runtime);
    let queued = session_call(&runtime, &session, Value::Null);
    runtime.close_session(&session).unwrap();
    assert_eq!(
        queued
            .wait(Duration::from_secs(3))
            .unwrap()
            .error
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(
        runtime.session(&session).unwrap().phase,
        EmbeddedSessionPhase::Closing
    );
    assert!(!running.snapshot().unwrap().phase.is_terminal());
    assert_eq!(runtime.resources().unwrap().resident, 1);
    assert_eq!(
        runtime
            .open_session(&pool, Duration::from_secs(1))
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        runtime.forget_session(&session).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    fs::write(
        layout.package_root.join("committed-session-effect"),
        b"committed",
    )
    .unwrap();
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!("written")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let result = running.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(result.phase, OperationPhase::Cancelled);
    assert!(
        result
            .host_effects
            .iter()
            .any(|effect| effect.effects == EffectState::Committed)
    );
    closed_session(&runtime, &session);
    assert_eq!(runtime.resources().unwrap().resident, 0);
    shutdown(&runtime);
}

/// Cancelling an undispatched open releases its reservation without ever evaluating its module source.
/// 取消尚未分发的打开请求会释放预留，且绝不执行模块源码。
#[test]
fn embedded_session_cancelled_queued_open_never_initializes() {
    let layout = SystemRuntimeTestLayout::new("embedded session queued open");
    let mut config = pool_config();
    config.max_running_calls = 1;
    let runtime = runtime(&layout, config);
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let mut ordinary_policy = pool_policy(InstanceReuse::Reusable);
    ordinary_policy.max_running_calls = 1;
    let ordinary = runtime
        .register_pool(
            definition(
                &layout,
                "return {call=function() return vulcan.host.call('test.wait',{}) end}",
            ),
            ordinary_policy,
            permissions(),
            "ordinary".into(),
        )
        .unwrap();
    let running = runtime
        .submit(call(&ordinary, Value::Null), Duration::from_secs(5))
        .unwrap();
    let request = host_request(&runtime);
    let mut session_policy = pool_policy(InstanceReuse::Session);
    session_policy.max_running_calls = 1;
    let pool = runtime.register_pool(definition(&layout,
        "local f=assert(io.open('session-initialized','w')); f:write('yes'); f:close(); return {call=function() return 1 end}"),
        session_policy, permissions(), "session".into()).unwrap();
    let opening = runtime.open_session(&pool, Duration::from_secs(5)).unwrap();
    assert_eq!(runtime.resources().unwrap().resident, 2);
    opening.operation.cancel().unwrap();
    assert_eq!(
        opening
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Cancelled
    );
    closed_session(&runtime, &opening.session_id);
    assert_eq!(runtime.resources().unwrap().resident, 1);
    assert!(!layout.package_root.join("session-initialized").exists());
    runtime.forget_session(&opening.session_id).unwrap();
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
        .unwrap();
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Succeeded
    );
    shutdown(&runtime);
}

/// A failed invocation retires the pinned VM and queued work cannot run on another instance.
/// 失败调用退役固定 VM，排队任务不能在另一实例上运行。
#[test]
fn embedded_session_failure_rejects_queued_work_without_migration() {
    let layout = SystemRuntimeTestLayout::new("embedded failed session");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let pool = runtime.register_pool(definition(&layout,
        "return {call=function(a) if a then vulcan.host.call('test.wait',{}); error('expected failure') end; local f=assert(io.open('unexpected-call','w')); f:close(); return 1 end}"),
        pool_policy(InstanceReuse::Session), permissions(), "r1".into()).unwrap();
    let session = opened(&runtime, &pool);
    let running = session_call(&runtime, &session, json!(true));
    let request = host_request(&runtime);
    let queued = session_call(&runtime, &session, json!(false));
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
        .unwrap();
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Failed
    );
    assert_eq!(
        queued
            .wait(Duration::from_secs(3))
            .unwrap()
            .error
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    assert!(closed_session(&runtime, &session).error.is_some());
    assert!(!layout.package_root.join("unexpected-call").exists());
    shutdown(&runtime);
}

/// Reaching a configured successful-use limit retires a session instead of silently resetting its state.
/// 达到配置的成功使用上限时退役会话，不静默重置状态。
#[test]
fn embedded_session_use_limit_retires_exact_state() {
    let layout = SystemRuntimeTestLayout::new("embedded session use limit");
    let runtime = runtime(&layout, pool_config());
    let mut policy = pool_policy(InstanceReuse::Session);
    policy.max_uses = Some(1);
    let pool = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 17 end}"),
            policy,
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let session = opened(&runtime, &pool);
    assert_eq!(
        session_call(&runtime, &session, Value::Null)
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(17))
    );
    assert!(closed_session(&runtime, &session).error.is_none());
    assert_eq!(
        runtime
            .submit_session(
                &session,
                "call".into(),
                Value::Null,
                LuaInvocationContext::default(),
                Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    shutdown(&runtime);
}

/// Idle close waits for the real Lua finalizer while unrelated execution and control remain available.
/// 空闲关闭等待真实 Lua 终结器，同时无关执行与控制仍可用。
#[test]
fn embedded_session_idle_close_waits_for_actual_vm_retirement() {
    let layout = SystemRuntimeTestLayout::new("embedded idle session finalizer");
    let runtime = runtime(&layout, pool_config());
    let release = FinalizerRelease(layout.package_root.join("session-finalizer-release"));
    let source = r#"
        local open, clock = io.open, os.clock
        local proxy = newproxy(true)
        getmetatable(proxy).__gc = function()
            local f=assert(open('session-finalizer-entered','w')); f:write('yes'); f:close()
            local deadline=clock()+5
            repeat
                local ok, release=pcall(open,'session-finalizer-release','r')
                if ok and release then release:close(); break end
            until clock() >= deadline
            local done=assert(open('session-finalizer-finished','w')); done:write('yes'); done:close()
        end
        return {call=function() return proxy ~= nil end}
    "#;
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let session = opened(&runtime, &pool);
    runtime.close_session(&session).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !layout
        .package_root
        .join("session-finalizer-entered")
        .exists()
    {
        assert!(
            Instant::now() < deadline,
            "real session finalizer must enter"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        runtime.session(&session).unwrap().phase,
        EmbeddedSessionPhase::Closing
    );
    assert_eq!(
        runtime.forget_session(&session).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(runtime.resources().unwrap().resident, 1);
    let ordinary = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 23 end}"),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "other".into(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .submit(call(&ordinary, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!(23))
    );
    runtime.request_close().unwrap();
    assert!(!runtime.poll_closed().unwrap());
    drop(release);
    closed_session(&runtime, &session);
    assert_eq!(
        fs::read_to_string(layout.package_root.join("session-finalizer-finished")).unwrap(),
        "yes"
    );
    shutdown(&runtime);
}

/// Closing an old generation preserves its exact session identity and cannot move calls to a replacement pool.
/// 关闭旧代次保留其精确会话身份，不能把调用迁移到替代池。
#[test]
fn embedded_session_generation_replacement_requires_explicit_new_session() {
    let layout = SystemRuntimeTestLayout::new("embedded session generation");
    let runtime = runtime(&layout, pool_config());
    let old = runtime
        .register_pool(
            definition(&layout, "return {call=function() return 'old' end}"),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let old_session = opened(&runtime, &old);
    let mut next_definition = definition(&layout, "return {call=function() return 'new' end}");
    next_definition.generation = "generation-new".into();
    let next = runtime
        .register_pool(
            next_definition,
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r2".into(),
        )
        .unwrap();
    let new_session = opened(&runtime, &next);
    runtime.close_pool(&old).unwrap();
    assert_eq!(
        runtime
            .submit_session(
                &old_session,
                "call".into(),
                Value::Null,
                LuaInvocationContext::default(),
                Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    closed_session(&runtime, &old_session);
    assert_eq!(
        runtime.forget_pool(&old).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        session_call(&runtime, &new_session, Value::Null)
            .wait(Duration::from_secs(3))
            .unwrap()
            .value,
        Some(json!("new"))
    );
    assert_eq!(runtime.session(&old_session).unwrap().pool_id, old);
    runtime.forget_session(&old_session).unwrap();
    runtime.forget_pool(&old).unwrap();
    assert_eq!(
        runtime.session(&old_session).unwrap_err().code,
        EmbeddedErrorCode::NotFound
    );
    shutdown(&runtime);
}

/// Initialization failure is never replayed and cannot leave a ready session or release unconfirmed VM ownership.
/// 初始化失败绝不重放，不能留下就绪会话或释放尚未确认的 VM 所有权。
#[test]
fn embedded_session_failed_initialization_is_not_retried() {
    let layout = SystemRuntimeTestLayout::new("embedded session initialization failure");
    let runtime = runtime(&layout, pool_config());
    let pool = runtime.register_pool(definition(&layout,
        "local f=assert(io.open('initialization-attempts','a')); f:write('attempt'); f:close(); error('initialization failed')"),
        pool_policy(InstanceReuse::Session), permissions(), "r1".into()).unwrap();
    let opening = runtime.open_session(&pool, Duration::from_secs(5)).unwrap();
    assert_eq!(
        opening
            .operation
            .wait(Duration::from_secs(3))
            .unwrap()
            .phase,
        OperationPhase::Failed
    );
    assert_eq!(runtime.resources().unwrap().resident, 0);
    assert!(
        closed_session(&runtime, &opening.session_id)
            .error
            .is_some()
    );
    assert_eq!(
        fs::read_to_string(layout.package_root.join("initialization-attempts")).unwrap(),
        "attempt"
    );
    shutdown(&runtime);
}

/// A failed VM closes session admission before its blocked finalizer finishes, so queued work can be rejected promptly.
/// 失败 VM 在阻塞终结器结束前关闭会话入场，使排队任务能及时拒绝。
#[test]
fn embedded_session_failure_closes_admission_before_retirement_finishes() {
    let layout = SystemRuntimeTestLayout::new("embedded session failing finalizer");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.wait",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let release = FinalizerRelease(layout.package_root.join("failed-session-release"));
    let source = r#"
        local open, clock = io.open, os.clock
        local proxy = newproxy(true)
        getmetatable(proxy).__gc = function()
            local f=assert(open('failed-session-entered','w')); f:close()
            local deadline=clock()+5
            repeat
                local ok, release=pcall(open,'failed-session-release','r')
                if ok and release then release:close(); break end
            until clock() >= deadline
        end
        return {call=function(a)
            assert(proxy ~= nil)
            if a then vulcan.host.call('test.wait',{}); error('call failed') end
            return 'must not run'
        end}
    "#;
    let pool = runtime
        .register_pool(
            definition(&layout, source),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let session = opened(&runtime, &pool);
    let running = session_call(&runtime, &session, json!(true));
    let request = host_request(&runtime);
    let queued = session_call(&runtime, &session, json!(false));
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
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !layout.package_root.join("failed-session-entered").exists() {
        assert!(Instant::now() < deadline, "failed VM finalizer must enter");
        std::thread::yield_now();
    }
    assert_eq!(
        queued
            .wait(Duration::from_secs(3))
            .unwrap()
            .error
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(
        runtime.session(&session).unwrap().phase,
        EmbeddedSessionPhase::Closing
    );
    assert_eq!(running.snapshot().unwrap().phase, OperationPhase::Cleaning);
    assert_eq!(runtime.resources().unwrap().resident, 1);
    assert_eq!(
        runtime
            .submit_session(
                &session,
                "call".into(),
                Value::Null,
                LuaInvocationContext::default(),
                Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Closed
    );
    drop(release);
    assert_eq!(
        running.wait(Duration::from_secs(3)).unwrap().phase,
        OperationPhase::Failed
    );
    closed_session(&runtime, &session);
    shutdown(&runtime);
}
