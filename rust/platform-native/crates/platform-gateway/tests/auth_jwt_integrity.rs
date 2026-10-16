//! Observable crypto assertions from tests/core/auth/test_jwt.py, using the real
//! streaming-host decoder. No database, session authority, or credentials required.
use ai_platform_gateway::migration_host::jwt::JwtDecoder;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

#[test]
fn actual_decoder_enforces_signature_expiry_audience_issuer_and_algorithm() {
    let secret = b"synthetic-jwt-integrity-fixture-only";
    let now = chrono::Utc::now().timestamp();
    let base = json!({"sub":"user_1","user_id":"user_1","exp":now+3600,"iat":now-1,
        "tenant_id":"tenant-fixture","tier":"enterprise"});
    let sign = |claims: &Value, key: &[u8], algorithm| {
        jsonwebtoken::encode(
            &Header::new(algorithm),
            claims,
            &EncodingKey::from_secret(key),
        )
        .unwrap()
    };
    let decoder = |audience, issuer| {
        JwtDecoder::new(
            std::str::from_utf8(secret).unwrap(),
            &["HS256".into()],
            audience,
            issuer,
        )
        .unwrap()
    };
    let plain = decoder(None, None);
    let token = sign(&base, secret, Algorithm::HS256);
    let verified = plain.decode(&token).unwrap();
    assert_eq!(verified["sub"], "user_1");
    assert_eq!(verified["tenant_id"], "tenant-fixture");
    assert_eq!(verified["tier"], "enterprise");
    assert!(
        plain
            .decode(&sign(
                &base,
                b"different-synthetic-fixture-key",
                Algorithm::HS256
            ))
            .is_err()
    );
    assert!(plain.decode("not.a.jwt").is_err());
    assert!(
        plain
            .decode(&sign(&base, secret, Algorithm::HS512))
            .is_err()
    );
    let mut expired = base.clone();
    expired["exp"] = json!(now - 3600);
    assert!(
        plain
            .decode(&sign(&expired, secret, Algorithm::HS256))
            .is_err()
    );
    let mut audience = base.clone();
    audience["aud"] = json!("fixture-app");
    let audience_token = sign(&audience, secret, Algorithm::HS256);
    assert_eq!(
        decoder(Some("fixture-app"), None)
            .decode(&audience_token)
            .unwrap()["aud"],
        "fixture-app"
    );
    assert!(
        decoder(Some("other-app"), None)
            .decode(&audience_token)
            .is_err()
    );
    assert!(decoder(Some("fixture-app"), None).decode(&token).is_err());
    assert_eq!(plain.decode(&audience_token).unwrap()["aud"], "fixture-app");
    let mut issuer = base.clone();
    issuer["iss"] = json!("fixture-issuer");
    let issuer_token = sign(&issuer, secret, Algorithm::HS256);
    assert_eq!(
        decoder(None, Some("fixture-issuer"))
            .decode(&issuer_token)
            .unwrap()["iss"],
        "fixture-issuer"
    );
    assert!(
        decoder(None, Some("other-issuer"))
            .decode(&issuer_token)
            .is_err()
    );
    assert!(
        decoder(None, Some("fixture-issuer"))
            .decode(&token)
            .is_err()
    );
}
