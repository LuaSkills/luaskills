use super::*;
use std::sync::Arc;

/// Create a fresh finite operation budget for journal behavior tests.
/// 为日志行为测试创建新的有限操作预算。
fn control() -> Arc<CallControl> {
    Arc::new(CallControl::new(Duration::from_secs(10)).unwrap())
}

/// Cancellation intent cannot hide live execution or overwrite committed effects.
/// 取消意图不能掩盖活跃执行，也不能覆盖已提交副作用。
#[test]
fn embedded_operation_cancellation_and_effects_remain_separate() {
    // Journal ownership remains independent of the caller's wait handle.
    // 日志所有权独立于调用方等待句柄。
    let registry = OperationRegistry::new("runtime-one".to_owned(), &config()).unwrap();
    // Shared original budget and unique execution owner.
    // 共享的原始预算与唯一执行所有者。
    let (handle, mut owner) = registry.admit(control()).unwrap();
    // Stable identity remains queryable during pending cancellation.
    // 取消待完成期间，稳定身份仍可查询。
    let id = handle.snapshot().unwrap().operation_id;
    owner.advance(OperationPhase::Running).unwrap();
    assert!(handle.cancel().unwrap());
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Running);
    assert!(handle.snapshot().unwrap().cancellation_requested);
    assert_eq!(
        registry.forget(&id).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(
            Err(EmbeddedError::new(
                EmbeddedErrorCode::Cancelled,
                "cancelled after commit",
            )),
            EffectState::Committed,
        )
        .unwrap();
    // The write stays committed even after cooperative cancellation completes.
    // 即使协作取消完成，写入仍保持已提交。
    let snapshot = registry.get(&id).unwrap().snapshot().unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Cancelled);
    assert_eq!(snapshot.effects, EffectState::Committed);
    assert!(!handle.cancel().unwrap());
    assert!(
        owner
            .complete(Ok(serde_json::Value::Null), EffectState::RolledBack)
            .is_err()
    );
    registry.forget(&id).unwrap();
    assert!(registry.get(&id).is_err());
}

/// Caller wait expiration does not cancel work; successful JSON null is retained.
/// 调用方等待过期不会取消工作；成功的 JSON 空值会被保留。
#[test]
fn embedded_operation_wait_timeout_preserves_live_work() {
    // One pending record exercises the nonblocking wait query.
    // 一个待完成记录验证非阻塞等待查询。
    let registry = OperationRegistry::new("wait-runtime".to_owned(), &config()).unwrap();
    // The owner proves completion remains possible after the caller stops waiting.
    // 所有者证明调用方停止等待后仍可完成。
    let (handle, mut owner) = registry.admit(control()).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    assert_eq!(
        handle.wait(Duration::ZERO).unwrap().phase,
        OperationPhase::Running
    );
    assert!(!owner.control().is_cancelled());
    // A separate waiter exercises condition-variable notification.
    // 独立等待者验证条件变量通知。
    let waiter = std::thread::spawn(move || handle.wait(Duration::from_secs(2)).unwrap());
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(serde_json::Value::Null), EffectState::NotApplicable)
        .unwrap();
    // Successful null differs from missing results before completion.
    // 成功的空值不同于完成前的结果缺失。
    let snapshot = waiter.join().unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Succeeded);
    assert_eq!(snapshot.value, Some(serde_json::Value::Null));
}

/// Full retention rejects work and explicit expiration never reuses an old ID.
/// 保留容量已满时拒绝工作，显式过期绝不复用旧 ID。
#[test]
fn embedded_operation_retention_is_bounded_and_ids_are_not_reused() {
    // Smaller valid limits make exhaustion deterministic.
    // 较小的有效上限使耗尽可以确定复现。
    let mut limits = config();
    limits.max_queued_calls = 2;
    limits.max_operations = 2;
    // Explicit namespace keeps each runtime's operation IDs separate.
    // 显式命名空间保持各运行时操作 ID 分离。
    let registry = OperationRegistry::new("retention-runtime".to_owned(), &limits).unwrap();
    // First record will expire only after actual cleanup completes.
    // 第一条记录仅在实际清理完成后过期。
    let (first, mut owner) = registry.admit(control()).unwrap();
    // Unfinished second record must never be silently evicted.
    // 未完成的第二条记录绝不能被静默驱逐。
    let (_second, _second_owner) = registry.admit(control()).unwrap();
    assert!(registry.admit(control()).is_err());
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(serde_json::Value::Null), EffectState::NotStarted)
        .unwrap();
    // Preserve the original opaque identity across retention changes.
    // 跨保留变化保存原始不透明身份。
    let first_id = first.snapshot().unwrap().operation_id;
    registry.forget(&first_id).unwrap();
    // Fresh admission receives a fresh ID rather than replaying a removed record.
    // 新入场得到新 ID，不重放已移除记录。
    let (next, _next_owner) = registry.admit(control()).unwrap();
    assert_ne!(next.snapshot().unwrap().operation_id, first_id);
}

/// Oversized results fail while preserving host-confirmed commit evidence.
/// 超大结果失败，同时保留宿主确认的提交证据。
#[test]
fn embedded_operation_result_bound_preserves_commit_evidence() {
    // Explicit authoritative result limit used by both journal and test input.
    // 日志与测试输入共同使用的显式权威结果上限。
    let limits = config();
    // Result retention is bounded independently of execution ownership.
    // 结果保留独立于执行所有权保持有界。
    let registry = OperationRegistry::new("result-runtime".to_owned(), &limits).unwrap();
    // A rejected early completion retains the owner so it can still finish correctly.
    // 被拒绝的提前完成保留所有者，使其仍可正确完成。
    let (handle, mut owner) = registry.admit(control()).unwrap();
    assert!(
        owner
            .complete(Ok(serde_json::Value::Null), EffectState::NotStarted)
            .is_err()
    );
    owner.advance(OperationPhase::Running).unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(
            Ok(serde_json::Value::String(
                "x".repeat(limits.max_value_bytes + 1),
            )),
            EffectState::Committed,
        )
        .unwrap();
    // Result delivery failure cannot pretend a successful write was rolled back.
    // 结果交付失败不能假装成功写入已回滚。
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.phase, OperationPhase::Failed);
    assert_eq!(snapshot.effects, EffectState::Committed);
    assert_eq!(
        snapshot.error.unwrap().code,
        EmbeddedErrorCode::CapacityExceeded
    );
}

/// Exact-size application values are not rejected because of Rust's internal Result envelope.
/// 不因 Rust 内部 Result 信封而拒绝恰好达到上限的应用值。
#[test]
fn embedded_operation_value_limit_excludes_internal_result_envelope() {
    // JSON true uses exactly four bytes, independently of transport metadata.
    // JSON true 恰好使用四个字节，与传输元数据无关。
    let mut limits = config();
    limits.max_value_bytes = serde_json::to_vec(&true).unwrap().len();
    // The same explicit limit is used for registry admission and completion.
    // 注册表入场与完成使用同一显式上限。
    let registry = OperationRegistry::new("exact-limit".to_owned(), &limits).unwrap();
    // Complete a valid read-only value at the exact declared byte boundary.
    // 在精确声明字节边界完成有效只读值。
    let (handle, mut owner) = registry.admit(control()).unwrap();
    owner.advance(OperationPhase::Running).unwrap();
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(
            Ok(serde_json::Value::Bool(true)),
            EffectState::NotApplicable,
        )
        .unwrap();
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Succeeded);
}
