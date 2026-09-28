use super::*;

/// One immutable aggregate policy and its queue/retention counters, guarded by the scheduler lock.
/// 由调度锁保护的单个不可变聚合策略及其队列、保留计数。
pub(super) struct ScheduledPlugin {
    /// Host-approved aggregate budgets, never copied from an arbitrary first pool.
    /// 宿主批准的聚合预算，绝不从任意首个池复制。
    pub(super) config: EmbeddedPluginConfig,
    /// Accepted queued requests across every execution domain.
    /// 全部执行域已接纳的排队请求。
    pub(super) queued: usize,
    /// Exact serialized bytes retained in all plugin queues.
    /// 全部插件队列保留的精确序列化字节数。
    pub(super) bytes: usize,
    /// All retained operation identities, including completed results.
    /// 全部保留操作身份，包含已完成结果。
    pub(super) operations: usize,
    /// Future lifecycle operations already charged against the plugin retention limit.
    /// 已计入插件保留上限的未来生命周期操作。
    pub(super) reserved_operations: usize,
    /// Permanent admission closure until all owned metadata can be explicitly removed.
    /// 永久入场关闭，直到全部所属元数据可以显式移除。
    pub(super) closing: bool,
}

/// Plugin-wide observation from exact immutable pool ownership, including draining generations.
/// 根据精确不可变池归属形成的插件级观测，包含正在排空的代次。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedPluginSnapshot {
    /// Trusted host plugin identity used by module definitions and fair scheduling.
    /// 模块定义与公平调度使用的可信宿主插件身份。
    pub plugin_id: String,
    /// Immutable effective aggregate policy.
    /// 不可变有效聚合策略。
    pub config: EmbeddedPluginConfig,
    /// Actual resident VM counters through confirmed destruction.
    /// 持续记账到确认销毁的实际常驻 VM 计数。
    pub resources: PoolUsage,
    /// Actual residents plus all unused dedicated domain guarantees.
    /// 实际常驻实例与全部未使用专用域保证的合计。
    pub committed_resident_vms: usize,
    /// Dispatched unfinished operations, including initialization and cleanup.
    /// 已分发未完成操作，包含初始化与清理。
    pub active_operations: usize,
    /// Accepted queued calls across every domain and session.
    /// 全部域和会话已接纳的排队调用。
    pub queued_calls: usize,
    /// Exact serialized bytes still held by queued requests.
    /// 排队请求仍持有的精确序列化字节数。
    pub queued_bytes: usize,
    /// Retained pools, including closed metadata not explicitly forgotten.
    /// 保留池，包含尚未显式遗忘的已关闭元数据。
    pub retained_pools: usize,
    /// Retained fixed sessions, including closed records.
    /// 保留固定会话，包含已关闭记录。
    pub retained_sessions: usize,
    /// Retained operations, including results whose callers dropped their handles.
    /// 保留操作，包含调用方已丢弃句柄的结果。
    pub retained_operations: usize,
    /// Capacity reserved for closing long-lived instances, before their operation identities become queryable.
    /// 为关闭长生命周期实例预留的容量，此时相应操作身份尚不可查询。
    pub reserved_operations: usize,
    /// Whether new pool and call admission is permanently closed.
    /// 新池及新调用入场是否已永久关闭。
    pub closing: bool,
}

impl EmbeddedRuntime {
    /// Register explicit `config` for trusted `plugin_id` before any of its pools are activated.
    /// 在任何池激活前，为可信 `plugin_id` 注册显式 `config`。
    /// Return success or reject duplicates, invalid budgets and parent registration exhaustion.
    /// 返回成功，或拒绝重复身份、无效预算及父级注册耗尽。
    pub fn register_plugin(
        &self,
        plugin_id: String,
        config: EmbeddedPluginConfig,
    ) -> EmbeddedResult<()> {
        config.validate(self.center.pools.config())?;
        if plugin_id.trim().is_empty() {
            return Err(EmbeddedError::invalid("plugin identity must be nonempty"));
        }
        json_size(&plugin_id, self.center.pools.config().max_value_bytes)?;
        let mut state = self.center.lock()?;
        if state.closing {
            return Err(closed());
        }
        if state.plugins.contains_key(&plugin_id) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "plugin is already registered",
            ));
        }
        if state.plugins.len() >= self.center.pools.config().max_registered_plugins {
            return Err(capacity("retained plugin capacity reached"));
        }
        state.plugins.insert(
            plugin_id,
            ScheduledPlugin {
                config,
                queued: 0,
                bytes: 0,
                operations: 0,
                reserved_operations: 0,
                closing: false,
            },
        );
        Ok(())
    }

    /// Read the aggregate state of exact `plugin_id`; results include all retained generations.
    /// 读取精确 `plugin_id` 的聚合状态；结果包含全部保留代次。
    /// Return missing identity errors without probing another registry or default policy.
    /// 返回身份缺失错误，不探测其他注册表或默认策略。
    pub fn plugin(&self, plugin_id: &str) -> EmbeddedResult<EmbeddedPluginSnapshot> {
        let state = self.center.lock()?;
        let plugin = state.plugins.get(plugin_id).ok_or_else(plugin_not_found)?;
        let mut resources = PoolUsage::default();
        for pool in state
            .pools
            .values()
            .filter(|pool| pool.plugin_id == plugin_id)
        {
            let current = pool.pool.usage()?;
            for (total, value) in [
                (&mut resources.resident, current.resident),
                (&mut resources.creating, current.creating),
                (&mut resources.idle, current.idle),
                (&mut resources.running, current.running),
                (&mut resources.retiring, current.retiring),
            ] {
                *total = total
                    .checked_add(value)
                    .ok_or_else(|| internal("plugin resource count overflow"))?;
            }
        }
        Ok(EmbeddedPluginSnapshot {
            plugin_id: plugin_id.to_owned(),
            config: plugin.config.clone(),
            resources,
            committed_resident_vms: state.plugin_commitment(plugin_id, None)?,
            active_operations: state.plugin_active(plugin_id),
            queued_calls: plugin.queued,
            queued_bytes: plugin.bytes,
            retained_pools: state
                .pools
                .values()
                .filter(|pool| pool.plugin_id == plugin_id)
                .count(),
            retained_sessions: state.plugin_sessions(plugin_id),
            retained_operations: plugin.operations,
            reserved_operations: plugin.reserved_operations,
            closing: plugin.closing || state.closing,
        })
    }

    /// Permanently close `plugin_id` and every exact pool it owns; unrelated plugins remain admissible.
    /// 永久关闭 `plugin_id` 及其拥有的每个精确池；无关插件仍可入场。
    /// Return after requesting retirement, without claiming that running work or VMs have stopped.
    /// 请求退役后返回，不宣称运行任务或 VM 已停止。
    pub fn close_plugin(&self, plugin_id: &str) -> EmbeddedResult<()> {
        let pools = {
            let mut state = self.center.lock()?;
            state
                .plugins
                .get_mut(plugin_id)
                .ok_or_else(plugin_not_found)?
                .closing = true;
            state
                .pools
                .values_mut()
                .filter(|pool| pool.plugin_id == plugin_id)
                .map(|pool| {
                    pool.closed = true;
                    Arc::clone(&pool.pool)
                })
                .collect::<Vec<_>>()
        };
        let mut failure = None;
        // Attempt every exact close even if one metadata owner reports an infrastructure error.
        // 即使一个元数据所有者报告基础设施错误，也尝试关闭每个精确池。
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

    /// Forget closed `plugin_id` only after capacities, pools, sessions and retained operations were removed.
    /// 仅在容量、池、会话和保留操作移除后遗忘已关闭的 `plugin_id`。
    /// Return busy while any authoritative ownership or result metadata remains.
    /// 任何权威所有权或结果元数据仍存在时返回忙碌。
    pub fn forget_plugin(&self, plugin_id: &str) -> EmbeddedResult<()> {
        let mut state = self.center.lock()?;
        let plugin = state.plugins.get(plugin_id).ok_or_else(plugin_not_found)?;
        if (!plugin.closing && !state.closing)
            || plugin.operations != 0
            || plugin.reserved_operations != 0
            || state.pools.values().any(|pool| pool.plugin_id == plugin_id)
            || state
                .capacities
                .values()
                .any(|capacity| capacity.plugin_id == plugin_id)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "plugin ownership or retained records have not drained",
            ));
        }
        state.plugins.remove(plugin_id);
        Ok(())
    }
}

impl SchedulerState {
    /// Count dispatched ownership across `plugin_id`, including cleanup that no longer occupies an executor.
    /// 统计 `plugin_id` 全部已分发所有权，包含不再占执行线程的清理。
    pub(super) fn plugin_active(&self, plugin_id: &str) -> usize {
        self.pools
            .values()
            .filter(|pool| pool.plugin_id == plugin_id)
            .map(|pool| pool.active)
            .sum()
    }

    /// Count exact retained session identities through their non-removable owning pools.
    /// 经由不可移除的所属池统计精确保留会话身份。
    pub(super) fn plugin_sessions(&self, plugin_id: &str) -> usize {
        self.sessions
            .values()
            .filter(|session| {
                self.pools
                    .get(&session.pool_id)
                    .expect("retained session owns a retained pool")
                    .plugin_id
                    == plugin_id
            })
            .count()
    }

    /// Compute conservative committed residents, optionally allowing `requesting_pool` to consume its own guarantee.
    /// 计算保守常驻承诺；可选允许 `requesting_pool` 消费自身保证。
    /// Actual retirement may reduce counts while observed, but allocation is serialized by this scheduler lock.
    /// 实际退役可能在观测期间降低计数，但新分配由此调度锁串行化。
    pub(super) fn plugin_commitment(
        &self,
        plugin_id: &str,
        requesting_pool: Option<&str>,
    ) -> EmbeddedResult<usize> {
        self.plugin_commitment_with_capacity(plugin_id, requesting_pool, None)
    }

    /// Validate new domain `policy` for an explicitly registered plugin before any registration mutation.
    /// 在任何注册变更前，为显式注册插件校验新域 `policy`。
    /// Return a precise capacity or policy error; existing live generations continue to count.
    /// 返回精确容量或策略错误；既有活跃代次继续计费。
    pub(super) fn validate_plugin_pool(
        &self,
        plugin_id: &str,
        policy: &PluginPoolConfig,
    ) -> EmbeddedResult<()> {
        let plugin = self.plugins.get(plugin_id).ok_or_else(plugin_not_found)?;
        if plugin.closing {
            return Err(closed());
        }
        plugin.config.validate_pool(policy)?;
        if self
            .pools
            .values()
            .filter(|pool| pool.plugin_id == plugin_id)
            .count()
            >= plugin.config.max_registered_pools
        {
            return Err(capacity("plugin retained pool capacity reached"));
        }
        let committed = self.plugin_commitment(plugin_id, None)?;
        if policy.min_resident_vms > plugin.config.max_resident_vms.saturating_sub(committed) {
            return Err(capacity(
                "dedicated reservations exceed plugin resident capacity",
            ));
        }
        Ok(())
    }

    /// Return whether one new resident for exact `pool_id` fits its plugin's aggregate commitment.
    /// 返回精确 `pool_id` 的一个新常驻实例是否符合所属插件聚合承诺。
    pub(super) fn plugin_allows_allocation(&self, pool_id: &str) -> EmbeddedResult<bool> {
        let pool = self.pools.get(pool_id).ok_or_else(not_found)?;
        let plugin = self
            .plugins
            .get(&pool.plugin_id)
            .ok_or_else(plugin_not_found)?;
        Ok(!plugin.closing
            && self.plugin_commitment(&pool.plugin_id, Some(pool_id))?
                < plugin.config.max_resident_vms)
    }
}

/// Produce a stable missing-plugin error without attempting implicit registration.
/// 产生稳定的插件缺失错误，不尝试隐式注册。
fn plugin_not_found() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::NotFound,
        "embedded plugin is not registered",
    )
}

/// Return a bounded capacity diagnostic for the supplied static `message`.
/// 为传入的静态 `message` 返回有界容量诊断。
fn capacity(message: &str) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::CapacityExceeded, message)
}
