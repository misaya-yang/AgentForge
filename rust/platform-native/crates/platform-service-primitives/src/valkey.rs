//! Explicit coordination/cache boundary. The legacy Redis handle may only alias coord.
//! No process environment or credential values are retained in error messages.
pub mod acl;
mod config;
mod policy;
mod transport;
mod tls_errors;
pub use config::{EndpointConfig, Instance, ServiceIdentity, SplitConfig, ValkeyError};
pub use policy::{
    coordination_decision, memory_pressure_warning, verify_server, CacheKey, CacheKind,
    CoordinationDecision,
};
pub use transport::{CacheAvailability, CacheConnection, CoordConnection, SplitConnections};
#[cfg(test)]
mod tests;

#[cfg(test)]
mod real_tests;
