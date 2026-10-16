//! SDK read-only delegation uses existing JWT and signed Knowledge boundary.
//! Workers never choose an actor, HTTP target, provider key or arbitrary operation.
use super::*;
use ai_platform_gateway_foundation::{
    sdk_read_grants::{BoundRead, SdkReadGrants, datasets},
    service_registry::ServiceRegistry,
};
use axum::{Json, extract::State, http::HeaderMap};
use serde::Deserialize;
use serde_json::Value;
#[derive(Clone)]
pub struct SdkReads {
    pub grants: Arc<SdkReadGrants>,
    pub registry: Arc<ServiceRegistry>,
    pub sessions: Arc<dyn ai_platform_gateway_foundation::session_manager::SessionStorage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {
    pub dataset_id: String,
    pub query: String,
    pub top_k: usize,
}
pub(super) fn token(headers: &HeaderMap) -> Result<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| code(403, "sdk_read_grant_invalid"))
}
async fn actor(
    state: &KnowledgeProxy,
    token: &str,
    dataset: &str,
) -> Result<(BoundRead, ContentIdentity)> {
    let reads = state
        .sdk_reads
        .as_ref()
        .ok_or_else(|| code(503, "sdk_reads_unavailable"))?;
    let bound = reads
        .grants
        .admit(token, dataset)
        .map_err(|_| code(403, "sdk_read_not_allowed"))?;
    let user = actor_bound(state, &bound).await?;
    Ok((bound, user))
}
pub(super) async fn actor_bound(
    state: &KnowledgeProxy,
    bound: &BoundRead,
) -> Result<ContentIdentity> {
    let reads = state
        .sdk_reads
        .as_ref()
        .ok_or_else(|| code(503, "sdk_reads_unavailable"))?;
    let service = reads
        .registry
        .get(&bound.service_id)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(|| code(403, "sdk_service_unavailable"))?;
    let snapshot = service.snapshot();
    if snapshot.status != "active"
        || datasets(&snapshot).map_err(|_| code(403, "sdk_read_profile_changed"))?
            != bound.dataset_ids
        || ai_platform_gateway_foundation::sdk_capabilities::selected(&snapshot)
            .map_err(|_| code(403, "sdk_read_profile_changed"))?
            != bound.capability_ids
    {
        return Err(code(403, "sdk_read_profile_changed"));
    }
    let mut headers = HeaderMap::new();
    headers.insert("authorization", bound.authorization.clone());
    let user = state
        .identity(&headers, "/api/v1/knowledge/datasets", true)
        .await?;
    if !user.is_authenticated || user.user_id != bound.user_id || user.tenant_id != bound.tenant_id
    {
        return Err(code(403, "sdk_read_identity_changed"));
    }
    let session = reads
        .sessions
        .get(&bound.session_id)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(|| code(403, "sdk_session_unavailable"))?;
    if session["tenant_id"] != bound.tenant_id
        || session["user_id"] != bound.user_id
        || session["service_id"] != bound.service_id
        || session["expires_at"].as_str().is_some_and(|s| {
            chrono::DateTime::parse_from_rfc3339(s).map_or(true, |t| t <= chrono::Utc::now())
        })
    {
        return Err(code(403, "sdk_session_scope_invalid"));
    }
    let validator = ai_platform_gateway_foundation::request_validation::RequestValidator::new(
        &state.access.rbac,
    );
    validator
        .validate_service_auth(
            &ai_platform_gateway_foundation::request_validation::ValidationRequest {
                service_id: bound.service_id.clone(),
                user_id: Some(bound.user_id.clone()),
                ..Default::default()
            },
            &snapshot.validation_view(),
            &user.roles,
        )
        .map_err(|_| code(403, "sdk_service_access_revoked"))?;
    ai_platform_auth::rbac::require_capability(
        &state.access.rbac,
        &user.roles,
        &[],
        ai_platform_auth::rbac::Capability::AgentInvoke,
    )
    .map_err(|_| code(403, "sdk_service_access_revoked"))?;
    Ok(user)
}
async fn bounded(response: Response) -> Result<Value> {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .map_err(|_| code(502, "sdk_knowledge_response_too_large"))?;
    if !status.is_success() {
        return Err(code(status.as_u16(), "sdk_knowledge_access_denied"));
    }
    serde_json::from_slice(&bytes).map_err(|_| code(502, "sdk_knowledge_response_invalid"))
}
pub async fn retrieve(
    State(state): State<Arc<KnowledgeProxy>>,
    headers: HeaderMap,
    Json(input): Json<ReadInput>,
) -> Result<Json<Value>> {
    let grant = token(&headers)?.to_owned();
    if uuid::Uuid::parse_str(&input.dataset_id).is_err()
        || input.query.trim().is_empty()
        || input.query.len() > 4096
        || !(1..=5).contains(&input.top_k)
    {
        return Err(code(400, "sdk_knowledge_arguments_invalid"));
    }
    let response = tokio::time::timeout(Duration::from_secs(30), async {
        let (bound, user) = actor(&state, &grant, &input.dataset_id).await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        if let Ok(value) = axum::http::HeaderValue::from_str(&bound.request_id) {
            headers.insert("x-request-id", value);
        }
        let body = serde_json::to_vec(
            &json!({"query":input.query,"top_k":input.top_k,"hierarchical":false}),
        )
        .map_err(|_| unavailable())?;
        let result = bounded(
            state
                .forward(
                    user,
                    Method::POST,
                    format!("{}/retrieve", input.dataset_id),
                    String::new(),
                    headers,
                    body,
                )
                .await?,
        )
        .await?;
        let _ = actor(&state, &grant, &input.dataset_id).await?;
        if !state
            .sdk_reads
            .as_ref()
            .is_some_and(|s| s.grants.still_active(&grant))
        {
            return Err(code(403, "sdk_read_grant_expired"));
        }
        Ok::<_, ContentError>(Json(result))
    })
    .await
    .map_err(|_| code(504, "sdk_knowledge_timeout"))??;
    Ok(response)
}
/// Check every selected dataset before loading durable SDK conversation context.
/// This prevents use of earlier retrieved context after a dataset grant is revoked.
pub async fn admit(
    State(state): State<Arc<KnowledgeProxy>>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let grant = token(&headers)?.to_owned();
    let reads = state
        .sdk_reads
        .as_ref()
        .ok_or_else(|| code(503, "sdk_reads_unavailable"))?;
    let dataset = reads
        .grants
        .first_dataset(&grant)
        .ok_or_else(|| code(403, "sdk_read_grant_invalid"))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let (bound, user) = actor(&state, &grant, &dataset).await?;
        for id in &bound.dataset_ids {
            bounded(
                state
                    .forward(
                        user.clone(),
                        Method::GET,
                        format!("datasets/{id}"),
                        String::new(),
                        HeaderMap::new(),
                        vec![],
                    )
                    .await?,
            )
            .await?;
        }
        if !reads.grants.still_active(&grant) {
            return Err(code(403, "sdk_read_grant_expired"));
        }
        Ok::<_, ContentError>(Json(json!({"authorized":true})))
    })
    .await
    .map_err(|_| code(504, "sdk_knowledge_timeout"))?
}
