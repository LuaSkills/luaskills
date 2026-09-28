//! Capacity ownership is independent of module identity, state reuse and execution backend.
//! 容量归属独立于模块身份、状态复用及执行后端。

use super::{EmbeddedError, EmbeddedResult, EmbeddedRuntimeConfig, PoolKind};
use serde::{Deserialize, Serialize};

/// Immutable physical VM limits shared by multiple independently isolated module pools.
/// 多个独立隔离模块池共享的不可变物理 VM 限制。
/// This policy governs resident and actual execution permits, not scheduler queues or plugin authorization.
/// 此策略治理常驻及实际执行许可，不治理调度队列或插件授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct VmCapacityConfig {
    /// Shared capacity or a dedicated reservation that other capacity groups cannot borrow.
    /// 公共容量，或其他容量组不能借用的专用预留。
    pub kind: PoolKind,
    /// Minimum committed slots; zero explicitly permits an unreserved group.
    /// 最小承诺槽位；零明确允许无预留分组。
    pub min_resident_vms: usize,
    /// Maximum real slots across every member pool, including creation and retirement.
    /// 全部成员池实际槽位上限，包含创建及退役。
    pub max_resident_vms: usize,
    /// Maximum simultaneous physical execution permits across member pools.
    /// 全部成员池同时持有的物理执行许可上限。
    pub max_running_calls: usize,
}

impl VmCapacityConfig {
    /// Validate explicit limits against parent without modifying either policy; return any conflict.
    /// 针对父级校验显式限制，不修改任一策略；返回任何冲突。
    pub fn validate(&self, parent: &EmbeddedRuntimeConfig) -> EmbeddedResult<()> {
        parent.validate()?;
        if self.max_resident_vms == 0 || self.max_resident_vms > parent.max_resident_vms {
            return Err(EmbeddedError::invalid(
                "group resident limit is outside the parent budget",
            ));
        }
        if self.min_resident_vms > self.max_resident_vms {
            return Err(EmbeddedError::invalid(
                "group reservation exceeds its resident limit",
            ));
        }
        if self.kind == PoolKind::Shared && self.min_resident_vms != 0 {
            return Err(EmbeddedError::invalid(
                "shared groups cannot reserve dedicated capacity",
            ));
        }
        if self.max_running_calls == 0
            || self.max_running_calls > self.max_resident_vms
            || self.max_running_calls > parent.max_running_calls
        {
            return Err(EmbeddedError::invalid(
                "group running limit is outside the resident or parent budget",
            ));
        }
        Ok(())
    }
}
