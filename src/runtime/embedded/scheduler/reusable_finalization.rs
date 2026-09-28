//! Convert exact reusable-instance closing reservations into independently queryable lifecycle operations.
//! 将精确可复用实例的关闭预留转换为可独立查询的生命周期操作。

use super::*;

/// Schedule the retained instance's closing export under its original pool and plugin limits.
/// 在原池及插件上限下，调度保留实例的关闭导出。
/// Return false while execution admission is occupied; preserve the VM and its reserved slot for a later scan.
/// 执行入场被占用时返回假；为后续扫描保留 VM 及其预留槽。
pub(super) fn schedule(
    center: &SchedulerCenter,
    state: &mut SchedulerState,
    instance_id: &str,
) -> EmbeddedResult<bool> {
    let instance = state
        .reusable_instances
        .get(instance_id)
        .expect("closing reusable instance retained");
    let pool = state
        .pools
        .get(&instance.pool_id)
        .expect("reusable pool retained");
    let plugin_id = pool.plugin_id.clone();
    let plugin = state
        .plugins
        .get(&plugin_id)
        .expect("reusable plugin retained");
    if pool.active >= pool.pool.policy().max_running_calls
        || state.plugin_active(&plugin_id) >= plugin.config.max_running_calls
        || !state.capacity_allows_execution(&instance.pool_id)?
    {
        return Ok(false);
    }
    let instance = state
        .reusable_instances
        .get_mut(instance_id)
        .expect("closing reusable instance retained");
    let plan = instance
        .lease
        .as_ref()
        .expect("closing reusable lease")
        .finalization_plan()
        .expect("eligible reusable closing");
    // The context and retention slot were validated before this VM could initialize.
    // 此 VM 可初始化前已校验上下文及保留槽。
    let control = Arc::new(CallControl::new(Duration::from_millis(plan.timeout_ms))?);
    let (handle, owner) = center.operations.admit_reserved(
        instance
            .finalization_reservation
            .as_mut()
            .expect("reusable closing capacity reserved"),
        Arc::clone(&control),
    )?;
    instance.finalization_reservation.take();
    let id = handle.id().to_owned();
    let pool_id = instance.pool_id.clone();
    let request = ScheduledRequest::CloseInstance {
        pool_id: pool_id.clone(),
        context: std::mem::take(&mut instance.finalization_context),
    };
    let lease = instance.lease.take().expect("original reusable VM owned");
    instance.active = Some(id.clone());
    let plugin = state
        .plugins
        .get_mut(&plugin_id)
        .expect("reusable plugin retained");
    plugin.reserved_operations -= 1;
    plugin.operations += 1;
    state.operation_plugins.insert(id.clone(), plugin_id);
    state.live.insert(id.clone(), Arc::clone(&control));
    state
        .pools
        .get_mut(&pool_id)
        .expect("reusable pool retained")
        .active += 1;
    state.cleaning_count += 1;
    state.cleaning.push(PendingCompletion {
        finalization: Some(PendingFinalization::new(*lease, plan)),
        retained_lease: None,
        cleaning_started: false,
        call: ScheduledCall {
            reusable_instance: Some(instance_id.to_owned()),
            id,
            owner,
            control,
            request,
            bytes: 0,
        },
        // This independent lifecycle operation has no business export; its explicit baseline is successful null.
        // 此独立生命周期操作没有业务导出；其显式基线为成功空值。
        result: Ok(Value::Null),
        retirement: None,
        effects: EffectState::Unknown,
        dispatched: true,
    });
    center.changed.notify_all();
    Ok(true)
}
