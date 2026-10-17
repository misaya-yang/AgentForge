//! Ed25519 API-token key rotation. Private material is never serializable/Debug.
// SPDX-License-Identifier: Apache-2.0
use crate::{AuthError, Claims};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{
    SigningKey, VerifyingKey,
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const ACCESS_TOKEN_TTL: u64 = 300;
pub struct PublicKeyConfig {
    pub kid: String,
    pub public_pem: String,
    pub not_before: u64,
    pub not_after: u64,
}
struct PublicKey {
    decode: DecodingKey,
    x: String,
    not_before: u64,
    not_after: u64,
}
pub struct EdDsaKeyRing {
    active: String,
    encode: EncodingKey,
    keys: BTreeMap<String, PublicKey>,
}
fn valid_kid(kid: &str) -> bool {
    !kid.is_empty()
        && kid.len() <= 128
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
impl EdDsaKeyRing {
    /// Keep current+previous public keys for rotation overlap. The previous
    /// private key is deliberately unnecessary. No implicit key generation.
    pub fn new(
        active: String,
        private_pem: &str,
        public: Vec<PublicKeyConfig>,
    ) -> Result<Self, AuthError> {
        if !valid_kid(&active) || public.is_empty() || public.len() > 2 {
            return Err(AuthError::Configuration);
        }
        let signer =
            SigningKey::from_pkcs8_pem(private_pem).map_err(|_| AuthError::Configuration)?;
        let mut keys = BTreeMap::new();
        for key in public {
            if !valid_kid(&key.kid) || key.not_before >= key.not_after {
                return Err(AuthError::Configuration);
            }
            let verifier = VerifyingKey::from_public_key_pem(&key.public_pem)
                .map_err(|_| AuthError::Configuration)?;
            if key.kid == active && verifier != signer.verifying_key() {
                return Err(AuthError::Configuration);
            }
            let decode = DecodingKey::from_ed_pem(key.public_pem.as_bytes())
                .map_err(|_| AuthError::Configuration)?;
            if keys
                .insert(
                    key.kid,
                    PublicKey {
                        decode,
                        x: URL_SAFE_NO_PAD.encode(verifier.as_bytes()),
                        not_before: key.not_before,
                        not_after: key.not_after,
                    },
                )
                .is_some()
            {
                return Err(AuthError::Configuration);
            }
        }
        if !keys.contains_key(&active) {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            active,
            encode: EncodingKey::from_ed_pem(private_pem.as_bytes())
                .map_err(|_| AuthError::Configuration)?,
            keys,
        })
    }
    pub fn check_signing_window(&self, now: u64) -> Result<(), AuthError> {
        let key = &self.keys[&self.active];
        if now < key.not_before
            || now
                .checked_add(ACCESS_TOKEN_TTL)
                .is_none_or(|expiry| expiry > key.not_after)
        {
            return Err(AuthError::Configuration);
        }
        Ok(())
    }
    pub fn jwks(&self, now: u64) -> Value {
        json!({"keys":self.keys.iter().filter(|(_,key)|key.not_before<=now && now<key.not_after).map(|(kid,key)|json!({"kty":"OKP","crv":"Ed25519","use":"sig","alg":"EdDSA","kid":kid,"x":key.x})).collect::<Vec<_>>()})
    }
    pub(crate) fn issue(&self, claims: &Claims, now: u64) -> Result<String, AuthError> {
        let key = &self.keys[&self.active];
        if now < key.not_before || claims.exp > key.not_after {
            return Err(AuthError::Configuration);
        }
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(self.active.clone());
        jsonwebtoken::encode(&header, claims, &self.encode).map_err(|_| AuthError::Crypto)
    }
    pub(crate) fn verify(
        &self,
        token: &str,
        validation: &Validation,
        now: u64,
    ) -> Result<Claims, AuthError> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| AuthError::Token)?;
        if header.alg != Algorithm::EdDSA
            || header.jku.is_some()
            || header.jwk.is_some()
            || header.x5u.is_some()
        {
            return Err(AuthError::Token);
        }
        let key = self
            .keys
            .get(header.kid.as_deref().ok_or(AuthError::Token)?)
            .ok_or(AuthError::Token)?;
        if now < key.not_before || now >= key.not_after {
            return Err(AuthError::Token);
        }
        let claims = jsonwebtoken::decode::<Claims>(token, &key.decode, validation)
            .map_err(|_| AuthError::Token)?
            .claims;
        if claims.iat < key.not_before || claims.exp > key.not_after {
            return Err(AuthError::Token);
        }
        Ok(claims)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey, spki::der::pem::LineEnding};
    fn make_ring(seed: u8, kid: &str) -> EdDsaKeyRing {
        let private = SigningKey::from_bytes(&[seed; 32]);
        EdDsaKeyRing::new(
            kid.into(),
            private.to_pkcs8_pem(LineEnding::LF).unwrap().as_str(),
            vec![PublicKeyConfig {
                kid: kid.into(),
                public_pem: private
                    .verifying_key()
                    .to_public_key_pem(LineEnding::LF)
                    .unwrap(),
                not_before: 1,
                not_after: 10000,
            }],
        )
        .unwrap()
    }
    #[test]
    fn jwks_contains_only_public_material_and_strict_key_ids() {
        let ring = make_ring(7, "current");
        let set = ring.jwks(10);
        assert_eq!(set["keys"][0]["kid"], "current");
        assert!(set["keys"][0].get("d").is_none());
        assert!(ring.jwks(10000)["keys"].as_array().unwrap().is_empty());
        let claims = Claims {
            sub: "u".into(),
            user_id: "u".into(),
            tenant_id: "t".into(),
            email: None,
            roles: vec![],
            permissions: vec![],
            tier: "user".into(),
            exp: 400,
            iat: 100,
            nbf: None,
            jti: uuid::Uuid::new_v4().to_string(),
            aud: None,
            iss: None,
        };
        let token = ring.issue(&claims, 100).unwrap();
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.validate_exp = false;
        validation.validate_aud = false;
        assert!(ring.verify(&token, &validation, 100).is_ok());
        assert!(
            make_ring(8, "other")
                .verify(&token, &validation, 100)
                .is_err()
        );
        assert!(ring.verify(&token, &validation, 10000).is_err());
    }
    #[test]
    fn rotation_overlap_accepts_old_key_then_revocation_removes_it() {
        let old = SigningKey::from_bytes(&[3; 32]);
        let new = SigningKey::from_bytes(&[4; 32]);
        let public = |key: &SigningKey, kid: &str| PublicKeyConfig {
            kid: kid.into(),
            public_pem: key
                .verifying_key()
                .to_public_key_pem(LineEnding::LF)
                .unwrap(),
            not_before: 1,
            not_after: 10000,
        };
        let old_ring = EdDsaKeyRing::new(
            "old".into(),
            old.to_pkcs8_pem(LineEnding::LF).unwrap().as_str(),
            vec![public(&old, "old")],
        )
        .unwrap();
        let codec = |keys| {
            crate::TokenCodec::new(&[1; 32], Some("api".into()), Some("gateway".into()))
                .unwrap()
                .with_eddsa(keys)
                .unwrap()
        };
        let claims = Claims {
            sub: "u".into(),
            user_id: "u".into(),
            tenant_id: "t".into(),
            email: None,
            roles: vec![],
            permissions: vec![],
            tier: "user".into(),
            exp: 0,
            iat: 0,
            nbf: None,
            jti: String::new(),
            aud: None,
            iss: None,
        };
        let old_token = codec(old_ring).issue(claims.clone(), 100).unwrap();
        let overlap = codec(
            EdDsaKeyRing::new(
                "new".into(),
                new.to_pkcs8_pem(LineEnding::LF).unwrap().as_str(),
                vec![public(&old, "old"), public(&new, "new")],
            )
            .unwrap(),
        );
        assert!(overlap.verify(&old_token, 101).is_ok());
        assert!(overlap.verify(&old_token, 400).is_err());
        let new_token = overlap.issue(claims.clone(), 101).unwrap();
        assert_eq!(
            jsonwebtoken::decode_header(&new_token)
                .unwrap()
                .kid
                .as_deref(),
            Some("new")
        );
        let retired = codec(
            EdDsaKeyRing::new(
                "new".into(),
                new.to_pkcs8_pem(LineEnding::LF).unwrap().as_str(),
                vec![public(&new, "new")],
            )
            .unwrap(),
        );
        assert!(retired.verify(&old_token, 102).is_err());
        assert!(retired.verify(&new_token, 102).is_ok());
        // Existing CLI bearer remains cryptographically compatible during rollout.
        let legacy = crate::TokenCodec::new(&[1; 32], Some("api".into()), Some("gateway".into()))
            .unwrap()
            .issue(claims, 100)
            .unwrap();
        assert!(retired.verify(&legacy, 102).is_ok());
    }
}
