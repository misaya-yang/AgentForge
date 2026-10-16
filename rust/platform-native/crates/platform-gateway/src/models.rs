use super::{Gateway, error};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::FromRow;
#[derive(FromRow)]
struct ModelRow {
    model_id: String,
    provider_id: String,
    display_name: Option<String>,
    context_window: i64,
    max_output_tokens: i64,
    capability_revision: i64,
    supports_tools: bool,
    supports_vision: bool,
    catalog_capabilities: Value,
    capability_overrides: Value,
    input_price_per_1k: f64,
    output_price_per_1k: f64,
    access_level: String,
    sort_order: i64,
}
#[derive(Serialize)]
pub(crate) struct Model {
    pub(crate) id: String,
    name: String,
    pub(crate) provider: String,
    context_window: i64,
    max_output_tokens: i64,
    supports_vision: bool,
    supports_tools: bool,
    access_level: String,
    input_price_per_1k: f64,
    output_price_per_1k: f64,
    effective_capabilities: Value,
    capability_revision: i64,
    #[serde(skip)]
    sort_order: i64,
}
fn can_access(tier: &str, roles: &[String], required: &str) -> bool {
    if !["public", "premium", "admin"].contains(&required) {
        return false;
    }
    if tier == "admin" || roles.iter().any(|r| r == "admin") {
        return true;
    }
    required == "public"
        || (required == "premium" && ["premium", "enterprise", "admin"].contains(&tier))
}
fn project(rows: Vec<ModelRow>, tier: &str, roles: &[String]) -> Vec<Model> {
    let mut models = Vec::new();
    for row in rows {
        let id = row.model_id.trim().to_owned();
        let provider = row.provider_id.trim().to_owned();
        if id.is_empty()
            || provider.is_empty()
            || row.context_window <= 0
            || row.max_output_tokens <= 0
            || row.capability_revision < 1
            || !row.input_price_per_1k.is_finite()
            || !row.output_price_per_1k.is_finite()
            || !can_access(tier, roles, &row.access_level)
        {
            continue;
        }
        let effective_capabilities = super::capabilities::effective(
            &provider,
            &id,
            &row.catalog_capabilities,
            &row.capability_overrides,
            row.supports_tools,
            row.supports_vision,
        );
        models.push(Model {
            name: row
                .display_name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone()),
            id,
            provider,
            context_window: row.context_window,
            max_output_tokens: row.max_output_tokens,
            supports_vision: row.supports_vision,
            supports_tools: row.supports_tools,
            access_level: row.access_level,
            input_price_per_1k: row.input_price_per_1k,
            output_price_per_1k: row.output_price_per_1k,
            effective_capabilities,
            capability_revision: row.capability_revision,
            sort_order: row.sort_order,
        });
    }
    models.sort_by(|a, b| {
        (&a.provider, a.sort_order, &a.name, &a.id).cmp(&(
            &b.provider,
            b.sort_order,
            &b.name,
            &b.id,
        ))
    });
    models
}
pub(super) async fn list(State(state): State<Gateway>, headers: HeaderMap) -> Response {
    let principal = match super::resolve_read_identity(&state.auth, state.identity.as_deref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    match visible(&state.pool, &principal).await {
        Ok(models) => Json(json!({"models":models})).into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Model service is unavailable",
        ),
    }
}
pub(crate) async fn visible(
    pool: &sqlx::PgPool,
    principal: &crate::migration_content::ContentIdentity,
) -> Result<Vec<Model>, sqlx::Error> {
    let roles = &principal.roles;
    let tier = if roles.iter().any(|r| r == "admin") {
        "admin"
    } else if roles.iter().any(|r| r == "enterprise") {
        "enterprise"
    } else if roles.iter().any(|r| r == "premium" || r == "vip") {
        "premium"
    } else {
        &principal.tier
    };
    let rows=sqlx::query_as::<_,ModelRow>("SELECT model_id,provider_id,display_name,COALESCE(context_window,0)::bigint AS context_window,COALESCE(max_output_tokens,0)::bigint AS max_output_tokens,COALESCE(supports_vision,FALSE) AS supports_vision,COALESCE(supports_tools,FALSE) AS supports_tools,COALESCE(catalog_capabilities,'{}'::jsonb) AS catalog_capabilities,COALESCE(capability_overrides,'{}'::jsonb) AS capability_overrides,COALESCE(capability_revision,0)::bigint AS capability_revision,COALESCE(input_price_per_1k,0)::double precision AS input_price_per_1k,COALESCE(output_price_per_1k,0)::double precision AS output_price_per_1k,COALESCE(access_level,'') AS access_level,COALESCE(sort_order,0)::bigint AS sort_order FROM gateway.llm_models WHERE tenant_id=$1 AND is_enabled=TRUE")
        .bind(&principal.tenant_id).fetch_all(pool).await?;
    Ok(project(rows, tier, roles))
}

