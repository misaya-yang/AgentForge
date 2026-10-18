//! Public Gateway KB proxy. Knowledge mutations always cross the signed HTTP
//! boundary; only account confirmation and Agent-reference impact use Gateway SQL.
mod contract;
mod forwarding;
mod management;
pub mod sdk_capabilities;
pub mod sdk_models;
mod sdk_reads;
use crate::migration_content::{
    ContentAccess, ContentDatabase, ContentError, ContentIdentity, access::NativeContentAccess,
};
use crate::migration_knowledge_query::auth::GatewayAuth;
use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, Method},
    response::{IntoResponse, Response},
    routing::MethodFilter,
    routing::on,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

pub type Result<T> = std::result::Result<T, ContentError>;
#[derive(Clone)]
pub struct ProxyConfig {
    pub retrieval_rpc: Option<(String, ai_platform_rpc::RpcConfig)>,
    pub origin: reqwest::Url,
    pub allow_private_http: bool,
    pub json_body_mb: usize,
    pub file_body_mb: usize,
    pub batch_body_mb: usize,
}
impl ProxyConfig {
    pub fn new(origin: reqwest::Url) -> Self {
        Self {
            origin,
            allow_private_http: false,
            retrieval_rpc: None,
            json_body_mb: 4,
            file_body_mb: 16,
            batch_body_mb: 32,
        }
    }
    fn limit(&self, path: &str) -> (usize, bool) {
        let path = path.trim_end_matches('/');
        if path.ends_with("/documents/batch-upload") {
            (
                self.batch_body_mb.clamp(1, 48) * 1024 * 1024 + 256 * 1024,
                true,
            )
        } else if path.ends_with("/documents/upload") {
            (
                self.file_body_mb.clamp(1, 48) * 1024 * 1024 + 256 * 1024,
                true,
            )
        } else {
            (self.json_body_mb.clamp(1, 16) * 1024 * 1024, false)
        }
    }
}
pub struct KnowledgeProxy {
    pub context_metrics: Option<Arc<super::context_metrics::ContextMetricsCollector>>,
    retrieval_rpc: Option<ai_platform_rpc::RetrievalClient>,
    pub sdk_reads: Option<sdk_reads::SdkReads>,
    pub sdk_control: Option<Arc<crate::migration_agent::control::RuntimeControlPlane>>,
    config: ProxyConfig,
    signer: Arc<GatewayAuth>,
    access: Arc<NativeContentAccess>,
    database: ContentDatabase,
    passwords: ai_platform_auth::PasswordEngine,
    client: reqwest::Client,
    upload_slots: Arc<Semaphore>,
    upload_bytes: Arc<Semaphore>,
    connections: Arc<Semaphore>,
    breakers: std::sync::Mutex<std::collections::BTreeMap<String, Arc<forwarding::Breaker>>>,
    pub trace_capture: Option<crate::migration_knowledge_query::rag_trace::RagTraceCapture>,
}
impl KnowledgeProxy {
    /// Explicit operator-selected internal transport. Connection failure fails startup;
    /// a configured RPC never silently falls back to a differently governed HTTP hop.
    pub async fn configure_retrieval_rpc(
        &mut self,
        origin: String,
        config: ai_platform_rpc::RpcConfig,
    ) -> Result<()> {
        // The current RPC adapter has no authenticated workload peer transport.
        // An explicit RPC selection must never bypass required mTLS.
        if crate::migration_agent::control::execution_transport::tls_enabled() {
            return Err(unavailable());
        }
        let parsed = reqwest::Url::parse(&origin).map_err(|_| unavailable())?;
        let loopback = parsed
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
            || !(parsed.scheme() == "https"
                || parsed.scheme() == "http" && (loopback || self.config.allow_private_http))
        {
            return Err(unavailable());
        }
        self.retrieval_rpc = Some(
            ai_platform_rpc::RetrievalClient::connect(origin, config)
                .await
                .map_err(|_| unavailable())?,
        );
        Ok(())
    }

    /// Inert construction: no network, secrets discovery, worker, or KS SQL.
    pub fn configure_sdk_reads(
        &mut self,
        registry: Arc<ai_platform_gateway_foundation::service_registry::ServiceRegistry>,
        sessions: Arc<dyn ai_platform_gateway_foundation::session_manager::SessionStorage>,
    ) {
        self.sdk_reads = Some(sdk_reads::SdkReads {
            grants: registry.sdk_read_grants(),
            registry,
            sessions,
        });
    }
    pub fn new(
        config: ProxyConfig,
        signer: Arc<GatewayAuth>,
        access: Arc<NativeContentAccess>,
        database: ContentDatabase,
    ) -> Result<Self> {
        let u = &config.origin;
        let loopback = u
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if u.host_str().is_none()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || u.path() != "/"
            || !(u.scheme() == "https"
                || u.scheme() == "http" && (loopback || config.allow_private_http))
            || signer.version != "v2"
            || signer.allow_anonymous
            || signer.caller_service != "gateway"
            || signer.audience != "knowledge-service"
        {
            return Err(ContentError::new(
                503,
                "Invalid knowledge proxy configuration",
            ));
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|_| unavailable())?;
        Ok(Self {
            context_metrics: None,
            retrieval_rpc: None,
            sdk_reads: None,
            sdk_control: None,
            config,
            signer,
            access,
            database,
            passwords: ai_platform_auth::PasswordEngine::new(2).map_err(|_| unavailable())?,
            client,
            upload_slots: Arc::new(Semaphore::new(2)),
            upload_bytes: Arc::new(Semaphore::new(128 * 1024 * 1024)),
            connections: Arc::new(Semaphore::new(50)),
            breakers: Default::default(),
            trace_capture: None,
        })
    }
}
pub fn router(state: Arc<KnowledgeProxy>) -> Router {
    let methods = contract::METHODS
        .iter()
        .fold(MethodFilter::GET, |filter, method| {
            filter.or(match *method {
                "GET" => MethodFilter::GET,
                "POST" => MethodFilter::POST,
                "PUT" => MethodFilter::PUT,
                "PATCH" => MethodFilter::PATCH,
                "DELETE" => MethodFilter::DELETE,
                _ => unreachable!("declared proxy method"),
            })
        });
    let mut router = Router::new()
        .route(
            "/internal/v1/sdk-tools/capabilities/admit",
            axum::routing::post(sdk_capabilities::admit),
        )
        .route(
            "/internal/v1/sdk-tools/capabilities/execute",
            axum::routing::post(sdk_capabilities::execute),
        )
        .route(
            "/internal/v1/sdk-tools/knowledge/retrieve",
            axum::routing::post(sdk_reads::retrieve),
        )
        .route(
            "/internal/v1/sdk-tools/knowledge/admit",
            axum::routing::post(sdk_reads::admit),
        );
    for prefix in contract::PUBLIC_PREFIXES {
        router = router
            .route(&format!("{prefix}/"), on(methods, root_dispatch))
            .route(&format!("{prefix}/{{*path}}"), on(methods, dispatch));
    }
    router
        .with_state(state.clone())
        .merge(sdk_models::router(state.clone()))
        .layer(axum::middleware::from_fn_with_state(
            state.access.clone(),
            crate::migration_content::access::request_context,
        ))
}
fn unavailable() -> ContentError {
    ContentError::new(503, "Knowledge service is unavailable")
}
fn code(status: u16, value: &str) -> ContentError {
    ContentError::new(status, json!({"code":value}))
}
fn read_operation(method: &Method, path: &str) -> bool {
    contract::read_operation(method.as_str(), path)
}

impl KnowledgeProxy {
    async fn identity(
        &self,
        headers: &HeaderMap,
        path: &str,
        read: bool,
    ) -> Result<ContentIdentity> {
        let resolver = &self.access.identities;
        let mut selected = headers.clone();
        if !resolver.jwt_enabled {
            selected.remove("authorization");
        }
        if !resolver.api_key_enabled {
            selected.remove(&resolver.api_key_header);
        }
        let presented = selected.contains_key("authorization")
            || selected.contains_key(&resolver.api_key_header);
        if !presented {
            return self.access.resolve(headers).await;
        }
        let admission = resolver.admission.request(&selected, path);
        let principal = match admission.resolve().await {
            Ok(p) => p,
            Err(error) => {
                let fallback = self.access.resolve(headers).await?;
                if fallback.user_id.starts_with("apikey:") && fallback.tenant_id.is_empty() {
                    return Ok(fallback);
                }
                return Err(ContentError::new(error.status(), "Authentication required"));
            }
        };
        if let Some(key) = principal.api_key() {
            if !key.allows_knowledge(!read) {
                let required = if read {
                    "knowledge:read"
                } else {
                    "knowledge:write"
                };
                return Err(ContentError::new(
                    403,
                    json!({"message":format!("API key is missing required scope '{required}' for this knowledge-base operation; reissue or update keys that have no assigned scopes"),"required_scope":required,"key_scopes":key.declared_scopes}),
                ));
            }
        }
        Ok(ContentIdentity {
            tenant_id: principal.tenant_id().into(),
            user_id: principal.user_id().into(),
            is_authenticated: true,
            roles: principal.roles().to_vec(),
            permissions: principal.permissions().to_vec(),
            tier: principal.tier().into(),
        })
    }
}
async fn root_dispatch(State(state): State<Arc<KnowledgeProxy>>, request: Request) -> Response {
    match handle(state, String::new(), request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn dispatch(
    State(state): State<Arc<KnowledgeProxy>>,
    Path(path): Path<String>,
    request: Request,
) -> Response {
    match handle(state, path, request).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
async fn handle(state: Arc<KnowledgeProxy>, path: String, request: Request) -> Result<Response> {
    // Reject ambiguous segment encodings before URL normalization or signing.
    if path.len() > 8192
        || path.chars().any(char::is_control)
        || path.contains(['\\', '?', '#', '%'])
        || path.split('/').any(|s| s == "." || s == "..")
    {
        return Err(ContentError::new(400, "Invalid knowledge path"));
    }
    let user = state
        .identity(
            request.headers(),
            request.uri().path(),
            read_operation(request.method(), &path),
        )
        .await?;
    state
        .access
        .rate_limit(&user, "knowledge", None)
        .await
        .map_err(|mut error| {
            if error.status == 429 {
                error.detail = json!("Rate limit exceeded");
            }
            error
        })?;
    let context = request
        .extensions()
        .get::<super::middleware::RequestContext>()
        .cloned();
    let (mut parts, body) = request.into_parts();
    if let Some(context) = context {
        for (name, value) in [
            ("x-request-id", context.request_id),
            ("traceparent", context.traceparent),
        ] {
            if !parts.headers.contains_key(name)
                && let Ok(value) = axum::http::HeaderValue::from_str(&value)
            {
                parts.headers.insert(name, value);
            }
        }
    }
    let management_route = !parts.uri.path().starts_with("/api/v1/kb-tools/");
    let segments: Vec<_> = path.trim_matches('/').split('/').collect();
    if management_route && contract::dataset_impact(parts.method.as_str(), &path) {
        return management::impact(&state, &user, segments[1]).await;
    }
    let has_body =
        [Method::POST, Method::PUT, Method::PATCH, Method::DELETE].contains(&parts.method);
    let (limit, upload) = state.config.limit(&path);
    let _slots = if has_body && upload {
        Some(
            state
                .upload_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| capacity())?,
        )
    } else {
        None
    };
    let _memory = if has_body && upload {
        Some(
            state
                .upload_bytes
                .clone()
                .try_acquire_many_owned((limit * 2) as u32)
                .map_err(|_| capacity())?,
        )
    } else {
        None
    };
    let bytes = if has_body {
        to_bytes(body, limit)
            .await
            .map_err(|_| ContentError::new(413, "Request body too large"))?
            .to_vec()
    } else {
        vec![]
    };
    let bytes = if has_body && !upload {
        management::prepare_body(&state, &user, &parts.method, &path, bytes).await?
    } else {
        bytes
    };
    state
        .forward(
            user,
            parts.method,
            path,
            parts.uri.query().unwrap_or("").into(),
            parts.headers,
            bytes,
        )
        .await
}
fn capacity() -> ContentError {
    let mut e = ContentError::new(503, "KB upload capacity is temporarily unavailable");
    e.headers.insert("Retry-After".into(), "1".into());
    e
}
