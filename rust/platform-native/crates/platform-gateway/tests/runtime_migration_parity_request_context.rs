//! Source assistant request-id middleware assertions on real in-process Axum layers.
use ai_platform_gateway::migration_host::{
    context,
    middleware::{self, DrainState, MiddlewareConfig, MiddlewareState, RequestContext},
};
use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    http::Request,
    routing::get,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
fn app() -> Router {
    Router::new().route("/probe",get(|Extension(state):Extension<RequestContext>|async move{Json(json!({"state_request_id":state.request_id,"ctx_request_id":context::current().request_id}))}))
        .layer(axum::middleware::from_fn(context::bridge))
        .layer(axum::middleware::from_fn_with_state(MiddlewareState{config:Arc::new(MiddlewareConfig{anonymous_enabled:false,..Default::default()}),drain:Arc::new(DrainState::default())},middleware::request_context))
}
#[tokio::test]
async fn valid_inbound_id_is_preserved_in_state_context_and_response_then_reset() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/probe")
                .header("x-request-id", "abc-123_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-request-id"], "abc-123_test");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["state_request_id"], "abc-123_test");
    assert_eq!(body["ctx_request_id"], "abc-123_test");
    assert!(context::current().request_id.is_empty());
}
#[tokio::test]
async fn missing_and_each_malformed_id_mint_valid_service_uuid_without_context_leak() {
    for inbound in [
        None,
        Some("x".repeat(65)),
        Some("has spaces".into()),
        Some("has;semi".into()),
        Some("has<script>".into()),
        Some("".into()),
    ] {
        let mut builder = Request::builder().uri("/probe");
        if let Some(value) = &inbound {
            builder = builder.header("x-request-id", value);
        }
        let response = app()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let header = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let id = body["state_request_id"].as_str().unwrap();
        assert!(id.starts_with("svc-"));
        uuid::Uuid::parse_str(&id[4..]).unwrap();
        assert_eq!(id, header);
        assert_eq!(body["ctx_request_id"], id);
        assert_ne!(Some(id), inbound.as_deref());
        assert!(context::current().request_id.is_empty());
    }
}
