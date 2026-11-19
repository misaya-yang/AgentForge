//! Opt-in four-route port of `src/api/v1/health.py`.
//!
//! Construction is inert: no listener, scheduler, provider call, credentials or
//! environment lookup. The host must inject its real trusted identity resolver,
//! Gateway dependency probe/state and HTTP cancellation policy. Identity claims
//! in query/body/unsigned headers confer no authority. The exact, case-sensitive
//! admin/ops/operator/devops gate also requires an authenticated subject.
//!
//! Reuses HealthMonitor, GatewayModelMeta, configured_providers, and the very
//! same ProviderAdminProbeState used by provider administration. Health reads do
//! not start probes, refresh receipts, invalidate them or query another tenant.
//! Missing model metadata and ordinary collection errors become the canonical
//! opaque 503; probe/snapshot failures remain internal errors. Cancellation and
//! dropped futures never become provider-collection failures or start later work.
//!
//! Native boundaries: finite serde JSON with string object keys, typed trusted
//! identity and HealthStatus records. The existing monitor HashMap does not
//! promise Python insertion order. Runtime/worker presence and exact task.done()
//! observations are supplied by the host; this module does not invent readiness.
//! Registry and monitor wiring are mandatory, so absent Python app attributes
//! are host construction errors. It deliberately supplies no default resolver or
//! dependency snapshot. Guest/disabled-auth behavior belongs to the resolver.
//! All GET bodies and query parameters are ignored, including malformed JSON.
//! Encoded characters in the static health prefix require host normalization;
//! the matched suffix is decoded before route selection. Invalid UTF-8 path
//! diagnostics are native. Absolute slash redirects require a trusted origin;
//! otherwise the bare router returns 404. No forwarding header supplies origin.
use crate::{
    gateway_model_meta::{
        GatewayModelMeta, MetadataModels, MetadataProviders, configured_providers,
    },
    provider_admin_api::ProviderAdminProbeState,
    provider_health::{ProviderProbeError, ProviderProbeStatus},
    registry_health_monitor::{HealthMonitor, HealthStatus},
    service_registry::ServiceRegistry,
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, fmt, future::Future, pin::Pin, sync::Arc};

pub type HealthApiFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HealthApiError>> + Send + 'a>>;
#[derive(Clone, PartialEq)]
pub enum HealthApiError {
    Http { status: u16, detail: Value },
    Internal,
    Cancelled,
}
impl fmt::Debug for HealthApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "HealthApiError::Http({status})"),
            Self::Internal => f.write_str("HealthApiError::Internal"),
            Self::Cancelled => f.write_str("HealthApiError::Cancelled"),
        }
    }
}
impl HealthApiError {
    pub fn http(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Internal => Some(500),
            Self::Cancelled => None,
        }
    }
    pub fn response_body(&self, trace: &str) -> Option<Value> {
        Some(match self {
            Self::Http { detail, .. } => json!({"detail":detail}),
            Self::Cancelled => return None,
            Self::Internal => {
                let mut body = json!({"error":{"code":2000,"message":"Internal server error"}});
                if !trace.is_empty() {
                    body["error"]["trace_id"] = trace.into();
                }
                body
            }
        })
    }
}
/// Only a trusted resolver constructs this value. It is not Deserialize.
#[derive(Clone)]
pub struct HealthIdentity {
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    is_authenticated: bool,
}
impl HealthIdentity {
    pub fn from_resolved_identity(
        user_id: String,
        tenant_id: String,
        roles: Vec<String>,
        is_authenticated: bool,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            roles,
            is_authenticated,
        }
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
    pub fn is_authenticated(&self) -> bool {
        self.is_authenticated
    }
    fn tenant(&self) -> &str {
        if self.tenant_id.is_empty() {
            "default"
        } else {
            &self.tenant_id
        }
    }
}
pub fn require_admin(user: &HealthIdentity) -> Result<(), HealthApiError> {
    if !user.is_authenticated {
        return Err(HealthApiError::http(401, "Authentication required"));
    }
    if !user
        .roles
        .iter()
        .any(|role| matches!(role.as_str(), "admin" | "ops" | "operator" | "devops"))
    {
        return Err(HealthApiError::http(403, "Admin access required"));
    }
    Ok(())
}
pub trait HealthIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> HealthApiFuture<'a, HealthIdentity>;
}
/// Existing JWT/database-key bridge. Hosts needing canonical guest/disabled-auth
/// resolution inject that real resolver instead of treating this bridge as one.
impl HealthIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> HealthApiFuture<'a, HealthIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| HealthApiError::http(e.status(), e.to_string()))?;
            Ok(HealthIdentity::from_resolved_identity(
                user.user_id().into(),
                user.tenant_id().into(),
                user.roles().to_vec(),
                true,
            ))
        })
    }
}
/// The host captures object presence and the exact Python `done() is False`
/// predicate. `worker_running` is false for absent workers/tasks/methods.
#[derive(Clone, Copy, Debug)]
pub struct HealthFallbackState {
    pub runtime_present: bool,
    pub model_plane_present: bool,
    pub worker_running: bool,
}
pub trait HealthDependencyState: Send + Sync {
    /// None means no callable probe. Non-object results fall through to cache.
    /// A failing probe propagates and never falls back.
    fn probe(&self) -> Option<HealthApiFuture<'_, Value>>;
    fn cached_snapshot(&self) -> Option<Value>;
    fn fallback_state(&self) -> Result<HealthFallbackState, HealthApiError>;
}
pub async fn gateway_dependency_snapshot(
    state: &dyn HealthDependencyState,
) -> Result<Map<String, Value>, HealthApiError> {
    if let Some(probe) = state.probe() {
        let value = probe.await?;
        if let Value::Object(snapshot) = value {
            return Ok(snapshot);
        }
    }
    if let Some(Value::Object(snapshot)) = state.cached_snapshot() {
        return Ok(snapshot);
    }
    let state = state.fallback_state()?;
    let core_ready = state.runtime_present && state.model_plane_present;
    Ok(json!({
        "status":if core_ready {"ready"} else {"not_ready"},
        "core_ready":core_ready,"degraded":!state.worker_running,
        "core":{"agent_runtime":if state.runtime_present {"healthy"} else {"unavailable"},"model_plane":if state.model_plane_present {"healthy"} else {"unavailable"}},
        "capabilities":{"knowledge_service":"not_configured","image_worker":if state.worker_running {"healthy"} else {"unavailable"}}
    }).as_object().expect("object").clone())
}
pub fn dependency_service_status(snapshot: &Map<String, Value>, service_id: &str) -> Value {
    let empty = Map::new();
    let core = snapshot
        .get("core")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let capabilities = snapshot
        .get("capabilities")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if service_id == "gateway_core" {
        return json!({"status":if snapshot.get("core_ready") == Some(&Value::Bool(true)) {"healthy"} else {"unavailable"},"dependencies":core});
    }
    let required = core.contains_key(service_id);
    let source = if required { core } else { capabilities };
    json!({"status":source.get(service_id).cloned().unwrap_or_else(||json!("unavailable")),"required":required})
}
pub type HealthRecords = Vec<(String, Arc<HealthStatus>)>;
pub trait HealthStatusSource: Send + Sync {
    fn all_status(&self) -> Result<HealthRecords, HealthApiError>;
    fn get_status(&self, service_id: &str) -> Result<Option<Arc<HealthStatus>>, HealthApiError>;
}
impl HealthStatusSource for HealthMonitor {
    fn all_status(&self) -> Result<HealthRecords, HealthApiError> {
        Ok(HealthMonitor::all_status(self).into_iter().collect())
    }
    fn get_status(&self, service_id: &str) -> Result<Option<Arc<HealthStatus>>, HealthApiError> {
        Ok(HealthMonitor::get_status(self, service_id))
    }
}
pub trait HealthModelMetadata: Send + Sync {
    fn configured<'a>(&'a self, tenant: &'a str) -> HealthApiFuture<'a, Vec<String>>;
    fn model_counts<'a>(&'a self, tenant: &'a str) -> HealthApiFuture<'a, Map<String, Value>>;
}
impl<M: MetadataModels, P: MetadataProviders> HealthModelMetadata for GatewayModelMeta<M, P> {
    fn configured<'a>(&'a self, tenant: &'a str) -> HealthApiFuture<'a, Vec<String>> {
        Box::pin(async move {
            configured_providers(self, tenant)
                .await
                .map_err(|_| HealthApiError::Internal)
        })
    }
    fn model_counts<'a>(&'a self, tenant: &'a str) -> HealthApiFuture<'a, Map<String, Value>> {
        Box::pin(async move {
            self.count_enabled_models_by_provider(tenant)
                .await
                .map_err(|_| HealthApiError::Internal)
        })
    }
}
pub trait HealthProviderProbeSource: Send + Sync {
    fn status(
        &self,
        tenant: &str,
        provider: &str,
        configured: bool,
    ) -> Result<ProviderProbeStatus, ProviderProbeError>;
}
impl HealthProviderProbeSource for ProviderAdminProbeState {
    fn status(
        &self,
        tenant: &str,
        provider: &str,
        configured: bool,
    ) -> Result<ProviderProbeStatus, ProviderProbeError> {
        ProviderAdminProbeState::status(self, tenant, provider, configured)
    }
}
const DEPENDENCIES: [&str; 8] = [
    "gateway_core",
    "auth_config",
    "database",
    "redis",
    "agent_runtime",
    "model_plane",
    "knowledge_service",
    "image_worker",
];
const PROVIDERS: [(&str, &str); 6] = [
    ("openai", "OpenAI"),
    ("anthropic", "Anthropic"),
    ("deepseek", "DeepSeek"),
    ("dashscope", "阿里云 DashScope"),
    ("google", "Google Gemini"),
    ("google-vertex", "Google Vertex AI"),
];
#[derive(Clone)]
pub struct HealthApi {
    // The canonical public endpoint resolves the registry but never queries it.
    _registry: Arc<ServiceRegistry>,
    monitor: Arc<dyn HealthStatusSource>,
    dependencies: Arc<dyn HealthDependencyState>,
    model_meta: Option<Arc<dyn HealthModelMetadata>>,
    probes: Arc<dyn HealthProviderProbeSource>,
}
impl HealthApi {
    pub fn new(
        registry: Arc<ServiceRegistry>,
        monitor: Arc<dyn HealthStatusSource>,
        dependencies: Arc<dyn HealthDependencyState>,
        model_meta: Option<Arc<dyn HealthModelMetadata>>,
        probes: Arc<dyn HealthProviderProbeSource>,
    ) -> Self {
        Self {
            _registry: registry,
            monitor,
            dependencies,
            model_meta,
            probes,
        }
    }
    pub fn gateway_health(&self) -> Value {
        json!({"status":"ok"})
    }
    pub async fn all_services_health(
        &self,
        user: &HealthIdentity,
    ) -> Result<Value, HealthApiError> {
        require_admin(user)?;
        let snapshot = gateway_dependency_snapshot(self.dependencies.as_ref()).await?;
        let mut result = Map::new();
        for (id, status) in self.monitor.all_status()? {
            result.insert(id, project_status(&status, false)?);
        }
        for id in DEPENDENCIES {
            result.insert(id.into(), dependency_service_status(&snapshot, id));
        }
        // Preserve the second monitor read and the source's short circuit.
        let optional_degraded = snapshot.get("degraded") == Some(&Value::Bool(true))
            || self
                .monitor
                .all_status()?
                .iter()
                .any(|(_, status)| status.status != "healthy");
        let status = if snapshot.get("core_ready") != Some(&Value::Bool(true)) {
            "unavailable"
        } else if optional_degraded {
            "degraded"
        } else {
            "healthy"
        };
        result.insert("assistant".into(), json!({"status":status}));
        Ok(Value::Object(result))
    }
    pub async fn service_health(
        &self,
        user: &HealthIdentity,
        service_id: &str,
    ) -> Result<Value, HealthApiError> {
        require_admin(user)?;
        if DEPENDENCIES.contains(&service_id) {
            let snapshot = gateway_dependency_snapshot(self.dependencies.as_ref()).await?;
            let mut result = json!({"service_id":service_id})
                .as_object()
                .expect("object")
                .clone();
            result.extend(
                dependency_service_status(&snapshot, service_id)
                    .as_object()
                    .expect("object")
                    .clone(),
            );
            return Ok(Value::Object(result));
        }
        let status = self
            .monitor
            .get_status(service_id)?
            .ok_or_else(|| HealthApiError::http(404, "service health not found"))?;
        project_status(&status, true)
    }
    pub async fn all_providers_health(
        &self,
        user: &HealthIdentity,
    ) -> Result<Value, HealthApiError> {
        require_admin(user)?;
        let unavailable = || {
            HealthApiError::http(
                503,
                json!({"code":"PROVIDER_HEALTH_COLLECTION_UNAVAILABLE"}),
            )
        };
        let metadata = self.model_meta.as_ref().ok_or_else(unavailable)?;
        let tenant = user.tenant();
        let collected = async {
            let configured = metadata
                .configured(tenant)
                .await?
                .into_iter()
                .collect::<BTreeSet<_>>();
            let counts = metadata.model_counts(tenant).await?;
            Ok::<_, HealthApiError>((configured, counts))
        }
        .await;
        let (configured, counts) = match collected {
            Ok(value) => value,
            Err(HealthApiError::Cancelled) => return Err(HealthApiError::Cancelled),
            Err(_) => return Err(unavailable()),
        };
        let mut ids: BTreeSet<String> = PROVIDERS.iter().map(|(id, _)| (*id).into()).collect();
        ids.extend(configured.iter().cloned());
        ids.extend(counts.keys().cloned());
        let mut result = Map::new();
        for id in ids {
            let is_configured = configured.contains(&id);
            // Outside the collection try/except in the canonical endpoint.
            let probe = self
                .probes
                .status(tenant, &id, is_configured)
                .map_err(|_| HealthApiError::Internal)?;
            let name = PROVIDERS
                .iter()
                .find_map(|(key, name)| (*key == id).then_some(*name))
                .unwrap_or(&id);
            let count = if is_configured {
                counts.get(&id).cloned().unwrap_or_else(|| json!(0))
            } else {
                json!(0)
            };
            result.insert(id.clone(), json!({"name":name,"status":probe.status,"last_check":probe.last_check,"probe_source":probe.probe_source,"configured":is_configured,"model_count":count}));
        }
        Ok(Value::Object(result))
    }
}
fn project_status(status: &HealthStatus, include_id: bool) -> Result<Value, HealthApiError> {
    if !status.latency.is_finite() {
        return Err(HealthApiError::Internal);
    }
    let mut value = Map::new();
    if include_id {
        value.insert("service_id".into(), status.service_id.clone().into());
    }
    value.extend(json!({"status":status.status,"latency":status.latency,"last_check":status.last_check,"error":status.error}).as_object().expect("object").clone());
    Ok(Value::Object(value))
}
/// Extension presence, including an empty value, suppresses header trace fallback.
#[derive(Clone, Default)]
pub struct HealthRequestContext {
    pub trace_id: String,
}
pub type HealthCancellationFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;
pub trait HealthHttpCancellation: Send + Sync {
    fn cancelled(&self) -> HealthCancellationFuture<'_>;
}
#[derive(Clone)]
pub struct HealthHttpApi {
    api: HealthApi,
    identity: Arc<dyn HealthIdentityResolver>,
    cancellation: Arc<dyn HealthHttpCancellation>,
    redirect_origin: Option<String>,
}
impl HealthHttpApi {
    pub fn new(
        api: HealthApi,
        identity: Arc<dyn HealthIdentityResolver>,
        cancellation: Arc<dyn HealthHttpCancellation>,
    ) -> Self {
        Self {
            api,
            identity,
            cancellation,
            redirect_origin: None,
        }
    }
    pub fn with_redirect_origin(mut self, origin: &str) -> Result<Self, HealthApiError> {
        let parsed = reqwest::Url::parse(origin).map_err(|_| HealthApiError::Internal)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(HealthApiError::Internal);
        }
        self.redirect_origin = Some(parsed.as_str().trim_end_matches('/').into());
        Ok(self)
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/v1/health", any(dispatch))
            .route("/api/v1/health/", any(dispatch))
            .route("/api/v1/health/{*rest}", any(dispatch))
            .with_state(self)
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Route {
    Public,
    Services,
    Service,
    Providers,
}
fn route(path: &str) -> Option<(Route, String)> {
    match path {
        "/api/v1/health" => Some((Route::Public, String::new())),
        "/api/v1/health/services" => Some((Route::Services, String::new())),
        "/api/v1/health/providers" => Some((Route::Providers, String::new())),
        _ => {
            let id = path.strip_prefix("/api/v1/health/services/")?;
            (!id.is_empty() && !id.contains('/')).then(|| (Route::Service, id.into()))
        }
    }
}
fn decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((a * 16 + b) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}
// Starlette RedirectResponse quotes the full URL with this explicit safe set.
fn redirect_location(url: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(url.len());
    for byte in url.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~:/%#?=@[]!$&'()*+,;".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[(byte >> 4) as usize]));
            out.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    out
}
fn response(error: HealthApiError, trace: &str) -> Response {
    (
        StatusCode::from_u16(error.status().expect("non-cancellation response"))
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(
            error
                .response_body(trace)
                .expect("non-cancellation response"),
        ),
    )
        .into_response()
}
async fn dispatch(State(state): State<HealthHttpApi>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    // The source has no query/body parameters; never decode or poll this body.
    drop(body);
    let path = decode_path(parts.uri.path()).unwrap_or_default();
    let Some((route, id)) = route(&path) else {
        if path.ends_with('/')
            && route(path.trim_end_matches('/')).is_some()
            && let Some(origin) = &state.redirect_origin
        {
            let query = parts
                .uri
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(
                    header::LOCATION,
                    redirect_location(&format!("{origin}{}{query}", path.trim_end_matches('/'))),
                )],
            )
                .into_response();
        }
        // Axum computes Content-Length before stripping a HEAD body.
        return response(HealthApiError::http(404, "Not Found"), "");
    };
    if parts.method != Method::GET {
        let mut result = response(HealthApiError::http(405, "Method Not Allowed"), "");
        result
            .headers_mut()
            .insert(header::ALLOW, "GET".parse().expect("static method"));
        // Leave the JSON body here: Axum preserves its Content-Length and
        // strips it for HEAD after dispatch, matching Starlette.
        return result;
    }
    let trace = parts
        .extensions
        .get::<HealthRequestContext>()
        .map(|ctx| ctx.trace_id.clone())
        .unwrap_or_else(|| {
            ["x-trace-id", "x-request-id"]
                .into_iter()
                .find_map(|name| {
                    parts
                        .headers
                        .get(name)
                        .filter(|value| !value.as_bytes().is_empty())
                        .map(|value| value.as_bytes().iter().copied().map(char::from).collect())
                })
                .unwrap_or_default()
        });
    let outcome = async {
        if route == Route::Public {
            return Ok(state.api.gateway_health());
        }
        let user = state.identity.resolve(&parts.headers, &path).await?;
        match route {
            Route::Services => state.api.all_services_health(&user).await,
            Route::Service => state.api.service_health(&user, &id).await,
            Route::Providers => state.api.all_providers_health(&user).await,
            Route::Public => unreachable!(),
        }
    }
    .await;
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(HealthApiError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => response(error, &trace),
    }
}
