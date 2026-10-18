//! Request-proof v1 wire contract; keys are resolved from the authenticated credential.
use super::{ChannelStoreError, canonical_json as canonical};
use serde::{Deserialize, Serialize};
use serde_json::json;
type Result<T> = std::result::Result<T, ChannelStoreError>;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestProof {
    pub version: u32,
    pub device_id: String,
    pub key_class: String,
    pub algorithm: String,
    pub public_key: String,
    pub method: String,
    pub target: String,
    pub body_sha256: String,
    pub credential_sha256: String,
    pub nonce: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub signature: String,
}
impl RequestProof {
    pub fn payload(&self) -> String {
        canonical(
            &json!({"domain":"ai-platform.local-node.request-proof.v1","version":self.version,"device_id":self.device_id,"key_class":self.key_class,"algorithm":self.algorithm,"public_key":self.public_key,"method":self.method,"target":self.target,"body_sha256":self.body_sha256,"credential_sha256":self.credential_sha256,"nonce":self.nonce,"issued_at":self.issued_at,"expires_at":self.expires_at}),
        )
    }
    pub fn validate(&self, now: u64) -> Result<()> {
        let url = url::Url::parse(&self.target).map_err(|_| ChannelStoreError::Rejected)?;
        if self.version != 1
            || super::identity::key_format(&self.algorithm, &self.key_class, &self.public_key)
                .is_err()
            || self.public_key.is_empty()
            || self.public_key.len() > 128
            || !self.device_id.starts_with("dev_")
            || self.device_id.len() > 128
            || !matches!(
                self.method.as_str(),
                "GET" | "POST" | "PUT" | "DELETE" | "PATCH"
            )
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || self.target.len() > 8192
            || self.nonce.len() < 16
            || self.nonce.len() > 128
            || !self
                .nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || self.issued_at == 0
            || self.issued_at > now
            || now > self.expires_at
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > 60
            || [&self.body_sha256, &self.credential_sha256]
                .iter()
                .any(|s| {
                    s.len() != 64
                        || !s
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        {
            return Err(ChannelStoreError::Rejected);
        }
        Ok(())
    }
}

impl RequestProof {
    pub fn verify(
        &self,
        registered_key: &str,
        credential_hash: &str,
        device: &str,
        target: &str,
        body_hash: &str,
        now: u64,
    ) -> Result<()> {
        self.verify_registered(
            "ed25519",
            super::identity::SOFTWARE,
            registered_key,
            credential_hash,
            device,
            target,
            body_hash,
            now,
        )
    }
    pub fn verify_registered(
        &self,
        algorithm: &str,
        class: &str,
        registered_key: &str,
        credential_hash: &str,
        device: &str,
        target: &str,
        body_hash: &str,
        now: u64,
    ) -> Result<()> {
        self.validate(now)?;
        if self.algorithm != algorithm
            || self.key_class != class
            || self.public_key != registered_key
            || self.device_id != device
            || self.credential_sha256 != credential_hash
            || self.target != target
            || self.method != "POST"
            || self.body_sha256 != body_hash
        {
            return Err(ChannelStoreError::Rejected);
        }
        super::identity::verify_signature(
            algorithm,
            class,
            registered_key,
            &self.signature,
            self.payload().as_bytes(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::signature::{Ed25519KeyPair, KeyPair};
    fn proof() -> (RequestProof, String) {
        let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        let mut proof = RequestProof {
            version: 1,
            device_id: "dev_012345678901234567890123".into(),
            key_class: "os-vault-software-ed25519".into(),
            algorithm: "ed25519".into(),
            public_key: public.clone(),
            method: "POST".into(),
            target: "https://node.example/api/v1/local-nodes/control".into(),
            body_sha256: "a".repeat(64),
            credential_sha256: "b".repeat(64),
            nonce: "server_nonce_0123456789".into(),
            issued_at: 100,
            expires_at: 160,
            signature: String::new(),
        };
        proof.signature = URL_SAFE_NO_PAD.encode(key.sign(proof.payload().as_bytes()).as_ref());
        (proof, public)
    }
    #[test]
    fn verifies_registered_key_and_every_authenticated_request_binding() {
        let (proof, key) = proof();
        assert!(
            proof
                .verify(
                    &key,
                    &proof.credential_sha256,
                    &proof.device_id,
                    &proof.target,
                    &proof.body_sha256,
                    130
                )
                .is_ok()
        );
        for (key, credential, device, target, body, now) in [
            (
                URL_SAFE_NO_PAD.encode([4; 32]),
                proof.credential_sha256.clone(),
                proof.device_id.clone(),
                proof.target.clone(),
                proof.body_sha256.clone(),
                130,
            ),
            (
                key.clone(),
                "c".repeat(64),
                proof.device_id.clone(),
                proof.target.clone(),
                proof.body_sha256.clone(),
                130,
            ),
            (
                key.clone(),
                proof.credential_sha256.clone(),
                "dev_foreign".into(),
                proof.target.clone(),
                proof.body_sha256.clone(),
                130,
            ),
            (
                key.clone(),
                proof.credential_sha256.clone(),
                proof.device_id.clone(),
                format!("{}?changed=1", proof.target),
                proof.body_sha256.clone(),
                130,
            ),
            (
                key.clone(),
                proof.credential_sha256.clone(),
                proof.device_id.clone(),
                proof.target.clone(),
                "d".repeat(64),
                130,
            ),
            (
                key.clone(),
                proof.credential_sha256.clone(),
                proof.device_id.clone(),
                proof.target.clone(),
                proof.body_sha256.clone(),
                161,
            ),
            (
                key.clone(),
                proof.credential_sha256.clone(),
                proof.device_id.clone(),
                proof.target.clone(),
                proof.body_sha256.clone(),
                99,
            ),
        ] {
            assert!(
                proof
                    .verify(&key, &credential, &device, &target, &body, now)
                    .is_err()
            );
        }
        let mut changed = proof.clone();
        changed.nonce.push('x');
        assert!(
            changed
                .verify(
                    &key,
                    &proof.credential_sha256,
                    &proof.device_id,
                    &proof.target,
                    &proof.body_sha256,
                    130
                )
                .is_err()
        );
        let mut changed = proof.clone();
        changed.signature = URL_SAFE_NO_PAD.encode([0; 64]);
        assert!(
            changed
                .verify(
                    &key,
                    &proof.credential_sha256,
                    &proof.device_id,
                    &proof.target,
                    &proof.body_sha256,
                    130
                )
                .is_err()
        );
    }
    #[test]
    fn refuses_unknown_classes_and_overlong_windows_even_if_resigned() {
        let (mut proof, _) = proof();
        proof.key_class = "tpm".into();
        assert!(proof.validate(130).is_err());
        proof.key_class = "os-vault-software-ed25519".into();
        proof.expires_at = 161;
        assert!(proof.validate(130).is_err());
        proof.expires_at = 160;
        proof.target = "http://node.example/api/v1/local-nodes/control".into();
        assert!(proof.validate(130).is_err());
    }
}
