use super::registry::PreparedCapability;
use super::{CapabilityCaller, CapabilityOutcome};
use crate::runtime::embedded::retirement::MAINTENANCE_INTERVAL;
use crate::runtime::embedded::value_size::json_size;
use crate::runtime::embedded::{
    EffectState, EmbeddedError, EmbeddedErrorCode, EmbeddedResult, EmbeddedRuntimeConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

/// SDK request copied from an admitted, authenticated invocation.
/// 从已入场、已认证调用复制的 SDK 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRequest {
    /// Original operation effect record, absent only for untracked low-level calls.
    /// 原始操作副作用记录，仅未跟踪低层调用省略。
    pub effect_id: Option<String>,
    /// Never-reused identity used for completion and host-side deduplication.
    /// 用于完成与宿主侧去重、绝不复用的身份。
    pub request_id: String,
    /// Exact registration, independent from later replacement by name.
    /// 精确注册，独立于后续按名称替换。
    pub registration_id: String,
    /// Declared capability name.
    /// 声明的能力名称。
    pub name: String,
    /// Declared semantic interface version.
    /// 声明的语义接口版本。
    pub version: String,
    /// Trusted host identity, separate from business arguments.
    /// 可信宿主身份，独立于业务参数。
    pub caller: CapabilityCaller,
    /// Validated structured business arguments.
    /// 已校验的结构化业务参数。
    pub arguments: Value,
    /// Advisory remaining duration; the core retains the original deadline.
    /// 建议剩余时长；核心保留原始截止时间。
    pub remaining_ms: u64,
}

/// Observable request phase; cancellation does not imply handler termination.
/// 可观察请求阶段；取消不代表处理器已经终止。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRequestPhase {
    /// No SDK handler has received this request.
    /// 尚无 SDK 处理器收到此请求。
    Queued,
    /// Exactly one SDK consumer owns execution.
    /// 精确一个 SDK 消费者拥有执行权。
    Dispatched,
    /// An exclusive owner validates the result and releases admission outside locks.
    /// 独占所有者在锁外校验结果并释放入场许可。
    Completing,
    /// The actual handler has finished and admission is released.
    /// 真实处理器已结束且入场许可已释放。
    Completed,
}

/// Live request status for SDK cancellation and orderly runtime shutdown.
/// 用于 SDK 取消与运行时有序关闭的实时请求状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRequestStatus {
    /// Exact request identity.
    /// 精确请求身份。
    pub request_id: String,
    /// Actual execution lifecycle.
    /// 真实执行生命周期。
    pub phase: HostRequestPhase,
    /// Cooperative cancellation reason, preserved until actual completion.
    /// 协作取消原因，保留到真实完成。
    pub cancellation: Option<EmbeddedError>,
}

/// All mutable request metadata belongs to the broker's single short lock.
/// 全部可变请求元数据归属于代理的单个短时锁。
struct RequestRecord {
    /// Exact request delivered at most once.
    /// 最多投递一次的精确请求。
    request: HostRequest,
    /// Original invocation and admission, moved out only by the completion owner.
    /// 原始调用与入场许可，仅由完成所有者移出。
    prepared: Option<PreparedCapability>,
    /// Retained terminal outcome.
    /// 保留的终态结果。
    outcome: Option<CapabilityOutcome>,
    /// Actual phase, never inferred from cancellation.
    /// 真实阶段，绝不从取消推断。
    phase: HostRequestPhase,
    /// First cancellation reason wins deterministically.
    /// 首个取消原因确定性地生效。
    cancellation: Option<EmbeddedError>,
    /// A live Rust waiter still owns result retrieval.
    /// 活动 Rust 等待方仍拥有结果取回权。
    waiter_alive: bool,
    /// Reserved wire bytes including maximum output capacity.
    /// 包含最大输出容量的预留传输字节数。
    bytes: usize,
}

/// Bounded retained records and single-delivery ready queue.
/// 有界保留记录与单次投递就绪队列。
struct BrokerState {
    /// Monotonic request counter; overflow rejects admission.
    /// 单调请求计数器；溢出时拒绝入场。
    sequence: u64,
    /// Permanent shutdown gate.
    /// 永久关闭门。
    closing: bool,
    /// Ready identities owned by this runtime only.
    /// 仅由此运行时拥有的就绪身份。
    ready: VecDeque<String>,
    /// Includes delivered and completed requests until their waiter releases them.
    /// 包含已投递与已完成请求，直到等待方释放。
    records: BTreeMap<String, RequestRecord>,
    /// Recent completed identities distinguish duplicate completion from expiry.
    /// 近期完成身份用于区分重复完成与过期。
    tombstones: VecDeque<String>,
    /// Reserved bytes for all retained requests.
    /// 全部保留请求的预留字节数。
    bytes: usize,
}

/// Instance-owned reliable SDK control channel, independent from VM execution workers.
/// 实例拥有的可靠 SDK 控制通道，独立于 VM 执行工作线程。
pub struct HostRequestBroker {
    /// Trusted runtime namespace.
    /// 可信运行时命名空间。
    runtime_id: String,
    /// Hard retained-record maximum.
    /// 保留记录硬上限。
    max_records: usize,
    /// Hard aggregate request and output reservation maximum.
    /// 聚合请求与输出预留硬上限。
    max_bytes: usize,
    /// Short metadata lock; no host callback or permit destructor runs under it.
    /// 短时元数据锁；其内不运行宿主回调或许可析构器。
    state: Mutex<BrokerState>,
    /// Terminal publication wakes existing synchronous VM waiters.
    /// 终态发布唤醒已有同步 VM 等待方。
    changed: Condvar,
}

impl HostRequestBroker {
    /// Create an empty broker from already validated runtime limits.
    /// 根据已校验运行时上限创建空代理。
    pub(super) fn new(runtime_id: String, config: &EmbeddedRuntimeConfig) -> Arc<Self> {
        Arc::new(Self {
            runtime_id,
            max_records: config.max_host_requests,
            max_bytes: config.max_host_request_bytes,
            state: Mutex::new(BrokerState {
                sequence: 0,
                closing: false,
                ready: VecDeque::new(),
                records: BTreeMap::new(),
                tombstones: VecDeque::new(),
                bytes: 0,
            }),
            changed: Condvar::new(),
        })
    }

    /// Retain `prepared` until real completion; return its unique result owner.
    /// 保留 `prepared` 到真实完成；返回其唯一结果所有者。
    pub(super) fn submit(
        self: &Arc<Self>,
        prepared: PreparedCapability,
    ) -> EmbeddedResult<HostRequestHandle> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.closing {
            return Err(closed());
        }
        if state.records.len() >= self.max_records {
            return Err(capacity());
        }
        let sequence = state.sequence.checked_add(1).ok_or_else(capacity)?;
        let id = format!("{}:host:{sequence}", self.runtime_id);
        let request = HostRequest {
            effect_id: prepared.invocation.effect_id.clone(),
            request_id: id.clone(),
            registration_id: prepared.entry.id.clone(),
            name: prepared.entry.descriptor.name.clone(),
            version: prepared.entry.descriptor.version.clone(),
            caller: prepared.invocation.caller.clone(),
            arguments: prepared.invocation.arguments.clone(),
            remaining_ms: prepared.invocation.budget.remaining_ms(),
        };
        let bytes = json_size(&request, self.max_bytes)?
            .checked_add(prepared.entry.descriptor.max_output_bytes)
            .ok_or_else(capacity)?;
        let total = state
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(capacity)?;
        // Evidence observers can see the request identity before queue publication; burn it even on later failure.
        // 证据观察者可在队列发布前看到请求身份；即使随后失败也消耗此身份。
        state.sequence = sequence;
        prepared.effect.bind_request(&id)?;
        // Publish under the exact entry gate so unregister cannot miss a previously admitted request.
        // 在精确条目门内发布，使注销不能遗漏此前已入场请求。
        let entry = Arc::clone(&prepared.entry);
        entry.with_dispatch_gate(|| {
            state.bytes = total;
            state.ready.push_back(id.clone());
            state.records.insert(
                id.clone(),
                RequestRecord {
                    request,
                    prepared: Some(prepared),
                    outcome: None,
                    phase: HostRequestPhase::Queued,
                    cancellation: None,
                    waiter_alive: true,
                    bytes,
                },
            );
        })?;
        Ok(HostRequestHandle {
            broker: Arc::clone(self),
            id,
        })
    }

    /// Deliver up to `limit` requests once; cancelled queued requests never reach SDK code.
    /// 单次投递至多 `limit` 个请求；已取消的排队请求绝不进入 SDK 代码。
    pub fn take(&self, limit: usize) -> EmbeddedResult<Vec<HostRequest>> {
        if limit == 0 || limit > self.max_records {
            return Err(EmbeddedError::invalid("invalid host request batch size"));
        }
        // Return already-delivered requests even if a later attempt encounters an error.
        // 即使后续尝试遇到错误，也返回已投递请求。
        let mut requests = Vec::new();
        while requests.len() < limit {
            match self.take_next(|_| requests.try_reserve(1).map_err(|_| capacity())) {
                Ok(Some(request)) => requests.push(request),
                Ok(None) => break,
                Err(error) if requests.is_empty() => return Err(error),
                // The failing request remains pending; the next pump call surfaces its error.
                // 失败请求保持待处理；下次事件泵调用报告其错误。
                Err(_) => break,
            }
        }
        Ok(requests)
    }

    /// Deliver up to `limit` requests as one JSON array bounded by `max_bytes`, including brackets and commas.
    /// 将至多 `limit` 个请求作为一个 JSON 数组投递，由 `max_bytes` 限制且包含方括号与逗号。
    /// Encoding and output allocation precede dispatch; an oversized head remains queued for explicit recovery.
    /// 编码与输出分配先于分发；超大队首保持排队，供显式恢复。
    /// Return every already-delivered prefix even if the next request exceeds the remaining response budget.
    /// 即使下一个请求超过剩余响应预算，也返回全部已投递前缀。
    pub fn take_json(&self, limit: usize, max_bytes: usize) -> EmbeddedResult<Vec<u8>> {
        if limit == 0
            || limit > self.max_records
            || max_bytes < 2
            || max_bytes > isize::MAX as usize
        {
            return Err(EmbeddedError::invalid(
                "invalid host request JSON batch limits",
            ));
        }
        let mut output = Vec::new();
        output.try_reserve_exact(2).map_err(|_| capacity())?;
        output.push(b'[');
        let mut count = 0;
        while count < limit {
            let separator = usize::from(count != 0);
            let remaining = max_bytes.saturating_sub(output.len() + separator + 1);
            let mut encoded = Vec::new();
            let delivery = self.take_next(|request| {
                let size = json_size(request, remaining)?;
                // Reuse scratch storage if a cancelled head is skipped within this attempt.
                // 此尝试内跳过已取消队首时复用临时存储。
                encoded.clear();
                encoded.try_reserve_exact(size).map_err(|_| capacity())?;
                serde_json::to_writer(&mut encoded, request).map_err(|_| poisoned())?;
                // Reserve the delimiter and closing bracket as well, before execution authority can move.
                // 在执行权能够移动前，同时预留分隔符与结束方括号。
                output
                    .try_reserve_exact(encoded.len() + separator + 1)
                    .map_err(|_| capacity())
            });
            match delivery {
                Ok(Some(_)) => {
                    if separator != 0 {
                        output.push(b',');
                    }
                    output.extend_from_slice(&encoded);
                    count += 1;
                }
                Ok(None) => break,
                Err(error) if count == 0 => return Err(error),
                Err(_) => break,
            }
        }
        output.push(b']');
        Ok(output)
    }

    /// Take one request after trusted internal `prepare` succeeds; retain the queue head on delivery failure.
    /// 在可信内部 `prepare` 成功后取得一个请求；交付失败时保留队首。
    fn take_next(
        &self,
        mut prepare: impl FnMut(&HostRequest) -> EmbeddedResult<()>,
    ) -> EmbeddedResult<Option<HostRequest>> {
        loop {
            // The queue and dispatch phase share one authority throughout this transition.
            // 此变更期间队列与分发阶段共享同一个权威。
            let mut state = self.state.lock().map_err(|_| poisoned())?;
            if state.closing {
                return Ok(None);
            }
            let Some(id) = state.ready.front().cloned() else {
                return Ok(None);
            };
            let record = state.records.get_mut(&id).ok_or_else(poisoned)?;
            if record.phase != HostRequestPhase::Queued {
                return Err(poisoned());
            }
            let prepared = record.prepared.as_ref().ok_or_else(poisoned)?;
            let entry = Arc::clone(&prepared.entry);
            if let Err(error) = prepared.invocation.authorize() {
                if error.code == EmbeddedErrorCode::Internal {
                    return Err(error);
                }
                drop(state);
                self.cancel(&id, error)?;
                continue;
            }
            // Only trusted internal copying/encoding runs here; no foreign callback is accepted.
            // 此处仅执行可信内部复制／编码；不接受外部回调。
            // Prepare a deliverable representation before consuming execution authority.
            // 消费执行权前准备可交付表示。
            let mut request = record.request.clone();
            request.remaining_ms = prepared.invocation.budget.remaining_ms();
            prepare(&request)?;
            // Encoding may take time, so recheck the original authority immediately before dispatch.
            // 编码可能耗时，因此在分发前立即重新检查原始权威。
            let delivery = prepared.invocation.authorize().and_then(|()| {
                entry.with_dispatch_gate(|| {
                    prepared.effect.begin()?;
                    record.phase = HostRequestPhase::Dispatched;
                    Ok(request)
                })?
            });
            match delivery {
                Ok(request) => {
                    state.ready.pop_front();
                    return Ok(Some(request));
                }
                Err(error) if error.code == EmbeddedErrorCode::Internal => return Err(error),
                Err(error) => {
                    drop(state);
                    self.cancel(&id, error)?;
                }
            }
        }
    }

    /// Read live state after refreshing cancellation, deadline and permission revocation.
    /// 刷新取消、截止时间与权限撤销后读取实时状态。
    pub fn status(&self, id: &str) -> EmbeddedResult<HostRequestStatus> {
        self.refresh(id)?;
        let state = self.state.lock().map_err(|_| poisoned())?;
        let record = state.records.get(id).ok_or_else(|| unknown(&state, id))?;
        Ok(HostRequestStatus {
            request_id: id.to_owned(),
            phase: record.phase,
            cancellation: record.cancellation.clone(),
        })
    }

    /// Complete the exact delivered `id` once with actual `outcome`, preserving committed effects.
    /// 使用真实 `outcome` 单次完成精确已投递 `id`，保留已提交副作用。
    pub fn complete(&self, id: &str, outcome: CapabilityOutcome) -> EmbeddedResult<()> {
        self.refresh(id)?;
        let prepared = {
            let mut state = self.state.lock().map_err(|_| poisoned())?;
            if !state.records.contains_key(id) {
                return Err(unknown(&state, id));
            }
            let record = state.records.get_mut(id).ok_or_else(poisoned)?;
            match record.phase {
                HostRequestPhase::Queued => {
                    return Err(EmbeddedError::new(
                        EmbeddedErrorCode::Busy,
                        "host request has not been dispatched",
                    ));
                }
                HostRequestPhase::Completing | HostRequestPhase::Completed => {
                    return Err(already_completed());
                }
                HostRequestPhase::Dispatched => {}
            }
            record.phase = HostRequestPhase::Completing;
            record.prepared.take().ok_or_else(poisoned)?
        };
        prepared.effect.observe(outcome.effects);
        let mut outcome = prepared.entry.validate_outcome(outcome);
        if let Err(error) = prepared.invocation.authorize() {
            outcome.result = Err(error);
        }
        drop(prepared);
        self.publish(id, outcome)
    }

    /// Stop admission and request cooperative cancellation; dispatched handlers retain capacity.
    /// 停止入场并请求协作取消；已分发处理器继续保留容量。
    pub fn close(&self) {
        let ids = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closing = true;
            state.records.keys().cloned().collect::<Vec<_>>()
        };
        for id in ids {
            let _ = self.cancel(&id, closed());
        }
    }

    /// Drain undispatched requests after the exact `registration_id` admission gate has closed.
    /// 在精确 `registration_id` 入场门关闭后排空未分发请求。
    /// Already delivered handlers keep their original execution and effect ownership.
    /// 已投递处理器保留原始执行与副作用所有权。
    pub(super) fn retire_registration(&self, registration_id: &str) -> EmbeddedResult<()> {
        // The entry gate prevents both new publication and dispatch during this retirement scan.
        // 条目门在此退役扫描期间阻止新发布与分发。
        let ids = {
            let state = self.state.lock().map_err(|_| poisoned())?;
            state
                .records
                .iter()
                .filter(|(_, record)| {
                    record.request.registration_id == registration_id
                        && record.phase == HostRequestPhase::Queued
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        for id in ids {
            match self.cancel(&id, closed()) {
                Ok(()) => {}
                // A concurrent waiter may have completed and forgotten this previously observed identity.
                // 并发等待方可能已完成并遗忘此前已观察身份。
                Err(error)
                    if matches!(
                        error.code,
                        EmbeddedErrorCode::AlreadyCompleted | EmbeddedErrorCode::NotFound
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Return whether all retained requests and their result owners have drained.
    /// 返回全部保留请求及其结果所有者是否已排空。
    pub fn is_drained(&self) -> EmbeddedResult<bool> {
        Ok(self
            .state
            .lock()
            .map_err(|_| poisoned())?
            .records
            .is_empty())
    }

    /// Refresh a pending request without treating cancellation as handler completion.
    /// 刷新待处理请求，不将取消视作处理器完成。
    fn refresh(&self, id: &str) -> EmbeddedResult<()> {
        let error = {
            let state = self.state.lock().map_err(|_| poisoned())?;
            let record = state.records.get(id).ok_or_else(|| unknown(&state, id))?;
            match &record.prepared {
                Some(prepared) => prepared
                    .invocation
                    .authorize()
                    .and_then(|()| {
                        if record.phase == HostRequestPhase::Queued
                            && !prepared.entry.status()?.accepting
                        {
                            Err(closed())
                        } else {
                            Ok(())
                        }
                    })
                    .err(),
                None => None,
            }
        };
        if let Some(error) = error {
            self.cancel(id, error)?;
        }
        Ok(())
    }

    /// Cancel `id`; only an undispatched request can terminate immediately.
    /// 取消 `id`；仅未分发请求可以立即终止。
    fn cancel(&self, id: &str, error: EmbeddedError) -> EmbeddedResult<()> {
        let prepared = {
            let mut state = self.state.lock().map_err(|_| poisoned())?;
            if !state.records.contains_key(id) {
                return Err(unknown(&state, id));
            }
            let record = state.records.get_mut(id).ok_or_else(poisoned)?;
            if record.phase == HostRequestPhase::Completed {
                return Ok(());
            }
            record.cancellation.get_or_insert(error);
            if record.phase != HostRequestPhase::Queued {
                return Ok(());
            }
            record.phase = HostRequestPhase::Completing;
            let prepared = record.prepared.take().ok_or_else(poisoned)?;
            state.ready.retain(|ready| ready != id);
            prepared
        };
        drop(prepared);
        self.publish(
            id,
            CapabilityOutcome {
                result: Err(EmbeddedError::new(
                    EmbeddedErrorCode::Cancelled,
                    "host request cancelled before dispatch",
                )),
                effects: EffectState::NotStarted,
            },
        )
    }

    /// Publish only after callback ownership and admission have actually been released.
    /// 仅在回调所有权与入场许可真实释放后发布。
    fn publish(&self, id: &str, mut outcome: CapabilityOutcome) -> EmbeddedResult<()> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        let record = state.records.get_mut(id).ok_or_else(poisoned)?;
        if let Some(error) = &record.cancellation {
            outcome.result = Err(error.clone());
        }
        record.outcome = Some(outcome);
        record.phase = HostRequestPhase::Completed;
        if !record.waiter_alive {
            self.remove(&mut state, id);
        }
        self.changed.notify_all();
        Ok(())
    }

    /// Remove a terminal record and retain a bounded duplicate-completion tombstone.
    /// 移除终态记录并保留有界重复完成墓碑。
    fn remove(&self, state: &mut BrokerState, id: &str) {
        if let Some(record) = state.records.remove(id) {
            state.bytes -= record.bytes;
            state.ready.retain(|ready| ready != id);
            state.tombstones.push_back(id.to_owned());
            while state.tombstones.len() > self.max_records {
                state.tombstones.pop_front();
            }
        }
    }
}

/// Unique result owner; dropping it cancels interest without pretending SDK execution stopped.
/// 唯一结果所有者；丢弃时取消等待意愿，不假定 SDK 执行已停止。
pub struct HostRequestHandle {
    /// Broker remains alive through the whole pending request.
    /// 代理在整个待处理请求期间保持存活。
    broker: Arc<HostRequestBroker>,
    /// Exact owned identity.
    /// 精确拥有的身份。
    id: String,
}

impl HostRequestHandle {
    /// Return the stable request identity for diagnostics and explicit cancellation.
    /// 返回用于诊断与显式取消的稳定请求身份。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Read a terminal result if the actual execution owner has finished.
    /// 若真实执行所有者已结束，则读取终态结果。
    pub fn poll(&self) -> EmbeddedResult<Option<CapabilityOutcome>> {
        self.broker.refresh(&self.id)?;
        let state = self.broker.state.lock().map_err(|_| poisoned())?;
        Ok(state
            .records
            .get(&self.id)
            .ok_or_else(|| unknown(&state, &self.id))?
            .outcome
            .clone())
    }

    /// Wait on the existing execution thread until real completion, including cancellation drain.
    /// 在已有执行线程等待真实完成，包含取消排空。
    pub fn wait(&self) -> EmbeddedResult<CapabilityOutcome> {
        loop {
            if let Some(outcome) = self.poll()? {
                return Ok(outcome);
            }
            let state = self.broker.state.lock().map_err(|_| poisoned())?;
            // Periodic refresh observes external permission and deadline changes without one thread per callback.
            // 周期刷新观察外部权限与截止时间变化，无需每个回调独占线程。
            drop(
                self.broker
                    .changed
                    .wait_timeout(state, MAINTENANCE_INTERVAL)
                    .map_err(|_| poisoned())?,
            );
        }
    }

    /// Request local cooperative cancellation without cancelling unrelated parent calls.
    /// 请求局部协作取消，不取消无关父级调用。
    pub fn cancel(&self) -> EmbeddedResult<()> {
        self.broker.cancel(
            &self.id,
            EmbeddedError::new(EmbeddedErrorCode::Cancelled, "host request cancelled"),
        )
    }
}

impl Drop for HostRequestHandle {
    /// Retain dispatched orphan requests until the SDK explicitly acknowledges real completion.
    /// 保留已分发的孤立请求，直到 SDK 显式确认真实完成。
    fn drop(&mut self) {
        {
            let mut state = self
                .broker
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(record) = state.records.get_mut(&self.id) {
                record.waiter_alive = false;
                if record.phase == HostRequestPhase::Completed {
                    self.broker.remove(&mut state, &self.id);
                    return;
                }
            }
        }
        let _ = self.cancel();
    }
}

/// Report expired identity separately from a retained duplicate completion.
/// 将过期身份与保留的重复完成区分报告。
fn unknown(state: &BrokerState, id: &str) -> EmbeddedError {
    if state.tombstones.iter().any(|known| known == id) {
        already_completed()
    } else {
        EmbeddedError::new(
            EmbeddedErrorCode::NotFound,
            "host request identity is unknown or expired",
        )
    }
}

/// Return the stable duplicate-completion classification.
/// 返回稳定的重复完成分类。
fn already_completed() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::AlreadyCompleted,
        "host request already has a completion owner",
    )
}

/// Return a closed admission error without revealing unrelated registrations.
/// 返回关闭入场错误，不暴露无关注册。
fn closed() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Closed,
        "host request admission is closed",
    )
}

/// Return a capacity rejection without allocating an unbounded diagnostic.
/// 返回容量拒绝，不分配无界诊断。
fn capacity() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::CapacityExceeded,
        "host request capacity reached",
    )
}

/// Surface poisoned metadata instead of silently admitting work.
/// 显式报告中毒元数据，不静默接纳任务。
fn poisoned() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Internal,
        "host request state is inconsistent",
    )
}
