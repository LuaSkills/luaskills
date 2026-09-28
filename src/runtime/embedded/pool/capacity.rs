//! Explicit physical placement keeps shared guarantees separate from module lifetime.
//! 显式物理归属使共享保证与模块寿命分离。

use super::*;
use crate::runtime::embedded::{VmCapacityConfig, VmCapacitySnapshot};

/// Immutable placement of one module's physical allocations.
/// 单个模块物理分配的不可变归属。
#[derive(Debug, Clone)]
pub enum ModulePoolPlacement {
    /// Preserve the original independent module reservation semantics.
    /// 保留原独立模块预留语义。
    Independent {
        /// Exact module identity in this manager's governor.
        /// 此管理器治理器中的精确模块身份。
        group: String,
    },
    /// Share one already-registered capacity owner while retaining independent Lua state.
    /// 共享一个已注册容量所有者，同时保留独立 Lua 状态。
    Capacity {
        /// Exact module identity, distinct from its capacity owner's lifetime.
        /// 精确模块身份，其寿命独立于容量所有者。
        group: String,
        /// Explicit existing capacity identity; absence is an error, never a fallback.
        /// 显式既有容量身份；缺失为错误，绝不回退。
        capacity_id: String,
    },
}

impl EmbeddedPoolManager {
    /// Revise exact physical ownership for the formal scheduler; reject a closing parent.
    /// 为正式调度器修订精确物理归属；拒绝关闭中的父级。
    /// Return validation failures before modifying the governor's effective policy.
    /// 修改治理器生效策略前返回校验失败。
    pub(crate) fn revise_capacity(&self, id: &str, config: VmCapacityConfig) -> EmbeddedResult<()> {
        // Parent closure and policy publication use the same established admission gate.
        // 父级关闭与策略发布使用同一既有入场门。
        let state = self.state.lock().map_err(|_| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "pool manager lock is poisoned")
        })?;
        if state.closing {
            return Err(closed());
        }
        self.governor.revise_capacity(id, config)
    }

    /// Register a persistent physical capacity owner by exact id and config; return any admission failure.
    /// 按精确标识及配置注册持久物理容量所有者；返回任何入场失败。
    /// The registration consumes no VM and is serialized with parent closure.
    /// 注册不消耗 VM，并与父级关闭串行。
    pub fn register_capacity(&self, id: &str, config: VmCapacityConfig) -> EmbeddedResult<()> {
        // Closing and capacity publication use the same original parent gate.
        // 关闭及容量发布使用同一原父级门。
        let state = self.state.lock().map_err(|_| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "pool manager lock is poisoned")
        })?;
        if state.closing {
            return Err(closed());
        }
        self.governor.register_capacity(id, config)
    }

    /// Query exact capacity id; return actual physical usage and its still-held reservation.
    /// 查询精确容量标识；返回实际物理用量及仍持有的预留。
    pub fn capacity(&self, id: &str) -> EmbeddedResult<VmCapacitySnapshot> {
        self.governor.capacity(id)
    }

    /// Remove a capacity id only after all module registrations retire; return busy otherwise.
    /// 仅在全部模块注册退役后移除容量标识；否则返回忙碌。
    /// The explicit owner may release an empty guarantee without destroying previously returned pool handles.
    /// 显式所有者可释放空保证，而无需销毁先前返回的池句柄。
    pub fn unregister_capacity(&self, id: &str) -> EmbeddedResult<()> {
        self.governor.unregister_capacity(id)
    }

    /// Create an isolated module at explicit placement with policy and optional capabilities/resource owner.
    /// 在显式归属位置以策略及可选能力／资源所有者创建隔离模块。
    /// Return its pool after atomic publication; registration executes no Lua source or capability handler.
    /// 原子发布后返回池；注册不执行 Lua 源码或能力处理器。
    pub fn create_pool_with_placement(
        self: &Arc<Self>,
        placement: ModulePoolPlacement,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        capabilities: Option<ModuleCapabilities>,
        owner: Option<ModuleResourceOwner>,
    ) -> EmbeddedResult<Arc<ModulePool>> {
        definition.validate()?;
        for export in &definition.exports {
            export.compile()?;
        }
        // Parent admission closes atomically with publication of all reachable pools.
        // 父级入场相对全部可访问池的发布原子关闭。
        let mut state = self.state.lock().map_err(|_| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "pool manager lock is poisoned")
        })?;
        if state.closing {
            return Err(closed());
        }
        // The tagged placement selects one exact authority path without guessing or conversion.
        // 带标签归属选择唯一精确权威路径，不猜测或转换。
        let group = match placement {
            ModulePoolPlacement::Independent { group } => {
                self.governor.register_group(&group, policy.clone())?;
                group
            }
            ModulePoolPlacement::Capacity { group, capacity_id } => {
                self.governor
                    .register_group_in_capacity(&group, policy.clone(), &capacity_id)?;
                group
            }
        };
        // Every VM retains the original module registration, which in turn retains immutable membership.
        // 每个 VM 保留原模块注册，该注册又保留不可变成员关系。
        let pool = Arc::new(ModulePool {
            capabilities,
            manager: Arc::clone(self),
            registration: Arc::new(PoolRegistration {
                group,
                governor: Arc::clone(&self.governor),
                lifecycle: Mutex::new(RegistrationLifecycle {
                    closing: false,
                    released: false,
                    owner,
                }),
            }),
            definition,
            policy,
            state: Mutex::new(ModulePoolState {
                closed: false,
                idle: Vec::new(),
            }),
        });
        // Closed handles cannot accumulate unbounded weak entries in this registry.
        // 已关闭句柄不能在此注册表累积无界弱引用。
        state.pools.retain(|pool| {
            pool.upgrade().is_some_and(|pool| match pool.lock() {
                Ok(state) => !state.closed,
                // Preserve poisoned ownership so shutdown reports failure instead of losing resources.
                // 保留中毒所有权，使关闭报告失败而非丢失资源。
                Err(_) => true,
            })
        });
        state.pools.push(Arc::downgrade(&pool));
        Ok(pool)
    }
}
