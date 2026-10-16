use ai_platform_knowledge_service::{
    KnowledgeBackends, KnowledgeConnections, KnowledgeGraph, process::ProcessSettings,
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tower::ServiceExt;
fn settings(role: &str) -> ProcessSettings {
    ProcessSettings::from_values(
        BTreeMap::from([
            ("ENVIRONMENT".into(), "test".into()),
            (
                "AI_PLATFORM_INTERNAL_TOKEN".into(),
                "offline-test-signing-material".into(),
            ),
            (
                "AI_PLATFORM_CAPABILITY_PROOF_SECRET".into(),
                "offline-test-capability-material".into(),
            ),
            ("INTERNAL_COMM_STATE_BACKEND".into(), "memory".into()),
            ("INTERNAL_IDEMPOTENCY_BACKEND".into(), "memory".into()),
            ("KNOWLEDGE_RUNTIME_ROLE".into(), role.into()),
        ]),
        std::path::PathBuf::from("/tmp/knowledge-process-contract"),
    )
    .unwrap()
}
async fn router(role: &str) -> axum::Router {
    let settings = settings(role);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(10))
        .connect_lazy("postgres://localhost/unused")
        .unwrap();
    KnowledgeGraph::new(
        KnowledgeConnections {
            database: pool,
            replay_redis: None,
            idempotency_redis: None,
        },
        settings.configuration,
        KnowledgeBackends::default(),
    )
    .unwrap()
    .into_router()
    .unwrap()
}
#[tokio::test]
async fn health_is_liveness_not_database_readiness() {
    let response = router("api")
        .await
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body, json!({"status":"ok","service":"knowledge-service"}));
}
#[tokio::test]
async fn unavailable_release_identity_is_explicit_and_public() {
    let response = router("api")
        .await
        .oneshot(
            Request::builder()
                .uri("/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["code"], "RELEASE_MANIFEST_UNAVAILABLE");
    assert_eq!(body["service_id"], "knowledge-service");
}
#[tokio::test]
async fn worker_does_not_publish_http_release_identity() {
    let response = router("worker")
        .await
        .oneshot(
            Request::builder()
                .uri("/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
#[tokio::test]
async fn confluence_routes_reject_unsigned_requests() {
    for path in [
        "/api/v1/knowledge/d/sources/confluence/connections",
        "/api/v1/knowledge/d/sources/confluence/bindings/00000000-0000-0000-0000-000000000000",
        "/api/v1/knowledge/d/sources/confluence/b/sync/00000000-0000-0000-0000-000000000000",
    ] {
        let response = router("api")
            .await
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}
#[test]
fn explicit_source_feature_setting_is_preserved() {
    let mut values = BTreeMap::new();
    values.insert(
        "KNOWLEDGE_CONFLUENCE__MANUAL_SYNC_ENABLED".into(),
        "false".into(),
    );
    let settings =
        ProcessSettings::from_values(values, "/tmp/native-knowledge-test".into()).unwrap();
    assert!(!settings.configuration.confluence_enabled);
}

#[test]
fn native_provider_tuning_uses_source_environment_names() {
    let values = BTreeMap::from([
        ("KNOWLEDGE_EMBEDDINGS__BATCH_SIZE".into(), "7".into()),
        ("KNOWLEDGE_QDRANT__MAX_RETRIES".into(), "2".into()),
        ("KNOWLEDGE_QDRANT__RETRY_BASE_DELAY".into(), "0.25".into()),
        ("KNOWLEDGE_METADATA_LLM__ENABLED".into(), "true".into()),
        ("KB_PDF_SPLIT_PAGES_PER_PART".into(), "20".into()),
    ]);
    let settings =
        ProcessSettings::from_values(values, "/tmp/native-knowledge-tuning".into()).unwrap();
    assert_eq!(settings.configuration.embeddings.batch_size, 7);
    assert_eq!(settings.configuration.qdrant.max_retries, 2);
    assert_eq!(settings.configuration.qdrant.retry_base_delay, 0.25);
    assert_eq!(
        settings.configuration.processing.pdf_upload.pages_per_part,
        20
    );
}
