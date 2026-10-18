//! Authenticated canonical /metrics endpoint; dependency detail remains private.
use super::metrics::MetricsCollector;
use crate::setup_api::SetupIdentityResolver;
use ai_platform_auth::rbac::{Capability, Rbac, check_capability, permission_denied_detail};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use std::sync::Arc;
#[derive(Clone)]
pub struct MetricsHttpState {
    pub identity: Arc<dyn SetupIdentityResolver>,
    pub rbac: Arc<Rbac>,
    pub collector: Arc<MetricsCollector>,
}
async fn metrics(State(state): State<MetricsHttpState>, request: Request) -> Response {
    let trace = request
        .extensions()
        .get::<super::middleware::RequestContext>()
        .map(|c| c.trace_id.as_str())
        .unwrap_or("");
    let auth = match state
        .identity
        .resolve(request.headers(), request.uri().path())
        .await
    {
        Ok(auth) => auth,
        Err(error) => {
            return (
                StatusCode::from_u16(error.status().unwrap_or(500))
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(
                    error
                        .response_body(trace)
                        .unwrap_or(json!({"detail":"Request cancelled"})),
                ),
            )
                .into_response();
        }
    };
    let capability = Capability::GatewayMetricsRead;
    if !check_capability(&state.rbac, &auth.roles, &auth.permissions, capability).allowed() {
        state.identity.permission_denied(&auth, trace);
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail":permission_denied_detail(capability,trace,None)})),
        )
            .into_response();
    }
    (
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{}\n# HELP gateway_worker_fence_rejections_total Rejected singleton side-effect attempts\n# TYPE gateway_worker_fence_rejections_total counter\ngateway_worker_fence_rejections_total {}\n",
            state.collector.to_prometheus(),
            ai_platform_gateway_foundation::worker_lease::fence_rejections()),
    )
        .into_response()
}
pub fn router(state: MetricsHttpState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .with_state(state)
}
