//! Gateway current actor authority and proof-bound KS retrieval. These routers
//! are distinct from public JWT and gateway-to-KS trusted-user routes.
use super::{
    auth::GatewayAuth,
    types::{QueryError, Result, UserContext},
};
use crate::migration_agent::control::execution_transport::WorkloadSend;

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
use sqlx::PgPool;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
pub const ACTOR_PATH: &str = "/internal/v2/agent-capabilities/knowledge-actor";
#[derive(Clone)]
pub struct CurrentActorStore {
    pub pool: PgPool,
}
impl CurrentActorStore {
    pub async fn resolve(&self, tenant: &str, user: &str) -> Result<UserContext> {
        let account: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE user_id=$1 AND tenant_id=$2")
                .bind(user)
                .bind(tenant)
                .fetch_optional(&self.pool)
                .await?;
        let account = account
            .filter(|u| u["status"] == "active")
            .ok_or(QueryError::Forbidden)?;
        let roles:Vec<String>=sqlx::query_scalar("SELECT role_name FROM user_roles WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW()) ORDER BY granted_at").bind(user).fetch_all(&self.pool).await?;
        Ok(UserContext {
            user_id: user.into(),
            tenant_id: tenant.into(),
            user_tier: account["tier"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("normal")
                .into(),
            user_type: "runtime".into(),
            roles,
            is_authenticated: true,
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorRequest {
    tenant_id: String,
    user_id: String,
}
#[derive(Clone)]
pub struct GatewayActorState {
    pub store: CurrentActorStore,
    pub verifier: Arc<GatewayAuth>,
}
pub fn gateway_actor_router(state: GatewayActorState) -> Result<Router> {
    if state.verifier.caller_service != "knowledge-service"
        || state.verifier.audience != "gateway"
        || state.verifier.version != "v2"
        || state.verifier.allow_anonymous
    {
        return Err(QueryError::Validation(
            "knowledge actor requires knowledge-service to gateway HMAC v2".into(),
        ));
    }
    Ok(Router::new()
        .route(ACTOR_PATH, post(resolve_actor))
        .with_state(state))
}
fn failure(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"detail":{"code":code}}))).into_response()
}
async fn resolve_actor(State(state): State<GatewayActorState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, 4096).await {
        Ok(b) => b,
        Err(_) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "KB_ACTOR_REQUEST_TOO_LARGE"),
    };
    let header = parts
        .headers
        .get("x-gateway-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if state
        .verifier
        .verify(
            header,
            parts.method.as_str(),
            parts.uri.path(),
            parts.uri.query().unwrap_or(""),
            &bytes,
            &parts.headers,
        )
        .await
        .is_err()
    {
        return failure(StatusCode::FORBIDDEN, "KB_ACTOR_AUTH_DENIED");
    }
    let payload: ActorRequest = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(_) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "KB_ACTOR_REQUEST_INVALID"),
    };
    if [&payload.tenant_id, &payload.user_id]
        .iter()
        .any(|s| s.is_empty() || s.chars().count() > 255)
    {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "KB_ACTOR_REQUEST_INVALID");
    }
    match state.store.resolve(&payload.tenant_id,&payload.user_id).await{Ok(u)=>Json(json!({"tenant_id":u.tenant_id,"user_id":u.user_id,"roles":u.roles,"tier":u.user_tier,"status":"active"})).into_response(),Err(QueryError::Forbidden)=>failure(StatusCode::FORBIDDEN,"KB_ACTOR_DENIED"),Err(_)=>failure(StatusCode::SERVICE_UNAVAILABLE,"KB_ACTOR_UNAVAILABLE")}
}
/// Canonical gateway adapter: current actors come from gateway-owned tables;
/// every dataset/document/source decision comes from signed knowledge-service.
#[derive(Clone)]
pub struct HttpKnowledgeSourceAuthority {
    actors: CurrentActorStore,
    client: reqwest::Client,
    origin: reqwest::Url,
    signer: Arc<GatewayAuth>,
}
impl HttpKnowledgeSourceAuthority {
    pub fn new(
        actors: CurrentActorStore,
        origin: reqwest::Url,
        signer: Arc<GatewayAuth>,
    ) -> Result<Self> {
        if signer.caller_service != "gateway"
            || signer.audience != "knowledge-service"
            || signer.version != "v2"
            || signer.allow_anonymous
        {
            return Err(QueryError::Validation(
                "source authority requires gateway to knowledge-service HMAC v2".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| QueryError::DependenciesFailed)?;
        Ok(Self {
            actors,
            client,
            origin,
            signer,
        })
    }
    async fn authorize(&self, actor: &UserContext, path: &str, payload: Value) -> Result<Value> {
        let mut headers = HeaderMap::new();
        for (k, v) in [
            ("x-user-id", actor.user_id.clone()),
            ("x-tenant-id", actor.tenant_id.clone()),
            ("x-user-type", "user".into()),
            ("x-user-tier", actor.user_tier.clone()),
            ("x-user-roles", actor.roles.join(",")),
        ] {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|_| QueryError::Forbidden)?,
                v.parse().map_err(|_| QueryError::Forbidden)?,
            );
        }
        let bytes = serde_json::to_vec(&payload).map_err(|_| QueryError::DependenciesFailed)?;
        let signature = self.signer.sign("POST", path, "", &bytes, &headers)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let response = self
            .client
            .post(
                self.origin
                    .join(path)
                    .map_err(|_| QueryError::DependenciesFailed)?,
            )
            .headers(headers)
            .header("x-gateway-secret", signature)
            .header("content-type", "application/json")
            .body(bytes)
            .timeout(Duration::from_secs(5))
            .workload_send()
            .await
            .map_err(|_| QueryError::DependenciesFailed)?;
        if !response.status().is_success() {
            return Err(QueryError::DependenciesFailed);
        }
        tokio::time::timeout_at(deadline, response.json())
            .await
            .map_err(|_| QueryError::DependenciesFailed)?
            .map_err(|_| QueryError::InvalidProviderResponse)
    }
}
impl crate::migration_agent::entry::SourceAuthority for HttpKnowledgeSourceAuthority {
    fn allowed<'a>(
        &'a self,
        scope: &'a crate::migration_agent::Scope,
        refs: &'a crate::migration_agent::entry::SourceRefs,
    ) -> crate::migration_agent::AgentFuture<'a, bool> {
        Box::pin(async move {
            let result:Result<bool>=async{let actor=match self.actors.resolve(&scope.tenant_id,&scope.user_id).await{Ok(a)=>a,Err(QueryError::Forbidden)=>return Ok(false),Err(e)=>return Err(e)};let mut datasets=refs.datasets.clone();datasets.extend(refs.documents.iter().map(|r|r.0.clone()));datasets.extend(refs.versions.iter().map(|r|r.0.clone()));if datasets.iter().any(|s|s.is_empty())||refs.documents.iter().any(|r|r.1.is_empty())||refs.versions.iter().any(|r|r.1.is_empty()||r.2<=0||r.3.len()!=64||!r.3.bytes().all(|b|b.is_ascii_hexdigit())){return Ok(false)}let all:Vec<_>=datasets.iter().cloned().collect();for ids in all.chunks(200){let value=self.authorize(&actor,"/api/v1/internal/knowledge/datasets/authorize",json!({"dataset_ids":ids,"is_tenant_admin":actor.tenant_admin()})).await?;let allowed:BTreeSet<String>=value["allowed_dataset_ids"].as_array().ok_or(QueryError::InvalidProviderResponse)?.iter().map(|v|v.as_str().map(str::to_owned).ok_or(QueryError::InvalidProviderResponse)).collect::<Result<_>>()?;if ids.iter().any(|id|!allowed.contains(id)){return Ok(false)}}for dataset in datasets{let docs:Vec<_>=refs.documents.iter().filter(|r|r.0==dataset).map(|r|r.1.clone()).collect();for docs in docs.chunks(500){let value=self.authorize(&actor,"/api/v1/internal/knowledge/documents/authorize",json!({"dataset_id":dataset,"document_ids":docs})).await?;let allowed:BTreeSet<String>=value["allowed_document_ids"].as_array().ok_or(QueryError::InvalidProviderResponse)?.iter().map(|v|v.as_str().map(str::to_owned).ok_or(QueryError::InvalidProviderResponse)).collect::<Result<_>>()?;if docs.iter().any(|d|!allowed.contains(d)){return Ok(false)}}let versions:Vec<_>=refs.versions.iter().filter(|r|r.0==dataset).collect();for version_batch in versions.chunks(500){let requested:Vec<_>=version_batch.iter().map(|r|json!({"document_id":r.1,"source_version":r.2,"source_hash":r.3})).collect();let value=self.authorize(&actor,"/api/v1/internal/knowledge/document-sources/authorize",json!({"dataset_id":dataset,"references":requested})).await?;let mut allowed=BTreeSet::new();for v in value["allowed_references"].as_array().ok_or(QueryError::InvalidProviderResponse)?{allowed.insert((v["document_id"].as_str().ok_or(QueryError::InvalidProviderResponse)?.to_owned(),v["source_version"].as_i64().ok_or(QueryError::InvalidProviderResponse)?,v["source_hash"].as_str().ok_or(QueryError::InvalidProviderResponse)?.to_lowercase()));}if version_batch.iter().any(|r|!allowed.contains(&(r.1.clone(),r.2,r.3.to_lowercase()))){return Ok(false)}}}Ok(true)}.await;
            result.map_err(|_| {
                crate::migration_agent::AgentError::code(503, "ASSISTANT_SOURCE_CHECK_UNAVAILABLE")
            })
        })
    }
}
/// Publication/runtime audience authorization is intentionally distinct from
/// current-user artifact provenance. Guest and token principals need not have a
/// gateway users row; their authenticated Actor comes from runtime admission.
#[derive(Clone)]
pub struct HttpAgentRuntimeKnowledge {
    client: reqwest::Client,
    origin: reqwest::Url,
    signer: Arc<GatewayAuth>,
}
impl HttpAgentRuntimeKnowledge {
    pub fn new(origin: reqwest::Url, signer: Arc<GatewayAuth>) -> Result<Self> {
        if signer.caller_service != "gateway"
            || signer.audience != "knowledge-service"
            || signer.version != "v2"
            || signer.allow_anonymous
        {
            return Err(QueryError::Validation(
                "runtime knowledge requires gateway to knowledge-service HMAC v2".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| QueryError::DependenciesFailed)?;
        Ok(Self {
            client,
            origin,
            signer,
        })
    }
    async fn resolve(
        &self,
        actor: &crate::migration_agent::Actor,
        bindings: &[Value],
    ) -> Result<BTreeSet<String>> {
        let ids: Vec<String> = bindings
            .iter()
            .filter(|b| b.is_object())
            .filter_map(|b| b.get("dataset_id"))
            .filter_map(|v| match v {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(true) => Some("True".into()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let authenticated = crate::migration_agent::resources::authenticated(actor);
        let admin = authenticated && actor.is_admin();
        let mut headers = HeaderMap::new();
        let mut identity = vec![
            ("x-tenant-id", actor.tenant_id.clone()),
            ("x-user-id", actor.user_id.clone()),
            (
                "x-user-type",
                if authenticated { "user" } else { "anonymous" }.to_owned(),
            ),
        ];
        if !authenticated {
            identity.push(("x-user-tier", "anonymous".into()));
            identity.push(("x-user-roles", "guest".into()));
        } else {
            if !actor.roles.is_empty() {
                identity.push(("x-user-roles", actor.roles.join(",")));
            }
            if admin {
                identity.push(("x-user-tier", "admin".into()));
            }
        }
        for (k, v) in identity {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|_| QueryError::Forbidden)?,
                v.parse().map_err(|_| QueryError::Forbidden)?,
            );
        }
        let path = "/api/v1/internal/knowledge/datasets/authorize";
        let bytes = serde_json::to_vec(&json!({"dataset_ids":ids,"is_tenant_admin":admin}))
            .map_err(|_| QueryError::DependenciesFailed)?;
        let signature = self.signer.sign("POST", path, "", &bytes, &headers)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let response = self
            .client
            .post(
                self.origin
                    .join(path)
                    .map_err(|_| QueryError::DependenciesFailed)?,
            )
            .headers(headers)
            .header("x-gateway-secret", signature)
            .header("content-type", "application/json")
            .body(bytes)
            .timeout(Duration::from_secs(5))
            .workload_send()
            .await
            .map_err(|_| QueryError::DependenciesFailed)?;
        if !response.status().is_success() {
            return Err(QueryError::DependenciesFailed);
        }
        let value: Value = tokio::time::timeout_at(deadline, response.json())
            .await
            .map_err(|_| QueryError::DependenciesFailed)?
            .map_err(|_| QueryError::InvalidProviderResponse)?;
        let allowed: BTreeSet<String> = value["allowed_dataset_ids"]
            .as_array()
            .ok_or(QueryError::InvalidProviderResponse)?
            .iter()
            .map(|id| {
                id.as_str()
                    .map(str::to_owned)
                    .ok_or(QueryError::InvalidProviderResponse)
            })
            .collect::<Result<_>>()?;
        if ids.iter().any(|id| !allowed.contains(id)) {
            return Err(QueryError::Forbidden);
        }
        Ok(ids.into_iter().collect())
    }
}
impl crate::migration_agent::resources::RuntimeKnowledge for HttpAgentRuntimeKnowledge {
    fn effective<'a>(
        &'a self,
        actor: &'a crate::migration_agent::Actor,
        bindings: &'a [Value],
        _channel: &'a str,
        _agent: &'a str,
    ) -> crate::migration_agent::AgentFuture<'a, BTreeSet<String>> {
        Box::pin(async move {
            self.resolve(actor, bindings).await.map_err(|_| {
                crate::migration_agent::AgentError::code(503, "AGENT_KNOWLEDGE_UNAVAILABLE")
            })
        })
    }
}
