//! Scheduler-owned reusable readiness is distinct from physical allocator idleness.
//! 调度器拥有的可复用就绪状态与物理分配器空闲状态不同。

use super::*;

/// One exact reusable pool observation under the scheduler lock, not a future residency guarantee.
/// 在调度器锁下对单个精确可复用池的观测，不是未来驻留保证。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedReusablePoolSnapshot {
    /// Original registered pool identity.
    /// 原始已登记池身份。
    pub pool_id: String,
    /// Confirmed idle leases eligible for borrowing after applying declared idle expiration.
    /// 应用声明空闲过期规则后，已确认且符合借用条件的空闲租借数。
    pub ready: usize,
    /// Existing allocations still owned by initialization, business, checkpointing or retirement.
    /// 仍由初始化、业务、检查点或退役拥有的现有分配数。
    pub unavailable: usize,
    /// Actual allocator occupancy, including creating and retiring allocations.
    /// 真实分配器占用，包含创建及退役中的分配。
    pub physical: PoolUsage,
    /// Immutable module resident ceiling; aggregate resource admission may still reject allocation.
    /// 不可变模块常驻上限；聚合资源入场仍可能拒绝分配。
    pub max_resident_vms: usize,
    /// Permanent runtime or pool admission closure.
    /// 永久运行时或池入场关闭。
    pub closing: bool,
    /// Retained infrastructure or checkpoint failure currently fences new work.
    /// 保留基础设施或检查点故障当前阻止新工作。
    pub admission_blocked: bool,
}

impl EmbeddedRuntime {
    /// Observe exact id's formal cache; reject unknown and non-reusable pools without creating resources.
    /// 观测精确 id 的正式缓存；拒绝未知及非复用池，不创建资源。
    /// Apply due idle-expiration metadata before counting; Lua execution and teardown remain with existing workers.
    /// 计数前应用到期空闲元数据；Lua 执行及清理仍属于既有工作者。
    /// Return confirmed ready ownership separately from allocator idleness and retained unconfirmed operations.
    /// 将已确认就绪归属与分配器空闲及保留未确认操作分开返回。
    pub fn reusable_pool_status(&self, id: &str) -> EmbeddedResult<EmbeddedReusablePoolSnapshot> {
        // One scheduler lock excludes dispatch, terminal cache publication and closure interleaving.
        // 单个调度器锁排除分发、终态缓存发布和关闭的交错。
        let mut state = self.center.lock()?;
        // Unknown identities never resolve to another generation's pool.
        // 未知身份绝不解析至另一代次的池。
        let pool = state.pools.get(id).ok_or_else(not_found)?;
        if pool.pool.policy().reuse != InstanceReuse::Reusable {
            return Err(EmbeddedError::invalid(
                "reusable status requires an explicitly reusable pool",
            ));
        }
        reusable::expire(&mut state, id);
        // The same immutable pool remains registered throughout the locked observation.
        // 加锁观测期间，同一不可变池保持已登记。
        let pool = state.pools.get(id).expect("validated reusable pool");
        // Closure and fault evidence are authoritative, independent of currently resident VM counts.
        // 关闭及故障证据为权威，独立于当前驻留 VM 计数。
        let closing = state.closing || pool.closed;
        let admission_blocked = state.failure.is_some()
            || state.shared_checkpoint_failed
            || !state.persistence_failures.is_empty();
        // Only a lease returned by the scheduler's terminal publication can be counted as ready.
        // 仅由调度器终态发布归还的租借可以计为就绪。
        let ready = state
            .reusable_instances
            .values()
            .filter(|instance| {
                instance.pool_id == id
                    && !instance.closing
                    && instance.active.is_none()
                    && instance.lease.is_some()
            })
            .count();
        // Retained cache entries include work and cleanup whose physical ownership has not been acknowledged away.
        // 保留缓存条目包含尚未确认释放物理归属的工作及清理。
        let retained = state
            .reusable_instances
            .values()
            .filter(|instance| instance.pool_id == id)
            .count();
        Ok(EmbeddedReusablePoolSnapshot {
            pool_id: id.to_owned(),
            ready: if closing || admission_blocked {
                0
            } else {
                ready
            },
            unavailable: retained
                - if closing || admission_blocked {
                    0
                } else {
                    ready
                },
            physical: pool.pool.usage()?,
            max_resident_vms: pool.pool.policy().max_resident_vms,
            closing,
            admission_blocked,
        })
    }
}
