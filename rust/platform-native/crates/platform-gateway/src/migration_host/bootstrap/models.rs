//! Reuses the mature ProviderService/ModelService and durable operation executors.
//! Google credential exchange and mutable pricing-cache invalidation stay explicit
//! platform backend inputs; neither is approximated with a success callback.
use super::{BootstrapError, DatabasePools};
use crate::{
    model_provider_storage::{PgModelDatabase, PgProviderDatabase},
    model_service::{ModelPricingCache, ModelService, ModelServiceError},
    provider_cipher::NativeProviderCipher,
    provider_probe_http::ReqwestProviderHttp,
    provider_service::{
        GoogleCredentialState, ProviderCipher, ProviderClock, ProviderEnvironment,
        ProviderGoogleAuth, ProviderService, ServiceFuture,
    },
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
pub struct PricingCacheAdapter(pub Arc<dyn ModelPricingCache>);
impl ModelPricingCache for PricingCacheAdapter {
    fn invalidate_cache(&self) -> Result<(), ModelServiceError> {
        self.0.invalidate_cache()
    }
}
pub struct GoogleAuthAdapter(pub Arc<dyn ProviderGoogleAuth>);
impl ProviderGoogleAuth for GoogleAuthAdapter {
    fn acquire<'a>(
        &'a self,
        account: Option<Value>,
        scopes: &'a [&'a str],
    ) -> ServiceFuture<'a, GoogleCredentialState> {
        self.0.acquire(account, scopes)
    }
    fn refresh<'a>(&'a self, state: GoogleCredentialState) -> ServiceFuture<'a, String> {
        self.0.refresh(state)
    }
}
pub struct ExplicitEnvironment(pub BTreeMap<String, String>);
impl ProviderEnvironment for ExplicitEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}
pub struct UtcProviderClock;
impl ProviderClock for UtcProviderClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
pub type Models = ModelService<PgModelDatabase, PricingCacheAdapter>;
pub type Providers = ProviderService<
    PgProviderDatabase,
    NativeProviderCipher,
    ReqwestProviderHttp,
    GoogleAuthAdapter,
    ExplicitEnvironment,
    UtcProviderClock,
>;
pub struct ModelServices {
    pub pricing: Arc<dyn ModelPricingCache>,
    pub models: Arc<Models>,
    pub providers: Arc<Providers>,
}

/// Validate global provider configuration before accepting application traffic.
/// Startup reads only the protected credential column and emits no row content.
pub async fn validate_stored_credentials(
    pools: &DatabasePools,
    encryption_key: &str,
) -> Result<(), BootstrapError> {
    let records: Vec<String> = sqlx::query_scalar(
        "SELECT api_key_encrypted FROM gateway.llm_providers WHERE COALESCE(api_key_encrypted,'') <> ''",
    )
    .fetch_all(&pools.data)
    .await
    .map_err(|_| BootstrapError::Construction("provider_credential_validation"))?;
    for sealed in records {
        NativeProviderCipher
            .decrypt(&sealed, encryption_key)
            .map_err(|_| BootstrapError::Configuration("provider_credential_encryption"))?;
    }
    Ok(())
}
impl ModelServices {
    pub fn new(
        pools: &DatabasePools,
        encryption_key: String,
        environment: BTreeMap<String, String>,
        pricing: Arc<dyn ModelPricingCache>,
        google: Arc<dyn ProviderGoogleAuth>,
    ) -> Result<Self, BootstrapError> {
        let models = Arc::new(ModelService::new(
            PgModelDatabase::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("model_database"))?,
            PricingCacheAdapter(pricing.clone()),
        ));
        let providers = Arc::new(ProviderService::new(
            PgProviderDatabase::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("provider_database"))?,
            NativeProviderCipher,
            ReqwestProviderHttp,
            GoogleAuthAdapter(google),
            ExplicitEnvironment(environment),
            UtcProviderClock,
            encryption_key,
        ));
        Ok(Self {
            models,
            providers,
            pricing,
        })
    }
}

/// Arc projections preserve the same cache/DB services used by administration.
pub struct SharedMetadataModels(pub Arc<Models>);
impl crate::gateway_model_meta::MetadataModels for SharedMetadataModels {
    fn get_model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> crate::model_service::ModelFuture<'a, Option<crate::model_service::ModelRow>> {
        crate::gateway_model_meta::MetadataModels::get_model(
            self.0.as_ref(),
            tenant,
            model,
            provider,
        )
    }
    fn list_models<'a>(
        &'a self,
        tenant: &'a str,
        disabled: bool,
    ) -> crate::model_service::ModelFuture<'a, Vec<crate::model_service::ModelRow>> {
        crate::gateway_model_meta::MetadataModels::list_models(self.0.as_ref(), tenant, disabled)
    }
}
pub struct SharedMetadataProviders(pub Arc<Providers>);
impl crate::gateway_model_meta::MetadataProviders for SharedMetadataProviders {
    fn get_provider<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, Option<Value>> {
        crate::gateway_model_meta::MetadataProviders::get_provider(
            self.0.as_ref(),
            tenant,
            provider,
        )
    }
    fn list_providers<'a>(
        &'a self,
        tenant: &'a str,
        disabled: bool,
    ) -> ServiceFuture<'a, Vec<Value>> {
        crate::gateway_model_meta::MetadataProviders::list_providers(
            self.0.as_ref(),
            tenant,
            disabled,
        )
    }
}
impl ModelServices {
    pub fn metadata(
        &self,
        configured_providers: Vec<String>,
    ) -> Arc<
        crate::gateway_model_meta::GatewayModelMeta<SharedMetadataModels, SharedMetadataProviders>,
    > {
        Arc::new(crate::gateway_model_meta::GatewayModelMeta::new(
            SharedMetadataModels(self.models.clone()),
            SharedMetadataProviders(self.providers.clone()),
            configured_providers,
        ))
    }
}
impl crate::mcp_gateway_broker::McpEnvironment for ExplicitEnvironment {
    fn read(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

impl ModelServices {
    /// Config-only mature model/provider graph, including real mutable pricing
    /// invalidation and explicitly supplied Google credential configuration.
    pub fn native(
        pools: &DatabasePools,
        encryption_key: String,
        environment: BTreeMap<String, String>,
        cache_ttl_seconds: f64,
        google: crate::migration_host::google_auth::GoogleAuthConfig,
    ) -> Result<Self, BootstrapError> {
        let pricing = Arc::new(
            crate::migration_host::model_pricing::ModelPricingService::new(
                Some(pools.data.clone()),
                cache_ttl_seconds,
            ),
        );
        let google = Arc::new(
            crate::migration_host::google_auth::NativeGoogleAuth::new(google)
                .map_err(|_| BootstrapError::Configuration("google_auth"))?,
        );
        Self::new(pools, encryption_key, environment, pricing, google)
    }
}
