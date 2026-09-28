use super::{EmbeddedFfiStatus, transport};
use crate::runtime::embedded::{
    EmbeddedError, EmbeddedErrorCode, EmbeddedResult, EmbeddedRuntime, EmbeddedRuntimeConfig,
    EmbeddedRuntimeUsage, OperationJournalWorkerStatus, PoolUsage,
};
use crate::{LuaEngine, LuaEngineOptions};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

mod persistence;
use persistence::PersistenceOwner;
pub(super) use persistence::RuntimePersistenceConfig;

#[cfg(test)]
mod tests;

/// Initialization ownership is separate from the core's execution and closing state.
/// 初始化所有权独立于核心的执行及关闭状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) enum InitializationPhase {
    /// A known identity exists but no engine or worker has been constructed.
    /// 已存在已知身份，但尚未构造引擎或工作线程。
    Reserved,
    /// Exactly one native call owns construction.
    /// 精确一个原生调用拥有构造过程。
    Initializing,
    /// The actual core owner was stored successfully.
    /// 实际核心所有者已成功保存。
    Ready,
    /// Construction returned an explicit error; retained storage still requires verified drainage.
    /// 构造返回明确错误；已保留存储仍需验证排空。
    Failed,
    /// Construction panicked and safe library unloading cannot be proven.
    /// 构造发生 panic，无法证明可以安全卸载动态库。
    Faulted,
}

/// Short-lock lifetime metadata; runtime execution and destruction happen outside this lock.
/// 短锁寿命元数据；运行时执行与析构在此锁外发生。
struct RuntimeState {
    /// One-shot initialization state; failures are never retried under the same identity.
    /// 单次初始化状态；失败绝不在同一身份下重试。
    initialization: InitializationPhase,
    /// Permanent admission gate, including initialization still underway.
    /// 永久入场门，包含仍在进行的初始化。
    closing: bool,
    /// Exact removal fence for references cloned before transport deregistration.
    /// 在传输注销前已克隆引用的精确移除屏障。
    released: bool,
    /// Actual native users holding core references, including the constructing call.
    /// 持有核心引用的实际原生使用者，包含构造调用。
    active: usize,
    /// Actual core owner, retained until all workers and users have drained.
    /// 实际核心所有者，保留到全部工作线程与使用者排空。
    runtime: Option<Arc<EmbeddedRuntime>>,
    /// Storage created during initialization, retained even if later core construction fails.
    /// 初始化期间创建的存储，即使后续核心构造失败也继续保留。
    persistence: Option<Arc<PersistenceOwner>>,
    /// Retained initialization or close failure, queryable through the known slot identity.
    /// 保留的初始化或关闭失败，可通过已知槽身份查询。
    error: Option<EmbeddedError>,
}

/// One non-reusable FFI runtime identity allocated before construction can create native workers.
/// 在构造能够创建原生工作线程前分配的一个不可复用 FFI 运行时身份。
pub(super) struct RuntimeSlot {
    /// Exact transport-local control identity, independent of core operation namespaces.
    /// 精确传输局部控制身份，独立于核心操作命名空间。
    pub(super) id: String,
    /// Authoritative lifetime and initialization ownership.
    /// 权威寿命与初始化所有权。
    state: Mutex<RuntimeState>,
}

/// One active native user; the core reference is dropped before the active counter is released.
/// 一个活动原生使用者；核心引用在活动计数释放前被丢弃。
pub(super) struct RuntimeLease {
    /// Exact registered slot whose removal is blocked by this lease.
    /// 此租借阻止移除的精确注册槽。
    slot: Arc<RuntimeSlot>,
    /// Absent for storage-only cleanup after failed construction; otherwise dropped before the user fence.
    /// 构造失败后的纯存储清理中不存在；其他情况下在使用者屏障前释放。
    runtime: Option<Arc<EmbeddedRuntime>>,
    /// Optional durable ownership is dropped before this lease releases the native-user fence.
    /// 可选持久所有权在此租借释放原生使用者屏障前被丢弃。
    persistence: Option<Arc<PersistenceOwner>>,
}

/// Queryable construction and core closure evidence; no runtime implementation state is inferred by SDKs.
/// 可查询构造与核心关闭证据；SDK 不推断运行时实现状态。
#[derive(Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(super) struct RuntimeSnapshot {
    /// Exact FFI slot identity used by every control command.
    /// 每个控制命令使用的精确 FFI 槽身份。
    runtime_id: String,
    /// Actual runtime namespace, present only after successful construction.
    /// 实际运行时命名空间，仅在成功构造后存在。
    core_runtime_id: Option<String>,
    /// Actual one-shot construction state.
    /// 实际单次构造状态。
    initialization: InitializationPhase,
    /// Whether this slot has permanently closed admission.
    /// 此槽是否已永久关闭入场。
    closing: bool,
    /// True only after construction finishes and all created core and storage workers and receipts drain.
    /// 仅在构造结束且全部已创建核心、存储线程与回执排空后为真。
    closed: bool,
    /// Live scheduler observations directly from the core when available.
    /// 可用时直接来自核心的实时调度观测。
    usage: Option<EmbeddedRuntimeUsage>,
    /// Live resident and execution accounting directly from the core when available.
    /// 可用时直接来自核心的实时常驻与执行计数。
    resources: Option<PoolUsage>,
    /// Actual storage worker status when durable ownership has been created, including failed construction.
    /// 持久所有权创建后的实际存储工作线程状态，包含构造失败。
    persistence: Option<OperationJournalWorkerStatus>,
    /// Retained failure; a failed construction still owns its bounded registration until explicit release.
    /// 保留失败；构造失败仍拥有其有界注册，直到显式释放。
    error: Option<EmbeddedError>,
}

impl RuntimeSlot {
    /// Allocate one metadata-only slot; return an explicit transport failure on identity exhaustion.
    /// 分配一个仅含元数据的槽；身份耗尽时返回明确传输失败。
    pub(super) fn new() -> Result<Arc<Self>, EmbeddedFfiStatus> {
        Ok(Arc::new(Self {
            id: format!("ffi-runtime:{}", transport::identity()?),
            state: Mutex::new(RuntimeState {
                initialization: InitializationPhase::Reserved,
                closing: false,
                released: false,
                active: 0,
                runtime: None,
                persistence: None,
                error: None,
            }),
        }))
    }

    /// Lock authoritative metadata, rejecting poisoned or permanently released ownership.
    /// 锁定权威元数据，拒绝中毒或已永久释放的所有权。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, RuntimeState>> {
        let state = self
            .state
            .lock()
            .map_err(|_| internal("FFI runtime owner is poisoned"))?;
        if state.released {
            return Err(closed());
        }
        Ok(state)
    }

    /// Initialize once from explicit `options` and `config`; retained status owns the construction outcome.
    /// 从显式 `options` 与 `config` 初始化一次；保留状态拥有构造结果。
    /// A successful return acknowledges a completed attempt, whose success or failure is queried separately.
    /// 成功返回确认一次尝试已完成，其成功或失败另行查询。
    pub(super) fn initialize(
        &self,
        options: LuaEngineOptions,
        config: EmbeddedRuntimeConfig,
        persistence: Option<RuntimePersistenceConfig>,
    ) -> EmbeddedResult<()> {
        self.initialize_with(|| {
            config.validate()?;
            let engine = LuaEngine::new(options).map_err(|error| {
                EmbeddedError::new(
                    EmbeddedErrorCode::InvalidArgument,
                    format!("engine initialization failed: {error}"),
                )
            })?;
            match persistence {
                None => EmbeddedRuntime::new(Arc::new(engine), config),
                Some(persistence) => {
                    // Publish storage ownership before another fallible constructor can leave a native thread alive.
                    // 在其他可失败构造器可能留下活动原生线程前发布存储所有权。
                    let owner = PersistenceOwner::new(persistence)?;
                    self.lock()?.persistence = Some(Arc::clone(&owner));
                    EmbeddedRuntime::with_journal_worker(
                        Arc::new(engine),
                        config,
                        Arc::clone(&owner.writer),
                    )
                }
            }
        })
    }

    /// Run trusted internal `factory` outside metadata locks; retain panic state instead of declaring unload safe.
    /// 在元数据锁外运行可信内部 `factory`；保留 panic 状态，不宣称卸载安全。
    fn initialize_with(
        &self,
        factory: impl FnOnce() -> EmbeddedResult<EmbeddedRuntime>,
    ) -> EmbeddedResult<()> {
        {
            let mut state = self.lock()?;
            if state.closing {
                return Err(closed());
            }
            if state.initialization != InitializationPhase::Reserved {
                return Err(busy());
            }
            state.initialization = InitializationPhase::Initializing;
            state.active += 1;
        }
        let constructed = catch_unwind(AssertUnwindSafe(factory));
        let close_after_construction = {
            let mut state = self.lock()?;
            match constructed {
                Ok(Ok(runtime)) => {
                    state.runtime = Some(Arc::new(runtime));
                    state.initialization = InitializationPhase::Ready;
                }
                Ok(Err(error)) => {
                    state.error = Some(error);
                    state.initialization = InitializationPhase::Failed;
                }
                Err(_) => {
                    state.error = Some(internal(
                        "runtime initialization panicked; safe unloading is unproven",
                    ));
                    state.initialization = InitializationPhase::Faulted;
                }
            }
            if state.closing || state.initialization == InitializationPhase::Failed {
                Some((state.runtime.clone(), state.persistence.clone()))
            } else {
                None
            }
        };
        let close_result = close_after_construction
            .as_ref()
            .map(|(runtime, persistence)| {
                if let Some(runtime) = runtime {
                    runtime.request_close()
                } else {
                    if let Some(persistence) = persistence {
                        persistence.writer.request_close();
                    }
                    Ok(())
                }
            });
        drop(close_after_construction);
        let mut state = self.lock()?;
        if let Some(Err(error)) = close_result {
            state.error = Some(error);
        }
        state.active -= 1;
        Ok(())
    }

    /// Retain a core lease from locked `state`; callers run actual core methods after releasing metadata.
    /// 从已锁定 `state` 保留核心租借；调用方在释放元数据后运行实际核心方法。
    fn lease(self: &Arc<Self>, state: &mut RuntimeState) -> Option<RuntimeLease> {
        (state.runtime.is_some() || state.persistence.is_some()).then(|| {
            state.active += 1;
            RuntimeLease {
                slot: Arc::clone(self),
                runtime: state.runtime.clone(),
                persistence: state.persistence.clone(),
            }
        })
    }

    /// Retain the actual core for one command; `admission` rejects new work after the slot closes.
    /// 为一个命令保留实际核心；`admission` 在槽关闭后拒绝新工作。
    /// Control commands remain available for cancellation, host acknowledgements and drainage.
    /// 控制命令仍可用于取消、宿主确认及排空。
    pub(super) fn acquire(self: &Arc<Self>, admission: bool) -> EmbeddedResult<RuntimeLease> {
        let mut state = self.lock()?;
        if admission && state.closing {
            return Err(closed());
        }
        if state.runtime.is_none() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "FFI runtime has no initialized core; query runtime_status",
            ));
        }
        Ok(self
            .lease(&mut state)
            .expect("initialized slot creates one retained native lease"))
    }

    /// Permanently close this exact slot, including a future core still under construction.
    /// 永久关闭此精确槽，包含仍在构造中的未来核心。
    pub(super) fn request_close(self: &Arc<Self>) -> EmbeddedResult<()> {
        let lease = {
            let mut state = self.lock()?;
            state.closing = true;
            if state.initialization == InitializationPhase::Initializing {
                None
            } else {
                self.lease(&mut state)
            }
        };
        if let Some(lease) = lease {
            if let Some(runtime) = &lease.runtime {
                runtime.request_close()?;
            } else if let Some(persistence) = &lease.persistence {
                persistence.writer.request_close();
            }
        }
        Ok(())
    }

    /// Read actual construction, usage and worker-join evidence without holding metadata during core calls.
    /// 读取实际构造、用量与工作线程汇合证据，核心调用期间不持有元数据锁。
    pub(super) fn snapshot(self: &Arc<Self>) -> EmbeddedResult<RuntimeSnapshot> {
        let (mut snapshot, lease) = {
            let mut state = self.lock()?;
            let snapshot = RuntimeSnapshot {
                runtime_id: self.id.clone(),
                core_runtime_id: None,
                initialization: state.initialization,
                closing: state.closing,
                closed: state.closing
                    && matches!(
                        state.initialization,
                        InitializationPhase::Reserved | InitializationPhase::Failed
                    ),
                usage: None,
                resources: None,
                persistence: None,
                error: state.error.clone(),
            };
            (snapshot, self.lease(&mut state))
        };
        if let Some(lease) = lease {
            if let Some(runtime) = &lease.runtime {
                snapshot.core_runtime_id = Some(runtime.id().into());
                snapshot.usage = Some(runtime.usage()?);
                snapshot.resources = Some(runtime.resources()?);
            }
            if snapshot.closing
                && matches!(
                    snapshot.initialization,
                    InitializationPhase::Ready | InitializationPhase::Failed
                )
            {
                snapshot.closed = lease.poll_closed()?;
            }
            if let Some(persistence) = &lease.persistence {
                snapshot.persistence = Some(persistence.writer.status()?);
            }
        }
        Ok(snapshot)
    }

    /// Fence removal only after close, actual worker exit and all native users; drop the core outside metadata.
    /// 仅在关闭、实际工作线程退出和全部原生使用者结束后设置移除屏障；在元数据外丢弃核心。
    pub(super) fn release(&self) -> EmbeddedResult<()> {
        let removed = {
            let mut state = self.lock()?;
            if !state.closing
                || state.active != 0
                || state.initialization == InitializationPhase::Initializing
            {
                return Err(busy());
            }
            if state.initialization == InitializationPhase::Faulted {
                return Err(internal(
                    "faulted initialization prevents proving safe runtime release",
                ));
            }
            if let Some(runtime) = &state.runtime
                && !runtime.poll_closed()?
            {
                return Err(busy());
            }
            if let Some(persistence) = &state.persistence
                && !persistence.poll_closed()?
            {
                return Err(busy());
            }
            state.released = true;
            (state.runtime.take(), state.persistence.take())
        };
        drop(removed);
        Ok(())
    }
}

impl RuntimeLease {
    /// Observe actual core drainage before asking its independently owned storage worker to close.
    /// 请求独立拥有的存储工作线程关闭前，观测核心实际排空。
    /// Return false for remaining ownership; propagate failures rather than asserting safe library unload.
    /// 尚有所有权时返回假；传播失败，不断言动态库可安全卸载。
    fn poll_closed(&self) -> EmbeddedResult<bool> {
        if let Some(runtime) = &self.runtime
            && !runtime.poll_closed()?
        {
            return Ok(false);
        }
        match &self.persistence {
            Some(persistence) => persistence.poll_closed(),
            None => Ok(true),
        }
    }

    /// Borrow explicitly configured storage for this initialized native lease or reject memory-only mode.
    /// 借用此已初始化原生租借显式配置的存储，或拒绝纯内存模式。
    pub(super) fn persistence(&self) -> EmbeddedResult<&PersistenceOwner> {
        self.persistence.as_deref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "runtime persistence is not configured",
            )
        })
    }
    /// Borrow the actual core for this live lease; the private constructor always installs its owner.
    /// 为此活动租借借用实际核心；私有构造器始终安装其所有者。
    pub(super) fn runtime(&self) -> &EmbeddedRuntime {
        self.runtime
            .as_deref()
            .expect("live runtime lease owns its core")
    }
}

impl Drop for RuntimeLease {
    /// Drop the actual core reference before permitting slot release, even during an outer unwind.
    /// 在允许槽释放前丢弃实际核心引用，即使外层正在栈展开。
    fn drop(&mut self) {
        drop(self.runtime.take());
        drop(self.persistence.take());
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active -= 1;
    }
}

/// Return an explicit ownership conflict without discarding live construction or native users.
/// 返回明确所有权冲突，不丢弃活动构造或原生使用者。
fn busy() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::Busy,
        "FFI runtime ownership is still active",
    )
}

/// Return the permanent exact-identity closure error.
/// 返回永久精确身份关闭错误。
fn closed() -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::Closed, "FFI runtime admission is closed")
}

/// Return an infrastructure failure with the specific English `message`.
/// 返回带具体英文 `message` 的基础设施失败。
fn internal(message: &str) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::Internal, message)
}
