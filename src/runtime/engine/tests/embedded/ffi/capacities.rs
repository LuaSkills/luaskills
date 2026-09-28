//! Real C-ABI capacity commands preserve formal scheduling ownership and exact lifecycle identities.
//! 真实 C ABI 容量命令保留正式调度归属及精确生命周期身份。

use super::*;
use crate::runtime::embedded::{
    EmbeddedCapacityConfig, OperationPhase, PoolKind, VmCapacityConfig,
};

/// Return a capacity declaration and matching member policy using the existing parent fixture limits.
/// 使用既有父级夹具限制，返回容量声明及匹配成员策略。
fn policies() -> (
    EmbeddedCapacityConfig,
    crate::runtime::embedded::PluginPoolConfig,
) {
    // The aggregate reservation is charged once while both member modules remain independent.
    // 聚合预留仅计费一次，两个成员模块保持独立。
    let config = EmbeddedCapacityConfig {
        resources: VmCapacityConfig {
            kind: PoolKind::Dedicated,
            min_resident_vms: 1,
            max_resident_vms: 2,
            max_running_calls: 1,
        },
        max_queued_calls: 2,
        max_queued_bytes: pool_config().max_queued_bytes,
    };
    // Members inherit upper bounds without duplicating the dedicated minimum.
    // 成员继承上限，不重复专用最小值。
    let mut member = pool_policy(InstanceReuse::Reusable);
    member.kind = config.resources.kind;
    member.max_resident_vms = config.resources.max_resident_vms;
    member.max_running_calls = config.resources.max_running_calls;
    member.max_queued_calls = config.max_queued_calls;
    (config, member)
}

/// Public native frames expose grouped queues, immutable caller ownership and deferred physical closure.
/// 公开原生帧暴露分组队列、不可变调用方归属及延迟物理关闭。
#[test]
fn ffi_embedded_capacity_pipeline_retains_queue_and_closing_ownership() {
    // All runtime actions use the public C entrypoints and release every native response allocation.
    // 全部运行时动作使用公开 C 入口，并释放每个原生响应分配。
    let layout = SystemRuntimeTestLayout::new("ffi capacity pipeline");
    // Keep the real engine and the exact transport-local runtime alive through final acknowledgement.
    // 跨最终确认保留真实引擎及精确传输局部运行时。
    let client = Client::new(&layout);
    client.capability();
    // The Rust-derived contract is used as the explicit JSON declaration.
    // Rust 派生契约用作显式 JSON 声明。
    let (config, policy) = policies();
    // A fresh core-issued identity is returned before any module is registered.
    // 注册任何模块前先返回新的核心签发身份。
    let capacity =
        client
            .ok(json!({"type":"capacity_register","plugin_id":layout.package_id,"config":config}))
            ["capacity_id"]
            .as_str()
            .unwrap()
            .to_owned();
    assert_eq!(
        client.ok(json!({"type":"capacity_status","capacity_id":capacity}))["committed_resident_vms"],
        1
    );
    // The callback keeps the first module's execution allowance occupied.
    // 回调持续占用首个模块执行额度。
    let source = r#"
        -- Await the exact authorized host request and return its structured result.
        -- 等待精确已授权宿主请求并返回结构化结果。
        return {call=function() return vulcan.host.call('test.queue',{}) end}
    "#;
    // The first immutable module binds explicitly to the generated capacity identity.
    // 首个不可变模块显式绑定生成的容量身份。
    let first = client.ok(json!({"type":"pool_register","capacity_id":capacity,
        "definition":definition(&layout, source), "policy":policy,
        "permissions":["test.host"],"execution_revision":"capacity-first"}))["pool_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A separate module keeps its own source and revision under the same aggregate limits.
    // 独立模块在相同聚合限制下保留自身源码及修订。
    let second = client.ok(json!({"type":"pool_register","capacity_id":capacity,
        "definition":definition(&layout, r#"
            -- Return a fixed marker without external effects.
            -- 返回固定标记，不产生外部副作用。
            return {call=function() return 42 end}
        "#), "policy":policy, "permissions":["test.host"],"execution_revision":"capacity-second"}))
        ["pool_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The ordinary operation enters real Lua and waits for a host acknowledgement.
    // 普通操作进入真实 Lua，并等待宿主确认。
    let running = client.submit(&first, Value::Null);
    // Exact request identity is retained across capacity close.
    // 精确请求身份跨容量关闭保留。
    let request = client.host_request();
    // The second member cannot consume a second concurrent execution allowance.
    // 第二个成员不能消费第二份并发执行额度。
    let queued = client.submit(&second, Value::Null);
    // Observe actual scheduler counts through the public status command.
    // 通过公开状态命令观测实际调度计数。
    let status = client.ok(json!({"type":"capacity_status","capacity_id":capacity}));
    assert_eq!(status["config"], serde_json::to_value(&config).unwrap());
    assert_eq!(status["plugin_id"], layout.package_id);
    assert_eq!(status["active_operations"], 1);
    assert_eq!(status["queued_calls"], 1);
    assert_eq!(status["retained_pools"], 2);
    assert_eq!(client.snapshot(&queued).phase, OperationPhase::Queued);
    client.ok(json!({"type":"capacity_close","capacity_id":capacity}));
    assert_eq!(
        client.command(json!({"type":"capacity_forget","capacity_id":capacity}))["error"]["code"],
        "busy"
    );
    assert_eq!(
        client.terminal(&queued).error.unwrap().code,
        EmbeddedErrorCode::Closed
    );
    assert_eq!(
        client.ok(json!({"type":"capacity_status","capacity_id":capacity}))["active_operations"],
        1
    );
    client.ok(
        json!({"type":"host_request_complete","request_id":request.request_id,
        "outcome":{"ok":true,"value":null,"effects":"committed"}}),
    );
    assert_eq!(client.terminal(&running).phase, OperationPhase::Succeeded);
    // Physical retirement and cache metadata may finish after operation publication.
    // 物理退役及缓存元数据可能在操作发布后完成。
    let deadline = Instant::now() + Duration::from_secs(5);
    for pool in [&first, &second] {
        loop {
            // Retry only the explicitly busy forgetting operation, never business execution.
            // 仅重试明确忙碌的遗忘操作，绝不重试业务执行。
            let response = client.command(json!({"type":"pool_forget","pool_id":pool}));
            if response["status"] == "ok" {
                break;
            }
            assert_eq!(response["error"]["code"], "busy");
            assert!(Instant::now() < deadline, "real pool ownership must drain");
            std::thread::yield_now();
        }
    }
    assert_eq!(
        client.ok(json!({"type":"capacity_status","capacity_id":capacity}))["committed_resident_vms"],
        1
    );
    client.ok(json!({"type":"capacity_forget","capacity_id":capacity}));
    assert_eq!(
        client.command(json!({"type":"capacity_status","capacity_id":capacity}))["error"]["code"],
        "not_found"
    );
    for operation in [running, queued] {
        client.ok(json!({"type":"operation_forget","operation_id":operation}));
    }
    client.close();
}

/// Requested capacity identities never fall back to independent placement, and closure fences new guarantees.
/// 请求的容量身份绝不回退独立归属，关闭屏障阻止新保证入场。
#[test]
fn ffi_embedded_capacity_explicit_binding_and_runtime_closure_are_strict() {
    // A real package is needed even though registration must not execute its source.
    // 即使注册不得执行源码，也需要真实包。
    let layout = SystemRuntimeTestLayout::new("ffi capacity strict placement");
    // Public ABI setup registers the owning plugin before any capacity.
    // 公开 ABI 设置在任何容量之前注册所属插件。
    let client = Client::new(&layout);
    // Keep declarations compatible except for the identity deliberately under test.
    // 除刻意测试的身份外，保持声明相容。
    let (config, policy) = policies();
    // The registered capacity initially owns no members.
    // 已注册容量初始没有成员。
    let capacity =
        client
            .ok(json!({"type":"capacity_register","plugin_id":layout.package_id,"config":config}))
            ["capacity_id"]
            .as_str()
            .unwrap()
            .to_owned();
    assert_eq!(
        client.command(json!({"type":"pool_register","capacity_id":"missing",
        "definition":definition(&layout,"error('must not initialize')"), "policy":policy,
        "permissions":[],"execution_revision":"missing"}))["error"]["code"],
        "not_found"
    );
    assert_eq!(
        client.ok(json!({"type":"capacity_status","capacity_id":capacity}))["retained_pools"],
        0
    );
    // Explicit null uses the original independent placement and does not attach to the existing capacity.
    // 显式空值使用原独立归属，不附加到既有容量。
    let independent = client.ok(json!({"type":"pool_register","capacity_id":null,
        "definition":definition(&layout,"error('must not initialize')"), "policy":policy,
        "permissions":[],"execution_revision":"independent"}))["pool_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        client.ok(json!({"type":"capacity_status","capacity_id":capacity}))["retained_pools"],
        0
    );
    client.ok(json!({"type":"pool_close","pool_id":independent}));
    client.ok(json!({"type":"pool_forget","pool_id":independent}));
    client.root(json!({"type":"runtime_close","runtime_id":client.runtime_id}));
    assert_eq!(
        client.command(
            json!({"type":"capacity_register","plugin_id":layout.package_id,"config":config})
        )["error"]["code"],
        "closed"
    );
    assert_eq!(
        client.ok(json!({"type":"capacity_status","capacity_id":capacity}))["closing"],
        true
    );
    client.ok(json!({"type":"capacity_forget","capacity_id":capacity}));
    client.close();
}
