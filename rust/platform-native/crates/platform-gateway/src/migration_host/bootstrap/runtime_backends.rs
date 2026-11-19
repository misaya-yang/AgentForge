//! Real configured Redis publication and frozen-memory cleanup collaborators.
//! No transport operation, file deletion, or credential lookup occurs on build.
use super::{BootstrapError, DatabasePools};
use crate::{
    migration_agent::memory_cleanup::{MemoryFiles, RuntimeMemoryCleanup},
    model_admin_api::{AdminServiceError, AdminServiceFuture, ModelConfigPublisher},
    provider_admin_api::{ProviderAdminBackendError, ProviderAdminFuture, ProviderAdminPublisher},
};
use redis::aio::ConnectionManager;
use reqwest::header::{HeaderMap, HeaderValue};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

/// Publication remains request-owned; cancelling the route drops this future.
/// The mature APIs ignore an ordinary publication error after their DB write.
pub struct RedisConfigurationPublisher {
    redis: ConnectionManager,
}
impl RedisConfigurationPublisher {
    pub fn new(redis: ConnectionManager) -> Self {
        Self { redis }
    }
    async fn send(&self, channel: &str, payload: &str) -> Result<(), ()> {
        let mut connection = self.redis.clone();
        let _: i64 = redis::cmd("PUBLISH")
            .arg(channel)
            .arg(payload)
            .query_async(&mut connection)
            .await
            .map_err(|_| ())?;
        Ok(())
    }
}
impl ModelConfigPublisher for RedisConfigurationPublisher {
    fn publish<'a>(&'a self, channel: &'a str, payload: &'a str) -> AdminServiceFuture<'a, ()> {
        Box::pin(async move {
            self.send(channel, payload)
                .await
                .map_err(|_| AdminServiceError::Unavailable)
        })
    }
}
impl ProviderAdminPublisher for RedisConfigurationPublisher {
    fn publish<'a>(&'a self, channel: &'a str, payload: &'a str) -> ProviderAdminFuture<'a, ()> {
        Box::pin(async move {
            self.send(channel, payload).await.map_err(|_| {
                ProviderAdminBackendError::exception("configuration_publication_unavailable")
            })
        })
    }
}

pub fn memory_cleanup(
    pools: &DatabasePools,
    environment: &BTreeMap<String, String>,
) -> Result<Arc<RuntimeMemoryCleanup>, BootstrapError> {
    let read = |name: &str| environment.get(name).map(String::as_str).unwrap_or("");
    let base = PathBuf::from(
        environment
            .get("AGENT_RUNTIME_MEMORY_DIR")
            .map(String::as_str)
            .unwrap_or("/app/agent-memory"),
    );
    // The host document must resolve relative deployment paths. This avoids
    // ambient working-directory inference in an authority-scoped file adapter.
    if !base.is_absolute()
        || base
            .components()
            .any(|v| matches!(v, std::path::Component::ParentDir))
    {
        return Err(BootstrapError::Configuration("AGENT_RUNTIME_MEMORY_DIR"));
    }
    let origin = if !read("GATEWAY_KNOWLEDGE__QDRANT__URL").is_empty() {
        read("GATEWAY_KNOWLEDGE__QDRANT__URL")
    } else {
        read("KNOWLEDGE_QDRANT__URL")
    }
    .trim()
    .trim_end_matches('/');
    let key = if !read("GATEWAY_KNOWLEDGE__QDRANT__API_KEY").is_empty() {
        read("GATEWAY_KNOWLEDGE__QDRANT__API_KEY")
    } else {
        read("KNOWLEDGE_QDRANT__API_KEY")
    }
    .trim();
    let mut headers = HeaderMap::new();
    if !key.is_empty() {
        headers.insert(
            "api-key",
            HeaderValue::from_str(key)
                .map_err(|_| BootstrapError::Configuration("memory_qdrant_key"))?,
        );
    }
    let vector_url = if origin.is_empty() {
        None
    } else {
        Some(super::endpoint(origin, "memory_qdrant_url")?)
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(28))
        .build()
        .map_err(|_| BootstrapError::Construction("memory_cleanup_http"))?;
    Ok(Arc::new(RuntimeMemoryCleanup {
        pool: pools.data.clone(),
        files: MemoryFiles { base },
        vector_url,
        vector_headers: headers,
        client,
    }))
}

/// Captured canonical comma-delimited rotation map and active signing selection.
/// The ordered first key is retained even though verification uses a sorted map.
/// Contains secrets: intentionally neither Debug nor Serialize.
pub struct InternalSigningConfiguration {
    pub primary_secret: String,
    pub keys: BTreeMap<String, String>,
    pub active_key_id: String,
    pub active_secret: String,
}
impl InternalSigningConfiguration {
    pub fn from_environment(
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, BootstrapError> {
        let mode = ai_platform_service_primitives::workload_identity::Mode::parse(
            environment
                .get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
                .map(String::as_str)
                .unwrap_or("disabled"),
        )
        .map_err(|_| BootstrapError::Configuration("workload_mode_invalid"))?;
        let required = mode == ai_platform_service_primitives::workload_identity::Mode::Required;
        let primary_secret = environment
            .get("AI_PLATFORM_INTERNAL_TOKEN")
            .map(|v| v.trim())
            .unwrap_or("")
            .to_owned();
        if required
            && (!primary_secret.is_empty()
                || environment.contains_key("AI_PLATFORM_INTERNAL_TOKEN_FILE"))
        {
            return Err(BootstrapError::Configuration(
                "workload_shared_bearer_forbidden",
            ));
        }
        let mut keys = BTreeMap::new();
        let mut first = None;
        for item in environment
            .get("INTERNAL_AUTH_KEYS")
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
        {
            let Some((id, secret)) = item.trim().split_once(':') else {
                if required && !item.trim().is_empty() {
                    return Err(BootstrapError::Configuration("INTERNAL_AUTH_KEYS"));
                }
                continue;
            };
            let id = id.trim();
            let secret = secret.trim();
            if id.is_empty() || secret.is_empty() || (required && keys.contains_key(id)) {
                if required {
                    return Err(BootstrapError::Configuration("INTERNAL_AUTH_KEYS"));
                }
                continue;
            }
            if first.is_none() {
                first = Some(id.to_owned())
            }
            keys.insert(id.to_owned(), secret.to_owned());
        }
        let mut active = environment
            .get("INTERNAL_AUTH_ACTIVE_KEY_ID")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("local")
            .to_owned();
        if keys.is_empty() {
            if required {
                return Err(BootstrapError::Configuration(
                    "internal_signing_keyring_required",
                ));
            }
            keys.insert(active.clone(), primary_secret.clone());
        } else if !keys.contains_key(&active) {
            active = first.ok_or(BootstrapError::Configuration("INTERNAL_AUTH_KEYS"))?;
        }
        if (!required && primary_secret.chars().count() < 16)
            || keys.iter().any(|(id, secret)| {
                id.is_empty() || id.contains(':') || secret.chars().count() < 16
            })
        {
            return Err(BootstrapError::Configuration(
                "internal_signing_configuration",
            ));
        }
        let active_secret = keys
            .get(&active)
            .cloned()
            .ok_or(BootstrapError::Configuration("internal_active_key"))?;
        Ok(Self {
            primary_secret,
            keys,
            active_key_id: active,
            active_secret,
        })
    }
}

/// A retired transport credential is never filled from a domain signing key.
pub(super) fn transport_token(
    environment: &BTreeMap<String, String>,
    name: &'static str,
) -> Result<String, BootstrapError> {
    let mode = ai_platform_service_primitives::workload_identity::Mode::parse(
        environment
            .get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
            .map(String::as_str)
            .unwrap_or("disabled"),
    )
    .map_err(|_| BootstrapError::Configuration("workload_mode_invalid"))?;
    let value = environment.get(name).map(|v| v.trim()).unwrap_or("");
    if mode == ai_platform_service_primitives::workload_identity::Mode::Required {
        if !value.is_empty() || environment.contains_key(&format!("{name}_FILE")) {
            return Err(BootstrapError::Configuration(
                "workload_shared_bearer_forbidden",
            ));
        }
        return Ok(String::new());
    }
    Ok(value.into())
}

#[cfg(test)]
mod required_transport_tests {
    use super::*;
    fn required() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
            (
                "INTERNAL_AUTH_KEYS".into(),
                "rotated:independent-domain-key-one,old:independent-domain-key-two".into(),
            ),
            ("INTERNAL_AUTH_ACTIVE_KEY_ID".into(), "rotated".into()),
        ])
    }
    #[test]
    fn required_keeps_rotating_domain_keys_without_a_transport_secret() {
        let config = InternalSigningConfiguration::from_environment(&required()).unwrap();
        assert!(config.primary_secret.is_empty());
        assert_eq!(config.active_key_id, "rotated");
        assert_eq!(config.active_secret, "independent-domain-key-one");
        assert_eq!(config.keys.len(), 2);
        for key in [
            "AI_PLATFORM_INTERNAL_TOKEN",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
        ] {
            assert_eq!(transport_token(&required(), key).unwrap(), "");
            for (name, value) in [
                (key.to_string(), "retired-shared-bearer"),
                (format!("{key}_FILE"), "/unread/retired"),
            ] {
                let mut env = required();
                env.insert(name, value.into());
                assert!(transport_token(&env, key).is_err());
            }
        }
    }
    #[test]
    fn required_rejects_missing_invalid_duplicate_or_transport_fallback_keys() {
        for keys in [
            "",
            "malformed",
            "id:short",
            "id:independent-domain-key,malformed",
            "id:independent-domain-key,id:another-independent-key",
        ] {
            let mut env = required();
            env.insert("INTERNAL_AUTH_KEYS".into(), keys.into());
            assert!(InternalSigningConfiguration::from_environment(&env).is_err());
        }
        for name in [
            "AI_PLATFORM_INTERNAL_TOKEN",
            "AI_PLATFORM_INTERNAL_TOKEN_FILE",
        ] {
            let mut env = required();
            env.insert(name.into(), "retired-shared-bearer".into());
            assert!(InternalSigningConfiguration::from_environment(&env).is_err());
        }
    }
    #[test]
    fn disabled_and_dual_preserve_their_legacy_secret_contract() {
        for mode in ["disabled", "dual"] {
            let mut env = BTreeMap::from([("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), mode.into())]);
            assert!(InternalSigningConfiguration::from_environment(&env).is_err());
            env.insert(
                "AI_PLATFORM_INTERNAL_TOKEN".into(),
                "legacy-transport-secret".into(),
            );
            let config = InternalSigningConfiguration::from_environment(&env).unwrap();
            assert_eq!(config.primary_secret, "legacy-transport-secret");
            assert_eq!(config.active_secret, "legacy-transport-secret");
            assert_eq!(
                transport_token(&env, "AI_PLATFORM_INTERNAL_TOKEN").unwrap(),
                "legacy-transport-secret"
            );
        }
    }
}
