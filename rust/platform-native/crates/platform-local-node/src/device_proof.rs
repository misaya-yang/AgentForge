//! Opt-in request possession proof. Activation requires a server-issued one-use nonce
//! and atomic server-side replay consumption; this does not replace bearer transport.
use crate::{Error, Result, canonical, sha256, transport::PairingSigner};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRequestProof {
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
impl DeviceRequestProof {
    pub fn issue(
        signer: &(impl PairingSigner + ?Sized),
        device_id: &str,
        method: &str,
        target: &str,
        body: &[u8],
        credential: &[u8],
        nonce: &str,
        issued_at: u64,
        expires_at: u64,
    ) -> Result<Self> {
        let mut proof = Self {
            version: 1,
            device_id: device_id.into(),
            key_class: "os-vault-software-ed25519".into(),
            algorithm: signer.algorithm().into(),
            public_key: signer.public_key().into(),
            method: method.into(),
            target: target.into(),
            body_sha256: sha256(body),
            credential_sha256: sha256(credential),
            nonce: nonce.into(),
            issued_at,
            expires_at,
            signature: String::new(),
        };
        proof.validate(issued_at)?;
        proof.signature = signer.sign(proof.payload().as_bytes())?;
        Ok(proof)
    }
    pub fn payload(&self) -> String {
        canonical(
            &json!({"domain":"ai-platform.local-node.request-proof.v1","version":self.version,"device_id":self.device_id,"key_class":self.key_class,"algorithm":self.algorithm,"public_key":self.public_key,"method":self.method,"target":self.target,"body_sha256":self.body_sha256,"credential_sha256":self.credential_sha256,"nonce":self.nonce,"issued_at":self.issued_at,"expires_at":self.expires_at}),
        )
    }
    pub fn validate(&self, now: u64) -> Result<()> {
        let url = url::Url::parse(&self.target).map_err(|_| Error::Protocol)?;
        if self.version != 1
            || self.key_class != "os-vault-software-ed25519"
            || self.algorithm != "ed25519"
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
            return Err(Error::Protocol);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{Ed25519PairingSigner, PinnedEd25519Verifier};
    #[test]
    fn proof_binds_request_credential_nonce_and_expiry() {
        let signer = Ed25519PairingSigner::from_identity_secret(&[9; 32]).unwrap();
        let proof = DeviceRequestProof::issue(
            &signer,
            "dev_test",
            "POST",
            "https://gateway.example/heartbeat",
            b"body",
            b"credential",
            "server_nonce_123456",
            100,
            160,
        )
        .unwrap();
        let verifier =
            PinnedEd25519Verifier::new([("device".into(), signer.public_key().into())]).unwrap();
        assert!(verifier.verify_detached("device", proof.payload().as_bytes(), &proof.signature));
        assert!(proof.validate(161).is_err());
        assert!(proof.validate(99).is_err());
        for field in ["method", "target", "body", "credential", "nonce", "class"] {
            let mut changed = proof.clone();
            match field {
                "method" => changed.method = "GET".into(),
                "target" => changed.target.push_str("?changed=1"),
                "body" => changed.body_sha256 = sha256(b"other"),
                "credential" => changed.credential_sha256 = sha256(b"other"),
                "nonce" => changed.nonce.push('x'),
                _ => changed.key_class = "hardware".into(),
            }
            assert!(!verifier.verify_detached(
                "device",
                changed.payload().as_bytes(),
                &proof.signature
            ));
        }
        assert!(
            DeviceRequestProof::issue(
                &signer,
                "dev_test",
                "POST",
                "http://gateway.example/",
                b"",
                b"c",
                "server_nonce_123456",
                100,
                161
            )
            .is_err()
        );
    }
}
