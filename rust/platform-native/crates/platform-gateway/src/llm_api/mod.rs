//! Public provider-native APIs over the existing identity, admission and ledger.
//! Construction is inert. The host mounts this alongside its canonical routers.
mod codec;
pub mod guardrails;
#[cfg(test)]
mod handler_tests;
mod http;
mod meter;
mod native;
pub mod public_contract;
mod raw_json;
#[cfg(test)]
mod tests;
mod transport;
use crate::{
    capacity::CapacityResolver,
    dispatch_preflight::DispatchPreflight,
    dispatcher::{DispatchAdmission, DispatchLease},
    usage_recorder::UsageRecord,
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
};
pub use http::router;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
pub type LlmResult<T> = Result<T, LlmError>;
type LlmFuture<'a, T> = Pin<Box<dyn Future<Output = LlmResult<T>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LlmError {
    pub status: u16,
    pub code: &'static str,
}
impl LlmError {
    pub(crate) fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
}
impl IntoResponse for LlmError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            axum::Json(
                json!({"error":{"type":"gateway_error","code":self.code,"message":self.code}}),
            ),
        )
            .into_response()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Chat,
    Embeddings,
    Messages,
}
impl Surface {
    fn path(self) -> &'static str {
        match self {
            Self::Chat => "/v1/chat/completions",
            Self::Embeddings => "/v1/embeddings",
            Self::Messages => "/v1/messages",
        }
    }
    fn wire(self) -> &'static str {
        match self {
            Self::Chat => "chat_completions",
            Self::Embeddings => "embeddings",
            Self::Messages => "anthropic_messages",
        }
    }
}
/// IDs of existing registered services, selected by the operator, never callers.
#[derive(Clone)]
pub struct LlmBindings {
    pub chat_service: String,
    pub embeddings_service: String,
    pub messages_service: String,
    /// Must be the same header configured on the canonical AdmissionService.
    pub api_key_header: HeaderName,
}
impl LlmBindings {
    fn service(&self, surface: Surface) -> &str {
        match surface {
            Surface::Chat => &self.chat_service,
            Surface::Embeddings => &self.embeddings_service,
            Surface::Messages => &self.messages_service,
        }
    }
}
#[derive(Clone)]
struct Actor {
    tenant: String,
    user: String,
    roles: Vec<String>,
    tier: String,
    allowed_models: Vec<String>,
    constraints: crate::service_access_policy::ServiceAccessConstraints,
    routing: Option<Arc<crate::migration_host::routing_policy_v1::RoutingAuthority>>,
}
struct Invocation {
    client_request_id: Option<String>,
    external_audit: bool,
    surface: Surface,
    body: Bytes,
    payload: Value,
    stream: bool,
    request_id: String,
}
struct Prepared {
    invocation: Invocation,
    url: reqwest::Url,
    headers: HeaderMap,
    body: Bytes,
    timeout: Duration,
    record: UsageRecord,
    pricing: Value,
    route_decision: Option<axum::http::HeaderValue>,
    guardrails: Option<guardrails::Context>,
    lease: Box<dyn DispatchLease>,
}
trait Backend: Send + Sync {
    fn authenticate<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> LlmFuture<'a, Actor>;
    fn models<'a>(&'a self, actor: &'a Actor) -> LlmFuture<'a, Value>;
    fn prepare<'a>(
        &'a self,
        actor: &'a Actor,
        invocation: Invocation,
        headers: &'a HeaderMap,
    ) -> LlmFuture<'a, Prepared>;
    fn settle<'a>(
        &'a self,
        prepared: &'a Prepared,
        observed: meter::Observation,
        status: u16,
    ) -> LlmFuture<'a, ()>;
}
#[derive(Clone)]
pub struct LlmApi {
    backend: Arc<dyn Backend>,
    transport: Arc<dyn transport::Transport>,
    audit_pool: Option<sqlx::PgPool>,
}
impl LlmApi {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        auth: Arc<AdmissionService>,
        pool: sqlx::PgPool,
        preflight: Arc<DispatchPreflight>,
        admission: Arc<dyn DispatchAdmission>,
        capacity: Arc<CapacityResolver>,
        providers: Arc<dyn crate::migration_agent::model::RuntimeProviderCredentials>,
        bindings: LlmBindings,
    ) -> LlmResult<Self> {
        Self::new_with_routing(
            auth,
            pool,
            preflight,
            admission,
            capacity,
            providers,
            bindings,
            LlmRoutingBindings::default(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_routing(
        auth: Arc<AdmissionService>,
        pool: sqlx::PgPool,
        preflight: Arc<DispatchPreflight>,
        admission: Arc<dyn DispatchAdmission>,
        capacity: Arc<CapacityResolver>,
        providers: Arc<dyn crate::migration_agent::model::RuntimeProviderCredentials>,
        bindings: LlmBindings,
        routing: LlmRoutingBindings,
    ) -> LlmResult<Self> {
        if [
            bindings.chat_service.as_str(),
            bindings.embeddings_service.as_str(),
            bindings.messages_service.as_str(),
        ]
        .iter()
        .any(|s| s.trim().is_empty())
            || bindings.api_key_header == axum::http::header::AUTHORIZATION
        {
            return Err(LlmError::new(503, "llm_binding_invalid"));
        }
        Ok(Self {
            audit_pool: Some(pool.clone()),
            backend: Arc::new(native::NativeBackend::new(
                auth, pool, preflight, admission, capacity, providers, bindings, routing,
            )),
            transport: Arc::new(transport::ReqwestTransport::new()?),
        })
    }
}

/// Operator-only bindings, never decoded from an invocation or request headers.
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRouteBinding {
    pub tenant_id: String,
    pub alias: String,
    pub policy_id: String,
    pub policy_pin: Option<String>,
    pub seed: String,
}
#[derive(Clone, Default)]
pub struct LlmRoutingBindings(Vec<LlmRouteBinding>);
impl LlmRoutingBindings {
    pub fn new(entries: Vec<LlmRouteBinding>) -> LlmResult<Self> {
        let mut seen = std::collections::BTreeSet::new();
        if entries.len() > 1024 {
            return Err(LlmError::new(503, "llm_routing_binding_invalid"));
        }
        for e in &entries {
            if [&e.tenant_id, &e.alias, &e.policy_id, &e.seed]
                .iter()
                .any(|v| {
                    v.is_empty()
                        || v.len() > 512
                        || v.trim() != v.as_str()
                        || v.chars().any(char::is_control)
                })
                || e.seed.len() > 400
                || e.policy_id.len() > 128
                || e.alias.contains('/')
                || e.policy_pin.as_ref().is_some_and(|p| {
                    p.len() != 64
                        || !p
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                || !seen.insert((&e.tenant_id, &e.alias))
            {
                return Err(LlmError::new(503, "llm_routing_binding_invalid"));
            }
        }
        Ok(Self(entries))
    }
    fn get(&self, tenant: &str, alias: &str) -> Option<&LlmRouteBinding> {
        self.0
            .iter()
            .find(|b| b.tenant_id == tenant && b.alias == alias)
    }
}

impl LlmRoutingBindings {
    fn for_actor(
        &self,
        actor: &Actor,
        alias: &str,
    ) -> LlmResult<
        Option<(
            &LlmRouteBinding,
            Arc<crate::migration_host::routing_policy_v1::RoutingAuthority>,
        )>,
    > {
        let Some(binding) = self.get(&actor.tenant, alias) else {
            return Ok(None);
        };
        let authority = actor
            .routing
            .clone()
            .ok_or(LlmError::new(503, "llm_routing_authority_missing"))?;
        if authority.tenant() != actor.tenant || authority.actor() != actor.user {
            return Err(LlmError::new(403, "llm_routing_authority_mismatch"));
        }
        Ok(Some((binding, authority)))
    }
}
