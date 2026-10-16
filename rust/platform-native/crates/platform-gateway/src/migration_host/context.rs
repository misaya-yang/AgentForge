//! Request-id context bridge and safe internal HTTP correlation propagation.
use super::middleware::RequestContext;
use axum::{
    body::{Body, Bytes},
    extract::Request,
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
#[derive(Clone, Default)]
pub struct OutboundContext {
    pub request_id: String,
    pub trace_carrier: BTreeMap<String, String>,
}
/// Only an installed tracing adapter should construct this extension; incoming
/// carrier headers are not assumed to represent an active local tracing span.
#[derive(Clone, Default)]
pub struct ActiveTraceCarrier(pub BTreeMap<String, String>);
tokio::task_local! {static OUTBOUND:OutboundContext;}
pub fn current() -> OutboundContext {
    OUTBOUND.try_with(Clone::clone).unwrap_or_default()
}
pub async fn scoped<T>(context: OutboundContext, future: impl Future<Output = T>) -> T {
    OUTBOUND.scope(context, future).await
}
fn safe_id(value: &str, limit: usize, colon: bool) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') || colon && b == b':'
        })
}
// Match the existing Runtime/Worker reader limits. Keep these transport-only
// checks here rather than coupling Gateway to either execution implementation.
fn valid_traceparent(value: &str) -> bool {
    value.is_ascii()
        && value.len() == 55
        && value.as_bytes()[2] == b'-'
        && value.as_bytes()[35] == b'-'
        && value.as_bytes()[52] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 2 | 35 | 52) || byte.is_ascii_hexdigit())
        && !value[..2].eq_ignore_ascii_case("ff")
        && value[3..35].bytes().any(|byte| byte != b'0')
        && value[36..52].bytes().any(|byte| byte != b'0')
}
fn valid_tracestate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| byte == b' ' || (0x21..=0x7e).contains(&byte))
}
/// Capture only bounded correlation for one request's detached work. This is
/// not an identity carrier: actor, authorization, cookies and baggage never survive.
/// An absent tracing adapter remains absent; never use raw incoming headers.
pub fn capture() -> OutboundContext {
    let context = current();
    let request_id = context.request_id.trim();
    let request_id = if safe_id(request_id, 64, false) {
        request_id.to_owned()
    } else {
        format!("svc-{}", uuid::Uuid::new_v4())
    };
    let mut trace_carrier = BTreeMap::new();
    for name in ["traceparent", "tracestate"] {
        if let Some(value) = context.trace_carrier.get(name).map(|value| value.trim())
            && match name {
                "traceparent" => valid_traceparent(value),
                _ => valid_tracestate(value),
            }
        {
            trace_carrier.insert(name.into(), value.into());
        }
    }
    OutboundContext {
        request_id,
        trace_carrier,
    }
}
pub fn internal_http_headers(
    mut existing: BTreeMap<String, String>,
    run_id: Option<&str>,
    turn_id: Option<&str>,
    execution_id: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    // Do not forward raw caller-supplied trace headers. The active middleware
    // carrier below is the only source for downstream trace context.
    existing.retain(|key, _| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "traceparent" | "tracestate" | "baggage"
        )
    });
    let mut lowered: BTreeSet<String> = existing.keys().map(|k| k.to_lowercase()).collect();
    let context = capture();
    let request_id = context.request_id;
    if lowered.insert("x-request-id".into()) {
        existing.insert("x-request-id".into(), request_id);
    }
    for name in ["traceparent", "tracestate"] {
        if let Some(value) = context.trace_carrier.get(name).filter(|v| !v.is_empty())
            && lowered.insert(name.into())
        {
            existing.insert(name.into(), value.clone());
        }
    }
    for (name, value) in [
        ("x-ai-run-id", run_id),
        ("x-ai-turn-id", turn_id),
        ("x-ai-execution-id", execution_id),
    ] {
        let value = value.unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }
        if !safe_id(value, 128, true) {
            return Err("internal correlation id is malformed".into());
        }
        if lowered.insert(name.into()) {
            existing.insert(name.into(), value.into());
        }
    }
    Ok(existing)
}
struct ContextBody {
    body: Body,
    context: OutboundContext,
}
impl HttpBody for ContextBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        OUTBOUND.sync_scope(this.context.clone(), || {
            Pin::new(&mut this.body).poll_frame(cx)
        })
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}
pub async fn bridge(mut request: Request, next: Next) -> Response {
    let candidate = request
        .extensions()
        .get::<RequestContext>()
        .map(|c| c.request_id.trim().to_owned())
        .unwrap_or_default();
    let valid = |s: &str| {
        !s.is_empty()
            && s.chars().count() <= 64
            && s.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let request_id = if valid(&candidate) {
        candidate
    } else {
        let incoming = request
            .headers()
            .get("x-request-id")
            .map(|v| {
                v.as_bytes()
                    .iter()
                    .filter(|b| b.is_ascii())
                    .map(|b| char::from(*b))
                    .collect::<String>()
            })
            .unwrap_or_default();
        let incoming = incoming.trim();
        if valid(incoming) {
            incoming.into()
        } else {
            format!("svc-{}", uuid::Uuid::new_v4())
        }
    };
    if let Some(c) = request.extensions_mut().get_mut::<RequestContext>() {
        c.request_id = request_id.clone();
    }
    let context = OutboundContext {
        request_id,
        trace_carrier: request
            .extensions()
            .get::<ActiveTraceCarrier>()
            .map(|c| c.0.clone())
            .or_else(|| {
                request
                    .extensions()
                    .get::<ai_platform_gateway_foundation::distributed_tracing::TraceCarrier>()
                    .map(|c| c.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            })
            .unwrap_or_default(),
    };
    let mut response = scoped(context.clone(), next.run(request)).await;
    if let Ok(value) = axum::http::HeaderValue::from_str(&context.request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, Body::new(ContextBody { body, context }))
}
