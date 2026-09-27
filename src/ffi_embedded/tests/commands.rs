use super::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Send a complete JSON request and release any returned allocation before exposing its parsed envelope.
/// 发送完整 JSON 请求，并在暴露解析后信封前释放所有返回分配。
fn exchange(id: u64, bytes: &[u8]) -> Result<Value, i32> {
    let (status, result) = request(id, bytes);
    if status != 0 {
        assert!(result.ptr.is_null());
        assert_eq!(result.len, 0);
        assert_eq!(result.allocation_id, 0);
        return Err(status);
    }
    let envelope =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(result.ptr, result.len) });
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    let envelope = envelope.unwrap();
    #[cfg(feature = "contract-generation")]
    contract::tests::assert_exchange(&serde_json::from_slice(bytes).unwrap(), &envelope);
    Ok(envelope)
}

/// Submit a structured root command while preserving native and business failures as separate results.
/// 提交结构化根命令，同时将原生失败与业务失败保留为不同结果。
fn command(id: u64, command: Value) -> Result<Value, i32> {
    exchange(
        id,
        &serde_json::to_vec(&json!({
            "protocol_version":EMBEDDED_FFI_PROTOCOL_VERSION,"command":command
        }))
        .unwrap(),
    )
}

/// Reject incomplete, conflicting and ambiguous completion shapes before looking up a runtime.
/// 在查找运行时之前拒绝不完整、冲突及歧义的完成形状。
#[test]
fn ffi_embedded_completion_shape_is_strict_and_null_is_explicit() {
    let id = create(config());
    for outcome in [
        r#"{"ok":true,"effects":"committed"}"#,
        r#"{"ok":true,"value":null}"#,
        r#"{"ok":"true","value":null,"effects":"committed"}"#,
        r#"{"ok":true,"value":null,"effects":"committed","extra":0}"#,
        r#"{"ok":true,"value":null,"error":{"code":"internal","message":"x"},"effects":"committed"}"#,
        r#"{"ok":true,"value":null,"value":1,"effects":"committed"}"#,
        r#"{"ok":false,"effects":"not_started"}"#,
    ] {
        let bytes = format!(
            "{{\"protocol_version\":{EMBEDDED_FFI_PROTOCOL_VERSION},\"command\":{{\"type\":\"runtime\",\"runtime_id\":\"absent\",\"operation\":{{\"type\":\"host_request_complete\",\"request_id\":\"absent\",\"outcome\":{outcome}}}}}}}"
        );
        assert_eq!(
            exchange(id, bytes.as_bytes()),
            Err(EmbeddedFfiStatus::InvalidArgument as i32),
            "accepted malformed outcome: {outcome}"
        );
    }
    for outcome in [
        json!({"ok":true,"value":null,"effects":"committed"}),
        json!({"ok":false,"error":{"code":"internal","message":"host failed"},"effects":"committed"}),
    ] {
        let result = command(
            id,
            json!({"type":"runtime","runtime_id":"absent",
            "operation":{"type":"host_request_complete","request_id":"absent","outcome":outcome}}),
        )
        .unwrap();
        assert_eq!(result["error"]["code"], "not_found");
    }
    finish(id);
}

/// A registration whose identity cannot be returned must leave the core registry empty and recoverable.
/// 无法返回身份的注册必须使核心注册表保持为空且可以恢复。
#[test]
fn ffi_embedded_registration_response_capacity_precedes_core_mutation() {
    let mut limits = config();
    limits.max_response_bytes = 100;
    limits.max_request_bytes = 16384;
    let id = create(limits);
    let reserved = command(id, json!({"type":"runtime_reserve"})).unwrap();
    assert_eq!(reserved["status"], "ok");
    let runtime_id = reserved["result"]["runtime_id"].as_str().unwrap();
    let initialized = command(
        id,
        json!({"type":"runtime_initialize","runtime_id":runtime_id,
        "engine_options":runtimes::engine_options(),"runtime_config":runtimes::runtime_config()}),
    )
    .unwrap();
    assert_eq!(initialized["status"], "ok");
    let registration = command(
        id,
        json!({"type":"runtime","runtime_id":runtime_id,
        "operation":{"type":"capabilities_register","descriptors":[{
            "name":"test.capacity","version":"1.0.0","description":"Test capacity",
            "input_schema":true,"output_schema":true,"execution":"queued","permissions":[],
            "scope":"invocation","max_concurrent":1,"max_call_ms":1000,
            "max_input_bytes":32,"max_output_bytes":32,"effects":"read_only","idempotency":"none"
        }]}}),
    );
    assert_eq!(
        registration,
        Err(EmbeddedFfiStatus::CapacityExceeded as i32)
    );
    let listed = command(
        id,
        json!({"type":"runtime","runtime_id":runtime_id,
        "operation":{"type":"capabilities_list","permissions":[]}}),
    )
    .unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
    assert_eq!(listed["result"], json!([]));
    let closed = command(id, json!({"type":"runtime_close","runtime_id":runtime_id})).unwrap();
    assert_eq!(closed["status"], "ok");
    // Diagnostic snapshots may exceed this deliberately tiny transport, so read actual closure via the same owned slot.
    // 诊断快照可能超过此刻意缩小的传输，因此通过同一拥有槽读取实际关闭状态。
    let transport = transport::get(id).unwrap();
    let slot = transport.runtime(runtime_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while serde_json::to_value(slot.snapshot().unwrap()).unwrap()["closed"] != true {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let freed = command(id, json!({"type":"runtime_free","runtime_id":runtime_id})).unwrap();
    assert_eq!(freed["status"], "ok");
    finish(id);
}
