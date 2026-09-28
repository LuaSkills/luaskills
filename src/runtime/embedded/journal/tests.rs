use super::*;
use crate::runtime::embedded::{HostEffectRecord, OperationContext, OperationPhase};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

mod context;
mod identity;

/// Own one newly created temporary directory, isolated from other tests and user files.
/// 拥有一个新建临时目录，与其他测试及用户文件隔离。
struct Directory(PathBuf);

impl Directory {
    /// Create a random test directory and return its sole cleanup owner.
    /// 创建随机测试目录并返回其唯一清理所有者。
    fn new() -> Self {
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).unwrap();
        let name = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("luaskills-journal-{name}"));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// Return the database path within this uniquely owned directory.
    /// 返回此唯一拥有目录内的数据库路径。
    fn database(&self) -> PathBuf {
        self.0.join("operations.db")
    }
}

impl Drop for Directory {
    /// Remove only the directory created by this test after all SQLite owners have been dropped.
    /// 所有 SQLite 所有者释放后，仅删除此测试创建的目录。
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Return explicit small disk and logical limits for fault and reopen tests.
/// 返回故障及重新打开测试的显式小规模磁盘与逻辑上限。
fn config() -> OperationJournalConfig {
    OperationJournalConfig {
        max_records: 8,
        max_record_bytes: 16 * 1024,
        max_database_bytes: 128 * 1024,
    }
}

/// Build an unfinished unknown-effect checkpoint for `id`, without inventing terminal evidence.
/// 为 `id` 构造尚未结束且副作用未知的检查点，不编造终态证据。
fn snapshot(id: &str) -> OperationSnapshot {
    OperationSnapshot {
        context: OperationContext::Unbound,
        operation_id: id.into(),
        phase: OperationPhase::Running,
        cancellation_requested: false,
        effects: EffectState::Unknown,
        value: None,
        error: None,
        host_effects: Vec::new(),
    }
}

/// Actual disk reopen retains JSON null, terminal status and the original runtime namespace.
/// 真实磁盘重新打开保留 JSON 空值、终态及原始运行时命名空间。
#[test]
fn embedded_journal_reopen_preserves_null_and_original_identity() {
    let directory = Directory::new();
    let mut first = snapshot("operation");
    first.phase = OperationPhase::Succeeded;
    first.effects = EffectState::Committed;
    first.value = Some(Value::Null);
    {
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        assert_eq!(
            journal
                .insert("runtime-before-restart", &first)
                .unwrap()
                .revision,
            1
        );
    }
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    let stored = journal
        .get("runtime-before-restart", "operation")
        .unwrap()
        .unwrap();
    assert_eq!(stored.snapshot.value, Some(Value::Null));
    assert_eq!(stored.snapshot.phase, OperationPhase::Succeeded);
    assert_eq!(stored.snapshot.effects, EffectState::Committed);
    assert!(
        journal
            .get("runtime-after-restart", "operation")
            .unwrap()
            .is_none()
    );
    journal
        .forget("runtime-before-restart", "operation", stored.revision)
        .unwrap();
    assert!(
        journal
            .get("runtime-before-restart", "operation")
            .unwrap()
            .is_none()
    );
}

/// Missing success values remain absent through serialization while explicit null stays present.
/// 序列化往返时缺失的成功值保持缺失，显式空值保持存在。
#[test]
fn embedded_journal_snapshot_distinguishes_missing_result() {
    let encoded = serde_json::to_vec(&snapshot("operation")).unwrap();
    let decoded: OperationSnapshot = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.value, None);
    assert!(
        !serde_json::from_slice::<Value>(&encoded)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("value")
    );
}

/// Same-file owners fail promptly; closing the original owner permits explicit reopening.
/// 同文件所有者及时失败；关闭原始所有者后允许显式重新打开。
#[test]
fn embedded_journal_exclusive_owner_and_reopen() {
    let directory = Directory::new();
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    assert_eq!(
        OperationJournal::open(&directory.database(), config())
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Busy
    );
    drop(journal);
    OperationJournal::open(&directory.database(), config()).unwrap();
}

/// Revision races have exactly one winner and duplicate insertion never overwrites evidence.
/// 修订号竞态只有一个胜者，重复插入绝不覆盖证据。
#[test]
fn embedded_journal_revision_compare_and_swap() {
    let directory = Directory::new();
    let journal = Arc::new(OperationJournal::open(&directory.database(), config()).unwrap());
    journal.insert("runtime", &snapshot("operation")).unwrap();
    assert_eq!(
        journal
            .insert("runtime", &snapshot("operation"))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::AlreadyCompleted
    );
    let barrier = Arc::new(Barrier::new(3));
    let workers = (0..2)
        .map(|_| {
            let journal = Arc::clone(&journal);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                journal.replace("runtime", 1, &snapshot("operation"))
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results.into_iter().find_map(Result::err).unwrap().code,
        EmbeddedErrorCode::StaleGeneration
    );
    assert_eq!(
        journal
            .get("runtime", "operation")
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        journal
            .replace("runtime", u64::MAX, &snapshot("operation"))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::InvalidArgument
    );
}

/// Exhausted logical capacity preserves old records and cannot evict unresolved effects.
/// 逻辑容量耗尽保留旧记录，不能淘汰未解决的副作用。
#[test]
fn embedded_journal_capacity_and_unresolved_retention() {
    let directory = Directory::new();
    let mut limits = config();
    limits.max_records = 1;
    let journal = OperationJournal::open(&directory.database(), limits).unwrap();
    journal.insert("runtime", &snapshot("first")).unwrap();
    assert_eq!(
        journal
            .insert("runtime", &snapshot("second"))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert_eq!(
        journal.forget("runtime", "first", 1).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    let mut record = snapshot("first");
    record.phase = OperationPhase::Failed;
    record.effects = EffectState::Committed;
    record.host_effects.push(HostEffectRecord {
        caller: super::super::capabilities::CapabilityCaller {
            runtime_id: "runtime".into(),
            operation_id: "first".into(),
            plugin_id: "journal-plugin".into(),
            package_generation: "generation-one".into(),
            execution_revision: "revision-one".into(),
            security_partition: "test".into(),
            session_id: None,
            workspace_root: None,
        },
        effect_id: "effect".into(),
        registration_id: "registration".into(),
        capability_name: "write".into(),
        capability_version: "1".into(),
        request_id: Some("request".into()),
        phase: HostEffectPhase::Completed,
        effects: EffectState::Unknown,
    });
    journal.replace("runtime", 1, &record).unwrap();
    assert_eq!(
        journal.forget("runtime", "first", 2).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    record.host_effects[0].effects = EffectState::Committed;
    journal.replace("runtime", 2, &record).unwrap();
    assert_eq!(
        journal.forget("runtime", "first", 2).unwrap_err().code,
        EmbeddedErrorCode::StaleGeneration
    );
    journal.forget("runtime", "first", 3).unwrap();
    journal.insert("runtime", &snapshot("second")).unwrap();
}

/// Oversized input is rejected before disk mutation, and stricter reopen limits never erase history.
/// 超大输入在磁盘变更前被拒绝，更严格的重新打开上限绝不清除历史。
#[test]
fn embedded_journal_record_limit_and_reopen_limit() {
    let directory = Directory::new();
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    let mut record = snapshot("large");
    record.value = Some(json!("x".repeat(config().max_record_bytes)));
    assert_eq!(
        journal.insert("runtime", &record).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert!(journal.get("runtime", "large").unwrap().is_none());
    record.value = Some(json!("中".repeat(1024)));
    journal.insert("runtime", &record).unwrap();
    journal.insert("runtime", &snapshot("second")).unwrap();
    drop(journal);
    let mut limits = config();
    limits.max_records = 1;
    assert_eq!(
        OperationJournal::open(&directory.database(), limits)
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    limits = config();
    limits.max_record_bytes = 1024;
    assert_eq!(
        OperationJournal::open(&directory.database(), limits)
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    assert!(
        OperationJournal::open(&directory.database(), config())
            .unwrap()
            .get("runtime", "large")
            .unwrap()
            .is_some()
    );
}

/// An unrelated or future-version database stays byte-identical after rejection.
/// 不相关或未来版本数据库被拒绝后保持逐字节不变。
#[test]
fn embedded_journal_unknown_database_unchanged() {
    let directory = Directory::new();
    {
        let connection = Connection::open(directory.database()).unwrap();
        connection
            .execute_batch("CREATE TABLE other(secret TEXT); INSERT INTO other VALUES('retained')")
            .unwrap();
    }
    let original = std::fs::read(directory.database()).unwrap();
    assert_eq!(
        OperationJournal::open(&directory.database(), config())
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Unsupported
    );
    assert_eq!(std::fs::read(directory.database()).unwrap(), original);
    let future_path = directory.0.join("future.db");
    drop(OperationJournal::open(&future_path, config()).unwrap());
    {
        // Derive an unsupported future version from the actual current file instead of duplicating its version.
        // 从真实当前文件派生不支持的未来版本，不重复定义其版本。
        let connection = Connection::open(&future_path).unwrap();
        let current: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        connection
            .pragma_update(None, "user_version", current + 1)
            .unwrap();
    }
    let original = std::fs::read(&future_path).unwrap();
    assert_eq!(
        OperationJournal::open(&future_path, config())
            .err()
            .unwrap()
            .code,
        EmbeddedErrorCode::Unsupported
    );
    assert_eq!(std::fs::read(&future_path).unwrap(), original);
}

/// Digest and redundant-key validation each detect corrupted historical evidence on reopen.
/// 摘要及冗余键校验分别在重新打开时发现损坏的历史证据。
#[test]
fn embedded_journal_corruption_rejected() {
    for damage in ["digest", "identity", "schema"] {
        let directory = Directory::new();
        {
            let journal = OperationJournal::open(&directory.database(), config()).unwrap();
            journal.insert("runtime", &snapshot("operation")).unwrap();
        }
        {
            let connection = Connection::open(directory.database()).unwrap();
            match damage {
                "digest" => {
                    connection
                        .execute("UPDATE operations SET digest=zeroblob(32)", [])
                        .unwrap();
                }
                "identity" => {
                    connection
                        .execute("UPDATE operations SET operation_id='different'", [])
                        .unwrap();
                }
                "schema" => {
                    connection
                        .execute("CREATE TABLE unexpected(value TEXT)", [])
                        .unwrap();
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(
            OperationJournal::open(&directory.database(), config())
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::Internal,
            "{damage}"
        );
    }
}

/// A real SQLite page limit rolls back the failed insert and preserves every acknowledged row.
/// 真实 SQLite 页上限回滚失败插入并保留所有已确认行。
#[test]
fn embedded_journal_database_page_capacity() {
    let directory = Directory::new();
    let mut limits = config();
    limits.max_records = 128;
    limits.max_database_bytes = 16 * 1024;
    let journal = OperationJournal::open(&directory.database(), limits).unwrap();
    let mut retained = Vec::new();
    for index in 0..limits.max_records {
        let mut record = snapshot(&format!("operation-{index}"));
        record.value = Some(json!("x".repeat(4096)));
        match journal.insert("runtime", &record) {
            Ok(_) => retained.push(record.operation_id),
            Err(error) => {
                assert_eq!(error.code, EmbeddedErrorCode::CapacityExceeded);
                assert!(
                    journal
                        .get("runtime", &record.operation_id)
                        .unwrap()
                        .is_none()
                );
                break;
            }
        }
    }
    assert!(!retained.is_empty() && retained.len() < limits.max_records);
    assert!(std::fs::metadata(directory.database()).unwrap().len() <= limits.max_database_bytes);
    drop(journal);
    let journal = OperationJournal::open(&directory.database(), limits).unwrap();
    for id in retained {
        assert_eq!(journal.get("runtime", &id).unwrap().unwrap().revision, 1);
    }
}

/// Child process deliberately exits without destructors after a flushed uncommitted or committed write.
/// 子进程在已刷新的未提交或已提交写入后，故意不执行析构便退出。
#[test]
#[ignore = "only launched by the crash-recovery parent with an isolated database"]
fn embedded_journal_crash_child() {
    let path = PathBuf::from(std::env::var_os("LUASKILLS_JOURNAL_CRASH_PATH").unwrap());
    let mode = std::env::var("LUASKILLS_JOURNAL_CRASH_MODE").unwrap();
    let journal = OperationJournal::open(&path, config()).unwrap();
    journal.insert("runtime", &snapshot("operation")).unwrap();
    if mode == "before_commit" {
        let state = journal.lock().unwrap();
        state
            .connection
            .execute_batch(
                "BEGIN IMMEDIATE; UPDATE operations SET revision=2, document=zeroblob(8192)",
            )
            .unwrap();
        state.connection.cache_flush().unwrap();
        assert!(PathBuf::from(format!("{}-journal", path.display())).exists());
    } else {
        assert_eq!(mode, "after_commit");
        let mut record = snapshot("operation");
        record.phase = OperationPhase::Succeeded;
        record.effects = EffectState::Committed;
        record.value = Some(Value::Null);
        journal.replace("runtime", 1, &record).unwrap();
    }
    std::process::exit(87);
}

/// Restart recovers a real hot rollback journal and never invents success for the unfinished operation.
/// 重启恢复真实热回滚日志，绝不为未完成操作编造成功。
#[test]
fn embedded_journal_process_exit_recovery() {
    for mode in ["before_commit", "after_commit"] {
        let directory = Directory::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::embedded::journal::tests::embedded_journal_crash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("LUASKILLS_JOURNAL_CRASH_PATH", directory.database())
            .env("LUASKILLS_JOURNAL_CRASH_MODE", mode)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(87),
            "child did not reach crash point: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        let record = journal.get("runtime", "operation").unwrap().unwrap();
        if mode == "before_commit" {
            assert_eq!(record.revision, 1);
            assert_eq!(record.snapshot.phase, OperationPhase::Running);
            assert_eq!(record.snapshot.effects, EffectState::Unknown);
            assert_eq!(
                journal.forget("runtime", "operation", 1).unwrap_err().code,
                EmbeddedErrorCode::Busy
            );
        } else {
            assert_eq!(record.revision, 2);
            assert_eq!(record.snapshot.phase, OperationPhase::Succeeded);
            assert_eq!(record.snapshot.value, Some(Value::Null));
            assert_eq!(record.snapshot.effects, EffectState::Committed);
        }
    }
}

/// Recovery enumerates bounded single rows across original namespaces without guessing operation IDs.
/// 恢复按有界单行跨原始命名空间枚举，无需猜测操作 ID。
#[test]
fn embedded_journal_enumerates_history_after_restart() {
    let directory = Directory::new();
    {
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        for (runtime, operation) in [("second", "a"), ("first", "z"), ("first", "a")] {
            journal.insert(runtime, &snapshot(operation)).unwrap();
        }
    }
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    let mut cursor: Option<(String, String)> = None;
    let mut found = Vec::new();
    while let Some(record) = journal
        .next(
            cursor
                .as_ref()
                .map(|(runtime, operation)| (runtime.as_str(), operation.as_str())),
        )
        .unwrap()
    {
        cursor = Some((record.runtime_id, record.snapshot.operation_id));
        found.push(cursor.clone().unwrap());
    }
    assert_eq!(
        found,
        [
            ("first".into(), "a".into()),
            ("first".into(), "z".into()),
            ("second".into(), "a".into())
        ]
    );
}

/// Lost commit acknowledgement blocks reads and mutations until explicit reopen reveals exact evidence.
/// 提交确认丢失阻止读取及变更，直至显式重新打开揭示精确证据。
#[test]
fn embedded_journal_uncertain_commit_requires_reopen() {
    let directory = Directory::new();
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    journal.insert("runtime", &snapshot("operation")).unwrap();
    let mut retained = journal.get("runtime", "operation").unwrap().unwrap();
    retained.revision += 1;
    retained.snapshot.effects = EffectState::Committed;
    let document = journal.encode(&retained).unwrap();
    // Test-only fault: consume the actual commit before the transaction wrapper receives its receipt.
    // 仅测试故障：事务包装器接收回执前，先消耗真实提交。
    let error = journal
        .transaction(|connection| {
            connection
                .execute(
                    "UPDATE operations SET revision=?1, document=?2, digest=?3",
                    params![
                        retained.revision as i64,
                        document,
                        Sha256::digest(&document).as_slice()
                    ],
                )
                .map_err(storage::error)?;
            connection.execute_batch("COMMIT").map_err(storage::error)
        })
        .unwrap_err();
    assert_eq!(error, uncertain());
    assert_eq!(journal.get("runtime", "operation").unwrap_err(), error);
    assert_eq!(journal.next(None).unwrap_err(), error);
    assert_eq!(
        journal.insert("runtime", &snapshot("second")).unwrap_err(),
        error
    );
    drop(journal);
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    let record = journal.get("runtime", "operation").unwrap().unwrap();
    assert_eq!(record.revision, retained.revision);
    assert_eq!(record.snapshot.effects, EffectState::Committed);
    assert!(journal.get("runtime", "second").unwrap().is_none());
}

/// Invalid limits and relative paths are rejected without creating any database.
/// 无效上限及相对路径在创建任何数据库前被拒绝。
#[test]
fn embedded_journal_rejects_invalid_configuration() {
    let directory = Directory::new();
    for limits in [
        OperationJournalConfig {
            max_records: 0,
            ..config()
        },
        OperationJournalConfig {
            max_record_bytes: 0,
            ..config()
        },
        OperationJournalConfig {
            max_record_bytes: usize::MAX,
            ..config()
        },
        OperationJournalConfig {
            max_database_bytes: 1,
            ..config()
        },
        OperationJournalConfig {
            max_database_bytes: u64::MAX,
            ..config()
        },
    ] {
        assert!(OperationJournal::open(&directory.database(), limits).is_err());
        assert!(!directory.database().exists());
    }
    assert!(OperationJournal::open(Path::new("relative.db"), config()).is_err());
    assert!(OperationJournal::open(&directory.0, config()).is_err());
}
