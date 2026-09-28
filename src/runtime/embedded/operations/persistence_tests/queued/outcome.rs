//! Orphaned SDK confirmation must preserve its cleanup phase and actual admission.
//! 孤立 SDK 确认必须保留其清理阶段及真实入场许可。

use super::intent::{capabilities, queued_call};
use super::*;
use crate::runtime::embedded::HostEffectPhase;
use crate::runtime::embedded::capabilities::*;

/// Confirming an orphan after cleanup starts must not overwrite Cleaning with an older Running snapshot.
/// 清理开始后确认孤立请求不能用较旧执行快照覆盖清理阶段。
#[test]
fn embedded_effect_outcome_orphan_preserves_cleanup_ordering() {
    // The low-level fixture independently controls phase advancement and host completion.
    // 低层夹具独立控制阶段推进及宿主完成。
    let directory = Directory::new();
    // Use one real database and the same revision authority across phase and outcome writes.
    // 阶段及结果写入使用同一真实数据库和相同修订权威。
    let journal = directory.journal(journal_config());
    // The writer remains host-owned while the request waiter is deliberately dropped.
    // 请求等待方被有意丢弃期间，写入者仍由宿主拥有。
    let (writer, registry) = queued_registry(&journal);
    // This request has no native implementation and can only be completed by its SDK consumer.
    // 此请求没有原生实现，只能由其 SDK 消费者完成。
    let capabilities = capabilities(&registry.runtime_id, CapabilityExecution::Queued, None);
    // The operation owner survives its abandoned host-request waiter.
    // 操作所有者在宿主请求等待方被放弃后继续存活。
    let (operation, mut owner) = admit(&registry);
    owner.advance(OperationPhase::Running).unwrap();
    // Bind a stable request identity before actual delivery.
    // 实际交付前绑定稳定请求身份。
    let request = queued_call(&capabilities, &registry, &operation, &owner);
    // Polling this broker also drives original confirmation receipts without implicit retries.
    // 轮询此代理也会推进原始确认回执，不会隐式重试。
    let broker = capabilities.host_requests();
    // Preserve the actual delivered request to simulate the independently running SDK handler.
    // 保留真实已交付请求，模拟独立运行的 SDK 处理器。
    let mut delivered = None;
    poll(|| {
        delivered = broker.take(1)?.pop();
        Ok(delivered.is_some())
    })
    .unwrap();
    // The consumer retains identity after the waiter abandons observation.
    // 等待方放弃观测后，消费者继续保留身份。
    let delivered = delivered.unwrap();
    drop(request);
    owner.advance(OperationPhase::Cleaning).unwrap();
    // Storage is blocked only after the newer cleanup phase has actually acknowledged.
    // 仅在较新清理阶段确实确认后阻塞存储。
    let gate = journal.block_for_test();
    broker
        .complete(
            &delivered.request_id,
            CapabilityOutcome {
                result: Ok(json!("late result")),
                effects: EffectState::Committed,
            },
        )
        .unwrap();
    assert_eq!(
        broker.status(&delivered.request_id).unwrap().phase,
        HostRequestPhase::Completing
    );
    assert!(!broker.is_drained().unwrap());
    assert_eq!(
        owner
            .prepare_completion(Ok(Value::Null), EffectState::NotStarted)
            .unwrap_err()
            .code,
        EmbeddedErrorCode::Busy
    );
    assert_eq!(
        operation.snapshot().unwrap().phase,
        OperationPhase::Cleaning
    );
    drop(gate);
    poll(|| broker.is_drained()).unwrap();
    // Confirmation captures the current cleanup phase instead of replaying the phase observed at dispatch.
    // 确认捕获当前清理阶段，不重放分发时观测到的阶段。
    let stored = journal
        .get(&registry.runtime_id, operation.id())
        .unwrap()
        .unwrap();
    assert_eq!(stored.snapshot.phase, OperationPhase::Cleaning);
    assert!(
        stored
            .snapshot
            .host_effects
            .iter()
            .any(
                |record| record.request_id.as_deref() == Some(delivered.request_id.as_str())
                    && record.effects == EffectState::Committed
            )
    );
    assert!(
        operation
            .snapshot()
            .unwrap()
            .host_effects
            .iter()
            .all(|record| record.phase == HostEffectPhase::Completed)
    );
    owner
        .complete(Ok(Value::Null), EffectState::NotStarted)
        .unwrap();
    assert_eq!(
        operation.snapshot().unwrap().effects,
        EffectState::Committed
    );
    close(&writer);
}
