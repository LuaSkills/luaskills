//! Real SQLite acknowledgement loss across operation closing intent and outcome.
//! 操作关闭意图及结果跨真实 SQLite 确认丢失的验证。

use super::*;

/// Recover only retained closing checkpoints and never issue a second execution budget.
/// 只恢复保留的关闭检查点，绝不签发第二个执行预算。
#[test]
fn embedded_operation_finalization_persistence_preserves_original_attempt() {
    for committed in [false, true] {
        // Both transaction rollback and lost post-commit confirmation preserve the original candidate.
        // 事务回滚和提交后确认丢失均保留原候选。
        let directory = Directory::new();
        let journal = directory.journal(journal_config());
        let (writer, registry) = queued_registry(&journal);
        let (handle, mut owner) = admit(&registry);
        owner.advance(OperationPhase::Running).unwrap();
        let business = Ok(json!({"answer":null}));
        owner
            .prepare_finalization("shutdown".into(), business.clone(), Duration::from_secs(10))
            .unwrap();
        assert_eq!(
            owner.take_finalization_control().unwrap_err().code,
            EmbeddedErrorCode::Busy
        );
        journal.lose_next_confirmation_for_test(committed);
        assert!(poll(|| owner.poll_finalization()).is_err());
        assert!(handle.snapshot().unwrap().finalization.is_none());
        assert_eq!(
            owner.take_finalization_control().unwrap_err().code,
            EmbeddedErrorCode::Busy
        );
        assert_eq!(
            owner
                .prepare_finalization(
                    "replacement".into(),
                    Ok(Value::Null),
                    Duration::from_secs(10)
                )
                .unwrap_err()
                .code,
            EmbeddedErrorCode::Busy
        );
        assert!(journal.recover_storage().unwrap());
        assert!(owner.poll_finalization().is_err());
        if !owner.retry_advance().unwrap() {
            poll(|| owner.poll_finalization()).unwrap();
        }
        let control = owner.take_finalization_control().unwrap();
        let original_deadline = control.deadline();
        assert_eq!(
            owner.take_finalization_control().unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        let intent = journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .unwrap();
        assert_eq!(
            intent
                .snapshot
                .finalization
                .as_ref()
                .unwrap()
                .business
                .result(),
            business
        );
        assert!(
            intent
                .snapshot
                .finalization
                .as_ref()
                .unwrap()
                .outcome
                .is_none()
        );
        let closing = Err(EmbeddedError::new(
            EmbeddedErrorCode::ExecutionFailed,
            "closing returned failure",
        ));
        owner.prepare_finalization_outcome(closing.clone()).unwrap();
        journal.lose_next_confirmation_for_test(committed);
        assert!(poll(|| owner.poll_finalization()).is_err());
        assert!(
            handle
                .snapshot()
                .unwrap()
                .finalization
                .unwrap()
                .outcome
                .is_none()
        );
        assert_eq!(
            owner
                .complete(business.clone(), EffectState::Unknown)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::Busy
        );
        assert_eq!(
            owner
                .prepare_finalization_outcome(Ok(Value::Null))
                .unwrap_err()
                .code,
            EmbeddedErrorCode::Busy
        );
        assert!(journal.recover_storage().unwrap());
        assert!(owner.poll_finalization().is_err());
        if !owner.retry_advance().unwrap() {
            poll(|| owner.poll_finalization()).unwrap();
        }
        assert_eq!(control.deadline(), original_deadline);
        assert_eq!(
            owner.take_finalization_control().unwrap_err().code,
            EmbeddedErrorCode::Closed
        );
        let outcome = journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .unwrap();
        assert_eq!(
            outcome
                .snapshot
                .finalization
                .as_ref()
                .unwrap()
                .outcome
                .as_ref()
                .unwrap()
                .result(),
            closing
        );
        assert_eq!(outcome.snapshot.phase, OperationPhase::Cleaning);
        owner
            .complete(business.clone(), EffectState::Unknown)
            .unwrap();
        let terminal = journal
            .get(&registry.runtime_id, handle.id())
            .unwrap()
            .unwrap();
        assert_eq!(
            terminal
                .snapshot
                .finalization
                .as_ref()
                .unwrap()
                .business
                .result(),
            business
        );
        assert_eq!(
            terminal
                .snapshot
                .finalization
                .as_ref()
                .unwrap()
                .outcome
                .as_ref()
                .unwrap()
                .result(),
            closing
        );
        assert_eq!(terminal.snapshot.phase, OperationPhase::Failed);
        // Journal successors cannot erase or replace already observed stage evidence.
        // 日志后继不能抹除或替换已经观察到的阶段证据。
        let mut replaced = terminal.snapshot.clone();
        replaced.finalization.as_mut().unwrap().business = OperationOutcome::Succeeded {
            value: json!("different"),
        };
        assert_eq!(
            journal
                .replace(&registry.runtime_id, terminal.revision, &replaced)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
        replaced = terminal.snapshot.clone();
        replaced.finalization = None;
        assert_eq!(
            journal
                .replace(&registry.runtime_id, terminal.revision, &replaced)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
        close(&writer);
    }
}

/// Reject malformed closing history before persistence and preserve explicit-null stage values.
/// 持久化前拒绝畸形关闭历史，并保留阶段值的显式空值。
#[test]
fn embedded_operation_finalization_history_validates_shapes() {
    let directory = Directory::new();
    let journal = directory.journal(journal_config());
    let (writer, registry) = queued_registry(&journal);
    let (handle, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    owner
        .prepare_finalization("shutdown".into(), Ok(Value::Null), Duration::from_secs(10))
        .unwrap();
    poll(|| owner.poll_finalization()).unwrap();
    owner.take_finalization_control().unwrap();
    owner.prepare_finalization_outcome(Ok(Value::Null)).unwrap();
    poll(|| owner.poll_finalization()).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotApplicable)
        .unwrap();
    let original = journal
        .get(&registry.runtime_id, handle.id())
        .unwrap()
        .unwrap();
    let encoded = serde_json::to_vec(&original.snapshot).unwrap();
    let decoded: OperationSnapshot = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.finalization, original.snapshot.finalization);
    assert_eq!(
        decoded.finalization.as_ref().unwrap().business.result(),
        Ok(Value::Null)
    );
    for mutation in ["missing_outcome", "effect_count", "terminal_value", "phase"] {
        // Each mutation violates a distinct authoritative invariant.
        // 每项变更分别违反一项权威不变量。
        let mut invalid = original.snapshot.clone();
        match mutation {
            "missing_outcome" => invalid.finalization.as_mut().unwrap().outcome = None,
            "effect_count" => invalid.finalization.as_mut().unwrap().business_effect_count = 1,
            "terminal_value" => invalid.value = Some(json!("wrong")),
            "phase" => invalid.phase = OperationPhase::Running,
            _ => unreachable!(),
        }
        assert_eq!(
            journal
                .replace(&registry.runtime_id, original.revision, &invalid)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::InvalidArgument
        );
    }
    close(&writer);
}
