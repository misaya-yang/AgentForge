//! Source startup provider seeding followed by trusted model-catalog sync.
//! The caller explicitly runs this against the same model services later served
//! by the Gateway. Merely constructing configuration performs no I/O.
use super::{BootstrapError, models::ModelServices, settings_projection::SettingsProjection};
use crate::{
    catalog_http::ReqwestCatalogHttp,
    migration_host::settings::HostSettings,
    model_catalog_sync::ModelCatalogSyncService,
    provider_service::ProviderEnvironment,
    provider_startup_seeder::{
        ChatEndpointResolver, ProviderSeedResult, StartupLogLevel, StartupLogger,
        StartupSeedSettings, seed_startup_providers, sync_startup_model_catalog,
    },
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Internal captured legacy credential; intentionally not Debug or Serialize.
pub struct StartupModelConfiguration {
    pub tenant_id: String,
    pub legacy_dashscope_api_key: String,
}
impl StartupModelConfiguration {
    pub fn from_settings(settings: &HostSettings) -> Result<Self, BootstrapError> {
        let environment = BTreeMap::new();
        let projection = SettingsProjection {
            settings,
            environment: &environment,
        };
        let knowledge = projection.section("knowledge")?;
        if !knowledge.is_object() {
            return Err(BootstrapError::Configuration("knowledge"));
        }
        let legacy_dashscope_api_key = match knowledge.get("dashscope") {
            None => String::new(),
            Some(Value::Object(dashscope)) => match dashscope.get("api_key") {
                None => String::new(),
                Some(Value::String(key)) => key.clone(),
                _ => return Err(BootstrapError::Configuration("knowledge.dashscope.api_key")),
            },
            _ => return Err(BootstrapError::Configuration("knowledge.dashscope")),
        };
        Ok(Self {
            tenant_id: "default".into(),
            legacy_dashscope_api_key,
        })
    }
}

struct CapturedEnvironment<'a>(&'a BTreeMap<String, String>);
impl ProviderEnvironment for CapturedEnvironment<'_> {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// Seeder errors already use safe classes. Additionally suppress untrusted
/// custom IDs and pass every emitted message through the shared redactors.
pub struct RedactedStartupLogger;
impl StartupLogger for RedactedStartupLogger {
    fn log(&self, level: StartupLogLevel, message: &str) {
        let message = if message.starts_with("Custom provider ") {
            "Custom provider not in enum, skipping"
        } else {
            message
        };
        let safe = crate::migration_compat::eval_observation::redact_preview(
            &crate::metrics_redaction::redact_sensitive_text(message),
            2048,
        )
        .replace(['\r', '\n'], " ");
        let level = match level {
            StartupLogLevel::Debug => "debug",
            StartupLogLevel::Info => "info",
            StartupLogLevel::Warning => "warning",
        };
        eprintln!("model_startup {level}: {safe}");
    }
}

/// This async boundary performs the canonical ordered database writes when
/// explicitly awaited by the process startup. Individual provider/catalog
/// failures preserve the seeder's log-and-continue behavior and partial writes.
///
/// The result's configured providers include eligible DB-only providers, while
/// runtime_configured_providers contains only source-proven runtime credentials.
/// Pass the latter to ModelServices::metadata after this operation completes.
/// No provider discovery is performed: catalog sync always uses discover=false.
pub async fn initialize(
    models: &ModelServices,
    config: &StartupModelConfiguration,
    environment: &BTreeMap<String, String>,
) -> ProviderSeedResult {
    let environment = CapturedEnvironment(environment);
    let endpoints = ChatEndpointResolver::new(&environment);
    let logger = RedactedStartupLogger;
    let result = seed_startup_providers(
        Some(models.providers.as_ref()),
        &environment,
        &endpoints,
        StartupSeedSettings {
            tenant_id: &config.tenant_id,
            legacy_dashscope_api_key: &config.legacy_dashscope_api_key,
        },
        &logger,
    )
    .await;
    // The concrete HTTP collaborator is inert and unused with discover=false;
    // it is the same native catalog transport used by provider administration.
    let http = ReqwestCatalogHttp::new();
    let catalog =
        ModelCatalogSyncService::new(models.providers.as_ref(), models.models.as_ref(), &http);
    sync_startup_model_catalog(
        Some(&catalog),
        &result.configured_providers,
        &config.tenant_id,
        &logger,
    )
    .await;
    result
}
