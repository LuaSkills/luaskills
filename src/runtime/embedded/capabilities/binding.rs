use super::{CapabilityCaller, CapabilityPermissions, CapabilitySnapshot};
use crate::runtime::embedded::{
    EmbeddedError, EmbeddedErrorCode, EmbeddedResult, ModuleDefinition,
};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Immutable module authority binding; business arguments cannot replace these capabilities or grants.
/// 不可变模块权威绑定；业务参数不能替换这些能力或授权。
#[derive(Clone)]
pub struct ModuleCapabilities {
    /// Exact registration identities retained for this module generation.
    /// 为此模块代次保留的精确注册身份。
    pub(crate) snapshot: CapabilitySnapshot,
    /// Live revocation authority owned by the host.
    /// 由宿主拥有的实时撤权权威。
    pub(crate) permissions: Arc<CapabilityPermissions>,
    /// Immutable configuration and initialization revision for instance matching.
    /// 用于实例匹配的不可变配置与初始化修订。
    execution_revision: String,
    /// Optional immutable narrowing for source initialization; empty denies all host callbacks.
    /// 可选的源码初始化不可变收窄集合；空集合拒绝全部宿主回调。
    initialization_capabilities: Option<Arc<BTreeSet<String>>>,
}

impl ModuleCapabilities {
    /// Bind `snapshot` and live `permissions` to nonempty immutable `execution_revision`.
    /// 将 `snapshot` 与实时 `permissions` 绑定到非空不可变 `execution_revision`。
    /// Return an error before allocating a VM when the revision cannot identify an execution domain.
    /// 若修订无法标识执行域，则在分配 VM 前返回错误。
    pub fn new(
        snapshot: CapabilitySnapshot,
        permissions: Arc<CapabilityPermissions>,
        execution_revision: String,
    ) -> EmbeddedResult<Self> {
        if execution_revision.trim().is_empty() || execution_revision.contains('\0') {
            return Err(EmbeddedError::invalid(
                "module execution revision must be nonempty without NUL",
            ));
        }
        Ok(Self {
            snapshot,
            permissions,
            execution_revision,
            initialization_capabilities: None,
        })
    }

    /// Freeze exact callback `names` for source initialization without granting any business permission.
    /// 冻结源码初始化所用的精确回调 `names`，不授予任何业务权限。
    /// Return a narrowed binding; reject unavailable or unauthorized members before VM allocation.
    /// 返回收窄绑定；在 VM 分配前拒绝不可用或未授权成员。
    /// An empty set denies all initialization callbacks; unchanged bindings retain their original policy.
    /// 空集合拒绝全部初始化回调；未修改绑定保留其原策略。
    pub fn with_initialization_capabilities(
        mut self,
        names: BTreeSet<String>,
    ) -> EmbeddedResult<Self> {
        for name in &names {
            if !self.snapshot.has(name, &self.permissions)? {
                return Err(EmbeddedError::new(
                    EmbeddedErrorCode::PermissionDenied,
                    "initialization capability is unavailable or unauthorized",
                ));
            }
        }
        self.initialization_capabilities = Some(Arc::new(names));
        Ok(self)
    }

    /// Borrow the frozen initialization names; absence preserves the explicitly inherited business policy.
    /// 借用冻结初始化名称；省略表示保留显式继承的业务策略。
    pub fn initialization_capabilities(&self) -> Option<&BTreeSet<String>> {
        self.initialization_capabilities.as_deref()
    }

    /// Check exact `name` against initialization narrowing; live permissions remain independently enforced.
    /// 针对初始化收窄集合检查精确 `name`；实时权限仍独立执行。
    /// Return false for excluded names even when their descriptors require no general permissions.
    /// 即使描述符无需通用权限，被排除名称仍返回假。
    pub(crate) fn allows_initialization(&self, name: &str) -> bool {
        self.initialization_capabilities
            .as_ref()
            .is_none_or(|names| names.contains(name))
    }

    /// Return the exact snapshot identity used when matching resident instances.
    /// 返回匹配常驻实例时使用的精确快照身份。
    pub fn snapshot_revision(&self) -> String {
        self.snapshot.revision()
    }

    /// Return the immutable host configuration revision.
    /// 返回不可变宿主配置修订。
    pub fn execution_revision(&self) -> &str {
        &self.execution_revision
    }

    /// Derive authority from `definition` and explicit operation/session identity, never Lua arguments.
    /// 从 `definition` 与显式操作及会话身份派生权威，绝不使用 Lua 参数。
    pub(crate) fn caller(
        &self,
        definition: &ModuleDefinition,
        operation_id: String,
        session_id: Option<String>,
        request_id: Option<String>,
    ) -> EmbeddedResult<CapabilityCaller> {
        // Clone only trusted identity fields; module source and application arguments are excluded.
        // 仅克隆可信身份字段；不包含模块源码与应用参数。
        let caller = CapabilityCaller {
            runtime_id: self.snapshot.runtime_id().to_owned(),
            plugin_id: definition.plugin_id.clone(),
            package_generation: definition.generation.clone(),
            execution_revision: self.execution_revision.clone(),
            security_partition: definition.security_partition.clone(),
            operation_id,
            session_id,
            request_id,
            workspace_root: definition.workspace_root.clone(),
        };
        caller.validate(self.snapshot.runtime_id())?;
        Ok(caller)
    }
}
