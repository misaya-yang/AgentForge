//! Bounded enrichment of Gateway-owned JSON errors. Never buffer streams or
//! rewrite public provider wire protocols, OAuth errors, JSON-RPC or successes.
use ai_platform_service_primitives::problem::ProblemDetails;
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{
        HeaderValue,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    middleware::Next,
    response::Response,
};
use http_body::Body as _;
const LIMIT: usize = 64 * 1024;
pub async fn enrich(request: Request, next: Next) -> Response {
    let context = request
        .extensions()
        .get::<super::middleware::RequestContext>()
        .cloned();
    let provider_wire = request.uri().path().starts_with("/v1/")
        || request.uri().path().starts_with("/api/v1/proxy/")
        || request.uri().path() == "/api/v1/responses"
        || request.uri().path().starts_with("/api/v1/langgraph/")
        || request.uri().path().starts_with("/internal/")
        || request.uri().path().starts_with("/mcp")
        || request.uri().path().starts_with("/a2a/")
        || request.uri().path() == "/api/v1/invoke"
        || request.uri().path() == "/api/v1/stream";
    let response = next.run(request).await;
    if provider_wire
        || !response.status().is_client_error() && !response.status().is_server_error()
        || response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_none_or(|h| h.split(';').next() != Some("application/json"))
        || response
            .body()
            .size_hint()
            .upper()
            .is_none_or(|size| size > LIMIT as u64)
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            parts.headers.remove(CONTENT_LENGTH);
            return Response::from_parts(parts, Body::empty());
        }
    };
    let problem = serde_json::from_slice(&bytes)
        .ok()
        .and_then(|value| ProblemDetails::from_legacy(parts.status, value));
    let Some(mut problem) = problem else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if let Some(context) = context {
        problem
            .extensions
            .entry("trace_id")
            .or_insert(serde_json::json!(context.trace_id));
        if context.request_id.len() <= 128
            && context
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            problem.instance = Some(format!("urn:agentforge:request:{}", context.request_id));
        }
    }
    let strict = problem.strict();
    let Ok(bytes) = serde_json::to_vec(&problem) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    parts.headers.remove(CONTENT_LENGTH);
    if strict {
        parts.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
    }
    Response::from_parts(parts, Body::from(bytes))
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{Request, StatusCode},
        routing::get,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;
    #[tokio::test]
    async fn errors_keep_status_detail_retry_and_auth_headers() {
        let router = Router::new()
            .route(
                "/api/v1/test",
                get(|| async {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "4"), ("www-authenticate", "Bearer")],
                        Json(json!({"detail":"Wait"})),
                    )
                }),
            )
            .layer(axum::middleware::from_fn(enrich));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 429);
        assert_eq!(response.headers()["retry-after"], "4");
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), LIMIT).await.unwrap()).unwrap();
        assert_eq!(body["detail"], "Wait");
        assert_eq!(body["status"], 429);
    }
    #[tokio::test]
    async fn provider_wire_and_structured_detail_preserve_contracts() {
        for path in [
            "/v1/responses",
            "/api/v1/responses",
            "/api/v1/langgraph/test",
            "/api/v1/admin",
        ] {
            let original = json!({"detail":{"message":"Denied","required_permission":"test"}});
            let expected = original.clone();
            let router = Router::new()
                .route(
                    path,
                    get(move || {
                        let body = original.clone();
                        async move { (StatusCode::FORBIDDEN, Json(body)) }
                    }),
                )
                .layer(axum::middleware::from_fn(enrich));
            let response = router
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), LIMIT).await.unwrap())
                    .unwrap();
            assert_eq!(body["detail"], expected["detail"]);
            if path != "/api/v1/admin" {
                assert_eq!(body, expected);
            } else {
                assert_eq!(body["status"], 403);
            }
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    use axum::{Json, Router, middleware::from_fn_with_state, routing::get};
    use std::sync::Arc;
    use tower::ServiceExt;
    #[tokio::test]
    async fn enrichment_reads_the_host_generated_correlation_context() {
        let state = crate::migration_host::middleware::MiddlewareState {
            config: Arc::new(Default::default()),
            drain: Arc::new(Default::default()),
        };
        let router = Router::new()
            .route(
                "/error",
                get(|| async {
                    (
                        axum::http::StatusCode::CONFLICT,
                        Json(serde_json::json!({"detail":"conflict"})),
                    )
                }),
            )
            .layer(axum::middleware::from_fn(enrich))
            .layer(from_fn_with_state(
                state,
                crate::migration_host::middleware::tracing_context,
            ));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/error")
                    .header("x-request-id", "problem-context-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let trace = response
            .headers()
            .get("x-trace-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let request = "problem-context-test";
        let bytes = to_bytes(response.into_body(), LIMIT).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["trace_id"], trace);
        assert_eq!(
            value["instance"],
            format!("urn:agentforge:request:{request}")
        );
    }
}
