//! Verify the default production library rejects implicit database ownership through its public ABI.
//! 通过公开 ABI 验证默认生产库拒绝隐式数据库所有权。
#![cfg(not(feature = "sqlite-journal"))]

use luaskills::ffi_embedded::*;
use luaskills::ffi_standard::FfiBorrowedBuffer;
use luaskills::runtime::embedded::EmbeddedRuntimeConfig;
use luaskills::{LuaEngineOptions, LuaRuntimeHostOptions, LuaVmPoolConfig};
use serde_json::{Value, json};

/// Send `command` to `id`, copy its JSON response, and release the native result before returning it.
/// 将 `command` 发送至 `id`，复制 JSON 响应，并在返回前释放原生结果。
fn command(id: u64, command: Value) -> Value {
    // Serialized bytes remain borrowed until the synchronous ABI request returns.
    // 序列化字节保持借用，直到同步 ABI 请求返回。
    let bytes = serde_json::to_vec(&json!({"protocol_version":1,"command":command})).unwrap();
    // The transport owns the result allocation until its matching free call.
    // 传输拥有结果分配，直到调用匹配的释放函数。
    let mut result = FfiEmbeddedResultV1::default();
    // SAFETY: Input bytes and the writable result remain live for the entire synchronous call.
    // 安全性：输入字节与可写结果在整个同步调用期间保持有效。
    assert_eq!(
        unsafe {
            luaskills_ffi_embedded_request_v1(
                id,
                FfiBorrowedBuffer {
                    ptr: bytes.as_ptr(),
                    len: bytes.len(),
                },
                &mut result,
            )
        },
        0
    );
    // SAFETY: A successful transport call supplies this live, immutable result allocation.
    // 安全性：成功的传输调用提供此有效且不可变的结果分配。
    let response =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(result.ptr, result.len) })
            .unwrap();
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    response
}

/// Reject file persistence without the explicit adapter, retain queryable failure, and leave storage absent.
/// 没有显式适配器时拒绝文件持久化，保留可查询失败，并保持存储不存在。
#[test]
fn default_library_requires_explicit_journal_adapter() {
    // Public transport limits carry the complete engine configuration without creating a runtime.
    // 公开传输上限容纳完整引擎配置，但不创建运行时。
    let transport = FfiEmbeddedTransportConfigV1 {
        struct_size: std::mem::size_of::<FfiEmbeddedTransportConfigV1>() as u32,
        protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
        max_runtimes: 1,
        max_result_buffers: 1,
        max_result_bytes: 8192,
        max_response_bytes: 8192,
        max_request_bytes: 16384,
    };
    // Only this native transport identity is owned by the fixture.
    // 夹具仅拥有此原生传输身份。
    let mut id = 0;
    // SAFETY: Both pointers refer to correctly sized, live values for the duration of construction.
    // 安全性：两个指针在构造期间均指向尺寸正确且有效的值。
    assert_eq!(
        unsafe { luaskills_ffi_embedded_transport_new_v1(&transport, &mut id) },
        0
    );
    // Reservation owns no engine, threads or storage before explicit initialization.
    // 在显式初始化前，预留不拥有引擎、线程或存储。
    let runtime_id = command(id, json!({"type":"runtime_reserve"}))["result"]["runtime_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A unique path proves rejection does not create or overwrite any database file.
    // 唯一路径证明拒绝不会创建或覆盖任何数据库文件。
    let path = std::env::temp_dir().join(format!(
        "luaskills-no-journal-{}-{id}.db",
        std::process::id()
    ));
    assert!(!path.exists());
    // These explicit runtime limits match the existing native lifecycle test contract.
    // 这些显式运行时上限与现有原生寿命测试契约一致。
    let runtime_config = EmbeddedRuntimeConfig {
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
    };
    // Engine options are serialized from their owning Rust type rather than reconstructed field guesses.
    // 引擎选项从其所属 Rust 类型序列化，不猜测字段来重建配置。
    let engine_options = LuaEngineOptions::new(
        LuaVmPoolConfig {
            min_size: 1,
            max_size: 1,
            idle_ttl_secs: 1,
        },
        LuaRuntimeHostOptions::default(),
    );
    assert_eq!(
        command(
            id,
            json!({"type":"runtime_initialize", "runtime_id":runtime_id,
        "engine_options":engine_options, "runtime_config":runtime_config,
        "persistence":{"path":path,"journal":{"max_records":16,"max_record_bytes":16384,"max_database_bytes":131072},
        "worker":{"max_pending_writes":8,"max_pending_bytes":65536}}})
        )["status"],
        "ok"
    );
    // Initialization acknowledgement is separate from the authoritative retained failure state.
    // 初始化确认与权威保留失败状态分开处理。
    let snapshot = command(id, json!({"type":"runtime_status","runtime_id":runtime_id}));
    assert_eq!(snapshot["result"]["initialization"], "failed", "{snapshot}");
    assert!(
        snapshot["result"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("sqlite-journal"),
        "{snapshot}"
    );
    assert!(!path.exists());
    assert_eq!(
        command(id, json!({"type":"runtime_close","runtime_id":runtime_id}))["status"],
        "ok"
    );
    assert_eq!(
        command(id, json!({"type":"runtime_free","runtime_id":runtime_id}))["status"],
        "ok"
    );
    assert_eq!(luaskills_ffi_embedded_transport_close_v1(id), 0);
    assert_eq!(luaskills_ffi_embedded_transport_free_v1(id), 0);
}
