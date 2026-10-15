//! Configuration audit writer migrated from `src/services/metrics/audit_event_writer.py`.
//!
//! No constructor connects, writes, starts a task, or reads environment state. The
//! injected service wins whenever present; a service error is never retried through
//! the pool. Missing database returns before touching request/payload collaborators.
//! Missing pool returns only after building/normalizing the event, just like Python.
//! Errors propagate and this module emits no logs. Cancellation drops the awaited
//! collaborator future; it cannot undo an INSERT already accepted by the database.
//!
//! Payload boundary: JSON string keys, finite JSON numbers, rust_decimal's finite
//! 96-bit Decimal, microsecond naive/fixed-offset datetimes (years 1..=9999), and
//! UUID. Integral Decimal output must fit i64/u64. Python bytes/custom objects,
//! non-string keys, arbitrary integers and nonfinite values are not claimed.
//! Datetime offset fractions, leap seconds and submicrosecond precision are rejected.
//! Redaction precedes encoding, including masking unsupported sensitive values.
//! FastAPI's default sqlalchemy_safe removal of `_sa*` keys is preserved.
use crate::{
    client_ip::{TrustedProxies, get_client_ip},
    metrics_redaction::{is_sensitive_key, redact_sensitive_data},
    service_registry::python_truthy,
    usage_parser::python_value_to_string,
};
use ai_platform_auth::{admission::RequestPrincipal, service::Principal};
use axum::http::HeaderMap;
use chrono::{DateTime, Datelike, FixedOffset, NaiveDateTime, Timelike};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{future::Future, pin::Pin, sync::Arc};

pub type ConfigAuditFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), ConfigAuditError>> + Send + 'a>>;

/// Errors remain observable, without exposing backend messages or raw payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigAuditError {
    UnsupportedValue,
    IntegerRange,
    DatetimeRange,
    Service,
    PoolAcquire,
    PoolExecute,
    PoolRelease,
    /// Explicit cancellation reported by an injected Python-compatible boundary.
    Cancelled,
}
impl std::fmt::Display for ConfigAuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedValue => "config_audit_unsupported_value",
            Self::IntegerRange => "config_audit_integer_range",
            Self::DatetimeRange => "config_audit_datetime_range",
            Self::Service => "config_audit_service_error",
            Self::PoolAcquire => "config_audit_pool_acquire_error",
            Self::PoolExecute => "config_audit_pool_execute_error",
            Self::PoolRelease => "config_audit_pool_release_error",
            Self::Cancelled => "config_audit_cancelled",
        })
    }
}
impl std::error::Error for ConfigAuditError {}

/// Typed values that exist before FastAPI jsonable_encoder. Object order matters
/// for the source's json.dumps output; duplicate input keys are not supported.
#[derive(Clone)]
pub enum ConfigAuditValue {
    Json(Value),
    Decimal(Decimal),
    NaiveDatetime(NaiveDateTime),
    Datetime(DateTime<FixedOffset>),
    Uuid(uuid::Uuid),
    Object(Vec<(String, ConfigAuditValue)>),
    Array(Vec<ConfigAuditValue>),
    /// Explicit host boundary rejection, never an arbitrary Python object encoder.
    Unsupported,
}
impl From<Value> for ConfigAuditValue {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}
impl ConfigAuditValue {
    fn truthy(&self) -> bool {
        match self {
            Self::Json(value) => python_truthy(value),
            Self::Object(values) => !values.is_empty(),
            Self::Array(values) => !values.is_empty(),
            Self::Decimal(value) => !value.is_zero(),
            _ => true,
        }
    }
    fn has_api_key(&self) -> bool {
        let matches = |key: &str| matches!(key.to_lowercase().as_str(), "api_key" | "_api_key");
        match self {
            Self::Json(Value::Object(values)) => values.keys().any(|key| matches(key)),
            Self::Object(values) => values.iter().any(|(key, _)| matches(key)),
            _ => false,
        }
    }
    fn redacted(&self) -> Self {
        match self {
            Self::Json(value) => Self::Json(redact_sensitive_data(value)),
            Self::Object(values) => Self::Object(
                values
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            if is_sensitive_key(key) {
                                Self::Json(json!("***"))
                            } else {
                                value.redacted()
                            },
                        )
                    })
                    .collect(),
            ),
            Self::Array(values) => Self::Array(values.iter().map(Self::redacted).collect()),
            _ => self.clone(),
        }
    }
    fn encoded(self) -> Result<Value, ConfigAuditError> {
        Ok(match self {
            Self::Json(value) => sqlalchemy_safe(value),
            Self::Decimal(value) => {
                // fastapi.encoders.decimal_encoder uses int only when exponent >= 0.
                if value.scale() == 0 {
                    if let Ok(value) = i64::try_from(value.mantissa()) {
                        json!(value)
                    } else if let Ok(value) = u64::try_from(value.mantissa()) {
                        json!(value)
                    } else {
                        return Err(ConfigAuditError::IntegerRange);
                    }
                } else {
                    let number = value
                        .to_string()
                        .parse::<f64>()
                        .map_err(|_| ConfigAuditError::UnsupportedValue)?;
                    Value::Number(
                        serde_json::Number::from_f64(number)
                            .ok_or(ConfigAuditError::UnsupportedValue)?,
                    )
                }
            }
            Self::NaiveDatetime(value) => json!(datetime_text(value)?),
            Self::Datetime(value) => {
                let mut text = datetime_text(value.naive_local())?;
                let seconds = value.offset().local_minus_utc();
                let absolute = seconds.unsigned_abs();
                text.push_str(&format!(
                    "{}{:02}:{:02}",
                    if seconds < 0 { '-' } else { '+' },
                    absolute / 3600,
                    absolute % 3600 / 60
                ));
                if absolute % 60 != 0 {
                    text.push_str(&format!(":{:02}", absolute % 60));
                }
                json!(text)
            }
            Self::Uuid(value) => json!(value.hyphenated().to_string()),
            Self::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(Self::encoded)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Object(values) => {
                let mut output = Map::new();
                for (key, value) in values {
                    if !key.starts_with("_sa") {
                        output.insert(key, value.encoded()?);
                    }
                }
                Value::Object(output)
            }
            Self::Unsupported => return Err(ConfigAuditError::UnsupportedValue),
        })
    }
}
fn datetime_text(value: NaiveDateTime) -> Result<String, ConfigAuditError> {
    if !(1..=9999).contains(&value.year())
        || value.nanosecond() >= 1_000_000_000
        || !value.nanosecond().is_multiple_of(1000)
    {
        return Err(ConfigAuditError::DatetimeRange);
    }
    let mut text = value.format("%Y-%m-%dT%H:%M:%S").to_string();
    if value.nanosecond() != 0 {
        text.push_str(&format!(".{:06}", value.nanosecond() / 1000));
    }
    Ok(text)
}
fn sqlalchemy_safe(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .filter(|(key, _)| !key.starts_with("_sa"))
                .map(|(key, value)| (key, sqlalchemy_safe(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sqlalchemy_safe).collect()),
        other => other,
    }
}

/// The caller supplies already-established authority, never identity from headers
/// or configuration payloads. Both existing gateway auth principals are supported.
pub trait ConfigAuditIdentity: Send + Sync {
    fn user_id(&self) -> &str;
    fn tenant_id(&self) -> &str;
}
impl ConfigAuditIdentity for RequestPrincipal {
    fn user_id(&self) -> &str {
        self.user_id()
    }
    fn tenant_id(&self) -> &str {
        self.tenant_id()
    }
}
impl ConfigAuditIdentity for Principal {
    fn user_id(&self) -> &str {
        &self.account.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.account.tenant_id
    }
}

/// Request state values use the JSON scalar/container boundary for Python str().
/// HeaderMap preserves first-value, case-insensitive Starlette header semantics.
#[derive(Clone, Default)]
pub struct ConfigAuditRequestContext {
    pub request_id: Option<Value>,
    pub trace_id: Option<Value>,
    pub headers: HeaderMap,
    pub client_host: Option<String>,
}
impl ConfigAuditRequestContext {
    pub fn request_id(&self) -> String {
        self.request_id
            .as_ref()
            .filter(|value| python_truthy(value))
            .map(python_value_to_string)
            .unwrap_or_default()
    }
    pub fn trace_id(&self) -> String {
        self.trace_id
            .as_ref()
            .filter(|value| python_truthy(value))
            .map(python_value_to_string)
            .unwrap_or_else(|| self.request_id())
    }
    fn user_agent(&self) -> String {
        self.headers
            .get("user-agent")
            .map(|value| value.as_bytes().iter().copied().map(char::from).collect())
            .unwrap_or_default()
    }
}
pub trait ConfigAuditClientIp: Send + Sync {
    fn resolve(&self, request: &ConfigAuditRequestContext) -> String;
}
#[derive(Default)]
pub struct ProxyAwareConfigAuditClientIp {
    pub trusted: Option<TrustedProxies>,
}
impl ConfigAuditClientIp for ProxyAwareConfigAuditClientIp {
    fn resolve(&self, request: &ConfigAuditRequestContext) -> String {
        get_client_ip(
            &request.headers,
            request.client_host.as_deref(),
            self.trusted.as_ref(),
        )
    }
}

pub struct ConfigChange<'a> {
    pub resource_type: &'a str,
    pub resource_id: &'a str,
    pub action: &'a str,
    pub before: Option<&'a ConfigAuditValue>,
    pub after: Option<&'a ConfigAuditValue>,
}
#[derive(Clone, Serialize, PartialEq)]
pub struct ConfigAuditEvent {
    pub event_type: &'static str,
    pub user_id: Option<String>,
    pub tenant_id: String,
    pub ip_address: String,
    pub user_agent: String,
    pub resource_type: String,
    pub resource_id: String,
    pub action: String,
    pub request_summary: Value,
    pub response_summary: Value,
    pub status: &'static str,
    pub error_message: Option<String>,
    pub duration_ms: Option<i64>,
}
pub fn audit_payload(
    request: &ConfigAuditRequestContext,
    before: Option<&ConfigAuditValue>,
    after: Option<&ConfigAuditValue>,
) -> Result<Value, ConfigAuditError> {
    let encode = |value: Option<&ConfigAuditValue>| {
        value
            .filter(|value| value.truthy())
            .map_or_else(|| Ok(json!({})), |value| value.redacted().encoded())
    };
    Ok(json!({
        "trace_id": request.trace_id(),
        "request_id": request.request_id(),
        "before": encode(before)?,
        "after": encode(after)?,
    }))
}

pub trait ConfigAuditService: Send + Sync {
    fn record_audit_event<'a>(&'a self, event: &'a ConfigAuditEvent) -> ConfigAuditFuture<'a>;
}
/// Caller-owned pool adapter boundary includes acquire, execute and context release.
/// It must release its connection on success, error and cancellation. This module
/// neither invents a transaction nor connects to a live database. The SQL and
/// ordered positional values supplied here are the direct source fallback.
pub trait ConfigAuditPool: Send + Sync {
    fn execute_in_acquired_connection<'a>(
        &'a self,
        statement: &'a ConfigAuditStatement,
    ) -> ConfigAuditFuture<'a>;
}
pub trait ConfigAuditDatabase: Send + Sync {
    fn audit_service(&self) -> Option<&dyn ConfigAuditService>;
    fn pool(&self) -> Option<&dyn ConfigAuditPool>;
}
#[derive(Clone, PartialEq)]
pub struct ConfigAuditStatement {
    pub sql: &'static str,
    pub arguments: Vec<Value>,
}
pub const CONFIG_AUDIT_INSERT_SQL: &str = "\n            INSERT INTO audit_logs (\n                event_type, user_id, tenant_id, ip_address, user_agent,\n                resource_type, resource_id, action,\n                request_summary, response_summary, status, error_message, duration_ms\n            ) VALUES (\n                $1, $2, $3, $4, $5,\n                $6, $7, $8,\n                $9::jsonb, $10::jsonb, $11, $12, $13\n            )\n            ";

// Source json.dumps(..., ensure_ascii=False): spaces and Python float rendering
// matter at this injected SQL boundary. Object order follows the input order.
fn json_dumps(value: &Value) -> String {
    match value {
        Value::Number(_) => python_value_to_string(value),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(json_dumps).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    serde_json::to_string(key).expect("JSON string"),
                    json_dumps(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => serde_json::to_string(value).expect("JSON scalar"),
    }
}
impl ConfigAuditEvent {
    pub fn fallback_statement(&self) -> ConfigAuditStatement {
        ConfigAuditStatement {
            sql: CONFIG_AUDIT_INSERT_SQL,
            arguments: vec![
                json!(self.event_type),
                json!(self.user_id),
                json!(self.tenant_id),
                json!(self.ip_address),
                json!(self.user_agent),
                json!(self.resource_type),
                json!(self.resource_id),
                json!(self.action),
                json!(json_dumps(&self.request_summary)),
                json!(json_dumps(&self.response_summary)),
                json!(self.status),
                json!(self.error_message),
                json!(self.duration_ms),
            ],
        }
    }
}
#[derive(Clone)]
pub struct ConfigAuditWriter {
    database: Option<Arc<dyn ConfigAuditDatabase>>,
    client_ip: Arc<dyn ConfigAuditClientIp>,
}
impl ConfigAuditWriter {
    pub fn new(
        database: Option<Arc<dyn ConfigAuditDatabase>>,
        client_ip: Arc<dyn ConfigAuditClientIp>,
    ) -> Self {
        Self {
            database,
            client_ip,
        }
    }
    pub async fn record_config_change(
        &self,
        request: &ConfigAuditRequestContext,
        auth: &dyn ConfigAuditIdentity,
        change: ConfigChange<'_>,
    ) -> Result<(), ConfigAuditError> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let event = ConfigAuditEvent {
            event_type: "config_changed",
            user_id: (!auth.user_id().is_empty()).then(|| auth.user_id().to_owned()),
            tenant_id: if auth.tenant_id().is_empty() {
                "public"
            } else {
                auth.tenant_id()
            }
            .to_owned(),
            ip_address: self.client_ip.resolve(request),
            user_agent: request.user_agent(),
            resource_type: change.resource_type.to_owned(),
            resource_id: change.resource_id.to_owned(),
            action: change.action.to_owned(),
            request_summary: audit_payload(request, change.before, change.after)?,
            response_summary: json!({"status": "success", "has_api_key": change.after.is_some_and(ConfigAuditValue::has_api_key)}),
            status: "success",
            error_message: None,
            duration_ms: None,
        };
        if let Some(service) = database.audit_service() {
            return service.record_audit_event(&event).await;
        }
        let Some(pool) = database.pool() else {
            return Ok(());
        };
        pool.execute_in_acquired_connection(&event.fallback_statement())
            .await
    }
}
