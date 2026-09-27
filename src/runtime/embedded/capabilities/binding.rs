use super::{CapabilityCaller, CapabilityPermissions, CapabilitySnapshot};
use crate::runtime::embedded::{EmbeddedError, EmbeddedResult, ModuleDefinition};
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
        })
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
            workspace_root: definition.workspace_root.clone(),
        };
        caller.validate(self.snapshot.runtime_id())?;
        Ok(caller)
    }
}
