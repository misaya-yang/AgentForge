//! Internal correlation and inbound trace assertions, through actual context middleware.
use ai_platform_gateway::migration_host::{context, middleware as host};
use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    http::Request,
    middleware,
    routing::get,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tower::ServiceExt;
const PARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
#[tokio::test]
async fn internal_headers_preserve_safe_correlation_and_reset_after_request_scope() {
    let value = context::scoped(
        context::OutboundContext {
            request_id: "request-a".into(),
            trace_carrier: BTreeMap::from([
                ("traceparent".into(), PARENT.into()),
                ("tracestate".into(), "vendor=value".into()),
                (
                    "baggage".into(),
                    "ai-platform.request-class=interactive".into(),
                ),
            ]),
        },
        async {
            context::internal_http_headers(
                BTreeMap::from([
                    ("x-service-auth".into(), "opaque".into()),
                    (
                        "traceparent".into(),
                        "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01".into(),
                    ),
                    ("baggage".into(), "caller=untrusted".into()),
                ]),
                Some("run-a"),
                Some("turn-a"),
                Some("execution-a"),
            )
            .unwrap()
        },
    )
    .await;
    assert_eq!(
        value,
        BTreeMap::from([
            ("x-service-auth".into(), "opaque".into()),
            ("x-request-id".into(), "request-a".into()),
            ("traceparent".into(), PARENT.into()),
            ("tracestate".into(), "vendor=value".into()),
            ("x-ai-run-id".into(), "run-a".into()),
            ("x-ai-turn-id".into(), "turn-a".into()),
            ("x-ai-execution-id".into(), "execution-a".into())
        ])
    );
    assert!(!value.contains_key("baggage"));
    assert!(context::current().request_id.is_empty());
    assert!(
        context::internal_http_headers(BTreeMap::new(), Some("run-a\nsecret=value"), None, None)
            .unwrap_err()
            .contains("malformed")
    );
    let outside = context::internal_http_headers(BTreeMap::new(), None, None, None).unwrap();
    assert!(outside["x-request-id"].starts_with("svc-"));
    assert!(!outside["x-request-id"].contains(' '));
    let app = Router::new()
        .route(
            "/probe",
            get(|| async {
                Json(context::internal_http_headers(BTreeMap::new(), None, None, None).unwrap())
            }),
        )
        .layer(middleware::from_fn(context::bridge));
    let mut request = Request::builder()
        .uri("/probe")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(host::RequestContext {
        request_id: "gateway-request-a".into(),
        trace_id: String::new(),
        span_id: String::new(),
        traceparent: String::new(),
        anonymous_id: None,
    });
    let response = app.oneshot(request).await.unwrap();
    let payload: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(payload["x-request-id"], "gateway-request-a");
    assert!(context::current().request_id.is_empty());
}
#[tokio::test]
async fn tracing_middleware_reuses_valid_parent_and_replaces_unsafe_trace_id() {
    let state = host::MiddlewareState {
        config: Arc::new(host::MiddlewareConfig::default()),
        drain: Default::default(),
    };
    let app = Router::new()
        .route(
            "/probe",
            get(|Extension(c): Extension<host::RequestContext>| async move {
                Json(json!({"trace_id":c.trace_id,"span_id":c.span_id,"traceparent":c.traceparent}))
            }),
        )
        .layer(middleware::from_fn_with_state(state, host::tracing_context));
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let parent = format!("00-{trace_id}-00f067aa0ba902b7-01");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/probe")
                .header("traceparent", parent)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["traceparent"]
            .to_str()
            .unwrap()
            .starts_with(&format!("00-{trace_id}-"))
    );
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["trace_id"], trace_id);
    assert!(
        body["traceparent"]
            .as_str()
            .unwrap()
            .starts_with(&format!("00-{trace_id}-"))
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/probe")
                .header("x-trace-id", "bad<script>")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    let trace = body["trace_id"].as_str().unwrap();
    assert_ne!(trace, "bad<script>");
    assert_eq!(trace.len(), 32);
    assert!(trace.bytes().all(|c| c.is_ascii_hexdigit()));
}
