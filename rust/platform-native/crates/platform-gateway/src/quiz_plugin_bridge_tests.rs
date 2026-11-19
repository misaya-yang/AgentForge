use super::*;
use crate::artifact_shares_api::ArtifactSharesFuture;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

struct Identity(&'static str);
impl ArtifactSharesIdentityResolver for Identity {
    fn resolve<'a>(
        &'a self,
        _: &'a axum::http::HeaderMap,
    ) -> ArtifactSharesFuture<'a, ArtifactSharesIdentity> {
        Box::pin(async move {
            Ok(ArtifactSharesIdentity {
                user_id: "u".into(),
                tenant_id: Some(self.0.into()),
                is_authenticated: true,
                ..Default::default()
            })
        })
    }
}
struct Sources(AtomicUsize);
impl ArtifactShareScope for Sources {
    fn quiz_sources<'a>(
        &'a self,
        _: &'a ArtifactSharesIdentity,
        _: &'a str,
    ) -> ArtifactSharesFuture<'a, session_history::SourceReferences> {
        Box::pin(async { panic!("public-only fallback must not replace authenticated provenance") })
    }
    fn quiz_sources_with_origin<'a>(
        &'a self,
        _: &'a ArtifactSharesIdentity,
        _: &'a str,
        public: bool,
    ) -> ArtifactSharesFuture<'a, session_history::SourceReferences> {
        Box::pin(async move {
            assert!(
                !public,
                "authenticated legacy reads retain their original scope"
            );
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ArtifactSharesError::detail(404, "fixture quiz absent"))
        })
    }
    fn require_active<'a>(&'a self, _: &'a ArtifactSharesIdentity) -> ArtifactSharesFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn allowed<'a>(
        &'a self,
        _: &'a ArtifactSharesIdentity,
        _: &'a session_history::SourceReferences,
    ) -> ArtifactSharesFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}
fn catalog() -> Value {
    serde_json::json!({"capabilities":[{"id":"generate_quiz","plugin_id":"ai-quiz","content_hash":format!("sha256:{}","a".repeat(64)),"plugin_metadata":{"artifact_sha256":format!("sha256:{}","a".repeat(64)),"worker_active":true,"publisher_id":"reviewed","publisher_tenants":"[\"tenant-a\"]","activation_id":uuid::Uuid::new_v4().to_string()}}]})
}
fn mounted(tenant: &'static str, sources: Arc<Sources>) -> axum::Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/fixture")
        .unwrap();
    let control = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/fixture")
        .unwrap();
    router(
        &catalog(),
        pool,
        control,
        sources,
        Arc::new(Identity(tenant)),
        Default::default(),
        None,
        true,
    )
    .unwrap()
}
#[tokio::test]
async fn real_mounted_route_denies_foreign_publisher_tenant_before_storage() {
    let sources = Arc::new(Sources(AtomicUsize::new(0)));
    for method in ["GET", "DELETE"] {
        let response = mounted("tenant-b", sources.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/v1/assistant/quiz/00000000-0000-0000-0000-000000000001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(sources.0.load(Ordering::SeqCst), 0);
    let response = mounted("tenant-a", sources.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/assistant/quiz/00000000-0000-0000-0000-000000000001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(sources.0.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn all_nine_quiz_routes_are_attached_once_with_original_method_guards() {
    let sources = Arc::new(Sources(AtomicUsize::new(0)));
    let router = mounted("tenant-a", sources.clone());
    for (_, path) in ai_platform_plugin_quiz_gateway::quiz_api::QUIZ_ROUTES {
        let path = path
            .replace("{quiz_id}", "00000000-0000-0000-0000-000000000001")
            .replace("{attempt_id}", "00000000-0000-0000-0000-000000000002")
            .replace("{share_code}", "fixture-code");
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("TRACE")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    assert_eq!(sources.0.load(Ordering::SeqCst), 0);
}
