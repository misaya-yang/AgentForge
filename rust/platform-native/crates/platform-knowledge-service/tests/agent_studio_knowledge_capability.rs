//! Exact externally observable Knowledge capability-plane source cases.
use ai_platform_knowledge_service::{
    migration_content::{capabilities::canonical_json, sha256},
    migration_knowledge_query::{
        actor::{CapabilityBackend, CapabilityFuture, CapabilityState, capability_router},
        config::RetrieveRequest,
        types::{QueryError, RetrieveResult, UserContext},
    },
};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
struct Backend {
    calls: Mutex<Vec<String>>,
    denied: bool,
    image: bool,
}
impl CapabilityBackend for Backend {
    fn actor<'a>(&'a self, t: &'a str, u: &'a str) -> CapabilityFuture<'a, UserContext> {
        Box::pin(async move {
            assert_eq!((t, u), ("tenant-a", "user-a"));
            self.calls.lock().unwrap().push("actor".into());
            Ok(UserContext {
                user_id: u.into(),
                tenant_id: t.into(),
                user_tier: "admin".into(),
                roles: vec!["admin".into(), "platform_admin".into()],
                is_authenticated: true,
                user_type: "runtime".into(),
            })
        })
    }
    fn require_dataset<'a>(&'a self, u: &'a UserContext, id: &'a str) -> CapabilityFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(id, "ds-a");
            assert_eq!(u.user_tier, "admin");
            assert_eq!(u.roles, vec!["admin", "platform_admin"]);
            self.calls.lock().unwrap().push("acl".into());
            if self.denied {
                Err(QueryError::NotFound)
            } else {
                Ok(())
            }
        })
    }
    fn retrieve<'a>(
        &'a self,
        u: &'a UserContext,
        id: &'a str,
        r: RetrieveRequest,
    ) -> CapabilityFuture<'a, (Vec<RetrieveResult>, Value)> {
        Box::pin(async move {
            assert_eq!(u.user_id, "user-a");
            assert_eq!(id, "ds-a");
            assert_eq!(r.query, "hello");
            assert_eq!(r.top_k, 3);
            assert_eq!(r.score_threshold, Some(0.4));
            self.calls.lock().unwrap().push("retrieve".into());
            Ok((
                vec![RetrieveResult {
                    segment_id: "seg-a".into(),
                    document_id: "doc-a".into(),
                    score: 0.9,
                    text: "answer".into(),
                    metadata: json!({"source_type":"upload"}),
                    content_type: if self.image { "image" } else { "text" }.into(),
                    image_url: None,
                    vlm_description: None,
                    associated_images: vec![],
                }],
                json!({"retrieved_count":1}),
            ))
        })
    }
}
fn body() -> Value {
    json!({"query":"hello","top_k":3,"threshold":0.4})
}
fn backend(denied: bool, image: bool) -> Arc<Backend> {
    Arc::new(Backend {
        calls: Mutex::new(vec![]),
        denied,
        image,
    })
}
fn app(b: Arc<Backend>) -> axum::Router {
    capability_router(
        CapabilityState::with_backend(b, "internal-token".into(), vec![b'p'; 32])
            .with_legacy_window((chrono::Utc::now().timestamp_millis() + 120000) as u64),
    )
}
fn proof() -> String {
    let mut p = json!({"schema_version":"ai-platform-capability-proof/v1","method":"POST","path":"/internal/v2/capabilities/knowledge/ds-a/retrieve","body_sha256":sha256(&canonical_json(&body()).unwrap()),"tenant_id":"tenant-a","user_id":"user-a","session_id":"session-a","execution_id":"11111111-1111-4111-8111-111111111111","run_id":"run-a","expires_at":chrono::Utc::now().timestamp()+60,"nonce":"nonce-a"});
    let mut mac = Hmac::<Sha256>::new_from_slice(&[b'p'; 32]).unwrap();
    mac.update(&canonical_json(&p).unwrap());
    p["signature"] = json!(
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(canonical_json(&p).unwrap())
    )
}
async fn request(
    app: axum::Router,
    body: Value,
    token: &str,
    id: &str,
    tenant: &str,
) -> (u16, Value) {
    let mut r = Request::builder()
        .method("POST")
        .uri(format!("/internal/v2/capabilities/knowledge/{id}/retrieve"))
        .header("content-type", "application/json");
    for (k, v) in [
        ("x-ai-platform-internal-token", token),
        ("x-ai-tenant-id", tenant),
        ("x-ai-user-id", "user-a"),
        ("x-ai-session-id", "session-a"),
        ("x-ai-execution-id", "11111111-1111-4111-8111-111111111111"),
        ("x-ai-run-id", "run-a"),
    ] {
        r = r.header(k, v)
    }
    let response = app
        .oneshot(
            r.header("x-ai-capability-proof", proof())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 100000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn missing_or_wrong_internal_token_fails_before_backend() {
    for token in ["", "wrong"] {
        let b = backend(false, false);
        assert_eq!(
            request(app(b.clone()), body(), token, "ds-a", "tenant-a")
                .await
                .0,
            401
        );
        assert!(b.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn route_resolves_current_actor_checks_acl_and_preserves_retrieval_shape() {
    let b = backend(false, false);
    let (status, result) =
        request(app(b.clone()), body(), "internal-token", "ds-a", "tenant-a").await;
    assert_eq!(status, 200);
    assert_eq!(result["results"][0]["text"], "answer");
    assert_eq!(result["metadata"], json!({"retrieved_count":1}));
    assert_eq!(result["results"][0]["content_type"], "text");
    assert_eq!(result["results"][0]["image_url"], Value::Null);
    assert_eq!(*b.calls.lock().unwrap(), vec!["actor", "acl", "retrieve"]);
}
#[tokio::test]
async fn acl_failure_does_not_reveal_dataset_existence() {
    let b = backend(true, false);
    let (status, result) =
        request(app(b.clone()), body(), "internal-token", "ds-a", "tenant-a").await;
    assert_eq!(status, 404);
    assert_eq!(result["detail"], "Dataset not found");
    assert_eq!(*b.calls.lock().unwrap(), vec!["actor", "acl"]);
}
#[tokio::test]
async fn non_text_result_is_rejected() {
    let (status, result) = request(
        app(backend(false, true)),
        body(),
        "internal-token",
        "ds-a",
        "tenant-a",
    )
    .await;
    assert_eq!(status, 400);
    assert!(result.get("results").is_none());
}
#[tokio::test]
async fn proof_binds_dataset_body_and_scope() {
    let mut changed = body();
    changed["query"] = json!("changed");
    for (payload, id, tenant) in [
        (changed, "ds-a", "tenant-a"),
        (body(), "ds-b", "tenant-a"),
        (body(), "ds-a", "tenant-b"),
    ] {
        let b = backend(false, false);
        assert_eq!(
            request(app(b.clone()), payload, "internal-token", id, tenant)
                .await
                .0,
            401
        );
        assert!(b.calls.lock().unwrap().is_empty());
    }
}
