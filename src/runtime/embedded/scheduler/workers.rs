use super::*;
use std::collections::BTreeSet;

/// One admitted execution or an explicit pre-execution rejection, never an implicit replay.
/// 一次已入场执行或显式执行前拒绝，绝不是隐式重放。
enum Dispatch {
    /// Prepared ownership is exclusive and does not yet contain any newly executed source.
    /// 已准备所有权独占，且尚不包含任何新执行的源码。
    Ready(ScheduledCall, Box<ModuleLease>),
    /// Metadata admission failed without running plugin code.
    /// 元数据入场失败，未运行插件代码。
    Rejected(ScheduledCall, EmbeddedError),
}

/// Remove exact queued ownership and all queue charges while preserving unrelated plugin rotation.
/// 移除精确排队所有权与全部队列记账，同时保留无关插件轮转。
fn take_call(state: &mut SchedulerState, plugin: &str, index: usize) -> ScheduledCall {
    let queue = state
        .queues
        .get_mut(plugin)
        .expect("selected plugin queue exists");
    let call = queue.remove(index).expect("selected request exists");
    if queue.is_empty() {
        state.queues.remove(plugin);
        state.rotation.retain(|candidate| candidate != plugin);
    }
    state.queued -= 1;
    state.bytes -= call.bytes;
    state
        .pools
        .get_mut(call.request.pool_id())
        .expect("queued pool is retained")
        .queued -= 1;
    if let Some(id) = call.request.session_id() {
        state
            .sessions
            .get_mut(id)
            .expect("queued session exists")
            .queued -= 1;
    }
    call
}

/// Choose one ready plugin fairly, preserving FIFO dispatch within each immutable domain.
/// 公平选择一个就绪插件，并在每个不可变域内保留先进先出分发。
/// Capacity exhaustion leaves the request queued without executing initialization or consuming its identity again.
/// 容量耗尽时请求继续排队，不执行初始化，也不再次消费其身份。
fn select(state: &mut SchedulerState) -> EmbeddedResult<Option<Dispatch>> {
    let plugins = state.rotation.iter().cloned().collect::<Vec<_>>();
    let mut reclaimed = false;
    for (rotation_index, plugin) in plugins.iter().enumerate() {
        let mut visited = BTreeSet::new();
        let queue = state
            .queues
            .get(plugin)
            .expect("rotation references a live queue");
        for (index, call) in queue.iter().enumerate() {
            // Different sessions may progress independently; each session and ordinary domain stays FIFO.
            // 不同会话可以独立推进；每个会话与普通域内部仍保持先进先出。
            if !visited.insert((
                call.request.pool_id().to_owned(),
                call.request.session_id().map(str::to_owned),
            )) {
                continue;
            }
            let pool = state
                .pools
                .get(call.request.pool_id())
                .expect("queued pool is retained");
            if pool.closed
                || call.control.check().is_err()
                || pool.active >= pool.pool.policy().max_running_calls
            {
                continue;
            }
            let prepared = if let Some(id) = call.request.session_id() {
                let session = state.sessions.get(id).expect("queued session is retained");
                if session.closing || session.active.is_some() {
                    continue;
                }
                // Session capacity was reserved at creation; its exact lease is moved only after queue removal.
                // 会话容量已在创建时预留；仅在移除队列后转移其精确租借。
                None
            } else {
                let prepared = pool.pool.prepare(&call.control);
                if matches!(&prepared, Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
                {
                    if !reclaimed {
                        for candidate in state.pools.values() {
                            if candidate.pool.retire_idle_for_pressure()? {
                                reclaimed = true;
                                break;
                            }
                        }
                    }
                    continue;
                }
                Some(prepared)
            };
            state.rotation.rotate_left(rotation_index + 1);
            let call = take_call(state, plugin, index);
            let lease = match prepared {
                Some(Ok(lease)) => Box::new(lease),
                Some(Err(error)) => return Ok(Some(Dispatch::Rejected(call, error))),
                None => {
                    let session = state
                        .sessions
                        .get_mut(
                            call.request
                                .session_id()
                                .expect("session dispatch has identity"),
                        )
                        .expect("selected session is retained");
                    let lease = session
                        .lease
                        .take()
                        .ok_or_else(|| internal("idle session lost its lease"))?;
                    session.active = Some(call.id.clone());
                    lease
                }
            };
            state
                .pools
                .get_mut(call.request.pool_id())
                .expect("prepared pool exists")
                .active += 1;
            return Ok(Some(Dispatch::Ready(call, lease)));
        }
    }
    Ok(None)
}

/// Execute one prepared call while retaining lease ownership across all returned errors and Rust panics.
/// 执行一个已准备调用，跨所有返回错误与 Rust panic 保留租借所有权。
fn invoke(
    mut call: ScheduledCall,
    mut lease: ModuleLease,
    max_value_bytes: usize,
) -> PendingCompletion {
    // Execution keeps the exact session VM and original control across initialization and host calls.
    // 执行在初始化与宿主调用之间保留精确会话 VM 与原始控制。
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        call.owner.advance(OperationPhase::Initializing)?;
        lease.initialize_for_session(Arc::clone(&call.control), call.request.session_id())?;
        if let Some(request) = call.request.invocation() {
            call.owner.advance(OperationPhase::Running)?;
            lease.invoke(ModuleInvocation {
                operation_id: &call.id,
                session_id: call.request.session_id(),
                export: &request.export,
                arguments: &request.arguments,
                context: &request.context,
                control: Arc::clone(&call.control),
            })
        } else {
            Ok(
                serde_json::json!({ "session_id": call.request.session_id().expect("creation has identity") }),
            )
        }
    }));
    let mut result = match outcome {
        Ok(result) => result,
        Err(_) => Err(internal("embedded invocation panicked")),
    };
    let retained_size = match &result {
        Ok(value) => json_size(value, max_value_bytes),
        Err(error) => json_size(error, max_value_bytes),
    };
    if let Err(error) = retained_size {
        result = Err(error);
    }
    // A successful pinned lease stays exclusive through operation evidence sealing.
    // 成功的固定租借在操作证据封存前保持独占。
    let keep_session =
        call.request.session_id().is_some() && result.is_ok() && lease.can_retain_session();
    let (session_lease, retirement, result, effects) = if keep_session {
        (Some(Box::new(lease)), None, result, EffectState::Unknown)
    } else {
        if result.is_err() {
            lease.close();
        }
        match lease.finish() {
            Ok(ModuleRelease::Retiring(receipt)) => {
                (None, Some(receipt), result, EffectState::Unknown)
            }
            Ok(ModuleRelease::ReturnedToPool) => (None, None, result, EffectState::Unknown),
            Ok(ModuleRelease::NoInstance) => (None, None, result, EffectState::NotStarted),
            Err(error) => (None, None, Err(error), EffectState::Unknown),
        }
    };
    call.request.release_values();
    PendingCompletion {
        call,
        result,
        retirement,
        effects,
        session_lease,
        dispatched: true,
        cleaning_started: false,
    }
}

/// Run one fixed executor; all VM construction, initialization and callbacks happen outside scheduler locks.
/// 运行单个固定执行器；全部 VM 构造、初始化与回调均在调度锁外发生。
pub(super) fn execute(center: &Arc<SchedulerCenter>) -> EmbeddedResult<()> {
    loop {
        let dispatch = {
            let mut state = center.lock()?;
            loop {
                if state.closing {
                    return Ok(());
                }
                if let Some(dispatch) = select(&mut state)? {
                    break dispatch;
                }
                state = center
                    .changed
                    .wait_timeout(state, MAINTENANCE_INTERVAL)
                    .map_err(|_| internal("embedded scheduler wait is poisoned"))?
                    .0;
            }
        };
        let mut completion = match dispatch {
            Dispatch::Ready(call, lease) => {
                invoke(call, *lease, center.pools.config().max_value_bytes)
            }
            Dispatch::Rejected(call, error) => PendingCompletion {
                session_lease: None,
                call,
                result: Err(error),
                retirement: None,
                effects: EffectState::NotStarted,
                dispatched: false,
                cleaning_started: false,
            },
        };
        completion.call.owner.advance(OperationPhase::Cleaning)?;
        completion.cleaning_started = true;
        {
            let mut state = center.lock()?;
            if completion.session_lease.is_none()
                && let Some(id) = completion.call.request.session_id()
            {
                // Stop admission immediately when this VM cannot be retained, even if teardown blocks.
                // 此 VM 无法保留时立即停止入场，即使清理阻塞。
                let session = state
                    .sessions
                    .get_mut(id)
                    .expect("dispatched session retained");
                session.closing = true;
                if let Err(error) = &completion.result {
                    session.error.get_or_insert_with(|| error.clone());
                }
            }
            state.cleaning_count += 1;
            state.cleaning.push(completion);
        }
        center.changed.notify_all();
    }
}

/// Remove queued cancellation, deadline and closed-generation failures independently of busy executors.
/// 独立于忙碌执行器，移除排队取消、截止时间与已关闭代次错误。
fn reject_expired(state: &mut SchedulerState) {
    let plugins = state.rotation.iter().cloned().collect::<Vec<_>>();
    for plugin in plugins {
        let mut index = 0;
        while let Some(call) = state.queues.get(&plugin).and_then(|queue| queue.get(index)) {
            let error = match call.control.check() {
                Err(error) => Some(error),
                Ok(())
                    if state.closing
                        || state
                            .pools
                            .get(call.request.pool_id())
                            .expect("queued pool exists")
                            .closed
                        || call.request.session_id().is_some_and(|id| {
                            state
                                .sessions
                                .get(id)
                                .expect("queued session exists")
                                .closing
                        }) =>
                {
                    Some(closed())
                }
                Ok(()) => None,
            };
            if let Some(error) = error {
                let call = take_call(state, &plugin, index);
                let session_lease =
                    if let ScheduledRequest::OpenSession { session_id, .. } = &call.request {
                        let session = state
                            .sessions
                            .get_mut(session_id)
                            .expect("opening session exists");
                        session.closing = true;
                        session.error.get_or_insert_with(|| error.clone());
                        session.lease.take()
                    } else {
                        None
                    };
                state.cleaning_count += 1;
                state.cleaning.push(PendingCompletion {
                    session_lease,
                    call,
                    result: Err(error),
                    retirement: None,
                    effects: EffectState::NotStarted,
                    dispatched: false,
                    cleaning_started: false,
                });
            } else {
                index += 1;
            }
        }
    }
}

/// Finalize ready cleanup evidence without releasing still-owned VM or host callback state.
/// 完成就绪清理证据，且不释放仍被拥有的 VM 或宿主回调状态。
fn complete(
    center: &SchedulerCenter,
    mut completion: PendingCompletion,
) -> Option<PendingCompletion> {
    // Serialize readiness publication with close requests, without destroying a VM under this lock.
    // 使就绪发布与关闭请求串行化，且不在此锁下销毁 VM。
    let mut state = match center.lock() {
        Ok(state) => state,
        Err(error) => {
            center.fail(error);
            return Some(completion);
        }
    };
    if let Some(session_id) = completion.call.request.session_id() {
        let must_close = state.closing
            || state
                .pools
                .get(completion.call.request.pool_id())
                .expect("active pool exists")
                .closed
            || state
                .sessions
                .get(session_id)
                .expect("active session exists")
                .closing;
        if must_close && let Some(lease) = completion.session_lease.take() {
            state
                .sessions
                .get_mut(session_id)
                .expect("active session exists")
                .closing = true;
            drop(state);
            match lease.finish() {
                Ok(ModuleRelease::Retiring(receipt)) => completion.retirement = Some(receipt),
                Ok(ModuleRelease::NoInstance) => {}
                Ok(ModuleRelease::ReturnedToPool) => {
                    center.fail(internal("session VM returned to ordinary reuse"))
                }
                Err(error) => center.fail(error),
            }
            return Some(completion);
        }
    }
    if let Some(retirement) = &completion.retirement {
        match retirement.snapshot() {
            Ok(snapshot) if snapshot.phase != ModuleRetirementPhase::Completed => {
                return Some(completion);
            }
            Ok(_) => {}
            Err(error) => {
                drop(state);
                center.fail(error);
                return Some(completion);
            }
        }
    }
    if !completion.cleaning_started {
        if let Err(error) = completion.call.owner.advance(OperationPhase::Cleaning) {
            drop(state);
            center.fail(error);
            return Some(completion);
        }
        completion.cleaning_started = true;
    }
    match completion
        .call
        .owner
        .complete(completion.result.clone(), completion.effects)
    {
        Ok(()) => {
            state.live.remove(&completion.call.id);
            state.cleaning_count -= 1;
            if completion.dispatched {
                state
                    .pools
                    .get_mut(completion.call.request.pool_id())
                    .expect("active pool retained")
                    .active -= 1;
            }
            if let Some(session_id) = completion.call.request.session_id() {
                let session = state
                    .sessions
                    .get_mut(session_id)
                    .expect("unfinished session retained");
                session.unfinished -= 1;
                if completion.dispatched {
                    session.active = None;
                    if let Some(lease) = completion.session_lease.take() {
                        session.lease = Some(lease);
                        session.opened = true;
                    } else {
                        session.closing = true;
                    }
                } else if matches!(
                    completion.call.request,
                    ScheduledRequest::OpenSession { .. }
                ) {
                    session.closing = true;
                }
                if session.closing
                    && let Err(error) = &completion.result
                {
                    session.error.get_or_insert_with(|| error.clone());
                }
            }
            center.changed.notify_all();
            None
        }
        Err(error) if error.code == EmbeddedErrorCode::Busy => Some(completion),
        Err(error) => {
            drop(state);
            center.fail(error);
            Some(completion)
        }
    }
}

/// Supervise cancellation and completion while every executor may be blocked in host callbacks.
/// 在全部执行器可能阻塞于宿主回调时监督取消与完成。
pub(super) fn supervise(center: &Arc<SchedulerCenter>) -> EmbeddedResult<()> {
    loop {
        sessions::maintain(center)?;
        // Metadata snapshots avoid running pool maintenance under the scheduler lock.
        // 元数据快照避免在调度锁下运行池维护。
        let pools = center
            .lock()?
            .pools
            .values()
            .map(|pool| Arc::clone(&pool.pool))
            .collect::<Vec<_>>();
        for pool in pools {
            pool.retire_expired()?;
        }
        let pending = {
            let mut state = center.lock()?;
            reject_expired(&mut state);
            std::mem::take(&mut state.cleaning)
        };
        let pending = pending
            .into_iter()
            .filter_map(|completion| complete(center, completion))
            .collect::<Vec<_>>();
        let mut state = center.lock()?;
        state.cleaning.extend(pending);
        if state.closing
            && state.live.is_empty()
            && state.sessions.values().all(|session| session.closed)
        {
            drop(state);
            center.pools.request_close()?;
            if center.capabilities.close_and_poll()? {
                return Ok(());
            }
            state = center.lock()?;
        }
        center.changed.notify_all();
        let _ = center
            .changed
            .wait_timeout(state, MAINTENANCE_INTERVAL)
            .map_err(|_| internal("embedded supervisor wait is poisoned"))?;
    }
}
