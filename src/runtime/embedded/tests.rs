use super::*;
use std::time::Duration;

mod operations;

/// Return explicit small parent budgets for deterministic resource-pressure tests.
/// 返回确定性资源压力测试使用的显式小规模父级预算。
pub(super) fn config() -> EmbeddedRuntimeConfig {
    EmbeddedRuntimeConfig {
        max_registered_pools: 8,
        max_sessions: 8,
        max_registered_capabilities: 8,
        max_resident_vms: 3,
        max_running_calls: 2,
        max_queued_calls: 8,
        max_queued_bytes: 4096,
        max_operations: 16,
        max_effect_records_per_operation: 16,
        max_effect_bytes_per_operation: 8192,
        max_host_requests: 4,
        max_host_request_bytes: 4096,
        max_value_bytes: 1024,
    }
}

/// Return a group policy for `kind` with explicit non-lendable `reserved` capacity.
/// 返回具有显式不可出借 `reserved` 容量的 `kind` 分组策略。
fn policy(kind: PoolKind, reserved: usize) -> PluginPoolConfig {
    PluginPoolConfig {
        kind,
        min_resident_vms: reserved,
        max_resident_vms: 3,
        max_running_calls: 2,
        max_queued_calls: 8,
        reuse: InstanceReuse::Reusable,
        serial: false,
        backend: ExecutionBackend::InProcess,
        idle_ttl_ms: None,
        max_uses: None,
    }
}

/// Shared workloads cannot consume a dedicated group's idle reservation.
/// 公共工作负载不能消耗专用分组的空闲预留。
#[test]
fn embedded_governor_preserves_dedicated_reservations() {
    // Three physical slots leave one unreserved slot after dedicated admission.
    // 专用分组入场后，三个物理槽位剩余一个未预留槽位。
    let governor = PoolGovernor::new(config()).unwrap();
    governor
        .register_group("dedicated", policy(PoolKind::Dedicated, 2))
        .unwrap();
    governor
        .register_group("shared", policy(PoolKind::Shared, 0))
        .unwrap();
    // Retain the single unreserved slot while testing admission of both groups.
    // 测试两个分组入场时，保留唯一未预留槽位。
    let shared = governor.reserve("shared").unwrap();
    assert!(matches!(
        governor.reserve("shared"),
        Err(EmbeddedError {
            code: EmbeddedErrorCode::CapacityExceeded,
            ..
        })
    ));
    // Dedicated reservations remain available even after shared capacity is saturated.
    // 公共容量饱和后，专用预留仍然可用。
    let first = governor.reserve("dedicated").unwrap();
    // The second reserved slot proves the minimum is fully enforceable.
    // 第二个预留槽位证明最小保证可完整兑现。
    let second = governor.reserve("dedicated").unwrap();
    assert_eq!(governor.usage(None).unwrap().resident, 3);
    drop((shared, first, second));
    assert_eq!(governor.usage(None).unwrap().resident, 0);
}

/// Retirement remains charged until the owning resource actually releases its token.
/// 退役保持记账，直到所属资源实际释放其令牌。
#[test]
fn embedded_governor_retains_failed_cleanup_capacity() {
    // Parent budgets are explicit; no scheduler defaults enter this proof.
    // 父级预算显式提供；此证明不涉及调度器默认值。
    let governor = PoolGovernor::new(config()).unwrap();
    governor
        .register_group("group", policy(PoolKind::Shared, 0))
        .unwrap();
    // Simulate a VM that has finished work but whose cleanup is still pending.
    // 模拟已完成工作但仍等待清理的 VM。
    let mut slot = governor.reserve("group").unwrap();
    slot.mark_ready().unwrap();
    slot.mark_retiring().unwrap();
    assert_eq!(governor.usage(None).unwrap().retiring, 1);
    assert_eq!(
        governor.unregister_group("group").unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    assert!(slot.begin_execution().is_err());
    drop(slot);
    governor.unregister_group("group").unwrap();
    assert_eq!(governor.usage(None).unwrap().resident, 0);
}

/// Parent saturation cannot partially charge another group's execution budget.
/// 父级饱和不能部分占用另一分组的执行预算。
#[test]
fn embedded_governor_execution_admission_is_atomic() {
    // Two running permits are shared across three resident slots.
    // 三个常驻槽位共享两个运行许可。
    let governor = PoolGovernor::new(config()).unwrap();
    governor
        .register_group("first", policy(PoolKind::Shared, 0))
        .unwrap();
    governor
        .register_group("second", policy(PoolKind::Shared, 0))
        .unwrap();
    // Separate reservations model independently owned VMs.
    // 分离的预留模拟独立拥有的 VM。
    let mut first = governor.reserve("first").unwrap();
    // Another instance in the same group may run up to its declared limit.
    // 同一分组的另一实例可以运行到声明的上限。
    let mut second = governor.reserve("first").unwrap();
    // The other group must not receive a partially acquired permit.
    // 另一分组不得得到部分获取的许可。
    let mut third = governor.reserve("second").unwrap();
    // Initialization itself consumes execution capacity.
    // 初始化本身也消耗执行容量。
    let first_run = first.begin_execution().unwrap();
    // Saturate the parent with the second active initializer.
    // 用第二个活动初始化器占满父级。
    let second_run = second.begin_execution().unwrap();
    assert!(third.begin_execution().is_err());
    assert_eq!(governor.usage(Some("second")).unwrap().running, 0);
    drop(first_run);
    // Returned parent capacity immediately permits the other group to proceed.
    // 归还的父级容量立即允许另一分组推进。
    let third_run = third.begin_execution().unwrap();
    assert_eq!(governor.usage(None).unwrap().running, 2);
    drop((second_run, third_run));
    assert_eq!(governor.usage(None).unwrap().creating, 3);
}

/// Failed group registration must preserve both existing capacity and group identity.
/// 分组注册失败必须保留既有容量及分组身份。
#[test]
fn embedded_governor_rejects_impossible_registration_without_mutation() {
    // Dedicated reservations consume explicit parent budget even while no VM exists.
    // 即使尚无 VM，专用预留也占用显式父级预算。
    let governor = PoolGovernor::new(config()).unwrap();
    governor
        .register_group("first", policy(PoolKind::Dedicated, 2))
        .unwrap();
    assert_eq!(
        governor
            .register_group("second", policy(PoolKind::Dedicated, 2))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::CapacityExceeded
    );
    governor
        .register_group("second", policy(PoolKind::Dedicated, 1))
        .unwrap();
    assert_eq!(
        governor
            .register_group("first", policy(PoolKind::Shared, 0))
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
}

/// Contradictory or unsupported policies fail before any thread or VM is allocated.
/// 相互矛盾或不支持的策略在分配任何线程或 VM 前失败。
#[test]
fn embedded_config_rejects_silent_policy_downgrades() {
    // Preserve one valid configuration as the control for each invalid policy.
    // 保留有效配置，作为每个无效策略的对照。
    let parent = config();
    // A worker backend cannot silently become in-process execution.
    // 工作进程后端不能静默转成进程内执行。
    let mut group = policy(PoolKind::Shared, 0);
    group.backend = ExecutionBackend::WorkerProcess;
    assert_eq!(
        group.validate(&parent).unwrap_err().code,
        EmbeddedErrorCode::Unsupported
    );
    group.backend = ExecutionBackend::InProcess;
    group.serial = true;
    assert!(group.validate(&parent).is_err());
    group.max_running_calls = 1;
    group.validate(&parent).unwrap();
    group.min_resident_vms = 1;
    assert!(group.validate(&parent).is_err());
}

/// Public duration conversion must reject zero and overflow before creating a deadline.
/// 公开时长转换必须在创建截止时间前拒绝零与溢出。
#[test]
fn embedded_control_rejects_invalid_deadlines() {
    assert!(CallControl::new(Duration::ZERO).is_err());
    assert!(CallControl::new(Duration::MAX).is_err());
}

/// Local schema references validate strictly without using network or filesystem resolvers.
/// 本地 Schema 引用严格校验，不使用网络或文件系统解析器。
#[test]
fn embedded_schema_contracts_are_offline_and_validate_local_references() {
    // Bundled definitions are resolved without any external retrieval.
    // 打包的定义无需任何外部检索即可解析。
    let schema = serde_json::json!({"$defs":{"count":{"type":"integer","minimum":1}},"$ref":"#/$defs/count"});
    // One compiled contract is reused across all input checks.
    // 全部输入检查复用一个已编译契约。
    let contract = JsonContract::compile(&schema).unwrap();
    contract.validate(&serde_json::json!(2)).unwrap();
    assert!(contract.validate(&serde_json::json!(0)).is_err());
    assert!(
        contract
            .validate(&serde_json::json!("secret-value"))
            .is_err()
    );
    assert!(
        !contract
            .validate(&serde_json::json!("secret-value"))
            .unwrap_err()
            .message
            .contains("secret-value")
    );
    for reference in [
        "https://example.invalid/schema.json",
        "file:///not-authorized/schema.json",
    ] {
        assert!(JsonContract::compile(&serde_json::json!({"$ref":reference})).is_err());
    }
}

/// Unsupported schema dialects and nonlinear patterns are rejected explicitly.
/// 不支持的 Schema 方言与非线性模式被明确拒绝。
#[test]
fn embedded_schema_rejects_unsupported_contract_semantics() {
    assert!(JsonContract::compile(&serde_json::json!({"$schema":"http://json-schema.org/draft-07/schema#","type":"string"})).is_err());
    assert!(JsonContract::compile(&serde_json::json!({"type":"invalid"})).is_err());
    assert!(
        JsonContract::compile(&serde_json::json!({"type":"string","pattern":"(?=secret)"}))
            .is_err()
    );
}
