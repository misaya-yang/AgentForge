//! Opt-in six-route port of `src/api/v1/metrics.py`.
//!
//! Constructors are inert and this module mounts nothing by default. The host
//! injects verified identity, its configured RBAC, recorder, Redis, database,
//! clock, and cancellation policy. Query/body/header tenant and role claims never
//! establish authority. The first four routes expose global Redis state and need
//! BOTH GatewayMetricsRead and an explicit platform_admin/superadmin/super_admin
//! subject; the two security routes use GatewayMetricsRead and the trusted tenant.
//!
//! Reuses MetricsRecorder, its exact numeric helpers, billing decimal conversion,
//! ProtocolTimestamp and compute_data_status. The independent realtime metrics
//! and redaction services are deliberately not copied into this endpoint layer.
//! Source ordering, partial results on ordinary Redis failure, negative Python
//! slicing, stable ties, unsupported metric/granularity behavior, and the summary
//! schema's non-null rate validation are retained. In particular, an empty native
//! recorder returns null rates and therefore fails the canonical summary schema.
//!
//! Native boundaries: finite JSON and signed i64 counters; date years 1..9999;
//! query timestamps support ISO date/datetime and Pydantic numeric epochs, with
//! microsecond precision. Extended Python date forms and exact parser diagnostic
//! wording are not reproduced. JSON response validation accepts the native typed
//! recorder/database forms, not every Pydantic coercion of arbitrary Python data.
//! The AdmissionService bridge supports current JWT/database-key authority. A
//! host needing canonical disabled-auth/guest behavior injects its trusted resolver.
//! The bare router returns 404 for trailing slashes; the source redirects 307.
//! Exact absolute redirects require the host canonical URL/redirect policy; this
//! module never infers a trusted origin from forwarding headers. Unsupported HTTP
//! methods (including HEAD) return 405/Allow GET before any auth or backend work.
//! Transport malformed-percent diagnostics remain Axum-owned. Dropped futures
//! stop remaining work; explicit cancellation is propagated, never swallowed as
//! a collection error or assigned an invented HTTP status. PostgreSQL cancellation
//! uses the established supervised adapter; the host must drain after cancellation.
//! All backend error text is opaque, and no credentials/payloads enter diagnostics.
use crate::{
    billing::microcents_to_usd,
    metrics_recorder::{
        MetricValue, MetricsRecorder, compute_data_status, integer, integer_ratio, round,
    },
    postgres_operation::{PgOperationError, PgOperationStorage},
    protocol::ProtocolTimestamp,
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{collections::HashMap, fmt, future::Future, pin::Pin, sync::Arc};

pub type MetricsAdminFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MetricsAdminBackendError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricsAdminBackendError {
    Unavailable,
    Cancelled,
}
impl fmt::Display for MetricsAdminBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "metrics_backend_unavailable",
            Self::Cancelled => "metrics_backend_cancelled",
        })
    }
}
impl std::error::Error for MetricsAdminBackendError {}
impl From<PgOperationError> for MetricsAdminBackendError {
    fn from(value: PgOperationError) -> Self {
        if value == PgOperationError::Cancelled {
            Self::Cancelled
        } else {
            Self::Unavailable
        }
    }
}
#[derive(Clone, PartialEq)]
pub enum MetricsAdminError {
    Http { status: u16, detail: Value },
    Internal,
    Cancelled,
}
impl fmt::Debug for MetricsAdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "MetricsAdminError::Http({status})"),
            Self::Internal => f.write_str("MetricsAdminError::Internal"),
            Self::Cancelled => f.write_str("MetricsAdminError::Cancelled"),
        }
    }
}
impl MetricsAdminError {
    pub fn http(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Internal => Some(500),
            Self::Cancelled => None,
        }
    }
    pub fn response_body(&self, trace: &str) -> Option<Value> {
        Some(match self {
            Self::Http { detail, .. } => json!({"detail":detail}),
            Self::Cancelled => return None,
            Self::Internal => {
                let mut result = json!({"error":{"code":2000,"message":"Internal server error"}});
                if !trace.is_empty() {
                    result["error"]["trace_id"] = trace.into();
                }
                result
            }
        })
    }
}
impl From<MetricsAdminBackendError> for MetricsAdminError {
    fn from(value: MetricsAdminBackendError) -> Self {
        match value {
            MetricsAdminBackendError::Cancelled => Self::Cancelled,
            MetricsAdminBackendError::Unavailable => Self::Internal,
        }
    }
}
type ApiResult<T> = Result<T, MetricsAdminError>;

/// Trusted resolver output. Intentionally not Deserialize; never extract from a
/// request body, query, forwarding header or unsigned token claims.
#[derive(Clone)]
pub struct MetricsAdminIdentity {
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    permissions: Vec<String>,
    is_authenticated: bool,
}
impl MetricsAdminIdentity {
    pub fn from_resolved_identity(
        user_id: String,
        tenant_id: String,
        roles: Vec<String>,
        permissions: Vec<String>,
        is_authenticated: bool,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            roles,
            permissions,
            is_authenticated,
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
    pub fn is_authenticated(&self) -> bool {
        self.is_authenticated
    }
    fn tenant(&self) -> &str {
        if self.tenant_id.is_empty() {
            "public"
        } else {
            &self.tenant_id
        }
    }
}
pub type MetricsAdminAuthFuture<'a> =
    Pin<Box<dyn Future<Output = ApiResult<MetricsAdminIdentity>> + Send + 'a>>;
pub trait MetricsAdminIdentityResolver: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> MetricsAdminAuthFuture<'a>;
}
impl MetricsAdminIdentityResolver for AdmissionService {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> MetricsAdminAuthFuture<'a> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let principal = request
                .resolve()
                .await
                .map_err(|e| MetricsAdminError::http(e.status(), e.to_string()))?;
            Ok(MetricsAdminIdentity::from_resolved_identity(
                principal.user_id().into(),
                principal.tenant_id().into(),
                principal.roles().to_vec(),
                principal.permissions().to_vec(),
                true,
            ))
        })
    }
}
#[derive(Clone, Default)]
pub struct MetricsAdminRequestContext {
    /// Set by trusted host middleware, never derived from query identity fields.
    pub request_id: String,
    pub trace_id: String,
}
impl MetricsAdminRequestContext {
    fn trace(&self) -> &str {
        if self.request_id.is_empty() {
            &self.trace_id
        } else {
            &self.request_id
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetricsPermissionDeniedEvent {
    pub tenant_id: String,
    pub user_id: Option<String>,
    pub service_id: Option<String>,
    pub event_type: String,
    pub metadata: Value,
}
pub trait MetricsPermissionDeniedSink: Send + Sync {
    /// Corresponds to the source best-effort scheduling seam. The host owns any
    /// asynchronous recorder lifecycle. No task is implicitly spawned here.
    fn schedule(&self, event: MetricsPermissionDeniedEvent);
}

pub trait MetricsSummarySource: Send + Sync {
    fn today_summary(&self) -> MetricsAdminFuture<'_, Value>;
}
impl MetricsSummarySource for MetricsRecorder {
    fn today_summary(&self) -> MetricsAdminFuture<'_, Value> {
        Box::pin(async move { Ok(self.get_today_summary().await) })
    }
}
pub trait MetricsAdminRedis: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    fn get<'a>(&'a self, key: &'a MetricValue) -> MetricsAdminFuture<'a, MetricValue>;
    fn scan<'a>(
        &'a self,
        cursor: u64,
        pattern: &'a str,
        count: usize,
    ) -> MetricsAdminFuture<'a, (u64, Vec<MetricValue>)>;
}
pub trait MetricsAdminClock: Send + Sync {
    fn local_now(&self) -> NaiveDateTime;
    fn utc_now(&self) -> DateTime<Utc>;
    fn today(&self) -> NaiveDate {
        self.local_now().date()
    }
}
pub struct SystemMetricsAdminClock;
impl MetricsAdminClock for SystemMetricsAdminClock {
    fn local_now(&self) -> NaiveDateTime {
        Local::now().naive_local()
    }
    fn utc_now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SecurityDateRange {
    pub tenant_id: String,
    pub event_type: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SecurityBreakdownQuery {
    pub range: SecurityDateRange,
    pub dimension: String,
    pub limit: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SecurityTimeSeriesQuery {
    pub range: SecurityDateRange,
    pub user_id: Option<String>,
    pub service_id: Option<String>,
}
pub trait MetricsAdminDatabase: Send + Sync {
    fn enabled(&self) -> bool {
        true
    }
    /// Canonical database currently lacks get_all_services. This default models
    /// that caught AttributeError (active_services=0), rather than inventing SQL.
    fn all_services(&self) -> MetricsAdminFuture<'_, Vec<Value>> {
        Box::pin(async { Err(MetricsAdminBackendError::Unavailable) })
    }
    fn security_breakdown<'a>(
        &'a self,
        query: &'a SecurityBreakdownQuery,
    ) -> MetricsAdminFuture<'a, Vec<Value>>;
    fn security_timeseries<'a>(
        &'a self,
        query: &'a SecurityTimeSeriesQuery,
    ) -> MetricsAdminFuture<'a, Vec<Value>>;
    fn security_last_ingested<'a>(
        &'a self,
        query: &'a SecurityDateRange,
    ) -> MetricsAdminFuture<'a, Option<ProtocolTimestamp>>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HourlyMetric {
    pub hour: String,
    pub count: i64,
}
fn hundred() -> f64 {
    100.0
}
fn empty() -> String {
    "empty".into()
}
fn no_source() -> String {
    "none".into()
}
fn stale() -> i64 {
    9999
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MetricsSummary {
    pub total_requests: i64,
    pub success_rate: f64,
    pub avg_latency_ms: i64,
    pub active_services: i64,
    pub requests_by_hour: Vec<HourlyMetric>,
    #[serde(default)]
    pub latency_p50: i64,
    #[serde(default)]
    pub latency_p95: i64,
    #[serde(default)]
    pub latency_p99: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub estimated_cost_usd: f64,
    #[serde(default)]
    pub total_runs: i64,
    #[serde(default = "hundred")]
    pub run_success_rate: f64,
    #[serde(default)]
    pub avg_run_duration_ms: i64,
    pub last_updated: String,
    #[serde(default)]
    pub is_simulated: bool,
    #[serde(default = "empty")]
    pub data_status: String,
    #[serde(default = "stale")]
    pub data_freshness_minutes: i64,
    #[serde(default)]
    pub last_ingested_at: Option<String>,
    #[serde(default = "no_source")]
    pub data_source: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeSeriesPoint {
    pub timestamp: String,
    pub value: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeSeriesResponse {
    pub metric: String,
    pub granularity: String,
    pub start: String,
    pub end: String,
    pub data: Vec<TimeSeriesPoint>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TokenUsagePeriod {
    pub period: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TokenUsageResponse {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub estimated_cost_usd: f64,
    pub by_period: Vec<TokenUsagePeriod>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BreakdownItem {
    pub name: String,
    pub count: i64,
    pub percentage: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BreakdownResponse {
    pub dimension: String,
    pub items: Vec<BreakdownItem>,
}
pub type SecurityEventBreakdownItem = BreakdownItem;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SecurityEventBreakdownResponse {
    pub dimension: String,
    pub event_type: String,
    pub items: Vec<SecurityEventBreakdownItem>,
    pub start_date: String,
    pub end_date: String,
    pub data_status: String,
    pub data_freshness_minutes: i64,
    pub last_ingested_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SecurityEventTimeSeriesPoint {
    pub date: String,
    pub count: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SecurityEventTimeSeriesResponse {
    pub dimension: String,
    pub event_type: String,
    pub data: Vec<SecurityEventTimeSeriesPoint>,
    pub start_date: String,
    pub end_date: String,
    pub data_status: String,
    pub data_freshness_minutes: i64,
    pub last_ingested_at: Option<String>,
}
#[derive(Clone, Debug)]
pub struct MetricsTimeSeriesQuery {
    pub metric: String,
    pub start: ProtocolTimestamp,
    pub end: ProtocolTimestamp,
    pub granularity: String,
}
#[derive(Clone, Debug, Default)]
pub struct MetricsTokenQuery {
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
}
#[derive(Clone, Debug)]
pub struct MetricsBreakdownQuery {
    pub dimension: String,
    pub limit: i64,
    pub date_str: Option<String>,
}
#[derive(Clone, Debug)]
pub struct MetricsSecurityQuery {
    pub dimension: String,
    pub event_type: String,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub limit: i64,
    pub user_id: Option<String>,
    pub service_id: Option<String>,
}
impl Default for MetricsSecurityQuery {
    fn default() -> Self {
        Self {
            dimension: "user".into(),
            event_type: "auth_failed".into(),
            start_date: None,
            end_date: None,
            limit: 20,
            user_id: None,
            service_id: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricsAdminOperation {
    Summary,
    TimeSeries,
    Tokens,
    Breakdown,
    SecurityBreakdown,
    SecurityTimeSeries,
}
impl MetricsAdminOperation {
    fn global(self) -> bool {
        !matches!(self, Self::SecurityBreakdown | Self::SecurityTimeSeries)
    }
}
#[derive(Clone, Debug)]
pub enum MetricsAdminQuery {
    Summary,
    TimeSeries(MetricsTimeSeriesQuery),
    Tokens(MetricsTokenQuery),
    Breakdown(MetricsBreakdownQuery),
    SecurityBreakdown(MetricsSecurityQuery),
    SecurityTimeSeries(MetricsSecurityQuery),
}
impl MetricsAdminQuery {
    fn operation(&self) -> MetricsAdminOperation {
        match self {
            Self::Summary => MetricsAdminOperation::Summary,
            Self::TimeSeries(_) => MetricsAdminOperation::TimeSeries,
            Self::Tokens(_) => MetricsAdminOperation::Tokens,
            Self::Breakdown(_) => MetricsAdminOperation::Breakdown,
            Self::SecurityBreakdown(_) => MetricsAdminOperation::SecurityBreakdown,
            Self::SecurityTimeSeries(_) => MetricsAdminOperation::SecurityTimeSeries,
        }
    }
}
#[derive(Clone)]
pub struct MetricsAdminApi {
    recorder: Arc<dyn MetricsSummarySource>,
    redis: Option<Arc<dyn MetricsAdminRedis>>,
    database: Option<Arc<dyn MetricsAdminDatabase>>,
    clock: Arc<dyn MetricsAdminClock>,
    rbac: Rbac,
    denied: Option<Arc<dyn MetricsPermissionDeniedSink>>,
}
impl MetricsAdminApi {
    /// No connections, globals, environment access, spawned tasks or route mounts.
    pub fn new(
        recorder: Arc<dyn MetricsSummarySource>,
        redis: Option<Arc<dyn MetricsAdminRedis>>,
        database: Option<Arc<dyn MetricsAdminDatabase>>,
        clock: Arc<dyn MetricsAdminClock>,
        rbac: Rbac,
        denied: Option<Arc<dyn MetricsPermissionDeniedSink>>,
    ) -> Self {
        Self {
            recorder,
            redis,
            database,
            clock,
            rbac,
            denied,
        }
    }
    pub(crate) fn require(
        &self,
        user: &MetricsAdminIdentity,
        global: bool,
        context: &MetricsAdminRequestContext,
    ) -> ApiResult<()> {
        let roles: Vec<_> = user
            .roles
            .iter()
            .map(|s| s.trim_matches(python_whitespace).to_owned())
            .collect();
        let permissions: Vec<_> = user
            .permissions
            .iter()
            .map(|s| s.trim_matches(python_whitespace).to_owned())
            .collect();
        let capability = Capability::GatewayMetricsRead;
        let allowed = check_capability(&self.rbac, &roles, &permissions, capability).allowed();
        let platform = roles.iter().chain(&permissions).any(|s| {
            matches!(
                s.to_lowercase().as_str(),
                "platform_admin" | "superadmin" | "super_admin"
            )
        });
        if allowed && (!global || platform) {
            return Ok(());
        }
        if let Some(sink) = &self.denied {
            sink.schedule(MetricsPermissionDeniedEvent { tenant_id: user.tenant().into(), user_id: (!user.user_id.is_empty()).then(|| user.user_id.clone()), service_id: None, event_type: "auth_failed".into(), metadata: json!({"trace_id":context.trace(),"request_id":context.request_id,"required_capability":capability.name(),"status":"denied"}) });
        }
        let mut detail = permission_denied_detail(capability, context.trace(), None);
        if allowed && global {
            detail["reason"] = "platform_admin_required".into();
        }
        Err(MetricsAdminError::http(403, detail))
    }
    pub async fn execute(
        &self,
        user: &MetricsAdminIdentity,
        query: MetricsAdminQuery,
        context: &MetricsAdminRequestContext,
    ) -> ApiResult<Value> {
        self.require(user, query.operation().global(), context)?;
        match query {
            MetricsAdminQuery::Summary => encode(self.summary().await?),
            MetricsAdminQuery::TimeSeries(q) => encode(self.timeseries(q).await?),
            MetricsAdminQuery::Tokens(q) => encode(self.tokens(q).await?),
            MetricsAdminQuery::Breakdown(q) => encode(self.breakdown(q).await?),
            MetricsAdminQuery::SecurityBreakdown(q) => {
                encode(self.security_breakdown(user, q).await?)
            }
            MetricsAdminQuery::SecurityTimeSeries(q) => {
                encode(self.security_timeseries(user, q).await?)
            }
        }
    }
    fn redis(&self) -> Option<&dyn MetricsAdminRedis> {
        self.redis.as_deref().filter(|r| r.available())
    }
    async fn summary(&self) -> ApiResult<MetricsSummary> {
        let mut summary = self.recorder.today_summary().await?;
        let mut active_services = 0;
        if let Some(db) = &self.database {
            match db.all_services().await {
                Ok(rows) => {
                    // A non-mapping service raises in the source comprehension.
                    if rows.iter().all(Value::is_object) {
                        active_services = rows
                            .iter()
                            .filter(|row| {
                                row.get("status").and_then(Value::as_str) == Some("healthy")
                            })
                            .count();
                    }
                }
                Err(MetricsAdminBackendError::Cancelled) => {
                    return Err(MetricsAdminError::Cancelled);
                }
                Err(MetricsAdminBackendError::Unavailable) => (),
            }
        }
        let total = summary
            .get("total_requests")
            .and_then(Value::as_i64)
            .ok_or(MetricsAdminError::Internal)?;
        let object = summary.as_object_mut().ok_or(MetricsAdminError::Internal)?;
        object.insert("active_services".into(), json!(active_services));
        object.insert("is_simulated".into(), json!(total == 0));
        let now = self.clock.local_now();
        if !valid_local_datetime(now) {
            return Err(MetricsAdminError::Internal);
        }
        object.insert(
            "last_updated".into(),
            json!(ProtocolTimestamp::from(now).to_isoformat()),
        );
        serde_json::from_value(summary).map_err(|_| MetricsAdminError::Internal)
    }
    async fn timeseries(&self, q: MetricsTimeSeriesQuery) -> ApiResult<TimeSeriesResponse> {
        if !valid_local_datetime(local_timestamp(q.start))
            || !valid_local_datetime(local_timestamp(q.end))
        {
            return Err(MetricsAdminError::Internal);
        }
        let mut response = TimeSeriesResponse {
            metric: q.metric.clone(),
            granularity: q.granularity.clone(),
            start: q.start.to_isoformat(),
            end: q.end.to_isoformat(),
            data: vec![],
        };
        let Some(redis) = self.redis() else {
            return Ok(response);
        };
        let result: Result<(), MetricsAdminBackendError> = async {
            let delta = match q.granularity.as_str() {
                "minute" => Duration::minutes(1),
                "day" => Duration::days(1),
                _ => Duration::hours(1),
            };
            let mut current = q.start;
            while timestamp_le(current, q.end)? {
                let local = local_timestamp(current);
                let day = python_strftime_date(local.date());
                let hour = local.format("%H");
                let value = match q.metric.as_str() {
                    "requests" => {
                        read_int(
                            redis,
                            if q.granularity == "hour" {
                                format!("metrics:requests:hour:{day}:{hour}")
                            } else {
                                format!("metrics:requests:total:{day}")
                            },
                        )
                        .await?
                    }
                    "tokens" => {
                        let input = read_int(redis, format!("metrics:tokens:input:{day}")).await?;
                        let output =
                            read_int(redis, format!("metrics:tokens:output:{day}")).await?;
                        input
                            .checked_add(output)
                            .ok_or(MetricsAdminBackendError::Unavailable)?
                    }
                    "errors" if q.granularity == "hour" => {
                        read_int(redis, format!("metrics:errors:hour:{day}:{hour}")).await?
                    }
                    "runs" => read_int(redis, format!("metrics:runs:total:{day}")).await?,
                    _ => 0,
                };
                response.data.push(TimeSeriesPoint {
                    timestamp: current.to_isoformat(),
                    value: value as f64,
                });
                current = timestamp_add(current, delta)?;
            }
            Ok(())
        }
        .await;
        propagate_cancel(result)?;
        Ok(response)
    }
    fn date_range(
        &self,
        start: Option<NaiveDate>,
        end: Option<NaiveDate>,
        days: i64,
    ) -> ApiResult<(NaiveDate, NaiveDate)> {
        let end = end.unwrap_or_else(|| self.clock.today());
        if !valid_date(&end) || start.is_some_and(|v| !valid_date(&v)) {
            return Err(MetricsAdminError::Internal);
        }
        let start = match start {
            Some(start) => start,
            None => end
                .checked_sub_signed(Duration::days(days))
                .filter(valid_date)
                .ok_or(MetricsAdminError::Internal)?,
        };
        Ok((start, end))
    }
    async fn tokens(&self, q: MetricsTokenQuery) -> ApiResult<TokenUsageResponse> {
        let (start, end) = self.date_range(q.start_date, q.end_date, 6)?;
        let mut response = TokenUsageResponse {
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            by_period: vec![],
        };
        if let Some(redis) = self.redis() {
            let result: Result<(), MetricsAdminBackendError> = async {
                let mut current = start;
                while current <= end {
                    let period = python_strftime_date(current);
                    let input = read_int(redis, format!("metrics:tokens:input:{period}")).await?;
                    let output = read_int(redis, format!("metrics:tokens:output:{period}")).await?;
                    let cents = read_int(redis, format!("metrics:tokens:cost:{period}")).await?;
                    let micro =
                        read_int(redis, format!("metrics:tokens:cost_micro:{period}")).await?;
                    let cost = if micro > 0 {
                        microcents_to_usd(micro)
                            .to_string()
                            .parse::<f64>()
                            .map_err(|_| MetricsAdminBackendError::Unavailable)?
                    } else {
                        round(integer_ratio(cents, 100), 6)
                    };
                    response.total_input_tokens = response
                        .total_input_tokens
                        .checked_add(input)
                        .ok_or(MetricsAdminBackendError::Unavailable)?;
                    response.total_output_tokens = response
                        .total_output_tokens
                        .checked_add(output)
                        .ok_or(MetricsAdminBackendError::Unavailable)?;
                    response.estimated_cost_usd += cost;
                    response.by_period.push(TokenUsagePeriod {
                        period,
                        input_tokens: input,
                        output_tokens: output,
                        cost_usd: round(cost, 6),
                    });
                    current = current
                        .succ_opt()
                        .filter(valid_date)
                        .ok_or(MetricsAdminBackendError::Unavailable)?;
                }
                Ok(())
            }
            .await;
            propagate_cancel(result)?;
        }
        response.total_tokens = response
            .total_input_tokens
            .checked_add(response.total_output_tokens)
            .ok_or(MetricsAdminError::Internal)?;
        response.estimated_cost_usd = round(response.estimated_cost_usd, 6);
        Ok(response)
    }
    async fn breakdown(&self, q: MetricsBreakdownQuery) -> ApiResult<BreakdownResponse> {
        let mut response = BreakdownResponse {
            dimension: q.dimension.clone(),
            items: vec![],
        };
        let day = q
            .date_str
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| python_strftime_date(self.clock.local_now().date()));
        let Some(redis) = self.redis() else {
            return Ok(response);
        };
        let result: Result<Vec<BreakdownItem>, MetricsAdminBackendError> = async {
            let pattern = match q.dimension.as_str() {
                "assistant" => format!("metrics:runs:assistant:*:{day}"),
                "service" => format!("metrics:service:*:{day}"),
                _ => format!("metrics:tokens:user:*:input:{day}"),
            };
            let mut cursor = 0;
            let mut counts: Vec<(String, i64)> = vec![];
            loop {
                let (next, keys) = redis.scan(cursor, &pattern, 100).await?;
                cursor = next;
                for key in keys {
                    let count = integer(&redis.get(&key).await?)
                        .map_err(|_| MetricsAdminBackendError::Unavailable)?;
                    // bytes.split(str) raises TypeError, after GET, in Python.
                    let MetricValue::Text(key) = key else {
                        return Err(MetricsAdminBackendError::Unavailable);
                    };
                    let index = if q.dimension == "service" { 2 } else { 3 };
                    let id = key.split(':').nth(index).unwrap_or("unknown");
                    if let Some((_, prior)) = counts.iter_mut().find(|(name, _)| name == id) {
                        *prior = prior
                            .checked_add(count)
                            .ok_or(MetricsAdminBackendError::Unavailable)?;
                    } else {
                        counts.push((id.into(), count));
                    }
                }
                if cursor == 0 {
                    break;
                }
            }
            let sum = counts
                .iter()
                .try_fold(0i64, |sum, (_, count)| sum.checked_add(*count))
                .ok_or(MetricsAdminBackendError::Unavailable)?;
            let total = if sum == 0 { 1 } else { sum };
            counts.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            let length = if q.limit >= 0 {
                usize::try_from(q.limit)
                    .unwrap_or(usize::MAX)
                    .min(counts.len())
            } else {
                counts
                    .len()
                    .saturating_sub(usize::try_from(q.limit.unsigned_abs()).unwrap_or(usize::MAX))
            };
            Ok(counts
                .into_iter()
                .take(length)
                .map(|(name, count)| BreakdownItem {
                    name,
                    count,
                    percentage: round(integer_ratio(count, total) * 100.0, 1),
                })
                .collect())
        }
        .await;
        match result {
            Ok(items) => response.items = items,
            Err(MetricsAdminBackendError::Cancelled) => return Err(MetricsAdminError::Cancelled),
            Err(MetricsAdminBackendError::Unavailable) => (),
        }
        Ok(response)
    }
    fn security_range(
        &self,
        user: &MetricsAdminIdentity,
        q: &MetricsSecurityQuery,
    ) -> ApiResult<SecurityDateRange> {
        if !matches!(q.dimension.as_str(), "user" | "service") {
            return Err(MetricsAdminError::http(
                400,
                format!("Invalid dimension: {}", q.dimension),
            ));
        }
        if !matches!(
            q.event_type.as_str(),
            "auth_failed" | "rate_limited" | "quota_exceeded"
        ) {
            return Err(MetricsAdminError::http(
                400,
                format!("Invalid event_type: {}", q.event_type),
            ));
        }
        let (start_date, end_date) = self.date_range(q.start_date, q.end_date, 30)?;
        Ok(SecurityDateRange {
            tenant_id: user.tenant().into(),
            event_type: q.event_type.clone(),
            start_date,
            end_date,
        })
    }
    async fn security_breakdown(
        &self,
        user: &MetricsAdminIdentity,
        q: MetricsSecurityQuery,
    ) -> ApiResult<SecurityEventBreakdownResponse> {
        let range = self.security_range(user, &q)?;
        let mut items = vec![];
        let mut last = None;
        if let Some(db) = self.database.as_deref().filter(|db| db.enabled()) {
            let rows = db
                .security_breakdown(&SecurityBreakdownQuery {
                    range: range.clone(),
                    dimension: q.dimension.clone(),
                    limit: q.limit,
                })
                .await?;
            let counts = rows.iter().map(row_count).collect::<ApiResult<Vec<_>>>()?;
            let sum = checked_sum(counts.iter().copied())?;
            let total = if sum == 0 { 1 } else { sum };
            for (row, count) in rows.into_iter().zip(counts) {
                let name = row
                    .get("dimension_value")
                    .filter(|v| json_truth(v))
                    .map(python_value_to_string)
                    .unwrap_or_else(|| "unknown".into());
                items.push(BreakdownItem {
                    name,
                    count,
                    percentage: round(integer_ratio(count, total) * 100.0, 1),
                });
            }
            last = db.security_last_ingested(&range).await?;
        }
        if last.is_some_and(|v| !valid_local_datetime(local_timestamp(v))) {
            return Err(MetricsAdminError::Internal);
        }
        let (status, age) = self.freshness(last, checked_sum(items.iter().map(|item| item.count))?);
        Ok(SecurityEventBreakdownResponse {
            dimension: q.dimension,
            event_type: q.event_type,
            items,
            start_date: range.start_date.to_string(),
            end_date: range.end_date.to_string(),
            data_status: status.into(),
            data_freshness_minutes: age,
            last_ingested_at: last.map(ProtocolTimestamp::to_isoformat),
        })
    }
    async fn security_timeseries(
        &self,
        user: &MetricsAdminIdentity,
        q: MetricsSecurityQuery,
    ) -> ApiResult<SecurityEventTimeSeriesResponse> {
        let range = self.security_range(user, &q)?;
        let mut data = vec![];
        let mut last = None;
        if let Some(db) = self.database.as_deref().filter(|db| db.enabled()) {
            let query = SecurityTimeSeriesQuery {
                range: range.clone(),
                user_id: if q.dimension == "user" {
                    q.user_id
                } else {
                    None
                },
                service_id: if q.dimension == "service" {
                    q.service_id
                } else {
                    None
                },
            };
            let rows = db.security_timeseries(&query).await?;
            for row in rows {
                data.push(SecurityEventTimeSeriesPoint {
                    date: python_value_to_string(row.get("date").unwrap_or(&Value::Null)),
                    count: row_count(&row)?,
                });
            }
            // Deliberately not filtered by user/service: canonical freshness is
            // for the entire tenant/event/date range, unlike the displayed data.
            last = db.security_last_ingested(&range).await?;
        }
        if last.is_some_and(|v| !valid_local_datetime(local_timestamp(v))) {
            return Err(MetricsAdminError::Internal);
        }
        let (status, age) =
            self.freshness(last, checked_sum(data.iter().map(|point| point.count))?);
        Ok(SecurityEventTimeSeriesResponse {
            dimension: q.dimension,
            event_type: q.event_type,
            data,
            start_date: range.start_date.to_string(),
            end_date: range.end_date.to_string(),
            data_status: status.into(),
            data_freshness_minutes: age,
            last_ingested_at: last.map(ProtocolTimestamp::to_isoformat),
        })
    }
    fn freshness(&self, last: Option<ProtocolTimestamp>, total: i64) -> (&'static str, i64) {
        let last = last.map(|value| match value {
            ProtocolTimestamp::Naive(v) => v.and_utc().fixed_offset(),
            ProtocolTimestamp::Offset(v) => v,
        });
        compute_data_status(last, self.clock.utc_now(), Some(total), 60)
    }
}
fn encode<T: Serialize>(value: T) -> ApiResult<Value> {
    serde_json::to_value(value).map_err(|_| MetricsAdminError::Internal)
}
fn json_truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn row_count(row: &Value) -> ApiResult<i64> {
    if !row.is_object() {
        return Err(MetricsAdminError::Internal);
    }
    let value = row.get("total_events").unwrap_or(&Value::Null);
    if !json_truth(value) {
        return Ok(0);
    }
    match value {
        Value::String(text) => {
            integer(&MetricValue::Text(text.clone())).map_err(|_| MetricsAdminError::Internal)
        }
        Value::Bool(v) => Ok(i64::from(*v)),
        Value::Number(v) => v
            .as_i64()
            .or_else(|| {
                v.as_f64()
                    .filter(|v| *v >= i64::MIN as f64 && *v < 9_223_372_036_854_775_808.0)
                    .map(|v| v as i64)
            })
            .ok_or(MetricsAdminError::Internal),
        _ => Err(MetricsAdminError::Internal),
    }
}
fn checked_sum(mut values: impl Iterator<Item = i64>) -> ApiResult<i64> {
    values
        .try_fold(0i64, |sum, v| sum.checked_add(v))
        .ok_or(MetricsAdminError::Internal)
}
fn propagate_cancel(result: Result<(), MetricsAdminBackendError>) -> ApiResult<()> {
    if result == Err(MetricsAdminBackendError::Cancelled) {
        Err(MetricsAdminError::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) async fn read_int(
    redis: &dyn MetricsAdminRedis,
    key: String,
) -> Result<i64, MetricsAdminBackendError> {
    integer(&redis.get(&MetricValue::Text(key)).await?)
        .map_err(|_| MetricsAdminBackendError::Unavailable)
}
// CPython's Linux strftime("%Y") does not zero-pad years below 1000.
// This is only for source strftime Redis keys/periods, never ISO response dates.
pub(crate) fn python_strftime_date(value: NaiveDate) -> String {
    format!("{}-{:02}-{:02}", value.year(), value.month(), value.day())
}

fn valid_date(value: &NaiveDate) -> bool {
    (1..=9999).contains(&value.year())
}
fn valid_local_datetime(value: NaiveDateTime) -> bool {
    valid_date(&value.date()) && value.nanosecond() < 1_000_000_000
}

pub(crate) fn local_timestamp(value: ProtocolTimestamp) -> NaiveDateTime {
    match value {
        ProtocolTimestamp::Naive(v) => v,
        ProtocolTimestamp::Offset(v) => v.naive_local(),
    }
}
pub(crate) fn timestamp_le(
    a: ProtocolTimestamp,
    b: ProtocolTimestamp,
) -> Result<bool, MetricsAdminBackendError> {
    match (a, b) {
        (ProtocolTimestamp::Naive(a), ProtocolTimestamp::Naive(b)) => Ok(a <= b),
        (ProtocolTimestamp::Offset(a), ProtocolTimestamp::Offset(b)) => Ok(a <= b),
        _ => Err(MetricsAdminBackendError::Unavailable),
    }
}
pub(crate) fn timestamp_add(
    value: ProtocolTimestamp,
    delta: Duration,
) -> Result<ProtocolTimestamp, MetricsAdminBackendError> {
    let next = match value {
        ProtocolTimestamp::Naive(v) => v.checked_add_signed(delta).map(ProtocolTimestamp::from),
        ProtocolTimestamp::Offset(v) => v.checked_add_signed(delta).map(ProtocolTimestamp::from),
    }
    .ok_or(MetricsAdminBackendError::Unavailable)?;
    if valid_date(&local_timestamp(next).date()) {
        Ok(next)
    } else {
        Err(MetricsAdminBackendError::Unavailable)
    }
}

/// Native GET/SCAN adapter over an already-connected manager. `decode_responses`
/// preserves the source's byte-key breakdown failure instead of silently decoding.
#[derive(Clone)]
pub struct RedisMetricsAdminBackend {
    connection: redis::aio::ConnectionManager,
    decode_responses: bool,
}
impl RedisMetricsAdminBackend {
    pub fn new(connection: redis::aio::ConnectionManager, decode_responses: bool) -> Self {
        Self {
            connection,
            decode_responses,
        }
    }
    fn scalar(&self, bytes: Option<Vec<u8>>) -> Result<MetricValue, MetricsAdminBackendError> {
        match bytes {
            None => Ok(MetricValue::Missing),
            Some(v) if self.decode_responses => String::from_utf8(v)
                .map(MetricValue::Text)
                .map_err(|_| MetricsAdminBackendError::Unavailable),
            Some(v) => Ok(MetricValue::Bytes(v)),
        }
    }
}
impl MetricsAdminRedis for RedisMetricsAdminBackend {
    fn get<'a>(&'a self, key: &'a MetricValue) -> MetricsAdminFuture<'a, MetricValue> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let mut command = redis::cmd("GET");
            match key {
                MetricValue::Text(v) => {
                    command.arg(v);
                }
                MetricValue::Bytes(v) => {
                    command.arg(v);
                }
                MetricValue::Integer(v) => {
                    command.arg(*v);
                }
                MetricValue::Missing => return Err(MetricsAdminBackendError::Unavailable),
            }
            let value: Option<Vec<u8>> = command
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsAdminBackendError::Unavailable)?;
            self.scalar(value)
        })
    }
    fn scan<'a>(
        &'a self,
        cursor: u64,
        pattern: &'a str,
        count: usize,
    ) -> MetricsAdminFuture<'a, (u64, Vec<MetricValue>)> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let (cursor, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(count)
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsAdminBackendError::Unavailable)?;
            Ok((
                cursor,
                keys.into_iter()
                    .map(|key| self.scalar(Some(key)))
                    .collect::<Result<_, _>>()?,
            ))
        })
    }
}

/// Whitelisted identifiers only; all caller values remain bind parameters.
pub fn security_breakdown_sql(dimension: &str) -> Option<String> {
    let column = match dimension {
        "user" => "user_id",
        "service" => "service_id",
        _ => return None,
    };
    Some(format!(
        "SELECT {column} as dimension_value, SUM(event_count) as total_events FROM security_event_daily_aggregates WHERE tenant_id = $1 AND date >= $2 AND date <= $3 AND event_type = $4 AND {column} IS NOT NULL GROUP BY {column} ORDER BY total_events DESC LIMIT $5"
    ))
}
/// Empty filters are false in the source SQL builder, though passed through by
/// the endpoint. Keep parameter order user before service when both are present.
pub fn security_timeseries_sql(user_id: Option<&str>, service_id: Option<&str>) -> String {
    let mut query = "SELECT date, SUM(event_count) as total_events FROM security_event_daily_aggregates WHERE tenant_id = $1 AND date >= $2 AND date <= $3 AND event_type = $4".to_owned();
    let mut index = 5;
    if user_id.is_some_and(|v| !v.is_empty()) {
        query.push_str(&format!(" AND user_id = ${index}"));
        index += 1;
    }
    if service_id.is_some_and(|v| !v.is_empty()) {
        query.push_str(&format!(" AND service_id = ${index}"));
    }
    query.push_str(" GROUP BY date ORDER BY date");
    query
}
pub const SECURITY_LAST_INGESTED_SQL: &str = "SELECT MAX(updated_at) AS last_ingested FROM security_event_daily_aggregates WHERE tenant_id = $1 AND event_type = $2 AND date >= $3 AND date <= $4";

/// Caller-owned direct/session-pinned data and independent control pools. The
/// adapter adds no service query absent from the canonical database API.
#[derive(Clone)]
pub struct PgMetricsAdminDatabase {
    storage: PgOperationStorage,
}
impl PgMetricsAdminDatabase {
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, MetricsAdminBackendError> {
        Ok(Self {
            storage: PgOperationStorage::new(pool, control_pool)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), MetricsAdminBackendError> {
        self.storage.drain_cancelled().await.map_err(Into::into)
    }
}
impl MetricsAdminDatabase for PgMetricsAdminDatabase {
    fn security_breakdown<'a>(
        &'a self,
        query: &'a SecurityBreakdownQuery,
    ) -> MetricsAdminFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let Some(sql) = security_breakdown_sql(&query.dimension) else {
                return Ok(vec![]);
            };
            let query = query.clone();
            let mut connection = self.storage.acquire().await?;
            let rows = connection.execute(move |connection| Box::pin(async move {
                let rows = sqlx::query(&sql).bind(&query.range.tenant_id).bind(query.range.start_date).bind(query.range.end_date).bind(&query.range.event_type).bind(query.limit).fetch_all(connection).await?;
                rows.into_iter().map(|row| Ok(json!({"dimension_value":row.try_get::<Option<String>, _>("dimension_value")?,"total_events":row.try_get::<Option<i64>, _>("total_events")?}))).collect::<Result<Vec<_>, sqlx::Error>>()
            })).await?;
            Ok(rows)
        })
    }
    fn security_timeseries<'a>(
        &'a self,
        query: &'a SecurityTimeSeriesQuery,
    ) -> MetricsAdminFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let sql =
                security_timeseries_sql(query.user_id.as_deref(), query.service_id.as_deref());
            let query = query.clone();
            let mut connection = self.storage.acquire().await?;
            let rows = connection.execute(move |connection| Box::pin(async move {
                let mut statement = sqlx::query(&sql).bind(&query.range.tenant_id).bind(query.range.start_date).bind(query.range.end_date).bind(&query.range.event_type);
                if let Some(value) = query.user_id.as_deref().filter(|v| !v.is_empty()) { statement = statement.bind(value); }
                if let Some(value) = query.service_id.as_deref().filter(|v| !v.is_empty()) { statement = statement.bind(value); }
                statement.fetch_all(connection).await?.into_iter().map(|row| Ok(json!({"date":row.try_get::<NaiveDate,_>("date")?.to_string(),"total_events":row.try_get::<Option<i64>,_>("total_events")?}))).collect::<Result<Vec<_>, sqlx::Error>>()
            })).await?;
            Ok(rows)
        })
    }
    fn security_last_ingested<'a>(
        &'a self,
        query: &'a SecurityDateRange,
    ) -> MetricsAdminFuture<'a, Option<ProtocolTimestamp>> {
        Box::pin(async move {
            let query = query.clone();
            let mut connection = self.storage.acquire().await?;
            let value: Option<DateTime<Utc>> = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query_scalar(SECURITY_LAST_INGESTED_SQL)
                            .bind(&query.tenant_id)
                            .bind(&query.event_type)
                            .bind(query.start_date)
                            .bind(query.end_date)
                            .fetch_one(connection)
                            .await
                    })
                })
                .await?;
            Ok(value.map(|value| ProtocolTimestamp::from(value.fixed_offset())))
        })
    }
}

/// Semantic query validation runs after identity resolution and before endpoint
/// authorization, as FastAPI does. Unknown query fields are ignored; caller
/// tenant_id/roles/permissions never reach the trusted identity.
pub fn validate_query(
    operation: MetricsAdminOperation,
    values: &HashMap<String, String>,
) -> ApiResult<MetricsAdminQuery> {
    let mut errors = vec![];
    let query = match operation {
        MetricsAdminOperation::Summary => MetricsAdminQuery::Summary,
        MetricsAdminOperation::TimeSeries => {
            let metric = required_string(values, "metric", &mut errors);
            let start = required_timestamp(values, "start", &mut errors);
            let end = required_timestamp(values, "end", &mut errors);
            MetricsAdminQuery::TimeSeries(MetricsTimeSeriesQuery {
                metric,
                start,
                end,
                granularity: values
                    .get("granularity")
                    .cloned()
                    .unwrap_or_else(|| "hour".into()),
            })
        }
        MetricsAdminOperation::Tokens => {
            let start_date = optional_date(values, "start_date", &mut errors);
            let end_date = optional_date(values, "end_date", &mut errors);
            MetricsAdminQuery::Tokens(MetricsTokenQuery {
                start_date,
                end_date,
            })
        }
        MetricsAdminOperation::Breakdown => {
            let dimension = required_string(values, "dimension", &mut errors);
            let limit = query_integer(values, "limit", 10, false, &mut errors);
            MetricsAdminQuery::Breakdown(MetricsBreakdownQuery {
                dimension,
                limit,
                date_str: values.get("date_str").cloned(),
            })
        }
        MetricsAdminOperation::SecurityBreakdown | MetricsAdminOperation::SecurityTimeSeries => {
            let start_date = optional_date(values, "start_date", &mut errors);
            let end_date = optional_date(values, "end_date", &mut errors);
            let limit = if operation == MetricsAdminOperation::SecurityBreakdown {
                query_integer(values, "limit", 20, true, &mut errors)
            } else {
                20
            };
            let query = MetricsSecurityQuery {
                dimension: values
                    .get("dimension")
                    .cloned()
                    .unwrap_or_else(|| "user".into()),
                event_type: values
                    .get("event_type")
                    .cloned()
                    .unwrap_or_else(|| "auth_failed".into()),
                start_date,
                end_date,
                limit,
                user_id: values.get("user_id").cloned(),
                service_id: values.get("service_id").cloned(),
            };
            if operation == MetricsAdminOperation::SecurityBreakdown {
                MetricsAdminQuery::SecurityBreakdown(query)
            } else {
                MetricsAdminQuery::SecurityTimeSeries(query)
            }
        }
    };
    if errors.is_empty() {
        Ok(query)
    } else {
        Err(MetricsAdminError::http(422, errors))
    }
}
fn missing(name: &str) -> Value {
    json!({"type":"missing","loc":["query",name],"msg":"Field required","input":null})
}
fn required_string(
    values: &HashMap<String, String>,
    name: &str,
    errors: &mut Vec<Value>,
) -> String {
    match values.get(name) {
        Some(v) => v.clone(),
        None => {
            errors.push(missing(name));
            String::new()
        }
    }
}
fn required_timestamp(
    values: &HashMap<String, String>,
    name: &str,
    errors: &mut Vec<Value>,
) -> ProtocolTimestamp {
    if let Some(value) = values.get(name) {
        if let Some(parsed) = parse_query_timestamp(value) {
            return parsed;
        }
        errors.push(json!({"type":"datetime_from_date_parsing","loc":["query",name],"msg":"Input should be a valid datetime or date","input":value}));
    } else {
        errors.push(missing(name));
    }
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .into()
}
pub(crate) fn parse_query_timestamp(value: &str) -> Option<ProtocolTimestamp> {
    if let Some(parsed) = parse_query_iso(value) {
        return Some(parsed);
    }
    // Speedate accepts signed decimal numeric epochs, but no exponents,
    // whitespace or underscores. Keep its +/-2e10 seconds/milliseconds watershed.
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let number: f64 = value.parse().ok()?;
    if !number.is_finite() {
        return None;
    }
    // Split whole/fraction first, as Speedate does. Multiplying the full f64
    // epoch (or first dividing a millisecond epoch) introduces a second rounding
    // at today's large epoch, which can even turn a valid date into a non-midnight.
    let whole = number.trunc();
    if whole < i64::MIN as f64 || whole >= 9_223_372_036_854_775_808.0 {
        return None;
    }
    let whole = whole as i64;
    let (seconds, micros) = if number.abs() > 20_000_000_000.0 {
        (
            whole / 1000,
            (whole % 1000)
                .checked_mul(1000)?
                .checked_add((number.fract() * 1000.0).round() as i64)?,
        )
    } else {
        (whole, (number.fract() * 1_000_000.0).round() as i64)
    };
    let micros = seconds.checked_mul(1_000_000)?.checked_add(micros)?;
    let parsed = DateTime::from_timestamp_micros(micros)?;
    valid_date(&parsed.date_naive()).then(|| ProtocolTimestamp::from(parsed.fixed_offset()))
}
fn parse_query_iso(value: &str) -> Option<ProtocolTimestamp> {
    // Query parsing is narrower than Python datetime.fromisoformat used for
    // stored records: no hour-only times, leap seconds, or offset seconds.
    if !value.is_ascii() || value.len() < 10 {
        return None;
    }
    let date = NaiveDate::parse_from_str(&value[..10], "%Y-%m-%d").ok()?;
    if !valid_date(&date) || date.format("%Y-%m-%d").to_string() != value[..10] {
        return None;
    }
    if value.len() == 10 {
        return Some(date.and_hms_opt(0, 0, 0)?.into());
    }
    let bytes = value.as_bytes();
    if value.len() < 16 || !matches!(bytes[10], b'T' | b't' | b' ' | b'_') || bytes[13] != b':' {
        return None;
    }
    if ![bytes[11], bytes[12], bytes[14], bytes[15]]
        .iter()
        .all(u8::is_ascii_digit)
    {
        return None;
    }
    let hour: u32 = value[11..13].parse().ok()?;
    let minute: u32 = value[14..16].parse().ok()?;
    let mut second = 0;
    let mut micro = 0;
    let mut end = 16;
    if bytes.get(end) == Some(&b':') {
        if bytes.len() < end + 3 || !bytes[end + 1..end + 3].iter().all(u8::is_ascii_digit) {
            return None;
        }
        second = value[end + 1..end + 3].parse().ok()?;
        if second >= 60 {
            return None;
        }
        end += 3;
        if matches!(bytes.get(end), Some(b'.' | b',')) {
            end += 1;
            let start = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                return None;
            }
            let fraction = &value[start..end];
            let kept = fraction.len().min(6);
            micro = fraction[..kept].parse::<u32>().ok()? * 10u32.pow((6 - kept) as u32);
        }
    }
    let local = date.and_hms_micro_opt(hour, minute, second, micro)?;
    if end == bytes.len() {
        return Some(local.into());
    }
    let suffix = &value[end..];
    let offset = if matches!(suffix, "Z" | "z") {
        chrono::FixedOffset::east_opt(0)?
    } else {
        let suffix_bytes = suffix.as_bytes();
        if !matches!(suffix_bytes.first(), Some(b'+' | b'-')) {
            return None;
        }
        let (hour, minute) = match suffix.len() {
            5 => (&suffix[1..3], &suffix[3..5]),
            6 if suffix_bytes[3] == b':' => (&suffix[1..3], &suffix[4..6]),
            _ => return None,
        };
        if !hour
            .bytes()
            .chain(minute.bytes())
            .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let hour: i32 = hour.parse().ok()?;
        let minute: i32 = minute.parse().ok()?;
        if hour > 23 || minute > 59 {
            return None;
        }
        let seconds = (hour * 60 + minute) * 60 * if suffix_bytes[0] == b'-' { -1 } else { 1 };
        chrono::FixedOffset::east_opt(seconds)?
    };
    Some(
        DateTime::<chrono::FixedOffset>::from_naive_utc_and_offset(
            local.checked_sub_signed(Duration::seconds(i64::from(offset.local_minus_utc())))?,
            offset,
        )
        .into(),
    )
}

fn optional_date(
    values: &HashMap<String, String>,
    name: &str,
    errors: &mut Vec<Value>,
) -> Option<NaiveDate> {
    let value = values.get(name)?;
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        && valid_date(&date)
        && value == &date.format("%Y-%m-%d").to_string()
    {
        return Some(date);
    }
    if let Some(timestamp) = parse_query_timestamp(value) {
        let local = local_timestamp(timestamp);
        if local.time() == chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap() {
            return Some(local.date());
        }
        errors.push(json!({"type":"date_from_datetime_inexact","loc":["query",name],"msg":"Datetimes provided to dates should have zero time - e.g. be exact dates","input":value}));
    } else {
        errors.push(json!({"type":"date_from_datetime_parsing","loc":["query",name],"msg":"Input should be a valid date or datetime","input":value}));
    }
    None
}
pub(crate) fn query_integer(
    values: &HashMap<String, String>,
    name: &str,
    default: i64,
    bounded: bool,
    errors: &mut Vec<Value>,
) -> i64 {
    let Some(value) = values.get(name) else {
        return default;
    };
    let text = value.trim();
    let parsed = (text.is_ascii() && !text.is_empty())
        .then(|| integer(&MetricValue::Text(text.into())).ok())
        .flatten()
        .or_else(|| {
            if !text.is_ascii() {
                return None;
            }
            let (whole, fraction) = text.split_once('.')?;
            if whole.is_empty()
                || whole.trim() != whole
                || fraction.is_empty()
                || !fraction.bytes().all(|b| b == b'0')
            {
                return None;
            }
            integer(&MetricValue::Text(whole.into())).ok()
        });
    let Some(parsed) = parsed else {
        errors.push(json!({"type":"int_parsing","loc":["query",name],"msg":"Input should be a valid integer, unable to parse string as an integer","input":value}));
        return default;
    };
    if bounded && !(1..=100).contains(&parsed) {
        errors.push(if parsed < 1 { json!({"type":"greater_than_equal","loc":["query",name],"msg":"Input should be greater than or equal to 1","input":value,"ctx":{"ge":1}}) } else { json!({"type":"less_than_equal","loc":["query",name],"msg":"Input should be less than or equal to 100","input":value,"ctx":{"le":100}}) });
    }
    parsed
}

pub trait MetricsAdminCancellation: Send + Sync {
    /// Required explicit host policy. Python CancelledError has no HTTP response.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>>;
}
#[derive(Clone)]
struct HttpState {
    identity: Arc<dyn MetricsAdminIdentityResolver>,
    api: Arc<MetricsAdminApi>,
    cancellation: Arc<dyn MetricsAdminCancellation>,
}
/// No listener or default gateway mount. Host tracing/body limits remain external.
pub fn router(
    identity: Arc<dyn MetricsAdminIdentityResolver>,
    api: Arc<MetricsAdminApi>,
    cancellation: Arc<dyn MetricsAdminCancellation>,
) -> Router {
    Router::new()
        .route(
            "/api/v1/metrics/summary",
            get(http_summary)
                .head(http_method_not_allowed)
                .fallback(http_method_not_allowed),
        )
        .route(
            "/api/v1/metrics/timeseries",
            get(http_timeseries)
                .head(http_method_not_allowed)
                .fallback(http_method_not_allowed),
        )
        .route(
            "/api/v1/metrics/tokens",
            get(http_tokens)
                .head(http_method_not_allowed)
                .fallback(http_method_not_allowed),
        )
        .route(
            "/api/v1/metrics/breakdown",
            get(http_breakdown)
                .head(http_method_not_allowed)
                .fallback(http_method_not_allowed),
        )
        .route(
            "/api/v1/metrics/security/breakdown",
            get(http_security_breakdown)
                .head(http_method_not_allowed)
                .fallback(http_method_not_allowed),
        )
        .route(
            "/api/v1/metrics/security/timeseries",
            get(http_security_timeseries)
                .head(http_method_not_allowed)
                .fallback(http_method_not_allowed),
        )
        .with_state(HttpState {
            identity,
            api,
            cancellation,
        })
}
async fn http_method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, "GET")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
        .into_response()
}

async fn dispatch(
    state: HttpState,
    operation: MetricsAdminOperation,
    request: Request,
) -> Response {
    // These GET handlers never consume a body. Own the headers/URI before the
    // first await so no shared borrow of Request<Body> (Body is not Sync) can
    // make the Axum handler future non-Send.
    let (parts, body) = request.into_parts();
    drop(body);
    let context = parts
        .extensions
        .get::<MetricsAdminRequestContext>()
        .cloned()
        .unwrap_or_default();
    let headers = parts.headers;
    let uri = parts.uri;
    let result = async {
        let identity = state.identity.resolve(&headers, uri.path()).await?;
        let Query(values) = Query::<HashMap<String, String>>::try_from_uri(&uri)
            .map_err(|_| MetricsAdminError::http(422, "Invalid query parameters"))?;
        let query = validate_query(operation, &values)?;
        state.api.execute(&identity, query, &context).await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(MetricsAdminError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => (
            StatusCode::from_u16(error.status().expect("non-cancellation status"))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(
                error
                    .response_body(context.trace())
                    .expect("non-cancellation response"),
            ),
        )
            .into_response(),
    }
}
macro_rules! handler {
    ($name:ident, $operation:ident) => {
        async fn $name(State(state): State<HttpState>, request: Request) -> Response {
            dispatch(state, MetricsAdminOperation::$operation, request).await
        }
    };
}
handler!(http_summary, Summary);
handler!(http_timeseries, TimeSeries);
handler!(http_tokens, Tokens);
handler!(http_breakdown, Breakdown);
handler!(http_security_breakdown, SecurityBreakdown);
handler!(http_security_timeseries, SecurityTimeSeries);
