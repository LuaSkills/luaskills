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
        .get_mut(&call.request.pool_id)
        .expect("queued pool is retained")
        .queued -= 1;
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
            if !visited.insert(call.request.pool_id.clone()) {
                continue;
            }
            let pool = state
                .pools
                .get(&call.request.pool_id)
                .expect("queued pool is retained");
            if pool.closed || call.control.check().is_err() {
                continue;
            }
            if pool.active >= pool.pool.policy().max_running_calls {
                continue;
            }
            let prepared = pool.pool.prepare(&call.control);
            if matches!(&prepared, Err(error) if error.code == EmbeddedErrorCode::CapacityExceeded)
            {
                // A warm plugin must not starve the next cold plugin by continuously reclaiming its own idle VM.
                // 已预热插件不能不断重用自身空闲 VM，饿死下一个尚未预热的插件。
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
            // Move the selected plugin behind the others before removing an empty queue.
            // 在移除空队列前，将选定插件移动到其他插件之后。
            state.rotation.rotate_left(rotation_index + 1);
            let call = take_call(state, plugin, index);
            return Ok(Some(match prepared {
                Ok(lease) => {
                    state
                        .pools
                        .get_mut(&call.request.pool_id)
                        .expect("prepared pool exists")
                        .active += 1;
                    Dispatch::Ready(call, Box::new(lease))
                }
                Err(error) => Dispatch::Rejected(call, error),
            }));
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
    // The same control covers initialization, Lua execution and nested host requests.
    // 同一控制覆盖初始化、Lua 执行及嵌套宿主请求。
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        call.owner.advance(OperationPhase::Initializing)?;
        lease.initialize(Arc::clone(&call.control))?;
        call.owner.advance(OperationPhase::Running)?;
        lease.invoke(ModuleInvocation {
            operation_id: &call.id,
            session_id: None,
            export: &call.request.export,
            arguments: &call.request.arguments,
            context: &call.request.context,
            control: Arc::clone(&call.control),
        })
    }));
    let mut result = match outcome {
        Ok(result) => result,
        Err(_) => Err(internal("embedded invocation panicked")),
    };
    // Bound retained output before asynchronous cleanup can keep it alive indefinitely.
    // 在异步清理可能无限保留输出前约束其大小。
    let retained_size = match &result {
        Ok(value) => json_size(value, max_value_bytes),
        Err(error) => json_size(error, max_value_bytes),
    };
    if let Err(error) = retained_size {
        result = Err(error);
    }
    // An uncertain or failed call always retires its instance before operation completion.
    // 不确定或失败调用始终先退役实例，再完成操作。
    if result.is_err() {
        lease.close();
    }
    let release = lease.finish();
    let (retirement, result, effects) = match release {
        Ok(ModuleRelease::Retiring(receipt)) => (Some(receipt), result, EffectState::Unknown),
        Ok(ModuleRelease::ReturnedToPool) => (None, result, EffectState::Unknown),
        Ok(ModuleRelease::NoInstance) => (None, result, EffectState::NotStarted),
        Err(error) => (None, Err(error), EffectState::Unknown),
    };
    // Cleanup retains identities and outcomes, not unused application inputs or context copies.
    // 清理保留身份与结果，不保留不再使用的应用输入或上下文副本。
    call.request.arguments = Value::Null;
    call.request.context = LuaInvocationContext::default();
    PendingCompletion {
        call,
        result,
        retirement,
        effects,
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
                            .get(&call.request.pool_id)
                            .expect("queued pool exists")
                            .closed =>
                {
                    Some(closed())
                }
                Ok(()) => None,
            };
            if let Some(error) = error {
                let call = take_call(state, &plugin, index);
                state.cleaning_count += 1;
                state.cleaning.push(PendingCompletion {
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
    if let Some(retirement) = &completion.retirement {
        match retirement.snapshot() {
            Ok(snapshot) if snapshot.phase != ModuleRetirementPhase::Completed => {
                return Some(completion);
            }
            Ok(_) => {}
            Err(error) => {
                center.fail(error);
                return Some(completion);
            }
        }
    }
    // The unique owner may still be queued when the supervisor rejected an undispatched request.
    // 监督器拒绝未分发请求时，唯一所有者仍可能处于排队阶段。
    if !completion.cleaning_started {
        if let Err(error) = completion.call.owner.advance(OperationPhase::Cleaning) {
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
        Ok(()) => match center.lock() {
            Ok(mut state) => {
                state.live.remove(&completion.call.id);
                state.cleaning_count -= 1;
                if completion.dispatched {
                    state
                        .pools
                        .get_mut(&completion.call.request.pool_id)
                        .expect("active pool is retained")
                        .active -= 1;
                }
                center.changed.notify_all();
                None
            }
            Err(error) => {
                center.fail(error);
                Some(completion)
            }
        },
        Err(error) if error.code == EmbeddedErrorCode::Busy => Some(completion),
        Err(error) => {
            center.fail(error);
            Some(completion)
        }
    }
}

/// Supervise cancellation and completion while every executor may be blocked in host callbacks.
/// 在全部执行器可能阻塞于宿主回调时监督取消与完成。
pub(super) fn supervise(center: &Arc<SchedulerCenter>) -> EmbeddedResult<()> {
    loop {
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
        if state.closing && state.live.is_empty() {
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
