use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::runtime_logging::warn as log_warn;

/// Number of logical event slots reserved for every registered managed session.
/// 为每个已注册受管会话预留的逻辑事件槽数量。
const MANAGED_SESSION_EVENT_SLOTS_PER_SESSION: usize = 4;
/// Largest portable finite event-wait timeout, one millisecond below the Windows infinite sentinel.
/// 最大可移植有限事件等待超时，比 Windows 无限等待哨兵少一毫秒。
const MAX_MANAGED_SESSION_EVENT_WAIT_TIMEOUT_MS: u64 = u32::MAX as u64 - 1;
/// Initial delay before retrying one host wake callback that explicitly reported failure.
/// 宿主唤醒回调显式报告失败后的初始重试间隔。
const MANAGED_SESSION_WAKE_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(10);
/// Maximum delay capping exponential host wake callback retry backoff.
/// 限制宿主唤醒回调指数重试退避的最大间隔。
const MANAGED_SESSION_WAKE_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

thread_local! {
    /// Event-center addresses whose callbacks are currently executing on this thread.
    /// 当前线程正在执行回调的事件中心地址集合。
    static ACTIVE_MANAGED_SESSION_EVENT_CALLBACKS: RefCell<Vec<usize>> = const {
        RefCell::new(Vec::new())
    };
}

/// Kind of one host-visible managed-session event.
/// 单个宿主可见受管会话事件的类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeManagedSessionEventKind {
    /// Standard output has buffered data ready to read.
    /// 标准输出已有可读取的缓冲数据。
    StdoutReadable,
    /// Standard error has buffered data ready to read.
    /// 标准错误已有可读取的缓冲数据。
    StderrReadable,
    /// The managed child process has exited.
    /// 受管子进程已经退出。
    Exited,
    /// The managed session encountered one background failure.
    /// 受管会话遇到后台失败。
    Failed,
}

/// One sequenced event emitted by a registered managed process session.
/// 一个由已注册受管进程会话发出的带序号事件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeManagedSessionEvent {
    /// Opaque System lease identifier that owns the session.
    /// 拥有该会话的不透明 System 租约标识。
    pub system_lease_id: String,
    /// Stable System lease SID supplied by the host.
    /// 宿主提供的稳定 System 租约 SID。
    pub sid: String,
    /// SID-local System lease generation.
    /// SID 内部的 System 租约代际。
    pub generation: u64,
    /// Engine-local managed process session identifier.
    /// 引擎内部的受管进程会话标识。
    pub managed_session_id: u64,
    /// Event kind occupying one of the session's four logical slots.
    /// 占用会话四个逻辑槽之一的事件类型。
    pub kind: RuntimeManagedSessionEventKind,
    /// Event-center-global monotonically increasing sequence.
    /// 事件中心全局单调递增序号。
    pub sequence: u64,
}

/// Bounded result returned by one managed-session event poll or wait operation.
/// 单次受管会话事件轮询或等待操作返回的有界结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeManagedSessionEventBatch {
    /// Events removed from the center in sequence order.
    /// 按序号顺序从事件中心移除的事件。
    pub events: Vec<RuntimeManagedSessionEvent>,
    /// Pending logical event slots remaining after this destructive drain.
    /// 本次破坏性排空后仍待处理的逻辑事件槽数量。
    pub remaining: usize,
    /// Whether a wait returned because no event arrived before its deadline.
    /// 等待是否因截止时间前没有事件到达而返回。
    pub timed_out: bool,
}

/// Stable identity used to reserve and address one managed session's event slots.
/// 用于预留并寻址单个受管会话事件槽的稳定身份。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ManagedSessionEventToken {
    /// Opaque System lease identifier that owns the session.
    /// 拥有该会话的不透明 System 租约标识。
    system_lease_id: String,
    /// Stable System lease SID supplied by the host.
    /// 宿主提供的稳定 System 租约 SID。
    sid: String,
    /// SID-local System lease generation.
    /// SID 内部的 System 租约代际。
    generation: u64,
    /// Engine-local managed session identifier.
    /// 引擎内部的受管会话标识。
    managed_session_id: u64,
}

impl ManagedSessionEventToken {
    /// Build one event token from an authoritative System lease and managed-session identity.
    /// 根据权威 System 租约与受管会话身份构造一个事件令牌。
    ///
    /// `system_lease_id` and `sid` identify the owning System lease.
    /// `system_lease_id` 与 `sid` 标识所属 System 租约。
    ///
    /// `generation` and `managed_session_id` distinguish lease and session generations.
    /// `generation` 与 `managed_session_id` 区分租约及会话代际。
    ///
    /// Return one immutable token suitable for registration and event publication.
    /// 返回一个适合注册与事件发布的不可变令牌。
    pub(crate) fn new(
        system_lease_id: impl Into<String>,
        sid: impl Into<String>,
        generation: u64,
        managed_session_id: u64,
    ) -> Self {
        Self {
            system_lease_id: system_lease_id.into(),
            sid: sid.into(),
            generation,
            managed_session_id,
        }
    }
}

/// One occupied logical event slot identified by session token and event kind.
/// 一个由会话令牌与事件类型标识的已占用逻辑事件槽。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ManagedSessionEventSlot {
    /// Session token that owns the slot.
    /// 拥有该事件槽的会话令牌。
    token: ManagedSessionEventToken,
    /// Event kind represented by the slot.
    /// 该事件槽代表的事件类型。
    kind: RuntimeManagedSessionEventKind,
}

/// Wake callback invoked on an empty-to-nonempty event edge.
/// 在事件队列由空变为非空时调用的唤醒回调。
pub type RuntimeManagedSessionWakeCallback = Arc<dyn Fn() + Send + Sync + 'static>;

/// Fallible wake callback used by host ABIs whose callback reports scheduling success.
/// 供回调可报告调度成功与否的宿主 ABI 使用的可失败唤醒回调。
pub(crate) type FallibleRuntimeManagedSessionWakeCallback =
    Arc<dyn Fn() -> Result<(), String> + Send + Sync + 'static>;

/// Internal callback representation preserving the infallible Rust API and fallible host ABI.
/// 内部回调表示，同时保留不可失败 Rust API 与可失败宿主 ABI。
#[derive(Clone)]
enum ManagedSessionWakeCallback {
    /// Public Rust callback whose normal return completes the wake edge.
    /// 正常返回即完成唤醒边沿的公开 Rust 回调。
    Infallible(RuntimeManagedSessionWakeCallback),
    /// Host ABI callback whose error requires durable retry.
    /// 返回错误时需要持久重试的宿主 ABI 回调。
    Fallible(FallibleRuntimeManagedSessionWakeCallback),
}

impl ManagedSessionWakeCallback {
    /// Invoke this callback and normalize its completion into fallible internal form.
    /// 调用当前回调，并把完成结果规范为可失败内部形式。
    fn invoke(&self) -> Result<(), String> {
        match self {
            Self::Infallible(callback) => {
                callback();
                Ok(())
            }
            Self::Fallible(callback) => callback(),
        }
    }
}

/// Currently installed wake callback and its notification generation.
/// 当前安装的唤醒回调及其通知代际。
struct ManagedSessionEventCallbackRegistration {
    /// Monotonic callback registration generation.
    /// 单调递增的回调注册代际。
    generation: u64,
    /// Host callback invoked without event-center locks.
    /// 在不持有事件中心锁时调用的宿主回调。
    callback: ManagedSessionWakeCallback,
    /// Nonempty queue epoch already reported to this callback.
    /// 已向当前回调报告的非空队列纪元。
    last_notified_epoch: Option<u64>,
}

/// Callback invocation detached from the center lock.
/// 从事件中心锁中分离出的回调调用。
struct ManagedSessionEventCallbackInvocation {
    /// Registration generation charged with the in-flight call.
    /// 负责当前在途调用计数的注册代际。
    generation: u64,
    /// Nonempty queue epoch charged to this invocation.
    /// 当前调用负责的非空队列纪元。
    epoch: u64,
    /// Cloned callback invoked after releasing the center lock.
    /// 释放事件中心锁后调用的回调克隆。
    callback: ManagedSessionWakeCallback,
    /// Whether the first failure diagnostic has already been emitted.
    /// 是否已经输出首次失败诊断。
    failure_reported: bool,
    /// Number of explicit failures already returned for this queue epoch.
    /// 当前队列纪元已显式返回的失败次数。
    retry_attempt: u32,
}

/// One bounded callback retry item owned outside the event-center state lock.
/// 在事件中心状态锁外拥有的单个有界回调重试项。
struct ManagedSessionEventCallbackRetryItem {
    /// Detached callback invocation retaining its in-flight generation charge.
    /// 保留其在途代际计数的分离式回调调用。
    invocation: ManagedSessionEventCallbackInvocation,
    /// Bounded backoff to wait before the next invocation.
    /// 下次调用前等待的有界退避时长。
    retry_delay: Duration,
}

/// Single-slot retry scheduler for one event center's active callback generation.
/// 单槽重试调度器，用于单个事件中心的活动回调代际。
struct ManagedSessionEventCallbackRetryCenter {
    /// Mutable retry slot and closure state.
    /// 可变重试槽与关闭状态。
    state: Mutex<ManagedSessionEventCallbackRetryState>,
    /// Notification waking the retry worker on enqueue or closure.
    /// 在入队或关闭时唤醒重试工作线程的通知量。
    changed: Condvar,
}

/// Mutable state for the strictly single-slot callback retry queue.
/// 严格单槽回调重试队列的可变状态。
struct ManagedSessionEventCallbackRetryState {
    /// At most one active callback invocation can be retried per center.
    /// 每个事件中心最多可重试一个活动回调调用。
    pending: Option<ManagedSessionEventCallbackRetryItem>,
    /// Whether event-center destruction permanently closed retries.
    /// 事件中心析构是否已永久关闭重试。
    closed: bool,
}

/// Mutable state protected by one managed-session event center lock.
/// 由单个受管会话事件中心锁保护的可变状态。
struct ManagedSessionEventCenterState {
    /// Sessions whose four event slots are currently reserved.
    /// 当前已预留四个事件槽的会话集合。
    registered_sessions: HashSet<ManagedSessionEventToken>,
    /// Occupied logical slots used to coalesce repeated events.
    /// 用于合并重复事件的已占用逻辑槽集合。
    pending_slots: HashSet<ManagedSessionEventSlot>,
    /// Pending events ordered by their global sequence.
    /// 按全局序号排序的待处理事件。
    events: VecDeque<RuntimeManagedSessionEvent>,
    /// Last globally issued event sequence.
    /// 最近签发的全局事件序号。
    next_sequence: u64,
    /// Sequence of the event that began the current nonempty queue epoch.
    /// 开启当前非空队列纪元的事件序号。
    nonempty_epoch: Option<u64>,
    /// Whether the center rejects new sessions and events.
    /// 事件中心是否拒绝新会话与新事件。
    closed: bool,
    /// Currently installed edge-triggered wake callback.
    /// 当前安装的边沿触发唤醒回调。
    callback: Option<ManagedSessionEventCallbackRegistration>,
    /// Last callback registration generation allocated by the center.
    /// 事件中心最近分配的回调注册代际。
    next_callback_generation: u64,
    /// In-flight callback counts keyed by registration generation.
    /// 按注册代际索引的在途回调计数。
    inflight_callbacks: HashMap<u64, usize>,
}

/// Strictly bounded event center owned by one runtime engine.
/// 由单个运行时引擎拥有的严格有界事件中心。
pub(crate) struct ManagedSessionEventCenter {
    /// Maximum number of sessions that can reserve event slots.
    /// 可预留事件槽的最大会话数量。
    max_sessions: usize,
    /// Exact logical event capacity equal to session limit multiplied by four.
    /// 等于会话上限乘以四的精确逻辑事件容量。
    capacity: usize,
    /// Mutable event, registration, and callback state.
    /// 可变事件、注册及回调状态。
    state: Mutex<ManagedSessionEventCenterState>,
    /// Condition variable used by event waiters and center close.
    /// 供事件等待方与事件中心关闭使用的条件变量。
    changed: Condvar,
    /// Condition variable used to await retired callback generations.
    /// 用于等待已退役回调代际收敛的条件变量。
    callback_idle: Condvar,
    /// Single-slot scheduler for fallible callback retries and catch-up delivery.
    /// 用于可失败回调重试与补发投递的单槽调度器。
    callback_retry_center: Arc<ManagedSessionEventCallbackRetryCenter>,
    /// Join handle for the single callback retry worker.
    /// 单个回调重试工作线程的等待句柄。
    callback_retry_worker: Mutex<Option<JoinHandle<()>>>,
}

impl ManagedSessionEventCenter {
    /// Create one event center with four reserved logical slots per session.
    /// 创建一个为每个会话预留四个逻辑槽的事件中心。
    ///
    /// `max_sessions` must be positive and small enough for the exact capacity multiplication.
    /// `max_sessions` 必须为正数，并且与精确容量相乘时不得溢出。
    ///
    /// Return a shared event center or a stable configuration error.
    /// 返回共享事件中心或稳定的配置错误。
    pub(crate) fn new(max_sessions: usize) -> Result<Arc<Self>, String> {
        if max_sessions == 0 {
            return Err("managed session event max_sessions must be greater than 0".to_string());
        }
        // Exact queue capacity guaranteed by four slots for every registered session.
        // 由每个已注册会话的四个事件槽保证的精确队列容量。
        let capacity = max_sessions
            .checked_mul(MANAGED_SESSION_EVENT_SLOTS_PER_SESSION)
            .ok_or_else(|| "managed session event capacity overflow".to_string())?;
        let callback_retry_center = Arc::new(ManagedSessionEventCallbackRetryCenter::new());
        let center = Arc::new(Self {
            max_sessions,
            capacity,
            state: Mutex::new(ManagedSessionEventCenterState {
                registered_sessions: HashSet::new(),
                pending_slots: HashSet::new(),
                events: VecDeque::new(),
                next_sequence: 0,
                nonempty_epoch: None,
                closed: false,
                callback: None,
                next_callback_generation: 0,
                inflight_callbacks: HashMap::new(),
            }),
            changed: Condvar::new(),
            callback_idle: Condvar::new(),
            callback_retry_center: Arc::clone(&callback_retry_center),
            callback_retry_worker: Mutex::new(None),
        });
        let weak_center = Arc::downgrade(&center);
        let retry_worker = std::thread::Builder::new()
            .name("luaskills-managed-wake-retry".to_string())
            .spawn(move || {
                run_managed_session_event_callback_retry_worker(weak_center, callback_retry_center);
            })
            .map_err(|error| format!("spawn managed session wake retry worker: {error}"))?;
        *center
            .callback_retry_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(retry_worker);
        Ok(center)
    }

    /// Return the exact logical event capacity of this center.
    /// 返回当前事件中心的精确逻辑事件容量。
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Reserve four logical event slots for one managed session before process spawn.
    /// 在进程启动前为单个受管会话预留四个逻辑事件槽。
    ///
    /// `token` is the complete stable identity of the session being registered.
    /// `token` 是待注册会话的完整稳定身份。
    ///
    /// Return unit after reservation, or an error for duplicate, closed, or full centers.
    /// 预留完成后返回 unit；重复、已关闭或已满事件中心返回错误。
    pub(crate) fn register_session(&self, token: ManagedSessionEventToken) -> Result<(), String> {
        // Center state locked for one atomic capacity check and reservation.
        // 为一次原子容量检查与预留而加锁的事件中心状态。
        let mut state = self.lock_state();
        if state.closed {
            return Err("managed session event center is closed".to_string());
        }
        if state.registered_sessions.contains(&token) {
            return Err("managed session event token is already registered".to_string());
        }
        if state.registered_sessions.len() >= self.max_sessions {
            return Err(format!(
                "managed session event session limit exceeded; max_sessions={}",
                self.max_sessions
            ));
        }
        state.registered_sessions.insert(token);
        Ok(())
    }

    /// Remove one session reservation and every still-pending event owned by it.
    /// 移除单个会话预留及其仍待处理的全部事件。
    ///
    /// `token` identifies the session whose future publications must become stale.
    /// `token` 标识未来事件发布必须变为陈旧状态的会话。
    ///
    /// Return whether an active reservation was removed.
    /// 返回是否移除了一个活动预留。
    pub(crate) fn unregister_session(&self, token: &ManagedSessionEventToken) -> bool {
        // Center state locked while reservation and pending slots are removed together.
        // 在同时移除预留与待处理事件槽期间加锁的事件中心状态。
        let mut state = self.lock_state();
        if !state.registered_sessions.remove(token) {
            return false;
        }
        state.pending_slots.retain(|slot| &slot.token != token);
        state
            .events
            .retain(|event| !event_matches_token(event, token));
        if state.events.is_empty() {
            state.nonempty_epoch = None;
        }
        true
    }

    /// Publish one coalesced event for a registered managed session.
    /// 为一个已注册受管会话发布一个可合并事件。
    ///
    /// `token` selects the registered session and `kind` selects one of its four slots.
    /// `token` 选择已注册会话，`kind` 选择其四个事件槽之一。
    ///
    /// Return true for a newly queued event and false when an occupied slot coalesces it.
    /// 新事件入队返回 true；已占用事件槽合并该事件时返回 false。
    pub(crate) fn publish(
        &self,
        token: &ManagedSessionEventToken,
        kind: RuntimeManagedSessionEventKind,
    ) -> Result<bool, String> {
        // Detached callback invocation selected while the center lock is still held.
        // 在仍持有事件中心锁时选出的分离式回调调用。
        let callback_invocation = {
            // Center state locked while slot coalescing and sequence allocation are atomic.
            // 在事件槽合并与序号分配保持原子期间加锁的事件中心状态。
            let mut state = self.lock_state();
            if state.closed {
                return Err("managed session event center is closed".to_string());
            }
            if !state.registered_sessions.contains(token) {
                return Err("managed session event token is not registered".to_string());
            }
            // Logical slot used to merge repeated readable or terminal publications.
            // 用于合并重复可读或终态发布的逻辑事件槽。
            let slot = ManagedSessionEventSlot {
                token: token.clone(),
                kind,
            };
            if state.pending_slots.contains(&slot) {
                return Ok(false);
            }
            if state.pending_slots.len() >= self.capacity {
                return Err("managed session event capacity invariant was violated".to_string());
            }
            // Next globally monotonic event sequence allocated without wraparound.
            // 在不发生回绕的前提下分配的下一个全局单调事件序号。
            let sequence = state
                .next_sequence
                .checked_add(1)
                .ok_or_else(|| "managed session event sequence exhausted".to_string())?;
            state.next_sequence = sequence;
            // Whether this publication begins a new empty-to-nonempty queue epoch.
            // 当前发布是否开启一个新的队列由空变为非空纪元。
            let begins_nonempty_epoch = state.events.is_empty();
            state.pending_slots.insert(slot);
            state.events.push_back(RuntimeManagedSessionEvent {
                system_lease_id: token.system_lease_id.clone(),
                sid: token.sid.clone(),
                generation: token.generation,
                managed_session_id: token.managed_session_id,
                kind,
                sequence,
            });
            if begins_nonempty_epoch {
                state.nonempty_epoch = Some(sequence);
            }
            self.changed.notify_all();
            self.schedule_callback_locked(&mut state, None)
        };
        if let Some(invocation) = callback_invocation {
            self.callback_retry_center
                .enqueue(ManagedSessionEventCallbackRetryItem {
                    invocation,
                    retry_delay: Duration::ZERO,
                });
        }
        Ok(true)
    }

    /// Poll pending events without waiting.
    /// 以非等待方式轮询待处理事件。
    ///
    /// `max_events` is the positive maximum number of events removed in one call.
    /// `max_events` 是单次调用最多移除的正数事件数量。
    ///
    /// Return an immediate batch, including an empty batch when no event is ready.
    /// 立即返回一批事件；没有就绪事件时返回空批次。
    pub(crate) fn poll(
        &self,
        max_events: usize,
    ) -> Result<RuntimeManagedSessionEventBatch, String> {
        if max_events == 0 {
            return Err("managed session event max_events must be greater than 0".to_string());
        }
        // Immediate center snapshot used only for nonblocking destructive drain.
        // 仅用于非阻塞破坏性排空的即时事件中心快照。
        let mut state = self.lock_state();
        if state.closed && state.events.is_empty() {
            return Err("managed session event center is closed".to_string());
        }
        Ok(Self::drain_events_locked(&mut state, max_events, false))
    }

    /// Wait for pending events, center closure, or the requested timeout.
    /// 等待待处理事件、事件中心关闭或请求超时。
    ///
    /// `max_events` must be positive and bounds the returned batch.
    /// `max_events` 必须为正数，并限制返回批次大小。
    ///
    /// `timeout_ms` accepts zero for a true nonblocking poll and rejects unrepresentable deadlines.
    /// `timeout_ms` 接受零以执行真正非阻塞轮询，并拒绝无法表示的截止时间。
    ///
    /// Return events in sequence order, the remaining queue size, and an explicit timeout flag.
    /// 返回按序号排序的事件、剩余队列大小以及显式超时标记。
    pub(crate) fn wait(
        &self,
        max_events: usize,
        timeout_ms: u64,
    ) -> Result<RuntimeManagedSessionEventBatch, String> {
        if max_events == 0 {
            return Err("managed session event max_events must be greater than 0".to_string());
        }
        // Center state inspected immediately before any optional wait.
        // 在任何可选等待前立即检查的事件中心状态。
        let mut state = self.lock_state();
        if !state.events.is_empty() {
            return Ok(Self::drain_events_locked(&mut state, max_events, false));
        }
        if state.closed {
            return Err("managed session event center is closed".to_string());
        }
        if timeout_ms == 0 {
            return Ok(Self::drain_events_locked(&mut state, max_events, true));
        }
        if timeout_ms > MAX_MANAGED_SESSION_EVENT_WAIT_TIMEOUT_MS {
            return Err(format!(
                "managed session event timeout_ms is too large; maximum is {MAX_MANAGED_SESSION_EVENT_WAIT_TIMEOUT_MS}"
            ));
        }
        // Checked monotonic deadline preventing extreme timeout arithmetic overflow.
        // 防止极端超时时间运算溢出的已检查单调截止时间。
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(timeout_ms))
            .ok_or_else(|| "managed session event timeout_ms is too large".to_string())?;
        loop {
            if !state.events.is_empty() {
                return Ok(Self::drain_events_locked(&mut state, max_events, false));
            }
            if state.closed {
                return Err("managed session event center is closed".to_string());
            }
            // Remaining duration derived without subtracting a later time from an earlier deadline.
            // 在不使用较晚时间减去较早截止时间的前提下计算的剩余时长。
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(Self::drain_events_locked(&mut state, max_events, true));
            };
            // Timed condition-variable result with poison recovery.
            // 带锁中毒恢复的定时条件变量等待结果。
            let (next_state, timed_out) = self.wait_timeout_state(state, remaining);
            state = next_state;
            if timed_out && state.events.is_empty() && !state.closed {
                return Ok(Self::drain_events_locked(&mut state, max_events, true));
            }
        }
    }

    /// Replace or clear the edge-triggered wake callback and quiesce retired generations.
    /// 替换或清除边沿触发唤醒回调，并等待已退役代际收敛。
    ///
    /// `callback` installs a new callback when present and clears the callback when absent.
    /// `callback` 存在时安装新回调，不存在时清除回调。
    ///
    /// Return only after every callback generation retired by this operation has completed.
    /// 仅在当前操作退役的全部回调代际完成后返回。
    pub(crate) fn set_wake_callback(
        &self,
        callback: Option<RuntimeManagedSessionWakeCallback>,
    ) -> Result<(), String> {
        self.set_wake_callback_internal(callback.map(ManagedSessionWakeCallback::Infallible))
    }

    /// Replace or clear one fallible host-ABI wake callback with retry semantics.
    /// 以重试语义替换或清除一个可失败宿主 ABI 唤醒回调。
    pub(crate) fn set_fallible_wake_callback(
        &self,
        callback: Option<FallibleRuntimeManagedSessionWakeCallback>,
    ) -> Result<(), String> {
        self.set_wake_callback_internal(callback.map(ManagedSessionWakeCallback::Fallible))
    }

    /// Replace or clear one internal wake callback and quiesce retired generations.
    /// 替换或清除一个内部唤醒回调，并等待已退役代际收敛。
    fn set_wake_callback_internal(
        &self,
        callback: Option<ManagedSessionWakeCallback>,
    ) -> Result<(), String> {
        if self.callback_is_active_on_current_thread() {
            return Err(
                "managed session event callback cannot replace itself during invocation"
                    .to_string(),
            );
        }
        // Center state locked across callback replacement and condition-variable retirement waits.
        // 在回调替换与条件变量退役等待期间加锁的事件中心状态。
        let mut state = self.lock_state();
        if state.closed && callback.is_some() {
            return Err("managed session event center is closed".to_string());
        }
        // Optional generation validated before retiring the current callback.
        // 在退役当前回调前完成校验的可选新代际。
        let new_generation = callback
            .as_ref()
            .map(|_| {
                state
                    .next_callback_generation
                    .checked_add(1)
                    .ok_or_else(|| {
                        "managed session event callback generation exhausted".to_string()
                    })
            })
            .transpose()?;
        // Generation barrier covering every callback registration installed before this operation.
        // 覆盖当前操作前已安装全部回调注册的代际屏障。
        let retirement_barrier = state.next_callback_generation;
        // Retired callback retained until its in-flight invocations have completed.
        // 保留到其在途调用完成的已退役回调。
        let retired_callback = state.callback.take();
        // Optional generation allocated to the newly installed callback.
        // 为新安装回调分配的可选代际。
        let installed_generation = match (callback, new_generation) {
            (Some(callback), Some(generation)) => {
                state.next_callback_generation = generation;
                state.callback = Some(ManagedSessionEventCallbackRegistration {
                    generation,
                    callback,
                    last_notified_epoch: None,
                });
                Some(generation)
            }
            (None, None) => None,
            _ => unreachable!("callback presence and allocated generation must match"),
        };
        while has_inflight_callback_at_or_before(&state, retirement_barrier) {
            state = self.wait_callback_state(state);
        }
        // Existing pending events require one catch-up wake for the newly installed callback.
        // 已存在待处理事件时需要为新安装回调补发一次唤醒。
        let callback_invocation = installed_generation
            .and_then(|generation| self.schedule_callback_locked(&mut state, Some(generation)));
        drop(state);
        drop(retired_callback);
        if let Some(invocation) = callback_invocation {
            self.callback_retry_center
                .enqueue(ManagedSessionEventCallbackRetryItem {
                    invocation,
                    retry_delay: Duration::ZERO,
                });
        }
        Ok(())
    }

    /// Close the center, wake all event waiters, clear the callback, and await callback quiescence.
    /// 关闭事件中心、唤醒全部事件等待方、清除回调并等待回调收敛。
    ///
    /// Return true for the first close and false for an already closed center.
    /// 首次关闭返回 true；事件中心已关闭时返回 false。
    pub(crate) fn close(&self) -> Result<bool, String> {
        if self.callback_is_active_on_current_thread() {
            return Err(
                "managed session event callback cannot close its center during invocation"
                    .to_string(),
            );
        }
        // Center state locked while close becomes visible and callback generations retire.
        // 在关闭状态生效与回调代际退役期间加锁的事件中心状态。
        let mut state = self.lock_state();
        // Whether this operation performs the first state transition to closed.
        // 当前操作是否首次把状态切换为关闭。
        let first_close = !state.closed;
        state.closed = true;
        // Generation barrier covering every callback that could have started before close.
        // 覆盖关闭前可能已经启动的全部回调的代际屏障。
        let retirement_barrier = state.next_callback_generation;
        // Retired callback retained until all earlier invocations complete.
        // 保留到全部先前调用完成的已退役回调。
        let retired_callback = state.callback.take();
        self.changed.notify_all();
        while has_inflight_callback_at_or_before(&state, retirement_barrier) {
            state = self.wait_callback_state(state);
        }
        drop(state);
        drop(retired_callback);
        Ok(first_close)
    }

    /// Drain at most `max_events` queued events while the center state is locked.
    /// 在事件中心状态已加锁时最多取出 `max_events` 个排队事件。
    fn drain_events_locked(
        state: &mut ManagedSessionEventCenterState,
        max_events: usize,
        timed_out: bool,
    ) -> RuntimeManagedSessionEventBatch {
        // Exact number of available events removed in this batch.
        // 当前批次移除的精确可用事件数量。
        let event_count = max_events.min(state.events.len());
        // Bounded destination preserving queue sequence order.
        // 保持队列序号顺序的有界目标集合。
        let mut events = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            if let Some(event) = state.events.pop_front() {
                state.pending_slots.remove(&event_slot(&event));
                events.push(event);
            }
        }
        if state.events.is_empty() {
            state.nonempty_epoch = None;
        }
        RuntimeManagedSessionEventBatch {
            events,
            remaining: state.events.len(),
            timed_out,
        }
    }

    /// Select one callback invocation for the current nonempty queue epoch.
    /// 为当前非空队列纪元选择一次回调调用。
    ///
    /// `expected_generation` restricts catch-up delivery to one newly installed callback.
    /// `expected_generation` 把补发限制到一个新安装回调。
    ///
    /// Return a detached invocation after charging its in-flight counter.
    /// 在增加在途计数后返回一个分离式调用。
    fn schedule_callback_locked(
        &self,
        state: &mut ManagedSessionEventCenterState,
        expected_generation: Option<u64>,
    ) -> Option<ManagedSessionEventCallbackInvocation> {
        // Current nonempty epoch absent whenever the queue is empty.
        // 队列为空时不存在的当前非空纪元。
        let epoch = state.nonempty_epoch?;
        // Callback data copied after confirming generation and edge eligibility.
        // 确认代际及边沿资格后复制的回调数据。
        let (generation, callback) = {
            let registration = state.callback.as_mut()?;
            if expected_generation.is_some_and(|expected| expected != registration.generation)
                || registration.last_notified_epoch == Some(epoch)
                // The retry scheduler has one slot for the entire center, so callback generations
                // must serialize globally, including during replacement quiescence.
                // 重试调度器在整个事件中心仅有一个槽，因此回调代际必须全局串行，包括替换
                // 收敛期间。
                || !state.inflight_callbacks.is_empty()
            {
                return None;
            }
            registration.last_notified_epoch = Some(epoch);
            (registration.generation, registration.callback.clone())
        };
        *state.inflight_callbacks.entry(generation).or_insert(0) += 1;
        Some(ManagedSessionEventCallbackInvocation {
            generation,
            epoch,
            callback,
            failure_reported: false,
            retry_attempt: 0,
        })
    }

    /// Invoke one detached callback and always retire its in-flight count.
    /// 调用一个分离式回调，并始终减少其在途计数。
    fn invoke_callback(&self, mut invocation: ManagedSessionEventCallbackInvocation) {
        // Thread-local scope used to reject self-replacement deadlocks.
        // 用于拒绝回调自替换死锁的线程局部作用域。
        let callback_result = {
            let _scope = ManagedSessionEventCallbackExecutionScope::enter(self);
            // Callback panic is contained and treated as terminal for this edge; retry is reserved
            // for an explicit host failure result so a panicking callback cannot create a storm.
            // 隔离回调 panic，并将其视为当前边沿的终态；仅显式宿主失败结果会触发重试，避免
            // panic 回调造成风暴。
            catch_unwind(AssertUnwindSafe(|| invocation.callback.invoke()))
        };
        match callback_result {
            Ok(Ok(())) | Err(_) => {
                let generation = invocation.generation;
                // Callback ownership must be released before quiescence observes zero in-flight calls.
                // 在静默等待观察到零个在途调用前必须释放回调所有权。
                drop(invocation);
                self.finish_callback_invocation(generation);
            }
            Ok(Err(error)) => {
                invocation.retry_attempt = invocation.retry_attempt.saturating_add(1);
                if !invocation.failure_reported {
                    log_warn(format!(
                        "[LuaSkill:warn] managed session wake callback failed and will be retried: {error}"
                    ));
                    invocation.failure_reported = true;
                }
                // The existing in-flight charge is retained while the single-slot worker retries.
                // 单槽工作线程重试期间保留现有在途计数。
                let retry_delay = managed_session_wake_retry_delay(invocation.retry_attempt);
                self.callback_retry_center
                    .enqueue(ManagedSessionEventCallbackRetryItem {
                        invocation,
                        retry_delay,
                    });
            }
        }
    }

    /// Retire one callback invocation and schedule catch-up for a newer pending queue epoch.
    /// 退役一个回调调用，并为更新的待处理队列纪元调度补发。
    fn finish_callback_invocation(&self, generation: u64) {
        // Center state locked only after arbitrary callback code has returned.
        // 仅在任意回调代码返回后加锁的事件中心状态。
        let mut state = self.lock_state();
        // Whether the callback generation counter reached zero and can be removed.
        // 当前回调代际计数是否归零并可被移除。
        let remove_generation = match state.inflight_callbacks.get_mut(&generation) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => true,
            None => false,
        };
        if remove_generation {
            state.inflight_callbacks.remove(&generation);
        }
        // A queue may have drained and become nonempty again while the prior invocation was active.
        // 先前调用活动期间，队列可能已排空后再次变为非空。
        let catch_up = self.schedule_callback_locked(&mut state, Some(generation));
        self.callback_idle.notify_all();
        drop(state);
        if let Some(invocation) = catch_up {
            self.callback_retry_center
                .enqueue(ManagedSessionEventCallbackRetryItem {
                    invocation,
                    retry_delay: Duration::ZERO,
                });
        }
    }

    /// Retry one queued callback only while its generation and nonempty epoch remain current.
    /// 仅在其代际与非空纪元仍然有效时重试一个已排队回调。
    fn retry_callback_invocation(&self, item: ManagedSessionEventCallbackRetryItem) {
        let active = {
            let state = self.lock_state();
            state.callback.as_ref().is_some_and(|registration| {
                registration.generation == item.invocation.generation
                    && state.nonempty_epoch == Some(item.invocation.epoch)
            })
        };
        if active {
            self.invoke_callback(item.invocation);
        } else {
            let generation = item.invocation.generation;
            // Cancelled pending ownership is released before the retired generation is signalled idle.
            // 在通知已退役代际空闲前释放已取消的待处理所有权。
            drop(item);
            self.finish_callback_invocation(generation);
        }
    }

    /// Return whether this thread is executing a callback from the same center.
    /// 返回当前线程是否正在执行同一事件中心的回调。
    fn callback_is_active_on_current_thread(&self) -> bool {
        // Stable center address because every center is allocated behind an Arc.
        // 稳定的事件中心地址，因为每个事件中心都分配在 Arc 后方。
        let center_address = self as *const Self as usize;
        ACTIVE_MANAGED_SESSION_EVENT_CALLBACKS
            .with(|active| active.borrow().contains(&center_address))
    }

    /// Acquire the center state lock and recover any poisoned state.
    /// 获取事件中心状态锁，并恢复任何中毒状态。
    fn lock_state(&self) -> MutexGuard<'_, ManagedSessionEventCenterState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait for callback retirement and recover a poisoned center state lock.
    /// 等待回调退役，并恢复中毒的事件中心状态锁。
    fn wait_callback_state<'a>(
        &self,
        state: MutexGuard<'a, ManagedSessionEventCenterState>,
    ) -> MutexGuard<'a, ManagedSessionEventCenterState> {
        self.callback_idle
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait for event changes until `timeout` and recover a poisoned state lock.
    /// 在 `timeout` 期限内等待事件变化，并恢复中毒状态锁。
    fn wait_timeout_state<'a>(
        &self,
        state: MutexGuard<'a, ManagedSessionEventCenterState>,
        timeout: Duration,
    ) -> (MutexGuard<'a, ManagedSessionEventCenterState>, bool) {
        match self.changed.wait_timeout(state, timeout) {
            Ok((state, result)) => (state, result.timed_out()),
            Err(poisoned) => {
                let (state, result) = poisoned.into_inner();
                (state, result.timed_out())
            }
        }
    }
}

impl ManagedSessionEventCallbackRetryCenter {
    /// Create one open empty single-slot callback retry scheduler.
    /// 创建一个开放且为空的单槽回调重试调度器。
    fn new() -> Self {
        Self {
            state: Mutex::new(ManagedSessionEventCallbackRetryState {
                pending: None,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    /// Enqueue the only in-flight callback invocation and wake its worker.
    /// 入队唯一的在途回调调用并唤醒其工作线程。
    fn enqueue(&self, item: ManagedSessionEventCallbackRetryItem) {
        let mut state = self.lock_state();
        if state.closed {
            return;
        }
        // The event center admits only one in-flight invocation for the active generation.
        // 事件中心仅允许活动代际存在一个在途调用。
        assert!(
            state.pending.is_none(),
            "managed session callback retry slot invariant violated"
        );
        state.pending = Some(item);
        self.changed.notify_one();
    }

    /// Wait for and remove one retry item, or return `None` after closure.
    /// 等待并移除一个重试项，或在关闭后返回 `None`。
    fn wait_next(&self) -> Option<ManagedSessionEventCallbackRetryItem> {
        let mut state = self.lock_state();
        loop {
            if let Some(item) = state.pending.take() {
                return Some(item);
            }
            if state.closed {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Close retries, release any pending callback, and wake the worker.
    /// 关闭重试、释放任何待处理回调并唤醒工作线程。
    fn close(&self) {
        let mut state = self.lock_state();
        state.closed = true;
        state.pending = None;
        self.changed.notify_all();
    }

    /// Acquire retry state while recovering after lock poisoning.
    /// 获取重试状态，并在锁中毒后恢复。
    fn lock_state(&self) -> MutexGuard<'_, ManagedSessionEventCallbackRetryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Return capped exponential backoff for one explicit callback failure attempt.
/// 返回单次显式回调失败尝试对应的封顶指数退避时长。
fn managed_session_wake_retry_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let multiplier = 1u32.checked_shl(exponent).unwrap_or(u32::MAX);
    MANAGED_SESSION_WAKE_RETRY_INITIAL_DELAY
        .saturating_mul(multiplier)
        .min(MANAGED_SESSION_WAKE_RETRY_MAX_DELAY)
}

/// Run the single callback retry worker without retaining its event center.
/// 运行单个回调重试工作线程，且不持有其事件中心。
fn run_managed_session_event_callback_retry_worker(
    center: Weak<ManagedSessionEventCenter>,
    retry_center: Arc<ManagedSessionEventCallbackRetryCenter>,
) {
    while let Some(item) = retry_center.wait_next() {
        if !item.retry_delay.is_zero() {
            std::thread::sleep(item.retry_delay);
        }
        let Some(center) = center.upgrade() else {
            return;
        };
        center.retry_callback_invocation(item);
        // Release transient ownership before waiting so the worker never keeps the center alive.
        // 在等待前释放临时所有权，确保工作线程绝不延长事件中心生命周期。
        drop(center);
    }
}

impl Drop for ManagedSessionEventCenter {
    /// Close and join the callback retry worker without ever joining the current thread.
    /// 关闭并等待回调重试工作线程，同时绝不等待当前线程自身。
    fn drop(&mut self) {
        self.callback_retry_center.close();
        if let Some(worker) = self
            .callback_retry_worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
    }
}

/// Thread-local callback execution marker removed automatically after callback completion.
/// 回调完成后自动移除的线程局部回调执行标记。
struct ManagedSessionEventCallbackExecutionScope {
    /// Address of the event center whose callback is executing.
    /// 正在执行回调的事件中心地址。
    center_address: usize,
}

impl ManagedSessionEventCallbackExecutionScope {
    /// Enter one callback execution scope for the supplied event center.
    /// 为指定事件中心进入一个回调执行作用域。
    ///
    /// `center` remains alive for the complete synchronous callback invocation.
    /// `center` 在完整同步回调调用期间保持存活。
    ///
    /// Return a guard that removes the thread-local marker on drop.
    /// 返回一个在析构时移除线程局部标记的保护对象。
    fn enter(center: &ManagedSessionEventCenter) -> Self {
        // Stable address of the Arc-allocated event center.
        // Arc 分配事件中心的稳定地址。
        let center_address = center as *const ManagedSessionEventCenter as usize;
        ACTIVE_MANAGED_SESSION_EVENT_CALLBACKS.with(|active| {
            active.borrow_mut().push(center_address);
        });
        Self { center_address }
    }
}

impl Drop for ManagedSessionEventCallbackExecutionScope {
    /// Remove this callback scope from the current thread's active-center stack.
    /// 从当前线程活动事件中心栈中移除当前回调作用域。
    fn drop(&mut self) {
        ACTIVE_MANAGED_SESSION_EVENT_CALLBACKS.with(|active| {
            // Mutable active-center stack searched from the innermost callback outward.
            // 从最内层回调向外查找的可变活动事件中心栈。
            let mut active = active.borrow_mut();
            if let Some(index) = active
                .iter()
                .rposition(|address| *address == self.center_address)
            {
                active.remove(index);
            }
        });
    }
}

/// Return the logical slot occupied by one public event.
/// 返回单个公开事件占用的逻辑事件槽。
fn event_slot(event: &RuntimeManagedSessionEvent) -> ManagedSessionEventSlot {
    ManagedSessionEventSlot {
        token: ManagedSessionEventToken {
            system_lease_id: event.system_lease_id.clone(),
            sid: event.sid.clone(),
            generation: event.generation,
            managed_session_id: event.managed_session_id,
        },
        kind: event.kind,
    }
}

/// Return whether one public event belongs to the supplied session token.
/// 返回单个公开事件是否属于指定会话令牌。
fn event_matches_token(
    event: &RuntimeManagedSessionEvent,
    token: &ManagedSessionEventToken,
) -> bool {
    event.system_lease_id == token.system_lease_id
        && event.sid == token.sid
        && event.generation == token.generation
        && event.managed_session_id == token.managed_session_id
}

/// Return whether any callback generation at or before the retirement barrier is still active.
/// 返回退役屏障及之前是否仍有活动回调代际。
fn has_inflight_callback_at_or_before(
    state: &ManagedSessionEventCenterState,
    retirement_barrier: u64,
) -> bool {
    state
        .inflight_callbacks
        .iter()
        .any(|(generation, count)| *generation <= retirement_barrier && *count > 0)
}

#[cfg(test)]
mod tests {
    use super::{
        ManagedSessionEventCenter, ManagedSessionEventToken, RuntimeManagedSessionEventKind,
    };
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Build one unique managed-session event token for tests.
    /// 为测试构造一个唯一受管会话事件令牌。
    ///
    /// `id` is reused across lease, generation, and session identity fields for readability.
    /// `id` 为便于阅读而复用于租约、代际及会话身份字段。
    ///
    /// Return one complete token accepted by the event center.
    /// 返回一个事件中心可接受的完整令牌。
    fn test_token(id: u64) -> ManagedSessionEventToken {
        ManagedSessionEventToken::new(format!("lease-{id}"), format!("sid-{id}"), id, id)
    }

    /// Wait until one asynchronous callback counter reaches its required minimum.
    /// 等待一个异步回调计数器达到所需最小值。
    fn wait_for_callback_count(counter: &AtomicUsize, minimum: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while counter.load(Ordering::Acquire) < minimum && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(counter.load(Ordering::Acquire) >= minimum);
    }

    /// Probe whose drop records that callback-owned user data has been released.
    /// 析构时记录回调所拥有用户数据已被释放的探针。
    struct CallbackDropProbe {
        /// Shared flag set when the callback releases its final probe owner.
        /// 回调释放探针最终所有者时设置的共享标志。
        dropped: Arc<AtomicBool>,
    }

    impl Drop for CallbackDropProbe {
        /// Record callback-owned user-data release.
        /// 记录回调所拥有用户数据的释放。
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    /// Verify each registered session owns exactly four bounded logical slots.
    /// 验证每个已注册会话恰好拥有四个有界逻辑事件槽。
    #[test]
    fn event_center_reserves_four_bounded_slots_per_session() {
        // Single-session center whose exact logical capacity must be four.
        // 精确逻辑容量必须为四的单会话事件中心。
        let center = ManagedSessionEventCenter::new(1).expect("create event center");
        // Registered session whose four event kinds fill the center.
        // 其四种事件类型填满事件中心的已注册会话。
        let token = test_token(1);
        center
            .register_session(token.clone())
            .expect("register first session");
        assert_eq!(center.capacity(), 4);
        assert!(center.register_session(test_token(2)).is_err());

        for kind in [
            RuntimeManagedSessionEventKind::StdoutReadable,
            RuntimeManagedSessionEventKind::StderrReadable,
            RuntimeManagedSessionEventKind::Exited,
            RuntimeManagedSessionEventKind::Failed,
        ] {
            assert!(center.publish(&token, kind).expect("publish event"));
        }
        assert!(
            !center
                .publish(&token, RuntimeManagedSessionEventKind::Exited)
                .expect("coalesce terminal event")
        );

        // Full batch proving terminal events survive alongside both readable events.
        // 证明终态事件与两个可读事件共同保留的完整批次。
        let batch = center.poll(4).expect("poll full event batch");
        assert_eq!(batch.events.len(), 4);
        let kinds: HashSet<_> = batch.events.into_iter().map(|event| event.kind).collect();
        assert_eq!(kinds.len(), 4);
    }

    /// Verify repeated readable events coalesce and global sequences remain monotonic.
    /// 验证重复可读事件会合并且全局序号保持单调。
    #[test]
    fn readable_events_coalesce_with_monotonic_sequences() {
        // Two-session center used to prove sequence order crosses session identities.
        // 用于证明序号顺序跨越会话身份的双会话事件中心。
        let center = ManagedSessionEventCenter::new(2).expect("create event center");
        // First and second registered session tokens.
        // 第一与第二个已注册会话令牌。
        let first = test_token(1);
        let second = test_token(2);
        center.register_session(first.clone()).unwrap();
        center.register_session(second.clone()).unwrap();

        assert!(
            center
                .publish(&first, RuntimeManagedSessionEventKind::StdoutReadable)
                .unwrap()
        );
        assert!(
            !center
                .publish(&first, RuntimeManagedSessionEventKind::StdoutReadable)
                .unwrap()
        );
        assert!(
            center
                .publish(&second, RuntimeManagedSessionEventKind::StderrReadable)
                .unwrap()
        );

        // Ordered batch containing one event for each occupied slot.
        // 每个已占用事件槽各包含一个事件的有序批次。
        let batch = center.poll(8).unwrap();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0].sequence, 1);
        assert_eq!(batch.events[1].sequence, 2);

        assert!(
            center
                .publish(&first, RuntimeManagedSessionEventKind::Exited)
                .unwrap()
        );
        assert_eq!(center.poll(1).unwrap().events[0].sequence, 3);
    }

    /// Verify nonblocking poll, maximum batch size, and argument validation.
    /// 验证非阻塞轮询、最大批次大小及参数校验。
    #[test]
    fn poll_is_nonblocking_and_honors_max_events() {
        // Event center and token used by immediate polling paths.
        // 立即轮询路径使用的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();

        // Monotonic start time used to bound the zero-timeout path.
        // 用于限制零超时路径的单调开始时间。
        let started = Instant::now();
        let empty = center.poll(1).unwrap();
        assert!(empty.events.is_empty());
        assert_eq!(empty.remaining, 0);
        assert!(!empty.timed_out);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(center.poll(0).is_err());

        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StderrReadable)
            .unwrap();
        let first = center.poll(1).unwrap();
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.remaining, 1);
        assert!(!first.timed_out);
        let second = center.poll(1).unwrap();
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.remaining, 0);
        assert!(!second.timed_out);
    }

    /// Verify extreme wait timeouts are rejected deterministically before blocking.
    /// 验证极端等待超时会在阻塞前被确定性拒绝。
    #[test]
    fn huge_timeout_is_rejected_deterministically() {
        // Empty open center used to validate the portable finite timeout bound.
        // 用于验证可移植有限超时上限的空开放事件中心。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let error = center.wait(1, u64::MAX).unwrap_err();
        assert!(error.contains("timeout_ms is too large"));
    }

    /// Verify a waiting consumer wakes when one event is published.
    /// 验证等待中的消费者会在事件发布时被唤醒。
    #[test]
    fn wait_wakes_for_published_event() {
        // Shared center and registered token used across waiter and publisher threads.
        // 在等待与发布线程间共享的事件中心及已注册令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        // Readiness channel ensuring the waiter thread has started.
        // 确保等待线程已启动的就绪通道。
        let (ready_tx, ready_rx) = mpsc::channel();
        let waiting_center = center.clone();
        let waiter = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            waiting_center.wait(1, 5_000).unwrap()
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        let batch = waiter.join().unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.remaining, 0);
        assert!(!batch.timed_out);
    }

    /// Verify closing the center wakes waiters and rejects later publications.
    /// 验证关闭事件中心会唤醒等待方并拒绝后续发布。
    #[test]
    fn close_wakes_waiters_and_rejects_new_work() {
        // Shared center and token observed by the close waiter.
        // 由关闭等待方观察的共享事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let waiting_center = center.clone();
        let waiter = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            waiting_center.wait(1, 5_000)
        });
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(center.close().unwrap());
        assert!(!center.close().unwrap());
        let error = waiter.join().unwrap().unwrap_err();
        assert!(error.contains("event center is closed"));
        assert!(
            center
                .publish(&token, RuntimeManagedSessionEventKind::Exited)
                .is_err()
        );
        assert!(center.register_session(test_token(2)).is_err());
    }

    /// Verify unregister removes pending events and makes later publications stale.
    /// 验证注销会移除待处理事件并使后续发布变为陈旧。
    #[test]
    fn unregister_removes_pending_events_and_rejects_stale_publishers() {
        // Registered token removed before its pending event can be consumed.
        // 在待处理事件被消费前移除的已注册令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::Failed)
            .unwrap();
        assert!(center.unregister_session(&token));
        assert!(!center.unregister_session(&token));
        assert!(center.poll(4).unwrap().events.is_empty());
        assert!(
            center
                .publish(&token, RuntimeManagedSessionEventKind::Exited)
                .is_err()
        );
    }

    /// Verify callbacks fire once per empty-to-nonempty edge and catch up on registration.
    /// 验证回调每次队列由空变为非空仅触发一次，并在注册时补发唤醒。
    #[test]
    fn callback_is_edge_triggered_and_registration_catches_up() {
        // Center with one pending event before callback registration.
        // 在回调注册前已有一个待处理事件的事件中心。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        // Shared callback count observed after asynchronous dispatcher completion.
        // 在异步调度器完成后观察的共享回调计数。
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_callback = callback_count.clone();
        center
            .set_wake_callback(Some(Arc::new(move || {
                callback_count_for_callback.fetch_add(1, Ordering::AcqRel);
            })))
            .unwrap();
        wait_for_callback_count(&callback_count, 1);

        center
            .publish(&token, RuntimeManagedSessionEventKind::StderrReadable)
            .unwrap();
        assert_eq!(callback_count.load(Ordering::Acquire), 1);
        center.poll(4).unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::Exited)
            .unwrap();
        wait_for_callback_count(&callback_count, 2);
    }

    /// Verify an explicit host scheduling failure retries the same nonempty edge until accepted.
    /// 验证显式宿主调度失败会重试同一非空边沿，直至被接受。
    #[test]
    fn fallible_callback_failure_retries_without_new_event_publication() {
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let callback_attempts = Arc::clone(&attempts);
        center
            .set_fallible_wake_callback(Some(Arc::new(move || {
                let attempt = callback_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                if attempt == 1 {
                    Err("forced host scheduler rejection".to_string())
                } else {
                    Ok(())
                }
            })))
            .unwrap();

        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        wait_for_callback_count(&attempts, 2);
        assert_eq!(attempts.load(Ordering::Acquire), 2);
        assert_eq!(center.poll(1).unwrap().events.len(), 1);
        center.set_fallible_wake_callback(None).unwrap();
    }

    /// Verify draining a failed edge cancels backoff and callback clearing makes user data silent.
    /// 验证排空失败边沿会取消退避，且清除回调会使用户数据静默。
    #[test]
    fn fallible_callback_backoff_cancels_after_event_drain() {
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let callback_attempts = Arc::clone(&attempts);
        center
            .set_fallible_wake_callback(Some(Arc::new(move || {
                callback_attempts.fetch_add(1, Ordering::AcqRel);
                Err("persistent host scheduler rejection".to_string())
            })))
            .unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        wait_for_callback_count(&attempts, 1);
        assert_eq!(center.poll(1).unwrap().events.len(), 1);
        center.set_fallible_wake_callback(None).unwrap();
        let settled_attempts = attempts.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(attempts.load(Ordering::Acquire), settled_attempts);
    }

    /// Verify arbitrary host callback latency never blocks the event publisher thread.
    /// 验证任意宿主回调延迟绝不会阻塞事件发布线程。
    #[test]
    fn callback_dispatch_never_blocks_event_publisher() {
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let release = Arc::new(Barrier::new(2));
        let callback_release = Arc::clone(&release);
        let (entered_tx, entered_rx) = mpsc::channel();
        center
            .set_wake_callback(Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                callback_release.wait();
            })))
            .unwrap();
        let publisher_center = Arc::clone(&center);
        let (published_tx, published_rx) = mpsc::channel();
        let publisher = thread::spawn(move || {
            publisher_center
                .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
                .unwrap();
            published_tx.send(()).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        published_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release.wait();
        center.set_wake_callback(None).unwrap();
        publisher.join().unwrap();
    }

    /// Verify host wake retry backoff grows exponentially and remains capped.
    /// 验证宿主唤醒重试退避按指数增长且保持封顶。
    #[test]
    fn callback_retry_backoff_is_exponential_and_capped() {
        assert_eq!(
            super::managed_session_wake_retry_delay(1),
            super::MANAGED_SESSION_WAKE_RETRY_INITIAL_DELAY
        );
        assert_eq!(
            super::managed_session_wake_retry_delay(2),
            super::MANAGED_SESSION_WAKE_RETRY_INITIAL_DELAY.saturating_mul(2)
        );
        assert_eq!(
            super::managed_session_wake_retry_delay(u32::MAX),
            super::MANAGED_SESSION_WAKE_RETRY_MAX_DELAY
        );
    }

    /// Verify wake callbacks execute without holding the event-center state lock.
    /// 验证唤醒回调执行时不会持有事件中心状态锁。
    #[test]
    fn callback_can_poll_without_lock_reentrancy_deadlock() {
        // Center and token whose callback drains the event that triggered it.
        // 其回调会取出触发事件的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let callback_center = center.clone();
        let drained = Arc::new(AtomicUsize::new(0));
        let drained_for_callback = drained.clone();
        center
            .set_wake_callback(Some(Arc::new(move || {
                let batch = callback_center.poll(1).expect("callback poll");
                drained_for_callback.fetch_add(batch.events.len(), Ordering::AcqRel);
            })))
            .unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        wait_for_callback_count(&drained, 1);
        assert_eq!(drained.load(Ordering::Acquire), 1);
    }

    /// Verify callback replacement waits for the retired callback invocation to finish.
    /// 验证回调替换会等待已退役回调调用完成。
    #[test]
    fn callback_replacement_waits_for_inflight_invocation() {
        // Center and token used to hold one callback invocation in flight.
        // 用于保持一个回调调用处于在途状态的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let release = Arc::new(Barrier::new(2));
        let release_for_callback = release.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        center
            .set_wake_callback(Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_for_callback.wait();
            })))
            .unwrap();

        let publishing_center = center.clone();
        let publishing_token = token.clone();
        let publisher = thread::spawn(move || {
            publishing_center
                .publish(
                    &publishing_token,
                    RuntimeManagedSessionEventKind::StdoutReadable,
                )
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let replacing_center = center.clone();
        let (replace_started_tx, replace_started_rx) = mpsc::channel();
        let (replace_done_tx, replace_done_rx) = mpsc::channel();
        let replacer = thread::spawn(move || {
            replace_started_tx.send(()).unwrap();
            replacing_center
                .set_wake_callback(Some(Arc::new(|| {})))
                .unwrap();
            replace_done_tx.send(()).unwrap();
        });
        replace_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(
            replace_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        release.wait();
        replace_done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        publisher.join().unwrap();
        replacer.join().unwrap();
    }

    /// Verify replacement serializes a new generation behind one failing retired invocation.
    /// 验证替换会把新代际串行化到一个失败的已退役调用之后。
    #[test]
    fn callback_replacement_serializes_generations_through_failure_retry() {
        // Center and token used to retain one old callback while a new queue epoch arrives.
        // 用于在新队列纪元到达时保留一个旧回调的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        // Barrier holds the old fallible callback after its retry worker removed the single slot.
        // Barrier 会在重试工作线程取走单槽后暂停旧的可失败回调。
        let release = Arc::new(Barrier::new(2));
        let release_for_callback = Arc::clone(&release);
        let (entered_tx, entered_rx) = mpsc::channel();
        center
            .set_fallible_wake_callback(Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_for_callback.wait();
                Err("retired callback failure".to_string())
            })))
            .unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(center.poll(1).unwrap().events.len(), 1);

        // NewCount proves the replacement callback receives the epoch queued during retirement.
        // NewCount 证明替换回调会收到在退役期间排队的纪元。
        let new_count = Arc::new(AtomicUsize::new(0));
        let new_count_for_callback = Arc::clone(&new_count);
        let replacing_center = Arc::clone(&center);
        let (replace_done_tx, replace_done_rx) = mpsc::channel();
        let replacer = thread::spawn(move || {
            replacing_center
                .set_wake_callback(Some(Arc::new(move || {
                    new_count_for_callback.fetch_add(1, Ordering::AcqRel);
                })))
                .unwrap();
            replace_done_tx.send(()).unwrap();
        });
        // Installed generation becomes visible before replacement waits for the old generation.
        // 已安装代际会在替换等待旧代际之前变为可见。
        let install_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let installed = center
                .lock_state()
                .callback
                .as_ref()
                .is_some_and(|registration| registration.generation == 2);
            if installed {
                break;
            }
            assert!(
                Instant::now() < install_deadline,
                "replacement callback generation should become visible"
            );
            thread::sleep(Duration::from_millis(1));
        }
        center
            .publish(&token, RuntimeManagedSessionEventKind::StderrReadable)
            .unwrap();
        assert!(
            replace_done_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );

        release.wait();
        replace_done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        wait_for_callback_count(&new_count, 1);
        assert_eq!(center.poll(1).unwrap().events.len(), 1);
        center.set_wake_callback(None).unwrap();
        replacer.join().unwrap();
    }

    /// Verify callback clearing waits for in-flight calls before releasing callback-owned data.
    /// 验证清除回调会等待在途调用，并在之后释放回调所拥有的数据。
    #[test]
    fn callback_clear_waits_and_releases_callback_owned_data() {
        // Center and token used to hold the callback while clear waits.
        // 用于在清除等待期间保持回调的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let release = Arc::new(Barrier::new(2));
        let release_for_callback = release.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let probe = CallbackDropProbe {
            dropped: dropped.clone(),
        };
        center
            .set_wake_callback(Some(Arc::new(move || {
                let _probe = &probe;
                entered_tx.send(()).unwrap();
                release_for_callback.wait();
            })))
            .unwrap();

        let publishing_center = center.clone();
        let publishing_token = token.clone();
        let publisher = thread::spawn(move || {
            publishing_center
                .publish(
                    &publishing_token,
                    RuntimeManagedSessionEventKind::StdoutReadable,
                )
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let clearing_center = center.clone();
        let (clear_started_tx, clear_started_rx) = mpsc::channel();
        let (clear_done_tx, clear_done_rx) = mpsc::channel();
        let clearer = thread::spawn(move || {
            clear_started_tx.send(()).unwrap();
            clearing_center.set_wake_callback(None).unwrap();
            clear_done_tx.send(()).unwrap();
        });
        clear_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(
            clear_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        assert!(!dropped.load(Ordering::Acquire));
        release.wait();
        clear_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(dropped.load(Ordering::Acquire));
        publisher.join().unwrap();
        clearer.join().unwrap();
    }

    /// Verify a callback cannot synchronously replace itself and deadlock quiescence.
    /// 验证回调无法同步替换自身并导致收敛死锁。
    #[test]
    fn callback_self_replacement_returns_error_without_deadlock() {
        // Center and token whose callback attempts an unsupported self-clear.
        // 其回调尝试不受支持自清除的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        let callback_center = center.clone();
        let (result_tx, result_rx) = mpsc::channel();
        center
            .set_wake_callback(Some(Arc::new(move || {
                result_tx
                    .send(callback_center.set_wake_callback(None))
                    .unwrap();
            })))
            .unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        let error = result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .expect_err("self replacement should fail");
        assert!(error.contains("cannot replace itself"));
    }

    /// Verify callback panics cannot strand in-flight counts or poison callback replacement.
    /// 验证回调 panic 不会遗留在途计数或破坏回调替换。
    #[test]
    fn callback_panic_does_not_block_later_clear() {
        // Center and token whose wake callback intentionally panics.
        // 其唤醒回调故意 panic 的事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        center
            .set_wake_callback(Some(Arc::new(|| panic!("wake callback panic"))))
            .unwrap();
        center
            .publish(&token, RuntimeManagedSessionEventKind::StdoutReadable)
            .unwrap();
        center.set_wake_callback(None).unwrap();
    }

    /// Verify concurrent repeated publishers remain bounded by occupied logical slots.
    /// 验证并发重复发布者仍受已占用逻辑事件槽限制。
    #[test]
    fn concurrent_publishers_remain_bounded_and_coalesced() {
        // Shared center and token targeted by every producer thread.
        // 所有生产者线程共同指向的共享事件中心与令牌。
        let center = ManagedSessionEventCenter::new(1).unwrap();
        let token = test_token(1);
        center.register_session(token.clone()).unwrap();
        // Producer handles used to join every concurrent publisher.
        // 用于等待全部并发发布者的生产者句柄集合。
        let mut producers = Vec::new();
        for index in 0..8 {
            let producer_center = center.clone();
            let producer_token = token.clone();
            producers.push(thread::spawn(move || {
                let kind = if index % 2 == 0 {
                    RuntimeManagedSessionEventKind::StdoutReadable
                } else {
                    RuntimeManagedSessionEventKind::StderrReadable
                };
                for _ in 0..100 {
                    producer_center.publish(&producer_token, kind).unwrap();
                }
            }));
        }
        for producer in producers {
            producer.join().unwrap();
        }
        let batch = center.poll(8).unwrap();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0].sequence, 1);
        assert_eq!(batch.events[1].sequence, 2);
    }
}
