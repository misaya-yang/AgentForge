//! Knowledge workload signing and verification-only Gateway lease trust.
//! Reuses the existing purpose-separated wire, never Gateway signing material.
#[path = "../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lease_v3.rs"]
#[allow(dead_code)]
pub mod signature;
use crate::{KnowledgeError, Result, migration_knowledge_query as query};
use ai_platform_service_primitives::knowledge_model::{
    KnowledgeWorkloadSigner, ModelLease, ServiceModelLeaseVerifier,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Default)]
pub struct ModelPlaneSettings {
    pub origin: String,
    pub workload_key_file: Option<PathBuf>,
    pub gateway_public_keys_file: Option<PathBuf>,
    pub model_revisions: BTreeMap<String, String>,
}
fn now_ms() -> std::result::Result<u64, &'static str> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_millis() as u64)
        .map_err(|_| "model_plane_clock_invalid")
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadKeyFile {
    key_id: String,
    seed_hex: String,
    public_keys: signature::PublicKeyring,
}
pub struct WorkloadSigner {
    key_id: String,
    seed: [u8; 32],
    keys: signature::PublicKeyring,
}
impl KnowledgeWorkloadSigner for WorkloadSigner {
    fn sign(
        &self,
        path: &str,
        body: &[u8],
        tenant: &str,
        user: &str,
    ) -> std::result::Result<String, &'static str> {
        let reserve = ai_platform_service_primitives::knowledge_model::RESERVE_PATH;
        if path != reserve
            && !(path.starts_with(&format!("{reserve}/")) && path.ends_with("/execute"))
        {
            return Err("knowledge_workload_path_forbidden");
        }
        let issued = now_ms()?;
        let claims = json!({"caller_service":"knowledge-service","audience":"gateway","method":"POST","path":path,"body_sha256":format!("{:x}",Sha256::digest(body)),"tenant_id":tenant,"user_id":user,"nonce":uuid::Uuid::new_v4().to_string()});
        let envelope = signature::SignedEnvelope::issue(
            &self.seed,
            &self.key_id,
            "knowledge-model-workload",
            claims,
            issued,
            issued + 45_000,
            &self.keys,
        )?;
        signature::encode(signature::ENVELOPE_PREFIX, &envelope)
    }
}
pub struct GatewayLeaseVerifier {
    pub keys: signature::PublicKeyring,
}
impl ServiceModelLeaseVerifier for GatewayLeaseVerifier {
    fn verify(&self, token: &str, expected: &ModelLease) -> std::result::Result<(), &'static str> {
        let envelope: signature::SignedEnvelope =
            signature::decode(signature::ENVELOPE_PREFIX, token)?;
        self.keys
            .verify(&envelope, "knowledge-model-lease", now_ms()?)?;
        let claims = json!({"schema_version":"knowledge-model-lease/v1","lease_id":expected.lease_id,"call_id":expected.call_id,"request_sha256":expected.request_sha256,"tenant_id":expected.tenant_id,"user_id":expected.user_id,"model":expected.model,"expires_at_unix":expected.expires_at_unix});
        if envelope.claims != claims
            || envelope.expires_at_ms
                != expected
                    .expires_at_unix
                    .checked_mul(1000)
                    .ok_or("model_lease_expiry_invalid")?
        {
            return Err("model_lease_binding_invalid");
        }
        Ok(())
    }
}
fn read_controlled(path: &Path, private: bool) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        return Err(KnowledgeError::Configuration("model_plane_key_path"));
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| KnowledgeError::Configuration("model_plane_key_file"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 128 * 1024 {
        return Err(KnowledgeError::Configuration("model_plane_key_file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = metadata.permissions().mode();
        if permissions & 0o022 != 0 || (private && permissions & 0o077 != 0) {
            return Err(KnowledgeError::Configuration("model_plane_key_permissions"));
        }
    }
    std::fs::read(path).map_err(|_| KnowledgeError::Configuration("model_plane_key_file"))
}
impl ModelPlaneSettings {
    pub fn construct(&self) -> Result<Arc<query::model_plane::ModelPlaneClient>> {
        let keyfile = self
            .workload_key_file
            .as_deref()
            .ok_or(KnowledgeError::Configuration("knowledge_workload_key_file"))?;
        let pubfile =
            self.gateway_public_keys_file
                .as_deref()
                .ok_or(KnowledgeError::Configuration(
                    "gateway_model_public_keys_file",
                ))?;
        let own: WorkloadKeyFile = serde_json::from_slice(&read_controlled(keyfile, true)?)
            .map_err(|_| KnowledgeError::Configuration("knowledge_workload_key_format"))?;
        own.public_keys
            .validate()
            .map_err(|_| KnowledgeError::Configuration("knowledge_workload_keyring"))?;
        let seed = signature::unhex::<32>(&own.seed_hex)
            .map_err(|_| KnowledgeError::Configuration("knowledge_workload_seed"))?;
        let keys: signature::PublicKeyring =
            serde_json::from_slice(&read_controlled(pubfile, false)?)
                .map_err(|_| KnowledgeError::Configuration("gateway_model_public_key_format"))?;
        keys.validate()
            .map_err(|_| KnowledgeError::Configuration("gateway_model_public_keys"))?;
        let signer = Arc::new(WorkloadSigner {
            key_id: own.key_id,
            seed,
            keys: own.public_keys,
        });
        // Validate own key admission and key/seed match without making a model call.
        signer
            .sign(
                ai_platform_service_primitives::knowledge_model::RESERVE_PATH,
                b"{}",
                "startup",
                "startup",
            )
            .map_err(|_| KnowledgeError::Configuration("knowledge_workload_key"))?;
        let origin = reqwest::Url::parse(&self.origin)
            .map_err(|_| KnowledgeError::Configuration("knowledge_model_plane_origin"))?;
        let client = query::model_plane::ModelPlaneClient::for_workload(
            origin,
            signer,
            Arc::new(GatewayLeaseVerifier { keys }),
        )
        .and_then(|client| client.with_model_revisions(self.model_revisions.clone()))
        .map_err(|_| KnowledgeError::Configuration("knowledge_model_plane"))?;
        Ok(Arc::new(client))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ring(seed: &[u8; 32], now: u64) -> signature::PublicKeyring {
        signature::PublicKeyring {
            schema_version: "gateway-verification-keys/v2".into(),
            keys: vec![signature::PublicKey {
                key_id: "k".into(),
                public_key_hex: signature::hex(
                    &ed25519_dalek::SigningKey::from_bytes(seed)
                        .verifying_key()
                        .to_bytes(),
                ),
                not_before_ms: now - 1000,
                not_after_ms: now + 120_000,
                revoked: false,
            }],
        }
    }
    #[test]
    fn lease_verifier_rejects_tampered_scope_purpose_and_signature() {
        use ai_platform_service_primitives::knowledge_model::ModelIdentity;
        let now = now_ms().unwrap();
        let seed = [29; 32];
        let keys = ring(&seed, now);
        let mut lease = ModelLease {
            lease_id: "l".into(),
            lease_token: String::new(),
            call_id: "c".into(),
            request_sha256: "a".repeat(64),
            tenant_id: "t".into(),
            user_id: "u".into(),
            model: ModelIdentity {
                provider: "p".into(),
                model: "m".into(),
                revision: "r".into(),
            },
            expires_at_unix: (now + 60_000) / 1000,
        };
        let claims = json!({"schema_version":"knowledge-model-lease/v1","lease_id":lease.lease_id,"call_id":lease.call_id,"request_sha256":lease.request_sha256,"tenant_id":lease.tenant_id,"user_id":lease.user_id,"model":lease.model,"expires_at_unix":lease.expires_at_unix});
        let envelope = signature::SignedEnvelope::issue(
            &seed,
            "k",
            "knowledge-model-lease",
            claims.clone(),
            now,
            lease.expires_at_unix * 1000,
            &keys,
        )
        .unwrap();
        let token = signature::encode(signature::ENVELOPE_PREFIX, &envelope).unwrap();
        let verifier = GatewayLeaseVerifier { keys: keys.clone() };
        assert!(verifier.verify(&token, &lease).is_ok());
        lease.tenant_id = "other".into();
        assert!(verifier.verify(&token, &lease).is_err());
        lease.tenant_id = "t".into();
        let wrong = signature::SignedEnvelope::issue(
            &seed,
            "k",
            "knowledge-model-workload",
            claims,
            now,
            lease.expires_at_unix * 1000,
            &keys,
        )
        .unwrap();
        assert!(
            verifier
                .verify(
                    &signature::encode(signature::ENVELOPE_PREFIX, &wrong).unwrap(),
                    &lease
                )
                .is_err()
        );
        let mut forged = envelope;
        forged.signature_hex = "0".repeat(128);
        assert!(
            verifier
                .verify(
                    &signature::encode(signature::ENVELOPE_PREFIX, &forged).unwrap(),
                    &lease
                )
                .is_err()
        );
    }
}

#[cfg(test)]
pub(crate) fn no_network_test_client() -> Arc<query::model_plane::ModelPlaneClient> {
    struct Deny;
    impl KnowledgeWorkloadSigner for Deny {
        fn sign(
            &self,
            _: &str,
            _: &[u8],
            _: &str,
            _: &str,
        ) -> std::result::Result<String, &'static str> {
            Err("test_graph_never_dispatches_models")
        }
    }
    impl ServiceModelLeaseVerifier for Deny {
        fn verify(&self, _: &str, _: &ModelLease) -> std::result::Result<(), &'static str> {
            Err("test_graph_never_accepts_model_leases")
        }
    }
    Arc::new(
        query::model_plane::ModelPlaneClient::for_workload(
            "http://127.0.0.1:1".parse().unwrap(),
            Arc::new(Deny),
            Arc::new(Deny),
        )
        .unwrap(),
    )
}
