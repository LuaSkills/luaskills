//! Plugin-owned capacity registrations share physical guarantees and complete scheduling limits.
//! 插件自有容量注册共享物理保证及完整调度限制。

use super::*;

/// Immutable capacity policy owned by one plugin across isolated module generations.
/// 单个插件跨隔离模块代次持有的不可变容量策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedCapacityConfig {
    /// Sole physical guarantee and execution limit used by the governor and scheduler together.
    /// 治理器和调度器共同使用的唯一物理保证及执行上限。
    pub resources: VmCapacityConfig,
    /// Maximum accepted queued requests across every member pool.
    /// 全部成员池已接纳排队请求的数量上限。
    pub max_queued_calls: usize,
    /// Maximum exact serialized bytes of queued requests across members.
    /// 全部成员排队请求精确序列化字节数上限。
    pub max_queued_bytes: usize,
}

impl EmbeddedCapacityConfig {
    /// Validate all limits against exact parent and plugin budgets without changing any declaration.
    /// 针对精确父级及插件预算校验全部限制，不改变任何声明。
    /// Return an explicit conflict; zero reservations remain valid and queue limits must be positive.
    /// 返回明确冲突；零预留保持有效，队列上限必须为正。
    pub fn validate(
        &self,
        parent: &EmbeddedRuntimeConfig,
        plugin: &EmbeddedPluginConfig,
    ) -> EmbeddedResult<()> {
        plugin.validate(parent)?;
        self.resources.validate(parent)?;
        if self.resources.max_resident_vms > plugin.max_resident_vms
            || self.resources.max_running_calls > plugin.max_running_calls
            || self.max_queued_calls == 0
            || self.max_queued_calls > plugin.max_queued_calls
            || self.max_queued_bytes == 0
            || self.max_queued_bytes > plugin.max_queued_bytes
        {
            return Err(EmbeddedError::invalid(
                "capacity limits are outside the plugin budget",
            ));
        }
        Ok(())
    }
}

/// Retained scheduling ownership refers to the same exact low-level capacity identity.
/// 保留的调度归属引用同一精确低层容量身份。
pub(super) struct ScheduledCapacity {
    /// Immutable plugin owner; a member's definition must name the same plugin.
    /// 不可变插件所有者；成员定义必须指定同一插件。
    pub(super) plugin_id: String,
    /// Validated policy never replaced while this identity is retained.
    /// 此身份保留期间绝不替换的已校验策略。
    pub(super) config: EmbeddedCapacityConfig,
    /// Permanent closure rejects membership and business while permitting existing finalization.
    /// 永久关闭拒绝成员注册及业务，同时允许既有关闭执行。
    pub(super) closing: bool,
}

/// Actual capacity status includes physical ownership and scheduler work retained through cleanup.
/// 实际容量状态包含物理所有权及保留至清理完成的调度工作。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedCapacitySnapshot {
    /// Runtime-generated opaque identity, never reused after forgetting.
    /// 运行时生成的不透明身份，遗忘后绝不复用。
    pub capacity_id: String,
    /// Exact immutable plugin owner.
    /// 精确不可变插件所有者。
    pub plugin_id: String,
    /// Original complete policy, including physical and queued-work budgets.
    /// 原完整策略，包含物理及排队工作预算。
    pub config: EmbeddedCapacityConfig,
    /// Actual physical state, including creation, native waiting and retirement.
    /// 实际物理状态，包含创建、原生等待及退役。
    pub resources: PoolUsage,
    /// Non-lendable commitment remains visible even with zero physical VMs.
    /// 即使物理 VM 为零，不可借用承诺仍可见。
    pub committed_resident_vms: usize,
    /// Dispatched operations keep this charge through actual cleanup and result publication.
    /// 已分发操作跨实际清理及结果发布保留此计费。
    pub active_operations: usize,
    /// Accepted requests still waiting for execution admission.
    /// 仍等待执行入场的已接纳请求。
    pub queued_calls: usize,
    /// Exact serialized bytes retained by those queued requests.
    /// 这些排队请求保留的精确序列化字节数。
    pub queued_bytes: usize,
    /// Retained scheduled member identities, including closed pools awaiting explicit forgetting.
    /// 保留的已调度成员身份，包含等待显式遗忘的已关闭池。
    pub retained_pools: usize,
    /// Capacity, owning plugin or parent has permanently closed business admission.
    /// 容量、所属插件或父级已永久关闭业务入场。
    pub closing: bool,
}

impl EmbeddedRuntime {
    /// Register capacity for the exact existing plugin using config; return a fresh opaque identity.
    /// 使用 config 为精确既有插件注册容量；返回新的不透明身份。
    /// Parent and plugin guarantees are both checked before publication; no module code executes.
    /// 发布前同时检查父级及插件保证；不执行模块代码。
    pub fn register_capacity(
        &self,
        plugin_id: &str,
        config: EmbeddedCapacityConfig,
    ) -> EmbeddedResult<String> {
        // One scheduling transaction orders plugin ownership, physical commitment and parent closure.
        // 单个调度事务排序插件归属、物理承诺及父级关闭。
        let mut state = self.center.lock()?;
        if state.closing {
            return Err(closed());
        }
        // No implicit plugin registration or name-based fallback is permitted.
        // 不允许隐式插件注册或按名称回退。
        let plugin = state.plugins.get(plugin_id).ok_or_else(not_found)?;
        if plugin.closing {
            return Err(closed());
        }
        config.validate(self.center.pools.config(), &plugin.config)?;
        if state.capacities.len() >= self.center.pools.config().max_registered_pools
            || state
                .capacities
                .values()
                .filter(|capacity| capacity.plugin_id == plugin_id)
                .count()
                >= plugin.config.max_registered_pools
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "retained capacity registration limit reached",
            ));
        }
        if config.resources.min_resident_vms
            > plugin
                .config
                .max_resident_vms
                .saturating_sub(state.plugin_commitment(plugin_id, None)?)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "capacity reservations exceed plugin resident budget",
            ));
        }
        // Reserve an identity without publishing it on any validation or physical registration failure.
        // 预留身份；任何校验或物理注册失败时均不发布该身份。
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal("capacity identity exhausted"))?;
        // Fresh runtime-qualified capacity identity.
        // 新的运行时限定容量身份。
        let id = IdentityKind::Capacity.render(self.id(), sequence);
        self.center
            .pools
            .register_capacity(&id, config.resources.clone())?;
        state.sequence = sequence;
        state.capacities.insert(
            id.clone(),
            ScheduledCapacity {
                plugin_id: plugin_id.to_owned(),
                config,
                closing: false,
            },
        );
        Ok(id)
    }

    /// Observe exact capacity id and original plugin ownership; return actual counters or not found.
    /// 观测精确容量标识及原插件归属；返回实际计数或未找到。
    pub fn capacity(&self, id: &str) -> EmbeddedResult<EmbeddedCapacitySnapshot> {
        // Hold one scheduling snapshot while the physical governor reports its current counters.
        // 物理治理器报告当前计数期间，持有单个调度快照。
        let state = self.center.lock()?;
        // Exact retained capacity ownership is never resolved through a fallback.
        // 精确保留容量归属绝不通过回退解析。
        let capacity = state.capacities.get(id).ok_or_else(not_found)?;
        // Physical governor snapshot sampled under retained scheduling ownership.
        // 在保留调度归属期间采样的物理治理器快照。
        let physical = self.center.pools.capacity(id)?;
        // Counts and bytes are derived together from the authoritative request queues.
        // 数量和字节共同从权威请求队列派生。
        let (queued_calls, queued_bytes) = state.capacity_queue(id);
        Ok(EmbeddedCapacitySnapshot {
            capacity_id: id.to_owned(),
            plugin_id: capacity.plugin_id.clone(),
            config: capacity.config.clone(),
            resources: physical.resources,
            committed_resident_vms: physical.committed_resident_vms,
            active_operations: state.capacity_active(id),
            queued_calls,
            queued_bytes,
            retained_pools: state
                .pools
                .values()
                .filter(|pool| pool.capacity_id.as_deref() == Some(id))
                .count(),
            closing: capacity.closing
                || state.closing
                || state
                    .plugins
                    .get(&capacity.plugin_id)
                    .expect("capacity retains plugin")
                    .closing,
        })
    }

    /// Close capacity id and every exact member generation; running business drains without replay.
    /// 关闭容量标识及全部精确成员代次；运行中业务排空且不重放。
    /// Return after close requests, not after physical completion or reservation release.
    /// 在关闭请求后返回，不代表物理完成或预留释放。
    pub fn close_capacity(&self, id: &str) -> EmbeddedResult<()> {
        // Freeze admission and snapshot all original members before invoking physical close paths.
        // 调用物理关闭路径前，冻结入场并获取全部原成员快照。
        let pools = {
            // The scheduler gate orders closure against new member admission.
            // 调度门将关闭与新成员入场排序。
            let mut state = self.center.lock()?;
            state.capacities.get_mut(id).ok_or_else(not_found)?.closing = true;
            state
                .pools
                .values_mut()
                .filter(|pool| pool.capacity_id.as_deref() == Some(id))
                .map(|pool| {
                    pool.closed = true;
                    Arc::clone(&pool.pool)
                })
                .collect::<Vec<_>>()
        };
        // Attempt all members even when one physical close reports an infrastructure failure.
        // 即使一个物理关闭报告基础设施失败，也尝试全部成员。
        let mut failure = None;
        for pool in pools {
            if let Err(error) = pool.close() {
                failure.get_or_insert(error);
            }
        }
        self.center.changed.notify_all();
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Forget closed capacity id after every scheduled member was explicitly forgotten.
    /// 全部已调度成员被显式遗忘后，遗忘已关闭容量标识。
    /// Physical unregistration must also succeed before its plugin guarantee is released.
    /// 插件保证释放前，物理注销也必须成功。
    pub fn forget_capacity(&self, id: &str) -> EmbeddedResult<()> {
        // Keep old membership and physical release ordered against future registrations.
        // 相对未来注册排序旧成员关系及物理释放。
        let mut state = self.center.lock()?;
        // Exact retained capacity ownership is never resolved through a fallback.
        // 精确保留容量归属绝不通过回退解析。
        let capacity = state.capacities.get(id).ok_or_else(not_found)?;
        if !(capacity.closing
            || state.closing
            || state
                .plugins
                .get(&capacity.plugin_id)
                .expect("capacity retains plugin")
                .closing)
            || state
                .pools
                .values()
                .any(|pool| pool.capacity_id.as_deref() == Some(id))
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "capacity ownership has not drained",
            ));
        }
        self.center.pools.unregister_capacity(id)?;
        state.capacities.remove(id);
        self.center.changed.notify_all();
        Ok(())
    }

    /// Register an isolated module in capacity id using its immutable definition, policy, permissions and revision.
    /// 使用不可变定义、策略、权限及修订，在容量标识下注册隔离模块。
    /// Return a fresh pool identity only when both declarations name the same plugin.
    /// 仅在两份声明指定同一插件时返回新的池身份。
    pub fn register_pool_in_capacity(
        &self,
        capacity_id: &str,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        permissions: Arc<CapabilityPermissions>,
        revision: String,
    ) -> EmbeddedResult<String> {
        self.register_pool_internal(
            definition,
            policy,
            permissions,
            revision,
            None,
            Some(capacity_id),
        )
    }

    /// Register an isolated module in capacity id while retaining the supplied native resource owner.
    /// 在容量标识下注册隔离模块，同时保留给定原生资源所有者。
    /// Definition, policy, permissions and revision remain immutable; return the exact new pool identity.
    /// 定义、策略、权限及修订保持不可变；返回精确新池身份。
    pub fn register_pool_with_owner_in_capacity(
        &self,
        capacity_id: &str,
        definition: ModuleDefinition,
        policy: PluginPoolConfig,
        permissions: Arc<CapabilityPermissions>,
        revision: String,
        owner: ModuleResourceOwner,
    ) -> EmbeddedResult<String> {
        self.register_pool_internal(
            definition,
            policy,
            permissions,
            revision,
            Some(owner),
            Some(capacity_id),
        )
    }
}

impl SchedulerState {
    /// Return idle reusable members of capacity id that still retain physical ownership.
    /// 返回容量标识下仍保留物理所有权的空闲可复用成员数量。
    /// Closing members cannot satisfy an idle floor or be selected for a second retirement.
    /// 关闭中成员不能满足空闲下限，也不能被选中再次退役。
    pub(super) fn capacity_idle(&self, id: &str) -> usize {
        self.reusable_instances
            .values()
            .filter(|instance| {
                !instance.closing
                    && instance.active.is_none()
                    && instance.lease.is_some()
                    && self
                        .pools
                        .get(&instance.pool_id)
                        .expect("reusable pool retained")
                        .capacity_id
                        .as_deref()
                        == Some(id)
            })
            .count()
    }

    /// Validate member plugin and complete pool policy against exact retained capacity; return any conflict.
    /// 针对精确保留容量校验成员插件及完整池策略；返回任何冲突。
    pub(super) fn validate_capacity_member(
        &self,
        id: &str,
        plugin_id: &str,
        policy: &PluginPoolConfig,
    ) -> EmbeddedResult<()> {
        // A foreign capacity never grants another plugin access to its budget or physical reservation.
        // 外来容量绝不授予另一插件其预算或物理预留的访问权。
        let capacity = self.capacities.get(id).ok_or_else(not_found)?;
        if capacity.plugin_id != plugin_id {
            return Err(EmbeddedError::invalid(
                "capacity and module plugin ownership differ",
            ));
        }
        if capacity.closing {
            return Err(closed());
        }
        if policy.kind != capacity.config.resources.kind
            || policy.min_resident_vms != 0
            || policy.max_resident_vms > capacity.config.resources.max_resident_vms
            || policy.max_running_calls > capacity.config.resources.max_running_calls
            || policy.max_queued_calls > capacity.config.max_queued_calls
        {
            return Err(EmbeddedError::invalid(
                "module policy conflicts with its capacity owner",
            ));
        }
        Ok(())
    }

    /// Return dispatched member operations, including cleanup retained after execution returns.
    /// 返回已分发成员操作，包含执行返回后保留的清理。
    pub(super) fn capacity_active(&self, id: &str) -> usize {
        self.pools
            .values()
            .filter(|pool| pool.capacity_id.as_deref() == Some(id))
            .map(|pool| pool.active)
            .sum()
    }

    /// Return exact queued count and serialized bytes from the sole request queues.
    /// 从唯一请求队列返回精确排队数量及序列化字节数。
    pub(super) fn capacity_queue(&self, id: &str) -> (usize, usize) {
        self.queues
            .values()
            .flat_map(|queue| queue.iter())
            .filter(|call| {
                self.pools
                    .get(call.request.pool_id())
                    .expect("queued pool retained")
                    .capacity_id
                    .as_deref()
                    == Some(id)
            })
            .fold((0, 0), |(calls, bytes), call| {
                (calls + 1, bytes + call.bytes)
            })
    }

    /// Return actual member residency; pending physical retirement remains counted.
    /// 返回实际成员常驻量；待完成物理退役仍计入。
    pub(super) fn capacity_resident(&self, id: &str) -> EmbeddedResult<usize> {
        self.pools
            .values()
            .filter(|pool| pool.capacity_id.as_deref() == Some(id))
            .try_fold(0usize, |total, pool| {
                total
                    .checked_add(pool.pool.usage()?.resident)
                    .ok_or_else(|| internal("capacity residency overflow"))
            })
    }

    /// Check aggregate execution admission for pool id, including finalizers on closed capacity.
    /// 检查池标识的聚合执行入场，包含已关闭容量上的关闭器。
    /// Ordinary business closure is checked separately; return false only for an occupied execution allowance.
    /// 普通业务关闭单独检查；仅当执行额度已占用时返回假。
    pub(super) fn capacity_allows_execution(&self, pool_id: &str) -> EmbeddedResult<bool> {
        // Independent pools keep the original parent/plugin/domain limits without an implicit capacity.
        // 独立池保留原父级／插件／域限制，不采用隐式容量。
        let pool = self.pools.get(pool_id).ok_or_else(not_found)?;
        match &pool.capacity_id {
            None => Ok(true),
            Some(id) => Ok(self.capacity_active(id)
                < self
                    .capacities
                    .get(id)
                    .expect("member retains capacity")
                    .config
                    .resources
                    .max_running_calls),
        }
    }

    /// Reject accepted queued work that would exceed its capacity's exact count or byte limits.
    /// 拒绝会超出容量精确数量或字节上限的已接纳排队工作。
    /// Pool id and serialized bytes select one immutable owner; success does not publish any operation.
    /// 池标识及序列化字节数选择单个不可变所有者；成功不会发布操作。
    pub(super) fn validate_capacity_queue(
        &self,
        pool_id: &str,
        bytes: usize,
    ) -> EmbeddedResult<()> {
        // Explicit absence means independent placement, not failed lookup of a requested owner.
        // 明确缺省表示独立归属，不表示请求所有者查找失败。
        let pool = self.pools.get(pool_id).ok_or_else(not_found)?;
        if let Some(id) = &pool.capacity_id {
            // Exact retained capacity ownership is never resolved through a fallback.
            // 精确保留容量归属绝不通过回退解析。
            let capacity = self.capacities.get(id).expect("member retains capacity");
            if capacity.closing {
                return Err(closed());
            }
            // Existing queue ownership is measured before admitting the new request.
            // 接纳新请求前测量既有队列归属。
            let (queued, retained_bytes) = self.capacity_queue(id);
            if queued >= capacity.config.max_queued_calls
                || bytes
                    > capacity
                        .config
                        .max_queued_bytes
                        .saturating_sub(retained_bytes)
            {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::CapacityExceeded,
                    "capacity request queue limit reached",
                ));
            }
        }
        Ok(())
    }
}
