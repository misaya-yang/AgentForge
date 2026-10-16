//! Ed25519 device proof and pinned platform-signature verification.
use crate::{Error, Result, envelope::PlatformVerifier};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const DEVICE_KEY_DOMAIN: &[u8] = b"ai-platform.local-node.ed25519.device.v1\0";

pub struct Ed25519PairingSigner {
    seed: [u8; 32],
    public_key: String,
}

impl Ed25519PairingSigner {
    /// Derive a domain-separated Ed25519 seed from the existing 32-byte device
    /// identity secret. The input secret must come from SecureCredentialStore.
    pub fn from_identity_secret(secret: &[u8]) -> Result<Self> {
        if secret.len() != 32 {
            return Err(Error::Denied("device identity key invalid"));
        }
        let mut hasher = Sha256::new();
        hasher.update(DEVICE_KEY_DOMAIN);
        hasher.update(secret);
        let seed: [u8; 32] = hasher.finalize().into();
        let key = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|_| Error::Denied("device pairing signer unavailable"))?;
        Ok(Self {
            seed,
            public_key: URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
        })
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }
}

impl crate::transport::PairingSigner for Ed25519PairingSigner {
    fn algorithm(&self) -> &str {
        "ed25519"
    }

    fn public_key(&self) -> &str {
        &self.public_key
    }

    fn sign(&self, payload: &[u8]) -> Result<String> {
        let key = Ed25519KeyPair::from_seed_unchecked(&self.seed)
            .map_err(|_| Error::Denied("device pairing signer unavailable"))?;
        Ok(URL_SAFE_NO_PAD.encode(key.sign(payload).as_ref()))
    }
}

pub struct PinnedEd25519Verifier {
    keys: HashMap<String, Vec<u8>>,
}

impl PinnedEd25519Verifier {
    pub fn new(keys: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let mut parsed = HashMap::new();
        for (key_id, encoded) in keys {
            if key_id.is_empty()
                || key_id.len() > 255
                || key_id
                    .bytes()
                    .any(|byte| !(byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)))
            {
                return Err(Error::Denied("trusted platform key invalid"));
            }
            let key = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| Error::Denied("trusted platform key invalid"))?;
            if key.len() != 32 || parsed.insert(key_id, key).is_some() {
                return Err(Error::Denied("trusted platform key invalid"));
            }
        }
        if parsed.is_empty() {
            return Err(Error::Denied("pinned platform key required"));
        }
        Ok(Self { keys: parsed })
    }

    pub fn verify_detached(&self, key_id: &str, payload: &[u8], signature: &str) -> bool {
        let Some(key) = self.keys.get(key_id) else {
            return false;
        };
        let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
            return false;
        };
        UnparsedPublicKey::new(&ED25519, key)
            .verify(payload, &signature)
            .is_ok()
    }
}

impl PlatformVerifier for PinnedEd25519Verifier {
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> bool {
        self.verify_detached(key_id, payload, signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::PairingSigner;

    #[test]
    fn identity_secret_derives_stable_device_proof_and_verifies_pinned_key() {
        let first = Ed25519PairingSigner::from_identity_secret(&[7; 32]).unwrap();
        let second = Ed25519PairingSigner::from_identity_secret(&[7; 32]).unwrap();
        assert_eq!(first.public_key(), second.public_key());
        assert_eq!(first.algorithm(), "ed25519");
        let payload = b"pairing-proof-fixture";
        let signature = first.sign(payload).unwrap();
        let verifier = PinnedEd25519Verifier::new([(
            "operator-pinned-fixture".into(),
            first.public_key().into(),
        )])
        .unwrap();
        assert!(verifier.verify("operator-pinned-fixture", payload, &signature));
        assert!(!verifier.verify("unconfigured-platform", payload, &signature));
    }

    #[test]
    fn platform_verifier_rejects_unknown_key_tamper_and_malformed_signature() {
        let signer = Ed25519PairingSigner::from_identity_secret(&[9; 32]).unwrap();
        let payload = b"signed-platform-envelope";
        let signature = signer.sign(payload).unwrap();
        let verifier = PinnedEd25519Verifier::new([(
            "platform-test".into(),
            signer.public_key().into(),
        )])
        .unwrap();
        assert!(verifier.verify("platform-test", payload, &signature));
        assert!(!verifier.verify("other-key", payload, &signature));
        assert!(!verifier.verify("platform-test", b"tampered", &signature));
        assert!(!verifier.verify("platform-test", payload, "not-a-signature"));
    }
}
