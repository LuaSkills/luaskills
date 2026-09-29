//! Host-owned plugin execution, independent of the public lease manager.
//! 由宿主拥有的插件执行机制，与公开租约管理器独立。

pub mod capabilities;
mod capacity;
mod cleanup;
mod config;
mod control;
mod effects;
mod error;
mod governor;
mod identity;
mod journal;
mod module;
mod operations;
mod plugin_config;
mod pool;
mod resources;
mod retirement;
mod scheduler;
mod schema;
mod value_size;

#[cfg(test)]
mod tests;

pub use crate::runtime::engine::EmbeddedModule;
pub use capacity::VmCapacityConfig;
pub use cleanup::{
    ModuleAcquireFailure, ModuleRelease, ModuleRetirement, ModuleRetirementPhase,
    ModuleRetirementSnapshot,
};
pub use config::{
    EmbeddedRuntimeConfig, ExecutionBackend, InstanceReuse, PluginPoolConfig, PoolKind,
};
pub use control::CallControl;
pub use effects::{HostEffectPhase, HostEffectRecord};
pub use error::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
pub use governor::VmCapacitySnapshot;
pub use governor::{ExecutionPermit, PoolGovernor, PoolUsage, VmAllocationState, VmReservation};
pub(crate) use identity::IdentityKind;
pub use journal::{
    HostEffectReconciliation, JournalOperation, JournalWritePhase, JournalWriteReceipt,
    JournalWriteSnapshot, OperationJournal, OperationJournalConfig, OperationJournalWorker,
    OperationJournalWorkerConfig, OperationJournalWorkerStatus, OperationReconciliation,
    ReconciledExecution, ResolvedEffectState,
};
pub use module::{ModuleDefinition, ModuleExport, ModuleFinalizer, ModuleInvocation};
pub use operations::{
    EffectState, ModuleOperationContext, OperationContext, OperationFinalization, OperationHandle,
    OperationOutcome, OperationOwner, OperationPage, OperationPhase, OperationRegistry,
    OperationSnapshot,
};
pub use plugin_config::EmbeddedPluginConfig;
pub use pool::{EmbeddedPoolManager, ModuleLease, ModulePool, ModulePoolPlacement};
pub use resources::ModuleResourceOwner;
pub use scheduler::{
    CheckpointRetryState, EmbeddedCall, EmbeddedCapacityConfig, EmbeddedCapacityPolicySnapshot,
    EmbeddedCapacitySnapshot, EmbeddedPluginSnapshot, EmbeddedPrewarm,
    EmbeddedReusablePoolSnapshot, EmbeddedRuntime, EmbeddedRuntimeUsage, EmbeddedSessionOpening,
    EmbeddedSessionPhase, EmbeddedSessionSnapshot, OperationPersistenceFailure,
};
pub use schema::{EMBEDDED_SCHEMA_DIALECT, JsonContract};
pub use value_size::json_size;
