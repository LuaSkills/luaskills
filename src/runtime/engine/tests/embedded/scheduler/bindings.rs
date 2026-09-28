//! Explicit pool bindings preserve validated callback membership and reject foreign registry owners.
//! 显式池绑定保留已校验回调成员，并拒绝外来注册表所有者。

use super::*;

/// Capture runtime's actual registry authority with original grants; return its immutable binding.
/// 使用原授权捕获 runtime 的真实注册表权威；返回其不可变绑定。
fn captured(runtime: &EmbeddedRuntime) -> ModuleCapabilities {
    ModuleCapabilities::new(
        runtime.capabilities().snapshot().unwrap(),
        permissions(),
        "captured".into(),
    )
    .unwrap()
}

/// Actual registration uses the checked snapshot even if callbacks change before publication.
/// 即使回调在发布前变化，实际注册仍使用已检查快照。
#[test]
fn embedded_scheduler_exact_binding_rejects_foreign_registry_and_never_rereads_membership() {
    // One native parent owns all real pools and capacity accounting in this scenario.
    // 单个原生父级拥有此场景的所有真实池及容量记账。
    let layout = SystemRuntimeTestLayout::new("embedded scheduler exact callback binding");
    let runtime = runtime(&layout, pool_config());
    // An independent registry deliberately reuses the public namespace string.
    // 独立注册表刻意复用公开命名空间字符串。
    let impostor = CapabilityRegistry::new(runtime.id().to_owned(), pool_config()).unwrap();
    let foreign = ModuleCapabilities::new(
        impostor.snapshot().unwrap(),
        permissions(),
        "foreign".into(),
    )
    .unwrap();
    let source = "return {call=function() return vulcan.capabilities.call('test.version',{}) end}";
    let policy = pool_policy(InstanceReuse::Reusable);
    assert_eq!(
        runtime
            .register_pool_with_binding(
                definition(&layout, source),
                policy.clone(),
                foreign,
                None,
                None,
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::PermissionDenied
    );
    assert_eq!(
        runtime.plugin(&layout.package_id).unwrap().retained_pools,
        0
    );
    assert_eq!(runtime.resources().unwrap().resident, 0);

    // The same explicit binding API supports exact capacity members without adding independent guarantees.
    // 同一显式绑定接口支持精确容量成员，不额外添加独立保证。
    let capacity = EmbeddedCapacityConfig {
        resources: VmCapacityConfig {
            kind: policy.kind,
            min_resident_vms: 0,
            max_resident_vms: policy.max_resident_vms,
            max_running_calls: policy.max_running_calls,
        },
        max_queued_calls: policy.max_queued_calls,
        max_queued_bytes: pool_config().max_queued_bytes,
    };
    let capacity_id = runtime
        .register_capacity(&layout.package_id, capacity)
        .unwrap();
    // Capture an old native callback before unregistering and replacing its exact public name.
    // 在注销并替换精确公开名称之前捕获旧原生回调。
    let old_id = runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.version",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(|_| CapabilityOutcome {
                result: Ok(json!("old")),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap()
        .pop()
        .unwrap();
    let old_binding = captured(&runtime);
    runtime.capabilities().unregister(&old_id).unwrap();
    runtime
        .capabilities()
        .register(vec![CapabilityRegistrationRequest {
            descriptor: super::super::capabilities::descriptor(
                "test.version",
                CapabilityExecution::Native,
            ),
            native: Some(Arc::new(|_| CapabilityOutcome {
                result: Ok(json!("new")),
                effects: EffectState::NotApplicable,
            })),
        }])
        .unwrap();
    // A supplied snapshot cannot silently become the replacement merely because registration happens later.
    // 不能仅因为注册较晚，就将提供的快照静默变成替代实现。
    let old_pool = runtime
        .register_pool_with_binding(
            definition(&layout, source),
            policy.clone(),
            old_binding,
            None,
            Some(&capacity_id),
        )
        .unwrap();
    let new_pool = runtime
        .register_pool_with_binding(
            definition(&layout, source),
            policy,
            captured(&runtime),
            None,
            Some(&capacity_id),
        )
        .unwrap();
    for (pool, expected) in [
        (&old_pool, json!({"code":"closed"})),
        (&new_pool, json!("new")),
    ] {
        // Core results prove which captured callback was actually selected by the VM.
        // 核心结果证明 VM 实际选择了哪个捕获回调。
        let result = runtime
            .submit(call(pool, Value::Null), Duration::from_secs(3))
            .unwrap()
            .wait(Duration::from_secs(3))
            .unwrap();
        assert_eq!(
            result.phase,
            OperationPhase::Succeeded,
            "{:?}",
            result.error
        );
        let value = result.value.unwrap();
        if pool == &old_pool {
            assert_eq!(value["error"]["code"], expected["code"]);
        } else {
            assert_eq!(value["value"], expected);
        }
    }
    assert_eq!(runtime.capacity(&capacity_id).unwrap().retained_pools, 2);
    shutdown(&runtime);
}
