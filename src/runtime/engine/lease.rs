#[cfg(windows)]
use super::runlua::has_invalid_windows_path_syntax;
use super::runlua::{
    default_runlua_timeout_ms, lock_runlua_cwd_guard, resolve_host_default_text_encoding,
};
use super::*;
use crate::runtime::managed_package::{
    ManagedFilesystemObjectIdentity, ManagedRuntimeLeaseBinding,
    capture_managed_directory_identity, validate_lua_search_root_path,
    validate_managed_directory_identity,
};
use crate::runtime::path::{host_process_path_argument, normalize_host_input_path_text};
use mlua::{MetaMethod, UserData, UserDataMethods};

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

/// Required System Plugin package descriptor accepted only by the System lease surface.
/// 仅由 System 租约接口接收的必需 System Plugin 包描述符。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemRuntimePackageRequest {
    /// Stable System Plugin package identifier.
    /// 稳定的 System Plugin 包标识符。
    id: String,
    /// Absolute package root under the configured system Lua trust root.
    /// 位于已配置 System Lua 信任根下的绝对包根目录。
    root: String,
    /// Package-relative dependency manifest file.
    /// 包相对依赖清单文件。
    dependencies_file: String,
}

/// Strict System runtime-session creation request accepted by the dedicated host surface.
/// 专用宿主接口接收的严格 System 运行时会话创建请求。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemRuntimeSessionCreateRequest {
    /// Stable System session identifier supplied by the host.
    /// 宿主提供的稳定 System 会话标识。
    sid: String,
    /// Requested System lease TTL in seconds; zero means infinite.
    /// 请求的 System 租约有效期秒数；零表示无限期。
    #[serde(default)]
    ttl_sec: Option<u64>,
    /// Whether an existing System lease with the same SID should be replaced.
    /// 是否替换同一 SID 下已有的 System 租约。
    #[serde(default)]
    replace: bool,
    /// Optional host-controlled working directory.
    /// 可选的宿主控制工作目录。
    #[serde(default)]
    cwd: Option<String>,
    /// Optional host-owned workspace root exposed through the package context.
    /// 通过包上下文暴露的可选宿主工作区根目录。
    #[serde(default)]
    workspace_root: Option<String>,
    /// Structured host-owned mount metadata.
    /// 结构化宿主挂载元数据。
    #[serde(default = "default_runlua_exec_args")]
    mounts: Value,
    /// Required trusted System Plugin package descriptor.
    /// 必需的可信 System Plugin 包描述符。
    system_package: SystemRuntimePackageRequest,
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
    /// Activation-time native identity for a security-sensitive System lease cwd.
    /// 安全敏感 System 租约 cwd 的激活时平台原生身份。
    cwd_identity: Option<ManagedFilesystemObjectIdentity>,
    /// Optional workspace root tracked for host-side identity and prompt assembly.
    /// 供宿主侧身份与提示词装配跟踪的可选工作区根目录。
    workspace_root: Option<PathBuf>,
    /// Activation-time native identity for the optional System workspace authorization anchor.
    /// 可选 System 工作区授权锚点的激活时平台原生身份。
    workspace_root_identity: Option<ManagedFilesystemObjectIdentity>,
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
    /// Trusted managed-runtime package context installed for System Plugin execution.
    /// 为 System Plugin 执行安装的可信受管运行时包上下文。
    managed_package: Option<Arc<ManagedRuntimePackageContext>>,
}

impl RuntimeLeasePathContext {
    /// Revalidate every security-sensitive System lease directory before one evaluation.
    /// 在单次执行前重新校验每个安全敏感的 System 租约目录。
    ///
    /// Returns unit for public contexts or unchanged System cwd/workspace objects.
    /// 对公开上下文或未变化的 System cwd/工作区对象返回空值。
    fn validate_live_system_directory_identities(&self) -> Result<(), String> {
        if let Some(identity) = self.workspace_root_identity.as_ref() {
            let workspace_root = self.workspace_root.as_deref().ok_or_else(|| {
                "system runtime workspace_root identity has no canonical path".to_string()
            })?;
            validate_managed_directory_identity(workspace_root, identity, "workspace_root")?;
        }
        if let Some(identity) = self.cwd_identity.as_ref() {
            let cwd = self
                .cwd
                .as_deref()
                .ok_or_else(|| "system runtime cwd identity has no canonical path".to_string())?;
            validate_managed_directory_identity(cwd, identity, "cwd")?;
        }
        Ok(())
    }
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
    /// Managed package owners detached by pruning, replacement, or close and awaiting service retirement.
    /// 因清理、替换或关闭而分离并等待服务退役的受管包所有者。
    retired_owner_tokens: Vec<u64>,
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
    /// Exact managed package lifetime owner, absent only for public host-owned leases.
    /// 精确受管包生命周期所有者；仅公开宿主所有租约不存在该值。
    owner_token: Option<u64>,
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

/// Allocate the next manager-local lease sequence without wraparound.
/// 在不发生回绕的前提下分配下一个管理器局部租约序号。
///
/// `current` is the most recently published sequence.
/// `current` 是最近发布的序号。
///
/// Return the next unique sequence or a stable exhaustion error.
/// 返回下一个唯一序号或稳定的耗尽错误。
fn next_runtime_session_sequence(current: u64) -> Result<u64, RuntimeSessionError> {
    current.checked_add(1).ok_or_else(|| RuntimeSessionError {
        code: "lease_sequence_exhausted",
        message: "runtime session lease sequence is exhausted".to_string(),
    })
}

/// Allocate the next SID-local generation without identity reuse.
/// 在不复用身份的前提下分配下一个 SID 局部代际。
///
/// `current` is the most recently published generation and `sid` labels diagnostics.
/// `current` 是最近发布的代际，`sid` 用于标记诊断。
///
/// Return the next unique generation or a stable exhaustion error.
/// 返回下一个唯一代际或稳定的耗尽错误。
fn next_runtime_session_generation(current: u64, sid: &str) -> Result<u64, RuntimeSessionError> {
    current.checked_add(1).ok_or_else(|| RuntimeSessionError {
        code: "lease_generation_exhausted",
        message: format!("runtime session SID `{sid}` generation is exhausted"),
    })
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
                retired_owner_tokens: Vec::new(),
            }),
        }
    }

    /// Insert one new runtime session while enforcing SID uniqueness.
    /// 插入一个新的运行时会话并强制 SID 唯一。
    ///
    /// `profile`, `sid`, `ttl_sec`, and `replace` define the requested lease identity and policy.
    /// `profile`、`sid`、`ttl_sec` 与 `replace` 定义请求的租约身份及策略。
    /// `path_context` and `vm` are pending resources consumed only by a successful commit.
    /// `path_context` 与 `vm` 是仅由成功提交消费的待提交资源。
    ///
    /// Return the committed lease payload or a stable error without damaging an old active lease.
    /// 返回已提交租约载荷；失败时返回稳定错误且不破坏旧活跃租约。
    fn insert(
        &self,
        profile: RuntimeLeaseProfile,
        sid: String,
        ttl_sec: Option<u64>,
        replace: bool,
        path_context: RuntimeLeasePathContext,
        vm: LuaVm,
    ) -> Result<Value, RuntimeSessionError> {
        // Pending resources remain outside manager state until every fallible validation succeeds.
        // 在全部可失败校验成功前，待提交资源始终位于管理器状态之外。
        let mut pending_path_context = Some(path_context);
        let mut pending_vm = Some(vm);
        // Retired entries detached under the state lock and destroyed only after unlocking.
        // 在状态锁内摘除、仅在解锁后销毁的退役条目。
        let mut retired_entries = Vec::new();
        let mut state = self.lock_state()?;
        retired_entries.extend(Self::prune_inactive_locked(&mut state));
        let result = (|| {
            // Existing active lease selected for replacement only after busy/existence checks.
            // 仅在完成忙碌与存在性检查后选定的待替换活跃租约。
            let mut replacement_lease_id = None;
            if let Some(existing_lease_id) = state.sid_index.get(&sid).cloned() {
                if let Some(existing_session) = state
                    .leases
                    .get(&existing_lease_id)
                    .map(|entry| Arc::clone(&entry.session))
                {
                    let existing_session = match existing_session.try_lock() {
                        Ok(existing_session) => existing_session,
                        Err(TryLockError::WouldBlock) => {
                            return Err(RuntimeSessionError {
                                code: if replace {
                                    "lease_busy"
                                } else {
                                    "lease_exists"
                                },
                                message: if replace {
                                    format!(
                                        "runtime session SID `{sid}` cannot replace busy lease `{existing_lease_id}`"
                                    )
                                } else {
                                    format!(
                                        "runtime session SID `{sid}` already has lease `{existing_lease_id}`"
                                    )
                                },
                            });
                        }
                        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                    };
                    if let Some(error) = existing_session.inactive_error() {
                        drop(existing_session);
                        if let Some(entry) = Self::retire_active_lease_locked(
                            &mut state,
                            &existing_lease_id,
                            error.code,
                        ) {
                            retired_entries.push(entry);
                        }
                    } else if !replace {
                        return Err(RuntimeSessionError {
                            code: "lease_exists",
                            message: format!(
                                "runtime session SID `{sid}` already has lease `{existing_lease_id}`"
                            ),
                        });
                    } else {
                        replacement_lease_id = Some(existing_lease_id);
                    }
                } else {
                    state.sid_index.remove(&sid);
                }
            }
            if state.leases.len() >= 8 && replacement_lease_id.is_none() {
                return Err(RuntimeSessionError {
                    code: "lease_limit_exceeded",
                    message: "runtime session lease limit exceeded".to_string(),
                });
            }

            // Next lease sequence validated before publishing any manager state mutation.
            // 在发布任何管理器状态变更前完成校验的下一租约序号。
            let next_sequence = next_runtime_session_sequence(state.next_sequence)?;
            // Next SID-local generation validated without wraparound or identity reuse.
            // 在不发生回绕或身份复用的前提下校验的下一 SID 内部代际。
            let generation = next_runtime_session_generation(
                state.generations.get(&sid).copied().unwrap_or(0),
                &sid,
            )?;
            // Opaque identity derived entirely from already validated monotonic counters.
            // 完全由已校验单调计数器派生的不透明身份。
            let lease_id = format!("rt_{}_{}", unix_time_millis(), next_sequence);
            // Pending immutable path context borrowed for all System binding validation.
            // 为全部 System binding 校验借用的待提交不可变路径上下文。
            let path_context = pending_path_context
                .as_ref()
                .expect("pending runtime lease path context must exist before commit");
            if profile == RuntimeLeaseProfile::SystemLuaLib {
                // Required System package context proven before identity publication.
                // 在身份发布前已验证的必需 System 包上下文。
                let package =
                    path_context
                        .managed_package
                        .as_ref()
                        .ok_or_else(|| RuntimeSessionError {
                            code: "system_package_missing",
                            message: "system runtime lease is missing its trusted package context"
                                .to_string(),
                        })?;
                // Single-assignment binding completed before the session enters either manager index.
                // 在会话进入任一管理器索引前完成的单次赋值绑定。
                let binding = package.lease_binding().ok_or_else(|| RuntimeSessionError {
                    code: "system_package_binding_missing",
                    message: "system runtime package is missing its lease binding".to_string(),
                })?;
                binding
                    .bind(lease_id.clone(), generation)
                    .map_err(|message| RuntimeSessionError {
                        code: "system_package_binding_failed",
                        message,
                    })?;
            }
            // Exact managed package owner copied before path context moves into the persistent session.
            // 在路径上下文移入持久会话前复制的精确受管包所有者。
            let owner_token = path_context
                .managed_package
                .as_ref()
                .map(|package| package.owner_token());
            // Normalized lifetime and both monotonic/host-visible expiration timestamps.
            // 规范化生命周期以及单调时钟与宿主可见的两类过期时间戳。
            let ttl_sec = ttl_sec.map(|value| value.clamp(1, 3_600));
            let (expires_at, expires_at_unix_ms) = runtime_session_expiry(ttl_sec);
            // Shared active marker published with the new runtime session entry.
            // 与新运行时会话条目一同发布的共享活跃标记。
            let terminal_state = Arc::new(AtomicU8::new(RuntimeSessionTerminalState::Active as u8));
            // Fully validated path context consumed only at the non-fallible commit boundary.
            // 仅在不可失败提交边界消费的已完整校验路径上下文。
            let path_context = pending_path_context
                .take()
                .expect("pending runtime lease path context must exist at commit");
            // Fully constructed VM consumed only at the non-fallible commit boundary.
            // 仅在不可失败提交边界消费的已完整构造 VM。
            let vm = pending_vm
                .take()
                .expect("pending runtime lease VM must exist at commit");
            // New session assembled before any old active entry is detached.
            // 在摘除任何旧活跃条目前完成组装的新会话。
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
            // Stable response snapshot prepared before the manager indexes change.
            // 在管理器索引变化前准备的稳定响应快照。
            let snapshot = session.status_payload();
            let response_snapshot = snapshot.clone();
            // New active entry ready for the single locked commit transaction.
            // 已准备好参与单次锁内提交事务的新活跃条目。
            let new_entry = RuntimeSessionEntry {
                session: Arc::new(Mutex::new(session)),
                sid: sid.clone(),
                lease_id: lease_id.clone(),
                generation,
                profile,
                owner_token,
                terminal_state,
                snapshot,
            };

            // All remaining operations are infallible manager mutations: counters, old detach, new publish.
            // 后续仅包含不可失败的管理器变更：计数器更新、旧条目摘除与新条目发布。
            state.next_sequence = next_sequence;
            state.generations.insert(sid.clone(), generation);
            if let Some(existing_lease_id) = replacement_lease_id {
                let retired_entry = Self::retire_active_lease_locked(
                    &mut state,
                    &existing_lease_id,
                    "lease_replaced",
                )
                .expect("validated replacement lease must remain present during locked commit");
                retired_entries.push(retired_entry);
            }
            state.leases.insert(lease_id.clone(), new_entry);
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
                "system_package": response_snapshot.get("system_package").cloned().unwrap_or(Value::Null),
                "ttl_sec": ttl_sec,
                "expires_at_unix_ms": expires_at_unix_ms
            }))
        })();
        // Manager state must be unlocked before either retired or rejected VM resources are destroyed.
        // 销毁退役或被拒绝的 VM 资源前必须先释放管理器状态锁。
        drop(state);
        drop(retired_entries);
        drop(pending_vm);
        drop(pending_path_context);
        result
    }

    /// Get one session handle by lease id.
    /// 按租约 id 获取一个会话句柄。
    ///
    /// `lease_id` identifies the record while optional expected fields enforce host-echoed identity.
    /// `lease_id` 标识记录，可选 expected 字段用于强制校验宿主回传身份。
    ///
    /// Return the exact active session handle or its stable terminal/identity error.
    /// 返回精确活跃会话句柄或其稳定终态/身份错误。
    pub(super) fn get(
        &self,
        lease_id: &str,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Arc<Mutex<RuntimeSession>>, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        // Expired entries detached under lock and retained until after the lookup unlocks.
        // 在锁内摘除并保留到查找解锁后的过期条目。
        let retired_entries = Self::prune_inactive_locked(&mut state);
        let result = (|| {
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
        })();
        drop(state);
        drop(retired_entries);
        result
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
    ///
    /// `sid` and `expected_profile` optionally restrict the typed active entries included.
    /// `sid` 与 `expected_profile` 可选限制所包含的类型化活跃条目。
    ///
    /// Return a sorted JSON lease list after expired entries are detached safely.
    /// 在安全摘除过期条目后返回已排序 JSON 租约列表。
    fn list(
        &self,
        sid: Option<&str>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Value, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        // Expired entries detached under lock and retained until after list construction unlocks.
        // 在锁内摘除并保留到列表构造解锁后的过期条目。
        let retired_entries = Self::prune_inactive_locked(&mut state);
        let result = (|| {
            let mut entries = Vec::new();
            for entry in state.leases.values() {
                if sid.is_some_and(|expected_sid| entry.sid != expected_sid) {
                    continue;
                }
                if expected_profile
                    .is_some_and(|expected_profile| entry.profile != expected_profile)
                {
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
        })();
        drop(state);
        drop(retired_entries);
        result
    }

    /// Mark one runtime session closed.
    /// 将一个运行时会话标记为已关闭。
    ///
    /// `lease_id` identifies the record while optional expected fields enforce exact ownership.
    /// `lease_id` 标识记录，可选 expected 字段用于强制校验精确所有权。
    ///
    /// Return the final close payload or a stable terminal/identity error.
    /// 返回最终关闭载荷或稳定终态/身份错误。
    fn close(
        &self,
        lease_id: &str,
        expected_sid: Option<&str>,
        expected_generation: Option<u64>,
        expected_profile: Option<RuntimeLeaseProfile>,
    ) -> Result<Value, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        // Entries detached by pruning or close and destroyed only after the state lock is released.
        // 因清理或关闭而摘除、仅在释放状态锁后销毁的条目。
        let mut retired_entries = Self::prune_inactive_locked(&mut state);
        let result = (|| {
            let Some((session, terminal_state)) = state.leases.get(lease_id).map(|entry| {
                (
                    Arc::clone(&entry.session),
                    Arc::clone(&entry.terminal_state),
                )
            }) else {
                if let Some(tombstone) = state.tombstones.get(lease_id) {
                    Self::validate_tombstone_identity(
                        tombstone,
                        expected_sid,
                        expected_generation,
                    )?;
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
            drop(session);
            // Exact closed entry detached atomically with its tombstone and SID index transition.
            // 与墓碑及 SID 索引迁移原子完成摘除的精确关闭条目。
            let retired_entry =
                Self::retire_active_lease_locked(&mut state, lease_id, "lease_closed")
                    .expect("validated close lease must remain present during locked commit");
            retired_entries.push(retired_entry);
            Ok(payload)
        })();
        drop(state);
        drop(retired_entries);
        result
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

    /// Drain package owner tokens detached by completed manager mutations.
    /// 排空已完成管理器变更所分离的包所有者令牌。
    ///
    /// Return each exact owner once so the engine can retire both managed runtime services.
    /// 每个精确所有者仅返回一次，供引擎退役两类受管运行时服务。
    pub(super) fn take_retired_owner_tokens(&self) -> Result<Vec<u64>, RuntimeSessionError> {
        let mut state = self.lock_state()?;
        Ok(std::mem::take(&mut state.retired_owner_tokens))
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
    ///
    /// `state` is the locked manager state whose indexes are updated atomically.
    /// `state` 是以原子方式更新索引的已锁定管理器状态。
    ///
    /// Return detached entries that the caller must destroy only after releasing the state lock.
    /// 返回已摘除条目；调用方必须仅在释放状态锁后销毁它们。
    fn prune_inactive_locked(state: &mut RuntimeSessionManagerState) -> Vec<RuntimeSessionEntry> {
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
        // Retired entries returned to the caller for destruction after manager state unlocks.
        // 返回给调用方、供管理器状态解锁后销毁的退役条目。
        let mut retired_entries = Vec::with_capacity(removed.len());
        for lease_id in removed {
            if let Some(entry) = Self::retire_active_lease_locked(state, &lease_id, "lease_expired")
            {
                retired_entries.push(entry);
            }
        }
        let tombstone_ttl = runtime_session_tombstone_ttl();
        state.tombstones.retain(|_, tombstone| {
            // Newly created tombstones may be marginally newer than the prune-start snapshot.
            // 新创建的墓碑可能略晚于清理开始时的时间快照。
            now.checked_duration_since(tombstone.retired_at)
                .map(|age| age < tombstone_ttl)
                .unwrap_or(true)
        });
        retired_entries
    }

    /// Move one active lease into the terminal tombstone table.
    /// 将单个活跃租约移动到终态墓碑表中。
    ///
    /// `state`, `lease_id`, and `code` identify the locked manager, exact lease, and terminal reason.
    /// `state`、`lease_id` 与 `code` 标识已锁定管理器、精确租约及终态原因。
    ///
    /// Return the detached entry for lock-free destruction, or none when the lease is absent.
    /// 返回供锁外销毁的已摘除条目；租约不存在时返回空值。
    fn retire_active_lease_locked(
        state: &mut RuntimeSessionManagerState,
        lease_id: &str,
        code: &'static str,
    ) -> Option<RuntimeSessionEntry> {
        if let Some(entry) = state.leases.get(lease_id) {
            entry.terminal_state.store(
                runtime_session_terminal_state_from_code(code) as u8,
                Ordering::Release,
            );
        }
        // Exact active entry detached from manager ownership before tombstone publication.
        // 在发布墓碑前从管理器所有权中摘除的精确活跃条目。
        let entry = state.leases.remove(lease_id)?;
        if let Some(owner_token) = entry.owner_token {
            state.retired_owner_tokens.push(owner_token);
        }
        let tombstone = RuntimeSessionTombstone::from_entry(&entry, code);
        if state
            .sid_index
            .get(&tombstone.sid)
            .is_some_and(|current| current == lease_id)
        {
            state.sid_index.remove(&tombstone.sid);
        }
        state.tombstones.insert(lease_id.to_string(), tombstone);
        Some(entry)
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
            "system_package": session_status_system_package(self),
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
            "system_package": session_status_system_package(self),
            "ttl_sec": self.ttl_sec,
            "expires_at_unix_ms": self.expires_at_unix_ms,
            "closed": self.closed,
            "expired": self.is_expired()
        })
    }
}

impl RuntimeSessionTombstone {
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

/// Build the stable host-visible System Plugin package descriptor for one lease.
/// 为单个租约构造稳定的宿主可见 System Plugin 包描述符。
fn session_status_system_package(session: &RuntimeSession) -> Value {
    // Trusted package context exists only on the dedicated System surface.
    // 仅专用 System 接口存在的可信包上下文。
    let Some(package) = session.path_context.managed_package.as_ref() else {
        return Value::Null;
    };
    json!({
        "id": package.identity().package_id(),
        "root": render_host_visible_path(package.package_root()),
        "dependencies_file": package
            .dependency_manifest_path()
            .map(render_host_visible_path),
    })
}

/// Recursively immutable JSON container exposed to System Plugin Lua as userdata.
/// 以 userdata 形式暴露给 System Plugin Lua 的递归不可变 JSON 容器。
#[derive(Clone)]
pub(super) struct ReadonlyJsonContextValue {
    /// Exact JSON array or object retained outside mutable Lua tables.
    /// 保存在可变 Lua table 之外的精确 JSON 数组或对象。
    value: Value,
    /// Stable field label included in attempted-mutation diagnostics.
    /// 包含在修改尝试诊断中的稳定字段标签。
    label: String,
}

impl ReadonlyJsonContextValue {
    /// Return the exact JSON value for lossless host-result serialization.
    /// 返回用于无损宿主结果序列化的精确 JSON 值。
    pub(super) fn json_value(&self) -> &Value {
        &self.value
    }
}

/// Key retained by one detached read-only JSON iterator.
/// 单个分离式只读 JSON 迭代器保留的键。
#[derive(Clone)]
enum ReadonlyJsonIterationKey {
    /// One-based array index matching Lua sequence conventions.
    /// 符合 Lua 序列约定的一基数组索引。
    ArrayIndex(usize),
    /// UTF-8 object field name.
    /// UTF-8 对象字段名。
    ObjectKey(String),
}

impl UserData for ReadonlyJsonContextValue {
    /// Register immutable indexing, iteration, length, and mutation-rejection behavior.
    /// 注册不可变索引、迭代、长度与修改拒绝行为。
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, context, key: LuaValue| {
            // Child selected only from the exact retained JSON container shape.
            // 仅从精确保留的 JSON 容器形状中选择的子值。
            let child = match (&context.value, key) {
                (Value::Object(object), LuaValue::String(key)) => {
                    key.to_str().ok().and_then(|key| object.get(key.as_ref()))
                }
                (Value::Array(array), key) => {
                    readonly_json_array_index(&key).and_then(|index| array.get(index))
                }
                _ => None,
            };
            match child {
                Some(child) => readonly_json_context_child_to_lua(lua, child, &context.label),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_meta_method(
            MetaMethod::NewIndex,
            |_, context, (_key, _value): (LuaValue, LuaValue)| -> mlua::Result<()> {
                Err(mlua::Error::runtime(format!(
                    "{} is read-only",
                    context.label
                )))
            },
        );
        methods.add_meta_method(MetaMethod::Len, |_, context, ()| {
            Ok(match &context.value {
                Value::Array(array) => array.len(),
                _ => 0,
            })
        });
        methods.add_meta_method(MetaMethod::Pairs, |lua, context, ()| {
            create_readonly_json_context_iterator(lua, context, false)
        });
        methods.add_meta_method(MetaMethod::IPairs, |lua, context, ()| {
            create_readonly_json_context_iterator(lua, context, true)
        });
    }
}

/// Convert a Lua numeric key into one zero-based JSON array index.
/// 将 Lua 数值键转换为零基 JSON 数组索引。
///
/// `key` may be an integer or an exactly integral floating-point number.
/// `key` 可以是整数，也可以是精确整数形式的浮点数。
///
/// Returns `None` for nonpositive, fractional, nonfinite, or out-of-range keys.
/// 对非正数、小数、非有限值或越界键返回 `None`。
fn readonly_json_array_index(key: &LuaValue) -> Option<usize> {
    match key {
        LuaValue::Integer(index) => index
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok()),
        LuaValue::Number(index)
            if index.is_finite()
                && *index >= 1.0
                && index.fract() == 0.0
                && *index <= usize::MAX as f64 =>
        {
            Some(*index as usize - 1)
        }
        _ => None,
    }
}

/// Convert one child JSON value into an immutable Lua context value.
/// 将单个子 JSON 值转换为不可变 Lua 上下文值。
///
/// `value` is retained behind userdata for containers and converted natively for scalars.
/// `value` 为容器时保存在 userdata 后方，为标量时转换为原生值。
///
/// Returns one Lua value without exposing any mutable backing table.
/// 返回一个不暴露任何可变后备 table 的 Lua 值。
fn readonly_json_context_child_to_lua(
    lua: &Lua,
    value: &Value,
    label: &str,
) -> mlua::Result<LuaValue> {
    match value {
        Value::Array(_) | Value::Object(_) => lua
            .create_userdata(ReadonlyJsonContextValue {
                value: value.clone(),
                label: label.to_string(),
            })
            .map(LuaValue::UserData),
        _ => json_value_to_lua(lua, value),
    }
}

/// Create one detached iterator over a read-only JSON container.
/// 为一个只读 JSON 容器创建分离式迭代器。
///
/// `array_only` restricts `ipairs` to arrays while ordinary `pairs` also exposes object fields.
/// `array_only` 将 `ipairs` 限制为数组；普通 `pairs` 还会暴露对象字段。
///
/// Returns the iterator triple required by Lua generic-for semantics.
/// 返回 Lua 通用 for 语义所需的迭代器三元组。
fn create_readonly_json_context_iterator(
    lua: &Lua,
    context: &ReadonlyJsonContextValue,
    array_only: bool,
) -> mlua::Result<(Function, LuaValue, LuaValue)> {
    // Detached entries keep arbitrary Lua code outside any borrowed userdata reference.
    // 分离式条目使任意 Lua 代码都不会在 userdata 借用引用内执行。
    let entries = match &context.value {
        Value::Array(array) => array
            .iter()
            .enumerate()
            .map(|(index, value)| {
                (
                    ReadonlyJsonIterationKey::ArrayIndex(index + 1),
                    value.clone(),
                )
            })
            .collect::<Vec<_>>(),
        Value::Object(object) if !array_only => object
            .iter()
            .map(|(key, value)| {
                (
                    ReadonlyJsonIterationKey::ObjectKey(key.clone()),
                    value.clone(),
                )
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    // Stable label and cursor owned entirely by the iterator closure.
    // 完全由迭代器闭包拥有的稳定标签与游标。
    let label = context.label.clone();
    let mut entries = entries.into_iter();
    let iterator = lua.create_function_mut(move |lua, _arguments: MultiValue| {
        let Some((key, value)) = entries.next() else {
            return Ok(MultiValue::new());
        };
        // Lua key preserving object text or one-based array indexing.
        // 保留对象文本或一基数组索引的 Lua 键。
        let key = match key {
            ReadonlyJsonIterationKey::ArrayIndex(index) => LuaValue::Integer(
                i64::try_from(index)
                    .map_err(|_| mlua::Error::runtime("read-only JSON array index overflow"))?,
            ),
            ReadonlyJsonIterationKey::ObjectKey(key) => LuaValue::String(lua.create_string(&key)?),
        };
        let value = readonly_json_context_child_to_lua(lua, &value, &label)?;
        Ok(MultiValue::from_vec(vec![key, value]))
    })?;
    Ok((iterator, LuaValue::Nil, LuaValue::Nil))
}

/// Convert one JSON value into a recursively read-only Lua value.
/// 将单个 JSON 值转换为递归只读 Lua 值。
fn readonly_json_value_to_lua(lua: &Lua, value: &Value, label: &str) -> Result<LuaValue, String> {
    readonly_json_context_child_to_lua(lua, value, label)
        .map_err(|error| format!("failed to build read-only {label} Lua context: {error}"))
}

/// Host-private System context read through the protected `vulcan.runtime` metatable.
/// 通过受保护 `vulcan.runtime` 元表读取的宿主私有 System 上下文。
#[derive(Clone, Default)]
struct SystemPluginRuntimeContextSlot {
    /// Active trusted System package; absent before binding or in public runtime sessions.
    /// 活动可信 System 包；绑定前或公开运行时会话中不存在。
    package: Option<Arc<ManagedRuntimePackageContext>>,
}

/// Return whether one runtime-table key is owned exclusively by the host context boundary.
/// 返回单个运行时表键是否由宿主上下文边界独占所有。
///
/// `key` is the exact Lua string key and the result is true for the three protected fields.
/// `key` 是精确 Lua 字符串键；三个受保护字段会返回 true。
fn is_protected_system_runtime_context_key(key: &str) -> bool {
    matches!(key, "system_plugin" | "workspace_root" | "mounts")
}

/// Materialize one protected System runtime field from host-private app data.
/// 从宿主私有应用数据实例化单个受保护的 System 运行时字段。
///
/// `lua` owns the target VM and `key` selects one protected field.
/// `lua` 拥有目标 VM，`key` 选择一个受保护字段。
///
/// Returns the current immutable field value or nil when no System package is active.
/// 返回当前不可变字段值；没有活动 System 包时返回 nil。
fn system_runtime_context_value(lua: &Lua, key: &str) -> mlua::Result<LuaValue> {
    // Package clone is released from the app-data borrow before any Lua object is created.
    // 在创建任何 Lua 对象前解除应用数据借用并克隆包。
    let package = lua
        .app_data_ref::<SystemPluginRuntimeContextSlot>()
        .and_then(|slot| slot.package.clone());
    let Some(package) = package else {
        return Ok(LuaValue::Nil);
    };
    let binding = package.lease_binding().ok_or_else(|| {
        mlua::Error::runtime("system runtime package is missing its lease binding")
    })?;
    let identity = binding.identity().ok_or_else(|| {
        mlua::Error::runtime("system runtime package lease identity is not bound")
    })?;
    match key {
        "system_plugin" => readonly_json_value_to_lua(
            lua,
            &json!({
                "id": package.identity().package_id(),
                "root": render_host_visible_path(package.package_root()),
                "lease_id": identity.lease_id(),
                "sid": binding.sid(),
                "generation": identity.generation(),
            }),
            "vulcan.runtime.system_plugin",
        )
        .map_err(mlua::Error::runtime),
        "workspace_root" => match binding.workspace_root() {
            Some(workspace_root) => Ok(LuaValue::String(
                lua.create_string(render_host_visible_path(workspace_root))?,
            )),
            None => Ok(LuaValue::Nil),
        },
        "mounts" => readonly_json_value_to_lua(lua, binding.mounts(), "vulcan.runtime.mounts")
            .map_err(mlua::Error::runtime),
        _ => Ok(LuaValue::Nil),
    }
}

/// Install immutable root fields and remove Lua primitives capable of bypassing their metatables.
/// 安装不可变根字段，并移除能够绕过其元表的 Lua 原语。
///
/// `lua` is one newly created dedicated System VM; success permanently hardens that VM.
/// `lua` 是一个新建的专用 System VM；成功后会永久加固该 VM。
///
/// Returns unit after the protected metatable, rawset removal, and debug removal are complete.
/// 完成受保护元表、rawset 移除与 debug 移除后返回空值。
pub(super) fn install_system_runtime_context_boundary(lua: &Lua) -> Result<(), String> {
    let runtime = get_vulcan_runtime_table(lua)?;
    for key in ["system_plugin", "workspace_root", "mounts"] {
        runtime
            .raw_set(key, LuaValue::Nil)
            .map_err(|error| format!("failed to clear protected vulcan.runtime.{key}: {error}"))?;
    }
    let metatable = lua
        .create_table()
        .map_err(|error| format!("failed to create protected runtime metatable: {error}"))?;
    let index = lua
        .create_function(|lua, (_table, key): (Table, LuaValue)| {
            let LuaValue::String(key) = key else {
                return Ok(LuaValue::Nil);
            };
            let key = key.to_str()?;
            if is_protected_system_runtime_context_key(key.as_ref()) {
                system_runtime_context_value(lua, key.as_ref())
            } else {
                Ok(LuaValue::Nil)
            }
        })
        .map_err(|error| format!("failed to create protected runtime index: {error}"))?;
    let new_index = lua
        .create_function(|_lua, (table, key, value): (Table, LuaValue, LuaValue)| {
            if let LuaValue::String(key_text) = &key {
                let key_text = key_text.to_str()?;
                if is_protected_system_runtime_context_key(key_text.as_ref()) {
                    return Err(mlua::Error::runtime(format!(
                        "vulcan.runtime.{} is read-only",
                        key_text.as_ref()
                    )));
                }
            }
            table.raw_set(key, value)
        })
        .map_err(|error| format!("failed to create protected runtime newindex: {error}"))?;
    metatable
        .set("__index", index)
        .and_then(|()| metatable.set("__newindex", new_index))
        .and_then(|()| metatable.set("__metatable", "protected System runtime context"))
        .map_err(|error| format!("failed to configure protected runtime metatable: {error}"))?;
    runtime
        .set_metatable(Some(metatable))
        .map_err(|error| format!("failed to protect vulcan.runtime context: {error}"))?;
    // rawset and debug can bypass table/userdata metatables and are unnecessary in System plugins.
    // rawset 与 debug 可绕过表及 userdata 元表，且 System 插件并不需要它们。
    let globals = lua.globals();
    globals
        .set("rawset", LuaValue::Nil)
        .and_then(|()| globals.set("debug", LuaValue::Nil))
        .map_err(|error| format!("failed to remove unsafe System Lua globals: {error}"))?;
    let package: Table = globals
        .get("package")
        .map_err(|error| format!("failed to read System Lua package table: {error}"))?;
    let loaded: Table = package
        .get("loaded")
        .map_err(|error| format!("failed to read System Lua package.loaded: {error}"))?;
    loaded
        .set("debug", LuaValue::Nil)
        .map_err(|error| format!("failed to remove package.loaded.debug: {error}"))?;
    let preload: Table = package
        .get("preload")
        .map_err(|error| format!("failed to read System Lua package.preload: {error}"))?;
    preload
        .set("debug", LuaValue::Nil)
        .map_err(|error| format!("failed to remove package.preload.debug: {error}"))?;
    let _previous = lua.set_app_data(SystemPluginRuntimeContextSlot::default());
    Ok(())
}

/// Populate or clear the host-owned System Plugin fields on `vulcan.runtime`.
/// 在 `vulcan.runtime` 上填充或清除宿主所有的 System Plugin 字段。
fn populate_system_plugin_runtime_context(
    lua: &Lua,
    package: Option<&Arc<ManagedRuntimePackageContext>>,
) -> Result<(), String> {
    // App-data replacement is the only host write; Lua never receives a mutable root-field slot.
    // 应用数据替换是唯一宿主写入；Lua 永远不会获得可变根字段槽位。
    let _previous = lua.set_app_data(SystemPluginRuntimeContextSlot {
        package: package.cloned(),
    });
    Ok(())
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
    // Host/API-visible path spelling normalized before validation and native identity capture.
    // 在校验和捕获原生身份前归一化宿主/API 可见的路径形式。
    let normalized = normalize_host_input_path_text(trimmed)
        .map_err(|error| format!("{field_name}: {error}"))?;
    #[cfg(windows)]
    if has_invalid_windows_path_syntax(&normalized) {
        return Err(format!(
            "{field_name} contains unsupported Windows path syntax"
        ));
    }
    Ok(Some(PathBuf::from(normalized)))
}

/// Canonicalize one host-authorized System directory and require an absolute real directory.
/// 规范化一个宿主授权的 System 目录，并要求其为绝对真实目录。
///
/// `path` is the parsed candidate and `field_name` labels stable validation diagnostics.
/// `path` 是已解析候选路径，`field_name` 用于标记稳定校验诊断。
///
/// Returns the canonical directory used for all later containment checks.
/// 返回用于后续全部包含关系检查的规范目录。
fn canonicalize_system_runtime_directory(path: &Path, field_name: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{field_name} must be an absolute path"));
    }
    let canonical = fs::canonicalize(path).map_err(|error| {
        format!(
            "failed to canonicalize {field_name} {}: {error}",
            render_host_visible_path(path)
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        format!(
            "failed to inspect {field_name} {}: {error}",
            render_host_visible_path(&canonical)
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "{field_name} is not a directory: {}",
            render_host_visible_path(&canonical)
        ));
    }
    Ok(canonical)
}

/// Resolve and authorize one System lease cwd under its package or declared workspace root.
/// 在包根或已声明工作区根下解析并授权单个 System 租约 cwd。
///
/// `requested` is the optional host input, `package_root` is canonical, and `workspace_root`
/// is the optional canonical authorization anchor.
/// `requested` 是可选宿主输入，`package_root` 已规范化，`workspace_root` 是可选规范授权锚点。
///
/// Returns the canonical authorized cwd, defaulting to the package root.
/// 返回规范且已授权的 cwd，默认采用包根。
fn resolve_system_runtime_cwd(
    requested: Option<&str>,
    package_root: &Path,
    workspace_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let candidate = match normalize_optional_runtime_lease_path(requested, "cwd")? {
        Some(path) if path.is_absolute() => path,
        Some(path) => package_root.join(path),
        None => package_root.to_path_buf(),
    };
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "failed to canonicalize cwd {}: {error}",
            render_host_visible_path(&candidate)
        )
    })?;
    if !fs::metadata(&canonical)
        .map_err(|error| {
            format!(
                "failed to inspect cwd {}: {error}",
                render_host_visible_path(&canonical)
            )
        })?
        .is_dir()
    {
        return Err(format!(
            "cwd is not a directory: {}",
            render_host_visible_path(&canonical)
        ));
    }
    if !canonical.starts_with(package_root)
        && !workspace_root.is_some_and(|root| canonical.starts_with(root))
    {
        return Err(format!(
            "cwd {} is outside the System package root and authorized workspace root",
            render_host_visible_path(&canonical)
        ));
    }
    Ok(canonical)
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
    /// Select the isolated manager that owns the requested lease profile.
    /// 选择拥有指定租约类型的隔离管理器。
    fn runtime_session_manager(&self, profile: RuntimeLeaseProfile) -> &Arc<RuntimeSessionManager> {
        match profile {
            RuntimeLeaseProfile::Public => &self.public_runtime_sessions,
            RuntimeLeaseProfile::SystemLuaLib => &self.system_runtime_sessions,
        }
    }

    /// Return the effective fixed `system_lua_lib` directory for the current engine.
    /// 返回当前引擎生效的固定 `system_lua_lib` 目录。
    fn resolve_system_lua_lib_dir(&self) -> Result<PathBuf, String> {
        // Explicit System trust root derived from the host runtime layout.
        // 从宿主运行时布局派生的显式 System 信任根。
        let configured = self
            .host_options
            .system_lua_lib_dir
            .as_ref()
            .ok_or_else(|| "system runtime leases require system_lua_lib_dir".to_string())?;
        // Canonical trust root used for stable containment and host-visible status.
        // 用于稳定包含校验与宿主可见状态的规范信任根。
        let canonical = std::fs::canonicalize(configured).map_err(|error| {
            format!(
                "failed to canonicalize system_lua_lib_dir {}: {}",
                render_host_visible_path(configured),
                error
            )
        })?;
        if !canonical.is_dir() {
            return Err(format!(
                "system_lua_lib_dir is not a directory: {}",
                render_host_visible_path(&canonical)
            ));
        }
        Ok(canonical)
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
    fn resolve_public_runtime_lease_path_context(
        &self,
        request: &RuntimeSessionCreateRequest,
    ) -> Result<RuntimeLeasePathContext, String> {
        // Host-provided path context retained only by the ordinary public lease surface.
        // 仅由普通公开租约接口保留的宿主路径上下文。
        let context = RuntimeLeasePathContext {
            cwd: normalize_optional_runtime_lease_path(request.cwd.as_deref(), "cwd")?,
            cwd_identity: None,
            workspace_root: normalize_optional_runtime_lease_path(
                request.workspace_root.as_deref(),
                "workspace_root",
            )?,
            workspace_root_identity: None,
            lua_roots: normalize_runtime_lease_path_list(&request.lua_roots, "lua_roots")?,
            c_roots: normalize_runtime_lease_path_list(&request.c_roots, "c_roots")?,
            mounts: request.mounts.clone(),
            system_lua_lib_dir: None,
            managed_package: None,
        };
        if !context.mounts.is_object() && !context.mounts.is_null() {
            return Err("runtime lease mounts must be one JSON object when present".to_string());
        }
        Ok(context)
    }

    /// Resolve one strict System Plugin package and its isolated lease path context.
    /// 解析严格的 System Plugin 包及其隔离租约路径上下文。
    fn resolve_system_runtime_lease_path_context(
        &self,
        request: &SystemRuntimeSessionCreateRequest,
    ) -> Result<RuntimeLeasePathContext, String> {
        if !request.mounts.is_object() {
            return Err("system runtime lease mounts must be one JSON object".to_string());
        }
        // Canonical System trust root configured by the host.
        // 宿主配置的规范 System 信任根。
        let system_lua_lib_dir = self.resolve_system_lua_lib_dir()?;
        // Runtime root required to resolve managed interpreters and environments.
        // 解析受管解释器与环境所必需的运行时根目录。
        let runtime_root = self
            .host_options
            .runtime_root
            .as_ref()
            .ok_or_else(|| "system runtime leases require runtime_root".to_string())?;
        // Optional workspace identity retained by the trusted package binding.
        // 由可信包绑定保留的可选工作区身份。
        let workspace_root = normalize_optional_runtime_lease_path(
            request.workspace_root.as_deref(),
            "workspace_root",
        )?
        .map(|path| canonicalize_system_runtime_directory(&path, "workspace_root"))
        .transpose()?;
        // Optional native workspace object identity retained across the complete lease lifetime.
        // 跨越完整租约生命周期保留的可选工作区平台原生对象身份。
        let workspace_root_identity = workspace_root
            .as_deref()
            .map(|path| capture_managed_directory_identity(path, "workspace_root"))
            .transpose()?;
        // Unbound lease metadata completed atomically by the manager before publication.
        // 由管理器在发布前原子补全的未绑定租约元数据。
        let lease_binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            request.sid.clone(),
            workspace_root.clone(),
            request.mounts.clone(),
        ));
        // Canonical System Plugin context validated against the host trust root.
        // 针对宿主信任根校验后的规范 System Plugin 上下文。
        // ManagedRuntimeRoots shares the exact engine-selected distribution and environment roots.
        // ManagedRuntimeRoots 共享引擎选定的精确发行根与环境根。
        let managed_runtime_roots = self.managed_runtime_roots_for(runtime_root)?;
        let managed_package = ManagedRuntimePackageContext::for_system_plugin_with_roots(
            &request.system_package.id,
            Path::new(&request.system_package.root),
            managed_runtime_roots,
            &system_lua_lib_dir,
            &request.system_package.dependencies_file,
            lease_binding,
        )?;
        // Canonical cwd constrained to the package or the explicitly authorized workspace.
        // 限制在包或显式授权工作区内的规范 cwd。
        let cwd = resolve_system_runtime_cwd(
            request.cwd.as_deref(),
            managed_package.package_root(),
            workspace_root.as_deref(),
        )?;
        // Native cwd identity prevents same-name link, junction, or directory replacement after create.
        // cwd 平台原生身份防止创建后发生同名链接、junction 或目录替换。
        let cwd_identity = capture_managed_directory_identity(&cwd, "cwd")?;
        Ok(RuntimeLeasePathContext {
            cwd: Some(cwd),
            cwd_identity: Some(cwd_identity),
            workspace_root,
            workspace_root_identity,
            lua_roots: vec![managed_package.package_root().to_path_buf()],
            c_roots: Vec::new(),
            mounts: request.mounts.clone(),
            system_lua_lib_dir: Some(system_lua_lib_dir),
            managed_package: Some(managed_package),
        })
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
        value: &mlua::LuaString,
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
        let old_cpath: mlua::LuaString = package.get("cpath").map_err(|error| {
            format!("Failed to read package.cpath for runtime lease: {}", error)
        })?;
        let old_path: mlua::LuaString = package
            .get("path")
            .map_err(|error| format!("Failed to read package.path for runtime lease: {}", error))?;
        let mut cpath_prefix = String::new();
        for root in &path_context.c_roots {
            validate_lua_search_root_path(root, "c_roots")?;
            // Lua-compatible native-module root spelling without Windows verbatim syntax.
            // 不含 Windows verbatim 语法的 Lua 兼容原生模块根写法。
            let root = render_host_visible_path(root);
            #[cfg(windows)]
            {
                cpath_prefix.push_str(&format!("{}\\?.dll;{}\\?\\init.dll;", root, root));
            }
            #[cfg(target_os = "linux")]
            {
                cpath_prefix.push_str(&format!("{}/?.so;{}/?/init.so;", root, root));
            }
            #[cfg(target_os = "macos")]
            {
                cpath_prefix.push_str(&format!("{}/?.dylib;{}/?/init.dylib;", root, root));
            }
        }
        let mut path_prefix = String::new();
        for root in &path_context.lua_roots {
            validate_lua_search_root_path(root, "lua_roots")?;
            // Lua-compatible source-module root spelling without Windows verbatim syntax.
            // 不含 Windows verbatim 语法的 Lua 兼容源码模块根写法。
            let root = render_host_visible_path(root);
            #[cfg(windows)]
            {
                path_prefix.push_str(&format!("{}\\?.lua;{}\\?\\init.lua;", root, root));
            }
            #[cfg(unix)]
            {
                path_prefix.push_str(&format!("{}/?.lua;{}/?/init.lua;", root, root));
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
                // ExecutionCwd preserves the revalidated object while removing only Windows verbatim syntax.
                // ExecutionCwd 保留已重新校验的对象，同时仅移除 Windows verbatim 语法。
                let execution_cwd = host_process_path_argument(cwd);
                std::env::set_current_dir(&execution_cwd).map_err(|error| {
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
        self.create_public_runtime_session_json(request_json)
    }
    /// Create one ordinary public runtime session from the public request contract.
    /// 根据公开请求契约创建一个普通公开运行时会话。
    fn create_public_runtime_session_json(&self, request_json: &str) -> Result<String, String> {
        // Strictly decoded public lease request.
        // 严格解码后的公开租约请求。
        let mut request: RuntimeSessionCreateRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid runtime session create JSON: {}", error))?;
        request.sid = normalize_runtime_session_sid(&request.sid)?;
        // Finite public lease lifetime resolved from the public profile policy.
        // 根据公开 profile 策略解析的有限公开租约生命周期。
        let ttl_sec =
            Self::resolve_runtime_lease_ttl(RuntimeLeaseProfile::Public, request.ttl_sec)?;
        // Public host-owned path context without a managed package identity.
        // 不含受管包身份的公开宿主路径上下文。
        let path_context = self.resolve_public_runtime_lease_path_context(&request)?;
        // Persistent public VM retaining the currently loaded Skill registry.
        // 保留当前已加载 Skill 注册表的持久公开 VM。
        let vm = Self::create_runlua_vm(RunLuaVmBuildContext::from_engine(
            self,
            &self.skills,
            &self.entry_registry,
        ))?;
        Self::configure_runtime_lease_vm(&vm.lua, &path_context)?;
        Self::install_managed_io_compat_for_runtime(&vm.lua, self.host_options.as_ref())?;
        // Public manager used for insertion and any owner retirements produced by pruning.
        // 用于插入以及处理清理所产生所有者退役项的公开管理器。
        let manager = self.runtime_session_manager(RuntimeLeaseProfile::Public);
        let result = manager.insert(
            RuntimeLeaseProfile::Public,
            request.sid,
            ttl_sec,
            request.replace,
            path_context,
            vm,
        );
        self.retire_runtime_session_manager_owners(manager.as_ref(), "public lease create");
        let payload = result.unwrap_or_else(runtime_session_error_payload);
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
        // Selected manager whose prune retirements must be drained even when lookup fails.
        // 即使查找失败也必须排空其清理退役项的所选管理器。
        let manager = self.runtime_session_manager(expected_profile);
        let session_result = manager.get(
            &request.lease_id,
            request.sid.as_deref(),
            request.generation,
            Some(expected_profile),
        );
        self.retire_runtime_session_manager_owners(manager.as_ref(), "lease eval lookup");
        let session = match session_result {
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
                            "workspace_root": session_status_workspace_root_text(&session),
                            "system_lua_lib": session_status_system_lua_lib_text(&session),
                            "system_package": session_status_system_package(&session),
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
                .runtime_session_manager(expected_profile)
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
        // Selected manager used for status and all prune retirements committed by that read.
        // 用于状态读取及其提交全部清理退役项的所选管理器。
        let manager = self.runtime_session_manager(expected_profile);
        let result = manager.status(
            &request.lease_id,
            request.sid.as_deref(),
            request.generation,
            Some(expected_profile),
        );
        self.retire_runtime_session_manager_owners(manager.as_ref(), "lease status");
        let payload = result.unwrap_or_else(runtime_session_error_payload);
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
        // Selected manager used for listing and all expired owners pruned by that operation.
        // 用于列表读取及该操作清理全部过期所有者的所选管理器。
        let manager = self.runtime_session_manager(expected_profile);
        let result = manager.list(request.sid.as_deref(), Some(expected_profile));
        self.retire_runtime_session_manager_owners(manager.as_ref(), "lease list");
        let payload = result.unwrap_or_else(runtime_session_error_payload);
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
        // Selected manager used for close and every owner detached by its preceding prune.
        // 用于关闭及其前置清理所分离全部所有者的所选管理器。
        let manager = self.runtime_session_manager(expected_profile);
        let result = manager.close(
            &request.lease_id,
            request.sid.as_deref(),
            request.generation,
            Some(expected_profile),
        );
        self.retire_runtime_session_manager_owners(manager.as_ref(), "lease close");
        let payload = result.unwrap_or_else(runtime_session_error_payload);
        serde_json::to_string(&payload)
            .map_err(|error| format!("Runtime session close JSON encode failed: {}", error))
    }
    /// Create one `system_lua_lib` runtime lease through the dedicated system surface.
    /// 通过专用 system 接口创建一个 `system_lua_lib` 运行时租约。
    pub fn create_system_runtime_lease_json(&self, request_json: &str) -> Result<String, String> {
        // Strict System request rejects public lua_roots/c_roots and every unknown field.
        // 严格 System 请求拒绝公开接口的 lua_roots/c_roots 以及所有未知字段。
        let mut request: SystemRuntimeSessionCreateRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid system runtime session create JSON: {}", error))?;
        request.sid = normalize_runtime_session_sid(&request.sid)?;
        // Infinite-by-default System lifetime resolved from the dedicated profile policy.
        // 根据专用 profile 策略解析的默认无限期 System 生命周期。
        let ttl_sec =
            Self::resolve_runtime_lease_ttl(RuntimeLeaseProfile::SystemLuaLib, request.ttl_sec)?;
        // Trusted package and lease paths resolved before a VM or manager entry is created.
        // 在创建 VM 或管理器条目前解析的可信包与租约路径。
        let path_context = self.resolve_system_runtime_lease_path_context(&request)?;
        // Dedicated VM that cannot retain ordinary Skill dispatch state across reloads.
        // 不会跨重载保留普通 Skill 分发状态的专用 VM。
        let vm = self.create_system_runtime_vm()?;
        Self::configure_runtime_lease_vm(&vm.lua, &path_context)?;
        Self::install_managed_io_compat_for_runtime(&vm.lua, self.host_options.as_ref())?;
        // System manager insertion binds lease identity before publishing the session.
        // System 管理器插入会在发布会话前绑定租约身份。
        // Dedicated System manager whose replacement and prune retirements are drained together.
        // 将替换与清理退役项一并排空的专用 System 管理器。
        let manager = self.runtime_session_manager(RuntimeLeaseProfile::SystemLuaLib);
        let result = manager.insert(
            RuntimeLeaseProfile::SystemLuaLib,
            request.sid,
            ttl_sec,
            request.replace,
            path_context,
            vm,
        );
        self.retire_runtime_session_manager_owners(manager.as_ref(), "System lease create");
        let payload = result.unwrap_or_else(runtime_session_error_payload);
        serde_json::to_string(&payload).map_err(|error| {
            format!(
                "System runtime session create JSON encode failed: {}",
                error
            )
        })
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
        // System package roots are revalidated before Lua can use the persistent package search path.
        // 在 Lua 使用持久化包搜索路径前重新校验 System 包根。
        if let Some(package) = session.path_context.managed_package.as_ref() {
            package.validate_live_filesystem_identity()?;
        }
        // System cwd/workspace authorization remains bound to the exact activation-time objects.
        // System cwd/工作区授权始终绑定到激活时的精确对象。
        session
            .path_context
            .validate_live_system_directory_identities()?;
        reset_pooled_vm_request_scope(&session.vm.lua, self.host_options.as_ref())?;
        // Package-scoped transaction rolls back sessions created by a failed evaluation.
        // 包作用域事务会回滚失败执行期间创建的会话。
        let transaction_guard = session
            .path_context
            .managed_package
            .as_ref()
            .map(|package| {
                self.managed_runtime_services
                    .begin_transaction(package.owner_token())
            })
            .transpose()?;
        // Lightweight transaction identity installed for session.open registration.
        // 为 session.open 注册安装的轻量事务身份。
        let transaction_context = transaction_guard.as_ref().map(|guard| guard.context());
        // RAII app-data scope restores any previous transaction even on early failure.
        // 即使提前失败也会恢复此前事务的 RAII 应用数据作用域。
        let _transaction_scope =
            LuaManagedRuntimeTransactionScopeGuard::install(&session.vm.lua, transaction_context);
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
                // System leases install their trusted package; public leases explicitly clear it.
                // System 租约安装可信包；公开租约则显式清除包上下文。
                managed_package_context: session.path_context.managed_package.as_ref().map_or(
                    AnonymousLuaManagedPackageContext::Clear,
                    AnonymousLuaManagedPackageContext::Set,
                ),
            },
        )?;
        populate_system_plugin_runtime_context(
            &session.vm.lua,
            session.path_context.managed_package.as_ref(),
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
        if let Some(transaction_guard) = transaction_guard {
            transaction_guard.commit()?;
        }
        Ok(json_result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::LuaVm;
    #[cfg(windows)]
    use super::normalize_optional_runtime_lease_path;
    use super::{
        RuntimeLeasePathContext, RuntimeLeaseProfile, RuntimeSessionManager,
        RuntimeSessionTerminalState, next_runtime_session_generation,
        next_runtime_session_sequence,
    };
    use crate::runtime::managed_package::{
        ManagedRuntimeLeaseBinding, ManagedRuntimePackageContext,
    };
    use crate::runtime::path::render_host_visible_path;
    use mlua::{Lua, UserData};
    use serde_json::{Value, json};
    use std::fs;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// Verify host-injected lease paths drop Windows verbatim prefixes before retention.
    /// 验证宿主注入的租约路径会在留存前移除 Windows 逐字路径前缀。
    #[cfg(windows)]
    #[test]
    fn runtime_lease_path_normalization_strips_windows_verbatim_prefix() {
        // Normalized lease path retained by the ordinary and System lease context builders.
        // 普通租约与 System 租约上下文构造器留存的归一化路径。
        let normalized = normalize_optional_runtime_lease_path(
            Some(r"\\?\C:\workspace\plugin"),
            "workspace_root",
        )
        .expect("normalize runtime lease path")
        .expect("non-empty runtime lease path");
        assert_eq!(normalized, std::path::Path::new(r"C:\workspace\plugin"));
    }

    /// Verify host-injected leases reject Windows verbatim namespaces without safe equivalents.
    /// 验证宿主注入租约会拒绝不存在安全等价形式的 Windows verbatim 命名空间。
    #[cfg(windows)]
    #[test]
    fn runtime_lease_path_normalization_rejects_unsupported_verbatim_namespace() {
        // Stable validation error returned before canonicalization or identity capture.
        // 在规范化或捕获身份前返回的稳定校验错误。
        let error = normalize_optional_runtime_lease_path(
            Some(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\workspace"),
            "workspace_root",
        )
        .expect_err("unsupported lease namespace must fail");
        assert!(
            error.contains("unsupported Windows verbatim path namespace"),
            "unexpected lease path error: {error}"
        );
    }

    /// Userdata probe that records whether manager state is unlocked during Lua VM destruction.
    /// 在 Lua VM 析构期间记录管理器状态是否已解锁的 userdata 探针。
    struct ManagerStateDropProbe {
        /// Manager whose state lock must be available when this probe is destroyed.
        /// 当前探针析构时状态锁必须可用的管理器。
        manager: Arc<RuntimeSessionManager>,
        /// One-shot observation channel returned to the owning test.
        /// 返回给所属测试的一次性观测通道。
        observation: mpsc::Sender<bool>,
    }

    impl UserData for ManagerStateDropProbe {}

    impl Drop for ManagerStateDropProbe {
        /// Report whether the manager state lock is available before Lua finishes destroying userdata.
        /// 在 Lua 完成 userdata 销毁前报告管理器状态锁是否可用。
        fn drop(&mut self) {
            // Non-blocking lock result distinguishes correct lock-free teardown from nested destruction.
            // 非阻塞锁结果用于区分正确的锁外清理与嵌套在锁内的析构。
            let manager_state_is_unlocked = self.manager.state.try_lock().is_ok();
            let _ = self.observation.send(manager_state_is_unlocked);
        }
    }

    /// Build the empty host-owned path context used by public manager unit tests.
    /// 构造公开管理器单元测试使用的空宿主路径上下文。
    ///
    /// Return one context without filesystem roots or a managed package identity.
    /// 返回不含文件系统根或受管包身份的上下文。
    fn public_test_path_context() -> RuntimeLeasePathContext {
        RuntimeLeasePathContext {
            cwd: None,
            cwd_identity: None,
            workspace_root: None,
            workspace_root_identity: None,
            lua_roots: Vec::new(),
            c_roots: Vec::new(),
            mounts: json!({}),
            system_lua_lib_dir: None,
            managed_package: None,
        }
    }

    /// Build one strict System path context around an already validated package.
    /// 围绕一个已验证包构造严格 System 路径上下文。
    ///
    /// `system_lua_root` and `package` provide the trusted root and exact package identity.
    /// `system_lua_root` 与 `package` 提供可信根及精确包身份。
    ///
    /// Return a context suitable for direct System manager insertion tests.
    /// 返回适用于直接 System 管理器插入测试的上下文。
    fn system_test_path_context(
        system_lua_root: &std::path::Path,
        package: Arc<ManagedRuntimePackageContext>,
    ) -> RuntimeLeasePathContext {
        RuntimeLeasePathContext {
            cwd: None,
            cwd_identity: None,
            workspace_root: None,
            workspace_root_identity: None,
            lua_roots: Vec::new(),
            c_roots: Vec::new(),
            mounts: json!({}),
            system_lua_lib_dir: Some(system_lua_root.to_path_buf()),
            managed_package: Some(package),
        }
    }

    /// Build one minimal Lua VM for direct manager insertion tests.
    /// 构造一个用于直接管理器插入测试的最小 Lua VM。
    ///
    /// Return a fresh VM with no drop observation userdata.
    /// 返回不含析构观测 userdata 的全新 VM。
    fn plain_test_lua_vm() -> LuaVm {
        LuaVm {
            lua: Lua::new(),
            last_used_at: Instant::now(),
        }
    }

    /// Build one Lua VM whose final userdata destructor probes the manager lock.
    /// 构造一个会在最终 userdata 析构时探测管理器锁的 Lua VM。
    ///
    /// `manager` is the manager expected to be unlocked and `observation` receives the result.
    /// `manager` 是预期已解锁的管理器，`observation` 用于接收结果。
    ///
    /// Return a fresh VM retaining the probe in its global table.
    /// 返回在全局表中持有探针的全新 VM。
    fn drop_probed_test_lua_vm(
        manager: &Arc<RuntimeSessionManager>,
        observation: mpsc::Sender<bool>,
    ) -> LuaVm {
        // Lua state that owns the drop probe until the complete VM is destroyed.
        // 持有析构探针直到完整 VM 被销毁的 Lua 状态。
        let lua = Lua::new();
        // Rust userdata whose destructor performs the non-blocking state-lock observation.
        // 析构函数执行非阻塞状态锁观测的 Rust userdata。
        let probe = lua
            .create_userdata(ManagerStateDropProbe {
                manager: Arc::clone(manager),
                observation,
            })
            .expect("manager state drop probe should be created");
        lua.globals()
            .set("__runtime_session_manager_drop_probe", probe)
            .expect("manager state drop probe should be retained by Lua globals");
        LuaVm {
            lua,
            last_used_at: Instant::now(),
        }
    }

    /// Insert one direct public manager lease and return its stable response payload.
    /// 直接插入一个公开管理器租约并返回其稳定响应载荷。
    ///
    /// `manager`, `sid`, `replace`, and `vm` select the exact manager transaction under test.
    /// `manager`、`sid`、`replace` 与 `vm` 选择被测的精确管理器事务。
    ///
    /// Return the successful create payload or panic with the manager diagnostic.
    /// 返回成功创建载荷；失败时携带管理器诊断触发 panic。
    fn insert_public_test_lease(
        manager: &RuntimeSessionManager,
        sid: &str,
        replace: bool,
        vm: LuaVm,
    ) -> Value {
        manager
            .insert(
                RuntimeLeaseProfile::Public,
                sid.to_string(),
                None,
                replace,
                public_test_path_context(),
                vm,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "test runtime lease insertion failed: {}: {}",
                    error.code, error.message
                )
            })
    }

    /// Extract one lease identifier from a successful direct-manager response.
    /// 从成功的直接管理器响应中提取一个租约标识。
    ///
    /// `payload` is the response returned by `insert_public_test_lease`.
    /// `payload` 是 `insert_public_test_lease` 返回的响应。
    ///
    /// Return the owned opaque lease identifier.
    /// 返回自有的不透明租约标识。
    fn test_lease_id(payload: &Value) -> String {
        payload["lease_id"]
            .as_str()
            .expect("test lease id should be a string")
            .to_string()
    }

    /// Assert one drop observation proves destruction happened after manager unlock.
    /// 断言一次析构观测证明销毁发生在管理器解锁之后。
    ///
    /// `observation` receives the probe result and `operation` labels diagnostics.
    /// `observation` 接收探针结果，`operation` 用于标记诊断。
    fn assert_drop_observed_after_manager_unlock(
        observation: mpsc::Receiver<bool>,
        operation: &str,
    ) {
        // Bounded observation prevents a missing VM destructor from hanging the test suite.
        // 有界观测避免缺失的 VM 析构永久挂起测试套件。
        let manager_state_was_unlocked = observation
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("{operation} VM drop was not observed: {error}"));
        assert!(
            manager_state_was_unlocked,
            "{operation} destroyed a runtime-session VM while holding manager state"
        );
    }

    /// Assert replacement failures preserve the exact old active lease and all typed indexes.
    /// 断言替换失败会保留精确旧活跃租约及全部类型化索引。
    ///
    /// `manager`, `sid`, `lease_id`, and `generation` identify the expected unchanged lease.
    /// `manager`、`sid`、`lease_id` 与 `generation` 标识预期保持不变的租约。
    fn assert_test_lease_remains_active(
        manager: &RuntimeSessionManager,
        sid: &str,
        lease_id: &str,
        generation: u64,
    ) {
        // Manager state snapshot proving both active indexes still select the old entry.
        // 证明两个活跃索引仍选择旧条目的管理器状态快照。
        let state = manager
            .lock_state()
            .expect("runtime session manager state should remain available");
        assert_eq!(state.sid_index.get(sid).map(String::as_str), Some(lease_id));
        // Exact old active entry whose typed identity and terminal marker must remain unchanged.
        // 类型化身份与终态标记必须保持不变的精确旧活跃条目。
        let entry = state
            .leases
            .get(lease_id)
            .expect("old runtime lease should remain active");
        assert_eq!(entry.sid, sid);
        assert_eq!(entry.generation, generation);
        assert_eq!(
            entry
                .terminal_state
                .load(std::sync::atomic::Ordering::Acquire),
            RuntimeSessionTerminalState::Active as u8
        );
        assert!(!state.tombstones.contains_key(lease_id));
    }

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

    /// Verify lease sequence exhaustion returns a stable error instead of reusing an identifier.
    /// 验证租约序号耗尽会返回稳定错误，而不是复用标识符。
    #[test]
    fn runtime_session_sequence_exhaustion_is_explicit() {
        // Error produced at the only sequence value without a valid successor.
        // 在唯一不存在有效后继值的序号上产生的错误。
        let error = next_runtime_session_sequence(u64::MAX)
            .expect_err("maximum lease sequence must be rejected");
        assert_eq!(error.code, "lease_sequence_exhausted");
        assert!(error.message.contains("sequence is exhausted"));
    }

    /// Verify SID generation exhaustion returns a stable error instead of wrapping to zero.
    /// 验证 SID 代际耗尽会返回稳定错误，而不是回绕到零。
    #[test]
    fn runtime_session_generation_exhaustion_is_explicit() {
        // Error bound to the exact SID whose generation space is exhausted.
        // 绑定到代际空间已耗尽精确 SID 的错误。
        let error = next_runtime_session_generation(u64::MAX, "exhausted-sid")
            .expect_err("maximum lease generation must be rejected");
        assert_eq!(error.code, "lease_generation_exhausted");
        assert!(error.message.contains("`exhausted-sid`"));
    }

    /// Verify every fallible replacement validation preserves the exact old active lease.
    /// 验证每一项可失败替换校验都会保留精确旧活跃租约。
    #[test]
    fn runtime_session_failed_replace_is_atomic() {
        // Shared manager retained by pending-VM drop probes during each rejected replacement.
        // 在每次被拒绝替换期间由待提交 VM 析构探针共同持有的管理器。
        let manager = Arc::new(RuntimeSessionManager::new());
        // Stable SID reused by the active lease and every attempted replacement.
        // 由活跃租约与每次替换尝试复用的稳定 SID。
        let sid = "atomic-replace";
        // Initial active lease whose identity must survive every validation failure.
        // 身份必须在每次校验失败后存活的初始活跃租约。
        let created = insert_public_test_lease(&manager, sid, false, plain_test_lua_vm());
        let lease_id = test_lease_id(&created);
        let generation = created["generation"]
            .as_u64()
            .expect("initial test lease generation");

        {
            // Exhausted global sequence installed without touching the active lease indexes.
            // 在不触及活跃租约索引的前提下安装的已耗尽全局序号。
            let mut state = manager
                .lock_state()
                .expect("set exhausted runtime lease sequence");
            state.next_sequence = u64::MAX;
        }
        // Pending replacement VM probe used to verify rejection teardown is also lock-free.
        // 用于验证拒绝路径清理同样位于锁外的待替换 VM 探针。
        let (sequence_drop_tx, sequence_drop_rx) = mpsc::channel();
        let sequence_error = manager
            .insert(
                RuntimeLeaseProfile::Public,
                sid.to_string(),
                None,
                true,
                public_test_path_context(),
                drop_probed_test_lua_vm(&manager, sequence_drop_tx),
            )
            .expect_err("sequence exhaustion should reject replacement");
        assert_eq!(sequence_error.code, "lease_sequence_exhausted");
        assert_drop_observed_after_manager_unlock(sequence_drop_rx, "sequence rejection");
        assert_test_lease_remains_active(&manager, sid, &lease_id, generation);

        {
            // Restore the last committed counter before testing System package validation.
            // 在测试 System 包校验前恢复最近一次已提交计数器。
            let mut state = manager
                .lock_state()
                .expect("restore committed runtime lease sequence");
            state.next_sequence = 1;
        }
        // Pending System replacement lacks its mandatory package binding by construction.
        // 按构造即缺少必需包 binding 的待提交 System 替换。
        let (binding_drop_tx, binding_drop_rx) = mpsc::channel();
        let binding_error = manager
            .insert(
                RuntimeLeaseProfile::SystemLuaLib,
                sid.to_string(),
                None,
                true,
                public_test_path_context(),
                drop_probed_test_lua_vm(&manager, binding_drop_tx),
            )
            .expect_err("missing System package should reject replacement");
        assert_eq!(binding_error.code, "system_package_missing");
        assert_drop_observed_after_manager_unlock(binding_drop_rx, "System binding rejection");
        assert_test_lease_remains_active(&manager, sid, &lease_id, generation);
        {
            // Failed binding validation must not publish either precomputed identity counter.
            // binding 校验失败不得发布任一预计算身份计数器。
            let state = manager
                .lock_state()
                .expect("inspect counters after failed System binding validation");
            assert_eq!(state.next_sequence, 1);
            assert_eq!(state.generations.get(sid).copied(), Some(generation));
        }

        {
            // Exhausted SID-local generation installed independently from the valid sequence.
            // 独立于有效序号安装的已耗尽 SID 局部代际。
            let mut state = manager
                .lock_state()
                .expect("set exhausted runtime lease generation");
            state.generations.insert(sid.to_string(), u64::MAX);
        }
        // Pending generation replacement VM whose rejected destruction must remain lock-free.
        // 被拒绝销毁仍必须保持锁外执行的待代际替换 VM。
        let (generation_drop_tx, generation_drop_rx) = mpsc::channel();
        let generation_error = manager
            .insert(
                RuntimeLeaseProfile::Public,
                sid.to_string(),
                None,
                true,
                public_test_path_context(),
                drop_probed_test_lua_vm(&manager, generation_drop_tx),
            )
            .expect_err("generation exhaustion should reject replacement");
        assert_eq!(generation_error.code, "lease_generation_exhausted");
        assert_drop_observed_after_manager_unlock(generation_drop_rx, "generation rejection");
        assert_test_lease_remains_active(&manager, sid, &lease_id, generation);
    }

    /// Verify an already-bound System package cannot partially replace the old active System lease.
    /// 验证已绑定 System 包无法局部替换旧的活跃 System 租约。
    #[test]
    fn runtime_session_system_binding_failure_preserves_old_lease() {
        // Nanosecond suffix isolates the real package layout from parallel lease tests.
        // 纳秒后缀用于将真实包布局与并行租约测试隔离。
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        // Real runtime, trust-root, package, and dependency paths consumed by production validation.
        // 由生产校验消费的真实运行时、可信根、包与依赖路径。
        let runtime_root = std::env::temp_dir().join(format!(
            "luaskills_atomic_system_binding_{}_{}",
            std::process::id(),
            unique_suffix
        ));
        let system_lua_root = runtime_root.join("system_lua_lib");
        let package_root = system_lua_root.join("atomic-system-package");
        let dependencies_file = package_root.join("dependencies.yaml");
        fs::create_dir_all(&package_root).expect("create atomic System package root");
        fs::write(&dependencies_file, "{}\n")
            .expect("write atomic System package dependency manifest");
        // Canonical trust root reused by both independently owned package contexts.
        // 由两个独立所有包上下文复用的规范可信根。
        let canonical_system_lua_root =
            fs::canonicalize(&system_lua_root).expect("canonicalize atomic System trust root");
        // Stable SID shared by the old active lease and rejected replacement.
        // 由旧活跃租约与被拒绝替换共享的稳定 SID。
        let sid = "atomic-system-binding";
        // Fresh unbound metadata used by the initial valid System lease.
        // 由初始有效 System 租约使用的全新未绑定元数据。
        let initial_binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            sid.to_string(),
            None,
            json!({}),
        ));
        // First validated package lifetime whose manager identity is bound during insertion.
        // 在插入期间绑定管理器身份的首个已验证包生命周期。
        let initial_package = ManagedRuntimePackageContext::for_system_plugin(
            "atomic-system-package",
            &package_root,
            &runtime_root,
            &canonical_system_lua_root,
            "dependencies.yaml",
            initial_binding,
        )
        .expect("build initial atomic System package context");
        // Manager containing the old active System lease protected by replacement atomicity.
        // 包含受替换原子性保护的旧活跃 System 租约的管理器。
        let manager = Arc::new(RuntimeSessionManager::new());
        let created = manager
            .insert(
                RuntimeLeaseProfile::SystemLuaLib,
                sid.to_string(),
                None,
                false,
                system_test_path_context(&canonical_system_lua_root, initial_package),
                plain_test_lua_vm(),
            )
            .expect("initial atomic System lease should be inserted");
        let lease_id = test_lease_id(&created);
        let generation = created["generation"]
            .as_u64()
            .expect("initial atomic System generation");

        // Deliberately pre-bound metadata that forces the real single-assignment bind call to fail.
        // 有意预绑定、用于迫使真实单次赋值 bind 调用失败的元数据。
        let replacement_binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            sid.to_string(),
            None,
            json!({}),
        ));
        replacement_binding
            .bind("already-bound-test-lease".to_string(), 99)
            .expect("replacement binding should be pre-bound exactly once");
        // Second validated package lifetime carrying the deliberately invalid binding state.
        // 携带有意非法 binding 状态的第二个已验证包生命周期。
        let replacement_package = ManagedRuntimePackageContext::for_system_plugin(
            "atomic-system-package",
            &package_root,
            &runtime_root,
            &canonical_system_lua_root,
            "dependencies.yaml",
            replacement_binding,
        )
        .expect("build replacement atomic System package context");
        // Pending replacement VM probe also proves binding rejection teardown is lock-free.
        // 同时证明 binding 拒绝路径清理位于锁外的待替换 VM 探针。
        let (drop_tx, drop_rx) = mpsc::channel();
        let error = manager
            .insert(
                RuntimeLeaseProfile::SystemLuaLib,
                sid.to_string(),
                None,
                true,
                system_test_path_context(&canonical_system_lua_root, replacement_package),
                drop_probed_test_lua_vm(&manager, drop_tx),
            )
            .expect_err("already-bound System package should reject replacement");
        assert_eq!(error.code, "system_package_binding_failed");
        assert_drop_observed_after_manager_unlock(drop_rx, "System binding failure");
        assert_test_lease_remains_active(&manager, sid, &lease_id, generation);
        {
            // Failed binding must leave both manager identity counters at the old committed values.
            // binding 失败必须让两个管理器身份计数器保持旧的已提交值。
            let state = manager
                .lock_state()
                .expect("inspect counters after failed System bind");
            assert_eq!(state.next_sequence, 1);
            assert_eq!(state.generations.get(sid).copied(), Some(generation));
        }

        manager
            .close(
                &lease_id,
                Some(sid),
                Some(generation),
                Some(RuntimeLeaseProfile::SystemLuaLib),
            )
            .expect("initial atomic System lease should close");
        drop(manager);
        fs::remove_dir_all(&runtime_root).expect("remove atomic System package layout");
    }

    /// Verify close detaches the retired VM before its destructor can observe manager state.
    /// 验证 close 会先摘除退役 VM，再让其析构函数观测管理器状态。
    #[test]
    fn runtime_session_close_destroys_vm_after_manager_unlock() {
        // Manager and channel retained until the closed VM reports its lock observation.
        // 保留到已关闭 VM 报告锁观测结果的管理器与通道。
        let manager = Arc::new(RuntimeSessionManager::new());
        let (drop_tx, drop_rx) = mpsc::channel();
        // Active lease whose Lua state owns the lock-observing userdata.
        // Lua 状态拥有锁观测 userdata 的活跃租约。
        let created = insert_public_test_lease(
            &manager,
            "lock-free-close",
            false,
            drop_probed_test_lua_vm(&manager, drop_tx),
        );
        let lease_id = test_lease_id(&created);

        manager
            .close(
                &lease_id,
                Some("lock-free-close"),
                created["generation"].as_u64(),
                Some(RuntimeLeaseProfile::Public),
            )
            .expect("test runtime lease should close");
        assert_drop_observed_after_manager_unlock(drop_rx, "close");
    }

    /// Verify replace destroys the detached old VM only after publishing the new lease and unlocking.
    /// 验证 replace 仅在发布新租约并解锁后销毁已摘除旧 VM。
    #[test]
    fn runtime_session_replace_destroys_old_vm_after_manager_unlock() {
        // Manager and channel retained until the replaced VM reports its lock observation.
        // 保留到被替换 VM 报告锁观测结果的管理器与通道。
        let manager = Arc::new(RuntimeSessionManager::new());
        let (drop_tx, drop_rx) = mpsc::channel();
        // Old active lease carrying the destruction probe.
        // 携带析构探针的旧活跃租约。
        let old_created = insert_public_test_lease(
            &manager,
            "lock-free-replace",
            false,
            drop_probed_test_lua_vm(&manager, drop_tx),
        );
        let old_lease_id = test_lease_id(&old_created);
        // Replacement committed under the same state lock before old VM destruction.
        // 在销毁旧 VM 前于同一状态锁内提交的替换租约。
        let replacement =
            insert_public_test_lease(&manager, "lock-free-replace", true, plain_test_lua_vm());
        assert_ne!(test_lease_id(&replacement), old_lease_id);
        assert_drop_observed_after_manager_unlock(drop_rx, "replace");

        manager
            .close(
                replacement["lease_id"]
                    .as_str()
                    .expect("replacement lease id"),
                Some("lock-free-replace"),
                replacement["generation"].as_u64(),
                Some(RuntimeLeaseProfile::Public),
            )
            .expect("replacement test lease should close");
    }

    /// Verify expiration pruning destroys the detached VM only after the listing call unlocks.
    /// 验证过期清理仅在列表调用解锁后销毁已摘除 VM。
    #[test]
    fn runtime_session_prune_destroys_vm_after_manager_unlock() {
        // Manager and channel retained until the pruned VM reports its lock observation.
        // 保留到被清理 VM 报告锁观测结果的管理器与通道。
        let manager = Arc::new(RuntimeSessionManager::new());
        let (drop_tx, drop_rx) = mpsc::channel();
        // Finite lease carrying the destruction probe and later forced past its deadline.
        // 携带析构探针、随后被强制越过截止时间的有限租约。
        let created = manager
            .insert(
                RuntimeLeaseProfile::Public,
                "lock-free-prune".to_string(),
                Some(1),
                false,
                public_test_path_context(),
                drop_probed_test_lua_vm(&manager, drop_tx),
            )
            .expect("finite test runtime lease should be inserted");
        let lease_id = test_lease_id(&created);
        // Session handle detached from manager state before mutating only its test deadline.
        // 在仅修改测试截止时间前从管理器状态复制出的会话句柄。
        let session = {
            let state = manager
                .lock_state()
                .expect("inspect finite test runtime lease");
            Arc::clone(
                &state
                    .leases
                    .get(&lease_id)
                    .expect("finite test runtime lease entry")
                    .session,
            )
        };
        {
            // Expired timestamp used to force the next list operation through the prune path.
            // 用于强制下一次列表操作进入清理路径的已过期时间戳。
            let mut session = session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            session.expires_at = Some(
                Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .expect("expired test timestamp"),
            );
        }
        // Release the copied session Arc so the retired entry is the sole VM owner during prune.
        // 释放复制的会话 Arc，使退役条目在清理期间成为 VM 的唯一所有者。
        drop(session);

        let listed = manager
            .list(None, Some(RuntimeLeaseProfile::Public))
            .expect("runtime lease list should prune expired entries");
        assert_eq!(listed["leases"], json!([]));
        assert_drop_observed_after_manager_unlock(drop_rx, "prune");
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
