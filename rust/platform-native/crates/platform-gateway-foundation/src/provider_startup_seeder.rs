//! Source-faithful startup orchestration from `src/services/llm/startup_seeder.py`.
//!
//! Nothing runs on construction: environment values, endpoint resolution, provider
//! storage, catalog synchronization and logging are all explicitly injected. This
//! module registers no startup hook, reads no process environment, creates no
//! credentials, and opens no connection. Dropping a pending call drops its awaited
//! collaborator; no tasks are spawned and already-completed writes are not undone.
//!
//! ProviderService owns encryption and database behavior. The seeder passes selected
//! keys only to that trusted collaborator and compares its internal runtime key to
//! avoid re-encryption/updated_at churn. In particular a missing encryption key may
//! retain the canonical cipher's legacy plaintext behavior; no new cipher or secret
//! persistence policy is introduced here. Keys/endpoint results never derive Debug
//! or Serialize. Source exception interpolation is deliberately restricted to safe
//! error classes, never raw exception text, URLs, response bodies or credentials.
//!
//! `ChatEndpointResolver` transcribes the canonical config/endpoints.py chat helpers,
//! including Python whitespace and eager environment-read order. The default six
//! provider IDs equal the canonical ModelProvider enum; custom DB IDs are ignored.
//! Catalog synchronization delegates to its existing service, never to templates or
//! model writes directly. An absent catalog collaborator means one or both required
//! source services are absent. JSON service records bound Python's dynamic objects.

use crate::provider_service::{
    CreateProvider, ProviderCipher, ProviderClock, ProviderDatabase, ProviderEnvironment,
    ProviderGoogleAuth, ProviderHttp, ProviderService, ServiceError, ServiceFuture, UpdateProvider,
};
use crate::usage_parser::{python_value_to_string, python_whitespace};
use serde_json::Value;
use std::{collections::BTreeSet, fmt, future::Future, pin::Pin};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSeedResult {
    /// Environment order first, followed by the enabled database listing order.
    pub configured_providers: Vec<String>,
    /// DB-only and gateway-only legacy credentials cannot prove runtime readiness.
    pub runtime_configured_providers: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderSeedConfig {
    pub provider_id: &'static str,
    pub display_name: &'static str,
    pub api_type: &'static str,
    pub base_url: &'static str,
    pub env_key: &'static str,
    pub env_base_url: Option<&'static str>,
}

pub const DEFAULT_PROVIDER_CONFIGS: [ProviderSeedConfig; 6] = [
    ProviderSeedConfig {
        provider_id: "openai",
        display_name: "OpenAI",
        api_type: "openai",
        base_url: "https://api.openai.com",
        env_key: "OPENAI_API_KEY",
        env_base_url: Some("OPENAI_BASE_URL"),
    },
    ProviderSeedConfig {
        provider_id: "anthropic",
        display_name: "Anthropic",
        api_type: "anthropic",
        base_url: "https://api.anthropic.com",
        env_key: "ANTHROPIC_API_KEY",
        env_base_url: Some("ANTHROPIC_BASE_URL"),
    },
    ProviderSeedConfig {
        provider_id: "deepseek",
        display_name: "DeepSeek",
        api_type: "openai",
        base_url: "https://api.deepseek.com",
        env_key: "DEEPSEEK_API_KEY",
        env_base_url: Some("DEEPSEEK_BASE_URL"),
    },
    ProviderSeedConfig {
        provider_id: "dashscope",
        display_name: "Qwen/DashScope",
        api_type: "openai",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode",
        env_key: "DASHSCOPE_API_KEY",
        env_base_url: Some("DASHSCOPE_CHAT_BASE_URL"),
    },
    ProviderSeedConfig {
        provider_id: "google",
        display_name: "Google Gemini",
        api_type: "google",
        base_url: "https://generativelanguage.googleapis.com",
        env_key: "GEMINI_API_KEY",
        env_base_url: None,
    },
    ProviderSeedConfig {
        provider_id: "google-vertex",
        display_name: "Google Vertex AI",
        api_type: "google-vertex",
        base_url: "https://aiplatform.googleapis.com",
        env_key: "VERTEX_API_KEY",
        env_base_url: None,
    },
];

pub struct StartupSeedSettings<'a> {
    pub tenant_id: &'a str,
    pub legacy_dashscope_api_key: &'a str,
}
impl Default for StartupSeedSettings<'_> {
    fn default() -> Self {
        Self {
            tenant_id: "default",
            legacy_dashscope_api_key: "",
        }
    }
}
impl fmt::Debug for StartupSeedSettings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartupSeedSettings(<internal>)")
    }
}

/// Internal key comparison only, never a response or log payload.
pub struct StartupRuntimeConfig {
    api_key: Option<String>,
}
impl StartupRuntimeConfig {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}
impl fmt::Debug for StartupRuntimeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartupRuntimeConfig(<internal>)")
    }
}

pub trait StartupProviderService: Send + Sync {
    fn get_provider<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, Option<Value>>;
    fn create_provider<'a>(
        &'a self,
        tenant: &'a str,
        input: CreateProvider,
    ) -> ServiceFuture<'a, Value>;
    fn update_provider<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
        input: UpdateProvider,
    ) -> ServiceFuture<'a, Option<Value>>;
    fn get_runtime_provider_config<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, StartupRuntimeConfig>;
    fn list_providers<'a>(
        &'a self,
        tenant: &'a str,
        include_disabled: bool,
    ) -> ServiceFuture<'a, Vec<Value>>;
}

impl<D, C, H, G, E, T> StartupProviderService for ProviderService<D, C, H, G, E, T>
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
    fn create_provider<'a>(
        &'a self,
        tenant: &'a str,
        input: CreateProvider,
    ) -> ServiceFuture<'a, Value> {
        Box::pin(ProviderService::create_provider(self, tenant, input))
    }
    fn update_provider<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
        input: UpdateProvider,
    ) -> ServiceFuture<'a, Option<Value>> {
        Box::pin(ProviderService::update_provider(
            self, tenant, provider, input,
        ))
    }
    fn get_runtime_provider_config<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, StartupRuntimeConfig> {
        Box::pin(async move {
            let config =
                ProviderService::get_runtime_provider_config(self, tenant, provider).await?;
            Ok(StartupRuntimeConfig::new(
                config
                    .as_internal_value()
                    .get("api_key")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ))
        })
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

pub struct DashscopeChatEndpoint {
    pub api_key: String,
    pub base_url: String,
}
pub struct GoogleChatEndpoint {
    pub api_key: String,
    pub base_url: String,
    pub backend: String,
}
impl fmt::Debug for DashscopeChatEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DashscopeChatEndpoint(<internal>)")
    }
}
impl fmt::Debug for GoogleChatEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleChatEndpoint(<internal>)")
    }
}
pub trait StartupEndpointResolver: Send + Sync {
    fn resolve_dashscope_chat(&self) -> DashscopeChatEndpoint;
    fn resolve_google_chat(&self) -> GoogleChatEndpoint;
}

/// Inert resolver over an explicitly supplied environment; never `std::env`.
pub struct ChatEndpointResolver<'a> {
    environment: &'a dyn ProviderEnvironment,
}
impl<'a> ChatEndpointResolver<'a> {
    pub fn new(environment: &'a dyn ProviderEnvironment) -> Self {
        Self { environment }
    }
    fn first(&self, names: &[&str]) -> String {
        // Python evaluates all os.environ.get arguments before calling _first.
        let values: Vec<_> = names
            .iter()
            .map(|name| self.environment.get(name).unwrap_or_default())
            .collect();
        values
            .iter()
            .map(|value| value.trim_matches(python_whitespace))
            .find(|value| !value.is_empty())
            .unwrap_or("")
            .to_owned()
    }
}
impl StartupEndpointResolver for ChatEndpointResolver<'_> {
    fn resolve_dashscope_chat(&self) -> DashscopeChatEndpoint {
        let api_key = self.first(&["DASHSCOPE_CHAT_API_KEY", "DASHSCOPE_API_KEY"]);
        let raw_url = self.first(&["DASHSCOPE_CHAT_BASE_URL", "DASHSCOPE_BASE_URL"]);
        let base_url = if raw_url.is_empty() {
            "https://dashscope.aliyuncs.com/compatible-mode".into()
        } else {
            let mut cleaned = raw_url.trim_end_matches('/');
            for suffix in [
                "/compatible-mode/v1/chat/completions",
                "/compatible-mode/v1",
                "/compatible-mode",
                "/api/v1",
            ] {
                if let Some(base) = cleaned.strip_suffix(suffix) {
                    cleaned = base;
                    break;
                }
            }
            format!("{cleaned}/compatible-mode")
        };
        DashscopeChatEndpoint { api_key, base_url }
    }
    fn resolve_google_chat(&self) -> GoogleChatEndpoint {
        let mut backend = self
            .first(&["GOOGLE_CHAT_BACKEND", "GOOGLE_API_BACKEND"])
            .to_lowercase();
        if backend != "vertex" && backend != "ai_studio" {
            backend = "ai_studio".into();
        }
        let studio_key = self.first(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
        let (api_key, base_url) = if backend == "vertex" {
            let vertex_key = self.first(&["VERTEX_CHAT_API_KEY", "VERTEX_API_KEY"]);
            (
                if vertex_key.is_empty() {
                    studio_key
                } else {
                    vertex_key
                },
                "https://aiplatform.googleapis.com",
            )
        } else {
            (studio_key, "https://generativelanguage.googleapis.com")
        };
        GoogleChatEndpoint {
            api_key,
            base_url: base_url.into(),
            backend,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupLogLevel {
    Debug,
    Info,
    Warning,
}
pub trait StartupLogger: Send + Sync {
    fn log(&self, level: StartupLogLevel, message: &str);
}
pub struct QuietStartupLogger;
impl StartupLogger for QuietStartupLogger {
    fn log(&self, _: StartupLogLevel, _: &str) {}
}

pub async fn seed_startup_providers(
    provider_service: Option<&dyn StartupProviderService>,
    environment: &dyn ProviderEnvironment,
    endpoints: &dyn StartupEndpointResolver,
    settings: StartupSeedSettings<'_>,
    log: &dyn StartupLogger,
) -> ProviderSeedResult {
    let mut configured_providers = Vec::new();
    let mut runtime_configured_providers = BTreeSet::new();
    let legacy_backend = environment.get("GOOGLE_API_BACKEND").unwrap_or_default();
    let legacy_models = environment.get("GOOGLE_VERTEX_MODELS").unwrap_or_default();
    if legacy_backend
        .trim_matches(python_whitespace)
        .to_lowercase()
        == "vertex"
    {
        log.log(StartupLogLevel::Warning, "GOOGLE_API_BACKEND=vertex is deprecated. Add a 'google-vertex' provider in the Service Management UI (or set VERTEX_API_KEY so the default google-vertex provider is seeded at startup) and remove GOOGLE_API_BACKEND from your environment.");
    }
    if !legacy_models.trim_matches(python_whitespace).is_empty() {
        log.log(StartupLogLevel::Warning, "GOOGLE_VERTEX_MODELS is deprecated. Use the 'google-vertex' provider instead — models configured under that provider route to Vertex without needing per-model env overrides.");
    }
    for config in DEFAULT_PROVIDER_CONFIGS {
        let id = config.provider_id;
        let mut api_key = environment.get(config.env_key).unwrap_or_default();
        let mut assistant_runtime_key = api_key.clone();
        let mut base_url = Some(
            config
                .env_base_url
                .and_then(|key| environment.get(key))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| config.base_url.into()),
        );
        let mut google_backend = "ai_studio".to_owned();
        if id == "dashscope" {
            let resolved = endpoints.resolve_dashscope_chat();
            api_key = resolved.api_key;
            assistant_runtime_key = api_key.clone();
            base_url = Some(resolved.base_url);
            if api_key.is_empty() {
                api_key = settings.legacy_dashscope_api_key.into();
            }
        }
        if id == "google" {
            let resolved = endpoints.resolve_google_chat();
            api_key = resolved.api_key;
            assistant_runtime_key = api_key.clone();
            google_backend = resolved.backend;
            // Canonical source ignores the resolved Google URL, retaining the
            // static Studio URL or passing None when routed to legacy Vertex.
            if google_backend == "vertex" {
                base_url = None;
            }
        }
        if !api_key.is_empty() {
            configured_providers.push(id.to_owned());
            if !assistant_runtime_key.is_empty() {
                runtime_configured_providers.insert(id.to_owned());
            }
            if id == "google" && google_backend == "vertex" {
                log.log(
                    StartupLogLevel::Info,
                    "Google provider routed to Vertex (env-seeded)",
                );
            }
        }
        if let Some(service) = provider_service
            && let Err(error) =
                sync_provider(service, settings.tenant_id, &config, base_url, api_key, log).await
        {
            log.log(
                StartupLogLevel::Warning,
                &format!("Failed to sync provider {id} to database: {error}"),
            );
        }
    }
    if let Some(service) = provider_service {
        match service.list_providers(settings.tenant_id, false).await {
            Ok(providers) => {
                for provider in providers {
                    let id_value = provider.get("provider_id");
                    let id = id_value.and_then(Value::as_str).unwrap_or("");
                    if configured_providers.iter().any(|value| value == id)
                        || !provider.get("has_api_key").is_some_and(truthy)
                    {
                        continue;
                    }
                    if DEFAULT_PROVIDER_CONFIGS
                        .iter()
                        .any(|config| config.provider_id == id)
                    {
                        configured_providers.push(id.to_owned());
                        log.log(
                            StartupLogLevel::Info,
                            &format!("Provider {id} loaded from database"),
                        );
                    } else {
                        let display_id = id_value.map(python_value_to_string).unwrap_or_default();
                        log.log(
                            StartupLogLevel::Debug,
                            &format!("Custom provider {display_id} not in enum, skipping"),
                        );
                    }
                }
            }
            Err(error) => log.log(
                StartupLogLevel::Warning,
                &format!("Failed to load providers from database: {error}"),
            ),
        }
    }
    ProviderSeedResult {
        configured_providers,
        runtime_configured_providers,
    }
}

async fn sync_provider(
    service: &dyn StartupProviderService,
    tenant: &str,
    config: &ProviderSeedConfig,
    base_url: Option<String>,
    api_key: String,
    log: &dyn StartupLogger,
) -> Result<(), ServiceError> {
    let existing = service.get_provider(tenant, config.provider_id).await?;
    if existing.as_ref().is_none_or(|value| !truthy(value)) {
        service
            .create_provider(
                tenant,
                CreateProvider {
                    provider_id: config.provider_id.into(),
                    display_name: config.display_name.into(),
                    api_type: config.api_type.into(),
                    base_url,
                    api_key: if api_key.is_empty() {
                        None
                    } else {
                        Some(api_key)
                    },
                    metadata: None,
                    is_enabled: true,
                },
            )
            .await?;
        log.log(
            StartupLogLevel::Info,
            &format!("Created provider {} in database", config.provider_id),
        );
    } else if !api_key.is_empty() {
        let existing = existing.as_ref().expect("checked existing row");
        if existing.get("has_api_key").is_some_and(truthy)
            && existing.get("api_type").and_then(Value::as_str) == Some(config.api_type)
            && (base_url.is_none()
                || existing.get("base_url").and_then(Value::as_str) == base_url.as_deref())
        {
            let runtime = service
                .get_runtime_provider_config(tenant, config.provider_id)
                .await?;
            if runtime.api_key.as_deref() == Some(api_key.as_str()) {
                return Ok(());
            }
        }
        service
            .update_provider(
                tenant,
                config.provider_id,
                UpdateProvider {
                    api_key: Some(api_key),
                    api_type: Some(config.api_type.into()),
                    base_url,
                    ..Default::default()
                },
            )
            .await?;
        log.log(
            StartupLogLevel::Info,
            &format!(
                "Updated runtime configuration for provider {}",
                config.provider_id
            ),
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupCatalogError {
    SyncFailed,
    InvalidResult,
}
impl fmt::Display for StartupCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SyncFailed => "startup_catalog_sync_failed",
            Self::InvalidResult => "startup_catalog_invalid_result",
        })
    }
}
impl std::error::Error for StartupCatalogError {}
pub type StartupCatalogFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, StartupCatalogError>> + Send + 'a>>;
pub trait StartupCatalogSync: Send + Sync {
    fn sync_provider_models<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
        discover: bool,
    ) -> StartupCatalogFuture<'a>;
}

impl StartupCatalogSync for crate::model_catalog_sync::ModelCatalogSyncService<'_> {
    fn sync_provider_models<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
        discover: bool,
    ) -> StartupCatalogFuture<'a> {
        Box::pin(async move {
            crate::model_catalog_sync::ModelCatalogSyncService::sync_provider_models(
                self, tenant, provider, discover,
            )
            .await
            .map_err(|_| StartupCatalogError::SyncFailed)
        })
    }
}

/// Inject the existing catalog service only when both source services are present.
/// Always disables discovery, runs providers sequentially, and continues after an
/// ordinary provider failure. No credentials or provider network calls are needed.
pub async fn sync_startup_model_catalog(
    catalog: Option<&dyn StartupCatalogSync>,
    configured_providers: &[String],
    tenant_id: &str,
    log: &dyn StartupLogger,
) {
    let Some(catalog) = catalog else {
        return;
    };
    for id in configured_providers.iter().collect::<BTreeSet<_>>() {
        let result = catalog
            .sync_provider_models(tenant_id, id, false)
            .await
            .and_then(|result| {
                Ok((
                    result_len(&result, "created_models")?,
                    result_len(&result, "updated_models")?,
                ))
            });
        match result {
            Ok((created, updated)) if created != 0 || updated != 0 => log.log(StartupLogLevel::Info, &format!("Synced startup model catalog provider_id={id} created={created} updated={updated}")),
            Ok(_) => {},
            Err(error) => log.log(StartupLogLevel::Warning, &format!("Failed to sync startup model catalog for provider {id}: {error}")),
        }
    }
}
fn result_len(result: &Value, field: &str) -> Result<usize, StartupCatalogError> {
    match result.get(field) {
        None if result.is_object() => Ok(0),
        Some(Value::Array(values)) => Ok(values.len()),
        Some(Value::Object(values)) => Ok(values.len()),
        Some(Value::String(value)) => Ok(value.chars().count()),
        _ => Err(StartupCatalogError::InvalidResult),
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
