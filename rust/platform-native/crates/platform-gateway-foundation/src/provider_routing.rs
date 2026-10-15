//! Gateway-owned provider selection and provider request normalization.
//!
//! Canonical Python source map:
//! - `src/services/llm/provider_service.py`: provider vocabulary, runtime URL,
//!   environment-credential policy, and provider-row normalization.
//! - `src/services/llm/model_service.py::get_model`: tenant/provider scoping and
//!   the `model_provider_required` ambiguity error.
//! - `src/services/llm/model_failover.py`: server-only candidate/failover assembly,
//!   retry limits, safe warnings, credential fingerprints, and secret-key checks.
//! - `src/services/agent_runtime/model/request_builder.py`: phase-2 input checks,
//!   content extraction, native Responses input projection, and provider headers.
//!
//! This module consumes already-loaded, trusted Gateway records. It performs no
//! network, database, credential decryption, environment reads, or dispatch. The
//! caller must resolve the authenticated tenant and validate stored overrides
//! before assembly, as the Python control plane does. Runtime credentials and
//! headers are deliberately not `Serialize` or content-bearing `Debug` values;
//! they can be extracted explicitly for server-to-provider use only. Selection
//! is not authorization: lease, service ACL, capability, and quota enforcement
//! remain at their existing boundaries.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const SECRET_FIELD_NAMES: &[&str] = &[
    "_api_key",
    "api_key",
    "apikey",
    "api-key",
    "credential",
    "credentials",
    "password",
    "private_key",
    "secret",
    "service_account",
    "service_account_json",
    "token",
];

/// An error safe to return or log; it never embeds a provider record or key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRoutingError {
    pub code: String,
}

impl ModelRoutingError {
    fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

impl fmt::Display for ModelRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.code)
    }
}

impl std::error::Error for ModelRoutingError {}

/// A server-only payload matching Python's `configurable.gateway_model` shape.
///
/// Never attach the extracted value to a public response or log. Python's wire
/// contract includes `_api_key` both at the root and in failover candidates.
pub struct RuntimeModelConfig(Value);

impl RuntimeModelConfig {
    pub fn as_internal_value(&self) -> &Value {
        &self.0
    }

    pub fn into_internal_value(self) -> Value {
        self.0
    }
}

impl fmt::Debug for RuntimeModelConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeModelConfig(<server-only>)")
    }
}

/// Borrowed records from existing Gateway provider/model services, including an
/// already-resolved `api_key` when applicable. Records must carry `tenant_id`;
/// missing or foreign tenant IDs never match, including for fallback candidates.
pub struct RoutingSnapshot<'a> {
    providers: &'a [Value],
    models: &'a [Value],
}

impl<'a> RoutingSnapshot<'a> {
    pub fn new(providers: &'a [Value], models: &'a [Value]) -> Self {
        Self { providers, models }
    }

    pub fn get_provider(&self, tenant_id: &str, provider_id: &str) -> Option<&'a Value> {
        self.providers.iter().find(|record| {
            record.get("tenant_id").and_then(Value::as_str) == Some(tenant_id)
                && record.get("provider_id").and_then(Value::as_str) == Some(provider_id)
        })
    }

    /// Matches `ModelService.get_model`, including disabled rows when deciding
    /// ambiguity. `Some("")` is an explicit provider filter, not an omission.
    pub fn get_model(
        &self,
        tenant_id: &str,
        model_id: &str,
        provider_id: Option<&str>,
    ) -> Result<Option<&'a Value>, ModelRoutingError> {
        let mut matches = self.models.iter().filter(|record| {
            record.get("tenant_id").and_then(Value::as_str) == Some(tenant_id)
                && record.get("model_id").and_then(Value::as_str) == Some(model_id)
                && provider_id
                    .is_none_or(|id| record.get("provider_id").and_then(Value::as_str) == Some(id))
        });
        let first = matches.next();
        if provider_id.is_none() && matches.next().is_some() {
            return Err(ModelRoutingError::new("model_provider_required"));
        }
        Ok(first)
    }
}

/// Preserve precedence: explicit Google/Vertex/DashScope types precede the
/// historical DashScope URL/ID heuristic, which precedes OpenAI compatibility.
pub fn to_runtime_provider(provider: &Value) -> String {
    let api_type = trimmed_or_empty(field(provider, "api_type")).to_lowercase();
    let provider_id = trimmed_or_empty(field(provider, "provider_id")).to_lowercase();
    let base_url = trimmed_or_empty(field(provider, "base_url")).to_lowercase();
    match api_type.as_str() {
        "google" | "google-ai-studio" => "gemini".to_owned(),
        "google-vertex" | "vertex" => "vertex".to_owned(),
        "dashscope" | "aliyun" => "dashscope".to_owned(),
        _ if base_url.contains("dashscope.aliyuncs.com")
            || provider_id.starts_with("dashscope") =>
        {
            "dashscope".to_owned()
        }
        "openai" | "openai-compatible" => "openai".to_owned(),
        _ => api_type,
    }
}

pub fn normalize_runtime_base_url(provider: &Value) -> Option<String> {
    let base_url = trimmed_or_empty(field(provider, "base_url"));
    let base_url = base_url.trim_end_matches('/');
    if to_runtime_provider(provider) == "dashscope" {
        if base_url.is_empty() {
            return Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_owned());
        }
        if base_url.ends_with("/compatible-mode") {
            return Some(format!("{base_url}/v1"));
        }
    }
    (!base_url.is_empty()).then(|| base_url.to_owned())
}

pub fn allows_environment_credentials(provider: &Value) -> bool {
    let api_type = trimmed_or_empty(field(provider, "api_type")).to_lowercase();
    matches!(api_type.as_str(), "google-vertex" | "vertex")
        || runtime_allows_environment_credentials(provider)
}

fn runtime_allows_environment_credentials(provider: &Value) -> bool {
    truthy(field(provider, "allow_environment_credentials"))
        || truthy(field(provider, "uses_environment_credentials"))
}

/// Normalize a database provider row without exposing its encrypted key.
/// Like Python `_row_to_dict`, unrelated columns remain intact. This is not a
/// generic redactor and must be called on a DB row, not a runtime config.
pub fn normalize_provider_row(row: &Map<String, Value>) -> Map<String, Value> {
    if row.is_empty() {
        return Map::new();
    }
    let mut result = row.clone();
    let has_key = result
        .remove("api_key_encrypted")
        .is_some_and(|key| truthy(&key));
    result.insert("has_api_key".to_owned(), json!(has_key));
    let allow_environment = allows_environment_credentials(&Value::Object(result.clone()));
    result.insert(
        "allow_environment_credentials".to_owned(),
        json!(allow_environment),
    );
    let metadata = match result.remove("metadata") {
        Some(Value::Object(metadata)) => metadata,
        Some(Value::String(encoded)) => serde_json::from_str::<Value>(&encoded)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default(),
        _ => Map::new(),
    };
    result.insert("metadata".to_owned(), Value::Object(metadata));
    result
}

/// Recursive detection used by the existing public override validation layer.
/// This does not scan values or guess at secrets: the Python contract matches
/// the exact, case-insensitive key vocabulary after trimming whitespace.
pub fn has_secret_field(value: &Value) -> bool {
    match value {
        Value::Object(values) => values.iter().any(|(key, child)| {
            SECRET_FIELD_NAMES.contains(&key.trim().to_lowercase().as_str())
                || has_secret_field(child)
        }),
        Value::Array(values) => values.iter().any(has_secret_field),
        _ => false,
    }
}

/// Existing nonreversible cache discriminator: the first 16 SHA-256 hex digits.
pub fn safe_api_key_fingerprint(api_key: &Value) -> Option<String> {
    if !truthy(api_key) {
        return None;
    }
    let digest = Sha256::digest(python_string(api_key).as_bytes());
    Some(
        digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// Python `int(value)` semantics for persisted JSON scalars, then clamp to 1..10.
pub fn normalize_max_attempts(value: &Value, default: i64) -> usize {
    let parsed = match value {
        Value::Bool(value) => Some(i64::from(*value)),
        Value::Number(value) => value.as_i64().or_else(|| {
            value
                .as_f64()
                .map(|number| number.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
        }),
        Value::String(value) => parse_integer(value),
        _ => None,
    };
    parsed.unwrap_or(default).clamp(1, 10) as usize
}

/// An enabled nonempty fallback chain must be reachable even when configured
/// with one attempt; attempts never exceed the number of surviving candidates.
pub fn normalize_failover_attempts(value: &Value, candidate_count: usize, default: i64) -> usize {
    let attempts = normalize_max_attempts(value, default);
    let attempts = if candidate_count > 1 {
        attempts.max(2)
    } else {
        attempts
    };
    attempts.min(candidate_count.max(1))
}

/// Assemble one candidate from trusted, tenant-scoped persisted records.
/// Validation order and codes intentionally follow `build_runtime_candidate`.
pub fn build_runtime_candidate(
    tenant_id: &str,
    provider_id: &Value,
    model_id: &Value,
    temperature: &Value,
    cache_epoch: &Value,
    snapshot: &RoutingSnapshot<'_>,
    error_prefix: &str,
) -> Result<RuntimeModelConfig, ModelRoutingError> {
    let fail = |suffix| ModelRoutingError::new(format!("{error_prefix}_{suffix}"));
    let provider_id = trimmed_or_empty(provider_id);
    let model_id = trimmed_or_empty(model_id);
    if provider_id.is_empty() {
        return Err(fail("PROVIDER_REQUIRED"));
    }
    if model_id.is_empty() {
        return Err(fail("MODEL_REQUIRED"));
    }
    let provider = snapshot
        .get_provider(tenant_id, &provider_id)
        .ok_or_else(|| fail("PROVIDER_NOT_FOUND"))?;
    if !truthy(field(provider, "is_enabled")) {
        return Err(fail("PROVIDER_DISABLED"));
    }
    let model = snapshot
        .get_model(tenant_id, &model_id, Some(&provider_id))?
        .ok_or_else(|| fail("MODEL_NOT_FOUND"))?;
    if !truthy(field(model, "is_enabled")) {
        return Err(fail("MODEL_DISABLED"));
    }
    let api_key = field(provider, "api_key");
    let environment_credentials = if provider.get("runtime_provider").is_some() {
        runtime_allows_environment_credentials(provider)
    } else {
        allows_environment_credentials(provider)
    };
    if !truthy(api_key) && !environment_credentials {
        return Err(fail("API_KEY_MISSING"));
    }
    // Accept the same resolved provider-service output the Python builder
    // consumes. Otherwise compute the three fields from a trusted DB record.
    let runtime_provider = if provider.get("runtime_provider").is_some() {
        trimmed_or_empty(field(provider, "runtime_provider"))
    } else {
        to_runtime_provider(provider)
    };
    if runtime_provider.is_empty() {
        return Err(fail("PROVIDER_UNSUPPORTED"));
    }
    let base_url = provider
        .get("runtime_base_url")
        .cloned()
        .unwrap_or_else(|| json!(normalize_runtime_base_url(provider)));
    let cache_epoch = if truthy(cache_epoch) {
        python_string(cache_epoch)
    } else {
        "0".to_owned()
    };
    let mut candidate = json!({
        "enabled": true,
        "tenant_id": tenant_id,
        "provider_id": provider_id,
        "provider": runtime_provider,
        "model_id": model_id,
        "model": model_id,
        "temperature": temperature,
        "base_url": base_url,
        "api_key_fingerprint": safe_api_key_fingerprint(api_key),
        "cache_epoch": cache_epoch,
    });
    let map = candidate.as_object_mut().expect("candidate is an object");
    map.extend(provider_runtime_metadata(provider));
    if truthy(api_key) {
        map.insert("_api_key".to_owned(), api_key.clone());
    }
    Ok(RuntimeModelConfig(candidate))
}

/// Resolve a stored model override, preserving fallback order, duplicate
/// detection, invalid-candidate warnings, and primary-candidate fail-closed
/// behavior. Failover warnings contain IDs and safe error codes only.
pub fn build_runtime_model_override_config(
    tenant_id: &str,
    model_override: &Value,
    snapshot: &RoutingSnapshot<'_>,
) -> Result<RuntimeModelConfig, ModelRoutingError> {
    if !model_override.is_object() || !truthy(field(model_override, "enabled")) {
        return Err(ModelRoutingError::new("MODEL_OVERRIDE_INVALID"));
    }
    let temperature = field(model_override, "temperature");
    let cache_epoch = field(model_override, "cache_epoch");
    let primary = build_runtime_candidate(
        tenant_id,
        field(model_override, "provider_id"),
        field(model_override, "model_id"),
        temperature,
        cache_epoch,
        snapshot,
        "MODEL_OVERRIDE",
    )?
    .into_internal_value();
    let mut runtime_config = primary.clone();
    let failover = field(model_override, "failover");
    if !failover.is_object() || !truthy(field(failover, "enabled")) {
        return Ok(RuntimeModelConfig(runtime_config));
    }
    let max_attempts = normalize_max_attempts(field(failover, "max_attempts"), 3);
    let mut candidates = vec![primary.clone()];
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::from([(
        primary["provider_id"]
            .as_str()
            .expect("normalized provider")
            .to_owned(),
        primary["model_id"]
            .as_str()
            .expect("normalized model")
            .to_owned(),
    )]);
    if let Some(raw_candidates) = field(failover, "candidates").as_array() {
        for raw_candidate in raw_candidates {
            if !raw_candidate.is_object() {
                warnings.push(json!({"code": "MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID"}));
                continue;
            }
            let provider_id = trimmed_or_empty(field(raw_candidate, "provider_id"));
            let model_id = trimmed_or_empty(field(raw_candidate, "model_id"));
            let warning = |code: &str| {
                json!({
                    "provider_id": (!provider_id.is_empty()).then_some(provider_id.as_str()),
                    "model_id": (!model_id.is_empty()).then_some(model_id.as_str()),
                    "code": code,
                })
            };
            if provider_id.is_empty() || model_id.is_empty() {
                warnings.push(warning("MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID"));
                continue;
            }
            let identity = (provider_id.clone(), model_id.clone());
            if seen.contains(&identity) {
                warnings.push(warning("MODEL_OVERRIDE_FAILOVER_DUPLICATE"));
                continue;
            }
            match build_runtime_candidate(
                tenant_id,
                &json!(provider_id),
                &json!(model_id),
                temperature,
                cache_epoch,
                snapshot,
                "MODEL_OVERRIDE_FAILOVER",
            ) {
                Ok(candidate) => {
                    candidates.push(candidate.into_internal_value());
                    seen.insert(identity);
                }
                Err(error) => warnings.push(warning(&error.code)),
            }
        }
    }
    let attempts = normalize_failover_attempts(&json!(max_attempts), candidates.len(), 3);
    let mut failover = json!({"enabled": true, "max_attempts": attempts, "candidates": candidates});
    if !warnings.is_empty() {
        failover["warnings"] = Value::Array(warnings);
    }
    runtime_config["failover"] = failover;
    Ok(RuntimeModelConfig(runtime_config))
}

fn provider_runtime_metadata(provider: &Value) -> Map<String, Value> {
    let Some(metadata) = field(provider, "metadata").as_object() else {
        return Map::new();
    };
    let mut runtime = Map::new();
    for (source, target) in [
        ("project", "project"),
        ("project_id", "project"),
        ("google_cloud_project", "project"),
        ("location", "location"),
        ("region", "location"),
        ("google_cloud_location", "location"),
        ("credentials_path", "credentials_path"),
        ("google_application_credentials", "credentials_path"),
    ] {
        if let Some(value) = metadata.get(source).filter(|value| !value.is_null()) {
            let value = python_string(value).trim().to_owned();
            if !value.is_empty() {
                runtime.insert(target.to_owned(), json!(value));
            }
        }
    }
    runtime
}

/// Safe model-plane validation error with the existing HTTP status/code pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestNormalizationError {
    pub code: &'static str,
    pub status_code: u16,
}

impl fmt::Display for RequestNormalizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for RequestNormalizationError {}

pub fn validate_phase2_responses_input(
    body: &Map<String, Value>,
    allow_tool_transcript: bool,
) -> Result<(), RequestNormalizationError> {
    let invalid = RequestNormalizationError {
        code: "RUNTIME_RESPONSES_INPUT_INVALID",
        status_code: 400,
    };
    let input = body.get("input").unwrap_or(&Value::Null);
    if let Some(input) = input.as_str() {
        return if input.is_empty() {
            Err(invalid)
        } else {
            Ok(())
        };
    }
    let input = input
        .as_array()
        .filter(|input| !input.is_empty())
        .ok_or(invalid)?;
    for item in input {
        let item = item.as_object().ok_or(invalid)?;
        match item.get("type").and_then(Value::as_str) {
            Some("additional_tools" | "configuration_update") => {
                return Err(RequestNormalizationError {
                    code: "RUNTIME_INPUT_CONTROL_UNSUPPORTED",
                    status_code: 422,
                });
            }
            Some("function_call" | "function_call_output") if !allow_tool_transcript => {
                return Err(RequestNormalizationError {
                    code: "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE",
                    status_code: 422,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// The exact provider headers owned by Gateway. Client authorization/header
/// forwarding is intentionally absent. The profile must be server-authorized.
pub struct ProviderHeaders(BTreeMap<String, String>);

impl ProviderHeaders {
    pub fn as_internal_headers(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    pub fn into_internal_headers(self) -> BTreeMap<String, String> {
        self.0
    }
}

impl fmt::Debug for ProviderHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderHeaders(<server-only>)")
    }
}

pub fn provider_headers(profile: &Value, api_key: &str) -> ProviderHeaders {
    let mut headers = BTreeMap::from([
        ("Authorization".to_owned(), format!("Bearer {api_key}")),
        ("content-type".to_owned(), "application/json".to_owned()),
    ]);
    let cache = field(profile, "prompt_cache");
    if field(cache, "adapter_id").as_str() == Some("cache/dashscope-session-v1")
        && field(field(cache, "config"), "enabled") == &Value::Bool(true)
    {
        headers.insert("x-dashscope-session-cache".to_owned(), "enable".to_owned());
    }
    ProviderHeaders(headers)
}

pub fn content_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    let Some(parts) = value.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| match field(part, "type").as_str() {
            Some("input_text" | "output_text" | "text") => field(part, "text").as_str(),
            Some("encrypted_content") => field(part, "encrypted_content").as_str(),
            _ => None,
        })
        .collect()
}

/// Deep-copy non-OpenAI Responses input, exposing already-authorized internal
/// collaboration text on the wire. No input record is mutated in place.
pub fn native_responses_input(value: &Value) -> Value {
    let Some(items) = value.as_array() else {
        return value.clone();
    };
    Value::Array(
        items
            .iter()
            .map(|raw_item| {
                let mut item = raw_item.clone();
                let Some(parts) = field(raw_item, "content").as_array() else {
                    return item;
                };
                let content = Value::Array(
                    parts
                        .iter()
                        .map(|part| {
                            if field(part, "type").as_str() == Some("encrypted_content")
                                && let Some(text) = field(part, "encrypted_content").as_str()
                            {
                                return json!({"type": "input_text", "text": text});
                            }
                            part.clone()
                        })
                        .collect(),
                );
                if field(&item, "type").as_str() == Some("agent_message") {
                    json!({"type": "message", "role": "user", "content": content})
                } else {
                    item["content"] = content;
                    item
                }
            })
            .collect(),
    )
}

fn field<'a>(object: &'a Value, key: &str) -> &'a Value {
    object.get(key).unwrap_or(&Value::Null)
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

fn trimmed_or_empty(value: &Value) -> String {
    if truthy(value) {
        python_string(value).trim().to_owned()
    } else {
        String::new()
    }
}

fn python_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| {
                    format!("{}: {}", python_repr(&json!(key)), python_repr(value))
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(value) => value.to_string(),
    }
}

fn python_repr(value: &Value) -> String {
    let Value::String(value) = value else {
        return python_string(value);
    };
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut output = String::from(quote);
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character == quote => {
                output.push('\\');
                output.push(character);
            }
            character if character.is_control() => {
                if (character as u32) <= 0xff {
                    output.push_str(&format!("\\x{:02x}", character as u32));
                } else {
                    output.push_str(&format!("\\u{:04x}", character as u32));
                }
            }
            character => output.push(character),
        }
    }
    output.push(quote);
    output
}

fn parse_integer(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let (negative, digits) = if let Some(digits) = raw.strip_prefix('-') {
        (true, digits)
    } else {
        (false, raw.strip_prefix('+').unwrap_or(raw))
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value = 0_i64;
    for character in digits.chars() {
        if character == '_' {
            continue;
        }
        let digit = character.to_digit(10)?;
        value = value.saturating_mul(10).saturating_add(i64::from(digit));
    }
    Some(if negative { -value } else { value })
}

