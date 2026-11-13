//! Observable crypto assertions from tests/core/auth/test_jwt.py, using the real
//! streaming-host decoder. No database, session authority, or credentials required.
use ai_platform_auth::{JwtKeyConfig, JwtSigningMode};
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

#[test]
fn gateway_decoder_selects_rotated_public_key_by_kid_and_rejects_unknown_kid() {
    const OLD_PRIVATE: &str =
        include_str!("../../platform-auth/tests/fixtures/jwt-rotation-old-ed25519-private.pem");
    const NEW_PRIVATE: &str =
        include_str!("../../platform-auth/tests/fixtures/jwt-rotation-new-ed25519-private.pem");
    const OLD_X: &str = "pfn5-ZeMp4J4fOvlMv-I86va0mSjqPxX0xdwvEHc_oI";
    const NEW_X: &str = "gREJfMOuOfFXNB0MkL7rmbSzPDvgtF-zYzYCt9CkRBU";
    let now = chrono::Utc::now().timestamp() as u64;
    let key = |kid: &str, x: &str, verify_until: Option<u64>| JwtKeyConfig {
        kid: kid.into(),
        algorithm: "EdDSA".into(),
        public_jwk: json!({"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","key_ops":["verify"],"kid":kid,"x":x}),
        private_key_pem: None,
        verify_until,
    };
    let decoder = JwtDecoder::from_keyring_config(
        JwtSigningMode::AsymmetricKeyring,
        "",
        Some("new-2026"),
        &[
            key("old-2026", OLD_X, Some(now + 3_600)),
            key("new-2026", NEW_X, None),
        ],
        None,
        None,
    )
    .unwrap();
    let claims = json!({"sub":"fixture-user","iat":now,"exp":now+1800});
    let mut old_header = Header::new(Algorithm::EdDSA);
    old_header.kid = Some("old-2026".into());
    let old_token = jsonwebtoken::encode(
        &old_header,
        &claims,
        &EncodingKey::from_ed_pem(OLD_PRIVATE.as_bytes()).unwrap(),
    )
    .unwrap();
    let mut new_header = Header::new(Algorithm::EdDSA);
    new_header.kid = Some("new-2026".into());
    let new_token = jsonwebtoken::encode(
        &new_header,
        &claims,
        &EncodingKey::from_ed_pem(NEW_PRIVATE.as_bytes()).unwrap(),
    )
    .unwrap();
    assert_eq!(decoder.decode(&old_token).unwrap()["sub"], "fixture-user");
    assert_eq!(decoder.decode(&new_token).unwrap()["sub"], "fixture-user");

    let mut unknown_header = Header::new(Algorithm::EdDSA);
    unknown_header.kid = Some("unknown-2026".into());
    let unknown = jsonwebtoken::encode(
        &unknown_header,
        &claims,
        &EncodingKey::from_ed_pem(NEW_PRIVATE.as_bytes()).unwrap(),
    )
    .unwrap();
    assert_eq!(decoder.decode(&unknown).unwrap_err(), "Invalid token");
}
