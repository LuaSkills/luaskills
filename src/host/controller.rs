use crate::host::database::RuntimeDatabaseBindingContext;
use crate::host::options::{LuaRuntimeHostOptions, LuaRuntimeSpaceControllerProcessMode};
use crate::runtime::path::render_host_visible_path;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::{Handle, Runtime};
use vldb_controller_client::{
    BoxError, ClientRegistration, ControllerClient, ControllerClientConfig, ControllerProcessMode,
    SpaceKind, SpaceRegistration,
};

/// Shared host-side controller bridge that executes async controller SDK calls from sync runtime code.
/// 供同步运行时代码调用异步控制器 SDK 的共享宿主桥接。
pub struct LuaRuntimeSpaceControllerBridge {
    /// Cloneable SDK client shared by concurrent controller requests.
    /// 由并发控制器请求共享的可克隆 SDK 客户端。
    client: ControllerClient,
    /// Multi-thread Tokio runtime that schedules controller futures concurrently.
    /// 并发调度控制器 future 的多线程 Tokio 运行时。
    runtime: Runtime,
    /// Stable controller client-session scope used to isolate binding identifiers.
    /// 用于隔离绑定标识的稳定控制器客户端会话作用域。
    binding_scope_id: String,
}

/// Controller identifiers resolved after one runtime database binding has been attached.
/// 一个运行时数据库绑定完成 attach 后解析得到的控制器标识集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LuaRuntimeSpaceControllerBindingIds {
    /// Stable controller space identifier derived from the host binding context.
    /// 基于宿主绑定上下文派生出的稳定控制器空间标识。
    pub(crate) space_id: String,
    /// Client-scoped controller binding identifier derived from the host binding tag.
    /// 基于宿主绑定标签派生出的客户端隔离控制器绑定标识。
    pub(crate) binding_id: String,
}

impl LuaRuntimeSpaceControllerBridge {
    /// Build one controller bridge from host options and one stable backend-specific registration suffix.
    /// 基于宿主选项与稳定的后端注册后缀构建一个控制器桥接。
    pub fn new(
        host_options: &LuaRuntimeHostOptions,
        backend_suffix: &str,
    ) -> Result<Arc<Self>, String> {
        let controller_options = &host_options.space_controller;
        let endpoint = controller_options
            .endpoint
            .clone()
            .unwrap_or_else(|| "http://127.0.0.1:19801".to_string());
        let process_id = std::process::id();
        // Wall-clock registration timestamp included in the controller client name.
        // 包含在控制器客户端名称中的墙钟注册时间戳。
        let started_at_ms = system_time_to_controller_start_unix_millis(
            SystemTime::now(),
            "space controller client registration timestamp",
        )?;
        let registration = ClientRegistration {
            client_name: format!(
                "luaskills-{}-{}-{}",
                process_id, backend_suffix, started_at_ms
            ),
            host_kind: "luaskills".to_string(),
            process_id,
            process_name: backend_suffix.to_string(),
            lease_ttl_secs: Some(controller_options.default_lease_ttl_secs),
        };
        let config = ControllerClientConfig {
            endpoint,
            auto_spawn: controller_options.auto_spawn,
            spawn_executable: controller_options
                .executable_path
                .as_ref()
                .map(|path| render_host_visible_path(path)),
            spawn_process_mode: map_process_mode(controller_options.process_mode),
            minimum_uptime_secs: controller_options.minimum_uptime_secs,
            idle_timeout_secs: controller_options.idle_timeout_secs,
            default_lease_ttl_secs: controller_options.default_lease_ttl_secs,
            connect_timeout_secs: controller_options.connect_timeout_secs,
            startup_timeout_secs: controller_options.startup_timeout_secs,
            startup_retry_interval_ms: controller_options.startup_retry_interval_ms,
            lease_renew_interval_secs: controller_options.lease_renew_interval_secs,
        };
        let runtime = Runtime::new()
            .map_err(|error| format!("failed to create controller tokio runtime: {}", error))?;
        let client = ControllerClient::new(config, registration);
        run_controller_operation_with_client(&runtime, &client, |client| async move {
            client.connect().await
        })
        .map_err(|error| format!("failed to connect space controller client: {}", error))?;
        let binding_scope_id =
            resolve_controller_binding_scope_id(&runtime, &client).map_err(|error| {
                format!(
                    "failed to resolve space controller session scope: {}",
                    error
                )
            })?;
        Ok(Arc::new(Self {
            client,
            runtime,
            binding_scope_id,
        }))
    }

    /// Execute one controller SDK operation while transparently handling sync and async host threads.
    /// 透明兼容同步线程与异步宿主线程，执行一次控制器 SDK 操作。
    pub fn run<F, Fut, T>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce(ControllerClient) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, BoxError>> + Send + 'static,
        T: Send + 'static,
    {
        run_controller_operation_with_client(&self.runtime, &self.client, operation)
            .map_err(|error| format!("space controller request failed: {}", error))
    }

    /// Attach one stable binding context as one controller space before backend operations start.
    /// 在后端操作开始前，把稳定绑定上下文附着为一个控制器空间。
    pub fn attach_binding(&self, binding: &RuntimeDatabaseBindingContext) -> Result<(), String> {
        let registration = SpaceRegistration {
            space_id: controller_space_id_for_binding(binding),
            space_label: binding.space_label.clone(),
            space_kind: map_space_kind(&binding.space_label),
            space_root: binding.space_root.clone(),
        };
        self.run(move |client| async move { client.attach_space(registration).await })
            .map(|_| ())
    }

    /// Attach one database binding and return the controller identifiers needed by backend enable calls.
    /// 附着一个数据库绑定，并返回后端启用调用所需的控制器标识。
    ///
    /// The binding parameter is the stable host-facing database binding context.
    /// binding 参数是稳定的宿主侧数据库绑定上下文。
    ///
    /// Return the runtime-space id and client-scoped binding id resolved for this bridge session.
    /// 返回为当前桥接会话解析出的运行时空间标识与客户端隔离绑定标识。
    pub(crate) fn attach_binding_with_ids(
        &self,
        binding: &RuntimeDatabaseBindingContext,
    ) -> Result<LuaRuntimeSpaceControllerBindingIds, String> {
        let ids = self.binding_ids_for_binding(binding);
        self.attach_binding(binding)?;
        Ok(ids)
    }

    /// Resolve controller identifiers for one database binding without attaching it again.
    /// 为一个数据库绑定解析控制器标识，但不再次执行 attach。
    ///
    /// The binding parameter is the stable host-facing database binding context.
    /// binding 参数是稳定的宿主侧数据库绑定上下文。
    ///
    /// Return the runtime-space id and client-scoped binding id for this bridge session.
    /// 返回当前桥接会话中的运行时空间标识与客户端隔离绑定标识。
    pub(crate) fn binding_ids_for_binding(
        &self,
        binding: &RuntimeDatabaseBindingContext,
    ) -> LuaRuntimeSpaceControllerBindingIds {
        LuaRuntimeSpaceControllerBindingIds {
            space_id: controller_space_id_for_binding(binding),
            binding_id: self.controller_binding_id_for_binding(binding),
        }
    }

    /// Build one client-scoped controller binding identifier while preserving the stable host binding tag for diagnostics.
    /// 构造一个按客户端实例隔离的控制器绑定标识，同时保留稳定宿主绑定标签用于诊断。
    pub fn controller_binding_id_for_binding(
        &self,
        binding: &RuntimeDatabaseBindingContext,
    ) -> String {
        build_controller_binding_id(binding.binding_tag.as_str(), self.binding_scope_id.as_str())
    }
}

/// Execute one controller SDK operation safely from both sync code and threads already inside a Tokio runtime.
/// 兼容同步代码与已处于 Tokio 运行时中的线程，安全执行一次控制器 SDK 操作。
fn run_controller_operation_with_client<F, Fut, T>(
    runtime: &Runtime,
    client: &ControllerClient,
    operation: F,
) -> Result<T, BoxError>
where
    F: FnOnce(ControllerClient) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, BoxError>> + Send + 'static,
    T: Send + 'static,
{
    let client_clone = client.clone();
    run_future_on_bridge_runtime(runtime, operation(client_clone))
}

/// Resolve the current controller client session identifier and use it as the binding scope for this bridge instance.
/// 解析当前控制器客户端会话标识，并将其作为本桥接实例的绑定作用域。
fn resolve_controller_binding_scope_id(
    runtime: &Runtime,
    client: &ControllerClient,
) -> Result<String, BoxError> {
    run_controller_operation_with_client(runtime, client, |client| async move {
        let mut snapshots = client.list_clients().await?.into_iter();
        let snapshot = snapshots.next().ok_or_else(|| -> BoxError {
            "space controller client did not expose one visible client session".into()
        })?;
        if snapshots.next().is_some() {
            return Err::<String, BoxError>(
                "space controller client exposed multiple visible client sessions".into(),
            );
        }
        Ok(snapshot.client_session_id)
    })
}

/// Execute one Send future on the bridge-owned Tokio runtime without depending on the host runtime flavor.
/// 在桥接持有的 Tokio 运行时上执行一个可发送 future，并且不依赖宿主运行时 flavor。
fn run_future_on_bridge_runtime<Fut, T>(runtime: &Runtime, future: Fut) -> Result<T, BoxError>
where
    Fut: Future<Output = Result<T, BoxError>> + Send + 'static,
    T: Send + 'static,
{
    if Handle::try_current().is_ok() {
        return run_future_on_bridge_runtime_handle(runtime.handle().clone(), future);
    }
    runtime.block_on(future)
}

/// Dispatch one future onto the bridge runtime worker threads and wait synchronously for the result.
/// 把一个 future 分发到桥接运行时的工作线程上，并同步等待执行结果。
fn run_future_on_bridge_runtime_handle<Fut, T>(
    runtime_handle: Handle,
    future: Fut,
) -> Result<T, BoxError>
where
    Fut: Future<Output = Result<T, BoxError>> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    runtime_handle.spawn(async move {
        let result = future.await;
        let _ = sender.send(result);
    });
    receiver
        .recv()
        .unwrap_or_else(|_| Err("space controller task channel closed".into()))
}

impl Drop for LuaRuntimeSpaceControllerBridge {
    /// Best-effort shutdown the controller client when the bridge goes away.
    /// 在桥接析构时尽力关闭控制器客户端。
    fn drop(&mut self) {
        let client = self.client.clone();
        let _ = thread::Builder::new()
            .name("vulcan-space-controller-shutdown".to_string())
            .spawn(move || {
                let Ok(runtime) = Runtime::new() else {
                    return;
                };
                runtime.block_on(async move {
                    let _ =
                        tokio::time::timeout(Duration::from_millis(250), client.shutdown()).await;
                });
            });
    }
}

/// Map the host-facing process mode into the controller client SDK process mode.
/// 把宿主侧进程模式映射成控制器客户端 SDK 进程模式。
fn map_process_mode(mode: LuaRuntimeSpaceControllerProcessMode) -> ControllerProcessMode {
    match mode {
        LuaRuntimeSpaceControllerProcessMode::Service => ControllerProcessMode::Service,
        LuaRuntimeSpaceControllerProcessMode::Managed => ControllerProcessMode::Managed,
    }
}

/// Map one stable host space label into the controller SDK logical space kind.
/// 把稳定宿主空间标签映射成控制器 SDK 逻辑空间类型。
fn map_space_kind(space_label: &str) -> SpaceKind {
    match space_label.trim().to_ascii_uppercase().as_str() {
        "ROOT" => SpaceKind::Root,
        "USER" => SpaceKind::User,
        _ => SpaceKind::Project,
    }
}

/// Build the stable runtime-space identity used by the shared controller for one binding context.
/// 为单个绑定上下文构建供共享控制器使用的稳定运行时空间标识。
pub fn controller_space_id_for_binding(binding: &RuntimeDatabaseBindingContext) -> String {
    let normalized_label = normalize_controller_space_label(&binding.space_label);
    let mut digest = Sha256::new();
    digest.update(binding.space_label.trim().as_bytes());
    digest.update([0]);
    digest.update(binding.space_root.as_bytes());
    let hash_hex = format!("{:x}", digest.finalize());
    format!("{}-{}", normalized_label, &hash_hex[..16])
}

/// Build one controller binding identifier from the stable host binding tag and one bridge-scoped client session marker.
/// 基于稳定宿主绑定标签与桥接级客户端会话标识构造一个控制器绑定标识。
fn build_controller_binding_id(binding_tag: &str, binding_scope_id: &str) -> String {
    format!("{}@{}", binding_tag, binding_scope_id)
}

/// Convert one system time into the Unix millisecond component used by controller registrations.
/// 将单个系统时间转换为控制器注册使用的 Unix 毫秒组成部分。
///
/// The time parameter is the wall-clock timestamp to encode into the registration name.
/// time 参数是要编码进注册名称的墙钟时间戳。
///
/// The context parameter names the caller for precise error diagnostics.
/// context 参数命名调用方，用于精确错误诊断。
///
/// Returns the Unix millisecond timestamp used in the controller registration name.
/// 返回控制器注册名称使用的 Unix 毫秒时间戳。
///
/// Returns an error when the timestamp is before the Unix epoch.
/// 当时间戳早于 Unix epoch 时返回错误。
fn system_time_to_controller_start_unix_millis(
    time: SystemTime,
    context: &str,
) -> Result<u128, String> {
    // Duration measured from the Unix epoch for the controller registration timestamp.
    // 控制器注册时间戳相对于 Unix epoch 的持续时间。
    let duration = time.duration_since(UNIX_EPOCH).map_err(|error| {
        format!(
            "{} is before Unix epoch and cannot be used for a controller registration name: {}",
            context, error
        )
    })?;
    Ok(duration.as_millis())
}

/// Normalize one host-provided space label into a controller-safe identifier prefix.
/// 将宿主提供的空间标签标准化为控制器安全的标识符前缀。
fn normalize_controller_space_label(space_label: &str) -> String {
    let normalized: String = space_label
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if normalized.is_empty() {
        "SPACE".to_string()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LuaRuntimeSpaceControllerBridge, build_controller_binding_id, run_future_on_bridge_runtime,
        system_time_to_controller_start_unix_millis,
    };
    use crate::host::database::{
        RuntimeDatabaseBindingContext, RuntimeDatabaseBindingContextSpec, RuntimeDatabaseKind,
    };
    use crate::host::options::LuaRuntimeHostOptions;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::{Duration, Instant, UNIX_EPOCH};
    use tokio::runtime::{Builder, Runtime};
    use vldb_controller_client::BoxError;

    /// Build one controller bridge runtime used by bridge-execution tests.
    /// 构建一个供桥接执行测试使用的控制器运行时。
    fn build_bridge_runtime() -> Runtime {
        Runtime::new().expect("bridge runtime should build")
    }

    /// Return one zero-based nearest-rank percentile from sorted microsecond samples.
    /// 从已排序的微秒样本中返回一个从零开始的最邻近秩百分位数。
    ///
    /// Parameter `sorted_samples` contains ascending latency samples and must not be empty.
    /// 参数 `sorted_samples` 包含升序延迟样本，且不得为空。
    ///
    /// Parameter `percentile` is the inclusive percentile in the range 1 through 100.
    /// 参数 `percentile` 是 1 到 100 范围内的包含式百分位数。
    ///
    /// Returns the selected latency sample in microseconds.
    /// 返回选中的微秒延迟样本。
    fn nearest_rank_percentile_micros(sorted_samples: &[u128], percentile: usize) -> u128 {
        // Rank converts the one-based nearest-rank formula into a zero-based slice index.
        // Rank 将从一开始的最邻近秩公式转换为从零开始的切片索引。
        let rank = sorted_samples
            .len()
            .saturating_mul(percentile)
            .div_ceil(100)
            .saturating_sub(1)
            .min(sorted_samples.len().saturating_sub(1));
        sorted_samples[rank]
    }

    /// Measure real controller status RPC latency for one requested concurrency level.
    /// 测量指定并发级别下真实控制器状态 RPC 的延迟。
    ///
    /// Parameter `bridge` is connected to a real auto-spawned controller process.
    /// 参数 `bridge` 已连接到一个真实自动唤起的控制器进程。
    ///
    /// Parameter `concurrency` defines both synchronous caller threads and simultaneous request capacity.
    /// 参数 `concurrency` 同时定义同步调用线程数与并发请求容量。
    ///
    /// Returns sorted per-request microsecond samples plus the measured wall duration.
    /// 返回已排序的逐请求微秒样本与测得的墙钟时长。
    fn measure_real_controller_status_requests(
        bridge: &Arc<LuaRuntimeSpaceControllerBridge>,
        concurrency: usize,
    ) -> (Vec<u128>, Duration) {
        // RequestsPerCaller balances percentile sample size against one bounded acceptance run.
        // RequestsPerCaller 在百分位样本规模与有界验收运行之间取得平衡。
        const REQUESTS_PER_CALLER: usize = 8;
        // StartBarrier releases every synchronous caller together after all threads are ready.
        // StartBarrier 在所有线程就绪后同时释放每个同步调用方。
        let start_barrier = Arc::new(Barrier::new(concurrency + 1));
        // ResultChannel collects one latency batch from each caller without shared mutable vectors.
        // ResultChannel 从每个调用方收集一批延迟，避免共享可变向量。
        let (result_sender, result_receiver) = mpsc::channel();
        // Threads own the synchronous callers whose bridge requests must overlap safely.
        // Threads 持有桥接请求必须安全重叠的同步调用方。
        let mut threads = Vec::with_capacity(concurrency);

        for _ in 0..concurrency {
            // ThreadBridge shares the production bridge and its real controller client.
            // ThreadBridge 共享生产桥接及其真实控制器客户端。
            let thread_bridge = Arc::clone(bridge);
            // ThreadBarrier synchronizes this caller with the complete batch.
            // ThreadBarrier 将当前调用方与完整批次同步。
            let thread_barrier = Arc::clone(&start_barrier);
            // ThreadSender returns this caller's independent latency batch.
            // ThreadSender 返回当前调用方的独立延迟批次。
            let thread_sender = result_sender.clone();
            threads.push(thread::spawn(move || {
                thread_barrier.wait();
                // Latencies stores this caller's bounded real RPC measurements.
                // Latencies 保存当前调用方的有界真实 RPC 测量值。
                let mut latencies = Vec::with_capacity(REQUESTS_PER_CALLER);
                for _ in 0..REQUESTS_PER_CALLER {
                    // RequestStart bounds one controller get_status round trip.
                    // RequestStart 界定一次控制器 get_status 往返。
                    let request_start = Instant::now();
                    thread_bridge
                        .run(|client| async move { client.get_status().await })
                        .expect("real controller status request should succeed");
                    latencies.push(request_start.elapsed().as_micros());
                }
                thread_sender
                    .send(latencies)
                    .expect("send real controller latency batch");
            }));
        }
        drop(result_sender);
        // BatchStart measures all synchronized RPC calls at this concurrency level.
        // BatchStart 测量当前并发级别下全部同步 RPC 调用。
        let batch_start = Instant::now();
        start_barrier.wait();
        // Samples merges each caller batch after execution without affecting request timing.
        // Samples 在执行后合并每个调用方批次，不影响请求计时。
        let mut samples = result_receiver.into_iter().flatten().collect::<Vec<_>>();
        for thread in threads {
            thread
                .join()
                .expect("real controller request thread should not panic");
        }
        // BatchElapsed captures end-to-end synchronized completion time.
        // BatchElapsed 捕获同步批次的端到端完成时间。
        let batch_elapsed = batch_start.elapsed();
        samples.sort_unstable();
        (samples, batch_elapsed)
    }

    /// Verify controller registration timestamps accept normal post-epoch system times.
    /// 验证控制器注册时间戳会接受正常的 epoch 之后系统时间。
    #[test]
    fn controller_start_unix_millis_accepts_post_epoch_time() {
        // Timestamp one millisecond after the Unix epoch.
        // Unix epoch 之后一毫秒的时间戳。
        let timestamp = UNIX_EPOCH + Duration::from_millis(1);

        assert_eq!(
            system_time_to_controller_start_unix_millis(
                timestamp,
                "test controller registration timestamp"
            )
            .expect("post-epoch timestamp should convert"),
            1
        );
    }

    /// Verify controller registration timestamps reject pre-epoch system times.
    /// 验证控制器注册时间戳会拒绝早于 epoch 的系统时间。
    #[test]
    fn controller_start_unix_millis_rejects_pre_epoch_time() {
        // Timestamp one millisecond before the Unix epoch.
        // Unix epoch 之前一毫秒的时间戳。
        let timestamp = UNIX_EPOCH - Duration::from_millis(1);

        // Error returned for a pre-epoch controller registration timestamp conversion attempt.
        // 早于 epoch 的控制器注册时间戳转换尝试返回的错误。
        let error = system_time_to_controller_start_unix_millis(
            timestamp,
            "test controller registration timestamp",
        )
        .expect_err("pre-epoch timestamp should fail");

        assert!(
            error.starts_with(
                "test controller registration timestamp is before Unix epoch and cannot be used for a controller registration name:"
            ),
            "unexpected error: {}",
            error
        );
    }

    /// Verify bridge-owned futures still execute correctly for synchronous callers outside Tokio.
    /// 验证桥接持有的 future 在 Tokio 外部的同步调用方场景下仍能正确执行。
    #[test]
    fn bridge_runtime_executes_futures_for_sync_callers() {
        let runtime = build_bridge_runtime();
        let result = run_future_on_bridge_runtime(&runtime, async { Ok::<_, BoxError>(7usize) })
            .expect("sync caller path should succeed");
        assert_eq!(result, 7);
    }

    /// Verify bridge-owned futures do not panic when the host is already inside a current-thread Tokio runtime.
    /// 验证当宿主已经处于 current-thread Tokio 运行时中时，桥接持有的 future 不会触发 panic。
    #[test]
    fn bridge_runtime_executes_futures_inside_current_thread_tokio_runtime() {
        let bridge_runtime = build_bridge_runtime();
        let host_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread host runtime should build");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            host_runtime.block_on(async {
                run_future_on_bridge_runtime(&bridge_runtime, async { Ok::<_, BoxError>(11usize) })
                    .expect("current-thread caller path should succeed")
            })
        }))
        .expect("current-thread host runtime path should not panic");

        assert_eq!(result, 11);
    }

    /// Verify two synchronous callers can overlap on the bridge-owned runtime.
    /// 验证两个同步调用方可以在桥接持有的运行时上重叠执行。
    #[test]
    fn bridge_runtime_allows_concurrent_block_on_callers() {
        // runtime is shared directly without a request-wide mutex.
        // runtime 在没有请求级互斥锁的情况下被直接共享。
        let runtime = Arc::new(build_bridge_runtime());
        // start_barrier releases both synchronous callers at the same instant.
        // start_barrier 在同一时刻释放两个同步调用方。
        let start_barrier = Arc::new(Barrier::new(3));
        // active counts controller futures that have entered but not completed.
        // active 统计已经进入但尚未完成的控制器 future。
        let active = Arc::new(AtomicUsize::new(0));
        // entered is a monotonic rendezvous counter that cannot fall before both futures observe it.
        // entered 是单调汇合计数器，在两个 future 都观察到它之前不会下降。
        let entered = Arc::new(AtomicUsize::new(0));
        // maximum_active records the largest observed overlap.
        // maximum_active 记录观察到的最大重叠数量。
        let maximum_active = Arc::new(AtomicUsize::new(0));

        // first_thread runs one future through the shared runtime.
        // first_thread 通过共享运行时执行一个 future。
        let first_thread = {
            // thread_runtime is the first caller's shared runtime handle.
            // thread_runtime 是第一个调用方的共享运行时句柄。
            let thread_runtime = runtime.clone();
            // thread_barrier synchronizes the first caller with the other participants.
            // thread_barrier 将第一个调用方与其他参与方同步。
            let thread_barrier = start_barrier.clone();
            // thread_active shares the live overlap counter.
            // thread_active 共享活动重叠计数器。
            let thread_active = active.clone();
            // thread_entered shares the monotonic rendezvous counter.
            // thread_entered 共享单调汇合计数器。
            let thread_entered = entered.clone();
            // thread_maximum shares the maximum overlap counter.
            // thread_maximum 共享最大重叠计数器。
            let thread_maximum = maximum_active.clone();
            thread::spawn(move || {
                thread_barrier.wait();
                run_future_on_bridge_runtime(&thread_runtime, async move {
                    // active_now includes the current future after entering the runtime.
                    // active_now 包含当前 future 进入运行时后的活动数量。
                    let active_now = thread_active.fetch_add(1, Ordering::SeqCst) + 1;
                    thread_maximum.fetch_max(active_now, Ordering::SeqCst);
                    thread_entered.fetch_add(1, Ordering::SeqCst);
                    while thread_entered.load(Ordering::SeqCst) < 2 {
                        tokio::task::yield_now().await;
                    }
                    thread_active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, BoxError>(())
                })
            })
        };
        // second_thread runs another future through the same shared runtime.
        // second_thread 通过同一共享运行时执行另一个 future。
        let second_thread = {
            // thread_runtime is the second caller's shared runtime handle.
            // thread_runtime 是第二个调用方的共享运行时句柄。
            let thread_runtime = runtime.clone();
            // thread_barrier synchronizes the second caller with the other participants.
            // thread_barrier 将第二个调用方与其他参与方同步。
            let thread_barrier = start_barrier.clone();
            // thread_active shares the live overlap counter.
            // thread_active 共享活动重叠计数器。
            let thread_active = active.clone();
            // thread_entered shares the monotonic rendezvous counter.
            // thread_entered 共享单调汇合计数器。
            let thread_entered = entered.clone();
            // thread_maximum shares the maximum overlap counter.
            // thread_maximum 共享最大重叠计数器。
            let thread_maximum = maximum_active.clone();
            thread::spawn(move || {
                thread_barrier.wait();
                run_future_on_bridge_runtime(&thread_runtime, async move {
                    // active_now includes the current future after entering the runtime.
                    // active_now 包含当前 future 进入运行时后的活动数量。
                    let active_now = thread_active.fetch_add(1, Ordering::SeqCst) + 1;
                    thread_maximum.fetch_max(active_now, Ordering::SeqCst);
                    thread_entered.fetch_add(1, Ordering::SeqCst);
                    while thread_entered.load(Ordering::SeqCst) < 2 {
                        tokio::task::yield_now().await;
                    }
                    thread_active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, BoxError>(())
                })
            })
        };

        start_barrier.wait();
        first_thread
            .join()
            .expect("first controller caller must not panic")
            .expect("first controller future must succeed");
        second_thread
            .join()
            .expect("second controller caller must not panic")
            .expect("second controller future must succeed");
        assert!(maximum_active.load(Ordering::SeqCst) > 1);
    }

    /// Verify one slow controller future does not block an independent fast future.
    /// 验证一个缓慢控制器 future 不会阻塞另一个独立的快速 future。
    #[test]
    fn bridge_runtime_slow_future_does_not_block_fast_future() {
        // runtime is the same bridge-owned scheduler used by both synchronous callers.
        // runtime 是两个同步调用方共用的同一个桥接调度器。
        let runtime = Arc::new(build_bridge_runtime());
        // release_slow controls when the deliberately slow future may complete.
        // release_slow 控制刻意缓慢的 future 何时可以完成。
        let release_slow = Arc::new(AtomicBool::new(false));
        // entered channel confirms the slow future is already running before the fast call starts.
        // entered 通道确认慢 future 已在运行后才启动快速调用。
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);

        // slow_thread owns the first synchronous caller and remains pending until released.
        // slow_thread 持有第一个同步调用方，并在释放前保持等待。
        let slow_thread = {
            // thread_runtime is the slow caller's shared runtime handle.
            // thread_runtime 是慢调用方的共享运行时句柄。
            let thread_runtime = runtime.clone();
            // thread_release is the slow future's completion gate.
            // thread_release 是慢 future 的完成门闩。
            let thread_release = release_slow.clone();
            thread::spawn(move || {
                run_future_on_bridge_runtime(&thread_runtime, async move {
                    entered_sender
                        .send(())
                        .map_err(|error| -> BoxError { error.to_string().into() })?;
                    while !thread_release.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                    Ok::<_, BoxError>("slow")
                })
            })
        };

        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("slow controller future must enter the runtime");
        // fast_result channel reports whether the independent fast call completed promptly.
        // fast_result 通道报告独立快速调用是否及时完成。
        let (fast_sender, fast_receiver) = mpsc::sync_channel(1);
        // fast_thread runs the independent call while the slow future remains pending.
        // fast_thread 在慢 future 仍处于等待时运行独立调用。
        let fast_thread = {
            // thread_runtime is the fast caller's handle to the same runtime.
            // thread_runtime 是快速调用方指向同一运行时的句柄。
            let thread_runtime = runtime.clone();
            thread::spawn(move || {
                // result is the fast controller future outcome sent back to the test thread.
                // result 是发送回测试线程的快速控制器 future 结果。
                let result = run_future_on_bridge_runtime(&thread_runtime, async {
                    Ok::<_, BoxError>("fast")
                });
                let _ = fast_sender.send(result);
            })
        };
        // prompt_result distinguishes concurrent completion from request-wide serialization.
        // prompt_result 用于区分并发完成与请求级串行化。
        let prompt_result = fast_receiver.recv_timeout(Duration::from_secs(2));
        release_slow.store(true, Ordering::SeqCst);
        // slow_result is collected after release so every test thread terminates deterministically.
        // slow_result 在释放后收集，确保每个测试线程都确定结束。
        let slow_result = slow_thread
            .join()
            .expect("slow controller caller must not panic")
            .expect("slow controller future must succeed");
        fast_thread
            .join()
            .expect("fast controller caller must not panic");
        // fast_result is the prompt independent result captured before releasing the slow future.
        // fast_result 是释放慢 future 前捕获的及时独立结果。
        let fast_result = prompt_result
            .expect("fast controller future must finish while slow future is pending")
            .expect("fast controller future must succeed");

        assert_eq!(slow_result, "slow");
        assert_eq!(fast_result, "fast");
    }

    /// Verify a real controller can auto-spawn, attach, serve concurrent RPCs, and close.
    /// 验证真实控制器能够自动唤起、附加、处理并发 RPC 并关闭。
    #[test]
    #[ignore = "requires LUASKILLS_TEST_VLDB_CONTROLLER pointing to a real controller executable"]
    fn real_controller_grpc_connect_attach_concurrent_and_close() {
        // ExecutablePath is explicitly supplied by the acceptance environment after digest-verified download.
        // ExecutablePath 由验收环境在摘要校验下载后显式提供。
        let executable_path = std::env::var_os("LUASKILLS_TEST_VLDB_CONTROLLER")
            .map(PathBuf::from)
            .expect("LUASKILLS_TEST_VLDB_CONTROLLER must point to the real controller executable");
        assert!(
            executable_path.is_file(),
            "real controller executable does not exist: {}",
            executable_path.display()
        );
        // PortReservation obtains an unused loopback address without assuming the default controller is absent.
        // PortReservation 获取一个未使用的回环地址，不假设默认控制器不存在。
        let port_reservation = TcpListener::bind("127.0.0.1:0")
            .expect("reserve isolated real controller validation port");
        // ControllerAddress is the exact loopback address assigned by the operating system.
        // ControllerAddress 是操作系统分配的精确回环地址。
        let controller_address = port_reservation
            .local_addr()
            .expect("read reserved controller address");
        drop(port_reservation);
        // TempRoot isolates the attached real controller space from repository and user data.
        // TempRoot 将附加的真实控制器空间与仓库及用户数据隔离。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_real_controller_validation_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale generated fixture cleanup is limited to this process-namespaced temp root.
            // 陈旧生成夹具清理仅限当前进程命名的临时根目录。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("create real controller validation root");
        // HostOptions selects one isolated endpoint and the digest-verified executable.
        // HostOptions 选择一个隔离端点与经过摘要校验的可执行文件。
        let mut host_options = LuaRuntimeHostOptions::default();
        host_options.space_controller.endpoint = Some(format!("http://{controller_address}"));
        host_options.space_controller.auto_spawn = true;
        host_options.space_controller.executable_path = Some(executable_path);
        host_options.space_controller.minimum_uptime_secs = 1;
        host_options.space_controller.idle_timeout_secs = 1;
        host_options.space_controller.startup_timeout_secs = 30;
        // Bridge performs the production connect and client-session registration sequence.
        // Bridge 执行生产连接与客户端会话注册流程。
        let bridge = LuaRuntimeSpaceControllerBridge::new(&host_options, "real-acceptance")
            .expect("connect to auto-spawned real controller");
        // Binding identifies one real but temporary controller space.
        // Binding 标识一个真实但临时的控制器空间。
        let binding = RuntimeDatabaseBindingContext::new(RuntimeDatabaseBindingContextSpec {
            space_label: "ROOT".to_string(),
            skill_id: "real-controller-acceptance".to_string(),
            root_name: "ROOT".to_string(),
            space_root: temp_root.to_string_lossy().into_owned(),
            skill_dir: temp_root.join("skill").to_string_lossy().into_owned(),
            skill_dir_name: "skill".to_string(),
            database_kind: RuntimeDatabaseKind::Sqlite,
            default_database_path: temp_root.join("default.db").to_string_lossy().into_owned(),
        });
        bridge
            .attach_binding(&binding)
            .expect("attach temporary real controller space");

        for concurrency in [1_usize, 8, 32] {
            // Samples and elapsed are produced by real get_status gRPC round trips.
            // Samples 与 elapsed 来自真实 get_status gRPC 往返。
            let (samples, elapsed) = measure_real_controller_status_requests(&bridge, concurrency);
            // ExpectedSamples proves every synchronized caller completed its fixed request budget.
            // ExpectedSamples 证明每个同步调用方均完成固定请求预算。
            let expected_samples = concurrency * 8;
            assert_eq!(samples.len(), expected_samples);
            // Throughput is the completed real RPC count divided by batch wall time.
            // Throughput 是完成的真实 RPC 数除以批次墙钟时长。
            let throughput = samples.len() as f64 / elapsed.as_secs_f64();
            // P95 and P99 report nearest-rank per-request microsecond latency.
            // P95 与 P99 报告最邻近秩的逐请求微秒延迟。
            let p95 = nearest_rank_percentile_micros(&samples, 95);
            let p99 = nearest_rank_percentile_micros(&samples, 99);
            println!(
                "CONTROLLER_PERF concurrency={concurrency} requests={} elapsed_ms={} throughput_rps={throughput:.2} p95_us={p95} p99_us={p99}",
                samples.len(),
                elapsed.as_millis()
            );
        }

        drop(bridge);
        // Generated validation space cleanup is best effort after the bridge closes its client.
        // 桥接关闭客户端后，按最佳努力原则清理生成的验收空间。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify controller binding ids preserve the stable host tag while adding one client-scoped suffix.
    /// 验证控制器绑定标识会保留稳定宿主标签，并额外附加客户端作用域后缀。
    #[test]
    fn controller_binding_id_preserves_tag_and_adds_scope_suffix() {
        assert_eq!(
            build_controller_binding_id("ROOT-vulcan-ai-memory", "client-session-123"),
            "ROOT-vulcan-ai-memory@client-session-123"
        );
    }
}
