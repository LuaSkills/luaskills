use super::pools::{pool_config, pool_policy};
use super::*;
use crate::ffi_embedded::*;
use crate::ffi_standard::FfiBorrowedBuffer;
use crate::runtime::embedded::capabilities::{CapabilityEffects, CapabilityExecution, HostRequest};
use crate::runtime::embedded::{EmbeddedPluginConfig, InstanceReuse, OperationSnapshot};

mod capacities;
mod initialization;
mod persistence;
mod prewarm;

/// The public C transport executes a real module from the exact host-authorized external generation.
/// 公开 C 传输从宿主精确授权的外部代次执行真实模块。
#[test]
fn ffi_embedded_pipeline_authorizes_external_package_generation() {
    // Preserve the same SDK-style client while moving only the fixture package outside System.
    // 保持同一个 SDK 风格客户端，仅将夹具包移出 System。
    let layout = super::paths::external_layout("ffi external package");
    // Every command passes through the public versioned C ABI.
    // 每条命令都经过公开版本化 C ABI。
    let client = Client::new(&layout);
    // Reuse proves a real resident module is retained in the formally registered pool.
    // 复用证明正式注册池中保留了真实常驻模块。
    let pool = client.pool(&layout,
        "local n=0; return {call=function() n=n+1; return {root=vulcan.runtime.system_plugin.root,count=n} end}",
        InstanceReuse::Reusable);
    for count in [1, 2] {
        // Wait for the actual operation, not merely a successful submission response.
        // 等待实际操作，而非仅等待成功提交响应。
        let operation = client.submit(&pool, Value::Null);
        // The formal terminal state retains exact physical package output and state.
        // 正式终态保留精确物理包输出及状态。
        let done = client.terminal(&operation);
        assert!(done.error.is_none(), "{done:?}");
        assert_eq!(
            done.value,
            Some(json!({"root":render_host_visible_path(&layout.package_root),"count":count}))
        );
        client.ok(json!({"type":"operation_forget","operation_id":operation}));
    }
    client.close();
}

/// A test SDK client that calls only the new public C entrypoints for all runtime work.
/// 一个测试 SDK 客户端，全部运行时工作仅调用新的公开 C 入口。
struct Client {
    /// Native transport identity, preserved as all 64 bits.
    /// 原生传输身份，保留全部 64 位。
    transport_id: u64,
    /// Exact pre-reserved runtime control identity.
    /// 精确预留运行时控制身份。
    runtime_id: String,
}

/// Send a typed root `command` through the C ABI; copy and free any successful native result exactly once.
/// 通过 C ABI 发送类型化根 `command`；精确一次复制并释放任何成功原生结果。
fn root_request(transport_id: u64, command: Value) -> Result<Value, i32> {
    let bytes = serde_json::to_vec(
        &json!({"protocol_version":EMBEDDED_FFI_PROTOCOL_VERSION,"command":command}),
    )
    .unwrap();
    let mut result = FfiEmbeddedResultV1::default();
    let status = unsafe {
        luaskills_ffi_embedded_request_v1(
            transport_id,
            FfiBorrowedBuffer {
                ptr: bytes.as_ptr(),
                len: bytes.len(),
            },
            &mut result,
        )
    };
    if status != 0 {
        assert!(result.ptr.is_null());
        return Err(status);
    }
    let json =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(result.ptr, result.len) });
    assert_eq!(
        luaskills_ffi_embedded_result_free_v1(transport_id, result),
        0
    );
    let json = json.unwrap();
    #[cfg(feature = "contract-generation")]
    crate::ffi_embedded::contract::tests::assert_exchange(
        &serde_json::from_slice(&bytes).unwrap(),
        &json,
    );
    Ok(json)
}

impl Client {
    /// Construct a real client from explicit response and optional durable storage declarations.
    /// 从显式响应及可选持久存储声明构造真实客户端。
    fn with_storage(
        layout: &SystemRuntimeTestLayout,
        response_limit: u64,
        persistence: Value,
    ) -> Self {
        let config = FfiEmbeddedTransportConfigV1 {
            struct_size: std::mem::size_of::<FfiEmbeddedTransportConfigV1>() as u32,
            protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
            max_runtimes: 1,
            max_result_buffers: 4,
            max_result_bytes: response_limit * 4,
            max_response_bytes: response_limit,
            max_request_bytes: 262144,
        };
        let mut transport_id = 0;
        assert_eq!(
            unsafe { luaskills_ffi_embedded_transport_new_v1(&config, &mut transport_id) },
            0
        );
        let reserved = root_request(transport_id, json!({"type":"runtime_reserve"})).unwrap();
        assert_eq!(reserved["status"], "ok");
        let runtime_id = reserved["result"]["runtime_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let client = Self {
            transport_id,
            runtime_id,
        };
        let limits = pool_config();
        let initialized = client.root(json!({
            "type":"runtime_initialize", "runtime_id":client.runtime_id,
            "engine_options":LuaEngineOptions::new(LuaVmPoolConfig {min_size:1,max_size:1,idle_ttl_secs:1}, layout.host_options()),
            "runtime_config":limits, "persistence":persistence,
        }));
        assert_eq!(initialized["status"], "ok", "{initialized}");
        let status = client.root(json!({"type":"runtime_status","runtime_id":client.runtime_id}));
        assert_eq!(status["result"]["initialization"], "ready", "{status}");
        client.ok(json!({"type":"plugin_register", "plugin_id":layout.package_id,
            "config":EmbeddedPluginConfig {
                max_registered_pools:limits.max_registered_pools,max_sessions:limits.max_sessions,
                max_resident_vms:limits.max_resident_vms,max_running_calls:limits.max_running_calls,
                max_queued_calls:limits.max_queued_calls,max_queued_bytes:limits.max_queued_bytes,max_operations:limits.max_operations,
            }
        }));
        client
    }

    /// Construct and initialize a real native runtime under `layout`, then register its explicit plugin policy.
    /// 在 `layout` 下构造并初始化实际原生运行时，随后注册其显式插件策略。
    fn new(layout: &SystemRuntimeTestLayout) -> Self {
        Self::with_response_limit(layout, 32768)
    }

    /// Build a real native client with an explicit per-response byte limit for admission boundary tests.
    /// 使用显式逐响应字节上限构造实际原生客户端，用于入场边界测试。
    fn with_response_limit(layout: &SystemRuntimeTestLayout, response_limit: u64) -> Self {
        Self::with_storage(layout, response_limit, Value::Null)
    }

    /// Execute root `command` and require native transport delivery, leaving business status explicit.
    /// 执行根 `command` 并要求原生传输交付，保持业务状态显式。
    fn root(&self, command: Value) -> Value {
        root_request(self.transport_id, command).unwrap()
    }

    /// Execute `operation` within this exact runtime and return the complete business envelope.
    /// 在此精确运行时内执行 `operation`，返回完整业务信封。
    fn command(&self, operation: Value) -> Value {
        self.root(json!({"type":"runtime","runtime_id":self.runtime_id,"operation":operation}))
    }

    /// Require explicit business success for `operation`, then return its actual result value.
    /// 要求 `operation` 显式业务成功，随后返回实际结果值。
    fn ok(&self, operation: Value) -> Value {
        let envelope = self.command(operation);
        assert_eq!(envelope["status"], "ok", "{envelope}");
        envelope["result"].clone()
    }

    /// Register immutable module `source` in `layout` with exact reuse and live test permission authority.
    /// 在 `layout` 中以精确复用及实时测试权限权威注册不可变模块 `source`。
    fn pool(&self, layout: &SystemRuntimeTestLayout, source: &str, reuse: InstanceReuse) -> String {
        let result = self.ok(json!({"type":"pool_register", "definition":definition(layout, source),
            "policy":pool_policy(reuse), "permissions":["test.host"], "execution_revision":"ffi-v1"}));
        result["pool_id"].as_str().unwrap().to_owned()
    }

    /// Submit one structured ordinary call and return its immutable core operation identity.
    /// 提交一个结构化普通调用，返回其不可变核心操作身份。
    fn submit(&self, pool_id: &str, arguments: Value) -> String {
        let result = self.ok(json!({"type":"call_submit","timeout_ms":10000,
            "call":{"pool_id":pool_id,"export":"call","arguments":arguments,"context":LuaInvocationContext::default()}}));
        result["operation_id"].as_str().unwrap().to_owned()
    }

    /// Read live typed operation evidence using only the public wire contract.
    /// 仅使用公开线协议读取实时类型化操作证据。
    fn snapshot(&self, operation_id: &str) -> OperationSnapshot {
        serde_json::from_value(
            self.ok(json!({"type":"operation_status","operation_id":operation_id})),
        )
        .unwrap()
    }

    /// Wait for actual operation terminal state within a finite diagnostic deadline.
    /// 在有限诊断截止时间内等待实际操作终态。
    fn terminal(&self, operation_id: &str) -> OperationSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = self.snapshot(operation_id);
            if snapshot.phase.is_terminal() {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "operation did not terminate: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Register one real queued mutating capability and return its exact identity.
    /// 注册一个实际队列变更能力，并返回其精确身份。
    fn capability(&self) -> String {
        let mut descriptor =
            super::capabilities::descriptor("test.queue", CapabilityExecution::Queued);
        descriptor.effects = CapabilityEffects::Mutating;
        let result = self.ok(json!({"type":"capabilities_register","descriptors":[descriptor]}));
        let ids = result["registration_ids"].as_array().unwrap();
        assert_eq!(ids.len(), 1);
        ids.first().unwrap().as_str().unwrap().to_owned()
    }

    /// Pump until one actual host request arrives, preserving its authenticated caller and completion identity.
    /// 驱动事件泵直到一个实际宿主请求到达，保留其已认证调用方与完成身份。
    fn host_request(&self) -> HostRequest {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut requests: Vec<HostRequest> =
                serde_json::from_value(self.ok(json!({"type":"host_requests_take","limit":1})))
                    .unwrap();
            if let Some(request) = requests.pop() {
                return request;
            }
            assert!(
                Instant::now() < deadline,
                "host request did not reach the SDK pump"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Close core workers, require real join evidence, remove the runtime, and finally free the transport.
    /// 关闭核心工作线程，要求实际汇合证据，移除运行时，最后释放传输。
    fn close(self) {
        let response = self.root(json!({"type":"runtime_close","runtime_id":self.runtime_id}));
        assert_eq!(response["status"], "ok");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = self.root(json!({"type":"runtime_status","runtime_id":self.runtime_id}));
            assert_eq!(status["status"], "ok", "{status}");
            if status["result"]["closed"] == true {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "runtime ownership did not drain: {status}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let response = self.root(json!({"type":"runtime_free","runtime_id":self.runtime_id}));
        assert_eq!(response["status"], "ok", "{response}");
        assert_eq!(
            luaskills_ffi_embedded_transport_close_v1(self.transport_id),
            0
        );
        assert_eq!(
            luaskills_ffi_embedded_transport_free_v1(self.transport_id),
            0
        );
    }
}

/// Real Lua state, structured null/Unicode, operation retention and explicit plugin cleanup traverse only FFI.
/// 实际 Lua 状态、结构化空值／Unicode、操作保留及显式插件清理仅通过 FFI 贯通。
#[test]
fn ffi_embedded_pipeline_reuses_lua_state_and_releases_exact_metadata() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded ordinary pipeline");
    let client = Client::new(&layout);
    let pool_id = client.pool(
        &layout,
        "local n=0; return {call=function(a) n=n+1; return {count=n,arg=a} end}",
        InstanceReuse::Reusable,
    );
    let arguments = json!({"text":"中文\0🦀","null":null,"array":[],"object":{}});
    for count in 1..=2 {
        let operation_id = client.submit(&pool_id, arguments.clone());
        let done = client.terminal(&operation_id);
        assert!(done.error.is_none(), "{done:?}");
        assert_eq!(done.value.unwrap(), json!({"count":count,"arg":arguments}));
        client.ok(json!({"type":"operation_forget","operation_id":operation_id}));
    }
    client.ok(json!({"type":"plugin_close","plugin_id":layout.package_id}));
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.ok(json!({"type":"pool_status","pool_id":pool_id}))["resident"] != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    client.ok(json!({"type":"pool_forget","pool_id":pool_id}));
    client.ok(json!({"type":"plugin_forget","plugin_id":layout.package_id}));
    assert_eq!(
        client.command(json!({"type":"plugin_status","plugin_id":layout.package_id}))["error"]["code"],
        "not_found"
    );
    client.close();
}

/// A real SDK pump receives authenticated requests and preserves successful JSON null plus actual commit evidence.
/// 实际 SDK 事件泵收到已认证请求，并保留成功 JSON 空值与实际提交证据。
#[test]
fn ffi_embedded_pipeline_queued_callback_preserves_identity_null_and_effects() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded host pipeline");
    let client = Client::new(&layout);
    let registration = client.capability();
    let pool_id = client.pool(
        &layout,
        "return {call=function(a) return vulcan.capabilities.call('test.queue',a) end}",
        InstanceReuse::Reusable,
    );
    let operation_id = client.submit(&pool_id, json!({"plugin_id":"forged","text":"中文"}));
    let request = client.host_request();
    assert_eq!(request.registration_id, registration);
    assert_eq!(request.caller.operation_id, operation_id);
    assert_eq!(request.caller.plugin_id, layout.package_id);
    assert_eq!(request.arguments["plugin_id"], "forged");
    assert_eq!(
        client.snapshot(&operation_id).phase,
        crate::runtime::embedded::OperationPhase::WaitingForHost
    );
    client.ok(
        json!({"type":"host_request_complete","request_id":request.request_id,
        "outcome":{"ok":true,"value":null,"effects":"committed"}}),
    );
    let done = client.terminal(&operation_id);
    assert_eq!(
        done.value.unwrap(),
        json!({"ok":true,"value":null,"effects":"committed"})
    );
    let effect = done
        .host_effects
        .iter()
        .find(|effect| effect.request_id.as_deref() == Some(&request.request_id))
        .unwrap();
    assert_eq!(
        effect.effects,
        crate::runtime::embedded::EffectState::Committed
    );
    client.ok(json!({"type":"capability_unregister","registration_id":registration}));
    assert_eq!(
        client.ok(json!({"type":"capability_status","registration_id":registration}))["drained"],
        true
    );
    client.ok(json!({"type":"capability_forget","registration_id":registration}));
    client.close();
}

/// Runtime close rejects new work while allowing a dispatched SDK handler to report its late external commit.
/// 运行时关闭拒绝新工作，同时允许已分发 SDK 处理器报告迟到外部提交。
#[test]
fn ffi_embedded_pipeline_close_keeps_late_acknowledgement_and_commit_evidence() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded late commit");
    let client = Client::new(&layout);
    client.capability();
    let pool_id = client.pool(
        &layout,
        "return {call=function(a) return vulcan.capabilities.call('test.queue',a) end}",
        InstanceReuse::Reusable,
    );
    let operation_id = client.submit(&pool_id, Value::Null);
    let request = client.host_request();
    assert_eq!(
        client.root(json!({"type":"runtime_close","runtime_id":client.runtime_id}))["status"],
        "ok"
    );
    let rejected = client.command(json!({"type":"call_submit","timeout_ms":10000,
        "call":{"pool_id":pool_id,"export":"call","arguments":null,"context":LuaInvocationContext::default()}}));
    assert_eq!(rejected["error"]["code"], "closed");
    assert_eq!(
        client.root(json!({"type":"runtime_free","runtime_id":client.runtime_id}))["error"]["code"],
        "busy"
    );
    assert!(!client.snapshot(&operation_id).phase.is_terminal());
    client.ok(
        json!({"type":"host_request_complete","request_id":request.request_id,
        "outcome":{"ok":true,"value":null,"effects":"committed"}}),
    );
    let done = client.terminal(&operation_id);
    assert_eq!(done.error.unwrap().code, EmbeddedErrorCode::Cancelled);
    assert!(
        done.host_effects
            .iter()
            .any(|effect| effect.effects == crate::runtime::embedded::EffectState::Committed)
    );
    client.close();
}

/// FFI sessions retain their exact Lua instance across calls and release only after actual closure.
/// FFI 会话跨调用保留精确 Lua 实例，并仅在实际关闭后释放。
#[test]
fn ffi_embedded_pipeline_sessions_keep_state_and_explicit_close() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded session pipeline");
    let client = Client::new(&layout);
    let pool_id = client.pool(
        &layout,
        "local n=0; return {call=function() n=n+1; return n end}",
        InstanceReuse::Session,
    );
    let opening = client.ok(json!({"type":"session_open","pool_id":pool_id,"timeout_ms":10000}));
    let session_id = opening["session_id"].as_str().unwrap();
    assert!(
        client
            .terminal(opening["operation_id"].as_str().unwrap())
            .error
            .is_none()
    );
    for count in 1..=2 {
        let submitted = client.ok(
            json!({"type":"session_submit","session_id":session_id,"export":"call",
            "arguments":null,"context":LuaInvocationContext::default(),"timeout_ms":10000}),
        );
        let done = client.terminal(submitted["operation_id"].as_str().unwrap());
        assert_eq!(done.value, Some(json!(count)), "{done:?}");
    }
    client.ok(json!({"type":"session_close","session_id":session_id}));
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.ok(json!({"type":"session_status","session_id":session_id}))["phase"] != "closed" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    client.ok(json!({"type":"session_forget","session_id":session_id}));
    client.close();
}

/// Revocation updates the real pool binding; a reused Lua VM cannot dispatch a now-forbidden capability.
/// 撤权更新实际池绑定；复用 Lua VM 不能分发已禁止能力。
#[test]
fn ffi_embedded_pipeline_live_revocation_reaches_existing_pool_authority() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded permission revocation");
    let client = Client::new(&layout);
    client.capability();
    let pool_id = client.pool(
        &layout,
        "local n=0; return {call=function(a) n=n+1; return {count=n,outcome=vulcan.capabilities.call('test.queue',a)} end}",
        InstanceReuse::Reusable,
    );
    // Materialize and exercise the VM before revocation so the second call checks the retained binding.
    // 撤权前实际创建并执行 VM，确保第二次调用检查已保留的绑定。
    let first = client.submit(&pool_id, Value::Null);
    let request = client.host_request();
    client.ok(
        json!({"type":"host_request_complete","request_id":request.request_id,
        "outcome":{"ok":true,"value":null,"effects":"committed"}}),
    );
    let initial = client.terminal(&first);
    assert!(initial.error.is_none(), "{initial:?}");
    assert_eq!(
        initial.value.unwrap(),
        json!({"count":1,"outcome":{"ok":true,"value":null,"effects":"committed"}})
    );
    assert_eq!(
        client.ok(
            json!({"type":"pool_revoke_permission","pool_id":pool_id,"permission":"test.host"})
        ),
        true
    );
    assert_eq!(
        client.ok(
            json!({"type":"pool_revoke_permission","pool_id":pool_id,"permission":"test.host"})
        ),
        false
    );
    let operation_id = client.submit(&pool_id, Value::Null);
    let done = client.terminal(&operation_id);
    let value = done.value.unwrap();
    assert_eq!(value["count"], 2);
    assert_eq!(value["outcome"]["error"]["code"], "permission_denied");
    assert_eq!(
        client.ok(json!({"type":"host_requests_take","limit":1})),
        json!([])
    );
    client.close();
}

/// Malformed completion cannot consume a dispatched handler; a later failure may still carry committed effects.
/// 无效完成不能消费已分发处理器；后续失败仍可携带已提交副作用。
#[test]
fn ffi_embedded_pipeline_rejected_completion_keeps_handler_ownership() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded invalid completion");
    let client = Client::new(&layout);
    let registration = client.capability();
    let pool_id = client.pool(
        &layout,
        "return {call=function(a) return vulcan.capabilities.call('test.queue',a) end}",
        InstanceReuse::Reusable,
    );
    let operation_id = client.submit(&pool_id, Value::Null);
    let request = client.host_request();
    for outcome in [
        json!({"ok":false,"value":null,"effects":"committed"}),
        json!({"ok":true,"error":{"code":"internal","message":"bad discriminator"},"effects":"unknown"}),
    ] {
        let rejected = client.command(json!({"type":"host_request_complete","request_id":request.request_id,"outcome":outcome}));
        assert_eq!(rejected["error"]["code"], "invalid_argument");
        assert_eq!(
            client.ok(json!({"type":"host_request_status","request_id":request.request_id}))["phase"],
            "dispatched"
        );
    }
    client.ok(json!({"type":"capability_unregister","registration_id":registration}));
    assert_eq!(
        client.ok(json!({"type":"capability_status","registration_id":registration}))["drained"],
        false
    );
    client.ok(json!({"type":"host_request_complete","request_id":request.request_id,
        "outcome":{"ok":false,"error":{"code":"internal","message":"failed after commit"},"effects":"committed"}}));
    let terminal = client.terminal(&operation_id);
    assert!(
        terminal
            .host_effects
            .iter()
            .any(|effect| effect.effects == crate::runtime::embedded::EffectState::Committed)
    );
    assert_eq!(
        client.ok(json!({"type":"capability_status","registration_id":registration}))["drained"],
        true
    );
    client.ok(json!({"type":"capability_forget","registration_id":registration}));
    client.close();
}

/// An oversized callback frame must remain undispatched so cancellation can drain without a lost SDK acknowledgement.
/// 超大回调响应帧必须保持未分发，使取消能排空且不会丢失 SDK 确认。
#[test]
fn ffi_embedded_pipeline_response_budget_preserves_undispatched_callback() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded callback response budget");
    let client = Client::with_response_limit(&layout, 1200);
    client.capability();
    let pool_id = client.pool(
        &layout,
        "return {call=function(a) return vulcan.capabilities.call('test.queue',a) end}",
        InstanceReuse::Reusable,
    );
    let operation_id = client.submit(&pool_id, json!("x".repeat(1000)));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = client.command(json!({"type":"host_requests_take","limit":1}));
        if response["status"] == "error" {
            assert_eq!(response["error"]["code"], "capacity_exceeded", "{response}");
            break;
        }
        assert_eq!(
            response["result"],
            json!([]),
            "an oversized request was dispatched: {response}"
        );
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        client.ok(json!({"type":"operation_cancel","operation_id":operation_id})),
        true
    );
    // No request identity was delivered, so successful close proves that no dispatched handler was orphaned.
    // 未交付任何请求身份，因此关闭成功证明没有遗留已分发处理器。
    client.close();
}

/// A JSON batch containing native execution is rejected atomically instead of silently creating queued handlers.
/// 包含原生执行的 JSON 批次被原子拒绝，而非静默创建队列处理器。
#[test]
fn ffi_embedded_pipeline_capability_batch_never_downgrades_native_execution() {
    let layout = SystemRuntimeTestLayout::new("ffi embedded native callback rejection");
    let client = Client::new(&layout);
    let queued = super::capabilities::descriptor("test.queued", CapabilityExecution::Queued);
    let native = super::capabilities::descriptor("test.native", CapabilityExecution::Native);
    let rejected =
        client.command(json!({"type":"capabilities_register","descriptors":[queued,native]}));
    assert_eq!(rejected["error"]["code"], "unsupported");
    assert_eq!(
        client.ok(json!({"type":"capabilities_list","permissions":["test.host"]})),
        json!([])
    );
    client.close();
}
