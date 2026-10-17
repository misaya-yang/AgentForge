//! Explicit Knowledge model-plane composition on the existing Gateway listener.
use super::{BootstrapError, graph::GatewayGraph};
use crate::knowledge_model_plane::{
    self, KnowledgeModelPlane, LeaseAuthority, WorkloadAuthentication, WorkloadVerifier,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

const PUBLIC_KEYS: &str = "AI_PLATFORM_KNOWLEDGE_MODEL_WORKLOAD_PUBLIC_KEYS_FILE";
const PRICE_BOOK: &str = "AI_PLATFORM_KNOWLEDGE_MODEL_PRICE_BOOK_REVISION";
const GATEWAY_KEY: &str = "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE";

pub(super) struct Configuration {
    gateway_key_file: PathBuf,
    workload_keys: knowledge_model_plane::signature::PublicKeyring,
    price_book: String,
}
impl Configuration {
    pub(super) fn from_environment(
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, BootstrapError> {
        if [PUBLIC_KEYS, PRICE_BOOK].iter().all(|key| {
            environment
                .get(*key)
                .is_none_or(|value| value.trim().is_empty())
        }) {
            return Ok(None);
        }
        let required = |key| {
            environment
                .get(key)
                .filter(|v| !v.trim().is_empty())
                .ok_or(BootstrapError::Configuration(key))
        };
        let gateway_key_file = PathBuf::from(required(GATEWAY_KEY)?);
        if !gateway_key_file.is_absolute() {
            return Err(BootstrapError::Configuration(GATEWAY_KEY));
        }
        let public = super::auth::key_file(required(PUBLIC_KEYS)?, false)?;
        let workload_keys: knowledge_model_plane::signature::PublicKeyring =
            serde_json::from_str(&public)
                .map_err(|_| BootstrapError::Configuration(PUBLIC_KEYS))?;
        workload_keys
            .validate()
            .map_err(|_| BootstrapError::Configuration(PUBLIC_KEYS))?;
        let price_book = required(PRICE_BOOK)?.clone();
        if price_book.len() > 128
            || !price_book
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        {
            return Err(BootstrapError::Configuration(PRICE_BOOK));
        }
        Ok(Some(Self {
            gateway_key_file,
            workload_keys,
            price_book,
        }))
    }
    pub(super) fn router(self, graph: &GatewayGraph) -> Result<axum::Router, BootstrapError> {
        let signer = Arc::new(
            LeaseAuthority::from_private_keyring_file(&self.gateway_key_file)
                .map_err(|_| BootstrapError::Configuration(GATEWAY_KEY))?,
        );
        let workload = Arc::new(
            WorkloadVerifier::new(
                graph.pools.data.clone(),
                WorkloadAuthentication::Ed25519 {
                    keys: self.workload_keys,
                },
            )
            .map_err(|_| BootstrapError::Configuration(PUBLIC_KEYS))?,
        );
        let plane = KnowledgeModelPlane::new(
            graph.pools.data.clone(),
            signer,
            workload,
            graph.models.providers.clone(),
            graph.core.admission.clone(),
            graph.core.capacity.clone(),
            self.price_book,
            graph.history_knowledge.clone(),
        )
        .map_err(|_| BootstrapError::Construction("knowledge_model_plane"))?;
        Ok(knowledge_model_plane::router(plane))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_model_plane_configuration_fails_instead_of_enabling_a_fallback() {
        assert!(
            Configuration::from_environment(&BTreeMap::new())
                .unwrap()
                .is_none()
        );
        assert!(
            Configuration::from_environment(&BTreeMap::from([
                (PUBLIC_KEYS.into(), String::new()),
                (PRICE_BOOK.into(), String::new())
            ]))
            .unwrap()
            .is_none()
        );
        for key in [PUBLIC_KEYS, PRICE_BOOK] {
            assert!(
                Configuration::from_environment(&BTreeMap::from([(
                    key.into(),
                    "configured".into()
                )]))
                .is_err()
            );
        }
    }
}
