//! Instance-scoped host capabilities with explicit authorization and callback ownership.
//! 具有显式授权与回调所有权的实例级宿主能力。

mod broker;
mod registry;
mod types;

#[cfg(test)]
mod tests;

pub use broker::{
    HostRequest, HostRequestBroker, HostRequestHandle, HostRequestPhase, HostRequestStatus,
};
pub use registry::{
    CapabilityInvocation, CapabilityRegistrationRequest, CapabilityRegistrationStatus,
    CapabilityRegistry, CapabilitySnapshot, NativeCapability,
};
pub use types::{
    CapabilityBudget, CapabilityCaller, CapabilityDescriptor, CapabilityEffects,
    CapabilityExecution, CapabilityIdempotency, CapabilityOutcome, CapabilityPermissions,
    CapabilityScope,
};
