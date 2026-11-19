//! Source tests/contract/test_gateway_secret.py: real native HMAC verifier and replay store.
use ai_platform_gateway::migration_knowledge_query::auth::{GatewayAuth, ReplayStore};
use axum::http::{HeaderMap, HeaderValue};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeMap;
const SECRET: &str = "00000000000000000000000000000000";
const OTHER: &str = "11111111111111111111111111111111";
const PATH: &str = "/api/v1/knowledge/dataset-a/retrieve";
fn auth(secret: &str, version: &str) -> GatewayAuth {
    GatewayAuth::new(
        secret.into(),
        BTreeMap::new(),
        version.into(),
        ReplayStore::memory(),
    )
    .unwrap()
}
fn identity() -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in [
        ("x-user-id", "user-a"),
        ("x-tenant-id", "tenant-a"),
        ("x-user-roles", "user"),
    ] {
        h.insert(k, HeaderValue::from_static(v));
    }
    h
}
fn sign(a: &GatewayAuth) -> String {
    a.sign("POST", PATH, "top_k=5", b"{}", &identity()).unwrap()
}
async fn verify(a: &GatewayAuth, h: &str) -> bool {
    a.verify(h, "POST", PATH, "top_k=5", b"{}", &identity())
        .await
        .is_ok()
}
#[tokio::test]
async fn roundtrip_preserves_generated_request_id() {
    let s = auth(SECRET, "v2");
    let v = auth(SECRET, "v2");
    let h = sign(&s);
    let expected = h.split(':').nth(2).unwrap();
    assert_eq!(
        v.verify(&h, "POST", PATH, "top_k=5", b"{}", &identity())
            .await
            .unwrap(),
        expected
    );
}
#[tokio::test]
async fn mismatched_secret_and_tampered_signature_rejected() {
    let h = sign(&auth(SECRET, "v2"));
    assert!(!verify(&auth(OTHER, "v2"), &h).await);
    let mut bad = h.clone().into_bytes();
    let last = bad.last_mut().unwrap();
    *last = if *last == b'0' { b'1' } else { b'0' };
    assert!(!verify(&auth(SECRET, "v2"), std::str::from_utf8(&bad).unwrap()).await);
}
#[tokio::test]
async fn stale_and_future_timestamps_rejected_with_valid_hmac() {
    // Construct correctly signed v1 messages at explicit times instead of monkeypatching the clock.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i128;
    for delta in [-120_000, 120_000] {
        let ts = (now + delta).to_string();
        let canonical = format!("abc-123:{ts}");
        let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(canonical.as_bytes());
        let sig = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let header = format!("v1:abc-123:{ts}:{sig}");
        assert!(!verify(&auth(SECRET, "v1"), &header).await);
    }
}
#[tokio::test]
async fn missing_and_malformed_headers_rejected() {
    let a = auth(SECRET, "v2");
    for h in [
        "",
        "garbage",
        "v2",
        "v2:local:id:NaN:hash:signature",
        "v2:local:id:0:hash:signature",
        "v9:id:0:sig",
    ] {
        assert!(!verify(&a, h).await, "{h}");
    }
}
#[tokio::test]
async fn replay_rejected_but_independent_request_ids_allowed() {
    let s = auth(SECRET, "v2");
    let v = auth(SECRET, "v2");
    let a = sign(&s);
    let b = sign(&s);
    assert!(verify(&v, &a).await);
    assert!(!verify(&v, &a).await);
    assert!(verify(&v, &b).await);
}
#[tokio::test]
async fn forged_header_does_not_poison_replay_store() {
    let s = auth(SECRET, "v2");
    let v = auth(SECRET, "v2");
    let valid = sign(&s);
    let mut p = valid.split(':').map(str::to_owned).collect::<Vec<_>>();
    p[5] = "0".repeat(64);
    assert!(!verify(&v, &p.join(":")).await);
    assert!(verify(&v, &valid).await);
}
#[tokio::test]
async fn v2_binds_method_path_query_body_and_forwarded_identity() {
    let h = sign(&auth(SECRET, "v2"));
    for (method, path, query, bytes) in [
        ("GET", PATH, "top_k=5", b"{}".as_slice()),
        (
            "POST",
            "/api/v1/assistant/chat/stream",
            "top_k=5",
            b"{}".as_slice(),
        ),
        ("POST", PATH, "top_k=6", b"{}".as_slice()),
        ("POST", PATH, "top_k=5", b"{\"x\":1}".as_slice()),
    ] {
        assert!(
            auth(SECRET, "v2")
                .verify(&h, method, path, query, bytes, &identity())
                .await
                .is_err()
        );
    }
    for (name, value) in [
        ("x-tenant-id", "tenant-b"),
        ("x-user-id", "user-b"),
        ("x-user-roles", "admin"),
    ] {
        let mut changed = identity();
        changed.insert(name, HeaderValue::from_static(value));
        assert!(
            auth(SECRET, "v2")
                .verify(&h, "POST", PATH, "top_k=5", b"{}", &changed)
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn v1_is_rejected_by_v2_verifier() {
    assert!(!verify(&auth(SECRET, "v2"), &sign(&auth(SECRET, "v1"))).await);
}
#[tokio::test]
async fn v2_rotation_uses_key_id_instead_of_default_secret() {
    let keys = BTreeMap::from([
        ("active".into(), SECRET.into()),
        ("previous".into(), OTHER.into()),
    ]);
    let s = GatewayAuth::new(
        SECRET.into(),
        keys.clone(),
        "v2".into(),
        ReplayStore::memory(),
    )
    .unwrap();
    let v = GatewayAuth::new(OTHER.into(), keys, "v2".into(), ReplayStore::memory()).unwrap();
    let h = sign(&s);
    assert!(h.starts_with("v2:active:"));
    assert!(verify(&v, &h).await);
}
#[tokio::test]
async fn v2_scope_binds_caller_audience_and_allowed_path() {
    let s = auth(SECRET, "v2");
    let h = sign(&s);
    let mut wrong_caller = auth(SECRET, "v2");
    wrong_caller.caller_service = "other-service".into();
    assert!(!verify(&wrong_caller, &h).await);
    let mut wrong_audience = auth(SECRET, "v2");
    wrong_audience.audience = "other-audience".into();
    assert!(!verify(&wrong_audience, &h).await);
    let mut wrong_path = auth(SECRET, "v2");
    wrong_path.allowed_path_prefixes = vec!["/api/v1/assistant".into()];
    assert!(!verify(&wrong_path, &h).await);
}
#[test]
fn secret_length_guard() {
    for secret in ["", "short"] {
        assert!(
            GatewayAuth::new(
                secret.into(),
                BTreeMap::new(),
                "v2".into(),
                ReplayStore::memory()
            )
            .is_err()
        );
    }
}
