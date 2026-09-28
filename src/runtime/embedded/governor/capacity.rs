//! Persistent capacity registration binds multiple isolated module groups to one physical allowance.
//! 持久容量注册将多个隔离模块分组绑定至同一物理额度。

use super::*;

/// One atomic view of a capacity owner, including empty reservations and every member's real VM state.
/// 容量所有者的单次原子视图，包含空预留及全部成员的实际 VM 状态。
#[derive(Debug, Clone, Serialize)]
pub struct VmCapacitySnapshot {
    /// Exact currently published physical policy of the trusted owner.
    /// 可信所有者当前已发布的精确物理策略。
    pub config: VmCapacityConfig,
    /// Actual resident and execution state, never inflated to represent unused guarantees.
    /// 实际常驻及执行状态，绝不为表示未用保证而虚增。
    pub resources: PoolUsage,
    /// Actual resident slots or the reserved minimum, whichever is larger.
    /// 实际常驻槽位与预留最小值中的较大者。
    pub committed_resident_vms: usize,
    /// Registered member identities, including empty members that still prevent owner removal.
    /// 已注册成员身份，包含仍阻止所有者移除的空成员。
    pub registered_groups: usize,
}

impl PoolGovernor {
    /// Replace the exact capacity's physical limits after checking the complete parent commitment.
    /// 检查完整父级承诺后，替换精确容量的物理限制。
    /// The formal scheduler serializes policy revisions; existing allocations and permits remain owned.
    /// 正式调度器串行化策略修订；既有分配及许可保持归属。
    pub(crate) fn revise_capacity(&self, id: &str, config: VmCapacityConfig) -> EmbeddedResult<()> {
        config.validate(&self.config)?;
        // Validate replacement and publish it inside the allocation authority's original lock.
        // 在分配权威的原锁内校验替换并发布。
        let mut state = self.lock()?;
        // Missing ownership is never implicitly recreated by a revision.
        // 修订绝不隐式重建缺失归属。
        let previous = state
            .capacities
            .get(id)
            .ok_or_else(|| not_found("capacity identity does not exist"))?;
        // Existing real residents stay charged even when the new maximum is smaller.
        // 即使新上限更小，既有真实常驻仍保持计费。
        let resident = state.capacity_usage(id).resident;
        // The same lock makes subtraction exact; no concurrent teardown can change either sample.
        // 同一锁使减法精确；并发清理不能改变任一样本。
        let committed = state
            .commitment(None)?
            .checked_sub(resident.max(previous.min_resident_vms))
            .and_then(|other| other.checked_add(resident.max(config.min_resident_vms)))
            .ok_or_else(|| EmbeddedError::invalid("resident reservation overflow"))?;
        if committed > self.config.max_resident_vms {
            return Err(capacity(
                "revised reservations exceed available parent capacity",
            ));
        }
        state.capacities.insert(id.to_owned(), config);
        Ok(())
    }

    /// Register capacity with exact id and policy; return an error if its guarantee cannot be honored.
    /// 使用精确标识及策略注册容量；无法兑现其保证时返回错误。
    /// Registration is independent of module creation and does not initialize any Lua state.
    /// 注册独立于模块创建，不初始化任何 Lua 状态。
    pub fn register_capacity(&self, id: &str, config: VmCapacityConfig) -> EmbeddedResult<()> {
        if id.trim().is_empty() {
            return Err(EmbeddedError::invalid("capacity identity must be nonempty"));
        }
        config.validate(&self.config)?;
        // Validate and commit the unused guarantee in the same transaction as all VM admission.
        // 在与全部 VM 入场相同的事务内校验并提交未使用保证。
        let mut state = self.lock()?;
        if state.capacities.contains_key(id) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "capacity identity is already registered",
            ));
        }
        if state.capacities.len() >= self.config.max_registered_pools {
            return Err(capacity("registered capacity limit reached"));
        }
        if config.min_resident_vms
            > self
                .config
                .max_resident_vms
                .saturating_sub(state.commitment(None)?)
        {
            return Err(capacity(
                "dedicated reservations exceed available parent capacity",
            ));
        }
        state.capacities.insert(id.to_owned(), config);
        Ok(())
    }

    /// Remove capacity only after every member identity was removed; return a busy error otherwise.
    /// 仅在全部成员身份已移除后移除容量；否则返回忙碌错误。
    /// Empty but still-registered modules retain their selected policy and cannot be silently reassigned.
    /// 仍注册的空模块保留其选定策略，不能被静默重新分配。
    pub fn unregister_capacity(&self, id: &str) -> EmbeddedResult<()> {
        // Serialize removal against member registration and actual allocation.
        // 将移除与成员注册及实际分配串行化。
        let mut state = self.lock()?;
        if !state.capacities.contains_key(id) {
            return Err(not_found("capacity identity does not exist"));
        }
        if state.memberships.values().any(|owner| owner == id) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "capacity still owns registered module groups",
            ));
        }
        state.capacities.remove(id);
        Ok(())
    }

    /// Register isolated module group under existing capacity with its exact policy; return any conflict.
    /// 使用精确策略在既有容量下注册隔离模块分组；返回任何冲突。
    /// Members must declare zero local reservation and the same ownership kind, without expanding group limits.
    /// 成员必须声明零局部预留及相同归属类别，且不得扩大容量组限制。
    pub fn register_group_in_capacity(
        &self,
        group: &str,
        policy: PluginPoolConfig,
        capacity_id: &str,
    ) -> EmbeddedResult<()> {
        self.register_group_internal(group, policy, Some(capacity_id))
    }

    /// Return an atomic snapshot for exact capacity id, or not found without creating any owner.
    /// 返回精确容量标识的原子快照，或返回未找到且不创建任何所有者。
    pub fn capacity(&self, id: &str) -> EmbeddedResult<VmCapacitySnapshot> {
        // The same lock protects physical usage, membership and the current policy.
        // 同一锁保护物理用量、成员关系及当前策略。
        let state = self.lock()?;
        // Absence is an explicit lookup failure, not an inherited default.
        // 缺失是明确查找失败，不是继承默认值。
        let config = state
            .capacities
            .get(id)
            .ok_or_else(|| not_found("capacity identity does not exist"))?;
        // Counters are derived from the actual allocation ledger, without a second mutable total.
        // 计数从实际分配账本派生，不维护第二个可变总数。
        let resources = state.capacity_usage(id);
        Ok(VmCapacitySnapshot {
            committed_resident_vms: resources.resident.max(config.min_resident_vms),
            resources,
            config: config.clone(),
            registered_groups: state
                .memberships
                .values()
                .filter(|owner| owner.as_str() == id)
                .count(),
        })
    }

    /// Publish one module policy with optional explicit capacity; return without mutations on validation failure.
    /// 发布单个模块策略及可选显式容量；校验失败时返回且不修改状态。
    pub(super) fn register_group_internal(
        &self,
        group: &str,
        policy: PluginPoolConfig,
        capacity_id: Option<&str>,
    ) -> EmbeddedResult<()> {
        if group.trim().is_empty() {
            return Err(EmbeddedError::invalid("execution group must be nonempty"));
        }
        policy.validate(&self.config)?;
        // All validation and publication share the original governor metadata transaction.
        // 全部校验及发布共享原治理器元数据事务。
        let mut state = self.lock()?;
        if state.groups.contains_key(group) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "execution group is already registered",
            ));
        }
        if state.groups.len() >= self.config.max_registered_pools {
            return Err(capacity("registered pool limit reached"));
        }
        if let Some(id) = capacity_id {
            // Explicit membership must resolve exactly; missing capacity cannot become an independent pool.
            // 显式成员关系必须精确解析；缺失容量不能转成独立池。
            let capacity = state
                .capacities
                .get(id)
                .ok_or_else(|| not_found("capacity identity does not exist"))?;
            if policy.kind != capacity.kind
                || policy.min_resident_vms != 0
                || policy.max_resident_vms > capacity.max_resident_vms
                || policy.max_running_calls > capacity.max_running_calls
            {
                return Err(EmbeddedError::invalid(
                    "module policy conflicts with its capacity owner",
                ));
            }
        } else if policy.min_resident_vms
            > self
                .config
                .max_resident_vms
                .saturating_sub(state.commitment(None)?)
        {
            return Err(capacity(
                "dedicated reservations exceed available parent capacity",
            ));
        }
        // Publish membership only after every fallible admission check, without normalizing the policy.
        // 仅在全部可能失败的入场检查后发布成员关系，不归一化策略。
        state.groups.insert(group.to_owned(), policy);
        if let Some(id) = capacity_id {
            state.memberships.insert(group.to_owned(), id.to_owned());
        }
        Ok(())
    }
}

impl GovernorState {
    /// Sum real allocations for one exact capacity id across immutable member mappings.
    /// 根据不可变成员映射，对单个精确容量标识汇总实际分配。
    /// Return all phases, including pending construction and unsuccessful retirement.
    /// 返回全部阶段，包含待构造及尚未成功退役。
    pub(super) fn capacity_usage(&self, id: &str) -> PoolUsage {
        count_allocations(self.allocations.values().filter(|allocation| {
            self.memberships
                .get(&allocation.group)
                .is_some_and(|owner| owner == id)
        }))
    }

    /// Return total committed parent slots; a requesting module may consume its own owner's unused guarantee.
    /// 返回父级总承诺槽位；请求模块可以消费其自身所有者未使用保证。
    /// A None request is used for new guarantees, so every existing reservation remains fully charged.
    /// 空请求用于新增保证，因此全部既有预留仍完整计费。
    pub(super) fn commitment(&self, requesting_group: Option<&str>) -> EmbeddedResult<usize> {
        // Resolve only declared membership; an independent module has no aggregate capacity owner.
        // 仅解析已声明成员关系；独立模块没有聚合容量所有者。
        let requesting_capacity = requesting_group.and_then(|group| self.memberships.get(group));
        // Independent pools retain their original reservation semantics.
        // 独立池保留原预留语义。
        let mut committed = 0usize;
        for (group, policy) in &self.groups {
            if self.memberships.contains_key(group) {
                continue;
            }
            // The requesting independent pool consumes its own reservation.
            // 请求的独立池消费自身预留。
            let resident = group_usage(self, group).resident;
            let charge = if requesting_group == Some(group.as_str()) {
                resident
            } else {
                resident.max(policy.min_resident_vms)
            };
            committed = committed
                .checked_add(charge)
                .ok_or_else(|| EmbeddedError::invalid("resident reservation overflow"))?;
        }
        for (id, config) in &self.capacities {
            // Charge an aggregate guarantee once, including when it temporarily has no module members.
            // 聚合保证仅计费一次，包含暂时没有模块成员的情况。
            let resident = self.capacity_usage(id).resident;
            let charge = if requesting_capacity == Some(id) {
                resident
            } else {
                resident.max(config.min_resident_vms)
            };
            committed = committed
                .checked_add(charge)
                .ok_or_else(|| EmbeddedError::invalid("resident reservation overflow"))?;
        }
        Ok(committed)
    }
}
