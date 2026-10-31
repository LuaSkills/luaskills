//! Host-owned operation history, independent from any database implementation.
//! 由宿主拥有的操作历史，独立于任何数据库实现。

mod reconciliation;
#[cfg(any(test, feature = "sqlite-journal"))]
mod sqlite;
mod worker;

#[cfg(any(test, feature = "sqlite-journal"))]
use super::EmbeddedErrorCode;
use super::{EffectState, EmbeddedError, EmbeddedResult, HostEffectPhase, OperationSnapshot};
pub use reconciliation::{
    HostEffectReconciliation, OperationReconciliation, ReconciledExecution, ResolvedEffectState,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub use worker::{
    JournalWritePhase, JournalWriteReceipt, JournalWriteSnapshot, OperationJournalWorker,
    OperationJournalWorkerConfig, OperationJournalWorkerStatus,
};

/// Explicit retention budgets; SQLite journal/cache overhead is separate from the database-file cap.
/// 显式保留预算；SQLite 日志及缓存开销与数据库文件上限分开计算。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
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
    /// Separate final host attestation; the original snapshot remains unchanged, including unknown results.
    /// 独立最终宿主证明；原始快照保持不变，包括未知结果。
    #[serde(deserialize_with = "reconciliation::present_reconciliation")]
    pub reconciliation: Option<OperationReconciliation>,
}

/// Durable history supplied by a trusted host; each method must be atomic and bounded by host limits.
/// 可信宿主提供的持久历史；每个方法必须保持原子性并受宿主上限约束。
/// Success confirms durable storage. Uncertain commits must block access until explicit recovery.
/// 成功表示已确认持久存储。不确定提交必须阻止访问，直到显式恢复。
/// Hosts preserve identities, immutable contexts, finalization rules and existing records without eviction.
/// 宿主保留身份、不可变上下文、终态规则及既有记录，不自动淘汰。
pub trait OperationJournalBackend: Send + Sync {
    /// Validate identity syntax and size without I/O; return admission failure before queue allocation.
    /// 无 I/O 地校验身份语法和大小；在队列分配前返回入场错误。
    fn validate_key(&self, runtime_id: &str, operation_id: &str) -> EmbeddedResult<()>;
    /// Store `snapshot` for `runtime_id` at the successor of `previous`; return durable evidence.
    /// 将 `runtime_id` 的 `snapshot` 存储为 `previous` 的后继；返回持久证据。
    /// `None` inserts revision one; `reconcile` permits only an identical already-committed successor.
    /// `None` 插入修订一；`reconcile` 仅允许确认完全相同的已提交后继。
    fn write_checkpoint(
        &self,
        runtime_id: &str,
        previous: Option<u64>,
        snapshot: &OperationSnapshot,
        reconcile: bool,
    ) -> EmbeddedResult<JournalOperation>;

    /// Read exact `runtime_id` and `operation_id`; return retained evidence or confirmed absence.
    /// 读取精确 `runtime_id` 和 `operation_id`；返回保留证据或确认的缺失。
    fn get(&self, runtime_id: &str, operation_id: &str)
    -> EmbeddedResult<Option<JournalOperation>>;

    /// Read the next binary-ordered identity after `after`; `None` starts enumeration or ends results.
    /// 读取 `after` 之后按二进制排序的身份；参数 `None` 开始枚举，结果 `None` 表示结束。
    fn next(&self, after: Option<(&str, &str)>) -> EmbeddedResult<Option<JournalOperation>>;

    /// Remove exact terminal/reconciled evidence at `expected_revision`; return success only after commit.
    /// 删除 `expected_revision` 处的精确终态或已对账证据；仅在提交后返回成功。
    /// Unfinished or unknown effects require final host reconciliation before deletion.
    /// 未完成或未知副作用必须经过最终宿主对账才能删除。
    fn forget(
        &self,
        runtime_id: &str,
        operation_id: &str,
        expected_revision: u64,
    ) -> EmbeddedResult<()>;

    /// Attach `resolution` to the exact identity/revision; return its durable successor revision.
    /// 为精确身份及修订附加 `resolution`；返回其持久后继修订。
    /// Exact retries acknowledge the same successor; other revisions and attestations conflict.
    /// 精确重试确认同一后继；其他修订和证明发生冲突。
    fn reconcile(
        &self,
        runtime_id: &str,
        operation_id: &str,
        expected_revision: u64,
        resolution: &OperationReconciliation,
    ) -> EmbeddedResult<u64>;

    /// Validate retained storage after uncertainty; return whether recovery was required and completed.
    /// 不确定状态后校验保留存储；返回是否需要且已完成恢复。
    /// Never retry business work, create replacement storage or silently clear evidence.
    /// 绝不重试业务工作、创建替代存储或静默清除证据。
    fn recover_storage(&self) -> EmbeddedResult<bool>;
}

/// Engine-independent journal handle retaining one exact host backend for all worker operations.
/// 独立于引擎的日志句柄，为全部工作线程操作保留同一个精确宿主后端。
pub struct OperationJournal {
    /// Host-owned persistence implementation retained until all journal users release it.
    /// 保留至全部日志使用者释放的宿主持久化实现。
    backend: Arc<dyn OperationJournalBackend>,
    /// Real SQLite storage retained only for deterministic test fault controls.
    /// 仅为确定性测试故障控制保留的真实 SQLite 存储。
    #[cfg(test)]
    test_sqlite: Option<Arc<sqlite::OperationJournal>>,
}

impl OperationJournal {
    /// Check exact identity limits without storage access; return the host's admission result.
    /// 不访问存储地检查精确身份上限；返回宿主入场结果。
    fn validate_key(&self, runtime_id: &str, operation_id: &str) -> EmbeddedResult<()> {
        self.backend.validate_key(runtime_id, operation_id)
    }

    /// Persist the worker's exact candidate with its explicit retry policy; return durable evidence.
    /// 按工作线程的显式重试策略持久化精确候选；返回持久证据。
    fn write_checkpoint(
        &self,
        runtime_id: &str,
        previous: Option<u64>,
        snapshot: &OperationSnapshot,
        reconcile: bool,
    ) -> EmbeddedResult<JournalOperation> {
        self.backend
            .write_checkpoint(runtime_id, previous, snapshot, reconcile)
    }
    /// Retain `backend` without opening files; return an owner that forwards to the host implementation.
    /// 保留 `backend` 而不打开文件；返回向宿主实现转发的所有者。
    pub fn from_backend(backend: Arc<dyn OperationJournalBackend>) -> Self {
        Self {
            backend,
            #[cfg(test)]
            test_sqlite: None,
        }
    }

    /// Open host-selected SQLite `path` under `config`; return validated ownership or an explicit error.
    /// 按 `config` 打开宿主选择的 SQLite `path`；返回已校验所有权或明确错误。
    /// Requires the explicit `sqlite-journal` adapter; the default library exposes only host registration.
    /// 需要显式 `sqlite-journal` 适配器；默认库仅暴露宿主注册。
    #[cfg(any(test, feature = "sqlite-journal"))]
    pub fn open(path: &std::path::Path, config: OperationJournalConfig) -> EmbeddedResult<Self> {
        // Keep the existing format and exclusive-open validation in the optional adapter.
        // 在可选适配器中保留既有格式和独占打开校验。
        let backend = Arc::new(sqlite::OperationJournal::open(path, config)?);
        Ok(Self {
            backend: backend.clone(),
            #[cfg(test)]
            test_sqlite: Some(backend),
        })
    }

    /// Insert `snapshot` under exact `runtime_id`; return revision-one evidence or an explicit conflict.
    /// 按精确 `runtime_id` 插入 `snapshot`；返回修订一证据或明确冲突。
    pub fn insert(
        &self,
        runtime_id: &str,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<JournalOperation> {
        self.backend
            .write_checkpoint(runtime_id, None, snapshot, false)
    }

    /// Replace exact `expected_revision` for `runtime_id`; return the durable successor of `snapshot`.
    /// 替换 `runtime_id` 的精确 `expected_revision`；返回 `snapshot` 的持久后继。
    pub fn replace(
        &self,
        runtime_id: &str,
        expected_revision: u64,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<JournalOperation> {
        self.backend
            .write_checkpoint(runtime_id, Some(expected_revision), snapshot, false)
    }

    /// Commit the original scheduler candidate; return evidence, allowing only an identical retry.
    /// 提交调度器原始候选；返回证据，仅允许完全相同的重试。
    pub(in crate::runtime::embedded) fn checkpoint(
        &self,
        runtime_id: &str,
        previous: Option<u64>,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<JournalOperation> {
        self.backend
            .write_checkpoint(runtime_id, previous, snapshot, true)
    }

    /// Read exact `runtime_id` and `operation_id`; return historical evidence or absence.
    /// 读取精确 `runtime_id` 和 `operation_id`；返回历史证据或缺失。
    pub fn get(
        &self,
        runtime_id: &str,
        operation_id: &str,
    ) -> EmbeddedResult<Option<JournalOperation>> {
        self.backend.get(runtime_id, operation_id)
    }

    /// Enumerate one record after `after`; return the next binary-ordered identity or exhaustion.
    /// 枚举 `after` 之后的一条记录；返回下一个二进制排序身份或结束。
    pub fn next(&self, after: Option<(&str, &str)>) -> EmbeddedResult<Option<JournalOperation>> {
        self.backend.next(after)
    }

    /// Forget exact identity at `expected_revision`; return the host's durable deletion result.
    /// 遗忘 `expected_revision` 处的精确身份；返回宿主的持久删除结果。
    pub fn forget(
        &self,
        runtime_id: &str,
        operation_id: &str,
        expected_revision: u64,
    ) -> EmbeddedResult<()> {
        self.backend
            .forget(runtime_id, operation_id, expected_revision)
    }

    /// Apply final `resolution` to exact identity/revision; return the durable successor revision.
    /// 为精确身份及修订应用最终 `resolution`；返回持久后继修订。
    pub fn reconcile(
        &self,
        runtime_id: &str,
        operation_id: &str,
        expected_revision: u64,
        resolution: &OperationReconciliation,
    ) -> EmbeddedResult<u64> {
        self.backend
            .reconcile(runtime_id, operation_id, expected_revision, resolution)
    }

    /// Request validated recovery of the same backend; return whether recovery completed.
    /// 请求校验恢复同一个后端；返回是否完成了恢复。
    pub fn recover_storage(&self) -> EmbeddedResult<bool> {
        self.backend.recover_storage()
    }

    /// Hold the real adapter gate for tests; return a guard that releases it on drop.
    /// 为测试持有真实适配器门禁；返回释放时解除门禁的保护对象。
    #[cfg(test)]
    pub(crate) fn block_for_test(&self) -> impl Drop + '_ {
        self.test_sqlite
            .as_ref()
            .expect("SQLite test adapter")
            .block_for_test()
    }

    /// Lose the next real SQLite confirmation according to `committed`; used only by storage tests.
    /// 根据 `committed` 丢失下次真实 SQLite 确认；仅用于存储测试。
    #[cfg(test)]
    pub(crate) fn lose_next_confirmation_for_test(&self, committed: bool) {
        self.test_sqlite
            .as_ref()
            .expect("SQLite test adapter")
            .lose_next_confirmation_for_test(committed);
    }
}
