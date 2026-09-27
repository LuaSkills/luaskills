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

mod workers;

/// Owned structured request admitted under one original deadline.
/// 在单个原始截止时间下接纳的拥有所有权的结构化请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    request: EmbeddedCall,
    /// Exact admitted wire size from the authoritative serializer.
    /// 来自权威序列化器的精确入场线协议大小。
    bytes: usize,
}

/// Retained completion cannot become terminal before VM retirement and host evidence sealing.
/// 保留的完成记录在 VM 退役与宿主证据封存前不能成为终态。
struct PendingCompletion {
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
    /// Create independent execution around `engine` and explicit `config`, returning an owned runtime.
    /// 围绕 `engine` 与显式 `config` 创建独立执行，返回受管运行时。
    /// Failed worker construction closes already-started workers before returning the error.
    /// 工作线程构造失败时，在返回错误前关闭已经启动的工作线程。
    pub fn new(engine: Arc<LuaEngine>, config: EmbeddedRuntimeConfig) -> EmbeddedResult<Self> {
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
        let capabilities = CapabilityRegistry::new(id.clone(), config.clone())?;
        let operations = OperationRegistry::new(id.clone(), &config)?;
        let center = Arc::new(SchedulerCenter {
            id,
            capabilities,
            operations,
            pools: EmbeddedPoolManager::new(engine, config.clone())?,
            state: Mutex::new(SchedulerState {
                closing: false,
                failure: None,
                sequence: 0,
                pools: BTreeMap::new(),
                queues: BTreeMap::new(),
                rotation: VecDeque::new(),
                queued: 0,
                bytes: 0,
                live: BTreeMap::new(),
                cleaning: Vec::new(),
                cleaning_count: 0,
            }),
            changed: Condvar::new(),
        });
        let mut workers = Vec::new();
        // Spawn one independent supervisor and a fixed execution count; requests cannot add threads.
        // 启动一个独立监督器与固定数量执行线程；请求不能增加线程。
        for index in 0..=config.max_running_calls {
            let worker_center = Arc::clone(&center);
            let spawn = std::thread::Builder::new()
                .name(format!("luaskills-embedded-{index}"))
                .spawn(move || {
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
        if policy.reuse == InstanceReuse::Session {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "scheduled session ownership is not yet available",
            ));
        }
        let binding =
            ModuleCapabilities::new(self.center.capabilities.snapshot()?, permissions, revision)?;
        let inputs = definition
            .exports
            .iter()
            .map(|export| {
                JsonContract::compile(&export.input_schema)
                    .map(|contract| (export.name.clone(), contract))
            })
            .collect::<EmbeddedResult<BTreeMap<_, _>>>()?;
        let mut state = self.center.lock()?;
        if state.closing {
            return Err(closed());
        }
        if state.pools.len() >= self.center.pools.config().max_registered_pools {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "retained pool capacity reached",
            ));
        }
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal("pool identity exhausted"))?;
        let id = format!("{}:pool:{sequence}", self.id());
        let plugin_id = definition.plugin_id.clone();
        let pool = self.center.pools.create_pool_with_capabilities(
            id.clone(),
            definition,
            policy,
            binding,
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
        if state.closing {
            return Err(closed());
        }
        let pool = state.pools.get(&request.pool_id).ok_or_else(not_found)?;
        if pool.closed {
            return Err(closed());
        }
        pool.inputs
            .get(&request.export)
            .ok_or_else(|| EmbeddedError::invalid("module export is not declared"))?
            .validate(&request.arguments)?;
        if pool.queued >= pool.pool.policy().max_queued_calls
            || state.queued >= config.max_queued_calls
            || bytes > config.max_queued_bytes.saturating_sub(state.bytes)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "embedded request queue capacity reached",
            ));
        }
        let plugin = pool.plugin_id.clone();
        let (handle, owner) = self.center.operations.admit(Arc::clone(&control))?;
        let id = handle.snapshot()?.operation_id;
        state
            .pools
            .get_mut(&request.pool_id)
            .expect("validated pool exists")
            .queued += 1;
        state.queued += 1;
        state.bytes += bytes;
        state.live.insert(id.clone(), Arc::clone(&control));
        if !state.queues.contains_key(&plugin) {
            state.rotation.push_back(plugin.clone());
        }
        state
            .queues
            .entry(plugin)
            .or_default()
            .push_back(ScheduledCall {
                id,
                owner,
                control,
                request,
                bytes,
            });
        self.center.changed.notify_all();
        Ok(handle)
    }

    /// Query exact `id`; forgotten identities never imply execution did not happen.
    /// 查询精确 `id`；已遗忘身份绝不表示执行没有发生。
    pub fn operation(&self, id: &str) -> EmbeddedResult<OperationHandle> {
        self.center.operations.get(id)
    }

    /// Explicitly forget terminal `id`, leaving active execution evidence intact.
    /// 显式遗忘终态 `id`，保留活跃执行证据。
    pub fn forget_operation(&self, id: &str) -> EmbeddedResult<()> {
        self.center.operations.forget(id)
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
        if !pool.closed || pool.queued != 0 || pool.active != 0 || pool.pool.usage()?.resident != 0
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
