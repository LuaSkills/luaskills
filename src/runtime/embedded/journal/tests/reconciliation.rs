//! Verify final host attestations against real SQLite history and uncertain transactions.
//! 对照真实 SQLite 历史及不确定事务验证最终宿主证明。

use super::*;
use crate::runtime::embedded::{ModuleOperationContext, capabilities::CapabilityCaller};

/// Build original bound history with one unfinished unknown host effect; return immutable fixture evidence.
/// 构造包含一个未完成未知宿主副作用的原绑定历史；返回不可变夹具证据。
fn original() -> OperationSnapshot {
    // One authority supplies both operation admission and its original callback.
    // 同一权威同时提供操作入场及其原回调身份。
    let caller = CapabilityCaller {
        runtime_id: "runtime".into(),
        operation_id: "operation".into(),
        plugin_id: "plugin".into(),
        package_generation: "old-package".into(),
        execution_revision: "old-policy".into(),
        security_partition: "partition".into(),
        session_id: None,
        workspace_root: None,
    };
    // Retain all original fields so final evidence cannot manufacture a terminal result.
    // 保留全部原字段，防止最终证明制造终态结果。
    let mut original = snapshot("operation");
    original.context = OperationContext::Module(Box::new(ModuleOperationContext {
        finalization_instance_id: None,
        pool_id: "old-pool".into(),
        caller: caller.clone(),
        capability_revision: "old-capabilities".into(),
        export: Some("run".into()),
    }));
    original.host_effects.push(HostEffectRecord {
        caller,
        effect_id: "effect".into(),
        registration_id: "old-registration".into(),
        capability_name: "test.write".into(),
        capability_version: "1.0.0".into(),
        request_id: Some("old-request".into()),
        phase: HostEffectPhase::Running,
        effects: EffectState::Unknown,
    });
    original
}

/// Return complete trusted fixture evidence for `original`; no business operation is executed by this helper.
/// 返回 `original` 的完整可信夹具证据；本辅助函数不执行业务操作。
fn resolution(original: &OperationSnapshot) -> OperationReconciliation {
    OperationReconciliation {
        resolution_id: "resolution".into(),
        resolver: "host-auditor".into(),
        evidence: "audit:owner-stopped-and-effects-verified".into(),
        execution: if original.phase.is_terminal() {
            ReconciledExecution::ObservedTerminal
        } else {
            ReconciledExecution::StoppedWithoutResult
        },
        effects: ResolvedEffectState::Committed,
        host_effects: original
            .host_effects
            .iter()
            .map(|effect| HostEffectReconciliation {
                effect_id: effect.effect_id.clone(),
                effects: ResolvedEffectState::Committed,
                evidence: "transaction:original-committed".into(),
            })
            .collect(),
    }
}

/// Terminal and interrupted history stay unchanged through finalization, reopen and exact revision removal.
/// 终态及中断历史经最终对账、重新打开和精确修订删除时保持原快照不变。
#[test]
fn embedded_journal_reconciliation_preserves_original_and_is_final() {
    for terminal in [false, true] {
        // Use separate real databases for the two original execution states.
        // 为两种原执行状态使用独立真实数据库。
        let directory = Directory::new();
        // Only the original observed terminal branch contains a business result.
        // 仅原已观测终态分支包含业务结果。
        let mut original = original();
        if terminal {
            original.phase = OperationPhase::Succeeded;
            original.value = Some(Value::Null);
        }
        // Preserve complete original bytes for comparison after every administrative mutation.
        // 保留完整原始字节，用于每次管理变更后的比较。
        let original_bytes = serde_json::to_vec(&original).unwrap();
        // The host attestation has a stable identity and exact per-effect evidence.
        // 宿主证明具有稳定身份及精确逐副作用证据。
        let resolution = resolution(&original);
        // This owner performs all first mutations before a separate reopen.
        // 此所有者在独立重新打开前执行全部首次变更。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        journal.insert("runtime", &original).unwrap();
        assert_eq!(
            journal.forget("runtime", "operation", 1).unwrap_err().code,
            EmbeddedErrorCode::Busy
        );
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 1, &resolution)
                .unwrap(),
            2
        );
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 1, &resolution)
                .unwrap(),
            2
        );
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 2, &resolution)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::AlreadyCompleted
        );
        assert_eq!(
            journal.replace("runtime", 2, &original).unwrap_err().code,
            EmbeddedErrorCode::AlreadyCompleted
        );
        assert_eq!(
            journal
                .checkpoint("runtime", Some(2), &original)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::AlreadyCompleted
        );
        assert_eq!(
            journal
                .checkpoint("runtime", Some(1), &original)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::StaleGeneration
        );
        assert_eq!(
            journal.forget("runtime", "operation", 1).unwrap_err().code,
            EmbeddedErrorCode::StaleGeneration
        );
        drop(journal);
        // Reopen without a plugin or callback registry and retain original namespace and effect identities.
        // 无插件或回调注册表地重新打开，保留原命名空间及副作用身份。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        // Read both independent observations and the new final audit evidence.
        // 同时读取独立原观测及新增最终审计证据。
        let record = journal.get("runtime", "operation").unwrap().unwrap();
        assert_eq!(
            serde_json::to_vec(&record.snapshot).unwrap(),
            original_bytes
        );
        assert_eq!(record.reconciliation, Some(resolution));
        assert_eq!(record.revision, 2);
        assert!(journal.get("new-runtime", "operation").unwrap().is_none());
        journal
            .forget("runtime", "operation", record.revision)
            .unwrap();
        assert!(journal.get("runtime", "operation").unwrap().is_none());
    }
}

/// Missing, duplicate, contradictory and oversized evidence must leave the original row and revision untouched.
/// 缺失、重复、矛盾及超大证据必须使原行及修订保持不变。
#[test]
fn embedded_journal_reconciliation_rejects_incomplete_evidence() {
    for damage in [
        "id",
        "resolver",
        "evidence",
        "closure",
        "missing",
        "extra",
        "foreign",
        "effect-evidence",
        "known-effect",
        "known-aggregate",
        "aggregate",
        "duplicate",
        "budget",
    ] {
        // Each rejected candidate begins with an independently durable original.
        // 每个被拒候选从独立持久原始记录开始。
        let directory = Directory::new();
        // Only known-outcome and duplicate branches adjust the original fixture.
        // 仅已知结果及重复分支调整原始夹具。
        let mut original = original();
        if damage == "known-effect" {
            original.host_effects.first_mut().unwrap().effects = EffectState::RolledBack;
        }
        if damage == "known-aggregate" {
            original.effects = EffectState::RolledBack;
        }
        if damage == "duplicate" {
            original
                .host_effects
                .push(original.host_effects.first().unwrap().clone());
        }
        // Change exactly the designated proof requirement, preserving the other fields.
        // 仅改变指定证明要求，保留其他字段。
        let mut resolution = resolution(&original);
        match damage {
            "id" => resolution.resolution_id = " ".into(),
            "resolver" => resolution.resolver.clear(),
            "evidence" => resolution.evidence.clear(),
            "closure" => resolution.execution = ReconciledExecution::ObservedTerminal,
            "missing" => resolution.host_effects.clear(),
            "extra" => resolution
                .host_effects
                .push(resolution.host_effects.first().unwrap().clone()),
            "foreign" => {
                resolution.host_effects.first_mut().unwrap().effect_id = "different".into()
            }
            "effect-evidence" => resolution
                .host_effects
                .first_mut()
                .unwrap()
                .evidence
                .clear(),
            "aggregate" => resolution.effects = ResolvedEffectState::NotApplicable,
            "budget" => resolution.evidence = "x".repeat(config().max_record_bytes),
            "known-effect" | "known-aggregate" | "duplicate" => {}
            _ => unreachable!(),
        }
        // No rejected proof may consume a revision or weaken the retention guard.
        // 任何被拒证明均不得消耗修订或弱化保留门禁。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        // Compare the whole durable record, not only its error response.
        // 比较整个持久记录，而非仅检查错误响应。
        let before = serde_json::to_value(journal.insert("runtime", &original).unwrap()).unwrap();
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 1, &resolution)
                .unwrap_err()
                .code,
            if damage == "budget" {
                EmbeddedErrorCode::CapacityExceeded
            } else {
                EmbeddedErrorCode::InvalidArgument
            },
            "{damage}"
        );
        assert_eq!(
            serde_json::to_value(journal.get("runtime", "operation").unwrap().unwrap()).unwrap(),
            before,
            "{damage}"
        );
        assert_eq!(
            journal.forget("runtime", "operation", 1).unwrap_err().code,
            EmbeddedErrorCode::Busy
        );
    }
}

/// The original snapshot and final proof share one record budget, even when each fits independently.
/// 原始快照与最终证明共享单记录预算，即使各自单独可以容纳。
#[test]
fn embedded_journal_reconciliation_combined_budget_is_atomic() {
    // The expected complete successor defines an exact boundary without copying a production limit.
    // 预期完整后继定义精确边界，不复制生产上限。
    let original = original();
    // Keep a complete proof whose own encoded size is below the total record size.
    // 保留完整证明，其自身编码大小低于总记录大小。
    let resolution = resolution(&original);
    // Compose only a test expectation, not a production mutation path.
    // 仅组合测试预期，不作为生产变更路径。
    let successor = JournalOperation {
        runtime_id: "runtime".into(),
        revision: 2,
        snapshot: original.clone(),
        reconciliation: Some(resolution.clone()),
    };
    // Set the boundary one byte below the complete candidate.
    // 将边界设为比完整候选少一个字节。
    let mut limits = config();
    limits.max_record_bytes = serde_json::to_vec(&successor).unwrap().len() - 1;
    assert!(serde_json::to_vec(&resolution).unwrap().len() < limits.max_record_bytes);
    // Only the fixture-created database is mutated.
    // 仅变更夹具创建的数据库。
    let directory = Directory::new();
    // Store the smaller original before attempting the combined final record.
    // 尝试组合最终记录前，先存储较小原记录。
    let journal = OperationJournal::open(&directory.database(), limits).unwrap();
    // Full original bytes and revision remain authoritative after refusal.
    // 拒绝后，完整原字节及修订保持权威。
    let before = serde_json::to_value(journal.insert("runtime", &original).unwrap()).unwrap();
    assert_eq!(
        journal
            .reconcile("runtime", "operation", 1, &resolution)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        serde_json::to_value(journal.get("runtime", "operation").unwrap().unwrap()).unwrap(),
        before
    );
}

/// Both actual COMMIT and ROLLBACK with lost acknowledgements recover to one exact final revision.
/// 真实提交及回滚丢失确认后，均恢复到唯一精确最终修订。
#[test]
fn embedded_journal_reconciliation_recovers_uncertain_transaction() {
    for committed in [false, true] {
        // Fault injection operates on a real isolated SQLite transaction.
        // 故障注入作用于真实独立 SQLite 事务。
        let directory = Directory::new();
        // Keep the same journal owner across failed confirmation and recovery.
        // 跨确认失败及恢复保持同一日志所有者。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        // Persist an unfinished execution whose external truth is supplied separately.
        // 持久化未完成执行，其外部事实独立提供。
        let original = original();
        // Retry exactly these original audit bytes and identity.
        // 精确重试这些原始审计字节及身份。
        let resolution = resolution(&original);
        journal.insert("runtime", &original).unwrap();
        journal.lose_next_confirmation_for_test(committed);
        assert!(
            journal
                .reconcile("runtime", "operation", 1, &resolution)
                .is_err()
        );
        assert!(journal.get("runtime", "operation").is_err());
        assert!(journal.recover_storage().unwrap());
        // The actual recovered row distinguishes committed from rolled-back proof.
        // 实际恢复行区分已提交及已回滚证明。
        let recovered = journal.get("runtime", "operation").unwrap().unwrap();
        assert_eq!(recovered.reconciliation.is_some(), committed);
        assert_eq!(recovered.revision, if committed { 2 } else { 1 });
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 1, &resolution)
                .unwrap(),
            2
        );
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 1, &resolution)
                .unwrap(),
            2
        );
        // Reusing only the resolution identity cannot acknowledge different evidence.
        // 仅复用对账身份不能确认不同证据。
        let mut different = resolution.clone();
        different.evidence = "different-audit".into();
        assert_eq!(
            journal
                .reconcile("runtime", "operation", 1, &different)
                .unwrap_err()
                .code,
            EmbeddedErrorCode::StaleGeneration
        );
        assert_eq!(
            serde_json::to_value(
                journal
                    .get("runtime", "operation")
                    .unwrap()
                    .unwrap()
                    .snapshot
            )
            .unwrap(),
            serde_json::to_value(original).unwrap()
        );
    }
}

/// Concurrent identical retries share one commit, while competing final evidence has exactly one winner.
/// 并发相同重试共享一次提交，竞争最终证据则恰有一个胜者。
#[test]
fn embedded_journal_reconciliation_concurrent_cas() {
    for identical in [false, true] {
        // Both threads share the sole real database owner and synchronize before entering the transaction.
        // 两线程共享唯一真实数据库所有者，并在进入事务前同步。
        let directory = Directory::new();
        // Journal locking and SQLite transaction ownership jointly serialize the CAS.
        // 日志锁及 SQLite 事务所有权共同串行化比较交换。
        let journal = Arc::new(OperationJournal::open(&directory.database(), config()).unwrap());
        journal.insert("runtime", &original()).unwrap();
        // The main thread releases both contenders together.
        // 主线程同时释放两个竞争者。
        let barrier = Arc::new(Barrier::new(3));
        // Preserve join handles to observe both actual outcomes before dropping storage.
        // 保留连接句柄，在释放存储前观测两个实际结果。
        let workers = (0..2)
            .map(|index| {
                // Share the original durable authority, never duplicate a database connection owner.
                // 共享原持久权威，绝不重复数据库连接所有者。
                let journal = Arc::clone(&journal);
                // Synchronize these requests rather than relying on sleeps.
                // 同步请求，不依赖睡眠。
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Only competing branches deliberately choose distinct final audit identities.
                    // 仅竞争分支有意选择不同最终审计身份。
                    let mut resolution = resolution(&original());
                    if !identical {
                        resolution.resolution_id = format!("resolution-{index}");
                    }
                    barrier.wait();
                    journal.reconcile("runtime", "operation", 1, &resolution)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        // Observe completion of every thread before asserting shared state.
        // 断言共享状态前观测每个线程完成。
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            if identical { 2 } else { 1 }
        );
        assert!(
            outcomes
                .iter()
                .filter_map(|outcome| outcome.as_ref().err())
                .all(|error| error.code == EmbeddedErrorCode::StaleGeneration)
        );
        assert_eq!(
            journal
                .get("runtime", "operation")
                .unwrap()
                .unwrap()
                .revision,
            2
        );
    }
}

/// Recomputed checksums cannot hide missing or contradictory audit fields, and old format three stays untouched.
/// 重算摘要不能掩盖缺失或矛盾审计字段，旧第三版格式保持原样。
#[test]
fn embedded_journal_reconciliation_rejects_corrupt_or_legacy_history() {
    for damage in ["missing", "closure", "evidence", "revision", "legacy-three"] {
        // Only this isolated test database is intentionally damaged after its owner closes.
        // 仅在所有者关闭后有意损坏此独立测试数据库。
        let directory = Directory::new();
        // Obtain the real encoded final record before corrupting one semantic requirement.
        // 在损坏单个语义要求前取得真实编码最终记录。
        let mut encoded = {
            // Release this exclusive owner before opening the fault-injection connection.
            // 打开故障注入连接前释放此独占所有者。
            let journal = OperationJournal::open(&directory.database(), config()).unwrap();
            journal.insert("runtime", &original()).unwrap();
            journal
                .reconcile("runtime", "operation", 1, &resolution(&original()))
                .unwrap();
            serde_json::to_value(journal.get("runtime", "operation").unwrap().unwrap()).unwrap()
        };
        match damage {
            "missing" | "legacy-three" => {
                encoded.as_object_mut().unwrap().remove("reconciliation");
            }
            "closure" => encoded["reconciliation"]["execution"] = json!("observed_terminal"),
            "evidence" => encoded["reconciliation"]["evidence"] = json!(""),
            "revision" => encoded["revision"] = json!(1),
            _ => unreachable!(),
        }
        {
            // Recompute a valid checksum, requiring semantic decoding to reject the corruption.
            // 重算有效摘要，要求语义解码拒绝损坏。
            let bytes = serde_json::to_vec(&encoded).unwrap();
            // This raw connection intentionally bypasses production validation in the isolated fixture.
            // 此原始连接在独立夹具内有意绕过生产校验。
            let connection = Connection::open(directory.database()).unwrap();
            connection
                .execute(
                    "UPDATE operations SET revision=?1,document=?2,digest=?3",
                    params![
                        encoded["revision"].as_i64().unwrap(),
                        bytes,
                        Sha256::digest(&bytes).as_slice()
                    ],
                )
                .unwrap();
            if damage == "legacy-three" {
                connection.pragma_update(None, "user_version", 3).unwrap();
            }
        }
        // Refusal must preserve every original disk byte, not silently migrate unpublished formats.
        // 拒绝必须保留每个原始磁盘字节，不能静默迁移未发布格式。
        let before = std::fs::read(directory.database()).unwrap();
        assert_eq!(
            OperationJournal::open(&directory.database(), config())
                .err()
                .unwrap()
                .code,
            if damage == "legacy-three" {
                EmbeddedErrorCode::Unsupported
            } else {
                EmbeddedErrorCode::Internal
            },
            "{damage}"
        );
        assert_eq!(std::fs::read(directory.database()).unwrap(), before);
    }
}
