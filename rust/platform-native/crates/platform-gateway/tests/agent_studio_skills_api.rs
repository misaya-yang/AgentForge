//! Skill body semantics shared with the production HTTP handlers.
use ai_platform_gateway::migration_content::skills::{skill_test_input, skill_update_payload};
use serde_json::json;
#[test]
fn skill_test_accepts_empty_string_but_requires_string_input() {
    assert_eq!(skill_test_input(&json!({"input":""})).unwrap(), "");
    assert_eq!(
        skill_test_input(&json!({"input":"hello","ignored":true})).unwrap(),
        "hello"
    );
    for body in [
        json!({}),
        json!({"input":null}),
        json!({"input":12}),
        json!([]),
    ] {
        assert!(skill_test_input(&body).is_err());
    }
}
#[test]
fn skill_update_preserves_omissions_and_nulls_and_ignores_extra_fields() {
    assert_eq!(skill_update_payload(&json!({})).unwrap(), json!({}));
    assert_eq!(
        skill_update_payload(&json!({"title":null,"enabled":null,"entrypoint":"malicious"}))
            .unwrap(),
        json!({"title":null,"enabled":null})
    );
    assert_eq!(
        skill_update_payload(&json!({"enabled":"false","tags":["one"],"permissions":[]})).unwrap(),
        json!({"enabled":false,"tags":["one"],"permissions":[]})
    );
}
#[test]
fn skill_update_rejects_wrong_metadata_shapes() {
    for body in [
        json!([]),
        json!({"title":1}),
        json!({"tags":"tag"}),
        json!({"permissions":[1]}),
        json!({"enabled":"maybe"}),
    ] {
        assert!(skill_update_payload(&body).is_err());
    }
}

use ai_platform_gateway::migration_content::{
    ContentAccess, ContentDatabase, ContentFuture, ContentIdentity,
    artifacts::Artifact,
    skills::{SkillRepository, SkillsApi, router},
};
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request},
};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;
struct Access;
impl ContentAccess for Access {
    fn resolve<'a>(&'a self, _: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity> {
        Box::pin(async {
            Ok(ContentIdentity {
                tenant_id: "tenant-a".into(),
                user_id: "user-a".into(),
                is_authenticated: true,
                ..Default::default()
            })
        })
    }
    fn require_capability<'a>(
        &'a self,
        _: &'a ContentIdentity,
        c: &'a str,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            assert!(["GatewaySkillRead", "GatewaySkillWrite"].contains(&c));
            Ok(())
        })
    }
    fn rate_limit<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn authorize_session<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
    ) -> ContentFuture<'a, Value> {
        panic!("skill routes must not authorize session")
    }
    fn authorize_artifact_source<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a Artifact,
    ) -> ContentFuture<'a, ()> {
        panic!("skill routes must not read artifacts")
    }
    fn require_vision_model<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        panic!("skill routes must not invoke models")
    }
}
fn offline_app() -> axum::Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    let control = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    router(Arc::new(SkillsApi::new(
        SkillRepository {
            database: ContentDatabase::new(pool, control).unwrap(),
        },
        Arc::new(Access),
    )))
}
async fn request(app: &axum::Router, method: &str, path: &str, body: Value) -> (u16, Value) {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status().as_u16();
    let bytes = to_bytes(r.into_body(), 100000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn builtin_creator_remains_readable_and_testable_without_storage_or_network() {
    let app = offline_app();
    let (status, detail) = request(&app, "GET", "/skills/skill-create", Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(detail["source"], "builtin");
    assert_eq!(detail["artifact_type"], "bundled");
    assert!(
        detail["instructions"]
            .as_str()
            .unwrap()
            .contains("propose-review-test-enable")
    );
    let (status, test) = request(
        &app,
        "POST",
        "/skills/skill-create/test",
        json!({"input":"release-note"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(test["success"], true);
    assert!(test["result"].as_str().unwrap().contains("release-note"));
}
#[tokio::test]
async fn builtin_creator_is_read_only_and_get_does_not_imply_head() {
    let app = offline_app();
    for (method, path, body) in [
        ("PATCH", "/skills/skill-create", json!({"title":"forged"})),
        ("DELETE", "/skills/skill-create", Value::Null),
        ("POST", "/skills/skill-create/enable", Value::Null),
        ("POST", "/skills/skill-create/disable", Value::Null),
    ] {
        assert_eq!(request(&app, method, path, body).await.0, 404);
    }
    assert_eq!(
        request(&app, "HEAD", "/skills/skill-create", Value::Null)
            .await
            .0,
        405
    );
}
