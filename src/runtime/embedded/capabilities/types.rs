use super::super::{
    CallControl, EffectState, EmbeddedError, EmbeddedErrorCode, EmbeddedResult,
    EmbeddedRuntimeConfig, JsonContract,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Host execution transport, explicitly selected before a capability is published.
/// 宿主执行传输，在能力发布前显式选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum CapabilityExecution {
    /// Short cooperative Rust callback executed on the owning VM thread.
    /// 在所属 VM 线程执行的短时协作 Rust 回调。
    Native,
    /// Reliable request consumed and completed by an SDK event pump.
    /// 由 SDK 事件泵消费并完成的可靠请求。
    Queued,
}

/// Declared effect category; it does not make external mutations transactional.
/// 声明的副作用类别；它不会使外部变更自动具有事务性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum CapabilityEffects {
    /// Host contract promises no externally visible mutation.
    /// 宿主契约承诺不产生外部可见变更。
    ReadOnly,
    /// Host must report actual commit, rollback or unknown status.
    /// 宿主必须报告真实提交、回滚或未知状态。
    Mutating,
}

/// Explicit side-effect deduplication support, never inferred from an operation identifier.
/// 显式副作用去重支持，绝不从操作标识推断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum CapabilityIdempotency {
    /// Automatic replay is forbidden; an uncertain result needs host reconciliation.
    /// 禁止自动重放；不确定结果需要宿主对账。
    None,
    /// Host implementation deduplicates the supplied request identity durably.
    /// 宿主实现对提供的请求身份进行持久去重。
    HostRequest,
}

/// Scope required from the trusted caller, independent from Lua business arguments.
/// 可信调用方必须具备的作用域，独立于 Lua 业务参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum CapabilityScope {
    /// Available during an ordinary operation or session invocation.
    /// 在普通操作或会话调用期间可用。
    Invocation,
    /// Requires an explicitly bound session identity.
    /// 要求显式绑定的会话身份。
    Session,
}

/// Immutable capability declaration shared by Rust, generated contracts and SDKs.
/// Rust、生成契约与 SDK 共享的不可变能力声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct CapabilityDescriptor {
    /// Exact namespaced name; discovery exposes only authorized declarations.
    /// 精确命名空间名称；发现操作仅暴露已授权声明。
    pub name: String,
    /// Semantic interface version, independent from the core library version.
    /// 语义接口版本，独立于核心库版本。
    pub version: String,
    /// English description of the host capability for tool consumers.
    /// 面向工具消费者的宿主能力英文描述。
    pub description: String,
    /// Offline input value contract.
    /// 离线输入值契约。
    pub input_schema: Value,
    /// Offline output value contract.
    /// 离线输出值契约。
    pub output_schema: Value,
    /// Explicit native or queued dispatch protocol.
    /// 显式原生或队列分发协议。
    pub execution: CapabilityExecution,
    /// Every listed grant must still exist at each admission boundary.
    /// 每个入场边界仍必须拥有列出的全部授权。
    pub permissions: BTreeSet<String>,
    /// Required trusted invocation scope.
    /// 必需的可信调用作用域。
    pub scope: CapabilityScope,
    /// Maximum in-flight handlers, including cancelled handlers that have not stopped.
    /// 在途处理器上限，包含已取消但尚未停止的处理器。
    pub max_concurrent: usize,
    /// Per-capability budget capped by the original operation deadline.
    /// 受原始操作截止时间约束的单能力预算。
    pub max_call_ms: u64,
    /// Maximum serialized input bytes within the parent value limit.
    /// 父级值上限内的最大序列化输入字节数。
    pub max_input_bytes: usize,
    /// Maximum serialized output bytes within the parent value limit.
    /// 父级值上限内的最大序列化输出字节数。
    pub max_output_bytes: usize,
    /// Declared mutation category.
    /// 声明的变更类别。
    pub effects: CapabilityEffects,
    /// Explicit host deduplication contract.
    /// 显式宿主去重契约。
    pub idempotency: CapabilityIdempotency,
}

impl CapabilityDescriptor {
    /// Validate this descriptor against `config` and return its compiled input/output contracts.
    /// 针对 `config` 校验此描述，并返回编译后的输入与输出契约。
    pub(super) fn compile(
        &self,
        config: &EmbeddedRuntimeConfig,
    ) -> EmbeddedResult<(JsonContract, JsonContract)> {
        if self.name.trim().is_empty()
            || self.name.contains('\0')
            || self.description.trim().is_empty()
            || self
                .permissions
                .iter()
                .any(|permission| permission.trim().is_empty() || permission.contains('\0'))
        {
            return Err(EmbeddedError::invalid(
                "capability names, description and permissions must be explicit",
            ));
        }
        semver::Version::parse(&self.version)
            .map_err(|_| EmbeddedError::invalid("capability version must be semantic"))?;
        if self.execution == CapabilityExecution::Native
            && self.idempotency == CapabilityIdempotency::HostRequest
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "host-request idempotency requires queued execution",
            ));
        }
        if self.max_concurrent == 0
            || self.max_concurrent > config.max_host_requests
            || self.max_call_ms == 0
            || self.max_input_bytes == 0
            || self.max_output_bytes == 0
            || self.max_input_bytes > config.max_value_bytes
            || self.max_output_bytes > config.max_value_bytes
        {
            return Err(EmbeddedError::invalid(
                "capability budgets are outside the parent limits",
            ));
        }
        Instant::now()
            .checked_add(Duration::from_millis(self.max_call_ms))
            .ok_or_else(|| EmbeddedError::invalid("capability deadline cannot be represented"))?;
        Ok((
            JsonContract::compile(&self.input_schema)?,
            JsonContract::compile(&self.output_schema)?,
        ))
    }
}

/// Host-authenticated caller data copied outside plugin-controlled arguments.
/// 在插件可控参数之外复制的宿主认证调用方数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct CapabilityCaller {
    /// Runtime namespace that owns the registration and operation.
    /// 拥有注册及操作的运行时命名空间。
    pub runtime_id: String,
    /// Exact activated plugin identity.
    /// 精确激活的插件身份。
    pub plugin_id: String,
    /// Immutable package and dependency generation.
    /// 不可变包与依赖代次。
    pub package_generation: String,
    /// Immutable execution and initialization-configuration revision.
    /// 不可变执行与初始化配置修订。
    pub execution_revision: String,
    /// Trusted user/workspace partition, never copied from a Lua argument.
    /// 可信用户与工作区分区，绝不从 Lua 参数复制。
    pub security_partition: String,
    /// Exact operation whose original budget applies.
    /// 适用原始预算的精确操作。
    pub operation_id: String,
    /// Optional fixed session required by session-scoped capabilities.
    /// 会话作用域能力要求的可选固定会话。
    pub session_id: Option<String>,
    /// Explicit authorized workspace; absent means package-only context.
    /// 显式授权工作区；省略表示仅包内上下文。
    pub workspace_root: Option<String>,
}

impl CapabilityCaller {
    /// Verify nonempty authority fields and exact owning `runtime_id` before dispatch.
    /// 分发前校验非空权威字段及精确所属 `runtime_id`。
    pub(in crate::runtime::embedded) fn validate(&self, runtime_id: &str) -> EmbeddedResult<()> {
        if self.runtime_id != runtime_id
            || [
                &self.plugin_id,
                &self.package_generation,
                &self.execution_revision,
                &self.security_partition,
                &self.operation_id,
            ]
            .iter()
            .any(|value| value.trim().is_empty() || value.contains('\0'))
            || self
                .session_id
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.contains('\0'))
        {
            return Err(EmbeddedError::invalid(
                "capability caller identity is invalid",
            ));
        }
        Ok(())
    }
}

/// Live permission authority; immutable capability snapshots do not freeze revoked grants.
/// 实时权限权威；不可变能力快照不会冻结已撤销授权。
pub struct CapabilityPermissions {
    /// Host-owned grants, consulted before discovery, admission and queued dispatch.
    /// 宿主拥有的授权，在发现、入场及队列分发前检查。
    grants: RwLock<BTreeSet<String>>,
}

impl CapabilityPermissions {
    /// Construct explicit `grants`; no capability is implicitly authorized by registration alone.
    /// 构造显式 `grants`；仅注册能力不会隐式授权。
    pub fn new(grants: BTreeSet<String>) -> EmbeddedResult<Arc<Self>> {
        if grants
            .iter()
            .any(|grant| grant.trim().is_empty() || grant.contains('\0'))
        {
            return Err(EmbeddedError::invalid(
                "permission grants must be nonempty names",
            ));
        }
        Ok(Arc::new(Self {
            grants: RwLock::new(grants),
        }))
    }

    /// Revoke exact `permission`, returning whether an existing grant was removed.
    /// 撤销精确 `permission`，返回是否移除了已有授权。
    pub fn revoke(&self, permission: &str) -> EmbeddedResult<bool> {
        Ok(self
            .grants
            .write()
            .map_err(|_| {
                EmbeddedError::new(EmbeddedErrorCode::Internal, "permission state is poisoned")
            })?
            .remove(permission))
    }

    /// Require all `permissions`; rejection does not reveal the missing grant or other identities.
    /// 要求全部 `permissions`；拒绝时不暴露缺少的授权或其他身份。
    pub fn require(&self, permissions: &BTreeSet<String>) -> EmbeddedResult<()> {
        if !permissions.is_subset(&*self.grants.read().map_err(|_| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "permission state is poisoned")
        })?) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::PermissionDenied,
                "capability permission denied",
            ));
        }
        Ok(())
    }
}

/// One host call's original operation cancellation plus a non-renewable capability deadline.
/// 单个宿主调用的原始操作取消控制与不可续期能力截止时间。
#[derive(Clone)]
pub struct CapabilityBudget {
    /// Parent cancellation remains authoritative during native or SDK work.
    /// 原生或 SDK 工作期间父级取消仍为权威。
    control: Arc<CallControl>,
    /// Minimum of the parent deadline and declared capability duration.
    /// 父级截止时间与声明能力时长中的较早值。
    deadline: Instant,
}

impl CapabilityBudget {
    /// Derive a bounded child from original `control` and declared `max_call_ms`.
    /// 根据原始 `control` 与声明的 `max_call_ms` 派生受限子预算。
    pub(super) fn new(control: Arc<CallControl>, max_call_ms: u64) -> EmbeddedResult<Self> {
        control.check()?;
        // Checked duration arithmetic never extends the original operation's deadline.
        // 受检时长运算绝不延长原始操作截止时间。
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(max_call_ms))
            .ok_or_else(|| EmbeddedError::invalid("capability deadline cannot be represented"))?
            .min(control.deadline());
        Ok(Self { control, deadline })
    }

    /// Check live parent cancellation and the fixed child deadline.
    /// 检查实时父级取消与固定子截止时间。
    pub fn check(&self) -> EmbeddedResult<()> {
        self.control.check()?;
        if Instant::now() >= self.deadline {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::DeadlineExceeded,
                "capability deadline exceeded",
            ));
        }
        Ok(())
    }

    /// Report remaining milliseconds for SDK diagnostics without renewing the core deadline.
    /// 为 SDK 诊断报告剩余毫秒数，不延长核心截止时间。
    pub fn remaining_ms(&self) -> u64 {
        u64::try_from(
            self.deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }
}

/// Actual host result and effect evidence, including a failed response after a successful commit.
/// 真实宿主结果与副作用证据，包含成功提交后的失败响应。
#[derive(Debug, Clone)]
pub struct CapabilityOutcome {
    /// Structured application result or stable protocol error.
    /// 结构化应用结果或稳定协议错误。
    pub result: EmbeddedResult<Value>,
    /// Host-confirmed effect status, independent from cancellation and schema validation.
    /// 宿主确认的副作用状态，独立于取消与 Schema 校验。
    pub effects: EffectState,
}

impl CapabilityOutcome {
    /// Return the explicit success/error envelope shared by Lua and the versioned SDK protocol.
    /// 返回由 Lua 与版本化 SDK 协议共享的显式成功或错误信封。
    /// Successful null values remain present; failures carry no fabricated application value.
    /// 成功的空值保持存在；失败不携带虚构应用值。
    pub fn to_json(&self) -> Value {
        match &self.result {
            Ok(value) => serde_json::json!({"ok":true,"value":value,"effects":self.effects}),
            Err(error) => serde_json::json!({"ok":false,"error":error,"effects":self.effects}),
        }
    }
}
