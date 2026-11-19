//! Independent optional Valkey result tier. Coordination handles cannot enter this API.
use super::*;
use ai_platform_service_primitives::valkey::{
    CacheConnection, CacheKey, CacheKind, ServiceIdentity, ValkeyError,
};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
const MAX_BYTES: usize = 1_048_576;
#[derive(Clone)]
pub(super) struct SharedResultCache(CacheConnection);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u8,
    tenant: String,
    fingerprint: String,
    admitted_ms: u64,
    ttl_ms: u64,
    results: Vec<RetrieveResult>,
    metadata: Value,
}
pub(super) struct Hit {
    pub results: Vec<RetrieveResult>,
    pub metadata: Value,
    pub age: Duration,
    pub ttl: Duration,
}
fn now_ms() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| QueryError::DependenciesFailed)?
            .as_millis(),
    )
    .map_err(|_| QueryError::DependenciesFailed)
}
fn optional_error(error: ValkeyError) -> Result<()> {
    match error {
        ValkeyError::Transport | ValkeyError::Timeout | ValkeyError::Pressure => {
            tracing::warn!(error=%error,"optional Knowledge result cache degraded");
            Ok(())
        }
        _ => Err(QueryError::DependenciesFailed),
    }
}
fn decode(
    bytes: &[u8],
    tenant: &str,
    fingerprint: &str,
    ttl: Duration,
    now: u64,
) -> Result<Option<Hit>> {
    if bytes.len() > MAX_BYTES {
        return Err(QueryError::InvalidProviderResponse);
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| QueryError::InvalidProviderResponse)?;
    if envelope.schema != 1
        || envelope.tenant != tenant
        || envelope.fingerprint != fingerprint
        || envelope.ttl_ms == 0
        || envelope.ttl_ms > 300_000
    {
        return Err(QueryError::InvalidProviderResponse);
    }
    let Some(age) = now.checked_sub(envelope.admitted_ms) else {
        return Ok(None);
    };
    let age = Duration::from_millis(age);
    let admission_ttl = Duration::from_millis(envelope.ttl_ms);
    if age >= ttl.min(admission_ttl) {
        return Ok(None);
    }
    Ok(Some(Hit {
        results: envelope.results,
        metadata: envelope.metadata,
        age,
        ttl: admission_ttl,
    }))
}
impl SharedResultCache {
    pub(super) fn new(connection: CacheConnection) -> Self {
        Self(connection)
    }
    fn key(&self, tenant: &str, fingerprint: &str) -> Result<CacheKey> {
        if !matches!(
            self.0.service(),
            ServiceIdentity::KnowledgeApi | ServiceIdentity::KnowledgeWorker
        ) {
            return Err(QueryError::DependenciesFailed);
        }
        CacheKey::new(
            self.0.service(),
            CacheKind::Retrieval,
            tenant,
            fingerprint.as_bytes(),
        )
        .map_err(|_| QueryError::DependenciesFailed)
    }
    pub(super) async fn get(
        &self,
        tenant: &str,
        fingerprint: &str,
        ttl: Duration,
    ) -> Result<Option<Hit>> {
        let key = self.key(tenant, fingerprint)?;
        match self.0.get(&key).await {
            Ok(Some(bytes)) => decode(&bytes, tenant, fingerprint, ttl, now_ms()?),
            Ok(None) => Ok(None),
            Err(error) => {
                optional_error(error)?;
                Ok(None)
            }
        }
    }
    pub(super) async fn set(
        &self,
        tenant: &str,
        fingerprint: &str,
        results: &[RetrieveResult],
        metadata: &Value,
        ttl: Duration,
    ) -> Result<()> {
        // Remote tier intentionally has a five-minute upper bound, never extending
        // the original admission TTL. Short subsecond values remain local only.
        let seconds = ttl.as_secs().min(300);
        if seconds == 0 {
            return Ok(());
        }
        let key = self.key(tenant, fingerprint)?;
        let bytes = serde_json::to_vec(&Envelope {
            schema: 1,
            tenant: tenant.into(),
            fingerprint: fingerprint.into(),
            admitted_ms: now_ms()?,
            ttl_ms: seconds * 1000,
            results: results.to_vec(),
            metadata: metadata.clone(),
        })
        .map_err(|_| QueryError::InvalidProviderResponse)?;
        if bytes.len() > MAX_BYTES {
            return Ok(());
        }
        match self.0.set(&key, &bytes, seconds).await {
            Ok(()) => Ok(()),
            Err(error) => optional_error(error),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn bytes() -> Vec<u8> {
        serde_json::to_vec(&Envelope {
            schema: 1,
            tenant: "t".into(),
            fingerprint: "acl-generation-model-hash".into(),
            admitted_ms: 1000,
            ttl_ms: 2000,
            results: vec![],
            metadata: json!({"sentinel":true}),
        })
        .unwrap()
    }
    #[test]
    fn original_ttl_survives_promotion_and_cannot_be_revived_by_longer_configuration() {
        let hit = decode(
            &bytes(),
            "t",
            "acl-generation-model-hash",
            Duration::from_secs(20),
            2500,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.age, Duration::from_millis(1500));
        assert_eq!(hit.ttl, Duration::from_secs(2));
        assert!(
            decode(
                &bytes(),
                "t",
                "acl-generation-model-hash",
                Duration::from_secs(20),
                3000
            )
            .unwrap()
            .is_none()
        );
        assert!(
            decode(
                &bytes(),
                "t",
                "acl-generation-model-hash",
                Duration::from_secs(1),
                2000
            )
            .unwrap()
            .is_none()
        );
        assert!(
            decode(
                &bytes(),
                "t",
                "acl-generation-model-hash",
                Duration::from_secs(20),
                999
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn tenant_acl_generation_and_malformed_remote_values_fail_closed() {
        for (tenant, key) in [
            ("other", "acl-generation-model-hash"),
            ("t", "different-acl"),
            ("t", "different-generation"),
        ] {
            assert!(decode(&bytes(), tenant, key, Duration::from_secs(20), 1100).is_err());
        }
        assert!(decode(b"{}", "t", "key", Duration::from_secs(20), 1100).is_err());
        assert!(
            decode(
                &vec![0; MAX_BYTES + 1],
                "t",
                "key",
                Duration::from_secs(20),
                1100
            )
            .is_err()
        );
    }
    #[test]
    fn authentication_and_policy_are_never_cache_misses() {
        for error in [
            ValkeyError::Authentication,
            ValkeyError::ServerPolicy,
            ValkeyError::CacheScope,
            ValkeyError::Configuration,
            ValkeyError::CredentialFile,
        ] {
            assert!(optional_error(error).is_err());
        }
        for error in [
            ValkeyError::Transport,
            ValkeyError::Timeout,
            ValkeyError::Pressure,
        ] {
            assert!(optional_error(error).is_ok());
        }
    }
}
