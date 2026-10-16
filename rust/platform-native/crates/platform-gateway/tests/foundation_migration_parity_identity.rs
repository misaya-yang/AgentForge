//! Real decoder and identity-layer contracts; no fabricated successful JWT principal.
use ai_platform_gateway::migration_host::{
    identity::{self, IdentityConfig, StreamingIdentity},
    jwt::JwtDecoder,
};
use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    http::{HeaderValue, Request},
    middleware,
    routing::get,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
fn app(config: IdentityConfig) -> Router {
    Router::new()
        .route(
            "/probe/stream",
            get(|Extension(user): Extension<StreamingIdentity>| async move { Json(user.0) }),
        )
        .layer(middleware::from_fn_with_state(
            Arc::new(config),
            identity::inject,
        ))
}
async fn request(config: IdentityConfig, name: &str, value: &str) -> Value {
    let response = app(config)
        .oneshot(
            Request::builder()
                .uri("/probe/stream")
                .header(name, value)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}
fn configured() -> IdentityConfig {
    let decoder = JwtDecoder::new(
        "synthetic-fixture-key-not-used-outside-test",
        &["HS256".into()],
        None,
        None,
    )
    .unwrap();
    IdentityConfig {
        jwt_enabled: true,
        verify_jwt: Some(Arc::new(move |token| decoder.decode(token))),
        ..Default::default()
    }
}
#[tokio::test]
async fn jwt_identity_accepts_valid_claims_and_denies_forged_expired_malformed_or_disabled() {
    let now = chrono::Utc::now().timestamp();
    let claims = json!({"sub":"user123","tenant_id":"tenant1","tier":"premium","exp":now+3600});
    let sign = |claims: &Value, key: &str| {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(key.as_bytes()),
        )
        .unwrap()
    };
    let valid = sign(&claims, "synthetic-fixture-key-not-used-outside-test");
    let bearer = format!("Bearer {valid}");
    let user = request(configured(), "authorization", &bearer).await;
    assert_eq!(user["is_authenticated"], true);
    assert_eq!(user["user_id"], "user123");
    assert_eq!(user["tenant_id"], "tenant1");
    assert_eq!(user["tier"], "premium");
    let mut expired = claims.clone();
    expired["exp"] = json!(now - 3600);
    for token in [
        sign(&claims, "different-synthetic-test-key"),
        sign(&expired, "synthetic-fixture-key-not-used-outside-test"),
        "invalid.token.here".into(),
    ] {
        let user = request(configured(), "authorization", &format!("Bearer {token}")).await;
        assert_eq!(user["is_authenticated"], false);
        assert!(user["user_id"].as_str().unwrap().starts_with("anon:"));
    }
    let mut disabled = configured();
    disabled.jwt_enabled = false;
    assert_eq!(
        request(disabled, "authorization", &bearer).await["is_authenticated"],
        false
    );
    let unavailable = IdentityConfig {
        jwt_enabled: true,
        verify_jwt: None,
        ..Default::default()
    };
    assert_eq!(
        request(unavailable, "authorization", &bearer).await["is_authenticated"],
        false
    );
}
#[tokio::test]
async fn static_key_identity_requires_exact_nonempty_configured_key() {
    for (key, keys, expected) in [
        ("key-abc-123", vec!["key-abc-123", "key-def-456"], true),
        (
            "key-def-456",
            vec!["key-abc-123", "key-def-456", "key-ghi-789"],
            true,
        ),
        ("wrong-key", vec!["key-abc-123"], false),
        ("any-key", vec![], false),
        ("", vec!["key-abc-123"], false),
        ("only-key", vec!["only-key"], true),
    ] {
        let config = IdentityConfig {
            api_key_enabled: true,
            api_key_header: "x-api-key".into(),
            api_keys: keys.into_iter().map(str::to_owned).collect(),
            ..Default::default()
        };
        let user = request(config, "x-api-key", key).await;
        assert_eq!(user["is_authenticated"], expected);
        if expected {
            assert!(user["user_id"].as_str().unwrap().starts_with("apikey:"));
        } else {
            assert!(user["user_id"].as_str().unwrap().starts_with("anon:"));
        }
    }
}
#[tokio::test]
async fn identity_layer_preserves_both_set_cookie_values() {
    let app = Router::new()
        .route(
            "/probe/stream",
            get(|| async {
                let mut response = axum::response::Response::new(Body::empty());
                for cookie in [
                    "ag_embed_session=nonce; Path=/; HttpOnly",
                    "ag_anon_id=11111111-1111-4111-8111-111111111111; Path=/; HttpOnly",
                ] {
                    response
                        .headers_mut()
                        .append("set-cookie", HeaderValue::from_static(cookie));
                }
                response
            }),
        )
        .layer(middleware::from_fn_with_state(
            Arc::new(IdentityConfig::default()),
            identity::inject,
        ));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/probe/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(cookies.iter().any(|v| v.starts_with("ag_embed_session=")));
    assert!(cookies.iter().any(|v| v.starts_with("ag_anon_id=")));
    assert!(response.headers().contains_key("x-user-id"));
}
#[tokio::test]
async fn guest_session_ids_require_canonical_v4_or_bounded_hex_suffix() {
    for (id, valid) in [
        ("550e8400-e29b-41d4-a716-446655440000", true),
        ("550e8400e29b41d4a716446655440000", false),
        ("550e8400-e29b-11d4-a716-446655440000", false),
        ("guest_ABCDEF0123456789", true),
        ("guest_not-valid-hex", false),
    ] {
        let user = request(IdentityConfig::default(), "x-guest-session", id).await;
        assert_eq!(user["user_type"], if valid { "guest" } else { "anonymous" });
        assert_eq!(user["is_authenticated"], false);
    }
}

#[test]
fn invalid_signature_and_expiry_emit_sanitized_warning_events() {
    use tracing::{
        Event, Metadata, Subscriber,
        field::{Field, Visit},
        span::{Attributes, Id, Record},
    };
    #[derive(Clone)]
    struct Capture(Arc<std::sync::Mutex<Vec<String>>>);
    struct Fields(String);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!("{}={value:?};", field.name()));
        }
    }
    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                let mut fields = Fields(String::new());
                event.record(&mut fields);
                self.0.lock().unwrap().push(fields.0);
            }
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
    let capture = Capture(Arc::new(std::sync::Mutex::new(Vec::new())));
    let decoder = JwtDecoder::new(
        "synthetic-logging-fixture-secret",
        &["HS256".into()],
        None,
        None,
    )
    .unwrap();
    let claims = json!({"sub":"private-subject-fixture","exp":chrono::Utc::now().timestamp()+3600});
    let forged = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(b"different-fixture-key"),
    )
    .unwrap();
    let expired = encode(
        &Header::default(),
        &json!({"sub":"private-subject-fixture","exp":chrono::Utc::now().timestamp()-3600}),
        &EncodingKey::from_secret(b"synthetic-logging-fixture-secret"),
    )
    .unwrap();
    tracing::subscriber::with_default(capture.clone(), || {
        assert_eq!(decoder.decode(&forged).unwrap_err(), "Invalid token");
        assert_eq!(decoder.decode(&expired).unwrap_err(), "Invalid token");
    });
    let events = capture.0.lock().unwrap().join("\n");
    assert!(events.contains("gateway.jwt.invalid_signature"));
    assert!(events.contains("gateway.jwt.expired"));
    for forbidden in [
        forged.as_str(),
        expired.as_str(),
        "private-subject-fixture",
        "synthetic-logging-fixture-secret",
        "different-fixture-key",
    ] {
        assert!(!events.contains(forbidden));
    }
}
