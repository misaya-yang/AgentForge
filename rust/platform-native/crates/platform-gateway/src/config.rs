//! Existing GatewayModelMeta/config read contract, with explicit startup inputs.
use ai_platform_auth::{
    service::AuthService,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::Value;
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Default)]
pub struct RuntimeReadConfig {
    pub default_model: String,
    /// Only provider identifiers known configured in the separate Runtime.
    pub runtime_configured_providers: BTreeSet<String>,
    pub kb_enabled: bool,
    /// Already-installed, nonsecret capability names; never a live Runtime import.
    pub installed_catalog: Option<Value>,
}
#[derive(Clone)]
struct ConfigState {
    auth: Arc<AuthService>,
    pool: PgPool,
    options: RuntimeReadConfig,
    identity: Option<Arc<dyn crate::migration_content::ContentAccess>>,
}
pub fn router(auth: Arc<AuthService>, pool: PgPool, options: RuntimeReadConfig) -> Router {
    router_with_identity(auth, pool, options, None)
}
pub fn router_with_identity(auth: Arc<AuthService>, pool: PgPool, options: RuntimeReadConfig,
    identity: Option<Arc<dyn crate::migration_content::ContentAccess>>) -> Router {
    Router::new()
        .route("/api/v1/assistant/config", get(get_config).head(super::read_method_not_allowed))
        .with_state(ConfigState {
            auth,
            pool,
            options,
            identity,
        })
}
async fn get_config(State(state): State<ConfigState>, headers: HeaderMap) -> Response {
    let principal = match super::resolve_read_identity(&state.auth, state.identity.as_deref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let models = match super::models::visible(&state.pool, &principal).await {
        Ok(m) => m,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Model service is unavailable",
            );
        }
    };
    let mut configured = BTreeMap::new();
    for provider in models
        .iter()
        .map(|m| m.provider.as_str())
        .collect::<BTreeSet<_>>()
    {
        // Return only a boolean from storage: encrypted or plaintext keys never enter this process.
        let stored=sqlx::query_as::<_,(bool,bool)>("SELECT COALESCE(is_enabled,FALSE),COALESCE(length(api_key_encrypted)>0,FALSE) FROM gateway.llm_providers WHERE tenant_id=$1 AND provider_id=$2")
            .bind(&principal.tenant_id).bind(provider).fetch_optional(&state.pool).await;
        let available = matches!(stored,Ok(Some((true,has_key))) if has_key || state.options.runtime_configured_providers.contains(provider));
        configured.insert(provider.to_owned(), available);
    }
    let visible: Vec<_> = models
        .iter()
        .map(|m| super::assistant_catalog::ConfigModel {
            id: &m.id,
            provider: &m.provider,
        })
        .collect();
    Json(super::assistant_catalog::project_config(
        &visible,
        &state.options.default_model,
        &configured,
        state.options.kb_enabled,
        state.options.installed_catalog.as_ref(),
    ))
    .into_response()
}
