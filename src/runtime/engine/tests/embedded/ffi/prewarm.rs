//! Public C transport evidence for explicit instance prewarming and immutable pool closure.
//! 明确实例预热及不可变池关闭的公开 C 传输证据。

use super::*;
use crate::runtime::embedded::OperationPhase;

/// The versioned transport returns operation receipts, distinct VM identities, and a no-export context.
/// 版本化传输返回操作回执、不同 VM 身份及无导出上下文。
#[test]
fn ffi_embedded_pipeline_prewarm_preserves_real_reuse_and_closed_identity() {
    // Exercise serialized requests through the public ABI rather than calling Rust scheduling directly.
    // 通过公开 ABI 验证序列化请求，不直接调用 Rust 调度。
    let layout = SystemRuntimeTestLayout::new("ffi formal prewarm");
    let client = Client::new(&layout);
    let pool = client.pool(
        &layout,
        "local count=0; return {call=function() count=count+1; return count end}",
        InstanceReuse::Reusable,
    );
    // Retain exact public identities and count physical residents independently.
    // 保留精确公开身份，并独立统计物理驻留。
    let mut instances = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let receipt = client.ok(json!({"type":"instance_prewarm", "timeout_ms":5000,
            "request":{"pool_id":pool,"context":LuaInvocationContext::default()}}));
        let operation_id = receipt["operation_id"].as_str().unwrap();
        let result = client.terminal(operation_id);
        assert_eq!(
            result.phase,
            OperationPhase::Succeeded,
            "{:?}",
            result.error
        );
        let crate::runtime::embedded::OperationContext::Module(context) = result.context else {
            panic!("FFI prewarm must preserve explicit original module context");
        };
        assert!(context.prewarm);
        assert!(context.export.is_none());
        assert_eq!(context.pool_id, pool);
        assert!(
            instances.insert(
                result.value.unwrap()["instance_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            )
        );
    }
    assert_eq!(
        client.ok(json!({"type":"pool_status", "pool_id":pool}))["resident"],
        json!(instances.len())
    );
    for count in 1..=2 {
        let business = client.submit(&pool, Value::Null);
        assert_eq!(client.terminal(&business).value, Some(json!(count)));
    }
    // Closing the original pool fences new prewarm admission without resolving a replacement pool.
    // 关闭原池会封锁新预热入场，不解析替代池。
    client.ok(json!({"type":"pool_close", "pool_id":pool}));
    let rejected = client.command(json!({"type":"instance_prewarm", "timeout_ms":5000,
        "request":{"pool_id":pool,"context":LuaInvocationContext::default()}}));
    assert_eq!(rejected["error"]["code"], "closed");
    client.close();
}
