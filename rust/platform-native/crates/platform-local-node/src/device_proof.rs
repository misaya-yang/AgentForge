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
            key_class: signer.key_class().into(),
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
            || !matches!(
                (self.key_class.as_str(), self.algorithm.as_str()),
                ("os-vault-software-ed25519", "ed25519")
                    | (
                        "macos-secure-enclave-p256" | "windows-tpm-cng-p256" | "linux-tpm2-p256",
                        "ES256"
                    )
            )
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
    fn shared_gateway_wire_fixtures_match_native_canonical_payload_and_signatures() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../sdk/fixtures/local-node-key-assurance.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let p: DeviceRequestProof = serde_json::from_value(case["proof"].clone()).unwrap();
            assert_eq!(p.payload(), case["payload"].as_str().unwrap());
            p.validate(130).unwrap();
            let algorithm: &dyn ring::signature::VerificationAlgorithm = match p.algorithm.as_str()
            {
                "ed25519" => &ring::signature::ED25519,
                "ES256" => &ring::signature::ECDSA_P256_SHA256_ASN1,
                _ => panic!("fixture algorithm"),
            };
            let key = ring::signature::UnparsedPublicKey::new(
                algorithm,
                URL_SAFE_NO_PAD.decode(&p.public_key).unwrap(),
            );
            let signature = URL_SAFE_NO_PAD.decode(&p.signature).unwrap();
            key.verify(p.payload().as_bytes(), &signature).unwrap();
            let mut changed = p.clone();
            changed.body_sha256 = crate::sha256(b"different request");
            assert!(
                key.verify(changed.payload().as_bytes(), &signature)
                    .is_err()
            );
            changed = p.clone();
            changed.nonce.push('x');
            assert!(
                key.verify(changed.payload().as_bytes(), &signature)
                    .is_err()
            );
            changed = p.clone();
            changed.algorithm = if p.algorithm == "ES256" {
                "ed25519"
            } else {
                "ES256"
            }
            .into();
            assert!(changed.validate(130).is_err());
        }
    }
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
    #[test]
    fn hardware_wire_proof_cannot_claim_software_class_or_reuse_signature_for_another_body() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
        struct Fixture {
            key: EcdsaKeyPair,
            public: String,
        }
        impl PairingSigner for Fixture {
            fn algorithm(&self) -> &str {
                "ES256"
            }
            fn key_class(&self) -> &str {
                "windows-tpm-cng-p256"
            }
            fn public_key(&self) -> &str {
                &self.public
            }
            fn sign(&self, p: &[u8]) -> Result<String> {
                self.key
                    .sign(&ring::rand::SystemRandom::new(), p)
                    .map(|s| URL_SAFE_NO_PAD.encode(s.as_ref()))
                    .map_err(|_| Error::Integrity)
            }
        }
        // Software-generated fixture checks wire/signature binding only; it is
        // deliberately not evidence of hardware attestation or platform acceptance.
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        let signer = Fixture { key, public };
        let proof = DeviceRequestProof::issue(
            &signer,
            "dev_test",
            "POST",
            "https://gateway.test/api/v1/local-nodes/control",
            b"body",
            b"credential",
            "server_nonce_123456",
            100,
            160,
        )
        .unwrap();
        proof.validate(120).unwrap();
        let verify = |proof: &DeviceRequestProof| {
            ring::signature::UnparsedPublicKey::new(
                &ring::signature::ECDSA_P256_SHA256_ASN1,
                URL_SAFE_NO_PAD.decode(&proof.public_key).unwrap(),
            )
            .verify(
                proof.payload().as_bytes(),
                &URL_SAFE_NO_PAD.decode(&proof.signature).unwrap(),
            )
            .is_ok()
        };
        assert!(verify(&proof));
        let mut changed = proof.clone();
        changed.body_sha256 = sha256(b"other");
        assert!(!verify(&changed));
        changed = proof;
        changed.key_class = "os-vault-software-ed25519".into();
        assert!(changed.validate(120).is_err());
        assert!(!verify(&changed));
    }
}
