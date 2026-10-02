use super::super::value_size::json_size;
use super::super::{
    CallControl, EffectState, EmbeddedError, EmbeddedErrorCode, EmbeddedResult,
    EmbeddedRuntimeConfig, JsonContract,
};
use super::broker::{HostRequestBroker, HostRequestHandle};
use super::types::*;
use crate::runtime::embedded::effects::EffectAttempt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

#[cfg(test)]
mod tests;

/// Native callback receives only trusted context and validated business arguments.
/// 原生回调仅接收可信上下文及已校验业务参数。
pub type NativeCapability = Arc<dyn Fn(&CapabilityInvocation) -> CapabilityOutcome + Send + Sync>;

/// Immutable validated invocation context, constructed outside Lua-controlled data.
/// 在 Lua 可控数据之外构造的不可变已校验调用上下文。
pub struct CapabilityInvocation {
    /// Registered operation evidence identity, absent only for explicit low-level untracked calls.
    /// 注册操作证据身份，仅显式低层未跟踪调用省略。
    pub effect_id: Option<String>,
    /// Host-authenticated identity including the owning operation.
    /// 包含所属操作的宿主认证身份。
    pub caller: CapabilityCaller,
    /// Structured arguments validated against this registration's input contract.
    /// 已针对当前注册输入契约校验的结构化参数。
    pub arguments: Value,
    /// Cooperative cancellation and original bounded deadline.
    /// 协作取消与原始有界截止时间。
    pub budget: CapabilityBudget,
    /// Live permission authority retained for per-effect authorization checks.
    /// 为逐副作用授权检查保留的实时权限权威。
    permissions: Arc<CapabilityPermissions>,
    /// Exact required grants from the immutable registration.
    /// 来自不可变注册的精确必需授权。
    required: BTreeSet<String>,
}

impl CapabilityInvocation {
    /// Recheck cancellation, deadline and live authorization immediately before a host side effect.
    /// 在宿主副作用前立即重新检查取消、截止时间与实时授权。
    pub fn authorize(&self) -> EmbeddedResult<()> {
        self.budget.check()?;
        self.permissions.require(&self.required)
    }
}

/// One atomically published registration request.
/// 单个原子发布的注册请求。
pub struct CapabilityRegistrationRequest {
    /// Explicit transport, limits and value contracts.
    /// 显式传输、上限及值契约。
    pub descriptor: CapabilityDescriptor,
    /// Required for native registrations and forbidden for queued registrations.
    /// 原生注册必需，队列注册禁止提供。
    pub native: Option<NativeCapability>,
}

/// Observable lifetime of an exact registration, including unregistration still draining calls.
/// 精确注册的可观察生命周期，包含注销后仍在排空的调用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct CapabilityRegistrationStatus {
    /// Opaque identity, never a lossy language number.
    /// 不透明身份，绝不使用有精度损失的语言数值。
    pub registration_id: String,
    /// Exact declared capability name.
    /// 精确声明的能力名称。
    pub name: String,
    /// Whether new calls may enter this specific registration.
    /// 新调用是否可以进入此特定注册。
    pub accepting: bool,
    /// Actually executing or dispatched handlers, including pending cancellation.
    /// 实际执行或已分发的处理器，包含等待取消完成的处理器。
    pub in_flight: usize,
    /// True only after admission closed and all native callback references were released.
    /// 仅在入场关闭且全部原生回调引用释放后为真。
    pub drained: bool,
}

/// Shared admission authority for native and queued capabilities together.
/// 原生与队列能力共同使用的共享入场权威。
struct CapabilityAdmission {
    /// Exact parent in-flight maximum.
    /// 精确父级在途上限。
    limit: usize,
    /// Actual active handlers, mutated together with the registration count.
    /// 真实活动处理器数，与注册计数共同变更。
    running: Mutex<usize>,
}

/// Registration-local state, held only for short metadata operations.
/// 注册局部状态，仅为短时元数据操作持有。
struct EntryState {
    /// Permanent admission gate for this immutable registration.
    /// 当前不可变注册的永久入场门。
    accepting: bool,
    /// Runtime shutdown admits only registered finalization controls until actual operation drainage.
    /// 运行时关闭期间仅接纳已注册关闭控制，直到操作真正排空。
    finalization_only: bool,
    /// Actual admitted handlers retained until their execution owners finish.
    /// 保留到执行所有者结束的真实入场处理器数。
    running: usize,
    /// Native closure ownership is being released outside locks and may still run destructors.
    /// 原生闭包所有权正在锁外释放，可能仍在运行析构器。
    releasing: bool,
    /// Original native closure, released after the last admitted clone.
    /// 原生原始闭包，在最后一个入场克隆之后释放。
    native: Option<NativeCapability>,
}

/// Immutable identity and contracts plus an independently revocable admission gate.
/// 不可变身份与契约，以及可独立撤销的入场门。
pub(super) struct CapabilityEntry {
    /// Runtime-scoped opaque registration identity.
    /// 运行时作用域的不透明注册身份。
    pub(super) id: String,
    /// Immutable public declaration.
    /// 不可变公开声明。
    pub(super) descriptor: CapabilityDescriptor,
    /// Compiled input schema from this exact declaration.
    /// 来自此精确声明的编译输入 Schema。
    input: JsonContract,
    /// Compiled output schema from this exact declaration.
    /// 来自此精确声明的编译输出 Schema。
    output: JsonContract,
    /// Parent capacity shared by both dispatch modes.
    /// 两种分发模式共享的父级容量。
    admission: Arc<CapabilityAdmission>,
    /// No host code runs while this metadata is locked.
    /// 锁定此元数据时不运行宿主代码。
    state: Mutex<EntryState>,
}

impl CapabilityEntry {
    /// Run an internal metadata transition while the exact registration's dispatch gate remains open.
    /// 在精确注册的分发门保持开放时执行内部元数据变更。
    /// `transition` must not call host code; its return value is forwarded after releasing the gate.
    /// `transition` 不得调用宿主代码；释放门后转发其返回值。
    pub(super) fn with_dispatch_gate<T>(
        &self,
        finalization: bool,
        transition: impl FnOnce() -> T,
    ) -> EmbeddedResult<T> {
        // Keep unregistration linearized with queued-to-dispatched publication.
        // 保持注销与排队到已分发发布之间的线性化顺序。
        let state = self
            .state
            .lock()
            .map_err(|_| internal("capability state is poisoned"))?;
        if !state.accepting || (state.finalization_only && !finalization) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "capability registration is closed",
            ));
        }
        Ok(transition())
    }

    /// Close this exact registration; already-admitted handlers keep their original closure.
    /// 关闭此精确注册；已入场处理器保留其原始闭包。
    fn close(&self) {
        // Move user-owned closures outside the lock before dropping them.
        // 释放用户拥有的闭包前，将其移到锁外。
        let native = {
            // Cleanup recovers owned metadata without admitting work after poisoning.
            // 清理恢复已拥有元数据，且不在中毒后接纳任务。
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.accepting = false;
            if state.running == 0 && state.native.is_some() {
                state.releasing = true;
                state.native.take()
            } else {
                None
            }
        };
        if native.is_some() {
            drop(native);
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .releasing = false;
        }
    }

    /// Snapshot actual lifecycle; this never calls the host or probes alternative registrations.
    /// 获取真实生命周期快照；绝不调用宿主或探测替代注册。
    pub(super) fn status(&self) -> EmbeddedResult<CapabilityRegistrationStatus> {
        // One guard protects the complete lifecycle snapshot.
        // 单个保护锁保护完整生命周期快照。
        let state = self
            .state
            .lock()
            .map_err(|_| internal("capability state is poisoned"))?;
        Ok(CapabilityRegistrationStatus {
            registration_id: self.id.clone(),
            name: self.descriptor.name.clone(),
            accepting: state.accepting && !state.finalization_only,
            in_flight: state.running,
            drained: !state.accepting
                && state.running == 0
                && state.native.is_none()
                && !state.releasing,
        })
    }

    /// Admit against parent and registration budgets atomically, returning exclusive release authority.
    /// 原子地针对父级与注册预算入场，返回独占释放权威。
    pub(super) fn admit(
        self: &Arc<Self>,
        finalization: bool,
    ) -> EmbeddedResult<AdmittedCapability> {
        // Lock order is parent admission then registration; callbacks never hold either lock.
        // 锁顺序为父级入场后注册；回调绝不持有任一锁。
        let mut running = self
            .admission
            .running
            .lock()
            .map_err(|_| internal("capability admission is poisoned"))?;
        // The entry gate cannot close between validation and acquiring the callback clone.
        // 条目门不能在校验与获取回调克隆之间关闭。
        let mut state = self
            .state
            .lock()
            .map_err(|_| internal("capability state is poisoned"))?;
        if !state.accepting || (state.finalization_only && !finalization) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "capability registration is closed",
            ));
        }
        if *running >= self.admission.limit || state.running >= self.descriptor.max_concurrent {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "capability execution capacity reached",
            ));
        }
        *running += 1;
        state.running += 1;
        Ok(AdmittedCapability {
            native: state.native.clone(),
            _permit: CapabilityPermit {
                entry: Arc::clone(self),
            },
        })
    }

    /// Check actual host `outcome` without erasing externally committed effects.
    /// 检查真实宿主 `outcome`，且不抹去已对外提交的副作用。
    pub(super) fn validate_outcome(&self, mut outcome: CapabilityOutcome) -> CapabilityOutcome {
        // Error strings are host outputs too and must not bypass the retained value budget.
        // 错误字符串同样属于宿主输出，不能绕过保留值预算。
        let validation = match &outcome.result {
            Ok(value) => json_size(value, self.descriptor.max_output_bytes)
                .and_then(|_| self.output.validate(value)),
            Err(error) => json_size(error, self.descriptor.max_output_bytes).map(|_| ()),
        };
        if let Err(error) = validation {
            outcome.result = Err(error);
        }
        // Schema paths are generated diagnostics and may also exceed the caller's output allowance.
        // Schema 路径属于生成诊断，也可能超出调用方输出限额。
        if let Err(error) = &mut outcome.result
            && json_size(error, self.descriptor.max_output_bytes).is_err()
        {
            error.message = "host capability result rejected".into();
        }
        outcome
    }
}

/// Unique capacity owner for one admitted native or queued handler.
/// 单个已入场原生或队列处理器的唯一容量所有者。
struct CapabilityPermit {
    /// Exact entry remains alive even when removed from discovery.
    /// 精确条目即使从发现中移除也保持存活。
    entry: Arc<CapabilityEntry>,
}

impl Drop for CapabilityPermit {
    /// Release capacity only after the invocation's callback clone has already been destroyed.
    /// 仅在调用的回调克隆已销毁后释放容量。
    fn drop(&mut self) {
        // Preserve the same parent-to-entry lock order as admission.
        // 保持与入场相同的父级到条目锁顺序。
        let native = {
            // Recovering metadata is limited to releasing an already-owned permit.
            // 恢复元数据仅用于释放已拥有许可。
            let mut running = self
                .entry
                .admission
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // This unique permit corresponds to exactly one recorded handler.
            // 此唯一许可精确对应一个已记录处理器。
            let mut state = self
                .entry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *running -= 1;
            state.running -= 1;
            if !state.accepting && state.running == 0 && state.native.is_some() {
                state.releasing = true;
                state.native.take()
            } else {
                None
            }
        };
        if native.is_some() {
            drop(native);
            self.entry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .releasing = false;
        }
    }
}

/// Callback ownership drops before the permit, making drained status safe for SDK closure release.
/// 回调所有权先于许可释放，使已排空状态可用于安全释放 SDK 闭包。
pub(super) struct AdmittedCapability {
    /// Native implementation clone, absent only for the explicitly queued transport.
    /// 原生实现克隆，仅对显式队列传输省略。
    native: Option<NativeCapability>,
    /// Unique release authority, declared after the callback to enforce destruction order.
    /// 唯一释放权威，在回调后声明以保证销毁顺序。
    _permit: CapabilityPermit,
}

/// One admitted call retained by either native execution or a reliable SDK request.
/// 由原生执行或可靠 SDK 请求保留的单个已入场调用。
pub(super) struct PreparedCapability {
    /// Immutable execution stage proven by the operation-bound control, never by plugin arguments.
    /// 由绑定操作的控制证明的不可变执行阶段，绝不由插件参数决定。
    pub(super) finalization: bool,
    /// Exact immutable registration captured by the module's snapshot.
    /// 模块快照捕获的精确不可变注册。
    pub(super) entry: Arc<CapabilityEntry>,
    /// Validated arguments and trusted identity with live authorization.
    /// 已校验参数及具有实时授权的可信身份。
    pub(super) invocation: CapabilityInvocation,
    /// Admission and callback ownership until real completion.
    /// 保留到真实完成的入场与回调所有权。
    pub(super) admitted: AdmittedCapability,
    /// Evidence finalization follows actual callback and admission release by field destruction order.
    /// 通过字段析构顺序，使证据完成晚于真实回调与入场释放。
    pub(super) effect: EffectAttempt,
}

/// Registry metadata contains active names plus bounded historical registrations awaiting explicit forget.
/// 注册表元数据包含活动名称及等待显式遗忘的有界历史注册。
struct RegistryState {
    /// Permanent instance shutdown rejects new publication and snapshots.
    /// 永久实例关闭拒绝新发布与快照。
    closing: bool,
    /// Monotonic identity and snapshot revision, never reused after unregistering.
    /// 注销后绝不复用的单调身份与快照修订。
    sequence: u64,
    /// Exact active name to immutable registration identity mapping.
    /// 精确活动名称到不可变注册身份的映射。
    active: BTreeMap<String, String>,
    /// Retention bound includes closing registrations until callers forget drained ones.
    /// 保留上限包含关闭中注册，直到调用方遗忘已排空注册。
    entries: BTreeMap<String, Arc<CapabilityEntry>>,
}

/// Instance-owned registry with atomic batch publication and explicit callback retirement.
/// 实例拥有的注册表，支持原子批量发布与显式回调退役。
pub struct CapabilityRegistry {
    /// Reliable control queue shared by this registry and all of its immutable snapshots.
    /// 此注册表与其全部不可变快照共享的可靠控制队列。
    broker: Arc<HostRequestBroker>,
    /// Trusted namespace included in all opaque registration identities.
    /// 包含在全部不透明注册身份中的可信命名空间。
    runtime_id: String,
    /// Authoritative validated parent limits.
    /// 权威且已校验的父级上限。
    config: EmbeddedRuntimeConfig,
    /// Shared admission counts include every retained native and queued handler.
    /// 共享入场计数包含全部被保留的原生与队列处理器。
    admission: Arc<CapabilityAdmission>,
    /// Short registry lock never held while executing host implementations.
    /// 执行宿主实现时绝不持有的短时注册表锁。
    state: Mutex<RegistryState>,
}

impl CapabilityRegistry {
    /// Validate that snapshot shares this registry's actual broker, returning an error for foreign authority.
    /// 校验 snapshot 共享此注册表的真实代理；外来权威返回错误。
    /// Equal caller-supplied namespace strings never substitute for native ownership identity.
    /// 相同的调用方提供命名空间字符串绝不能替代原生所有权身份。
    pub(crate) fn validate_snapshot(&self, snapshot: &CapabilitySnapshot) -> EmbeddedResult<()> {
        if !Arc::ptr_eq(&self.broker, &snapshot.broker) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::PermissionDenied,
                "module capability binding belongs to another registry",
            ));
        }
        Ok(())
    }

    /// Construct an empty registry bound to `runtime_id` and validated `config`.
    /// 构造绑定 `runtime_id` 与已校验 `config` 的空注册表。
    pub fn new(runtime_id: String, config: EmbeddedRuntimeConfig) -> EmbeddedResult<Arc<Self>> {
        config.validate()?;
        if runtime_id.trim().is_empty() || runtime_id.contains('\0') {
            return Err(EmbeddedError::invalid("runtime identity must be nonempty"));
        }
        Ok(Arc::new(Self {
            broker: HostRequestBroker::new(runtime_id.clone(), &config),
            runtime_id,
            admission: Arc::new(CapabilityAdmission {
                limit: config.max_host_requests,
                running: Mutex::new(0),
            }),
            config,
            state: Mutex::new(RegistryState {
                closing: false,
                sequence: 0,
                active: BTreeMap::new(),
                entries: BTreeMap::new(),
            }),
        }))
    }

    /// Validate all `requests`, then publish them together; return opaque registration identities.
    /// 校验全部 `requests` 后一同发布；返回不透明注册身份。
    pub fn register(
        &self,
        requests: Vec<CapabilityRegistrationRequest>,
    ) -> EmbeddedResult<Vec<String>> {
        if requests.is_empty() {
            return Err(EmbeddedError::invalid(
                "capability registration batch is empty",
            ));
        }
        if requests.len() > self.config.max_registered_capabilities {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "capability registration batch exceeds capacity",
            ));
        }
        // Compile schemas before taking the publication lock.
        // 获取发布锁前编译 Schema。
        let prepared = requests
            .into_iter()
            .map(|request| {
                if (request.descriptor.execution == CapabilityExecution::Native)
                    != request.native.is_some()
                {
                    return Err(EmbeddedError::invalid(
                        "capability callback does not match its execution transport",
                    ));
                }
                // Exact input and output validators belong to this specific immutable descriptor.
                // 精确输入与输出校验器属于此特定不可变描述。
                let contracts = request.descriptor.compile(&self.config)?;
                Ok((request, contracts))
            })
            .collect::<EmbeddedResult<Vec<_>>>()?;
        // One publication transaction validates conflicts and all resource limits.
        // 单个发布事务校验冲突与全部资源上限。
        let mut state = self
            .state
            .lock()
            .map_err(|_| internal("capability registry is poisoned"))?;
        if state.closing {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "capability registry is closed",
            ));
        }
        if prepared.len()
            > self
                .config
                .max_registered_capabilities
                .saturating_sub(state.entries.len())
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "capability registration capacity reached",
            ));
        }
        // Duplicate names within the same batch are rejected before any entry becomes visible.
        // 同一批次中的重复名称在任何条目可见前被拒绝。
        let mut names = BTreeSet::new();
        for (request, _) in &prepared {
            if state.active.contains_key(&request.descriptor.name)
                || !names.insert(&request.descriptor.name)
            {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "capability name is already registered",
                ));
            }
        }
        state
            .sequence
            .checked_add(
                u64::try_from(prepared.len())
                    .map_err(|_| internal("registration count overflow"))?,
            )
            .ok_or_else(|| internal("capability identity exhausted"))?;
        // Publish only fully validated entries with retained callback ownership.
        // 仅发布已完整校验且保留回调所有权的条目。
        let mut ids = Vec::with_capacity(prepared.len());
        for (request, (input, output)) in prepared {
            state.sequence += 1;
            // String identities preserve their exact value across JavaScript and every FFI.
            // 字符串身份在 JavaScript 与全部 FFI 中保留精确值。
            let id = crate::runtime::embedded::IdentityKind::Capability
                .render(&self.runtime_id, state.sequence);
            state
                .active
                .insert(request.descriptor.name.clone(), id.clone());
            state.entries.insert(
                id.clone(),
                Arc::new(CapabilityEntry {
                    id: id.clone(),
                    descriptor: request.descriptor,
                    input,
                    output,
                    admission: Arc::clone(&self.admission),
                    state: Mutex::new(EntryState {
                        accepting: true,
                        finalization_only: false,
                        running: 0,
                        releasing: false,
                        native: request.native,
                    }),
                }),
            );
            ids.push(id);
        }
        Ok(ids)
    }

    /// Capture immutable registrations; later publication cannot redirect these exact identities.
    /// 捕获不可变注册；后续发布不能重定向这些精确身份。
    pub fn snapshot(&self) -> EmbeddedResult<CapabilitySnapshot> {
        // Snapshot reads the same atomic publication state as registration and unregistration.
        // 快照读取与注册和注销相同的原子发布状态。
        let state = self
            .state
            .lock()
            .map_err(|_| internal("capability registry is poisoned"))?;
        if state.closing {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "capability registry is closed",
            ));
        }
        Ok(CapabilitySnapshot {
            broker: Arc::clone(&self.broker),
            runtime_id: self.runtime_id.clone(),
            revision: state.sequence,
            entries: state
                .active
                .iter()
                .map(|(name, id)| {
                    (
                        name.clone(),
                        Arc::clone(state.entries.get(id).expect("active registration exists")),
                    )
                })
                .collect(),
        })
    }

    /// Close exact `id` admission before returning; do not release callbacks that are still executing.
    /// 返回前关闭精确 `id` 入场；不释放仍在执行的回调。
    pub fn unregister(&self, id: &str) -> EmbeddedResult<CapabilityRegistrationStatus> {
        // Remove only the exact active identity, leaving newer same-name registrations untouched.
        // 仅移除精确活动身份，不触及同名的新注册。
        let entry = {
            // No closure destructor is allowed under the registry publication lock.
            // 注册表发布锁内不允许执行闭包析构器。
            let mut state = self
                .state
                .lock()
                .map_err(|_| internal("capability registry is poisoned"))?;
            // A strong entry reference survives removal from discovery.
            // 条目强引用在从发现中移除后存活。
            let entry = Arc::clone(state.entries.get(id).ok_or_else(not_found)?);
            if state
                .active
                .get(&entry.descriptor.name)
                .is_some_and(|active| active == id)
            {
                // Removing a visible name publishes a new snapshot just like adding one.
                // 移除可见名称与新增名称一样，会发布新快照。
                let revision = state
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| internal("capability identity exhausted"))?;
                state.active.remove(&entry.descriptor.name);
                state.sequence = revision;
            }
            entry
        };
        entry.close();
        self.broker.retire_registration(id)?;
        entry.status()
    }

    /// Query exact `id`; expired records are never interpreted as completed execution.
    /// 查询精确 `id`；过期记录绝不被解释为执行已完成。
    pub fn status(&self, id: &str) -> EmbeddedResult<CapabilityRegistrationStatus> {
        // Clone before reading entry metadata so no nested registry lock is needed.
        // 读取条目元数据前先克隆，避免嵌套持有注册表锁。
        let entry = Arc::clone(
            self.state
                .lock()
                .map_err(|_| internal("capability registry is poisoned"))?
                .entries
                .get(id)
                .ok_or_else(not_found)?,
        );
        entry.status()
    }

    /// Return this runtime's SDK request queue; it never consumes a VM execution permit.
    /// 返回此运行时的 SDK 请求队列；它绝不消耗 VM 执行许可。
    pub fn host_requests(&self) -> Arc<HostRequestBroker> {
        Arc::clone(&self.broker)
    }

    /// Reject scheduler submission from a synchronous native callback in this runtime.
    /// 拒绝此运行时同步原生回调中的调度提交。
    /// Return success only when the caller cannot synchronously wait on its own execution slots.
    /// 仅在调用方不会同步等待自身执行槽时返回成功。
    pub(crate) fn check_submission(&self) -> EmbeddedResult<()> {
        if ACTIVE_CALLBACK_RUNTIMES.with(|active| active.borrow().contains(&self.runtime_id)) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "synchronous runtime reentry is forbidden",
            ));
        }
        Ok(())
    }

    /// Close publication and business dispatch while retaining registered closing-stage authority.
    /// 关闭发布与业务分发，同时保留已注册关闭阶段的执行权。
    /// Actual drainage closes the broker and releases callbacks; explicit revocation still closes every stage.
    /// 真正排空后关闭队列并释放回调；显式撤销仍关闭全部阶段。
    pub(crate) fn begin_shutdown(&self) -> EmbeddedResult<()> {
        let entries = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| internal("capability registry is poisoned"))?;
            state.closing = true;
            state.active.clear();
            state.entries.values().cloned().collect::<Vec<_>>()
        };
        for entry in entries {
            entry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .finalization_only = true;
        }
        Ok(())
    }

    /// Close publication, request cancellation and retire every exact callback outside registry locks.
    /// 关闭发布、请求取消并在注册锁外退役每个精确回调。
    /// Return true only after all actual callback ownership drains; closure destructors may block.
    /// 仅在全部真实回调所有权排空后返回 true；闭包析构器可能阻塞。
    pub(crate) fn close_and_poll(&self) -> EmbeddedResult<bool> {
        let entries = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| internal("capability registry is poisoned"))?;
            state.closing = true;
            state.active.clear();
            state.entries.values().cloned().collect::<Vec<_>>()
        };
        self.broker.close();
        let mut drained = true;
        for entry in entries {
            entry.close();
            drained &= entry.status()?.drained;
        }
        Ok(drained)
    }

    /// Forget only drained `id`; reject discarding evidence of a still-running callback.
    /// 仅遗忘已排空 `id`；拒绝丢弃仍在执行的回调证据。
    pub fn forget(&self, id: &str) -> EmbeddedResult<()> {
        // Retirement status cannot reopen, so validation and removal may share this short lock.
        // 退役状态不能重新打开，因此校验与移除可以共享此短时锁。
        let entry = {
            // No native callback remains when a drained record is removed.
            // 移除已排空记录时不再有原生回调。
            let mut state = self
                .state
                .lock()
                .map_err(|_| internal("capability registry is poisoned"))?;
            if !state
                .entries
                .get(id)
                .ok_or_else(not_found)?
                .status()?
                .drained
            {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "capability registration is not drained",
                ));
            }
            state.entries.remove(id)
        };
        drop(entry);
        Ok(())
    }
}

impl Drop for CapabilityRegistry {
    /// Close every registration while retaining admitted callbacks through their unique permits.
    /// 关闭全部注册，同时通过唯一许可保留已入场回调。
    fn drop(&mut self) {
        self.broker.close();
        for entry in self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .values()
        {
            entry.close();
        }
    }
}

/// Immutable name-to-registration snapshot; authorization and unregistration remain live.
/// 不可变名称到注册快照；授权与注销仍保持实时。
#[derive(Clone)]
pub struct CapabilitySnapshot {
    /// Exact instance queue, never a process-global callback bridge.
    /// 精确实例队列，绝不是进程级回调桥接。
    broker: Arc<HostRequestBroker>,
    /// Owning runtime identity, checked against each trusted caller.
    /// 所属运行时身份，针对每个可信调用方检查。
    runtime_id: String,
    /// Last publication sequence captured by this snapshot.
    /// 此快照捕获的最近发布序号。
    revision: u64,
    /// Immutable captured identities cannot silently resolve to a newer implementation.
    /// 不可变捕获身份不能静默解析为更新实现。
    entries: BTreeMap<String, Arc<CapabilityEntry>>,
}

thread_local! {
    /// Native callback stack rejects synchronous recursion into the same runtime.
    /// 原生回调栈拒绝同步递归进入同一运行时。
    static ACTIVE_CALLBACK_RUNTIMES: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// Stack guard restores native reentrancy tracking even after a callback panic.
/// 即使回调 panic 后也恢复原生重入跟踪的栈保护对象。
struct CallbackScope(String);

impl Drop for CallbackScope {
    /// Remove only this invocation's exact runtime identity.
    /// 仅移除此调用的精确运行时身份。
    fn drop(&mut self) {
        ACTIVE_CALLBACK_RUNTIMES.with(|active| {
            active.borrow_mut().remove(&self.0);
        });
    }
}

impl CapabilitySnapshot {
    /// Check whether exact `name` is active and authorized by current `permissions`.
    /// 检查精确 `name` 是否活动且被当前 `permissions` 授权。
    /// Missing or denied declarations are hidden; internal failures remain explicit.
    /// 隐藏缺失或拒绝的声明；内部失败仍显式报告。
    pub fn has(&self, name: &str, permissions: &CapabilityPermissions) -> EmbeddedResult<bool> {
        let Some(entry) = self.entries.get(name) else {
            return Ok(false);
        };
        match permissions.require(&entry.descriptor.permissions) {
            Ok(()) => Ok(entry.status()?.accepting),
            Err(error) if error.code == EmbeddedErrorCode::PermissionDenied => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Return the trusted runtime namespace captured with this immutable snapshot.
    /// 返回与此不可变快照一同捕获的可信运行时命名空间。
    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    /// Invoke exact `name` through its declared transport using host-authenticated context.
    /// 使用宿主认证上下文，通过声明的传输调用精确 `name`。
    /// Queued work waits on its existing VM worker until the actual host handler acknowledges completion.
    /// 队列任务在已有 VM 工作线程等待，直到真实宿主处理器确认完成。
    pub fn invoke(
        &self,
        name: &str,
        caller: CapabilityCaller,
        permissions: Arc<CapabilityPermissions>,
        arguments: Value,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<CapabilityOutcome> {
        // Resolve the transport from this precise registration, never by trying another callback path.
        // 从此精确注册解析传输，绝不尝试其他回调路径。
        let execution = self
            .entries
            .get(name)
            .ok_or_else(not_found)?
            .descriptor
            .execution;
        // Keep only scalar timing on the VM stack; diagnostics are emitted after Lua returns.
        // VM 栈内仅保留标量计时；诊断在 Lua 返回后发送。
        let _diagnostic_wait = control.measure_diagnostic_host_wait();
        match execution {
            CapabilityExecution::Native => {
                self.invoke_native(name, caller, permissions, arguments, control)
            }
            CapabilityExecution::Queued => {
                // Submission failure proves no handler was dispatched; a later control failure does not.
                // 提交失败证明未分发处理器；后续控制失败则不能证明。
                let handle = self.submit_queued(name, caller, permissions, arguments, control)?;
                Ok(handle.wait().unwrap_or_else(|error| CapabilityOutcome {
                    result: Err(error),
                    effects: EffectState::Unknown,
                }))
            }
        }
    }

    /// Prepare exact `name` for declared `execution` using authenticated context and original budget.
    /// 使用已认证上下文与原始预算，为声明的 `execution` 准备精确 `name`。
    /// Return unique ownership before any host implementation can execute.
    /// 在任何宿主实现可以执行前返回唯一所有权。
    fn prepare(
        &self,
        name: &str,
        caller: CapabilityCaller,
        permissions: Arc<CapabilityPermissions>,
        arguments: Value,
        control: Arc<CallControl>,
        execution: CapabilityExecution,
    ) -> EmbeddedResult<PreparedCapability> {
        caller.validate(&self.runtime_id)?;
        // The snapshot resolves one exact identity with no fallback to current registry contents.
        // 快照解析单个精确身份，不回退到当前注册表内容。
        let entry = Arc::clone(self.entries.get(name).ok_or_else(not_found)?);
        if entry.descriptor.execution != execution {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Unsupported,
                "capability dispatch does not match its declared transport",
            ));
        }
        if entry.descriptor.scope == CapabilityScope::Session && caller.session_id.is_none() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::PermissionDenied,
                "capability requires a bound session",
            ));
        }
        permissions.require(&entry.descriptor.permissions)?;
        json_size(&arguments, entry.descriptor.max_input_bytes)?;
        entry.input.validate(&arguments)?;
        // Admission retains capacity across every dispatched native or SDK handler.
        // 入场在全部已分发原生或 SDK 处理器期间保留容量。
        let finalization = control.is_finalization()?;
        let admitted = entry.admit(finalization)?;
        // Retention failure is detected before native execution or SDK publication can produce effects.
        // 在原生执行或 SDK 发布可能产生副作用前检测保留失败。
        let effect = control.reserve_effect(
            &caller,
            &entry.id,
            &entry.descriptor.name,
            &entry.descriptor.version,
        )?;
        // Child duration cannot extend the original operation deadline.
        // 子时长不能延长原始操作截止时间。
        let invocation = CapabilityInvocation {
            effect_id: effect.id().map(str::to_owned),
            caller,
            arguments,
            budget: CapabilityBudget::new(control, entry.descriptor.max_call_ms)?,
            permissions,
            required: entry.descriptor.permissions.clone(),
        };
        invocation.authorize()?;
        Ok(PreparedCapability {
            finalization,
            entry,
            invocation,
            admitted,
            effect,
        })
    }

    /// Submit queued `name` without blocking the SDK event pump or allocating a new OS thread.
    /// 提交队列 `name`，不阻塞 SDK 事件泵且不分配新操作系统线程。
    /// The returned handle owns the core waiter while dispatched handlers retain their own lifetime.
    /// 返回句柄拥有核心等待方，已分发处理器保留独立生命周期。
    pub fn submit_queued(
        &self,
        name: &str,
        caller: CapabilityCaller,
        permissions: Arc<CapabilityPermissions>,
        arguments: Value,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<HostRequestHandle> {
        // A native handler must not synchronously wait on its own runtime's SDK pump.
        // 原生处理器不得同步等待自身运行时的 SDK 事件泵。
        if ACTIVE_CALLBACK_RUNTIMES.with(|active| active.borrow().contains(&self.runtime_id)) {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "synchronous capability reentry is forbidden",
            ));
        }
        self.broker.submit(self.prepare(
            name,
            caller,
            permissions,
            arguments,
            control,
            CapabilityExecution::Queued,
        )?)
    }

    /// Return an opaque snapshot revision suitable for immutable VM matching.
    /// 返回适合不可变 VM 匹配的不透明快照修订。
    pub fn revision(&self) -> String {
        format!("{}:snapshot:{}", self.runtime_id, self.revision)
    }

    /// List only active declarations visible through current `permissions`.
    /// 仅列出通过当前 `permissions` 可见的活动声明。
    pub fn list(
        &self,
        permissions: &CapabilityPermissions,
    ) -> EmbeddedResult<Vec<CapabilityDescriptor>> {
        // Authorization failures hide a descriptor; internal failures remain explicit.
        // 授权失败隐藏描述；内部失败仍明确报告。
        let mut descriptors = Vec::new();
        for entry in self.entries.values() {
            match permissions.require(&entry.descriptor.permissions) {
                Ok(()) if entry.status()?.accepting => descriptors.push(entry.descriptor.clone()),
                Ok(()) => {}
                Err(error) if error.code == EmbeddedErrorCode::PermissionDenied => {}
                Err(error) => return Err(error),
            }
        }
        Ok(descriptors)
    }

    /// Invoke exact native `name` with trusted `caller`, live grants, structured `arguments` and original `control`.
    /// 使用可信 `caller`、实时授权、结构化 `arguments` 与原始 `control` 调用精确原生 `name`。
    /// Return actual effect evidence; cancellation never rewrites a confirmed commit.
    /// 返回真实副作用证据；取消绝不改写已确认提交。
    pub fn invoke_native(
        &self,
        name: &str,
        caller: CapabilityCaller,
        permissions: Arc<CapabilityPermissions>,
        arguments: Value,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<CapabilityOutcome> {
        if !ACTIVE_CALLBACK_RUNTIMES
            .with(|active| active.borrow_mut().insert(self.runtime_id.clone()))
        {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "synchronous capability reentry is forbidden",
            ));
        }
        // Scope restores reentry state on admission failure, callback error and panic.
        // 作用域在入场失败、回调错误与 panic 时恢复重入状态。
        let _scope = CallbackScope(self.runtime_id.clone());
        // Native and queued transports share exactly the same identity, schema and grant checks.
        // 原生与队列传输共享完全相同的身份、Schema 与授权检查。
        let PreparedCapability {
            finalization,
            entry,
            invocation,
            admitted,
            effect,
        } = self.prepare(
            name,
            caller,
            permissions,
            arguments,
            control,
            CapabilityExecution::Native,
        )?;
        // Publication validated callback presence for this exact transport.
        // 发布已针对精确传输校验回调存在性。
        let callback = admitted
            .native
            .as_ref()
            .expect("native registration owns its validated callback");
        // Disk acknowledgement precedes dispatch, and waiting cannot extend the original authority or deadline.
        // 磁盘确认先于分发，等待不能延长原始权限或截止时间。
        effect.checkpoint_start(true)?;
        invocation.authorize()?;
        entry.with_dispatch_gate(finalization, || effect.begin())??;
        // Fixed panic diagnostics do not expose private callback values.
        // 固定 panic 诊断不暴露私有回调值。
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(&invocation)))
                .unwrap_or_else(|_| CapabilityOutcome {
                    result: Err(EmbeddedError::new(
                        EmbeddedErrorCode::ExecutionFailed,
                        "native capability panicked",
                    )),
                    effects: if entry.descriptor.effects == CapabilityEffects::ReadOnly {
                        EffectState::NotApplicable
                    } else {
                        EffectState::Unknown
                    },
                });
        // Cancellation or invalid output must preserve actual commit evidence.
        // 取消或无效输出必须保留真实提交证据。
        effect.observe(outcome.effects);
        let mut outcome = entry.validate_outcome(outcome);
        // Retain the bounded original result and actual admission through disk failure and explicit recovery.
        // 跨磁盘失败及显式恢复保留有界原始结果和真实入场许可。
        effect.confirm_native_outcome()?;
        if let Err(error) = invocation.authorize() {
            outcome.result = Err(error);
        }
        drop(admitted);
        drop(effect);
        Ok(outcome)
    }
}

/// Produce a fixed internal diagnostic without copying callback arguments or secrets.
/// 生成固定内部诊断，不复制回调参数或秘密。
fn internal(message: &str) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::Internal, message)
}

/// Report absence from the exact requested registry or snapshot.
/// 报告精确请求注册表或快照中的缺失。
fn not_found() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::NotFound,
        "capability identity is unknown or expired",
    )
}
