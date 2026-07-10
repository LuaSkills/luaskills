#[cfg(windows)]
use super::runlua::has_invalid_windows_path_syntax;
use super::runlua::{
    default_runlua_timeout_ms, lock_runlua_cwd_guard, resolve_host_default_text_encoding,
};
use super::*;

/// Runtime session creation request accepted by the host-facing JSON API.
/// 面向宿主 JSON API 的运行时会话创建请求。
#[derive(Debug, Deserialize)]
struct RuntimeSessionCreateRequest {
    /// Stable session identifier supplied by the host or AI task.
    /// 宿主或 AI 任务提供的稳定会话标识。
    sid: String,
    /// Requested lease TTL in seconds.
    /// 请求的租约有效期秒数。
    #[serde(default)]
    ttl_sec: Option<u64>,
    /// Whether an existing session with the same SID should be replaced.
    /// 是否替换同一 SID 下已经存在的会话。
    #[serde(default)]
    replace: bool,
    /// Optional lease working directory controlled by the host.
    /// 由宿主控制的可选租约工作目录。
    #[serde(default)]
    cwd: Option<String>,
    /// Optional workspace root attached to the lease for host-side bookkeeping.
    /// 供宿主侧记账与注入使用的可选工作区根目录。
    #[serde(default)]
    workspace_root: Option<String>,
    /// Optional extra Lua module roots prepended for this lease.
    /// 为当前租约前置追加的可选 Lua 模块根目录集合。
    #[serde(default)]
    lua_roots: Vec<String>,
    /// Optional extra native module roots prepended for this lease.
    /// 为当前租约前置追加的可选原生模块根目录集合。
    #[serde(default)]
    c_roots: Vec<String>,
    /// Optional structured host-owned mount metadata recorded on the lease.
    /// 记录在租约上的可选宿主挂载元数据。
    #[serde(default = "default_runlua_exec_args")]
    mounts: Value,
}

/// Runtime session eval request accepted by the host-facing JSON API.
/// 面向宿主 JSON API 的运行时会话执行请求。
#[derive(Debug, Deserialize)]
struct RuntimeSessionEvalRequest {
    /// Opaque lease identifier returned by create.
    /// create 返回的不透明租约标识。
    lease_id: String,
    /// Optional stable session identifier echoed by the host wrapper.
    /// 由宿主包装层回传的可选稳定会话标识。
    #[serde(default)]
    sid: Option<String>,
    /// Optional SID-local generation echoed by the host wrapper.
    /// 由宿主包装层回传的可选 SID 内 generation。
    #[serde(default)]
    generation: Option<u64>,
    /// Inline Lua source code executed inside the persistent VM.
    /// 在持久 VM 内执行的内联 Lua 源码。
    code: String,
    /// Structured arguments exposed to Lua as `args`.
    /// 以 `args` 形式暴露给 Lua 的结构化参数。
    #[serde(default = "default_runlua_exec_args")]
    args: Value,
    /// Maximum execution time in milliseconds.
    /// 最大执行时长（毫秒）。
    #[serde(default = "default_runlua_timeout_ms")]
    timeout_ms: u64,
    /// Optional request-scoped host metadata injected for the current evaluation.
    /// 为本次执行注入的可选请求级宿主元数据。
    #[serde(default)]
    request_context: Option<RuntimeRequestContext>,
    /// Host-resolved client budget object injected for the current evaluation.
    /// 为本次执行注入的宿主解析客户端预算对象。
    #[serde(default = "default_runlua_exec_args")]
    client_budget: Value,
    /// Host-resolved tool config object injected for the current evaluation.
    /// 为本次执行注入的宿主解析工具配置对象。
    #[serde(default = "default_runlua_exec_args")]
    tool_config: Value,
}

/// Runtime session identifier request accepted by status and close APIs.
/// status 与 close API 接收的运行时会话标识请求。
#[derive(Debug, Deserialize)]
struct RuntimeSessionLeaseRequest {
    /// Opaque lease identifier returned by create.
    /// create 返回的不透明租约标识。
    lease_id: String,
    /// Optional stable session identifier echoed by the host wrapper.
    /// 由宿主包装层回传的可选稳定会话标识。
    #[serde(default)]
    sid: Option<String>,
    /// Optional SID-local generation echoed by the host wrapper.
    /// 由宿主包装层回传的可选 SID 内 generation。
    #[serde(default)]
    generation: Option<u64>,
}

/// Runtime session list request accepted by the host-facing JSON API.
/// 面向宿主 JSON API 的运行时会话列表请求。
#[derive(Debug, Deserialize)]
struct RuntimeSessionListRequest {
    /// Optional stable session identifier used to filter the active lease list.
    /// 用于过滤活跃租约列表的可选稳定会话标识。
    #[serde(default)]
    sid: Option<String>,
}

/// Stable lease profile that determines lifetime defaults and runtime path semantics.
/// 决定生命周期默认值与运行时路径语义的稳定租约类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeLeaseProfile {
    /// Ordinary public runtime lease used by hosts for finite stateful execution.
    /// 宿主用于有限期有状态执行的普通公开租约。
    Public,
    /// Host-owned `system_lua_lib` runtime lease with fixed library-directory semantics.
    /// 带固定库目录语义的宿主自有 `system_lua_lib` 运行时租约。
    SystemLuaLib,
}

impl RuntimeLeaseProfile {
    /// Return the stable host-visible profile string.
    /// 返回面向宿主的稳定 profile 字符串。
    fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::SystemLuaLib => "system_lua_lib",
        }
    }
}

/// Host-owned per-lease path context independent from ordinary skill directory semantics.
/// 独立于普通 skill 目录语义的宿主租约路径上下文。
#[derive(Debug, Clone)]
struct RuntimeLeasePathContext {
    /// Effective working directory applied while evaluating the lease.
    /// 执行租约时生效的工作目录。
    cwd: Option<PathBuf>,
    /// Optional workspace root tracked for host-side identity and prompt assembly.
    /// 供宿主侧身份与提示词装配跟踪的可选工作区根目录。
    workspace_root: Option<PathBuf>,
    /// Extra Lua module roots permanently prepended to the lease VM.
    /// 永久前置到租约虚拟机中的额外 Lua 模块根目录。
    lua_roots: Vec<PathBuf>,
    /// Extra native module roots permanently prepended to the lease VM.
    /// 永久前置到租约虚拟机中的额外原生模块根目录。
    c_roots: Vec<PathBuf>,
    /// Host-owned structured mount metadata.
    /// 宿主拥有的结构化挂载元数据。
    mounts: Value,
    /// Effective fixed system Lua library directory when the lease profile is `system_lua_lib`.
    /// 当租约 profile 为 `system_lua_lib` 时生效的固定系统 Lua 库目录。
    system_lua_lib_dir: Option<PathBuf>,
}

/// Manager for persistent runtime sessions owned by one LuaEngine.
/// 单个 LuaEngine 拥有的持久运行时会话管理器。
pub(super) struct RuntimeSessionManager {
    /// Mutable lease maps protected for cross-call coordination.
    /// 用于跨调用协调的可变租约映射。
    state: Mutex<RuntimeSessionManagerState>,
}

/// Mutable state inside the runtime session manager.
/// 运行时会话管理器内部的可变状态。
struct RuntimeSessionManagerState {
    /// Active or recently closed leases keyed by opaque lease id.
    /// 按不透明租约 id 索引的活跃或刚关闭租约。
    leases: HashMap<String, RuntimeSessionEntry>,
    /// Current lease id keyed by stable SID.
    /// 按稳定 SID 索引的当前租约 id。
    sid_index: HashMap<String, String>,
    /// Terminal lease tombstones retained for stable post-close and post-replace errors.
    /// 为关闭后与替换后稳定错误而保留的终态租约墓碑。
    tombstones: HashMap<String, RuntimeSessionTombstone>,
    /// Last issued generation for each SID.
    /// 每个 SID 已签发的最新 generation。
    generations: HashMap<String, u64>,
    /// Monotonic local sequence used to build lease ids.
    /// 用于构造租约 id 的本地单调序号。
    next_sequence: u64,
}

/// One persistent Lua VM runtime session.
/// 单个持久 Lua VM 运行时会话。
pub(super) struct RuntimeSession {
    /// Stable session identifier supplied by the caller.
    /// 调用方提供的稳定会话标识。
    sid: String,
    /// Opaque lease identifier used for subsequent calls.
    /// 后续调用使用的不透明租约标识。
    lease_id: String,
    /// SID-local generation number.
    /// SID 内部的 generation 编号。
    generation: u64,
    /// Stable lease profile describing whether the VM is one public lease or one `system_lua_lib` lease.
    /// 描述当前 VM 属于公开租约还是 `system_lua_lib` 租约的稳定 profile。
    profile: RuntimeLeaseProfile,
    /// Lease TTL in seconds refreshed by successful calls.
    /// 成功调用会刷新的租约 TTL 秒数。
    ttl_sec: Option<u64>,
    /// Monotonic expiration timestamp used for local cleanup.
    /// 用于本地清理的单调过期时间戳。
    expires_at: Option<Instant>,
    /// Host-visible expiration timestamp in Unix milliseconds.
    /// 面向宿主可见的 Unix 毫秒过期时间戳。
    expires_at_unix_ms: Option<u128>,
    /// Host-owned runtime path context applied to the lease.
    /// 应用于当前租约的宿主运行时路径上下文。
    path_context: RuntimeLeasePathContext,
    /// Persistent Lua VM retained by this session.
    /// 此会话保留的持久 Lua VM。
    vm: LuaVm,
    /// Shared terminal-state marker visible across stale handles and manager retirement paths.
    /// 在陈旧句柄与管理器退役路径之间共享可见的终态状态标记。
    terminal_state: Arc<AtomicU8>,
    /// Whether the lease has been explicitly closed.
    /// 租约是否已经被显式关闭。
    closed: bool,
}

/// Active runtime-session entry stored in the manager table.
/// 存储在管理器表中的活跃运行时会话条目。
struct RuntimeSessionEntry {
    /// Locked runtime session state and retained VM.
    /// 已加锁的运行时会话状态与保留 VM。
    session: Arc<Mutex<RuntimeSession>>,
    /// Stable session identifier copied from the runtime session at insertion time.
    /// 插入时从运行时会话复制出的稳定会话标识。
    sid: String,
    /// Opaque lease identifier copied from the runtime session at insertion time.
    /// 插入时从运行时会话复制出的不透明租约标识。
    lease_id: String,
    /// SID-local generation copied from the runtime session at insertion time.
    /// 插入时从运行时会话复制出的 SID 内 generation。
    generation: u64,
    /// Stable lease profile copied from the runtime session at insertion time.
    /// 插入时从运行时会话复制出的稳定租约类型。
    profile: RuntimeLeaseProfile,
    /// Shared terminal-state marker that can be flipped without taking the session VM lock.
    /// 可在不获取会话 VM 锁的前提下切换的共享终态状态标记。
    terminal_state: Arc<AtomicU8>,
    /// Lock-free snapshot used by list operations.
    /// 供列表操作使用的无锁快照。
    snapshot: Value,
}

impl RuntimeSessionEntry {
    /// Return the active-list payload with typed lease identity restored over the display snapshot.
    /// 返回 active list 载荷，并用 typed 租约身份覆盖展示快照中的身份字段。
    fn list_payload(&self) -> Result<Value, RuntimeSessionError> {
        let mut snapshot = self.snapshot.clone();
        let Some(object) = snapshot.as_object_mut() else {
            return Err(RuntimeSessionError {
                code: "lease_snapshot_corrupted",
                message: format!(
                    "runtime session lease `{}` active snapshot is not a JSON object",
                    self.lease_id
                ),
            });
        };
        object.insert("sid".to_string(), Value::String(self.sid.clone()));
        object.insert("lease_id".to_string(), Value::String(self.lease_id.clone()));
        object.insert("generation".to_string(), json!(self.generation));
        object.insert(
            "profile".to_string(),
            Value::String(self.profile.as_str().to_string()),
        );
        Ok(snapshot)
    }
}

/// Stable runtime-session terminal states stored in the shared atomic marker.
/// 存储在共享原子标记中的稳定运行时会话终态状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum RuntimeSessionTerminalState {
    /// Lease is still active.
    /// 租约仍然处于活跃状态。
    Active = 0,
    /// Lease has been explicitly closed.
    /// 租约已被显式关闭。
    Closed = 1,
    /// Lease has expired.
    /// 租约已经过期。
    Expired = 2,
    /// Lease has been replaced by a newer SID generation.
    /// 租约已被同 SID 的更新 generation 替换。
    Replaced = 3,
}

/// Runtime session operation error with a stable code.
/// 带稳定错误码的运行时会话操作错误。
#[derive(Debug)]
pub(super) struct RuntimeSessionError {
    /// Stable error code for host recovery logic.
    /// 供宿主恢复逻辑使用的稳定错误码。
    pub(super) code: &'static str,
    /// Human-readable error message.
    /// 面向人的错误消息。
    pub(super) message: String,
}

/// Terminal lease record retained after one session leaves the active pool.
/// 单个会话离开活跃池后保留的终态租约记录。
struct RuntimeSessionTombstone {
    /// Stable session identifier originally bound to the lease.
    /// 原本绑定到该租约的稳定会话标识。
    sid: String,
    /// Opaque lease identifier preserved for post-terminal lookups.
    /// 为终态后续查询保留的不透明租约标识。
    lease_id: String,
    /// SID-local generation number preserved for diagnostics.
    /// 用于诊断的 SID 内 generation 编号。
    generation: u64,
    /// Stable lease profile preserved for post-terminal profile checks.
    /// 为终态后的 profile 校验保留的稳定租约类型。
    profile: RuntimeLeaseProfile,
    /// Stable terminal error code reported after the lease leaves the active pool.
    /// 租约离开活跃池后返回的稳定终态错误码。
    code: &'static str,
    /// Monotonic retirement timestamp used to evict stale tombstones.
    /// 用于清理陈旧墓碑的单调退役时间戳。
    retired_at: Instant,
}

/// Return the default empty args object for runlua execution.
/// 返回 runlua 执行默认使用的空参数对象。
pub(super) fn default_runlua_exec_args() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Return the default TTL used by persistent runtime sessions.
/// 返回持久运行时会话使用的默认 TTL。
fn default_runtime_session_ttl_sec() -> u64 {
    600
}

impl RuntimeSessionEvalRequest {
    /// Convert the eval request into one request-scoped invocation context snapshot.
    /// 将执行请求转换为一份请求级调用上下文快照。
    fn to_invocation_context(&self) -> LuaInvocationContext {
        LuaInvocationContext::new(
            self.request_context.clone(),
            self.client_budget.clone(),
            self.tool_config.clone(),
        )
    }
}

impl RuntimeSessionManager {
    /// Create an empty persistent runtime session manager.
    /// 创建空的持久运行时会话管理器。
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(RuntimeSessionManagerState {
                leases: HashMap::new(),
                sid_index: HashMap::new(),
                tombstones: HashMap::new(),
                generations: HashMap::new(),
                next_sequence: 0,
            }),
        }
    }

    /// Insert one new runtime session while enforcing SID uniqueness.
    /// 插入一个新的运行时会话并强制 SID 唯一。
    fn insert(
        &self,
        profile: RuntimeLeaseProfile,
        sid: String,
        ttl_sec: Option<u64>,
        replace: bool,
        path_context: RuntimeLeasePathContext,
        vm: LuaVm,
    ) -> Result<Value, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        Self::prune_inactive_locked(&mut state);
        if let Some(existing_lease_id) = state.sid_index.get(&sid).cloned() {
            if let Some(existing_session) = state
                .leases
                .get(&existing_lease_id)
                .map(|entry| Arc::clone(&entry.session))
            {
                let existing_session = match existing_session.try_lock() {
                    Ok(existing_session) => existing_session,
                    Err(TryLockError::WouldBlock) => {
                        if replace {
                            return Err(RuntimeSessionError {
                                code: "lease_busy",
                                message: format!(
                                    "runtime session SID `{sid}` cannot replace busy lease `{existing_lease_id}`"
                                ),
                            });
                        }
                        return Err(RuntimeSessionError {
                            code: "lease_exists",
                            message: format!(
                                "runtime session SID `{sid}` already has lease `{existing_lease_id}`"
                            ),
                        });
                    }
                    Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                };
                if let Some(error) = existing_session.inactive_error() {
                    Self::retire_active_lease_locked(&mut state, &existing_lease_id, error.code);
                } else if !replace {
                    return Err(RuntimeSessionError {
                        code: "lease_exists",
                        message: format!(
                            "runtime session SID `{sid}` already has lease `{existing_lease_id}`"
                        ),
                    });
                } else {
                    Self::retire_active_lease_locked(
                        &mut state,
                        &existing_lease_id,
                        "lease_replaced",
                    );
                }
            } else {
                state.sid_index.remove(&sid);
            }
        }
        if state.leases.len() >= 8 {
            return Err(RuntimeSessionError {
                code: "lease_limit_exceeded",
                message: "runtime session lease limit exceeded".to_string(),
            });
        }

        state.next_sequence = state.next_sequence.saturating_add(1);
        let generation = state
            .generations
            .get(&sid)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        state.generations.insert(sid.clone(), generation);
        let lease_id = format!("rt_{}_{}", unix_time_millis(), state.next_sequence);
        let ttl_sec = ttl_sec.map(|value| value.clamp(1, 3_600));
        let (expires_at, expires_at_unix_ms) = runtime_session_expiry(ttl_sec);
        let terminal_state = Arc::new(AtomicU8::new(RuntimeSessionTerminalState::Active as u8));
        let session = RuntimeSession {
            sid: sid.clone(),
            lease_id: lease_id.clone(),
            generation,
            profile,
            ttl_sec,
            expires_at,
            expires_at_unix_ms,
            path_context,
            vm,
            terminal_state: Arc::clone(&terminal_state),
            closed: false,
        };
        let snapshot = session.status_payload();
        let response_snapshot = snapshot.clone();
        state.leases.insert(
            lease_id.clone(),
            RuntimeSessionEntry {
                session: Arc::new(Mutex::new(session)),
                sid: sid.clone(),
                lease_id: lease_id.clone(),
                generation,
                profile,
                terminal_state,
                snapshot,
            },
        );
        state.sid_index.insert(sid.clone(), lease_id.clone());

        Ok(json!({
            "ok": true,
            "sid": sid,
            "lease_id": lease_id,
            "generation": generation,
            "profile": profile.as_str(),
            "lifetime": if ttl_sec.is_some() { "finite" } else { "infinite" },
            "cwd": response_snapshot.get("cwd").cloned().unwrap_or(Value::Null),
            "workspace_root": response_snapshot
                .get("workspace_root")
                .cloned()
                .unwrap_or(Value::Null),
            "system_lua_lib": response_snapshot.get("system_lua_lib").cloned().unwrap_or(Value::Null),
            "ttl_sec": ttl_sec,
            "expires_at_unix_ms": expires_at_unix_ms
        }))
    }

    /// Get one session handle by lease id.
    /// 按租约 id 获取一个会话句柄。
    pub(super) fn get(
        &self,
        lease_id: &str,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Arc<Mutex<RuntimeSession>>, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        Self::prune_inactive_locked(&mut state);
        if let Some(entry) = state.leases.get(lease_id) {
            let session = Arc::clone(&entry.session);
            let session_guard = Self::try_lock_session(&session, lease_id)?;
            Self::validate_session_identity(&session_guard, expected_sid, expected_generation)?;
            Self::validate_session_profile(&session_guard, expected_profile)?;
            drop(session_guard);
            return Ok(session);
        }
        if let Some(tombstone) = state.tombstones.get(lease_id) {
            Self::validate_tombstone_identity(tombstone, expected_sid, expected_generation)?;
            Self::validate_tombstone_profile(tombstone, expected_profile)?;
            return Err(tombstone.as_error());
        }
        Err(RuntimeSessionError {
            code: "lease_not_found",
            message: format!("runtime session lease `{lease_id}` was not found"),
        })
    }

    /// Return a compact status payload for one runtime session.
    /// 返回单个运行时会话的紧凑状态载荷。
    fn status(
        &self,
        lease_id: &str,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Value, RuntimeSessionError> {
        let session = self.get(
            lease_id,
            expected_sid,
            expected_generation,
            expected_profile,
        )?;
        let session = Self::try_lock_session(&session, lease_id)?;
        if let Some(error) = session.inactive_error() {
            return Err(error);
        }
        Ok(session.status_payload())
    }

    /// Return a stable active-lease listing payload.
    /// 返回稳定的活跃租约列表载荷。
    fn list(
        &self,
        sid: Option<&str>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Value, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        Self::prune_inactive_locked(&mut state);
        let mut entries = Vec::new();
        for entry in state.leases.values() {
            if sid.is_some_and(|expected_sid| entry.sid != expected_sid) {
                continue;
            }
            if expected_profile.is_some_and(|expected_profile| entry.profile != expected_profile) {
                continue;
            }
            entries.push(entry);
        }
        entries.sort_by(|left, right| compare_runtime_session_entries(left, right));
        let mut leases = Vec::with_capacity(entries.len());
        for entry in entries {
            leases.push(entry.list_payload()?);
        }
        Ok(json!({
            "ok": true,
            "leases": leases,
        }))
    }

    /// Mark one runtime session closed.
    /// 将一个运行时会话标记为已关闭。
    fn close(
        &self,
        lease_id: &str,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Value, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        Self::prune_inactive_locked(&mut state);
        let Some((session, terminal_state)) = state.leases.get(lease_id).map(|entry| {
            (
                Arc::clone(&entry.session),
                Arc::clone(&entry.terminal_state),
            )
        }) else {
            if let Some(tombstone) = state.tombstones.get(lease_id) {
                Self::validate_tombstone_identity(tombstone, expected_sid, expected_generation)?;
                Self::validate_tombstone_profile(tombstone, expected_profile)?;
                return Err(tombstone.as_error());
            }
            return Err(RuntimeSessionError {
                code: "lease_not_found",
                message: format!("runtime session lease `{lease_id}` was not found"),
            });
        };
        let mut session = Self::try_lock_session(&session, lease_id)?;
        Self::validate_session_identity(&session, expected_sid, expected_generation)?;
        Self::validate_session_profile(&session, expected_profile)?;
        terminal_state.store(RuntimeSessionTerminalState::Closed as u8, Ordering::Release);
        session.closed = true;
        let payload = session.close_payload();
        let tombstone = RuntimeSessionTombstone::from_session(&session, "lease_closed");
        let sid = session.sid.clone();
        drop(session);
        state.leases.remove(lease_id);
        if state
            .sid_index
            .get(&sid)
            .is_some_and(|current| current == lease_id)
        {
            state.sid_index.remove(&sid);
        }
        state.tombstones.insert(lease_id.to_string(), tombstone);
        Ok(payload)
    }

    /// Update the cached active snapshot for one runtime session lease when it is still active.
    /// 当运行时会话租约仍然活跃时更新其缓存快照。
    fn update_active_snapshot(
        &self,
        lease_id: &str,
        snapshot: Value,
    ) -> Result<(), RuntimeSessionError> {
        let mut state = self.lock_state()?;
        if let Some(entry) = state.leases.get_mut(lease_id) {
            entry.snapshot = snapshot;
        }
        Ok(())
    }

    /// Replace one active snapshot for tests that verify typed lease identity remains authoritative.
    /// 为验证 typed 租约身份仍为权威来源的测试替换单个活跃快照。
    #[cfg(test)]
    pub(super) fn replace_active_snapshot_for_test(&self, lease_id: &str, snapshot: Value) {
        // Test-only state mutation used to simulate a corrupted display snapshot.
        // 仅用于测试的状态修改，用来模拟损坏的展示快照。
        let mut state = self
            .lock_state()
            .expect("runtime session manager state should be available in tests");
        if let Some(entry) = state.leases.get_mut(lease_id) {
            entry.snapshot = snapshot;
        }
    }

    /// Lock the manager state with a stable runtime error.
    /// 使用稳定运行时错误锁定管理器状态。
    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, RuntimeSessionManagerState>, RuntimeSessionError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Try to acquire one runtime session lock, keeping busy sessions distinct from poisoned sessions.
    /// 尝试获取单个运行时会话锁，同时区分真正忙碌的会话与已 poison 但可恢复的会话。
    fn try_lock_session<'a>(
        session: &'a Arc<Mutex<RuntimeSession>>,
        lease_id: &str,
    ) -> Result<std::sync::MutexGuard<'a, RuntimeSession>, RuntimeSessionError> {
        match session.try_lock() {
            Ok(session) => Ok(session),
            Err(TryLockError::WouldBlock) => Err(RuntimeSessionError {
                code: "lease_busy",
                message: format!("runtime session lease `{lease_id}` is busy"),
            }),
            Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        }
    }

    /// Remove expired or closed sessions from the active indexes.
    /// 从活跃索引中移除已过期或已关闭的会话。
    fn prune_inactive_locked(state: &mut RuntimeSessionManagerState) {
        let now = Instant::now();
        let mut removed = Vec::new();
        for (lease_id, entry) in &state.leases {
            let should_remove = entry
                .session
                .try_lock()
                .map(|session| session.expires_at.is_some() && session.is_expired())
                .unwrap_or_else(|error| match error {
                    TryLockError::WouldBlock => false,
                    TryLockError::Poisoned(poisoned) => {
                        let session = poisoned.into_inner();
                        session.expires_at.is_some() && session.is_expired()
                    }
                });
            if should_remove {
                removed.push(lease_id.clone());
            }
        }
        for lease_id in removed {
            Self::retire_active_lease_locked(state, &lease_id, "lease_expired");
        }
        let tombstone_ttl = runtime_session_tombstone_ttl();
        state
            .tombstones
            .retain(|_, tombstone| now.duration_since(tombstone.retired_at) < tombstone_ttl);
    }

    /// Move one active lease into the terminal tombstone table.
    /// 将单个活跃租约移动到终态墓碑表中。
    fn retire_active_lease_locked(
        state: &mut RuntimeSessionManagerState,
        lease_id: &str,
        code: &'static str,
    ) {
        if let Some(entry) = state.leases.get(lease_id) {
            entry.terminal_state.store(
                runtime_session_terminal_state_from_code(code) as u8,
                Ordering::Release,
            );
        }
        let Some(entry) = state.leases.remove(lease_id) else {
            return;
        };
        let tombstone = RuntimeSessionTombstone::from_entry(&entry, code);
        if state
            .sid_index
            .get(&tombstone.sid)
            .is_some_and(|current| current == lease_id)
        {
            state.sid_index.remove(&tombstone.sid);
        }
        state.tombstones.insert(lease_id.to_string(), tombstone);
    }

    /// Validate one active runtime session against optional host-echoed identity fields.
    /// 使用可选宿主回传身份字段校验单个活跃运行时会话。
    fn validate_session_identity(
        session: &RuntimeSession,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
    ) -> Result<(), RuntimeSessionError> {
        Self::validate_identity_parts(
            &session.lease_id,
            &session.sid,
            session.generation,
            expected_sid,
            expected_generation,
        )
    }

    /// Validate one terminal runtime-session tombstone against optional host-echoed identity fields.
    /// 使用可选宿主回传身份字段校验单个终态运行时会话墓碑。
    fn validate_tombstone_identity(
        tombstone: &RuntimeSessionTombstone,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
    ) -> Result<(), RuntimeSessionError> {
        Self::validate_identity_parts(
            &tombstone.lease_id,
            &tombstone.sid,
            tombstone.generation,
            expected_sid,
            expected_generation,
        )
    }

    /// Validate one active runtime-session profile against the expected public or system surface.
    /// 使用预期的公开或系统表面对单个活跃运行时会话 profile 进行校验。
    fn validate_session_profile(
        session: &RuntimeSession,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<(), RuntimeSessionError> {
        if let Some(expected_profile) = expected_profile
            && session.profile != expected_profile
        {
            return Err(RuntimeSessionError {
                code: "lease_profile_mismatch",
                message: format!(
                    "runtime session lease `{}` belongs to profile `{}`, not `{}`",
                    session.lease_id,
                    session.profile.as_str(),
                    expected_profile.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Validate one terminal runtime-session tombstone profile against the expected public or system surface.
    /// 使用预期的公开或系统表面对单个终态运行时会话墓碑 profile 进行校验。
    fn validate_tombstone_profile(
        tombstone: &RuntimeSessionTombstone,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<(), RuntimeSessionError> {
        if let Some(expected_profile) = expected_profile
            && tombstone.profile != expected_profile
        {
            return Err(RuntimeSessionError {
                code: "lease_profile_mismatch",
                message: format!(
                    "runtime session lease `{}` belongs to profile `{}`, not `{}`",
                    tombstone.lease_id,
                    tombstone.profile.as_str(),
                    expected_profile.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Validate the stable SID and generation of one runtime-session record.
    /// 校验单个运行时会话记录的稳定 SID 与 generation。
    fn validate_identity_parts(
        lease_id: &str,
        actual_sid: &str,
        actual_generation: u64,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
    ) -> Result<(), RuntimeSessionError> {
        if let Some(expected_sid) = expected_sid
            && actual_sid != expected_sid
        {
            return Err(RuntimeSessionError {
                code: "lease_sid_mismatch",
                message: format!(
                    "runtime session lease `{lease_id}` belongs to sid `{actual_sid}`, not `{expected_sid}`"
                ),
            });
        }
        if let Some(expected_generation) = expected_generation
            && actual_generation != expected_generation
        {
            return Err(RuntimeSessionError {
                code: "lease_generation_mismatch",
                message: format!(
                    "runtime session lease `{lease_id}` generation mismatch: expected {expected_generation}, actual {actual_generation}"
                ),
            });
        }
        Ok(())
    }
}

impl RuntimeSession {
    /// Return the stable non-active error when this session can no longer serve host calls.
    /// 当当前会话不再能够服务宿主调用时返回稳定的非活跃错误。
    fn inactive_error(&self) -> Option<RuntimeSessionError> {
        if let Some(code) =
            runtime_session_terminal_code_from_state(self.terminal_state.load(Ordering::Acquire))
        {
            return Some(RuntimeSessionError {
                code,
                message: format!(
                    "{} (sid `{}`, generation {})",
                    runtime_session_terminal_message(code, &self.lease_id),
                    self.sid,
                    self.generation
                ),
            });
        }
        if self.closed {
            return Some(RuntimeSessionError {
                code: "lease_closed",
                message: format!("runtime session lease `{}` is closed", self.lease_id),
            });
        }
        if self.is_expired() {
            return Some(RuntimeSessionError {
                code: "lease_expired",
                message: format!("runtime session lease `{}` is expired", self.lease_id),
            });
        }
        None
    }

    /// Refresh the lease expiration after one accepted operation.
    /// 在一次已接受操作后刷新租约过期时间。
    fn refresh(&mut self) {
        let (expires_at, expires_at_unix_ms) = runtime_session_expiry(self.ttl_sec);
        self.expires_at = expires_at;
        self.expires_at_unix_ms = expires_at_unix_ms;
    }

    /// Return whether this runtime session is expired.
    /// 返回此运行时会话是否已经过期。
    fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
    }

    /// Return a JSON status payload for this runtime session.
    /// 返回此运行时会话的 JSON 状态载荷。
    fn status_payload(&self) -> Value {
        json!({
            "ok": runtime_session_terminal_code_from_state(
                self.terminal_state.load(Ordering::Acquire),
            ).is_none() && !self.closed && !self.is_expired(),
            "sid": self.sid.clone(),
            "lease_id": self.lease_id.clone(),
            "generation": self.generation,
            "profile": self.profile.as_str(),
            "lifetime": if self.ttl_sec.is_some() { "finite" } else { "infinite" },
            "cwd": session_status_cwd_text(self),
            "workspace_root": session_status_workspace_root_text(self),
            "system_lua_lib": session_status_system_lua_lib_text(self),
            "ttl_sec": self.ttl_sec,
            "expires_at_unix_ms": self.expires_at_unix_ms,
            "closed": self.closed,
            "expired": self.is_expired()
        })
    }

    /// Return a JSON payload for one successful close operation.
    /// 返回一次成功关闭操作的 JSON 载荷。
    fn close_payload(&self) -> Value {
        json!({
            "ok": true,
            "sid": self.sid.clone(),
            "lease_id": self.lease_id.clone(),
            "generation": self.generation,
            "profile": self.profile.as_str(),
            "lifetime": if self.ttl_sec.is_some() { "finite" } else { "infinite" },
            "cwd": session_status_cwd_text(self),
            "workspace_root": session_status_workspace_root_text(self),
            "system_lua_lib": session_status_system_lua_lib_text(self),
            "ttl_sec": self.ttl_sec,
            "expires_at_unix_ms": self.expires_at_unix_ms,
            "closed": self.closed,
            "expired": self.is_expired()
        })
    }
}

impl RuntimeSessionTombstone {
    /// Build one terminal tombstone from one active runtime session snapshot.
    /// 基于单个活跃运行时会话快照构建终态墓碑。
    fn from_session(session: &RuntimeSession, code: &'static str) -> Self {
        Self {
            sid: session.sid.clone(),
            lease_id: session.lease_id.clone(),
            generation: session.generation,
            profile: session.profile,
            code,
            retired_at: Instant::now(),
        }
    }

    /// Build one terminal tombstone from one active manager entry without parsing display JSON.
    /// 基于单个活跃管理器条目构建终态墓碑，不反解析展示 JSON。
    fn from_entry(entry: &RuntimeSessionEntry, code: &'static str) -> Self {
        Self {
            sid: entry.sid.clone(),
            lease_id: entry.lease_id.clone(),
            generation: entry.generation,
            profile: entry.profile,
            code,
            retired_at: Instant::now(),
        }
    }

    /// Convert this tombstone into one stable runtime-session error.
    /// 将当前墓碑转换为稳定的运行时会话错误。
    fn as_error(&self) -> RuntimeSessionError {
        RuntimeSessionError {
            code: self.code,
            message: format!(
                "{} (sid `{}`, generation {})",
                runtime_session_terminal_message(self.code, &self.lease_id),
                self.sid,
                self.generation
            ),
        }
    }
}

/// Return the current Unix timestamp in milliseconds.
/// 返回当前 Unix 毫秒时间戳。
fn unix_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// Return the host-visible cwd string stored on one runtime lease status payload.
/// 返回单个运行时租约状态载荷上记录的宿主可见 cwd 字符串。
fn session_status_cwd_text(session: &RuntimeSession) -> Option<String> {
    session
        .path_context
        .cwd
        .as_deref()
        .map(render_host_visible_path)
}

/// Return the host-visible workspace-root string stored on one runtime lease status payload.
/// 返回单个运行时租约状态载荷上记录的宿主可见工作区根目录字符串。
fn session_status_workspace_root_text(session: &RuntimeSession) -> Option<String> {
    session
        .path_context
        .workspace_root
        .as_deref()
        .map(render_host_visible_path)
}

/// Return the host-visible `system_lua_lib` directory string stored on one runtime lease status payload.
/// 返回单个运行时租约状态载荷上记录的宿主可见 `system_lua_lib` 目录字符串。
fn session_status_system_lua_lib_text(session: &RuntimeSession) -> Option<String> {
    session
        .path_context
        .system_lua_lib_dir
        .as_deref()
        .map(render_host_visible_path)
}

/// Calculate monotonic and host-visible expiration timestamps.
/// 计算单调时间与宿主可见的过期时间戳。
fn runtime_session_expiry(ttl_sec: Option<u64>) -> (Option<Instant>, Option<u128>) {
    let Some(ttl_sec) = ttl_sec else {
        return (None, None);
    };
    let ttl = Duration::from_secs(ttl_sec);
    (
        Some(Instant::now() + ttl),
        Some(unix_time_millis().saturating_add(ttl.as_millis())),
    )
}

/// Return the tombstone retention window for terminal runtime session records.
/// 返回终态运行时会话记录的墓碑保留时间窗口。
fn runtime_session_tombstone_ttl() -> Duration {
    Duration::from_secs(3_600)
}

/// Convert one stable terminal error code into its shared atomic terminal-state value.
/// 将稳定终态错误码转换为共享原子终态状态值。
fn runtime_session_terminal_state_from_code(code: &'static str) -> RuntimeSessionTerminalState {
    match code {
        "lease_closed" => RuntimeSessionTerminalState::Closed,
        "lease_expired" => RuntimeSessionTerminalState::Expired,
        "lease_replaced" => RuntimeSessionTerminalState::Replaced,
        _ => RuntimeSessionTerminalState::Active,
    }
}

/// Convert one shared atomic terminal-state value back into its stable terminal error code.
/// 将共享原子终态状态值转换回稳定终态错误码。
fn runtime_session_terminal_code_from_state(state: u8) -> Option<&'static str> {
    match state {
        value if value == RuntimeSessionTerminalState::Closed as u8 => Some("lease_closed"),
        value if value == RuntimeSessionTerminalState::Expired as u8 => Some("lease_expired"),
        value if value == RuntimeSessionTerminalState::Replaced as u8 => Some("lease_replaced"),
        _ => None,
    }
}

/// Compare two runtime-session entries for stable host-visible listing order.
/// 比较两个运行时会话条目以生成稳定的宿主可见列表顺序。
fn compare_runtime_session_entries(
    left: &RuntimeSessionEntry,
    right: &RuntimeSessionEntry,
) -> std::cmp::Ordering {
    left.sid
        .cmp(&right.sid)
        .then_with(|| left.generation.cmp(&right.generation))
        .then_with(|| left.lease_id.cmp(&right.lease_id))
}

/// Build one stable human-readable message for one terminal runtime-session state.
/// 为单个运行时会话终态构建稳定的人类可读消息。
fn runtime_session_terminal_message(code: &'static str, lease_id: &str) -> String {
    match code {
        "lease_closed" => format!("runtime session lease `{lease_id}` is closed"),
        "lease_expired" => format!("runtime session lease `{lease_id}` is expired"),
        "lease_replaced" => format!("runtime session lease `{lease_id}` was replaced"),
        _ => format!("runtime session lease `{lease_id}` is not active"),
    }
}

/// Build a stable JSON error payload for runtime session operations.
/// 为运行时会话操作构建稳定 JSON 错误载荷。
fn runtime_session_error_payload(error: RuntimeSessionError) -> Value {
    json!({
        "ok": false,
        "error_code": error.code,
        "message": error.message
    })
}

/// Validate and normalize one runtime session SID.
/// 校验并归一化单个运行时会话 SID。
fn normalize_runtime_session_sid(value: &str) -> Result<String, String> {
    let sid = value.trim();
    if sid.is_empty() {
        return Err("runtime session sid must not be empty".to_string());
    }
    if sid.len() > 128 {
        return Err("runtime session sid must not exceed 128 bytes".to_string());
    }
    if sid.contains('\0') {
        return Err("runtime session sid must not contain NUL bytes".to_string());
    }
    Ok(sid.to_string())
}

/// Validate and normalize one optional host-provided lease path.
/// 校验并归一化单个可选宿主租约路径。
fn normalize_optional_runtime_lease_path(
    value: Option<&str>,
    field_name: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.contains('\0') {
        return Err(format!("{field_name} must not contain NUL bytes"));
    }
    #[cfg(windows)]
    if has_invalid_windows_path_syntax(trimmed) {
        return Err(format!(
            "{field_name} contains unsupported Windows path syntax"
        ));
    }
    Ok(Some(PathBuf::from(trimmed)))
}

/// Validate and normalize one host-provided lease path list.
/// 校验并归一化一组宿主租约路径列表。
fn normalize_runtime_lease_path_list(
    values: &[String],
    field_name: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut normalized = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let item = normalize_optional_runtime_lease_path(
            Some(value.as_str()),
            &format!("{field_name}[{index}]"),
        )?;
        if let Some(item) = item {
            normalized.push(item);
        }
    }
    Ok(normalized)
}

impl LuaEngine {
    /// Return the effective fixed `system_lua_lib` directory for the current engine.
    /// 返回当前引擎生效的固定 `system_lua_lib` 目录。
    fn resolve_system_lua_lib_dir(&self) -> PathBuf {
        self.host_options
            .system_lua_lib_dir
            .clone()
            .or_else(|| {
                self.runtime_skill_roots
                    .first()
                    .map(|root| root.skills_dir.clone())
            })
            .unwrap_or_else(|| PathBuf::from("skills"))
    }
    /// Resolve the effective TTL semantics of one runtime lease create request.
    /// 解析单个运行时租约创建请求的生效 TTL 语义。
    fn resolve_runtime_lease_ttl(
        profile: RuntimeLeaseProfile,
        requested_ttl_sec: Option<u64>,
    ) -> Result<Option<u64>, String> {
        match profile {
            RuntimeLeaseProfile::Public => match requested_ttl_sec {
                Some(0) => Err("runtime lease ttl_sec must be greater than 0".to_string()),
                Some(value) => Ok(Some(value.clamp(1, 3_600))),
                None => Ok(Some(default_runtime_session_ttl_sec())),
            },
            RuntimeLeaseProfile::SystemLuaLib => match requested_ttl_sec {
                None | Some(0) => Ok(None),
                Some(value) => Ok(Some(value.clamp(1, 3_600))),
            },
        }
    }
    /// Resolve one host-owned runtime lease path context from the create request and lease profile.
    /// 根据创建请求与租约 profile 解析一份宿主拥有的运行时租约路径上下文。
    fn resolve_runtime_lease_path_context(
        &self,
        request: &RuntimeSessionCreateRequest,
        profile: RuntimeLeaseProfile,
    ) -> Result<RuntimeLeasePathContext, String> {
        let mut context = RuntimeLeasePathContext {
            cwd: normalize_optional_runtime_lease_path(request.cwd.as_deref(), "cwd")?,
            workspace_root: normalize_optional_runtime_lease_path(
                request.workspace_root.as_deref(),
                "workspace_root",
            )?,
            lua_roots: normalize_runtime_lease_path_list(&request.lua_roots, "lua_roots")?,
            c_roots: normalize_runtime_lease_path_list(&request.c_roots, "c_roots")?,
            mounts: request.mounts.clone(),
            system_lua_lib_dir: None,
        };
        if !context.mounts.is_object() && !context.mounts.is_null() {
            return Err("runtime lease mounts must be one JSON object when present".to_string());
        }
        if profile == RuntimeLeaseProfile::SystemLuaLib {
            let system_dir = self.resolve_system_lua_lib_dir();
            std::fs::create_dir_all(&system_dir).map_err(|error| {
                format!(
                    "failed to create system_lua_lib_dir {}: {}",
                    render_host_visible_path(&system_dir),
                    error
                )
            })?;
            context.system_lua_lib_dir = Some(system_dir.clone());
            if context.cwd.is_none() {
                context.cwd = Some(system_dir.clone());
            }
            if !context.lua_roots.iter().any(|root| root == &system_dir) {
                context.lua_roots.insert(0, system_dir);
            }
        }
        Ok(context)
    }
    /// Decode one existing Lua package search path before prepending lease module roots.
    /// 在前置租约模块根目录前，解码单个已有 Lua package 搜索路径。
    ///
    /// Parameters: `value` is the Lua string read from `package.path` or `package.cpath`.
    /// 参数说明：`value` 是从 `package.path` 或 `package.cpath` 读取的 Lua 字符串。
    ///
    /// Parameters: `field_name` is the package field name included in diagnostics.
    /// 参数说明：`field_name` 是诊断信息中使用的 package 字段名。
    ///
    /// Returns the decoded UTF-8 path text or a descriptive runtime lease initialization error.
    /// 返回解码后的 UTF-8 路径文本，或描述性的运行时租约初始化错误。
    fn runtime_lease_package_search_path_text(
        value: &mlua::String,
        field_name: &str,
    ) -> Result<String, String> {
        value
            .to_str()
            .map(|text| text.to_string())
            .map_err(|error| {
                format!("runtime lease package.{field_name} is not valid UTF-8: {error}")
            })
    }

    /// Prepend one set of host-owned Lua and native module roots to one lease VM.
    /// 将一组宿主拥有的 Lua 与原生模块根目录前置到单个租约虚拟机上。
    fn configure_runtime_lease_vm(
        lua: &Lua,
        path_context: &RuntimeLeasePathContext,
    ) -> Result<(), String> {
        let package: Table = lua.globals().get("package").map_err(|error| {
            format!(
                "Failed to get Lua package table for runtime lease: {}",
                error
            )
        })?;
        let old_cpath: mlua::String = package.get("cpath").map_err(|error| {
            format!("Failed to read package.cpath for runtime lease: {}", error)
        })?;
        let old_path: mlua::String = package
            .get("path")
            .map_err(|error| format!("Failed to read package.path for runtime lease: {}", error))?;
        let mut cpath_prefix = String::new();
        for root in &path_context.c_roots {
            #[cfg(windows)]
            {
                cpath_prefix.push_str(&format!(
                    "{}\\?.dll;{}\\?\\init.dll;",
                    root.display(),
                    root.display()
                ));
            }
            #[cfg(target_os = "linux")]
            {
                cpath_prefix.push_str(&format!(
                    "{}/?.so;{}/?/init.so;",
                    root.display(),
                    root.display()
                ));
            }
            #[cfg(target_os = "macos")]
            {
                cpath_prefix.push_str(&format!(
                    "{}/?.dylib;{}/?/init.dylib;",
                    root.display(),
                    root.display()
                ));
            }
        }
        let mut path_prefix = String::new();
        for root in &path_context.lua_roots {
            #[cfg(windows)]
            {
                path_prefix.push_str(&format!(
                    "{}\\?.lua;{}\\?\\init.lua;",
                    root.display(),
                    root.display()
                ));
            }
            #[cfg(unix)]
            {
                path_prefix.push_str(&format!(
                    "{}/?.lua;{}/?/init.lua;",
                    root.display(),
                    root.display()
                ));
            }
        }
        if !cpath_prefix.is_empty() {
            // Existing native module search path must survive prefixing unless Lua exposes invalid bytes.
            // 已有原生模块搜索路径必须在前置拼接后保留，除非 Lua 暴露了非法字节。
            let old_cpath_text = Self::runtime_lease_package_search_path_text(&old_cpath, "cpath")?;
            let new_cpath = format!("{}{}", cpath_prefix, old_cpath_text);
            package
                .set(
                    "cpath",
                    lua.create_string(&new_cpath).map_err(|error| {
                        format!("Failed to build runtime lease cpath string: {}", error)
                    })?,
                )
                .map_err(|error| format!("Failed to set runtime lease package.cpath: {}", error))?;
        }
        if !path_prefix.is_empty() {
            // Existing Lua module search path must survive prefixing unless Lua exposes invalid bytes.
            // 已有 Lua 模块搜索路径必须在前置拼接后保留，除非 Lua 暴露了非法字节。
            let old_path_text = Self::runtime_lease_package_search_path_text(&old_path, "path")?;
            let new_path = format!("{}{}", path_prefix, old_path_text);
            package
                .set(
                    "path",
                    lua.create_string(&new_path).map_err(|error| {
                        format!("Failed to build runtime lease path string: {}", error)
                    })?,
                )
                .map_err(|error| format!("Failed to set runtime lease package.path: {}", error))?;
        }
        Ok(())
    }
    /// Evaluate one Lua chunk while temporarily switching the process cwd when the lease owns one cwd.
    /// 当租约拥有 cwd 时，在临时切换进程工作目录后执行一段 Lua 代码。
    fn eval_lua_value_with_optional_cwd(
        lua: &Lua,
        wrapper: &str,
        cwd: Option<&Path>,
    ) -> Result<LuaValue, mlua::Error> {
        match cwd {
            Some(cwd) => {
                let _cwd_guard = lock_runlua_cwd_guard();
                let original_dir = std::env::current_dir().map_err(|error| {
                    mlua::Error::runtime(format!("runtime lease cwd: {}", error))
                })?;
                std::env::set_current_dir(cwd).map_err(|error| {
                    mlua::Error::runtime(format!(
                        "runtime lease set cwd {}: {}",
                        render_host_visible_path(cwd),
                        error
                    ))
                })?;
                let execution = lua.load(wrapper).eval::<LuaValue>();
                let restore_result = std::env::set_current_dir(&original_dir).map_err(|error| {
                    mlua::Error::runtime(format!("runtime lease restore cwd: {}", error))
                });
                match (execution, restore_result) {
                    (Ok(value), Ok(())) => Ok(value),
                    (Err(error), Ok(())) => Err(error),
                    (_, Err(error)) => Err(error),
                }
            }
            None => lua.load(wrapper).eval::<LuaValue>(),
        }
    }
    /// Create one persistent runtime lease and return a stable JSON response.
    /// 创建一个持久运行时租约并返回稳定 JSON 响应。
    pub fn create_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        self.create_runtime_session_with_profile_json(request_json, RuntimeLeaseProfile::Public)
    }
    /// Create one persistent runtime lease under the selected profile and return a stable JSON response.
    /// 在所选 profile 下创建一个持久运行时租约并返回稳定 JSON 响应。
    fn create_runtime_session_with_profile_json(
        &self,
        request_json: &str,
        profile: RuntimeLeaseProfile,
    ) -> Result<String, String> {
        let mut request: RuntimeSessionCreateRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid runtime session create JSON: {}", error))?;
        request.sid = normalize_runtime_session_sid(&request.sid)?;
        let ttl_sec = Self::resolve_runtime_lease_ttl(profile, request.ttl_sec)?;
        let path_context = self.resolve_runtime_lease_path_context(&request, profile)?;
        let vm = Self::create_runlua_vm(
            &self.skills,
            &self.entry_registry,
            self.host_options.clone(),
            self.skill_config_store.clone(),
            self.runtime_skill_roots.clone(),
            self.lancedb_host.clone(),
            self.sqlite_host.clone(),
        )?;
        Self::configure_runtime_lease_vm(&vm.lua, &path_context)?;
        Self::install_managed_io_compat_for_runtime(&vm.lua, self.host_options.as_ref())?;
        let payload = self
            .runtime_sessions
            .insert(
                profile,
                request.sid,
                ttl_sec,
                request.replace,
                path_context,
                vm,
            )
            .unwrap_or_else(runtime_session_error_payload);
        serde_json::to_string(&payload)
            .map_err(|error| format!("Runtime session create JSON encode failed: {}", error))
    }
    /// Evaluate Lua code inside one persistent runtime lease and return a stable JSON response.
    /// 在一个持久运行时租约中执行 Lua 代码并返回稳定 JSON 响应。
    pub fn eval_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        self.eval_runtime_session_with_profile_json(request_json, RuntimeLeaseProfile::Public)
    }
    /// Evaluate Lua code inside one persistent runtime lease under the selected profile.
    /// 在所选 profile 下的持久运行时租约中执行 Lua 代码。
    fn eval_runtime_session_with_profile_json(
        &self,
        request_json: &str,
        expected_profile: RuntimeLeaseProfile,
    ) -> Result<String, String> {
        let mut request: RuntimeSessionEvalRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid runtime session eval JSON: {}", error))?;
        if let Some(sid) = request.sid.as_mut() {
            *sid = normalize_runtime_session_sid(sid)?;
        }
        if request.timeout_ms == 0 {
            return Err("runtime session eval timeout_ms must be greater than 0".to_string());
        }
        let session = match self.runtime_sessions.get(
            &request.lease_id,
            request.sid.as_deref(),
            request.generation,
            Some(expected_profile),
        ) {
            Ok(session) => session,
            Err(error) => {
                return serde_json::to_string(&runtime_session_error_payload(error)).map_err(
                    |encode_error| {
                        format!("Runtime session eval JSON encode failed: {}", encode_error)
                    },
                );
            }
        };
        let mut session = match RuntimeSessionManager::try_lock_session(&session, &request.lease_id)
        {
            Ok(session) => session,
            Err(error) => {
                let payload = runtime_session_error_payload(error);
                return serde_json::to_string(&payload).map_err(|error| {
                    format!("Runtime session eval JSON encode failed: {}", error)
                });
            }
        };
        let (payload, refreshed_snapshot) = match Self::ensure_runtime_session_active(&mut session)
        {
            Ok(()) => match self.eval_runtime_session_locked(&mut session, &request) {
                Ok(result) => {
                    session.refresh();
                    (
                        json!({
                            "ok": true,
                            "sid": session.sid.clone(),
                            "lease_id": session.lease_id.clone(),
                            "generation": session.generation,
                            "profile": session.profile.as_str(),
                            "lifetime": if session.ttl_sec.is_some() { "finite" } else { "infinite" },
                            "cwd": session_status_cwd_text(&session),
                            "system_lua_lib": session_status_system_lua_lib_text(&session),
                            "expires_at_unix_ms": session.expires_at_unix_ms,
                            "result": result
                        }),
                        Some(session.status_payload()),
                    )
                }
                Err(message) => (
                    runtime_session_error_payload(RuntimeSessionError {
                        code: "eval_failed",
                        message,
                    }),
                    Some(session.status_payload()),
                ),
            },
            Err(error) => (runtime_session_error_payload(error), None),
        };
        drop(session);
        if let Some(snapshot) = refreshed_snapshot {
            let _ = self
                .runtime_sessions
                .update_active_snapshot(&request.lease_id, snapshot);
        }
        serde_json::to_string(&payload)
            .map_err(|error| format!("Runtime session eval JSON encode failed: {}", error))
    }
    /// Return status for one persistent runtime lease as JSON.
    /// 以 JSON 返回一个持久运行时租约的状态。
    pub fn runtime_lease_status_json(&self, request_json: &str) -> Result<String, String> {
        self.runtime_session_status_with_profile_json(request_json, RuntimeLeaseProfile::Public)
    }
    /// Return status for one persistent runtime lease under the selected profile as JSON.
    /// 以 JSON 返回所选 profile 下的持久运行时租约状态。
    fn runtime_session_status_with_profile_json(
        &self,
        request_json: &str,
        expected_profile: RuntimeLeaseProfile,
    ) -> Result<String, String> {
        let mut request: RuntimeSessionLeaseRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid runtime session status JSON: {}", error))?;
        if let Some(sid) = request.sid.as_mut() {
            *sid = normalize_runtime_session_sid(sid)?;
        }
        let payload = self
            .runtime_sessions
            .status(
                &request.lease_id,
                request.sid.as_deref(),
                request.generation,
                Some(expected_profile),
            )
            .unwrap_or_else(runtime_session_error_payload);
        serde_json::to_string(&payload)
            .map_err(|error| format!("Runtime session status JSON encode failed: {}", error))
    }
    /// List active persistent runtime leases and return a stable JSON response.
    /// 列出活跃的持久运行时租约并返回稳定 JSON 响应。
    pub fn list_runtime_leases_json(&self, request_json: &str) -> Result<String, String> {
        self.list_runtime_sessions_with_profile_json(request_json, RuntimeLeaseProfile::Public)
    }
    /// List active persistent runtime leases under the selected profile and return a stable JSON response.
    /// 列出所选 profile 下活跃的持久运行时租约并返回稳定 JSON 响应。
    fn list_runtime_sessions_with_profile_json(
        &self,
        request_json: &str,
        expected_profile: RuntimeLeaseProfile,
    ) -> Result<String, String> {
        let mut request: RuntimeSessionListRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid runtime session list JSON: {}", error))?;
        if let Some(sid) = request.sid.as_mut() {
            *sid = normalize_runtime_session_sid(sid)?;
        }
        let payload = self
            .runtime_sessions
            .list(request.sid.as_deref(), Some(expected_profile))
            .unwrap_or_else(runtime_session_error_payload);
        serde_json::to_string(&payload)
            .map_err(|error| format!("Runtime session list JSON encode failed: {}", error))
    }
    /// Close one persistent runtime lease and return its final status as JSON.
    /// 关闭一个持久运行时租约并以 JSON 返回其最终状态。
    pub fn close_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        self.close_runtime_session_with_profile_json(request_json, RuntimeLeaseProfile::Public)
    }
    /// Close one persistent runtime lease under the selected profile and return its final status as JSON.
    /// 关闭所选 profile 下的持久运行时租约并以 JSON 返回最终状态。
    fn close_runtime_session_with_profile_json(
        &self,
        request_json: &str,
        expected_profile: RuntimeLeaseProfile,
    ) -> Result<String, String> {
        let mut request: RuntimeSessionLeaseRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid runtime session close JSON: {}", error))?;
        if let Some(sid) = request.sid.as_mut() {
            *sid = normalize_runtime_session_sid(sid)?;
        }
        let payload = self
            .runtime_sessions
            .close(
                &request.lease_id,
                request.sid.as_deref(),
                request.generation,
                Some(expected_profile),
            )
            .unwrap_or_else(runtime_session_error_payload);
        serde_json::to_string(&payload)
            .map_err(|error| format!("Runtime session close JSON encode failed: {}", error))
    }
    /// Create one `system_lua_lib` runtime lease through the dedicated system surface.
    /// 通过专用 system 接口创建一个 `system_lua_lib` 运行时租约。
    pub fn create_system_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        self.create_runtime_session_with_profile_json(
            request_json,
            RuntimeLeaseProfile::SystemLuaLib,
        )
    }
    /// Evaluate one `system_lua_lib` runtime lease through the dedicated system surface.
    /// 通过专用 system 接口执行一个 `system_lua_lib` 运行时租约。
    pub fn eval_system_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        self.eval_runtime_session_with_profile_json(request_json, RuntimeLeaseProfile::SystemLuaLib)
    }
    /// Return status for one `system_lua_lib` runtime lease through the dedicated system surface.
    /// 通过专用 system 接口返回单个 `system_lua_lib` 运行时租约状态。
    pub fn system_runtime_lease_status_json(&self, request_json: &str) -> Result<String, String> {
        self.runtime_session_status_with_profile_json(
            request_json,
            RuntimeLeaseProfile::SystemLuaLib,
        )
    }
    /// List `system_lua_lib` runtime leases through the dedicated system surface.
    /// 通过专用 system 接口列出 `system_lua_lib` 运行时租约。
    pub fn list_system_runtime_leases_json(&self, request_json: &str) -> Result<String, String> {
        self.list_runtime_sessions_with_profile_json(
            request_json,
            RuntimeLeaseProfile::SystemLuaLib,
        )
    }
    /// Close one `system_lua_lib` runtime lease through the dedicated system surface.
    /// 通过专用 system 接口关闭单个 `system_lua_lib` 运行时租约。
    pub fn close_system_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        self.close_runtime_session_with_profile_json(
            request_json,
            RuntimeLeaseProfile::SystemLuaLib,
        )
    }
    /// Install the managed Lua `io` compatibility table in a persistent runtime VM.
    /// 在持久运行时 VM 中安装托管 Lua `io` 兼容表。
    fn install_managed_io_compat_for_runtime(
        lua: &Lua,
        host_options: &LuaRuntimeHostOptions,
    ) -> Result<(), String> {
        if !host_options.capabilities.enable_managed_io_compat {
            return Ok(());
        }
        let default_encoding = resolve_host_default_text_encoding(host_options)?;
        let vulcan = get_vulcan_table(lua)?;
        let vulcan_io = vulcan
            .get::<Table>("io")
            .map_err(|error| format!("Failed to get vulcan.io: {}", error))?;
        install_managed_io_compat(lua, &vulcan_io, default_encoding).map_err(|error| {
            format!(
                "Failed to install managed io compatibility for runtime session: {}",
                error
            )
        })
    }
    /// Ensure one locked runtime session can still execute.
    /// 确保一个已锁定运行时会话仍可执行。
    pub(super) fn ensure_runtime_session_active(
        session: &mut RuntimeSession,
    ) -> Result<(), RuntimeSessionError> {
        if let Some(error) = session.inactive_error() {
            return Err(error);
        }
        if session.ttl_sec.is_some() {
            session.refresh();
        }
        Ok(())
    }
    /// Evaluate one request while holding the selected runtime session lock.
    /// 持有所选运行时会话锁时执行一个请求。
    fn eval_runtime_session_locked(
        &self,
        session: &mut RuntimeSession,
        request: &RuntimeSessionEvalRequest,
    ) -> Result<Value, String> {
        reset_pooled_vm_request_scope(&session.vm.lua, self.host_options.as_ref())?;
        let invocation_context = request.to_invocation_context();
        Self::populate_anonymous_lua_context(
            &session.vm.lua,
            AnonymousLuaExecutionContext {
                invocation_context: Some(&invocation_context),
                internal_context: VulcanInternalExecutionContext {
                    tool_name: None,
                    skill_name: None,
                    entry_name: None,
                    root_name: None,
                    luaexec_active: true,
                    luaexec_caller_tool_name: None,
                },
                entry_file: None,
                dependency_context: AnonymousLuaDependencyContext::ClearWithHostOptions(
                    self.host_options.as_ref(),
                ),
            },
        )?;
        let args_table = json_to_lua_table(&session.vm.lua, &request.args)?;
        session
            .vm
            .lua
            .globals()
            .set("__runlua_args", args_table)
            .map_err(|error| format!("Failed to set runtime session args: {}", error))?;
        let wrapper = format!(
            "return (function()\n  local args = __runlua_args\n  {}\nend)()",
            request.code
        );
        Self::install_runlua_timeout_guard(&session.vm.lua, request.timeout_ms)
            .map_err(|error| error.to_string())?;
        let eval_result = Self::eval_lua_value_with_optional_cwd(
            &session.vm.lua,
            &wrapper,
            session.path_context.cwd.as_deref(),
        );
        Self::remove_runlua_timeout_guard(&session.vm.lua);
        let result = eval_result.map_err(|error| {
            let msg = format!("Runtime session eval error: {}", error);
            log_error(format!("[LuaSkill:error] {}", msg));
            msg
        })?;
        let json_result = lua_value_to_json(&result)?;
        clear_runlua_args_global(&session.vm.lua)?;
        Ok(json_result)
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeSessionManager;
    use crate::runtime::path::render_host_visible_path;
    use std::panic::{self, AssertUnwindSafe};

    /// Verify runtime lease package search paths keep valid UTF-8 text unchanged.
    /// 验证运行时租约 package 搜索路径会原样保留合法 UTF-8 文本。
    #[test]
    fn runtime_lease_package_search_path_text_preserves_valid_utf8() {
        // Lua VM used only to allocate a package search path string.
        // 仅用于分配 package 搜索路径字符串的 Lua 虚拟机。
        let lua = mlua::Lua::new();
        // Existing search path fixture that should survive decoding unchanged.
        // 解码后应保持不变的已有搜索路径夹具。
        let search_path = lua
            .create_string("?.lua;?/init.lua;")
            .expect("valid package search path string should be created");
        // Decoded search path returned by the runtime lease helper.
        // 运行时租约辅助函数返回的已解码搜索路径。
        let decoded =
            super::LuaEngine::runtime_lease_package_search_path_text(&search_path, "path")
                .expect("valid package search path should decode");

        assert_eq!(decoded, "?.lua;?/init.lua;");
    }

    /// Verify runtime lease package search paths reject invalid UTF-8 instead of dropping old paths.
    /// 验证运行时租约 package 搜索路径会拒绝非法 UTF-8，而不是丢弃旧路径。
    #[test]
    fn runtime_lease_package_search_path_text_rejects_invalid_utf8() {
        // Lua VM used only to allocate an invalid package search path string.
        // 仅用于分配非法 package 搜索路径字符串的 Lua 虚拟机。
        let lua = mlua::Lua::new();
        // Invalid search path bytes that cannot be represented as UTF-8 text.
        // 无法表示为 UTF-8 文本的非法搜索路径字节。
        let search_path = lua
            .create_string([0xff])
            .expect("invalid package search path bytes should be created");
        // Error returned when the runtime lease helper refuses to erase the existing path.
        // 运行时租约辅助函数拒绝擦除已有路径时返回的错误。
        let error = super::LuaEngine::runtime_lease_package_search_path_text(&search_path, "cpath")
            .expect_err("invalid package search path should be rejected");

        assert!(
            error.contains("runtime lease package.cpath is not valid UTF-8"),
            "unexpected error: {}",
            error
        );
    }

    /// Verify the runtime session manager state remains accessible after its lock is poisoned.
    /// 验证运行时会话管理器状态锁 poison 后仍可继续访问。
    #[test]
    fn runtime_session_manager_state_recovers_after_poisoned_lock() {
        // Manager whose internal state lock is intentionally poisoned and then recovered.
        // 内部状态锁会被故意制造 poison 并随后恢复的管理器。
        let manager = RuntimeSessionManager::new();
        // Captured panic result from a holder that poisons only the manager state lock.
        // 单个管理器状态锁持有者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the runtime session manager state lock.
            // 仅用于制造运行时会话管理器状态锁 poison 的保护对象。
            let _guard = manager
                .state
                .lock()
                .expect("initial runtime session manager lock");
            panic!("poison runtime session manager state for recovery test");
        }));

        assert!(poison_result.is_err());

        // Recovered manager state guard used to prove the lease indexes remain reachable.
        // 用于证明租约索引仍可访问的已恢复管理器状态保护对象。
        let state = manager
            .lock_state()
            .expect("recover runtime session manager state");
        assert!(state.leases.is_empty());
        assert!(state.sid_index.is_empty());
    }

    /// Verify runtime lease cwd errors render paths through the host-visible formatter.
    /// 验证运行时租约 cwd 错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn runtime_lease_cwd_error_uses_host_visible_path() {
        // Temporary root that isolates the invalid cwd fixture.
        // 隔离非法 cwd 夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_runtime_lease_cwd_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // File path intentionally passed as cwd so set_current_dir fails.
        // 有意作为 cwd 传入的文件路径，用于触发 set_current_dir 失败。
        let cwd_path = temp_root.join("not-a-directory.txt");
        std::fs::write(&cwd_path, b"not a directory").expect("cwd fixture file should be written");
        // Lua VM used only to call the lease cwd wrapper directly.
        // 仅用于直接调用租约 cwd 包装器的 Lua 虚拟机。
        let lua = mlua::Lua::new();
        // Error returned by the real cwd-switching evaluation path.
        // 真实 cwd 切换执行路径返回的错误。
        let error =
            super::LuaEngine::eval_lua_value_with_optional_cwd(&lua, "return 1", Some(&cwd_path))
                .expect_err("file cwd should fail")
                .to_string();
        // Expected diagnostic fragment rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断片段。
        let expected_fragment = format!(
            "runtime lease set cwd {}:",
            render_host_visible_path(&cwd_path)
        );

        assert!(
            error.contains(&expected_fragment),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }
}
