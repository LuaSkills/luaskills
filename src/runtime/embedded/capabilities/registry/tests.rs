use super::*;
use crate::runtime::embedded::OperationRegistry;
use crate::runtime::embedded::tests::config;
use serde_json::json;
use std::time::Duration;

/// A request exposed in operation evidence must not be reused when registration closes before queue publication.
/// 请求在操作证据中暴露后，即使注册在队列发布前关闭也不得复用其身份。
#[test]
fn embedded_effect_failed_queue_publication_never_reuses_observed_request_identity() {
    // Prepare through the production admission path and stop at the exact publication boundary.
    // 通过生产入场路径准备，并停在精确发布边界。
    let limits = config();
    let registry = CapabilityRegistry::new("runtime".into(), limits.clone()).unwrap();
    let operations = OperationRegistry::new("runtime".into(), &limits).unwrap();
    let control = Arc::new(CallControl::new(Duration::from_secs(10)).unwrap());
    let (operation, _owner) = operations.admit(Arc::clone(&control)).unwrap();
    let descriptor = CapabilityDescriptor {
        name: "test.queued".into(),
        version: "1.0.0".into(),
        description: "Queued identity probe".into(),
        input_schema: json!(true),
        output_schema: json!(true),
        execution: CapabilityExecution::Queued,
        permissions: BTreeSet::new(),
        scope: CapabilityScope::Invocation,
        max_concurrent: 1,
        max_call_ms: 5000,
        max_input_bytes: 1024,
        max_output_bytes: 1024,
        effects: CapabilityEffects::ReadOnly,
        idempotency: CapabilityIdempotency::None,
    };
    let id = registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor.clone(),
            native: None,
        }])
        .unwrap()
        .remove(0);
    let caller = CapabilityCaller {
        runtime_id: "runtime".into(),
        plugin_id: "plugin".into(),
        package_generation: "generation".into(),
        execution_revision: "revision".into(),
        security_partition: "partition".into(),
        operation_id: operation.snapshot().unwrap().operation_id,
        session_id: None,
        workspace_root: None,
    };
    let grants = CapabilityPermissions::new(BTreeSet::new()).unwrap();
    let prepared = registry
        .snapshot()
        .unwrap()
        .prepare(
            "test.queued",
            caller.clone(),
            Arc::clone(&grants),
            Value::Null,
            Arc::clone(&control),
            CapabilityExecution::Queued,
        )
        .unwrap();
    registry.unregister(&id).unwrap();
    assert_eq!(
        registry.broker.submit(prepared).err().unwrap().code,
        EmbeddedErrorCode::Closed
    );
    let failed = operation
        .snapshot()
        .unwrap()
        .host_effects
        .into_iter()
        .find(|effect| effect.registration_id == id)
        .unwrap();
    let exposed_id = failed
        .request_id
        .expect("the publication failure occurs after request identity attachment");
    assert_eq!(failed.effects, EffectState::NotStarted);
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor,
            native: None,
        }])
        .unwrap();
    let next = registry
        .snapshot()
        .unwrap()
        .submit_queued("test.queued", caller, grants, Value::Null, control)
        .unwrap();
    assert_ne!(next.id(), exposed_id);
    next.cancel().unwrap();
}
