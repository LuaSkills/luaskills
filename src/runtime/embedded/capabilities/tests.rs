use super::*;
use crate::runtime::embedded::tests::config;
use crate::runtime::embedded::{CallControl, EffectState, EmbeddedErrorCode};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod delivery;
mod effects;
mod finalization;
mod lifetime;
mod queued;

/// Build a real native capability with explicit permission and value contracts.
/// 构造具有显式权限与值契约的真实原生能力。
fn descriptor(name: &str) -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: name.into(),
        version: "1.0.0".into(),
        description: "Test host capability".into(),
        input_schema: json!(true),
        output_schema: json!(true),
        execution: CapabilityExecution::Native,
        permissions: BTreeSet::from(["test.read".into()]),
        scope: CapabilityScope::Invocation,
        max_concurrent: 1,
        max_call_ms: 5000,
        max_input_bytes: 1024,
        max_output_bytes: 1024,
        effects: CapabilityEffects::ReadOnly,
        idempotency: CapabilityIdempotency::None,
    }
}

/// Return independently allocated live grants for each isolated test caller.
/// 为各个隔离测试调用方返回独立分配的实时授权。
fn grants() -> Arc<CapabilityPermissions> {
    CapabilityPermissions::new(BTreeSet::from(["test.read".into()])).unwrap()
}

/// Construct a fixed host-authenticated caller with an operation identity.
/// 构造具有操作身份的固定宿主认证调用方。
fn caller() -> CapabilityCaller {
    CapabilityCaller {
        runtime_id: "runtime-a".into(),
        plugin_id: "trusted-plugin".into(),
        package_generation: "package-a".into(),
        execution_revision: "execution-a".into(),
        security_partition: "workspace-a".into(),
        operation_id: "operation-a".into(),
        session_id: None,
        workspace_root: None,
    }
}

/// Return a generous finite parent deadline for behavior tests.
/// 返回用于行为测试的较宽裕有限父级截止时间。
fn control() -> Arc<CallControl> {
    Arc::new(CallControl::new(Duration::from_secs(10)).unwrap())
}

/// Wrap a read-only native response without inventing a transaction commit.
/// 包装只读原生响应，不编造事务提交。
fn value(value: Value) -> CapabilityOutcome {
    CapabilityOutcome {
        result: Ok(value),
        effects: EffectState::NotApplicable,
    }
}

/// Register exact `name` and `callback`, returning the stable opaque registration identity.
/// 注册精确 `name` 与 `callback`，返回稳定不透明注册身份。
fn register(registry: &CapabilityRegistry, name: &str, callback: NativeCapability) -> String {
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: descriptor(name),
            native: Some(callback),
        }])
        .unwrap()
        .remove(0)
}

/// Publication is all-or-nothing, and snapshots cannot silently discover or redirect later implementations.
/// 发布保持全有或全无，快照不能静默发现或重定向到后续实现。
#[test]
fn embedded_capabilities_publish_atomic_snapshots_and_exact_replacements() {
    // Each registry owns its implementation map rather than a process-wide callback slot.
    // 每个注册表拥有自身实现映射，而非进程级回调槽。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    // Empty snapshot remains empty even after registrations are published.
    // 注册发布后，空快照仍为空。
    let empty = registry.snapshot().unwrap();
    // Duplicate batch must not partially expose its first entry.
    // 重复批次不能部分暴露其首个条目。
    let duplicate = registry.register(vec![
        CapabilityRegistrationRequest {
            descriptor: descriptor("test.echo"),
            native: Some(Arc::new(|_| value(json!(1)))),
        },
        CapabilityRegistrationRequest {
            descriptor: descriptor("test.echo"),
            native: Some(Arc::new(|_| value(json!(2)))),
        },
    ]);
    assert_eq!(duplicate.unwrap_err().code, EmbeddedErrorCode::Busy);
    assert!(
        registry
            .snapshot()
            .unwrap()
            .list(&grants())
            .unwrap()
            .is_empty()
    );
    // First exact identity will be retired while its immutable snapshot remains alive.
    // 第一个精确身份将在其不可变快照仍存活时被退役。
    let old_id = register(&registry, "test.echo", Arc::new(|_| value(json!("old"))));
    let old = registry.snapshot().unwrap();
    assert!(empty.list(&grants()).unwrap().is_empty());
    assert_eq!(
        old.invoke_native("test.echo", caller(), grants(), Value::Null, control())
            .unwrap()
            .result
            .unwrap(),
        json!("old")
    );
    registry.unregister(&old_id).unwrap();
    assert_ne!(old.revision(), registry.snapshot().unwrap().revision());
    // The same name receives a new identity; old unregister calls must not affect it.
    // 同名能力获得新身份；旧注销调用不得影响新能力。
    let new_id = register(&registry, "test.echo", Arc::new(|_| value(json!("new"))));
    assert_ne!(old_id, new_id);
    registry.unregister(&old_id).unwrap();
    assert_eq!(
        old.invoke_native("test.echo", caller(), grants(), Value::Null, control())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .invoke_native("test.echo", caller(), grants(), Value::Null, control())
            .unwrap()
            .result
            .unwrap(),
        json!("new")
    );
    registry.forget(&old_id).unwrap();
    assert_eq!(
        registry.status(&old_id).unwrap_err().code,
        EmbeddedErrorCode::NotFound
    );
}

/// Authority comes from the host caller, and revoking permissions affects existing snapshots immediately.
/// 权威来自宿主调用方，撤销权限立即影响既有快照。
#[test]
fn embedded_capabilities_keep_identity_and_revocation_outside_arguments() {
    // Immutable snapshot contains the real callback but no implicit permission grant.
    // 不可变快照包含真实回调，但不隐式授予权限。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    register(
        &registry,
        "test.identity",
        Arc::new(|call| {
            value(json!({"owner":call.caller.plugin_id,"argument":call.arguments["plugin_id"]}))
        }),
    );
    // Frozen registration membership still consults live permissions.
    // 冻结的注册成员仍检查实时权限。
    let snapshot = registry.snapshot().unwrap();
    let permissions = grants();
    assert_eq!(
        snapshot
            .invoke_native(
                "test.identity",
                caller(),
                Arc::clone(&permissions),
                json!({"plugin_id":"forged"}),
                control()
            )
            .unwrap()
            .result
            .unwrap(),
        json!({"owner":"trusted-plugin","argument":"forged"})
    );
    permissions.revoke("test.read").unwrap();
    assert!(snapshot.list(&permissions).unwrap().is_empty());
    assert_eq!(
        snapshot
            .invoke_native(
                "test.identity",
                caller(),
                permissions,
                Value::Null,
                control()
            )
            .unwrap_err()
            .code,
        EmbeddedErrorCode::PermissionDenied
    );
    // A trusted caller from another runtime cannot reuse this registry's snapshot.
    // 来自其他运行时的可信调用方不能复用此注册表快照。
    let mut wrong = caller();
    wrong.runtime_id = "runtime-b".into();
    assert_eq!(
        snapshot
            .invoke_native("test.identity", wrong, grants(), Value::Null, control())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
}

/// Unregistration retains the real executing callback and reports busy until its final clone is gone.
/// 注销保留真实执行中的回调，直到最后一个克隆消失前报告忙碌。
#[test]
fn embedded_capabilities_unregister_drains_real_callback_ownership() {
    // Channel barriers expose actual execution rather than counting mocked calls.
    // 通道屏障暴露真实执行，而非统计模拟调用。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    // Native callbacks require Sync; the one receiver remains exclusively consumed.
    // 原生回调要求 Sync；单个接收器仍被独占消费。
    let release_rx = Mutex::new(release_rx);
    let callback: NativeCapability = Arc::new(move |_| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        value(json!("finished"))
    });
    // Weak ownership proves when every core callback reference is actually released.
    // 弱所有权证明核心全部回调引用实际释放的时刻。
    let weak = Arc::downgrade(&callback);
    let id = register(&registry, "test.block", callback);
    let snapshot = registry.snapshot().unwrap();
    let worker_snapshot = snapshot.clone();
    let worker = std::thread::spawn(move || {
        worker_snapshot.invoke_native("test.block", caller(), grants(), Value::Null, control())
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        snapshot
            .invoke_native("test.block", caller(), grants(), Value::Null, control())
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    let status = registry.unregister(&id).unwrap();
    assert!(!status.accepting);
    assert_eq!(status.in_flight, 1);
    assert!(!status.drained);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        registry.forget(&id).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap().result.unwrap(),
        json!("finished")
    );
    assert!(registry.status(&id).unwrap().drained);
    assert!(weak.upgrade().is_none());
}

/// Cancellation and oversized responses must preserve committed side-effect evidence.
/// 取消与超大响应必须保留已提交副作用证据。
#[test]
fn embedded_capabilities_preserve_effects_after_cancellation_and_invalid_output() {
    // A mutating capability explicitly reports its durable commit.
    // 有副作用能力显式报告其持久提交。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let cancelled = control();
    let callback_control = Arc::clone(&cancelled);
    let mut contract = descriptor("test.commit");
    contract.effects = CapabilityEffects::Mutating;
    contract.max_output_bytes = 4;
    registry
        .register(vec![CapabilityRegistrationRequest {
            descriptor: contract,
            native: Some(Arc::new(move |_| {
                callback_control.cancel();
                CapabilityOutcome {
                    result: Ok(json!("too large")),
                    effects: EffectState::Committed,
                }
            })),
        }])
        .unwrap();
    let outcome = registry
        .snapshot()
        .unwrap()
        .invoke_native("test.commit", caller(), grants(), Value::Null, cancelled)
        .unwrap();
    assert_eq!(
        outcome.result.unwrap_err().code,
        EmbeddedErrorCode::Cancelled
    );
    assert_eq!(outcome.effects, EffectState::Committed);
}

/// Native recursive calls fail explicitly without deadlocking or corrupting the next independent call.
/// 原生递归调用明确失败，不死锁且不破坏后续独立调用。
#[test]
fn embedded_capabilities_reject_synchronous_reentry_and_recover_after_panic() {
    // Weak capture avoids making the registry own itself through its callback.
    // 弱捕获避免注册表通过回调拥有自身。
    let registry = CapabilityRegistry::new("runtime-a".into(), config()).unwrap();
    let weak = Arc::downgrade(&registry);
    register(
        &registry,
        "test.reentry",
        Arc::new(move |_| {
            let result = weak.upgrade().unwrap().snapshot().unwrap().invoke_native(
                "test.reentry",
                caller(),
                grants(),
                Value::Null,
                control(),
            );
            value(json!(result.unwrap_err().code))
        }),
    );
    register(
        &registry,
        "test.panic",
        Arc::new(|_| panic!("private callback panic")),
    );
    let snapshot = registry.snapshot().unwrap();
    for _ in 0..2 {
        assert_eq!(
            snapshot
                .invoke_native("test.reentry", caller(), grants(), Value::Null, control())
                .unwrap()
                .result
                .unwrap(),
            json!("busy")
        );
        assert_eq!(
            snapshot
                .invoke_native("test.panic", caller(), grants(), Value::Null, control())
                .unwrap()
                .result
                .unwrap_err()
                .message,
            "native capability panicked"
        );
    }
}
