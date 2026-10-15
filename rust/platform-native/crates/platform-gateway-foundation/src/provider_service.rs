//! Canonical `src/services/llm/provider_service.py` service orchestration.
//!
//! All collaborators are explicitly injected and construction is inert. No routes,
//! workers, environment reads, database connections, or provider traffic are started.
//! Database rows are trusted service records, not user request JSON. Tenant-scoped
//! SQL includes disabled rows except the default list. Delete remains two ordered
//! nontransactional statements, including the source's substring status check.
//! Dropping an in-flight future drops the awaited collaborator; no task is spawned.
//! Adapters must implement cancellation/draining before reusing external resources.
//!
//! Crypto authority: src/core/crypto.py reexports ai_gateway_core.security.secrets.
//! ProviderCipher MUST implement that exact Fernet enc: format and legacy passthrough
//! behavior (including absent key/invalid ciphertext); this module invents no cipher.
//! GoogleAuth owns official Google credentials construction and refresh, separately,
//! preserving the required-project check before refresh. No live adapters are wired.
//!
//! Deliberate safety difference: probe errors and non-success bodies use fixed safe
//! messages rather than source exception/body text, which can contain credentials.
//! Runtime configs, DB bindings, probe requests and credentials have redacted Debug
//! and no Serialize implementation. Extraction APIs are explicitly internal-only.
//! JSON/typed inputs bound the source's arbitrary Python values. UTC timestamps are
//! typed DB parameters. Metadata serialization is semantically JSON-equivalent;
//! formatting/order is not a wire promise for PostgreSQL jsonb.

use crate::provider_routing;
use crate::usage_parser::{python_value_to_string, python_whitespace};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use std::{fmt, future::Future, pin::Pin};

// Verbatim Python list_providers SQL literal.
pub const LIST_ALL_SQL: &str = r#"
                SELECT provider_id, tenant_id, display_name, api_type, base_url,
                       api_key_encrypted, metadata, is_enabled, created_at, updated_at
                FROM llm_providers
                WHERE tenant_id = $1
                ORDER BY display_name
            "#;

// Verbatim Python list_providers SQL literal.
pub const LIST_ENABLED_SQL: &str = r#"
                SELECT provider_id, tenant_id, display_name, api_type, base_url,
                       api_key_encrypted, metadata, is_enabled, created_at, updated_at
                FROM llm_providers
                WHERE tenant_id = $1 AND is_enabled = true
                ORDER BY display_name
            "#;

// Verbatim Python get_provider SQL literal.
pub const GET_SQL: &str = r#"
            SELECT provider_id, tenant_id, display_name, api_type, base_url,
                   api_key_encrypted, metadata, is_enabled, created_at, updated_at
            FROM llm_providers
            WHERE tenant_id = $1 AND provider_id = $2
        "#;

// Verbatim Python create_provider SQL literal.
pub const CREATE_SQL: &str = r#"
            INSERT INTO llm_providers (
                provider_id, tenant_id, display_name, api_type, base_url,
                api_key_encrypted, metadata, is_enabled
            ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8)
            RETURNING provider_id, tenant_id, display_name, api_type, base_url,
                      api_key_encrypted, metadata, is_enabled, created_at, updated_at
        "#;

// Verbatim Python _get_api_key SQL literal.
pub const KEY_SQL: &str = r#"
            SELECT api_key_encrypted
            FROM llm_providers
            WHERE tenant_id = $1 AND provider_id = $2
        "#;

pub const DELETE_MODELS_SQL: &str =
    "DELETE FROM llm_models WHERE tenant_id = $1 AND provider_id = $2";
pub const DELETE_PROVIDER_SQL: &str =
    "DELETE FROM llm_providers WHERE tenant_id = $1 AND provider_id = $2";

pub type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ServiceError>> + Send + 'a>>;
pub type ProviderRow = Map<String, Value>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceError {
    Database,
    /// Redacted PostgreSQL unique violation, retained for source HTTP 409 mapping.
    UniqueConflict,
    Cipher,
    InvalidRow,
    ProviderNotFound,
    Timeout,
    Connect,
    GoogleAuth,
    Http,
}
impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if matches!(self, Self::UniqueConflict) {
            f.write_str("duplicate key")
        } else {
            write!(f, "provider_service_{:?}", self)
        }
    }
}
impl std::error::Error for ServiceError {}

#[derive(Clone, PartialEq)]
pub enum Bind {
    Text(String),
    Null,
    Bool(bool),
    Timestamp(DateTime<Utc>),
}
impl fmt::Debug for Bind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Bind(<internal>)")
    }
}
/// An implementation executes exactly the supplied SQL and ordered typed bindings.
/// It must not silently filter disabled rows or combine the two delete statements.
pub trait ProviderDatabase: Send + Sync {
    fn fetch<'a>(&'a self, sql: &'a str, params: Vec<Bind>) -> ServiceFuture<'a, Vec<ProviderRow>>;
    fn fetchrow<'a>(
        &'a self,
        sql: &'a str,
        params: Vec<Bind>,
    ) -> ServiceFuture<'a, Option<ProviderRow>>;
    fn execute<'a>(&'a self, sql: &'a str, params: Vec<Bind>) -> ServiceFuture<'a, String>;
}
pub trait ProviderCipher: Send + Sync {
    fn encrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError>;
    fn decrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError>;
}
pub trait ProviderClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
pub trait ProviderEnvironment: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
}

/// Secret-bearing requests may be inspected only by a trusted HTTP adapter.
pub struct ProbeRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    pub timeout_seconds: u64,
}
impl fmt::Debug for ProbeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProbeRequest(<internal>)")
    }
}
pub struct ProbeResponse {
    pub status: u16,
    pub elapsed_milliseconds: u64,
}
pub trait ProviderHttp: Send + Sync {
    fn send<'a>(&'a self, request: ProbeRequest) -> ServiceFuture<'a, ProbeResponse>;
}

/// Opaque adapter credential state. No tokens or account JSON in Debug or errors.
pub struct GoogleCredentialState {
    pub internal_state: Value,
    pub default_project: Option<String>,
}
impl fmt::Debug for GoogleCredentialState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleCredentialState(<internal>)")
    }
}
pub const GOOGLE_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
pub trait ProviderGoogleAuth: Send + Sync {
    /// None selects google.auth.default; Some selects service_account credentials.
    fn acquire<'a>(
        &'a self,
        service_account: Option<Value>,
        scopes: &'a [&'a str],
    ) -> ServiceFuture<'a, GoogleCredentialState>;
    fn refresh<'a>(&'a self, credentials: GoogleCredentialState) -> ServiceFuture<'a, String>;
}

pub struct InternalRuntimeProviderConfig(Value);
impl InternalRuntimeProviderConfig {
    /// Only for server-to-Agent calls. Never return to a browser or log it.
    pub fn as_internal_value(&self) -> &Value {
        &self.0
    }
    pub fn into_internal_value(self) -> Value {
        self.0
    }
}
impl fmt::Debug for InternalRuntimeProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InternalRuntimeProviderConfig(<internal>)")
    }
}

pub struct CreateProvider {
    pub provider_id: String,
    pub display_name: String,
    pub api_type: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub metadata: Option<ProviderRow>,
    pub is_enabled: bool,
}
impl CreateProvider {
    pub fn new(provider_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            display_name: display_name.into(),
            api_type: "openai".into(),
            base_url: None,
            api_key: None,
            metadata: None,
            is_enabled: true,
        }
    }
}
#[derive(Default)]
pub struct UpdateProvider {
    pub display_name: Option<String>,
    pub api_type: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub metadata: Option<ProviderRow>,
    pub is_enabled: Option<bool>,
}
// Deliberately no Debug on inputs holding plaintext credentials.

pub struct ProviderService<D, C, H, G, E, T> {
    db: D,
    cipher: C,
    http: H,
    google: G,
    environment: E,
    clock: T,
    encryption_key: String,
}
impl<D, C, H, G, E, T> ProviderService<D, C, H, G, E, T>
where
    D: ProviderDatabase,
    C: ProviderCipher,
    H: ProviderHttp,
    G: ProviderGoogleAuth,
    E: ProviderEnvironment,
    T: ProviderClock,
{
    pub fn new(
        db: D,
        cipher: C,
        http: H,
        google: G,
        environment: E,
        clock: T,
        encryption_key: String,
    ) -> Self {
        Self {
            db,
            cipher,
            http,
            google,
            environment,
            clock,
            encryption_key,
        }
    }
    pub async fn list_providers(
        &self,
        tenant: &str,
        include_disabled: bool,
    ) -> Result<Vec<Value>, ServiceError> {
        let rows = self
            .db
            .fetch(
                if include_disabled {
                    LIST_ALL_SQL
                } else {
                    LIST_ENABLED_SQL
                },
                vec![text(tenant)],
            )
            .await?;
        Ok(rows.iter().map(normalize).collect())
    }
    pub async fn get_provider(
        &self,
        tenant: &str,
        id: &str,
    ) -> Result<Option<Value>, ServiceError> {
        Ok(self
            .db
            .fetchrow(GET_SQL, scope(tenant, id))
            .await?
            .filter(|row| !row.is_empty())
            .as_ref()
            .map(normalize))
    }
    pub async fn create_provider(
        &self,
        tenant: &str,
        input: CreateProvider,
    ) -> Result<Value, ServiceError> {
        let key = input
            .api_key
            .filter(|key| !key.is_empty())
            .map(|key| self.cipher.encrypt(&key, &self.encryption_key))
            .transpose()?;
        let row = self
            .db
            .fetchrow(
                CREATE_SQL,
                vec![
                    text(&input.provider_id),
                    text(tenant),
                    text(&input.display_name),
                    text(&input.api_type),
                    optional(input.base_url),
                    optional(key),
                    text(
                        &serde_json::to_string(&input.metadata.unwrap_or_default())
                            .map_err(|_| ServiceError::InvalidRow)?,
                    ),
                    Bind::Bool(input.is_enabled),
                ],
            )
            .await?;
        Ok(row.as_ref().map(normalize).unwrap_or_else(|| json!({})))
    }
    pub async fn update_provider(
        &self,
        tenant: &str,
        id: &str,
        input: UpdateProvider,
    ) -> Result<Option<Value>, ServiceError> {
        let mut updates = Vec::new();
        let mut params = Vec::new();
        for (column, value) in [
            ("display_name", input.display_name),
            ("api_type", input.api_type),
            ("base_url", input.base_url),
        ] {
            if let Some(value) = value {
                params.push(text(&value));
                updates.push(format!("{column} = ${}", params.len()));
            }
        }
        if let Some(key) = input.api_key {
            params.push(text(&self.cipher.encrypt(&key, &self.encryption_key)?));
            updates.push(format!("api_key_encrypted = ${}", params.len()));
        }
        if let Some(metadata) = input.metadata {
            params.push(text(
                &serde_json::to_string(&metadata).map_err(|_| ServiceError::InvalidRow)?,
            ));
            updates.push(format!("metadata = ${}::jsonb", params.len()));
        }
        if let Some(enabled) = input.is_enabled {
            params.push(Bind::Bool(enabled));
            updates.push(format!("is_enabled = ${}", params.len()));
        }
        if updates.is_empty() {
            return self.get_provider(tenant, id).await;
        }
        params.push(Bind::Timestamp(self.clock.now()));
        updates.push(format!("updated_at = ${}", params.len()));
        let idx = params.len() + 1;
        params.extend(scope(tenant, id));
        // Verbatim Python update_provider f-string; only field/parameter interpolation.
        let sql = format!(
            "\n            UPDATE llm_providers\n            SET {}\n            WHERE tenant_id = ${idx} AND provider_id = ${}\n            RETURNING provider_id, tenant_id, display_name, api_type, base_url,\n                      api_key_encrypted, metadata, is_enabled, created_at, updated_at\n        ",
            updates.join(", "),
            idx + 1
        );
        Ok(self
            .db
            .fetchrow(&sql, params)
            .await?
            .filter(|row| !row.is_empty())
            .as_ref()
            .map(normalize))
    }
    pub async fn delete_provider(&self, tenant: &str, id: &str) -> Result<bool, ServiceError> {
        self.db
            .execute(DELETE_MODELS_SQL, scope(tenant, id))
            .await?;
        Ok(self
            .db
            .execute(DELETE_PROVIDER_SQL, scope(tenant, id))
            .await?
            .contains("DELETE 1"))
    }
    async fn get_api_key(&self, tenant: &str, id: &str) -> Result<Option<String>, ServiceError> {
        let row = self.db.fetchrow(KEY_SQL, scope(tenant, id)).await?;
        match row.as_ref().and_then(|row| row.get("api_key_encrypted")) {
            None => Ok(None),
            Some(value) if !truthy(value) => Ok(None),
            Some(Value::String(value)) => {
                self.cipher.decrypt(value, &self.encryption_key).map(Some)
            }
            _ => Err(ServiceError::InvalidRow),
        }
    }
    pub async fn get_runtime_provider_config(
        &self,
        tenant: &str,
        id: &str,
    ) -> Result<InternalRuntimeProviderConfig, ServiceError> {
        let mut provider = self
            .get_provider(tenant, id)
            .await?
            .ok_or(ServiceError::ProviderNotFound)?;
        let key = self.get_api_key(tenant, id).await?;
        provider["api_key"] = json!(key);
        let inference = python_normalized_inference(&provider);
        provider["runtime_provider"] = json!(provider_routing::to_runtime_provider(&inference));
        provider["runtime_base_url"] =
            json!(provider_routing::normalize_runtime_base_url(&inference));
        provider["allow_environment_credentials"] =
            json!(provider_routing::allows_environment_credentials(&inference));
        Ok(InternalRuntimeProviderConfig(provider))
    }
    pub async fn test_connection(&self, tenant: &str, id: &str) -> Result<Value, ServiceError> {
        // Source DB/decrypt failures occur outside its try/except and propagate.
        let Some(provider) = self.get_provider(tenant, id).await? else {
            return Ok(failure("Provider not found"));
        };
        let key = self.get_api_key(tenant, id).await?;
        let api_type = provider.get("api_type").cloned().unwrap_or(json!("openai"));
        if key.as_deref().is_none_or(str::is_empty) && api_type != "google-vertex" {
            return Ok(failure("No API key configured"));
        }
        match self
            .probe(&provider, &api_type, key.as_deref().unwrap_or(""))
            .await
        {
            Ok(value) => Ok(value),
            Err(ServiceError::Timeout) => Ok(failure("Connection timed out")),
            Err(ServiceError::Connect) => Ok(failure("Connection failed")),
            Err(_) => Ok(failure("Error: provider probe failed")),
        }
    }
    fn env_value(&self, name: &str) -> Option<Value> {
        self.environment
            .get(name)
            .filter(|v| !v.is_empty())
            .map(Value::String)
    }
    async fn probe(
        &self,
        provider: &Value,
        api_type: &Value,
        key: &str,
    ) -> Result<Value, ServiceError> {
        let base = provider
            .get("base_url")
            .map(python_value_to_string)
            .unwrap_or_default();
        let mut request = ProbeRequest {
            method: "GET",
            url: String::new(),
            headers: Vec::new(),
            body: None,
            timeout_seconds: 10,
        };
        match api_type.as_str() {
            Some("openai") => {
                request.url = format!("{base}/v1/models");
                request
                    .headers
                    .push(("Authorization".into(), format!("Bearer {key}")));
            }
            Some("anthropic") => {
                request.method = "POST";
                request.url = format!("{base}/v1/messages");
                request.headers = vec![
                    ("x-api-key".into(), key.into()),
                    ("anthropic-version".into(), "2023-06-01".into()),
                    ("content-type".into(), "application/json".into()),
                ];
                request.body = Some(
                    json!({"model":"claude-3-5-haiku-20241022", "max_tokens":1, "messages":[{"role":"user", "content":"Hi"}]}),
                );
            }
            Some("google") => {
                request.url = format!("{base}/v1beta/models?key={key}");
            }
            Some("google-vertex")
                if key.is_empty() || key.trim_start_matches(python_whitespace).starts_with('{') =>
            {
                let metadata = provider.get("metadata").cloned().unwrap_or(json!({}));
                let mut project = metadata
                    .get("project")
                    .filter(|v| truthy(v))
                    .cloned()
                    .or_else(|| metadata.get("project_id").filter(|v| truthy(v)).cloned())
                    .or_else(|| self.env_value("GOOGLE_CLOUD_PROJECT"))
                    .or_else(|| self.env_value("GOOGLE_PROJECT_ID"));
                let location = metadata
                    .get("location")
                    .filter(|v| truthy(v))
                    .cloned()
                    .or_else(|| metadata.get("region").filter(|v| truthy(v)).cloned())
                    .or_else(|| self.env_value("GOOGLE_CLOUD_LOCATION"))
                    .or_else(|| self.env_value("GOOGLE_CLOUD_REGION"))
                    .unwrap_or(json!("us-central1"));
                let account: Option<Value> = if key.is_empty() {
                    None
                } else {
                    Some(serde_json::from_str(key).map_err(|_| ServiceError::GoogleAuth)?)
                };
                if project.is_none() {
                    project = account
                        .as_ref()
                        .and_then(|a| a.get("project_id"))
                        .filter(|v| truthy(v))
                        .cloned();
                }
                let credentials = self.google.acquire(account, &[GOOGLE_SCOPE]).await?;
                if key.is_empty() && project.is_none() {
                    project = credentials
                        .default_project
                        .as_ref()
                        .filter(|v| !v.is_empty())
                        .map(|v| json!(v));
                }
                let Some(project) = project else {
                    return Ok(failure("Google Cloud project is required"));
                };
                let token = self.google.refresh(credentials).await?;
                let location = python_value_to_string(&location);
                let project = python_value_to_string(&project);
                request.url = format!(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models"
                );
                request
                    .headers
                    .push(("Authorization".into(), format!("Bearer {token}")));
            }
            Some("google-vertex") => {
                request.url = format!("{base}/v1/publishers/google/models?key={key}");
            }
            _ => {
                return Ok(failure(&format!(
                    "Unknown API type: {}",
                    python_value_to_string(api_type)
                )));
            }
        }
        let response = self.http.send(request).await?;
        Ok(if matches!(response.status, 200 | 201) {
            json!({"success":true, "message":"Connection successful", "latency_ms":response.elapsed_milliseconds})
        } else {
            failure(&format!("API returned {}", response.status))
        })
    }
}
fn text(value: &str) -> Bind {
    Bind::Text(value.to_owned())
}
fn optional(value: Option<String>) -> Bind {
    value.map(Bind::Text).unwrap_or(Bind::Null)
}
fn scope(tenant: &str, id: &str) -> Vec<Bind> {
    vec![text(tenant), text(id)]
}
/// Keep stored fields untouched while matching Python str.strip for inference.
/// Rust str::trim omits Python's ASCII separators U+001C through U+001F.
fn python_normalized_inference(provider: &Value) -> Value {
    let mut inference = provider.clone();
    for name in ["api_type", "provider_id", "base_url"] {
        let value = provider.get(name).filter(|value| truthy(value));
        inference[name] = json!(
            value
                .map(|value| python_value_to_string(value)
                    .trim_matches(python_whitespace)
                    .to_owned())
                .unwrap_or_default()
        );
    }
    inference
}
fn normalize(row: &ProviderRow) -> Value {
    let mut result = Value::Object(provider_routing::normalize_provider_row(row));
    if !row.is_empty() {
        let inference = python_normalized_inference(&Value::Object(row.clone()));
        result["allow_environment_credentials"] =
            json!(provider_routing::allows_environment_credentials(&inference));
    }
    result
}
fn failure(message: &str) -> Value {
    json!({"success":false, "message":message})
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
