//! Canonical `gateway_model_meta.py` and `provider_setup.py` read-only facade.
//!
//! Reuses the existing model/provider services; constructors only retain injected
//! collaborators and normalize runtime provider identifiers. No cache, connection,
//! credentials, HTTP client, task, or environment lookup is created here. Dropping
//! an operation drops its current collaborator future and never starts later reads.
//! Service errors are retained as typed sources rather than converted to absence.
//!
//! Rows retain JSON values (including Python truthiness and str coercion); model
//! count keys follow the canonical service's string provider-id contract. Invalid
//! non-string count keys are rejected rather than silently stringified. Arbitrary
//! Python objects and non-string dictionary keys are outside that native contract.

use crate::{
    model_service::{
        ModelClock, ModelDatabase, ModelFuture, ModelObserver, ModelPricingCache, ModelRow,
        ModelService, ModelServiceError,
    },
    provider_service::{
        ProviderCipher, ProviderClock, ProviderDatabase, ProviderEnvironment, ProviderGoogleAuth,
        ProviderHttp, ProviderService, ServiceError, ServiceFuture,
    },
    service_registry::python_truthy,
    usage_parser::{python_value_to_string, python_whitespace},
};
use serde_json::{Map, Value};
use std::{collections::HashSet, error::Error, fmt};

/// A service-level seam; `None` means the source omitted provider_id entirely.
pub trait MetadataModels: Send + Sync {
    fn get_model<'a>(
        &'a self,
        tenant_id: &'a str,
        model_id: &'a str,
        provider_id: Option<&'a str>,
    ) -> ModelFuture<'a, Option<ModelRow>>;
    fn list_models<'a>(
        &'a self,
        tenant_id: &'a str,
        include_disabled: bool,
    ) -> ModelFuture<'a, Vec<ModelRow>>;
}
impl<D: ModelDatabase, P: ModelPricingCache, C: ModelClock, O: ModelObserver> MetadataModels
    for ModelService<D, P, C, O>
{
    fn get_model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> ModelFuture<'a, Option<ModelRow>> {
        Box::pin(ModelService::get_model(self, tenant, model, provider))
    }
    fn list_models<'a>(
        &'a self,
        tenant: &'a str,
        include_disabled: bool,
    ) -> ModelFuture<'a, Vec<ModelRow>> {
        Box::pin(ModelService::list_models(
            self,
            tenant,
            None,
            include_disabled,
            None,
        ))
    }
}

pub trait MetadataProviders: Send + Sync {
    fn get_provider<'a>(
        &'a self,
        tenant_id: &'a str,
        provider_id: &'a str,
    ) -> ServiceFuture<'a, Option<Value>>;
    fn list_providers<'a>(
        &'a self,
        tenant_id: &'a str,
        include_disabled: bool,
    ) -> ServiceFuture<'a, Vec<Value>>;
}
impl<D, C, H, G, E, T> MetadataProviders for ProviderService<D, C, H, G, E, T>
where
    D: ProviderDatabase,
    C: ProviderCipher,
    H: ProviderHttp,
    G: ProviderGoogleAuth,
    E: ProviderEnvironment,
    T: ProviderClock,
{
    fn get_provider<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, Option<Value>> {
        Box::pin(ProviderService::get_provider(self, tenant, provider))
    }
    fn list_providers<'a>(
        &'a self,
        tenant: &'a str,
        include_disabled: bool,
    ) -> ServiceFuture<'a, Vec<Value>> {
        Box::pin(ProviderService::list_providers(
            self,
            tenant,
            include_disabled,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataError {
    Model(ModelServiceError),
    Provider(ServiceError),
    /// Source list comprehension raises KeyError, not an absent-provider result.
    MissingProviderId,
    InvalidProviderRow,
    InvalidModelProviderId,
}
impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(error) => error.fmt(f),
            Self::Provider(error) => error.fmt(f),
            Self::MissingProviderId => f.write_str("missing provider_id"),
            Self::InvalidProviderRow => f.write_str("invalid provider metadata row"),
            Self::InvalidModelProviderId => f.write_str("invalid model provider_id"),
        }
    }
}
impl Error for MetadataError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model(error) => Some(error),
            Self::Provider(error) => Some(error),
            _ => None,
        }
    }
}
impl From<ModelServiceError> for MetadataError {
    fn from(error: ModelServiceError) -> Self {
        Self::Model(error)
    }
}
impl From<ServiceError> for MetadataError {
    fn from(error: ServiceError) -> Self {
        Self::Provider(error)
    }
}

pub struct GatewayModelMeta<M, P> {
    pub model_service: M,
    pub provider_service: P,
    runtime_configured_providers: HashSet<String>,
}
impl<M: MetadataModels, P: MetadataProviders> GatewayModelMeta<M, P> {
    pub fn new<I, V>(model_service: M, provider_service: P, runtime_configured_providers: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Into<Value>,
    {
        Self {
            model_service,
            provider_service,
            runtime_configured_providers: runtime_configured_providers
                .into_iter()
                .map(|value| {
                    python_value_to_string(&value.into())
                        .trim_matches(python_whitespace)
                        .to_owned()
                })
                .filter(|value| !value.is_empty())
                .collect(),
        }
    }

    pub async fn get_access_level(
        &self,
        tenant_id: &str,
        model_id: &str,
        provider_id: Option<&str>,
    ) -> Result<Option<Value>, MetadataError> {
        let row = self
            .model_service
            .get_model(
                tenant_id,
                model_id,
                provider_id.filter(|provider| !provider.is_empty()),
            )
            .await?;
        // Missing and explicit null both become Python None. Falsy non-null
        // values are returned unchanged; the source performs no validation.
        Ok(row.and_then(|row| row.get("access_level").filter(|v| !v.is_null()).cloned()))
    }

    pub async fn is_provider_configured(
        &self,
        tenant_id: &str,
        provider_id: &str,
    ) -> Result<bool, MetadataError> {
        let Some(row) = self
            .provider_service
            .get_provider(tenant_id, provider_id)
            .await?
            .filter(python_truthy)
        else {
            return Ok(false);
        };
        let row = row.as_object().ok_or(MetadataError::InvalidProviderRow)?;
        Ok(row.get("is_enabled").is_some_and(python_truthy)
            && (row.get("has_api_key").is_some_and(python_truthy)
                || self.runtime_configured_providers.contains(provider_id)))
    }

    /// Preserve service order, duplicates, whitespace, and raw JSON identifiers.
    pub async fn list_enabled_providers(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<Value>, MetadataError> {
        self.provider_service
            .list_providers(tenant_id, false)
            .await?
            .into_iter()
            .map(|row| {
                row.as_object()
                    .ok_or(MetadataError::InvalidProviderRow)?
                    .get("provider_id")
                    .cloned()
                    .ok_or(MetadataError::MissingProviderId)
            })
            .collect()
    }

    /// Insertion order matches Python dict, including whitespace-only string IDs.
    pub async fn count_enabled_models_by_provider(
        &self,
        tenant_id: &str,
    ) -> Result<Map<String, Value>, MetadataError> {
        let rows = self.model_service.list_models(tenant_id, false).await?;
        let mut counts = Map::new();
        for row in rows {
            if let Some(provider) = row.get("provider_id").filter(|v| python_truthy(v)) {
                let provider = provider
                    .as_str()
                    .ok_or(MetadataError::InvalidModelProviderId)?;
                let count = counts.get(provider).and_then(Value::as_u64).unwrap_or(0) + 1;
                counts.insert(provider.to_owned(), Value::from(count));
            }
        }
        Ok(counts)
    }
}

/// Canonical `provider_setup.configured_providers`; reads are strictly sequential.
/// Mark seen before awaiting, so even an unconfigured duplicate is checked once.
pub async fn configured_providers<M: MetadataModels, P: MetadataProviders>(
    model_meta: &GatewayModelMeta<M, P>,
    tenant_id: &str,
) -> Result<Vec<String>, MetadataError> {
    let enabled = model_meta.list_enabled_providers(tenant_id).await?;
    let mut configured = Vec::new();
    let mut seen = HashSet::new();
    for raw_provider_id in enabled {
        let provider_id = if python_truthy(&raw_provider_id) {
            python_value_to_string(&raw_provider_id)
                .trim_matches(python_whitespace)
                .to_owned()
        } else {
            String::new()
        };
        if provider_id.is_empty() || !seen.insert(provider_id.clone()) {
            continue;
        }
        if model_meta
            .is_provider_configured(tenant_id, &provider_id)
            .await?
        {
            configured.push(provider_id);
        }
    }
    Ok(configured)
}
