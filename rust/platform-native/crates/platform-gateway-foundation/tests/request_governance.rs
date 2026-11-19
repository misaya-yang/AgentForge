use ai_platform_gateway_foundation::request_governance::{
    self, RequestCancellation, RequestGovernance, RequestGovernanceConfig,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::Extension,
    http::{Request, StatusCode},
    routing::get,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;
fn state(max: usize, header: Option<u64>, idle: Option<u64>) -> RequestGovernance {
    RequestGovernance::new(RequestGovernanceConfig::new(max, header, idle).unwrap())
}
fn mount(router: Router, state: RequestGovernance) -> Router {
    router.layer(axum::middleware::from_fn_with_state(
        state,
        request_governance::govern,
    ))
}
fn request(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}
#[tokio::test]
async fn overload_holds_permit_until_body_drop_and_preserves_health() {
    let state = state(1, None, None);
    let app = mount(
        Router::new()
            .route("/", get(|| async { "ok" }))
            .route("/health", get(|| async { "alive" })),
        state.clone(),
    );
    let first = app.clone().oneshot(request("/")).await.unwrap();
    assert_eq!(state.available_permits(), 0);
    let rejected = app.clone().oneshot(request("/")).await.unwrap();
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers()["retry-after"], "1");
    assert_eq!(
        app.clone()
            .oneshot(request("/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(first);
    assert_eq!(state.available_permits(), 1);
    assert_eq!(
        app.oneshot(request("/")).await.unwrap().status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn header_deadline_cancels_handler_and_releases_capacity() {
    let state = state(1, Some(10), None);
    let seen = Arc::new(Mutex::new(None));
    let capture = seen.clone();
    let app = mount(
        Router::new().route(
            "/",
            get(move |Extension(signal): Extension<RequestCancellation>| {
                let capture = capture.clone();
                async move {
                    *capture.lock().unwrap() = Some(signal);
                    std::future::pending::<&'static str>().await
                }
            }),
        ),
        state.clone(),
    );
    let response = app.oneshot(request("/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(seen.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(state.available_permits(), 1);
}
#[tokio::test]
async fn sse_has_idle_deadline_but_no_total_response_deadline() {
    let state = state(1, Some(10), Some(200));
    let app = mount(
        Router::new().route(
            "/",
            get(|| async {
                let stream = futures_util::stream::unfold(0, |n| async move {
                    if n == 3 {
                        None
                    } else {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Some((Ok::<_, std::io::Error>(format!("data: {n}\n\n")), n + 1))
                    }
                });
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("x-request-id", "existing"),
                    ],
                    Body::from_stream(stream),
                )
            }),
        ),
        state.clone(),
    );
    let response = app.oneshot(request("/")).await.unwrap();
    assert_eq!(response.headers()["x-request-id"], "existing");
    let data = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&data[..], b"data: 0\n\ndata: 1\n\ndata: 2\n\n");
    assert_eq!(state.available_permits(), 1);
}
#[tokio::test]
async fn idle_timeout_ends_stream_and_cancels_scope() {
    let state = state(1, Some(100), Some(10));
    let seen = Arc::new(Mutex::new(None));
    let capture = seen.clone();
    let app = mount(
        Router::new().route(
            "/",
            get(move |Extension(signal): Extension<RequestCancellation>| {
                let capture = capture.clone();
                async move {
                    *capture.lock().unwrap() = Some(signal);
                    Body::from_stream(futures_util::stream::pending::<
                        Result<String, std::io::Error>,
                    >())
                }
            }),
        ),
        state.clone(),
    );
    let response = app.oneshot(request("/")).await.unwrap();
    assert!(to_bytes(response.into_body(), 1024).await.is_err());
    assert!(seen.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(state.available_permits(), 1);
}
#[tokio::test]
async fn caller_drop_propagates_cancellation_without_collecting_body() {
    let state = state(1, None, None);
    let seen = Arc::new(Mutex::new(None));
    let capture = seen.clone();
    let app = mount(
        Router::new().route(
            "/",
            get(move |Extension(signal): Extension<RequestCancellation>| {
                let capture = capture.clone();
                async move {
                    *capture.lock().unwrap() = Some(signal);
                    "response"
                }
            }),
        ),
        state.clone(),
    );
    let response = app.oneshot(request("/")).await.unwrap();
    assert!(!seen.lock().unwrap().as_ref().unwrap().is_cancelled());
    drop(response);
    assert!(seen.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(state.available_permits(), 1);
}
#[test]
fn typed_config_rejects_invalid_limits_and_accepts_explicit_disabled_deadlines() {
    for value in [
        serde_json::json!({"max_concurrent_requests":0}),
        serde_json::json!({"response_header_timeout_ms":0}),
        serde_json::json!({"response_body_idle_timeout_ms":-1}),
        serde_json::json!({"unknown":1}),
    ] {
        assert!(serde_json::from_value::<RequestGovernanceConfig>(value).is_err());
    }
    assert!(serde_json::from_value::<RequestGovernanceConfig>(serde_json::json!({"response_header_timeout_ms":null,"response_body_idle_timeout_ms":null})).is_ok());
}

#[tokio::test]
async fn caller_drop_while_waiting_for_headers_cancels_actual_handler_scope() {
    let state = state(1, None, None);
    let seen = Arc::new(Mutex::new(None));
    let capture = seen.clone();
    let app = mount(
        Router::new().route(
            "/",
            get(move |Extension(signal): Extension<RequestCancellation>| {
                let capture = capture.clone();
                async move {
                    *capture.lock().unwrap() = Some(signal);
                    std::future::pending::<&'static str>().await
                }
            }),
        ),
        state.clone(),
    );
    let mut operation = Box::pin(app.oneshot(request("/")));
    // Poll once deterministically, like a live server waiting for response headers.
    assert!(futures_util::poll!(&mut operation).is_pending());
    assert_eq!(state.available_permits(), 0);
    assert!(!seen.lock().unwrap().as_ref().unwrap().is_cancelled());
    drop(operation);
    assert!(seen.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(state.available_permits(), 1);
}

#[tokio::test]
async fn auth_and_trace_context_are_forwarded_without_synthesizing_identity() {
    let state = state(2, None, None);
    let app = mount(
        Router::new().route(
            "/",
            get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer opaque-test");
                assert_eq!(
                    headers["traceparent"],
                    "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01"
                );
                (
                    StatusCode::FORBIDDEN,
                    [("x-request-id", "preserved")],
                    "domain denied",
                )
            }),
        ),
        state,
    );
    let mut req = request("/");
    req.headers_mut()
        .insert("authorization", "Bearer opaque-test".parse().unwrap());
    req.headers_mut().insert(
        "traceparent",
        "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01"
            .parse()
            .unwrap(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers()["x-request-id"], "preserved");
    assert_eq!(
        &to_bytes(response.into_body(), 1024).await.unwrap()[..],
        b"domain denied"
    );
}
