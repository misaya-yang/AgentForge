#[path = "../../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/workload_identity.rs"]
mod shared;
pub(crate) use shared::{
    TransportError, WorkloadIdentity, WorkloadSend, configure_environment, required, tls_enabled,
};
