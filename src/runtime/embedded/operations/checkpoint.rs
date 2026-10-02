//! Explicit checkpoint ownership for blocking execution and nonblocking supervision.
//! 用于阻塞执行和非阻塞监督的显式检查点所有权。

use super::*;
use crate::runtime::embedded::{
    CheckpointRetryState, JournalWritePhase, JournalWriteReceipt, JournalWriteSnapshot,
    OperationJournalWorker, OperationPersistenceFailure,
};
use std::sync::TryLockError;

mod outcome;

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
            Self::Direct(journal) => journal
                .checkpoint(runtime_id, revision, snapshot)
                .map(|record| record.revision),
            Self::Queued(writer) => {
                // This explicitly synchronous path may only run on an execution thread.
                // 此显式同步路径只能在执行线程运行。
                let receipt =
                    writer.submit_checkpoint(runtime_id, revision, Arc::new(snapshot.clone()))?;
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
        writer.submit_checkpoint(&self.runtime_id, *revision, snapshot)
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
    /// Storage revision is acknowledged, but terminal publication can still await scheduler bookkeeping.
    /// 存储修订已确认，但终态发布仍可能等待调度记账。
    Acknowledged,
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

/// Immutable phase/result/intent candidate plus its unique disk attempt; polling cannot replace the candidate.
/// 不可变阶段、结果或意图候选及其唯一磁盘尝试；轮询不能替换候选。
pub(super) struct PendingCheckpoint {
    /// Host checkpoints retain an exact purpose; acknowledgement cannot publish a fictitious handler lifecycle.
    /// 宿主检查点保留精确用途；确认不能发布虚构的处理器生命周期。
    effect: Option<CheckpointEffect>,
    /// Last failed attempt stays observable throughout its explicitly requested retry.
    /// 上次失败尝试在显式请求的重试全程保持可观测。
    failure: Option<EmbeddedError>,
    /// Retry requests belong to this exact immutable candidate, not a mutable operation-wide flag.
    /// 重试请求属于此精确不可变候选，不属于可变的操作级标记。
    retry: CheckpointRetryState,
    /// The exact owned snapshot submitted to storage, never reconstructed from newer observations.
    /// 提交到存储的精确自有快照，绝不从较新观测重新构造。
    snapshot: Arc<OperationSnapshot>,
    /// Retained attempt state controls first submission, observation and explicit retry.
    /// 保留的尝试状态控制首次提交、观测和显式重试。
    attempt: Attempt,
}

/// Distinguish pre-execution permission from a returned handler's trusted outcome evidence.
/// 区分执行前许可与已返回处理器的可信结果证据。
enum CheckpointEffect {
    /// The exact effect may execute only after this intent is acknowledged.
    /// 此意图确认后，精确副作用才可执行。
    Start(String),
    /// The exact effect's result and admission remain owned until confirmation is acknowledged.
    /// 在确认得到回执前，精确副作用的结果及入场许可继续被拥有。
    Outcome(String),
}

impl PendingCheckpoint {
    /// Identify an owner lifecycle candidate without exposing host-effect checkpoint internals.
    /// 标识所有者生命周期候选，不暴露宿主副作用检查点内部结构。
    pub(super) fn is_lifecycle_phase(&self, phase: OperationPhase) -> bool {
        self.snapshot.phase == phase && self.effect.is_none()
    }

    /// Retain immutable `snapshot` before the first bounded queue admission.
    /// 在首次有界队列入场前保留不可变 `snapshot`。
    pub(super) fn new(snapshot: Arc<OperationSnapshot>) -> Self {
        Self {
            effect: None,
            failure: None,
            retry: CheckpointRetryState::Waiting,
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
        // Consume explicit requests only for this already retained candidate.
        // 仅为此已保留候选消耗显式请求。
        let retry = retry || self.retry == CheckpointRetryState::Requested;
        if retry {
            self.retry = CheckpointRetryState::Retrying;
        }
        // Preserve failure identity while waiting for the retry's original receipt.
        // 等待重试的原始回执期间保留故障身份。
        let result = self.drive_attempt(history, wait, retry);
        match &result {
            Ok(true) => self.failure = None,
            Ok(false) => {}
            Err(error) => {
                self.failure = Some(error.clone());
                self.retry = CheckpointRetryState::Waiting;
            }
        }
        result
    }

    /// Submit or observe one retained attempt; `retry` never replaces its immutable snapshot.
    /// 提交或观测一次保留尝试；`retry` 绝不替换其不可变快照。
    fn drive_attempt(
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
            Attempt::Acknowledged => return Ok(true),
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
                self.attempt = Attempt::Acknowledged;
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

impl Operation {
    /// Persist the exact ledger record's execution intent for `effect_id`; `wait` selects execution-thread waiting.
    /// 持久化 `effect_id` 对应精确账本记录的执行意图；`wait` 选择执行线程等待。
    /// Share phase ordering and revision authority; acknowledging intent does not itself dispatch a handler.
    /// 共享阶段排序及修订权威；确认意图本身不分发处理器。
    pub(in crate::runtime::embedded) fn checkpoint_effect_start(
        &self,
        effect_id: &str,
        wait: bool,
    ) -> EmbeddedResult<bool> {
        // This path is reached only through the immutable persistent ledger binding installed at admission.
        // 此路径仅通过入场时安装的不可变持久账本绑定进入。
        let history = self.history.as_ref().ok_or_else(poisoned)?;
        if !history.is_queued() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "persistent host dispatch requires a journal worker",
            ));
        }
        // Nonblocking callers never wait for another execution thread's checkpoint gate.
        // 非阻塞调用方绝不等待另一执行线程的检查点门禁。
        let mut transition = if wait {
            self.transition.lock().map_err(|_| poisoned())?
        } else {
            match self.transition.try_lock() {
                Ok(transition) => transition,
                Err(TryLockError::WouldBlock) => return Ok(false),
                Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
            }
        };
        if let Some(checkpoint) = transition.as_mut() {
            // A retained phase belongs to its owner; a callback must not publish that owner's transition.
            // 保留阶段属于其所有者；回调不能发布该所有者的变更。
            if !matches!(checkpoint.effect, Some(CheckpointEffect::Start(_))) {
                return if wait {
                    Err(EmbeddedError::new(
                        EmbeddedErrorCode::Busy,
                        "operation phase checkpoint is pending",
                    ))
                } else {
                    Ok(false)
                };
            }
            // Drain an earlier intent even if its requester was cancelled; never retry a known failure here.
            // 即使原请求方已取消也排空较早意图；此处绝不重试已知失败。
            let same_effect =
                matches!(&checkpoint.effect, Some(CheckpointEffect::Start(id)) if id == effect_id);
            if !checkpoint.drive(history, wait, false)? {
                return Ok(false);
            }
            transition.take();
            if same_effect {
                return Ok(true);
            }
        }
        // Capture only after acquiring the sole mutation gate so an older intent cannot overwrite a newer phase.
        // 仅在取得唯一变更门禁后捕获，避免旧意图覆盖较新阶段。
        let mut snapshot = self.lock()?.clone();
        if snapshot.phase.is_terminal() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "operation is terminal",
            ));
        }
        snapshot.cancellation_requested = self.control.is_cancelled();
        snapshot.host_effects = self.effects.snapshot()?;
        // The exact ledger owns identity and request binding; no application argument supplies either value.
        // 精确账本拥有身份与请求绑定；两者均不由应用参数提供。
        let record = snapshot
            .host_effects
            .iter_mut()
            .find(|record| record.effect_id == effect_id)
            .ok_or_else(poisoned)?;
        if record.phase != super::super::HostEffectPhase::Prepared {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host effect was already dispatched",
            ));
        }
        // Disk records conservative execution permission before the live ledger changes to actual dispatch.
        // 磁盘先记录保守执行许可，实时账本随后才变更为实际分发。
        record.phase = super::super::HostEffectPhase::Running;
        record.effects = EffectState::Unknown;
        snapshot.effects = merge_effects(snapshot.effects, &snapshot.host_effects);
        // Keep the original candidate across queue rejection, cancellation and explicit owner recovery.
        // 跨队列拒绝、取消及显式所有者恢复保留原始候选。
        let mut checkpoint = PendingCheckpoint::new(Arc::new(snapshot));
        checkpoint.effect = Some(CheckpointEffect::Start(effect_id.to_owned()));
        *transition = Some(checkpoint);
        if !transition
            .as_mut()
            .expect("retained execution intent")
            .drive(history, wait, false)?
        {
            return Ok(false);
        }
        transition.take();
        Ok(true)
    }
}

impl OperationOwner {
    /// Inspect the retained phase before scheduler cleanup, without waiting for another transition owner.
    /// 调度清理前检查保留阶段，不等待另一个变更所有者。
    pub(crate) fn pending_phase(&self) -> EmbeddedResult<Option<OperationPhase>> {
        // A busy phase gate is not permission to bypass the original candidate.
        // 阶段门禁忙碌不代表可以绕过原始候选。
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
        // A returned handler still owns confirmation and its actual admission; cleanup cannot consume it.
        // 已返回处理器仍拥有确认及真实入场许可；清理不能消费它。
        if transition.as_ref().is_some_and(|checkpoint| {
            matches!(checkpoint.effect, Some(CheckpointEffect::Outcome(_)))
        }) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "host outcome confirmation is still owned",
            ));
        }
        Ok(transition
            .as_ref()
            .map(|checkpoint| checkpoint.snapshot.phase))
    }

    /// Observe or explicitly retry terminal persistence outside scheduler metadata, without publishing completion.
    /// 在调度元数据之外观测或显式重试终态持久化，不发布完成状态。
    pub(crate) fn poll_completion_checkpoint(&mut self, retry: bool) -> EmbeddedResult<bool> {
        self.drive_completion_checkpoint(false, retry)
    }

    /// Drive terminal storage under explicit `wait` and `retry` policy while keeping publication separate.
    /// 按显式 `wait` 及 `retry` 策略推进终态存储，同时保持发布独立。
    fn drive_completion_checkpoint(&mut self, wait: bool, retry: bool) -> EmbeddedResult<bool> {
        if self.operation.lock()?.phase != OperationPhase::Cleaning {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation completion requires finished execution and cleanup",
            ));
        }
        // Preserve original business evidence and the cancellation observation captured at preparation.
        // 保留原始业务证据及准备时捕获的取消观测。
        let snapshot = self.pending_completion.as_ref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no retained terminal checkpoint",
            )
        })?;
        // Memory mode is ready immediately; direct storage is never silently executed by a poll.
        // 内存模式立即就绪；轮询绝不静默执行直接存储。
        let Some(history) = &self.operation.history else {
            return Ok(true);
        };
        if !history.is_queued() {
            return Err(EmbeddedError::invalid(
                "nonblocking checkpoints require a journal worker",
            ));
        }
        // An acknowledged checkpoint remains ready on repeated observations without advancing revision twice.
        // 已确认检查点在重复观测中保持就绪，不重复推进修订。
        let checkpoint = self
            .completion_checkpoint
            .get_or_insert_with(|| PendingCheckpoint::new(Arc::clone(snapshot)));
        checkpoint.drive(history, wait, retry)
    }

    /// Publish an already acknowledged terminal result with no storage work, allowing atomic scheduler bookkeeping.
    /// 不执行存储工作地发布已确认终态结果，使调度记账可以保持原子性。
    /// Return the notification handoff; the caller must consume it only after releasing all bookkeeping locks.
    /// 返回通知交接；调用方必须在释放全部记账锁后消费它。
    pub(crate) fn publish_completion(&mut self) -> EmbeddedResult<TerminalNotification> {
        if self.operation.history.is_some()
            && !self
                .completion_checkpoint
                .as_ref()
                .is_some_and(|checkpoint| matches!(checkpoint.attempt, Attempt::Acknowledged))
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "terminal checkpoint is not acknowledged",
            ));
        }
        // Acquire public state only after storage is done; the scheduler may hold its metadata lock here.
        // 存储结束后才获取公开状态；调度器可以在此处持有其元数据锁。
        let mut current = self.operation.lock()?;
        if current.phase != OperationPhase::Cleaning || self.pending_completion.is_none() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "operation has no publishable terminal checkpoint",
            ));
        }
        self.completion_checkpoint.take();
        *current = Arc::unwrap_or_clone(
            self.pending_completion
                .take()
                .expect("validated original terminal candidate"),
        );
        self.operation.changed.notify_all();
        Ok(TerminalNotification {
            operation: Arc::clone(&self.operation),
        })
    }

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
        // Intent persistence authorizes a separate live dispatch; recovery alone never dispatches a handler.
        // 意图持久化授权另行执行实时分发；恢复本身绝不分发处理器。
        if matches!(checkpoint.effect, Some(CheckpointEffect::Outcome(_))) {
            // Explicit low-level recovery acknowledges storage, leaving publication to the actual handler owner.
            // 显式低层恢复确认存储，将发布留给真实处理器所有者。
            self.operation.changed.notify_all();
            return Ok(true);
        }
        if checkpoint.effect.is_none() {
            *self.operation.lock()? = checkpoint.snapshot.as_ref().clone();
        }
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
        // Both business and closing outcomes use the same authoritative value-budget normalization.
        // 业务与关闭结果共用同一个权威值预算规范化规则。
        // Move the already-owned bounded outcome instead of cloning its complete value or error.
        // 移动已拥有的有界结果，避免克隆其完整值或错误。
        let result = match OperationOutcome::bounded(result, self.operation.max_value_bytes) {
            OperationOutcome::Succeeded { value } => Ok(value),
            OperationOutcome::Failed { error } => Err(error),
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
        // Closing cannot be skipped or replace the frozen original business result.
        // 不能跳过关闭，也不能替换冻结的原业务结果。
        let result = match &snapshot.finalization {
            Some(finalization) => {
                if !self
                    .finalization
                    .as_ref()
                    .is_some_and(|owner| owner.outcome_prepared)
                {
                    return Err(EmbeddedError::new(
                        EmbeddedErrorCode::Busy,
                        "closing outcome is not acknowledged",
                    ));
                }
                if finalization.business.result() != result {
                    return Err(EmbeddedError::invalid(
                        "completion cannot replace the original business outcome",
                    ));
                }
                match (&finalization.business, &finalization.outcome) {
                    (_, None) => {
                        return Err(EmbeddedError::new(
                            EmbeddedErrorCode::Busy,
                            "closing outcome is missing",
                        ));
                    }
                    (OperationOutcome::Failed { error }, _) => Err(error.clone()),
                    (_, Some(OperationOutcome::Failed { error })) => Err(error.clone()),
                    (_, Some(OperationOutcome::Succeeded { .. })) => result,
                }
            }
            None => result,
        };
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
        if !self.drive_completion_checkpoint(wait, retry)? {
            return Ok(false);
        }
        // This direct owner path holds no scheduler metadata when consuming terminal notification.
        // 此直接所有者路径消费终态通知时不持有调度元数据。
        self.publish_completion()?.notify();
        Ok(true)
    }
}

impl OperationRegistry {
    /// Report the explicit queued-history selection without maintaining another runtime mode flag.
    /// 报告显式队列历史选择，不维护另一个运行时模式标记。
    pub(crate) fn has_queued_history(&self) -> bool {
        matches!(self.journal, Some(HistoryBackend::Queued(_)))
    }

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
