use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use serde::{Deserialize, Serialize};

/// Explicit parent budgets; hosts resolve defaults once before construction.
/// 显式父级预算；宿主在构造前一次性解析默认值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedRuntimeConfig {
    /// Maximum retained plugin registrations, including closed entries awaiting explicit removal.
    /// 保留插件注册的数量上限，包含等待显式移除的已关闭条目。
    pub max_registered_plugins: usize,
    /// Maximum concurrently registered pool declarations, including draining generations.
    /// 同时注册的池声明数量上限，包含正在排空的代次。
    pub max_registered_pools: usize,
    /// Maximum retained session identities, including closed sessions awaiting explicit removal.
    /// 保留会话身份的数量上限，包含等待显式移除的已关闭会话。
    pub max_sessions: usize,
    /// Maximum retained capability registrations, including draining or unforgotten retired entries.
    /// 能力注册保留上限，包含正在排空或尚未遗忘的已退役条目。
    pub max_registered_capabilities: usize,
    /// Maximum resident VMs, including creation and pending teardown.
    /// 最大常驻 VM 数，包含创建中与等待清理的实例。
    pub max_resident_vms: usize,
    /// Maximum concurrent calls, including calls waiting for host results.
    /// 最大并发调用数，包含等待宿主结果的调用。
    pub max_running_calls: usize,
    /// Maximum accepted requests waiting for an execution slot.
    /// 等待执行许可的已接纳请求数量上限。
    pub max_queued_calls: usize,
    /// Maximum serialized bytes retained by queued requests.
    /// 排队请求保留的序列化字节数上限。
    pub max_queued_bytes: usize,
    /// Maximum operation records retained, including unfinished operations.
    /// 操作记录保留数量上限，包含未完成操作。
    pub max_operations: usize,
    /// Maximum host effect records retained by one operation, including completed callbacks.
    /// 单次操作保留的宿主副作用记录上限，包含已完成回调。
    pub max_effect_records_per_operation: usize,
    /// Maximum serialized module context and effect metadata bytes retained by one operation.
    /// 单次操作保留的模块上下文及副作用元数据序列化字节上限。
    pub max_effect_bytes_per_operation: usize,
    /// Maximum pending host requests across all plugin instances.
    /// 所有插件实例待完成宿主请求的数量上限。
    pub max_host_requests: usize,
    /// Maximum request bytes and reserved application output bytes, including dispatched calls.
    /// 请求字节与预留应用输出字节上限，包含已分发调用。
    /// Fixed protocol error metadata is separately bounded by the retained request count.
    /// 固定协议错误元数据由保留请求数量独立约束。
    pub max_host_request_bytes: usize,
    /// Maximum serialized application value bytes; fixed protocol error metadata is separate.
    /// 应用值的最大序列化字节数；固定协议错误元数据独立计算。
    pub max_value_bytes: usize,
}

impl EmbeddedRuntimeConfig {
    /// Validate these explicit budgets; zero never means unlimited capacity.
    /// 校验这些显式预算；零永远不表示无限容量。
    /// Return an error before allocating threads, VMs, or queue storage.
    /// 在分配线程、VM 或队列存储前返回错误。
    pub fn validate(&self) -> EmbeddedResult<()> {
        if self.max_effect_records_per_operation == 0 || self.max_effect_bytes_per_operation == 0 {
            return Err(EmbeddedError::invalid(
                "effect retention limits must be positive",
            ));
        }
        if self.max_registered_plugins == 0
            || self.max_registered_pools == 0
            || self.max_registered_capabilities == 0
            || self.max_sessions == 0
        {
            return Err(EmbeddedError::invalid(
                "registration limits must be positive",
            ));
        }
        if self.max_resident_vms == 0 || self.max_running_calls == 0 {
            return Err(EmbeddedError::invalid(
                "resident and running limits must be positive",
            ));
        }
        if self.max_running_calls > self.max_resident_vms {
            return Err(EmbeddedError::invalid(
                "running limit exceeds resident VM limit",
            ));
        }
        if self.max_queued_calls == 0 || self.max_queued_bytes == 0 {
            return Err(EmbeddedError::invalid(
                "queue count and byte limits must be positive",
            ));
        }
        if self.max_operations < self.max_running_calls
            || self.max_operations < self.max_queued_calls
        {
            return Err(EmbeddedError::invalid(
                "operation limit must cover running and queue limits individually",
            ));
        }
        if self.max_host_requests == 0 || self.max_value_bytes == 0 {
            return Err(EmbeddedError::invalid(
                "host request and value limits must be positive",
            ));
        }
        if self.max_value_bytes > self.max_queued_bytes {
            return Err(EmbeddedError::invalid(
                "value byte limit exceeds queue byte limit",
            ));
        }
        if self.max_value_bytes > self.max_host_request_bytes {
            return Err(EmbeddedError::invalid(
                "value byte limit exceeds host request byte limit",
            ));
        }
        Ok(())
    }
}

/// Capacity ownership, separate from instance reuse and ordering requirements.
/// 容量归属，与实例复用及顺序要求相互独立。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum PoolKind {
    /// Capacity shared across independently keyed plugin instances.
    /// 在具有独立匹配键的插件实例之间共享容量。
    Shared,
    /// Host-approved capacity with a non-lendable minimum reservation.
    /// 宿主批准且具有不可出借最小预留的容量。
    Dedicated,
}

/// Explicit module-state lifetime selected by a validated plugin contract.
/// 由已校验插件契约选择的显式模块状态寿命。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum InstanceReuse {
    /// Destroy the instance after one invocation.
    /// 一次调用后销毁实例。
    SingleCall,
    /// Reuse only within the same immutable generation and security partition.
    /// 仅在相同不可变代次与安全分区内复用。
    Reusable,
    /// Retain one instance for an explicitly opened session.
    /// 为显式打开的会话保留一个固定实例。
    Session,
}

/// Declared backend; unavailable variants are rejected instead of downgraded.
/// 声明的执行后端；不可用的取值直接拒绝，不降级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum ExecutionBackend {
    /// Execute in owned Lua VMs inside the current host process.
    /// 在当前宿主进程内的受管 Lua VM 中执行。
    InProcess,
    /// Reserved protocol identity; no worker backend is advertised yet.
    /// 预留的协议身份；目前尚未声明工作进程后端可用。
    WorkerProcess,
}

/// Immutable capacity policy for one host-assigned plugin execution group.
/// 单个宿主分配的插件执行分组的不可变容量策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct PluginPoolConfig {
    /// Shared or dedicated ownership of resident capacity.
    /// 常驻容量的公共或专用归属。
    pub kind: PoolKind,
    /// Non-lendable reservation; only dedicated groups may reserve capacity.
    /// 不可出借的预留；仅专用分组可以预留容量。
    pub min_resident_vms: usize,
    /// Maximum resident instances in this exact immutable execution domain.
    /// 此精确不可变执行域的最大常驻实例数。
    pub max_resident_vms: usize,
    /// Maximum simultaneously executing calls in this group.
    /// 当前分组同时执行的调用数上限。
    pub max_running_calls: usize,
    /// Maximum pending calls in this group.
    /// 当前分组等待调用的数量上限。
    pub max_queued_calls: usize,
    /// Module state lifetime; legacy stateless calls select single-call mode.
    /// 模块状态寿命；旧无状态调用选择单次模式。
    pub reuse: InstanceReuse,
    /// Whether all calls in this group require FIFO serialization.
    /// 当前分组的全部调用是否要求先进先出的串行执行。
    pub serial: bool,
    /// Requested execution backend, validated before activation.
    /// 激活前校验的所请求执行后端。
    pub backend: ExecutionBackend,
    /// Idle retirement threshold; absent explicitly disables idle retirement.
    /// 空闲退役阈值；省略明确表示关闭空闲退役。
    pub idle_ttl_ms: Option<u64>,
    /// Maximum successful uses before retirement; absent disables this limit.
    /// 退役前成功使用次数上限；省略表示关闭此上限。
    pub max_uses: Option<u64>,
}

impl PluginPoolConfig {
    /// Project physical limits into the sole capacity validator without changing reuse or queue policy.
    /// 将物理限制投影至唯一容量校验器，不改变复用或队列策略。
    /// Returns the exact declared limits, without defaults or normalization.
    /// 返回精确声明限制，不添加默认值或进行归一化。
    pub fn capacity(&self) -> super::VmCapacityConfig {
        super::VmCapacityConfig {
            kind: self.kind,
            min_resident_vms: self.min_resident_vms,
            max_resident_vms: self.max_resident_vms,
            max_running_calls: self.max_running_calls,
        }
    }

    /// Validate against `parent` without silently normalizing host intent.
    /// 针对 `parent` 校验，且不静默归一化宿主意图。
    /// Return a structured policy or unsupported-backend error on conflict.
    /// 冲突时返回结构化策略错误或后端不支持错误。
    pub fn validate(&self, parent: &EmbeddedRuntimeConfig) -> EmbeddedResult<()> {
        parent.validate()?;
        if self.backend != ExecutionBackend::InProcess {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "worker process backend is not available",
            ));
        }
        self.capacity().validate(parent)?;
        if self.serial && self.max_running_calls != 1 {
            return Err(EmbeddedError::invalid(
                "serial groups must declare exactly one running call",
            ));
        }
        if self.max_queued_calls == 0 || self.max_queued_calls > parent.max_queued_calls {
            return Err(EmbeddedError::invalid(
                "group queue limit is outside the parent budget",
            ));
        }
        if self.idle_ttl_ms == Some(0) || self.max_uses == Some(0) {
            return Err(EmbeddedError::invalid(
                "optional idle and reuse limits must be positive",
            ));
        }
        Ok(())
    }
}
