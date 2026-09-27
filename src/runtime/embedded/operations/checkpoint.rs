//! Explicit checkpoint ownership for blocking execution and nonblocking supervision.
//! 用于阻塞执行和非阻塞监督的显式检查点所有权。

use super::*;
use crate::runtime::embedded::{
    JournalWritePhase, JournalWriteReceipt, JournalWriteSnapshot, OperationJournalWorker,
};
use std::sync::TryLockError;

/// Host-selected storage strategy; direct and queued persistence are explicit constructor choices.
/// 宿主选择的存储策略；直接持久化和队列持久化由构造入口显式区分。
#[derive(Clone)]
pub(super) enum HistoryBackend {
    /// Synchronous low-level journal for callers that own their blocking execution context.
    /// 供拥有阻塞执行上下文的调用方使用的同步底层日志。
    Direct(Arc<OperationJournal>),
    /// Shared bounded writer whose receipts can be observed without waiting for disk.
    /// 可在不等待磁盘时观测回执的共享有界写入者。
    Queued(Arc<OperationJournalWorker>),
}

impl HistoryBackend {
    /// Persist `snapshot` for `runtime_id` against exact `revision`, returning its acknowledged successor.
    /// 为 `runtime_id` 按精确 `revision` 持久化 `snapshot`，返回确认的后继修订。
    pub(super) fn checkpoint(
        &self,
        runtime_id: &str,
        revision: Option<u64>,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<u64> {
        match self {
            Self::Direct(journal) => match revision {
                None => journal.insert(runtime_id, snapshot),
                Some(previous) => journal.replace(runtime_id, previous, snapshot),
            }
            .map(|record| record.revision),
            Self::Queued(writer) => {
                // This explicitly synchronous path may only run on an execution thread.
                // 此显式同步路径只能在执行线程运行。
                let receipt = writer.submit(runtime_id, revision, Arc::new(snapshot.clone()))?;
                acknowledged(&receipt.wait_until_completed()?)
            }
        }
    }
}

impl OperationHistory {
    /// Return whether this history explicitly selected the nonblocking-capable writer.
    /// 返回此历史是否显式选择了支持非阻塞观测的写入者。
    pub(super) fn is_queued(&self) -> bool {
        matches!(self.backend, HistoryBackend::Queued(_))
    }

    /// Submit one immutable `snapshot` using the last acknowledged revision; never wait for disk.
    /// 使用最后确认修订提交不可变 `snapshot`；绝不等待磁盘。
    fn submit(&self, snapshot: Arc<OperationSnapshot>) -> EmbeddedResult<JournalWriteReceipt> {
        // Reject misuse before touching a synchronous journal from a control thread.
        // 在控制线程接触同步日志前拒绝错误用法。
        let HistoryBackend::Queued(writer) = &self.backend else {
            return Err(EmbeddedError::invalid(
                "nonblocking checkpoints require a journal worker",
            ));
        };
        // Owner mutation ordering ensures no second checkpoint races this captured revision.
        // 所有者变更顺序确保不存在与捕获修订竞争的第二个检查点。
        let revision = self.revision.lock().map_err(|_| poisoned())?;
        writer.submit(&self.runtime_id, *revision, snapshot)
    }

    /// Accept `next` only for the still-current `previous` revision from the original receipt.
    /// 仅当原回执的 `previous` 修订仍为当前值时接受 `next`。
    fn accept(&self, previous: Option<u64>, next: u64) -> EmbeddedResult<()> {
        // The revision remains separate from the public observation mutex.
        // 修订号继续与公开观测互斥锁分离。
        let mut revision = self.revision.lock().map_err(|_| poisoned())?;
        if *revision != previous {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation checkpoint acknowledgement is out of order",
            ));
        }
        *revision = Some(next);
        Ok(())
    }
}

/// State of one original checkpoint attempt; failed polling never creates another disk write.
/// 单个原始检查点尝试的状态；失败轮询绝不创建另一次磁盘写入。
enum Attempt {
    /// No attempt has been submitted yet.
    /// 尚未提交尝试。
    New,
    /// Actual worker receipt retains execution ownership until acknowledgement.
    /// 真实工作线程回执在确认前保留执行所有权。
    Submitted(JournalWriteReceipt),
    /// Known completed rejection or rejected admission, retained until explicit retry.
    /// 已知完成的拒绝或入场拒绝，保留至显式重试。
    Failed(EmbeddedError),
    /// Receipt observation failed; keep actual ownership and refuse an unproven retry.
    /// 回执观测失败；保留真实所有权并拒绝缺乏证明的重试。
    Unobservable {
        /// The original receipt cannot be discarded as if execution had ended.
        /// 不能将原始回执当作执行已经结束而丢弃。
        _receipt: JournalWriteReceipt,
        /// Fixed observation failure returned on every subsequent poll.
        /// 每次后续轮询都返回的固定观测故障。
        error: EmbeddedError,
    },
}

/// Immutable phase/result candidate plus its unique disk attempt; polling cannot replace the candidate.
/// 不可变阶段或结果候选及其唯一磁盘尝试；轮询不能替换候选。
pub(super) struct PendingCheckpoint {
    /// The exact owned snapshot submitted to storage, never reconstructed from newer observations.
    /// 提交到存储的精确自有快照，绝不从较新观测重新构造。
    snapshot: Arc<OperationSnapshot>,
    /// Retained attempt state controls first submission, observation and explicit retry.
    /// 保留的尝试状态控制首次提交、观测和显式重试。
    attempt: Attempt,
}

impl PendingCheckpoint {
    /// Retain immutable `snapshot` before the first bounded queue admission.
    /// 在首次有界队列入场前保留不可变 `snapshot`。
    fn new(snapshot: Arc<OperationSnapshot>) -> Self {
        Self {
            snapshot,
            attempt: Attempt::New,
        }
    }

    /// Drive this exact checkpoint against `history`; `wait` blocks only execution callers.
    /// 针对 `history` 推进此精确检查点；`wait` 仅让执行调用方阻塞。
    /// `retry` permits one new attempt after a known failure; normal polling never resubmits.
    /// `retry` 允许已知失败后新尝试一次；普通轮询绝不重新提交。
    fn drive(
        &mut self,
        history: &OperationHistory,
        wait: bool,
        retry: bool,
    ) -> EmbeddedResult<bool> {
        if retry && matches!(self.attempt, Attempt::Failed(_)) {
            self.attempt = Attempt::New;
        }
        if matches!(self.attempt, Attempt::New) {
            self.attempt = match history.submit(Arc::clone(&self.snapshot)) {
                Ok(receipt) => Attempt::Submitted(receipt),
                Err(error) => Attempt::Failed(error),
            };
        }
        // Borrow the actual receipt until its observation has been captured.
        // 保持借用真实回执，直至捕获其观测。
        let receipt = match &self.attempt {
            Attempt::Submitted(receipt) => receipt,
            Attempt::Failed(error) | Attempt::Unobservable { error, .. } => {
                return Err(error.clone());
            }
            Attempt::New => unreachable!("initial queue admission resolves its attempt state"),
        };
        // Preserve the exact compare-and-swap predecessor, independent of concurrent public cancellation.
        // 保留精确比较交换前驱，独立于并发公开取消。
        let previous = receipt.expected_revision();
        // Waiting uses the receipt's local condition variable, never a scheduler or public state lock.
        // 等待使用回执本地条件变量，绝不使用调度器或公开状态锁。
        let observed = if wait {
            receipt.wait_until_completed()
        } else {
            receipt.snapshot()
        };
        // A poisoned observation cannot prove that the admitted transaction stopped.
        // 中毒观测不能证明已接纳事务已经停止。
        let observed = match observed {
            Ok(observed) => observed,
            Err(error) => {
                // Clone actual ownership before replacing its failed observation state.
                // 替换失败观测状态前克隆真实所有权。
                let retained = receipt.clone();
                self.attempt = Attempt::Unobservable {
                    _receipt: retained,
                    error: error.clone(),
                };
                return Err(error);
            }
        };
        if observed.phase != JournalWritePhase::Completed {
            return Ok(false);
        }
        match acknowledged(&observed) {
            Ok(next) => {
                history.accept(previous, next)?;
                Ok(true)
            }
            Err(error) => {
                // Completed failure releases the worker receipt, but keeps this original candidate and error.
                // 完成失败释放工作线程回执，但保留此原始候选和错误。
                self.attempt = Attempt::Failed(error.clone());
                Err(error)
            }
        }
    }
}

impl OperationOwner {
    /// Start or observe nonterminal `phase` without waiting for disk or a competing owner transition.
    /// 开始或观测非终态 `phase`，不等待磁盘或竞争中的所有者变更。
    /// Return false while pending; retain failures until an explicit retry of the original phase.
    /// 待完成时返回假；保留故障直至显式重试原始阶段。
    pub fn poll_advance(&self, phase: OperationPhase) -> EmbeddedResult<bool> {
        if self.pending_completion.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has a retained terminal checkpoint",
            ));
        }
        // Direct journals cannot silently turn a control-thread poll into blocking I/O.
        // 直接日志不能将控制线程轮询静默变成阻塞 I/O。
        if self
            .operation
            .history
            .as_ref()
            .is_some_and(|history| !history.is_queued())
        {
            return Err(EmbeddedError::invalid(
                "nonblocking checkpoints require a journal worker",
            ));
        }
        // A concurrent blocking execution transition is observed as pending, never waited upon.
        // 并发阻塞执行变更被观测为待完成，绝不等待它。
        let mut transition = match self.operation.transition.try_lock() {
            Ok(transition) => transition,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
        };
        if self.operation.history.is_none() {
            advance_snapshot(&mut *self.operation.lock()?, phase)?;
            return Ok(true);
        }
        self.drive_advance(&mut transition, phase, false, false)
    }

    /// Explicitly retry the retained nonterminal checkpoint without rerunning plugin code or waiting.
    /// 显式重试保留的非终态检查点，不重新运行插件代码或等待。
    pub fn retry_advance(&self) -> EmbeddedResult<bool> {
        // The same gate protects ordinary advancement and its explicit repair path.
        // 同一门禁保护普通推进及其显式修复路径。
        let mut transition = match self.operation.transition.try_lock() {
            Ok(transition) => transition,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
        };
        // Identity and phase come from the retained candidate, never from a replacement argument.
        // 身份及阶段来自保留候选，绝不来自替代参数。
        let phase = transition
            .as_ref()
            .ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "operation has no retained phase checkpoint",
                )
            })?
            .snapshot
            .phase;
        self.drive_advance(&mut transition, phase, false, true)
    }

    /// Drive exact `phase` under the already-held mutation gate, publishing only an acknowledged snapshot.
    /// 在已持有的变更门禁下推进精确 `phase`，仅发布已确认快照。
    pub(super) fn drive_advance(
        &self,
        transition: &mut Option<PendingCheckpoint>,
        phase: OperationPhase,
        wait: bool,
        retry: bool,
    ) -> EmbeddedResult<bool> {
        // An admitted phase can neither be replaced nor overtaken by a newer transition.
        // 已入场阶段既不能被替换，也不能被较新变更超越。
        if let Some(checkpoint) = transition.as_ref() {
            if checkpoint.snapshot.phase != phase {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "observe or retry the original phase checkpoint before another transition",
                ));
            }
        } else {
            // Capture after acquiring the sole phase mutation gate so a stale snapshot cannot overtake a newer one.
            // 获取唯一阶段变更门禁后才捕获快照，避免旧快照超越较新快照。
            let mut snapshot = self.operation.lock()?.clone();
            advance_snapshot(&mut snapshot, phase)?;
            snapshot = self.operation.project(snapshot)?;
            snapshot.phase = phase;
            *transition = Some(PendingCheckpoint::new(Arc::new(snapshot)));
        }
        // Queue-backed registries install immutable history before exposing an operation.
        // 队列注册表在暴露操作前安装不可变历史。
        let history = self
            .operation
            .history
            .as_ref()
            .expect("queued operation has history");
        // The retained candidate remains owned even when bounded queue admission itself fails.
        // 即使有界队列入场本身失败，仍保有候选所有权。
        let checkpoint = transition.as_mut().expect("phase candidate retained");
        if !checkpoint.drive(history, wait, retry)? {
            return Ok(false);
        }
        *self.operation.lock()? = checkpoint.snapshot.as_ref().clone();
        transition.take();
        self.operation.changed.notify_all();
        Ok(true)
    }

    /// Freeze bounded business `result` and trusted `effects` after cleanup, without submitting disk I/O.
    /// 清理后冻结有界业务 `result` 和可信 `effects`，不提交磁盘 I/O。
    /// A second preparation cannot replace the original outcome; completion still requires a receipt.
    /// 第二次准备不能替换原始结果；完成仍需回执。
    pub fn prepare_completion(
        &mut self,
        result: EmbeddedResult<Value>,
        effects: EffectState,
    ) -> EmbeddedResult<()> {
        if self.pending_completion.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "retry the original retained terminal checkpoint before supplying another outcome",
            ));
        }
        // Preparation cannot bypass an unfinished or failed phase checkpoint.
        // 准备不能绕过未完成或失败的阶段检查点。
        let transition = self
            .operation
            .transition
            .try_lock()
            .map_err(|error| match error {
                TryLockError::WouldBlock => EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "operation transition is still owned",
                ),
                TryLockError::Poisoned(_) => poisoned(),
            })?;
        if transition.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has a retained phase checkpoint",
            ));
        }
        // Bound retained application evidence before constructing the immutable completion attempt.
        // 构造不可变完成尝试前限制保留的应用证据。
        let encoded = match &result {
            Ok(value) => json_size(value, self.operation.max_value_bytes),
            Err(error) => json_size(error, self.operation.max_value_bytes),
        };
        // Keep fixed diagnostics rather than retain oversized business values.
        // 保留固定诊断，不保留超大业务值。
        let result = match encoded {
            Ok(_) => result,
            Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => {
                Err(EmbeddedError::new(
                    EmbeddedErrorCode::CapacityExceeded,
                    "operation result exceeds the configured byte limit",
                ))
            }
            Err(_) => Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation result serialization failed",
            )),
        };
        // The public observation remains Cleaning until terminal persistence is acknowledged.
        // 终态持久化确认前，公开观测保持 Cleaning。
        let mut snapshot = self.operation.lock()?.clone();
        if snapshot.phase != OperationPhase::Cleaning {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation completion requires finished execution and cleanup",
            ));
        }
        snapshot.host_effects = self.operation.effects.seal()?;
        snapshot.effects = merge_effects(effects, &snapshot.host_effects);
        snapshot.cancellation_requested = self.operation.control.is_cancelled();
        match result {
            Ok(value) => {
                snapshot.phase = OperationPhase::Succeeded;
                snapshot.value = Some(value);
            }
            Err(error) => {
                snapshot.phase = if error.code == EmbeddedErrorCode::Cancelled {
                    OperationPhase::Cancelled
                } else {
                    OperationPhase::Failed
                };
                snapshot.error = Some(error);
            }
        }
        self.pending_completion = Some(Arc::new(snapshot));
        Ok(())
    }

    /// Submit or observe the original prepared completion without disk waiting or automatic retry.
    /// 提交或观测原始已准备完成状态，不等待磁盘，也不自动重试。
    pub fn poll_completion(&mut self) -> EmbeddedResult<bool> {
        if self.operation.history.is_none() {
            self.retry_completion()?;
            return Ok(true);
        }
        self.drive_completion(false, false)
    }

    /// Submit or explicitly retry the original terminal write without waiting; in-flight attempts are only observed.
    /// 非阻塞提交或显式重试原始终态写入；仍在途的尝试只被观测。
    pub fn retry_completion_nonblocking(&mut self) -> EmbeddedResult<bool> {
        if self.operation.history.is_none() {
            self.retry_completion()?;
            return Ok(true);
        }
        self.drive_completion(false, true)
    }

    /// Drive the frozen terminal candidate with explicit `wait` and `retry` policy; publish only after acknowledgement.
    /// 按显式 `wait` 及 `retry` 策略推进冻结终态候选；仅在确认后发布。
    pub(super) fn drive_completion(&mut self, wait: bool, retry: bool) -> EmbeddedResult<bool> {
        // Reject synchronous storage before any control-thread submission can occur.
        // 在任何控制线程提交发生前拒绝同步存储。
        let history = self
            .operation
            .history
            .as_ref()
            .expect("persistent completion has history");
        if !history.is_queued() {
            return Err(EmbeddedError::invalid(
                "nonblocking checkpoints require a journal worker",
            ));
        }
        if self.operation.lock()?.phase != OperationPhase::Cleaning {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation completion requires finished execution and cleanup",
            ));
        }
        // This immutable result remains the sole source even when cancellation changes during disk waiting.
        // 即使磁盘等待期间取消发生变化，此不可变结果仍是唯一来源。
        let snapshot = self.pending_completion.as_ref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no retained terminal checkpoint",
            )
        })?;
        // First observation creates one attempt; subsequent observations keep its exact identity and bytes.
        // 首次观测创建一次尝试；后续观测保留其精确身份及字节。
        let checkpoint = self
            .completion_checkpoint
            .get_or_insert_with(|| PendingCheckpoint::new(Arc::clone(snapshot)));
        if !checkpoint.drive(history, wait, retry)? {
            return Ok(false);
        }
        // Drop redundant receipt/candidate ownership before moving the terminal result when it is unique.
        // 结果唯一时移动终态结果之前，先释放冗余回执和候选所有权。
        let mut current = self.operation.lock()?;
        self.completion_checkpoint.take();
        *current = Arc::unwrap_or_clone(
            self.pending_completion
                .take()
                .expect("acknowledged completion retains its original result"),
        );
        self.operation.changed.notify_all();
        Ok(true)
    }
}

impl OperationRegistry {
    /// Create a registry backed by exact shared `writer` and fresh trusted `runtime_id` using `config` limits.
    /// 按 `config` 上限，以精确共享 `writer` 和全新可信 `runtime_id` 创建注册表。
    /// The host owns writer close/join; admission stays memory-only and owner polling never waits for disk.
    /// 宿主拥有写入者关闭及等待责任；入场仍仅在内存进行，所有者轮询绝不等待磁盘。
    pub fn with_journal_worker(
        runtime_id: String,
        config: &EmbeddedRuntimeConfig,
        writer: Arc<OperationJournalWorker>,
    ) -> EmbeddedResult<Self> {
        // Reuse the sole registry configuration validator before selecting queued history.
        // 选择队列历史前复用唯一注册表配置校验器。
        let mut registry = Self::new(runtime_id, config)?;
        registry.journal = Some(HistoryBackend::Queued(writer));
        Ok(registry)
    }
}

/// Extract a completed successful revision from `snapshot`, preserving explicit storage failure.
/// 从 `snapshot` 提取成功完成的修订号，保留明确存储故障。
fn acknowledged(snapshot: &JournalWriteSnapshot) -> EmbeddedResult<u64> {
    match (snapshot.phase, snapshot.revision, snapshot.error.as_ref()) {
        (JournalWritePhase::Completed, Some(revision), None) => Ok(revision),
        (JournalWritePhase::Completed, None, Some(error)) => Err(error.clone()),
        _ => Err(EmbeddedError::new(
            EmbeddedErrorCode::Internal,
            "journal acknowledgement is inconsistent",
        )),
    }
}

/// Report a fixed mutation-gate failure without exposing application values.
/// 报告固定变更门禁故障，不暴露应用值。
pub(super) fn poisoned() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "operation checkpoint lock is poisoned",
    )
}
