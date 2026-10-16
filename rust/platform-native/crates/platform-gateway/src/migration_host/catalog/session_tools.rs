//! Source-equivalent assistant session tool diagnostics. The next-turn estimate
//! reuses the live Runtime catalog; no client descriptor becomes executable.
use super::*;
use crate::{
    assistant_catalog::{PolicySettings, project_policies, project_tools},
    migration_agent::control::RuntimeControlPlane,
    migration_content::{ContentAccess, ContentError},
    sessions_api::SessionsApiManager,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use std::collections::HashMap;

#[derive(Clone)]
pub struct SessionToolsState {
    pub catalog: Arc<CatalogService>,
    pub access: Arc<dyn ContentAccess>,
    pub sessions: Arc<dyn SessionsApiManager>,
    pub control: Arc<RuntimeControlPlane>,
    pub default_model: String,
}

pub fn router(state: SessionToolsState) -> Router {
    Router::new()
        .route(
            crate::migration_host::native_public_contract::SESSION_TOOLS,
            get(tools).head(method_not_allowed),
        )
        .with_state(state)
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [("allow", "GET")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}

fn unavailable() -> ContentError {
    ContentError::new(503, "Tool catalog is unavailable")
}

fn projection(readonly: &Value, records: &HashMap<&str, &Value>) -> Vec<Value> {
    ["tools", "mcp", "deferred"].into_iter().flat_map(|key| {
        readonly[key].as_array().into_iter().flatten().filter_map(|descriptor| {
            let name = descriptor["name"].as_str()?;
            let record = records.get(name).copied().unwrap_or(&Value::Null);
            let fallback = |key: &str| {
                descriptor.get(key).filter(|v| crate::service_registry::python_truthy(v))
                    .or_else(|| record.get(key).filter(|v| crate::service_registry::python_truthy(v)))
                    .cloned().unwrap_or(json!("unknown"))
            };
            Some(json!({"name":name,"effect":fallback("effect"),"approval":fallback("approval"),
                "required_inputs":record["input_schema"]["required"].as_array().cloned().unwrap_or_default(),
                "device_required":matches!(name,"local_node_catalog"|"local_node_action")}))
        })
    }).collect()
}

async fn tools(
    State(state): State<SessionToolsState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let user = state.access.resolve(&headers).await?;
    if !user.is_authenticated {
        return Err(ContentError::new(401, "Authentication required"));
    }
    let session = state
        .sessions
        .get(&session_id)
        .await
        .map_err(|_| ContentError::new(500, "Failed to verify session"))?
        .filter(|s| s.user_id == user.user_id && s.tenant_id == user.tenant_id)
        .ok_or_else(|| ContentError::new(404, "Session not found"))?;
    let policy = sqlx::query_scalar::<_, Value>("SELECT jsonb_build_object('allowed_tools',allowed_tools,'blocked_tools',blocked_tools,'allowed_categories',allowed_categories,'max_calls_per_session',max_calls_per_session,'max_calls_per_minute',max_calls_per_minute) FROM tenant_tool_policies WHERE tenant_id=$1")
        .bind(&user.tenant_id).fetch_optional(&state.catalog.pool).await.map_err(|_| unavailable())?.unwrap_or(json!({}));
    let policy = project_policies(&state.catalog.records, &policy, &PolicySettings::default())
        .map_err(|_| unavailable())?;
    let visible: BTreeSet<String> = project_tools(
        &state.catalog.records,
        true,
        &user.tier,
        &user.roles,
        &serde_json::to_value(policy).map_err(|_| unavailable())?,
    )
    .map_err(|_| unavailable())?
    .into_iter()
    .map(|tool| tool.name)
    .collect();
    let records: HashMap<&str, &Value> = state
        .catalog
        .records
        .records()
        .iter()
        .filter_map(|record| record["name"].as_str().map(|name| (name, record)))
        .collect();
    let row = sqlx::query_as::<_, (Value, String, String)>("SELECT s.snapshot,r.run_id,r.status FROM assistant_runtime_snapshots AS s JOIN assistant_runs AS r ON r.run_id=s.run_id WHERE s.session_id=$1 AND s.tenant_id=$2 AND s.user_id=$3 ORDER BY s.created_at DESC LIMIT 1")
        .bind(&session_id).bind(&user.tenant_id).bind(&user.user_id).fetch_optional(&state.catalog.pool).await.map_err(|_| ContentError::new(500,"Failed to load session tool snapshot"))?;
    let last = match row {
        None => Value::Null,
        Some((snapshot, run, status)) => {
            let snapshot = match snapshot {
                Value::String(raw) => serde_json::from_str(&raw)?,
                other => other,
            };
            json!({"run_id":run,"status":status,"tools":projection(&snapshot["readonly_capabilities"],&records)})
        }
    };
    let model = session
        .config
        .as_ref()
        .and_then(|v| v.get("selected_model"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .unwrap_or(&state.default_model);
    let mut current = json!({"status":"unavailable","tools":[]});
    if !model.is_empty() {
        let scope = Scope {
            tenant_id: user.tenant_id.clone(),
            user_id: user.user_id.clone(),
            session_id,
        };
        if let Ok(model_row) = state
            .control
            .models
            .model_optional(&user.tenant_id, model, None)
            .await
        {
            let revision = model_row
                .as_ref()
                .and_then(|row| row["capability_revision"].as_i64())
                .filter(|n| *n != 0)
                .unwrap_or(1);
            if let Ok(readonly) = state
                .control
                .catalog
                .fetch(&scope, model, revision, None)
                .await
            {
                current = json!({"status":"available_now","tools":projection(&readonly,&records)});
            }
        }
    }
    // Preserve source catalog order rather than HashMap iteration order.
    let platform: Vec<Value> = state.catalog.records.records().iter().map(|record| {
        let name = record["name"].as_str().unwrap_or("");
        json!({"name":name,"description":record["description"],"tenant_visible":visible.contains(name),
            "required_inputs":record["input_schema"]["required"].as_array().cloned().unwrap_or_default(),
            "device_required":matches!(name,"local_node_catalog"|"local_node_action")})
    }).collect();
    Ok(Json(
        json!({"platform_catalog":platform,"next_turn_estimate":current,"last_run_pinned":last,
        "note":"The next run pins its own tool set; Worker, permission and device state may change before it starts."}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_preserves_groups_and_falls_back_to_catalog() {
        let record = json!({"effect":"read","approval":"none","input_schema":{"required":["id"]}});
        let records = HashMap::from([("local_node_action", &record)]);
        let actual = projection(
            &json!({"tools":[null,{"name":"local_node_action","effect":""}],"mcp":[{"name":"external","effect":"write","approval":"ask"}],"deferred":[{"bad":true}]}),
            &records,
        );
        assert_eq!(actual,json!([{"name":"local_node_action","effect":"read","approval":"none","required_inputs":["id"],"device_required":true},{"name":"external","effect":"write","approval":"ask","required_inputs":[],"device_required":false}]).as_array().unwrap().clone());
    }
}
