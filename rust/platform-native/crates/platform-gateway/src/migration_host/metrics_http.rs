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
    pub scrape_token: Option<Arc<String>>,
}
fn scrape_token_authorized(headers: &axum::http::HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let mut values = headers.get_all(axum::http::header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let Some(presented) = value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    presented.len() == expected.len()
        && ring::constant_time::verify_slices_are_equal(presented.as_bytes(), expected.as_bytes())
            .is_ok()
}
async fn metrics(State(state): State<MetricsHttpState>, request: Request) -> Response {
    let trace = request
        .extensions()
        .get::<super::middleware::RequestContext>()
        .map(|c| c.trace_id.as_str())
        .unwrap_or("");
    if !scrape_token_authorized(
        request.headers(),
        state.scrape_token.as_deref().map(String::as_str),
    ) {
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
    }
    (
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        state.collector.to_prometheus(),
    )
        .into_response()
}
pub fn router(state: MetricsHttpState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_api::{HealthApiError, HealthApiFuture};
    use crate::setup_api::SetupIdentity;
    use axum::{body::Body, http::Request};
    use std::collections::HashMap;
    use tower::ServiceExt;

    struct UnusedIdentity;
    impl SetupIdentityResolver for UnusedIdentity {
        fn resolve<'a>(
            &'a self,
            _: &'a axum::http::HeaderMap,
            _: &'a str,
        ) -> HealthApiFuture<'a, SetupIdentity> {
            Box::pin(async { Err(HealthApiError::Internal) })
        }
    }

    #[tokio::test]
    async fn configured_scrape_token_opens_prometheus_endpoint_without_user_rbac() {
        let app = router(MetricsHttpState {
            identity: Arc::new(UnusedIdentity),
            rbac: Arc::new(Rbac::new(HashMap::new())),
            collector: Arc::new(MetricsCollector::new()),
            scrape_token: Some(Arc::new(
                "synthetic-prometheus-token-0123456789abcdef".into(),
            )),
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header(
                        axum::http::header::AUTHORIZATION,
                        "Bearer synthetic-prometheus-token-0123456789abcdef",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("# HELP gateway_requests_total"));
    }
}
