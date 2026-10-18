//! Request-id context bridge and safe internal HTTP correlation propagation.
use super::middleware::RequestContext;
use ai_platform_capability_contract::private_transport::{Budget, Trace};
use axum::{
    body::{Body, Bytes},
    extract::Request,
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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
tokio::task_local! {static OUTBOUND:OutboundContext; static COMMAND_BUDGET:Option<Budget>;}
pub fn current() -> OutboundContext {
    OUTBOUND.try_with(Clone::clone).unwrap_or_default()
}
pub fn scoped<T>(
    context: OutboundContext,
    future: impl Future<Output = T>,
) -> impl Future<Output = T> {
    let budget = COMMAND_BUDGET.try_with(Clone::clone).unwrap_or_default();
    scoped_with_budget(context, budget, future)
}
/// Explicit capture for a same-request detached command; never read public headers.
pub fn capture_budget() -> Option<Budget> {
    COMMAND_BUDGET.try_with(Clone::clone).unwrap_or_default()
}
pub fn scoped_with_budget<T>(
    context: OutboundContext,
    budget: Option<Budget>,
    future: impl Future<Output = T>,
) -> impl Future<Output = T> {
    let policy = ai_platform_auth::cedar::resources::capture();
    async move {
        ai_platform_auth::cedar::resources::scoped(
            policy,
            COMMAND_BUDGET.scope(budget, OUTBOUND.scope(context, future)),
        )
        .await
    }
}
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
/// Existing local route cap, not a verified model/SDK lease expiry. An optional
/// privately installed Budget may only shorten it. Raw HTTP budget headers never enter.
pub fn command_budget(cap: Duration) -> Result<Budget, String> {
    let now = Instant::now();
    let wall = wall_ms();
    let local = Budget::admit(
        cap,
        wall.saturating_add(cap.as_millis().min(u64::MAX as u128) as u64),
        wall,
        now,
    )
    .map_err(|_| "private_transport_budget_invalid")?;
    if let Some(parent) = capture_budget() {
        let mut headers = vec![];
        parent
            .append(&mut headers, wall, now)
            .map_err(|_| "private_transport_budget_expired")?;
        return local
            .constrain_authenticated(&headers, wall, now)
            .map_err(|_| "private_transport_budget_invalid".into());
    }
    Ok(local)
}
fn safe_id(value: &str, limit: usize, colon: bool) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') || colon && b == b':'
        })
}
/// Capture only bounded correlation for one request's detached work. This is
/// not an identity carrier: no actor, authorization, cookies or baggage survive.
/// An absent tracing adapter remains absent; never use raw incoming headers.
pub fn capture() -> OutboundContext {
    let context = current();
    let request_id = context.request_id.trim();
    let request_id = if safe_id(request_id, 64, false) {
        request_id.to_owned()
    } else {
        format!("svc-{}", uuid::Uuid::new_v4())
    };
    let incoming = context.trace_carrier.into_iter().collect::<Vec<_>>();
    let mut carrier = vec![];
    Trace::from_admitted_carrier(&incoming).append(&mut carrier);
    let trace_carrier = carrier.into_iter().collect();
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
    let context = capture();
    let mut transport = existing.into_iter().collect::<Vec<_>>();
    Trace::from_admitted_carrier(
        &context
            .trace_carrier
            .clone()
            .into_iter()
            .collect::<Vec<_>>(),
    )
    .append(&mut transport);
    existing = transport.into_iter().collect();
    let mut lowered: BTreeSet<String> = existing.keys().map(|k| k.to_lowercase()).collect();
    let request_id = context.request_id;
    if lowered.insert("x-request-id".into()) {
        existing.insert("x-request-id".into(), request_id);
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
    budget: Option<Budget>,
}
impl HttpBody for ContextBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        COMMAND_BUDGET.sync_scope(this.budget.clone(), || {
            OUTBOUND.sync_scope(this.context.clone(), || {
                Pin::new(&mut this.body).poll_frame(cx)
            })
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
    // Only a trusted in-process auth/admission adapter can install this extension.
    let budget = request.extensions().get::<Budget>().cloned();
    let mut response = scoped_with_budget(context.clone(), budget.clone(), next.run(request)).await;
    if let Ok(value) = axum::http::HeaderValue::from_str(&context.request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(ContextBody {
            body,
            context,
            budget,
        }),
    )
}

#[cfg(test)]
#[path = "context_budget_tests.rs"]
mod budget_tests;
