use super::capabilities::ModuleCapabilities;
use super::retirement::RetirementService;
use super::{
    CallControl, EmbeddedError, EmbeddedErrorCode, EmbeddedModule, EmbeddedResult,
    EmbeddedRuntimeConfig, InstanceReuse, ModuleAcquireFailure, ModuleDefinition, ModuleFinalizer,
    ModuleInvocation, ModuleOperationContext, ModuleRelease, ModuleResourceOwner, ModuleRetirement,
    OperationContext, PluginPoolConfig, PoolGovernor, PoolUsage, VmReservation,
};
use crate::runtime::engine::LuaEngine;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// Process-local instance identities never repeat when a pool or manager is recreated.
/// 池或管理器重新创建时，进程内实例身份绝不重复。
static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Parent admission metadata; weak pool references avoid ownership cycles.
/// 父级入场元数据；池的弱引用避免所有权循环。
struct PoolManagerState {
    /// Closing permanently prevents publishing another pool.
    /// 关闭永久阻止发布其他池。
    closing: bool,
    /// Currently reachable pools; removed owners are pruned during registration.
    /// 当前可访问的池；注册时清理已移除所有者。
    pools: Vec<Weak<ModulePool>>,
}

mod capacity;
pub use capacity::ModulePoolPlacement;

/// Parent ownership for real VM pools and one shared background retirement service.
/// 真实 VM 池与单个共享后台退役服务的父级所有权。
pub struct EmbeddedPoolManager {
    /// Parent closure serializes with new pool publication.
    /// 父级关闭相对新池发布保持串行。
    state: Mutex<PoolManagerState>,
    /// Shared aggregate budgets; callers observe but cannot bypass admission.
    /// 共享聚合预算；调用方可以观察但不能绕过入场。
    governor: Arc<PoolGovernor>,
    /// Immutable execution services independent from the host skill-manager write lock.
    /// 独立于宿主技能管理器写锁的不可变执行服务。
    engine: Arc<LuaEngine>,
    /// Cleanup keeps real VM ownership until native resources have drained.
    /// 清理在原生资源排空前保留真实 VM 所有权。
    retirement: RetirementService,
}

impl EmbeddedPoolManager {
    /// Construct explicit `config` budgets around immutable `engine` services.
    /// 围绕不可变 `engine` 服务构造显式 `config` 预算。
    /// Return a shared manager with one cleanup worker and no eagerly allocated VMs.
    /// 返回共享管理器，包含一个清理线程且不提前分配 VM。
    pub fn new(engine: Arc<LuaEngine>, config: EmbeddedRuntimeConfig) -> EmbeddedResult<Arc<Self>> {
        // Validate limits before starting any worker.
        // 启动任何工作线程前校验上限。
        let governor = PoolGovernor::new(config)?;
        Ok(Arc::new(Self {
            state: Mutex::new(PoolManagerState {
                closing: false,
                pools: Vec::new(),
            }),
            governor,
            engine,
            retirement: RetirementService::new()?,
        }))
    }

    /// Register an unbound module pool under explicit `group`, `definition` and `policy`.
    /// 根据显式 `group`、`definition` 与 `policy` 注册未绑定模块池。
    /// Unbound modules cannot reach legacy process-global host callbacks.
    /// 未绑定模块无法访问旧进程全局宿主回调。
    pub fn create_pool(
        self: &Arc<Self>,
        group: String,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
    ) -> EmbeddedResult<Arc<ModulePool>> {
        self.create_pool_internal(group, definition, policy, None, None)
    }

    /// Register a pool with immutable `capabilities` in the same generation and security domain.
    /// 在相同代次与安全域中注册具有不可变 `capabilities` 的池。
    /// Every resident VM inherits this exact snapshot and configuration revision.
    /// 每个常驻 VM 继承此精确快照与配置修订。
    pub fn create_pool_with_capabilities(
        self: &Arc<Self>,
        group: String,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        capabilities: ModuleCapabilities,
    ) -> EmbeddedResult<Arc<ModulePool>> {
        self.create_pool_internal(group, definition, policy, Some(capabilities), None)
    }

    /// Registers `group`, `definition`, and `policy` with explicit optional capabilities and a real host `owner`.
    /// 使用显式可选能力及真实宿主 `owner` 注册 `group`、`definition` 和 `policy`。
    /// Returns a pool retaining the owner through pending allocation, live VMs, and actual retirement.
    /// 返回跨等待分配、活动 VM 及实际退役保留所有者的池。
    pub fn create_pool_with_owner(
        self: &Arc<Self>,
        group: String,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        capabilities: Option<ModuleCapabilities>,
        owner: ModuleResourceOwner,
    ) -> EmbeddedResult<Arc<ModulePool>> {
        self.create_pool_internal(group, definition, policy, capabilities, Some(owner))
    }

    /// Register immutable `definition` and `policy` under exact host `group` identity.
    /// 将不可变 `definition` 与 `policy` 注册到精确宿主 `group` 身份。
    /// Each pool has one complete generation and security domain; capacity is shared by the parent.
    /// 每个池具有一个完整代次与安全域；容量由父级共享。
    pub(super) fn create_pool_internal(
        self: &Arc<Self>,
        group: String,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        capabilities: Option<ModuleCapabilities>,
        owner: Option<ModuleResourceOwner>,
    ) -> EmbeddedResult<Arc<ModulePool>> {
        self.create_pool_with_placement(
            ModulePoolPlacement::Independent { group },
            definition,
            policy,
            capabilities,
            owner,
        )
    }

    /// Return parent usage including failed or still-running retirement.
    /// 返回父级用量，包含失败或仍在运行的退役。
    pub fn usage(&self) -> EmbeddedResult<PoolUsage> {
        self.governor.usage(None)
    }

    /// Return the sole authoritative parent budgets.
    /// 返回唯一权威的父级预算。
    pub fn config(&self) -> &EmbeddedRuntimeConfig {
        self.governor.config()
    }

    /// Stop all pool admission and retire idle VMs; running and pinned owners remain charged.
    /// 停止全部池入场并退役空闲 VM；运行与固定所有者仍计入容量。
    pub fn request_close(&self) -> EmbeddedResult<()> {
        // Snapshot strong owners before releasing the parent metadata lock.
        // 释放父级元数据锁前获取强所有者快照。
        let pools = {
            // New pool publication cannot race past this closing transition.
            // 新池发布不能越过此次关闭转换。
            let mut state = self.state.lock().map_err(|_| {
                EmbeddedError::new(EmbeddedErrorCode::Internal, "pool manager lock is poisoned")
            })?;
            state.closing = true;
            state
                .pools
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        // Attempt every pool, even if one metadata lock reports a prior panic.
        // 即使某个元数据锁报告先前 panic，也尝试全部池。
        let mut first_error = None;
        for pool in pools {
            if let Err(error) = pool.close() {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Report complete shutdown only after every VM is destroyed and the cleanup worker is joined.
    /// 仅在全部 VM 销毁且清理工作线程已等待退出后报告关闭完成。
    /// Calling this before `request_close` reports false without changing admission.
    /// 在 `request_close` 前调用返回 false，且不改变入场。
    pub fn poll_closed(&self) -> EmbeddedResult<bool> {
        if !self
            .state
            .lock()
            .map_err(|_| {
                EmbeddedError::new(EmbeddedErrorCode::Internal, "pool manager lock is poisoned")
            })?
            .closing
        {
            return Ok(false);
        }
        if self.usage()?.resident != 0 {
            return Ok(false);
        }
        self.retirement.try_shutdown()
    }
}

/// Registration retained by both its pool and every resident module from that pool.
/// 由所属池及该池全部常驻模块共同保留的注册。
pub(super) struct PoolRegistration {
    /// Serializes release and usage so stale handles cannot observe a same-named new pool.
    /// 串行化释放与用量查询，防止旧句柄观察同名新池。
    lifecycle: Mutex<RegistrationLifecycle>,
    /// Exact registered group, never a guessed plugin or workspace alias.
    /// 精确注册分组，绝不是猜测的插件或工作区别名。
    group: String,
    /// Parent ledger remains alive through final registration removal.
    /// 父级账本保持存活，直到最后移除注册。
    governor: Arc<PoolGovernor>,
}

/// Closing and actual unregistration are distinct until every resident has been destroyed.
/// 全部常驻实例销毁前，关闭与实际注销保持不同状态。
struct RegistrationLifecycle {
    /// The pool will never reserve another instance from this registration.
    /// 池不再通过此注册预留其他实例。
    closing: bool,
    /// Exact registration was removed; stale handles return their own zero usage.
    /// 精确注册已移除；旧句柄返回自身的零用量。
    released: bool,
    /// Host resources remain owned until this registration closes and all actual residents are destroyed.
    /// 此注册关闭且全部真实常驻实例销毁前，持续拥有宿主资源。
    owner: Option<ModuleResourceOwner>,
}

impl PoolRegistration {
    /// Request permanent retirement and release guarantees if no residents remain.
    /// 请求永久退役，若无剩余常驻实例则释放保证。
    fn request_retirement(&self) -> EmbeddedResult<()> {
        self.lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closing = true;
        self.release_if_drained()
    }

    /// Release a closing registration exactly once; occupied resources remain explicitly registered.
    /// 恰好释放一次正在关闭的注册；占用的资源仍显式保持注册。
    fn release_if_drained(&self) -> EmbeddedResult<()> {
        // Hold this identity lock across unregistration and its published completion flag.
        // 跨越注销及其完成标记发布期间持有此身份锁。
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !lifecycle.closing || lifecycle.released {
            return Ok(());
        }
        match self.governor.unregister_group(&self.group) {
            Ok(()) => {
                lifecycle.released = true;
                // Only the closing, fully drained registration may relinquish the real host lease.
                // 只有正在关闭且已完全排空的注册才可以放弃真实宿主租约。
                let owner = lifecycle.owner.take();
                drop(lifecycle);
                // Drop outside registration metadata; keeping a closed pool handle does not pin old packages.
                // 在注册元数据之外释放；保留已关闭池句柄不会固定旧插件包。
                drop(owner);
                Ok(())
            }
            Err(error) if error.code == EmbeddedErrorCode::Busy => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Query this lifetime; released identities cannot alias a newly registered group name.
    /// 查询此生命周期；已释放身份不能成为新注册同名分组的别名。
    fn usage(&self) -> EmbeddedResult<PoolUsage> {
        // The same guard protects against recreation between lookup and lifetime validation.
        // 同一保护锁防止查询与生命周期校验之间发生重新创建。
        let lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lifecycle.released {
            return Ok(PoolUsage::default());
        }
        self.governor.usage(Some(&self.group))
    }
}

/// Final field guard rechecks registration retirement only after VM and resident token destruction.
/// 最后字段保护对象仅在 VM 与常驻令牌销毁后重新检查注册退役。
struct ResidentRegistration {
    /// Strong identity ownership prevents removal while this resident can execute.
    /// 强身份所有权防止在此常驻实例仍可执行时移除注册。
    registration: Arc<PoolRegistration>,
}

impl Drop for ResidentRegistration {
    /// Free old-generation reservations even when callers retain already-closed pool handles.
    /// 即使调用方保留已关闭池句柄，也释放旧代次预留。
    fn drop(&mut self) {
        if let Err(error) = self.registration.release_if_drained() {
            crate::runtime_logging::error(format!("pool registration retirement failed: {error}"));
        }
    }
}

impl Drop for PoolRegistration {
    /// Release dedicated guarantees only after the final resident token is destroyed.
    /// 仅在最后一个常驻令牌销毁后释放专用预留。
    fn drop(&mut self) {
        if let Err(error) = self.request_retirement() {
            crate::runtime_logging::error(format!("pool registration retirement failed: {error}"));
        }
    }
}

/// Exclusive resident ownership; declaration order makes the VM die before its capacity token.
/// 独占常驻所有权；声明顺序使 VM 先于其容量令牌销毁。
pub(super) struct ResidentModule {
    /// Actual VM and native resources, always destroyed first.
    /// 真实 VM 与原生资源，始终最先销毁。
    pub(super) module: EmbeddedModule,
    /// Capacity remains charged through module and native finalizers.
    /// 容量记账覆盖模块与原生终结器。
    pub(super) reservation: VmReservation,
    /// Successful business invocations, excluding initialization and health preparation.
    /// 成功业务调用次数，不包含初始化与健康准备。
    uses: u64,
    /// Most recent successful return to the idle pool.
    /// 最近一次成功归还空闲池的时间。
    idle_since: Instant,
    /// Private instance identity useful for proving reuse and partition isolation.
    /// 用于证明复用与分区隔离的私有实例身份。
    instance_id: String,
    /// Registration drops after the VM and its reservation.
    /// 注册在 VM 及其预留之后释放。
    _registration: ResidentRegistration,
    /// Read-only evidence does not retain this VM after destruction.
    /// 只读证据不会在销毁后保留此 VM。
    pub(super) retirement: ModuleRetirement,
}

/// Short-lock metadata with no execution or teardown inside the lock.
/// 短时锁元数据，锁内不执行或清理。
struct ModulePoolState {
    /// Admission is permanently closed for this immutable generation.
    /// 此不可变代次的入场已永久关闭。
    closed: bool,
    /// Idle instances exclusively owned by the pool.
    /// 由池独占拥有的空闲实例。
    idle: Vec<ResidentModule>,
}

/// Reusable pool bound to one immutable plugin generation and complete security context.
/// 绑定单个不可变插件代次与完整安全上下文的可复用池。
pub struct ModulePool {
    /// Frozen capability membership and configuration revision for every resident instance.
    /// 每个常驻实例使用的冻结能力成员与配置修订。
    capabilities: Option<ModuleCapabilities>,
    /// Parent capacity and shared cleanup services.
    /// 父级容量与共享清理服务。
    manager: Arc<EmbeddedPoolManager>,
    /// Lifetime registration also retained by every resident.
    /// 同时由全部常驻实例保留的生命周期注册。
    registration: Arc<PoolRegistration>,
    /// Fixed code, package identity, mounts, workspace and security partition.
    /// 固定代码、包身份、挂载、工作区及安全分区。
    definition: ModuleDefinition,
    /// Explicit capacity and reuse declaration, never normalized implicitly.
    /// 显式容量与复用声明，绝不隐式归一化。
    policy: PluginPoolConfig,
    /// Admission and idle ownership change atomically.
    /// 入场与空闲所有权变更保持原子性。
    state: Mutex<ModulePoolState>,
}

impl ModulePool {
    /// Borrow the immutable declared finalizer for lifecycle admission before VM initialization.
    /// 在 VM 初始化之前，借用不可变的已声明关闭回调以安排生命周期入场。
    /// Return none only when the registered module declares no automatic closing export.
    /// 仅已注册模块未声明自动关闭导出时返回空值。
    pub(super) fn finalizer(&self) -> Option<&ModuleFinalizer> {
        self.definition.finalizer.as_ref()
    }

    /// Derive immutable authority for `operation_id`, optional `session_id` and declared `export` from this exact pool.
    /// 从此精确池为 `operation_id`、可选 `session_id` 及已声明 `export` 派生不可变权威。
    /// Return context without locking, executing source or consulting a newer capability registration.
    /// 返回上下文，不加锁、不执行源码，也不查询较新的能力注册。
    pub(super) fn operation_context(
        &self,
        operation_id: &str,
        session_id: Option<&str>,
        export: Option<&str>,
        request_id: Option<&str>,
    ) -> EmbeddedResult<OperationContext> {
        // Formal scheduler pools always own a binding; low-level unbound pools cannot impersonate them.
        // 正式调度器的池始终拥有绑定；低层未绑定池不能冒充它们。
        let binding = self.capabilities.as_ref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "pool has no capability binding",
            )
        })?;
        Ok(OperationContext::Module(Box::new(ModuleOperationContext {
            finalization_instance_id: None,
            pool_id: self.registration.group.clone(),
            caller: binding.caller(
                &self.definition,
                operation_id.to_owned(),
                session_id.map(str::to_owned),
                request_id.map(str::to_owned),
            )?,
            capability_revision: binding.snapshot_revision(),
            export: export.map(str::to_owned),
        })))
    }

    /// Revoke live `permission` from this exact module binding without replacing its capability snapshot.
    /// 从此精确模块绑定撤销实时 `permission`，不替换能力快照。
    pub(super) fn revoke_capability_permission(&self, permission: &str) -> EmbeddedResult<bool> {
        let binding = self.capabilities.as_ref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "pool has no capability binding",
            )
        })?;
        binding.permissions.revoke(permission)
    }

    /// Reserve ordinary call ownership using `control`, without constructing a VM or executing Lua.
    /// 使用 `control` 预留普通调用所有权，不构造 VM 或执行 Lua。
    /// Return a prepared lease for one worker, or a capacity error safe to keep queued.
    /// 返回供单个工作线程使用的已准备租借，或可安全保持排队的容量错误。
    pub(crate) fn prepare(self: &Arc<Self>, control: &CallControl) -> EmbeddedResult<ModuleLease> {
        if self.policy.reuse == InstanceReuse::Session {
            return Err(EmbeddedError::invalid(
                "session pools require explicit session ownership",
            ));
        }
        self.prepare_owned(control, false)
    }

    /// Reserve exact ownership under `control`; `pinned` selects session-only state.
    /// 在 `control` 下预留精确所有权；`pinned` 选择仅会话使用的状态。
    /// This metadata-only step cannot execute initialization or host capabilities.
    /// 此元数据步骤不能执行初始化或宿主能力。
    pub(super) fn prepare_owned(
        self: &Arc<Self>,
        control: &CallControl,
        pinned: bool,
    ) -> EmbeddedResult<ModuleLease> {
        self.prepare_with_budget(control, pinned, true)
    }

    /// Prepare exact ownership under `control`; `allow_new` gates allocation, never existing idle reuse.
    /// 在 `control` 下准备精确所有权；`allow_new` 约束新分配，不约束既有空闲复用。
    /// `pinned` creates a fresh fixed-session instance; return a lease or an explicit capacity error.
    /// `pinned` 创建新的固定会话实例；返回租借或明确容量错误。
    pub(super) fn prepare_with_budget(
        self: &Arc<Self>,
        control: &CallControl,
        pinned: bool,
        allow_new: bool,
    ) -> EmbeddedResult<ModuleLease> {
        control.check()?;
        self.retire_expired()?;
        // Pool closure, idle selection and parent reservation share one admission transaction.
        // 池关闭、空闲选择与父级预留共享一次入场事务。
        let mut state = self.lock()?;
        if state.closed {
            return Err(closed());
        }
        let (resident, pending) = if !pinned && let Some(resident) = state.idle.pop() {
            (Some(resident), None)
        } else {
            if !allow_new {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::CapacityExceeded,
                    "plugin resident capacity is occupied or reserved",
                ));
            }
            // Opaque instance identities never repeat, including abandoned preparations.
            // 不透明实例身份绝不重复，包含已放弃的准备。
            let sequence = NEXT_INSTANCE_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| {
                    EmbeddedError::new(
                        EmbeddedErrorCode::Internal,
                        "pool instance identity exhausted",
                    )
                })?;
            let reservation = self.manager.governor.reserve(&self.registration.group)?;
            (
                None,
                Some(PendingAllocation {
                    reservation,
                    instance_id: format!("embedded-vm:{sequence}"),
                }),
            )
        };
        Ok(ModuleLease {
            ready: resident.is_some(),
            initialization_attempted: false,
            retirement: resident
                .as_ref()
                .map(|resident| resident.retirement.clone()),
            pending,
            pool: Arc::clone(self),
            resident,
            pinned,
            invocation_attempted: false,
        })
    }

    /// Acquire with `control`, retaining exact cleanup evidence if initialization fails.
    /// 使用 `control` 获取实例，初始化失败时保留精确清理证据。
    /// Return the lease or the original failure plus any actual allocated lifetime.
    /// 返回租借，或原始错误及实际已分配生命周期。
    pub fn acquire_tracked(
        self: &Arc<Self>,
        control: Arc<CallControl>,
    ) -> Result<ModuleLease, ModuleAcquireFailure> {
        if self.policy.reuse == InstanceReuse::Session {
            return Err(ModuleAcquireFailure {
                error: EmbeddedError::invalid("session pools require open_session"),
                retirement: None,
            });
        }
        // Allocation installs this evidence before any plugin source can execute.
        // 分配在任何插件源码可以执行前安装此证据。
        let mut retirement = None;
        self.acquire_owned(control, false, &mut retirement)
            .map_err(|error| ModuleAcquireFailure { error, retirement })
    }

    /// Open a pinned session with `control`, retaining cleanup evidence on failure.
    /// 使用 `control` 打开固定会话，失败时保留清理证据。
    /// Return exclusive session ownership or its original initialization failure.
    /// 返回独占会话所有权，或其原始初始化错误。
    pub fn open_session_tracked(
        self: &Arc<Self>,
        control: Arc<CallControl>,
    ) -> Result<ModuleLease, ModuleAcquireFailure> {
        if self.policy.reuse != InstanceReuse::Session {
            return Err(ModuleAcquireFailure {
                error: EmbeddedError::invalid("pool does not declare session reuse"),
                retirement: None,
            });
        }
        // A failed session never returns mutable state to ordinary reuse.
        // 失败会话绝不把可变状态归还普通复用路径。
        let mut retirement = None;
        self.acquire_owned(control, true, &mut retirement)
            .map_err(|error| ModuleAcquireFailure { error, retirement })
    }

    /// Prepare `count` warm instances under original `control`, returning actual idle ownership.
    /// 在原始 `control` 下准备 `count` 个预热实例，并归还真实空闲所有权。
    /// Failure preserves successfully initialized instances and reports the original error.
    /// 失败时保留已成功初始化实例，并报告原始错误。
    pub fn prewarm(
        self: &Arc<Self>,
        count: usize,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<()> {
        if self.policy.reuse != InstanceReuse::Reusable || count > self.policy.max_resident_vms {
            return Err(EmbeddedError::invalid(
                "prewarm requires reusable instances within the declared resident limit",
            ));
        }
        // Keep every acquired lease until the target is reached so one VM cannot satisfy it repeatedly.
        // 达到目标前保留全部租借，防止同一个 VM 重复满足目标。
        let mut leases = Vec::with_capacity(count);
        for _ in 0..count {
            leases.push(self.acquire(Arc::clone(&control))?);
        }
        drop(leases);
        Ok(())
    }

    /// Acquire an ordinary invocation instance using the original `control` deadline.
    /// 使用原始 `control` 截止时间获取普通调用实例。
    /// Capacity errors leave the request unexecuted; only declared reusable instances return to idle.
    /// 容量错误使请求保持未执行；仅声明为可复用的实例归还空闲池。
    pub fn acquire(self: &Arc<Self>, control: Arc<CallControl>) -> EmbeddedResult<ModuleLease> {
        self.acquire_tracked(control)
            .map_err(|failure| failure.error)
    }

    /// Pin one instance for an explicit session under original `control` initialization budget.
    /// 在原始 `control` 初始化预算下为显式会话固定一个实例。
    /// Idle session ownership charges resident capacity without charging execution capacity.
    /// 空闲会话所有权计入常驻容量，但不计入执行容量。
    pub fn open_session(
        self: &Arc<Self>,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<ModuleLease> {
        self.open_session_tracked(control)
            .map_err(|failure| failure.error)
    }

    /// Acquire under `control`; `pinned` prevents ordinary reuse and `retirement` retains failed ownership evidence.
    /// 在 `control` 下获取；`pinned` 阻止普通复用，`retirement` 保留失败所有权证据。
    /// Return exclusive ownership or the original failure, with allocated lifetime evidence populated first.
    /// 返回独占所有权或原始错误，已分配生命周期证据会先行填入。
    fn acquire_owned(
        self: &Arc<Self>,
        control: Arc<CallControl>,
        pinned: bool,
        retirement: &mut Option<ModuleRetirement>,
    ) -> EmbeddedResult<ModuleLease> {
        // Reservation happens separately so the scheduler can dispatch without running plugin code.
        // 单独进行预留，使调度器可以分发而不运行插件代码。
        let mut lease = if pinned {
            self.prepare_owned(control.as_ref(), true)?
        } else {
            self.prepare(control.as_ref())?
        };
        let initialized = lease.initialize(control);
        *retirement = lease.retirement.clone();
        if let Err(error) = initialized {
            lease.close();
            return Err(error);
        }
        Ok(lease)
    }

    /// Stop admission and asynchronously retire idle instances; borrowed instances retire on return.
    /// 停止入场并异步退役空闲实例；已借出实例在归还时退役。
    pub fn close(&self) -> EmbeddedResult<()> {
        // Transfer idle ownership before scheduling any cleanup.
        // 调度任何清理前转移空闲所有权。
        let idle = {
            // Closing is idempotent and never changes persistent host configuration.
            // 关闭幂等，且绝不修改持久宿主配置。
            let mut state = self.lock()?;
            state.closed = true;
            std::mem::take(&mut state.idle)
        };
        for module in idle {
            self.manager.retirement.enqueue(module);
        }
        self.registration.request_retirement()
    }

    /// Retire expired idle instances and return their count; pinned sessions are unaffected.
    /// 退役已过期空闲实例并返回数量；固定会话不受影响。
    pub fn retire_expired(&self) -> EmbeddedResult<usize> {
        // Expiration is an explicit policy; absent TTL never means an invented default.
        // 过期是显式策略；缺少 TTL 绝不表示编造默认值。
        let Some(ttl) = self.policy.idle_ttl_ms.map(Duration::from_millis) else {
            return Ok(0);
        };
        // Move ownership out of the lock before any module teardown.
        // 在任何模块清理前把所有权移出锁。
        let expired = {
            // Retain the declared minimum warm residents when reclaiming idle capacity.
            // 回收空闲容量时保留声明的最小预热常驻数。
            let mut state = self.lock()?;
            // This pass only removes as many idle instances as the reservation permits.
            // 本轮仅移除预留允许数量的空闲实例。
            let removable = state
                .idle
                .len()
                .saturating_sub(self.policy.min_resident_vms);
            // Extract in reverse order to avoid shifting yet-unchecked indices.
            // 按逆序提取，避免移动尚未检查的索引。
            let mut expired = Vec::new();
            for index in (0..state.idle.len()).rev() {
                if expired.len() < removable && state.idle[index].idle_since.elapsed() >= ttl {
                    expired.push(state.idle.swap_remove(index));
                }
            }
            expired
        };
        // The result counts retirement requests; actual capacity remains charged until cleanup.
        // 结果统计退役请求；实际容量在清理前仍被记账。
        let count = expired.len();
        for module in expired {
            self.manager.retirement.enqueue(module);
        }
        Ok(count)
    }

    /// Return exact group resource accounting including pinned and retiring instances.
    /// 返回包含固定与退役实例的精确分组资源记账。
    pub fn usage(&self) -> EmbeddedResult<PoolUsage> {
        self.registration.usage()
    }

    /// Return actual counters and the still-registered dedicated commitment as one identity-safe observation.
    /// 以一次身份安全观测返回实际计数与仍已注册的专用承诺。
    /// A released old registration contributes zero, even while the host retains its pool handle.
    /// 已释放旧注册贡献零，即使宿主仍保留池句柄。
    pub(super) fn accounting(&self) -> EmbeddedResult<(PoolUsage, usize)> {
        let lifecycle = self.registration.lifecycle.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "pool registration lock is poisoned",
            )
        })?;
        if lifecycle.released {
            return Ok((PoolUsage::default(), 0));
        }
        let usage = self
            .registration
            .governor
            .usage(Some(&self.registration.group))?;
        let committed = usage.resident.max(self.policy.min_resident_vms);
        Ok((usage, committed))
    }

    /// Return the immutable host-approved pool declaration.
    /// 返回不可变且经宿主批准的池声明。
    pub fn policy(&self) -> &PluginPoolConfig {
        &self.policy
    }

    /// Lock short-lived admission metadata, reporting poisoning explicitly.
    /// 锁定短时入场元数据，并明确报告中毒。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, ModulePoolState>> {
        self.state.lock().map_err(|_| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "module pool lock is poisoned")
        })
    }

    /// Return `resident` after exclusive use; `pinned` session state always retires.
    /// 独占使用后归还 `resident`；`pinned` 会话状态始终退役。
    fn release(&self, mut resident: ResidentModule, pinned: bool) -> ModuleRelease {
        // A panic during Lua execution leaves the module non-reusable before unwinding.
        // Lua 执行期间发生 panic 时，模块在栈展开前已不可复用。
        let reusable = !pinned
            && self.policy.reuse == InstanceReuse::Reusable
            && resident.module.is_reusable()
            && self
                .policy
                .max_uses
                .is_none_or(|limit| resident.uses < limit);
        if reusable {
            // Returning ownership and reporting disposition share the same closure boundary.
            // 归还所有权与报告去向共享同一个关闭边界。
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.closed && !self.state.is_poisoned() {
                resident.idle_since = Instant::now();
                state.idle.push(resident);
                return ModuleRelease::ReturnedToPool;
            }
        }
        // The receipt tracks this instance even if another resident remains active forever.
        // 即使其他常驻实例一直活跃，此回执也只跟踪当前实例。
        let retirement = resident.retirement.clone();
        self.manager.retirement.enqueue(resident);
        ModuleRelease::Retiring(retirement)
    }
}

impl Drop for ModulePool {
    /// Transfer all remaining idle instances to the shared bounded cleanup worker.
    /// 将全部剩余空闲实例转移到共享有界清理工作线程。
    fn drop(&mut self) {
        // No leases remain because each lease owns a strong pool reference.
        // 不再有租借，因为每个租借都拥有池的强引用。
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for module in std::mem::take(&mut state.idle) {
            self.manager.retirement.enqueue(module);
        }
        if let Err(error) = self.registration.request_retirement() {
            crate::runtime_logging::error(format!("pool registration retirement failed: {error}"));
        }
    }
}

/// Reserved identity and capacity before any actual VM allocation or initialization.
/// 在任何真实 VM 分配或初始化前预留的身份与容量。
struct PendingAllocation {
    /// Parent and group capacity charged through allocation or explicit abandonment.
    /// 计入父级与分组账本的容量，持续到分配或显式放弃。
    reservation: VmReservation,
    /// Immutable instance identity selected during fair scheduler dispatch.
    /// 公平调度分发期间选定的不可变实例身份。
    instance_id: String,
}

/// Exclusive invocation or pinned-session ownership; concurrent calls require different leases.
/// 独占调用或固定会话所有权；并发调用需要不同租借。
pub struct ModuleLease {
    /// Reserved capacity before construction; never executes code when abandoned.
    /// 构造前的预留容量；放弃时绝不执行代码。
    pending: Option<PendingAllocation>,
    /// True only after successful initialization or a validated reusable checkout.
    /// 仅在初始化成功或已校验可复用借用后为真。
    ready: bool,
    /// Initialization is attempted once; failure cannot replay source-side effects.
    /// 初始化仅尝试一次；失败不能重放源码副作用。
    initialization_attempted: bool,
    /// Receipt remains queryable after explicit close consumes resident ownership.
    /// 显式关闭消费常驻所有权后，回执仍可查询。
    retirement: Option<ModuleRetirement>,
    /// Strong reference preserves pool registration and cleanup scheduling.
    /// 强引用保留池注册与清理调度。
    pool: Arc<ModulePool>,
    /// Present until explicit close or final return.
    /// 显式关闭或最终归还前保持存在。
    resident: Option<ResidentModule>,
    /// Session ownership never returns mutable session state to the general idle pool.
    /// 会话所有权绝不把可变会话状态归还普通空闲池。
    pinned: bool,
    /// Single-call contracts forbid a second attempt even when input validation failed.
    /// 单次调用契约禁止第二次尝试，即使首次参数校验失败。
    invocation_attempted: bool,
}

impl ModuleLease {
    /// Borrow the exact reserved or constructed instance identity before scheduler publication.
    /// 在调度器发布前借用精确预留或已构造的实例身份。
    /// Return closed only when this lease no longer owns either explicit allocation state.
    /// 仅此租借不再拥有任一明确分配状态时返回已关闭。
    pub(super) fn allocation_id(&self) -> EmbeddedResult<&str> {
        match (&self.pending, &self.resident) {
            (Some(pending), None) => Ok(&pending.instance_id),
            (None, Some(resident)) => Ok(&resident.instance_id),
            _ => Err(closed()),
        }
    }

    /// Clone an eligible initialized module's immutable closing declaration for the scheduler.
    /// 为调度器克隆符合条件的已初始化模块的不可变关闭声明。
    pub(crate) fn finalization_plan(&self) -> Option<ModuleFinalizer> {
        self.resident
            .as_ref()
            .filter(|resident| resident.module.can_finalize())
            .and_then(|resident| resident.module.definition().finalizer.clone())
    }

    /// Construct and initialize prepared ownership under original `control`, without renewing its budget.
    /// 在原始 `control` 下构造并初始化已准备所有权，不续期预算。
    /// The caller retains this lease across errors and panic recovery so cleanup remains observable.
    /// 调用方跨错误及 panic 恢复保留此租借，使清理持续可观察。
    pub(crate) fn initialize(&mut self, control: Arc<CallControl>) -> EmbeddedResult<()> {
        self.initialize_for_session(control, None)
    }

    /// Initialize once under `control`, propagating the host-owned optional `session_id`.
    /// 在 `control` 下仅初始化一次，传递宿主拥有的可选 `session_id`。
    /// Return the original initialization failure without replaying source.
    /// 返回原始初始化错误，不重放源码。
    pub(super) fn initialize_for_session(
        &mut self,
        control: Arc<CallControl>,
        session_id: Option<&str>,
    ) -> EmbeddedResult<()> {
        control.check()?;
        if self.pool.lock()?.closed {
            return Err(closed());
        }
        if self.ready {
            return Ok(());
        }
        if self.initialization_attempted {
            return Err(closed());
        }
        self.initialization_attempted = true;
        // Capacity was committed before dispatch; abandoning before construction releases only that reservation.
        // 容量在分发前已提交；构造前放弃仅释放该预留。
        let PendingAllocation {
            mut reservation,
            instance_id,
        } = self.pending.take().ok_or_else(closed)?;
        let permit = reservation.begin_execution()?;
        let module = self
            .pool
            .manager
            .engine
            .allocate_embedded_module(self.pool.definition.clone(), &instance_id)?;
        drop(permit);
        let receipt = ModuleRetirement::new(instance_id.clone());
        self.retirement = Some(receipt.clone());
        self.resident = Some(ResidentModule {
            retirement: receipt,
            module,
            reservation,
            uses: 0,
            idle_since: Instant::now(),
            instance_id,
            _registration: ResidentRegistration {
                registration: Arc::clone(&self.pool.registration),
            },
        });
        // Install actual ownership before initialization can invoke host code or unwind.
        // 在初始化可能调用宿主代码或栈展开前安装实际所有权。
        let resident = self
            .resident
            .as_mut()
            .expect("allocated ownership is installed");
        {
            let _permit = resident.reservation.begin_execution()?;
            if let Some(capabilities) = &self.pool.capabilities {
                resident.module.bind_capabilities(capabilities.clone())?;
            }
            resident
                .module
                .initialize_for_session(control, session_id)?;
        }
        resident.reservation.mark_ready()?;
        if self.pool.lock()?.closed {
            return Err(closed());
        }
        self.ready = true;
        Ok(())
    }

    /// Return this exact instance's receipt, including after explicit asynchronous close.
    /// 返回此精确实例的回执，包含显式异步关闭之后。
    /// This observes lifetime only and cannot initiate or cancel retirement.
    /// 此操作仅观察生命周期，不能发起或取消退役。
    pub fn retirement_handle(&self) -> EmbeddedResult<ModuleRetirement> {
        self.retirement.clone().ok_or_else(closed)
    }

    /// Consume the lease and report whether request ownership returned or still needs retirement.
    /// 消费租借，并报告请求所有权已归还还是仍需退役。
    /// A previously closed lease returns the same retirement evidence, never false reuse.
    /// 已关闭租借返回同一退役证据，绝不误报复用。
    pub fn finish(mut self) -> EmbeddedResult<ModuleRelease> {
        self.pending.take();
        match self.resident.take() {
            Some(resident) => Ok(self.pool.release(resident, self.pinned)),
            None => Ok(match self.retirement.clone() {
                Some(receipt) => ModuleRelease::Retiring(receipt),
                None => ModuleRelease::NoInstance,
            }),
        }
    }

    /// Invoke `invocation` under exact parent and group permits; no whole-engine lock is held.
    /// 在精确父级与分组许可下执行 `invocation`；不持有整个引擎锁。
    pub fn invoke(&mut self, invocation: ModuleInvocation<'_>) -> EmbeddedResult<Value> {
        invocation.control.check()?;
        if !self.ready {
            return Err(closed());
        }
        if self.pool.policy.reuse == InstanceReuse::SingleCall && self.invocation_attempted {
            return Err(closed());
        }
        // This check linearizes call admission against generation closure.
        // 此检查使调用入场相对代次关闭线性化。
        let state = self.pool.lock()?;
        if state.closed {
            return Err(closed());
        }
        // A closed lease cannot acquire execution capacity.
        // 已关闭租借不能获取执行容量。
        let resident = self.resident.as_mut().ok_or_else(closed)?;
        if self
            .pool
            .policy
            .max_uses
            .is_some_and(|limit| resident.uses >= limit)
        {
            return Err(closed());
        }
        // Checked use accounting prevents unlimited reuse after sequence overflow.
        // 受检使用计数防止序号溢出后无限复用。
        let next_use = resident.uses.checked_add(1).ok_or_else(|| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "module use counter exhausted")
        })?;
        // The permit remains charged during host waiting and request cleanup.
        // 等待宿主与请求清理期间许可仍计入容量。
        let _permit = resident.reservation.begin_execution()?;
        drop(state);
        self.invocation_attempted = true;
        // Actual structured invocation, without eval-generated wrappers.
        // 真实结构化调用，不生成 eval 包装。
        let value = resident.module.invoke(invocation)?;
        resident.uses = next_use;
        Ok(value)
    }

    /// Attempt one declared closing export on this VM with an independently supplied finite control.
    /// 使用独立提供的有限控制，在此 VM 上尝试一次声明的关闭导出。
    /// Return its result separately from business execution; call finish or close to retire the VM.
    /// 将其结果与业务执行分别返回；调用 finish 或 close 才会退役 VM。
    /// Closed pools and exhausted business-use limits still allow cleanup, without restoring permissions.
    /// 已关闭池和耗尽的业务使用额度仍允许清理，但不恢复权限。
    /// Capacity rejection precedes the attempt and permits explicit retry; all entered attempts are terminal.
    /// 容量拒绝发生于尝试之前并允许显式重试；所有已经进入的尝试均为终态。
    pub fn finalize(&mut self, invocation: ModuleInvocation<'_>) -> EmbeddedResult<Value> {
        // Keep real VM ownership and execution accounting until the closing call actually returns.
        // 在关闭调用实际返回前保持真实 VM 所有权和执行记账。
        let resident = self.resident.as_mut().ok_or_else(closed)?;
        let _permit = resident.reservation.begin_execution()?;
        self.ready = false;
        resident.module.finalize(invocation)
    }

    /// Return whether a pinned VM can serve another call within its declared use limit.
    /// 返回固定 VM 是否可以在声明的使用上限内服务下一次调用。
    pub(super) fn can_retain_session(&self) -> bool {
        self.ready
            && self.resident.as_ref().is_some_and(|resident| {
                self.pool
                    .policy
                    .max_uses
                    .is_none_or(|limit| resident.uses < limit)
            })
    }

    /// Return immutable instance identity or a closed-handle error.
    /// 返回不可变实例身份，或已关闭句柄错误。
    pub fn instance_id(&self) -> EmbeddedResult<&str> {
        self.resident
            .as_ref()
            .map(|resident| resident.instance_id.as_str())
            .ok_or_else(closed)
    }

    /// Retire this exact instance asynchronously; repeated close is harmless.
    /// 异步退役此精确实例；重复关闭无副作用。
    pub fn close(&mut self) {
        self.ready = false;
        self.pending.take();
        if let Some(resident) = self.resident.take() {
            self.pool.manager.retirement.enqueue(resident);
        }
    }
}

impl Drop for ModuleLease {
    /// Return safe reusable state or retain failed/session ownership for real retirement.
    /// 归还安全可复用状态，或保留失败与会话所有权以执行真实退役。
    fn drop(&mut self) {
        self.pending.take();
        if let Some(resident) = self.resident.take() {
            self.pool.release(resident, self.pinned);
        }
        if let Err(error) = self.pool.registration.release_if_drained() {
            crate::runtime_logging::error(format!("pool registration retirement failed: {error}"));
        }
    }
}

/// Construct the explicit closed-generation error without guessing another pool.
/// 构造明确的代次关闭错误，不猜测其他池。
fn closed() -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::Closed, "module pool or lease is closed")
}
