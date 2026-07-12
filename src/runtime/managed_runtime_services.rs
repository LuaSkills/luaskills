use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::runtime::managed_session_events::{
    ManagedSessionEventCenter, ManagedSessionEventToken, RuntimeManagedSessionEventKind,
};
use crate::runtime::process_session::{
    ManagedProcessSessionCleanup, ManagedProcessSessionCleanupHandle, ManagedProcessSessionCore,
    ManagedProcessSessionObserver,
};
use crate::runtime_logging::warn as log_warn;
use crate::runtime_options::LuaRuntimeManagedRuntimeConfig;

/// Delay between retries after one owner teardown attempt fails.
/// 单次所有者清理尝试失败后的重试间隔。
const MANAGED_RUNTIME_RETIREMENT_RETRY_DELAY: Duration = Duration::from_millis(10);

/// System lease identity needed to route background events to the correct host consumer.
/// 将后台事件路由到正确宿主消费者所需的 System 租约身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedRuntimeSessionEventIdentity {
    /// Opaque System lease identifier.
    /// 不透明 System 租约标识。
    pub(crate) lease_id: String,
    /// Stable System lease SID.
    /// 稳定 System 租约 SID。
    pub(crate) sid: String,
    /// SID-local System lease generation.
    /// SID 内 System 租约代际。
    pub(crate) generation: u64,
}

/// Lightweight transaction context installed in Lua app data during one lease evaluation.
/// 单次租约执行期间安装到 Lua 应用数据中的轻量事务上下文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ManagedRuntimeTransactionContext {
    /// Engine-local resource transaction identifier.
    /// 引擎内部资源事务标识。
    transaction_id: u64,
    /// Exact package-lifetime owner token.
    /// 精确包生命周期所有者令牌。
    owner_token: u64,
}

/// Engine-owned service coordinating managed sessions, transactions, and background events.
/// 引擎拥有的服务，用于协调受管会话、事务与后台事件。
pub(crate) struct ManagedRuntimeServices {
    /// Validated host-selected persistent-session limits and defaults.
    /// 已校验的宿主选择持久会话上限与默认值。
    config: LuaRuntimeManagedRuntimeConfig,
    /// Mutable resource registry protected across Lua and background lifecycle calls.
    /// 在 Lua 与后台生命周期调用之间受保护的可变资源注册表。
    state: Mutex<ManagedRuntimeServicesState>,
    /// Durable bounded event center shared with host polling and wake callbacks.
    /// 与宿主轮询及唤醒回调共享的持久有界事件中心。
    event_center: Arc<ManagedSessionEventCenter>,
    /// Retry scheduler that retains only owner tokens and never process resources.
    /// 仅保留所有者令牌、绝不持有进程资源的重试调度器。
    retirement_retry_center: Arc<ManagedRuntimeRetirementRetryCenter>,
    /// Join handle for the single engine-local retirement retry worker.
    /// 单个引擎内部退役重试工作线程的等待句柄。
    retirement_retry_worker: Mutex<Option<JoinHandle<()>>>,
    /// Test-only count of deterministic teardown failures still to inject.
    /// 仅测试使用的待注入确定性清理失败次数。
    #[cfg(test)]
    forced_teardown_failures: AtomicUsize,
}

/// Engine-local scheduler for retrying owner teardown without retaining the engine.
/// 引擎内部调度器，用于在不持有引擎的情况下重试所有者清理。
struct ManagedRuntimeRetirementRetryCenter {
    /// Mutable retry queue protected independently from the managed-session registry.
    /// 与受管会话注册表相互独立保护的可变重试队列。
    state: Mutex<ManagedRuntimeRetirementRetryState>,
    /// Notification used to wake the single retry worker or close it deterministically.
    /// 用于唤醒单个重试工作线程或确定关闭它的通知量。
    changed: Condvar,
}

/// Mutable state for the deduplicated owner-retirement retry queue.
/// 去重所有者退役重试队列的可变状态。
struct ManagedRuntimeRetirementRetryState {
    /// Fair FIFO queue of owner- or session-scoped teardown targets.
    /// 所有者或会话粒度清理目标的公平 FIFO 队列。
    targets: VecDeque<ManagedRuntimeRetryTarget>,
    /// Deduplication set matching every target currently queued.
    /// 与当前已排队全部目标匹配的去重集合。
    queued_targets: HashSet<ManagedRuntimeRetryTarget>,
    /// Whether engine teardown permanently closed this scheduler.
    /// 引擎清理是否已永久关闭此调度器。
    closed: bool,
}

/// Exact teardown scope scheduled for persistent engine-local retry.
/// 为引擎内部持久重试调度的精确清理范围。
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum ManagedRuntimeRetryTarget {
    /// Every remaining session belonging to one retired package owner.
    /// 属于单个已退役包所有者的全部剩余会话。
    Owner(u64),
    /// One failed rollback or lifecycle session without affecting committed siblings.
    /// 单个失败回滚或生命周期会话，不影响已提交的同级会话。
    Session(u64),
}

/// Mutable engine-owned managed runtime resource state.
/// 引擎拥有的可变受管运行时资源状态。
struct ManagedRuntimeServicesState {
    /// Last allocated engine-local managed session identifier.
    /// 最近分配的引擎内部受管会话标识。
    next_session_id: u64,
    /// Last allocated resource transaction identifier.
    /// 最近分配的资源事务标识。
    next_transaction_id: u64,
    /// All live or launching sessions keyed by engine-local identifier.
    /// 按引擎内部标识索引的全部活动或启动中会话。
    sessions: HashMap<u64, ManagedRuntimeSessionRecord>,
    /// Open evaluation transactions keyed by transaction identifier.
    /// 按事务标识索引的开放执行事务。
    transactions: HashMap<u64, ManagedRuntimeTransactionRecord>,
}

/// One strongly owned managed session registry record.
/// 单个强所有权受管会话注册记录。
struct ManagedRuntimeSessionRecord {
    /// Exact package-lifetime owner token.
    /// 精确包生命周期所有者令牌。
    owner_token: u64,
    /// Strong process core retained until successful teardown and registry cleanup.
    /// 保留到成功清理进程与注册表为止的强进程核心。
    process: Option<ManagedProcessSessionCore>,
    /// Shared cleanup handle allowing engine rollback to release userdata-owned resources immediately.
    /// 允许引擎回滚立即释放 userdata 所有资源的共享清理句柄。
    cleanup: Option<Arc<ManagedProcessSessionCleanupHandle>>,
    /// Optional System event token reserved before process launch.
    /// 进程启动前预留的可选 System 事件令牌。
    event_token: Option<ManagedSessionEventToken>,
}

/// Session identifiers created inside one open evaluation transaction.
/// 单个开放执行事务内创建的会话标识集合。
struct ManagedRuntimeTransactionRecord {
    /// Exact package-lifetime owner token authorized for this transaction.
    /// 当前事务授权的精确包生命周期所有者令牌。
    owner_token: u64,
    /// Sessions requiring rollback if evaluation fails.
    /// 执行失败时需要回滚的会话。
    session_ids: Vec<u64>,
}

/// RAII evaluation transaction that rolls back newly created sessions unless committed.
/// 除非提交，否则会回滚新建会话的 RAII 执行事务。
pub(crate) struct ManagedRuntimeTransactionGuard {
    /// Owning services instance.
    /// 所属服务实例。
    services: Arc<ManagedRuntimeServices>,
    /// Lightweight context copied into the Lua VM.
    /// 复制到 Lua VM 中的轻量上下文。
    context: ManagedRuntimeTransactionContext,
    /// Whether commit or explicit rollback already completed.
    /// 提交或显式回滚是否已经完成。
    finished: bool,
}

/// Pre-launch session reservation carrying an optional background observer.
/// 携带可选后台观察器的启动前会话预留。
pub(crate) struct ManagedRuntimeSessionReservation {
    /// Weak service reference preventing a reservation from extending engine lifetime.
    /// 防止预留延长引擎生命周期的弱服务引用。
    services: Weak<ManagedRuntimeServices>,
    /// Reserved engine-local session identifier.
    /// 已预留的引擎内部会话标识。
    session_id: u64,
    /// Optional observer publishing System session events.
    /// 发布 System 会话事件的可选观察器。
    observer: Option<Arc<dyn ManagedProcessSessionObserver>>,
    /// Whether activation transferred the reservation into the live registry.
    /// 激活是否已将预留转移到活动注册表。
    activated: bool,
}

/// Process observer that translates package-agnostic notifications into durable event slots.
/// 将包无关通知转换为持久事件槽的进程观察器。
struct ManagedRuntimeSessionEventObserver {
    /// Weak event center avoiding background ownership of the engine.
    /// 避免后台线程拥有引擎的弱事件中心。
    event_center: Weak<ManagedSessionEventCenter>,
    /// Immutable registered session event token.
    /// 不可变已注册会话事件令牌。
    token: ManagedSessionEventToken,
}

impl ManagedRuntimeServices {
    /// Create one empty engine-owned resource service with stable default limits.
    /// 使用稳定默认上限创建一个空的引擎所有资源服务。
    #[cfg(test)]
    pub(crate) fn new() -> Result<Arc<Self>, String> {
        Self::new_with_config(LuaRuntimeManagedRuntimeConfig::default())
    }

    /// Create one empty engine-owned resource service with a host-selected validated policy.
    /// 使用宿主选择且已校验的策略创建一个空的引擎所有资源服务。
    ///
    /// `config` controls the engine session capacity and the per-stream default buffer size.
    /// `config` 控制引擎会话容量与每个流的默认缓冲大小。
    ///
    /// Returns a live service or a configuration/event-worker initialization error.
    /// 返回活动服务，或配置及事件工作线程初始化错误。
    pub(crate) fn new_with_config(
        config: LuaRuntimeManagedRuntimeConfig,
    ) -> Result<Arc<Self>, String> {
        config.validate()?;
        // Event center reserves four reliable logical slots for every possible session.
        // 事件中心为每个可能会话预留四个可靠逻辑槽。
        let event_center =
            ManagedSessionEventCenter::new(config.persistent_session_limit_per_engine)?;
        // Independent retry center lets its worker wait without extending engine lifetime.
        // 独立重试中心使工作线程能够在不延长引擎生命周期的情况下等待。
        let retirement_retry_center = Arc::new(ManagedRuntimeRetirementRetryCenter::new());
        let services = Arc::new(Self {
            config,
            state: Mutex::new(ManagedRuntimeServicesState {
                next_session_id: 0,
                next_transaction_id: 0,
                sessions: HashMap::new(),
                transactions: HashMap::new(),
            }),
            event_center,
            retirement_retry_center: Arc::clone(&retirement_retry_center),
            retirement_retry_worker: Mutex::new(None),
            #[cfg(test)]
            forced_teardown_failures: AtomicUsize::new(0),
        });
        let weak_services = Arc::downgrade(&services);
        let retry_worker = std::thread::Builder::new()
            .name("luaskills-managed-retirement".to_string())
            .spawn(move || {
                run_managed_runtime_retirement_retry_worker(weak_services, retirement_retry_center);
            })
            .map_err(|error| format!("spawn managed runtime retirement retry worker: {error}"))?;
        *services
            .retirement_retry_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(retry_worker);
        Ok(services)
    }

    /// Return the host-selected default retained bytes for each persistent-session output stream.
    /// 返回宿主选择的每个持久会话输出流默认保留字节数。
    pub(crate) fn persistent_session_default_buffer_limit_bytes_per_stream(&self) -> usize {
        self.config
            .persistent_session_default_buffer_limit_bytes_per_stream
    }

    /// Return the host-selected maximum launching or live persistent sessions for this engine.
    /// 返回宿主选择的当前引擎启动中或活动持久会话最大数量。
    #[cfg(test)]
    pub(crate) fn persistent_session_limit_per_engine(&self) -> usize {
        self.config.persistent_session_limit_per_engine
    }

    /// Return the durable event center used by Engine and FFI host APIs.
    /// 返回 Engine 与 FFI 宿主 API 使用的持久事件中心。
    pub(crate) fn event_center(&self) -> Arc<ManagedSessionEventCenter> {
        Arc::clone(&self.event_center)
    }

    /// Begin one lease-evaluation resource transaction for an exact package lifetime.
    /// 为精确包生命周期开始一个租约执行资源事务。
    pub(crate) fn begin_transaction(
        self: &Arc<Self>,
        owner_token: u64,
    ) -> Result<ManagedRuntimeTransactionGuard, String> {
        // Mutable state locked only while allocating and publishing the transaction record.
        // 仅在分配并发布事务记录期间锁定的可变状态。
        let mut state = self.lock_state();
        // Nonwrapping transaction identifier.
        // 不可回绕的事务标识。
        let transaction_id = state.next_transaction_id.checked_add(1).ok_or_else(|| {
            "managed runtime transaction identifier space is exhausted".to_string()
        })?;
        state.next_transaction_id = transaction_id;
        state.transactions.insert(
            transaction_id,
            ManagedRuntimeTransactionRecord {
                owner_token,
                session_ids: Vec::new(),
            },
        );
        Ok(ManagedRuntimeTransactionGuard {
            services: Arc::clone(self),
            context: ManagedRuntimeTransactionContext {
                transaction_id,
                owner_token,
            },
            finished: false,
        })
    }

    /// Reserve one session id, transaction slot, and optional System event registration.
    /// 预留一个会话 id、事务槽与可选 System 事件注册。
    pub(crate) fn reserve_session(
        self: &Arc<Self>,
        owner_token: u64,
        transaction: Option<ManagedRuntimeTransactionContext>,
        event_identity: Option<ManagedRuntimeSessionEventIdentity>,
    ) -> Result<ManagedRuntimeSessionReservation, String> {
        // Session id allocated and transaction ownership validated under the registry lock.
        // 在注册表锁内分配并验证事务所有权的会话 id。
        let session_id = {
            let mut state = self.lock_state();
            if state.sessions.len() >= self.config.persistent_session_limit_per_engine {
                return Err(format!(
                    "managed runtime session limit exceeded: {}",
                    self.config.persistent_session_limit_per_engine
                ));
            }
            if let Some(transaction) = transaction {
                let record = state
                    .transactions
                    .get(&transaction.transaction_id)
                    .ok_or_else(|| {
                        "managed runtime resource transaction is not active".to_string()
                    })?;
                if record.owner_token != owner_token || transaction.owner_token != owner_token {
                    return Err("managed runtime resource transaction owner mismatch".to_string());
                }
            }
            let session_id = state.next_session_id.checked_add(1).ok_or_else(|| {
                "managed runtime session identifier space is exhausted".to_string()
            })?;
            state.next_session_id = session_id;
            state.sessions.insert(
                session_id,
                ManagedRuntimeSessionRecord {
                    owner_token,
                    process: None,
                    cleanup: None,
                    event_token: None,
                },
            );
            if let Some(transaction) = transaction {
                state
                    .transactions
                    .get_mut(&transaction.transaction_id)
                    .expect("validated managed runtime transaction must remain present")
                    .session_ids
                    .push(session_id);
            }
            session_id
        };
        // Optional System event token registered before observer-visible process output can occur.
        // 在观察器可见进程输出发生前注册的可选 System 事件令牌。
        let event_token = event_identity.map(|identity| {
            ManagedSessionEventToken::new(
                identity.lease_id,
                identity.sid,
                identity.generation,
                session_id,
            )
        });
        if let Some(token) = event_token.as_ref()
            && let Err(error) = self.event_center.register_session(token.clone())
        {
            self.remove_reservation_record(session_id);
            return Err(error);
        }
        {
            // Registry update attaching the successfully registered event token.
            // 将成功注册事件令牌附加到记录的注册表更新。
            let mut state = self.lock_state();
            let Some(record) = state.sessions.get_mut(&session_id) else {
                if let Some(token) = event_token.as_ref() {
                    self.event_center.unregister_session(token);
                }
                return Err("managed runtime session reservation disappeared".to_string());
            };
            record.event_token = event_token.clone();
        }
        // Optional observer holds only a weak center reference and immutable token.
        // 仅持有弱事件中心引用与不可变令牌的可选观察器。
        let observer = event_token.map(|token| {
            Arc::new(ManagedRuntimeSessionEventObserver {
                event_center: Arc::downgrade(&self.event_center),
                token,
            }) as Arc<dyn ManagedProcessSessionObserver>
        });
        Ok(ManagedRuntimeSessionReservation {
            services: Arc::downgrade(self),
            session_id,
            observer,
            activated: false,
        })
    }

    /// Build userdata cleanup that unregisters the session before running language-specific cleanup.
    /// 构造 userdata 清理逻辑，先注销会话，再运行语言专属清理。
    pub(crate) fn session_cleanup(
        self: &Arc<Self>,
        session_id: u64,
        next: Option<ManagedProcessSessionCleanup>,
    ) -> Arc<ManagedProcessSessionCleanupHandle> {
        // Weak service reference prevents a Lua userdata from extending engine lifetime.
        // 弱服务引用防止 Lua userdata 延长引擎生命周期。
        let services = Arc::downgrade(self);
        // Independent weak reference used only to enqueue an exact retry after userdata Drop failure.
        // 仅用于在 userdata 析构失败后入队精确重试的独立弱引用。
        let retry_services = Arc::downgrade(self);
        ManagedProcessSessionCleanupHandle::new_with_retry(
            Box::new(move || {
                if let Some(services) = services.upgrade() {
                    services.unregister_session(session_id);
                }
                if let Some(next) = next {
                    next();
                }
            }),
            Box::new(move || {
                if let Some(services) = retry_services.upgrade() {
                    services.retirement_retry_center.enqueue_session(session_id);
                }
            }),
        )
    }

    /// Retire every managed session owned by one exact package lifetime.
    /// 退役由一个精确包生命周期拥有的全部受管会话。
    pub(crate) fn retire_owner(&self, owner_token: u64) -> Result<(), String> {
        let result = self.retry_retired_owner(owner_token);
        if result.is_err() {
            // A deduplicated background retry guarantees one consumed lease retirement is not lost.
            // 去重后台重试保证已消费的租约退役通知不会丢失。
            self.retirement_retry_center.enqueue(owner_token);
        }
        result
    }

    /// Retry teardown for every record still owned by one retired package lifetime.
    /// 重试清理由一个已退役包生命周期仍然拥有的全部记录。
    fn retry_retired_owner(&self, owner_token: u64) -> Result<(), String> {
        // Session records detached under lock and torn down after releasing it.
        // 在锁内摘除并在释放锁后清理的会话记录。
        let records = {
            let mut state = self.lock_state();
            let session_ids = state
                .sessions
                .iter()
                .filter_map(|(session_id, record)| {
                    (record.owner_token == owner_token).then_some(*session_id)
                })
                .collect::<Vec<_>>();
            for transaction in state.transactions.values_mut() {
                transaction
                    .session_ids
                    .retain(|session_id| !session_ids.contains(session_id));
            }
            session_ids
                .into_iter()
                .filter_map(|session_id| {
                    state
                        .sessions
                        .remove(&session_id)
                        .map(|record| (session_id, record))
                })
                .collect::<Vec<_>>()
        };
        self.teardown_records(records)
    }

    /// Retry teardown for one exact failed session without touching other owner sessions.
    /// 重试清理一个精确失败会话，不触及同一所有者的其他会话。
    ///
    /// `session_id` identifies the record republished by a previous failed teardown. Missing records
    /// are already complete and therefore succeed idempotently.
    /// `session_id` 标识上一次清理失败后重新发布的记录。记录缺失表示已完成，因此幂等成功。
    fn retry_session(&self, session_id: u64) -> Result<(), String> {
        let record = self.lock_state().sessions.remove(&session_id);
        match record {
            Some(record) => self.teardown_records(vec![(session_id, record)]),
            None => Ok(()),
        }
    }

    /// Unregister one session after its userdata has already torn down the process tree.
    /// 在 userdata 已清理进程树后注销单个会话。
    fn unregister_session(&self, session_id: u64) {
        // Optional record removed without retaining any process ownership.
        // 在不保留任何进程所有权的情况下移除的可选记录。
        let record = {
            let mut state = self.lock_state();
            for transaction in state.transactions.values_mut() {
                transaction
                    .session_ids
                    .retain(|candidate| *candidate != session_id);
            }
            state.sessions.remove(&session_id)
        };
        if let Some(token) = record.and_then(|record| record.event_token) {
            self.event_center.unregister_session(&token);
        }
    }

    /// Remove one failed pre-launch reservation and its transaction reference.
    /// 移除一个失败的启动前预留及其事务引用。
    fn remove_reservation_record(&self, session_id: u64) {
        self.unregister_session(session_id);
    }

    /// Activate one reservation by installing a strong process core.
    /// 通过安装强进程核心激活一个预留。
    fn activate_session(
        &self,
        session_id: u64,
        process: ManagedProcessSessionCore,
        cleanup: Arc<ManagedProcessSessionCleanupHandle>,
    ) -> Result<(), String> {
        let mut state = self.lock_state();
        let record = state
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| "managed runtime session reservation is no longer active".to_string())?;
        if record.process.is_some() {
            return Err("managed runtime session reservation is already active".to_string());
        }
        record.process = Some(process);
        record.cleanup = Some(cleanup);
        Ok(())
    }

    /// Commit one open transaction and retain every created session under its owner.
    /// 提交一个开放事务，并在其所有者下保留全部已创建会话。
    fn commit_transaction(&self, context: ManagedRuntimeTransactionContext) -> Result<(), String> {
        let mut state = self.lock_state();
        let record = state
            .transactions
            .remove(&context.transaction_id)
            .ok_or_else(|| "managed runtime resource transaction is not active".to_string())?;
        if record.owner_token != context.owner_token {
            return Err("managed runtime resource transaction owner mismatch".to_string());
        }
        Ok(())
    }

    /// Roll back one open transaction and terminate only sessions created by that evaluation.
    /// 回滚一个开放事务，并仅终止该次执行创建的会话。
    fn rollback_transaction(
        &self,
        context: ManagedRuntimeTransactionContext,
    ) -> Result<(), String> {
        // Transaction sessions detached atomically from the live registry.
        // 从活动注册表原子摘除的事务会话。
        let records = {
            let mut state = self.lock_state();
            let Some(transaction) = state.transactions.remove(&context.transaction_id) else {
                return Ok(());
            };
            if transaction.owner_token != context.owner_token {
                return Err("managed runtime resource transaction owner mismatch".to_string());
            }
            transaction
                .session_ids
                .into_iter()
                .filter_map(|session_id| {
                    state
                        .sessions
                        .remove(&session_id)
                        .map(|record| (session_id, record))
                })
                .collect::<Vec<_>>()
        };
        self.teardown_records(records)
    }

    /// Terminate detached process cores and unregister their durable event slots outside locks.
    /// 在锁外终止已摘除进程核心并注销其持久事件槽。
    fn teardown_records(
        &self,
        records: Vec<(u64, ManagedRuntimeSessionRecord)>,
    ) -> Result<(), String> {
        // First lifecycle error retained while every remaining resource still receives cleanup.
        // 在仍清理全部剩余资源时保留的首个生命周期错误。
        let mut first_error = None;
        // Failed live records retained so explicit retry or later owner retirement remains possible.
        // 保留清理失败的活动记录，使显式重试或后续所有者退役仍然可行。
        let mut retry_records = Vec::new();
        for (session_id, mut record) in records {
            if let Some(token) = record.event_token.as_ref() {
                self.event_center.unregister_session(token);
            }
            // Event identity is retired even when process teardown must be retried later.
            // 即使之后必须重试进程清理，也会立即退役事件身份。
            record.event_token = None;
            #[cfg(test)]
            let process_result = if self.consume_forced_teardown_failure() {
                Err("forced managed runtime teardown failure".to_string())
            } else {
                match record.process.as_ref() {
                    Some(process) => process.kill().map(|_| ()),
                    None => Ok(()),
                }
            };
            #[cfg(not(test))]
            let process_result = match record.process.as_ref() {
                Some(process) => process.kill().map(|_| ()),
                None => Ok(()),
            };
            if let Err(error) = process_result {
                first_error.get_or_insert(error);
                // A concurrent successful userdata close consumes cleanup and makes reinsertion stale.
                // 并发成功的 userdata close 会消费 cleanup，此时重新插入记录已经过时。
                if record
                    .cleanup
                    .as_ref()
                    .is_some_and(|cleanup| cleanup.is_pending())
                {
                    retry_records.push((session_id, record));
                }
                continue;
            }
            if let Some(cleanup) = record.cleanup {
                cleanup.run_once();
            }
        }
        if !retry_records.is_empty() {
            // Failed records republished only after all blocking teardown attempts have released their locks.
            // 仅在全部阻塞清理尝试释放锁之后重新发布失败记录。
            let retry_session_ids = retry_records
                .iter()
                .map(|(session_id, _record)| *session_id)
                .collect::<Vec<_>>();
            let mut state = self.lock_state();
            for (session_id, record) in retry_records {
                state.sessions.entry(session_id).or_insert(record);
            }
            drop(state);
            for session_id in retry_session_ids {
                self.retirement_retry_center.enqueue_session(session_id);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Tear down final service records with bounded retries and fail-safe resource retention.
    /// 通过有界重试清理服务最终记录，并在失败时采用资源保留的安全策略。
    ///
    /// `records` owns every process still registered when the final service owner is dropped.
    /// `records` 拥有最终服务所有者释放时仍已注册的全部进程。
    ///
    /// This function returns no value because it runs from `Drop`; cleanup callbacks execute only
    /// after process-tree termination succeeds, so a persistent OS failure never deletes live inputs.
    /// 此函数因从 `Drop` 运行而不返回值；清理回调只在进程树终止成功后执行，因而持续的操作系统
    /// 失败绝不会删除活动进程仍在使用的输入。
    fn teardown_records_for_shutdown(&self, mut records: Vec<(u64, ManagedRuntimeSessionRecord)>) {
        // Three attempts cover transient reader/process races without making engine destruction unbounded.
        // 三次尝试覆盖瞬态读取器与进程竞态，同时避免引擎销毁无界等待。
        const SHUTDOWN_TEARDOWN_ATTEMPTS: usize = 3;
        for attempt in 1..=SHUTDOWN_TEARDOWN_ATTEMPTS {
            let mut retry_records = Vec::new();
            for (session_id, mut record) in records {
                if let Some(token) = record.event_token.take() {
                    self.event_center.unregister_session(&token);
                }
                let result = match record.process.as_ref() {
                    Some(process) => process.kill().map(|_| ()),
                    None => Ok(()),
                };
                match result {
                    Ok(()) => {
                        if let Some(cleanup) = record.cleanup.take() {
                            cleanup.run_once();
                        }
                    }
                    Err(error) => {
                        if attempt == SHUTDOWN_TEARDOWN_ATTEMPTS {
                            log_warn(format!(
                                "[LuaSkill:warn] managed runtime session {session_id} shutdown teardown failed after {SHUTDOWN_TEARDOWN_ATTEMPTS} attempts; package resources were retained: {error}"
                            ));
                        } else {
                            retry_records.push((session_id, record));
                        }
                    }
                }
            }
            if retry_records.is_empty() {
                return;
            }
            records = retry_records;
            std::thread::yield_now();
        }
    }

    /// Acquire mutable service state while recovering after lock poisoning.
    /// 获取可变服务状态，并在锁中毒后恢复。
    fn lock_state(&self) -> MutexGuard<'_, ManagedRuntimeServicesState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Consume one deterministic test-only teardown failure when configured.
    /// 在已配置时消费一次仅测试使用的确定性清理失败。
    #[cfg(test)]
    fn consume_forced_teardown_failure(&self) -> bool {
        self.forced_teardown_failures
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }
}

impl ManagedRuntimeRetirementRetryCenter {
    /// Create one open empty deduplicated owner retry queue.
    /// 创建一个开放且为空的去重所有者重试队列。
    fn new() -> Self {
        Self {
            state: Mutex::new(ManagedRuntimeRetirementRetryState {
                targets: VecDeque::new(),
                queued_targets: HashSet::new(),
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    /// Enqueue one owner token exactly once and wake the retry worker.
    /// 仅一次入队一个所有者令牌并唤醒重试工作线程。
    fn enqueue(&self, owner_token: u64) {
        self.enqueue_target(ManagedRuntimeRetryTarget::Owner(owner_token));
    }

    /// Enqueue one exact session retry without broadening cleanup to its whole owner.
    /// 入队一个精确会话重试，不把清理范围扩大到其整个所有者。
    fn enqueue_session(&self, session_id: u64) {
        self.enqueue_target(ManagedRuntimeRetryTarget::Session(session_id));
    }

    /// Enqueue one deduplicated target at the FIFO tail and wake the worker.
    /// 在 FIFO 队尾入队一个去重目标并唤醒工作线程。
    fn enqueue_target(&self, target: ManagedRuntimeRetryTarget) {
        let mut state = self.lock_state();
        if state.closed {
            return;
        }
        if state.queued_targets.insert(target) {
            state.targets.push_back(target);
            self.changed.notify_one();
        }
    }

    /// Wait for and remove one owner token, or return `None` after closure.
    /// 等待并移除一个所有者令牌，或在关闭后返回 `None`。
    fn wait_next(&self) -> Option<ManagedRuntimeRetryTarget> {
        let mut state = self.lock_state();
        loop {
            if let Some(target) = state.targets.pop_front() {
                state.queued_targets.remove(&target);
                return Some(target);
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

    /// Close the retry queue, discard pending tokens, and wake its worker.
    /// 关闭重试队列、丢弃待处理令牌并唤醒其工作线程。
    fn close(&self) {
        let mut state = self.lock_state();
        state.closed = true;
        state.targets.clear();
        state.queued_targets.clear();
        self.changed.notify_all();
    }

    /// Acquire retry state while recovering after lock poisoning.
    /// 获取重试状态，并在锁中毒后恢复。
    fn lock_state(&self) -> MutexGuard<'_, ManagedRuntimeRetirementRetryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Run the single engine-local owner-retirement retry worker until service closure.
/// 运行单个引擎内部所有者退役重试工作线程，直至服务关闭。
fn run_managed_runtime_retirement_retry_worker(
    services: Weak<ManagedRuntimeServices>,
    retry_center: Arc<ManagedRuntimeRetirementRetryCenter>,
) {
    while let Some(target) = retry_center.wait_next() {
        let Some(services) = services.upgrade() else {
            return;
        };
        let result = match target {
            ManagedRuntimeRetryTarget::Owner(owner_token) => {
                services.retry_retired_owner(owner_token)
            }
            ManagedRuntimeRetryTarget::Session(session_id) => services.retry_session(session_id),
        };
        // Release the transient strong service owner before sleeping or requeueing. This lets the
        // engine drop on any thread and prevents the retry worker from owning its own join target.
        // 在休眠或重新入队前释放临时强服务所有权。这使引擎可在任意线程析构，并防止重试工作
        // 线程持有自身等待目标。
        drop(services);
        if result.is_err() {
            std::thread::sleep(MANAGED_RUNTIME_RETIREMENT_RETRY_DELAY);
            retry_center.enqueue_target(target);
        }
    }
}

impl ManagedRuntimeTransactionGuard {
    /// Return the lightweight transaction context installed into Lua app data.
    /// 返回安装到 Lua 应用数据中的轻量事务上下文。
    pub(crate) fn context(&self) -> ManagedRuntimeTransactionContext {
        self.context
    }

    /// Commit this evaluation transaction exactly once.
    /// 仅一次提交当前执行事务。
    pub(crate) fn commit(mut self) -> Result<(), String> {
        self.services.commit_transaction(self.context)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for ManagedRuntimeTransactionGuard {
    /// Roll back newly created sessions when evaluation exits without a successful commit.
    /// 当执行未成功提交便退出时回滚新建会话。
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Err(error) = self.services.rollback_transaction(self.context) {
            log_warn(format!(
                "[LuaSkill:warn] managed runtime transaction rollback failed: {error}"
            ));
        }
        self.finished = true;
    }
}

impl ManagedRuntimeSessionReservation {
    /// Return the optional background observer installed before process launch.
    /// 返回进程启动前安装的可选后台观察器。
    pub(crate) fn observer(&self) -> Option<Arc<dyn ManagedProcessSessionObserver>> {
        self.observer.clone()
    }

    /// Activate the reservation with one successfully launched process core.
    /// 使用一个成功启动的进程核心激活预留。
    pub(crate) fn activate(
        mut self,
        core: &ManagedProcessSessionCore,
        next_cleanup: Option<ManagedProcessSessionCleanup>,
    ) -> Result<(u64, Arc<ManagedProcessSessionCleanupHandle>), String> {
        let services = self
            .services
            .upgrade()
            .ok_or_else(|| "managed runtime services are no longer available".to_string())?;
        // Shared handle is published atomically with the process so rollback can clean every resource.
        // 共享句柄与进程一起原子发布，使回滚能够清理全部资源。
        let cleanup = services.session_cleanup(self.session_id, next_cleanup);
        let final_cleanup = Arc::clone(&cleanup);
        core.install_final_reaper_keepalive(Box::new(move || final_cleanup.run_once()))?;
        services.activate_session(self.session_id, core.clone(), Arc::clone(&cleanup))?;
        self.activated = true;
        Ok((self.session_id, cleanup))
    }
}

impl Drop for ManagedRuntimeSessionReservation {
    /// Cancel an unpublished reservation when launch or activation fails.
    /// 当启动或激活失败时取消未发布的预留。
    fn drop(&mut self) {
        if self.activated {
            return;
        }
        if let Some(services) = self.services.upgrade() {
            services.remove_reservation_record(self.session_id);
        }
    }
}

impl ManagedProcessSessionObserver for ManagedRuntimeSessionEventObserver {
    /// Publish one coalescible stdout-readable event.
    /// 发布一个可合并的 stdout 可读事件。
    fn stdout_readable(&self) {
        self.publish(RuntimeManagedSessionEventKind::StdoutReadable);
    }

    /// Publish one coalescible stderr-readable event.
    /// 发布一个可合并的 stderr 可读事件。
    fn stderr_readable(&self) {
        self.publish(RuntimeManagedSessionEventKind::StderrReadable);
    }

    /// Publish one reliable exited terminal event.
    /// 发布一个可靠的 exited 终态事件。
    fn exited(&self) {
        self.publish(RuntimeManagedSessionEventKind::Exited);
    }

    /// Publish one reliable failed terminal event.
    /// 发布一个可靠的 failed 终态事件。
    fn failed(&self) {
        self.publish(RuntimeManagedSessionEventKind::Failed);
    }
}

impl ManagedRuntimeSessionEventObserver {
    /// Publish one event if the center and registration remain active.
    /// 在事件中心与注册仍然活动时发布单个事件。
    fn publish(&self, kind: RuntimeManagedSessionEventKind) {
        if let Some(event_center) = self.event_center.upgrade() {
            let _ = event_center.publish(&self.token, kind);
        }
    }
}

impl Drop for ManagedRuntimeServices {
    /// Terminate all remaining processes and close the event center during engine teardown.
    /// 在引擎清理期间终止全部剩余进程并关闭事件中心。
    fn drop(&mut self) {
        self.retirement_retry_center.close();
        // The final Arc may be released by the retry worker after a transient upgrade. Never join
        // the current thread; dropping its handle detaches it after the queue has already closed.
        // 最后一个 Arc 可能由重试工作线程在临时升级后释放。绝不等待当前线程；队列已关闭后
        // 丢弃其句柄即可分离该线程。
        if let Some(worker) = self
            .retirement_retry_worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
        // All records drained while no other strong service owner can remain.
        // 在不可能残留其他强服务所有者时排空的全部记录。
        let records = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .drain()
            .collect::<Vec<_>>();
        self.teardown_records_for_shutdown(records);
        let _ = self.event_center.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Verify the host-selected per-engine persistent-session limit applies before process launch.
    /// 验证宿主选择的单引擎持久会话上限会在进程启动前生效。
    #[test]
    fn configured_session_limit_bounds_prelaunch_reservations() {
        // Config limits the engine to one launching or live persistent session.
        // Config 将引擎限制为一个启动中或活动持久会话。
        let config = LuaRuntimeManagedRuntimeConfig {
            persistent_session_limit_per_engine: 1,
            ..LuaRuntimeManagedRuntimeConfig::default()
        };
        // Services is the production registry and event-center path configured by the host.
        // Services 是由宿主配置的生产注册表与事件中心路径。
        let services = ManagedRuntimeServices::new_with_config(config)
            .expect("create single-session managed services");
        // FirstReservation consumes the sole capacity slot without spawning a child.
        // FirstReservation 在不启动子进程的情况下占用唯一容量槽。
        let first_reservation = services
            .reserve_session(71, None, None)
            .expect("reserve first managed session");
        // Error proves a second prelaunch reservation cannot exceed the configured engine limit.
        // Error 证明第二个启动前预留不能超过已配置引擎上限。
        let error = services
            .reserve_session(71, None, None)
            .err()
            .expect("second managed session must exceed configured limit");

        assert_eq!(error, "managed runtime session limit exceeded: 1");
        drop(first_reservation);
    }

    /// A failed first owner teardown must be retried without another lease retirement notification.
    /// 所有者首次清理失败后必须在没有另一条租约退役通知的情况下自动重试。
    #[test]
    fn owner_retirement_failure_is_retried_until_record_cleanup() {
        let services = ManagedRuntimeServices::new().expect("create retrying managed services");
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_probe = Arc::clone(&cleanup_count);
        let cleanup = ManagedProcessSessionCleanupHandle::new(Box::new(move || {
            cleanup_probe.fetch_add(1, Ordering::AcqRel);
        }));
        let owner_token = 41;
        {
            let mut state = services.lock_state();
            state.next_session_id = 1;
            state.sessions.insert(
                1,
                ManagedRuntimeSessionRecord {
                    owner_token,
                    process: None,
                    cleanup: Some(cleanup),
                    event_token: None,
                },
            );
        }
        services
            .forced_teardown_failures
            .store(1, Ordering::Release);

        let first_error = services
            .retire_owner(owner_token)
            .expect_err("forced first owner teardown should fail");
        assert_eq!(first_error, "forced managed runtime teardown failure");

        let deadline = Instant::now() + Duration::from_secs(2);
        while cleanup_count.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(cleanup_count.load(Ordering::Acquire), 1);
        assert!(
            services
                .lock_state()
                .sessions
                .values()
                .all(|record| record.owner_token != owner_token),
            "background retirement retry must remove the failed owner record"
        );
    }

    /// A failed transaction rollback must retry only its exact session until cleanup succeeds.
    /// 事务回滚失败后必须仅重试其精确会话，直到清理成功。
    #[test]
    fn transaction_rollback_failure_retries_exact_session() {
        let services = ManagedRuntimeServices::new().expect("create retrying managed services");
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_probe = Arc::clone(&cleanup_count);
        let cleanup = ManagedProcessSessionCleanupHandle::new(Box::new(move || {
            cleanup_probe.fetch_add(1, Ordering::AcqRel);
        }));
        let context = ManagedRuntimeTransactionContext {
            transaction_id: 7,
            owner_token: 52,
        };
        {
            let mut state = services.lock_state();
            state.sessions.insert(
                9,
                ManagedRuntimeSessionRecord {
                    owner_token: context.owner_token,
                    process: None,
                    cleanup: Some(cleanup),
                    event_token: None,
                },
            );
            state.transactions.insert(
                context.transaction_id,
                ManagedRuntimeTransactionRecord {
                    owner_token: context.owner_token,
                    session_ids: vec![9],
                },
            );
        }
        services
            .forced_teardown_failures
            .store(1, Ordering::Release);

        let error = services
            .rollback_transaction(context)
            .expect_err("forced first rollback should fail");
        assert_eq!(error, "forced managed runtime teardown failure");

        let deadline = Instant::now() + Duration::from_secs(2);
        while cleanup_count.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(cleanup_count.load(Ordering::Acquire), 1);
        assert!(!services.lock_state().sessions.contains_key(&9));
    }

    /// A userdata teardown-failure request must automatically drain its exact service record.
    /// userdata 清理失败请求必须自动排空其精确服务记录。
    #[test]
    fn userdata_teardown_failure_request_retries_exact_session() {
        // Services owns the durable retry worker and the synthetic committed session record.
        // Services 拥有持久重试 Worker 与合成的已提交会话记录。
        let services = ManagedRuntimeServices::new().expect("create userdata retry services");
        // CleanupCount records completion after the retry worker consumes the exact record.
        // CleanupCount 记录重试 Worker 消费精确记录后的完成次数。
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_probe = Arc::clone(&cleanup_count);
        let session_id = 17;
        let cleanup = services.session_cleanup(
            session_id,
            Some(Box::new(move || {
                cleanup_probe.fetch_add(1, Ordering::AcqRel);
            })),
        );
        {
            // Registry record matches one activated session whose process already reached terminal state.
            // 注册表记录模拟一个进程已进入终态的已激活会话。
            let mut state = services.lock_state();
            state.sessions.insert(
                session_id,
                ManagedRuntimeSessionRecord {
                    owner_token: 61,
                    process: None,
                    cleanup: Some(Arc::clone(&cleanup)),
                    event_token: None,
                },
            );
        }

        cleanup.request_teardown_retry();

        // Deadline bounds the assertion while allowing the engine-local retry worker to run.
        // Deadline 限制断言等待时间，同时允许引擎内部重试 Worker 运行。
        let deadline = Instant::now() + Duration::from_secs(2);
        while cleanup_count.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(cleanup_count.load(Ordering::Acquire), 1);
        assert!(!services.lock_state().sessions.contains_key(&session_id));
    }

    /// Retry targets must preserve FIFO order while deduplicating repeated enqueue attempts.
    /// 重试目标必须在去重重复入队的同时保持 FIFO 顺序。
    #[test]
    fn retry_center_is_deduplicated_fifo() {
        let center = ManagedRuntimeRetirementRetryCenter::new();
        center.enqueue_target(ManagedRuntimeRetryTarget::Owner(3));
        center.enqueue_target(ManagedRuntimeRetryTarget::Session(8));
        center.enqueue_target(ManagedRuntimeRetryTarget::Owner(3));
        assert!(matches!(
            center.wait_next(),
            Some(ManagedRuntimeRetryTarget::Owner(3))
        ));
        assert!(matches!(
            center.wait_next(),
            Some(ManagedRuntimeRetryTarget::Session(8))
        ));
    }
}
