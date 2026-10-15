//! Unmounted GET/PUT auth configuration from `src/api/v1/config.py`.
//!
//! Legacy roles-only `admin` authorization is intentional. PUT replaces runtime
//! defaults, retains a secret for null/omitted/empty input, and retains raw keys
//! only for null/omitted input. It never changes authentication settings or
//! reloads validators. GET combines independent settings and runtime projections.
//! Runtime mutation happens before Redis and audit awaits; failure/cancellation
//! cannot roll it back. After a Redis await, audit observes the latest runtime
//! state, including a concurrent PUT/key append, as the source does.
//!
//! Shared runtime state stores the source's separate key metadata and raw auth
//! keys under one short synchronous lock. Snapshots are immutable copies, never
//! validator authority. Secrets/raw keys have no Debug/Serialize projection.
//! Constructors are inert; the optional Redis adapter accepts an existing native
//! connection and only writes the non-secret summary using SETEX config:auth 300.
//! Disconnected wrapper writes are no-ops; errors never switch to that profile.
//!
//! HTTP factory is opt-in and requires trusted identity and cancellation policy.
//! JSON syntax precedes identity, then model validation, then authorization.
//! Missing/empty Content-Type retains raw bytes. Invalid JSON diagnostic text,
//! invalid UTF-8, transport limits, decoded static-prefix routing, and trusted-origin
//! slash redirects remain host
//! boundaries. JSON numbers are finite/i64-bounded. Merge custom-fallback routers
//! only after reset_fallback(), as with the other unmounted factories.
use crate::{
    config_audit_writer::{
        ConfigAuditError, ConfigAuditRequestContext, ConfigAuditValue, ConfigAuditWriter,
        ConfigChange,
    },
    rate_config_api::boolean,
    service_config_api::{
        ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture, ServiceConfigIdentity,
        ServiceConfigIdentityResolver,
    },
};
use ai_platform_auth::rbac::Rbac;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use redis::aio::ConnectionManager;
use serde_json::{Value, json};
use std::{
    fmt,
    sync::{Arc, Mutex},
};

pub const AUTH_CONFIG_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/config/auth", "get_auth_config"),
    ("PUT", "/config/auth", "update_auth_config"),
];
pub type AuthConfigError = ServiceConfigError;
pub type AuthConfigFuture<'a, T> = ServiceConfigFuture<'a, T>;

/// Parsed source request. Private secret fields cannot escape via diagnostics.
#[derive(Clone)]
pub struct AuthConfigUpdate {
    jwt_enabled: bool,
    jwt_secret: Option<String>,
    jwt_algorithms: Vec<String>,
    jwt_issuer: Option<String>,
    api_key_enabled: bool,
    api_key_header: String,
    api_keys: Option<Vec<String>>,
}
impl Default for AuthConfigUpdate {
    fn default() -> Self {
        Self {
            jwt_enabled: false,
            jwt_secret: None,
            jwt_algorithms: vec!["HS256".into()],
            jwt_issuer: None,
            api_key_enabled: false,
            api_key_header: "X-API-Key".into(),
            api_keys: None,
        }
    }
}
impl fmt::Debug for AuthConfigUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthConfigUpdate([redacted])")
    }
}
#[derive(Clone)]
pub struct RuntimeAuthSnapshot {
    jwt_enabled: bool,
    jwt_secret: String,
    jwt_algorithms: Vec<String>,
    jwt_issuer: Option<String>,
    api_key_enabled: bool,
    api_key_header: String,
    api_keys: Vec<String>,
}
impl Default for RuntimeAuthSnapshot {
    fn default() -> Self {
        Self {
            jwt_enabled: false,
            jwt_secret: String::new(),
            jwt_algorithms: vec!["HS256".into()],
            jwt_issuer: None,
            api_key_enabled: false,
            api_key_header: "X-API-Key".into(),
            api_keys: Vec::new(),
        }
    }
}
impl RuntimeAuthSnapshot {
    pub fn summary(&self) -> Value {
        json!({"jwt_enabled":self.jwt_enabled,
            "jwt_secret_configured":!self.jwt_secret.is_empty(),
            "jwt_algorithms":self.jwt_algorithms,"jwt_issuer":self.jwt_issuer,
            "api_key_enabled":self.api_key_enabled,
            "api_key_header":if self.api_key_header.is_empty(){"X-API-Key"}else{&self.api_key_header},
            "api_keys_count":self.api_keys.len()})
    }
    pub fn key_count(&self) -> usize {
        self.api_keys.len()
    }
}
impl fmt::Debug for RuntimeAuthSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeAuthSnapshot")
            .field("summary", &self.summary())
            .finish()
    }
}
#[derive(Default)]
struct RuntimeAuthState {
    auth: RuntimeAuthSnapshot,
    key_metadata: Vec<Value>,
}
#[derive(Default)]
pub struct RuntimeAuthStore {
    state: Mutex<RuntimeAuthState>,
}
impl fmt::Debug for RuntimeAuthStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeAuthStore")
            .field("summary", &self.snapshot().summary())
            .finish()
    }
}
impl RuntimeAuthStore {
    pub fn snapshot(&self) -> RuntimeAuthSnapshot {
        self.state.lock().unwrap().auth.clone()
    }
    /// Synchronous source assignment. Separate key metadata is not cleared.
    pub fn replace(&self, update: AuthConfigUpdate) -> RuntimeAuthSnapshot {
        let mut state = self.state.lock().unwrap();
        let jwt_secret = update
            .jwt_secret
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| state.auth.jwt_secret.clone());
        let api_keys = update
            .api_keys
            .unwrap_or_else(|| state.auth.api_keys.clone());
        state.auth = RuntimeAuthSnapshot {
            jwt_enabled: update.jwt_enabled,
            jwt_secret,
            jwt_algorithms: update.jwt_algorithms,
            jwt_issuer: update.jwt_issuer,
            api_key_enabled: update.api_key_enabled,
            api_key_header: update.api_key_header,
            api_keys,
        };
        state.auth.clone()
    }
    /// Source fallback create appends separate metadata and the raw auth key.
    /// Callers must pass only the source metadata fields (including key_hash).
    pub fn append_api_key(&self, raw_key: String, metadata: Value) {
        let mut state = self.state.lock().unwrap();
        state.key_metadata.push(metadata);
        state.auth.api_keys.push(raw_key);
    }
    pub fn key_count(&self) -> usize {
        self.state.lock().unwrap().auth.api_keys.len()
    }
    pub fn key_metadata(&self) -> Vec<Value> {
        self.state.lock().unwrap().key_metadata.clone()
    }
}

/// Secret-free settings read seam; PUT never writes through this interface.
#[derive(Clone, Debug)]
pub struct AuthConfigSettings {
    pub jwt_enabled: bool,
    pub jwt_algorithms: Vec<String>,
    pub jwt_issuer: Option<String>,
    pub jwt_audience: Option<String>,
    pub api_key_enabled: bool,
    pub api_key_header: String,
    pub api_key_count: usize,
}
impl Default for AuthConfigSettings {
    fn default() -> Self {
        Self {
            jwt_enabled: false,
            jwt_algorithms: vec!["HS256".into()],
            jwt_issuer: None,
            jwt_audience: None,
            api_key_enabled: false,
            api_key_header: "X-API-Key".into(),
            api_key_count: 0,
        }
    }
}
pub trait AuthConfigSettingsSource: Send + Sync {
    fn snapshot(&self) -> AuthConfigSettings;
}
impl AuthConfigSettingsSource for AuthConfigSettings {
    fn snapshot(&self) -> AuthConfigSettings {
        self.clone()
    }
}
pub trait AuthConfigCache: Send + Sync {
    fn enabled(&self) -> bool;
    fn cache_auth<'a>(&'a self, summary: &'a Value) -> AuthConfigFuture<'a, ()>;
}
/// No URL, credentials, startup, listener, task, or automatic fallback.
#[derive(Clone)]
pub struct RedisAuthConfigCache {
    enabled: bool,
    connection: Option<ConnectionManager>,
}
impl RedisAuthConfigCache {
    pub fn connected(connection: ConnectionManager) -> Self {
        Self {
            enabled: true,
            connection: Some(connection),
        }
    }
    /// enabled=true models an enabled RedisStorage before/after client lifetime.
    pub fn disconnected(enabled: bool) -> Self {
        Self {
            enabled,
            connection: None,
        }
    }
}
impl AuthConfigCache for RedisAuthConfigCache {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn cache_auth<'a>(&'a self, summary: &'a Value) -> AuthConfigFuture<'a, ()> {
        Box::pin(async move {
            let Some(mut connection) = self.connection.clone() else {
                return Ok(());
            };
            // Same ensure_ascii=False JSON separators/order as RedisStorage.save.
            let encoded = python_json(summary);
            redis::cmd("SETEX")
                .arg("config:auth")
                .arg(300)
                .arg(encoded)
                .query_async::<()>(&mut connection)
                .await
                .map_err(|_| AuthConfigError::Internal)
        })
    }
}
fn python_json(value: &Value) -> String {
    match value {
        Value::Object(v) => format!(
            "{{{}}}",
            v.iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    serde_json::to_string(key).expect("JSON string"),
                    python_json(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Array(v) => format!(
            "[{}]",
            v.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        _ => serde_json::to_string(value).expect("JSON value"),
    }
}
pub struct AuthConfigDependencies {
    pub identity: Arc<dyn ServiceConfigIdentityResolver>,
    pub rbac: Rbac,
    pub settings: Arc<dyn AuthConfigSettingsSource>,
    pub runtime: Arc<RuntimeAuthStore>,
    pub cache: Option<Arc<dyn AuthConfigCache>>,
    pub audit: ConfigAuditWriter,
}
pub struct AuthConfigApi {
    dependencies: AuthConfigDependencies,
}
#[derive(Clone, Copy, Debug)]
pub enum AuthConfigOperation {
    Get,
    Update,
}
impl AuthConfigApi {
    pub fn new(dependencies: AuthConfigDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: AuthConfigOperation,
        headers: &HeaderMap,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, AuthConfigError> {
        let identity = self
            .dependencies
            .identity
            .resolve(headers, "/api/v1/config/auth")
            .await?;
        self.execute(operation, &identity, body, context).await
    }
    pub async fn execute(
        &self,
        operation: AuthConfigOperation,
        identity: &ServiceConfigIdentity,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, AuthConfigError> {
        let update = match operation {
            AuthConfigOperation::Update => Some(validate_auth_config_update(body)?),
            AuthConfigOperation::Get => None,
        };
        let d = &self.dependencies;
        d.rbac
            .require(&identity.roles, "admin")
            .map_err(|error| AuthConfigError::Http {
                status: 403,
                body: json!({"error":error}),
            })?;
        if let Some(update) = update {
            let summary = d.runtime.replace(update).summary();
            if let Some(cache) = d.cache.as_ref().filter(|cache| cache.enabled()) {
                cache.cache_auth(&summary).await?;
            }
            let after = ConfigAuditValue::from(d.runtime.snapshot().summary());
            d.audit
                .record_config_change(
                    context,
                    identity,
                    ConfigChange {
                        resource_type: "auth",
                        resource_id: "gateway-auth",
                        action: "update",
                        before: None,
                        after: Some(&after),
                    },
                )
                .await
                .map_err(|error| {
                    if error == ConfigAuditError::Cancelled {
                        AuthConfigError::Cancelled
                    } else {
                        AuthConfigError::Internal
                    }
                })?;
            Ok(json!({"status":"success","message":"鉴权配置已更新"}))
        } else {
            let settings = d.settings.snapshot();
            Ok(
                json!({"jwt":{"enabled":settings.jwt_enabled,"algorithms":settings.jwt_algorithms,
                "issuer":settings.jwt_issuer,"audience":settings.jwt_audience},
                "api_key":{"enabled":settings.api_key_enabled,"header_name":settings.api_key_header,
                    "key_count":settings.api_key_count},"runtime":d.runtime.snapshot().summary()}),
            )
        }
    }
}
fn validation(kind: &str, loc: Vec<Value>, input: &Value) -> Value {
    let message = match kind {
        "model_type" => "Input should be a valid dictionary or instance of AuthConfigUpdate",
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from"
        }
        "missing" => "Field required",
        "string_type" => "Input should be a valid string",
        "list_type" => "Input should be a valid list",
        "bool_type" => "Input should be a valid boolean",
        "bool_parsing" => "Input should be a valid boolean, unable to interpret input",
        _ => unreachable!("known auth model validation"),
    };
    let mut value = json!({"type":kind,"loc":loc,"msg":message,"input":input});
    if kind == "model_type" {
        value["ctx"] = json!({"class_name":"AuthConfigUpdate"});
    }
    value
}
pub fn validate_auth_config_update(body: &Value) -> Result<AuthConfigUpdate, AuthConfigError> {
    let Some(object) = body.as_object() else {
        return Err(AuthConfigError::validation(vec![validation(
            "model_type",
            vec![json!("body")],
            body,
        )]));
    };
    let mut update = AuthConfigUpdate::default();
    let mut errors = Vec::new();
    for field in [
        "jwt_enabled",
        "jwt_secret",
        "jwt_algorithms",
        "jwt_issuer",
        "api_key_enabled",
        "api_key_header",
        "api_keys",
    ] {
        let Some(value) = object.get(field) else {
            continue;
        };
        let loc = vec![json!("body"), json!(field)];
        let parsed = match field {
            "jwt_enabled" | "api_key_enabled" => boolean(value).map(|v| {
                if field == "jwt_enabled" {
                    update.jwt_enabled = v;
                } else {
                    update.api_key_enabled = v;
                }
            }),
            "jwt_secret" | "jwt_issuer" if value.is_null() => Ok(()),
            "jwt_secret" | "jwt_issuer" | "api_key_header" => {
                value.as_str().ok_or("string_type").map(|v| match field {
                    "jwt_secret" => update.jwt_secret = Some(v.into()),
                    "jwt_issuer" => update.jwt_issuer = Some(v.into()),
                    _ => update.api_key_header = v.into(),
                })
            }
            "api_keys" if value.is_null() => Ok(()),
            "jwt_algorithms" | "api_keys" => match value.as_array() {
                None => Err("list_type"),
                Some(values) => {
                    let mut out = Vec::new();
                    for (index, item) in values.iter().enumerate() {
                        if let Some(item) = item.as_str() {
                            out.push(item.into());
                        } else {
                            let mut at = loc.clone();
                            at.push(json!(index));
                            errors.push(validation("string_type", at, item));
                        }
                    }
                    if field == "jwt_algorithms" {
                        update.jwt_algorithms = out;
                    } else {
                        update.api_keys = Some(out);
                    }
                    Ok(())
                }
            },
            _ => unreachable!("known field"),
        };
        if let Err(kind) = parsed {
            errors.push(validation(kind, loc, value));
        }
    }
    if errors.is_empty() {
        Ok(update)
    } else {
        Err(AuthConfigError::validation(errors))
    }
}

#[derive(Clone)]
struct HttpState {
    api: Arc<AuthConfigApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(api: Arc<AuthConfigApi>, cancellation: Arc<dyn ServiceConfigCancellation>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/auth",
            any(method_fallback).get(http_get).put(http_put),
        )
        .route_layer(middleware::from_fn(route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn method_fallback() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
async fn route_guard(request: Request, next: Next) -> Response {
    if ["GET", "PUT"].contains(&request.method().as_str()) {
        next.run(request).await
    } else {
        // Let Axum suppress HEAD after it computes the original JSON length.
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
fn http_context(
    state: &HttpState,
    headers: &HeaderMap,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
) -> ConfigAuditRequestContext {
    let mut context = trusted
        .map(|Extension(v)| v)
        .unwrap_or_else(|| state.api.dependencies.identity.context(headers));
    context.headers = headers.clone();
    context
}
async fn result(
    state: &HttpState,
    value: Result<Value, AuthConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(AuthConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let (status, body) = error.response(context).expect("noncancelled");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
async fn http_get(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    result(
        &state,
        state
            .api
            .handle(AuthConfigOperation::Get, &headers, &Value::Null, &context)
            .await,
        &context,
    )
    .await
}
async fn http_put(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        let is_json = crate::http_content_type::is_json_content_type(&headers);
        if is_json {
            match serde_json::from_slice(&bytes) { Ok(value)=>value, Err(_)=>return result(&state,
            Err(AuthConfigError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await }
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    };
    let identity = match state
        .api
        .dependencies
        .identity
        .resolve(&headers, "/api/v1/config/auth")
        .await
    {
        Ok(identity) => identity,
        Err(error) => return result(&state, Err(error), &context).await,
    };
    if !body.is_object() {
        let kind = if body.is_null() {
            "missing"
        } else {
            "model_attributes_type"
        };
        return result(
            &state,
            Err(AuthConfigError::validation(vec![validation(
                kind,
                vec![json!("body")],
                &body,
            )])),
            &context,
        )
        .await;
    }
    result(
        &state,
        state
            .api
            .execute(AuthConfigOperation::Update, &identity, &body, &context)
            .await,
        &context,
    )
    .await
}

