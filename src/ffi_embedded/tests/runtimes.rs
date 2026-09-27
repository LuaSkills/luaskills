use super::*;
use crate::runtime::embedded::EmbeddedRuntimeConfig;
use crate::{LuaEngineOptions, LuaRuntimeHostOptions, LuaVmPoolConfig};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Provide explicit small formal runtime limits independent of transport response budgets.
/// 提供独立于传输响应预算的显式小型正式运行时限制。
pub(in crate::ffi_embedded) fn runtime_config() -> EmbeddedRuntimeConfig {
    EmbeddedRuntimeConfig {
        max_registered_plugins: 4,
        max_registered_pools: 4,
        max_sessions: 4,
        max_registered_capabilities: 4,
        max_resident_vms: 2,
        max_running_calls: 1,
        max_queued_calls: 4,
        max_queued_bytes: 4096,
        max_operations: 8,
        max_effect_records_per_operation: 8,
        max_effect_bytes_per_operation: 4096,
        max_host_requests: 4,
        max_host_request_bytes: 4096,
        max_value_bytes: 1024,
    }
}

/// Build the existing engine option contract without external distributions or business packages.
/// 构造现有引擎选项契约，不依赖外部分发包或业务包。
pub(in crate::ffi_embedded) fn engine_options() -> LuaEngineOptions {
    LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 1,
        },
        LuaRuntimeHostOptions::default(),
    )
}

/// Create a transport whose explicit input allowance can carry complete serialized engine options.
/// 创建输入额度足以携带完整序列化引擎选项的显式传输。
fn transport() -> u64 {
    let mut limits = config();
    limits.max_request_bytes = 16384;
    create(limits)
}

/// Execute `command` through the actual C entrypoint and copy/release its result before returning JSON.
/// 通过实际 C 入口执行 `command`，在返回 JSON 前复制并释放结果。
fn command(id: u64, command: Value) -> Value {
    let bytes = serde_json::to_vec(&json!({"protocol_version":1,"command":command})).unwrap();
    let (status, result) = request(id, &bytes);
    assert_eq!(status, 0, "native transport rejected command: {command}");
    let response =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(result.ptr, result.len) })
            .unwrap();
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    response
}

/// Reserve and return a known runtime identity; this command cannot create an engine or native worker.
/// 预留并返回已知运行时身份；此命令不能创建引擎或原生工作线程。
fn reserve(id: u64) -> String {
    let response = command(id, json!({"type":"runtime_reserve"}));
    assert_eq!(response["status"], "ok");
    response["result"]["runtime_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Attempt real one-shot initialization for `runtime_id` and require the attempt receipt.
/// 为 `runtime_id` 尝试实际单次初始化，并要求获得尝试回执。
fn initialize(id: u64, runtime_id: &str, limits: EmbeddedRuntimeConfig) {
    let response = command(
        id,
        json!({
            "type":"runtime_initialize", "runtime_id":runtime_id,
            "engine_options":engine_options(), "runtime_config":limits,
        }),
    );
    assert_eq!(response["status"], "ok", "{response}");
    assert_eq!(response["result"]["runtime_id"], runtime_id);
}

/// Read current status for the exact `runtime_id` through the public JSON protocol.
/// 通过公开 JSON 协议读取精确 `runtime_id` 的当前状态。
fn status(id: u64, runtime_id: &str) -> Value {
    let response = command(id, json!({"type":"runtime_status","runtime_id":runtime_id}));
    assert_eq!(response["status"], "ok", "{response}");
    response["result"].clone()
}

/// Wait only for actual native closure evidence, with a finite diagnostic test deadline.
/// 仅等待实际原生关闭证据，并使用有限诊断测试截止时间。
fn wait_closed(id: u64, runtime_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while status(id, runtime_id)["closed"] != true {
        assert!(
            Instant::now() < deadline,
            "runtime workers did not exit: {runtime_id}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Require successful `action` for a known runtime, never converting transport success into business success.
/// 要求已知运行时的 `action` 成功，绝不将传输成功转为业务成功。
fn action(id: u64, runtime_id: &str, action: &str) {
    let response = command(id, json!({"type":action,"runtime_id":runtime_id}));
    assert_eq!(response["status"], "ok", "{response}");
}

/// Exercise actual native creation, separate core identity, query, worker drain and final release.
/// 验证实际原生创建、独立核心身份、查询、工作线程排空及最终释放。
#[test]
fn ffi_embedded_runtime_lifecycle_retains_known_identity_until_workers_exit() {
    let id = transport();
    let runtime_id = reserve(id);
    let reserved = status(id, &runtime_id);
    assert_eq!(reserved["initialization"], "reserved");
    assert!(reserved["core_runtime_id"].is_null());
    initialize(id, &runtime_id, runtime_config());
    let ready = status(id, &runtime_id);
    assert_eq!(ready["initialization"], "ready");
    assert!(
        ready["core_runtime_id"]
            .as_str()
            .unwrap()
            .starts_with("embedded:")
    );
    assert_ne!(ready["core_runtime_id"], runtime_id);
    assert_eq!(ready["usage"]["active_operations"], 0);
    assert_eq!(ready["closed"], false);
    let denied = command(id, json!({"type":"runtime_free","runtime_id":runtime_id}));
    assert_eq!(denied["error"]["code"], "busy");
    action(id, &runtime_id, "runtime_close");
    wait_closed(id, &runtime_id);
    action(id, &runtime_id, "runtime_free");
    let missing = command(id, json!({"type":"runtime_status","runtime_id":runtime_id}));
    assert_eq!(missing["error"]["code"], "not_found");
    finish(id);
}

/// Bounded reservations include uninitialized slots and cannot be addressed through another transport.
/// 有界预留包含未初始化槽，且不能通过其他传输访问。
#[test]
fn ffi_embedded_runtime_registration_limits_isolation_and_stale_ids() {
    let mut limits = config();
    limits.max_runtimes = 1;
    let id = create(limits);
    let other = create(limits);
    let first = reserve(id);
    assert_eq!(
        command(id, json!({"type":"runtime_reserve"}))["error"]["code"],
        "capacity_exceeded"
    );
    assert_eq!(
        command(other, json!({"type":"runtime_close","runtime_id":first}))["error"]["code"],
        "not_found"
    );
    action(id, &first, "runtime_close");
    action(id, &first, "runtime_free");
    let next = reserve(id);
    assert_ne!(first, next);
    assert_eq!(
        command(id, json!({"type":"runtime_close","runtime_id":first}))["error"]["code"],
        "not_found"
    );
    action(id, &next, "runtime_close");
    action(id, &next, "runtime_free");
    finish(id);
    finish(other);
}

/// Failed initialization is retained under the existing identity and cannot be retried silently.
/// 失败初始化保留在已有身份下，不能静默重试。
#[test]
fn ffi_embedded_runtime_initialization_failure_is_queryable_and_not_retried() {
    let id = transport();
    let runtime_id = reserve(id);
    let mut limits = runtime_config();
    limits.max_running_calls = 0;
    initialize(id, &runtime_id, limits);
    let failed = status(id, &runtime_id);
    assert_eq!(failed["initialization"], "failed");
    assert_eq!(failed["error"]["code"], "invalid_argument");
    assert!(failed["core_runtime_id"].is_null());
    let retried = command(
        id,
        json!({
            "type":"runtime_initialize", "runtime_id":runtime_id,
            "engine_options":engine_options(), "runtime_config":runtime_config(),
        }),
    );
    assert_eq!(retried["error"]["code"], "busy");
    action(id, &runtime_id, "runtime_close");
    assert_eq!(status(id, &runtime_id)["closed"], true);
    action(id, &runtime_id, "runtime_free");
    finish(id);
}

/// An undeliverable reserve receipt cannot publish an unreachable runtime registration.
/// 无法交付的预留回执不能发布不可到达的运行时注册。
#[test]
fn ffi_embedded_runtime_response_capacity_is_checked_before_registration() {
    let mut limits = config();
    limits.max_response_bytes = 1;
    let id = create(limits);
    let (status, result) = request(
        id,
        br#"{"protocol_version":1,"command":{"type":"runtime_reserve"}}"#,
    );
    assert_eq!(status, EmbeddedFfiStatus::CapacityExceeded as i32);
    assert!(result.ptr.is_null());
    // A leaked registration would make actual transport release return Busy here.
    // 若泄漏注册，此处实际传输释放会返回 Busy。
    finish(id);
}

/// Closing a transport reaches every real runtime while preserving explicit per-runtime release authority.
/// 关闭传输到达每个实际运行时，同时保留显式逐运行时释放权威。
#[test]
fn ffi_embedded_transport_close_drains_all_runtimes_before_release() {
    let id = transport();
    let first = reserve(id);
    let second = reserve(id);
    initialize(id, &first, runtime_config());
    initialize(id, &second, runtime_config());
    assert_ne!(
        status(id, &first)["core_runtime_id"],
        status(id, &second)["core_runtime_id"]
    );
    assert_eq!(luaskills_ffi_embedded_transport_close_v1(id), 0);
    assert_eq!(
        command(id, json!({"type":"runtime_reserve"}))["error"]["code"],
        "closed"
    );
    assert_eq!(
        luaskills_ffi_embedded_transport_free_v1(id),
        EmbeddedFfiStatus::Busy as i32
    );
    for runtime_id in [&first, &second] {
        wait_closed(id, runtime_id);
        action(id, runtime_id, "runtime_free");
    }
    finish(id);
}

/// New runtime creation and queries do not depend on the legacy engine registry's execution gate.
/// 新运行时创建及查询不依赖旧引擎注册表的执行门。
#[test]
fn ffi_embedded_runtime_does_not_acquire_legacy_engine_registry() {
    let _legacy_registry = crate::ffi::lock_ffi_engine_registry();
    let id = transport();
    let runtime_id = reserve(id);
    initialize(id, &runtime_id, runtime_config());
    assert_eq!(status(id, &runtime_id)["initialization"], "ready");
    action(id, &runtime_id, "runtime_close");
    wait_closed(id, &runtime_id);
    action(id, &runtime_id, "runtime_free");
    finish(id);
}
