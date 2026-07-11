use crate::host::database::RuntimeDatabaseBindingContext;
use crate::host::options::{LuaRuntimeHostOptions, LuaRuntimeSpaceControllerProcessMode};
use crate::runtime::path::render_host_visible_path;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
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
    client: ControllerClient,
    runtime: Mutex<Runtime>,
    binding_scope_id: String,
}

/// Acquire one controller bridge runtime and return its guard, recovering after lock poisoning.
/// 获取并返回单个控制器桥接运行时保护对象；如果锁已 poison，则恢复继续使用。
fn lock_controller_runtime(runtime: &Mutex<Runtime>) -> MutexGuard<'_, Runtime> {
    runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            runtime: Mutex::new(runtime),
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
        let runtime = lock_controller_runtime(&self.runtime);
        run_controller_operation_with_client(&runtime, &self.client, operation)
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
        build_controller_binding_id, lock_controller_runtime, run_future_on_bridge_runtime,
        system_time_to_controller_start_unix_millis,
    };
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::Mutex;
    use std::time::{Duration, UNIX_EPOCH};
    use tokio::runtime::{Builder, Runtime};
    use vldb_controller_client::BoxError;

    /// Build one controller bridge runtime used by bridge-execution tests.
    /// 构建一个供桥接执行测试使用的控制器运行时。
    fn build_bridge_runtime() -> Runtime {
        Runtime::new().expect("bridge runtime should build")
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

    /// Verify the controller bridge runtime remains usable after its mutex is poisoned.
    /// 验证控制器桥接运行时互斥锁 poison 后仍可继续使用。
    #[test]
    fn controller_runtime_lock_recovers_after_poisoned_lock() {
        // Runtime mutex used to mimic the bridge-owned controller runtime slot.
        // 用于模拟桥接持有的控制器运行时槽位的运行时互斥锁。
        let runtime = Mutex::new(build_bridge_runtime());
        // Captured panic result from a holder that poisons only the runtime mutex.
        // 单个运行时互斥锁持有者制造 poison 后被捕获的 panic 结果。
        let poison_result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Guard used only to poison the controller runtime lock.
            // 仅用于制造控制器运行时锁 poison 的保护对象。
            let _guard = runtime.lock().expect("initial controller runtime lock");
            panic!("poison controller runtime for recovery test");
        }));

        assert!(poison_result.is_err());

        // Recovered runtime guard used to execute one bridge-owned future.
        // 用于执行单个桥接 future 的已恢复运行时保护对象。
        let recovered_runtime = lock_controller_runtime(&runtime);
        // Future result proving the recovered runtime still executes work.
        // 用于证明已恢复运行时仍可执行任务的 future 结果。
        let result =
            run_future_on_bridge_runtime(&recovered_runtime, async { Ok::<_, BoxError>(13usize) })
                .expect("recovered controller runtime should execute future");
        assert_eq!(result, 13);
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
