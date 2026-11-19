//! Explicit policy routing adapter, mounted only after schema admission.
// SPDX-License-Identifier: Apache-2.0
mod adapter;
pub mod http;
mod postgres;
pub use adapter::{CandidateSet, QuoteWorkload, pricing_matches_decision};
use ai_platform_auth::admission::RequestPrincipal;
pub use ai_platform_gateway_foundation::routing_policy::*;
use sqlx::PgPool;
#[derive(Clone)]
pub struct RoutingPolicyService {
    pool: PgPool,
}
/// Constructed only from the existing verified request principal, never wire JSON.
pub struct RoutingAuthority {
    tenant: String,
    actor: String,
    roles: Vec<String>,
    tier: String,
    allowed_models: Vec<String>,
    manage: bool,
}
impl RoutingAuthority {
    pub fn from_principal(p: &RequestPrincipal) -> Result<Self> {
        if p.tenant_id().is_empty() || p.user_id().is_empty() {
            return Err(RoutingError::Forbidden);
        }
        Ok(Self {
            tenant: p.tenant_id().into(),
            actor: p.user_id().into(),
            roles: p.roles().to_vec(),
            tier: p.tier().into(),
            allowed_models: p
                .api_key()
                .map(|k| k.allowed_models.clone())
                .unwrap_or_default(),
            manage: p.roles().iter().any(|r| r == "admin")
                || p.permissions().iter().any(|s| s == "routing:manage"),
        })
    }
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn actor(&self) -> &str {
        &self.actor
    }
}
impl RoutingPolicyService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
fn storage(_: sqlx::Error) -> RoutingError {
    RoutingError::Storage
}

/// Call only with the persisted/read-back result, before dispatch. Incoming
/// client headers are never accepted as a decision reference.
pub fn decision_header(decision: &Decision) -> Result<axum::http::HeaderValue> {
    decision.verify()?;
    axum::http::HeaderValue::from_str(&decision.id).map_err(|_| RoutingError::InvalidSnapshot)
}
