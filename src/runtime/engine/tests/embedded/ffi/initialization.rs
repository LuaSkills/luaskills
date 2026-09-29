//! Public JSON registration freezes initialization authority for independent and grouped pools.
//! 公开 JSON 注册为独立池及分组池冻结初始化权威。

use super::*;
use crate::runtime::embedded::OperationPhase;

/// Omission and null inherit; empty and explicit lists narrow real cold and prewarmed source execution.
/// 省略及空值继承；空列表及显式列表收窄真实冷启动与预热源码执行。
#[test]
fn ffi_embedded_initialization_policy_preserves_placement_and_business_authority() {
    for declaration in [
        None,
        Some(Value::Null),
        Some(json!([])),
        Some(json!(["test.queue"])),
    ] {
        for grouped in [false, true] {
            for prewarm in [false, true] {
                // Each matrix entry owns a real engine so no previous pool or callback can mask admission.
                // 每项组合拥有真实引擎，避免先前池或回调掩盖入场行为。
                let layout = SystemRuntimeTestLayout::new("ffi initialization policy");
                let client = Client::new(&layout);
                client.capability();
                let allowed = declaration.as_ref() != Some(&json!([]));
                let source = format!(
                    "local init = vulcan.host.call('test.queue', 'initialization'); assert(init.ok == {allowed}); return {{call=function() return vulcan.host.call('test.queue','business') end}}"
                );
                let mut registration = json!({"type":"pool_register", "definition":definition(&layout, &source),
                    "policy":pool_policy(InstanceReuse::Reusable), "permissions":["test.host"], "execution_revision":"initialization-v1"});
                if let Some(names) = &declaration {
                    registration["initialization_capabilities"] = names.clone();
                }
                if grouped {
                    let (config, policy) = super::capacities::policies();
                    let receipt = client.ok(json!({"type":"capacity_register","plugin_id":layout.package_id,"config":config}));
                    registration["capacity_id"] = receipt["capacity_id"].clone();
                    registration["policy"] = serde_json::to_value(policy).expect("member policy");
                }
                let pool = client.ok(registration)["pool_id"]
                    .as_str()
                    .expect("pool identity")
                    .to_owned();
                // Source execution is selected explicitly; the same immutable policy guards both paths.
                // 显式选择源码执行入口；同一不可变策略保护两条路径。
                let mut operation = if prewarm {
                    client.ok(json!({"type":"instance_prewarm","timeout_ms":5000,
                        "request":{"pool_id":pool,"context":LuaInvocationContext::default()}}))["operation_id"]
                        .as_str().expect("prewarm identity").to_owned()
                } else {
                    client.submit(&pool, Value::Null)
                };
                if allowed {
                    let request = client.host_request();
                    assert_eq!(request.arguments, json!("initialization"));
                    client.ok(
                        json!({"type":"host_request_complete","request_id":request.request_id,
                        "outcome":{"ok":true,"value":null,"effects":"committed"}}),
                    );
                }
                if prewarm {
                    assert_eq!(client.terminal(&operation).phase, OperationPhase::Succeeded);
                    operation = client.submit(&pool, Value::Null);
                }
                // Observing the real business request proves denied initialization never entered the broker.
                // 观测真实业务请求证明被拒绝的初始化从未进入代理队列。
                let request = client.host_request();
                assert_eq!(request.arguments, json!("business"));
                client.ok(
                    json!({"type":"host_request_complete","request_id":request.request_id,
                    "outcome":{"ok":true,"value":"acknowledged","effects":"committed"}}),
                );
                let done = client.terminal(&operation);
                assert_eq!(done.phase, OperationPhase::Succeeded, "{done:?}");
                assert_eq!(
                    done.value.expect("business outcome")["value"],
                    "acknowledged"
                );
                assert_eq!(
                    client.ok(json!({"type":"host_requests_take","limit":1})),
                    json!([])
                );
                client.close();
            }
        }
    }
}

/// Serialized initialization names cannot grant unavailable callbacks or replace ordinary permissions.
/// 序列化初始化名称不能授予不可用回调，也不能替代普通权限。
#[test]
fn ffi_embedded_initialization_policy_rejects_unauthorized_registration() {
    let layout = SystemRuntimeTestLayout::new("ffi invalid initialization policy");
    let client = Client::new(&layout);
    client.capability();
    for (names, permissions) in [
        (json!(["missing.callback"]), json!(["test.host"])),
        (json!(["test.queue"]), json!([])),
    ] {
        let response = client.command(json!({"type":"pool_register",
            "definition":definition(&layout, "error('must never execute')"),
            "policy":pool_policy(InstanceReuse::Reusable), "permissions":permissions,
            "execution_revision":"invalid-initialization", "initialization_capabilities":names}));
        assert_eq!(response["error"]["code"], "permission_denied");
    }
    client.close();
}
