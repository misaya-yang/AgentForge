//! Pure port of `src/core/gateway/capacity.py`; no admission, Redis, or environment reads.
//!
//! The service-config boundary is the Python dictionary/JSON shape. Python objects
//! with `capacity_config`, nested dataclasses, or `metadata` must first be resolved
//! by their owning adapter into this dictionary shape. They are not introspected
//! here. JSON integer values are bounded to i64; overflow is an explicit error.
//! Mode and cluster epoch remain descriptive, as in the Python resolver.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityBudget {
    pub key: String,
    pub limit: i64,
    pub queue_max: i64,
    pub queue_timeout_ms: i64,
    pub scope: String,
    pub source: String,
    pub enforced: bool,
    pub source_status: String,
    pub shared: bool,
}
impl CapacityBudget {
    pub fn to_status(&self, inflight: i64, queue_depth: i64) -> Value {
        let mut row = serde_json::to_value(self).expect("capacity budget is JSON serializable");
        row["inflight"] = inflight.into();
        row["queue_depth"] = queue_depth.into();
        row
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityError {
    UnknownResourceKind,
    InvalidNumber,
    IntegerOverflow,
    InvalidSnapshot,
}
impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownResourceKind => "unknown_capacity_resource_kind",
            Self::InvalidNumber => "invalid_capacity_number",
            Self::IntegerOverflow => "capacity_integer_overflow",
            Self::InvalidSnapshot => "invalid_capacity_snapshot",
        })
    }
}
impl std::error::Error for CapacityError {}

const DEFAULTS: &[(&str, i64, i64, &str, bool)] = &[
    ("gateway.total_inflight", 64, 128, "gateway", false),
    ("gateway.stream_inflight", 16, 128, "gateway", false),
    ("gateway.non_stream_inflight", 48, 128, "gateway", false),
    ("upstream.langgraph_agent", 4, 16, "upstream", true),
    ("upstream.agent_runtime", 8, 32, "upstream", true),
    ("upstream.knowledge_service", 12, 48, "upstream", true),
    ("upstream.image_generation", 2, 8, "upstream", true),
    ("provider.dashscope_cn", 8, 16, "provider", true),
    ("provider.dashscope_intl", 8, 16, "provider", true),
    ("provider.google_gemini", 4, 16, "provider", true),
    ("provider.google_vertex", 1, 4, "provider", true),
];

pub fn default_budgets() -> Vec<CapacityBudget> {
    DEFAULTS
        .iter()
        .map(|(key, limit, queue_max, scope, shared)| CapacityBudget {
            key: (*key).into(),
            limit: *limit,
            queue_max: *queue_max,
            queue_timeout_ms: 3000,
            scope: (*scope).into(),
            source: "default".into(),
            enforced: true,
            source_status: "real".into(),
            shared: *shared,
        })
        .collect()
}
fn default_budget(key: &str) -> Option<CapacityBudget> {
    default_budgets()
        .into_iter()
        .find(|budget| budget.key == key)
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
fn python_string(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
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
                    format!(
                        "{}: {}",
                        python_repr(&Value::String(key.clone())),
                        python_repr(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
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
    let escaped: String = value
        .chars()
        .map(|character| match character {
            '\\' => "\\\\".into(),
            '\n' => "\\n".into(),
            '\r' => "\\r".into(),
            '\t' => "\\t".into(),
            value if value == quote => format!("\\{value}"),
            value if value.is_control() => format!("\\x{:02x}", value as u32),
            value => value.to_string(),
        })
        .collect();
    format!("{quote}{escaped}{quote}")
}

pub fn normalize_capacity_group(value: &Value, prefix: &str) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let normalized = python_string(value).trim().to_lowercase().replace('-', "_");
    if normalized.is_empty() {
        return None;
    }
    Some(
        normalized
            .strip_prefix(&format!("{prefix}."))
            .unwrap_or(&normalized)
            .to_owned(),
    )
}

/// Exact dictionary precedence: an empty `capacity` falls through to
/// `capacity_config`, but a truthy non-dictionary does not. A null nested legacy
/// limit can be filled by the top-level legacy limit; a zero cannot.
pub fn capacity_config_from_service(service_config: Option<&Value>) -> Map<String, Value> {
    let Some(service) = service_config.and_then(Value::as_object) else {
        return Map::new();
    };
    let mut result = service
        .get("capacity")
        .filter(|value| truthy(value))
        .or_else(|| service.get("capacity_config").filter(|value| truthy(value)))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(limit) = service
        .get("concurrency_limit")
        .filter(|value| !value.is_null())
        && result.get("concurrency_limit").is_none_or(Value::is_null)
    {
        result.insert("concurrency_limit".into(), limit.clone());
    }
    result
}

pub fn service_upstream_group(service_id: &str, configured_group: &Value) -> Option<String> {
    if let Some(group) =
        normalize_capacity_group(configured_group, "upstream").filter(|group| !group.is_empty())
    {
        return Some(group);
    }
    Some(
        match service_id.trim().to_lowercase().as_str() {
            "local-2024-agent" | "langgraph-agent" | "agent" => "langgraph_agent",
            "assistant" | "agent-runtime" => "agent_runtime",
            "knowledge" | "knowledge-service" => "knowledge_service",
            "image-generation" => "image_generation",
            _ => return None,
        }
        .into(),
    )
}

pub fn provider_budget_key(provider_id: &Value) -> Option<String> {
    let normalized = normalize_capacity_group(provider_id, "provider")?;
    let group = match normalized.as_str() {
        "google" | "gemini" => "google_gemini",
        "dashscope" => "dashscope_cn",
        "vertex" => "google_vertex",
        value => value,
    };
    let key = format!("provider.{group}");
    default_budget(&key).map(|_| key)
}

fn truncated_integer(value: f64) -> Result<i64, CapacityError> {
    if !value.is_finite() {
        return Err(CapacityError::InvalidNumber);
    }
    let value = value.trunc();
    if value < i64::MIN as f64 || value >= -(i64::MIN as f64) {
        return Err(CapacityError::IntegerOverflow);
    }
    Ok(value as i64)
}
fn numeric_text(value: &str) -> Result<String, CapacityError> {
    let value = value.trim();
    // int() and float() both accept separators only between decimal digits.
    if value.bytes().enumerate().any(|(index, byte)| {
        byte == b'_'
            && (!value
                .as_bytes()
                .get(index.wrapping_sub(1))
                .is_some_and(u8::is_ascii_digit)
                || !value
                    .as_bytes()
                    .get(index + 1)
                    .is_some_and(u8::is_ascii_digit))
    }) {
        return Err(CapacityError::InvalidNumber);
    }
    Ok(value.replace('_', ""))
}
fn integer(value: &Value) -> Result<i64, CapacityError> {
    match value {
        Value::Bool(value) => Ok(i64::from(*value)),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                return Ok(value);
            }
            if number.is_u64() {
                return Err(CapacityError::IntegerOverflow);
            }
            truncated_integer(number.as_f64().ok_or(CapacityError::InvalidNumber)?)
        }
        Value::String(value) => numeric_text(value)?
            .parse()
            .map_err(|_| CapacityError::InvalidNumber),
        _ => Err(CapacityError::InvalidNumber),
    }
}
fn float(value: &Value) -> Result<f64, CapacityError> {
    match value {
        Value::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
        Value::Number(value) => value.as_f64().ok_or(CapacityError::InvalidNumber),
        Value::String(value) => numeric_text(value)?
            .parse()
            .map_err(|_| CapacityError::InvalidNumber),
        _ => Err(CapacityError::InvalidNumber),
    }
}
fn preferred<'a>(config: &'a Map<String, Value>, first: &str, second: &str) -> Option<&'a Value> {
    config.get(first).or_else(|| config.get(second))
}
fn override_budget(
    mut budget: CapacityBudget,
    config: &Map<String, Value>,
) -> Result<CapacityBudget, CapacityError> {
    if config.is_empty() {
        return Ok(budget);
    }
    if let Some(value) =
        preferred(config, "concurrency_limit", "limit").filter(|value| !value.is_null())
    {
        budget.limit = integer(value)?.max(1);
    }
    if let Some(value) =
        preferred(config, "queue_max", "max_queue_size").filter(|value| !value.is_null())
    {
        budget.queue_max = integer(value)?.max(0);
    }
    if let Some(value) = config
        .get("queue_timeout_ms")
        .filter(|value| !value.is_null())
    {
        budget.queue_timeout_ms = integer(value)?.max(1);
    } else if let Some(value) = config
        .get("queue_timeout_seconds")
        .filter(|value| !value.is_null())
    {
        budget.queue_timeout_ms = truncated_integer(float(value)? * 1000.0)?.max(1);
    }
    budget.source = "service_config".into();
    Ok(budget)
}
fn configured_or_missing(
    key: String,
    config: &Map<String, Value>,
) -> Result<CapacityBudget, CapacityError> {
    let mut budget = CapacityBudget {
        scope: if key.starts_with("service.") {
            "service"
        } else {
            "upstream"
        }
        .into(),
        key,
        limit: 1,
        queue_max: 0,
        queue_timeout_ms: 1,
        source: "missing".into(),
        enforced: false,
        source_status: "missing".into(),
        shared: false,
    };
    if preferred(config, "concurrency_limit", "limit").is_none_or(Value::is_null) {
        return Ok(budget);
    }
    budget.enforced = true;
    budget.shared = true;
    budget.source_status = "real".into();
    budget.queue_max = 16;
    budget.queue_timeout_ms = 3000;
    override_budget(budget, config)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapacityRequest {
    pub tenant_id: String,
    pub service_id: String,
    pub request_class: String,
    #[serde(default)]
    pub upstream_group: Value,
    #[serde(default)]
    pub provider_id: Value,
    #[serde(default)]
    pub is_admin_read: bool,
    #[serde(default)]
    pub service_config: Value,
    #[serde(default = "request_kind")]
    pub resource_kind: String,
}
fn request_kind() -> String {
    "request".into()
}

#[derive(Clone, Debug)]
pub struct CapacityResolver {
    pub mode: String,
    pub cluster_epoch: String,
}
impl Default for CapacityResolver {
    fn default() -> Self {
        Self {
            mode: "single-node".into(),
            cluster_epoch: "uat-2026-05".into(),
        }
    }
}
impl CapacityResolver {
    pub fn resolve(&self, request: &CapacityRequest) -> Result<Vec<CapacityBudget>, CapacityError> {
        // Deliberately global budget keys: Python does not use tenant_id here.
        let config = capacity_config_from_service(Some(&request.service_config));
        let kind = request.resource_kind.as_str();
        let mut budgets = if kind == "request" {
            vec![
                default_budget("gateway.total_inflight").expect("static budget"),
                default_budget(if request.request_class == "stream" {
                    "gateway.stream_inflight"
                } else {
                    "gateway.non_stream_inflight"
                })
                .expect("static budget"),
            ]
        } else {
            let limit = match kind {
                "run" => 8,
                "provider" => 32,
                "capability" => 16,
                "job" => 4,
                "sse" => 64,
                _ => return Err(CapacityError::UnknownResourceKind),
            };
            vec![CapacityBudget {
                key: format!("gateway.{kind}_inflight"),
                limit,
                queue_max: 32,
                queue_timeout_ms: 3000,
                scope: kind.into(),
                source: "default".into(),
                enforced: true,
                source_status: "real".into(),
                shared: kind != "sse",
            }]
        };
        if kind == "sse" || request.is_admin_read {
            return Ok(budgets);
        }
        let group = service_upstream_group(
            &request.service_id,
            config
                .get("upstream_group")
                .filter(|value| truthy(value))
                .unwrap_or(&request.upstream_group),
        );
        if let Some(group) = group {
            let key = format!("upstream.{group}");
            budgets.push(if let Some(default) = default_budget(&key) {
                override_budget(default, &config)?
            } else {
                configured_or_missing(key, &config)?
            });
        } else {
            budgets.push(configured_or_missing(
                format!(
                    "service.{}",
                    if request.service_id.is_empty() {
                        "unknown"
                    } else {
                        &request.service_id
                    }
                ),
                &config,
            )?);
        }
        if kind != "request" {
            for budget in &mut budgets {
                if matches!(budget.scope.as_str(), "upstream" | "service") {
                    budget.key = format!("{kind}.{}", budget.key);
                }
            }
        }
        if matches!(kind, "request" | "provider")
            && let Some(key) = provider_budget_key(&request.provider_id)
        {
            budgets.push(default_budget(&key).expect("validated static budget"));
        }
        let mut deduped: Vec<CapacityBudget> = Vec::new();
        for budget in budgets {
            if let Some(index) = deduped.iter().position(|old| old.key == budget.key) {
                deduped[index] = budget;
            } else {
                deduped.push(budget);
            }
        }
        Ok(deduped)
    }

    pub fn status_budgets(&self, snapshot: Option<&Value>) -> Result<Vec<Value>, CapacityError> {
        let snapshot = snapshot.filter(|value| truthy(value));
        let snapshot = snapshot
            .map(|value| value.as_object().ok_or(CapacityError::InvalidSnapshot))
            .transpose()?;
        let mut budgets = default_budgets();
        budgets.sort_by(|left, right| left.key.cmp(&right.key));
        budgets
            .into_iter()
            .map(|budget| {
                let usage = snapshot
                    .and_then(|snapshot| snapshot.get(&budget.key))
                    .map(|usage| usage.as_object().ok_or(CapacityError::InvalidSnapshot))
                    .transpose()?;
                let count = |key| -> Result<i64, CapacityError> {
                    usage
                        .and_then(|usage| usage.get(key))
                        .filter(|value| truthy(value))
                        .map(integer)
                        .transpose()
                        .map(|value| value.unwrap_or(0))
                };
                Ok(budget.to_status(count("inflight")?, count("queue_depth")?))
            })
            .collect()
    }
}

