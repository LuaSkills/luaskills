//! Public C transport evidence for explicit instance prewarming and immutable pool closure.
//! 明确实例预热及不可变池关闭的公开 C 传输证据。

use super::*;
use crate::runtime::embedded::OperationPhase;

/// Public C queries distinguish actual initialization, confirmed readiness and admission closure.
/// 公开 C 查询区分真实初始化、已确认就绪及入场关闭。
#[test]
fn ffi_embedded_reusable_readiness_preserves_exact_pool_and_callback_ownership() {
    // All commands cross the public JSON transport and the real Lua initializer waits for a host acknowledgement.
    // 全部命令经过公开 JSON 传输，真实 Lua 初始化器等待宿主确认。
    let layout = SystemRuntimeTestLayout::new("ffi reusable readiness");
    let client = Client::new(&layout);
    client.capability();
    // This reusable declaration retains the same real instance after initialization completes.
    // 此可复用声明在初始化完成后保留同一真实实例。
    let pool = client.pool(
        &layout,
        "assert(vulcan.host.call('test.queue',nil).ok); return {call=function() return true end}",
        InstanceReuse::Reusable,
    );
    // No query can manufacture a warm instance before explicit work starts.
    // 明确工作开始前，任何查询都不能制造热实例。
    let cold = client.ok(json!({"type":"pool_reusable_status","pool_id":pool}));
    assert_eq!(cold["pool_id"], pool);
    assert_eq!(cold["ready"], 0);
    assert_eq!(cold["physical"]["resident"], 0);
    // Keep the actual initialization operation independently from the control-query replies.
    // 独立于控制查询回复保留真实初始化操作。
    let receipt = client.ok(json!({"type":"instance_prewarm","timeout_ms":5000,
        "request":{"pool_id":pool,"context":LuaInvocationContext::default()}}));
    let operation = receipt["operation_id"]
        .as_str()
        .expect("native initialization identity");
    let request = client.host_request();
    // A delivered real callback is not a confirmed idle lease.
    // 已交付真实回调不是已确认空闲租借。
    let running = client.ok(json!({"type":"pool_reusable_status","pool_id":pool}));
    assert_eq!(running["ready"], 0);
    assert_eq!(running["unavailable"], 1);
    assert_eq!(running["physical"]["resident"], 1);
    assert!(!client.snapshot(operation).phase.is_terminal());
    client.ok(
        json!({"type":"host_request_complete","request_id":request.request_id,
        "outcome":{"ok":true,"value":null,"effects":"committed"}}),
    );
    assert_eq!(client.terminal(operation).phase, OperationPhase::Succeeded);
    // Formal terminal publication makes the exact original lease borrowable.
    // 正式终态发布使精确原租借可被借用。
    let ready = client.ok(json!({"type":"pool_reusable_status","pool_id":pool}));
    assert_eq!(ready["ready"], 1);
    assert_eq!(ready["unavailable"], 0);
    assert_eq!(ready["closing"], false);
    assert_eq!(ready["admission_blocked"], false);
    // A wrong pool kind and unknown identity fail before initializing another module.
    // 错误池种类及未知身份在初始化另一模块前失败。
    let single = client.pool(
        &layout,
        "error('must not execute')",
        InstanceReuse::SingleCall,
    );
    assert_eq!(
        client.command(json!({"type":"pool_reusable_status","pool_id":single}))["error"]["code"],
        "invalid_argument"
    );
    assert_eq!(
        client.command(json!({"type":"pool_reusable_status","pool_id":"unknown"}))["error"]["code"],
        "not_found"
    );
    client.root(json!({"type":"runtime_close","runtime_id":client.runtime_id}));
    // Runtime closure fences work while the same pool remains queryable on the control path.
    // 运行时关闭阻止工作，同时同一池在控制路径保持可查询。
    let closing = client.ok(json!({"type":"pool_reusable_status","pool_id":pool}));
    assert_eq!(closing["closing"], true);
    assert_eq!(closing["ready"], 0);
    client.close();
}

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
