use super::*;
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
#[derive(Clone)]
pub struct CatalogHttpState {
    pub service: Arc<CatalogService>,
    pub internal_token: Arc<String>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    tenant_id: String,
    user_id: String,
    session_id: String,
    model_id: String,
    capability_revision: i64,
    #[serde(default)]
    capability_allowlist: Option<Vec<Value>>,
}
pub fn router(state: CatalogHttpState) -> Router {
    Router::new()
        .route("/internal/v2/agent-capabilities/catalog", post(catalog))
        .with_state(state)
}
fn response(status: u16, detail: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"detail":detail})),
    )
        .into_response()
}
async fn catalog(State(state): State<CatalogHttpState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, 4 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return response(413, "request too large"),
    };
    let p: Payload = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return response(422, "Invalid catalog request"),
    };
    if [&p.tenant_id, &p.user_id, &p.session_id, &p.model_id]
        .iter()
        .any(|s| s.is_empty() || s.chars().count() > 255)
        || p.capability_revision < 1
        || p.capability_allowlist
            .as_ref()
            .is_some_and(|a| a.iter().any(|v| !v.is_object()))
    {
        return response(422, "Invalid catalog request");
    }
    let header = |key: &str| {
        parts
            .headers
            .get(key)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let token = header("x-ai-platform-internal-token");
    if state.internal_token.is_empty()
        || token.is_empty()
        || !crate::migration_content::capabilities::constant_time_equal(
            &state.internal_token,
            token,
        )
    {
        return response(401, "internal authorization failed");
    }
    for (key, field, wanted) in [
        ("x-ai-tenant-id", "tenant", &p.tenant_id),
        ("x-ai-user-id", "user", &p.user_id),
        ("x-ai-session-id", "session", &p.session_id),
    ] {
        let value = header(key);
        if value.is_empty() || value.chars().count() > 255 || value.chars().any(|c| (c as u32) < 32)
        {
            return response(403, &format!("{field} scope is invalid"));
        }
        if value != wanted {
            return response(403, "catalog scope mismatch");
        }
    }
    let scope = Scope {
        tenant_id: p.tenant_id,
        user_id: p.user_id,
        session_id: p.session_id,
    };
    match state
        .service
        .resolve(
            &scope,
            &p.model_id,
            p.capability_revision,
            p.capability_allowlist.as_deref(),
        )
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            let detail = match text(&e.detail, "code") {
                "CAPABILITY_CATALOG_SCOPE_INVALID" => "catalog scope is invalid",
                "CAPABILITY_CATALOG_SCOPE_UNAUTHORIZED" => "catalog scope is not authorized",
                "CAPABILITY_CATALOG_IDENTITY_UNAVAILABLE" => "catalog identity unavailable",
                "CAPABILITY_CATALOG_POLICY_UNAVAILABLE" => "catalog policy unavailable",
                "CAPABILITY_CATALOG_SOURCE_UNAVAILABLE" => "capability catalog unavailable",
                "CAPABILITY_CATALOG_WORKER_UNAVAILABLE" => "capability worker unavailable",
                _ => "capability catalog is invalid",
            };
            response(e.status, detail)
        }
    }
}
