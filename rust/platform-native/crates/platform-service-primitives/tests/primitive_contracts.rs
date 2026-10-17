//! Infrastructure-only contract tests; no Gateway/auth/domain implementation is linked.
use ai_platform_service_primitives::{
    cancellation::AdmissionCancellation,
    dns_policy::oauth_form_safe_address,
    provider_cipher::NativeProviderCipher,
    provider_endpoints::{ChatEndpointResolver, EndpointDomain},
    provider_service::ProviderCipher,
    python_json::python_json,
};
use std::collections::BTreeMap;
#[test]
fn canonical_json_keeps_unicode_float_and_sorted_key_identity() {
    let value = serde_json::json!({"z":1e-5,"a":"α","zero":-0.0});
    assert_eq!(
        python_json(&value, false, false),
        "{\"a\":\"α\",\"z\":1e-05,\"zero\":-0.0}"
    );
    assert_eq!(
        python_json(&serde_json::json!("α"), true, false),
        "\"\\u03b1\""
    );
}
#[test]
fn cipher_envelope_round_trip_and_legacy_fallback_are_unchanged() {
    let cipher = NativeProviderCipher::new();
    let encrypted = cipher.encrypt("synthetic-only", "synthetic-key").unwrap();
    assert!(encrypted.starts_with("enc:"));
    assert_eq!(
        cipher.decrypt(&encrypted, "synthetic-key").unwrap(),
        "synthetic-only"
    );
    assert_eq!(cipher.decrypt(&encrypted, "wrong-key").unwrap(), encrypted);
    assert_eq!(cipher.decrypt("legacy", "key").unwrap(), "legacy");
}
#[test]
fn dns_policy_keeps_source_shared_range_exception_and_rejects_private() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.1.2",
        "::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(!oauth_form_safe_address(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["8.8.8.8", "100.64.0.1", "::ffff:100.64.0.1"] {
        assert!(oauth_form_safe_address(ip.parse().unwrap()), "{ip}");
    }
}
#[test]
fn endpoint_resolution_preserves_domain_override_and_python_whitespace() {
    let env = BTreeMap::from([
        ("DASHSCOPE_API_KEY".into(), "fallback".into()),
        (
            "DASHSCOPE_EMBEDDING_API_KEY".into(),
            "\u{1c}specific\u{1f}".into(),
        ),
        (
            "DASHSCOPE_BASE_URL".into(),
            "https://example.invalid/compatible-mode/v1/".into(),
        ),
    ]);
    let resolver = ChatEndpointResolver::new(&env);
    let embedding = resolver.resolve_dashscope(EndpointDomain::Embedding);
    assert_eq!(embedding.api_key, "specific");
    assert_eq!(embedding.base_url, "https://example.invalid/api/v1");
    let ocr = resolver.resolve_dashscope(EndpointDomain::Ocr);
    assert_eq!(ocr.api_key, "fallback");
}
#[tokio::test]
async fn cancellation_clones_keep_the_same_owner_signal() {
    let signal = AdmissionCancellation::default();
    let other = signal.clone();
    assert!(!other.is_cancelled());
    signal.cancel();
    other.cancelled().await;
    assert!(other.is_cancelled());
}

#[tokio::test]
async fn content_error_retains_wire_status_detail_headers_and_redacted_sqlstate() {
    use ai_platform_service_primitives::{content::ContentError, postgres_error::PgOperationError};
    use axum::response::IntoResponse;
    let mut error = ContentError::code(409, "conflict", "changed");
    error.headers.insert("retry-after".into(), "1".into());
    let response = error.into_response();
    assert_eq!(response.status(), 409);
    assert_eq!(response.headers()["retry-after"], "1");
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"detail":{"code":"conflict","message":"changed"}})
    );
    let error = ContentError::from(PgOperationError::Database {
        sqlstate: Some("42P01".into()),
    });
    assert_eq!(error.status, 503);
    assert!(error.missing_schema());
    assert_eq!(error.detail, "Database unavailable");
}
