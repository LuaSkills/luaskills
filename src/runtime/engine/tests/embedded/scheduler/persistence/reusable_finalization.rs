//! Real reusable-instance finalization and bounded discovery across all lifecycle triggers.
//! 跨全部生命周期触发条件的真实可复用实例关闭及有界发现。

use super::super::finalization::closing_definition;
use super::*;

/// Runtime close cancels reusable business work but keeps its independent closing callback alive until acknowledgement.
/// 运行时关闭取消可复用业务任务，但保持独立关闭回调存活直到确认。
#[test]
fn embedded_reusable_finalization_runtime_close_waits_for_host_ack() {
    let layout = SystemRuntimeTestLayout::new("reusable closing host acknowledgement");
    let runtime = runtime(&layout, pool_config());
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::super::capabilities::descriptor(
                "test.close",
                CapabilityExecution::Queued,
            ),
            native: None,
        }])
        .unwrap();
    let source = "local host=vulcan.host.call; return {call=function() local r=host('test.close','business'); assert(r.ok); return r.value end, shutdown=function() local r=host('test.close','closing'); assert(r.ok); return r.value end}";
    let pool = runtime
        .register_pool(
            closing_definition(&layout, source, 5000),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let business = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
    let request = host_request(&runtime);
    runtime.request_close().unwrap();
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &request.request_id,
            CapabilityOutcome {
                result: Ok(json!("ack")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let closing_request = host_request(&runtime);
    assert_eq!(closing_request.arguments, json!("closing"));
    assert!(closing_request.caller.session_id.is_none());
    let closing = closing_operation(&runtime, &pool, business.id());
    assert_eq!(closing_request.caller.operation_id, closing.id());
    assert_ne!(closing.id(), business.id());
    assert_eq!(
        business.wait(OBSERVE).unwrap().phase,
        OperationPhase::Cancelled
    );
    assert!(!runtime.poll_closed().unwrap());
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
    runtime
        .capabilities()
        .host_requests()
        .complete(
            &closing_request.request_id,
            CapabilityOutcome {
                result: Ok(json!("closed")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    let result = closing.wait(OBSERVE).unwrap();
    assert_eq!(result.phase, OperationPhase::Succeeded);
    assert_eq!(result.host_effects.len(), 1);
    assert_eq!(
        result
            .finalization
            .unwrap()
            .outcome
            .unwrap()
            .result()
            .unwrap(),
        json!("closed")
    );
    shutdown(&runtime);
}

/// Idle expiry, pressure, and each owning scope close the original idle VM exactly once.
/// 空闲过期、容量压力及各拥有作用域均精确关闭原空闲 VM 一次。
#[test]
fn embedded_reusable_finalization_idle_pressure_and_scope_closure() {
    for trigger in ["idle", "pressure", "pool", "plugin", "runtime"] {
        let layout = SystemRuntimeTestLayout::new("reusable closing lifecycle triggers");
        let mut config = pool_config();
        config.max_resident_vms = 1;
        config.max_running_calls = 1;
        let runtime = runtime(&layout, config);
        let mut policy = pool_policy(InstanceReuse::Reusable);
        policy.max_resident_vms = 1;
        policy.max_running_calls = 1;
        if trigger == "idle" {
            policy.idle_ttl_ms = Some(1);
        }
        let source = "local n=0; return {call=function() n=n+1; return n end, shutdown=function() local f=assert(io.open('closing-count','a')); f:write('x'); f:close(); return n end}";
        let pool = runtime
            .register_pool(
                closing_definition(&layout, source, 1000),
                policy.clone(),
                permissions(),
                "r1".into(),
            )
            .unwrap();
        let business = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        assert_eq!(business.wait(OBSERVE).unwrap().value, Some(json!(1)));
        match trigger {
            "idle" => {}
            "pressure" => {
                let other = runtime
                    .register_pool(
                        definition(&layout, "return {call=function() return 23 end}"),
                        policy,
                        permissions(),
                        "r2".into(),
                    )
                    .unwrap();
                let operation = runtime.submit(call(&other, Value::Null), OBSERVE).unwrap();
                assert_eq!(operation.wait(OBSERVE).unwrap().value, Some(json!(23)));
            }
            "pool" => runtime.close_pool(&pool).unwrap(),
            "plugin" => runtime.close_plugin(&layout.package_id).unwrap(),
            "runtime" => runtime.request_close().unwrap(),
            _ => unreachable!(),
        }
        let closing = closing_operation(&runtime, &pool, business.id())
            .wait(OBSERVE)
            .unwrap();
        assert_eq!(
            closing.phase,
            OperationPhase::Succeeded,
            "trigger: {trigger}"
        );
        assert_eq!(
            closing
                .finalization
                .unwrap()
                .outcome
                .unwrap()
                .result()
                .unwrap(),
            json!(1)
        );
        shutdown(&runtime);
        assert_eq!(
            fs::read_to_string(layout.package_root.join("closing-count")).unwrap(),
            "x"
        );
    }
}

/// Disk rollback and lost commit acknowledgement retain the exact VM and never replay closing side effects.
/// 磁盘回滚及提交确认丢失保留精确 VM，且绝不重放关闭副作用。
#[test]
fn embedded_reusable_finalization_durable_recovery_retains_original_instance() {
    for committed in [false, true] {
        let layout = SystemRuntimeTestLayout::new("reusable closing durable recovery");
        let (journal, writer, runtime) =
            durable_runtime(&layout, pool_config(), journal_config(16, 1024 * 1024));
        let release = FinalizerRelease(layout.package_root.join("closing-release"));
        let source = r#"
            -- VM-local state survives the business result and both closing disk failures.
            -- VM 局部状态跨业务结果及两次关闭磁盘失败保留。
            local open, clock = io.open, os.clock
            local n=0
            return {call=function() n=n+1; return n end, shutdown=function()
                assert(n==1)
                local count=assert(open('closing-count','a')); count:write('x'); count:close()
                local entered=assert(open('closing-entered','w')); entered:write('yes'); entered:close()
                -- Release only after the host arms the next real database confirmation failure.
                -- 仅在宿主设置下一次真实数据库确认故障后释放。
                local deadline=clock()+8
                repeat
                    local ok, released=pcall(open,'closing-release','r')
                    if ok and released then released:close(); return n end
                until clock() >= deadline
                error('fixture gate expired')
            end}
        "#;
        let pool = runtime
            .register_pool(
                closing_definition(&layout, source, OBSERVE.as_millis() as u64),
                pool_policy(InstanceReuse::Reusable),
                permissions(),
                "r1".into(),
            )
            .unwrap();
        let business = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        let original = business.wait(OBSERVE).unwrap();
        assert_eq!(original.value, Some(json!(1)));
        journal.lose_next_confirmation_for_test(committed);
        runtime.close_pool(&pool).unwrap();
        let closing = closing_operation(&runtime, &pool, business.id());
        assert_eq!(failure(&runtime, &closing).phase, OperationPhase::Cleaning);
        assert!(!layout.package_root.join("closing-count").exists());
        assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
        runtime.request_close().unwrap();
        assert!(!runtime.poll_closed().unwrap());
        assert!(journal.recover_storage().unwrap());
        assert!(runtime.retry_checkpoint(closing.id()).unwrap());
        until(
            || layout.package_root.join("closing-entered").exists(),
            "closing never resumed after intent recovery",
        );
        journal.lose_next_confirmation_for_test(committed);
        drop(release);
        assert_eq!(failure(&runtime, &closing).phase, OperationPhase::Cleaning);
        assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 1);
        assert!(journal.recover_storage().unwrap());
        assert!(runtime.retry_checkpoint(closing.id()).unwrap());
        let result = closing.wait(OBSERVE).unwrap();
        assert_eq!(result.phase, OperationPhase::Succeeded);
        assert_eq!(
            result
                .finalization
                .unwrap()
                .outcome
                .unwrap()
                .result()
                .unwrap(),
            json!(1)
        );
        assert_eq!(
            serde_json::to_value(business.snapshot().unwrap()).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        assert_eq!(
            fs::read_to_string(layout.package_root.join("closing-count")).unwrap(),
            "x"
        );
        let historical = journal.get(runtime.id(), closing.id()).unwrap().unwrap();
        assert!(
            matches!(&historical.snapshot.context, OperationContext::Module(context) if context.finalization_instance_id.is_some())
        );
        shutdown_durable(&runtime, &writer);
    }
}

/// Discover a new independent closing operation after retained `after`, without sorting opaque IDs.
/// 在保留的 `after` 之后发现新的独立关闭操作，不排序不透明身份。
/// Return its original handle within the fixture budget, preserving native status as the authority.
/// 在夹具预算内返回其原始句柄，保留原生状态作为权威。
fn closing_operation(runtime: &EmbeddedRuntime, pool: &str, after: &str) -> OperationHandle {
    let mut found = None;
    let mut cursor = Some(after.to_owned());
    until(
        || {
            let page = runtime
                .list_operations(Some(pool), cursor.as_deref(), 1)
                .unwrap();
            for id in page.operation_ids {
                let operation = runtime.operation(&id).unwrap();
                let snapshot = operation.snapshot().unwrap();
                if matches!(&snapshot.context, OperationContext::Module(context) if context.finalization_instance_id.is_some())
                {
                    found = Some(operation);
                    return true;
                }
            }
            cursor = page.after_operation_id;
            false
        },
        "independent instance finalization was never published",
    );
    found.expect("observed closing operation retained")
}

/// Same-VM closing publishes after newer business identities and never changes earlier business results.
/// 同 VM 关闭发布在更新业务身份之后，且绝不改变先前业务结果。
#[test]
fn embedded_reusable_finalization_same_vm_and_publication_pagination() {
    let layout = SystemRuntimeTestLayout::new("reusable closing discovery");
    // Page validation derives from this fixture's actual runtime configuration.
    // 分页校验派生自本夹具的实际运行时配置。
    let config = pool_config();
    let maximum = config.max_operations;
    let runtime = runtime(&layout, config);
    let pool = runtime.register_pool(closing_definition(&layout,
        "local n=0; return {call=function() n=n+1; return n end, shutdown=function() return n end}", 1000),
        pool_policy(InstanceReuse::Reusable), permissions(), "r1".into()).unwrap();
    let mut business = Vec::new();
    for expected in 1..=3 {
        let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
        let result = operation.wait(OBSERVE).unwrap();
        assert_eq!(result.value, Some(json!(expected)));
        assert!(result.finalization.is_none());
        business.push((operation, serde_json::to_value(result).unwrap()));
    }
    assert_eq!(
        runtime
            .plugin(&layout.package_id)
            .unwrap()
            .reserved_operations,
        1
    );
    let page = runtime.list_operations(Some(&pool), None, 2).unwrap();
    assert_eq!(
        page.operation_ids,
        business
            .iter()
            .take(2)
            .map(|(operation, _)| operation.id().to_owned())
            .collect::<Vec<_>>()
    );
    assert!(page.has_more);
    runtime.close_pool(&pool).unwrap();
    let closing = closing_operation(&runtime, &pool, business.last().unwrap().0.id());
    let result = closing.wait(OBSERVE).unwrap();
    assert_eq!(result.phase, OperationPhase::Succeeded);
    assert_eq!(result.value, Some(Value::Null));
    let context = match &result.context {
        OperationContext::Module(context) => context,
        _ => panic!("module authority missing"),
    };
    assert!(
        context
            .finalization_instance_id
            .as_ref()
            .unwrap()
            .starts_with("embedded-vm:")
    );
    assert!(context.caller.session_id.is_none());
    assert_eq!(
        result
            .finalization
            .unwrap()
            .outcome
            .unwrap()
            .result()
            .unwrap(),
        json!(3)
    );
    for (operation, previous) in business {
        assert_ne!(operation.id(), closing.id());
        assert_eq!(
            serde_json::to_value(operation.snapshot().unwrap()).unwrap(),
            previous
        );
    }
    runtime.forget_pool(&pool).unwrap();
    // Discovery continues through original context after pool metadata is explicitly forgotten.
    // 池元数据被显式遗忘后，发现仍通过原始上下文继续。
    assert_eq!(
        runtime
            .list_operations(Some(&pool), None, maximum)
            .unwrap()
            .operation_ids
            .len(),
        4
    );
    assert!(runtime.list_operations(None, None, 0).is_err());
    assert!(runtime.list_operations(None, None, maximum + 1).is_err());
    assert!(
        runtime
            .list_operations(Some("another-pool"), Some(closing.id()), 1)
            .is_err()
    );
    runtime.forget_operation(closing.id()).unwrap();
    assert_eq!(
        runtime
            .list_operations(None, Some(closing.id()), 1)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::NotFound
    );
    shutdown(&runtime);
}

/// Closing reservations survive globally or plugin-full retention and reject unreservable initialization.
/// 关闭预留在全局或插件保留额度满时存活，并拒绝无法预留的初始化。
#[test]
fn embedded_reusable_finalization_reserves_capacity_before_initialization() {
    for global in [true, false] {
        for maximum in [1, 2] {
            let layout = SystemRuntimeTestLayout::new("reusable closing reserved capacity");
            let mut config = pool_config();
            config.max_queued_calls = 1;
            config.max_running_calls = 1;
            if global {
                config.max_operations = maximum;
            }
            let mut plugin = plugin_policy(&config);
            plugin.max_operations = maximum;
            let runtime = runtime_with_plugin(&layout, config, plugin);
            let mut policy = pool_policy(InstanceReuse::Reusable);
            policy.max_queued_calls = 1;
            policy.max_running_calls = 1;
            let source = "local f=assert(io.open('initialized','w')); f:write('yes'); f:close(); return {call=function() return 7 end, shutdown=function() return 'closed' end}";
            let pool = runtime
                .register_pool(
                    closing_definition(&layout, source, 1000),
                    policy,
                    permissions(),
                    "r1".into(),
                )
                .unwrap();
            let business = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
            let result = business.wait(OBSERVE).unwrap();
            if maximum == 1 {
                assert_eq!(
                    result.error.unwrap().code,
                    EmbeddedErrorCode::CapacityExceeded
                );
                assert!(!layout.package_root.join("initialized").exists());
                assert_eq!(
                    runtime
                        .plugin(&layout.package_id)
                        .unwrap()
                        .reserved_operations,
                    0
                );
            } else {
                assert_eq!(result.value, Some(json!(7)));
                assert_eq!(
                    runtime
                        .plugin(&layout.package_id)
                        .unwrap()
                        .reserved_operations,
                    1
                );
                assert_eq!(
                    runtime
                        .submit(call(&pool, Value::Null), OBSERVE)
                        .err()
                        .unwrap()
                        .code,
                    EmbeddedErrorCode::CapacityExceeded
                );
                runtime.close_pool(&pool).unwrap();
                assert_eq!(
                    closing_operation(&runtime, &pool, business.id())
                        .wait(OBSERVE)
                        .unwrap()
                        .phase,
                    OperationPhase::Succeeded
                );
                let usage = runtime.plugin(&layout.package_id).unwrap();
                assert_eq!(
                    (usage.retained_operations, usage.reserved_operations),
                    (2, 0)
                );
            }
            shutdown(&runtime);
        }
    }
}

/// Failed initialization releases its unused closing slot and publishes no fabricated closing operation.
/// 失败初始化释放未使用关闭槽，且不发布伪造的关闭操作。
#[test]
fn embedded_reusable_finalization_failed_initialization_releases_reservation() {
    let layout = SystemRuntimeTestLayout::new("reusable closing failed initialization");
    let runtime = runtime(&layout, pool_config());
    let pool = runtime
        .register_pool(
            closing_definition(&layout, "error('initialization failed')", 1000),
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "r1".into(),
        )
        .unwrap();
    let business = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
    assert_eq!(
        business.wait(OBSERVE).unwrap().phase,
        OperationPhase::Failed
    );
    until(
        || {
            runtime
                .plugin(&layout.package_id)
                .unwrap()
                .reserved_operations
                == 0
        },
        "unused closing reservation leaked",
    );
    assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 0);
    // One failed business record is the entire page; no independent closing record may follow.
    // 一个失败业务记录就是完整页面；不得存在后续独立关闭记录。
    let page = runtime.list_operations(Some(&pool), None, 1).unwrap();
    assert_eq!(page.operation_ids, vec![business.id().to_owned()]);
    assert!(!page.has_more);
    shutdown(&runtime);
}

/// Business errors, deadlines, and use exhaustion each close once with independent closing success or failure.
/// 业务错误、截止及使用额度耗尽均关闭一次，关闭独立成功或失败。
#[test]
fn embedded_reusable_finalization_business_and_closing_outcomes_are_independent() {
    for (business_body, business_error, max_uses) in [
        (
            "error('business failed')",
            Some(EmbeddedErrorCode::ExecutionFailed),
            None,
        ),
        (
            "while true do end",
            Some(EmbeddedErrorCode::DeadlineExceeded),
            None,
        ),
        ("return n", None, Some(1)),
    ] {
        for (closing_body, closing_error) in [
            ("return n", None),
            (
                "error('closing failed')",
                Some(EmbeddedErrorCode::ExecutionFailed),
            ),
            (
                "while true do end",
                Some(EmbeddedErrorCode::DeadlineExceeded),
            ),
        ] {
            let layout = SystemRuntimeTestLayout::new("reusable independent closing outcomes");
            let runtime = runtime(&layout, pool_config());
            let source = format!(
                "local n=0; return {{call=function() n=n+1; {business_body} end, shutdown=function() assert(n==1); {closing_body} end}}"
            );
            let mut policy = pool_policy(InstanceReuse::Reusable);
            policy.max_uses = max_uses;
            let pool = runtime
                .register_pool(
                    closing_definition(&layout, &source, 50),
                    policy,
                    permissions(),
                    "r1".into(),
                )
                .unwrap();
            let business = runtime
                .submit(call(&pool, Value::Null), Duration::from_millis(100))
                .unwrap();
            let result = business.wait(OBSERVE).unwrap();
            assert_eq!(
                result.error.as_ref().map(|error| error.code),
                business_error
            );
            let closing = closing_operation(&runtime, &pool, business.id())
                .wait(OBSERVE)
                .unwrap();
            assert_eq!(
                closing.error.as_ref().map(|error| error.code),
                closing_error
            );
            assert_eq!(
                serde_json::to_value(business.snapshot().unwrap()).unwrap(),
                serde_json::to_value(result).unwrap()
            );
            assert_eq!(runtime.pool_resources(&pool).unwrap().resident, 0);
            shutdown(&runtime);
        }
    }
}
