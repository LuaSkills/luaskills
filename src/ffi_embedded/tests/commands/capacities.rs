//! Capacity mutation responses must be reserved before changing authoritative ownership.
//! 容量变更响应必须在改变权威归属前完成预留。

use super::*;
use crate::ffi_embedded::{commands::RuntimeCommand, control};
use crate::runtime::embedded::{
    EmbeddedCapacityConfig, EmbeddedPluginConfig, PoolKind, VmCapacityConfig,
};

/// An undeliverable capacity receipt leaves no reservation, and tiny close/forget responses do not mutate state.
/// 无法交付的容量回执不留下预留，过小关闭／遗忘响应也不改变状态。
#[test]
fn ffi_embedded_capacity_response_preflight_prevents_unobservable_mutation() {
    // The transport can return empty acknowledgements but not a maximal generated capacity identity.
    // 传输能返回空确认，但不能返回最长生成容量身份。
    let mut limits = config();
    limits.max_response_bytes = 100;
    limits.max_request_bytes = 16384;
    // Retain the exact transport used by every public native exchange.
    // 保留每次公开原生交换使用的精确传输。
    let id = create(limits);
    // The two-step runtime lifecycle publishes a control identity before initialization.
    // 两阶段运行时生命周期在初始化前发布控制身份。
    let runtime_id = runtimes::reserve(id);
    // Initialization acknowledges its original identity within this deliberately small response budget.
    // 初始化在此刻意缩小的响应预算内确认原始身份。
    let parent = runtimes::runtime_config();
    assert_eq!(
        command(
            id,
            json!({"type":"runtime_initialize","runtime_id":runtime_id,
        "engine_options":runtimes::engine_options(),"runtime_config":parent})
        )
        .unwrap()["status"],
        "ok"
    );
    // The owning plugin budget is exact and inherits the existing fixture limits as a whole.
    // 所属插件预算精确且整体继承既有夹具限制。
    let plugin = EmbeddedPluginConfig {
        max_registered_pools: parent.max_registered_pools,
        max_sessions: parent.max_sessions,
        max_resident_vms: parent.max_resident_vms,
        max_running_calls: parent.max_running_calls,
        max_queued_calls: parent.max_queued_calls,
        max_queued_bytes: parent.max_queued_bytes,
        max_operations: parent.max_operations,
    };
    assert_eq!(
        command(
            id,
            json!({"type":"runtime","runtime_id":runtime_id,"operation":{
        "type":"plugin_register","plugin_id":"owner","config":plugin}})
        )
        .unwrap()["status"],
        "ok"
    );
    // An unused dedicated minimum would be observable as a leaked plugin commitment.
    // 未使用专用最小值会作为泄漏的插件承诺被观察到。
    let capacity_config = EmbeddedCapacityConfig {
        resources: VmCapacityConfig {
            kind: PoolKind::Dedicated,
            min_resident_vms: 1,
            max_resident_vms: 1,
            max_running_calls: 1,
        },
        max_queued_calls: parent.max_queued_calls,
        max_queued_bytes: parent.max_queued_bytes,
    };
    assert_eq!(
        command(
            id,
            json!({"type":"runtime","runtime_id":runtime_id,"operation":{
        "type":"capacity_register","plugin_id":"owner","config":capacity_config}})
        ),
        Err(EmbeddedFfiStatus::CapacityExceeded as i32)
    );
    // Inspect the same authoritative owner because a full diagnostic cannot fit the tiny transport.
    // 完整诊断无法装入微小传输，因此检查同一权威所有者。
    let transport = transport::get(id).unwrap();
    // The retained slot owns the actual formal runtime, rather than a duplicate test registry.
    // 保留槽拥有实际正式运行时，而非重复测试注册表。
    let slot = transport.runtime(&runtime_id).unwrap();
    // The lease remains alive only for observation and exact mutation-boundary tests.
    // 租借仅为观测及精确变更边界测试保持存活。
    let lease = slot.acquire(false).unwrap();
    assert_eq!(
        lease
            .runtime()
            .plugin("owner")
            .unwrap()
            .committed_resident_vms,
        0
    );
    // Direct setup obtains an observable identity so close and forget can be tested independently.
    // 直接设置取得可观察身份，以独立测试关闭及遗忘。
    let capacity_id = lease
        .runtime()
        .register_capacity("owner", capacity_config)
        .unwrap();
    // The revision preflight reserves its longest possible token, not only the currently short value.
    // 修订预检预留最长可能令牌，而非仅预留当前短值。
    let before = lease.runtime().capacity_policy(&capacity_id).unwrap();
    // This valid change would alter queue admission if an undeliverable command were executed.
    // 若执行无法交付的命令，此合法变更会改变队列入场。
    let mut revised = before.capacity.config.clone();
    revised.max_queued_calls -= 1;
    // Derive the exact envelope boundary from the same serializer used by native mutation preflight.
    // 从原生变更预检使用的同一序列化器派生精确信封边界。
    let required =
        crate::ffi_embedded::protocol::respond::<String>(Ok(u64::MAX.to_string()), usize::MAX)
            .unwrap()
            .len();
    assert_eq!(
        control::execute(
            &slot,
            RuntimeCommand::CapacityRevise {
                capacity_id: capacity_id.clone(),
                expected_revision: before.revision.clone(),
                config: revised.clone(),
            },
            required - 1
        ),
        Err(EmbeddedFfiStatus::CapacityExceeded)
    );
    assert_eq!(
        lease
            .runtime()
            .capacity_policy(&capacity_id)
            .unwrap()
            .revision,
        before.revision
    );
    assert_eq!(
        lease.runtime().capacity(&capacity_id).unwrap().config,
        before.capacity.config
    );
    // The public command succeeds with an observable opaque token and never silently retries a stale predecessor.
    // 公开命令通过可观察不透明令牌成功，且绝不静默重试过期前驱。
    let response = command(id, json!({"type":"runtime","runtime_id":runtime_id,"operation":{
        "type":"capacity_revise","capacity_id":capacity_id,"expected_revision":before.revision,"config":revised
    }})).unwrap();
    assert_eq!(response["status"], "ok");
    assert!(response["result"].is_string());
    assert_ne!(response["result"], before.revision);
    assert_eq!(
        lease.runtime().capacity(&capacity_id).unwrap().config,
        revised
    );
    assert_eq!(
        control::execute(
            &slot,
            RuntimeCommand::CapacityClose {
                capacity_id: capacity_id.clone()
            },
            1
        ),
        Err(EmbeddedFfiStatus::CapacityExceeded)
    );
    assert!(!lease.runtime().capacity(&capacity_id).unwrap().closing);
    assert_eq!(
        command(
            id,
            json!({"type":"runtime","runtime_id":runtime_id,"operation":{
        "type":"capacity_close","capacity_id":capacity_id}})
        )
        .unwrap()["status"],
        "ok"
    );
    assert_eq!(
        control::execute(
            &slot,
            RuntimeCommand::CapacityForget {
                capacity_id: capacity_id.clone()
            },
            1
        ),
        Err(EmbeddedFfiStatus::CapacityExceeded)
    );
    assert_eq!(
        lease
            .runtime()
            .capacity(&capacity_id)
            .unwrap()
            .committed_resident_vms,
        1
    );
    assert_eq!(
        command(
            id,
            json!({"type":"runtime","runtime_id":runtime_id,"operation":{
        "type":"capacity_forget","capacity_id":capacity_id}})
        )
        .unwrap()["status"],
        "ok"
    );
    assert_eq!(
        lease
            .runtime()
            .plugin("owner")
            .unwrap()
            .committed_resident_vms,
        0
    );
    drop(lease);
    assert_eq!(
        command(id, json!({"type":"runtime_close","runtime_id":runtime_id})).unwrap()["status"],
        "ok"
    );
    // Native shutdown is observed from the same slot without requiring an oversized diagnostic response.
    // 从同一槽观测原生关闭，无需超大诊断响应。
    let deadline = Instant::now() + Duration::from_secs(5);
    while serde_json::to_value(slot.snapshot().unwrap()).unwrap()["closed"] != true {
        assert!(
            Instant::now() < deadline,
            "actual runtime ownership must drain"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        command(id, json!({"type":"runtime_free","runtime_id":runtime_id})).unwrap()["status"],
        "ok"
    );
    finish(id);
}
