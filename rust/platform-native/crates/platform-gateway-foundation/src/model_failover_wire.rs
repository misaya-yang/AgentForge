//! Canonical `src/services/llm/model_failover.py` resolved-service boundary.
//!
//! Unlike `provider_routing`, this adapter accepts **only already-resolved**
//! runtime provider records. Missing runtime fields are never derived from DB
//! provider types/URLs, and environment credentials require explicit flags.
//! Existing assembly, ordered fallback warnings and tenant/provider selection
//! are reused. There is no retry execution or provider traffic here.
//!
//! Typed boundary: JSON values and an immutable, tenant-labelled service-result
//! snapshot replace Python objects and awaited service calls. Lookup misses map
//! to the canonical errors. Other service exceptions and the modern/legacy
//! model-service method dispatch remain the caller's responsibility. Python
//! non-JSON objects, nonfinite floats, and arbitrary-size JSON integers are not
//! represented. Decimal strings retain CPython's default 4,300-digit int limit.
//!
//! These builders accept trusted persisted overrides, NOT browser payloads.
//! `has_secret_field` belongs at the existing browser validation boundary;
//! builders deliberately do not add rejection absent from the canonical source.
//! Runtime payloads must never be returned to browsers or logged.

use crate::provider_routing::{self, ModelRoutingError, RoutingSnapshot, RuntimeModelConfig};
use crate::usage_parser::{python_value_to_string, python_whitespace};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Safe codes are shared with routing; no record or credential enters errors.
pub type ModelOverrideRuntimeError = ModelRoutingError;

/// Trusted output of provider/model services, never public request records.
/// No Debug/Serialize implementation: provider records can contain credentials.
pub struct ResolvedRuntimeSnapshot {
    providers: Vec<Value>,
    models: Vec<Value>,
}

impl ResolvedRuntimeSnapshot {
    pub fn new(providers: &[Value], models: &[Value]) -> Self {
        let providers = providers
            .iter()
            .map(|provider| {
                let mut provider = provider.clone();
                if let Some(record) = provider.as_object_mut() {
                    // Presence disables provider_routing's DB-row inference. Null
                    // still produces the canonical unsupported/missing-key errors.
                    let runtime =
                        normalized_id(record.get("runtime_provider").unwrap_or(&Value::Null));
                    record.insert("runtime_provider".into(), json!(runtime));
                    record.entry("runtime_base_url").or_insert(Value::Null);
                    if let Some(metadata) =
                        record.get_mut("metadata").and_then(Value::as_object_mut)
                    {
                        for value in metadata.values_mut() {
                            if !value.is_null() {
                                *value = json!(
                                    python_value_to_string(value).trim_matches(python_whitespace)
                                );
                            }
                        }
                    }
                }
                provider
            })
            .collect();
        Self {
            providers,
            models: models.to_vec(),
        }
    }

    fn routing_snapshot(&self) -> RoutingSnapshot<'_> {
        RoutingSnapshot::new(&self.providers, &self.models)
    }
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

fn normalized_id(value: &Value) -> String {
    if truthy(value) {
        python_value_to_string(value)
            .trim_matches(python_whitespace)
            .to_owned()
    } else {
        String::new()
    }
}

pub fn has_secret_field(value: &Value) -> bool {
    match value {
        Value::Object(values) => values.iter().any(|(key, child)| {
            // Reuse the exact vocabulary while matching Python str.strip().
            provider_routing::has_secret_field(&json!({key.trim_matches(python_whitespace): null}))
                || has_secret_field(child)
        }),
        Value::Array(values) => values.iter().any(has_secret_field),
        _ => false,
    }
}

pub fn safe_api_key_fingerprint(api_key: &Value) -> Option<String> {
    truthy(api_key).then(|| {
        let digest = Sha256::digest(python_value_to_string(api_key).as_bytes());
        digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    })
}

// CPython Unicode decimal zero codepoints. Generated from unicodedata.decimal;
// these mirror the existing usage parser's table, without changing shared files.
const DECIMAL_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650,
    0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0,
    0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950,
    0x1fbf0,
];

fn parse_integer(raw: &str) -> Option<i64> {
    // Python int() excludes the four separator controls accepted by str.strip.
    let raw = raw.trim_matches(char::is_whitespace);
    let (negative, digits) = if let Some(rest) = raw.strip_prefix('-') {
        (true, rest)
    } else {
        (false, raw.strip_prefix('+').unwrap_or(raw))
    };
    let mut number = 0_i64;
    let mut previous_digit = false;
    let mut count = 0;
    for ch in digits.chars() {
        if ch == '_' && previous_digit {
            previous_digit = false;
            continue;
        }
        let cp = ch as u32;
        let index = DECIMAL_ZEROS.partition_point(|zero| *zero <= cp);
        let zero = DECIMAL_ZEROS.get(index.checked_sub(1)?)?;
        let digit = cp - zero;
        if digit >= 10 {
            return None;
        }
        count += 1;
        if count > 4300 {
            return None;
        }
        number = number.saturating_mul(10).saturating_add(i64::from(digit));
        previous_digit = true;
    }
    if !previous_digit {
        return None;
    }
    Some(if negative { -number } else { number })
}

pub fn normalize_max_attempts(value: &Value, default: i64) -> usize {
    if let Value::String(text) = value {
        parse_integer(text).unwrap_or(default).clamp(1, 10) as usize
    } else {
        provider_routing::normalize_max_attempts(value, default)
    }
}

pub fn normalize_failover_attempts(value: &Value, candidate_count: usize, default: i64) -> usize {
    let attempts = normalize_max_attempts(value, default);
    let attempts = if candidate_count > 1 {
        attempts.max(2)
    } else {
        attempts
    };
    attempts.min(candidate_count.max(1))
}

/// Server-only output with explicit extraction and redacted Debug, reused from
/// routing without making the private payload serializable.
pub struct ResolvedRuntimeModelConfig {
    payload: Value,
    route_fingerprint_v2: String,
}
impl ResolvedRuntimeModelConfig {
    pub fn as_internal_value(&self) -> &Value {
        &self.payload
    }
    pub fn into_internal_value(self) -> Value {
        self.payload
    }
    pub fn route_fingerprint_v2(&self) -> &str {
        &self.route_fingerprint_v2
    }
}
impl std::fmt::Debug for ResolvedRuntimeModelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedRuntimeModelConfig(<server-only>)")
    }
}

fn corrected_fingerprints(runtime: RuntimeModelConfig) -> ResolvedRuntimeModelConfig {
    fn correct(value: &mut Value) {
        let fingerprint = safe_api_key_fingerprint(value.get("_api_key").unwrap_or(&Value::Null));
        value["api_key_fingerprint"] = json!(fingerprint);
    }
    let route_fingerprint_v2 = runtime.route_fingerprint_v2().to_owned();
    let mut value = runtime.into_internal_value();
    correct(&mut value);
    if let Some(candidates) = value
        .pointer_mut("/failover/candidates")
        .and_then(Value::as_array_mut)
    {
        for candidate in candidates {
            correct(candidate);
        }
    }
    ResolvedRuntimeModelConfig {
        payload: value,
        route_fingerprint_v2,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_runtime_candidate(
    tenant_id: &str,
    provider_id: &Value,
    model_id: &Value,
    temperature: &Value,
    cache_epoch: &Value,
    snapshot: &ResolvedRuntimeSnapshot,
    error_prefix: &str,
) -> Result<ResolvedRuntimeModelConfig, ModelOverrideRuntimeError> {
    let cache_epoch = if truthy(cache_epoch) {
        python_value_to_string(cache_epoch)
    } else {
        "0".into()
    };
    provider_routing::build_runtime_candidate(
        tenant_id,
        &json!(normalized_id(provider_id)),
        &json!(normalized_id(model_id)),
        temperature,
        &json!(cache_epoch),
        &snapshot.routing_snapshot(),
        error_prefix,
    )
    .map(corrected_fingerprints)
}

pub fn build_runtime_model_override_config(
    tenant_id: &str,
    model_override: &Value,
    snapshot: &ResolvedRuntimeSnapshot,
) -> Result<ResolvedRuntimeModelConfig, ModelOverrideRuntimeError> {
    let mut config = model_override.clone();
    fn normalize_ids(value: &mut Value) {
        if let Some(record) = value.as_object_mut() {
            for key in ["provider_id", "model_id"] {
                let normalized = normalized_id(record.get(key).unwrap_or(&Value::Null));
                record.insert(key.into(), json!(normalized));
            }
        }
    }
    normalize_ids(&mut config);
    if let Some(record) = config.as_object_mut() {
        let epoch = record.get("cache_epoch").unwrap_or(&Value::Null);
        let epoch = if truthy(epoch) {
            python_value_to_string(epoch)
        } else {
            "0".into()
        };
        record.insert("cache_epoch".into(), json!(epoch));
        if let Some(failover) = record.get_mut("failover").and_then(Value::as_object_mut) {
            let attempts =
                normalize_max_attempts(failover.get("max_attempts").unwrap_or(&Value::Null), 3);
            failover.insert("max_attempts".into(), json!(attempts));
            if let Some(candidates) = failover.get_mut("candidates").and_then(Value::as_array_mut) {
                for candidate in candidates {
                    normalize_ids(candidate);
                }
            }
        }
    }
    provider_routing::build_runtime_model_override_config(
        tenant_id,
        &config,
        &snapshot.routing_snapshot(),
    )
    .map(corrected_fingerprints)
}
