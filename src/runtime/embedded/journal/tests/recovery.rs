//! Real transaction outcomes with lost confirmations and explicit same-owner recovery.
//! 确认丢失的真实事务结果与显式同所有者恢复。

use super::*;

/// Both insert and replacement reconcile exact committed candidates or retry only rolled-back storage work.
/// 插入及替换均对账精确已提交候选，或仅重试已回滚的存储工作。
#[test]
fn embedded_journal_recovery_reconciles_exact_checkpoint() {
    for committed in [false, true] {
        for previous in [None, Some(1)] {
            // Each case has an independently owned SQLite file and real exclusive connection.
            // 每个场景拥有独立 SQLite 文件及真实独占连接。
            let directory = Directory::new();
            // Preserve the same journal object while replacing only its failed storage connection.
            // 保持同一日志对象，仅替换其故障存储连接。
            let journal = OperationJournal::open(&directory.database(), config()).unwrap();
            assert!(!journal.recover_storage().unwrap());
            // A prior revision is explicitly present only for replacement cases.
            // 仅替换场景明确存在先前修订。
            let original = snapshot("operation");
            if previous.is_some() {
                journal.insert("runtime", &original).unwrap();
            }
            // Preserve explicit null so reconciliation must distinguish it from a missing result.
            // 保留显式空值，使对账必须将其与缺失结果区分。
            let mut candidate = original.clone();
            candidate.phase = OperationPhase::Succeeded;
            candidate.effects = EffectState::NotApplicable;
            candidate.value = Some(Value::Null);
            journal.lose_next_confirmation_for_test(committed);
            assert_eq!(
                journal
                    .checkpoint("runtime", previous, &candidate)
                    .unwrap_err(),
                uncertain()
            );
            assert_eq!(
                journal.get("runtime", "operation").unwrap_err(),
                uncertain()
            );
            assert_eq!(
                journal
                    .checkpoint("runtime", previous, &candidate)
                    .unwrap_err(),
                uncertain()
            );
            assert!(journal.recover_storage().unwrap());
            // Actual recovered state, not the failed reply, identifies the transaction's durable outcome.
            // 实际恢复状态而非失败回执标识事务持久结果。
            let recovered = journal.get("runtime", "operation").unwrap();
            if committed {
                assert_eq!(recovered.unwrap().snapshot.value, Some(Value::Null));
            } else if previous.is_some() {
                assert_eq!(recovered.unwrap().snapshot.phase, OperationPhase::Running);
            } else {
                assert!(recovered.is_none());
            }
            // The exact original retry advances at most once, even when its acknowledgement was lost.
            // 即使确认丢失，精确原重试也至多推进一次。
            let acknowledged = journal.checkpoint("runtime", previous, &candidate).unwrap();
            assert_eq!(acknowledged.revision, previous.unwrap_or(0) + 1);
            assert_eq!(
                journal
                    .checkpoint("runtime", previous, &candidate)
                    .unwrap()
                    .revision,
                acknowledged.revision
            );
            // A missing result is a different candidate and cannot borrow the committed null result.
            // 缺失结果属于不同候选，不能借用已提交空值结果。
            let mut changed = candidate.clone();
            changed.value = None;
            assert_eq!(
                journal
                    .checkpoint("runtime", previous, &changed)
                    .unwrap_err()
                    .code,
                if previous.is_none() {
                    EmbeddedErrorCode::AlreadyCompleted
                } else {
                    EmbeddedErrorCode::StaleGeneration
                }
            );
            assert_eq!(
                journal.insert("runtime", &candidate).unwrap_err().code,
                EmbeddedErrorCode::AlreadyCompleted
            );
            if let Some(previous) = previous {
                assert_eq!(
                    journal
                        .replace("runtime", previous, &candidate)
                        .unwrap_err()
                        .code,
                    EmbeddedErrorCode::StaleGeneration
                );
            }
            drop(journal);
            // A separate host can inspect the same original identity after all live owners leave.
            // 全部活动所有者离开后，独立宿主可检查同一原始身份。
            let reopened = OperationJournal::open(&directory.database(), config()).unwrap();
            // Reopening never assigns this history to a newly selected runtime namespace.
            // 重新打开绝不将此历史分配给新选择的运行时命名空间。
            let retained = reopened.get("runtime", "operation").unwrap().unwrap();
            assert_eq!(retained.revision, acknowledged.revision);
            assert_eq!(retained.snapshot.value, Some(Value::Null));
            assert!(
                reopened
                    .get("different-runtime", "operation")
                    .unwrap()
                    .is_none()
            );
        }
    }
}

/// Missing files and corrupt documents leave recovery blocked and never erase or replace historical bytes.
/// 文件缺失及文档损坏使恢复保持阻塞，绝不擦除或替换历史字节。
#[test]
fn embedded_journal_recovery_failure_preserves_storage_and_retry() {
    for missing in [true, false] {
        // Corrupt only an isolated test-owned database after releasing its real connection.
        // 仅在释放真实连接后损坏独立测试数据库。
        let directory = Directory::new();
        // This object must remain usable for a later explicit recovery of the original file.
        // 此对象必须可用于稍后对原始文件进行显式恢复。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        journal.lose_next_confirmation_for_test(true);
        assert_eq!(
            journal
                .insert("runtime", &snapshot("operation"))
                .unwrap_err(),
            uncertain()
        );
        journal
            .state
            .lock()
            .unwrap()
            .connection
            .take()
            .unwrap()
            .close()
            .unwrap();
        // Preserve exact committed bytes to restore infrastructure without synthesizing records.
        // 保留精确已提交字节，用于恢复基础设施，不合成记录。
        let original = std::fs::read(directory.database()).unwrap();
        // The sibling backup belongs to the same unique test directory.
        // 相邻备份归属于同一个唯一测试目录。
        let backup = directory.0.join("saved.db");
        if missing {
            std::fs::rename(directory.database(), &backup).unwrap();
            assert_eq!(
                journal.recover_storage().unwrap_err().code,
                EmbeddedErrorCode::NotFound
            );
            assert!(!directory.database().exists());
            std::fs::rename(&backup, directory.database()).unwrap();
        } else {
            {
                // Keep the SQLite file structurally valid but corrupt one retained digest.
                // 保持 SQLite 文件结构有效，但损坏单个保留摘要。
                let connection = rusqlite::Connection::open(directory.database()).unwrap();
                connection
                    .execute("UPDATE operations SET digest=zeroblob(32)", [])
                    .unwrap();
            }
            // Failed recovery must leave the supplied corrupt evidence byte-for-byte intact.
            // 失败恢复必须逐字节保留所提供的损坏证据。
            let corrupt_bytes = std::fs::read(directory.database()).unwrap();
            assert_eq!(
                journal.recover_storage().unwrap_err().code,
                EmbeddedErrorCode::Internal
            );
            assert_eq!(std::fs::read(directory.database()).unwrap(), corrupt_bytes);
            std::fs::write(directory.database(), &original).unwrap();
        }
        assert_eq!(
            journal.get("runtime", "operation").unwrap_err(),
            uncertain()
        );
        assert!(journal.recover_storage().unwrap());
        assert_eq!(
            journal
                .get("runtime", "operation")
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        assert!(!journal.recover_storage().unwrap());
    }
}
