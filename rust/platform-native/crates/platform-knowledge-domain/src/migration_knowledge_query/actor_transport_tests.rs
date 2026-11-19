use super::*;
use axum::{body::Body, http::Request};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;
const PATH: &str = "/internal/v2/capabilities/knowledge/d/retrieve";
struct Backend {
    actors: AtomicUsize,
    calls: AtomicUsize,
    slow: bool,
    foreign: bool,
}
impl CapabilityBackend for Backend {
    fn actor<'a>(&'a self, _: &'a str, _: &'a str) -> CapabilityFuture<'a, UserContext> {
        self.actors.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(UserContext {
                tenant_id: if self.foreign { "other" } else { "t" }.into(),
                user_id: "u".into(),
                roles: vec!["user".into()],
                user_tier: "normal".into(),
                user_type: "user".into(),
                is_authenticated: true,
            })
        })
    }
    fn require_dataset<'a>(&'a self, _: &'a UserContext, _: &'a str) -> CapabilityFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn retrieve<'a>(
        &'a self,
        _: &'a UserContext,
        _: &'a str,
        _: RetrieveRequest,
    ) -> CapabilityFuture<'a, (Vec<super::super::types::RetrieveResult>, Value)> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let transport = super::super::private_transport::current()
                .expect("authenticated transport installed");
            let mut headers = Vec::new();
            transport.trace.append(&mut headers);
            assert!(
                headers
                    .iter()
                    .all(|(key, _)| key != "baggage" && key != "tracestate")
            );
            if self.slow {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok((vec![], json!({})))
        })
    }
}
fn proof(body: &Value) -> String {
    use crate::migration_content::capabilities::canonical_json;
    let hash = Sha256::digest(canonical_json(body).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut value = json!({"schema_version":"ai-platform-capability-proof/v1","method":"POST","path":PATH,"body_sha256":hash,"tenant_id":"t","user_id":"u","session_id":"s","execution_id":"e","run_id":"r","expires_at":chrono::Utc::now().timestamp()+30,"nonce":"transport-test"});
    let mut mac = Hmac::<Sha256>::new_from_slice(&[7; 32]).unwrap();
    mac.update(&canonical_json(&value).unwrap());
    value["signature"] = json!(
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    format!(
        "v1.{}",
        URL_SAFE_NO_PAD.encode(canonical_json(&value).unwrap())
    )
}
async fn send(
    token: &str,
    valid_proof: bool,
    malformed: bool,
    slow: bool,
    foreign: bool,
) -> (StatusCode, Arc<Backend>) {
    let backend = Arc::new(Backend {
        actors: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        slow,
        foreign,
    });
    let app = capability_router(
        CapabilityState::with_backend(backend.clone(), "internal".into(), vec![7; 32])
            .with_legacy_window((chrono::Utc::now().timestamp_millis() + 120000) as u64),
    );
    let body = json!({"query":"q","top_k":5,"threshold":null});
    let mut request = Request::post(PATH)
        .header("content-type", "application/json")
        .header("x-ai-platform-internal-token", token)
        .header("x-ai-tenant-id", "t")
        .header("x-ai-user-id", "u")
        .header("x-ai-session-id", "s")
        .header("x-ai-execution-id", "e")
        .header("x-ai-run-id", "r")
        .header(
            "x-ai-capability-proof",
            if valid_proof {
                proof(&body)
            } else {
                "invalid".into()
            },
        )
        .header(
            "traceparent",
            "00-11111111111111111111111111111111-2222222222222222-01",
        )
        .header("tracestate", "invalid")
        .header("baggage", "secret=value");
    request = request.header("x-ai-budget-ms", if malformed { "invalid" } else { "20" });
    if !malformed {
        request = request
            .header(
                "x-ai-deadline-unix-ms",
                (chrono::Utc::now().timestamp_millis() + 1000).to_string(),
            )
            .header("x-ai-hop-limit", "4");
    }
    let response = app
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), backend)
}
#[tokio::test]
async fn authentication_and_current_actor_precede_budget_admission() {
    for (token, proof, status, actors) in [
        ("wrong", true, StatusCode::UNAUTHORIZED, 0),
        ("internal", false, StatusCode::UNAUTHORIZED, 0),
        ("internal", true, StatusCode::REQUEST_TIMEOUT, 1),
    ] {
        let (actual, backend) = send(token, proof, true, false, false).await;
        assert_eq!(actual, status);
        assert_eq!(backend.actors.load(Ordering::SeqCst), actors);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
    let (status, backend) = send("internal", true, true, false, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(super::super::private_transport::current().is_none());
}
#[tokio::test]
async fn authenticated_budget_bounds_actual_retrieval_and_correlation_is_sanitized() {
    let (status, backend) = send("internal", true, false, true, false).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    let (status, backend) = send("internal", true, false, false, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}
