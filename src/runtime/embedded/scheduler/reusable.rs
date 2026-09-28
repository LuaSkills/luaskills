//! Formal reusable leases stay owned through business evidence and cache retirement decisions.
//! 正式可复用租借持续被拥有，覆盖业务证据及缓存退役决定。

use super::*;
use std::time::Instant;

/// One exact reusable allocation; metadata never owns a second copy of its actual VM.
/// 一个精确可复用分配；元数据绝不拥有其实际 VM 的第二份副本。
pub(super) struct ScheduledReusable {
    /// Future independent closing capacity, acquired before any initialization can run.
    /// 未来独立关闭容量，在任何初始化可执行前取得。
    pub(super) finalization_reservation: Option<super::super::operations::OperationReservation>,
    /// Last actually dispatched host context, retained only when closing was declared.
    /// 最后实际分发的宿主上下文，仅声明关闭时保留。
    pub(super) finalization_context: LuaInvocationContext,
    /// Immutable registered execution domain.
    /// 不可变已注册执行域。
    pub(super) pool_id: String,
    /// Idle exclusive lease; absent while one operation or the retirement supervisor owns it.
    /// 空闲独占租借；一个操作或退役监督器拥有它时省略。
    pub(super) lease: Option<Box<ModuleLease>>,
    /// Business operation retaining the exact VM through terminal publication.
    /// 持续拥有精确 VM 到终态发布的业务操作。
    pub(super) active: Option<String>,
    /// Permanent retirement request; the allocation cannot return to reusable admission.
    /// 永久退役请求；分配不能重新进入复用入场。
    pub(super) closing: bool,
    /// Last acknowledged business completion, used only by the explicitly declared idle TTL.
    /// 最后已确认业务完成时刻，仅用于明确声明的空闲期限。
    pub(super) idle_since: Instant,
    /// Real retirement evidence retained independently of its original business operation.
    /// 独立于原业务操作保留的真实退役证据。
    retirement: Option<ModuleRetirement>,
    /// The supervisor temporarily owns the lease outside the scheduler metadata lock.
    /// 监督器在调度元数据锁之外临时拥有租借。
    retiring: bool,
}

/// Claim idle state or reserve a new VM for exact pool and operation identities under the original control.
/// 在原控制下，为精确池和操作身份认领空闲状态或预留新 VM。
/// Return exclusive ownership with closing context, or none for physical pressure; reservation errors reject before initialization.
/// 返回独占所有权及关闭上下文；物理压力返回空值，预留错误在初始化前拒绝。
pub(super) fn prepare(
    center: &SchedulerCenter,
    state: &mut SchedulerState,
    pool_id: &str,
    operation_id: &str,
    context: &LuaInvocationContext,
    control: &CallControl,
) -> EmbeddedResult<Option<(ModuleLease, String)>> {
    control.check()?;
    expire(state, pool_id);
    // Exactly one owner decides whether confirmed state is reusable.
    // 恰好一个所有者决定已确认状态是否可复用。
    if let Some((id, instance)) = state.reusable_instances.iter_mut().find(|(_, instance)| {
        instance.pool_id == pool_id
            && !instance.closing
            && instance.active.is_none()
            && instance.lease.is_some()
    }) {
        if instance.finalization_reservation.is_some() {
            instance.finalization_context = context.clone();
        }
        instance.active = Some(operation_id.to_owned());
        return Ok(Some((
            *instance.lease.take().expect("idle reusable lease"),
            id.clone(),
        )));
    }
    let allow_new = state.plugin_allows_allocation(pool_id)?;
    let pool = state.pools.get(pool_id).expect("registered reusable pool");
    let plugin_id = pool.plugin_id.clone();
    let plugin = state
        .plugins
        .get(&plugin_id)
        .expect("registered reusable plugin");
    if pool.pool.finalizer().is_some()
        && plugin.operations >= plugin.config.max_operations - plugin.reserved_operations
    {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::CapacityExceeded,
            "plugin operation capacity cannot reserve reusable finalization",
        ));
    }
    let lease = match pool.pool.prepare_with_budget(control, true, allow_new) {
        Ok(lease) => lease,
        Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded => return Ok(None),
        Err(error) => return Err(error),
    };
    let id = lease.allocation_id()?.to_owned();
    // This preparation has executed no source; failed reservation releases only uninitialized capacity.
    // 此准备尚未执行源码；预留失败仅释放未初始化容量。
    let finalization_reservation = match pool.pool.finalizer() {
        Some(plan) => Some(center.operations.reserve_module(
            &pool.pool,
            None,
            &id,
            &plan.export,
        )?),
        None => None,
    };
    let finalization_context = if finalization_reservation.is_some() {
        state
            .plugins
            .get_mut(&plugin_id)
            .expect("registered reusable plugin")
            .reserved_operations += 1;
        context.clone()
    } else {
        LuaInvocationContext::default()
    };
    state.reusable_instances.insert(
        id.clone(),
        ScheduledReusable {
            pool_id: pool_id.to_owned(),
            lease: None,
            active: Some(operation_id.to_owned()),
            closing: false,
            idle_since: Instant::now(),
            retirement: None,
            retiring: false,
            finalization_reservation,
            finalization_context,
        },
    );
    Ok(Some((lease, id)))
}

/// Mark expired idle leases without destroying them or consuming the domain's warm minimum.
/// 标记过期空闲租借，不销毁它们，也不消耗执行域的最小预热数量。
/// The supervisor later transfers marked ownership into actual retirement outside metadata locks.
/// 监督器随后在元数据锁外将已标记所有权转入真实退役。
fn expire(state: &mut SchedulerState, pool_id: &str) {
    let policy = state
        .pools
        .get(pool_id)
        .expect("reusable pool retained")
        .pool
        .policy();
    let Some(ttl) = policy.idle_ttl_ms.map(Duration::from_millis) else {
        return;
    };
    let idle = state
        .reusable_instances
        .values()
        .filter(|instance| {
            instance.pool_id == pool_id
                && !instance.closing
                && instance.active.is_none()
                && instance.lease.is_some()
        })
        .count();
    let mut removable = idle.saturating_sub(policy.min_resident_vms);
    for instance in state
        .reusable_instances
        .values_mut()
        .filter(|instance| instance.pool_id == pool_id)
    {
        if removable != 0
            && !instance.closing
            && instance.active.is_none()
            && instance.lease.is_some()
            && instance.idle_since.elapsed() >= ttl
        {
            instance.closing = true;
            removable -= 1;
        }
    }
}

/// Request one eligible idle eviction for global pressure or the explicitly constrained plugin.
/// 为全局容量压力或明确受限插件请求一次符合条件的空闲驱逐。
/// Return whether ownership was marked; actual capacity remains charged until the retirement receipt completes.
/// 返回是否已标记所有权；实际容量持续记账到退役回执完成。
pub(super) fn reclaim(state: &mut SchedulerState, plugin_id: Option<&str>) -> bool {
    for (pool_id, pool) in &state.pools {
        if plugin_id.is_some_and(|plugin| pool.plugin_id != plugin) {
            continue;
        }
        let idle = state
            .reusable_instances
            .values()
            .filter(|instance| {
                instance.pool_id == *pool_id
                    && !instance.closing
                    && instance.active.is_none()
                    && instance.lease.is_some()
            })
            .count();
        if idle <= pool.pool.policy().min_resident_vms {
            continue;
        }
        if let Some(instance) = state.reusable_instances.values_mut().find(|instance| {
            instance.pool_id == *pool_id
                && !instance.closing
                && instance.active.is_none()
                && instance.lease.is_some()
        }) {
            instance.closing = true;
            return true;
        }
    }
    false
}

/// Drain closed and expired idle ownership while preserving real VM retirement evidence.
/// 排空已关闭及过期的空闲所有权，同时保留真实 VM 退役证据。
/// Return infrastructure failures without pretending that still-owned physical resources disappeared.
/// 返回基础设施故障，不假装仍被拥有的物理资源已经消失。
pub(super) fn maintain(center: &SchedulerCenter) -> EmbeddedResult<()> {
    let retiring = {
        let mut state = center.lock()?;
        let pools = state.pools.keys().cloned().collect::<Vec<_>>();
        for pool_id in pools {
            expire(&mut state, &pool_id);
        }
        let closing_pools = state
            .pools
            .iter()
            .filter(|(_, pool)| pool.closed)
            .map(|(id, _)| id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let closing = state.closing;
        let mut retiring = Vec::new();
        let mut finalizing = Vec::new();
        for (id, instance) in &mut state.reusable_instances {
            instance.closing |= closing || closing_pools.contains(&instance.pool_id);
            if !instance.closing {
                continue;
            }
            // Pool generation closure drains admitted calls; only explicit cancellation changes their control.
            // 池代次关闭排空已入场调用；仅显式取消改变其控制。
            if instance.active.is_none()
                && let Some(lease) = &instance.lease
            {
                if lease.finalization_plan().is_some() {
                    finalizing.push(id.clone());
                } else {
                    instance.retiring = true;
                    retiring.push((
                        id.clone(),
                        instance.lease.take().expect("idle reusable lease"),
                    ));
                }
            }
        }
        for id in finalizing {
            reusable_finalization::schedule(center, &mut state, &id)?;
        }
        retiring
    };
    for (id, lease) in retiring {
        let release = lease.finish()?;
        let mut state = center.lock()?;
        let instance = state
            .reusable_instances
            .get_mut(&id)
            .expect("retiring reusable instance");
        instance.retirement = match release {
            ModuleRelease::Retiring(receipt) => Some(receipt),
            ModuleRelease::NoInstance => None,
            ModuleRelease::ReturnedToPool => {
                return Err(internal(
                    "scheduler-owned reusable lease returned to physical cache",
                ));
            }
        };
        instance.retiring = false;
    }
    let mut state = center.lock()?;
    prune(&mut state)
}

/// Remove only cache metadata whose exclusive lease and real retirement receipt have drained.
/// 仅移除独占租借及真实退役回执均已排空的缓存元数据。
/// Allow explicit pool forgetting to observe completed retirement without waiting for the next scan.
/// 允许显式遗忘池立即观察已完成退役，无需等待下一次扫描。
pub(super) fn prune(state: &mut SchedulerState) -> EmbeddedResult<()> {
    let mut drained = Vec::new();
    for (id, instance) in &state.reusable_instances {
        if instance.closing
            && instance.active.is_none()
            && instance.lease.is_none()
            && !instance.retiring
            && match &instance.retirement {
                Some(receipt) => receipt.snapshot()?.phase == ModuleRetirementPhase::Completed,
                None => true,
            }
        {
            drained.push(id.clone());
        }
    }
    for id in drained {
        remove(state, &id);
    }
    Ok(())
}

/// Release drained metadata and unused closing reservations after failed initialization or completed retirement.
/// 在初始化失败或退役完成后，释放已排空元数据及未使用关闭预留。
/// The caller must already own proof that no lease or in-flight completion remains in this record.
/// 调用方必须已经拥有该记录不再保留租借或执行中完成任务的证据。
pub(super) fn remove(state: &mut SchedulerState, id: &str) {
    let instance = state
        .reusable_instances
        .remove(id)
        .expect("drained reusable instance retained");
    if instance.finalization_reservation.is_some() {
        let plugin_id = &state
            .pools
            .get(&instance.pool_id)
            .expect("reusable pool retained")
            .plugin_id;
        state
            .plugins
            .get_mut(plugin_id)
            .expect("reusable plugin retained")
            .reserved_operations -= 1;
    }
}
