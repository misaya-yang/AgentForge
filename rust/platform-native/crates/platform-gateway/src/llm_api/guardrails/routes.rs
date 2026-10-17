use super::*;
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
};
use sqlx::Row;
#[derive(Clone)]
struct StateData {
    service: GuardrailService,
    auth: Arc<AdmissionService>,
}
pub fn router(service: GuardrailService, auth: Arc<AdmissionService>) -> Router {
    Router::new()
        .route(
            "/api/v1/guardrails/policy",
            get(read_policy).put(write_policy),
        )
        .route("/api/v1/guardrails/decisions/{request_id}", get(decisions))
        .layer(axum::extract::DefaultBodyLimit::max(128 * 1024))
        .with_state(StateData { service, auth })
}
struct Authority {
    tenant: String,
    user: String,
    manage: bool,
}
async fn authority(s: &StateData, h: &HeaderMap, path: &str) -> LlmResult<Authority> {
    let request = s.auth.request(h, path);
    let p = request
        .resolve()
        .await
        .map_err(|e| LlmError::new(e.status(), "llm_guardrails_auth_required"))?;
    if p.user_id().is_empty() || p.tenant_id().is_empty() {
        return Err(LlmError::new(403, "llm_guardrails_forbidden"));
    }
    Ok(Authority {
        tenant: p.tenant_id().into(),
        user: p.user_id().into(),
        manage: p.roles().iter().any(|r| r == "admin")
            || p.permissions().iter().any(|p| p == "guardrails:manage"),
    })
}
fn response(value: Value) -> Response {
    let mut r = Json(value).into_response();
    r.headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    r
}
async fn read_policy(State(s): State<StateData>, headers: HeaderMap) -> LlmResult<Response> {
    let a = authority(&s, &headers, "/api/v1/guardrails/policy").await?;
    if !a.manage {
        return Err(LlmError::new(403, "llm_guardrails_forbidden"));
    }
    let raw:Option<Value>=sqlx::query_scalar("SELECT metadata->'guardrails_v1' FROM gateway.tenants WHERE tenant_id=$1 AND status='active'").bind(&a.tenant).fetch_optional(&s.service.pool).await.map_err(|_|policy::invalid())?.ok_or(LlmError::new(404,"llm_guardrails_tenant_missing"))?;
    let pin = stored_pin(raw.as_ref())?;
    Ok(response(json!({"policy":raw,"policy_pin":pin})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation {
    policy: Policy,
    expected_pin: Option<String>,
}
async fn write_policy(
    State(s): State<StateData>,
    headers: HeaderMap,
    Json(input): Json<Mutation>,
) -> LlmResult<Response> {
    let a = authority(&s, &headers, "/api/v1/guardrails/policy").await?;
    if !a.manage {
        return Err(LlmError::new(403, "llm_guardrails_forbidden"));
    }
    let pin = compile_policy(input.policy.clone()).await?.pin;
    let mut tx = s
        .service
        .pool
        .begin()
        .await
        .map_err(|_| policy::invalid())?;
    let old:Option<Value>=sqlx::query_scalar("SELECT metadata->'guardrails_v1' FROM gateway.tenants WHERE tenant_id=$1 AND status='active' FOR UPDATE").bind(&a.tenant).fetch_optional(&mut *tx).await.map_err(|_|policy::invalid())?.ok_or(LlmError::new(404,"llm_guardrails_tenant_missing"))?;
    let old_pin = stored_pin(old.as_ref())?;
    if old_pin != input.expected_pin {
        return Err(LlmError::new(409, "llm_guardrails_policy_conflict"));
    }
    sqlx::query("UPDATE gateway.tenants SET metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{guardrails_v1}',$2::jsonb,true),updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1").bind(&a.tenant).bind(serde_json::to_value(&input.policy).map_err(|_|policy::invalid())?).execute(&mut *tx).await.map_err(|_|policy::invalid())?;
    sqlx::query("INSERT INTO gateway.audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,response_summary,status) VALUES('guardrails_policy_changed',$1,$2,'tenant_guardrails',$2,'update',$3,$4,'success')").bind(&a.user).bind(&a.tenant).bind(json!({"previous_pin":old_pin})).bind(json!({"policy_pin":pin,"version":input.policy.version,"enabled":input.policy.enabled})).execute(&mut *tx).await.map_err(|_|policy::invalid())?;
    tx.commit()
        .await
        .map_err(|_| LlmError::new(503, "llm_guardrails_policy_commit_unconfirmed"))?;
    Ok(response(json!({"policy_pin":pin})))
}
async fn decisions(
    State(s): State<StateData>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> LlmResult<Response> {
    let a = authority(&s, &headers, "/api/v1/guardrails/decisions").await?;
    if id.len() > 255 || id.chars().any(char::is_control) {
        return Err(LlmError::new(400, "llm_guardrails_request_invalid"));
    }
    let rows=sqlx::query("SELECT id,response_summary,created_at::text AS created_at FROM gateway.audit_logs WHERE tenant_id=$1 AND resource_id=$2 AND event_type='guardrail_decision' AND (user_id=$3 OR $4) ORDER BY id LIMIT 16").bind(&a.tenant).bind(&id).bind(&a.user).bind(a.manage).fetch_all(&s.service.pool).await.map_err(|_|policy::invalid())?;
    if rows.is_empty() {
        return Err(LlmError::new(404, "llm_guardrails_decision_missing"));
    }
    let decisions=rows.into_iter().map(|r|Ok(json!({"audit_id":r.try_get::<i64,_>("id").map_err(|_|policy::invalid())?,"decision":r.try_get::<Value,_>("response_summary").map_err(|_|policy::invalid())?,"created_at":r.try_get::<String,_>("created_at").map_err(|_|policy::invalid())?}))).collect::<LlmResult<Vec<_>>>()?;
    Ok(response(json!({"request_id":id,"decisions":decisions})))
}

fn stored_pin(value: Option<&Value>) -> LlmResult<Option<String>> {
    value
        .filter(|v| !v.is_null())
        .map(|v| {
            let raw = serde_json::to_vec(v).map_err(|_| policy::invalid())?;
            ai_platform_gateway_foundation::canonical_json_v2::CanonicalDocument::from_json(&raw)
                .and_then(|d| d.sha256_hex())
                .map_err(|_| policy::invalid())
        })
        .transpose()
}
