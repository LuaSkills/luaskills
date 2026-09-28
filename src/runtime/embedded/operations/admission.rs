//! Atomic operation identity and trusted context admission.
//! 原子操作身份与可信上下文入场。

use super::*;
use crate::runtime::embedded::{IdentityKind, ModulePool};

impl OperationRegistry {
    /// Atomically convert optional reserved capacity or admit ordinary work with the supplied context factory.
    /// 使用给定上下文构造器，原子转换可选预留容量或接纳普通任务。
    /// Failed context validation or control binding leaves any reservation owned and publishes nothing.
    /// 上下文校验或控制绑定失败时继续拥有预留，且不发布任何记录。
    pub(super) fn admit_with_reservation(
        &self,
        control: Arc<CallControl>,
        reservation: Option<&mut OperationReservation>,
        make_context: impl FnOnce(&str) -> EmbeddedResult<OperationContext>,
    ) -> EmbeddedResult<(OperationHandle, OperationOwner)> {
        // Reserved lifecycle admission starts no business execution; its separate finalization control starts later.
        // 预留生命周期入场不启动业务执行；其独立关闭控制稍后才启动。
        if reservation.is_none() {
            control.check()?;
        }
        // Allocate identity and retention capacity in the same transaction.
        // 在同一次事务中分配身份与保留容量。
        let mut state = self.lock()?;
        if let Some(owned) = reservation.as_ref() {
            if !owned.active || !Arc::ptr_eq(&owned.state, &self.state) {
                return Err(EmbeddedError::invalid(
                    "operation reservation belongs to another registry or was consumed",
                ));
            }
        } else if state.records.len() >= self.max_operations - state.reserved {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "operation retention capacity reached",
            ));
        }
        // Checked counters never wrap into a previously used public identity.
        // 受检计数器绝不回绕到之前使用过的公开身份。
        let sequence = if reservation.is_some() {
            state.sequence
        } else {
            state.sequence.checked_add(1).ok_or_else(|| {
                EmbeddedError::new(EmbeddedErrorCode::Internal, "operation identity exhausted")
            })?
        };
        // Opaque strings preserve the full identity in every supported SDK.
        // 不透明字符串在所有受支持 SDK 中保留完整身份。
        let id = match reservation.as_ref() {
            Some(owned) => owned.id.clone(),
            None => IdentityKind::Operation.render(&self.runtime_id, sequence),
        };
        // Derive module authority from immutable host state before publishing this operation or its control binding.
        // 发布此操作或其控制绑定前，从不可变宿主状态派生模块权威。
        let context = match reservation.as_ref() {
            Some(owned) => owned.context.clone(),
            None => make_context(&id)?,
        };
        context.validate(&self.runtime_id, &id)?;
        // Bound dynamic module metadata with the same per-operation budget as its eventual host effects.
        // 使用与后续宿主副作用相同的逐操作预算限制动态模块元数据。
        let context_bytes = match &context {
            OperationContext::Unbound => 0,
            OperationContext::Module(_) => json_size(&context, self.max_effect_bytes)?,
        };
        // The ledger checks future callbacks against this exact admission-time identity.
        // 账本对照此精确入场身份检查未来回调。
        let caller = context.caller().cloned();
        // Construct the immutable persistence binding before exposing the ledger through shared control.
        // 在通过共享控制对象暴露账本之前构造不可变持久绑定。
        let operation = Arc::new_cyclic(|operation| Operation {
            transition: Mutex::new(None),
            history: self.journal.as_ref().map(|journal| OperationHistory {
                backend: journal.clone(),
                runtime_id: self.runtime_id.clone(),
                revision: Mutex::new(None),
            }),
            id: id.clone(),
            effects: EffectLedger::new(
                self.runtime_id.clone(),
                id.clone(),
                self.max_effect_records,
                self.max_effect_bytes,
                self.journal.as_ref().map(|_| operation.clone()),
                caller,
                context_bytes,
            ),
            control: Arc::clone(&control),
            snapshot: Mutex::new(OperationSnapshot {
                finalization: None,
                context,
                host_effects: Vec::new(),
                operation_id: id.clone(),
                phase: OperationPhase::Queued,
                cancellation_requested: false,
                effects: EffectState::NotStarted,
                value: None,
                error: None,
            }),
            changed: Condvar::new(),
            max_value_bytes: self.max_value_bytes,
        });
        // The original control belongs to one operation; failed attachment publishes no registry identity.
        // 原始控制对象只归属于一个操作；绑定失败不发布注册表身份。
        control.attach_effects(
            Arc::clone(&operation.effects),
            super::super::effects::EffectAdmissionStage::Business,
        )?;
        if let Some(owned) = reservation {
            state.reserved -= 1;
            owned.active = false;
        }
        state.sequence = sequence;
        state.records.insert(id, Arc::clone(&operation));
        Ok((
            OperationHandle {
                operation: Arc::clone(&operation),
            },
            OperationOwner {
                finalization: None,
                operation,
                pending_completion: None,
                completion_checkpoint: None,
            },
        ))
    }
}

impl OperationRegistry {
    /// Admit a formal module operation from the exact pool, optional session and export under the original control.
    /// 在原始控制下，从精确池、可选会话及导出接纳正式模块操作。
    /// Return the same handle/owner pair as low-level admission; no Lua or host callback runs in this transaction.
    /// 返回与低层入场相同的句柄／所有者对；此事务不运行 Lua 或宿主回调。
    pub(in crate::runtime::embedded) fn admit_module(
        &self,
        control: Arc<CallControl>,
        pool: &ModulePool,
        session_id: Option<&str>,
        export: Option<&str>,
    ) -> EmbeddedResult<(OperationHandle, OperationOwner)> {
        self.admit_context(control, |id| pool.operation_context(id, session_id, export))
    }

    /// Allocate one identity and derive its context using a private metadata-only factory before control publication.
    /// 分配单个身份，并在控制发布前使用私有纯元数据构造器派生其上下文。
    /// Return owned observation/execution handles or a rejection that publishes no identity or context.
    /// 返回自有观察／执行句柄，或不发布任何身份与上下文的拒绝。
    pub(super) fn admit_context(
        &self,
        control: Arc<CallControl>,
        make_context: impl FnOnce(&str) -> EmbeddedResult<OperationContext>,
    ) -> EmbeddedResult<(OperationHandle, OperationOwner)> {
        self.admit_with_reservation(control, None, make_context)
    }
}
