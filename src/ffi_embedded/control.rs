use super::commands::RuntimeCommand;
use super::protocol::{PreparedSuccess, respond};
use super::responses;
use super::runtime::RuntimeSlot;
use super::wire::{
    CapacityReceipt, OperationReceipt, PoolReceipt, RegistrationReceipt, SessionReceipt,
};
use super::{EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus};
use crate::runtime::embedded::capabilities::{
    CapabilityExecution, CapabilityPermissions, CapabilityRegistrationRequest, HostRequestBroker,
};
use crate::runtime::embedded::{
    EmbeddedError, EmbeddedErrorCode, EmbeddedResult, EmbeddedRuntime, IdentityKind,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Execute `command` using the exact `slot`; prove success response capacity before every mutation.
/// 使用精确 `slot` 执行 `command`；在每次变更前证明成功响应容量。
pub(super) fn execute(
    slot: &Arc<RuntimeSlot>,
    command: RuntimeCommand,
    limit: usize,
) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    let lease = match slot.acquire(command.admits_work()) {
        Ok(lease) => lease,
        Err(error) => return respond::<()>(Err(error), limit),
    };
    let runtime = lease.runtime();
    match command {
        RuntimeCommand::OperationPersistenceFailure { operation_id } => {
            respond::<responses::OperationPersistenceFailure>(
                runtime.persistence_failure(&operation_id),
                limit,
            )
        }
        RuntimeCommand::OperationRetryCheckpoint { operation_id } => {
            mutate::<responses::OperationRetryCheckpoint>(
                &false,
                || runtime.retry_checkpoint(&operation_id),
                limit,
            )
        }
        RuntimeCommand::StorageStatus {} => respond::<responses::StorageStatus>(
            lease.persistence().and_then(|owner| owner.writer.status()),
            limit,
        ),
        RuntimeCommand::StorageRecover {} => mutate::<responses::StorageRecover>(
            &false,
            || lease.persistence()?.journal.recover_storage(),
            limit,
        ),
        RuntimeCommand::StorageWorkerRecover {} => mutate::<responses::StorageWorkerRecover>(
            &false,
            || lease.persistence()?.writer.recover_worker(),
            limit,
        ),
        RuntimeCommand::HistoryGet {
            history_runtime_id,
            operation_id,
        } => respond::<responses::HistoryGet>(
            lease
                .persistence()
                .and_then(|owner| owner.journal.get(&history_runtime_id, &operation_id)),
            limit,
        ),
        RuntimeCommand::HistoryNext { after } => respond::<responses::HistoryNext>(
            lease.persistence().and_then(|owner| {
                owner.journal.next(
                    after
                        .as_ref()
                        .map(|cursor| (cursor.runtime_id.as_str(), cursor.operation_id.as_str())),
                )
            }),
            limit,
        ),
        RuntimeCommand::HistoryReconcile {
            history_runtime_id,
            operation_id,
            expected_revision,
            resolution,
        } => mutate::<responses::HistoryReconcile>(
            &u64::MAX,
            || {
                require_history_released(runtime, &history_runtime_id, &operation_id)?;
                lease.persistence()?.journal.reconcile(
                    &history_runtime_id,
                    &operation_id,
                    expected_revision,
                    &resolution,
                )
            },
            limit,
        ),
        RuntimeCommand::HistoryForget {
            history_runtime_id,
            operation_id,
            expected_revision,
        } => mutate::<responses::HistoryForget>(
            &(),
            || {
                require_history_released(runtime, &history_runtime_id, &operation_id)?;
                lease.persistence()?.journal.forget(
                    &history_runtime_id,
                    &operation_id,
                    expected_revision,
                )
            },
            limit,
        ),
        RuntimeCommand::PluginRegister { plugin_id, config } => {
            mutate::<responses::PluginRegister>(
                &(),
                || runtime.register_plugin(plugin_id, config),
                limit,
            )
        }
        RuntimeCommand::PluginStatus { plugin_id } => {
            respond::<responses::PluginStatus>(runtime.plugin(&plugin_id), limit)
        }
        RuntimeCommand::PluginClose { plugin_id } => {
            mutate::<responses::PluginClose>(&(), || runtime.close_plugin(&plugin_id), limit)
        }
        RuntimeCommand::PluginForget { plugin_id } => {
            mutate::<responses::PluginForget>(&(), || runtime.forget_plugin(&plugin_id), limit)
        }
        RuntimeCommand::CapacityRegister { plugin_id, config } => {
            // Reserve the longest core-issued identity before any physical guarantee is published.
            // 发布任何物理保证前，预留最长核心签发身份。
            let sample = CapacityReceipt {
                capacity_id: IdentityKind::Capacity.longest(runtime.id()),
            };
            mutate::<responses::CapacityRegister>(
                &sample,
                || {
                    runtime
                        .register_capacity(&plugin_id, config)
                        .map(|capacity_id| CapacityReceipt { capacity_id })
                },
                limit,
            )
        }
        RuntimeCommand::CapacityStatus { capacity_id } => {
            respond::<responses::CapacityStatus>(runtime.capacity(&capacity_id), limit)
        }
        RuntimeCommand::CapacityPolicy { capacity_id } => {
            respond::<responses::CapacityPolicy>(runtime.capacity_policy(&capacity_id), limit)
        }
        RuntimeCommand::CapacityRevise {
            capacity_id,
            expected_revision,
            config,
        } => {
            // The native scheduler renders a u64 sequence as an opaque decimal string.
            // 原生调度器将 u64 序号呈现为不透明十进制字符串。
            // Reserve the longest such success before any policy or cache ownership can change.
            // 在任何策略或缓存归属可能变化前预留此类最长成功响应。
            mutate::<responses::CapacityRevise>(
                &u64::MAX.to_string(),
                || runtime.revise_capacity(&capacity_id, &expected_revision, config),
                limit,
            )
        }
        RuntimeCommand::CapacityClose { capacity_id } => {
            mutate::<responses::CapacityClose>(&(), || runtime.close_capacity(&capacity_id), limit)
        }
        RuntimeCommand::CapacityForget { capacity_id } => mutate::<responses::CapacityForget>(
            &(),
            || runtime.forget_capacity(&capacity_id),
            limit,
        ),
        RuntimeCommand::PoolRegister {
            capacity_id,
            definition,
            policy,
            permissions,
            execution_revision,
        } => {
            let sample = PoolReceipt {
                pool_id: IdentityKind::Pool.longest(runtime.id()),
            };
            mutate::<responses::PoolRegister>(
                &sample,
                || {
                    let grants = CapabilityPermissions::new(permissions)?;
                    // Explicit optional placement preserves legacy independence without lookup fallbacks.
                    // 显式可选归属保留旧独立行为，不采用查找回退。
                    let registered = match capacity_id {
                        Some(capacity_id) => runtime.register_pool_in_capacity(
                            &capacity_id,
                            *definition,
                            policy,
                            grants,
                            execution_revision,
                        ),
                        None => {
                            runtime.register_pool(*definition, policy, grants, execution_revision)
                        }
                    };
                    registered.map(|pool_id| PoolReceipt { pool_id })
                },
                limit,
            )
        }
        RuntimeCommand::PoolStatus { pool_id } => {
            respond::<responses::PoolStatus>(runtime.pool_resources(&pool_id), limit)
        }
        RuntimeCommand::PoolClose { pool_id } => {
            mutate::<responses::PoolClose>(&(), || runtime.close_pool(&pool_id), limit)
        }
        RuntimeCommand::PoolForget { pool_id } => {
            mutate::<responses::PoolForget>(&(), || runtime.forget_pool(&pool_id), limit)
        }
        RuntimeCommand::PoolRevokePermission {
            pool_id,
            permission,
        } => mutate::<responses::PoolRevokePermission>(
            &false,
            || runtime.revoke_pool_permission(&pool_id, &permission),
            limit,
        ),
        RuntimeCommand::CallSubmit { call, timeout_ms } => {
            let sample = OperationReceipt {
                operation_id: IdentityKind::Operation.longest(runtime.id()),
            };
            mutate::<responses::CallSubmit>(
                &sample,
                || {
                    runtime
                        .submit(*call, Duration::from_millis(timeout_ms))
                        .map(|operation| OperationReceipt {
                            operation_id: operation.id().to_owned(),
                        })
                },
                limit,
            )
        }
        RuntimeCommand::SessionOpen {
            pool_id,
            timeout_ms,
        } => {
            let sample = SessionReceipt {
                session_id: IdentityKind::Session.longest(runtime.id()),
                operation_id: IdentityKind::Operation.longest(runtime.id()),
            };
            mutate::<responses::SessionOpen>(
                &sample,
                || {
                    runtime
                        .open_session(&pool_id, Duration::from_millis(timeout_ms))
                        .map(|opening| SessionReceipt {
                            session_id: opening.session_id,
                            operation_id: opening.operation.id().to_owned(),
                        })
                },
                limit,
            )
        }
        RuntimeCommand::SessionSubmit {
            session_id,
            export,
            arguments,
            context,
            timeout_ms,
        } => {
            let sample = OperationReceipt {
                operation_id: IdentityKind::Operation.longest(runtime.id()),
            };
            mutate::<responses::SessionSubmit>(
                &sample,
                || {
                    runtime
                        .submit_session(
                            &session_id,
                            export,
                            arguments,
                            *context,
                            Duration::from_millis(timeout_ms),
                        )
                        .map(|operation| OperationReceipt {
                            operation_id: operation.id().to_owned(),
                        })
                },
                limit,
            )
        }
        RuntimeCommand::SessionStatus { session_id } => {
            respond::<responses::SessionStatus>(runtime.session(&session_id), limit)
        }
        RuntimeCommand::SessionClose { session_id } => {
            mutate::<responses::SessionClose>(&(), || runtime.close_session(&session_id), limit)
        }
        RuntimeCommand::SessionForget { session_id } => {
            mutate::<responses::SessionForget>(&(), || runtime.forget_session(&session_id), limit)
        }
        RuntimeCommand::OperationList {
            pool_id,
            after_operation_id,
            limit: page_limit,
        } => respond::<responses::OperationList>(
            runtime.list_operations(
                pool_id.as_deref(),
                after_operation_id.as_deref(),
                page_limit,
            ),
            limit,
        ),
        RuntimeCommand::OperationStatus { operation_id } => respond::<responses::OperationStatus>(
            runtime
                .operation(&operation_id)
                .and_then(|operation| operation.snapshot()),
            limit,
        ),
        RuntimeCommand::OperationWait {
            operation_id,
            wait_ms,
        } => respond::<responses::OperationWait>(
            runtime
                .operation(&operation_id)
                .and_then(|operation| operation.wait(Duration::from_millis(wait_ms))),
            limit,
        ),
        RuntimeCommand::OperationCancel { operation_id } => mutate::<responses::OperationCancel>(
            &false,
            || {
                runtime
                    .operation(&operation_id)
                    .and_then(|operation| operation.cancel())
            },
            limit,
        ),
        RuntimeCommand::OperationForget { operation_id } => mutate::<responses::OperationForget>(
            &(),
            || runtime.forget_operation(&operation_id),
            limit,
        ),
        RuntimeCommand::CapabilitiesRegister { descriptors } => {
            // Native closures cannot be reconstructed from JSON; never downgrade a declaration silently.
            // 无法从 JSON 重建原生闭包；绝不静默降级声明。
            if descriptors
                .iter()
                .any(|descriptor| descriptor.execution != CapabilityExecution::Queued)
            {
                return respond::<()>(
                    Err(EmbeddedError::new(
                        EmbeddedErrorCode::Unsupported,
                        "FFI capabilities require explicitly queued execution",
                    )),
                    limit,
                );
            }
            let sample = RegistrationReceipt {
                registration_ids: vec![
                    IdentityKind::Capability.longest(runtime.id());
                    descriptors.len()
                ],
            };
            mutate::<responses::CapabilitiesRegister>(
                &sample,
                || {
                    runtime
                        .capabilities()
                        .register(
                            descriptors
                                .into_iter()
                                .map(|descriptor| CapabilityRegistrationRequest {
                                    descriptor,
                                    native: None,
                                })
                                .collect(),
                        )
                        .map(|registration_ids| RegistrationReceipt { registration_ids })
                },
                limit,
            )
        }
        RuntimeCommand::CapabilitiesList { permissions } => respond::<responses::CapabilitiesList>(
            CapabilityPermissions::new(permissions)
                .and_then(|grants| runtime.capabilities().snapshot()?.list(&grants)),
            limit,
        ),
        RuntimeCommand::CapabilityStatus { registration_id } => {
            respond::<responses::CapabilityStatus>(
                runtime.capabilities().status(&registration_id),
                limit,
            )
        }
        RuntimeCommand::CapabilityUnregister { registration_id } => {
            mutate::<responses::CapabilityUnregister>(
                &(),
                || {
                    runtime
                        .capabilities()
                        .unregister(&registration_id)
                        .map(|_| ())
                },
                limit,
            )
        }
        RuntimeCommand::CapabilityForget { registration_id } => {
            mutate::<responses::CapabilityForget>(
                &(),
                || runtime.capabilities().forget(&registration_id),
                limit,
            )
        }
        RuntimeCommand::HostRequestsTake { limit: count } => {
            take_host_requests(&runtime.capabilities().host_requests(), count, limit)
        }
        RuntimeCommand::HostRequestStatus { request_id } => {
            respond::<responses::HostRequestStatus>(
                runtime.capabilities().host_requests().status(&request_id),
                limit,
            )
        }
        RuntimeCommand::HostRequestComplete {
            request_id,
            outcome,
        } => mutate::<responses::HostRequestComplete>(
            &(),
            || {
                runtime
                    .capabilities()
                    .host_requests()
                    .complete(&request_id, outcome.into_outcome()?)
            },
            limit,
        ),
    }
}

/// Reject mutation of `operation_id` history in `history_runtime_id` while this runtime retains its owner.
/// 此运行时仍保留所有者时，拒绝变更 `history_runtime_id` 内的 `operation_id` 历史。
/// Return permission only for absent live identity; external stopped-owner evidence remains the trusted host's duty.
/// 仅在活动身份缺失时返回许可；外部所有者停止证据仍由可信宿主负责。
fn require_history_released(
    runtime: &EmbeddedRuntime,
    history_runtime_id: &str,
    operation_id: &str,
) -> EmbeddedResult<()> {
    if history_runtime_id == runtime.id() {
        match runtime.operation(operation_id) {
            Ok(_) => {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "forget the retained runtime operation before reconciling or removing its history",
                ));
            }
            Err(error) if error.code == EmbeddedErrorCode::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Retain a success allocation for `sample`, run `action`, and encode its actual result without reallocating success storage.
/// 为 `sample` 保留成功分配，运行 `action`，编码实际结果时不重新分配成功存储。
fn mutate<T: Serialize>(
    sample: &impl Serialize,
    action: impl FnOnce() -> EmbeddedResult<T>,
    limit: usize,
) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    let prepared = PreparedSuccess::new(sample, limit)?;
    match action() {
        Ok(result) => prepared.finish(&result),
        Err(error) => respond::<()>(Err(error), limit),
    }
}

/// Pre-admit the complete response frame before broker dispatch; append only already-encoded bounded JSON afterward.
/// 在代理分发前预先接纳完整响应帧；之后仅追加已编码的有界 JSON。
fn take_host_requests(
    broker: &HostRequestBroker,
    count: usize,
    limit: usize,
) -> Result<Vec<u8>, EmbeddedFfiStatus> {
    let prefix = format!(
        "{{\"protocol_version\":{EMBEDDED_FFI_PROTOCOL_VERSION},\"status\":\"ok\",\"result\":"
    );
    let body_limit = limit
        .checked_sub(prefix.len() + 1)
        .filter(|available| *available >= 2)
        .ok_or(EmbeddedFfiStatus::CapacityExceeded)?;
    let mut response = Vec::new();
    // No fallible output allocation may remain after a callback acquires execution authority.
    // 回调取得执行权后，不能仍有可能失败的输出分配。
    response
        .try_reserve_exact(limit)
        .map_err(|_| EmbeddedFfiStatus::CapacityExceeded)?;
    let batch = match broker.take_json(count, body_limit) {
        Ok(batch) => batch,
        Err(error) => return respond::<Value>(Err(error), limit),
    };
    response.extend_from_slice(prefix.as_bytes());
    response.extend_from_slice(&batch);
    response.push(b'}');
    Ok(response)
}
