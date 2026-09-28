//! One response type authority used by both native dispatch and offline schema generation.
//! 原生分发及离线 Schema 生成共同使用的唯一响应类型权威。

use super::wire::{
    CapacityReceipt, OperationReceipt, PoolReceipt, RegistrationReceipt, SessionReceipt,
};
use crate::runtime::embedded::capabilities::{CapabilityDescriptor, CapabilityRegistrationStatus};
use crate::runtime::embedded::{
    EmbeddedCapacitySnapshot, EmbeddedPluginSnapshot, EmbeddedSessionSnapshot, OperationSnapshot,
    PoolUsage,
};

/// Successful `plugin_register` result, enforced by the native dispatcher before serialization.
/// `plugin_register` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PluginRegister = ();

/// Exact failed checkpoint observation; absence means no currently retained fault.
/// 精确失败检查点观测；缺失表示当前没有保留故障。
pub(super) type OperationPersistenceFailure =
    Option<crate::runtime::embedded::OperationPersistenceFailure>;
/// Whether one original checkpoint retry was newly requested.
/// 是否新请求了单次原检查点重试。
pub(super) type OperationRetryCheckpoint = bool;
/// Actual storage-thread and receipt ownership.
/// 实际存储线程及回执所有权。
pub(super) type StorageStatus = crate::runtime::embedded::OperationJournalWorkerStatus;
/// Whether failed storage was explicitly reopened and validated.
/// 是否显式重新打开并校验了失败存储。
pub(super) type StorageRecover = bool;
/// Whether one supervised failed writer thread was explicitly reconstructed.
/// 是否显式重建了一个受监督失败写入线程。
pub(super) type StorageWorkerRecover = bool;
/// One exact original history record, or explicit absence with no execution inference.
/// 单个精确原历史记录，或不推断执行状态的明确缺失。
pub(super) type HistoryGet = Option<crate::runtime::embedded::JournalOperation>;
/// One record after the original cursor, or explicit end of enumeration.
/// 原游标后的一条记录，或明确枚举结束。
pub(super) type HistoryNext = Option<crate::runtime::embedded::JournalOperation>;
/// Durable successor revision of the final trusted-host reconciliation.
/// 最终可信宿主对账的持久后继修订。
pub(super) type HistoryReconcile = u64;
/// Acknowledgement of exact historical removal.
/// 精确历史删除的确认。
pub(super) type HistoryForget = ();

/// Successful `plugin_status` result, enforced by the native dispatcher before serialization.
/// `plugin_status` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PluginStatus = EmbeddedPluginSnapshot;

/// Successful `plugin_close` result, enforced by the native dispatcher before serialization.
/// `plugin_close` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PluginClose = ();

/// Successful `plugin_forget` result, enforced by the native dispatcher before serialization.
/// `plugin_forget` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PluginForget = ();

/// Successful capacity registration returns the exact retained identity.
/// 成功容量注册返回精确保留身份。
pub(super) type CapacityRegister = CapacityReceipt;

/// Capacity status uses the same authoritative snapshot as the formal Rust scheduler.
/// 容量状态使用与正式 Rust 调度器相同的权威快照。
pub(super) type CapacityStatus = EmbeddedCapacitySnapshot;

/// Atomic native revision, complete policy and actual convergence state.
/// 原子原生修订、完整策略及实际收敛状态。
pub(super) type CapacityPolicy = crate::runtime::embedded::EmbeddedCapacityPolicySnapshot;

/// Opaque committed revision echoed without numeric conversion.
/// 原样回传且不经数值转换的已提交不透明修订。
pub(super) type CapacityRevise = String;

/// Capacity close acknowledges admission closure without promising physical completion.
/// 容量关闭确认入场已关闭，不承诺物理完成。
pub(super) type CapacityClose = ();

/// Capacity forget acknowledges actual removal after all members have been forgotten.
/// 容量遗忘确认全部成员遗忘后的实际移除。
pub(super) type CapacityForget = ();

/// Successful `pool_register` result, enforced by the native dispatcher before serialization.
/// `pool_register` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PoolRegister = PoolReceipt;

/// Successful `pool_status` result, enforced by the native dispatcher before serialization.
/// `pool_status` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PoolStatus = PoolUsage;

/// Successful `pool_close` result, enforced by the native dispatcher before serialization.
/// `pool_close` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PoolClose = ();

/// Successful `pool_forget` result, enforced by the native dispatcher before serialization.
/// `pool_forget` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PoolForget = ();

/// Successful `pool_revoke_permission` result, enforced by the native dispatcher before serialization.
/// `pool_revoke_permission` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type PoolRevokePermission = bool;

/// Successful `call_submit` result, enforced by the native dispatcher before serialization.
/// `call_submit` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type CallSubmit = OperationReceipt;

/// Successful `session_open` result, enforced by the native dispatcher before serialization.
/// `session_open` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type SessionOpen = SessionReceipt;

/// Successful `session_submit` result, enforced by the native dispatcher before serialization.
/// `session_submit` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type SessionSubmit = OperationReceipt;

/// Successful `session_status` result, enforced by the native dispatcher before serialization.
/// `session_status` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type SessionStatus = EmbeddedSessionSnapshot;

/// Successful `session_close` result, enforced by the native dispatcher before serialization.
/// `session_close` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type SessionClose = ();

/// Successful `session_forget` result, enforced by the native dispatcher before serialization.
/// `session_forget` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type SessionForget = ();

/// Successful `operation_status` result, enforced by the native dispatcher before serialization.
/// `operation_status` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type OperationStatus = OperationSnapshot;

/// Bounded retained-operation discovery shares the native registry's authoritative page type.
/// 有界保留操作发现共享原生注册表的权威分页类型。
pub(super) type OperationList = crate::runtime::embedded::OperationPage;

/// Successful `operation_wait` result, enforced by the native dispatcher before serialization.
/// `operation_wait` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type OperationWait = OperationSnapshot;

/// Successful `operation_cancel` result, enforced by the native dispatcher before serialization.
/// `operation_cancel` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type OperationCancel = bool;

/// Successful `operation_forget` result, enforced by the native dispatcher before serialization.
/// `operation_forget` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type OperationForget = ();

/// Successful `capabilities_register` result, enforced by the native dispatcher before serialization.
/// `capabilities_register` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type CapabilitiesRegister = RegistrationReceipt;

/// Successful `capabilities_list` result, enforced by the native dispatcher before serialization.
/// `capabilities_list` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type CapabilitiesList = Vec<CapabilityDescriptor>;

/// Successful `capability_status` result, enforced by the native dispatcher before serialization.
/// `capability_status` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type CapabilityStatus = CapabilityRegistrationStatus;

/// Successful `capability_unregister` result, enforced by the native dispatcher before serialization.
/// `capability_unregister` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type CapabilityUnregister = ();

/// Successful `capability_forget` result, enforced by the native dispatcher before serialization.
/// `capability_forget` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type CapabilityForget = ();

/// Successful `host_request_status` result, enforced by the native dispatcher before serialization.
/// `host_request_status` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type HostRequestStatus = crate::runtime::embedded::capabilities::HostRequestStatus;

/// Successful `host_request_complete` result, enforced by the native dispatcher before serialization.
/// `host_request_complete` 成功结果，在序列化前由原生分发器强制校验。
pub(super) type HostRequestComplete = ();

/// Generate every runtime response schema from the same aliases used by actual dispatch.
/// 生成实际分发所用相同别名的全部运行时响应 Schema。
/// Return a command-keyed map; host batch frames use the broker's actual HostRequest serialization.
/// 返回命令索引映射；宿主批次帧使用代理实际 HostRequest 序列化。
#[cfg(feature = "contract-generation")]
pub(super) fn schemas() -> serde_json::Value {
    // An ordinary map avoids macro recursion growth when new native commands are added.
    // 普通映射避免新增原生命令时宏递归深度增长。
    serde_json::Value::Object([
        ("operation_persistence_failure", super::contract::response::<OperationPersistenceFailure>()),
        ("operation_retry_checkpoint", super::contract::response::<OperationRetryCheckpoint>()),
        ("storage_status", super::contract::response::<StorageStatus>()),
        ("storage_recover", super::contract::response::<StorageRecover>()),
        ("storage_worker_recover", super::contract::response::<StorageWorkerRecover>()),
        ("history_get", super::contract::response::<HistoryGet>()),
        ("history_next", super::contract::response::<HistoryNext>()),
        ("history_reconcile", super::contract::response::<HistoryReconcile>()),
        ("history_forget", super::contract::response::<HistoryForget>()),
        ("plugin_register", super::contract::response::<PluginRegister>()),
        ("plugin_status", super::contract::response::<PluginStatus>()),
        ("plugin_close", super::contract::response::<PluginClose>()),
        ("plugin_forget", super::contract::response::<PluginForget>()),
        ("capacity_register", super::contract::response::<CapacityRegister>()),
        ("capacity_status", super::contract::response::<CapacityStatus>()),
        ("capacity_policy", super::contract::response::<CapacityPolicy>()),
        ("capacity_revise", super::contract::response::<CapacityRevise>()),
        ("capacity_close", super::contract::response::<CapacityClose>()),
        ("capacity_forget", super::contract::response::<CapacityForget>()),
        ("pool_register", super::contract::response::<PoolRegister>()),
        ("pool_status", super::contract::response::<PoolStatus>()),
        ("pool_close", super::contract::response::<PoolClose>()),
        ("pool_forget", super::contract::response::<PoolForget>()),
        ("pool_revoke_permission", super::contract::response::<PoolRevokePermission>()),
        ("call_submit", super::contract::response::<CallSubmit>()),
        ("session_open", super::contract::response::<SessionOpen>()),
        ("session_submit", super::contract::response::<SessionSubmit>()),
        ("session_status", super::contract::response::<SessionStatus>()),
        ("session_close", super::contract::response::<SessionClose>()),
        ("session_forget", super::contract::response::<SessionForget>()),
        ("operation_status", super::contract::response::<OperationStatus>()),
        ("operation_list", super::contract::response::<OperationList>()),
        ("operation_wait", super::contract::response::<OperationWait>()),
        ("operation_cancel", super::contract::response::<OperationCancel>()),
        ("operation_forget", super::contract::response::<OperationForget>()),
        ("capabilities_register", super::contract::response::<CapabilitiesRegister>()),
        ("capabilities_list", super::contract::response::<CapabilitiesList>()),
        ("capability_status", super::contract::response::<CapabilityStatus>()),
        ("capability_unregister", super::contract::response::<CapabilityUnregister>()),
        ("capability_forget", super::contract::response::<CapabilityForget>()),
        ("host_request_status", super::contract::response::<HostRequestStatus>()),
        ("host_request_complete", super::contract::response::<HostRequestComplete>()),
        ("host_requests_take", super::contract::response::<Vec<crate::runtime::embedded::capabilities::HostRequest>>()),
    ].into_iter().map(|(command, schema)| (command.to_owned(), schema)).collect())
}
