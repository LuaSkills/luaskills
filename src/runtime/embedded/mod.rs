//! Host-owned plugin execution, independent of the public lease manager.
//! 由宿主拥有的插件执行机制，与公开租约管理器独立。

pub mod capabilities;
mod config;
mod control;
mod effects;
mod error;
mod governor;
mod module;
mod operations;
mod pool;
mod retirement;
mod schema;
mod value_size;

#[cfg(test)]
mod tests;

pub use crate::runtime::engine::EmbeddedModule;
pub use config::{
    EmbeddedRuntimeConfig, ExecutionBackend, InstanceReuse, PluginPoolConfig, PoolKind,
};
pub use control::CallControl;
pub use effects::{HostEffectPhase, HostEffectRecord};
pub use error::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
pub use governor::{ExecutionPermit, PoolGovernor, PoolUsage, VmAllocationState, VmReservation};
pub use module::{ModuleDefinition, ModuleExport, ModuleInvocation};
pub use operations::{
    EffectState, OperationHandle, OperationOwner, OperationPhase, OperationRegistry,
    OperationSnapshot,
};
pub use pool::{EmbeddedPoolManager, ModuleLease, ModulePool};
pub use schema::{EMBEDDED_SCHEMA_DIALECT, JsonContract};
