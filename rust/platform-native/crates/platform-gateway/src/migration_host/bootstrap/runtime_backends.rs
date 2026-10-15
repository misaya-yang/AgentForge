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
        let primary_secret = environment
            .get("AI_PLATFORM_INTERNAL_TOKEN")
            .map(|v| v.trim())
            .unwrap_or("")
            .to_owned();
        let mut keys = BTreeMap::new();
        let mut first = None;
        for item in environment
            .get("INTERNAL_AUTH_KEYS")
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
        {
            let Some((id, secret)) = item.trim().split_once(':') else {
                continue;
            };
            let id = id.trim();
            let secret = secret.trim();
            if id.is_empty() || secret.is_empty() {
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
            keys.insert(active.clone(), primary_secret.clone());
        } else if !keys.contains_key(&active) {
            active = first.ok_or(BootstrapError::Configuration("INTERNAL_AUTH_KEYS"))?;
        }
        if primary_secret.chars().count() < 16
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
