//! Bridge tenant model/provider management into the Gateway dispatcher without
//! persisting synthetic service rows. The normal dispatcher still owns model
//! authorization, quotas, admission, adapter execution, and usage accounting.
use super::models::{Models, Providers};
use crate::{
    openai_model_catalog::{
        OpenAiModelCatalog, OpenAiModelCatalogFuture, OpenAiModelCatalogUnavailable,
    },
    protocol::{
        ContentType, InvocationMode, ServiceAuthConfig, ServiceConfig, ServiceDefinition,
        ServiceType, TransportType,
    },
    provider_routing,
    provider_startup_seeder::DEFAULT_PROVIDER_CONFIGS,
    service_ownership::ServiceHandle,
    service_registry::MANAGED_OPENAI_SERVICE_ID_PREFIX,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

pub struct DatabaseOpenAiModelCatalog {
    models: Arc<Models>,
    providers: Arc<Providers>,
    environment: BTreeMap<String, String>,
}

impl DatabaseOpenAiModelCatalog {
    pub fn new(
        models: Arc<Models>,
        providers: Arc<Providers>,
        environment: BTreeMap<String, String>,
    ) -> Self {
        Self {
            models,
            providers,
            environment,
        }
    }

    async fn services(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<ServiceHandle>, OpenAiModelCatalogUnavailable> {
        let rows = self
            .models
            .list_models(tenant_id, None, true, None)
            .await
            .map_err(|_| OpenAiModelCatalogUnavailable)?;
        let mut counts = HashMap::<String, usize>::new();
        for row in &rows {
            if let Some(id) = row.get("model_id").and_then(Value::as_str) {
                *counts.entry(id.trim().to_owned()).or_default() += 1;
            }
        }

        let mut services = Vec::new();
        let mut provider_cache = HashMap::<String, Option<Value>>::new();
        for row in rows {
            let Some(model_id) = row
                .get("model_id")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
            else {
                continue;
            };
            // The OpenAI wire names models by model_id alone. Do not expose an
            // ambiguous name that the canonical model service cannot resolve.
            if counts.get(model_id) != Some(&1) || row.get("is_enabled") != Some(&Value::Bool(true))
            {
                continue;
            }
            let Some(provider_id) = row
                .get("provider_id")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
            else {
                continue;
            };
            let provider = if let Some(cached) = provider_cache.get(provider_id) {
                match cached {
                    Some(provider) => provider.clone(),
                    None => continue,
                }
            } else {
                let loaded = match self
                    .providers
                    .get_runtime_provider_config(tenant_id, provider_id)
                    .await
                {
                    Ok(provider) => Some(provider.into_internal_value()),
                    Err(crate::provider_service::ServiceError::ProviderNotFound) => None,
                    Err(_) => return Err(OpenAiModelCatalogUnavailable),
                };
                provider_cache.insert(provider_id.to_owned(), loaded.clone());
                let Some(provider) = loaded else {
                    continue;
                };
                provider
            };
            if provider.get("is_enabled") != Some(&Value::Bool(true)) {
                continue;
            }
            if let Some(service) =
                build_model_service(tenant_id, &row, &provider, &self.environment)
            {
                services.push(ServiceHandle::new(service));
            }
        }
        Ok(services)
    }
}

impl OpenAiModelCatalog for DatabaseOpenAiModelCatalog {
    fn services_for_tenant<'a>(&'a self, tenant_id: &'a str) -> OpenAiModelCatalogFuture<'a> {
        Box::pin(self.services(tenant_id))
    }
}

fn model_service_type(row: &Map<String, Value>) -> ServiceType {
    let capabilities = row
        .get("effective_capabilities")
        .or_else(|| row.get("catalog_capabilities"))
        .unwrap_or(&Value::Null);
    let declared = [
        row.get("service_type").and_then(Value::as_str),
        row.get("model_kind").and_then(Value::as_str),
        row.get("model_type").and_then(Value::as_str),
        row.get("task_type").and_then(Value::as_str),
        capabilities.get("kind").and_then(Value::as_str),
        capabilities.get("type").and_then(Value::as_str),
        capabilities.get("model_type").and_then(Value::as_str),
        capabilities.get("task_type").and_then(Value::as_str),
    ];
    if declared
        .iter()
        .flatten()
        .any(|v| v.eq_ignore_ascii_case("embedding"))
    {
        return ServiceType::Embedding;
    }
    if ["embedding", "embeddings"].iter().any(|kind| {
        capabilities.get("modalities").is_some_and(|modalities| {
            modalities
                .get("output")
                .and_then(Value::as_array)
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str().is_some_and(|v| v.eq_ignore_ascii_case(kind)))
                })
        })
    }) || capabilities.get("supports_embeddings") == Some(&Value::Bool(true))
    {
        return ServiceType::Embedding;
    }
    let model_id = row
        .get("model_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if model_id.contains("embedding") || model_id.contains("embed") {
        ServiceType::Embedding
    } else {
        ServiceType::Generative
    }
}

fn api_key(
    provider: &Value,
    provider_id: &str,
    environment: &BTreeMap<String, String>,
) -> Option<String> {
    provider
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            if provider.get("allow_environment_credentials") != Some(&Value::Bool(true)) {
                return None;
            }
            DEFAULT_PROVIDER_CONFIGS
                .iter()
                .find(|config| config.provider_id == provider_id)
                .and_then(|config| environment.get(config.env_key))
                .filter(|key| !key.trim().is_empty())
                .cloned()
        })
}

fn service_id(tenant: &str, provider: &str, model: &str) -> String {
    let digest = Sha256::digest(format!("{tenant}\0{provider}\0{model}").as_bytes());
    // usage_records.service_id is VARCHAR(64). Keep the reserved prefix and
    // 192 bits of the tenant/provider/model digest (48 hex chars) within it.
    const DIGEST_BYTES: usize = 24;
    let hex = digest[..DIGEST_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{MANAGED_OPENAI_SERVICE_ID_PREFIX}{hex}")
}

fn build_model_service(
    tenant_id: &str,
    model: &Map<String, Value>,
    provider: &Value,
    environment: &BTreeMap<String, String>,
) -> Option<ServiceDefinition> {
    let model_id = model.get("model_id")?.as_str()?.trim();
    let provider_id = model.get("provider_id")?.as_str()?.trim();
    let access_level = model.get("access_level")?.as_str()?;
    if !["public", "premium", "admin"].contains(&access_level) {
        return None;
    }
    let runtime_provider = provider
        .get("runtime_provider")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| provider_routing::to_runtime_provider(provider));
    if !matches!(runtime_provider.as_str(), "openai" | "dashscope") {
        return None;
    }
    let key = api_key(provider, provider_id, environment)?;
    let raw_base = provider
        .get("runtime_base_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| provider_routing::normalize_runtime_base_url(provider))?;
    let base_url = raw_base
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or(raw_base.trim_end_matches('/'))
        .to_owned();
    if base_url.is_empty() {
        return None;
    }

    let kind = model_service_type(model);
    let model_name = model
        .get("display_name")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(model_id);
    let mut service =
        ServiceDefinition::new(service_id(tenant_id, provider_id, model_id), model_name);
    service.description = format!("Managed {runtime_provider} model");
    service.service_type = kind;
    service.supported_modes = if kind == ServiceType::Embedding {
        vec![InvocationMode::Sync]
    } else {
        vec![InvocationMode::Sync, InvocationMode::Stream]
    };
    service.connector_type = TransportType::Http;
    service.connector_config = json!({
        "base_url": base_url,
        "headers": { "authorization": format!("Bearer {key}") }
    });
    service.accepted_content_types = vec![ContentType::Text];
    service.output_content_types = if kind == ServiceType::Embedding {
        vec![ContentType::Json]
    } else {
        vec![ContentType::Text]
    };
    service.metadata = json!({
        "adapter_type": "openai",
        "model": model_id,
        "provider": provider_id,
        "openai_model_id": model_id,
        "openai_compat_scope": {
            "tenant_id": tenant_id,
            "access_level": access_level
        }
    });
    service.service_config = Some(ServiceConfig {
        auth: ServiceAuthConfig {
            enabled: true,
            require_auth: true,
            ..Default::default()
        },
        ..Default::default()
    });
    Some(service)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, provider: &str, kind: Option<&str>, access: &str) -> Map<String, Value> {
        let mut row = Map::from_iter([
            ("model_id".into(), json!(id)),
            ("provider_id".into(), json!(provider)),
            ("display_name".into(), json!(id)),
            ("is_enabled".into(), json!(true)),
            ("access_level".into(), json!(access)),
        ]);
        if let Some(kind) = kind {
            row.insert("task_type".into(), json!(kind));
        }
        row
    }

    fn provider(api_type: &str, key: Option<&str>) -> Value {
        json!({
            "provider_id":"dashscope", "tenant_id":"tenant-a", "api_type":api_type,
            "is_enabled":true, "runtime_provider":if api_type == "google" {"gemini"} else {"dashscope"},
            "runtime_base_url":"https://dashscope.aliyuncs.com/compatible-mode/v1",
            "api_key":key, "allow_environment_credentials":false
        })
    }

    #[test]
    fn managed_service_ids_fit_usage_columns_and_are_stable_and_tenant_scoped() {
        let id = service_id("tenant-a", "provider", "model");
        assert!(id.starts_with(MANAGED_OPENAI_SERVICE_ID_PREFIX));
        assert!(id.is_ascii());
        assert!(id.len() <= 64);
        assert_eq!(id, service_id("tenant-a", "provider", "model"));
        assert_ne!(id, service_id("tenant-b", "provider", "model"));
        assert_eq!(id.len(), MANAGED_OPENAI_SERVICE_ID_PREFIX.len() + 48);
    }

    #[test]
    fn managed_model_projects_to_openai_dispatch_service_without_persisted_service_creation() {
        let model = model("qwen3.8-flash", "dashscope", Some("chat"), "premium");
        let service = build_model_service(
            "tenant-a",
            &model,
            &provider("openai", Some("sensitive-test-key")),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(service.service_id.starts_with("managed-openai-"));
        assert!(service.service_id.len() <= 64);
        assert_eq!(service.metadata["adapter_type"], "openai");
        assert_eq!(service.metadata["openai_model_id"], "qwen3.8-flash");
        assert_eq!(
            service.metadata["openai_compat_scope"]["tenant_id"],
            "tenant-a"
        );
        assert_eq!(
            service.connector_config["base_url"],
            "https://dashscope.aliyuncs.com/compatible-mode"
        );
        assert_eq!(
            service.connector_config["headers"]["authorization"],
            "Bearer sensitive-test-key"
        );
        assert_eq!(service.service_type, ServiceType::Generative);
        assert_eq!(
            service.supported_modes,
            vec![InvocationMode::Sync, InvocationMode::Stream]
        );
    }

    #[test]
    fn managed_embedding_model_is_sync_only_and_unusable_credentials_fail_closed() {
        let model = model(
            "text-embedding-3-small",
            "dashscope",
            Some("embedding"),
            "public",
        );
        let service = build_model_service(
            "tenant-a",
            &model,
            &provider("openai", Some("key")),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(service.service_type, ServiceType::Embedding);
        assert_eq!(service.supported_modes, vec![InvocationMode::Sync]);
        assert!(
            build_model_service(
                "tenant-a",
                &model,
                &provider("openai", None),
                &BTreeMap::new()
            )
            .is_none()
        );
        assert!(
            build_model_service(
                "tenant-a",
                &model,
                &provider("google", Some("key")),
                &BTreeMap::new()
            )
            .is_none()
        );
        let mut missing_access = model.clone();
        missing_access.remove("access_level");
        assert!(
            build_model_service(
                "tenant-a",
                &missing_access,
                &provider("openai", Some("key")),
                &BTreeMap::new(),
            )
            .is_none()
        );
    }

    #[test]
    fn environment_key_fallback_requires_provider_policy_and_known_mapping() {
        let model = model("qwen3.8-flash", "dashscope", Some("chat"), "public");
        let mut provider = provider("openai", None);
        provider["allow_environment_credentials"] = json!(true);
        let environment = BTreeMap::from([("DASHSCOPE_API_KEY".into(), "test-env-key".into())]);
        assert!(build_model_service("tenant-a", &model, &provider, &environment).is_some());
        provider["allow_environment_credentials"] = json!(false);
        assert!(build_model_service("tenant-a", &model, &provider, &environment).is_none());
    }
}
