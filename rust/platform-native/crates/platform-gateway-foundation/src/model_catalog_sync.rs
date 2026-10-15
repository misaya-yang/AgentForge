//! Canonical `src/services/llm/model_catalog_sync.py` orchestration.
//!
//! Construction is inert. Provider/model services and GET transport are injected;
//! no environment, database, credential source, HTTP client or task is created.
//! Discovery executes GET using a Google key query parameter or a compatible
//! provider Bearer header, with a 10-second timeout. It owns pooling/client cleanup; the
//! Python optional-client versus per-call-client ownership choice is not imposed
//! on the host. No live adapter is wired here.
//!
//! Discovery failures are swallowed only after runtime-key lookup, matching the
//! source try boundary. Metadata conversion errors and ordinary upsert errors
//! propagate. Upserts are ordered, nontransactional and may partially commit.
//! Dropping a future cancels the current await and never starts later upserts;
//! adapters own draining/commit ambiguity. Request/key Debug never prints secrets.
//!
//! JSON values bound Python's arbitrary objects; catalog integers use i64. Native
//! ModelService already omits detailed capability validation diagnostics, so its
//! adapter uses that existing error text. Other injected model services can retain
//! the original capability-conflict detail. No upstream response/error is logged.

use crate::{
    model_service::{
        ModelClock, ModelDatabase, ModelFields, ModelObserver, ModelPricingCache, ModelRow,
        ModelService, ModelServiceError,
    },
    provider_service::{
        ProviderCipher, ProviderClock, ProviderDatabase, ProviderEnvironment, ProviderGoogleAuth,
        ProviderHttp, ProviderService, ServiceError, ServiceFuture,
    },
    provider_templates::{
        CatalogModel, CatalogModelKwargs, ProviderTemplate, find_provider_template_for_config,
        get_provider_template,
    },
    usage_parser::{decimal_digit, python_value_to_string, python_whitespace},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, future::Future, pin::Pin};

/// Internal-only secret. No serialization and no plaintext in Debug.
pub struct CatalogApiKey(String);
impl CatalogApiKey {
    /// Source `str(api_key) if api_key else None`, for runtime configuration only.
    pub fn from_runtime_value(value: &Value) -> Option<Self> {
        truthy(value).then(|| Self(python_value_to_string(value)))
    }
}
impl fmt::Debug for CatalogApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CatalogApiKey(<internal>)")
    }
}

pub trait CatalogProviders: Send + Sync {
    fn get_provider<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, Option<Value>>;
    fn get_runtime_api_key<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, Option<CatalogApiKey>>;
}
impl<D, C, H, G, E, T> CatalogProviders for ProviderService<D, C, H, G, E, T>
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
    fn get_runtime_api_key<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
    ) -> ServiceFuture<'a, Option<CatalogApiKey>> {
        Box::pin(async move {
            let config = self.get_runtime_provider_config(tenant, provider).await?;
            Ok(config
                .as_internal_value()
                .get("api_key")
                .and_then(CatalogApiKey::from_runtime_value))
        })
    }
}

pub struct CatalogUpsert {
    pub status: String,
    pub model: ModelRow,
    pub capability_changed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogUpsertError {
    /// The source catches ModelCapabilityError only, then continues the loop.
    Capability(String),
    Service(ModelServiceError),
}
pub type CatalogUpsertFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CatalogUpsert, CatalogUpsertError>> + Send + 'a>>;
pub trait CatalogModels: Send + Sync {
    fn upsert_model_from_catalog<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
        kwargs: CatalogModelKwargs,
    ) -> CatalogUpsertFuture<'a>;
}
impl<D: ModelDatabase, P: ModelPricingCache, C: ModelClock, O: ModelObserver> CatalogModels
    for ModelService<D, P, C, O>
{
    fn upsert_model_from_catalog<'a>(
        &'a self,
        tenant: &'a str,
        provider: &'a str,
        kwargs: CatalogModelKwargs,
    ) -> CatalogUpsertFuture<'a> {
        Box::pin(async move {
            let fields = ModelFields {
                display_name: kwargs.display_name,
                context_window: kwargs.context_window,
                max_output_tokens: kwargs.max_output_tokens,
                supports_vision: kwargs.supports_vision,
                supports_tools: kwargs.supports_tools,
                input_price_per_1k: kwargs.input_price_per_1k,
                output_price_per_1k: kwargs.output_price_per_1k,
                access_level: kwargs.access_level,
                sort_order: kwargs.sort_order,
                catalog_capabilities: kwargs
                    .catalog_capabilities
                    .map(|v| {
                        v.as_object()
                            .cloned()
                            .ok_or(CatalogUpsertError::Service(ModelServiceError::InvalidRow))
                    })
                    .transpose()?,
            };
            let (status, model, capability_changed) = ModelService::upsert_model_from_catalog(
                self,
                tenant,
                provider,
                &kwargs.model_id,
                fields,
                true,
            )
            .await
            .map_err(|error| match error {
                ModelServiceError::Capability => CatalogUpsertError::Capability(error.to_string()),
                other => CatalogUpsertError::Service(other),
            })?;
            Ok(CatalogUpsert {
                status: status.into(),
                model,
                capability_changed,
            })
        })
    }
}

/// Contains credentials. Only trusted transport implementations may inspect it.
pub struct CatalogRequest {
    pub method: &'static str,
    pub url: String,
    pub params: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub timeout_seconds: u64,
}
impl fmt::Debug for CatalogRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CatalogRequest(<internal>)")
    }
}
/// The service consults the JSON result only after checking status. A transport
/// may decode eagerly; wire decoding/encoding and client cleanup are adapter work.
pub struct CatalogResponse {
    pub status_code: u16,
    pub json: Result<Value, CatalogHttpError>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogHttpError {
    Transport,
    Timeout,
    InvalidJson,
}
pub type CatalogHttpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CatalogResponse, CatalogHttpError>> + Send + 'a>>;
pub trait CatalogHttp: Send + Sync {
    fn get(&self, request: CatalogRequest) -> CatalogHttpFuture<'_>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogSyncError {
    ProviderNotFound,
    Provider(ServiceError),
    Model(ModelServiceError),
    InvalidProvider,
    InvalidDiscovery,
    InvalidMetadata,
    /// Python int(string) ValueError, without retaining the submitted value.
    InvalidIntegerLiteral,
    IntegerOverflow,
    InvalidModel,
}
impl fmt::Display for CatalogSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderNotFound => f.write_str("PROVIDER_NOT_FOUND"),
            Self::Provider(error) => error.fmt(f),
            Self::Model(error) => error.fmt(f),
            _ => write!(f, "model_catalog_sync_{self:?}"),
        }
    }
}
impl std::error::Error for CatalogSyncError {}
impl From<ServiceError> for CatalogSyncError {
    fn from(error: ServiceError) -> Self {
        Self::Provider(error)
    }
}

pub struct ModelCatalogSyncService<'a> {
    provider_service: &'a dyn CatalogProviders,
    model_service: &'a dyn CatalogModels,
    http_client: &'a dyn CatalogHttp,
}
impl<'a> ModelCatalogSyncService<'a> {
    pub fn new(
        provider_service: &'a dyn CatalogProviders,
        model_service: &'a dyn CatalogModels,
        http_client: &'a dyn CatalogHttp,
    ) -> Self {
        Self {
            provider_service,
            model_service,
            http_client,
        }
    }
    pub async fn sync_provider_models(
        &self,
        tenant_id: &str,
        provider_id: &str,
        discover: bool,
    ) -> Result<Value, CatalogSyncError> {
        let provider = self
            .provider_service
            .get_provider(tenant_id, provider_id)
            .await?
            .filter(truthy)
            .ok_or(CatalogSyncError::ProviderNotFound)?;
        let provider_row = provider
            .as_object()
            .ok_or(CatalogSyncError::InvalidProvider)?;
        let api_type = optional_string(provider_row.get("api_type"))?;
        let base_url = optional_string(provider_row.get("base_url"))?;
        let template =
            find_provider_template_for_config(provider_id, api_type, base_url).or_else(|| {
                matches!(
                    api_type
                        .map(|value| value.trim().to_ascii_lowercase())
                        .as_deref(),
                    Some("openai" | "openai-compatible")
                )
                .then(|| get_provider_template("custom-openai-compatible"))
                .flatten()
            });
        let Some(template) = template else {
            return Ok(json!({"provider_id": provider_id, "template_id": null,
                "created_models": [], "updated_models": [], "skipped_models": [],
                "discovery_warnings": [format!("No trusted provider template is available for {provider_id}.")]}));
        };
        let mut catalog: BTreeMap<_, _> = template
            .default_models
            .iter()
            .map(|entry| (entry.model_id.clone(), entry.clone()))
            .collect();
        let mut skipped = Vec::new();
        let mut warnings = Vec::new();
        let discovered = if discover {
            let outcome = self.discover_models(tenant_id, &provider, template).await?;
            warnings.extend(outcome.warnings);
            outcome.models
        } else {
            Vec::new()
        };
        for item in &discovered {
            let Some(id) = item
                .get("model_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            if let Some(base) = catalog.get_mut(id) {
                *base = merge_discovered_metadata(base, item)?;
            } else {
                skipped.push(
                    json!({"model_id": id, "reason": "discovered_model_not_in_trusted_catalog"}),
                );
            }
        }
        if discover
            && matches!(
                template.discovery_strategy.as_str(),
                "google_ai_studio_models_list"
                    | "vertex_best_effort"
                    | "openai_compatible_best_effort"
            )
            && discovered.is_empty()
        {
            warnings.push(if template.default_models.is_empty() {
                "discovery_no_models: No usable models were discovered; configure models manually or fix the reported discovery issue.".into()
            } else {
                format!("{} discovery returned no usable catalog models; used trusted catalog defaults.", template.display_name)
            });
        }
        if !discovered.is_empty() && template.default_models.is_empty() {
            warnings.push("discovery_requires_review: Discovered model IDs are listed as skipped because this provider has no trusted capability catalog. Add models explicitly and verify their capabilities before enabling them.".into());
        }
        let mut entries: Vec<_> = catalog.into_values().collect();
        entries.sort_by(|a, b| {
            b.sort_order
                .cmp(&a.sort_order)
                .then(a.model_id.cmp(&b.model_id))
        });
        let mut created = Vec::new();
        let mut updated = Vec::new();
        let mut changed = Vec::new();
        for entry in entries {
            let upsert = match self
                .model_service
                .upsert_model_from_catalog(
                    tenant_id,
                    provider_id,
                    entry.to_model_kwargs(Some(provider_id)),
                )
                .await
            {
                Ok(value) => value,
                Err(CatalogUpsertError::Capability(error)) => {
                    skipped.push(json!({"model_id": entry.model_id,
                        "reason": format!("capability_override_conflict: {error}")}));
                    continue;
                }
                Err(CatalogUpsertError::Service(error)) => {
                    return Err(CatalogSyncError::Model(error));
                }
            };
            let mut payload = serde_json::Map::new();
            for field in ["model_id", "provider_id", "display_name", "is_enabled"] {
                payload.insert(
                    field.into(),
                    upsert
                        .model
                        .get(field)
                        .cloned()
                        .ok_or(CatalogSyncError::InvalidModel)?,
                );
            }
            let payload = Value::Object(payload);
            if upsert.capability_changed {
                changed.push(payload.clone());
            }
            if upsert.status == "created" {
                created.push(payload);
            } else {
                updated.push(payload);
            }
        }
        Ok(
            json!({"provider_id": provider_id, "template_id": template.template_id,
            "created_models": created, "updated_models": updated, "skipped_models": skipped,
            "discovery_warnings": warnings, "capability_changed_models": changed}),
        )
    }
    async fn discover_models(
        &self,
        tenant: &str,
        provider: &Value,
        template: &ProviderTemplate,
    ) -> Result<DiscoveryOutcome, CatalogSyncError> {
        let strategy = template.discovery_strategy.as_str();
        let (default_base, path, vertex) = match strategy {
            "google_ai_studio_models_list" => (
                "https://generativelanguage.googleapis.com",
                "/v1beta/models",
                false,
            ),
            "vertex_best_effort" => (
                "https://aiplatform.googleapis.com",
                "/v1/publishers/google/models",
                true,
            ),
            "openai_compatible_best_effort" => ("", "/models", false),
            _ => return Ok(DiscoveryOutcome::default()),
        };
        let compatible = strategy == "openai_compatible_best_effort";
        let id = provider
            .get("provider_id")
            .and_then(Value::as_str)
            .ok_or(CatalogSyncError::InvalidProvider)?;
        // Credential-service failures still propagate. Never misreport an internal
        // credential failure as an upstream model-list result.
        let Some(key) = self
            .provider_service
            .get_runtime_api_key(tenant, id)
            .await?
        else {
            return Ok(DiscoveryOutcome::warning(
                "discovery_credentials_unavailable: Configure provider credentials before discovering models.",
            ));
        };
        if vertex && key.0.trim_start_matches(python_whitespace).starts_with('{') {
            return Ok(DiscoveryOutcome::warning(
                "discovery_credentials_unsupported: Model discovery cannot use a service-account document as an API key; trusted defaults were retained.",
            ));
        }
        let base = provider
            .get("base_url")
            .filter(|v| truthy(v))
            .map(python_value_to_string)
            .unwrap_or_else(|| default_base.into());
        let base = base.trim().trim_end_matches('/');
        // Reuse the gateway's /v1/models contract, while accepting an already
        // versioned custom base. No endpoint is inferred for custom providers.
        let url = if compatible {
            let Ok(parsed) = reqwest::Url::parse(base) else {
                return Ok(DiscoveryOutcome::warning(
                    "discovery_base_url_invalid: Configure an absolute HTTP(S) provider base URL.",
                ));
            };
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Ok(DiscoveryOutcome::warning(
                    "discovery_base_url_invalid: Use an HTTP(S) base URL without credentials, query parameters or fragments.",
                ));
            }
            if base.ends_with("/v1") {
                format!("{base}{path}")
            } else {
                format!("{base}/v1{path}")
            }
        } else {
            format!("{base}{path}")
        };
        let (params, headers) = if compatible {
            (
                Vec::new(),
                vec![("Authorization".into(), format!("Bearer {}", key.0))],
            )
        } else {
            (vec![("key".into(), key.0)], Vec::new())
        };
        let request = CatalogRequest {
            method: "GET",
            url,
            params,
            headers,
            timeout_seconds: 10,
        };
        let response = match self.http_client.get(request).await {
            Ok(response) => response,
            Err(CatalogHttpError::Timeout) => {
                return Ok(DiscoveryOutcome::warning(
                    "discovery_timeout: Model discovery timed out; check provider connectivity and retry.",
                ));
            }
            Err(_) => {
                return Ok(DiscoveryOutcome::warning(
                    "discovery_transport_failed: Model discovery could not reach the provider; check its URL and connectivity.",
                ));
            }
        };
        if response.status_code != 200 {
            let warning = match response.status_code {
                401 | 403 => {
                    "discovery_access_denied: The provider rejected model-list access; check credentials and permissions."
                }
                404 | 405 => {
                    "discovery_endpoint_unsupported: This provider does not expose the expected model-list endpoint; configure models manually."
                }
                429 => {
                    "discovery_rate_limited: The provider rate-limited model discovery; retry later."
                }
                500..=599 => {
                    "discovery_provider_unavailable: The provider is temporarily unavailable; retry later."
                }
                _ => {
                    "discovery_http_failed: The provider did not return a successful model list; check its configuration."
                }
            };
            return Ok(DiscoveryOutcome::warning(warning));
        }
        let Ok(payload) = response.json else {
            return Ok(DiscoveryOutcome::warning(
                "discovery_invalid_json: The provider returned an unreadable model list; check its API protocol.",
            ));
        };
        let parsed = if compatible {
            parse_openai_models(&payload)
        } else {
            parse_google_models(&payload)
        };
        let Ok(mut models) = parsed else {
            return Ok(DiscoveryOutcome::warning(
                "discovery_invalid_response: The provider returned an incompatible model-list shape; check its API protocol.",
            ));
        };
        let mut warnings = Vec::new();
        if models.len() > MAX_DISCOVERED_MODELS
            || (compatible
                && payload
                    .get("data")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| rows.len() > MAX_DISCOVERED_MODELS))
        {
            models.truncate(MAX_DISCOVERED_MODELS);
            warnings.push("discovery_result_limit: Model discovery was limited to 1000 models; the result is incomplete.".into());
        }
        if payload
            .get("nextPageToken")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
            || payload.get("has_more").and_then(Value::as_bool) == Some(true)
        {
            warnings.push("discovery_more_pages: The provider has additional model-list pages; this sync used the first page only.".into());
        }
        Ok(DiscoveryOutcome { models, warnings })
    }
}

const MAX_DISCOVERED_MODELS: usize = 1000;

#[derive(Default)]
struct DiscoveryOutcome {
    models: Vec<Value>,
    warnings: Vec<String>,
}
impl DiscoveryOutcome {
    fn warning(message: &str) -> Self {
        Self {
            models: Vec::new(),
            warnings: vec![message.into()],
        }
    }
}

/// Model-list membership is discovery evidence only. It never asserts tools,
/// vision, context limits, price or runtime compatibility. Unknown identifiers
/// remain skipped by the trusted-catalog admission step above.
pub fn parse_openai_models(payload: &Value) -> Result<Vec<Value>, CatalogSyncError> {
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or(CatalogSyncError::InvalidDiscovery)?;
    let mut models = BTreeMap::new();
    for row in rows.iter().take(MAX_DISCOVERED_MODELS + 1) {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
            .ok_or(CatalogSyncError::InvalidDiscovery)?;
        models
            .entry(id.to_owned())
            .or_insert_with(|| json!({"model_id": id}));
    }
    let result = models.into_values().collect();
    Ok(result)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn optional_string(value: Option<&Value>) -> Result<Option<&str>, CatalogSyncError> {
    value
        .filter(|v| truthy(v))
        .map(|v| v.as_str().ok_or(CatalogSyncError::InvalidProvider))
        .transpose()
}

pub fn parse_google_models(payload: &Value) -> Result<Vec<Value>, CatalogSyncError> {
    let object = payload
        .as_object()
        .ok_or(CatalogSyncError::InvalidDiscovery)?;
    let Some(rows) = object.get("models") else {
        return Ok(Vec::new());
    };
    // Empty strings/dicts are iterable but yield no rows in Python.
    if matches!(rows, Value::String(v) if v.is_empty())
        || matches!(rows, Value::Object(v) if v.is_empty())
    {
        return Ok(Vec::new());
    }
    let rows = rows.as_array().ok_or(CatalogSyncError::InvalidDiscovery)?;
    let mut items = Vec::new();
    for row in rows {
        let row = row.as_object().ok_or(CatalogSyncError::InvalidDiscovery)?;
        let name = row
            .get("name")
            .filter(|v| truthy(v))
            .map(python_value_to_string)
            .unwrap_or_default();
        let model_id = name
            .trim_matches(python_whitespace)
            .rsplit('/')
            .next()
            .unwrap_or("");
        if model_id.is_empty() {
            continue;
        }
        if let Some(methods) = row.get("supportedGenerationMethods").filter(|v| truthy(v)) {
            let contains = match methods {
                Value::Array(v) => v.iter().any(|v| v == "generateContent"),
                Value::String(v) => v.contains("generateContent"),
                Value::Object(v) => v.contains_key("generateContent"),
                _ => return Err(CatalogSyncError::InvalidDiscovery),
            };
            if !contains {
                continue;
            }
        }
        items.push(json!({"model_id": model_id,
            "display_name": row.get("displayName").filter(|v| truthy(v)).cloned().unwrap_or(json!(model_id)),
            "context_window": row.get("inputTokenLimit"), "max_output_tokens": row.get("outputTokenLimit")}));
    }
    Ok(items)
}

pub fn merge_discovered_metadata(
    base: &CatalogModel,
    discovered: &Value,
) -> Result<CatalogModel, CatalogSyncError> {
    let discovered = discovered
        .as_object()
        .ok_or(CatalogSyncError::InvalidMetadata)?;
    let mut result = base.clone();
    if let Some(display) = discovered.get("display_name").filter(|v| truthy(v)) {
        result.display_name = python_value_to_string(display);
    }
    if let Some(value) = discovered.get("context_window").filter(|v| truthy(v)) {
        result.context_window = python_int(value)?;
    }
    if let Some(value) = discovered.get("max_output_tokens").filter(|v| truthy(v)) {
        result.max_output_tokens = python_int(value)?;
    }
    Ok(result)
}
fn python_int(value: &Value) -> Result<i64, CatalogSyncError> {
    match value {
        Value::Bool(v) => Ok(i64::from(*v)),
        Value::Number(v) if v.is_i64() => Ok(v.as_i64().expect("i64")),
        Value::Number(v) if v.is_u64() => Err(CatalogSyncError::IntegerOverflow),
        Value::Number(v) => {
            let number = v.as_f64().ok_or(CatalogSyncError::InvalidMetadata)?.trunc();
            if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number) {
                return Err(CatalogSyncError::IntegerOverflow);
            }
            Ok(number as i64)
        }
        Value::String(v) => {
            let text = v.trim_matches(char::is_whitespace);
            let mut chars = text.chars().peekable();
            let mut normalized = String::new();
            if chars.peek().is_some_and(|c| matches!(c, '+' | '-')) {
                normalized.push(chars.next().expect("sign"));
            }
            let mut previous_digit = false;
            let mut digits = 0usize;
            while let Some(c) = chars.next() {
                if let Some(digit) = decimal_digit(c) {
                    digits += 1;
                    // CPython's default decimal int-string limit counts leading
                    // zeros and Unicode decimal digits, but not sign/underscores.
                    if digits > 4300 {
                        return Err(CatalogSyncError::InvalidIntegerLiteral);
                    }
                    normalized.push(char::from(b'0' + digit));
                    previous_digit = true;
                } else if c == '_'
                    && previous_digit
                    && chars.peek().and_then(|c| decimal_digit(*c)).is_some()
                {
                    previous_digit = false;
                } else {
                    return Err(CatalogSyncError::InvalidIntegerLiteral);
                }
            }
            if !previous_digit {
                return Err(CatalogSyncError::InvalidIntegerLiteral);
            }
            normalized
                .parse()
                .map_err(|_| CatalogSyncError::IntegerOverflow)
        }
        _ => Err(CatalogSyncError::InvalidMetadata),
    }
}
