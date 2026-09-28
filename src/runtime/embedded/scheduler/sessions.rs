use super::super::operations::OperationReservation;
use super::*;

/// Observable pinned-session lifecycle; closing never implies that its VM is already destroyed.
/// 可观察的固定会话生命周期；正在关闭绝不表示其 VM 已销毁。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum EmbeddedSessionPhase {
    /// Creation is queued, initializing, or publishing its operation result.
    /// 创建正在排队、初始化或发布操作结果。
    Opening,
    /// The exact VM is idle and ready for another call.
    /// 精确 VM 空闲，能够接收下一次调用。
    Ready,
    /// One call owns the VM through execution and operation cleanup.
    /// 一次调用持有 VM，覆盖执行与操作清理。
    Running,
    /// Admission stopped; actual execution, queued rejection or retirement remains unfinished.
    /// 入场已停止；实际执行、队列拒绝或退役尚未完成。
    Closing,
    /// All queued work and actual VM ownership have drained.
    /// 全部排队任务和实际 VM 所有权均已排空。
    Closed,
}

/// Bounded retained session observation with immutable pool ownership.
/// 具有不可变池归属的有界保留会话观测。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedSessionSnapshot {
    /// Runtime-issued opaque identity, never reused after forgetting.
    /// 运行时签发的不透明身份，遗忘后绝不复用。
    pub session_id: String,
    /// Exact generation and security partition fixed at creation.
    /// 创建时固定的精确代次与安全分区。
    pub pool_id: String,
    /// Current lifecycle observation.
    /// 当前生命周期观测。
    pub phase: EmbeddedSessionPhase,
    /// Current operation, including initialization and cleanup; absent while idle.
    /// 当前操作，包含初始化与清理；空闲时省略。
    pub active_operation: Option<String>,
    /// Independently retained closing operation; absent until eligible session cleanup is scheduled.
    /// 独立保留的关闭操作；符合条件的会话清理被调度前省略。
    pub finalization_operation: Option<String>,
    /// Accepted calls waiting behind this session's current owner.
    /// 此会话当前所有者之后等待的已接纳调用数。
    pub queued_calls: usize,
    /// First business or closing failure; a later cleanup error cannot replace the original failure.
    /// 首次业务或关闭错误；后续清理错误不能替换原始错误。
    pub error: Option<EmbeddedError>,
}

/// Accepted asynchronous creation; callers wait on `operation` before submitting session calls.
/// 已接纳的异步创建；调用方等待 `operation` 后才提交会话调用。
pub struct EmbeddedSessionOpening {
    /// Immutable session identity immediately available for cancellation or observation.
    /// 可立即用于取消或观察的不可变会话身份。
    pub session_id: String,
    /// Retained initialization operation using the caller's original deadline.
    /// 使用调用方原始截止时间的保留初始化操作。
    pub operation: OperationHandle,
}

/// Scheduler-owned session slot; the lease is absent only while execution or retirement owns it.
/// 调度器拥有的会话槽；仅执行或退役持有租借时槽内租借才省略。
pub(super) struct ScheduledSession {
    /// One reserved closing record, acquired before any initialization effects.
    /// 在任何初始化副作用前取得的一个预留关闭记录。
    pub(super) finalization_reservation: Option<OperationReservation>,
    /// Stable cleanup identity remains observable after the active operation completes.
    /// 活动操作完成后仍可观察的稳定清理身份。
    pub(super) finalization_operation: Option<String>,
    /// Trusted context from the most recently dispatched business call; opening uses the empty context.
    /// 最近已分发业务调用的可信上下文；开启使用空上下文。
    pub(super) finalization_context: LuaInvocationContext,
    /// Exact pool, never resolved again by plugin name.
    /// 精确池，绝不再按插件名解析。
    pub(super) pool_id: String,
    /// Idle or not-yet-initialized VM ownership.
    /// 空闲或尚未初始化的 VM 所有权。
    pub(super) lease: Option<Box<ModuleLease>>,
    /// Exclusive dispatched operation retained through its terminal publication.
    /// 保留到终态发布的独占已分发操作。
    pub(super) active: Option<String>,
    /// Exact queued ownership count, including an undispatched open request.
    /// 精确排队所有权计数，包含尚未分发的打开请求。
    pub(super) queued: usize,
    /// All accepted operations, including rejected queue entries awaiting terminal publication.
    /// 全部已接纳操作，包含等待终态发布的已拒绝队列条目。
    pub(super) unfinished: usize,
    /// Initialization succeeded and its operation was published.
    /// 初始化已成功且操作已发布。
    pub(super) opened: bool,
    /// Permanent stop request; never migrates the VM to another generation.
    /// 永久停止请求；绝不把 VM 迁移到其他代次。
    pub(super) closing: bool,
    /// Actual ownership and all associated operations have drained.
    /// 实际所有权与全部关联操作均已排空。
    pub(super) closed: bool,
    /// Supervisor temporarily owns the lease outside the metadata lock.
    /// 监督器在元数据锁外临时持有租借。
    retiring: bool,
    /// Actual retirement receipt for an idle session explicitly closed by the host.
    /// 宿主显式关闭空闲会话的实际退役回执。
    retirement: Option<ModuleRetirement>,
    /// First failure is retained, without accumulating error history.
    /// 保留首次错误，不累计错误历史。
    pub(super) error: Option<EmbeddedError>,
}

impl ScheduledSession {
    /// Project this exact slot into a stable lifecycle without inspecting a running VM.
    /// 将此精确槽投影为稳定生命周期，不检查正在运行的 VM。
    fn phase(&self) -> EmbeddedSessionPhase {
        if self.closed {
            EmbeddedSessionPhase::Closed
        } else if self.closing {
            EmbeddedSessionPhase::Closing
        } else if !self.opened {
            EmbeddedSessionPhase::Opening
        } else if self.active.is_some() {
            EmbeddedSessionPhase::Running
        } else {
            EmbeddedSessionPhase::Ready
        }
    }
}

/// Explicit request variants avoid fabricated exports or arguments for lifecycle operations.
/// 显式请求变体避免为生命周期操作伪造导出或参数。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ScheduledRequest {
    /// Independent finalization of one exact scheduler-owned reusable instance.
    /// 一个精确调度器所有可复用实例的独立关闭。
    CloseInstance {
        /// Original registered execution domain.
        /// 原始已注册执行域。
        pool_id: String,
        /// Last actually dispatched host context from this same instance.
        /// 来自此同一实例最后实际分发的宿主上下文。
        context: LuaInvocationContext,
    },
    /// Explicit lifecycle work consumes reserved retention and never invokes a business export.
    /// 显式生命周期任务消费预留保留容量，绝不调用业务导出。
    CloseSession {
        /// Exact original registered pool.
        /// 精确原始注册池。
        pool_id: String,
        /// Exact pinned session being closed.
        /// 正在关闭的精确固定会话。
        session_id: String,
        /// Last actually dispatched host context, not a queued or user-selected replacement.
        /// 最后实际分发的宿主上下文，不是排队或用户选择的替代上下文。
        context: LuaInvocationContext,
    },
    /// An ordinary invocation governed by the registered pool policy.
    /// 受已注册池策略治理的普通调用。
    Invoke(EmbeddedCall),
    /// Creation initializes the reserved VM without invoking a business export.
    /// 创建初始化预留 VM，不调用业务导出。
    OpenSession {
        /// Exact pool selected by the host.
        /// 宿主选择的精确池。
        pool_id: String,
        /// Runtime-issued session identity.
        /// 运行时签发的会话身份。
        session_id: String,
    },
    /// A call to one pinned session; its pool identity was resolved under the scheduler lock.
    /// 对一个固定会话的调用；其池身份在调度锁内解析。
    InvokeSession {
        /// Trusted registered session identity.
        /// 可信已注册会话身份。
        session_id: String,
        /// Fully owned invocation with the session's exact pool identity.
        /// 包含会话精确池身份的完整拥有所有权的调用。
        call: EmbeddedCall,
    },
}

impl ScheduledRequest {
    /// Return the immutable pool key for every declared request variant.
    /// 返回每种已声明请求变体的不可变池键。
    pub(super) fn pool_id(&self) -> &str {
        match self {
            Self::Invoke(call) | Self::InvokeSession { call, .. } => &call.pool_id,
            Self::OpenSession { pool_id, .. }
            | Self::CloseSession { pool_id, .. }
            | Self::CloseInstance { pool_id, .. } => pool_id,
        }
    }

    /// Return the trusted session identity only for explicit session requests.
    /// 仅为显式会话请求返回可信会话身份。
    pub(super) fn session_id(&self) -> Option<&str> {
        match self {
            Self::Invoke(_) | Self::CloseInstance { .. } => None,
            Self::OpenSession { session_id, .. }
            | Self::InvokeSession { session_id, .. }
            | Self::CloseSession { session_id, .. } => Some(session_id),
        }
    }

    /// Return the business invocation, or none for explicit opening and closing lifecycle requests.
    /// 返回业务调用；显式开启及关闭生命周期请求返回空值。
    pub(super) fn invocation(&self) -> Option<&EmbeddedCall> {
        match self {
            Self::Invoke(call) | Self::InvokeSession { call, .. } => Some(call),
            Self::OpenSession { .. } | Self::CloseSession { .. } | Self::CloseInstance { .. } => {
                None
            }
        }
    }

    /// Release application values after execution while retaining exact lifecycle identities.
    /// 执行后释放应用值，同时保留精确生命周期身份。
    pub(super) fn release_values(&mut self) {
        match self {
            Self::Invoke(call) | Self::InvokeSession { call, .. } => {
                call.arguments = Value::Null;
                call.context = LuaInvocationContext::default();
            }
            Self::OpenSession { .. } => {}
            Self::CloseSession { context, .. } | Self::CloseInstance { context, .. } => {
                *context = LuaInvocationContext::default()
            }
        }
    }
}

impl EmbeddedRuntime {
    /// Reserve a pinned VM in `pool_id` and queue initialization under `timeout`.
    /// 在 `pool_id` 中预留固定 VM，并在 `timeout` 下排队初始化。
    /// Return an exact session and operation, or reject immediately when resident capacity is full.
    /// 返回精确会话与操作；常驻容量已满时立即拒绝。
    pub fn open_session(
        &self,
        pool_id: &str,
        timeout: Duration,
    ) -> EmbeddedResult<EmbeddedSessionOpening> {
        self.center.capabilities.check_submission()?;
        let control = Arc::new(CallControl::new(timeout)?);
        let mut state = self.center.lock()?;
        if state.closing {
            return Err(closed());
        }
        state.check_persistence_admission()?;
        if state.sessions.len() >= self.center.pools.config().max_sessions {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "retained session capacity reached",
            ));
        }
        let pool = state.pools.get(pool_id).ok_or_else(not_found)?;
        if pool.closed {
            return Err(closed());
        }
        if pool.pool.policy().reuse != InstanceReuse::Session {
            return Err(EmbeddedError::invalid(
                "pool does not declare session reuse",
            ));
        }
        let plugin = state
            .plugins
            .get(&pool.plugin_id)
            .expect("registered pool owns plugin policy");
        if plugin.closing {
            return Err(closed());
        }
        if state.plugin_sessions(&pool.plugin_id) >= plugin.config.max_sessions {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "plugin retained session capacity reached",
            ));
        }
        let plugin_id = pool.plugin_id.clone();
        let needs_finalization = pool.pool.finalizer().is_some();
        if needs_finalization
            && plugin.operations >= plugin.config.max_operations - plugin.reserved_operations
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "plugin operation retention capacity reached",
            ));
        }
        let lease = pool.pool.prepare_with_budget(
            &control,
            true,
            state.plugin_allows_allocation(pool_id)?,
        )?;
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal("session identity exhausted"))?;
        let session_id = IdentityKind::Session.render(self.id(), state.sequence);
        let request = ScheduledRequest::OpenSession {
            pool_id: pool_id.to_owned(),
            session_id: session_id.clone(),
        };
        let bytes = json_size(&request, self.center.pools.config().max_queued_bytes)?;
        // Reserve without creating an operation or starting its closing deadline while the session is idle.
        // 预留时不创建操作，也不在会话空闲期间启动其关闭截止时间。
        let finalization_reservation = if needs_finalization {
            let pool = state.pools.get(pool_id).expect("registered pool");
            let reserved = self.center.operations.reserve_module(
                &pool.pool,
                Some(&session_id),
                lease.allocation_id()?,
                &pool.pool.finalizer().expect("declared finalizer").export,
            )?;
            state
                .plugins
                .get_mut(&plugin_id)
                .expect("registered plugin")
                .reserved_operations += 1;
            Some(reserved)
        } else {
            None
        };
        state.sessions.insert(
            session_id.clone(),
            ScheduledSession {
                finalization_reservation,
                finalization_operation: None,
                finalization_context: LuaInvocationContext::default(),
                pool_id: pool_id.to_owned(),
                lease: Some(Box::new(lease)),
                active: None,
                queued: 0,
                unfinished: 0,
                opened: false,
                closing: false,
                closed: false,
                retiring: false,
                retirement: None,
                error: None,
            },
        );
        match self.center.enqueue(&mut state, request, control, bytes) {
            Ok(operation) => Ok(EmbeddedSessionOpening {
                session_id,
                operation,
            }),
            Err(error) => {
                // No Lua code has run, so releasing this preparation cannot invoke user destructors.
                // 尚未运行 Lua 代码，因此释放此准备不会调用用户析构器。
                if needs_finalization {
                    state
                        .plugins
                        .get_mut(&plugin_id)
                        .expect("registered plugin")
                        .reserved_operations -= 1;
                }
                state.sessions.remove(&session_id);
                Err(error)
            }
        }
    }

    /// Queue `export`, `arguments` and trusted `context` on `session_id` under the original `timeout`.
    /// 在原始 `timeout` 下，把 `export`、`arguments` 与可信 `context` 排入 `session_id`。
    /// Return a queryable operation; opening or closed sessions reject admission explicitly.
    /// 返回可查询操作；正在打开或已关闭会话明确拒绝入场。
    pub fn submit_session(
        &self,
        session_id: &str,
        export: String,
        arguments: Value,
        context: LuaInvocationContext,
        timeout: Duration,
    ) -> EmbeddedResult<OperationHandle> {
        self.center.capabilities.check_submission()?;
        let control = Arc::new(CallControl::new(timeout)?);
        json_size(&arguments, self.center.pools.config().max_value_bytes)?;
        let mut state = self.center.lock()?;
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(session_not_found)?;
        if session.closing || session.closed {
            return Err(closed());
        }
        if !session.opened {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "session initialization is unfinished",
            ));
        }
        let request = ScheduledRequest::InvokeSession {
            session_id: session_id.to_owned(),
            call: EmbeddedCall {
                pool_id: session.pool_id.clone(),
                export,
                arguments,
                context,
            },
        };
        let bytes = json_size(&request, self.center.pools.config().max_queued_bytes)?;
        self.center.enqueue(&mut state, request, control, bytes)
    }

    /// Permanently reject new work for `session_id` and request cancellation of its actual owner.
    /// 永久拒绝 `session_id` 的新任务，并请求取消其实际所有者。
    /// Return immediately; observe `session` until actual queued and VM ownership is closed.
    /// 立即返回；通过 `session` 观察直到实际队列与 VM 所有权关闭。
    pub fn close_session(&self, session_id: &str) -> EmbeddedResult<()> {
        let mut state = self.center.lock()?;
        let session = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(session_not_found)?;
        session.closing = true;
        let active = session.active.clone();
        if let Some(control) = active.as_ref().and_then(|id| state.live.get(id)) {
            control.cancel();
        }
        self.center.changed.notify_all();
        Ok(())
    }

    /// Read the exact retained `session_id`; never redirect a stale identity to a new generation.
    /// 读取精确保留的 `session_id`；绝不把过期身份重定向到新代次。
    /// Return current lifecycle and bounded failure evidence without waiting for execution.
    /// 返回当前生命周期与有界失败证据，不等待执行。
    pub fn session(&self, session_id: &str) -> EmbeddedResult<EmbeddedSessionSnapshot> {
        let state = self.center.lock()?;
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(session_not_found)?;
        Ok(EmbeddedSessionSnapshot {
            session_id: session_id.to_owned(),
            pool_id: session.pool_id.clone(),
            phase: session.phase(),
            active_operation: session.active.clone(),
            finalization_operation: session.finalization_operation.clone(),
            queued_calls: session.queued,
            error: session.error.clone(),
        })
    }

    /// Forget only the fully closed `session_id`; live ownership always returns busy.
    /// 仅遗忘完全关闭的 `session_id`；活跃所有权始终返回忙碌。
    /// Return success without altering already retained operation records.
    /// 返回成功，不修改已经保留的操作记录。
    pub fn forget_session(&self, session_id: &str) -> EmbeddedResult<()> {
        let mut state = self.center.lock()?;
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(session_not_found)?;
        if !session.closed {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "session ownership has not drained",
            ));
        }
        state.sessions.remove(session_id);
        Ok(())
    }
}

impl SchedulerCenter {
    /// Admit `request` atomically with all queue and operation budgets under authoritative `state`.
    /// 在权威 `state` 下，结合全部队列与操作预算原子接纳 `request`。
    /// Keep the supplied original `control` and exact serialized `bytes`; return a queryable handle.
    /// 保留传入的原始 `control` 与精确序列化 `bytes`；返回可查询句柄。
    pub(super) fn enqueue(
        &self,
        state: &mut SchedulerState,
        request: ScheduledRequest,
        control: Arc<CallControl>,
        bytes: usize,
    ) -> EmbeddedResult<OperationHandle> {
        control.check()?;
        if state.closing {
            return Err(closed());
        }
        state.check_persistence_admission()?;
        let pool = state.pools.get(request.pool_id()).ok_or_else(not_found)?;
        if pool.closed {
            return Err(closed());
        }
        if let Some(call) = request.invocation() {
            pool.inputs
                .get(&call.export)
                .ok_or_else(|| EmbeddedError::invalid("module export is not declared"))?
                .validate(&call.arguments)?;
        }
        let config = self.pools.config();
        if pool.queued >= pool.pool.policy().max_queued_calls
            || state.queued >= config.max_queued_calls
            || bytes > config.max_queued_bytes.saturating_sub(state.bytes)
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "embedded request queue capacity reached",
            ));
        }
        let plugin = pool.plugin_id.clone();
        let plugin_state = state
            .plugins
            .get(&plugin)
            .expect("registered pool owns plugin policy");
        if plugin_state.closing {
            return Err(closed());
        }
        if plugin_state.queued >= plugin_state.config.max_queued_calls
            || bytes
                > plugin_state
                    .config
                    .max_queued_bytes
                    .saturating_sub(plugin_state.bytes)
            || plugin_state.operations
                >= plugin_state.config.max_operations - plugin_state.reserved_operations
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "plugin queue or retained operation capacity reached",
            ));
        }
        // Freeze this pool's authority before publishing the operation or its shared control identity.
        // 发布操作或其共享控制身份前，冻结此池的权威。
        let (handle, owner) = self.operations.admit_module(
            Arc::clone(&control),
            &pool.pool,
            request.session_id(),
            request.invocation().map(|call| call.export.as_str()),
        )?;
        let id = handle.id().to_owned();
        let plugin_state = state
            .plugins
            .get_mut(&plugin)
            .expect("validated plugin exists");
        plugin_state.queued += 1;
        plugin_state.bytes += bytes;
        plugin_state.operations += 1;
        state.operation_plugins.insert(id.clone(), plugin.clone());
        state
            .pools
            .get_mut(request.pool_id())
            .expect("validated pool exists")
            .queued += 1;
        if let Some(session_id) = request.session_id() {
            let session = state
                .sessions
                .get_mut(session_id)
                .expect("validated session exists");
            session.queued += 1;
            session.unfinished += 1;
        }
        state.queued += 1;
        state.bytes += bytes;
        state.live.insert(id.clone(), Arc::clone(&control));
        if !state.queues.contains_key(&plugin) {
            state.rotation.push_back(plugin.clone());
        }
        state
            .queues
            .entry(plugin)
            .or_default()
            .push_back(ScheduledCall {
                reusable_instance: None,
                id,
                owner,
                control,
                request,
                bytes,
            });
        self.changed.notify_all();
        Ok(handle)
    }
}

/// Retire explicitly closed idle sessions outside locks, then publish closure from exact receipts.
/// 在锁外退役显式关闭的空闲会话，再根据精确回执发布关闭。
/// Return infrastructure errors without declaring unfinished ownership released.
/// 返回基础设施错误，不宣称未完成所有权已释放。
pub(super) fn maintain(center: &SchedulerCenter) -> EmbeddedResult<()> {
    let retiring = {
        let mut state = center.lock()?;
        let closing_pools = state
            .pools
            .iter()
            .filter(|(_, pool)| pool.closed)
            .map(|(id, _)| id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let closing = state.closing;
        let mut cancel = Vec::new();
        let mut retiring = Vec::new();
        let mut finalizing = Vec::new();
        for (id, session) in &mut state.sessions {
            session.closing |= closing || closing_pools.contains(&session.pool_id);
            if !session.closing {
                continue;
            }
            if let Some(active) = &session.active {
                if session.finalization_operation.as_ref() != Some(active) {
                    cancel.push(active.clone());
                }
            } else if session.unfinished == 0
                && let Some(lease) = &session.lease
            {
                if lease.finalization_plan().is_some() {
                    finalizing.push(id.clone());
                } else {
                    session.retiring = true;
                    retiring.push((
                        id.clone(),
                        session.lease.take().expect("idle session lease"),
                    ));
                }
            }
        }
        for id in cancel {
            if let Some(control) = state.live.get(&id) {
                control.cancel();
            }
        }
        for id in finalizing {
            sessions_finalization::schedule(center, &mut state, &id)?;
        }
        retiring
    };
    for (id, lease) in retiring {
        let release = lease.finish()?;
        let mut state = center.lock()?;
        let session = state
            .sessions
            .get_mut(&id)
            .expect("retiring session cannot be forgotten");
        session.retirement = match release {
            ModuleRelease::Retiring(receipt) => Some(receipt),
            ModuleRelease::NoInstance => None,
            ModuleRelease::ReturnedToPool => {
                return Err(internal("pinned session returned to ordinary reuse"));
            }
        };
        session.retiring = false;
    }
    let mut state = center.lock()?;
    let mut unused = Vec::new();
    for session in state.sessions.values_mut() {
        if session.closing
            && session.active.is_none()
            && session.unfinished == 0
            && session.lease.is_none()
            && !session.retiring
        {
            let drained = match &session.retirement {
                Some(receipt) => receipt.snapshot()?.phase == ModuleRetirementPhase::Completed,
                None => true,
            };
            session.closed = drained;
            if drained {
                session.finalization_context = LuaInvocationContext::default();
                if let Some(reservation) = session.finalization_reservation.take() {
                    // Failed or cancelled initialization never created an eligible closing export.
                    // 失败或取消的初始化从未创建符合关闭条件的导出。
                    unused.push((session.pool_id.clone(), reservation));
                }
            }
        }
    }
    for (pool_id, reservation) in unused {
        let plugin_id = state
            .pools
            .get(&pool_id)
            .expect("session pool retained")
            .plugin_id
            .clone();
        state
            .plugins
            .get_mut(&plugin_id)
            .expect("session plugin retained")
            .reserved_operations -= 1;
        drop(reservation);
    }
    Ok(())
}

/// Report a missing exact session without exposing another runtime's identities.
/// 报告精确会话不存在，不暴露其他运行时的身份。
fn session_not_found() -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::NotFound, "embedded session not found")
}
