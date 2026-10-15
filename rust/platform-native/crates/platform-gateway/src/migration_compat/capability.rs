//! Durable private execution authority; proof contract is shared with content brokers.
use super::common::{CompatError, hex, text};
use crate::migration_content::capabilities::CapabilityAuthorization;
pub use crate::migration_content::capabilities::CapabilityScope;
use axum::http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
pub fn canonical_json(value: &Value) -> String {
    String::from_utf8(
        crate::migration_content::capabilities::canonical_json(value)
            .expect("JSON value serializes"),
    )
    .expect("JSON is UTF-8")
}
pub fn body_hash(value: &Value) -> String {
    hex(&Sha256::digest(canonical_json(value).as_bytes()))
}
pub struct CapabilityAuthority {
    authorization: CapabilityAuthorization,
    pub pool: PgPool,
}
impl CapabilityAuthority {
    pub fn new(pool: PgPool, internal_token: String, proof_secret: Vec<u8>) -> Self {
        Self {
            pool,
            authorization: CapabilityAuthorization::new(internal_token, proof_secret),
        }
    }
    pub fn verify(
        &self,
        headers: &HeaderMap,
        path: &str,
        body: &Value,
    ) -> Result<CapabilityScope, CompatError> {
        self.authorization
            .authorize(headers, path, body, chrono::Utc::now().timestamp())
            .map_err(|error| CompatError::value(error.status, error.detail))
    }
    pub async fn active_execution(
        &self,
        scope: &CapabilityScope,
        capability: &str,
        effect: &str,
        approval: &str,
        arguments_hash: &str,
    ) -> Result<Value, CompatError> {
        if uuid::Uuid::parse_str(&scope.execution_id).is_err()
            || uuid::Uuid::parse_str(&scope.run_id).is_err()
            || scope.tool_call_id.is_empty()
            || scope.tool_call_id.len() > 255
            || scope.tool_call_id.chars().any(|c| c < ' ')
        {
            return Err(CompatError::new(403, "capability scope invalid"));
        }
        let row=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM assistant_capability_executions e WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND run_id=$5::uuid").bind(&scope.execution_id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(&scope.run_id).fetch_optional(&self.pool).await?.ok_or_else(||CompatError::new(403,"capability execution not active"))?;
        if text(&row, "capability_id") != capability
            || text(&row, "effect") != effect
            || text(&row, "approval_status") != approval
            || !matches!(text(&row, "status"), "dispatched" | "running")
            || text(&row, "tool_call_id") != scope.tool_call_id
            || text(&row, "arguments_sha256").trim() != arguments_hash.trim_start_matches("sha256:")
        {
            return Err(CompatError::new(403, "capability execution not active"));
        }
        Ok(row)
    }
}
