//! Opt-in GET/PUT port of `src/api/v1/config.py`'s load-balancer configuration.
//!
//! Reuses the accepted Rbac, LoadBalancer and ConfigAuditWriter. The host injects
//! current verified identity, configured RBAC/settings, shared runtime dictionary,
//! optional existing balancer and audit writer. Construction does no I/O, starts
//! no task and mounts no production route. The module-global Python dictionary is
//! represented by an explicitly shared store; hosts with multiple router instances
//! must share it. It intentionally starts at round_robin independently of settings.
//!
//! Legacy `rbac.require(auth.roles, "admin")` is exact: no capability conversion,
//! trimming, case folding, separate-permissions merging or authentication flag gate.
//! PUT accepts every JSON string, including unknown, empty and whitespace strings.
//! Mutation order is dictionary, existing balancer strategy, awaited audit. Settings
//! never change and audit failure/cancellation never rolls the mutations back. The
//! same balancer, runtime seam and round-robin counters survive every change.
//!
//! A Tokio mutex permits the existing balancer to remain shared with dispatch. PUT
//! acquires its guard BEFORE mutating either state, then performs both mutations
//! without an await and releases the guard before audit. Dropping while waiting
//! changes nothing; dropping during audit retains both changes. Host-owned adapters
//! retain responsibility for cancellation cleanup/commit ambiguity.
//!
//! HTTP order is method, JSON syntax, identity, semantic body validation, RBAC,
//! settings/read or mutation/audit. Missing Content-Type policy is explicitly
//! injected because the installed oracle framework differs from the lockfile. HEAD is 405 with Allow GET and no collaborators.
//! Generic errors follow canonical trace-state presence, then X-Trace-ID/X-Request-ID;
//! legacy permission/auth errors have no trace field. Trusted request extensions
//! carry audit state and peer data; headers never establish authority. The injected
//! audit writer, not this module, owns forwarded-address trust.
//!
//! Boundaries: finite native JSON, UTF-8 body data, typed settings/identity and a
//! real LoadBalancer (not arbitrary Python objects). Malformed JSON diagnostic text
//! and offsets, non-UTF-8 body diagnostics and transport body limits are native.
//! Bare router trailing slashes return 404; canonical 307 redirects require the
//! host's trusted URL policy. No origin is inferred from Host/forwarded headers.
//! The AdmissionService bridge covers its JWT/database-key modes, not all source
//! disabled-auth/guest modes or every cached AuthContext role projection. Exact
//! source authentication projections need the host resolver. Explicit typed
//! cancellation requires a host transport callback, never an invented HTTP status.
use crate::{
    config_audit_writer::{
        ConfigAuditError, ConfigAuditIdentity, ConfigAuditRequestContext, ConfigAuditValue,
        ConfigAuditWriter, ConfigChange,
    },
    registry_load_balancer::LoadBalancer,
    service_registry::python_truthy,
};
use ai_platform_auth::{admission::AdmissionService, rbac::Rbac};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

#[derive(Clone, PartialEq)]
pub enum BalancerConfigError {
    Http { status: u16, body: Value },
    Internal,
    Cancelled,
}
impl fmt::Debug for BalancerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "BalancerConfigError::Http({status})"),
            Self::Internal => f.write_str("BalancerConfigError::Internal"),
            Self::Cancelled => f.write_str("BalancerConfigError::Cancelled"),
        }
    }
}
impl BalancerConfigError {
    pub fn legacy(status: u16, message: impl Into<String>) -> Self {
        Self::Http {
            status,
            body: json!({"error": message.into()}),
        }
    }
    pub fn validation(detail: Value) -> Self {
        Self::Http {
            status: 422,
            body: json!({"detail": detail}),
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Internal => Some(500),
            Self::Cancelled => None,
        }
    }
    pub fn response_body(&self, context: &ConfigAuditRequestContext) -> Option<Value> {
        Some(match self {
            Self::Http { body, .. } => body.clone(),
            Self::Cancelled => return None,
            Self::Internal => {
                let mut body = json!({"error":{"code":2000,"message":"Internal server error"}});
                // Present null/false/empty state suppresses header fallback, just
                // like hasattr(request.state, "trace_id") in the source handler.
                let trace = context.trace_id.clone().or_else(|| {
                    ["x-trace-id", "x-request-id"].iter().find_map(|key| {
                        context
                            .headers
                            .get(*key)
                            .filter(|v| !v.is_empty())
                            .map(|v| {
                                Value::String(
                                    v.as_bytes().iter().copied().map(char::from).collect(),
                                )
                            })
                    })
                });
                if let Some(trace) = trace.filter(python_truthy) {
                    body["error"]["trace_id"] = trace;
                }
                body
            }
        })
    }
}
impl From<ConfigAuditError> for BalancerConfigError {
    fn from(error: ConfigAuditError) -> Self {
        if error == ConfigAuditError::Cancelled {
            Self::Cancelled
        } else {
            Self::Internal
        }
    }
}
type ApiResult<T> = Result<T, BalancerConfigError>;

/// Trusted output only; deliberately not Deserialize. The source endpoint reads
/// only roles. Permissions must already have been merged by the trusted resolver
/// if its canonical authentication path does so; this layer never merges them.
#[derive(Clone)]
pub struct BalancerConfigIdentity {
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    permissions: Vec<String>,
    is_authenticated: bool,
}
impl BalancerConfigIdentity {
    pub fn from_resolved_identity(
        user_id: String,
        tenant_id: String,
        roles: Vec<String>,
        permissions: Vec<String>,
        is_authenticated: bool,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            roles,
            permissions,
            is_authenticated,
        }
    }
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
    pub fn permissions(&self) -> &[String] {
        &self.permissions
    }
    pub fn is_authenticated(&self) -> bool {
        self.is_authenticated
    }
}
impl ConfigAuditIdentity for BalancerConfigIdentity {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
pub type BalancerConfigAuthFuture<'a> =
    Pin<Box<dyn Future<Output = ApiResult<BalancerConfigIdentity>> + Send + 'a>>;
pub trait BalancerConfigIdentityResolver: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap, path: &'a str)
    -> BalancerConfigAuthFuture<'a>;
}
impl BalancerConfigIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> BalancerConfigAuthFuture<'a> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let identity = request
                .resolve()
                .await
                .map_err(|error| BalancerConfigError::legacy(error.status(), error.to_string()))?;
            Ok(BalancerConfigIdentity::from_resolved_identity(
                identity.user_id().into(),
                identity.tenant_id().into(),
                identity.roles().to_vec(),
                identity.permissions().to_vec(),
                true,
            ))
        })
    }
}

/// Read-only host settings view; GET reads it only after RBAC. PUT never reads or
/// writes it. A fixed String is useful when host settings are immutable.
pub trait BalancerConfigSettings: Send + Sync {
    fn strategy(&self) -> ApiResult<String>;
}
impl BalancerConfigSettings for String {
    fn strategy(&self) -> ApiResult<String> {
        Ok(self.clone())
    }
}
#[derive(Default)]
struct RuntimeConfig {
    strategy: String,
}
/// Explicit replacement for the Python module-level dictionary; share this Arc
/// across all router instances in the same canonical process scope.
pub struct BalancerConfigStore {
    runtime: Mutex<RuntimeConfig>,
}
impl Default for BalancerConfigStore {
    fn default() -> Self {
        Self::new("round_robin")
    }
}
impl BalancerConfigStore {
    pub fn new(strategy: impl Into<String>) -> Self {
        Self {
            runtime: Mutex::new(RuntimeConfig {
                strategy: strategy.into(),
            }),
        }
    }
    pub fn strategy(&self) -> ApiResult<String> {
        self.runtime
            .lock()
            .map(|value| value.strategy.clone())
            .map_err(|_| BalancerConfigError::Internal)
    }
    fn replace(&self, strategy: &str) -> ApiResult<()> {
        self.runtime
            .lock()
            .map_err(|_| BalancerConfigError::Internal)?
            .strategy = strategy.to_owned();
        Ok(())
    }
}
/// Share exactly this balancer with dispatch; do not construct a replacement on
/// PUT, which would reset the accepted RoundRobinStrategy's per-service counters.
pub type SharedConfigLoadBalancer = Arc<tokio::sync::Mutex<LoadBalancer>>;

pub struct BalancerConfigApi {
    pub identity: Arc<dyn BalancerConfigIdentityResolver>,
    pub rbac: Arc<Rbac>,
    pub settings: Arc<dyn BalancerConfigSettings>,
    pub runtime: Arc<BalancerConfigStore>,
    pub load_balancer: Option<SharedConfigLoadBalancer>,
    pub audit: Arc<ConfigAuditWriter>,
}
impl BalancerConfigApi {
    fn require(&self, auth: &BalancerConfigIdentity) -> ApiResult<()> {
        self.rbac
            .require(auth.roles(), "admin")
            .map_err(|message| BalancerConfigError::legacy(403, message))
    }
    pub fn get(&self, auth: &BalancerConfigIdentity) -> ApiResult<Value> {
        self.require(auth)?;
        let strategy = self.settings.strategy()?;
        Ok(json!({
            "strategy": strategy,
            "runtime": {"strategy": self.runtime.strategy()?},
            "available_strategies": [
                {"value":"round_robin","label":"轮询","description":"按顺序依次选择实例"},
                {"value":"random","label":"随机","description":"随机选择实例"},
                {"value":"weighted","label":"加权","description":"根据权重概率选择实例"},
                {"value":"consistent_hash","label":"一致性哈希","description":"相同会话路由到相同实例"}
            ]
        }))
    }
    /// `body` has already passed canonical JSON/schema validation.
    pub async fn update(
        &self,
        auth: &BalancerConfigIdentity,
        context: &ConfigAuditRequestContext,
        body: &LoadBalancerConfigUpdate,
    ) -> ApiResult<Value> {
        self.require(auth)?;
        if let Some(load_balancer) = &self.load_balancer {
            let mut load_balancer = load_balancer.lock().await;
            self.runtime.replace(&body.strategy)?;
            load_balancer.strategy_name.clone_from(&body.strategy);
        } else {
            self.runtime.replace(&body.strategy)?;
        }
        let after = ConfigAuditValue::from(json!({"strategy": body.strategy}));
        self.audit
            .record_config_change(
                context,
                auth,
                ConfigChange {
                    resource_type: "load_balancer",
                    resource_id: "gateway",
                    action: "update",
                    before: None,
                    after: Some(&after),
                },
            )
            .await?;
        Ok(json!({"status":"success","message":"负载均衡配置已更新"}))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadBalancerConfigUpdate {
    pub strategy: String,
}
impl Default for LoadBalancerConfigUpdate {
    fn default() -> Self {
        Self {
            strategy: "round_robin".into(),
        }
    }
}
/// FastAPI JSON boundary: null/missing body is a required-body error; other
/// non-objects fail model_attributes_type. Extra fields are ignored, not merged.
pub fn validate_update(body: &Value) -> ApiResult<LoadBalancerConfigUpdate> {
    if body.is_null() {
        return Err(BalancerConfigError::validation(json!([
            {"type":"missing","loc":["body"],"msg":"Field required","input":null}
        ])));
    }
    let Some(object) = body.as_object() else {
        return Err(BalancerConfigError::validation(json!([
            {"type":"model_attributes_type","loc":["body"],
             "msg":"Input should be a valid dictionary or object to extract fields from","input":body}
        ])));
    };
    let Some(strategy) = object.get("strategy") else {
        return Ok(LoadBalancerConfigUpdate::default());
    };
    let Some(strategy) = strategy.as_str() else {
        return Err(BalancerConfigError::validation(json!([
            {"type":"string_type","loc":["body","strategy"],"msg":"Input should be a valid string","input":strategy}
        ])));
    };
    Ok(LoadBalancerConfigUpdate {
        strategy: strategy.to_owned(),
    })
}

pub type BalancerConfigCancellationFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;
pub trait BalancerConfigCancellation: Send + Sync {
    fn cancelled(&self) -> BalancerConfigCancellationFuture<'_>;
}
#[derive(Clone)]
struct HttpState {
    content_type: BalancerConfigContentTypePolicy,
    api: Arc<BalancerConfigApi>,
    cancellation: Arc<dyn BalancerConfigCancellation>,
}
/// Explicit transport-version choice. FastAPI 0.141.1 defaults to requiring the
/// JSON header; the repository locks 0.137.1, unavailable in the offline reference
/// runtime. Tagged 0.137.1 routing.py statically confirms the same strict default.
/// The host selects its verified canonical behavior; no silent default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalancerConfigContentTypePolicy {
    RequireJsonHeader,
    AssumeJsonWhenMissing,
}
pub const BALANCER_CONFIG_PATH: &str = "/api/v1/config/load-balancer";
pub fn router(
    api: Arc<BalancerConfigApi>,
    cancellation: Arc<dyn BalancerConfigCancellation>,
    content_type: BalancerConfigContentTypePolicy,
) -> Router {
    Router::new()
        .route(BALANCER_CONFIG_PATH, any(http))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState {
            api,
            cancellation,
            content_type,
        })
}
async fn result(
    state: &HttpState,
    context: &ConfigAuditRequestContext,
    value: ApiResult<Value>,
) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(BalancerConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => (
            StatusCode::from_u16(error.status().expect("noncancelled error"))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(error.response_body(context).expect("noncancelled error")),
        )
            .into_response(),
    }
}
fn decode_body(
    headers: &HeaderMap,
    bytes: Bytes,
    policy: BalancerConfigContentTypePolicy,
) -> ApiResult<Value> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let json_content = headers
        .get("content-type")
        .filter(|value| !value.is_empty())
        .map(crate::http_content_type::is_json_media_type)
        .unwrap_or(policy == BalancerConfigContentTypePolicy::AssumeJsonWhenMissing);
    if !json_content {
        return Ok(json!(String::from_utf8_lossy(&bytes)));
    }
    serde_json::from_slice(&bytes).map_err(|_| BalancerConfigError::validation(json!([
        {"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}}
    ])))
}
async fn http(State(state): State<HttpState>, request: Request) -> Response {
    if !matches!(*request.method(), Method::GET | Method::PUT) {
        // Axum sets Content-Length before suppressing the HEAD representation.
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let put = request.method() == Method::PUT;
    let headers = request.headers().clone();
    let path = request.uri().path().to_owned();
    let mut context = request
        .extensions()
        .get::<ConfigAuditRequestContext>()
        .cloned()
        .unwrap_or_default();
    context.headers = headers.clone();
    let body = if put {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(bytes) => bytes,
            Err(error) => return error.into_response(),
        };
        match decode_body(&headers, bytes, state.content_type) {
            Ok(body) => body,
            Err(error) => return result(&state, &context, Err(error)).await,
        }
    } else {
        Value::Null
    };
    let auth = match state.api.identity.resolve(&headers, &path).await {
        Ok(auth) => auth,
        Err(error) => return result(&state, &context, Err(error)).await,
    };
    let response = if put {
        match validate_update(&body) {
            Ok(body) => state.api.update(&auth, &context, &body).await,
            Err(error) => Err(error),
        }
    } else {
        state.api.get(&auth)
    };
    result(&state, &context, response).await
}
