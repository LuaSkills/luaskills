use super::{EmbeddedFfiStatus, transport};
use crate::runtime::embedded::{
    EmbeddedError, EmbeddedErrorCode, EmbeddedResult, EmbeddedRuntime, EmbeddedRuntimeConfig,
    EmbeddedRuntimeUsage, PoolUsage,
};
use crate::{LuaEngine, LuaEngineOptions};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(test)]
mod tests;

/// Initialization ownership is separate from the core's execution and closing state.
/// 初始化所有权独立于核心的执行及关闭状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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
    /// Construction returned an explicit error after releasing unpublished resources.
    /// 构造在释放未发布资源后返回明确错误。
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
    /// Optional only to allow explicit drop ordering in the destructor.
    /// 仅为允许析构器中的显式释放顺序而设为可选。
    runtime: Option<Arc<EmbeddedRuntime>>,
}

/// Queryable construction and core closure evidence; no runtime implementation state is inferred by SDKs.
/// 可查询构造与核心关闭证据；SDK 不推断运行时实现状态。
#[derive(Serialize)]
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
    /// True only after native core workers have exited, or no core was ever created.
    /// 仅当原生核心工作线程已退出或从未创建核心时为真。
    closed: bool,
    /// Live scheduler observations directly from the core when available.
    /// 可用时直接来自核心的实时调度观测。
    usage: Option<EmbeddedRuntimeUsage>,
    /// Live resident and execution accounting directly from the core when available.
    /// 可用时直接来自核心的实时常驻与执行计数。
    resources: Option<PoolUsage>,
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
    ) -> EmbeddedResult<()> {
        self.initialize_with(|| {
            config.validate()?;
            let engine = LuaEngine::new(options).map_err(|error| {
                EmbeddedError::new(
                    EmbeddedErrorCode::InvalidArgument,
                    format!("engine initialization failed: {error}"),
                )
            })?;
            EmbeddedRuntime::new(Arc::new(engine), config)
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
            if state.closing {
                state.runtime.clone()
            } else {
                None
            }
        };
        let close_result = close_after_construction
            .as_ref()
            .map(|runtime| runtime.request_close());
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
        state.runtime.as_ref().map(|runtime| {
            state.active += 1;
            RuntimeLease {
                slot: Arc::clone(self),
                runtime: Some(Arc::clone(runtime)),
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
        self.lease(&mut state).ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "FFI runtime has no initialized core; query runtime_status",
            )
        })
    }

    /// Permanently close this exact slot, including a future core still under construction.
    /// 永久关闭此精确槽，包含仍在构造中的未来核心。
    pub(super) fn request_close(self: &Arc<Self>) -> EmbeddedResult<()> {
        let lease = {
            let mut state = self.lock()?;
            state.closing = true;
            self.lease(&mut state)
        };
        if let Some(lease) = lease {
            lease.runtime().request_close()?;
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
                error: state.error.clone(),
            };
            (snapshot, self.lease(&mut state))
        };
        if let Some(lease) = lease {
            let runtime = lease.runtime();
            snapshot.core_runtime_id = Some(runtime.id().into());
            snapshot.usage = Some(runtime.usage()?);
            snapshot.resources = Some(runtime.resources()?);
            snapshot.closed = runtime.poll_closed()?;
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
            state.released = true;
            state.runtime.take()
        };
        drop(removed);
        Ok(())
    }
}

impl RuntimeLease {
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
