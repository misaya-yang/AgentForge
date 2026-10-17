//! Purpose-separated control-to-sandbox jobs. Platform leases/tokens and signing
//! secrets are deliberately absent from this wire contract.
use crate::{CapabilityScopeV2, canonical_json_hash};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SANDBOX_JOB_SCHEMA: &str = "sandbox-job/v1";
pub const MAX_SANDBOX_JOB_TTL_MS: u64 = 360_000;
const DOMAIN: &[u8] = b"agentforge/control-to-sandbox/v1\0";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxJob {
    pub schema_version: String,
    pub job_id: String,
    pub scope: CapabilityScopeV2,
    pub run_id: String,
    pub tool_call_id: String,
    pub arguments_hash: String,
    pub issued_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    pub code: String,
    pub inputs: Value,
    pub limits: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxAccess {
    pub job_id: String,
    pub scope: CapabilityScopeV2,
    pub job_digest: String,
    pub issued_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SandboxMessage {
    Execute(SandboxJob),
    Poll(SandboxAccess),
    Cancel(SandboxAccess),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedSandboxMessage {
    pub key_id: String,
    pub message: SandboxMessage,
    pub signature_hex: String,
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value.bytes().any(|byte| byte.is_ascii_control())
}
impl SandboxJob {
    pub fn digest(&self) -> Result<String, &'static str> {
        canonical_json_hash(&serde_json::to_value(self).map_err(|_| "sandbox_serialization")?)
            .map_err(|_| "sandbox_serialization")
    }
}
impl SandboxMessage {
    fn validate(&self, now: u64) -> Result<(), &'static str> {
        let (id, scope, issued, expires) = match self {
            Self::Execute(job) => {
                if job.schema_version != SANDBOX_JOB_SCHEMA
                    || !valid_text(&job.run_id, 64)
                    || !valid_text(&job.tool_call_id, 160)
                    || !job.arguments_hash.starts_with("sha256:")
                    || job.arguments_hash.len() != 71
                    || job.code.is_empty()
                    || job.code.len() > 2_000_000
                    || !job.inputs.is_array()
                    || !job.limits.is_object()
                {
                    return Err("sandbox_job_invalid");
                }
                (
                    &job.job_id,
                    &job.scope,
                    job.issued_at_epoch_ms,
                    job.expires_at_epoch_ms,
                )
            }
            Self::Poll(access) | Self::Cancel(access) => {
                if !access.job_digest.starts_with("sha256:") || access.job_digest.len() != 71 {
                    return Err("sandbox_access_invalid");
                }
                (
                    &access.job_id,
                    &access.scope,
                    access.issued_at_epoch_ms,
                    access.expires_at_epoch_ms,
                )
            }
        };
        if !valid_text(id, 64)
            || !valid_text(&scope.tenant_id, 255)
            || !valid_text(&scope.user_id, 255)
            || !valid_text(&scope.session_id, 255)
            || expires <= issued
            || expires.saturating_sub(issued) > MAX_SANDBOX_JOB_TTL_MS
            || now >= expires
            || issued > now.saturating_add(5000)
        {
            return Err("sandbox_message_expired_or_invalid");
        }
        Ok(())
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, &'static str> {
        let digest =
            canonical_json_hash(&serde_json::to_value(self).map_err(|_| "sandbox_serialization")?)
                .map_err(|_| "sandbox_serialization")?;
        Ok([DOMAIN, digest.as_bytes()].concat())
    }
}

/// This type exists only in the control process; it is neither Debug nor Serialize.
pub struct SandboxJobSigner {
    key_id: String,
    key: SigningKey,
}
impl SandboxJobSigner {
    pub fn new(key_id: String, secret: [u8; 32]) -> Result<Self, &'static str> {
        if !valid_text(&key_id, 128) {
            return Err("sandbox_key_id_invalid");
        }
        Ok(Self {
            key_id,
            key: SigningKey::from_bytes(&secret),
        })
    }
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
    pub fn seal(
        &self,
        message: SandboxMessage,
        now: u64,
    ) -> Result<SealedSandboxMessage, &'static str> {
        message.validate(now)?;
        let signature = self.key.sign(&message.signing_bytes()?);
        Ok(SealedSandboxMessage {
            key_id: self.key_id.clone(),
            message,
            signature_hex: hex::encode(signature.to_bytes()),
        })
    }
}

/// Verify-only authority: the sandbox holds no signing material.
#[derive(Clone)]
pub struct SandboxJobVerifier {
    keys: BTreeMap<String, VerifyingKey>,
}
impl SandboxJobVerifier {
    pub fn new(keys: BTreeMap<String, [u8; 32]>) -> Result<Self, &'static str> {
        if keys.is_empty() || keys.len() > 16 {
            return Err("sandbox_verifier_keys_invalid");
        }
        let keys = keys
            .into_iter()
            .map(|(id, bytes)| {
                if !valid_text(&id, 128) {
                    return Err("sandbox_key_id_invalid");
                }
                VerifyingKey::from_bytes(&bytes)
                    .map(|key| (id, key))
                    .map_err(|_| "sandbox_public_key_invalid")
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { keys })
    }
    pub fn verify<'a>(
        &self,
        sealed: &'a SealedSandboxMessage,
        now: u64,
    ) -> Result<&'a SandboxMessage, &'static str> {
        sealed.message.validate(now)?;
        let key = self.keys.get(&sealed.key_id).ok_or("sandbox_key_unknown")?;
        let bytes = hex::decode(&sealed.signature_hex).map_err(|_| "sandbox_signature_invalid")?;
        let signature = Signature::from_slice(&bytes).map_err(|_| "sandbox_signature_invalid")?;
        key.verify_strict(&sealed.message.signing_bytes()?, &signature)
            .map_err(|_| "sandbox_signature_invalid")?;
        Ok(&sealed.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job() -> SandboxJob {
        SandboxJob {
            schema_version: SANDBOX_JOB_SCHEMA.into(),
            job_id: "job".into(),
            scope: CapabilityScopeV2 {
                tenant_id: "tenant".into(),
                user_id: "actor".into(),
                session_id: "session".into(),
            },
            run_id: "run".into(),
            tool_call_id: "call".into(),
            arguments_hash: format!("sha256:{}", "a".repeat(64)),
            issued_at_epoch_ms: 100,
            expires_at_epoch_ms: 1000,
            code: "print(1)".into(),
            inputs: serde_json::json!([]),
            limits: serde_json::json!({}),
        }
    }
    #[test]
    fn scope_content_operation_and_expiry_are_signed() {
        let signer = SandboxJobSigner::new("control".into(), [7; 32]).unwrap();
        let verifier = SandboxJobVerifier::new(BTreeMap::from([(
            "control".into(),
            signer.public_key_bytes(),
        )]))
        .unwrap();
        let sealed = signer.seal(SandboxMessage::Execute(job()), 100).unwrap();
        assert!(verifier.verify(&sealed, 100).is_ok());
        for field in ["scope", "code", "key", "operation"] {
            let mut altered = sealed.clone();
            match field {
                "scope" => {
                    if let SandboxMessage::Execute(job) = &mut altered.message {
                        job.scope.tenant_id = "other".into();
                    }
                }
                "code" => {
                    if let SandboxMessage::Execute(job) = &mut altered.message {
                        job.code = "print(2)".into();
                    }
                }
                "key" => altered.key_id = "unknown".into(),
                _ => {
                    altered.message = SandboxMessage::Poll(SandboxAccess {
                        job_id: "job".into(),
                        scope: job().scope,
                        job_digest: job().digest().unwrap(),
                        issued_at_epoch_ms: 100,
                        expires_at_epoch_ms: 1000,
                    })
                }
            }
            assert!(verifier.verify(&altered, 100).is_err(), "{field}");
        }
        assert!(verifier.verify(&sealed, 1000).is_err());
    }
    #[test]
    fn wire_cannot_carry_platform_lease_or_credentials() {
        let mut value = serde_json::to_value(job()).unwrap();
        for forbidden in [
            "lease",
            "signature",
            "internal_token",
            "database_url",
            "lease_signing_secret",
        ] {
            value[forbidden] = Value::String("credential".into());
            assert!(serde_json::from_value::<SandboxJob>(value.clone()).is_err());
            value.as_object_mut().unwrap().remove(forbidden);
        }
    }
}
