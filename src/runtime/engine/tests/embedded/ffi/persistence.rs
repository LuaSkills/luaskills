//! Real Lua history crosses the public C ABI and retains its original identity after reopening storage.
//! 真实 Lua 历史经过公开 C ABI，并在重新打开存储后保留原始身份。

use super::*;

/// Live ownership and unresolved effects independently retain history; reopening never adopts historical handles.
/// 活动所有权与未决副作用独立保留历史；重新打开绝不接管历史句柄。
#[test]
fn ffi_embedded_pipeline_persistence_preserves_context_and_reopened_history() {
    // The existing package fixture owns the temporary directory and real Lua source.
    // 既有包夹具拥有临时目录与真实 Lua 源码。
    let layout = SystemRuntimeTestLayout::new("ffi embedded durable history");
    // Storage belongs to the trusted test host and shares the fixture's cleanup lifetime.
    // 存储属于可信测试宿主，并共享夹具清理寿命。
    let storage = json!({"path":layout.package_root.join("operations.db"),
        "journal":{"max_records":16,"max_record_bytes":32768,"max_database_bytes":262144},
        "worker":{"max_pending_writes":8,"max_pending_bytes":131072}});
    // One explicitly persistent core performs the original business call.
    // 一个显式持久核心执行原始业务调用。
    let client = Client::with_storage(&layout, 32768, storage.clone());
    // The original core namespace is distinct from its FFI control identity.
    // 原始核心命名空间独立于其 FFI 控制身份。
    let namespace = client.root(json!({"type":"runtime_status","runtime_id":client.runtime_id}))["result"]["core_runtime_id"].clone();
    // The call has no host callbacks, so its identity must come from admission itself.
    // 调用没有宿主回调，因此身份必须来自入场本身。
    let pool_id = client.pool(
        &layout,
        "return {call=function(a) return {original=a} end}",
        InstanceReuse::Reusable,
    );
    // Retain the exact original operation identity across native release and reopen.
    // 跨原生释放及重新打开保留精确原始操作身份。
    let operation_id = client.submit(&pool_id, json!("原始结果"));
    // Terminal publication proves the actual persistent checkpoint was acknowledged.
    // 终态发布证明实际持久检查点已确认。
    let done = client.terminal(&operation_id);
    assert_eq!(done.value, Some(json!({"original":"原始结果"})));
    assert!(
        client
            .ok(json!({"type":"operation_persistence_failure","operation_id":operation_id}))
            .is_null()
    );
    assert_eq!(
        client.command(json!({"type":"operation_retry_checkpoint","operation_id":operation_id}))["error"]
            ["code"],
        "busy"
    );
    // The complete wire record is compared after reopening, not reconstructed from new registrations.
    // 重新打开后比较完整线记录，不从新注册重建。
    let history = client.ok(
        json!({"type":"history_get","history_runtime_id":namespace,"operation_id":operation_id}),
    );
    assert_eq!(history["snapshot"], serde_json::to_value(&done).unwrap());
    assert_eq!(history["snapshot"]["context"]["pool_id"], pool_id);
    assert_eq!(client.ok(json!({"type":"history_next"})), history);
    assert_eq!(client.command(json!({"type":"history_forget","history_runtime_id":namespace,"operation_id":operation_id,"expected_revision":history["revision"]}))["error"]["code"], "busy");
    client.close();
    // A new runtime may read old evidence but cannot use its operation as a current handle.
    // 新运行时可以读取旧证据，但不能将其操作用作当前句柄。
    let reopened = Client::with_storage(&layout, 32768, storage);
    assert_ne!(
        reopened.root(json!({"type":"runtime_status","runtime_id":reopened.runtime_id}))["result"]
            ["core_runtime_id"],
        namespace
    );
    assert_eq!(
        reopened.ok(
            json!({"type":"history_get","history_runtime_id":namespace,"operation_id":operation_id})
        ),
        history
    );
    assert_eq!(
        reopened.command(json!({"type":"operation_status","operation_id":operation_id}))["error"]["code"],
        "not_found"
    );
    assert_eq!(history["snapshot"]["effects"], "unknown");
    assert_eq!(reopened.command(json!({"type":"history_forget","history_runtime_id":namespace,"operation_id":operation_id,"expected_revision":history["revision"]}))["error"]["code"], "busy");
    assert_eq!(reopened.ok(json!({"type":"history_next"})), history);
    // Forgetting current in-memory state still cannot erase unresolved durable evidence.
    // 遗忘当前内存状态仍不能抹除未决持久证据。
    let next_pool = reopened.pool(
        &layout,
        "return {call=function(a) return a end}",
        InstanceReuse::Reusable,
    );
    // The new operation remains a different authority even when its business result matches.
    // 即使业务结果匹配，新操作仍为不同权威。
    let next_operation = reopened.submit(&next_pool, json!(true));
    reopened.terminal(&next_operation);
    // Exact current identity avoids assuming lexical order between independently random namespaces.
    // 精确当前身份避免假定独立随机命名空间的字典顺序。
    let next_namespace =
        reopened.root(json!({"type":"runtime_status","runtime_id":reopened.runtime_id}))["result"]
            ["core_runtime_id"]
            .clone();
    // History remains independently addressable after the live operation is removed.
    // 活动操作移除后，历史仍可独立寻址。
    let next_history = reopened.ok(json!({"type":"history_get","history_runtime_id":next_namespace,"operation_id":next_operation}));
    reopened.ok(json!({"type":"operation_forget","operation_id":next_operation}));
    assert_eq!(reopened.command(json!({"type":"history_forget","history_runtime_id":next_history["runtime_id"],"operation_id":next_operation,"expected_revision":next_history["revision"]}))["error"]["code"], "busy");
    assert_eq!(reopened.ok(json!({"type":"history_get","history_runtime_id":next_namespace,"operation_id":next_operation})), next_history);
    reopened.close();
}
