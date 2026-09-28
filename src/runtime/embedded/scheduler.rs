use super::capabilities::{CapabilityPermissions, CapabilityRegistry, ModuleCapabilities};
use super::retirement::MAINTENANCE_INTERVAL;
use super::value_size::json_size;
use super::*;
use crate::LuaInvocationContext;
use crate::runtime::engine::LuaEngine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

mod finalization;
mod persistence;
mod plugins;
mod sessions;
mod workers;
use finalization::PendingFinalization;

pub use persistence::{CheckpointRetryState, OperationPersistenceFailure};
pub use plugins::EmbeddedPluginSnapshot;
use plugins::ScheduledPlugin;
pub use sessions::{EmbeddedSessionOpening, EmbeddedSessionPhase, EmbeddedSessionSnapshot};
use sessions::{ScheduledRequest, ScheduledSession};

/// Owned structured request admitted under one original deadline.
/// 在单个原始截止时间下接纳的拥有所有权的结构化请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedCall {
    /// Exact immutable pool identity returned by this runtime.
    /// 此运行时返回的精确不可变池身份。
    pub pool_id: String,
    /// Exact declared module export, never an evaluated code fragment.
    /// 精确声明的模块导出，绝不是求值代码片段。
    pub export: String,
    /// Application value whose encoded size is checked before admission.
    /// 入场前检查编码大小的应用值。
    pub arguments: Value,
    /// Trusted host context retained and charged with the queued request.
    /// 与排队请求一并保留和计费的可信宿主上下文。
    pub context: LuaInvocationContext,
}

/// Live scheduler observations; queue bytes exclude already-dispatched request values.
/// 实时调度观测；队列字节不包含已分发的请求值。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedRuntimeUsage {
    /// Requests waiting for actual resource admission.
    /// 等待实际资源入场的请求。
    pub queued_calls: usize,
    /// Exact serialized queued request bytes.
    /// 排队请求的精确序列化字节数。
    pub queued_bytes: usize,
    /// Nonqueued unfinished operations, including rejected requests awaiting terminal publication.
    /// 不在队列中的未完成操作，包含等待终态发布的已拒绝请求。
    pub active_operations: usize,
    /// Operations whose execution returned and whose cleanup remains owned.
    /// 执行已返回但仍拥有清理的操作。
    pub cleaning_operations: usize,
    /// Whether new admission has permanently closed.
    /// 新入场是否已永久关闭。
    pub closing: bool,
}

/// Registered immutable execution domain retained while any operation can still refer to it.
/// 只要任何操作仍可能引用，就保留的已注册不可变执行域。
struct ScheduledPool {
    /// Input contracts reject invalid requests before module initialization can have effects.
    /// 输入契约在模块初始化可能产生副作用前拒绝无效请求。
    inputs: BTreeMap<String, JsonContract>,
    /// Actual governed VM ownership.
    /// 实际受治理的 VM 所有权。
    pool: Arc<ModulePool>,
    /// Trusted plugin key used for fair round-robin admission.
    /// 公平轮转入场使用的可信插件键。
    plugin_id: String,
    /// Accepted queued requests for this exact domain.
    /// 此精确域已接纳的排队请求。
    queued: usize,
    /// Dispatched operations; serial domains retain order through actual cleanup.
    /// 已分发操作；串行域保持顺序直到真实清理结束。
    active: usize,
    /// Generation closure rejects future calls without retargeting queued work.
    /// 代次关闭拒绝未来调用，且不把排队任务改投其他目标。
    closed: bool,
}

/// Unique operation ownership moves from the queue to one worker and then the cleanup supervisor.
/// 唯一操作所有权从队列转移到一个工作线程，再转移至清理监督器。
struct ScheduledCall {
    /// Stable original operation identity.
    /// 稳定的原始操作身份。
    id: String,
    /// Sole authority allowed to advance and complete execution.
    /// 唯一允许推进和完成执行的权威。
    owner: OperationOwner,
    /// Original cancellation and time budget.
    /// 原始取消控制与时间预算。
    control: Arc<CallControl>,
    /// Owned request remains charged until actual dispatch.
    /// 拥有所有权的请求在实际分发前持续计费。
    request: ScheduledRequest,
    /// Exact admitted wire size from the authoritative serializer.
    /// 来自权威序列化器的精确入场线协议大小。
    bytes: usize,
}

/// Retained completion cannot become terminal before VM retirement and host evidence sealing.
/// 保留的完成记录在 VM 退役与宿主证据封存前不能成为终态。
struct PendingCompletion {
    /// Same-VM closing work retained through its intent, actual execution and durable outcome.
    /// 跨意图、真实执行及持久结果保留的同 VM 关闭工作。
    finalization: Option<PendingFinalization>,
    /// Successful session ownership remains exclusive until operation evidence is sealed.
    /// 成功会话的所有权保持独占，直到操作证据封存。
    session_lease: Option<Box<ModuleLease>>,
    /// Whether the sole owner has already entered cleaning, independent of execution admission.
    /// 唯一所有者是否已进入清理，独立于执行入场。
    cleaning_started: bool,
    /// Unique execution authority and stable request identity.
    /// 唯一执行权威与稳定请求身份。
    call: ScheduledCall,
    /// Actual invocation outcome retained without replaying execution.
    /// 保留的实际调用结果，不重放执行。
    result: EmbeddedResult<Value>,
    /// Exact retirement evidence; absent means no outstanding VM teardown.
    /// 精确退役证据；省略表示没有未完成 VM 清理。
    retirement: Option<ModuleRetirement>,
    /// Conservative outer evidence; host callback records retain stronger individual facts.
    /// 保守的外层证据；宿主回调记录保留更强的逐项事实。
    effects: EffectState,
    /// Whether this operation consumed a domain execution slot.
    /// 此操作是否消费了域执行槽。
    dispatched: bool,
}

/// Short-lock scheduling metadata contains no Lua execution or native destructor work.
/// 短时锁调度元数据不包含 Lua 执行或原生析构工作。
struct SchedulerState {
    /// Closing continuations use existing workers and retain the original active-operation charge.
    /// 关闭续行使用既有工作线程，并保留原活动操作记账。
    finalizing: VecDeque<PendingCompletion>,
    /// Last supervised shared-checkpoint observation pauses dispatch while an active callback awaits repair.
    /// 上次监督得到的共享检查点观测，在活动回调等待修复时暂停分发。
    shared_checkpoint_failed: bool,
    /// One observable fault per unfinished operation, retained through an explicitly requested retry.
    /// 每个未完成操作的一项可观测故障，跨显式请求的重试保留。
    persistence_failures: BTreeMap<String, OperationPersistenceFailure>,
    /// Cleanup count includes work temporarily detached by the supervisor.
    /// 清理计数包含被监督器临时摘除的任务。
    cleaning_count: usize,
    /// Permanent admission closure.
    /// 永久入场关闭。
    closing: bool,
    /// First infrastructure failure remains observable during shutdown.
    /// 首次基础设施失败在关闭期间保持可观察。
    failure: Option<EmbeddedError>,
    /// Never-reused pool identity counter within the random runtime namespace.
    /// 随机运行时命名空间内绝不复用的池身份计数器。
    sequence: u64,
    /// Exact domain registrations, never resolved by mutable plugin name at execution time.
    /// 精确域注册，执行时绝不通过可变插件名重新解析。
    pools: BTreeMap<String, ScheduledPool>,
    /// Immutable plugin-wide admission authority spans all of its pool generations.
    /// 不可变插件级入场权威覆盖其全部池代次。
    plugins: BTreeMap<String, ScheduledPlugin>,
    /// Retained operation ownership survives pool removal and is released only by explicit forgetting.
    /// 保留操作归属在池移除后仍存在，仅通过显式遗忘释放。
    operation_plugins: BTreeMap<String, String>,
    /// Bounded exact session identities; closed entries remain queryable until explicitly forgotten.
    /// 有界精确会话身份；关闭条目在显式遗忘前仍可查询。
    sessions: BTreeMap<String, ScheduledSession>,
    /// Per-plugin FIFO containers, scanned without bypassing a domain's earlier request.
    /// 逐插件先进先出容器，扫描时不越过同一域更早的请求。
    queues: BTreeMap<String, VecDeque<ScheduledCall>>,
    /// Plugin rotation, so one plugin cannot gain weight by registering more execution domains.
    /// 插件轮转，使单个插件不能通过注册更多执行域增加权重。
    rotation: VecDeque<String>,
    /// Queue count excludes requests after a successful preparation.
    /// 队列计数不包含成功准备之后的请求。
    queued: usize,
    /// Serialized bytes owned by currently queued requests.
    /// 当前排队请求拥有的序列化字节数。
    bytes: usize,
    /// All unfinished controls permit independent shutdown cancellation.
    /// 全部未完成控制允许独立关闭取消。
    live: BTreeMap<String, Arc<CallControl>>,
    /// Actual execution ended; final completion belongs to the supervisor.
    /// 实际执行已结束；最终完成归监督器所有。
    cleaning: Vec<PendingCompletion>,
}

/// Shared center is retained by workers without a cycle through public join handles.
/// 工作线程保留共享中心，但不经由公开线程句柄形成引用环。
struct SchedulerCenter {
    /// Fresh process-independent namespace created from operating-system randomness.
    /// 由操作系统随机源创建的全新、独立于进程身份的命名空间。
    id: String,
    /// Actual capacity governor, VM pools and retirement worker.
    /// 实际容量治理器、VM 池及退役工作线程。
    pools: Arc<EmbeddedPoolManager>,
    /// Instance-owned capabilities and SDK control queue.
    /// 实例拥有的能力及 SDK 控制队列。
    capabilities: Arc<CapabilityRegistry>,
    /// Bounded operation authority independent of client handles.
    /// 独立于客户端句柄的有界操作权威。
    operations: OperationRegistry,
    /// The sole scheduling metadata lock.
    /// 唯一调度元数据锁。
    state: Mutex<SchedulerState>,
    /// Submission, resource release, cancellation maintenance and shutdown wake workers.
    /// 提交、资源释放、取消维护与关闭唤醒工作线程。
    changed: Condvar,
}

impl SchedulerCenter {
    /// Lock authoritative metadata or return an explicit infrastructure failure.
    /// 锁定权威元数据，或返回显式基础设施错误。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, SchedulerState>> {
        self.state
            .lock()
            .map_err(|_| internal("embedded scheduler lock is poisoned"))
    }

    /// Preserve the first `error`, reject new work and cooperatively cancel all live execution.
    /// 保留首次 `error`，拒绝新任务并协作取消全部活跃执行。
    fn fail(&self, error: EmbeddedError) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failure.get_or_insert(error);
        state.closing = true;
        for control in state.live.values() {
            control.cancel();
        }
        self.changed.notify_all();
    }
}

/// Formal owned execution runtime with fixed workers and an independent cleanup/control supervisor.
/// 具有固定工作线程及独立清理与控制监督器的正式受管执行运行时。
pub struct EmbeddedRuntime {
    /// Workers own this center until actual execution and cleanup end.
    /// 工作线程保留此中心，直到实际执行与清理结束。
    center: Arc<SchedulerCenter>,
    /// Explicitly joined before close can succeed; dropping a handle is never proof of termination.
    /// 关闭成功前显式等待退出；丢弃句柄绝不证明终止。
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl EmbeddedRuntime {
    /// Build runtime phase persistence using exact host-owned `writer`; the host closes and joins that writer separately.
    /// 使用精确宿主自有 `writer` 构造运行时阶段持久化；宿主另行关闭并等待该写入者。
    /// Host dispatch and returned-effect evidence share this writer; cross-process reconciliation remains separate.
    /// 宿主分发及已返回副作用证据共享此写入者；跨进程对账仍是独立边界。
    pub fn with_journal_worker(
        engine: Arc<LuaEngine>,
        config: EmbeddedRuntimeConfig,
        writer: Arc<OperationJournalWorker>,
    ) -> EmbeddedResult<Self> {
        Self::build(engine, config, Some(writer))
    }

    /// Construct fixed workers for `engine` and validated `config`, using only the explicitly selected `writer`.
    /// 为 `engine` 及已校验 `config` 构造固定工作线程，仅使用显式选择的 `writer`。
    fn build(
        engine: Arc<LuaEngine>,
        config: EmbeddedRuntimeConfig,
        writer: Option<Arc<OperationJournalWorker>>,
    ) -> EmbeddedResult<Self> {
        config.validate()?;
        // Namespace creation fails explicitly; time and process IDs are not fallback identities.
        // 命名空间创建明确失败；时间和进程号不作为备用身份。
        let mut entropy = [0u8; 32];
        getrandom::fill(&mut entropy)
            .map_err(|_| internal("runtime identity randomness unavailable"))?;
        let id = format!(
            "embedded:{}",
            entropy
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        // Capabilities retain this exact fresh runtime namespace.
        // 能力保留此精确全新运行时命名空间。
        let capabilities = CapabilityRegistry::new(id.clone(), config.clone())?;
        // Backend selection is explicit; no probing or automatic persistence fallback occurs.
        // 后端选择明确；不探测，也不自动回退持久化。
        let operations = match writer {
            Some(writer) => OperationRegistry::with_journal_worker(id.clone(), &config, writer)?,
            None => OperationRegistry::new(id.clone(), &config)?,
        };
        // The center owns all scheduling metadata independently from thread join handles.
        // 中心独立于线程等待句柄拥有全部调度元数据。
        let center = Arc::new(SchedulerCenter {
            id,
            capabilities,
            operations,
            pools: EmbeddedPoolManager::new(engine, config.clone())?,
            state: Mutex::new(SchedulerState {
                finalizing: VecDeque::new(),
                closing: false,
                failure: None,
                sequence: 0,
                pools: BTreeMap::new(),
                plugins: BTreeMap::new(),
                operation_plugins: BTreeMap::new(),
                sessions: BTreeMap::new(),
                queues: BTreeMap::new(),
                rotation: VecDeque::new(),
                queued: 0,
                bytes: 0,
                live: BTreeMap::new(),
                cleaning: Vec::new(),
                cleaning_count: 0,
                persistence_failures: BTreeMap::new(),
                shared_checkpoint_failed: false,
            }),
            changed: Condvar::new(),
        });
        // Only the configured fixed workers and one supervisor can be created.
        // 仅允许创建已配置固定工作线程及一个监督器。
        let mut workers = Vec::new();
        // Spawn one independent supervisor and a fixed execution count; requests cannot add threads.
        // 启动一个独立监督器与固定数量执行线程；请求不能增加线程。
        for index in 0..=config.max_running_calls {
            // Real worker ownership outlives any observing runtime handle.
            // 真实工作线程所有权寿命超过任何观测运行时句柄。
            let worker_center = Arc::clone(&center);
            // Every spawned worker reports failure through the retained center.
            // 每个启动的工作线程都通过保留中心报告故障。
            let spawn = std::thread::Builder::new()
                .name(format!("luaskills-embedded-{index}"))
                .spawn(move || {
                    // Preserve infrastructure failure without unwinding through the host.
                    // 保留基础设施故障，不通过宿主展开栈。
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if index == 0 {
                            workers::supervise(&worker_center)
                        } else {
                            workers::execute(&worker_center)
                        }
                    }));
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => worker_center.fail(error),
                        Err(_) => worker_center.fail(internal("embedded worker panicked")),
                    }
                });
            match spawn {
                Ok(worker) => workers.push(worker),
                Err(_) => {
                    center.fail(internal("embedded worker creation failed"));
                    for worker in workers {
                        let _ = worker.join();
                    }
                    center.pools.request_close()?;
                    return Err(internal("embedded worker creation failed"));
                }
            }
        }
        Ok(Self {
            center,
            workers: Mutex::new(workers),
        })
    }

    /// Create independent execution around `engine` and explicit `config`, returning an owned runtime.
    /// 围绕 `engine` 与显式 `config` 创建独立执行，返回受管运行时。
    /// Failed worker construction closes already-started workers before returning the error.
    /// 工作线程构造失败时，在返回错误前关闭已经启动的工作线程。
    pub fn new(engine: Arc<LuaEngine>, config: EmbeddedRuntimeConfig) -> EmbeddedResult<Self> {
        Self::build(engine, config, None)
    }

    /// Return this runtime's immutable opaque namespace.
    /// 返回此运行时不可变的不透明命名空间。
    pub fn id(&self) -> &str {
        &self.center.id
    }

    /// Return instance-owned capability registration and independent SDK request control.
    /// 返回实例拥有的能力注册与独立 SDK 请求控制。
    pub fn capabilities(&self) -> Arc<CapabilityRegistry> {
        Arc::clone(&self.center.capabilities)
    }

    /// Register immutable `definition` and `policy` with current capabilities, `permissions` and `revision`.
    /// 使用当前能力、`permissions` 与 `revision` 注册不可变 `definition` 和 `policy`。
    /// Return an opaque pool identity; ordinary calls cannot silently become pinned sessions.
    /// 返回不透明池身份；普通调用不能静默变为固定会话。
    pub fn register_pool(
        &self,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        permissions: Arc<CapabilityPermissions>,
        revision: String,
    ) -> EmbeddedResult<String> {
        self.register_pool_internal(definition, policy, permissions, revision, None)
    }

    /// Registers the exact module/policy/permission revision while retaining native host `owner` resources.
    /// 注册精确模块、策略及权限修订，同时保留原生宿主 `owner` 资源。
    /// Returns the scheduled pool id; close releases ownership only after all real VMs and reservations drain.
    /// 返回调度池标识；关闭仅在全部真实 VM 及预留排空后释放所有权。
    pub fn register_pool_with_owner(
        &self,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        permissions: Arc<CapabilityPermissions>,
        revision: String,
        owner: ModuleResourceOwner,
    ) -> EmbeddedResult<String> {
        self.register_pool_internal(definition, policy, permissions, revision, Some(owner))
    }

    /// Publishes one validated pool with an optional Rust-only generation owner.
    /// 发布一个已校验池，并可携带仅供 Rust 使用的代次所有者。
    /// Inputs freeze definition, policy, permissions and revision; errors publish no scheduled pool.
    /// 输入冻结定义、策略、权限及修订；错误不会发布调度池。
    fn register_pool_internal(
        &self,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        permissions: Arc<CapabilityPermissions>,
        revision: String,
        owner: Option<ModuleResourceOwner>,
    ) -> EmbeddedResult<String> {
        policy.validate(self.center.pools.config())?;
        if let Some(finalizer) = &definition.finalizer {
            if policy.reuse != InstanceReuse::SingleCall {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Unsupported,
                    "automatic finalization currently requires single-call reuse",
                ));
            }
            json_size(finalizer, self.center.pools.config().max_value_bytes)?;
        }
        // Capability membership remains frozen independently from physical resource ownership.
        // 能力成员独立于物理资源所有权保持冻结。
        let binding =
            ModuleCapabilities::new(self.center.capabilities.snapshot()?, permissions, revision)?;
        // Compile admission contracts before any scheduled pool is published.
        // 发布任何调度池之前编译入场契约。
        let inputs = definition
            .exports
            .iter()
            .map(|export| {
                JsonContract::compile(&export.input_schema)
                    .map(|contract| (export.name.clone(), contract))
            })
            .collect::<EmbeddedResult<BTreeMap<_, _>>>()?;
        // The scheduler metadata gate makes registration atomic with shutdown.
        // 调度器元数据门使注册相对关闭保持原子性。
        let mut state = self.center.lock()?;
        if state.closing {
            return Err(closed());
        }
        state.validate_plugin_pool(&definition.plugin_id, &policy)?;
        if state.pools.len() >= self.center.pools.config().max_registered_pools {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "retained pool capacity reached",
            ));
        }
        // Identity allocation cannot wrap or reuse an earlier pool handle.
        // 身份分配不能回绕或复用先前池句柄。
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal("pool identity exhausted"))?;
        // Bind both scheduling and physical ownership to the same generated pool id.
        // 将调度及物理所有权绑定到同一生成池标识。
        let id = IdentityKind::Pool.render(self.id(), sequence);
        // Copy only identity metadata; the real resource owner moves into the registration.
        // 仅复制身份元数据；真实资源所有者移动到注册中。
        let plugin_id = definition.plugin_id.clone();
        // Every allocation and resident inherits the same owning registration.
        // 每个分配及常驻实例继承同一个拥有资源的注册。
        let pool = self.center.pools.create_pool_internal(
            id.clone(),
            definition,
            policy,
            Some(binding),
            owner,
        )?;
        state.sequence = sequence;
        state.pools.insert(
            id.clone(),
            ScheduledPool {
                inputs,
                pool,
                plugin_id,
                queued: 0,
                active: 0,
                closed: false,
            },
        );
        Ok(id)
    }

    /// Submit owned `request` under an original finite `timeout`, returning query/cancel authority.
    /// 在原始有限 `timeout` 下提交拥有所有权的 `request`，返回查询与取消权威。
    /// Queue count, bytes, exact pool validity and operation retention are admitted atomically.
    /// 队列数量、字节、精确池有效性与操作保留容量原子入场。
    pub fn submit(
        &self,
        request: EmbeddedCall,
        timeout: Duration,
    ) -> EmbeddedResult<OperationHandle> {
        self.center.capabilities.check_submission()?;
        let control = Arc::new(CallControl::new(timeout)?);
        let config = self.center.pools.config();
        json_size(&request.arguments, config.max_value_bytes)?;
        let bytes = json_size(&request, config.max_queued_bytes)?;
        let mut state = self.center.lock()?;
        let pool = state.pools.get(&request.pool_id).ok_or_else(not_found)?;
        if pool.pool.policy().reuse == InstanceReuse::Session {
            return Err(EmbeddedError::invalid(
                "session pools require an explicit session",
            ));
        }
        self.center.enqueue(
            &mut state,
            ScheduledRequest::Invoke(request),
            control,
            bytes,
        )
    }

    /// Query exact `id`; forgotten identities never imply execution did not happen.
    /// 查询精确 `id`；已遗忘身份绝不表示执行没有发生。
    pub fn operation(&self, id: &str) -> EmbeddedResult<OperationHandle> {
        self.center.operations.get(id)
    }

    /// Explicitly forget terminal `id`, leaving active execution evidence intact.
    /// 显式遗忘终态 `id`，保留活跃执行证据。
    pub fn forget_operation(&self, id: &str) -> EmbeddedResult<()> {
        let mut state = self.center.lock()?;
        let plugin_id = state
            .operation_plugins
            .get(id)
            .ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::NotFound,
                    "embedded operation is not retained",
                )
            })?
            .clone();
        self.center.operations.forget(id)?;
        state.operation_plugins.remove(id);
        state
            .plugins
            .get_mut(&plugin_id)
            .expect("retained operation owns a registered plugin")
            .operations -= 1;
        Ok(())
    }

    /// Close exact `id` admission; queued work fails on this generation instead of retargeting.
    /// 关闭精确 `id` 入场；排队任务在此代次失败，而不改投其他目标。
    pub fn close_pool(&self, id: &str) -> EmbeddedResult<()> {
        let pool = {
            let mut state = self.center.lock()?;
            let pool = state.pools.get_mut(id).ok_or_else(not_found)?;
            pool.closed = true;
            Arc::clone(&pool.pool)
        };
        pool.close()?;
        self.center.changed.notify_all();
        Ok(())
    }

    /// Forget closed exact `id` only after all queued, executing and resident ownership drains.
    /// 仅在全部排队、执行及常驻所有权排空后遗忘已关闭精确 `id`。
    /// Old identities remain unknown and cannot resolve to a newly registered generation.
    /// 旧身份保持未知，不能解析到新注册代次。
    pub fn forget_pool(&self, id: &str) -> EmbeddedResult<()> {
        let mut state = self.center.lock()?;
        let pool = state.pools.get(id).ok_or_else(not_found)?;
        if !pool.closed
            || pool.queued != 0
            || pool.active != 0
            || pool.pool.usage()?.resident != 0
            || state.sessions.values().any(|session| session.pool_id == id)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "pool ownership has not drained",
            ));
        }
        let removed = state.pools.remove(id);
        drop(state);
        drop(removed);
        Ok(())
    }

    /// Read current queue and active-operation accounting without invoking plugin code.
    /// 读取当前队列与活跃操作记账，不调用插件代码。
    pub fn usage(&self) -> EmbeddedResult<EmbeddedRuntimeUsage> {
        let state = self.center.lock()?;
        Ok(EmbeddedRuntimeUsage {
            queued_calls: state.queued,
            queued_bytes: state.bytes,
            active_operations: state.live.len() - state.queued,
            cleaning_operations: state.cleaning_count,
            closing: state.closing,
        })
    }

    /// Query actual parent VM accounting, including construction and pending retirement.
    /// 查询实际父级 VM 记账，包含构造中及待完成退役。
    pub fn resources(&self) -> EmbeddedResult<PoolUsage> {
        self.center.pools.usage()
    }

    /// Query exact retained `id` resource ownership without resolving mutable plugin names.
    /// 查询精确保留 `id` 的资源所有权，不解析可变插件名。
    pub fn pool_resources(&self, id: &str) -> EmbeddedResult<PoolUsage> {
        let pool = Arc::clone(
            &self
                .center
                .lock()?
                .pools
                .get(id)
                .ok_or_else(not_found)?
                .pool,
        );
        pool.usage()
    }

    /// Revoke exact `permission` for `pool_id` using its existing live authority; return whether a grant changed.
    /// 使用既有实时权威为 `pool_id` 撤销精确 `permission`；返回授权是否发生变化。
    pub fn revoke_pool_permission(&self, pool_id: &str, permission: &str) -> EmbeddedResult<bool> {
        let pool = self
            .center
            .lock()?
            .pools
            .get(pool_id)
            .ok_or_else(not_found)?
            .pool
            .clone();
        pool.revoke_capability_permission(permission)
    }

    /// Reject new work and request cancellation without blocking on a VM or language callback.
    /// 拒绝新任务并请求取消，不阻塞等待 VM 或语言回调。
    pub fn request_close(&self) -> EmbeddedResult<()> {
        {
            let mut state = self.center.lock()?;
            state.closing = true;
            for control in state.live.values() {
                control.cancel();
            }
        }
        self.center.capabilities.begin_shutdown()?;
        self.center.changed.notify_all();
        Ok(())
    }

    /// Return true only after execution, supervision and actual VM retirement threads are joined.
    /// 仅在执行、监督及实际 VM 退役线程均已等待退出后返回 true。
    /// A caller must retain the runtime and its dynamic library while this returns false or fails.
    /// 此方法返回 false 或失败时，调用方必须保留运行时及其动态库。
    pub fn poll_closed(&self) -> EmbeddedResult<bool> {
        {
            let state = self.center.lock()?;
            if let Some(error) = &state.failure {
                return Err(error.clone());
            }
            if !state.closing || !state.live.is_empty() {
                return Ok(false);
            }
        }
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| internal("embedded worker handles are poisoned"))?;
        if workers.iter().any(|worker| !worker.is_finished()) {
            return Ok(false);
        }
        for worker in workers.drain(..) {
            if worker.join().is_err() {
                let error = internal("embedded worker panicked");
                self.center.fail(error.clone());
                return Err(error);
            }
        }
        self.center.pools.poll_closed()
    }
}

impl Drop for EmbeddedRuntime {
    /// Request cooperative shutdown; unfinished workers keep their center and executable resources alive.
    /// 请求协作关闭；未完成工作线程保留其中心与可执行资源。
    fn drop(&mut self) {
        let _ = self.request_close();
    }
}

/// Return the fixed closed-runtime diagnostic.
/// 返回固定的运行时已关闭诊断。
fn closed() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Closed,
        "embedded runtime or pool is closed",
    )
}
/// Return the fixed unknown-domain diagnostic without probing an alternate identity.
/// 返回固定的未知域诊断，不探测替代身份。
fn not_found() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::NotFound,
        "embedded pool identity is unknown",
    )
}
/// Classify an exact invariant failure `message` as an internal error.
/// 将精确不变量失败 `message` 分类为内部错误。
fn internal(message: &str) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::Internal, message)
}
