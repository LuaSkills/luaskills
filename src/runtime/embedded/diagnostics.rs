//! Optional private measurements use the existing logger and exact original identities.
//! 可选私有测量使用既有日志器及精确原身份。
//! Heap values are live samples, not peaks; host waiting overlaps its surrounding Lua stage.
//! 堆值为存活采样而非峰值；宿主等待与外围 Lua 阶段重叠。
//! Synchronous subscribers must not block or reenter; emission is outside measured stage intervals.
//! 同步订阅者不得阻塞或重入；发送位于被测阶段区间之外。

use super::control::HostWaitSnapshot;
use crate::runtime::logging::{DiagnosticSubscriber, diagnostic_is_current, send_diagnostic};
use std::time::{Duration, Instant};

/// Fixed stage observations contain no arguments, paths or host secrets.
/// 固定阶段观测不包含参数、路径或宿主秘密。
#[derive(Default)]
pub(crate) struct PhaseObservation {
    /// Actual monotonic interval, absent when no work was performed.
    /// 实际单调时间区间；未执行工作时省略。
    pub(crate) elapsed: Option<Duration>,
    /// Lua allocator's current live byte count, never a post-destruction zero or peak claim.
    /// Lua 分配器当前存活字节数；绝不冒充销毁后的零值或峰值。
    pub(crate) lua_heap_bytes: Option<usize>,
    /// Exact host-call interval delta nested within this stage.
    /// 此阶段内嵌套的精确宿主调用区间差值。
    pub(crate) host_wait: Option<HostWaitSnapshot>,
    /// Actual returned stage outcome, absent for queue and heap-only observations.
    /// 实际返回的阶段结果；队列及仅堆观测时省略。
    pub(crate) succeeded: Option<bool>,
}

/// One admitted operation's optional subscriber and original queue boundaries.
/// 单个已入场操作的可选订阅者及原队列边界。
pub(crate) struct OperationDiagnostics {
    /// Subscriber captured before entering the scheduler ownership lock.
    /// 在进入调度器所有权锁前捕获的订阅者。
    subscriber: DiagnosticSubscriber,
    /// Exact runtime namespace, never inferred from an instance's display name.
    /// 精确运行时命名空间，绝不从实例显示名推断。
    runtime_id: String,
    /// Exact original immutable execution domain.
    /// 精确原不可变执行域。
    pool_id: String,
    /// Exact admitted operation identity.
    /// 精确已入场操作身份。
    operation_id: String,
    /// Actual insertion boundary captured only for subscribed operations.
    /// 仅订阅操作捕获的实际插入边界。
    enqueued: Instant,
    /// Actual removal interval captured under the existing queue lock, emitted after unlock.
    /// 在既有队列锁下捕获的实际移除区间，解锁后发送。
    dequeued_elapsed: Option<Duration>,
}

impl OperationDiagnostics {
    /// Check current subscriber identity outside all runtime ownership locks before measurement.
    /// 在测量前于全部运行时所有权锁外检查当前订阅者身份。
    pub(crate) fn enabled(&self) -> bool {
        diagnostic_is_current(&self.subscriber)
    }

    /// Capture subscriber and trusted runtime, pool and operation identities at actual enqueue.
    /// 在实际入队时捕获 subscriber 及可信运行时、池和操作身份。
    /// Return fixed private metadata; caller has already captured subscriber outside scheduler locks.
    /// 返回固定私有元数据；调用方已在调度器锁外捕获 subscriber。
    pub(crate) fn new(
        subscriber: DiagnosticSubscriber,
        runtime_id: &str,
        pool_id: &str,
        operation_id: &str,
    ) -> Self {
        Self {
            subscriber,
            runtime_id: runtime_id.to_owned(),
            pool_id: pool_id.to_owned(),
            operation_id: operation_id.to_owned(),
            enqueued: Instant::now(),
            dequeued_elapsed: None,
        }
    }

    /// Record real queue removal without querying or calling the logger under its ownership lock.
    /// 记录真实队列移除，不在其所有权锁下查询或调用日志器。
    pub(crate) fn dequeued(&mut self) {
        self.dequeued_elapsed = Some(self.enqueued.elapsed());
    }

    /// Emit measured queue residence after unlocking; no VM is claimed before allocation.
    /// 解锁后发送被测队列驻留时间；分配前不声称拥有 VM。
    pub(crate) fn emit_queue(&mut self) {
        // Consume the real removal interval once, including pre-execution rejection and cleanup retries.
        // 仅消费一次真实移除区间，覆盖执行前拒绝及清理重试。
        let Some(elapsed) = self.dequeued_elapsed.take() else {
            return;
        };
        self.emit(
            "queue",
            None,
            PhaseObservation {
                elapsed: Some(elapsed),
                ..PhaseObservation::default()
            },
        );
    }

    /// Emit phase and observations for this operation with optional actually allocated instance_id.
    /// 为此操作发送 phase 和 observations，可附实际已分配的 instance_id。
    /// Return unit without changing lifecycle state, including when the subscriber panics.
    /// 返回空值且不改变生命周期状态，包括订阅者 panic 时。
    pub(crate) fn emit(
        &self,
        phase: &str,
        instance_id: Option<&str>,
        observation: PhaseObservation,
    ) {
        send_diagnostic(&self.subscriber, || {
            serde_json::json!({
                "luaskills_embedded_diagnostic": 1,
                "runtime_id": self.runtime_id,
                "pool_id": self.pool_id,
                "operation_id": self.operation_id,
                "instance_id": instance_id,
                "phase": phase,
                "elapsed_ns": observation.elapsed.map(|duration| duration.as_nanos()),
                "lua_heap_bytes": observation.lua_heap_bytes,
                "host_wait": observation.host_wait.map(|wait| serde_json::json!({
                    "elapsed_ns": wait.elapsed_ns,
                    "calls": wait.calls,
                })),
                "succeeded": observation.succeeded,
            })
            .to_string()
        });
    }

    /// Retain only instance metadata for eventual retirement, without associating the last business call.
    /// 仅为最终退役保留实例元数据，不将最后业务调用归属其上。
    /// Return metadata with no VM, pool owner, operation owner or resource reservation references.
    /// 返回不包含 VM、池所有者、操作所有者或资源预留引用的元数据。
    pub(crate) fn instance(&self, instance_id: &str) -> InstanceDiagnostics {
        InstanceDiagnostics {
            subscriber: self.subscriber.clone(),
            runtime_id: self.runtime_id.clone(),
            pool_id: self.pool_id.clone(),
            instance_id: instance_id.to_owned(),
        }
    }
}

/// Instance-only observations remain valid after its VM and all reservations have been destroyed.
/// 实例专属观测在其 VM 及全部预留销毁后仍有效。
pub(crate) struct InstanceDiagnostics {
    /// Existing subscriber, with no strong VM or runtime ownership captured by this metadata.
    /// 既有订阅者；此元数据不捕获强 VM 或运行时所有权。
    subscriber: DiagnosticSubscriber,
    /// Exact runtime owning this instance's original allocation.
    /// 拥有此实例原分配的精确运行时。
    runtime_id: String,
    /// Original immutable allocation domain.
    /// 原不可变分配域。
    pool_id: String,
    /// Actual destroyed instance identity.
    /// 实际销毁的实例身份。
    instance_id: String,
}

impl InstanceDiagnostics {
    /// Return whether the original subscriber still exists before starting retirement measurement.
    /// 在启动退役测量前返回原订阅者是否仍存在。
    pub(crate) fn enabled(&self) -> bool {
        diagnostic_is_current(&self.subscriber)
    }

    /// Emit actual close and destruction intervals after the Completed receipt is published and locks released.
    /// 在 Completed 回执发布且锁释放后发送实际关闭及销毁区间。
    /// heap_before_close is a live sample; no heap value is fabricated after destruction.
    /// heap_before_close 为存活采样；销毁后不虚构堆值。
    pub(crate) fn retired(&self, close: Duration, destruction: Duration, heap_before_close: usize) {
        send_diagnostic(&self.subscriber, || {
            serde_json::json!({
                "luaskills_embedded_diagnostic": 1,
                "runtime_id": self.runtime_id,
                "pool_id": self.pool_id,
                "operation_id": null,
                "instance_id": self.instance_id,
                "phase": "retired",
                "close_elapsed_ns": close.as_nanos(),
                "destruction_elapsed_ns": destruction.as_nanos(),
                "lua_heap_before_close_bytes": heap_before_close,
            })
            .to_string()
        });
    }
}
