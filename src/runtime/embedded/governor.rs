use super::{
    EmbeddedError, EmbeddedErrorCode, EmbeddedResult, EmbeddedRuntimeConfig, PluginPoolConfig,
    VmCapacityConfig,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

mod capacity;
pub use capacity::VmCapacitySnapshot;

/// Observable lifecycle of one resident allocation, including failed retirement.
/// 单个常驻分配的可观察生命周期，包含失败退役。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmAllocationState {
    /// The VM is being initialized and already occupies its resident budget.
    /// VM 正在初始化且已占用常驻预算。
    Creating,
    /// The initialized VM is idle, possibly pinned to a session.
    /// 初始化完成的 VM 处于空闲状态，可能固定在会话上。
    Idle,
    /// The VM owns an execution permit, including host-result waiting time.
    /// VM 持有执行许可，包含等待宿主结果的时间。
    Running,
    /// Resource teardown has not yet completed; capacity remains charged.
    /// 资源清理尚未完成；容量仍然记账。
    Retiring,
}

/// Current counters for one governor or one execution group.
/// 单个治理器或执行分组的当前计数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct PoolUsage {
    /// All allocated slots, including creating and retiring instances.
    /// 全部分配槽位，包含正在创建与退役的实例。
    pub resident: usize,
    /// Allocations that have not finished initialization.
    /// 尚未完成初始化的分配。
    pub creating: usize,
    /// Initialized allocations that do not currently execute.
    /// 已初始化且当前未执行的分配。
    pub idle: usize,
    /// Allocations holding a parent and group execution permit together.
    /// 同时持有父级与分组执行许可的分配。
    pub running: usize,
    /// Allocations retained until teardown has genuinely completed.
    /// 保留到清理真正完成的分配。
    pub retiring: usize,
}

/// Private allocation identity retained under a single metadata lock.
/// 在单个元数据锁下保留的私有分配身份。
struct Allocation {
    /// Host-assigned immutable execution-group identity.
    /// 宿主分配的不可变执行分组身份。
    group: String,
    /// Current lifecycle phase, never inferred from queue position.
    /// 当前生命周期阶段，从不根据队列位置推断。
    phase: VmAllocationState,
}

/// Metadata only: no Lua values, destructors, host callbacks, or user code.
/// 仅包含元数据：不含 Lua 值、析构器、宿主回调或用户代码。
struct GovernorState {
    /// Validated immutable policies indexed by explicit execution-group identity.
    /// 按显式执行分组身份索引的已校验不可变策略。
    groups: BTreeMap<String, PluginPoolConfig>,
    /// Persistent capacity ownership outlives individual module groups and their package generations.
    /// 持久容量归属比单个模块分组及其包代次存活更久。
    capacities: BTreeMap<String, VmCapacityConfig>,
    /// Each grouped module belongs to exactly one explicitly registered capacity owner.
    /// 每个已分组模块精确属于单个显式注册的容量所有者。
    memberships: BTreeMap<String, String>,
    /// Live allocations counted through real teardown.
    /// 记账到实际清理结束的活跃分配。
    allocations: BTreeMap<u64, Allocation>,
    /// Monotonic private slot sequence; exhaustion is an explicit error.
    /// 私有单调槽位序号；耗尽时明确报错。
    sequence: u64,
}

/// Atomic parent/group resource authority shared explicitly by plugin runtimes.
/// 由插件运行时显式共享的父级与分组原子资源权威。
pub struct PoolGovernor {
    /// Validated parent budgets never silently enlarged by group admission.
    /// 不会因分组入场而静默扩大的已校验父级预算。
    config: EmbeddedRuntimeConfig,
    /// One short metadata lock makes multi-level admission atomic.
    /// 单个短时元数据锁使多层入场决策保持原子性。
    state: Mutex<GovernorState>,
}

impl PoolGovernor {
    /// Construct an empty governor from validated explicit `config` budgets.
    /// 根据已校验的显式 `config` 预算构造空治理器。
    /// Return shared ownership without starting threads or allocating any VM.
    /// 返回共享所有权，不启动线程且不分配任何 VM。
    pub fn new(config: EmbeddedRuntimeConfig) -> EmbeddedResult<Arc<Self>> {
        config.validate()?;
        Ok(Arc::new(Self {
            config,
            state: Mutex::new(GovernorState {
                groups: BTreeMap::new(),
                capacities: BTreeMap::new(),
                memberships: BTreeMap::new(),
                allocations: BTreeMap::new(),
                sequence: 0,
            }),
        }))
    }

    /// Register immutable `policy` for `group`; reject conflicts or impossible reservations.
    /// 为 `group` 注册不可变 `policy`；拒绝冲突或无法满足的预留。
    /// Return success without changing any existing group's effective policy.
    /// 成功返回时不改变任何既有分组的生效策略。
    pub fn register_group(&self, group: &str, policy: PluginPoolConfig) -> EmbeddedResult<()> {
        self.register_group_internal(group, policy, None)
    }

    /// Remove `group` only when every real allocation has already been released.
    /// 仅在全部真实分配已经释放后移除 `group`。
    /// Return a busy error for creating, active, idle, or failed-retirement instances.
    /// 对创建中、活动、空闲或退役失败的实例返回忙碌错误。
    pub fn unregister_group(&self, group: &str) -> EmbeddedResult<()> {
        // Unregistration and concurrent allocation are serialized by this metadata guard.
        // 此元数据保护串行化注销与并发分配。
        let mut state = self.lock()?;
        if !state.groups.contains_key(group) {
            return Err(not_found("execution group does not exist"));
        }
        if group_usage(&state, group).resident != 0 {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "execution group still owns resident allocations",
            ));
        }
        state.groups.remove(group);
        state.memberships.remove(group);
        Ok(())
    }

    /// Reserve one creating VM for `group`, charging parent and group atomically.
    /// 为 `group` 预留一个创建中的 VM，同时原子计入父级与分组。
    /// Return a non-cloneable slot that must outlive the actual VM and its teardown.
    /// 返回不可克隆的槽位，该槽位必须比实际 VM 及其清理存活更久。
    pub fn reserve(self: &Arc<Self>, group: &str) -> EmbeddedResult<VmReservation> {
        // Admission commits all physical levels under the original metadata lock.
        // 入场在原元数据锁下提交全部物理层级。
        let mut state = self.lock()?;
        // Require an exact module group; capacity membership is immutable after registration.
        // 要求精确模块分组；容量成员关系在注册后不可变。
        let policy = state
            .groups
            .get(group)
            .ok_or_else(|| not_found("execution group does not exist"))?;
        if group_usage(&state, group).resident >= policy.max_resident_vms {
            return Err(capacity("execution group resident limit reached"));
        }
        if let Some(id) = state.memberships.get(group) {
            // Registered membership cannot outlive its capacity owner.
            // 已注册成员关系不能比其容量所有者存活更久。
            let policy = state
                .capacities
                .get(id)
                .expect("capacity remains while members exist");
            if state.capacity_usage(id).resident >= policy.max_resident_vms {
                return Err(capacity("capacity group resident limit reached"));
            }
        }
        if state.commitment(Some(group))? >= self.config.max_resident_vms {
            return Err(capacity("parent resident capacity is occupied or reserved"));
        }
        // Checked identity allocation happens before publishing metadata.
        // 在发布元数据前执行受检身份分配。
        let id = state.sequence.checked_add(1).ok_or_else(|| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "allocation identity exhausted")
        })?;
        state.sequence = id;
        state.allocations.insert(
            id,
            Allocation {
                group: group.to_owned(),
                phase: VmAllocationState::Creating,
            },
        );
        Ok(VmReservation {
            governor: Arc::clone(self),
            id,
        })
    }

    /// Return all current counters, or only those belonging to explicit `group`.
    /// 返回全部当前计数，或仅返回显式 `group` 所属计数。
    /// Missing groups fail rather than looking like empty groups.
    /// 缺失分组报错，不伪装成空分组。
    pub fn usage(&self, group: Option<&str>) -> EmbeddedResult<PoolUsage> {
        // Snapshot under the same lock as allocation and execution admission.
        // 在与分配及执行入场相同的锁下获取快照。
        let state = self.lock()?;
        if let Some(group) = group {
            if !state.groups.contains_key(group) {
                return Err(not_found("execution group does not exist"));
            }
            return Ok(group_usage(&state, group));
        }
        Ok(count_allocations(state.allocations.values()))
    }

    /// Return the immutable parent configuration as the sole budget authority.
    /// 返回不可变父级配置，作为唯一预算权威。
    pub fn config(&self) -> &EmbeddedRuntimeConfig {
        &self.config
    }

    /// Lock metadata, explicitly rejecting poisoned admission state.
    /// 锁定元数据，明确拒绝中毒的入场状态。
    /// Teardown uses a separate recovery path solely to release already-owned slots.
    /// 清理仅为释放已有槽位使用单独恢复路径。
    fn lock(&self) -> EmbeddedResult<MutexGuard<'_, GovernorState>> {
        self.state.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "pool governor lock is poisoned",
            )
        })
    }
}

/// Resident capacity token; drop only after the corresponding VM is destroyed.
/// 常驻容量令牌；仅在相应 VM 销毁后释放。
pub struct VmReservation {
    /// Strong owner prevents losing the ledger while any allocation exists.
    /// 强所有者引用防止分配尚存在时丢失账本。
    governor: Arc<PoolGovernor>,
    /// Private slot identity; callers cannot construct or clone a reservation.
    /// 私有槽位身份；调用方不能构造或克隆预留。
    id: u64,
}

impl VmReservation {
    /// Mark successful initialization ready; reject invalid lifecycle transitions.
    /// 将成功初始化标为就绪；拒绝无效生命周期转换。
    pub fn mark_ready(&mut self) -> EmbeddedResult<()> {
        self.transition(VmAllocationState::Creating, VmAllocationState::Idle)
    }

    /// Begin actual execution, atomically charging both parent and group permits.
    /// 开始实际执行，同时原子计入父级与分组许可。
    /// The returned borrow prevents retiring or destroying a still-running slot.
    /// 返回的借用阻止退役或销毁仍在执行的槽位。
    pub fn begin_execution(&mut self) -> EmbeddedResult<ExecutionPermit<'_>> {
        // Keep permit admission and phase mutation inside one metadata transaction.
        // 将许可入场与阶段变更保留在同一次元数据事务内。
        let previous = {
            // This reservation is the sole owner of its slot identity.
            // 当前预留是其槽位身份的唯一所有者。
            let mut state = self.governor.lock()?;
            // Missing records are an internal ownership defect, never a default idle slot.
            // 记录缺失属于内部所有权缺陷，绝不按默认空闲槽位处理。
            let allocation = state
                .allocations
                .get(&self.id)
                .ok_or_else(|| not_found("VM reservation no longer exists"))?;
            if !matches!(
                allocation.phase,
                VmAllocationState::Creating | VmAllocationState::Idle
            ) {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::Busy,
                    "VM is not available for execution",
                ));
            }
            // Immutable group registration cannot disappear while resident allocations exist.
            // 常驻分配存在时，不可变分组注册不能消失。
            let policy = state
                .groups
                .get(&allocation.group)
                .ok_or_else(|| not_found("execution group does not exist"))?;
            if group_usage(&state, &allocation.group).running >= policy.max_running_calls
                || count_allocations(state.allocations.values()).running
                    >= self.governor.config.max_running_calls
            {
                return Err(capacity("execution capacity is occupied"));
            }
            if let Some(id) = state.memberships.get(&allocation.group) {
                // Cross-module capacity admission is atomic with the existing parent and leaf checks.
                // 跨模块容量入场与既有父级及叶级检查保持原子性。
                let capacity_policy = state
                    .capacities
                    .get(id)
                    .expect("capacity remains while members exist");
                if state.capacity_usage(id).running >= capacity_policy.max_running_calls {
                    return Err(capacity("capacity group execution limit reached"));
                }
            }
            // The permit restores the exact original construction or idle state.
            // 许可恢复精确原构造或空闲状态。
            let previous = allocation.phase;
            state
                .allocations
                .get_mut(&self.id)
                .expect("owned allocation exists under the same lock")
                .phase = VmAllocationState::Running;
            previous
        };
        Ok(ExecutionPermit {
            reservation: self,
            previous,
        })
    }

    /// Retain capacity while owned resources are being retired or cleanup is retried.
    /// 所属资源退役或重试清理时保留容量。
    /// A live execution permit prevents calling this method through exclusive borrowing.
    /// 活跃执行许可通过独占借用阻止调用此方法。
    pub fn mark_retiring(&mut self) -> EmbeddedResult<()> {
        // No destructor runs inside the metadata lock.
        // 元数据锁内不运行任何析构器。
        let mut state = self.governor.lock()?;
        // Access the sole owned allocation rather than searching candidate identities.
        // 访问唯一所属分配，不搜索候选身份。
        let allocation = state
            .allocations
            .get_mut(&self.id)
            .ok_or_else(|| not_found("VM reservation no longer exists"))?;
        if allocation.phase == VmAllocationState::Running {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "VM is still running",
            ));
        }
        allocation.phase = VmAllocationState::Retiring;
        Ok(())
    }

    /// Apply one exact `expected` to `next` lifecycle transition.
    /// 应用一次精确的 `expected` 到 `next` 生命周期转换。
    fn transition(
        &mut self,
        expected: VmAllocationState,
        next: VmAllocationState,
    ) -> EmbeddedResult<()> {
        // One lock protects both validation and mutation.
        // 同一锁保护校验与变更。
        let mut state = self.governor.lock()?;
        // The private token addresses exactly one allocation.
        // 私有令牌精确指向一个分配。
        let allocation = state
            .allocations
            .get_mut(&self.id)
            .ok_or_else(|| not_found("VM reservation no longer exists"))?;
        if allocation.phase != expected {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Busy,
                "VM lifecycle transition is invalid",
            ));
        }
        allocation.phase = next;
        Ok(())
    }
}

impl Drop for VmReservation {
    /// Release metadata after its owner has destroyed the actual VM and resources.
    /// 在所有者销毁实际 VM 与资源后释放元数据。
    fn drop(&mut self) {
        // Poison recovery is restricted to releasing an already-owned allocation.
        // 中毒恢复仅用于释放已有分配。
        let mut state = self
            .governor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.allocations.remove(&self.id);
    }
}

/// Exclusive running permit whose lifetime is bounded by its resident reservation.
/// 生命周期受常驻预留约束的独占运行许可。
pub struct ExecutionPermit<'a> {
    /// Exclusive reservation borrow prevents premature resident-capacity release.
    /// 预留独占借用阻止提前释放常驻容量。
    reservation: &'a mut VmReservation,
    /// Pre-execution phase restored after initialization or invocation finishes.
    /// 初始化或调用完成后恢复的执行前阶段。
    previous: VmAllocationState,
}

impl Drop for ExecutionPermit<'_> {
    /// Return both execution permits together while keeping resident capacity.
    /// 同时归还两级执行许可，并保留常驻容量。
    fn drop(&mut self) {
        // Recovery only unwinds the exact permit that this value owns.
        // 恢复仅撤销此值拥有的精确许可。
        let mut state = self
            .reservation
            .governor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .allocations
            .get_mut(&self.reservation.id)
            .expect("execution permit exclusively borrows its live reservation")
            .phase = self.previous;
    }
}

/// Count the supplied allocation iterator without retaining references after the snapshot.
/// 统计给定分配迭代器，在快照完成后不保留引用。
fn count_allocations<'a>(allocations: impl Iterator<Item = &'a Allocation>) -> PoolUsage {
    // Counters are derived from the sole allocation map, never independently incremented.
    // 计数从唯一分配映射派生，从不独立递增。
    let mut usage = PoolUsage::default();
    for allocation in allocations {
        usage.resident += 1;
        match allocation.phase {
            VmAllocationState::Creating => usage.creating += 1,
            VmAllocationState::Idle => usage.idle += 1,
            VmAllocationState::Running => usage.running += 1,
            VmAllocationState::Retiring => usage.retiring += 1,
        }
    }
    usage
}

/// Snapshot only allocations that belong to exact `group` in `state`.
/// 仅快照 `state` 中精确属于 `group` 的分配。
fn group_usage(state: &GovernorState, group: &str) -> PoolUsage {
    count_allocations(
        state
            .allocations
            .values()
            .filter(|allocation| allocation.group == group),
    )
}

/// Return a stable capacity error with the supplied English `message`.
/// 返回使用给定英文 `message` 的稳定容量错误。
fn capacity(message: &str) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::CapacityExceeded, message)
}

/// Return a stable identity error with the supplied English `message`.
/// 返回使用给定英文 `message` 的稳定身份错误。
fn not_found(message: &str) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::NotFound, message)
}
