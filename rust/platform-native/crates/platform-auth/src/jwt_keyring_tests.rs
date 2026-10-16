use super::*;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;

const OLD_PRIVATE: &str = include_str!("../tests/fixtures/jwt-rotation-old-ed25519-private.pem");
const NEW_PRIVATE: &str = include_str!("../tests/fixtures/jwt-rotation-new-ed25519-private.pem");
const OLD_PUBLIC_X: &str = "pfn5-ZeMp4J4fOvlMv-I86va0mSjqPxX0xdwvEHc_oI";
const NEW_PUBLIC_X: &str = "gREJfMOuOfFXNB0MkL7rmbSzPDvgtF-zYzYCt9CkRBU";

fn key(
    kid: &str,
    x: &str,
    private_key_pem: Option<&str>,
    verify_until: Option<u64>,
) -> JwtKeyConfig {
    JwtKeyConfig {
        kid: kid.to_owned(),
        algorithm: "EdDSA".to_owned(),
        public_jwk: json!({
            "kty":"OKP",
            "crv":"Ed25519",
            "alg":"EdDSA",
            "use":"sig",
            "key_ops":["verify"],
            "kid":kid,
            "x":x
        }),
        private_key_pem: private_key_pem.map(str::to_owned),
        verify_until,
    }
}

fn claims() -> Claims {
    Claims {
        sub: "rotation-fixture-user".into(),
        user_id: "rotation-fixture-user".into(),
        tenant_id: "rotation-fixture-tenant".into(),
        email: None,
        roles: vec!["user".into()],
        permissions: vec![],
        tier: "normal".into(),
        exp: 0,
        iat: 0,
        nbf: None,
        jti: String::new(),
        aud: None,
        iss: None,
    }
}

fn rotated_codec(active_kid: &str) -> TokenCodec {
    let keys = if active_kid == "new-2026" {
        vec![
            key("old-2026", OLD_PUBLIC_X, None, Some(10_901)),
            key("new-2026", NEW_PUBLIC_X, Some(NEW_PRIVATE), None),
        ]
    } else {
        vec![key("old-2026", OLD_PUBLIC_X, Some(OLD_PRIVATE), None)]
    };
    TokenCodec::from_keyring(
        JwtSigningMode::AsymmetricKeyring,
        b"",
        Some(active_kid),
        &keys,
        None,
        None,
    )
    .unwrap()
}

#[test]
fn issuance_active_key_switch_retains_old_key_until_token_expiry() {
    let before_rotation = rotated_codec("old-2026");
    let old_token = before_rotation.issue(claims(), 100).unwrap();
    let old_header = jsonwebtoken::decode_header(&old_token).unwrap();
    assert_eq!(old_header.kid.as_deref(), Some("old-2026"));

    let after_rotation = rotated_codec("new-2026");
    assert!(after_rotation.verify(&old_token, 10_899).is_ok());
    let new_token = after_rotation.issue(claims(), 200).unwrap();
    let new_header = jsonwebtoken::decode_header(&new_token).unwrap();
    assert_eq!(new_header.kid.as_deref(), Some("new-2026"));
    assert!(after_rotation.verify(&new_token, 200).is_ok());

    let live_jwks = after_rotation.jwks_at(10_900);
    let live_kids = live_jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|key| key["kid"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(live_kids, vec!["new-2026", "old-2026"]);
    for key in live_jwks["keys"].as_array().unwrap() {
        assert_eq!(key["key_ops"], json!(["verify"]));
        for private_field in ["d", "p", "q", "dp", "dq", "qi", "oth", "k"] {
            assert!(key.get(private_field).is_none());
        }
    }

    // The old token expires before its public key's planned retirement time.
    assert!(after_rotation.verify(&old_token, 10_900).is_err());
    let expired_jwks = after_rotation.jwks_at(10_901);
    let expired_kids = expired_jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|key| key["kid"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(expired_kids, vec!["new-2026"]);
}

#[test]
fn asymmetric_mode_rejects_unknown_or_missing_kid_even_with_valid_signature() {
    let codec = rotated_codec("new-2026");
    let mut claims = claims();
    claims.iat = 200;
    claims.exp = 10_900;
    claims.jti = uuid::Uuid::new_v4().to_string();
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("unknown-2026".into());
    let encoding = EncodingKey::from_ed_pem(NEW_PRIVATE.as_bytes()).unwrap();
    let unknown = jsonwebtoken::encode(&header, &claims, &encoding).unwrap();
    assert!(matches!(codec.verify(&unknown, 201), Err(AuthError::Token)));

    header.kid = None;
    let missing = jsonwebtoken::encode(&header, &claims, &encoding).unwrap();
    assert!(matches!(codec.verify(&missing, 201), Err(AuthError::Token)));
}

#[test]
fn legacy_hs256_mode_is_explicit_and_keeps_kidless_tokens_verifiable() {
    let secret = b"legacy-compatibility-test-secret-32bytes!";
    let codec = TokenCodec::new(secret, None, None).unwrap();
    let issued = codec.issue(claims(), 500).unwrap();
    assert_eq!(
        jsonwebtoken::decode_header(&issued).unwrap().kid.as_deref(),
        Some("legacy")
    );
    assert!(codec.verify(&issued, 501).is_ok());

    let mut old_claims = claims();
    old_claims.iat = 500;
    old_claims.exp = 10_000;
    old_claims.jti = uuid::Uuid::new_v4().to_string();
    let old_without_kid = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &old_claims,
        &EncodingKey::from_secret(secret),
    )
    .unwrap();
    assert!(codec.verify(&old_without_kid, 501).is_ok());
    assert_eq!(codec.jwks_at(501), json!({"keys":[]}));

    let mut unknown_header = Header::new(Algorithm::HS256);
    unknown_header.kid = Some("unknown".into());
    let unknown = jsonwebtoken::encode(
        &unknown_header,
        &old_claims,
        &EncodingKey::from_secret(secret),
    )
    .unwrap();
    assert!(matches!(codec.verify(&unknown, 501), Err(AuthError::Token)));
}

#[test]
fn asymmetric_default_never_falls_back_to_a_legacy_secret() {
    assert!(
        TokenCodec::from_keyring(
            JwtSigningMode::AsymmetricKeyring,
            b"legacy-compatibility-test-secret-32bytes!",
            None,
            &[],
            None,
            None,
        )
        .is_err()
    );
}

#[test]
fn keyring_rejects_private_jwks_fields_and_mismatched_signing_pairs() {
    let mut private_jwk = key("new-2026", NEW_PUBLIC_X, Some(NEW_PRIVATE), None);
    private_jwk.public_jwk["d"] = json!("must-not-be-published");
    assert!(
        TokenCodec::from_keyring(
            JwtSigningMode::AsymmetricKeyring,
            b"",
            Some("new-2026"),
            &[private_jwk],
            None,
            None,
        )
        .is_err()
    );

    let mismatched = key("new-2026", OLD_PUBLIC_X, Some(NEW_PRIVATE), None);
    assert!(
        TokenCodec::from_keyring(
            JwtSigningMode::AsymmetricKeyring,
            b"",
            Some("new-2026"),
            &[mismatched],
            None,
            None,
        )
        .is_err()
    );
}
