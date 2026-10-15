//! Executable service construction for the durable Gateway profile.
//!
//! Inputs are resolved settings and already-owned connections. No constructor
//! reads credentials, loads dotenv, migrates schema, binds a listener, or starts
//! a worker. Missing platform backends are mandatory typed inputs, not no-ops.
pub mod adapters;
pub mod auth;
pub mod compat;
pub mod configuration;
pub mod content;
pub mod core;
pub mod evaluation;
pub mod foundation;
pub mod foundation_backends;
pub mod foundation_settings;
pub mod graph;
pub mod langgraph_preparation;
pub mod models;
pub mod native_policies;
pub mod process;
pub mod runtime;
pub mod runtime_backends;
pub mod settings_projection;
pub mod startup_models;
pub mod storage;
pub mod timestamps;
pub mod usage_event_bus;
pub mod workers;

use sqlx::PgPool;
#[derive(Clone)]
pub struct DatabasePools {
    pub data: PgPool,
    /// Independent, same-server/session-pinned pool for cancellation supervision.
    pub control: PgPool,
}
#[derive(Debug, Clone, Copy)]
pub enum BootstrapError {
    Configuration(&'static str),
    Construction(&'static str),
}
impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(name) => write!(f, "Invalid gateway configuration: {name}"),
            Self::Construction(name) => write!(f, "Gateway service construction failed: {name}"),
        }
    }
}
impl std::error::Error for BootstrapError {}
/// Validate registered HTTP endpoints without resolving DNS or contacting them.
/// Endpoint-specific clients retain their stricter path/signing constraints.
pub fn endpoint(value: &str, name: &'static str) -> Result<reqwest::Url, BootstrapError> {
    let url = reqwest::Url::parse(value).map_err(|_| BootstrapError::Configuration(name))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BootstrapError::Configuration(name));
    }
    Ok(url)
}
