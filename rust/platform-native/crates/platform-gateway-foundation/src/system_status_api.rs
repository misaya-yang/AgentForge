//! Opt-in GET /api/v1/config/status from src/api/v1/config.py.
//!
//! Reuses the actual shared RuntimeRateRules, RateConfigDatabase, BalancerConfigStore,
//! CapacityAdmissionController and CapacityResolver. Settings enabled flags are
//! independent from object/pool presence. Construction does no I/O, launches no
//! task, reads no environment and mounts no production route. Hosts provide the
//! trusted resolver, settings, database/pool presence and cancellation policy.
//!
//! The endpoint counts every returned database row, without another filter. The
//! canonical Database.get_rate_limits() defaults enabled=True, so its native
//! SqlRateConfigDatabase::list implementation correctly reads enabled rows; runtime
//! lists and alternate database implementations can contain disabled rows. Any
//! nonempty count selects "runtime", even when settings or individual rules are
//! disabled. Ordinary DB failures fall back to runtime; cancellation propagates.
//!
//! Redis uses the canonical RedisStorage.ping semantics: absent client, command
//! error or non-PONG response is false. The actual ConnectionManager adapter sends
//! PING on the host's existing connection and never opens one itself. Dropping its
//! future abandons the read; no task is spawned here. Redis reconnection/lifetimes
//! remain owned by ConnectionManager and the host.
//!
//! Authorization is legacy roles-only Rbac.require(..., "admin"): no capability,
//! tenant restriction, trimming, separate permission merge or authenticated-flag
//! gate. Bodies/query fields never grant authority and GET bodies are unread.
//! HEAD is 405/Allow GET with source Content-Length and no collaborators. Generic
//! errors reuse accepted trace-state-presence semantics and legacy errors omit
//! trace. A bare trailing slash is 404; trusted host redirect policy is external.
//!
//! Typed native settings and collaborator presence replace arbitrary Python object
//! introspection. The host supplies the shared resolver (default construction is
//! CapacityResolver::default) and keeps it shared with dispatch. Existing shared
//! runtime rules do not model Python list identity on concurrent list replacement;
//! the fallback rereads the native shared list after a failing database await.
use crate::{
    balancer_config_api::{
        BalancerConfigError, BalancerConfigIdentity, BalancerConfigIdentityResolver,
        BalancerConfigStore,
    },
    capacity::CapacityResolver,
    capacity_admission::CapacityAdmissionController,
    config_audit_writer::ConfigAuditRequestContext,
    rate_config_api::{RateConfigDatabase, RateConfigError},
    rate_policy_store::RuntimeRateRules,
};
use ai_platform_auth::rbac::Rbac;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use redis::aio::ConnectionManager;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc};

pub use crate::balancer_config_api::{
    BalancerConfigCancellation as SystemStatusCancellation,
    BalancerConfigCancellationFuture as SystemStatusCancellationFuture,
    BalancerConfigError as SystemStatusError, BalancerConfigIdentity as SystemStatusIdentity,
    BalancerConfigIdentityResolver as SystemStatusIdentityResolver,
};
pub type SystemStatusFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SystemStatusError>> + Send + 'a>>;

/// Trusted settings projection, independent of runtime objects and their flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemStatusSettingsSnapshot {
    pub database_enabled: bool,
    pub redis_enabled: bool,
    pub jwt_enabled: bool,
    pub api_key_enabled: bool,
    pub rate_limits_enabled: bool,
}
pub trait SystemStatusSettings: Send + Sync {
    fn snapshot(&self) -> Result<SystemStatusSettingsSnapshot, SystemStatusError>;
}
impl SystemStatusSettings for SystemStatusSettingsSnapshot {
    fn snapshot(&self) -> Result<Self, SystemStatusError> {
        Ok(*self)
    }
}
/// Presence of this object models app.state.database. pool_present is precisely
/// `_pool is not None`, not a connectivity probe or the settings enabled flag.
/// A missing get_rate_limits method is represented by rate_limits=None. Supply
/// the same RateConfigDatabase used by the configuration route, not PgRateRuleSource.
pub struct SystemStatusDatabase {
    pub pool_present: bool,
    pub rate_limits: Option<Arc<dyn RateConfigDatabase>>,
}
pub trait SystemStatusRedis: Send + Sync {
    fn enabled(&self) -> bool;
    /// This is the RedisStorage.ping boundary. Ordinary failures become false;
    /// explicit cancellation is never folded into connection status.
    fn ping(&self) -> SystemStatusFuture<'_, bool>;
}
/// The host owns connection setup and credential handling. None is the canonical
/// enabled RedisStorage whose client has not been initialized (or was closed).
pub struct NativeSystemStatusRedis {
    enabled: bool,
    connection: Option<ConnectionManager>,
}
impl NativeSystemStatusRedis {
    pub fn new(enabled: bool, connection: Option<ConnectionManager>) -> Self {
        Self {
            enabled,
            connection,
        }
    }
}
impl SystemStatusRedis for NativeSystemStatusRedis {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn ping(&self) -> SystemStatusFuture<'_, bool> {
        Box::pin(async move {
            let Some(connection) = &self.connection else {
                return Ok(false);
            };
            let response: Result<String, _> = redis::cmd("PING")
                .query_async(&mut connection.clone())
                .await;
            Ok(response.is_ok_and(|value| value == "PONG"))
        })
    }
}

pub struct SystemStatusApi {
    pub identity: Arc<dyn BalancerConfigIdentityResolver>,
    pub rbac: Arc<Rbac>,
    pub settings: Arc<dyn SystemStatusSettings>,
    pub database: Option<SystemStatusDatabase>,
    pub redis: Option<Arc<dyn SystemStatusRedis>>,
    pub runtime_rules: Arc<RuntimeRateRules>,
    pub balancer: Arc<BalancerConfigStore>,
    pub admission: Option<Arc<CapacityAdmissionController>>,
    pub capacity_resolver: Arc<CapacityResolver>,
}
impl SystemStatusApi {
    pub async fn get(&self, auth: &BalancerConfigIdentity) -> Result<Value, SystemStatusError> {
        self.rbac
            .require(auth.roles(), "admin")
            .map_err(|error| SystemStatusError::legacy(403, error))?;
        // Settings existence/access is a trusted host boundary, after legacy RBAC.
        let settings = self.settings.snapshot()?;
        let mut configured_rules_count = self.runtime_rules.snapshot().len();
        if let Some(database) = self
            .database
            .as_ref()
            .and_then(|database| database.rate_limits.as_ref())
            .filter(|database| database.enabled())
        {
            configured_rules_count = match database.list().await {
                Ok(rows) => rows.len(),
                Err(RateConfigError::Cancelled) => return Err(SystemStatusError::Cancelled),
                Err(_) => self.runtime_rules.snapshot().len(),
            };
        }
        let runtime_enabled = settings.rate_limits_enabled || configured_rules_count > 0;
        let enforcement_source = if configured_rules_count > 0 {
            "runtime"
        } else if settings.rate_limits_enabled {
            "settings"
        } else {
            "disabled"
        };
        // Capture before the Redis await, exactly as the source endpoint does.
        let snapshot = self
            .admission
            .as_ref()
            .map(|admission| {
                serde_json::to_value(admission.snapshot()).map_err(|_| SystemStatusError::Internal)
            })
            .transpose()?;
        let database_connected = self
            .database
            .as_ref()
            .is_some_and(|database| database.pool_present);
        let redis_connected = match self.redis.as_ref().filter(|redis| redis.enabled()) {
            Some(redis) => match redis.ping().await {
                Ok(connected) => connected,
                Err(SystemStatusError::Cancelled) => return Err(SystemStatusError::Cancelled),
                Err(_) => false,
            },
            None => false,
        };
        // Source reads strategy and resolver fields after the Redis await. Share
        // the actual store; do not cache its value at router construction.
        let strategy = self.balancer.strategy()?;
        let budgets = self
            .capacity_resolver
            .status_budgets(snapshot.as_ref())
            .map_err(|_| SystemStatusError::Internal)?;
        Ok(json!({
            "database": {"enabled": settings.database_enabled, "connected": database_connected},
            "redis": {"enabled": settings.redis_enabled, "connected": redis_connected},
            "authentication": {"jwt_enabled": settings.jwt_enabled, "api_key_enabled": settings.api_key_enabled},
            "rate_limiting": {"enabled": settings.rate_limits_enabled, "configured_rules_count": configured_rules_count,
                "runtime_enabled": runtime_enabled, "enforcement_source": enforcement_source},
            "load_balancer": {"strategy": strategy},
            "capacity": {
                "gateway_instance_id": self.admission.as_ref().map_or("gateway-1", |admission| admission.gateway_instance_id()),
                "cluster_epoch": self.capacity_resolver.cluster_epoch,
                "mode": self.capacity_resolver.mode,
                "budgets": budgets,
                "control_plane": {"health_polling_class": "control_plane_health", "service_budget_exempt": true, "process_budget_enforced": true}
            }
        }))
    }
}

pub const SYSTEM_STATUS_PATH: &str = "/api/v1/config/status";
#[derive(Clone)]
struct HttpState {
    api: Arc<SystemStatusApi>,
    cancellation: Arc<dyn SystemStatusCancellation>,
}
pub fn router(
    api: Arc<SystemStatusApi>,
    cancellation: Arc<dyn SystemStatusCancellation>,
) -> Router {
    Router::new()
        .route(SYSTEM_STATUS_PATH, any(http))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn http(State(state): State<HttpState>, request: Request) -> Response {
    if *request.method() != Method::GET {
        // Let Axum compute Content-Length before suppressing the HEAD body.
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let (parts, _) = request.into_parts();
    let mut context = parts
        .extensions
        .get::<ConfigAuditRequestContext>()
        .cloned()
        .unwrap_or_default();
    context.headers = parts.headers.clone();
    let outcome = async {
        let auth = state
            .api
            .identity
            .resolve(&parts.headers, parts.uri.path())
            .await?;
        state.api.get(&auth).await
    }
    .await;
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(BalancerConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => (
            StatusCode::from_u16(error.status().expect("noncancelled error"))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(error.response_body(&context).expect("noncancelled error")),
        )
            .into_response(),
    }
}
