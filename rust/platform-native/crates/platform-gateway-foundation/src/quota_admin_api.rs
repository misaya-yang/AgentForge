//! Opt-in, unmounted quota administration from `src/api/v1/quota.py`.
//!
//! Eleven routes retain the source's distinct direct/overview pool boundaries,
//! capability → tenant → subject order, nontransactional writes, audit-after-write
//! failure, read reauthorization after set/reset, and swallowed overview exceptions
//! (including partially incremented counts). Missing direct pool still acknowledges
//! alerts and audits reset requests. No scheduler reset, cache invalidation, pricing
//! conversion, background task, environment read, listener, or connection is created.
//!
//! Identity/roles/permissions come only from the injected trusted resolver; request
//! body/query identities are ignored. Host RBAC and permission-denial scheduling
//! are explicit collaborators. AdmissionService supports verified JWT/database key
//! authority; guest/disabled-auth modes require the host's AuthContext resolver.
//!
//! Native adapters reuse supervised PgQuotaStore and PgOperationStorage.
//! Constructors reject unsupervised stores and do not create pools or connections.
//! The host injects check policy/RPM/security effects through QuotaAdminChecker,
//! returning the mature billing::QuotaCheckResult. After aborting and joining
//! callers, drain the native service and all native direct/overview adapters
//! before shutting down pools/runtime. No production mount is added here.
//! Native timestamp binding is deliberately injected: Python passes naive datetime
//! unchanged to asyncpg's timestamptz codec. The existing asyncpg 0.31.0 cutoff
//! probe verified process-local interpretation (UTC/New_York process zones;
//! UTC/Honolulu DB zones). The host supplies that policy rather than silently
//! assigning UTC here. ProtocolTimestamp parses the existing native ISO subset,
//! not every CPython ISO/week-date spelling.
//!
//! Exact integral cents remain i64 throughout; request overflow fails 422 and native
//! store arithmetic remains checked. Python arbitrary integers, nonfinite numbers,
//! exotic int-string forms and response Python objects are outside this JSON/native
//! boundary. Overview ratio comparisons intentionally use source f64 presentation.
//! Invalid expiry detail is fixed safe text, rather than Python exception contents.
//! Injected backend exceptions are sanitized; service error values retain source
//! HTTPException detail semantics (null uses the HTTP status reason phrase). Response schemas remove undeclared fields.
//! Transport parse/content-type/body-limit diagnostics are Axum-specific; semantic
//! JSON/query errors are 422. The host supplies trusted request state/trace metadata.
//!
//! Cancellation never becomes a fabricated HTTP status. Dropping a future drops
//! its active collaborator, without retrying or starting later business work. Native
//! supervisors retain already-started statements only until terminal rollback/drain. Connection
//! adapters must release on drop; explicit close models context-exit failure.
//! Autocommit already accepted by the DB cannot be undone by cancelling the caller.
use crate::{
    billing::{QuotaCheckResult, effective_quota_limits},
    config_audit_writer::{
        ConfigAuditError, ConfigAuditIdentity, ConfigAuditRequestContext, ConfigAuditValue,
        ConfigAuditWriter, ConfigChange,
    },
    metrics_recorder::integer_ratio,
    postgres_operation::{PgOperationConnection, PgOperationError, PgOperationStorage},
    protocol::ProtocolTimestamp,
    quota_store::{PgQuotaStore, QuotaStoreError, QuotaUpdate},
    service_registry::python_truthy,
    usage_parser::python_whitespace,
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Json, Router,
    extract::{FromRequest, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, fmt, future::Future, pin::Pin, sync::Arc};

#[derive(Clone, PartialEq)]
pub enum QuotaAdminError {
    Http {
        status: u16,
        detail: Value,
    },
    ModelValidation(Vec<Value>),
    Internal,
    /// Redacted native failure; SQLSTATE is host-readable, never sent in HTTP.
    Database {
        sqlstate: Option<String>,
    },
    Cancelled,
}
impl fmt::Debug for QuotaAdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "QuotaAdminError::Http({status})"),
            Self::ModelValidation(_) => f.write_str("QuotaAdminError::ModelValidation"),
            Self::Internal => f.write_str("QuotaAdminError::Internal"),
            Self::Database { .. } => f.write_str("QuotaAdminError::Database"),
            Self::Cancelled => f.write_str("QuotaAdminError::Cancelled"),
        }
    }
}
impl QuotaAdminError {
    pub fn http(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: {
                let value = detail.into();
                if value.is_null() {
                    json!(
                        StatusCode::from_u16(status)
                            .ok()
                            .and_then(|s| s.canonical_reason())
                            .unwrap_or("Unknown Status")
                    )
                } else {
                    value
                }
            },
        }
    }
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate } => sqlstate.as_deref(),
            _ => None,
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::ModelValidation(_) => Some(400),
            Self::Internal | Self::Database { .. } => Some(500),
            Self::Cancelled => None,
        }
    }
    pub fn response_body(&self) -> Option<Value> {
        match self {
            Self::Http { detail, .. } => Some(json!({"detail":detail})),
            Self::ModelValidation(errors) => Some(
                json!({"error":{"code":1001,"message":"Request validation failed","details":{"errors":errors}}}),
            ),
            Self::Internal | Self::Database { .. } => {
                Some(json!({"error":{"code":2000,"message":"Internal server error"}}))
            }
            Self::Cancelled => None,
        }
    }
    /// Installed canonical ExceptionHandler behavior; HTTPException details are
    /// untouched. Presence of state.trace_id suppresses header fallback, even if
    /// that state value is empty/null. request_id state is not a fallback here.
    pub fn response_body_with_context(&self, context: &QuotaAdminContext) -> Option<Value> {
        let mut body = self.response_body()?;
        if matches!(
            self,
            Self::Internal | Self::Database { .. } | Self::ModelValidation(_)
        ) {
            let trace = context.audit.trace_id.clone().or_else(|| {
                context
                    .audit
                    .headers
                    .get("x-trace-id")
                    .filter(|v| !v.is_empty())
                    .or_else(|| context.audit.headers.get("x-request-id"))
                    .map(|v| {
                        json!(
                            v.as_bytes()
                                .iter()
                                .copied()
                                .map(char::from)
                                .collect::<String>()
                        )
                    })
            });
            if let Some(trace) = trace.filter(python_truthy) {
                body["error"]["trace_id"] = trace;
            }
        }
        Some(body)
    }
}
pub type QuotaAdminResult<T> = Result<T, QuotaAdminError>;
pub type QuotaAdminFuture<'a, T> = Pin<Box<dyn Future<Output = QuotaAdminResult<T>> + Send + 'a>>;
/// Trusted AuthContext output, intentionally not Deserialize and never extracted
/// from caller-controlled JSON, role headers, tenant headers or query parameters.
#[derive(Clone)]
pub struct QuotaAdminIdentity {
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    permissions: Vec<String>,
}
impl QuotaAdminIdentity {
    pub fn from_resolved_identity(
        user_id: String,
        tenant_id: String,
        roles: Vec<String>,
        permissions: Vec<String>,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            roles,
            permissions,
        }
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
    pub fn permissions(&self) -> &[String] {
        &self.permissions
    }
}
impl ConfigAuditIdentity for QuotaAdminIdentity {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
pub trait QuotaAdminIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> QuotaAdminFuture<'a, QuotaAdminIdentity>;
}
impl QuotaAdminIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> QuotaAdminFuture<'a, QuotaAdminIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let principal = request
                .resolve()
                .await
                .map_err(|e| QuotaAdminError::http(e.status(), e.to_string()))?;
            Ok(QuotaAdminIdentity::from_resolved_identity(
                principal.user_id().into(),
                principal.tenant_id().into(),
                principal.roles().to_vec(),
                principal.permissions().to_vec(),
            ))
        })
    }
}
#[derive(Clone, Default)]
pub struct QuotaAdminContext {
    pub audit: ConfigAuditRequestContext,
    pub path: String,
}
impl QuotaAdminContext {
    fn capability_trace(&self) -> &str {
        self.audit
            .request_id
            .as_ref()
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.audit
                    .trace_id
                    .as_ref()
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("")
    }
}
/// Host schedules canonical best-effort security notification synchronously. The
/// endpoint does not await it; tenant/subject denials intentionally do not call it.
pub trait QuotaPermissionDenied: Send + Sync {
    fn schedule(
        &self,
        context: &QuotaAdminContext,
        auth: &QuotaAdminIdentity,
        capability: Capability,
    );
}
pub trait QuotaAdminAuthorization: Send + Sync {
    fn require(
        &self,
        context: &QuotaAdminContext,
        auth: &QuotaAdminIdentity,
        capability: Capability,
    ) -> QuotaAdminResult<()>;
}
pub struct RbacQuotaAdminAuthorization {
    rbac: Rbac,
    denied: Arc<dyn QuotaPermissionDenied>,
}
impl RbacQuotaAdminAuthorization {
    pub fn new(rbac: Rbac, denied: Arc<dyn QuotaPermissionDenied>) -> Self {
        Self { rbac, denied }
    }
}
impl QuotaAdminAuthorization for RbacQuotaAdminAuthorization {
    fn require(
        &self,
        context: &QuotaAdminContext,
        auth: &QuotaAdminIdentity,
        capability: Capability,
    ) -> QuotaAdminResult<()> {
        let normalized = |values: &[String]| -> Vec<String> {
            values
                .iter()
                .map(|v| v.trim_matches(python_whitespace).to_owned())
                .collect()
        };
        if check_capability(
            &self.rbac,
            &normalized(&auth.roles),
            &normalized(&auth.permissions),
            capability,
        )
        .allowed()
        {
            Ok(())
        } else {
            self.denied.schedule(context, auth, capability);
            Err(QuotaAdminError::http(
                403,
                permission_denied_detail(capability, context.capability_trace(), None),
            ))
        }
    }
}
fn scope(
    context: &QuotaAdminContext,
    auth: &QuotaAdminIdentity,
    target: Option<&str>,
    cap: Capability,
) -> QuotaAdminResult<()> {
    let reason = if auth.tenant_id.is_empty() {
        Some("Permission denied: tenant scope required for quota operations")
    } else if target.is_some_and(|t| t != auth.user_id)
        && !auth.roles.iter().any(|r| r == "admin" || r == "operator")
    {
        Some("Permission denied: cannot access another user's quota")
    } else {
        None
    };
    if let Some(message) = reason {
        Err(QuotaAdminError::http(
            403,
            json!({"message":message,"required_capability":cap.name(),"required_permission":cap.canonical_permission(),"trace_id":context.audit.request_id()}),
        ))
    } else {
        Ok(())
    }
}

fn parse_quota_expiry(raw: &str) -> QuotaAdminResult<ProtocolTimestamp> {
    let invalid = || {
        QuotaAdminError::http(
            400,
            "Invalid temporary_expires_at: invalid ISO8601 timestamp",
        )
    };
    let value =
        ProtocolTimestamp::parse_isoformat(&raw.replace('Z', "+00:00")).map_err(|_| invalid())?;
    let local = match value {
        ProtocolTimestamp::Naive(v) => v,
        ProtocolTimestamp::Offset(v) => v.naive_local(),
    };
    // Python validates local civil year, not UTC year across an offset boundary.
    if !(1..=9999).contains(&local.year()) || local.nanosecond() >= 1_000_000_000 {
        return Err(invalid());
    }
    // CPython's fromisoformat truncates submicrosecond fractional seconds.
    let nanos = local.nanosecond() / 1000 * 1000;
    Ok(match value {
        ProtocolTimestamp::Naive(v) => {
            ProtocolTimestamp::Naive(v.with_nanosecond(nanos).ok_or_else(invalid)?)
        }
        ProtocolTimestamp::Offset(v) => {
            ProtocolTimestamp::Offset(v.with_nanosecond(nanos).ok_or_else(invalid)?)
        }
    })
}
#[derive(Clone)]
pub struct QuotaAdminSet {
    pub fields: Value,
    pub temporary_expires_at: Option<ProtocolTimestamp>,
}
pub trait QuotaAdminService: Send + Sync {
    fn get<'a>(&'a self, tenant: &'a str, user: &'a str) -> QuotaAdminFuture<'a, Option<Value>>;
    fn set<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        body: QuotaAdminSet,
    ) -> QuotaAdminFuture<'a, Value>;
    fn check<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        estimated_tokens: i64,
    ) -> QuotaAdminFuture<'a, Value>;
    fn forecast<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        lookback_days: i64,
    ) -> QuotaAdminFuture<'a, Value>;
    fn alerts<'a>(
        &'a self,
        tenant: &'a str,
        limit: i64,
        unacknowledged_only: bool,
    ) -> QuotaAdminFuture<'a, Vec<Value>>;
    fn block<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        reason: &'a str,
    ) -> QuotaAdminFuture<'a, bool>;
    fn unblock<'a>(&'a self, tenant: &'a str, user: &'a str) -> QuotaAdminFuture<'a, bool>;
}
pub trait QuotaAdminChecker: Send + Sync {
    fn check<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        estimated_tokens: i64,
    ) -> QuotaAdminFuture<'a, QuotaCheckResult>;
}
/// Required host boundary for asyncpg timestamptz's treatment of naive local time.
/// It must preserve the deployment's canonical codec behavior; there is no implicit
/// local timezone read and no guessed UTC conversion in this module.
pub trait QuotaTimestampBinder: Send + Sync {
    fn bind(&self, value: ProtocolTimestamp) -> QuotaAdminResult<DateTime<Utc>>;
}
impl QuotaAdminSet {
    /// Pure conversion into the existing quota-store contract. No persistence is
    /// performed. PgQuotaAdminService requires the supervised store so both
    /// explicit and implicit get/create writes share caller-owned cancellation.
    pub fn to_store_update(
        &self,
        timestamp: &dyn QuotaTimestampBinder,
    ) -> QuotaAdminResult<QuotaUpdate> {
        let validated = validate_set_quota(&self.fields)?;
        let v = &validated;
        Ok(QuotaUpdate {
            daily_token_limit: v["daily_token_limit"].as_i64(),
            monthly_token_limit: v["monthly_token_limit"].as_i64(),
            monthly_cost_limit_cents: v["monthly_cost_limit_cents"].as_i64(),
            requests_per_minute: v["requests_per_minute"].as_i64(),
            requests_per_day: v["requests_per_day"].as_i64(),
            warning_threshold: v["warning_threshold"]
                .as_i64()
                .ok_or(QuotaAdminError::Internal)?,
            overage_strategy: v["overage_strategy"].as_str().map(str::to_owned),
            downgraded_model: v["downgraded_model"].as_str().map(str::to_owned),
            temporary_extra_tokens: v["temporary_extra_tokens"].as_i64(),
            temporary_extra_cost_cents: v["temporary_extra_cost_cents"].as_i64(),
            temporary_expires_at: self
                .temporary_expires_at
                .map(|v| timestamp.bind(v))
                .transpose()?,
        })
    }
}
pub trait QuotaAdminAudit: Send + Sync {
    fn record<'a>(
        &'a self,
        context: &'a QuotaAdminContext,
        auth: &'a QuotaAdminIdentity,
        change: ConfigChange<'a>,
    ) -> QuotaAdminFuture<'a, ()>;
}
impl QuotaAdminAudit for ConfigAuditWriter {
    fn record<'a>(
        &'a self,
        c: &'a QuotaAdminContext,
        a: &'a QuotaAdminIdentity,
        change: ConfigChange<'a>,
    ) -> QuotaAdminFuture<'a, ()> {
        Box::pin(async move {
            self.record_config_change(&c.audit, a, change)
                .await
                .map_err(|e| {
                    if e == ConfigAuditError::Cancelled {
                        QuotaAdminError::Cancelled
                    } else {
                        QuotaAdminError::Internal
                    }
                })
        })
    }
}

pub const SUMMARY_SQL: &str = "SELECT COUNT(*) as total_users, COUNT(*) FILTER (WHERE is_blocked = TRUE) as blocked_users, COUNT(*) FILTER ( WHERE daily_token_limit IS NOT NULL AND current_daily_tokens::float / NULLIF(daily_token_limit, 0) >= warning_threshold / 100.0 AND current_daily_tokens < daily_token_limit ) as warning_users, COUNT(*) FILTER ( WHERE (daily_token_limit IS NOT NULL AND current_daily_tokens >= daily_token_limit) OR (monthly_cost_limit_cents IS NOT NULL AND current_monthly_cost_cents >= monthly_cost_limit_cents) ) as exceeded_users FROM user_quotas WHERE tenant_id = $1";
pub const ALERT_COUNT_SQL: &str =
    "SELECT COUNT(*) as count FROM quota_alerts WHERE tenant_id = $1 AND is_acknowledged = FALSE";
pub const ACKNOWLEDGE_SQL: &str = "UPDATE quota_alerts SET is_acknowledged = TRUE, acknowledged_at = CURRENT_TIMESTAMP, acknowledged_by = $2 WHERE id = $1 AND tenant_id = $3";
pub const RESET_DAILY_SQL: &str = "UPDATE user_quotas SET current_daily_tokens = 0, current_daily_requests = 0, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = $1 AND user_id = $2";
pub const RESET_MONTHLY_SQL: &str = "UPDATE user_quotas SET current_monthly_tokens = 0, current_monthly_cost_cents = 0, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = $1 AND user_id = $2";
pub fn overview_sql(sort_by: &str) -> String {
    let col = match sort_by {
        "monthly_cost" => "current_monthly_cost_cents",
        "status" => "is_blocked DESC, current_daily_tokens",
        _ => "current_daily_tokens",
    };
    format!(
        "SELECT user_id, current_daily_tokens, daily_token_limit, current_monthly_tokens, monthly_token_limit, current_monthly_cost_cents, monthly_cost_limit_cents, is_blocked, blocked_reason, overage_strategy, downgraded_model, warning_threshold, temporary_extra_tokens, temporary_extra_cost_cents, temporary_expires_at FROM user_quotas WHERE tenant_id = $1 ORDER BY {col} DESC LIMIT $2"
    )
}
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaAdminStatement {
    pub sql: String,
    pub arguments: Vec<Value>,
}
impl QuotaAdminStatement {
    fn new(sql: impl Into<String>, arguments: Vec<Value>) -> Self {
        Self {
            sql: sql.into(),
            arguments,
        }
    }
}
pub trait QuotaAdminConnection: Send {
    fn fetchrow<'a>(
        &'a mut self,
        statement: QuotaAdminStatement,
    ) -> QuotaAdminFuture<'a, Option<Value>>;
    fn fetch<'a>(&'a mut self, statement: QuotaAdminStatement) -> QuotaAdminFuture<'a, Vec<Value>>;
    fn execute<'a>(&'a mut self, statement: QuotaAdminStatement) -> QuotaAdminFuture<'a, String>;
    /// Release on success/error; Drop must also release when an await is cancelled.
    fn close(self: Box<Self>) -> QuotaAdminFuture<'static, ()>;
}
pub trait QuotaAdminPool: Send + Sync {
    fn acquire(&self) -> QuotaAdminFuture<'_, Box<dyn QuotaAdminConnection>>;
}
/// Native source-service bridge. Construction is inert and explicitly rejects
/// legacy/unsupervised PgQuotaStore instances, including unavailable(). The host
/// retains the checker (policy/RPM/security effects) and process-timezone binder.
/// Direct and overview pools remain separate optional QuotaAdminApi arguments.
#[derive(Clone)]
pub struct PgQuotaAdminService {
    store: PgQuotaStore,
    checker: Arc<dyn QuotaAdminChecker>,
    timestamp: Arc<dyn QuotaTimestampBinder>,
}
impl PgQuotaAdminService {
    pub fn new(
        store: PgQuotaStore,
        checker: Arc<dyn QuotaAdminChecker>,
        timestamp: Arc<dyn QuotaTimestampBinder>,
    ) -> Result<Self, QuotaStoreError> {
        if !store.is_supervised() {
            return Err(QuotaStoreError::SupervisionRequired);
        }
        Ok(Self {
            store,
            checker,
            timestamp,
        })
    }
    /// Abort and join all caller tasks, then drain this service AND each native
    /// direct/overview pool before closing either data/control pool or runtime.
    pub async fn drain_cancelled(&self) -> Result<(), QuotaStoreError> {
        self.store.drain_cancelled().await
    }
}
fn native_store_error(error: QuotaStoreError) -> QuotaAdminError {
    match error {
        QuotaStoreError::Cancelled => QuotaAdminError::Cancelled,
        error => QuotaAdminError::Database {
            sqlstate: error.sqlstate().map(|value| value.into_owned()),
        },
    }
}
fn native_operation_error(error: PgOperationError) -> QuotaAdminError {
    match error {
        PgOperationError::Cancelled => QuotaAdminError::Cancelled,
        error => QuotaAdminError::Database {
            sqlstate: error.sqlstate().map(str::to_owned),
        },
    }
}
fn source_store_error(error: &QuotaStoreError) -> &'static str {
    // Never forward SQLx/backend diagnostic text, even if a future store change
    // accidentally supplies a legacy Database variant at this native boundary.
    match error {
        QuotaStoreError::DatabaseUnavailable => "Database not available",
        QuotaStoreError::QuotaNotFound => "Quota not found",
        QuotaStoreError::DateOutOfRange => "quota_date_out_of_range",
        QuotaStoreError::IntegerOverflow => "billing_integer_overflow",
        _ => "quota_database_unavailable",
    }
}
impl QuotaAdminService for PgQuotaAdminService {
    fn get<'a>(&'a self, tenant: &'a str, user: &'a str) -> QuotaAdminFuture<'a, Option<Value>> {
        Box::pin(async move {
            match self
                .store
                .try_get_or_create_quota_at(tenant, user, Utc::now())
                .await
            {
                Ok(quota) => Ok(quota.map(|quota| quota.to_value())),
                Err(QuotaStoreError::Cancelled) => Err(QuotaAdminError::Cancelled),
                Err(_) => Ok(None),
            }
        })
    }
    fn set<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        body: QuotaAdminSet,
    ) -> QuotaAdminFuture<'a, Value> {
        Box::pin(async move {
            let update = body.to_store_update(self.timestamp.as_ref())?;
            // Source ordering includes an internal, swallowed read after the
            // committed write; API audit and read reauthorization follow later.
            self.store
                .supervised_set_user_quota_at(tenant, user, &update, Utc::now())
                .await
                .map_err(native_store_error)
        })
    }
    fn check<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        estimated_tokens: i64,
    ) -> QuotaAdminFuture<'a, Value> {
        Box::pin(async move {
            Ok(self
                .checker
                .check(tenant, user, estimated_tokens)
                .await?
                .to_dict())
        })
    }
    fn forecast<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        lookback_days: i64,
    ) -> QuotaAdminFuture<'a, Value> {
        Box::pin(async move {
            match self
                .store
                .try_get_quota_forecast_at(tenant, user, lookback_days, Utc::now())
                .await
            {
                Ok(value) => Ok(value),
                Err(QuotaStoreError::Cancelled) => Err(QuotaAdminError::Cancelled),
                Err(error) => Ok(json!({"error":source_store_error(&error)})),
            }
        })
    }
    fn alerts<'a>(
        &'a self,
        tenant: &'a str,
        limit: i64,
        unacknowledged_only: bool,
    ) -> QuotaAdminFuture<'a, Vec<Value>> {
        Box::pin(async move {
            match self
                .store
                .try_get_quota_alerts(tenant, limit, unacknowledged_only)
                .await
            {
                Ok(alerts) => Ok(alerts),
                Err(QuotaStoreError::Cancelled) => Err(QuotaAdminError::Cancelled),
                Err(_) => Ok(vec![]),
            }
        })
    }
    fn block<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        reason: &'a str,
    ) -> QuotaAdminFuture<'a, bool> {
        Box::pin(async move {
            match self.store.try_block_user(tenant, user, reason).await {
                Ok(_) => Ok(true), // UPDATE 0 is successful in the source.
                Err(QuotaStoreError::Cancelled) => Err(QuotaAdminError::Cancelled),
                Err(_) => Ok(false),
            }
        })
    }
    fn unblock<'a>(&'a self, tenant: &'a str, user: &'a str) -> QuotaAdminFuture<'a, bool> {
        Box::pin(async move {
            match self.store.try_unblock_user(tenant, user).await {
                Ok(_) => Ok(true),
                Err(QuotaStoreError::Cancelled) => Err(QuotaAdminError::Cancelled),
                Err(_) => Ok(false),
            }
        })
    }
}

/// Exact-SQL native direct/overview adapter, using the same caller-owned
/// supervision as quota storage. Supply separate independent control and data
/// pools on the same server/database/role, with direct or session-pinned access.
/// Construction starts no connection/task. Each statement commits independently;
/// a COMMIT already dispatched can succeed despite caller cancellation.
#[derive(Clone)]
pub struct PgQuotaAdminPool {
    operations: PgOperationStorage,
}
impl PgQuotaAdminPool {
    pub fn new(pool: sqlx::PgPool, control_pool: sqlx::PgPool) -> Result<Self, QuotaStoreError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, control_pool)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), QuotaStoreError> {
        self.operations.drain_cancelled().await.map_err(Into::into)
    }
}
struct PgQuotaAdminConnection {
    connection: PgOperationConnection,
}
impl QuotaAdminPool for PgQuotaAdminPool {
    fn acquire(&self) -> QuotaAdminFuture<'_, Box<dyn QuotaAdminConnection>> {
        Box::pin(async move {
            let connection = self
                .operations
                .acquire()
                .await
                .map_err(native_operation_error)?;
            Ok(Box::new(PgQuotaAdminConnection { connection }) as Box<dyn QuotaAdminConnection>)
        })
    }
}
#[derive(sqlx::FromRow, serde::Serialize)]
struct PgQuotaSummaryRow {
    total_users: i64,
    blocked_users: i64,
    warning_users: i64,
    exceeded_users: i64,
}
#[derive(sqlx::FromRow, serde::Serialize)]
struct PgQuotaOverviewRow {
    user_id: String,
    current_daily_tokens: Option<i64>,
    daily_token_limit: Option<i64>,
    current_monthly_tokens: Option<i64>,
    monthly_token_limit: Option<i64>,
    current_monthly_cost_cents: Option<i64>,
    monthly_cost_limit_cents: Option<i64>,
    is_blocked: Option<bool>,
    blocked_reason: Option<String>,
    overage_strategy: Option<String>,
    downgraded_model: Option<String>,
    warning_threshold: Option<i32>,
    temporary_extra_tokens: Option<i64>,
    temporary_extra_cost_cents: Option<i64>,
    temporary_expires_at: Option<DateTime<Utc>>,
}
fn native_row_value(row: impl serde::Serialize) -> QuotaAdminResult<Value> {
    serde_json::to_value(row).map_err(|_| QuotaAdminError::Internal)
}
fn native_text(arguments: &[Value], index: usize) -> QuotaAdminResult<String> {
    arguments
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(QuotaAdminError::Internal)
}
/// PostgreSQL UUID binding without changing Cargo feature unification or the
/// canonical `id = $1` SQL. Uses SQLx's public Type/Encode contract only.
struct PgQuotaAlertId(uuid::Uuid);
impl sqlx::Type<sqlx::Postgres> for PgQuotaAlertId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("uuid")
    }
}
impl<'q> sqlx::Encode<'q, sqlx::Postgres> for PgQuotaAlertId {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buffer.extend_from_slice(self.0.as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
    fn size_hint(&self) -> usize {
        16
    }
}
impl QuotaAdminConnection for PgQuotaAdminConnection {
    fn fetchrow<'a>(
        &'a mut self,
        statement: QuotaAdminStatement,
    ) -> QuotaAdminFuture<'a, Option<Value>> {
        Box::pin(async move {
            if statement.arguments.len() != 1 {
                return Err(QuotaAdminError::Internal);
            }
            let tenant = native_text(&statement.arguments, 0)?;
            match statement.sql.as_str() {
                SUMMARY_SQL => self
                    .connection
                    .execute(move |connection| {
                        Box::pin(async move {
                            sqlx::query_as::<_, PgQuotaSummaryRow>(SUMMARY_SQL)
                                .bind(tenant)
                                .fetch_optional(connection)
                                .await
                        })
                    })
                    .await
                    .map_err(native_operation_error)?
                    .map(native_row_value)
                    .transpose(),
                ALERT_COUNT_SQL => Ok(self
                    .connection
                    .execute(move |connection| {
                        Box::pin(async move {
                            sqlx::query_scalar::<_, i64>(ALERT_COUNT_SQL)
                                .bind(tenant)
                                .fetch_optional(connection)
                                .await
                        })
                    })
                    .await
                    .map_err(native_operation_error)?
                    .map(|count| json!({"count":count}))),
                _ => Err(QuotaAdminError::Internal),
            }
        })
    }
    fn fetch<'a>(&'a mut self, statement: QuotaAdminStatement) -> QuotaAdminFuture<'a, Vec<Value>> {
        Box::pin(async move {
            if statement.arguments.len() != 2
                || !["daily_tokens", "monthly_cost", "status"]
                    .iter()
                    .any(|sort| statement.sql == overview_sql(sort))
            {
                return Err(QuotaAdminError::Internal);
            }
            let tenant = native_text(&statement.arguments, 0)?;
            let limit = statement.arguments[1]
                .as_i64()
                .ok_or(QuotaAdminError::Internal)?;
            let rows = self
                .connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query_as::<_, PgQuotaOverviewRow>(&statement.sql)
                            .bind(tenant)
                            .bind(limit)
                            .fetch_all(connection)
                            .await
                    })
                })
                .await
                .map_err(native_operation_error)?;
            rows.into_iter().map(native_row_value).collect()
        })
    }
    fn execute<'a>(&'a mut self, statement: QuotaAdminStatement) -> QuotaAdminFuture<'a, String> {
        Box::pin(async move {
            let affected = match statement.sql.as_str() {
                ACKNOWLEDGE_SQL if statement.arguments.len() == 3 => {
                    let id = uuid::Uuid::parse_str(&native_text(&statement.arguments, 0)?)
                        .map(PgQuotaAlertId)
                        .map_err(|_| QuotaAdminError::Database { sqlstate: None })?;
                    let user = native_text(&statement.arguments, 1)?;
                    let tenant = native_text(&statement.arguments, 2)?;
                    self.connection
                        .execute(move |connection| {
                            Box::pin(async move {
                                sqlx::query(ACKNOWLEDGE_SQL)
                                    .bind(id)
                                    .bind(user)
                                    .bind(tenant)
                                    .execute(connection)
                                    .await
                                    .map(|result| result.rows_affected())
                            })
                        })
                        .await
                        .map_err(native_operation_error)?
                }
                RESET_DAILY_SQL | RESET_MONTHLY_SQL if statement.arguments.len() == 2 => {
                    let tenant = native_text(&statement.arguments, 0)?;
                    let user = native_text(&statement.arguments, 1)?;
                    self.connection
                        .execute(move |connection| {
                            Box::pin(async move {
                                sqlx::query(&statement.sql)
                                    .bind(tenant)
                                    .bind(user)
                                    .execute(connection)
                                    .await
                                    .map(|result| result.rows_affected())
                            })
                        })
                        .await
                        .map_err(native_operation_error)?
                }
                _ => return Err(QuotaAdminError::Internal),
            };
            Ok(format!("UPDATE {affected}"))
        })
    }
    fn close(self: Box<Self>) -> QuotaAdminFuture<'static, ()> {
        Box::pin(async move {
            // Completed statements already drained their transaction. A dropped
            // in-flight caller leaves only the owned supervisor until host drain.
            drop(self);
            Ok(())
        })
    }
}


fn integer(value: &Value) -> Result<i64, &'static str> {
    match value {
        Value::Bool(v) => Ok(i64::from(*v)),
        Value::Number(n) => {
            if let Some(v) = n.as_i64() {
                return Ok(v);
            }
            if n.is_u64() {
                return Err("int_parsing_size");
            }
            let v = n.as_f64().ok_or("int_type")?;
            if v.fract() != 0.0 {
                return Err("int_from_float");
            }
            if v < i64::MIN as f64 || v >= 9223372036854775808.0 {
                return Err("int_parsing_size");
            }
            Ok(v as i64)
        }
        Value::String(s) => {
            let s = s.trim();
            let clean = s.replace('_', "");
            // Underscores are permitted only between digits, as in pydantic-core.
            if s.as_bytes().iter().enumerate().any(|(i, b)| {
                *b == b'_'
                    && (i == 0
                        || i + 1 == s.len()
                        || !s.as_bytes()[i - 1].is_ascii_digit()
                        || !s.as_bytes()[i + 1].is_ascii_digit())
            }) {
                return Err("int_parsing");
            }
            let (whole, fraction) = clean
                .split_once('.')
                .map_or((clean.as_str(), None), |(a, b)| (a, Some(b)));
            if fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|c| c == b'0')) {
                return Err("int_parsing");
            }
            whole.parse::<i64>().map_err(|e| {
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
fn bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(v) => Some(*v),
        Value::Number(n) => match n.as_f64()? {
            0.0 => Some(false),
            1.0 => Some(true),
            _ => None,
        },
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "1" | "true" | "t" | "yes" | "y" | "on" => Some(true),
            "0" | "false" | "f" | "no" | "n" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
fn validation(kind: &str, loc: Value, input: &Value, ctx: Option<Value>) -> Value {
    let message = match kind {
        "missing" => "Field required".into(),
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from".into()
        }
        "int_type" => "Input should be a valid integer".into(),
        "int_parsing" => {
            "Input should be a valid integer, unable to parse string as an integer".into()
        }
        "int_from_float" => {
            "Input should be a valid integer, got a number with a fractional part".into()
        }
        "int_parsing_size" => "Integer exceeds supported signed 64-bit range".into(),
        "string_type" => "Input should be a valid string".into(),
        "bool_parsing" => "Input should be a valid boolean, unable to interpret input".into(),
        "greater_than_equal" => format!(
            "Input should be greater than or equal to {}",
            ctx.as_ref().unwrap()["ge"]
        ),
        "less_than_equal" => format!(
            "Input should be less than or equal to {}",
            ctx.as_ref().unwrap()["le"]
        ),
        "string_too_short" => "String should have at least 1 character".into(),
        "string_too_long" => format!(
            "String should have at most {} characters",
            ctx.as_ref().unwrap()["max_length"]
        ),
        "literal_error" => {
            "Input should be 'hard_block', 'rate_limit', 'downgrade_model' or 'allow_but_alert'"
                .into()
        }
        _ => "Invalid value".into(),
    };
    let mut error = json!({"type":kind,"loc":loc,"msg":message,"input":input});
    if let Some(ctx) = ctx {
        error["ctx"] = ctx;
    }
    error
}
fn invalid(errors: Vec<Value>) -> QuotaAdminError {
    QuotaAdminError::http(422, Value::Array(errors))
}
const SET_INT_FIELDS: [&str; 8] = [
    "daily_token_limit",
    "monthly_token_limit",
    "monthly_cost_limit_cents",
    "requests_per_minute",
    "requests_per_day",
    "warning_threshold",
    "temporary_extra_tokens",
    "temporary_extra_cost_cents",
];
const SET_FIELDS: [&str; 11] = [
    "daily_token_limit",
    "monthly_token_limit",
    "monthly_cost_limit_cents",
    "requests_per_minute",
    "requests_per_day",
    "warning_threshold",
    "overage_strategy",
    "downgraded_model",
    "temporary_extra_tokens",
    "temporary_extra_cost_cents",
    "temporary_expires_at",
];
fn strategy(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("hard_block" | "rate_limit" | "downgrade_model" | "allow_but_alert")
    )
}
pub fn validate_set_quota(body: &Value) -> QuotaAdminResult<Value> {
    let Some(object) = body.as_object() else {
        return Err(invalid(vec![validation(
            "model_attributes_type",
            json!(["body"]),
            body,
            None,
        )]));
    };
    let mut output = Map::new();
    let mut errors = vec![];
    for name in SET_FIELDS {
        let default = match name {
            "warning_threshold" => json!(80),
            "overage_strategy" => json!("allow_but_alert"),
            _ => Value::Null,
        };
        let value = object.get(name).unwrap_or(&default);
        let loc = json!(["body", name]);
        if value.is_null() && !matches!(name, "warning_threshold" | "overage_strategy") {
            output.insert(name.into(), Value::Null);
            continue;
        }
        if SET_INT_FIELDS.contains(&name) {
            match integer(value) {
                Err(kind) => errors.push(validation(kind, loc, value, None)),
                Ok(v) => {
                    if v < 0 {
                        errors.push(validation(
                            "greater_than_equal",
                            loc,
                            value,
                            Some(json!({"ge":0})),
                        ));
                    } else if name == "warning_threshold" && v > 100 {
                        errors.push(validation(
                            "less_than_equal",
                            loc,
                            value,
                            Some(json!({"le":100})),
                        ));
                    } else {
                        output.insert(name.into(), json!(v));
                    }
                }
            }
        } else if name == "overage_strategy" {
            if strategy(value) {
                output.insert(name.into(), value.clone());
            } else {
                errors.push(validation("literal_error",loc,value,Some(json!({"expected":"'hard_block', 'rate_limit', 'downgrade_model' or 'allow_but_alert'"}))));
            }
        } else if let Some(s) = value.as_str() {
            if name == "downgraded_model" && s.chars().count() > 128 {
                errors.push(validation(
                    "string_too_long",
                    loc,
                    value,
                    Some(json!({"max_length":128})),
                ));
            } else {
                output.insert(name.into(), value.clone());
            }
        } else {
            errors.push(validation("string_type", loc, value, None));
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(output))
    } else {
        Err(invalid(errors))
    }
}
pub fn validate_block_user(body: &Value) -> QuotaAdminResult<Value> {
    let Some(object) = body.as_object() else {
        return Err(invalid(vec![validation(
            "model_attributes_type",
            json!(["body"]),
            body,
            None,
        )]));
    };
    let loc = json!(["body", "reason"]);
    let Some(reason) = object.get("reason") else {
        return Err(invalid(vec![validation("missing", loc, body, None)]));
    };
    let error = if let Some(s) = reason.as_str() {
        if s.is_empty() {
            Some(validation(
                "string_too_short",
                loc,
                reason,
                Some(json!({"min_length":1})),
            ))
        } else if s.chars().count() > 256 {
            Some(validation(
                "string_too_long",
                loc,
                reason,
                Some(json!({"max_length":256})),
            ))
        } else {
            None
        }
    } else {
        Some(validation("string_type", loc, reason, None))
    };
    if let Some(error) = error {
        Err(invalid(vec![error]))
    } else {
        Ok(json!({"reason":reason}))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaAdminOperation {
    Summary,
    Overview,
    Alerts,
    Acknowledge,
    Get,
    Set,
    Check,
    Forecast,
    Reset,
    Block,
    Unblock,
}
impl QuotaAdminOperation {
    fn write(self) -> bool {
        matches!(
            self,
            Self::Acknowledge | Self::Set | Self::Reset | Self::Block | Self::Unblock
        )
    }
    fn subject(self) -> bool {
        !matches!(
            self,
            Self::Summary | Self::Overview | Self::Alerts | Self::Acknowledge
        )
    }
}
#[derive(Clone)]
pub struct QuotaAdminParameters {
    pub limit: i64,
    pub sort_by: String,
    pub unacknowledged_only: bool,
    pub estimated_tokens: i64,
    pub lookback_days: i64,
    pub reset_type: String,
}
pub fn validate_quota_query(
    op: QuotaAdminOperation,
    query: &HashMap<String, String>,
) -> QuotaAdminResult<QuotaAdminParameters> {
    let mut output = QuotaAdminParameters {
        limit: 50,
        sort_by: query
            .get("sort_by")
            .cloned()
            .unwrap_or_else(|| "daily_tokens".into()),
        unacknowledged_only: true,
        estimated_tokens: 0,
        lookback_days: 7,
        reset_type: query
            .get("reset_type")
            .cloned()
            .unwrap_or_else(|| "daily".into()),
    };
    let mut errors = vec![];
    for (name, lower, upper) in match op {
        QuotaAdminOperation::Overview | QuotaAdminOperation::Alerts => {
            vec![("limit", 1, Some(200))]
        }
        QuotaAdminOperation::Check => vec![("estimated_tokens", 0, None)],
        QuotaAdminOperation::Forecast => vec![("lookback_days", 1, Some(30))],
        _ => vec![],
    } {
        if let Some(value) = query.get(name) {
            let input = json!(value);
            let loc = json!(["query", name]);
            match integer(&input) {
                Err(kind) => errors.push(validation(kind, loc, &input, None)),
                Ok(v) => {
                    if v < lower {
                        errors.push(validation(
                            "greater_than_equal",
                            loc,
                            &input,
                            Some(json!({"ge":lower})),
                        ));
                    } else if upper.is_some_and(|max| v > max) {
                        errors.push(validation(
                            "less_than_equal",
                            loc,
                            &input,
                            Some(json!({"le":upper})),
                        ));
                    } else {
                        match name {
                            "limit" => output.limit = v,
                            "lookback_days" => output.lookback_days = v,
                            _ => output.estimated_tokens = v,
                        }
                    }
                }
            }
        }
    }
    if op == QuotaAdminOperation::Alerts
        && let Some(raw) = query.get("unacknowledged_only")
    {
        match bool_value(&json!(raw)) {
            Some(v) => output.unacknowledged_only = v,
            None => errors.insert(
                0,
                validation(
                    "bool_parsing",
                    json!(["query", "unacknowledged_only"]),
                    &json!(raw),
                    None,
                ),
            ),
        }
    }
    if errors.is_empty() {
        Ok(output)
    } else {
        Err(invalid(errors))
    }
}
// Response projection. '?' marks nullable, with None indicating a required field.
fn model_error(name: &str, kind: &str) -> Value {
    let message = match kind {
        "float_type" => "Input should be a valid number".into(),
        "float_parsing" => {
            "Input should be a valid number, unable to parse string as a number".into()
        }
        "bool_type" => "Input should be a valid boolean".into(),
        "dict_type" => "Input should be a valid dictionary".into(),
        _ => validation(kind, json!([]), &Value::Null, None)["msg"]
            .as_str()
            .unwrap_or("Invalid value")
            .to_owned(),
    };
    json!({"field":name,"message":message,"type":kind})
}
fn project(input: &Value, fields: &[(&str, &str, Option<Value>)]) -> QuotaAdminResult<Value> {
    let object = input.as_object().ok_or(QuotaAdminError::Internal)?;
    let mut output = Map::new();
    let mut errors = vec![];
    for (name, kind, default) in fields {
        let Some(value) = object.get(*name).or(default.as_ref()) else {
            errors.push(model_error(name, "missing"));
            continue;
        };
        if value.is_null() && kind.ends_with('?') {
            output.insert((*name).into(), Value::Null);
            continue;
        }
        let result: Result<Value, &str> = match kind.trim_end_matches('?') {
            "i" => integer(value).map(|v| json!(v)),
            "s" => value.as_str().map(|v| json!(v)).ok_or("string_type"),
            "b" => bool_value(value).map(|v| json!(v)).ok_or(
                if value.is_string() || value.is_number() {
                    "bool_parsing"
                } else {
                    "bool_type"
                },
            ),
            "f" => {
                let number = match value {
                    Value::Bool(v) => Ok(f64::from(u8::from(*v))),
                    Value::Number(n) => n.as_f64().ok_or("float_type"),
                    Value::String(s) => s.trim().parse::<f64>().map_err(|_| "float_parsing"),
                    _ => Err("float_type"),
                };
                match number {
                    Ok(n) if !n.is_finite() => return Err(QuotaAdminError::Internal),
                    other => other.map(|n| json!(n)),
                }
            }
            "d" => {
                if value.is_object() {
                    Ok(value.clone())
                } else {
                    Err("dict_type")
                }
            }
            "strategy" => {
                if strategy(value) {
                    Ok(value.clone())
                } else {
                    Err("literal_error")
                }
            }
            _ => return Err(QuotaAdminError::Internal),
        };
        match result {
            Ok(v) => {
                output.insert((*name).into(), v);
            }
            Err(kind) => errors.push(model_error(name, kind)),
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(output))
    } else {
        Err(QuotaAdminError::ModelValidation(errors))
    }
}
fn null() -> Option<Value> {
    Some(Value::Null)
}
fn quota_response(value: Option<Value>, tenant: &str, user: &str) -> QuotaAdminResult<Value> {
    let value = value.filter(python_truthy).unwrap_or_else(|| json!({}));
    let empty = json!({});
    let mut out = json!({});
    for (name, fields) in [
        (
            "limits",
            vec![
                ("daily_tokens", "i?", null()),
                ("monthly_tokens", "i?", null()),
                ("monthly_cost_cents", "i?", null()),
                ("requests_per_minute", "i?", null()),
                ("requests_per_day", "i?", null()),
            ],
        ),
        (
            "current_usage",
            vec![
                ("daily_tokens", "i", Some(json!(0))),
                ("monthly_tokens", "i", Some(json!(0))),
                ("monthly_cost_cents", "i", Some(json!(0))),
                ("daily_requests", "i", Some(json!(0))),
            ],
        ),
        (
            "status",
            vec![
                ("is_blocked", "b", Some(json!(false))),
                ("blocked_reason", "s?", null()),
            ],
        ),
        (
            "policy",
            vec![
                (
                    "overage_strategy",
                    "strategy",
                    Some(json!("allow_but_alert")),
                ),
                ("downgraded_model", "s?", null()),
            ],
        ),
        (
            "temporary_boost",
            vec![
                ("extra_tokens", "i", Some(json!(0))),
                ("extra_cost_cents", "i", Some(json!(0))),
                ("expires_at", "s?", null()),
            ],
        ),
        (
            "resets",
            vec![
                ("daily_reset_at", "s?", null()),
                ("monthly_reset_at", "s?", null()),
            ],
        ),
    ] {
        out[name] = project(value.get(name).unwrap_or(&empty), &fields)?;
    }
    let parent = project(
        &value,
        &[
            ("tenant_id", "s", Some(json!(tenant))),
            ("user_id", "s", Some(json!(user))),
            ("warning_threshold", "i", Some(json!(80))),
        ],
    )?;
    out.as_object_mut()
        .ok_or(QuotaAdminError::Internal)?
        .extend(parent.as_object().ok_or(QuotaAdminError::Internal)?.clone());
    Ok(out)
}
fn forecast_response(value: &Value) -> QuotaAdminResult<Value> {
    let mut errors = vec![];
    let mut out = match project(value, &[("tenant_id", "s", None), ("user_id", "s", None)]) {
        Ok(v) => v,
        Err(QuotaAdminError::ModelValidation(e)) => {
            errors.extend(e);
            json!({})
        }
        Err(e) => return Err(e),
    };
    for (name, model, fields) in [
        (
            "window",
            "QuotaForecastWindow",
            vec![
                ("month_start", "s", None),
                ("today", "s", None),
                ("days_elapsed", "i", None),
                ("days_remaining", "i", None),
                ("lookback_days", "i", None),
            ],
        ),
        (
            "tokens",
            "QuotaForecastTokens",
            vec![
                ("current", "i", None),
                ("avg_daily", "f", None),
                ("projected_month_end", "i", None),
                ("limit", "i?", null()),
                ("projected_usage_pct", "f?", null()),
                ("predicted_breach_date", "s?", null()),
            ],
        ),
        (
            "cost",
            "QuotaForecastCost",
            vec![
                ("current_cents", "f", None),
                ("current_usd", "f", None),
                ("avg_daily_cents", "f", None),
                ("projected_month_end_cents", "i", None),
                ("projected_month_end_usd", "f", None),
                ("limit_cents", "i?", null()),
                ("limit_usd", "f?", null()),
                ("projected_usage_pct", "f?", null()),
                ("predicted_breach_date", "s?", null()),
            ],
        ),
    ] {
        let Some(nested) = value.get(name) else {
            errors.push(model_error(name, "missing"));
            continue;
        };
        if !nested.is_object() {
            errors.push(json!({"field":name,"type":"model_type","message":format!("Input should be a valid dictionary or instance of {model}")}));
            continue;
        }
        match project(nested, &fields) {
            Ok(v) => out[name] = v,
            Err(QuotaAdminError::ModelValidation(mut nested)) => {
                for error in &mut nested {
                    error["field"] =
                        json!(format!("{name}.{}", error["field"].as_str().unwrap_or("")));
                }
                errors.extend(nested);
            }
            Err(e) => return Err(e),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(QuotaAdminError::ModelValidation(errors))
    }
}
fn alert_response(value: &Value) -> QuotaAdminResult<Value> {
    project(
        value,
        &[
            ("id", "s", None),
            ("user_id", "s", None),
            ("alert_type", "s", None),
            ("threshold_value", "i?", None),
            ("current_value", "i?", None),
            ("limit_value", "i?", None),
            ("message", "s?", None),
            ("is_acknowledged", "b", None),
            ("created_at", "s?", None),
        ],
    )
}
fn required<'a>(row: &'a Value, name: &str) -> QuotaAdminResult<&'a Value> {
    row.get(name).ok_or(QuotaAdminError::Internal)
}
fn row_int(row: &Value, name: &str, default: i64) -> QuotaAdminResult<i64> {
    let value = required(row, name)?;
    if !python_truthy(value) {
        return Ok(default);
    }
    if let Value::Number(n) = value
        && n.is_f64()
    {
        let n = n.as_f64().ok_or(QuotaAdminError::Internal)?.trunc();
        return integer(&json!(n)).map_err(|_| QuotaAdminError::Internal);
    }
    integer(value).map_err(|_| QuotaAdminError::Internal)
}
// PgQuotaOverviewRow serializes timestamptz as RFC3339. Normalize that
// explicit-offset representation to UTC; do not guess a process timezone for
// naive strings or introduce a numeric epoch contract for injected adapters.
fn overview_expiry(value: &Value) -> QuotaAdminResult<Option<DateTime<Utc>>> {
    if value.is_null() {
        return Ok(None);
    }
    let raw = value.as_str().ok_or(QuotaAdminError::Internal)?;
    DateTime::parse_from_rfc3339(raw)
        .map(|value| Some(value.with_timezone(&Utc)))
        .map_err(|_| QuotaAdminError::Internal)
}

fn overview_item(row: &Value, now: DateTime<Utc>) -> QuotaAdminResult<(Value, &'static str)> {
    let daily = row_int(row, "current_daily_tokens", 0)?;
    let cost = row_int(row, "current_monthly_cost_cents", 0)?;
    let tokens = row_int(row, "current_monthly_tokens", 0)?;
    let warning = row_int(row, "warning_threshold", 80)?;
    let optional = |name| -> QuotaAdminResult<Option<i64>> {
        let value = required(row, name)?;
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(integer(value).map_err(|_| QuotaAdminError::Internal)?))
        }
    };
    let dl = optional("daily_token_limit")?;
    let cl = optional("monthly_cost_limit_cents")?;
    let tl = optional("monthly_token_limit")?;
    let extra_tokens = row_int(row, "temporary_extra_tokens", 0)?;
    let extra_cost_cents = row_int(row, "temporary_extra_cost_cents", 0)?;
    let expires_at = overview_expiry(required(row, "temporary_expires_at")?)?;
    let limits =
        effective_quota_limits(dl, tl, cl, extra_tokens, extra_cost_cents, expires_at, now)
            .map_err(|_| QuotaAdminError::Internal)?;
    let source = |limit: Option<i64>, extra| {
        if limit.is_none_or(|limit| limit == 0) {
            "unlimited"
        } else if limits.temporary_boost_active && extra != 0 {
            "configured_plus_temporary_boost"
        } else {
            "configured"
        }
    };
    let observation = json!({
        "observed_at": now.to_rfc3339(),
        "effective": {
            "daily_tokens": limits.daily_tokens,
            "monthly_tokens": limits.monthly_tokens,
            "monthly_cost_cents": limits.monthly_cost_cents,
        },
        "sources": {
            "daily_tokens": source(limits.daily_tokens, extra_tokens),
            "monthly_tokens": source(limits.monthly_tokens, extra_tokens),
            "monthly_cost_cents": source(limits.monthly_cost_cents, extra_cost_cents),
        },
        "temporary_boost": {
            "extra_tokens": extra_tokens,
            "extra_cost_cents": extra_cost_cents,
            "expires_at": expires_at.map(|value| value.to_rfc3339()),
            "active": limits.temporary_boost_active,
        },
    });
    let blocked = python_truthy(required(row, "is_blocked")?);
    let exceeds = |used, limit: Option<i64>| limit.is_some_and(|l| l != 0 && used >= l);
    let warns = |used, limit: Option<i64>| {
        limit.is_some_and(|l| l > 0 && (integer_ratio(used, l) * 100.0) >= warning as f64)
    };
    let status = if blocked {
        "blocked"
    } else if exceeds(daily, limits.daily_tokens)
        || exceeds(cost, limits.monthly_cost_cents)
        || exceeds(tokens, limits.monthly_tokens)
    {
        "exceeded"
    } else if warns(daily, limits.daily_tokens) || warns(cost, limits.monthly_cost_cents) {
        "warning"
    } else {
        "ok"
    };
    // Remaining row lookups and Pydantic validation occur AFTER count increments.
    Ok((
        json!({"daily_tokens_used":daily,"daily_tokens_limit":dl,"monthly_cost_used_cents":cost,"monthly_cost_limit_cents":cl,"monthly_tokens_used":tokens,"monthly_tokens_limit":tl,"is_blocked":blocked,"status":status,"limit_observation":observation}),
        status,
    ))
}
fn complete_overview_item(mut value: Value, row: &Value) -> QuotaAdminResult<Value> {
    for name in [
        "user_id",
        "blocked_reason",
        "overage_strategy",
        "downgraded_model",
    ] {
        value[name] = required(row, name)?.clone();
    }
    if !python_truthy(&value["overage_strategy"]) {
        value["overage_strategy"] = json!("allow_but_alert");
    }
    project(
        &value,
        &[
            ("user_id", "s", None),
            ("daily_tokens_used", "i", None),
            ("daily_tokens_limit", "i?", None),
            ("monthly_cost_used_cents", "i", None),
            ("monthly_cost_limit_cents", "i?", None),
            ("monthly_tokens_used", "i", None),
            ("monthly_tokens_limit", "i?", None),
            ("is_blocked", "b", None),
            ("blocked_reason", "s?", None),
            ("overage_strategy", "s", None),
            ("downgraded_model", "s?", None),
            ("status", "s", None),
            ("limit_observation", "d", None),
        ],
    )
}

#[derive(Clone)]
pub struct QuotaAdminApi {
    service: Arc<dyn QuotaAdminService>,
    direct_pool: Option<Arc<dyn QuotaAdminPool>>,
    overview_pool: Option<Arc<dyn QuotaAdminPool>>,
    authorization: Arc<dyn QuotaAdminAuthorization>,
    audit: Arc<dyn QuotaAdminAudit>,
}
impl QuotaAdminApi {
    pub fn new(
        service: Arc<dyn QuotaAdminService>,
        direct_pool: Option<Arc<dyn QuotaAdminPool>>,
        overview_pool: Option<Arc<dyn QuotaAdminPool>>,
        authorization: Arc<dyn QuotaAdminAuthorization>,
        audit: Arc<dyn QuotaAdminAudit>,
    ) -> Self {
        Self {
            service,
            direct_pool,
            overview_pool,
            authorization,
            audit,
        }
    }
    fn authorize(
        &self,
        context: &QuotaAdminContext,
        auth: &QuotaAdminIdentity,
        cap: Capability,
        target: Option<&str>,
    ) -> QuotaAdminResult<()> {
        self.authorization.require(context, auth, cap)?;
        scope(context, auth, target, cap)
    }
    async fn get(
        &self,
        context: &QuotaAdminContext,
        auth: &QuotaAdminIdentity,
        user: &str,
    ) -> QuotaAdminResult<Value> {
        self.authorize(context, auth, Capability::GatewayQuotaRead, Some(user))?;
        quota_response(
            self.service.get(&auth.tenant_id, user).await?,
            &auth.tenant_id,
            user,
        )
    }
    async fn audit(
        &self,
        context: &QuotaAdminContext,
        auth: &QuotaAdminIdentity,
        user: &str,
        action: &str,
        before: Option<Value>,
        after: Value,
    ) -> QuotaAdminResult<()> {
        let before = before.map(ConfigAuditValue::from);
        let after = ConfigAuditValue::from(after);
        self.audit
            .record(
                context,
                auth,
                ConfigChange {
                    resource_type: "quota",
                    resource_id: user,
                    action,
                    before: before.as_ref(),
                    after: Some(&after),
                },
            )
            .await
    }
    async fn summary(&self, tenant: &str) -> QuotaAdminResult<Value> {
        let mut out = json!({"total_users":0,"blocked_users":0,"warning_users":0,"exceeded_users":0,"unacknowledged_alerts":0});
        if let Some(pool) = &self.direct_pool {
            let mut c = pool.acquire().await?;
            let result = async {
                if let Some(row) = c
                    .fetchrow(QuotaAdminStatement::new(SUMMARY_SQL, vec![json!(tenant)]))
                    .await?
                    .filter(python_truthy)
                {
                    for key in [
                        "total_users",
                        "blocked_users",
                        "warning_users",
                        "exceeded_users",
                    ] {
                        let value = required(&row, key)?;
                        out[key] = if python_truthy(value) {
                            value.clone()
                        } else {
                            json!(0)
                        };
                    }
                }
                if let Some(row) = c
                    .fetchrow(QuotaAdminStatement::new(
                        ALERT_COUNT_SQL,
                        vec![json!(tenant)],
                    ))
                    .await?
                    .filter(python_truthy)
                {
                    let value = required(&row, "count")?;
                    out["unacknowledged_alerts"] = if python_truthy(value) {
                        value.clone()
                    } else {
                        json!(0)
                    };
                }
                Ok(())
            }
            .await;
            c.close().await?;
            result?;
        }
        Ok(out)
    }
    async fn overview(&self, tenant: &str, p: &QuotaAdminParameters) -> QuotaAdminResult<Value> {
        self.overview_at(tenant, p, Utc::now()).await
    }
    // Capture the clock once for the entire response, before waiting on the pool.
    async fn overview_at(
        &self,
        tenant: &str,
        p: &QuotaAdminParameters,
        observed_at: DateTime<Utc>,
    ) -> QuotaAdminResult<Value> {
        let mut users = vec![];
        let mut summary = json!({"total":0,"blocked":0,"exceeded":0,"warning":0,"ok":0});
        if let Some(pool) = &self.overview_pool {
            let result = async {
                let mut c = pool.acquire().await?;
                let result = async {
                    let rows = c
                        .fetch(QuotaAdminStatement::new(
                            overview_sql(&p.sort_by),
                            vec![json!(tenant), json!(p.limit)],
                        ))
                        .await?;
                    for row in rows {
                        let (item, status) = overview_item(&row, observed_at)?;
                        for key in [status, "total"] {
                            summary[key] = json!(
                                summary[key]
                                    .as_i64()
                                    .ok_or(QuotaAdminError::Internal)?
                                    .checked_add(1)
                                    .ok_or(QuotaAdminError::Internal)?
                            );
                        }
                        users.push(complete_overview_item(item, &row)?);
                    }
                    Ok(())
                }
                .await;
                c.close().await?;
                result
            }
            .await;
            if result == Err(QuotaAdminError::Cancelled) {
                return result.map(|()| Value::Null);
            }
        }
        Ok(json!({"users":users,"summary":summary}))
    }
    async fn direct_execute(
        &self,
        statement: QuotaAdminStatement,
    ) -> QuotaAdminResult<Option<String>> {
        let Some(pool) = &self.direct_pool else {
            return Ok(None);
        };
        let mut c = pool.acquire().await?;
        let result = c.execute(statement).await;
        c.close().await?;
        result.map(Some)
    }
    /// Inputs are already authenticated; all semantic validation still precedes
    /// handler capability checks, matching FastAPI dependency/validation ordering.
    pub async fn execute(
        &self,
        operation: QuotaAdminOperation,
        auth: &QuotaAdminIdentity,
        id: &str,
        body: &Value,
        query: &HashMap<String, String>,
        context: &QuotaAdminContext,
    ) -> QuotaAdminResult<Value> {
        let p = validate_quota_query(operation, query)?;
        let body = match operation {
            QuotaAdminOperation::Set => validate_set_quota(body)?,
            QuotaAdminOperation::Block => validate_block_user(body)?,
            _ => Value::Null,
        };
        let cap = if operation.write() {
            Capability::GatewayQuotaWrite
        } else {
            Capability::GatewayQuotaRead
        };
        self.authorize(context, auth, cap, operation.subject().then_some(id))?;
        let tenant = &auth.tenant_id;
        match operation {
            QuotaAdminOperation::Summary => self.summary(tenant).await,
            QuotaAdminOperation::Overview => self.overview(tenant, &p).await,
            QuotaAdminOperation::Alerts => {
                let alerts = self
                    .service
                    .alerts(tenant, p.limit, p.unacknowledged_only)
                    .await?;
                let count = alerts.len();
                let alerts = alerts
                    .iter()
                    .map(alert_response)
                    .collect::<QuotaAdminResult<Vec<_>>>()?;
                Ok(json!({"alerts":alerts,"total":count}))
            }
            QuotaAdminOperation::Acknowledge => {
                if self
                    .direct_execute(QuotaAdminStatement::new(
                        ACKNOWLEDGE_SQL,
                        vec![json!(id), json!(auth.user_id), json!(tenant)],
                    ))
                    .await?
                    .is_some_and(|result| result.contains("UPDATE 0"))
                {
                    return Err(QuotaAdminError::http(404, "Alert not found"));
                }
                Ok(json!({"success":true,"alert_id":id,"acknowledged":true}))
            }
            QuotaAdminOperation::Get => {
                quota_response(self.service.get(tenant, id).await?, tenant, id)
            }
            QuotaAdminOperation::Set => {
                let expiry = body["temporary_expires_at"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(parse_quota_expiry)
                    .transpose()?;
                let result = self
                    .service
                    .set(
                        tenant,
                        id,
                        QuotaAdminSet {
                            fields: body.clone(),
                            temporary_expires_at: expiry,
                        },
                    )
                    .await?;
                if let Some(error) = result.get("error") {
                    return Err(QuotaAdminError::http(500, error.clone()));
                }
                let after = Value::Object(
                    body.as_object()
                        .ok_or(QuotaAdminError::Internal)?
                        .iter()
                        .filter(|(_, v)| !v.is_null())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
                self.audit(context, auth, id, "set", None, after).await?;
                self.get(context, auth, id).await
            }
            QuotaAdminOperation::Check => project(
                &self.service.check(tenant, id, p.estimated_tokens).await?,
                &[
                    ("status", "s", None),
                    ("message", "s", None),
                    ("can_proceed", "b", None),
                    ("daily_tokens", "d", None),
                    ("monthly_cost", "d", None),
                    ("daily_requests", "d", None),
                    ("policy", "d", Some(json!({}))),
                ],
            ),
            QuotaAdminOperation::Forecast => {
                let value = self.service.forecast(tenant, id, p.lookback_days).await?;
                if let Some(error) = value.get("error") {
                    return Err(QuotaAdminError::http(500, error.clone()));
                }
                forecast_response(&value)
            }
            QuotaAdminOperation::Reset => {
                let sql = match p.reset_type.as_str() {
                    "daily" => RESET_DAILY_SQL,
                    "monthly" => RESET_MONTHLY_SQL,
                    _ => {
                        return Err(QuotaAdminError::http(
                            400,
                            "Invalid reset_type. Must be 'daily' or 'monthly'",
                        ));
                    }
                };
                self.direct_execute(QuotaAdminStatement::new(
                    sql,
                    vec![json!(tenant), json!(id)],
                ))
                .await?;
                self.audit(
                    context,
                    auth,
                    id,
                    &format!("reset_{}", p.reset_type),
                    Some(json!({"user_id":id,"reset_type":p.reset_type})),
                    json!({"reset_type":p.reset_type}),
                )
                .await?;
                self.get(context, auth, id).await
            }
            QuotaAdminOperation::Block => {
                let reason = body["reason"].as_str().ok_or(QuotaAdminError::Internal)?;
                if !self.service.block(tenant, id, reason).await? {
                    return Err(QuotaAdminError::http(500, "Failed to block user"));
                }
                self.audit(
                    context,
                    auth,
                    id,
                    "block",
                    Some(json!({"user_id":id})),
                    json!({"reason":reason,"is_blocked":true}),
                )
                .await?;
                Ok(json!({"success":true,"user_id":id,"is_blocked":true,"reason":reason}))
            }
            QuotaAdminOperation::Unblock => {
                if !self.service.unblock(tenant, id).await? {
                    return Err(QuotaAdminError::http(500, "Failed to unblock user"));
                }
                self.audit(
                    context,
                    auth,
                    id,
                    "unblock",
                    Some(json!({"user_id":id,"is_blocked":true})),
                    json!({"is_blocked":false}),
                )
                .await?;
                Ok(json!({"success":true,"user_id":id,"is_blocked":false}))
            }
        }
    }
}
pub trait QuotaAdminCancellation: Send + Sync {
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>>;
}
#[derive(Clone)]
struct HttpState {
    api: Arc<QuotaAdminApi>,
    identity: Arc<dyn QuotaAdminIdentityResolver>,
    cancellation: Arc<dyn QuotaAdminCancellation>,
}
/// Explicit factory only: construction is inert and the default host is untouched.
pub fn router(
    identity: Arc<dyn QuotaAdminIdentityResolver>,
    api: Arc<QuotaAdminApi>,
    cancellation: Arc<dyn QuotaAdminCancellation>,
) -> Router {
    Router::new()
        .route(
            "/api/v1/quota/summary",
            get(http_summary)
                .put(http_set_summary)
                .head(method_not_allowed_get)
                .fallback(method_not_allowed_get),
        )
        .route(
            "/api/v1/quota/users-overview",
            get(http_overview)
                .put(http_set_overview)
                .head(method_not_allowed_get)
                .fallback(method_not_allowed_get),
        )
        .route(
            "/api/v1/quota/alerts/list",
            get(http_alerts)
                .head(method_not_allowed_get)
                .fallback(method_not_allowed_get),
        )
        .route(
            "/api/v1/quota/alerts/{alert_id}/acknowledge",
            post(http_ack).fallback(method_not_allowed_post),
        )
        .route(
            "/api/v1/quota/{user_id}",
            get(http_get)
                .put(http_set)
                .head(method_not_allowed_get)
                .fallback(method_not_allowed_get),
        )
        .route(
            "/api/v1/quota/{user_id}/check",
            get(http_check)
                .head(method_not_allowed_get)
                .fallback(method_not_allowed_get),
        )
        .route(
            "/api/v1/quota/{user_id}/forecast",
            get(http_forecast)
                .head(method_not_allowed_get)
                .fallback(method_not_allowed_get),
        )
        .route(
            "/api/v1/quota/{user_id}/reset",
            post(http_reset).fallback(method_not_allowed_post),
        )
        .route(
            "/api/v1/quota/{user_id}/block",
            post(http_block).fallback(method_not_allowed_post),
        )
        .route(
            "/api/v1/quota/{user_id}/unblock",
            post(http_unblock).fallback(method_not_allowed_post),
        )
        .with_state(HttpState {
            api,
            identity,
            cancellation,
        })
}
// Starlette tries later routes after a path-only (wrong method) match, while
// Axum commits to a static path first. Preserve PUT for users named these static
// GET endpoints explicitly. HEAD is not registered by canonical FastAPI GET.
async fn http_set_summary(State(state): State<HttpState>, request: Request) -> Response {
    dispatch(state, QuotaAdminOperation::Set, "summary".into(), request).await
}
async fn http_set_overview(State(state): State<HttpState>, request: Request) -> Response {
    dispatch(
        state,
        QuotaAdminOperation::Set,
        "users-overview".into(),
        request,
    )
    .await
}
async fn method_not_allowed_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, "GET")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
        .into_response()
}
async fn method_not_allowed_post() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, "POST")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
        .into_response()
}
async fn dispatch(
    state: HttpState,
    operation: QuotaAdminOperation,
    id: String,
    request: Request,
) -> Response {
    // ASGI routes against its decoded path; a slash cannot be one {user_id}
    // or {alert_id} segment. Axum decodes captures after path matching, so reject
    // these decoded separators before authentication or any service side effect.
    if id.contains('/') {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let headers = request.headers().clone();
    let mut context = request
        .extensions()
        .get::<QuotaAdminContext>()
        .cloned()
        .unwrap_or_default();
    context.audit.headers = headers.clone();
    context.path = request.uri().path().into();
    let uri = request.uri().clone();
    let needs_body = matches!(
        operation,
        QuotaAdminOperation::Set | QuotaAdminOperation::Block
    );
    // Canonical explicit JSON syntax parsing precedes dependencies. Missing or
    // non-JSON Content-Type remains semantic bytes until after identity.
    let parsed_body = if needs_body {
        match axum::body::Bytes::from_request(request, &()).await {
            Ok(bytes) => {
                crate::model_admin_api::http_body(&headers, bytes).map_err(|error| match error {
                    crate::model_admin_api::ModelAdminError::Http { status, body } => {
                        QuotaAdminError::Http {
                            status,
                            detail: body.get("detail").cloned().unwrap_or(body),
                        }
                    }
                    _ => QuotaAdminError::Internal,
                })
            }
            Err(_) => Err(QuotaAdminError::http(422, "Invalid JSON request body")),
        }
    } else {
        Ok(Value::Null)
    };
    let result = async {
        let body = parsed_body?;
        let auth = state.identity.resolve(&headers, &context.path).await?;
        let query = Query::<HashMap<String, String>>::try_from_uri(&uri)
            .map_err(|_| QuotaAdminError::http(422, "Invalid query parameters"))?
            .0;
        if needs_body && body.is_null() {
            return Err(invalid(vec![validation(
                "missing",
                json!(["body"]),
                &Value::Null,
                None,
            )]));
        }
        state
            .api
            .execute(operation, &auth, &id, &body, &query, &context)
            .await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(QuotaAdminError::Cancelled) => state.cancellation.cancelled().await,
        Err(e) => (
            StatusCode::from_u16(e.status().expect("noncancelled status"))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(
                e.response_body_with_context(&context)
                    .expect("noncancelled body"),
            ),
        )
            .into_response(),
    }
}
macro_rules! handler {
    ($name:ident,$op:ident) => {
        async fn $name(State(state): State<HttpState>, request: Request) -> Response {
            dispatch(state, QuotaAdminOperation::$op, String::new(), request).await
        }
    };
    ($name:ident,$op:ident,path) => {
        async fn $name(
            State(state): State<HttpState>,
            Path(id): Path<String>,
            request: Request,
        ) -> Response {
            dispatch(state, QuotaAdminOperation::$op, id, request).await
        }
    };
}
handler!(http_summary, Summary);
handler!(http_overview, Overview);
handler!(http_alerts, Alerts);
handler!(http_ack, Acknowledge, path);
handler!(http_get, Get, path);
handler!(http_set, Set, path);
handler!(http_check, Check, path);
handler!(http_forecast, Forecast, path);
handler!(http_reset, Reset, path);
handler!(http_block, Block, path);
handler!(http_unblock, Unblock, path);
