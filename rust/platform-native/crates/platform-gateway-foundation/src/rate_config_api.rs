//! Unmounted rate-configuration API from `src/api/v1/config.py`.
//!
//! GET retains the source's roles-only `admin` check and legacy error envelope;
//! writes use the mature GatewayRateLimitWrite capability, including explicit
//! permissions. No platform-admin/tenant restriction or positivity/enum validation
//! is invented. These global-state permissions require host acceptance before mount.
//!
//! Memory is the supplied mature RuntimeRateRules, never a second rule store.
//! Every runtime GET and successful mutation publishes that source then invalidates
//! the supplied mature resolver, before awaiting the mature configuration audit.
//! DB GET does neither. DB POST deliberately forwards only five values, so request
//! strategy/enabled survive in the audit but not the SQL write. Duplicate runtime
//! rules, disabled runtime rows, DB enabled-only reads, and absent-row DELETE success
//! are canonical behavior. Audit failures cannot undo earlier effects.
//!
//! Construction is inert: no connections, tasks, listener, environment or secrets.
//! The optional SQL adapter preserves full API rows, not the resolver's reduced
//! projection. Native PostgreSQL uses the existing owned-operation supervisor;
//! hosts must abort/join callers then drain_cancelled before closing either pool.
//! Transaction/pool supervision has the mature supervisor's documented timing and
//! cancellation limitations; already-dispatched COMMIT cannot be undone. An explicit
//! timestamp renderer is required rather than inventing the source host's policy.
//!
//! JSON integers are bounded to i64; arbitrary Python integers/objects/nonfinite
//! JSON are not claimed. Semantic validation follows canonical Pydantic. Malformed
//! JSON diagnostics/body limits and invalid URL encoding are transport boundaries.
//! HEAD/method rejection occurs before identity or collaborators. Trailing-slash
//! redirects require trusted host origin policy; this factory returns 404 instead.
//! Cancellation propagates separately and requires explicit host transport policy.
use crate::{
    config_audit_writer::{
        ConfigAuditError, ConfigAuditIdentity, ConfigAuditRequestContext, ConfigAuditValue,
        ConfigAuditWriter, ConfigChange,
    },
    postgres_operation::{PgOperationError, PgOperationStorage},
    rate_policy_store::{RatePolicyResolver, RuntimeRateRules},
    service_registry::python_truthy,
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{MatchedPath, Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{collections::HashMap, fmt, future::Future, pin::Pin, sync::Arc};

pub const RATE_CONFIG_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/config/rate-limits", "list_rate_limits"),
    ("POST", "/config/rate-limits", "create_rate_limit"),
    (
        "DELETE",
        "/config/rate-limits/{scope}/{scope_id}",
        "delete_rate_limit",
    ),
];
pub type RateConfigFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RateConfigError>> + Send + 'a>>;
#[derive(Clone, PartialEq)]
pub enum RateConfigError {
    Http { status: u16, body: Value },
    Internal,
    Cancelled,
}
impl fmt::Debug for RateConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "RateConfigError::Http({status})"),
            Self::Internal => f.write_str("RateConfigError::Internal"),
            Self::Cancelled => f.write_str("RateConfigError::Cancelled"),
        }
    }
}
impl RateConfigError {
    pub fn detail(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            body: json!({"detail":detail.into()}),
        }
    }
    pub fn validation(errors: Vec<Value>) -> Self {
        Self::detail(422, Value::Array(errors))
    }
    pub fn response(&self, context: &ConfigAuditRequestContext) -> Option<(u16, Value)> {
        match self {
            Self::Http { status, body } => Some((*status, body.clone())),
            Self::Cancelled => None,
            Self::Internal => {
                let mut body = json!({"error":{"code":2000,"message":"Internal server error"}});
                let trace = context.trace_id.clone().unwrap_or_else(|| {
                    ["x-trace-id", "x-request-id"]
                        .iter()
                        .find_map(|name| {
                            context
                                .headers
                                .get(*name)
                                .filter(|v| !v.is_empty())
                                .map(|v| {
                                    v.as_bytes()
                                        .iter()
                                        .copied()
                                        .map(char::from)
                                        .collect::<String>()
                                })
                        })
                        .map_or(Value::Null, |v| json!(v))
                });
                if python_truthy(&trace) {
                    body["error"]["trace_id"] = trace;
                }
                Some((500, body))
            }
        }
    }
}
impl From<ConfigAuditError> for RateConfigError {
    fn from(value: ConfigAuditError) -> Self {
        if value == ConfigAuditError::Cancelled {
            Self::Cancelled
        } else {
            Self::Internal
        }
    }
}
impl From<PgOperationError> for RateConfigError {
    fn from(value: PgOperationError) -> Self {
        if value == PgOperationError::Cancelled {
            Self::Cancelled
        } else {
            Self::Internal
        }
    }
}
/// Trusted output only; no Deserialize implementation or public-header authority.
#[derive(Clone, Default)]
pub struct RateConfigIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}
impl ConfigAuditIdentity for RateConfigIdentity {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
pub trait RateConfigIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> RateConfigFuture<'a, RateConfigIdentity>;
    /// Host-owned request state, never inferred from body/query fields.
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
    /// Host owns canonical best-effort denial event scheduling and its lifetime.
    fn permission_denied(
        &self,
        _identity: &RateConfigIdentity,
        _context: &ConfigAuditRequestContext,
    ) {
    }
}
impl RateConfigIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> RateConfigFuture<'a, RateConfigIdentity> {
        Box::pin(async move {
            let admission = self.request(headers, path);
            let user = admission
                .resolve()
                .await
                .map_err(|e| RateConfigError::Http {
                    status: e.status(),
                    body: json!({"error":e.to_string()}),
                })?;
            Ok(RateConfigIdentity {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
                roles: user.roles().to_vec(),
                permissions: user.permissions().to_vec(),
            })
        })
    }
}
/// Source app.state.rate_limit_rules publication. Host receives the SAME mature
/// source used by the resolver. No permission/event/cache policy is hidden here.
pub trait RateConfigPublication: Send + Sync {
    fn publish(&self, rules: Arc<RuntimeRateRules>);
}
pub struct NoopRateConfigPublication;
impl RateConfigPublication for NoopRateConfigPublication {
    fn publish(&self, _rules: Arc<RuntimeRateRules>) {}
}
#[derive(Clone, Debug, PartialEq)]
pub struct RateLimitSave {
    pub scope: String,
    pub scope_id: String,
    pub requests: i64,
    pub window: i64,
    pub burst: i64,
}
pub trait RateConfigDatabase: Send + Sync {
    fn enabled(&self) -> bool;
    fn supports_delete(&self) -> bool {
        true
    }
    fn list(&self) -> RateConfigFuture<'_, Vec<Value>>;
    fn save<'a>(&'a self, rule: &'a RateLimitSave) -> RateConfigFuture<'a, ()>;
    fn delete<'a>(&'a self, scope: &'a str, scope_id: &'a str) -> RateConfigFuture<'a, bool>;
}
#[derive(Clone, Debug)]
pub enum RateConfigOperation {
    List,
    Create,
    Delete { scope: String, scope_id: String },
}
impl RateConfigOperation {
    fn path(&self) -> String {
        match self {
            Self::List | Self::Create => "/api/v1/config/rate-limits".into(),
            Self::Delete { scope, scope_id } => {
                format!("/api/v1/config/rate-limits/{scope}/{scope_id}")
            }
        }
    }
}
pub struct RateConfigApi {
    identity: Arc<dyn RateConfigIdentityResolver>,
    rbac: Rbac,
    database: Option<Arc<dyn RateConfigDatabase>>,
    runtime: Arc<RuntimeRateRules>,
    resolver: RatePolicyResolver,
    publication: Arc<dyn RateConfigPublication>,
    audit: ConfigAuditWriter,
}
impl RateConfigApi {
    pub fn new(
        identity: Arc<dyn RateConfigIdentityResolver>,
        rbac: Rbac,
        database: Option<Arc<dyn RateConfigDatabase>>,
        runtime: Arc<RuntimeRateRules>,
        resolver: RatePolicyResolver,
        publication: Arc<dyn RateConfigPublication>,
        audit: ConfigAuditWriter,
    ) -> Self {
        Self {
            identity,
            rbac,
            database,
            runtime,
            resolver,
            publication,
            audit,
        }
    }
    fn sync(&self) {
        self.publication.publish(self.runtime.clone());
        self.resolver.invalidate();
    }
    pub async fn handle(
        &self,
        operation: RateConfigOperation,
        headers: &HeaderMap,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, RateConfigError> {
        let identity = self.identity.resolve(headers, &operation.path()).await?;
        self.execute(operation, &identity, body, context).await
    }
    /// Direct endpoint-model boundary uses RateLimitRule.model_validate semantics.
    /// HTTP top-level required-body/from-attributes differences are handled below.
    pub async fn execute(
        &self,
        operation: RateConfigOperation,
        identity: &RateConfigIdentity,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, RateConfigError> {
        let rule = if matches!(operation, RateConfigOperation::Create) {
            Some(validate_rate_rule(body)?)
        } else {
            None
        };
        if matches!(operation, RateConfigOperation::List) {
            self.rbac
                .require(&identity.roles, "admin")
                .map_err(|error| RateConfigError::Http {
                    status: 403,
                    body: json!({"error":error}),
                })?;
        } else {
            let roles: Vec<_> = identity
                .roles
                .iter()
                .map(|s| s.trim_matches(python_whitespace).to_owned())
                .collect();
            let permissions: Vec<_> = identity
                .permissions
                .iter()
                .map(|s| s.trim_matches(python_whitespace).to_owned())
                .collect();
            if !check_capability(
                &self.rbac,
                &roles,
                &permissions,
                Capability::GatewayRateLimitWrite,
            )
            .allowed()
            {
                self.identity.permission_denied(identity, context);
                let trace = context
                    .request_id
                    .as_ref()
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        context
                            .trace_id
                            .as_ref()
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                    })
                    .unwrap_or("");
                return Err(RateConfigError::detail(
                    403,
                    permission_denied_detail(Capability::GatewayRateLimitWrite, trace, None),
                ));
            }
        }
        let database = self.database.as_ref().filter(|db| db.enabled());
        match operation {
            RateConfigOperation::List => {
                if let Some(db) = database {
                    return Ok(json!({"rules":db.list().await?}));
                }
                self.sync();
                Ok(json!({"rules":self.runtime.snapshot()}))
            }
            RateConfigOperation::Create => {
                let rule = rule.expect("create was validated");
                let scope = rule["scope"].as_str().expect("validated scope");
                let scope_id = rule["scope_id"].as_str().unwrap_or("");
                if let Some(db) = database {
                    db.save(&RateLimitSave {
                        scope: scope.into(),
                        scope_id: scope_id.into(),
                        requests: rule["requests"].as_i64().unwrap(),
                        window: rule["window"].as_i64().unwrap(),
                        burst: rule["burst"].as_i64().unwrap(),
                    })
                    .await?;
                } else {
                    self.runtime.update(|rules| rules.push(rule.clone()));
                }
                self.sync();
                let after = ConfigAuditValue::from(rule.clone());
                self.audit
                    .record_config_change(
                        context,
                        identity,
                        ConfigChange {
                            resource_type: "rate_limit",
                            resource_id: &format!("{scope}:{scope_id}"),
                            action: "create",
                            before: None,
                            after: Some(&after),
                        },
                    )
                    .await?;
                Ok(json!({"status":"success","message":"限流规则已创建"}))
            }
            RateConfigOperation::Delete { scope, scope_id } => {
                if let Some(db) = database.filter(|db| db.supports_delete()) {
                    db.delete(&scope, &scope_id).await?;
                } else {
                    self.runtime.update(|rules| {
                        let mut retained = Vec::with_capacity(rules.len());
                        for rule in rules.iter() {
                            if !rule.is_object() {
                                return Err(RateConfigError::Internal);
                            }
                            if !(stored_text(rule.get("scope")) == scope
                                && stored_text(rule.get("scope_id")) == scope_id)
                            {
                                retained.push(rule.clone());
                            }
                        }
                        *rules = retained;
                        Ok(())
                    })?;
                }
                self.sync();
                let before = ConfigAuditValue::from(json!({"scope":scope,"scope_id":scope_id}));
                self.audit
                    .record_config_change(
                        context,
                        identity,
                        ConfigChange {
                            resource_type: "rate_limit",
                            resource_id: &format!("{scope}:{scope_id}"),
                            action: "delete",
                            before: Some(&before),
                            after: None,
                        },
                    )
                    .await?;
                Ok(json!({"status":"success","message":"限流规则已删除"}))
            }
        }
    }
}
fn stored_text(value: Option<&Value>) -> String {
    value
        .filter(|v| python_truthy(v))
        .map(python_value_to_string)
        .unwrap_or_default()
}
fn validation(kind: &str, field: Option<&str>, input: &Value) -> Value {
    let message = match kind {
        "missing" => "Field required",
        "model_type" => "Input should be a valid dictionary or instance of RateLimitRule",
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from"
        }
        "string_type" => "Input should be a valid string",
        "int_type" => "Input should be a valid integer",
        "int_parsing" => "Input should be a valid integer, unable to parse string as an integer",
        "int_from_float" => "Input should be a valid integer, got a number with a fractional part",
        "int_parsing_size" => "Unable to parse input string as an integer, exceeded maximum size",
        "bool_type" => "Input should be a valid boolean",
        "bool_parsing" => "Input should be a valid boolean, unable to interpret input",
        _ => "Invalid value",
    };
    let loc = field.map_or_else(|| json!(["body"]), |v| json!(["body", v]));
    let mut error = json!({"type":kind,"loc":loc,"msg":message,"input":input});
    if kind == "model_type" {
        error["ctx"] = json!({"class_name":"RateLimitRule"});
    }
    error
}
pub fn integer(value: &Value) -> Result<i64, &'static str> {
    match value {
        Value::Bool(v) => Ok(i64::from(*v)),
        Value::Number(v) => {
            if let Some(v) = v.as_i64() {
                return Ok(v);
            }
            if v.is_u64() {
                return Err("int_parsing_size");
            }
            let v = v.as_f64().ok_or("int_type")?;
            if v.fract() != 0.0 {
                return Err("int_from_float");
            }
            if v <= i64::MIN as f64 || v >= 9223372036854775808.0 {
                return Err("int_parsing_size");
            }
            Ok(v as i64)
        }
        Value::String(v) => {
            let raw = v.trim();
            let (digits, fraction) = raw
                .split_once('.')
                .map_or((raw, None), |(a, b)| (a, Some(b)));
            if fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b == b'0')) {
                return Err("int_parsing");
            }
            let bytes = digits.as_bytes();
            if bytes.iter().enumerate().any(|(i, b)| {
                *b == b'_'
                    && (i == 0
                        || !bytes[i - 1].is_ascii_digit()
                        || !bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
            }) {
                return Err("int_parsing");
            }
            digits.replace('_', "").parse::<i64>().map_err(|e| {
                if matches!(
                    e.kind(),
                    std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow
                ) {
                    "int_parsing_size"
                } else {
                    "int_parsing"
                }
            })
        }
        _ => Err("int_type"),
    }
}
pub fn boolean(value: &Value) -> Result<bool, &'static str> {
    match value {
        Value::Bool(v) => Ok(*v),
        Value::Number(v) => {
            if let Some(v) = v.as_i64() {
                return match v {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err("bool_parsing"),
                };
            }
            match v.as_f64() {
                Some(0.0) => Ok(false),
                Some(1.0) => Ok(true),
                Some(v) if v.fract() == 0.0 && v > i64::MIN as f64 && v < 9223372036854775808.0 => {
                    Err("bool_parsing")
                }
                _ => Err("bool_type"),
            }
        }
        Value::String(v) => match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "t" | "yes" | "y" | "on" => Ok(true),
            "0" | "false" | "f" | "no" | "n" | "off" => Ok(false),
            _ => Err("bool_parsing"),
        },
        _ => Err("bool_type"),
    }
}
pub fn validate_rate_rule(body: &Value) -> Result<Value, RateConfigError> {
    let Some(object) = body.as_object() else {
        return Err(RateConfigError::validation(vec![validation(
            "model_type",
            None,
            body,
        )]));
    };
    let mut result = serde_json::Map::new();
    let mut errors = Vec::new();
    for field in [
        "scope", "scope_id", "requests", "window", "burst", "strategy", "enabled",
    ] {
        let Some(value) = object.get(field) else {
            match field {
                "scope" | "requests" | "window" => {
                    errors.push(validation("missing", Some(field), body))
                }
                "scope_id" => {
                    result.insert(field.into(), Value::Null);
                }
                "burst" => {
                    result.insert(field.into(), json!(0));
                }
                "strategy" => {
                    result.insert(field.into(), json!("sliding_window"));
                }
                "enabled" => {
                    result.insert(field.into(), json!(true));
                }
                _ => unreachable!(),
            };
            continue;
        };
        let parsed = match field {
            "scope_id" if value.is_null() => Ok(Value::Null),
            "scope" | "scope_id" | "strategy" => {
                value.as_str().map(|s| json!(s)).ok_or("string_type")
            }
            "requests" | "window" | "burst" => integer(value).map(|v| json!(v)),
            "enabled" => boolean(value).map(|v| json!(v)),
            _ => unreachable!(),
        };
        match parsed {
            Ok(v) => {
                result.insert(field.into(), v);
            }
            Err(kind) => errors.push(validation(kind, Some(field), value)),
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(result))
    } else {
        Err(RateConfigError::validation(errors))
    }
}

// The API's full-row SQL boundary is distinct from PgRateRuleSource, which serves
// resolver projections. These constants are byte-checked against canonical AST.
pub const RATE_CONFIG_SAVE_SQL: &str = "\n                INSERT INTO rate_limit_config (\n                    scope, scope_id, requests, window_seconds, burst, strategy, priority\n                ) VALUES ($1, $2, $3, $4, $5, $6, $7)\n                ON CONFLICT (scope, scope_id) DO UPDATE SET\n                    requests = EXCLUDED.requests,\n                    window_seconds = EXCLUDED.window_seconds,\n                    burst = EXCLUDED.burst,\n                    strategy = EXCLUDED.strategy,\n                    priority = EXCLUDED.priority,\n                    updated_at = NOW()\n            ";
pub const RATE_CONFIG_SELECT_SQL: &str = "\n            SELECT id, scope, scope_id, requests, window_seconds,\n                   burst, strategy, enabled, priority, created_at, updated_at\n            FROM rate_limit_config WHERE 1=1\n        ";
pub const RATE_CONFIG_DELETE_SQL: &str =
    "DELETE FROM rate_limit_config WHERE scope = $1 AND scope_id = $2";
#[derive(Clone, Debug, PartialEq)]
pub struct RateConfigStatement {
    pub sql: String,
    pub arguments: Vec<Value>,
    pub fetch: bool,
}
#[derive(Clone, Default, PartialEq)]
pub struct RateConfigSqlResult {
    pub rows: Vec<Value>,
    pub affected: u64,
}
pub trait RateConfigPool: Send + Sync {
    /// Encompasses acquire/statement/release; release failure remains observable.
    fn run<'a>(
        &'a self,
        statement: RateConfigStatement,
    ) -> RateConfigFuture<'a, RateConfigSqlResult>;
}
pub struct SqlRateConfigDatabase {
    enabled: bool,
    pool: Option<Arc<dyn RateConfigPool>>,
}
impl SqlRateConfigDatabase {
    pub fn new(enabled: bool, pool: Option<Arc<dyn RateConfigPool>>) -> Self {
        Self { enabled, pool }
    }
    pub async fn get_rate_limits(
        &self,
        scope: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<Vec<Value>, RateConfigError> {
        let Some(pool) = &self.pool else {
            return Ok(vec![]);
        };
        let mut sql = RATE_CONFIG_SELECT_SQL.to_owned();
        let mut arguments = vec![];
        if let Some(scope) = scope.filter(|s| !s.is_empty()) {
            arguments.push(json!(scope));
            sql.push_str(&format!(" AND scope = ${}", arguments.len()));
        }
        if let Some(enabled) = enabled {
            arguments.push(json!(enabled));
            sql.push_str(&format!(" AND enabled = ${}", arguments.len()));
        }
        sql.push_str(" ORDER BY priority DESC, scope");
        Ok(pool
            .run(RateConfigStatement {
                sql,
                arguments,
                fetch: true,
            })
            .await?
            .rows)
    }
    pub async fn save_rate_limit(
        &self,
        rule: &RateLimitSave,
        strategy: &str,
        priority: i64,
    ) -> Result<(), RateConfigError> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        pool.run(RateConfigStatement {
            sql: RATE_CONFIG_SAVE_SQL.into(),
            arguments: vec![
                json!(rule.scope),
                json!(rule.scope_id),
                json!(rule.requests),
                json!(rule.window),
                json!(rule.burst),
                json!(strategy),
                json!(priority),
            ],
            fetch: false,
        })
        .await?;
        Ok(())
    }
}
impl RateConfigDatabase for SqlRateConfigDatabase {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn list(&self) -> RateConfigFuture<'_, Vec<Value>> {
        Box::pin(self.get_rate_limits(None, Some(true)))
    }
    fn save<'a>(&'a self, rule: &'a RateLimitSave) -> RateConfigFuture<'a, ()> {
        Box::pin(self.save_rate_limit(rule, "sliding_window", 0))
    }
    fn delete<'a>(&'a self, scope: &'a str, scope_id: &'a str) -> RateConfigFuture<'a, bool> {
        Box::pin(async move {
            let Some(pool) = &self.pool else {
                return Ok(false);
            };
            Ok(pool
                .run(RateConfigStatement {
                    sql: RATE_CONFIG_DELETE_SQL.into(),
                    arguments: vec![json!(scope), json!(scope_id)],
                    fetch: false,
                })
                .await?
                .affected
                == 1)
        })
    }
}
/// A host-supplied Python/driver timestamp projection. Input is an actual PG
/// timestamptz instant; formatter owns offset/precision spelling and range checks.
pub trait RateConfigTimestampFormatter: Send + Sync {
    fn format(&self, value: DateTime<Utc>) -> Result<String, RateConfigError>;
}
pub struct PgRateConfigPool {
    storage: PgOperationStorage,
    timestamps: Arc<dyn RateConfigTimestampFormatter>,
}
impl PgRateConfigPool {
    pub fn new(
        pool: PgPool,
        control_pool: PgPool,
        timestamps: Arc<dyn RateConfigTimestampFormatter>,
    ) -> Result<Self, RateConfigError> {
        Ok(Self {
            storage: PgOperationStorage::new(pool, control_pool)?,
            timestamps,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), RateConfigError> {
        self.storage.drain_cancelled().await.map_err(Into::into)
    }
}
impl RateConfigPool for PgRateConfigPool {
    fn run<'a>(
        &'a self,
        statement: RateConfigStatement,
    ) -> RateConfigFuture<'a, RateConfigSqlResult> {
        Box::pin(async move {
            let mut connection = self.storage.acquire().await?;
            let fetch = statement.fetch;
            let timestamps = self.timestamps.clone();
            let result = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query(&statement.sql);
                        for value in &statement.arguments {
                            query = match value {
                                Value::String(v) => query.bind(v),
                                Value::Bool(v) => query.bind(*v),
                                Value::Number(v) => query.bind(
                                    v.as_i64().and_then(|v| i32::try_from(v).ok()).ok_or_else(
                                        || {
                                            sqlx::Error::Protocol(
                                                "rate_config_integer_range".into(),
                                            )
                                        },
                                    )?,
                                ),
                                _ => {
                                    return Err(sqlx::Error::Protocol(
                                        "rate_config_invalid_binding".into(),
                                    ));
                                }
                            };
                        }
                        if !fetch {
                            return Ok((vec![], query.execute(connection).await?.rows_affected()));
                        }
                        Ok((query.fetch_all(connection).await?, 0))
                    })
                })
                .await?;
            let mut rows = vec![];
            for row in result.0 {
                let get_error = |_: sqlx::Error| RateConfigError::Internal;
                let created: Option<DateTime<Utc>> =
                    row.try_get("created_at").map_err(get_error)?;
                let updated: Option<DateTime<Utc>> =
                    row.try_get("updated_at").map_err(get_error)?;
                rows.push(json!({"id":row.try_get::<i32,_>("id").map_err(get_error)?,"scope":row.try_get::<String,_>("scope").map_err(get_error)?,"scope_id":row.try_get::<String,_>("scope_id").map_err(get_error)?,
                "requests":row.try_get::<i32,_>("requests").map_err(get_error)?,"window_seconds":row.try_get::<i32,_>("window_seconds").map_err(get_error)?,"burst":row.try_get::<i32,_>("burst").map_err(get_error)?,
                "strategy":row.try_get::<String,_>("strategy").map_err(get_error)?,"enabled":row.try_get::<bool,_>("enabled").map_err(get_error)?,"priority":row.try_get::<i32,_>("priority").map_err(get_error)?,
                "created_at":created.map(|v|timestamps.format(v)).transpose()?,"updated_at":updated.map(|v|timestamps.format(v)).transpose()?}));
            }
            Ok(RateConfigSqlResult {
                rows,
                affected: result.1,
            })
        })
    }
}

pub type RateConfigCancellationFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;
pub trait RateConfigCancellation: Send + Sync {
    fn cancelled(&self) -> RateConfigCancellationFuture<'_>;
}
#[derive(Clone)]
struct HttpState {
    api: Arc<RateConfigApi>,
    cancellation: Arc<dyn RateConfigCancellation>,
}
pub fn router(api: Arc<RateConfigApi>, cancellation: Arc<dyn RateConfigCancellation>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/rate-limits",
            any(method_fallback).get(http_list).post(http_create),
        )
        .route(
            "/api/v1/config/rate-limits/{scope}/{scope_id}",
            any(method_fallback).delete(http_delete),
        )
        .route_layer(middleware::from_fn(route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn method_fallback() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
async fn route_guard(
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let params = match params {
        Ok(Path(v)) => v,
        Err(e) => return e.into_response(),
    };
    let invalid = params.values().any(|v| v.contains('/'));
    let allowed: &[&str] = if matched.as_str() == "/api/v1/config/rate-limits" {
        &["GET", "POST"]
    } else {
        &["DELETE"]
    };
    if !invalid && allowed.contains(&request.method().as_str()) {
        return next.run(request).await;
    }
    // Axum sets Content-Length before suppressing the HEAD representation.
    if invalid {
        (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
    } else {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
async fn result(
    state: &HttpState,
    result: Result<Value, RateConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(RateConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let (status, body) = error.response(context).expect("noncancelled error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
fn http_context(
    state: &HttpState,
    headers: &HeaderMap,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
) -> ConfigAuditRequestContext {
    let mut context = trusted
        .map(|Extension(value)| value)
        .unwrap_or_else(|| state.api.identity.context(headers));
    context.headers = headers.clone();
    context
}
async fn http_list(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    result(
        &state,
        state
            .api
            .handle(RateConfigOperation::List, &headers, &Value::Null, &context)
            .await,
        &context,
    )
    .await
}
async fn http_delete(
    State(state): State<HttpState>,
    Path((scope, scope_id)): Path<(String, String)>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    result(
        &state,
        state
            .api
            .handle(
                RateConfigOperation::Delete { scope, scope_id },
                &headers,
                &Value::Null,
                &context,
            )
            .await,
        &context,
    )
    .await
}
async fn http_create(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        let is_json = crate::http_content_type::is_json_content_type(&headers);
        if is_json {
            match serde_json::from_slice(&bytes){Ok(v)=>v,Err(_)=>return result(&state,Err(RateConfigError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await}
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    };
    // FastAPI parses JSON first, resolves dependencies next, then validates fields.
    let identity = match state
        .api
        .identity
        .resolve(&headers, "/api/v1/config/rate-limits")
        .await
    {
        Ok(v) => v,
        Err(e) => return result(&state, Err(e), &context).await,
    };
    if !body.is_object() {
        let kind = if body.is_null() {
            "missing"
        } else {
            "model_attributes_type"
        };
        return result(
            &state,
            Err(RateConfigError::validation(vec![validation(
                kind, None, &body,
            )])),
            &context,
        )
        .await;
    }
    result(
        &state,
        state
            .api
            .execute(RateConfigOperation::Create, &identity, &body, &context)
            .await,
        &context,
    )
    .await
}
