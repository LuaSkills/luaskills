//! Explicit disk history; records never become live operations merely by reopening the journal.
//! 显式磁盘历史；重新打开日志绝不使记录自动成为活动操作。

mod storage;
#[cfg(test)]
mod tests;

use super::value_size::json_size;
use super::{
    EffectState, EmbeddedError, EmbeddedErrorCode, EmbeddedResult, HostEffectPhase,
    OperationSnapshot,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// Explicit retention budgets; SQLite journal/cache overhead is separate from the database-file cap.
/// 显式保留预算；SQLite 日志及缓存开销与数据库文件上限分开计算。
#[derive(Debug, Clone, Copy)]
pub struct OperationJournalConfig {
    /// Maximum retained operations across all runtime namespaces; no automatic eviction occurs.
    /// 所有运行时命名空间合计保留的最大操作数；不自动淘汰。
    pub max_records: usize,
    /// Maximum UTF-8 JSON bytes for one complete stored record, including identities and revision.
    /// 单条完整存储记录的最大 UTF-8 JSON 字节数，包含身份及修订号。
    pub max_record_bytes: usize,
    /// Maximum main database bytes, rounded down to whole SQLite pages.
    /// 主数据库最大字节数，向下取整至完整 SQLite 页。
    pub max_database_bytes: u64,
}

/// Historical checkpoint, not a live handle and not evidence authorizing execution replay.
/// 历史检查点，不是活动句柄，也不是授权执行重放的证据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalOperation {
    /// Original runtime namespace, never rebound to the namespace of a restarted runtime.
    /// 原始运行时命名空间，绝不重新绑定到重启后的命名空间。
    pub runtime_id: String,
    /// Monotonic compare-and-swap revision; positive and bounded by SQLite's signed integer.
    /// 单调比较交换修订号；为正数且受 SQLite 有符号整数范围约束。
    pub revision: u64,
    /// Exact last committed observation; an unfinished phase remains unfinished after restart.
    /// 最后提交的精确观测；未结束的阶段在重启后仍保持未结束。
    pub snapshot: OperationSnapshot,
}

/// Single connection and sticky unproven-commit failure protected by the journal's own lock.
/// 日志专用锁保护的单连接及不可自动清除的提交未证实故障。
struct JournalState {
    /// Exclusive database connection retained until the journal itself is dropped.
    /// 保留至日志本身释放的独占数据库连接。
    connection: Connection,
    /// Uncertain commit/rollback blocks subsequent mutations until close and explicit reopen.
    /// 不确定的提交或回滚阻止后续变更，直至关闭并显式重新打开。
    failure: Option<EmbeddedError>,
}

/// Bounded synchronous history owned by a trusted host; callers must not hold scheduler locks.
/// 由可信宿主拥有的有界同步历史；调用方不得持有调度器锁。
/// One owner opens a local database in a host-controlled directory; plugins never choose this path.
/// 单个所有者打开宿主管理目录内的本地数据库；插件不得选择此路径。
pub struct OperationJournal {
    /// Validated immutable limits shared by every read and mutation.
    /// 所有读取及变更共享的已校验不可变上限。
    config: OperationJournalConfig,
    /// Serializes disk transactions independently from runtime metadata locks.
    /// 独立于运行时元数据锁，串行化磁盘事务。
    state: Mutex<JournalState>,
}

impl OperationJournal {
    /// Open absolute local `path` under `config`, validating every retained record before returning.
    /// 按 `config` 打开绝对本地 `path`，返回前校验所有保留记录。
    /// Existing unknown formats, corruption, insufficient limits and another owner fail explicitly.
    /// 已有未知格式、损坏、上限不足或其他所有者都会明确失败。
    pub fn open(path: &Path, config: OperationJournalConfig) -> EmbeddedResult<Self> {
        let connection = storage::open(path, config)?;
        let journal = Self {
            config,
            state: Mutex::new(JournalState {
                connection,
                failure: None,
            }),
        };
        {
            let state = journal.lock()?;
            let count: i64 = state
                .connection
                .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
                .map_err(storage::error)?;
            if count < 0 || count as u64 > config.max_records as u64 {
                return Err(capacity());
            }
            // Stream one row at a time so validation never allocates the entire history.
            // 每次流式处理一行，避免校验时分配全部历史。
            let mut statement = state
                .connection
                .prepare(
                    "SELECT runtime_id, operation_id, revision, document, digest FROM operations",
                )
                .map_err(storage::error)?;
            let mut rows = statement.query([]).map_err(storage::error)?;
            while let Some(row) = rows.next().map_err(storage::error)? {
                journal.decode(row)?;
            }
        }
        Ok(journal)
    }

    /// Retain `snapshot` for exact `runtime_id` at revision one; duplicate identities never overwrite.
    /// 以修订号一保留精确 `runtime_id` 的 `snapshot`；重复身份绝不覆盖。
    pub fn insert(
        &self,
        runtime_id: &str,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<JournalOperation> {
        self.validate_key(runtime_id, &snapshot.operation_id)?;
        json_size(snapshot, self.config.max_record_bytes)?;
        let record = JournalOperation {
            runtime_id: runtime_id.to_owned(),
            revision: 1,
            snapshot: snapshot.clone(),
        };
        let document = self.encode(&record)?;
        self.transaction(|connection| {
            if self.read(connection, runtime_id, &snapshot.operation_id)?.is_some() {
                return Err(EmbeddedError::new(EmbeddedErrorCode::AlreadyCompleted, "operation history identity already exists"));
            }
            let count: i64 = connection.query_row("SELECT count(*) FROM operations", [], |row| row.get(0)).map_err(storage::error)?;
            if count as u64 >= self.config.max_records as u64 { return Err(capacity()); }
            connection.execute("INSERT INTO operations(runtime_id, operation_id, revision, document, digest) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![runtime_id, snapshot.operation_id, record.revision as i64, document, Sha256::digest(&document).as_slice()]).map_err(storage::error)?;
            Ok(())
        })?;
        Ok(record)
    }

    /// Replace the exact record only at `expected_revision`, returning the new durable revision.
    /// 仅在 `expected_revision` 匹配时替换精确记录，返回新的持久修订号。
    /// The trusted execution/reconciliation owner supplies state; this store never infers effect outcomes.
    /// 状态由可信执行或对账所有者提供；存储层绝不推断副作用结果。
    pub fn replace(
        &self,
        runtime_id: &str,
        expected_revision: u64,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<JournalOperation> {
        self.validate_key(runtime_id, &snapshot.operation_id)?;
        json_size(snapshot, self.config.max_record_bytes)?;
        let revision = expected_revision
            .checked_add(1)
            .filter(|value| expected_revision > 0 && *value <= i64::MAX as u64)
            .ok_or_else(|| {
                EmbeddedError::invalid("operation history revision is exhausted or invalid")
            })?;
        let record = JournalOperation {
            runtime_id: runtime_id.to_owned(),
            revision,
            snapshot: snapshot.clone(),
        };
        let document = self.encode(&record)?;
        self.transaction(|connection| {
            self.expect(connection, runtime_id, &snapshot.operation_id, expected_revision)?;
            connection.execute("UPDATE operations SET revision=?3, document=?4, digest=?5 WHERE runtime_id=?1 AND operation_id=?2",
                params![runtime_id, snapshot.operation_id, revision as i64, document, Sha256::digest(&document).as_slice()]).map_err(storage::error)?;
            Ok(())
        })?;
        Ok(record)
    }

    /// Read historical evidence by exact identity; absence does not prove that execution never occurred.
    /// 按精确身份读取历史证据；不存在不证明从未执行。
    pub fn get(
        &self,
        runtime_id: &str,
        operation_id: &str,
    ) -> EmbeddedResult<Option<JournalOperation>> {
        self.validate_key(runtime_id, operation_id)?;
        self.read(&self.lock()?.connection, runtime_id, operation_id)
    }

    /// Return one historical row after exact `(runtime_id, operation_id)` in binary key order.
    /// 按二进制键序返回精确 `(runtime_id, operation_id)` 之后的一条历史记录。
    /// Pass `None` to start recovery enumeration; concurrent edits are not a multi-call snapshot.
    /// 传入 `None` 开始恢复枚举；并发编辑不构成跨调用快照。
    pub fn next(&self, after: Option<(&str, &str)>) -> EmbeddedResult<Option<JournalOperation>> {
        if let Some((runtime_id, operation_id)) = after {
            self.validate_key(runtime_id, operation_id)?;
        }
        let state = self.lock()?;
        let result = match after {
            None => state.connection.query_row("SELECT runtime_id, operation_id, revision, document, digest FROM operations ORDER BY runtime_id, operation_id LIMIT 1", [], |row| Ok(self.decode(row))),
            Some((runtime_id, operation_id)) => state.connection.query_row("SELECT runtime_id, operation_id, revision, document, digest FROM operations WHERE (runtime_id, operation_id) > (?1, ?2) ORDER BY runtime_id, operation_id LIMIT 1", params![runtime_id, operation_id], |row| Ok(self.decode(row))),
        };
        result.optional().map_err(storage::error)?.transpose()
    }

    /// Delete only terminal reconciled evidence at the exact revision; unresolved evidence retains capacity.
    /// 仅按精确修订号删除终态且已对账的证据；未解决证据继续占有容量。
    pub fn forget(
        &self,
        runtime_id: &str,
        operation_id: &str,
        expected_revision: u64,
    ) -> EmbeddedResult<()> {
        self.validate_key(runtime_id, operation_id)?;
        self.transaction(|connection| {
            let record = self.expect(connection, runtime_id, operation_id, expected_revision)?;
            if !record.snapshot.phase.is_terminal()
                || record.snapshot.effects == EffectState::Unknown
                || record.snapshot.host_effects.iter().any(|effect| {
                    effect.phase != HostEffectPhase::Completed
                        || effect.effects == EffectState::Unknown
                })
            {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "unresolved operation history cannot be forgotten",
                ));
            }
            connection
                .execute(
                    "DELETE FROM operations WHERE runtime_id=?1 AND operation_id=?2",
                    params![runtime_id, operation_id],
                )
                .map_err(storage::error)?;
            Ok(())
        })
    }

    /// Execute one `mutation` atomically; preserve uncertain commit/rollback failures without retrying.
    /// 原子执行一次 `mutation`；保留不确定的提交或回滚故障，不重试。
    fn transaction(
        &self,
        mutation: impl FnOnce(&Connection) -> EmbeddedResult<()>,
    ) -> EmbeddedResult<()> {
        let mut state = self.lock()?;
        let JournalState {
            connection,
            failure,
        } = &mut *state;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage::error)?;
        match mutation(&transaction) {
            Ok(()) => match transaction.commit() {
                Ok(()) => Ok(()),
                Err(_) => {
                    let error = uncertain();
                    *failure = Some(error.clone());
                    Err(error)
                }
            },
            // SQLITE_FULL and selected I/O errors may already have rolled back the transaction.
            // SQLITE_FULL 及部分 I/O 错误可能已经回滚事务。
            // This connection is exclusively owned and mutation closures never commit themselves.
            // 此连接被独占，且变更闭包绝不自行提交。
            Err(error) if transaction.is_autocommit() => Err(error),
            Err(error) => match transaction.rollback() {
                Ok(()) => Err(error),
                Err(_) => {
                    let error = uncertain();
                    *failure = Some(error.clone());
                    Err(error)
                }
            },
        }
    }

    /// Validate and encode `record` within the configured byte budget before starting a transaction.
    /// 开始事务前，在配置字节预算内校验并编码 `record`。
    fn encode(&self, record: &JournalOperation) -> EmbeddedResult<Vec<u8>> {
        self.validate_key(&record.runtime_id, &record.snapshot.operation_id)?;
        json_size(record, self.config.max_record_bytes)?;
        serde_json::to_vec(record)
            .map_err(|_| EmbeddedError::invalid("operation history serialization failed"))
    }

    /// Reject empty or over-budget lookup identities before SQLite allocates their bound parameters.
    /// SQLite 分配绑定参数前，拒绝空或超出预算的查询身份。
    fn validate_key(&self, runtime_id: &str, operation_id: &str) -> EmbeddedResult<()> {
        if runtime_id.is_empty() || operation_id.is_empty() {
            return Err(EmbeddedError::invalid(
                "operation history identities must not be empty",
            ));
        }
        if runtime_id.len() > self.config.max_record_bytes
            || operation_id.len() > self.config.max_record_bytes
        {
            return Err(capacity());
        }
        Ok(())
    }

    /// Read a single row using `connection`, checking its bounded encoding, digest and redundant identity.
    /// 使用 `connection` 读取单行，检查其有界编码、摘要及冗余身份。
    fn read(
        &self,
        connection: &Connection,
        runtime_id: &str,
        operation_id: &str,
    ) -> EmbeddedResult<Option<JournalOperation>> {
        connection.query_row("SELECT runtime_id, operation_id, revision, document, digest FROM operations WHERE runtime_id=?1 AND operation_id=?2",
            params![runtime_id, operation_id], |row| Ok(self.decode(row))).optional().map_err(storage::error)?.transpose()
    }

    /// Require the original key and revision before changing or deleting a record.
    /// 变更或删除记录前，要求原始键与修订号一致。
    fn expect(
        &self,
        connection: &Connection,
        runtime_id: &str,
        operation_id: &str,
        revision: u64,
    ) -> EmbeddedResult<JournalOperation> {
        let record = self
            .read(connection, runtime_id, operation_id)?
            .ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::NotFound,
                    "operation history was not found",
                )
            })?;
        if record.revision != revision {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::StaleGeneration,
                "operation history revision changed",
            ));
        }
        Ok(record)
    }

    /// Decode borrowed SQLite row bytes, allocating JSON only after size and checksum validation.
    /// 解码借用的 SQLite 行字节，仅在大小及摘要校验后分配 JSON。
    fn decode(&self, row: &rusqlite::Row<'_>) -> EmbeddedResult<JournalOperation> {
        let document = row
            .get_ref(3)
            .map_err(storage::error)?
            .as_blob()
            .map_err(|_| corrupt())?;
        if document.len() > self.config.max_record_bytes {
            return Err(capacity());
        }
        let digest = row
            .get_ref(4)
            .map_err(storage::error)?
            .as_blob()
            .map_err(|_| corrupt())?;
        if Sha256::digest(document).as_slice() != digest {
            return Err(corrupt());
        }
        let record: JournalOperation = serde_json::from_slice(document).map_err(|_| corrupt())?;
        let runtime_id = row
            .get_ref(0)
            .map_err(storage::error)?
            .as_str()
            .map_err(|_| corrupt())?;
        let operation_id = row
            .get_ref(1)
            .map_err(storage::error)?
            .as_str()
            .map_err(|_| corrupt())?;
        let revision: i64 = row.get(2).map_err(storage::error)?;
        if runtime_id.is_empty()
            || operation_id.is_empty()
            || record.runtime_id != runtime_id
            || record.snapshot.operation_id != operation_id
            || revision <= 0
            || record.revision != revision as u64
        {
            return Err(corrupt());
        }
        Ok(record)
    }

    /// Acquire journal ownership; poisoning never becomes a guessed successful checkpoint.
    /// 获取日志所有权；锁中毒绝不转化为猜测的成功检查点。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, JournalState>> {
        let state = self.state.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation history lock is poisoned",
            )
        })?;
        // A failed rollback may leave connection-local uncommitted bytes; never label them durable.
        // 回滚失败可能留下连接内未提交字节；绝不将其标记为持久证据。
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        Ok(state)
    }
}

/// Return a bounded retention error without exposing database paths or application values.
/// 返回有界保留错误，不暴露数据库路径或应用值。
fn capacity() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::CapacityExceeded,
        "operation history exceeds configured retention limits",
    )
}

/// Return a corruption diagnostic; no migration, clearing or replay is attempted.
/// 返回损坏诊断；不尝试迁移、清空或重放。
fn corrupt() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "operation history record is corrupt",
    )
}

/// Return the sticky uncertainty state requiring retained evidence and explicit recovery.
/// 返回要求保留证据并显式恢复的持续不确定状态。
fn uncertain() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "operation history commit is uncertain; close and reconcile before further mutations",
    )
}
