//! Gateway-owned Agent Studio and Runtime bridge. The existing runtime remains
//! the sole owner of the Agent loop, turn ordering and approval consumption.
pub mod analytics;
pub mod approval;
pub mod sdk_approvals;
pub mod capture;
pub mod control;
pub mod deletion;
pub mod deletion_contract;
pub mod entry;
pub mod entry_request;
mod event_transport;
pub mod governance;
pub mod history;
pub mod idempotency;
mod instructions;
pub mod memory_cleanup;
pub mod model;
pub mod model_projection;
pub mod public;
pub mod published_attachments;
pub mod release;
pub mod release_contract;
pub mod resources;
pub mod routes;
pub mod scheduled;
pub mod runtime_contract;
pub mod runtime_envelope;
pub mod readonly_capabilities;
pub mod spec;
pub mod studio;
pub mod threads;

use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
pub use routes::{AgentState, router};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
pub type AgentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AgentError>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct AgentError {
    pub status: u16,
    pub detail: Value,
}
impl AgentError {
    pub fn code(status: u16, code: impl Into<String>) -> Self {
        Self {
            status,
            detail: json!({"code":code.into()}),
        }
    }
    pub fn validation(errors: Vec<Value>) -> Self {
        Self {
            status: 422,
            detail: json!({"code":"AGENT_SPEC_INVALID","errors":errors}),
        }
    }
    pub fn conflict(revision: i64) -> Self {
        Self {
            status: 409,
            detail: json!({"code":"AGENT_DRAFT_CONFLICT","current_revision":revision}),
        }
    }
}
impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.detail
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("AGENT_ERROR")
        )
    }
}
impl std::error::Error for AgentError {}
impl From<sqlx::Error> for AgentError {
    fn from(error: sqlx::Error) -> Self {
        let db = error.as_database_error();
        if db.is_some_and(|e| e.message().contains("AGENT_LAST_OWNER")) {
            return Self::code(409, "AGENT_LAST_OWNER");
        }
        if db.and_then(|e| e.code()).as_deref() == Some("23505") {
            return Self::code(409, "AGENT_CONFLICT");
        }
        Self::code(503, "AGENT_STORAGE_UNAVAILABLE")
    }
}
impl IntoResponse for AgentError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":self.detail})),
        )
            .into_response()
    }
}

/// This value is constructed by authenticated host authority, never from JSON.
#[derive(Clone, Default)]
pub struct Actor {
    pub tenant_id: String,
    pub user_id: String,
    pub roles: Vec<String>,
    pub tier: String,
}
impl Actor {
    pub fn is_admin(&self) -> bool {
        self.tier.eq_ignore_ascii_case("admin")
            || self
                .roles
                .iter()
                .any(|r| matches!(r.to_lowercase().as_str(), "admin" | "tenant_admin"))
    }
    pub fn require_tenant(&self) -> Result<(), AgentError> {
        if self.user_id.is_empty() {
            return Err(AgentError::code(401, "AUTHENTICATION_REQUIRED"));
        }
        if self.tenant_id.is_empty() || self.tenant_id == "public" {
            return Err(AgentError::code(403, "TENANT_REQUIRED"));
        }
        Ok(())
    }
    pub fn scope(&self, session_id: impl Into<String>) -> Scope {
        Scope {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: session_id.into(),
        }
    }
    pub async fn resolve(
        auth: &AdmissionService,
        headers: &HeaderMap,
        path: &str,
    ) -> Result<Self, AgentError> {
        let request = auth.request(headers, path);
        let principal = request
            .resolve()
            .await
            .map_err(|e| AgentError::code(e.status(), "AUTHENTICATION_REQUIRED"))?;
        let actor = Self {
            tenant_id: principal.tenant_id().into(),
            user_id: principal.user_id().into(),
            roles: principal.roles().to_vec(),
            tier: principal.tier().into(),
        };
        actor.require_tenant()?;
        Ok(actor)
    }
}
#[derive(Clone, Debug)]
pub struct Scope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
}
pub(crate) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn uuid(value: &str) -> Result<String, AgentError> {
    uuid::Uuid::parse_str(value)
        .map(|v| v.to_string())
        .map_err(|_| AgentError::code(422, "INVALID_UUID"))
}
pub(crate) fn object(v: &Value) -> Value {
    if v.is_object() { v.clone() } else { json!({}) }
}

pub mod scheduled_notifications;

pub mod model_timing;
