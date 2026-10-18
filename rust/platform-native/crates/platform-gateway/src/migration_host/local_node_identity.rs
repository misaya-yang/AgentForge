//! Server-owned Local Node key admission. Hardware names on the wire are claims.
//! Verified assurance requires an authority-enrolled, signed platform-verifier
//! result bound to this exact owner/device/key and its retained platform evidence.
use super::{ChannelStoreError, canonical_json};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
type Result<T> = std::result::Result<T, ChannelStoreError>;
pub const SOFTWARE: &str = "os-vault-software-ed25519";
pub const HARDWARE_CLASSES: &[&str] = &[
    "macos-secure-enclave-p256",
    "windows-tpm-cng-p256",
    "linux-tpm2-p256",
];
fn rejected<T>() -> Result<T> {
    Err(ChannelStoreError::Rejected)
}
fn decode(value: &str) -> Result<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ChannelStoreError::Rejected)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return rejected();
    }
    Ok(bytes)
}
pub fn key_format(algorithm: &str, class: &str, public: &str) -> Result<Vec<u8>> {
    let key = decode(public)?;
    match (algorithm, class) {
        ("ed25519", SOFTWARE) if key.len() == 32 => Ok(key),
        ("ES256", class) if HARDWARE_CLASSES.contains(&class) && key.len() == 65 && key[0] == 4 => {
            Ok(key)
        }
        _ => rejected(),
    }
}
// Only canonical ASN.1 DER INTEGERs, no raw JOSE r||s, negative/zero/padded
// integers, long-form lengths or trailing bytes. ring validates scalar ranges
// and the uncompressed SEC1 point on P-256 during cryptographic verification.
fn strict_der(bytes: &[u8]) -> bool {
    if !(8..=72).contains(&bytes.len()) || bytes[0] != 0x30 || bytes[1] as usize != bytes.len() - 2
    {
        return false;
    }
    let mut at = 2;
    for _ in 0..2 {
        if at + 2 > bytes.len() || bytes[at] != 2 {
            return false;
        }
        let n = bytes[at + 1] as usize;
        at += 2;
        if !(1..=33).contains(&n) || at + n > bytes.len() {
            return false;
        }
        let v = &bytes[at..at + n];
        if v[0] & 0x80 != 0
            || v.iter().all(|v| *v == 0)
            || (n > 1 && v[0] == 0 && v[1] & 0x80 == 0)
            || (n == 33 && v[0] != 0)
        {
            return false;
        }
        at += n;
    }
    at == bytes.len()
}
pub fn signature_format(algorithm: &str, signature: &str) -> Result<Vec<u8>> {
    let bytes = decode(signature)?;
    match algorithm {
        "ed25519" if bytes.len() == 64 => Ok(bytes),
        "ES256" if strict_der(&bytes) => Ok(bytes),
        _ => rejected(),
    }
}
pub fn verify_signature(
    algorithm: &str,
    class: &str,
    public: &str,
    signature: &str,
    message: &[u8],
) -> Result<()> {
    let key = key_format(algorithm, class, public)?;
    let signature = signature_format(algorithm, signature)?;
    let verifier: &dyn ring::signature::VerificationAlgorithm = match algorithm {
        "ed25519" => &ring::signature::ED25519,
        "ES256" => &ring::signature::ECDSA_P256_SHA256_ASN1,
        _ => return rejected(),
    };
    ring::signature::UnparsedPublicKey::new(verifier, key)
        .verify(message, &signature)
        .map_err(|_| ChannelStoreError::Rejected)
}
pub fn platform_matches(class: &str, platform: &str) -> bool {
    matches!(
        (class, platform),
        (SOFTWARE, "macos" | "windows" | "linux")
            | ("macos-secure-enclave-p256", "macos")
            | ("windows-tpm-cng-p256", "windows")
            | ("linux-tpm2-p256", "linux")
    )
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct IdentityPolicy {
    pub allow_software: bool,
    pub allow_unverified_p256: bool,
    pub require_verified_hardware: bool,
    pub allowed_hardware_classes: Vec<String>,
}
impl Default for IdentityPolicy {
    fn default() -> Self {
        Self {
            allow_software: true,
            allow_unverified_p256: true,
            require_verified_hardware: false,
            allowed_hardware_classes: HARDWARE_CLASSES.iter().map(|v| (*v).into()).collect(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assurance {
    Software,
    UnverifiedP256,
    VerifiedHardware,
}
impl IdentityPolicy {
    pub fn admit(&self, algorithm: &str, class: &str, verified: bool) -> Result<Assurance> {
        if self
            .allowed_hardware_classes
            .iter()
            .any(|v| !HARDWARE_CLASSES.contains(&v.as_str()))
        {
            return rejected();
        }
        match (algorithm, class) {
            ("ed25519", SOFTWARE) if self.allow_software && !self.require_verified_hardware => {
                Ok(Assurance::Software)
            }
            ("ES256", class)
                if HARDWARE_CLASSES.contains(&class)
                    && self.allowed_hardware_classes.iter().any(|v| v == class) =>
            {
                if verified && matches!(class, "windows-tpm-cng-p256" | "linux-tpm2-p256") {
                    Ok(Assurance::VerifiedHardware)
                } else if self.allow_unverified_p256 && !self.require_verified_hardware {
                    Ok(Assurance::UnverifiedP256)
                } else {
                    rejected()
                }
            }
            _ => rejected(),
        }
    }
}
/// This assertion is created by a separately enrolled platform attestation
/// verifier AFTER validating the retained native TPM certify evidence. It is
/// never accepted from pairing JSON, and the Gateway role cannot enroll/modify
/// trust roots or attestation rows. A self-signed assertion is not hardware proof.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareAssertion {
    pub domain: String,
    pub attestation_id: String,
    pub verifier_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub device_id: String,
    pub public_key_sha256: String,
    pub hardware_class: String,
    pub platform: String,
    pub evidence_format: String,
    pub evidence_sha256: String,
    pub attestation_nonce: String,
    pub verified_at: u64,
    pub expires_at: u64,
}
pub struct AttestationScope<'a> {
    pub tenant: &'a str,
    pub user: &'a str,
    pub device: &'a str,
    pub public: &'a str,
    pub class: &'a str,
    pub platform: &'a str,
}
impl HardwareAssertion {
    pub fn verify(
        &self,
        scope: AttestationScope<'_>,
        verifier_id: &str,
        verifier_public: &str,
        signature: &str,
        evidence: &[u8],
        now: u64,
    ) -> Result<()> {
        let public = key_format("ES256", scope.class, scope.public)?;
        // macOS SE key generation currently has no enrolled attestation flow.
        // Never equate Secure Enclave naming, a vendor device certificate, or
        // key non-exportability observed by the client with key attestation.
        if self.domain != "ai-platform.local-node.hardware-attestation.v1"
            || uuid::Uuid::parse_str(&self.attestation_id).is_err()
            || self.verifier_id != verifier_id
            || self.tenant_id != scope.tenant
            || self.user_id != scope.user
            || self.device_id != scope.device
            || self.hardware_class != scope.class
            || self.platform != scope.platform
            || !platform_matches(scope.class, scope.platform)
            || !matches!(scope.class, "windows-tpm-cng-p256" | "linux-tpm2-p256")
            || self.public_key_sha256 != format!("{:x}", Sha256::digest(public))
            || self.evidence_format != "tpm2-certify"
            || evidence.len() < 16
            || evidence.len() > 65536
            || self.evidence_sha256 != format!("{:x}", Sha256::digest(evidence))
            || self.verified_at == 0
            || self.verified_at > now
            || now >= self.expires_at
            || self.expires_at <= self.verified_at
            || self.expires_at - self.verified_at > 86400 * 90
        {
            return rejected();
        }
        let value = serde_json::to_value(self).map_err(|_| ChannelStoreError::Rejected)?;
        verify_signature(
            "ed25519",
            SOFTWARE,
            verifier_public,
            signature,
            canonical_json(&value).as_bytes(),
        )?;
        verify_tpm_certify(scope.public, &self.attestation_nonce, evidence)
    }
}

// Evidence contains a real TPM2 Certify statement and P-256 AK signature.
// The enrolled verifier roots that AK to actual platform trust/endorsement
// credentials. Gateway additionally verifies the native statement, nonce,
// TPM-generated/fixed key attributes and exact enrolled SEC1 key name itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TpmCertifyEvidence {
    version: u32,
    public_area: String,
    attestation: String,
    signature: String,
    attestation_key: String,
}
struct TpmReader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> TpmReader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .ok_or(ChannelStoreError::Rejected)?;
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or(ChannelStoreError::Rejected)?;
        self.at = end;
        Ok(out)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| ChannelStoreError::Rejected)?,
        ))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ChannelStoreError::Rejected)?,
        ))
    }
    fn sized(&mut self) -> Result<&'a [u8]> {
        let len = self.u16()? as usize;
        self.take(len)
    }
    fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
}
fn verify_tpm_certify(public: &str, nonce: &str, evidence: &[u8]) -> Result<()> {
    let e: TpmCertifyEvidence =
        serde_json::from_slice(evidence).map_err(|_| ChannelStoreError::Rejected)?;
    let nonce = decode(nonce)?;
    if e.version != 1 || nonce.len() != 32 {
        return rejected();
    }
    let area = decode(&e.public_area)?;
    let attest = decode(&e.attestation)?;
    if area.len() > 1024 || attest.len() > 4096 {
        return rejected();
    }
    // public_area is TPMT_PUBLIC, with no outer TPM2B length. ECC, SHA256 name,
    // fixedTPM/fixedParent/sensitiveDataOrigin/sign, NULL symmetric and KDF,
    // NIST P-256, ECDSA-SHA256 or the TPM's algorithm-neutral NULL scheme.
    let mut r = TpmReader {
        bytes: &area,
        at: 0,
    };
    if r.u16()? != 0x0023 || r.u16()? != 0x000b || r.u32()? & 0x0004_0032 != 0x0004_0032 {
        return rejected();
    }
    let _auth_policy = r.sized()?;
    if r.u16()? != 0x0010 {
        return rejected();
    }
    match r.u16()? {
        0x0010 => {}
        0x0018 => {
            if r.u16()? != 0x000b {
                return rejected();
            }
        }
        _ => return rejected(),
    }
    if r.u16()? != 0x0003 || r.u16()? != 0x0010 {
        return rejected();
    }
    let x = r.sized()?;
    let y = r.sized()?;
    let key = decode(public)?;
    if x.len() != 32
        || y.len() != 32
        || !r.done()
        || key.len() != 65
        || key[0] != 4
        || &key[1..33] != x
        || &key[33..] != y
    {
        return rejected();
    }
    let mut name = vec![0x00, 0x0b];
    name.extend(Sha256::digest(&area));
    let mut r = TpmReader {
        bytes: &attest,
        at: 0,
    };
    if r.u32()? != 0xff54_4347 || r.u16()? != 0x8017 {
        return rejected();
    }
    if r.sized()?.is_empty() || r.sized()? != nonce.as_slice() {
        return rejected();
    }
    let clock = r.take(17)?;
    if clock[16] != 1 {
        return rejected();
    }
    r.take(8)?; // firmwareVersion, part of the signed native statement
    if r.sized()? != name.as_slice() || r.sized()?.is_empty() || !r.done() {
        return rejected();
    }
    verify_signature(
        "ES256",
        "windows-tpm-cng-p256",
        &e.attestation_key,
        &e.signature,
        &attest,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn wire() -> Vec<Value> {
        serde_json::from_str::<Value>(include_str!(
            "../../../../../../sdk/fixtures/local-node-key-assurance.json"
        ))
        .unwrap()["cases"]
            .as_array()
            .unwrap()
            .clone()
    }
    #[test]
    fn shared_client_proofs_bind_registered_algorithm_class_key_and_request() {
        for case in wire() {
            let p: super::super::request_proof::RequestProof =
                serde_json::from_value(case["proof"].clone()).unwrap();
            assert_eq!(p.payload(), case["payload"].as_str().unwrap());
            let check = |p: &super::super::request_proof::RequestProof| {
                p.verify_registered(
                    &case["proof"]["algorithm"].as_str().unwrap(),
                    &case["proof"]["key_class"].as_str().unwrap(),
                    &case["proof"]["public_key"].as_str().unwrap(),
                    &"b".repeat(64),
                    "dev_012345678901234567890123",
                    "https://node.example/api/v1/local-nodes/control",
                    &"a".repeat(64),
                    130,
                )
            };
            assert!(check(&p).is_ok());
            for field in [
                "algorithm",
                "key_class",
                "public_key",
                "device_id",
                "body_sha256",
                "credential_sha256",
                "nonce",
                "target",
                "expires_at",
            ] {
                let mut changed = case["proof"].clone();
                changed[field] = if field == "expires_at" {
                    json!(129)
                } else {
                    json!("foreign")
                };
                let changed = serde_json::from_value(changed).unwrap();
                assert!(check(&changed).is_err(), "{field}");
            }
            assert!(
                p.verify_registered(
                    "ed25519",
                    SOFTWARE,
                    &p.public_key,
                    &p.credential_sha256,
                    &p.device_id,
                    &p.target,
                    &p.body_sha256,
                    161
                )
                .is_err()
            );
        }
    }
    #[test]
    fn p256_rejects_compressed_off_curve_aliased_keys_and_non_der_signatures() {
        let cases = wire();
        let p = &cases[1]["proof"];
        let public = p["public_key"].as_str().unwrap();
        let signature = p["signature"].as_str().unwrap();
        let payload = cases[1]["payload"].as_str().unwrap().as_bytes();
        let mut key = decode(public).unwrap();
        key[0] = 2;
        assert!(
            verify_signature(
                "ES256",
                "windows-tpm-cng-p256",
                &URL_SAFE_NO_PAD.encode(key),
                signature,
                payload
            )
            .is_err()
        );
        let mut off_curve = vec![0; 65];
        off_curve[0] = 4;
        assert!(
            verify_signature(
                "ES256",
                "windows-tpm-cng-p256",
                &URL_SAFE_NO_PAD.encode(off_curve),
                signature,
                payload
            )
            .is_err()
        );
        assert!(key_format("ES256", "windows-tpm-cng-p256", &format!("{public}=")).is_err());
        assert!(verify_signature("ed25519", SOFTWARE, public, signature, payload).is_err());
        for bytes in [
            vec![1; 64],
            vec![0x30, 6, 2, 1, 0, 2, 1, 1],
            vec![0x30, 7, 2, 2, 0, 1, 2, 1, 1],
            vec![0x30, 6, 2, 1, 0x80, 2, 1, 1],
            vec![0x30, 6, 2, 1, 1, 2, 1, 1, 0],
        ] {
            assert!(signature_format("ES256", &URL_SAFE_NO_PAD.encode(bytes)).is_err());
        }
    }
    #[test]
    fn a_hardware_class_claim_alone_never_meets_verified_policy() {
        let default = IdentityPolicy::default();
        for class in HARDWARE_CLASSES {
            assert_eq!(
                default.admit("ES256", class, false),
                Ok(Assurance::UnverifiedP256)
            );
        }
        let strict = IdentityPolicy {
            require_verified_hardware: true,
            ..Default::default()
        };
        assert!(strict.admit("ed25519", SOFTWARE, false).is_err());
        for class in HARDWARE_CLASSES {
            assert!(strict.admit("ES256", class, false).is_err());
        }
        let denied = IdentityPolicy {
            allow_software: false,
            allow_unverified_p256: false,
            allowed_hardware_classes: vec!["linux-tpm2-p256".into()],
            ..Default::default()
        };
        assert!(denied.admit("ES256", "windows-tpm-cng-p256", true).is_err());
        assert!(denied.admit("ES256", "linux-tpm2-p256", false).is_err());
        assert!(denied.admit("ed25519", SOFTWARE, false).is_err());
    }
    #[test]
    fn tpm_native_proof_rejects_claim_only_wrong_nonce_key_attributes_and_ak_signature() {
        let cases = wire();
        let public = cases[1]["proof"]["public_key"].as_str().unwrap();
        let nonce = URL_SAFE_NO_PAD.encode([3; 32]);
        let evidence = tpm_fixture(public);
        assert!(verify_tpm_certify(public, &nonce, &evidence).is_ok());
        assert!(verify_tpm_certify(public, &URL_SAFE_NO_PAD.encode([4; 32]), &evidence).is_err());
        assert!(verify_tpm_certify(public, &nonce, b"a client hardware class assertion").is_err());
        for field in ["public_area", "attestation", "signature", "attestation_key"] {
            let mut value: Value = serde_json::from_slice(&evidence).unwrap();
            let mut bytes = decode(value[field].as_str().unwrap()).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            value[field] = json!(URL_SAFE_NO_PAD.encode(bytes));
            assert!(
                verify_tpm_certify(public, &nonce, &serde_json::to_vec(&value).unwrap()).is_err(),
                "{field}"
            );
        }
        let mut value: Value = serde_json::from_slice(&evidence).unwrap();
        let mut area = decode(value["public_area"].as_str().unwrap()).unwrap();
        area[7] &= !0x20;
        value["public_area"] = json!(URL_SAFE_NO_PAD.encode(area));
        assert!(verify_tpm_certify(public, &nonce, &serde_json::to_vec(&value).unwrap()).is_err());
    }
    fn tpm_fixture(public: &str) -> Vec<u8> {
        use ring::signature::{EcdsaKeyPair, KeyPair};
        let random = ring::rand::SystemRandom::new();
        let document =
            EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING, &random)
                .unwrap();
        let ak = EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            document.as_ref(),
            &random,
        )
        .unwrap();
        let mut area = vec![
            0, 0x23, 0, 0x0b, 0, 4, 0, 0x32, 0, 0, 0, 0x10, 0, 0x10, 0, 3, 0, 0x10,
        ];
        let key = decode(public).unwrap();
        area.extend([0, 32]);
        area.extend(&key[1..33]);
        area.extend([0, 32]);
        area.extend(&key[33..]);
        let mut name = vec![0, 0x0b];
        name.extend(Sha256::digest(&area));
        let mut attest = vec![0xff, 0x54, 0x43, 0x47, 0x80, 0x17, 0, 34];
        attest.extend(&name);
        attest.extend([0, 32]);
        attest.extend([3; 32]);
        attest.extend([0; 16]);
        attest.push(1);
        attest.extend([0; 8]);
        attest.extend([0, 34]);
        attest.extend(&name);
        attest.extend([0, 34]);
        attest.extend(&name);
        let signature = ak.sign(&random, &attest).unwrap();
        serde_json::to_vec(&json!({"version":1,"public_area":URL_SAFE_NO_PAD.encode(area),"attestation":URL_SAFE_NO_PAD.encode(attest),"signature":URL_SAFE_NO_PAD.encode(signature.as_ref()),"attestation_key":URL_SAFE_NO_PAD.encode(ak.public_key().as_ref())})).unwrap()
    }
    #[test]
    fn enrolled_verifier_assertions_bind_platform_evidence_owner_and_exact_key() {
        use ring::signature::{Ed25519KeyPair, KeyPair};
        let cases = wire();
        let public = cases[1]["proof"]["public_key"].as_str().unwrap();
        let authority = Ed25519KeyPair::from_seed_unchecked(&[19; 32]).unwrap();
        let signer = URL_SAFE_NO_PAD.encode(authority.public_key().as_ref());
        let evidence = tpm_fixture(public);
        let mut a = HardwareAssertion {
            domain: "ai-platform.local-node.hardware-attestation.v1".into(),
            attestation_id: "00000000-0000-4000-8000-000000000001".into(),
            verifier_id: "fixture-verifier".into(),
            tenant_id: "tenant-one".into(),
            user_id: "user-one".into(),
            device_id: "dev_fixture".into(),
            public_key_sha256: format!("{:x}", Sha256::digest(decode(public).unwrap())),
            hardware_class: "windows-tpm-cng-p256".into(),
            platform: "windows".into(),
            evidence_format: "tpm2-certify".into(),
            evidence_sha256: format!("{:x}", Sha256::digest(&evidence)),
            attestation_nonce: URL_SAFE_NO_PAD.encode([3; 32]),
            verified_at: 100,
            expires_at: 200,
        };
        let signature = URL_SAFE_NO_PAD.encode(
            authority
                .sign(canonical_json(&serde_json::to_value(&a).unwrap()).as_bytes())
                .as_ref(),
        );
        let scope = || AttestationScope {
            tenant: "tenant-one",
            user: "user-one",
            device: "dev_fixture",
            public,
            class: "windows-tpm-cng-p256",
            platform: "windows",
        };
        // Software-generated TPM-format/AK fixtures validate wire cryptography,
        // never physical TPM origin or real verifier/platform enrollment.
        assert!(
            a.verify(
                scope(),
                "fixture-verifier",
                &signer,
                &signature,
                &evidence,
                130
            )
            .is_ok()
        );
        let stranger = Ed25519KeyPair::from_seed_unchecked(&[20; 32]).unwrap();
        assert!(
            a.verify(
                scope(),
                "fixture-verifier",
                &URL_SAFE_NO_PAD.encode(stranger.public_key().as_ref()),
                &signature,
                &evidence,
                130
            )
            .is_err()
        );
        assert!(
            a.verify(
                scope(),
                "client-chosen-verifier",
                &signer,
                &signature,
                &evidence,
                130
            )
            .is_err()
        );
        assert!(
            a.verify(
                scope(),
                "fixture-verifier",
                &signer,
                &signature,
                b"changed native evidence",
                130
            )
            .is_err()
        );
        assert!(
            a.verify(
                scope(),
                "fixture-verifier",
                &signer,
                &signature,
                &evidence,
                200
            )
            .is_err()
        );
        for field in [
            "tenant_id",
            "user_id",
            "device_id",
            "public_key_sha256",
            "hardware_class",
            "platform",
            "evidence_format",
            "evidence_sha256",
            "attestation_nonce",
        ] {
            let mut value = serde_json::to_value(&a).unwrap();
            value[field] = json!("foreign");
            // Even the trusted verifier cannot authorize an assertion for a different scope.
            let sig =
                URL_SAFE_NO_PAD.encode(authority.sign(canonical_json(&value).as_bytes()).as_ref());
            let changed: HardwareAssertion = serde_json::from_value(value).unwrap();
            assert!(
                changed
                    .verify(scope(), "fixture-verifier", &signer, &sig, &evidence, 130)
                    .is_err(),
                "{field}"
            );
        }
        a.hardware_class = "macos-secure-enclave-p256".into();
        a.platform = "macos".into();
        let signature = URL_SAFE_NO_PAD.encode(
            authority
                .sign(canonical_json(&serde_json::to_value(&a).unwrap()).as_bytes())
                .as_ref(),
        );
        assert!(
            a.verify(
                AttestationScope {
                    class: "macos-secure-enclave-p256",
                    platform: "macos",
                    ..scope()
                },
                "fixture-verifier",
                &signer,
                &signature,
                &evidence,
                130
            )
            .is_err()
        );
    }
}
