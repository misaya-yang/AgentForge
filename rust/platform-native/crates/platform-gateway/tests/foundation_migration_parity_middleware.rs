//! Observable HTTP ports for API version, CORS, request-size and streaming headers.
use ai_platform_gateway::migration_host::{cors, middleware as host, version};
use axum::{
    Extension, Json, Router,
    body::{Body, Bytes, to_bytes},
    http::Request,
    middleware,
    routing::{get, post},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;
#[tokio::test]
async fn vendor_version_and_deprecated_route_headers_are_observable() {
    let config = Arc::new(version::VersionConfig {
        deprecated_routes: std::collections::BTreeMap::from([(
            "/api/v1/legacy".into(),
            "2027-01-01".into(),
        )]),
        ..Default::default()
    });
    let app = Router::new()
        .route(
            "/api/v1/ping",
            get(|Extension(v): Extension<version::ApiVersion>| async move {
                Json(json!({"version":v.0}))
            }),
        )
        .route("/api/v1/legacy", get(|| async { Json(json!({"ok":true})) }))
        .layer(middleware::from_fn_with_state(config, version::negotiate));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/ping")
                .header("accept", "application/vnd.ai-gateway.v2+json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&to_bytes(response.into_body(), 1024).await.unwrap())
            .unwrap(),
        json!({"version":"v2"})
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/legacy")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["deprecation"], "true");
    assert_eq!(response.headers()["sunset"], "2027-01-01");
}
#[tokio::test]
async fn credentialed_cors_does_not_allow_unlisted_origin() {
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(
            Arc::new(cors::CorsConfig {
                allow_origins: vec!["http://localhost:3000".into()],
                allow_credentials: true,
                ..Default::default()
            }),
            cors::apply,
        ));
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/health")
                .header("origin", "http://evil.example")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-origin")
    );
}
#[tokio::test]
async fn request_body_limit_rejects_oversize_and_preserves_exact_boundary() {
    let state = host::MiddlewareState {
        config: Arc::new(host::MiddlewareConfig {
            body_limit: 8,
            ..Default::default()
        }),
        drain: Default::default(),
    };
    // Actual production guarded path replaces the source fixture's custom /chat path.
    let app = Router::new()
        .route(
            "/api/v1/assistant/chat",
            post(|body: Bytes| async move { Json(json!({"size":body.len()})) }),
        )
        .layer(middleware::from_fn_with_state(
            state,
            host::request_body_limit,
        ));
    for (length, status) in [(9, 413), (8, 200)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/assistant/chat")
                    .header("content-length", length.to_string())
                    .body(Body::from("123456789"[..length].to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        if status == 200 {
            assert_eq!(
                serde_json::from_slice::<Value>(
                    &to_bytes(response.into_body(), 1024).await.unwrap()
                )
                .unwrap(),
                json!({"size":8})
            );
        }
    }
}
#[tokio::test]
async fn security_headers_preserve_first_chunk_without_collecting_stream() {
    for path in ["/api/v1/assistant/chat/stream", "/v1/responses"] {
        let finished = Arc::new(AtomicBool::new(false));
        let state = finished.clone();
        let app = Router::new()
            .route(
                path,
                get(move || {
                    let finished = state.clone();
                    async move {
                        let first = stream::once(async {
                            Ok::<_, std::io::Error>(Bytes::from_static(b"chunk"))
                        });
                        let rest = stream::once(async move {
                            std::future::pending::<()>().await;
                            finished.store(true, Ordering::SeqCst);
                            Ok::<_, std::io::Error>(Bytes::new())
                        });
                        Body::from_stream(first.chain(rest))
                    }
                }),
            )
            .layer(middleware::from_fn(host::security_headers));
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["x-frame-options"], "DENY");
        let mut body = response.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_secs(1), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first, Bytes::from_static(b"chunk"));
        assert!(!finished.load(Ordering::SeqCst));
    }
}
