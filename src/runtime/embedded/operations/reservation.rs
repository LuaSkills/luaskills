//! Bounded lifecycle admission reserved before a long-lived VM can create effects.
//! 在长生命周期 VM 能够产生副作用前预留有界生命周期入场。

use super::*;
use crate::runtime::embedded::{IdentityKind, ModulePool};

/// A noncloneable slot owned by exactly one registry; dropping an unused slot only releases metadata.
/// 精确归属一个注册表的不可克隆槽；丢弃未使用槽仅释放元数据。
pub(in crate::runtime::embedded) struct OperationReservation {
    /// Internally allocated identity; it is not queryable until the reservation becomes an operation.
    /// 内部分配的身份；预留转为操作之前不可查询。
    pub(super) id: String,
    /// Validated immutable module authority captured before the session can initialize.
    /// 在会话可以初始化前捕获的已校验不可变模块权威。
    pub(super) context: OperationContext,
    /// Exact original registry identity, retained even if its public owner is dropped.
    /// 精确原注册表身份，即使其公开所有者丢弃也继续保留。
    pub(super) state: Arc<Mutex<OperationRegistryState>>,
    /// True until successful conversion into one retained operation.
    /// 成功转换为一个保留操作之前为真。
    pub(super) active: bool,
}

impl Drop for OperationReservation {
    /// Release unused capacity without callbacks, disk access or public operation publication.
    /// 释放未使用容量，不调用回调、不访问磁盘，也不发布公开操作。
    fn drop(&mut self) {
        if self.active {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reserved -= 1;
        }
    }
}

impl OperationRegistry {
    /// Allocate a capacity slot and validate its immutable context before publication or execution.
    /// 在发布或执行前分配容量槽，并校验其不可变上下文。
    /// The metadata-only factory receives the exact reserved identity; any rejection keeps capacity unchanged.
    /// 纯元数据构造器接收精确预留身份；任何拒绝都保持容量不变。
    fn reserve_context(
        &self,
        make_context: impl FnOnce(&str) -> EmbeddedResult<OperationContext>,
    ) -> EmbeddedResult<OperationReservation> {
        let mut state = self.lock()?;
        if state.records.len() >= self.max_operations - state.reserved {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "operation retention capacity reached",
            ));
        }
        let sequence = state.sequence.checked_add(1).ok_or_else(|| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "operation identity exhausted")
        })?;
        let id = IdentityKind::Operation.render(&self.runtime_id, sequence);
        let context = make_context(&id)?;
        context.validate(&self.runtime_id, &id)?;
        json_size(&context, self.max_effect_bytes)?;
        state.sequence = sequence;
        state.reserved += 1;
        Ok(OperationReservation {
            id,
            context,
            state: Arc::clone(&self.state),
            active: true,
        })
    }

    /// Reserve one future operation under the same limit as retained records; no deadline starts here.
    /// 在与保留记录相同的上限下预留一个未来操作；此处不启动截止时间。
    /// Return unique ownership or capacity exhaustion before any plugin execution.
    /// 返回唯一所有权，或在任何插件执行前报告容量耗尽。
    pub(in crate::runtime::embedded) fn reserve_module(
        &self,
        pool: &ModulePool,
        session_id: &str,
        export: &str,
    ) -> EmbeddedResult<OperationReservation> {
        self.reserve_context(|id| pool.operation_context(id, Some(session_id), Some(export)))
    }

    /// Convert this registry's reservation using fresh control and immutable module context.
    /// 使用新控制及不可变模块上下文转换此注册表的预留。
    /// On rejection retain the reservation; on success return one queryable operation and its sole owner.
    /// 拒绝时保留预留；成功时返回一个可查询操作及其唯一所有者。
    pub(in crate::runtime::embedded) fn admit_reserved(
        &self,
        reservation: &mut OperationReservation,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<(OperationHandle, OperationOwner)> {
        self.admit_with_reservation(control, Some(reservation), |_| {
            Err(EmbeddedError::invalid(
                "reserved admission must use its captured context",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lifecycle reservation admits after its observation control expires; actual cleanup receives a fresh budget.
    /// 生命周期预留在观察控制过期后仍可入场；实际清理获得全新预算。
    #[test]
    fn embedded_operation_reservation_uses_independent_finalization_budget() {
        let registry = OperationRegistry::new(
            "reserved".into(),
            &crate::runtime::embedded::tests::config(),
        )
        .unwrap();
        let mut reservation = registry
            .reserve_context(|_| Ok(OperationContext::Unbound))
            .unwrap();
        let control = Arc::new(CallControl::new(Duration::from_millis(1)).unwrap());
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            control.check().unwrap_err().code,
            EmbeddedErrorCode::DeadlineExceeded
        );
        let (handle, mut owner) = registry.admit_reserved(&mut reservation, control).unwrap();
        owner
            .prepare_finalization("close".into(), Ok(Value::Null), Duration::from_secs(1))
            .unwrap();
        let closing = owner.take_finalization_control().unwrap();
        assert!(closing.check().is_ok());
        owner.prepare_finalization_outcome(Ok(Value::Null)).unwrap();
        owner
            .complete(Ok(Value::Null), EffectState::NotStarted)
            .unwrap();
        assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Succeeded);
        assert!(!reservation.active);
        assert_eq!(registry.lock().unwrap().reserved, 0);
    }

    /// Unused, foreign, consumed and failed-binding reservations cannot leak capacity or publish duplicate identities.
    /// 未使用、外来、已消费及绑定失败的预留都不能泄漏容量或发布重复身份。
    #[test]
    fn embedded_operation_reservation_ownership_and_capacity_are_exact() {
        let mut config = crate::runtime::embedded::tests::config();
        config.max_operations = config.max_running_calls.max(config.max_queued_calls);
        let registry = OperationRegistry::new("reserved".into(), &config).unwrap();
        let foreign = OperationRegistry::new("foreign".into(), &config).unwrap();
        let mut reservations = Vec::new();
        for _ in 0..config.max_operations {
            reservations.push(
                registry
                    .reserve_context(|_| Ok(OperationContext::Unbound))
                    .unwrap(),
            );
        }
        assert_eq!(
            registry
                .admit(Arc::new(CallControl::new(Duration::from_secs(1)).unwrap()))
                .err()
                .unwrap()
                .code,
            EmbeddedErrorCode::CapacityExceeded
        );
        let mut reservation = reservations.pop().unwrap();
        assert!(
            foreign
                .admit_reserved(
                    &mut reservation,
                    Arc::new(CallControl::new(Duration::from_secs(1)).unwrap())
                )
                .is_err()
        );
        assert!(reservation.active);
        let bound = Arc::new(CallControl::new(Duration::from_secs(1)).unwrap());
        let (_, _) = foreign.admit(Arc::clone(&bound)).unwrap();
        assert!(registry.admit_reserved(&mut reservation, bound).is_err());
        assert!(reservation.active);
        let (handle, _) = registry
            .admit_reserved(
                &mut reservation,
                Arc::new(CallControl::new(Duration::from_secs(1)).unwrap()),
            )
            .unwrap();
        assert!(
            registry
                .admit_reserved(
                    &mut reservation,
                    Arc::new(CallControl::new(Duration::from_secs(1)).unwrap())
                )
                .is_err()
        );
        assert_eq!(registry.lock().unwrap().records.len(), 1);
        drop(reservations);
        assert_eq!(registry.lock().unwrap().reserved, 0);
        let (next, _) = registry
            .admit(Arc::new(CallControl::new(Duration::from_secs(1)).unwrap()))
            .unwrap();
        assert_ne!(handle.id(), next.id());
    }
}
