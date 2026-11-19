//! Knowledge process binding. Signature authority encloses response idempotency;
//! capability proof routes keep their independent authorization and no response cache.
use super::{KnowledgeError, KnowledgeGraph, Result, lifecycle::KnowledgeLifecycle};
use crate::{
    host::{cors, idempotency, middleware},
    migration_knowledge_query::auth,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use http_body::{Body as HttpBody, Frame};
use serde_json::json;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

#[derive(Clone)]
struct Boundary {
    auth: Arc<auth::GatewayAuth>,
    idempotency: Arc<idempotency::IdempotencyConfig>,
}
fn exempt(path: &str) -> bool {
    [
        "/health",
        "/health/live",
        "/health/ready",
        "/metrics",
        "/version",
    ]
    .contains(&path)
        || path.starts_with("/internal/v2/capabilities/")
}
async fn signed(State(boundary): State<Boundary>, request: Request, next: Next) -> Response {
    if exempt(request.uri().path()) {
        next.run(request).await
    } else {
        auth::verify_gateway_signature(State(boundary.auth), request, next).await
    }
}
async fn cached(State(boundary): State<Boundary>, request: Request, next: Next) -> Response {
    if exempt(request.uri().path())
        || request.uri().path() == ai_platform_service_primitives::knowledge_model::SCOPE_CHECK_PATH
    {
        next.run(request).await
    } else {
        idempotency::middleware(State((*boundary.idempotency).clone()), request, next).await
    }
}
// Keep drain ownership until the final body frame or cancellation, including SSE.
struct DrainingBody {
    body: Body,
    _permit: Option<middleware::DrainPermit>,
}
impl HttpBody for DrainingBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(result, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self._permit.take();
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}
async fn drain(
    State(lifecycle): State<Arc<KnowledgeLifecycle>>,
    request: Request,
    next: Next,
) -> Response {
    if request.uri().path().starts_with("/health") || request.uri().path() == "/metrics" {
        return next.run(request).await;
    }
    let Some(permit) = lifecycle.drain.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", "5")],
            Json(json!({"detail":"shutting down"})),
        )
            .into_response();
    };
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(DrainingBody {
            body,
            _permit: Some(permit),
        }),
    )
}
async fn ready(State(lifecycle): State<Arc<KnowledgeLifecycle>>) -> Response {
    let snapshot = lifecycle.snapshot().await;
    let ready = snapshot["core_ready"] == true;
    (if ready {StatusCode::OK}else{StatusCode::SERVICE_UNAVAILABLE},Json(json!({"status":if ready {"ready"}else{"not_ready"},"service":"knowledge-service","checks":{"core":if ready {"healthy"}else{"unavailable"}},"capabilities":snapshot["capabilities"]}))).into_response()
}
pub(super) fn router(graph: KnowledgeGraph) -> Result<Router> {
    let api_enabled = graph.application.is_some();
    let metrics = super::metrics::Metrics::new(graph.lifecycle.pool.clone());
    let release = graph.release_identity.clone();
    let routes = graph
        .application
        .map(|app| {
            app.into_router()
                .map_err(|_| KnowledgeError::Construction("knowledge_router"))
        })
        .transpose()?
        .unwrap_or_default();
    let mut probes = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"status":"ok","service":"knowledge-service"})) }),
        )
        .route("/health/ready", get(ready))
        .route(
            "/health/live",
            get(|| async { Json(json!({"status":"alive","service":"knowledge-service"})) }),
        )
        .with_state(graph.lifecycle.clone())
        .merge(
            Router::new()
                .route("/metrics", get(super::metrics::scrape))
                .with_state(metrics),
        );
    if api_enabled {
        probes = probes.merge(crate::host::release::router(release));
    }
    let state = middleware::MiddlewareState {
        config: Arc::new(middleware::MiddlewareConfig {
            anonymous_enabled: false,
            body_limit: graph.maximum_upload_bytes,
            ..Default::default()
        }),
        drain: graph.lifecycle.drain.clone(),
    };
    let boundary = Boundary {
        auth: graph.service_auth,
        idempotency: graph.idempotency,
    };
    let workload = graph.workload.clone();
    let router = routes
        .merge(probes)
        .layer(axum::middleware::from_fn_with_state(
            graph.cors,
            cors::apply,
        ))
        .layer(axum::middleware::from_fn_with_state(
            graph.request_governance,
            ai_platform_service_primitives::request_governance::govern,
        ))
        .layer(axum::middleware::from_fn(crate::tracing::inbound))
        .layer(axum::middleware::from_fn_with_state(graph.lifecycle, drain))
        .layer(axum::middleware::from_fn_with_state(
            state,
            middleware::request_context,
        ))
        .layer(axum::middleware::from_fn_with_state(
            boundary.clone(),
            cached,
        ))
        .layer(axum::middleware::from_fn_with_state(boundary, signed))
        .layer(axum::middleware::from_fn(security_headers));
    Ok(match workload {
        Some(config) => super::workload::protect(router, config),
        None => router,
    })
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("x-frame-options", "DENY".parse().unwrap());
    headers.insert(
        "strict-transport-security",
        "max-age=31536000; includeSubDomains".parse().unwrap(),
    );
    response
}
