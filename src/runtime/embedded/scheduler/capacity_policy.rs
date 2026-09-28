//! Explicit capacity policy revisions preserve admitted work and real physical ownership.
//! 显式容量策略修订保留已入场工作及真实物理归属。

use super::*;

/// One atomic policy revision and its actual convergence state for an exact capacity.
/// 单个精确容量的原子策略修订及其实际收敛状态。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedCapacityPolicySnapshot {
    /// Opaque predecessor token for compare-and-swap; clients must echo it without numeric conversion.
    /// 比较交换使用的不透明前驱令牌；客户端必须原样回传，不得数值转换。
    pub revision: String,
    /// Current policy and physical/scheduling ownership sampled under the same scheduler gate.
    /// 在同一调度门下采样的当前策略及物理／调度归属。
    pub capacity: EmbeddedCapacitySnapshot,
    /// Actual usage exceeds at least one current limit; existing work is still allowed to drain.
    /// 实际用量超过至少一个当前上限；既有工作仍允许排空。
    pub pending_convergence: bool,
}

impl EmbeddedRuntime {
    /// Read exact capacity id's current revision, policy and convergence together, or return not found.
    /// 一并读取精确容量标识的当前修订、策略及收敛状态，或返回未找到。
    pub fn capacity_policy(&self, id: &str) -> EmbeddedResult<EmbeddedCapacityPolicySnapshot> {
        // One scheduling gate prevents a revision from separating the token from its actual policy.
        // 同一调度门防止修订使令牌与实际策略脱离。
        let state = self.center.lock()?;
        // Query the original physical owner while scheduling ownership cannot change.
        // 在调度归属不能变化时查询原物理所有者。
        let capacity = state.capacity_snapshot(id, &self.center.pools)?;
        // Existing permits and queues are grandfathered, never removed to manufacture convergence.
        // 既有许可及队列保留，绝不通过移除它们伪造收敛。
        let pending_convergence = capacity.resources.resident
            > capacity.config.resources.max_resident_vms
            || capacity.resources.running > capacity.config.resources.max_running_calls
            || capacity.active_operations > capacity.config.resources.max_running_calls
            || capacity.queued_calls > capacity.config.max_queued_calls
            || capacity.queued_bytes > capacity.config.max_queued_bytes;
        Ok(EmbeddedCapacityPolicySnapshot {
            revision: state
                .capacities
                .get(id)
                .expect("snapshot retains capacity")
                .policy_revision
                .to_string(),
            capacity,
            pending_convergence,
        })
    }

    /// Replace id's full config only if expected_revision is current; return the committed opaque token.
    /// 仅当 expected_revision 为当前值时替换 id 的完整配置；返回已提交的不透明令牌。
    /// Reject conflicts before any mutation; preserve admitted calls, fixed sessions and immutable module declarations.
    /// 任何变更前拒绝冲突；保留已入场调用、固定会话及不可变模块声明。
    pub fn revise_capacity(
        &self,
        id: &str,
        expected_revision: &str,
        config: EmbeddedCapacityConfig,
    ) -> EmbeddedResult<String> {
        // This gate serializes registrations, revision publication, queue admission and shutdown.
        // 此门串行化注册、修订发布、队列入场及关闭。
        let mut state = self.center.lock()?;
        // Exact capacity ownership is never replaced or inferred from a mutable plugin name.
        // 精确容量归属绝不被替换，也不通过可变插件名推断。
        let capacity = state.capacities.get(id).ok_or_else(not_found)?;
        // The original plugin retains aggregate authority over every capacity revision.
        // 原插件对全部容量修订保留聚合权威。
        let plugin = state
            .plugins
            .get(&capacity.plugin_id)
            .expect("capacity retains plugin");
        if state.closing || capacity.closing || plugin.closing {
            return Err(closed());
        }
        if capacity.policy_revision.to_string() != expected_revision {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "capacity policy revision conflict",
            ));
        }
        config.validate(self.center.pools.config(), &plugin.config)?;
        if capacity.config == config {
            return Ok(capacity.policy_revision.to_string());
        }
        // Only a resident shrink requires retiring existing caches; growth and queue edits keep warm state.
        // 仅常驻缩容需要退役既有缓存；扩容及队列编辑保留预热状态。
        let retire_cached =
            config.resources.max_resident_vms < capacity.config.resources.max_resident_vms;
        // A dispatched worker may not have acquired its physical permit yet; never invalidate that admission.
        // 已分发工作线程可能尚未取得物理许可；绝不使该入场失效。
        if config.resources.max_running_calls < capacity.config.resources.max_running_calls
            && state.capacity_active(id) > config.resources.max_running_calls
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "dispatched operations must drain before lowering the execution limit",
            ));
        }
        // Recompute with the proposed floor instead of subtracting differently timed usage snapshots.
        // 使用拟定下限重新计算，不对采样时间不同的用量快照做减法。
        let commitment = state.plugin_commitment_with_capacity(
            &capacity.plugin_id,
            None,
            Some((id, config.resources.min_resident_vms)),
        )?;
        if commitment > plugin.config.max_resident_vms {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "revised reservations exceed plugin resident budget",
            ));
        }
        // Check token exhaustion before physical publication; no fallible policy operation follows it.
        // 物理发布前检查令牌耗尽；发布后不再执行可能失败的策略操作。
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal("capacity policy revision exhausted"))?;
        self.center
            .pools
            .revise_capacity(id, config.resources.clone())?;
        state.sequence = sequence;
        // Retain the original identity and update the complete scheduler policy at the same linearization point.
        // 保留原身份，并在同一线性化点更新完整调度策略。
        let capacity = state
            .capacities
            .get_mut(id)
            .expect("capacity retained under revision gate");
        capacity.config = config;
        capacity.policy_revision = sequence;
        if retire_cached {
            // Invalidate the old reusable cache without cancelling active work or touching fixed sessions.
            // 使旧复用缓存失效，不取消活动工作，也不触碰固定会话。
            let members = state
                .pools
                .iter()
                .filter(|(_, pool)| pool.capacity_id.as_deref() == Some(id))
                .map(|(pool_id, _)| pool_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            for instance in state.reusable_instances.values_mut() {
                if members.contains(&instance.pool_id) {
                    instance.closing = true;
                }
            }
        }
        self.center.changed.notify_all();
        Ok(sequence.to_string())
    }
}

impl SchedulerState {
    /// Snapshot exact id's current capacity using pools as the sole physical authority; caller holds the scheduler gate.
    /// 使用 pools 作为唯一物理权威快照精确 id 的当前容量；调用方持有调度门。
    pub(super) fn capacity_snapshot(
        &self,
        id: &str,
        pools: &EmbeddedPoolManager,
    ) -> EmbeddedResult<EmbeddedCapacitySnapshot> {
        // Exact retained capacity ownership is never resolved through a fallback.
        // 精确保留容量归属绝不通过回退解析。
        let capacity = self.capacities.get(id).ok_or_else(not_found)?;
        // Physical governor snapshot sampled under retained scheduling ownership.
        // 在保留调度归属期间采样的物理治理器快照。
        let physical = pools.capacity(id)?;
        // Counts and bytes are derived together from the authoritative request queues.
        // 数量和字节共同从权威请求队列派生。
        let (queued_calls, queued_bytes) = self.capacity_queue(id);
        Ok(EmbeddedCapacitySnapshot {
            capacity_id: id.to_owned(),
            plugin_id: capacity.plugin_id.clone(),
            config: capacity.config.clone(),
            resources: physical.resources,
            committed_resident_vms: physical.committed_resident_vms,
            active_operations: self.capacity_active(id),
            queued_calls,
            queued_bytes,
            retained_pools: self
                .pools
                .values()
                .filter(|pool| pool.capacity_id.as_deref() == Some(id))
                .count(),
            closing: capacity.closing
                || self.closing
                || self
                    .plugins
                    .get(&capacity.plugin_id)
                    .expect("capacity retains plugin")
                    .closing,
        })
    }

    /// Recompute plugin commitment with an optional exact capacity floor replacement; return a conservative charge.
    /// 使用可选精确容量下限替换重新计算插件承诺；返回保守计费值。
    /// requesting_pool may consume its own guarantee; releases can only decrease samples while this gate excludes allocations.
    /// requesting_pool 可消费自身保证；此门排除分配时，释放仅能使采样值减少。
    pub(super) fn plugin_commitment_with_capacity(
        &self,
        plugin_id: &str,
        requesting_pool: Option<&str>,
        replacement: Option<(&str, usize)>,
    ) -> EmbeddedResult<usize> {
        // Only the requesting pool's declared owner may spend its unused guarantee.
        // 只有请求池已声明的所有者可以消费其未使用保证。
        let requesting_capacity = requesting_pool
            .and_then(|id| self.pools.get(id))
            .and_then(|pool| pool.capacity_id.as_deref());
        // Independent domains retain their original commitment and release semantics.
        // 独立域保留原承诺及释放语义。
        let mut committed = 0usize;
        for (id, pool) in self
            .pools
            .iter()
            .filter(|(_, pool)| pool.plugin_id == plugin_id && pool.capacity_id.is_none())
        {
            // A closing physical registration may already have returned its unused independent guarantee.
            // 关闭中的物理注册可能已经归还其未使用独立保证。
            let (usage, guarantee) = pool.pool.accounting()?;
            // The requester spends only its own still-unused independent guarantee.
            // 请求方仅消费自身尚未使用的独立保证。
            let charge = if requesting_pool == Some(id.as_str()) {
                usage.resident
            } else {
                guarantee
            };
            committed = committed
                .checked_add(charge)
                .ok_or_else(|| internal("plugin commitment overflow"))?;
        }
        for (id, capacity) in self
            .capacities
            .iter()
            .filter(|(_, capacity)| capacity.plugin_id == plugin_id)
        {
            // Aggregate commitment is charged once even when the capacity currently has no members.
            // 即使容量当前没有成员，聚合承诺也仅计费一次。
            let resident = self.capacity_resident(id)?;
            // One aggregate guarantee remains charged across every member generation.
            // 单个聚合保证跨全部成员代次保持计费。
            let charge = if requesting_capacity == Some(id.as_str()) {
                resident
            } else {
                resident.max(match replacement {
                    Some((replaced, minimum)) if replaced == id => minimum,
                    _ => capacity.config.resources.min_resident_vms,
                })
            };
            committed = committed
                .checked_add(charge)
                .ok_or_else(|| internal("plugin commitment overflow"))?;
        }
        Ok(committed)
    }
}
