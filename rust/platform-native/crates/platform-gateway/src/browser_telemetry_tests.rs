use super::*;
use crate::migration_content::{ContentError, ContentFuture};
use axum::{body::Body, http::Request};
use tower::ServiceExt;

struct FixedIdentity(ContentIdentity);
impl ContentAccess for FixedIdentity {
    fn resolve<'a>(&'a self, _: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity> {
        Box::pin(async { Ok(self.0.clone()) })
    }
    fn require_capability<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn rate_limit<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn authorize_session<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
    ) -> ContentFuture<'a, serde_json::Value> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn authorize_artifact_source<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a crate::migration_content::artifacts::Artifact,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn require_vision_model<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
}
fn principal(tenant: &str) -> ContentIdentity {
    ContentIdentity {
        user_id: "verified-user".into(),
        tenant_id: tenant.into(),
        is_authenticated: true,
        tier: "normal".into(),
        roles: vec!["user".into()],
        permissions: vec![],
    }
}
fn api(identity: ContentIdentity) -> BrowserTelemetryApi {
    BrowserTelemetryApi::new(
        Arc::new(FixedIdentity(identity)),
        vec!["tenant-a".into()],
        vec!["https://console.example".into()],
        100,
    )
    .unwrap()
}
fn record() -> serde_json::Value {
    json!({"kind":"metric","name":"INP","route":"/assistant","value":120})
}
fn session(api: &BrowserTelemetryApi, identity: &ContentIdentity, sampled: bool) -> String {
    let id = "0123456789abcdef0123456789abcdef".to_owned();
    api.state.lock().unwrap().sessions.insert(
        id.clone(),
        SampleSession {
            authority: authority(identity),
            sampled,
            created: Instant::now(),
        },
    );
    id
}
fn post_request(id: &str, records: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(PATH)
        .header("origin", "https://console.example")
        .header("sec-fetch-site", "same-origin")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"session_id":id,"records":records}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn collector_accepts_sampled_records_for_verified_authority_only() {
    let identity = principal("tenant-a");
    let api = api(identity.clone());
    let id = session(&api, &identity, true);
    let response = router(api)
        .oneshot(post_request(&id, json!([record()])))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}
#[tokio::test]
async fn client_tenant_and_unknown_raw_error_fields_are_rejected() {
    let identity = principal("tenant-a");
    let api = api(identity.clone());
    let id = session(&api, &identity, true);
    let mut forged = record();
    forged["tenant_id"] = json!("tenant-b");
    assert_eq!(
        router(api.clone())
            .oneshot(post_request(&id, json!([forged])))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let raw = json!({"kind":"error","name":"Error","route":"/assistant","frames":[],"message":"PRIVATE PROMPT"});
    assert_eq!(
        router(api)
            .oneshot(post_request(&id, json!([raw])))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn missing_sample_and_foreign_origin_fail_closed() {
    let identity = principal("tenant-a");
    let api = api(identity.clone());
    let id = session(&api, &identity, false);
    assert_eq!(
        router(api.clone())
            .oneshot(post_request(&id, json!([record()])))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    session(&api, &identity, true);
    let mut request = post_request(&id, json!([record()]));
    request
        .headers_mut()
        .insert("origin", "https://outside.example".parse().unwrap());
    assert_eq!(
        router(api).oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn verified_tenant_and_permission_changes_cannot_reuse_a_sampling_session() {
    let original = principal("tenant-a");
    let mut changed = original.clone();
    changed.permissions.push("new-permission".into());
    let changed_api = api(changed);
    let id = session(&changed_api, &original, true);
    assert_eq!(
        router(changed_api)
            .oneshot(post_request(&id, json!([record()])))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let foreign = api(principal("tenant-b"));
    let request = Request::builder()
        .uri("/api/v1/telemetry/browser/config")
        .header("x-ai-tenant-id", "tenant-a")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router(foreign).oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn collector_bounds_body_batch_and_per_authority_rate() {
    let identity = principal("tenant-a");
    let api = api(identity.clone());
    let id = session(&api, &identity, true);
    assert_eq!(
        router(api.clone())
            .oneshot(post_request(&id, json!(vec![record(); 21])))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    for _ in 0..3 {
        assert_eq!(
            router(api.clone())
                .oneshot(post_request(&id, json!(vec![record(); 20])))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(
        router(api.clone())
            .oneshot(post_request(&id, json!([record()])))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let oversized = Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .body(Body::from("x".repeat(MAX_BODY + 1)))
        .unwrap();
    assert_eq!(
        router(api).oneshot(oversized).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
#[test]
fn strict_schema_excludes_queries_dynamic_routes_and_arbitrary_locations() {
    for value in [
        json!({"kind":"metric","name":"prompt","route":"/assistant","value":1}),
        json!({"kind":"metric","name":"LCP","route":"/knowledge/private?token=secret","value":1}),
        json!({"kind":"error","name":"Error","route":"/assistant","frames":[{"asset":"https://outside.example/secret.js","line":1,"column":2}]}),
    ] {
        let record: BrowserRecord = serde_json::from_value(value).unwrap();
        assert!(!record.valid("0123456789abcdef0123456789abcdef"));
    }
}
