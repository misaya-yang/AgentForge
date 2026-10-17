//! Gateway current actor authority and proof-bound KS retrieval. These routers
//! are distinct from public JWT and gateway-to-KS trusted-user routes.
use super::{
    auth::GatewayAuth,
    config::RetrieveRequest,
    service::RetrievalService,
    types::{QueryError, Result, UserContext},
};
use crate::migration_content::capabilities::{
    CapabilityScope, constant_time_equal, verify_capability_proof,
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
pub const ACTOR_PATH: &str = "/internal/v2/agent-capabilities/knowledge-actor";
#[derive(Clone)]
pub struct RuntimeActorClient {
    client: reqwest::Client,
    origin: reqwest::Url,
    signer: Arc<GatewayAuth>,
}
impl RuntimeActorClient {
    pub fn new(origin: reqwest::Url, signer: Arc<GatewayAuth>) -> Result<Self> {
        if signer.caller_service != "knowledge-service"
            || signer.audience != "gateway"
            || signer.version != "v2"
            || signer.allow_anonymous
        {
            return Err(QueryError::Validation(
                "runtime actor signer scope is invalid".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| QueryError::DependenciesFailed)?;
        Ok(Self {
            client,
            origin,
            signer,
        })
    }
    pub async fn resolve(&self, tenant: &str, user: &str) -> Result<UserContext> {
        let body = serde_json::to_vec(&json!({"tenant_id":tenant,"user_id":user}))
            .map_err(|_| QueryError::DependenciesFailed)?;
        let signature = self
            .signer
            .sign("POST", ACTOR_PATH, "", &body, &HeaderMap::new())?;
        let url = self
            .origin
            .join(ACTOR_PATH)
            .map_err(|_| QueryError::DependenciesFailed)?;
        let response = self
            .client
            .post(url)
            .header("x-gateway-secret", signature)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| QueryError::DependenciesFailed)?;
        if response.status() == StatusCode::FORBIDDEN {
            return Err(QueryError::Forbidden);
        }
        if !response.status().is_success() {
            return Err(QueryError::DependenciesFailed);
        }
        let value: Value = response.json().await.map_err(|_| QueryError::Forbidden)?;
        let roles = value["roles"].as_array().ok_or(QueryError::Forbidden)?;
        if value["tenant_id"] != tenant
            || value["user_id"] != user
            || value["status"] != "active"
            || !value["tier"].is_string()
            || roles
                .iter()
                .any(|r| r.as_str().is_none_or(|s| s.is_empty()))
        {
            return Err(QueryError::Forbidden);
        }
        Ok(UserContext {
            tenant_id: tenant.into(),
            user_id: user.into(),
            roles: roles
                .iter()
                .filter_map(|r| r.as_str().map(str::to_owned))
                .collect(),
            user_tier: value["tier"].as_str().unwrap_or("normal").into(),
            user_type: "runtime".into(),
            is_authenticated: true,
        })
    }
}
/// Injectable boundary mirrors the source capability service and actor resolver.
/// The production adapter retains authoritative actor, ACL and retrieval calls.
pub type CapabilityFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;
pub trait CapabilityBackend: Send + Sync {
    fn actor<'a>(&'a self, tenant: &'a str, user: &'a str) -> CapabilityFuture<'a, UserContext>;
    fn require_dataset<'a>(
        &'a self,
        actor: &'a UserContext,
        id: &'a str,
    ) -> CapabilityFuture<'a, ()>;
    fn retrieve<'a>(
        &'a self,
        actor: &'a UserContext,
        id: &'a str,
        request: RetrieveRequest,
    ) -> CapabilityFuture<'a, (Vec<super::types::RetrieveResult>, Value)>;
}
struct NativeCapabilityBackend {
    retrieval: Arc<RetrievalService>,
    actors: RuntimeActorClient,
}
impl CapabilityBackend for NativeCapabilityBackend {
    fn actor<'a>(&'a self, tenant: &'a str, user: &'a str) -> CapabilityFuture<'a, UserContext> {
        Box::pin(self.actors.resolve(tenant, user))
    }
    fn require_dataset<'a>(
        &'a self,
        actor: &'a UserContext,
        id: &'a str,
    ) -> CapabilityFuture<'a, ()> {
        Box::pin(async move {
            self.retrieval
                .store
                .require_dataset_access(actor, id, "viewer")
                .await
                .map(|_| ())
        })
    }
    fn retrieve<'a>(
        &'a self,
        actor: &'a UserContext,
        id: &'a str,
        request: RetrieveRequest,
    ) -> CapabilityFuture<'a, (Vec<super::types::RetrieveResult>, Value)> {
        Box::pin(self.retrieval.retrieve(actor, id, request))
    }
}
#[derive(Clone)]
pub struct CapabilityState {
    backend: Arc<dyn CapabilityBackend>,
    internal_token: Arc<String>,
    proof_secret: Arc<Vec<u8>>,
}
impl CapabilityState {
    pub fn new(
        retrieval: Arc<RetrievalService>,
        actors: RuntimeActorClient,
        internal_token: String,
        proof_secret: Vec<u8>,
    ) -> Self {
        Self::with_backend(
            Arc::new(NativeCapabilityBackend { retrieval, actors }),
            internal_token,
            proof_secret,
        )
    }
    pub fn with_backend(
        backend: Arc<dyn CapabilityBackend>,
        internal_token: String,
        proof_secret: Vec<u8>,
    ) -> Self {
        Self {
            backend,
            internal_token: Arc::new(internal_token),
            proof_secret: Arc::new(proof_secret),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityRequest {
    query: String,
    #[serde(default = "five")]
    top_k: usize,
    #[serde(default)]
    threshold: Option<f64>,
}
fn five() -> usize {
    5
}
pub fn capability_router(state: CapabilityState) -> Router {
    Router::new()
        .route(
            "/internal/v2/capabilities/knowledge/{dataset_id}/retrieve",
            post(retrieve_capability),
        )
        .with_state(state)
}
fn detail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"detail":message}))).into_response()
}
async fn retrieve_capability(
    State(state): State<CapabilityState>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let token = header("x-ai-platform-internal-token");
    if state.internal_token.is_empty()
        || token.is_empty()
        || !constant_time_equal(&state.internal_token, token)
    {
        return detail(StatusCode::UNAUTHORIZED, "capability authentication failed");
    }
    let bytes =
        match tokio::time::timeout(Duration::from_secs(30), to_bytes(body, 256 * 1024)).await {
            Ok(Ok(b)) => b,
            Ok(Err(_)) => {
                return detail(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "capability request too large",
                );
            }
            Err(_) => return detail(StatusCode::REQUEST_TIMEOUT, "capability request timed out"),
        };
    let p: CapabilityRequest = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(_) => {
            return detail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid retrieval request",
            );
        }
    };
    if p.query.is_empty()
        || p.query.chars().count() > 4096
        || !(1..=100).contains(&p.top_k)
        || p.threshold
            .is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
    {
        return detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid retrieval request",
        );
    }
    let scope = CapabilityScope {
        tenant_id: header("x-ai-tenant-id").trim().into(),
        user_id: header("x-ai-user-id").trim().into(),
        session_id: header("x-ai-session-id").trim().into(),
        execution_id: header("x-ai-execution-id").into(),
        run_id: header("x-ai-run-id").into(),
        tool_call_id: String::new(),
        arguments_hash: String::new(),
    };
    let payload = serde_json::to_value(&p).unwrap_or(Value::Null);
    let verified_proof = match verify_capability_proof(
        &state.proof_secret,
        header("x-ai-capability-proof"),
        "POST",
        parts.uri.path(),
        &payload,
        &scope,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(proof) => proof,
        Err(_) => return detail(StatusCode::UNAUTHORIZED, "capability proof invalid"),
    };
    if [&scope.tenant_id, &scope.user_id, &scope.session_id]
        .iter()
        .any(|s| s.is_empty() || s.chars().count() > 255)
    {
        return detail(StatusCode::FORBIDDEN, "capability identity invalid");
    }
    let actor = match state.backend.actor(&scope.tenant_id, &scope.user_id).await {
        Ok(a) => a,
        Err(QueryError::Forbidden) => {
            return detail(StatusCode::FORBIDDEN, "capability actor denied");
        }
        Err(_) => {
            return detail(
                StatusCode::SERVICE_UNAVAILABLE,
                "capability actor unavailable",
            );
        }
    };
    if actor.tenant_id != scope.tenant_id || actor.user_id != scope.user_id || actor.anonymous() {
        return detail(StatusCode::FORBIDDEN, "capability actor denied");
    }
    let expiry = verified_proof["expires_at"]
        .as_u64()
        .and_then(|value| value.checked_mul(1000))
        .unwrap_or(0);
    let transport = match super::private_transport::AdmittedTransport::after_authenticated(
        &parts.headers,
        expiry,
    ) {
        Ok(transport) => transport,
        Err(_) => {
            return detail(
                StatusCode::REQUEST_TIMEOUT,
                "private request budget invalid or expired",
            );
        }
    };
    match transport.run(async{
    if state.backend.require_dataset(&actor, &id).await.is_err() {
        return detail(StatusCode::NOT_FOUND, "Dataset not found");
    }
    match state
        .backend
        .retrieve(
            &actor,
            &id,
            RetrieveRequest {
                query: p.query,
                top_k: p.top_k,
                score_threshold: p.threshold,
                ..Default::default()
            },
        )
        .await
    {
        Ok((results, metadata)) => {
            let mut rows = Vec::new();
            for r in results {
                if !["text", "text/plain"].contains(&r.content_type.to_lowercase().as_str()) {
                    return detail(StatusCode::BAD_REQUEST, "Invalid retrieval request");
                }
                rows.push(json!({"segment_id":r.segment_id,"document_id":r.document_id,"score":r.score,"text":r.text,"metadata":r.metadata,"content_type":"text","image_url":null,"vlm_description":null,"associated_images":[],"source_type":r.metadata["source_type"],"citation_text":r.metadata["citation_text"],"source_reference":r.metadata.get("source_reference").cloned().unwrap_or_else(||json!({}))}));
            }
            Json(json!({"results":rows,"metadata":metadata})).into_response()
        }
        Err(QueryError::Validation(_)) => {
            detail(StatusCode::BAD_REQUEST, "Invalid retrieval request")
        }
        Err(QueryError::Forbidden | QueryError::NotFound) => {
            detail(StatusCode::NOT_FOUND, "Dataset not found")
        }
        Err(e) => e.into_response(),
    }
    }).await{Ok(response)=>response,Err(_)=>detail(StatusCode::GATEWAY_TIMEOUT,"private request budget expired")}
}

#[cfg(test)]
#[path = "actor_transport_tests.rs"]
mod transport_tests;
