//! Session closing operations consume capacity reserved before initialization.
//! 会话关闭操作消费初始化之前预留的容量。

use super::*;

/// Schedule one eligible session finalizer under ordinary pool/plugin execution limits.
/// 在普通池和插件执行上限下调度一个符合条件的会话关闭回调。
/// Return false when execution capacity is occupied; retain the original VM and reservation.
/// 执行容量被占用时返回假；继续保留原 VM 与预留。
pub(super) fn schedule(
    center: &SchedulerCenter,
    state: &mut SchedulerState,
    session_id: &str,
) -> EmbeddedResult<bool> {
    let session = state
        .sessions
        .get(session_id)
        .expect("closing session retained");
    let pool = state
        .pools
        .get(&session.pool_id)
        .expect("session pool retained");
    let plugin_id = pool.plugin_id.clone();
    let plugin = state
        .plugins
        .get(&plugin_id)
        .expect("session plugin retained");
    if pool.active >= pool.pool.policy().max_running_calls
        || state.plugin_active(&plugin_id) >= plugin.config.max_running_calls
    {
        return Ok(false);
    }
    let session = state
        .sessions
        .get_mut(session_id)
        .expect("closing session retained");
    let plan = session
        .lease
        .as_ref()
        .expect("closing lease retained")
        .finalization_plan()
        .expect("closing lease is eligible");
    // Admission cannot lose a capacity race; immutable context was validated before initialization.
    // 入场不会在容量竞争中失败；不可变上下文已在初始化前校验。
    let control = Arc::new(CallControl::new(Duration::from_millis(plan.timeout_ms))?);
    let (handle, owner) = center.operations.admit_reserved(
        session
            .finalization_reservation
            .as_mut()
            .expect("closing capacity reserved"),
        Arc::clone(&control),
    )?;
    session.finalization_reservation.take();
    let id = handle.id().to_owned();
    let pool_id = session.pool_id.clone();
    let request = ScheduledRequest::CloseSession {
        pool_id: pool_id.clone(),
        session_id: session_id.to_owned(),
        context: std::mem::take(&mut session.finalization_context),
    };
    let lease = session.lease.take().expect("closing VM owned");
    session.finalization_operation = Some(id.clone());
    session.active = Some(id.clone());
    session.unfinished += 1;
    let plugin = state
        .plugins
        .get_mut(&plugin_id)
        .expect("session plugin retained");
    plugin.reserved_operations -= 1;
    plugin.operations += 1;
    state.operation_plugins.insert(id.clone(), plugin_id);
    state.live.insert(id.clone(), Arc::clone(&control));
    state
        .pools
        .get_mut(&pool_id)
        .expect("session pool retained")
        .active += 1;
    state.cleaning_count += 1;
    state.cleaning.push(PendingCompletion {
        finalization: Some(PendingFinalization::new(*lease, plan)),
        retained_lease: None,
        cleaning_started: false,
        call: ScheduledCall {
            reusable_instance: None,
            id,
            owner,
            control,
            request,
            bytes: 0,
        },
        // No business export runs in this operation; null is the explicit lifecycle baseline.
        // 此操作不运行业务导出；空值是显式生命周期基线。
        result: Ok(Value::Null),
        retirement: None,
        effects: EffectState::Unknown,
        dispatched: true,
    });
    center.changed.notify_all();
    Ok(true)
}
