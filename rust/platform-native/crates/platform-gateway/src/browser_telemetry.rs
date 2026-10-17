//! Authenticated, explicitly tenant-enabled browser RUM collector. No external exporter or raw error text.
//! Host must mount `router(api)` inside its existing native request/identity context middleware.
use crate::migration_content::{ContentAccess, ContentIdentity};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const PATH: &str = "/api/v1/telemetry/browser";
const MAX_BODY: usize = 16 * 1024;
const MAX_SESSIONS: usize = 4096;
const SESSION_TTL: Duration = Duration::from_secs(900);
const ROUTES: &[&str] = &[
    "/",
    "/assistant",
    "/playground",
    "/knowledge/:id",
    "/agents/:id",
    "/eval/:id",
    "/settings",
    "/dashboard",
    "/tasks",
    "/other",
];

#[derive(Clone)]
pub struct BrowserTelemetryApi {
    identity: Arc<dyn ContentAccess>,
    allowed_tenants: Arc<HashSet<String>>,
    allowed_origins: Arc<HashSet<String>>,
    sample_bps: u16,
    state: Arc<Mutex<CollectorState>>,
}
#[derive(Default)]
struct CollectorState {
    sessions: HashMap<String, SampleSession>,
    limits: HashMap<String, RateWindow>,
}
struct SampleSession {
    authority: String,
    sampled: bool,
    created: Instant,
}
struct RateWindow {
    start: Instant,
    count: usize,
}
impl BrowserTelemetryApi {
    /// Empty tenant/origin sets fail closed; sampling is capped at 10% independently of client input.
    pub fn new(
        identity: Arc<dyn ContentAccess>,
        tenants: Vec<String>,
        origins: Vec<String>,
        sample_bps: u16,
    ) -> Result<Self, &'static str> {
        if sample_bps > 1000 {
            return Err("Browser telemetry sampling cannot exceed 10 percent");
        }
        for origin in &origins {
            let url = url::Url::parse(origin).map_err(|_| "Invalid telemetry origin")?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
                || url.origin().ascii_serialization() != *origin
            {
                return Err("Telemetry requires canonical HTTP origins");
            }
        }
        if tenants
            .iter()
            .any(|tenant| tenant.is_empty() || tenant.len() > 128)
        {
            return Err("Invalid telemetry tenant allowlist");
        }
        Ok(Self {
            identity,
            allowed_tenants: Arc::new(tenants.into_iter().collect()),
            allowed_origins: Arc::new(origins.into_iter().collect()),
            sample_bps,
            state: Arc::new(Mutex::new(CollectorState::default())),
        })
    }
    fn origin_allowed(&self, headers: &HeaderMap, mutation: bool) -> bool {
        if self.allowed_origins.is_empty() {
            return false;
        }
        if let Some(site) = headers
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
        {
            if !matches!(site, "same-origin" | "none") {
                return false;
            }
        }
        match headers.get("origin").and_then(|value| value.to_str().ok()) {
            Some(origin) => self.allowed_origins.contains(origin),
            None => !mutation,
        }
    }
    async fn principal(
        &self,
        headers: &HeaderMap,
        mutation: bool,
    ) -> Result<ContentIdentity, Response> {
        if !self.origin_allowed(headers, mutation) {
            return Err(reject(StatusCode::FORBIDDEN));
        }
        let principal = self
            .identity
            .resolve(headers)
            .await
            .map_err(IntoResponse::into_response)?;
        if !principal.is_authenticated
            || principal.user_id.is_empty()
            || !self.allowed_tenants.contains(&principal.tenant_id)
        {
            return Err(reject(StatusCode::FORBIDDEN));
        }
        Ok(principal)
    }
}
fn reject(status: StatusCode) -> Response {
    (
        status,
        Json(json!({"detail":"Browser telemetry unavailable"})),
    )
        .into_response()
}
fn authority(principal: &ContentIdentity) -> String {
    let source = serde_json::to_vec(&json!([
        principal.tenant_id,
        principal.user_id,
        principal.roles,
        principal.permissions,
        principal.tier
    ]))
    .unwrap_or_default();
    format!("{:x}", Sha256::digest(source))
}
fn admit(
    state: &mut CollectorState,
    key: String,
    cost: usize,
    maximum: usize,
    now: Instant,
) -> bool {
    state
        .limits
        .retain(|_, window| now.duration_since(window.start) < Duration::from_secs(60));
    if state.limits.len() >= MAX_SESSIONS && !state.limits.contains_key(&key) {
        return false;
    }
    let window = state.limits.entry(key).or_insert(RateWindow {
        start: now,
        count: 0,
    });
    if window.count.saturating_add(cost) > maximum {
        return false;
    }
    window.count += cost;
    true
}
pub fn router(api: BrowserTelemetryApi) -> Router {
    Router::new()
        .route("/api/v1/telemetry/browser/config", get(configuration))
        .route(PATH, post(collect))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(api)
}
async fn configuration(State(api): State<BrowserTelemetryApi>, headers: HeaderMap) -> Response {
    let principal = match api.principal(&headers, false).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let now = Instant::now();
    let mut state = api.state.lock().unwrap_or_else(|error| error.into_inner());
    state
        .sessions
        .retain(|_, session| now.duration_since(session.created) < SESSION_TTL);
    let scope = authority(&principal);
    if !admit(&mut state, format!("config:{scope}"), 1, 12, now)
        || state.sessions.len() >= MAX_SESSIONS
    {
        return reject(StatusCode::TOO_MANY_REQUESTS);
    }
    let id = uuid::Uuid::new_v4();
    let draw = u16::from_be_bytes([id.as_bytes()[0], id.as_bytes()[1]]) as u32;
    let sampled = draw * 10_000 / 65_536 < u32::from(api.sample_bps);
    let session_id = id.simple().to_string();
    state.sessions.insert(
        session_id.clone(),
        SampleSession {
            authority: scope,
            sampled,
            created: now,
        },
    );
    ([("cache-control", "no-store")], Json(json!({"enabled":api.sample_bps > 0,"sampled":sampled,"sessionId":session_id,"sampleRate":f64::from(api.sample_bps)/10_000.,"ttlSeconds":SESSION_TTL.as_secs(),"sourceMapsEnabled":false}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    session_id: String,
    records: Vec<BrowserRecord>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum BrowserRecord {
    Metric {
        name: String,
        route: String,
        value: f64,
        #[serde(default)]
        quality: Option<String>,
        #[serde(default)]
        trace_id: Option<String>,
        #[serde(default)]
        span_id: Option<String>,
        #[serde(default)]
        http_status: Option<u16>,
    },
    Error {
        name: String,
        route: String,
        frames: Vec<ErrorFrame>,
        #[serde(default)]
        trace_id: Option<String>,
    },
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorFrame {
    asset: String,
    line: u32,
    column: u32,
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && value.bytes().any(|byte| byte != b'0')
}
impl BrowserRecord {
    fn valid(&self, session_id: &str) -> bool {
        match self {
            Self::Metric {
                name,
                route,
                value,
                quality,
                trace_id,
                span_id,
                http_status,
            } => {
                [
                    "INP",
                    "LCP",
                    "CLS",
                    "route_commit",
                    "api_headers",
                    "chat_first_token",
                    "chat_duration",
                ]
                .contains(&name.as_str())
                    && ROUTES.contains(&route.as_str())
                    && value.is_finite()
                    && *value >= 0.
                    && *value <= (if name == "CLS" { 10. } else { 3_600_000. })
                    && quality
                        .as_ref()
                        .is_none_or(|value| matches!(value.as_str(), "native" | "observed"))
                    && trace_id.as_ref().is_none_or(|value| value == session_id)
                    && span_id.as_ref().is_none_or(|value| hex(value, 16))
                    && http_status.is_none_or(|value| (100..=599).contains(&value))
            }
            Self::Error {
                name,
                route,
                frames,
                trace_id,
            } => {
                [
                    "Error",
                    "TypeError",
                    "RangeError",
                    "ReferenceError",
                    "SyntaxError",
                    "UnhandledRejection",
                    "RenderError",
                ]
                .contains(&name.as_str())
                    && ROUTES.contains(&route.as_str())
                    && frames.len() <= 5
                    && trace_id.as_ref().is_none_or(|value| value == session_id)
                    && frames.iter().all(|frame| {
                        frame.asset.starts_with("/assets/")
                            && frame.asset.ends_with(".js")
                            && frame.asset.len() <= 132
                            && frame.asset[8..].bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                            })
                            && (1..=10_000_000).contains(&frame.line)
                            && (1..=10_000_000).contains(&frame.column)
                    })
            }
        }
    }
}
async fn collect(
    State(api): State<BrowserTelemetryApi>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let principal = match api.principal(&headers, true).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim() == "application/json")
        })
    {
        return reject(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let batch: Batch = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return reject(StatusCode::BAD_REQUEST),
    };
    if !hex(&batch.session_id, 32)
        || batch.records.is_empty()
        || batch.records.len() > 20
        || batch
            .records
            .iter()
            .any(|record| !record.valid(&batch.session_id))
    {
        return reject(StatusCode::BAD_REQUEST);
    }
    let now = Instant::now();
    let scope = authority(&principal);
    {
        let mut state = api.state.lock().unwrap_or_else(|error| error.into_inner());
        let permitted = state
            .sessions
            .get(&batch.session_id)
            .is_some_and(|session| {
                session.authority == scope
                    && session.sampled
                    && now.duration_since(session.created) < SESSION_TTL
            });
        if !permitted {
            return reject(StatusCode::FORBIDDEN);
        }
        if !admit(
            &mut state,
            format!("records:{scope}"),
            batch.records.len(),
            60,
            now,
        ) || !admit(
            &mut state,
            format!("tenant:{}", principal.tenant_id),
            batch.records.len(),
            600,
            now,
        ) {
            return reject(StatusCode::TOO_MANY_REQUESTS);
        }
    }
    for record in batch.records {
        // Only the already validated schema is emitted; no headers, request body or user-supplied tenant.
        tracing::info!(target: "browser_telemetry", tenant_id = %principal.tenant_id, page_session = %batch.session_id, record = %serde_json::to_string(&record).unwrap_or_default(), "browser telemetry");
    }
    StatusCode::ACCEPTED.into_response()
}

#[cfg(test)]
#[path = "browser_telemetry_tests.rs"]
mod tests;
