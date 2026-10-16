//! Original private broker body/path/tenant/expiry proof checks, without network or DB calls.
use ai_platform_gateway::migration_content::{
    capabilities::{CapabilityAuthorization, canonical_json},
    read_broker::bounded_search_result,
    sha256,
};
use axum::http::HeaderMap;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
fn proof(body: &Value, path: &str, expires: i64) -> String {
    let mut p = json!({"schema_version":"ai-platform-capability-proof/v1","method":"POST","path":path,"body_sha256":sha256(&canonical_json(body).unwrap()),"tenant_id":"tenant-a","user_id":"user-a","session_id":"session-a","execution_id":"11111111-1111-4111-8111-111111111111","run_id":"run-a","expires_at":expires,"nonce":"nonce-a"});
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
fn headers(body: &Value, path: &str, expires: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (name, value) in [
        ("x-ai-platform-internal-token", "internal-test-token"),
        ("x-ai-tenant-id", "tenant-a"),
        ("x-ai-user-id", "user-a"),
        ("x-ai-session-id", "session-a"),
        ("x-ai-execution-id", "11111111-1111-4111-8111-111111111111"),
        ("x-ai-run-id", "run-a"),
    ] {
        h.insert(name, value.parse().unwrap());
    }
    h.insert(
        "x-ai-capability-proof",
        proof(body, path, expires).parse().unwrap(),
    );
    h
}
#[test]
fn valid_proof_binds_body_scope_path_and_expiry() {
    let auth = CapabilityAuthorization::new("internal-test-token".into(), vec![b'p'; 32]);
    let body = json!({"offset":0,"limit":8});
    let path = "/internal/v2/agent-capabilities/artifacts/art_12345678/read";
    let h = headers(&body, path, 1060);
    let scope = auth.authorize(&h, path, &body, 1000).unwrap();
    assert_eq!(scope.tenant_id, "tenant-a");
    assert_eq!(scope.user_id, "user-a");
    assert_eq!(scope.session_id, "session-a");
    assert_eq!(
        auth.authorize(&h, path, &json!({"offset":0,"limit":9}), 1000)
            .unwrap_err()
            .status,
        401
    );
    assert_eq!(
        auth.authorize(
            &h,
            "/internal/v2/agent-capabilities/artifacts/art_87654321/read",
            &body,
            1000
        )
        .unwrap_err()
        .status,
        401
    );
    let mut foreign = h.clone();
    foreign.insert("x-ai-tenant-id", "tenant-b".parse().unwrap());
    assert_eq!(
        auth.authorize(&foreign, path, &body, 1000)
            .unwrap_err()
            .status,
        401
    );
    assert_eq!(
        auth.authorize(&headers(&body, path, 999), path, &body, 1000)
            .unwrap_err()
            .status,
        401
    );
}
#[test]
fn internal_token_and_proof_are_independent_requirements() {
    let auth = CapabilityAuthorization::new("internal-test-token".into(), vec![b'p'; 32]);
    let body = json!({});
    let path = "/internal/v2/agent-capabilities/web-search";
    let mut h = headers(&body, path, 1060);
    h.insert("x-ai-platform-internal-token", "wrong".parse().unwrap());
    assert_eq!(
        auth.authorize(&h, path, &body, 1000).unwrap_err().status,
        401
    );
    h.insert(
        "x-ai-platform-internal-token",
        "internal-test-token".parse().unwrap(),
    );
    h.remove("x-ai-capability-proof");
    assert_eq!(
        auth.authorize(&h, path, &body, 1000).unwrap_err().status,
        401
    );
}
#[test]
fn scope_is_validated_before_downstream_work() {
    let auth = CapabilityAuthorization::new("internal-test-token".into(), vec![b'p'; 32]);
    let body = json!({});
    let path = "/internal/v2/agent-capabilities/web-search";
    let mut h = headers(&body, path, 1060);
    h.remove("x-ai-user-id");
    assert_eq!(
        auth.authorize(&h, path, &body, 1000).unwrap_err().status,
        403
    );
}
#[test]
fn search_results_are_bounded_and_reject_non_http_urls() {
    let v=bounded_search_result(&json!({"title":"T".repeat(600),"url":"https://example.com/source","content":"中".repeat(5000),"score":3.})).unwrap();
    assert_eq!(v["title"].as_str().unwrap().chars().count(), 500);
    assert_eq!(v["content"].as_str().unwrap().chars().count(), 4000);
    assert_eq!(v["score"], 1.);
    for url in ["file:///private/file", "javascript:alert(1)", "not-a-url"] {
        assert!(bounded_search_result(&json!({"title":"t","url":url,"content":"c"})).is_none());
    }
}
