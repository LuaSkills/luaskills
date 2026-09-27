use super::commands::RuntimeCommand;
use super::protocol::{PreparedSuccess, respond};
use super::responses;
use super::runtime::RuntimeSlot;
use super::wire::{OperationReceipt, PoolReceipt, RegistrationReceipt, SessionReceipt};
use super::{EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus};
use crate::runtime::embedded::capabilities::{
    CapabilityExecution, CapabilityPermissions, CapabilityRegistrationRequest, HostRequestBroker,
};
use crate::runtime::embedded::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult, IdentityKind};
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
        RuntimeCommand::PoolRegister {
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
                    runtime
                        .register_pool(*definition, policy, grants, execution_revision)
                        .map(|pool_id| PoolReceipt { pool_id })
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
