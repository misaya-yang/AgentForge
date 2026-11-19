use super::*;
use axum::{Json, http::StatusCode};
use serde_json::Value;
const CONFIRMATION: &str = "_gateway_delete_confirmation";
pub(super) async fn prepare_body(
    state: &KnowledgeProxy,
    user: &ContentIdentity,
    method: &Method,
    path: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>> {
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) if e.to_string().contains("recursion limit") => {
            return Err(code(400, "KB_JSON_NESTING_LIMIT"));
        }
        Err(_) => Value::Null,
    };
    if payload.get(CONFIRMATION).is_some() {
        return Err(code(400, "KB_RESERVED_CONFIRMATION_FIELD"));
    }
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if !contract::dataset_delete(method.as_str(), path) {
        return Ok(body);
    }
    if !user.is_authenticated || user.tenant_id.is_empty() || user.tenant_id == "public" {
        return Err(code(403, "KB_ACCOUNT_CONFIRMATION_REQUIRED"));
    }
    let password = payload["password"]
        .as_str()
        .filter(|v| !v.is_empty() && v.chars().count() <= 128)
        .ok_or_else(|| code(400, "KB_PASSWORD_CONFIRMATION_INVALID"))?
        .to_owned();
    let tenant = user.tenant_id.clone();
    let id = user.user_id.clone();
    let hash: Option<String> = state
        .database
        .run(move |c| {
            Box::pin(async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT password_hash FROM gateway.users WHERE user_id=$1 AND tenant_id=$2",
                )
                .bind(id)
                .bind(tenant)
                .fetch_optional(c)
                .await
                .map(Option::flatten)
            })
        })
        .await
        .map_err(|_| code(503, "KB_CONFIRMATION_UNAVAILABLE"))?;
    let verified = state
        .passwords
        .verify(password, hash.unwrap_or_default())
        .await
        .map_err(|_| code(503, "KB_CONFIRMATION_UNAVAILABLE"))?;
    if !verified {
        return Err(code(403, "KB_PASSWORD_CONFIRMATION_INVALID"));
    }
    let object = payload
        .as_object_mut()
        .ok_or_else(|| code(400, "KB_PASSWORD_CONFIRMATION_INVALID"))?;
    object.remove("password");
    object.insert(CONFIRMATION.into(),json!({"action":"dataset.delete","dataset_id":parts[1],"user_id":user.user_id,"tenant_id":user.tenant_id,"expires_at":chrono::Utc::now().timestamp()+60}));
    serde_json::to_vec(&payload).map_err(|_| code(503, "KB_CONFIRMATION_UNAVAILABLE"))
}
pub(super) async fn impact(
    state: &KnowledgeProxy,
    user: &ContentIdentity,
    dataset: &str,
) -> Result<Response> {
    if !user.is_authenticated || user.user_id.is_empty() {
        return Err(ContentError::new(401, "Authentication required"));
    }
    if user.tenant_id.is_empty() || user.tenant_id == "public" {
        return Err(ContentError::new(403, "Tenant identity required"));
    }
    let path = format!("/api/v1/knowledge/datasets/{dataset}");
    let mut response = state
        .upstream(
            Method::GET,
            &path,
            "",
            super::forwarding::trusted_headers(user, &HeaderMap::new())?,
            vec![],
        )
        .await
        .map_err(|_| ContentError::new(503, "Dataset impact authority unavailable"))?;
    if !response.status().is_success() {
        return Err(match response.status() {
            StatusCode::FORBIDDEN => ContentError::new(403, "Dataset owner access required"),
            StatusCode::NOT_FOUND => ContentError::new(404, "Dataset not found"),
            _ => ContentError::new(503, "Dataset impact authority unavailable"),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let authority: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ContentError::new(503, "Dataset impact authority unavailable"))?;
    if authority["dataset_id"] != dataset {
        return Err(ContentError::new(
            503,
            "Dataset impact authority unavailable",
        ));
    }
    if authority["tenant_id"] != user.tenant_id || authority["my_permission"] != "owner" {
        return Err(ContentError::new(403, "Dataset owner access required"));
    }
    let tenant = user.tenant_id.clone();
    let id = user.user_id.clone();
    let dataset_owned = dataset.to_owned();
    let admin = user.tier.eq_ignore_ascii_case("admin")
        || user
            .roles
            .iter()
            .any(|r| r.eq_ignore_ascii_case("admin") || r.eq_ignore_ascii_case("tenant_admin"));
    let rows: Vec<Value> = state
        .database
        .run(move |c| {
            Box::pin(async move {
                sqlx::query_scalar(&format!(
                    "SELECT to_jsonb(impact) FROM ({}) impact",
                    include_str!("impact.sql")
                ))
                .bind(tenant)
                .bind(dataset_owned)
                .bind(id)
                .bind(admin)
                .fetch_all(c)
                .await
            })
        })
        .await
        .map_err(|_| ContentError::new(503, "Dataset impact unavailable"))?;
    let mut counts = json!({"current_draft":{"visible":0,"hidden":0},"active_publication":{"visible":0,"hidden":0},"historical_version":{"visible":0,"hidden":0}});
    let mut visible = vec![];
    let mut hidden = 0;
    for row in rows {
        let is_visible = row["visible"].as_bool().unwrap_or(false);
        let bucket = if is_visible { "visible" } else { "hidden" };
        for (kind, present) in [
            (
                "current_draft",
                row["current_draft"].as_bool().unwrap_or(false),
            ),
            (
                "active_publication",
                row["active_publication"].as_bool().unwrap_or(false),
            ),
            (
                "historical_version",
                row["historical_version_count"].as_i64().unwrap_or(0) > 0,
            ),
        ] {
            if present {
                counts[kind][bucket] = json!(counts[kind][bucket].as_i64().unwrap_or(0) + 1);
            }
        }
        if is_visible {
            visible.push(json!({"agent_id":row["agent_id"],"name":row["name"],"current_draft":row["current_draft"],"active_publication":row["active_publication"],"historical_version_count":row["historical_version_count"]}));
        } else {
            hidden += 1;
        }
    }
    Ok(Json(json!({"dataset_id":dataset,"visible_agents":visible,"hidden_agent_count":hidden,"counts":counts})).into_response())
}
