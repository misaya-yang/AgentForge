pub use ai_platform_gateway_foundation::request_governance;
pub use ai_platform_gateway_foundation::distributed_tracing;
pub use ai_platform_gateway_foundation::assistant_catalog;
pub use ai_platform_gateway_foundation::billing;
pub use ai_platform_gateway_foundation::billing_store;
pub use ai_platform_gateway_foundation::capacity;
pub use ai_platform_gateway_foundation::capacity_admission;
pub use ai_platform_gateway_foundation::circuit_breaker;
pub use ai_platform_gateway_foundation::client_ip;
pub mod browser_telemetry;
pub mod config;
pub use ai_platform_gateway_foundation::dispatch_preflight;
pub use ai_platform_gateway_foundation::dispatcher;
pub use ai_platform_gateway_foundation::dispatcher_rate_limiter;
pub use ai_platform_gateway_foundation::gateway_dispatcher;
pub use ai_platform_gateway_foundation::generic_rest_adapter;
pub(crate) use ai_platform_gateway_foundation::http_content_type;
pub use ai_platform_gateway_foundation::invocation_http;
pub mod knowledge;
pub mod knowledge_http;
pub mod knowledge_model_plane;
pub use ai_platform_gateway_foundation::load_shedder;
pub use ai_platform_gateway_foundation::pricing_receipt;
pub use ai_platform_gateway_foundation::protocol;
pub use ai_platform_gateway_foundation::provider_routing;
pub use ai_platform_gateway_foundation::quota_store;
pub use ai_platform_gateway_foundation::rate_limits;
pub use ai_platform_gateway_foundation::rate_policy_store;
pub use ai_platform_gateway_foundation::registry_storage;
pub use ai_platform_gateway_foundation::request_validation;
pub use ai_platform_gateway_foundation::service_ownership;
pub use ai_platform_gateway_foundation::service_registry;
pub mod session_history;
pub use ai_platform_gateway_foundation::session_manager;
pub use ai_platform_gateway_foundation::session_storage;
pub use ai_platform_gateway_foundation::stream_statistics;
pub use ai_platform_gateway_foundation::task_http;
pub use ai_platform_gateway_foundation::task_actor;
pub use ai_platform_gateway_foundation::task_delivery;
pub use ai_platform_gateway_foundation::task_manager;
pub use ai_platform_gateway_foundation::task_storage;
pub use ai_platform_gateway_foundation::task_worker;
pub use ai_platform_gateway_foundation::usage_parser;
pub use ai_platform_gateway_foundation::usage_pricing;
pub use ai_platform_gateway_foundation::usage_recorder;
// Gateway migration route factories; no listener or alternative Agent loop.
use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::AuthService,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use ai_platform_gateway_foundation::native_contract::sessions::{self as session_contract, ListLimit as Limit, SessionList};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use std::sync::Arc;
#[derive(Clone)]
struct Gateway {
    auth: Arc<AuthService>,
    pool: PgPool,
    identity: Option<Arc<dyn migration_content::ContentAccess>>,
}
/// Shared native user-context boundary. Mount alongside auth::http::router on the same listener.
#[derive(Clone, Default)]
pub struct GatewayOptions {
    pub identity: Option<Arc<dyn migration_content::ContentAccess>>,
    pub runtime: config::RuntimeReadConfig,
    pub sessions: session_history::SessionConfig,
    pub catalog: assistant_catalog::AssistantCatalogOptions,
    pub knowledge: Option<Arc<knowledge::KnowledgeService>>,
}
pub fn router(auth: Arc<AuthService>, pool: PgPool) -> Router {
    router_with_options(auth, pool, GatewayOptions::default())
}
pub fn router_with_options(
    auth: Arc<AuthService>,
    pool: PgPool,
    options: GatewayOptions,
) -> Router {
    base_router(auth, pool, options, true)
}
/// Host migration mounts the complete signed knowledge proxy separately.
pub fn router_without_knowledge_proxy(
    auth: Arc<AuthService>,
    pool: PgPool,
    options: GatewayOptions,
) -> Router {
    base_router(auth, pool, options, false)
}
fn base_router(
    auth: Arc<AuthService>,
    pool: PgPool,
    mut options: GatewayOptions,
    include_knowledge_proxy: bool,
) -> Router {
    options.runtime.kb_enabled = options.knowledge.is_some();
    if options.sessions.source_access.is_none()
        && let Some(knowledge) = options.knowledge.clone()
    {
        options.sessions.source_access = Some(knowledge);
    }
    let knowledge_proxy = if include_knowledge_proxy {
        knowledge_http::router(auth.clone(), options.knowledge.clone())
    } else {
        Router::new()
    };
    Router::new()
        .route(session_contract::COLLECTION, get(sessions).head(read_method_not_allowed))
        .route("/api/v1/assistant/models", get(models::list).head(read_method_not_allowed))
        .with_state(Gateway {
            auth: auth.clone(),
            pool: pool.clone(),
            identity: options.identity.clone(),
        })
        .merge(config::router_with_identity(auth.clone(), pool.clone(), options.runtime, options.identity.clone()))
        .merge(knowledge::router_with_identity(auth.clone(), options.knowledge.clone(), options.identity.clone()))
        .merge(knowledge_proxy)
        .merge(session_history::router(
            auth.clone(),
            pool.clone(),
            options.sessions,
            options.identity.clone(),
        ))
        .merge(assistant_catalog::router_with_identity(auth, pool, options.catalog,
            options.identity.map(|identity| Arc::new(CatalogReadIdentity(identity)) as Arc<dyn assistant_catalog::CatalogIdentityResolver>)))
}

#[derive(Serialize, FromRow)]
struct SessionSummary {
    session_id: String,
    user_id: String,
    tenant_id: String,
    service_id: Option<String>,
    #[serde(serialize_with = "serialize_time")]
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(serialize_with = "serialize_time")]
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
    metadata: Value,
    message_count: i32,
}
fn serialize_time<S: serde::Serializer>(
    value: &Option<chrono::DateTime<chrono::Utc>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value
        .map(|time| {
            time.to_rfc3339_opts(
                if time.timestamp_subsec_micros() == 0 {
                    chrono::SecondsFormat::Secs
                } else {
                    chrono::SecondsFormat::Micros
                },
                false,
            )
        })
        .serialize(serializer)
}
fn error(status: StatusCode, detail: &'static str) -> Response {
    (status, Json(json!({"detail":detail}))).into_response()
}
struct CatalogReadIdentity(Arc<dyn migration_content::ContentAccess>);
impl assistant_catalog::CatalogIdentityResolver for CatalogReadIdentity {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap)
        -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<assistant_catalog::CatalogIdentity, Response>> + Send + 'a>> {
        Box::pin(async move {
            let user = self.0.resolve(headers).await.map_err(IntoResponse::into_response)?;
            Ok(assistant_catalog::CatalogIdentity {
                user_id: user.user_id, tenant_id: user.tenant_id, tier: user.tier,
                roles: user.roles, is_authenticated: user.is_authenticated,
            })
        })
    }
}

/// Resolve the same verified user-context authority used by content routes.
/// Standalone legacy factories retain JWT support; the production graph always
/// supplies NativeContentAccess, including API-key and anonymous-cookie policy.
async fn resolve_read_identity(
    auth: &AuthService,
    identity: Option<&dyn migration_content::ContentAccess>,
    headers: &HeaderMap,
) -> Result<migration_content::ContentIdentity, Response> {
    if let Some(identity) = identity {
        return identity.resolve(headers).await.map_err(IntoResponse::into_response);
    }
    let token = match select_credential(headers, &HeaderName::from_static("x-api-key")) {
        Ok(SelectedCredential::Jwt(token)) => token,
        _ => return Err(error(StatusCode::UNAUTHORIZED, "Authentication required")),
    };
    let principal = auth.authenticate(token).await.map_err(|e| error(
        StatusCode::from_u16(e.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        "Authentication failed",
    ))?;
    Ok(migration_content::ContentIdentity {
        user_id: principal.account.user_id,
        tenant_id: principal.account.tenant_id,
        tier: principal.authority.tier,
        roles: principal.authority.roles,
        permissions: principal.authority.permissions,
        is_authenticated: true,
    })
}

async fn sessions(
    State(state): State<Gateway>,
    headers: HeaderMap,
    query: Result<Query<Limit>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let limit = match query {
        Ok(Query(Limit { limit })) if (1..=session_contract::LIST_MAX).contains(&limit) => limit,
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid limit"),
    };
    let principal = match resolve_read_identity(&state.auth, state.identity.as_deref(), &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    // Identity always comes from verified authority, never optional public filters.
    if principal.user_id.is_empty() || principal.tenant_id.is_empty() {
        return error(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    let result = sqlx::query_as::<_, SessionSummary>("SELECT session_id,user_id,tenant_id,service_id,created_at,updated_at,jsonb_build_object('title',metadata->'title','langgraph_thread_id',metadata->'langgraph_thread_id','folder',metadata->'folder','pinned',metadata->'pinned') AS metadata,0::integer AS message_count FROM assistant.sessions WHERE user_id=$1 AND tenant_id=$2 AND (service_id=ANY($3::text[]) OR service_id IS NULL OR service_id='') AND status='active' ORDER BY updated_at DESC LIMIT $4")
        .bind(&principal.user_id).bind(&principal.tenant_id).bind(vec!["__builtin_assistant__", "assistant"]).bind(limit).fetch_all(&state.pool).await;
    match result {
        Ok(sessions) => {
            let total = sessions.len();
            Json(SessionList { sessions, total }).into_response()
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list sessions"),
    }
}

pub(crate) use ai_platform_gateway_foundation::capabilities;

mod models;

pub use ai_platform_gateway_foundation::registry_health_monitor;

pub use ai_platform_gateway_foundation::guest_session_manager;
pub use ai_platform_gateway_foundation::guest_session_redis;

pub use ai_platform_gateway_foundation::billing_scheduler;

pub use ai_platform_gateway_foundation::registry_load_balancer;

pub(crate) use ai_platform_gateway_foundation::postgres_operation;

pub use ai_platform_gateway_foundation::model_failover;

pub use ai_platform_gateway_foundation::metrics_redaction;

pub use ai_platform_gateway_foundation::metrics_recorder;
pub use ai_platform_gateway_foundation::provider_health;

pub use ai_platform_gateway_foundation::model_service;
pub use ai_platform_gateway_foundation::provider_service;

pub use ai_platform_gateway_foundation::model_provider_storage;
pub use ai_platform_gateway_foundation::provider_probe_http;

pub use ai_platform_gateway_foundation::realtime_metrics;

pub use ai_platform_gateway_foundation::gateway_model_meta;

pub use ai_platform_gateway_foundation::provider_templates;

pub use ai_platform_gateway_foundation::model_catalog_sync;

pub use ai_platform_gateway_foundation::provider_startup_seeder;

pub use ai_platform_gateway_foundation::config_audit_postgres;
pub use ai_platform_gateway_foundation::config_audit_writer;

pub use ai_platform_gateway_foundation::catalog_http;

pub use ai_platform_gateway_foundation::model_admin_api;

pub use ai_platform_gateway_foundation::provider_admin_api;
pub use ai_platform_gateway_foundation::provider_cipher;

pub use ai_platform_gateway_foundation::metrics_admin_api;

pub use ai_platform_gateway_foundation::usage_admin_api;

pub use ai_platform_gateway_foundation::quota_admin_api;

pub use ai_platform_gateway_foundation::balancer_config_api;

pub use ai_platform_gateway_foundation::rate_config_api;

pub use ai_platform_gateway_foundation::sessions_api;

pub use ai_platform_gateway_foundation::health_api;

pub use ai_platform_gateway_foundation::dashboard_api;

pub use ai_platform_gateway_foundation::service_config_api;

pub use ai_platform_gateway_foundation::service_access_policy;

pub use ai_platform_gateway_foundation::langgraph_service_validation;

pub use ai_platform_gateway_foundation::service_access_resolver;

pub use ai_platform_gateway_foundation::services_api;

pub use ai_platform_gateway_foundation::auth_config_api;

pub use ai_platform_gateway_foundation::system_status_api;

pub use ai_platform_gateway_foundation::config_api_keys_api;

pub use ai_platform_gateway_foundation::config_api_keys_store;

pub use ai_platform_gateway_foundation::self_service_api_keys;

pub use ai_platform_gateway_foundation::self_service_api_keys_api;

pub use ai_platform_gateway_foundation::self_service_api_keys_store;

pub use ai_platform_gateway_foundation::users_api;

pub use ai_platform_gateway_foundation::users_store;

pub use ai_platform_gateway_foundation::roles_api;

pub use ai_platform_gateway_foundation::roles_store;

pub use ai_platform_gateway_foundation::tenant_policies_api;

pub use ai_platform_gateway_foundation::tenant_policies_store;

pub use ai_platform_gateway_foundation::setup_api;

pub use ai_platform_gateway_foundation::architecture_status_api;

pub use ai_platform_gateway_foundation::presign_api;

pub use ai_platform_gateway_foundation::connector_admin_api;

pub use ai_platform_gateway_foundation::connector_admin_store;

pub use ai_platform_gateway_foundation::tool_inventory_api;

pub mod artifact_shares_api;

pub mod artifact_shares_store;

pub mod artifact_share_scope;

pub use ai_platform_gateway_foundation::mcp_discovery;

pub use ai_platform_gateway_foundation::mcp_gateway_broker;

pub use ai_platform_gateway_foundation::mcp_http;

pub use ai_platform_gateway_foundation::mcp_registry_api;

pub use ai_platform_gateway_foundation::mcp_registry_store;

pub use ai_platform_gateway_foundation::mcp_registry_validation;

pub use ai_platform_gateway_foundation::local_nodes_types;

pub use ai_platform_gateway_foundation::local_nodes_api;

pub use ai_platform_gateway_foundation::local_nodes_validation;

pub use ai_platform_gateway_foundation::local_nodes_store;

pub mod quiz_types;

pub mod quiz_api;

pub mod quiz_rate;

pub mod quiz_scope;

pub mod quiz_store;

pub mod quiz_share_store;

pub mod quiz_share_service;

pub use ai_platform_gateway_foundation::quiz_grading;

pub mod quiz_entropy;

pub(crate) use ai_platform_gateway_foundation::quiz_unicode;

pub use ai_platform_gateway_foundation::assistant_memory_types;

pub use ai_platform_gateway_foundation::assistant_memory_store;

pub use ai_platform_gateway_foundation::assistant_memory_api;

pub use ai_platform_gateway_foundation::assistant_memory_validation;

pub use ai_platform_gateway_foundation::connector_user_types;

pub use ai_platform_gateway_foundation::connector_user_store;

pub use ai_platform_gateway_foundation::connector_user_service;

pub use ai_platform_gateway_foundation::connector_user_api;

pub use ai_platform_gateway_foundation::connector_user_search;

pub use ai_platform_gateway_foundation::connector_user_activation;

pub use ai_platform_gateway_foundation::connector_oauth_state;

pub use ai_platform_gateway_foundation::connector_oauth_cipher;

pub use ai_platform_gateway_foundation::connector_oauth_http;

pub use ai_platform_gateway_foundation::connector_principal_store;

pub use ai_platform_gateway_foundation::connector_principal_validation;

pub mod migration_host;
pub use ai_platform_gateway_integrations::migration_adapters;

pub mod migration_agent;

pub use ai_platform_gateway_integrations::migration_knowledge_ingest;

pub mod migration_knowledge_query;

pub mod migration_content;

pub mod migration_compat;

pub use ai_platform_gateway_integrations::migration_local_node;
mod local_node_content_bridge;

pub mod migration_trace;

#[cfg(test)]
mod native_session_contract_tests {
    use super::*;
    #[test]
    fn actual_list_serializer_matches_source_owned_session_shape() {
        let row = SessionSummary {
            session_id: "s".into(), user_id: "u".into(), tenant_id: "t".into(),
            service_id: None, created_at: None, updated_at: None,
            metadata: json!({"title":null,"folder":null,"pinned":null,"langgraph_thread_id":null}),
            message_count: 0,
        };
        let actual = serde_json::to_value(SessionList { sessions: vec![row], total: 1 }).unwrap();
        let shape = session_contract::session_schema();
        assert_eq!(actual["sessions"][0].as_object().unwrap().keys().collect::<Vec<_>>(),
            shape["properties"].as_object().unwrap().keys().collect::<Vec<_>>());
        assert_eq!(actual["sessions"][0]["created_at"],Value::Null);
        assert_eq!(actual["sessions"][0]["message_count"],0);
        assert_eq!(actual["total"],1);
    }
    #[test]
    fn actual_query_extractor_preserves_limit_rules() {
        let uri = "/?unknown=ignored".parse().unwrap();
        assert_eq!(Query::<Limit>::try_from_uri(&uri).unwrap().0.limit,50);
        assert_eq!(Query::<session_contract::HistoryLimit>::try_from_uri(&uri).unwrap().0.limit,100);
        for invalid in ["/?limit=2&limit=3", "/?limit=1.5", "/?limit=oops"] {
            let uri = invalid.parse().unwrap();
            assert!(Query::<Limit>::try_from_uri(&uri).is_err());
            assert!(Query::<session_contract::HistoryLimit>::try_from_uri(&uri).is_err());
        }
        for (text, allowed) in [("/?limit=0",false),("/?limit=1",true),("/?limit=200",true),("/?limit=201",false)] {
            let uri = text.parse().unwrap();
            let limit=Query::<Limit>::try_from_uri(&uri).unwrap().0.limit;
            assert_eq!((1..=session_contract::LIST_MAX).contains(&limit),allowed);
        }
    }
}

#[cfg(test)]
mod read_identity_tests;

/// FastAPI GET declarations do not implicitly expose HEAD.
pub(crate) async fn read_method_not_allowed() -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::METHOD_NOT_ALLOWED,
     [("allow", "GET")],
     axum::Json(serde_json::json!({"detail":"Method Not Allowed"}))).into_response()
}
pub mod llm_api;

pub mod task_batch_accounting;
