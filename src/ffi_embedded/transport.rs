use super::types::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Exact transport owners indexed independently of the legacy engine registry.
/// 独立于旧引擎注册表、按精确身份索引的传输所有者。
static TRANSPORTS: OnceLock<Mutex<BTreeMap<u64, Arc<Transport>>>> = OnceLock::new();
/// Opaque native identities never wrap or repeat while this library remains loaded.
/// 此动态库保持加载期间，不透明原生身份绝不回绕或重复。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Short-lock ownership metadata; no VM execution, callbacks or foreign memory release occurs inside it.
/// 短锁所有权元数据；其中不执行 VM、回调或外部内存释放。
struct TransportState {
    /// New runtime creation is permanently disabled after close is requested.
    /// 请求关闭后永久禁用新运行时创建。
    closing: bool,
    /// A removed registry owner rejects calls from any previously cloned references.
    /// 已移除注册表所有者拒绝任何此前克隆引用的调用。
    released: bool,
    /// Pending requests reserve a result slot before any command mutation.
    /// 待完成请求在任何命令变更前预留结果槽。
    pending: usize,
    /// Published bytes plus maximum response reservations for actual pending calls.
    /// 已发布字节与实际待完成调用的最大响应预留。
    bytes: usize,
    /// Exact boxed allocations retained until explicit identity-checked release.
    /// 保留到显式身份校验释放的精确 boxed 分配。
    results: BTreeMap<u64, Box<[u8]>>,
}

/// Independent FFI client transport; configuration and outstanding results outlive individual calls.
/// 独立 FFI 客户端传输；配置和未释放结果比单次调用存活更久。
pub(super) struct Transport {
    /// Host-resolved immutable response and request budgets.
    /// 宿主解析的不可变响应与请求预算。
    pub(super) config: TransportConfig,
    /// Authoritative bounded ownership state.
    /// 权威有界所有权状态。
    state: Mutex<TransportState>,
}

/// Reserve one exact response before dispatch, retaining transport ownership through unwind and cancellation.
/// 分发前预留一个精确响应，跨栈展开与取消保留传输所有权。
pub(super) struct ResponseReservation {
    /// The actual transport cannot be released while this response is still being produced.
    /// 此响应仍在生成期间，实际传输不能释放。
    transport: Arc<Transport>,
    /// Unique allocation identity consumed even if the request is later rejected.
    /// 即使请求随后被拒绝也已消费的唯一分配身份。
    id: u64,
    /// Whether published ownership replaced this pending reservation.
    /// 已发布所有权是否已替代此待完成预留。
    published: bool,
}

/// Lock the independent registry; return an explicit error on poisoned authority.
/// 锁定独立注册表；权威中毒时返回明确错误。
fn registry() -> Result<MutexGuard<'static, BTreeMap<u64, Arc<Transport>>>, EmbeddedFfiStatus> {
    TRANSPORTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| EmbeddedFfiStatus::Internal)
}

/// Allocate a never-reused identity or reject exhaustion before publication.
/// 分配绝不复用的身份，或在发布前拒绝身份耗尽。
fn identity() -> Result<u64, EmbeddedFfiStatus> {
    NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| EmbeddedFfiStatus::Internal)
}

/// Create a transport from explicit validated `config`; no runtime or user code is constructed here.
/// 从显式已校验 `config` 创建传输；此处不构造运行时或执行用户代码。
pub(super) fn create(config: TransportConfig) -> Result<u64, EmbeddedFfiStatus> {
    let transport = Arc::new(Transport {
        config,
        state: Mutex::new(TransportState {
            closing: false,
            released: false,
            pending: 0,
            bytes: 0,
            results: BTreeMap::new(),
        }),
    });
    let id = identity()?;
    registry()?.insert(id, transport);
    Ok(id)
}

/// Clone exact `id` under a short lock; all subsequent work happens after the registry lock is released.
/// 在短锁下克隆精确 `id`；全部后续任务在注册表锁释放后运行。
pub(super) fn get(id: u64) -> Result<Arc<Transport>, EmbeddedFfiStatus> {
    registry()?
        .get(&id)
        .cloned()
        .ok_or(EmbeddedFfiStatus::NotFound)
}

/// Remove only closed and fully drained `id`; pre-existing clones are atomically prevented from reentering.
/// 仅移除已关闭且完全排空的 `id`；原有克隆引用被原子阻止再次入场。
pub(super) fn release(id: u64) -> Result<(), EmbeddedFfiStatus> {
    let removed = {
        let mut registry = registry()?;
        let transport = registry.get(&id).ok_or(EmbeddedFfiStatus::NotFound)?;
        let mut state = transport.lock()?;
        if !state.closing || state.pending != 0 || !state.results.is_empty() {
            return Err(EmbeddedFfiStatus::Busy);
        }
        state.released = true;
        drop(state);
        registry.remove(&id)
    };
    drop(removed);
    Ok(())
}

impl Transport {
    /// Lock actual metadata and reject infrastructure corruption explicitly.
    /// 锁定实际元数据，并明确拒绝基础设施损坏。
    fn lock(&self) -> Result<MutexGuard<'_, TransportState>, EmbeddedFfiStatus> {
        self.state.lock().map_err(|_| EmbeddedFfiStatus::Internal)
    }

    /// Request permanent creation closure; return without releasing outstanding buffers or requests.
    /// 请求永久关闭创建入口；返回时不释放未完成缓冲或请求。
    pub(super) fn request_close(&self) -> Result<(), EmbeddedFfiStatus> {
        let mut state = self.lock()?;
        if state.released {
            return Err(EmbeddedFfiStatus::Closed);
        }
        state.closing = true;
        Ok(())
    }

    /// Reserve result count and worst-case bytes before dispatch; closing still permits drain-control requests.
    /// 分发前预留结果数量与最坏情况字节；正在关闭仍允许排空控制请求。
    /// Return capacity errors before command side effects, never after silently discarding a result owner.
    /// 在命令副作用前返回容量错误，绝不静默丢弃结果所有者后才报错。
    pub(super) fn reserve(self: &Arc<Self>) -> Result<ResponseReservation, EmbeddedFfiStatus> {
        let mut state = self.lock()?;
        if state.released {
            return Err(EmbeddedFfiStatus::Closed);
        }
        if state.results.len() + state.pending >= self.config.max_result_buffers
            || self.config.max_response_bytes
                > self.config.max_result_bytes.saturating_sub(state.bytes)
        {
            return Err(EmbeddedFfiStatus::CapacityExceeded);
        }
        let id = identity()?;
        state.pending += 1;
        state.bytes += self.config.max_response_bytes;
        Ok(ResponseReservation {
            transport: Arc::clone(self),
            id,
            published: false,
        })
    }

    /// Release only `result` with matching identity, pointer and length; never dereference a supplied result pointer.
    /// 仅释放身份、指针与长度匹配的 `result`；绝不解引用传入的结果指针。
    /// Return a stable error for duplicate, stale, foreign or altered result descriptors.
    /// 为重复、陈旧、外来或被修改的结果描述符返回稳定错误。
    pub(super) fn free_result(&self, result: FfiEmbeddedResultV1) -> Result<(), EmbeddedFfiStatus> {
        let removed = {
            let mut state = self.lock()?;
            if state.released {
                return Err(EmbeddedFfiStatus::Closed);
            }
            let owned = state
                .results
                .get(&result.allocation_id)
                .ok_or(EmbeddedFfiStatus::NotFound)?;
            if owned.as_ptr() != result.ptr || owned.len() != result.len {
                return Err(EmbeddedFfiStatus::InvalidArgument);
            }
            state.bytes -= owned.len();
            state.results.remove(&result.allocation_id)
        };
        drop(removed);
        Ok(())
    }
}

impl ResponseReservation {
    /// Publish owned `bytes` within the pre-admitted maximum; return a read-only descriptor with one release authority.
    /// 在预先接纳的最大值内发布拥有所有权的 `bytes`；返回具有一个释放权威的只读描述符。
    pub(super) fn publish(
        mut self,
        bytes: Vec<u8>,
    ) -> Result<FfiEmbeddedResultV1, EmbeddedFfiStatus> {
        if bytes.is_empty() || bytes.len() > self.transport.config.max_response_bytes {
            return Err(EmbeddedFfiStatus::CapacityExceeded);
        }
        let bytes = bytes.into_boxed_slice();
        let result = FfiEmbeddedResultV1 {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
            allocation_id: self.id,
        };
        {
            let mut state = self.transport.lock()?;
            state.bytes -= self.transport.config.max_response_bytes - bytes.len();
            state.pending -= 1;
            state.results.insert(self.id, bytes);
            self.published = true;
        }
        Ok(result)
    }
}

impl Drop for ResponseReservation {
    /// Release only unpublished reservation metadata, including when parsing or command execution unwinds.
    /// 仅释放未发布预留元数据，包含解析或命令执行栈展开时。
    fn drop(&mut self) {
        if !self.published {
            let mut state = self
                .transport
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending -= 1;
            state.bytes -= self.transport.config.max_response_bytes;
        }
    }
}
