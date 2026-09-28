//! One response type authority used by both native dispatch and offline schema generation.
//! 原生分发及离线 Schema 生成共同使用的唯一响应类型权威。

use super::wire::{OperationReceipt, PoolReceipt, RegistrationReceipt, SessionReceipt};
use crate::runtime::embedded::capabilities::{CapabilityDescriptor, CapabilityRegistrationStatus};
use crate::runtime::embedded::{
    EmbeddedPluginSnapshot, EmbeddedSessionSnapshot, OperationSnapshot, PoolUsage,
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
    serde_json::json!({
        "operation_persistence_failure": super::contract::response::<OperationPersistenceFailure>(),
        "operation_retry_checkpoint": super::contract::response::<OperationRetryCheckpoint>(),
        "storage_status": super::contract::response::<StorageStatus>(),
        "storage_recover": super::contract::response::<StorageRecover>(),
        "history_get": super::contract::response::<HistoryGet>(),
        "history_next": super::contract::response::<HistoryNext>(),
        "history_reconcile": super::contract::response::<HistoryReconcile>(),
        "history_forget": super::contract::response::<HistoryForget>(),
        "plugin_register": super::contract::response::<PluginRegister>(),
        "plugin_status": super::contract::response::<PluginStatus>(),
        "plugin_close": super::contract::response::<PluginClose>(),
        "plugin_forget": super::contract::response::<PluginForget>(),
        "pool_register": super::contract::response::<PoolRegister>(),
        "pool_status": super::contract::response::<PoolStatus>(),
        "pool_close": super::contract::response::<PoolClose>(),
        "pool_forget": super::contract::response::<PoolForget>(),
        "pool_revoke_permission": super::contract::response::<PoolRevokePermission>(),
        "call_submit": super::contract::response::<CallSubmit>(),
        "session_open": super::contract::response::<SessionOpen>(),
        "session_submit": super::contract::response::<SessionSubmit>(),
        "session_status": super::contract::response::<SessionStatus>(),
        "session_close": super::contract::response::<SessionClose>(),
        "session_forget": super::contract::response::<SessionForget>(),
        "operation_status": super::contract::response::<OperationStatus>(),
        "operation_wait": super::contract::response::<OperationWait>(),
        "operation_cancel": super::contract::response::<OperationCancel>(),
        "operation_forget": super::contract::response::<OperationForget>(),
        "capabilities_register": super::contract::response::<CapabilitiesRegister>(),
        "capabilities_list": super::contract::response::<CapabilitiesList>(),
        "capability_status": super::contract::response::<CapabilityStatus>(),
        "capability_unregister": super::contract::response::<CapabilityUnregister>(),
        "capability_forget": super::contract::response::<CapabilityForget>(),
        "host_request_status": super::contract::response::<HostRequestStatus>(),
        "host_request_complete": super::contract::response::<HostRequestComplete>(),
        "host_requests_take": super::contract::response::<Vec<crate::runtime::embedded::capabilities::HostRequest>>(),
    })
}
