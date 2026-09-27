use super::{EmbeddedError, EmbeddedResult, EmbeddedRuntimeConfig, PluginPoolConfig};
use serde::{Deserialize, Serialize};

/// Immutable host-approved aggregate budgets across every generation and execution domain of one plugin.
/// 一个插件的全部代次与执行域共享的不可变宿主批准聚合预算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedPluginConfig {
    /// Maximum retained pool identities, including closed generations awaiting explicit removal.
    /// 保留池身份的数量上限，包含等待显式移除的已关闭代次。
    pub max_registered_pools: usize,
    /// Maximum retained sessions, including closed session records.
    /// 保留会话的数量上限，包含已关闭会话记录。
    pub max_sessions: usize,
    /// Maximum actual resident VMs plus other domains' unused dedicated reservations.
    /// 实际常驻 VM 与其他域未使用专用预留的合计上限。
    pub max_resident_vms: usize,
    /// Maximum dispatched operations, retained through initialization, host waiting and cleanup.
    /// 已分发操作上限，计费覆盖初始化、宿主等待及清理。
    pub max_running_calls: usize,
    /// Maximum accepted queued calls across all domains and sessions.
    /// 全部域和会话已接纳排队调用的合计上限。
    pub max_queued_calls: usize,
    /// Maximum exact serialized queued request bytes across this plugin.
    /// 此插件全部排队请求精确序列化字节数上限。
    pub max_queued_bytes: usize,
    /// Maximum retained operations, including completed results not explicitly forgotten.
    /// 保留操作的数量上限，包含尚未显式遗忘的已完成结果。
    pub max_operations: usize,
}

impl EmbeddedPluginConfig {
    /// Validate explicit positive budgets against `parent`; return errors without modifying either policy.
    /// 针对 `parent` 校验显式正数预算；返回错误，不修改任一策略。
    pub fn validate(&self, parent: &EmbeddedRuntimeConfig) -> EmbeddedResult<()> {
        parent.validate()?;
        for (value, limit) in [
            (self.max_registered_pools, parent.max_registered_pools),
            (self.max_sessions, parent.max_sessions),
            (self.max_resident_vms, parent.max_resident_vms),
            (self.max_running_calls, parent.max_running_calls),
            (self.max_queued_calls, parent.max_queued_calls),
            (self.max_queued_bytes, parent.max_queued_bytes),
            (self.max_operations, parent.max_operations),
        ] {
            if value == 0 || value > limit {
                return Err(EmbeddedError::invalid(
                    "plugin budget is outside the parent limit",
                ));
            }
        }
        if self.max_running_calls > self.max_resident_vms
            || self.max_running_calls > self.max_operations
            || self.max_queued_calls > self.max_operations
        {
            return Err(EmbeddedError::invalid(
                "plugin execution and retention budgets are inconsistent",
            ));
        }
        Ok(())
    }

    /// Check immutable domain `policy` against these aggregate limits before pool registration.
    /// 在池注册前，针对这些聚合上限检查不可变域 `policy`。
    /// Return a policy error instead of silently shrinking the requested domain.
    /// 返回策略错误，不静默缩减所请求的域。
    pub(super) fn validate_pool(&self, policy: &PluginPoolConfig) -> EmbeddedResult<()> {
        if policy.max_resident_vms > self.max_resident_vms
            || policy.max_running_calls > self.max_running_calls
            || policy.max_queued_calls > self.max_queued_calls
        {
            return Err(EmbeddedError::invalid(
                "execution domain exceeds its plugin budget",
            ));
        }
        Ok(())
    }
}
