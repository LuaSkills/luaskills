//! Real capability admission must respect the operation's already-frozen module identity.
//! 真实能力入场必须遵守操作已经冻结的模块身份。

use super::*;
use crate::runtime::embedded::capabilities::*;
use crate::runtime::embedded::tests::config;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Reusing an exact operation ID with another module's authority fails before invoking native host code.
/// 复用精确操作 ID 却携带另一模块权威时，在调用原生宿主代码前失败。
#[test]
fn embedded_operation_context_rejects_foreign_callback_before_execution() {
    // The real operation and capability registries share an explicitly selected test namespace.
    // 真实操作及能力注册表共享明确选择的测试命名空间。
    let limits = config();
    // Retain real operation metadata using the same configured limits as capability admission.
    // 使用与能力入场相同的配置上限保留真实操作元数据。
    let operations = OperationRegistry::new("runtime".into(), &limits).unwrap();
    // This registry performs actual descriptor validation and native admission.
    // 此注册表执行真实描述校验及原生入场。
    let capabilities = CapabilityRegistry::new("runtime".into(), limits.clone()).unwrap();
    // Count actual callback entry independently from returned errors or retained records.
    // 独立于返回错误或保留记录，统计真实回调进入次数。
    let calls = Arc::new(AtomicUsize::new(0));
    // Share only the execution counter with the native callback.
    // 仅与原生回调共享执行计数器。
    let called = Arc::clone(&calls);
    // This bounded test deadline supplies both operation and descriptor timing.
    // 此有界测试截止时长同时提供操作及描述的时间配置。
    let timeout = Duration::from_secs(5);
    capabilities
        .register(vec![CapabilityRegistrationRequest {
            descriptor: CapabilityDescriptor {
                name: "test.bound".into(),
                version: "1.0.0".into(),
                description: "Bound caller test".into(),
                input_schema: json!(true),
                output_schema: json!(true),
                execution: CapabilityExecution::Native,
                permissions: BTreeSet::new(),
                scope: CapabilityScope::Invocation,
                max_concurrent: 1,
                max_call_ms: timeout.as_millis().try_into().unwrap(),
                max_input_bytes: limits.max_value_bytes,
                max_output_bytes: limits.max_value_bytes,
                effects: CapabilityEffects::ReadOnly,
                idempotency: CapabilityIdempotency::None,
            },
            native: Some(Arc::new(move |_| {
                called.fetch_add(1, Ordering::SeqCst);
                CapabilityOutcome {
                    result: Ok(Value::Null),
                    effects: EffectState::NotApplicable,
                }
            })),
        }])
        .unwrap();
    // Freeze the same actual membership that will resolve this capability.
    // 冻结用于解析此能力的同一实际成员快照。
    let snapshot = capabilities.snapshot().unwrap();
    // One original deadline and cancellation identity belongs to the admitted operation.
    // 单个原始截止时间及取消身份归属于入场操作。
    let control = Arc::new(CallControl::new(timeout).unwrap());
    // The private factory models the scheduler's pre-publication binding without executing plugin source.
    // 私有构造器模拟调度器发布前绑定，不执行插件源码。
    let (operation, mut owner) = operations
        .admit_context(Arc::clone(&control), |id| {
            Ok(OperationContext::Module(Box::new(ModuleOperationContext {
                finalization_instance_id: None,
                pool_id: "pool".into(),
                capability_revision: snapshot.revision(),
                export: Some("call".into()),
                caller: CapabilityCaller {
                    runtime_id: "runtime".into(),
                    operation_id: id.into(),
                    plugin_id: "plugin".into(),
                    package_generation: "generation".into(),
                    execution_revision: "revision".into(),
                    security_partition: "partition".into(),
                    session_id: None,
                    workspace_root: None,
                },
            })))
        })
        .unwrap();
    owner.advance(OperationPhase::Initializing).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    // Read admitted identity through the public operation snapshot, not a parallel fixture authority.
    // 通过公开操作快照读取入场身份，不另设并行夹具权威。
    let caller = operation
        .snapshot()
        .unwrap()
        .context
        .caller()
        .unwrap()
        .clone();
    // This read-only fixture capability deliberately requires no extra grants.
    // 此只读夹具能力明确不要求额外授权。
    let permissions = CapabilityPermissions::new(BTreeSet::new()).unwrap();
    // Change each module-bound field while deliberately keeping the runtime and operation IDs valid.
    // 有意保持运行时及操作 ID 有效，同时逐项变更模块绑定字段。
    for field in [
        "plugin",
        "generation",
        "revision",
        "partition",
        "session",
        "workspace",
    ] {
        // Preserve valid operation ownership while substituting one unauthorized module field.
        // 保留有效操作归属，同时替换一个未授权模块字段。
        let mut foreign = caller.clone();
        match field {
            "plugin" => foreign.plugin_id = "foreign".into(),
            "generation" => foreign.package_generation = "foreign".into(),
            "revision" => foreign.execution_revision = "foreign".into(),
            "partition" => foreign.security_partition = "foreign".into(),
            "session" => foreign.session_id = Some("foreign".into()),
            "workspace" => foreign.workspace_root = Some("D:/foreign".into()),
            _ => unreachable!(),
        }
        // A real dispatch attempt must fail before native entry and before publishing any effect record.
        // 真实分发尝试必须在原生进入及发布任何副作用记录前失败。
        let rejected = snapshot
            .invoke_native(
                "test.bound",
                foreign,
                Arc::clone(&permissions),
                Value::Null,
                Arc::clone(&control),
            )
            .unwrap_err();
        assert_eq!(rejected.code, EmbeddedErrorCode::InvalidArgument, "{field}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(operation.snapshot().unwrap().host_effects.is_empty());
    }
    snapshot
        .invoke_native(
            "test.bound",
            caller.clone(),
            permissions,
            Value::Null,
            control,
        )
        .unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotApplicable)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        operation
            .snapshot()
            .unwrap()
            .host_effects
            .iter()
            .all(|effect| effect.caller == caller)
    );
}
