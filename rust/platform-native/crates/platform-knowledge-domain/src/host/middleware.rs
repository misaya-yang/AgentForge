//! Source: core/middleware/_streaming and request_body_limit.py.
//! Only high-cost JSON request bodies are bounded; response streams are never collected.
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub request_id: String,
    pub trace_id: String,
    pub span_id: String,
    pub traceparent: String,
    pub anonymous_id: Option<String>,
}
#[derive(Clone)]
pub struct MiddlewareConfig {
    pub anonymous_enabled: bool,
    pub anonymous_header: String,
    pub anonymous_cookie: String,
    pub anonymous_ttl_days: i64,
    pub anonymous_same_site: String,
    pub trace_excluded: BTreeSet<String>,
    pub body_limit: usize,
    pub request_governance:
        ai_platform_service_primitives::request_governance::RequestGovernanceConfig,
}
impl Default for MiddlewareConfig {
    fn default() -> Self {
        Self {
            anonymous_enabled: true,
            anonymous_header: "X-AG-Anonymous-Id".into(),
            anonymous_cookie: "ag_anon_id".into(),
            anonymous_ttl_days: 365,
            anonymous_same_site: "lax".into(),
            trace_excluded: ["/health", "/health/live", "/health/ready"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            body_limit: 8 * 1024 * 1024,
            request_governance: Default::default(),
        }
    }
}
#[derive(Default)]
struct DrainInner {
    draining: bool,
    inflight: usize,
}
#[derive(Default)]
pub struct DrainState {
    inner: Mutex<DrainInner>,
    zero: Notify,
}
pub struct DrainPermit(Arc<DrainState>);
impl Drop for DrainPermit {
    fn drop(&mut self) {
        let mut s = self.0.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.inflight = s.inflight.saturating_sub(1);
        if s.inflight == 0 {
            self.0.zero.notify_waiters();
        }
    }
}
impl DrainState {
    pub fn begin_drain(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .draining = true;
    }
    pub fn draining(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .draining
    }
    pub fn inflight(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inflight
    }
    pub fn enter(self: &Arc<Self>) -> Option<DrainPermit> {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.draining {
            None
        } else {
            s.inflight += 1;
            Some(DrainPermit(self.clone()))
        }
    }
    pub async fn wait_drained(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                let notified = self.zero.notified();
                if self.inflight() == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .is_ok()
    }
}
#[derive(Clone)]
pub struct MiddlewareState {
    pub config: Arc<MiddlewareConfig>,
    pub drain: Arc<DrainState>,
}
fn ascii(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .map(|v| {
            v.as_bytes()
                .iter()
                .copied()
                .filter(u8::is_ascii)
                .map(char::from)
                .collect()
        })
        .unwrap_or_default()
}
fn safe_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 64
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}
fn safe_trace(s: &str) -> bool {
    s.len() == 32
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s != "00000000000000000000000000000000"
}
fn trace(headers: &HeaderMap) -> (String, String, String) {
    let parent = ascii(headers, "traceparent");
    let parts: Vec<_> = parent.trim().split('-').collect();
    let incoming = ascii(headers, "x-trace-id");
    let id = if parts.len() == 4
        && parts[0] == "00"
        && parts[2].len() == 16
        && parts[3].len() == 2
        && safe_trace(parts[1])
    {
        parts[1].to_owned()
    } else if safe_trace(&incoming) {
        incoming
    } else {
        uuid::Uuid::new_v4().simple().to_string()
    };
    let span = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
    let parent = format!("00-{id}-{span}-01");
    (id, span, parent)
}
fn append(headers: &mut HeaderMap, key: &str, value: &str) {
    if let (Ok(k), Ok(v)) = (
        HeaderName::from_bytes(key.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.append(k, v);
    }
}
fn body_limited(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/assistant/chat"
            | "/api/v1/assistant/chat/stream"
            | "/internal/v1/agent-model-plane/responses"
    ) || path.starts_with("/api/v1/public/agents/")
}
fn probe(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/health/live" | "/health/ready" | "/metrics"
    )
}
fn rejection(status: StatusCode, detail: &str, retry: &str) -> Response {
    (
        status,
        [("retry-after", retry)],
        Json(json!({"detail":detail})),
    )
        .into_response()
}

pub async fn request_body_limit(
    State(state): State<MiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    if !body_limited(request.uri().path()) {
        return next.run(request).await;
    }
    if let Some(raw) = request.headers().get("content-length") {
        let text = String::from_utf8_lossy(raw.as_bytes());
        if !text.is_empty() {
            match text.trim().parse::<i128>() {
                Ok(n) if n <= state.config.body_limit as i128 => {}
                _ => {
                    return rejection(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large", "0");
                }
            }
        }
    }
    let (parts, body) = request.into_parts();
    match to_bytes(body, state.config.body_limit).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(_) => rejection(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large", "0"),
    }
}
pub async fn request_context(
    State(state): State<MiddlewareState>,
    mut request: Request,
    next: Next,
) -> Response {
    let config = &state.config;
    let path = request.uri().path().to_owned();
    let incoming = ascii(request.headers(), "x-request-id");
    let incoming = incoming.trim();
    let request_id = if safe_request_id(incoming) {
        incoming.to_owned()
    } else {
        format!("svc-{}", uuid::Uuid::new_v4())
    };
    let (trace_id, span_id, traceparent) = trace(request.headers());
    let mut anonymous_id = None;
    let mut set_cookie = false;
    if config.anonymous_enabled {
        let raw = ascii(request.headers(), &config.anonymous_header);
        let raw = if raw.is_empty() {
            ascii(request.headers(), "cookie")
                .split(';')
                .find_map(|part| {
                    part.trim()
                        .split_once('=')
                        .filter(|(key, _)| *key == config.anonymous_cookie)
                        .map(|(_, v)| v.to_owned())
                })
                .unwrap_or_default()
        } else {
            raw
        };
        let valid = !raw.is_empty() && uuid::Uuid::parse_str(&raw).is_ok();
        set_cookie = !valid;
        anonymous_id = Some(if valid {
            raw
        } else {
            uuid::Uuid::new_v4().to_string()
        });
    }
    if let Some(existing) = request.extensions_mut().get_mut::<RequestContext>() {
        existing.anonymous_id = anonymous_id.clone();
    } else {
        request.extensions_mut().insert(RequestContext {
            request_id,
            trace_id: trace_id.clone(),
            span_id: span_id.clone(),
            traceparent: traceparent.clone(),
            anonymous_id: anonymous_id.clone(),
        });
    }
    let mut response = next.run(request).await;
    if !config.trace_excluded.contains(&path) {
        append(response.headers_mut(), "x-trace-id", &trace_id);
        append(response.headers_mut(), "x-span-id", &span_id);
        append(response.headers_mut(), "traceparent", &traceparent);
    }
    if set_cookie && let Some(id) = anonymous_id {
        let cookie = format!(
            "{}={id}; Max-Age={}; Path=/; HttpOnly; SameSite={}",
            config.anonymous_cookie,
            config.anonymous_ttl_days.saturating_mul(86400),
            config.anonymous_same_site
        );
        append(response.headers_mut(), "set-cookie", &cookie);
        append(response.headers_mut(), &config.anonymous_header, &id);
    }
    response
}
pub async fn security_headers(request: Request, next: Next) -> Response {
    let embed = request.uri().path().starts_with("/embed/agents/");
    let mut response = next.run(request).await;
    response.headers_mut().remove("x-frame-options");
    for (k, v) in [
        ("x-content-type-options", "nosniff"),
        ("x-xss-protection", "1; mode=block"),
        ("referrer-policy", "strict-origin-when-cross-origin"),
        (
            "strict-transport-security",
            "max-age=31536000; includeSubDomains",
        ),
    ] {
        append(response.headers_mut(), k, v);
    }
    if !embed {
        append(response.headers_mut(), "x-frame-options", "DENY");
    }
    response
}
pub async fn graceful_drain(
    State(state): State<MiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    if probe(request.uri().path()) {
        return next.run(request).await;
    }
    let Some(permit) = state.drain.enter() else {
        return rejection(StatusCode::SERVICE_UNAVAILABLE, "shutting down", "5");
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

/// The split layers preserve the source order: authentication sees incoming
/// anonymous identity, while generated anonymous cookies happen after rate admission.
pub async fn anonymous_identity(
    State(state): State<MiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    let mut config = (*state.config).clone();
    config.trace_excluded.insert(request.uri().path().into());
    request_context(
        State(MiddlewareState {
            config: Arc::new(config),
            drain: state.drain,
        }),
        request,
        next,
    )
    .await
}
pub async fn tracing_context(
    State(state): State<MiddlewareState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let incoming = ascii(request.headers(), "x-request-id");
    let incoming = incoming.trim();
    let request_id = if safe_request_id(incoming) {
        incoming.to_owned()
    } else {
        format!("svc-{}", uuid::Uuid::new_v4())
    };
    let (trace_id, span_id, traceparent) = trace(request.headers());
    request.extensions_mut().insert(RequestContext {
        request_id,
        trace_id: trace_id.clone(),
        span_id: span_id.clone(),
        traceparent: traceparent.clone(),
        anonymous_id: None,
    });
    let mut response = next.run(request).await;
    if !state.config.trace_excluded.contains(&path) {
        append(response.headers_mut(), "x-trace-id", &trace_id);
        append(response.headers_mut(), "x-span-id", &span_id);
        append(response.headers_mut(), "traceparent", &traceparent);
    }
    response
}

/// Separate source OTel middleware; streaming response IDs remain the existing
/// transport contract while this SDK carrier supplies actual parented spans.
pub async fn distributed_tracing(mut request: Request, next: Next) -> Response {
    use ai_platform_service_primitives::distributed_tracing as telemetry;
    if let Some(id) = request
        .extensions()
        .get::<RequestContext>()
        .map(|c| c.request_id.clone())
    {
        request.extensions_mut().insert(telemetry::RequestId(id));
    }
    telemetry::inbound(request, next).await
}
