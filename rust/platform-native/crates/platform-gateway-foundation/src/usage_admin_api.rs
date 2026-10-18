//! Inert, opt-in usage API from `src/api/v1/usage.py`.
//!
//! The factory never mounts the gateway, opens connections, reads credentials or
//! starts tasks. The host supplies trusted AuthContext/RBAC/request state, clocks,
//! recorder and cancellation policy. Caller-provided tenant/identity fields never
//! establish authority. All ten canonical GET routes are retained.
//!
//! Source scope quirks are intentional and require host security acceptance before
//! mounting: export/models are tenant-wide, per-user top_models omits user_id, and
//! trace detail checks tenant but not user ownership. Empty user identity falls
//! back to the requested user. Only exact `admin`/`operator` roles bypass scoping.
//! Permission-denied observation is an explicit host callback, not a hidden task.
//!
//! Native reads reuse PgBillingStore for summary/breakdown/timeseries/freshness;
//! no pricing/accounting algorithm or usage writer is duplicated. Missing latency,
//! failure and trace reads use source SQL and source exception fallbacks. Decimal
//! values are truncated before integer conversion. JSON/native numeric bounds are
//! i64 and 96-bit Decimal; arbitrary Python numbers and nonfinite JSON are outside
//! this adapter. Native PG timestamps use UTC, as in the existing billing adapter.
//!
//! Ordinary query failures are swallowed only at the same recorder boundaries as
//! Python; freshness errors propagate. Cancellation is never swallowed, retried,
//! or mapped to an invented status. Dropping a future drops its current await.
//! The host owns pool session/timezone/timeout/cancellation cleanup policy.
//! Source trailing-slash redirects belong to trusted host assembly. This bare
//! factory returns 404 and never fabricates an origin from request headers.
//! Query transport decoding belongs to Axum; semantic query/response validation is
//! source-derived. FastAPI's malformed-percent/parser diagnostics are not claimed.
use crate::{
    billing_store::{self, Granularity, PgBillingStore, UsageFilter},
    metrics_recorder::{compute_data_status, integer_ratio},
    protocol::ProtocolTimestamp,
    realtime_metrics::python_float_sum,
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, Timelike, Utc};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde_json::{Map, Value, json};
use sqlx::{Column, PgPool, Row, TypeInfo, ValueRef};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub const USAGE_ADMIN_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/usage/summary", "get_usage_summary"),
    ("GET", "/usage/breakdown", "get_usage_breakdown"),
    ("GET", "/usage/timeseries", "get_usage_timeseries"),
    (
        "GET",
        "/usage/performance-breakdown",
        "get_performance_breakdown",
    ),
    ("GET", "/usage/failure-breakdown", "get_failure_breakdown"),
    ("GET", "/usage/traces", "list_request_traces"),
    ("GET", "/usage/traces/{request_id}", "get_request_trace"),
    ("GET", "/usage/user/{user_id}", "get_user_usage"),
    ("GET", "/usage/export", "export_usage"),
    ("GET", "/usage/models", "get_model_usage"),
];
pub type UsageAdminFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UsageAdminError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq)]
pub enum UsageAdminError {
    Http { status: u16, detail: Value },
    Internal,
    ResponseValidation(Vec<Value>),
    Cancelled,
}
impl UsageAdminError {
    pub fn http(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
        }
    }
}
/// Trusted resolver output; deliberately not Deserialize or Serialize.
#[derive(Clone, Default)]
pub struct ResolvedUsageIdentity {
    pub tenant_id: String,
    pub user_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}
#[derive(Clone, Default)]
pub struct UsageRequestContext {
    pub request_id: Value,
    pub trace_id: Option<Value>,
}
impl UsageRequestContext {
    fn trace(&self) -> &str {
        self.request_id
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.trace_id
                    .as_ref()
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("")
    }
    pub fn exception_trace(&self, headers: &HeaderMap) -> Value {
        self.trace_id.clone().unwrap_or_else(|| {
            ["x-trace-id", "x-request-id"]
                .iter()
                .find_map(|key| {
                    headers
                        .get(*key)
                        .filter(|v| !v.as_bytes().is_empty())
                        .map(|v| {
                            v.as_bytes()
                                .iter()
                                .map(|b| char::from(*b))
                                .collect::<String>()
                        })
                })
                .map(|v| json!(v))
                .unwrap_or(Value::Null)
        })
    }
    fn request_id_text(&self) -> String {
        if truthy(&self.request_id) {
            python_value_to_string(&self.request_id)
        } else {
            String::new()
        }
    }
}
pub trait UsageIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> UsageAdminFuture<'a, ResolvedUsageIdentity>;
    /// Matches canonical best-effort scheduling. The host, not this module, owns
    /// any task lifetime and audit storage; failures must not change the denial.
    fn permission_denied(&self, _identity: &ResolvedUsageIdentity, _context: &UsageRequestContext) {
    }
}
impl UsageIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> UsageAdminFuture<'a, ResolvedUsageIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let p = request
                .resolve()
                .await
                .map_err(|e| UsageAdminError::http(e.status(), e.to_string()))?;
            Ok(ResolvedUsageIdentity {
                tenant_id: p.tenant_id().into(),
                user_id: p.user_id().into(),
                roles: p.roles().to_vec(),
                permissions: p.permissions().to_vec(),
            })
        })
    }
}
pub trait UsageAdminClock: Send + Sync {
    fn today(&self) -> NaiveDate;
    fn utc_now(&self) -> DateTime<Utc>;
    /// Python datetime.now() is local and naive. Host explicitly chooses zone.
    fn local_now(&self) -> NaiveDateTime;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageReadOperation {
    Summary,
    Breakdown,
    Timeseries,
    Latency,
    Failure,
    Traces,
    Trace,
    Freshness,
}
impl UsageReadOperation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Summary => "get_usage_summary",
            Self::Breakdown => "get_usage_breakdown",
            Self::Timeseries => "get_usage_timeseries",
            Self::Latency => "get_latency_breakdown_timeseries",
            Self::Failure => "get_failure_breakdown",
            Self::Traces => "list_request_traces",
            Self::Trace => "get_request_trace_by_id",
            Self::Freshness => "get_last_ingested_at",
        }
    }
}
/// Internal, source-shaped kwargs. This interface is not an HTTP/deserialization
/// surface. Implementations receive tenant_id only from the trusted API identity.
/// Values are canonical recorder outputs, dates/timestamps represented as ISO text.
pub trait UsageAdminRecorder: Send + Sync {
    fn call<'a>(
        &'a self,
        operation: UsageReadOperation,
        arguments: Value,
    ) -> UsageAdminFuture<'a, Value>;
}
#[derive(Clone, Debug)]
pub enum UsageAdminOperation {
    Summary,
    Breakdown,
    Timeseries,
    Performance,
    Failure,
    Traces,
    Trace(String),
    User(String),
    Export,
    Models,
}
impl UsageAdminOperation {
    pub fn path(&self) -> String {
        format!(
            "/api/v1/usage/{}",
            match self {
                Self::Summary => "summary".into(),
                Self::Breakdown => "breakdown".into(),
                Self::Timeseries => "timeseries".into(),
                Self::Performance => "performance-breakdown".into(),
                Self::Failure => "failure-breakdown".into(),
                Self::Traces => "traces".into(),
                Self::Trace(id) => format!("traces/{id}"),
                Self::User(id) => format!("user/{id}"),
                Self::Export => "export".into(),
                Self::Models => "models".into(),
            }
        )
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum UsageAdminOutput {
    Json(Value),
    Csv { content: String, filename: String },
}
pub struct UsageAdminApi {
    identity: Arc<dyn UsageIdentityResolver>,
    recorder: Arc<dyn UsageAdminRecorder>,
    clock: Arc<dyn UsageAdminClock>,
    rbac: Rbac,
    dimension_error: String,
}
impl UsageAdminApi {
    /// `dimension_order` is the canonical Python process's set iteration order,
    /// which is hash-seed dependent; no stable public order exists in the source.
    pub fn new(
        identity: Arc<dyn UsageIdentityResolver>,
        recorder: Arc<dyn UsageAdminRecorder>,
        clock: Arc<dyn UsageAdminClock>,
        rbac: Rbac,
        dimension_order: [&str; 5],
    ) -> Result<Self, UsageAdminError> {
        let mut sorted = dimension_order;
        sorted.sort_unstable();
        if sorted != ["assistant", "model", "provider", "service", "user"] {
            return Err(UsageAdminError::Internal);
        }
        Ok(Self {
            identity,
            recorder,
            clock,
            rbac,
            dimension_error: format!(
                "Invalid dimension. Must be one of: {}",
                dimension_order.join(", ")
            ),
        })
    }
    pub async fn handle(
        &self,
        operation: UsageAdminOperation,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
        context: &UsageRequestContext,
    ) -> Result<UsageAdminOutput, UsageAdminError> {
        // Dependencies resolve before request-field validation, capability checks
        // happen inside endpoint bodies only after FastAPI query validation.
        let identity = self.identity.resolve(headers, &operation.path()).await?;
        let q = parse_query(&operation, query)?;
        let capability_roles: Vec<String> = identity
            .roles
            .iter()
            .map(|s| s.trim_matches(python_whitespace).to_owned())
            .collect();
        let capability_permissions: Vec<String> = identity
            .permissions
            .iter()
            .map(|s| s.trim_matches(python_whitespace).to_owned())
            .collect();
        if !check_capability(
            &self.rbac,
            &capability_roles,
            &capability_permissions,
            Capability::GatewayUsageRead,
        )
        .allowed()
        {
            self.identity.permission_denied(&identity, context);
            return Err(UsageAdminError::http(
                403,
                permission_denied_detail(Capability::GatewayUsageRead, context.trace(), None),
            ));
        }
        if identity.tenant_id.is_empty() {
            return Err(UsageAdminError::http(
                403,
                json!({"message":"Permission denied: tenant scope required for usage queries","required_capability":"GatewayUsageRead","required_permission":"console:usage:view","trace_id":context.request_id_text()}),
            ));
        }
        if let UsageAdminOperation::Trace(id) = &operation {
            let row = self
                .recorder
                .call(
                    UsageReadOperation::Trace,
                    json!({"tenant_id":identity.tenant_id,"request_id":id}),
                )
                .await?;
            if !truthy(&row) {
                return Err(UsageAdminError::http(404, "Trace not found"));
            }
            return Ok(UsageAdminOutput::Json(project(&row, Schema::Trace)?));
        }
        match &operation {
            UsageAdminOperation::Breakdown
                if !["model", "user", "assistant", "service", "provider"]
                    .contains(&q.dimension.as_deref().unwrap_or("model")) =>
            {
                return Err(UsageAdminError::http(400, self.dimension_error.clone()));
            }
            UsageAdminOperation::Failure
                if !["service", "model", "provider", "user"]
                    .contains(&q.dimension.as_deref().unwrap_or("service")) =>
            {
                return Err(UsageAdminError::http(400, "Invalid dimension"));
            }
            _ => {}
        }
        let end = q.end.unwrap_or_else(|| self.clock.today());
        let days = if matches!(
            operation,
            UsageAdminOperation::Timeseries | UsageAdminOperation::Export
        ) {
            30
        } else {
            7
        };
        let start = match q.start {
            Some(v) => v,
            None => end
                .checked_sub_signed(TimeDelta::days(days))
                .filter(|v| v.year_ce().0)
                .ok_or(UsageAdminError::Internal)?,
        };
        let mut base = json!({"tenant_id":identity.tenant_id,"start_date":start.to_string(),"end_date":end.to_string()});
        let mut scoped_user = match &operation {
            UsageAdminOperation::User(id) => Some(id.clone()),
            _ => query.get("user_id").cloned(),
        };
        if !identity
            .roles
            .iter()
            .any(|r| r == "admin" || r == "operator")
            && !identity.user_id.is_empty()
        {
            scoped_user = Some(identity.user_id.clone());
        }
        let body = match operation {
            UsageAdminOperation::Summary => {
                base["user_id"] = json!(scoped_user);
                copy_filters(
                    &mut base,
                    query,
                    &["model", "service_id", "assistant_id", "provider"],
                );
                let mut summary = self
                    .recorder
                    .call(UsageReadOperation::Summary, base)
                    .await?;
                let source_status = summary
                    .as_object_mut()
                    .ok_or(UsageAdminError::Internal)?
                    .remove("data_status");
                let total = summary.get("total_requests").and_then(Value::as_i64);
                let mut freshness = self
                    .freshness(&identity.tenant_id, start, end, "day", total)
                    .await?;
                if source_status
                    .as_ref()
                    .is_some_and(|v| v.is_array() || v.is_object())
                {
                    return Err(UsageAdminError::Internal);
                }
                if matches!(
                    source_status.as_ref().and_then(Value::as_str),
                    Some("collection_error" | "unavailable")
                ) {
                    freshness["data_status"] = source_status.unwrap();
                }
                if !summary.get("total_requests").is_some_and(truthy) {
                    summary["success_rate"] = Value::Null;
                }
                merge(&mut summary, freshness)?;
                project(&summary, Schema::Summary)?
            }
            UsageAdminOperation::Breakdown => {
                let dimension = q.dimension.as_deref().unwrap_or("model");
                if !["model", "user", "assistant", "service", "provider"].contains(&dimension) {
                    return Err(UsageAdminError::http(400, self.dimension_error.clone()));
                }
                base["dimension"] = json!(dimension);
                base["user_id"] = json!(scoped_user);
                base["service_id"] = json!(query.get("service_id"));
                base["limit"] = json!(q.limit.unwrap_or(20));
                let rows = self
                    .recorder
                    .call(UsageReadOperation::Breakdown, base)
                    .await?;
                let total_requests = sum_int(&rows, "requests")?;
                let total_cost = sum_float(&rows, "cost_usd")?;
                let mut response = json!({"dimension":dimension,"items":object_list(&rows)?,"start_date":start.to_string(),"end_date":end.to_string(),"total_cost_usd":round(total_cost,4)});
                merge(
                    &mut response,
                    self.freshness(&identity.tenant_id, start, end, "day", Some(total_requests))
                        .await?,
                )?;
                response
            }
            UsageAdminOperation::Timeseries | UsageAdminOperation::Performance => {
                let performance = matches!(operation, UsageAdminOperation::Performance);
                let granularity = q.granularity.as_deref().unwrap_or("day");
                base["user_id"] = json!(scoped_user);
                copy_filters(&mut base, query, &["model", "service_id", "provider"]);
                base["granularity"] = json!(granularity);
                let rows = self
                    .recorder
                    .call(
                        if performance {
                            UsageReadOperation::Latency
                        } else {
                            UsageReadOperation::Timeseries
                        },
                        base,
                    )
                    .await?;
                let count = sum_int(
                    &rows,
                    if performance {
                        "request_count"
                    } else {
                        "requests"
                    },
                )?;
                let freshness = self
                    .freshness(&identity.tenant_id, start, end, granularity, Some(count))
                    .await?;
                let mut response = json!({"data":project_list(&rows,if performance {Schema::Latency}else{Schema::Series})?,"start_date":start.to_string(),"end_date":end.to_string(),"granularity":granularity});
                merge(&mut response, freshness)?;
                response
            }
            UsageAdminOperation::Failure => {
                let dimension = q.dimension.as_deref().unwrap_or("service");
                if !["service", "model", "provider", "user"].contains(&dimension) {
                    return Err(UsageAdminError::http(400, "Invalid dimension"));
                }
                base["dimension"] = json!(dimension);
                base["user_id"] = json!(scoped_user);
                copy_filters(&mut base, query, &["model", "service_id", "provider"]);
                base["limit"] = json!(q.limit.unwrap_or(50));
                let rows = self
                    .recorder
                    .call(UsageReadOperation::Failure, base)
                    .await?;
                json!({"dimension":dimension,"items":project_list(&rows,Schema::Failure)?,"start_date":start.to_string(),"end_date":end.to_string()})
            }
            UsageAdminOperation::Traces => {
                base["user_id"] = json!(scoped_user);
                copy_filters(
                    &mut base,
                    query,
                    &["service_id", "provider", "model", "status", "error_type"],
                );
                base["limit"] = json!(q.limit.unwrap_or(100));
                project_list(
                    &self.recorder.call(UsageReadOperation::Traces, base).await?,
                    Schema::Trace,
                )?
            }
            UsageAdminOperation::User(_) => {
                let mut summary_args = base.clone();
                summary_args["user_id"] = json!(scoped_user);
                let mut summary = self
                    .recorder
                    .call(UsageReadOperation::Summary, summary_args.clone())
                    .await?;
                let freshness = self
                    .freshness(
                        &identity.tenant_id,
                        start,
                        end,
                        "day",
                        summary.get("total_requests").and_then(Value::as_i64),
                    )
                    .await?;
                let mut breakdown_args = base.clone();
                breakdown_args["dimension"] = json!("model");
                breakdown_args["limit"] = json!(5);
                let top = self
                    .recorder
                    .call(UsageReadOperation::Breakdown, breakdown_args)
                    .await?;
                let trend = self
                    .recorder
                    .call(UsageReadOperation::Timeseries, summary_args)
                    .await?;
                // Duplicate explicit keyword is a canonical TypeError after all reads.
                if summary.get("data_status").is_some() {
                    return Err(UsageAdminError::Internal);
                }
                merge(&mut summary, freshness)?;
                let summary = project(&summary, Schema::Summary)?;
                let trend = project_list(&trend, Schema::Series)?;
                let top = object_list_for(&top, "top_models")?;
                json!({"user_id":scoped_user.ok_or(UsageAdminError::Internal)?,"summary":summary,"top_models":top,"daily_trend":trend})
            }
            UsageAdminOperation::Export => {
                let rows = self
                    .recorder
                    .call(UsageReadOperation::Timeseries, base.clone())
                    .await?;
                let dimension = q.dimension.as_deref().unwrap_or("model");
                base["dimension"] = json!(dimension);
                base["limit"] = json!(100);
                let breakdown = self
                    .recorder
                    .call(UsageReadOperation::Breakdown, base)
                    .await?;
                if q.format.as_deref().unwrap_or("csv").to_lowercase() == "json" {
                    json!({"period":{"start":start.to_string(),"end":end.to_string()},"timeseries":rows,"breakdown":{"dimension":dimension,"items":breakdown},"exported_at":naive_iso(self.clock.local_now())})
                } else {
                    return Ok(UsageAdminOutput::Csv {
                        content: export_csv(&rows, &breakdown, dimension)?,
                        filename: format!("usage_export_{start}_{end}.csv"),
                    });
                }
            }
            UsageAdminOperation::Models => {
                base["dimension"] = json!("model");
                base["limit"] = json!(50);
                let rows = self
                    .recorder
                    .call(UsageReadOperation::Breakdown, base)
                    .await?;
                json!({"period":{"start":start.to_string(),"end":end.to_string()},"totals":{"requests":sum_int(&rows,"requests")?,"tokens":sum_int(&rows,"total_tokens")?,"cost_usd":round(sum_float(&rows,"cost_usd")?,4)},"models":rows})
            }
            UsageAdminOperation::Trace(_) => unreachable!(),
        };
        Ok(UsageAdminOutput::Json(body))
    }
    async fn freshness(
        &self,
        tenant: &str,
        start: NaiveDate,
        end: NaiveDate,
        granularity: &str,
        total: Option<i64>,
    ) -> Result<Value, UsageAdminError> {
        let last=self.recorder.call(UsageReadOperation::Freshness,json!({"tenant_id":tenant,"start_date":start.to_string(),"end_date":end.to_string(),"granularity":granularity})).await?;
        let parsed = if last.is_null() {
            None
        } else {
            Some(parse_timestamp(
                last.as_str().ok_or(UsageAdminError::Internal)?,
            )?)
        };
        let (status, minutes) = compute_data_status(parsed, self.clock.utc_now(), total, 60);
        Ok(json!({"data_status":status,"data_freshness_minutes":minutes,"last_ingested_at":last}))
    }
}
use chrono::Datelike;

#[derive(Default)]
struct ParsedQuery {
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    limit: Option<i64>,
    dimension: Option<String>,
    granularity: Option<String>,
    format: Option<String>,
}
fn query_error(
    field: &str,
    input: &str,
    kind: &str,
    msg: impl Into<String>,
    ctx: Option<Value>,
) -> Value {
    let mut value = json!({"type":kind,"loc":["query",field],"msg":msg.into(),"input":input});
    if let Some(ctx) = ctx {
        value["ctx"] = ctx;
    }
    value
}
fn parse_query(
    operation: &UsageAdminOperation,
    q: &HashMap<String, String>,
) -> Result<ParsedQuery, UsageAdminError> {
    let mut result = ParsedQuery::default();
    if matches!(operation, UsageAdminOperation::Trace(_)) {
        return Ok(result);
    }
    let mut errors = vec![];
    for (name, target) in [
        ("start_date", &mut result.start),
        ("end_date", &mut result.end),
    ] {
        if let Some(raw) = q.get(name) {
            match query_date(raw) {
                Ok(v) => *target = Some(v),
                Err((kind, msg, ctx)) => errors.push(query_error(name, raw, kind, msg, ctx)),
            }
        }
    }
    let maximum = match operation {
        UsageAdminOperation::Breakdown => Some(100),
        UsageAdminOperation::Failure => Some(200),
        UsageAdminOperation::Traces => Some(500),
        _ => None,
    };
    if let Some(max) = maximum
        && let Some(raw) = q.get("limit")
    {
        match parse_integer_text(raw) {
            Some(v) if v < 1 => errors.push(query_error(
                "limit",
                raw,
                "greater_than_equal",
                "Input should be greater than or equal to 1",
                Some(json!({"ge":1})),
            )),
            Some(v) if v > max => errors.push(query_error(
                "limit",
                raw,
                "less_than_equal",
                format!("Input should be less than or equal to {max}"),
                Some(json!({"le":max})),
            )),
            Some(v) => result.limit = Some(v as i64),
            None => errors.push(query_error(
                "limit",
                raw,
                "int_parsing",
                "Input should be a valid integer, unable to parse string as an integer",
                None,
            )),
        }
    }
    if !errors.is_empty() {
        return Err(UsageAdminError::http(422, errors));
    }
    result.dimension = q.get("dimension").cloned();
    result.granularity = q.get("granularity").cloned();
    result.format = q.get("format").cloned();
    Ok(result)
}
fn parse_integer_text(raw: &str) -> Option<i128> {
    let text = raw.trim();
    let (negative, text) = if let Some(t) = text.strip_prefix('-') {
        (true, t)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text, None), |(a, b)| (a, Some(b)));
    if let Some(fraction) = fraction
        && (fraction.is_empty() || !fraction.bytes().all(|c| c == b'0'))
    {
        return None;
    }
    let bytes = whole.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut number = 0_i128;
    for (i, c) in bytes.iter().enumerate() {
        if c.is_ascii_digit() {
            number = number
                .saturating_mul(10)
                .saturating_add(i128::from(c - b'0'));
        } else if *c == b'_'
            && i > 0
            && i + 1 < bytes.len()
            && bytes[i - 1].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
        {
        } else {
            return None;
        }
    }
    Some(if negative { -number } else { number })
}

type DateQueryError = (&'static str, String, Option<Value>);
fn query_date(raw: &str) -> Result<NaiveDate, DateQueryError> {
    let invalid = |reason: &str| {
        (
            "date_from_datetime_parsing",
            format!("Input should be a valid date or datetime, {reason}"),
            Some(json!({"error":reason})),
        )
    };
    let inexact = || {
        (
            "date_from_datetime_inexact",
            "Datetimes provided to dates should have zero time - e.g. be exact dates".into(),
            None,
        )
    };
    if raw
        .bytes()
        .all(|c| c.is_ascii_digit() || c == b'.' || c == b'-' || c == b'+')
        && !raw.is_empty()
        && let Ok(number) = raw.parse::<f64>()
        && number.is_finite()
    {
        let seconds = if number.abs() > 20_000_000_000.0 {
            number / 1000.0
        } else {
            number
        };
        let micros = (seconds * 1_000_000.0).round();
        let whole = (micros / 1_000_000.0).floor();
        let nanos = ((micros - whole * 1_000_000.0) * 1_000.0).round() as u32;
        if let Some(dt) = DateTime::<Utc>::from_timestamp(whole as i64, nanos) {
            if dt.hour() == 0
                && dt.minute() == 0
                && dt.second() == 0
                && dt.nanosecond() == 0
                && dt.year() >= 1
                && dt.year() <= 9999
            {
                return Ok(dt.date_naive());
            }
            return Err(inexact());
        }
    }
    if raw.len() < 10 {
        return Err(invalid("input is too short"));
    }
    let b = raw.as_bytes();
    if !b[..4].iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid character in year"));
    }
    if b[4] != b'-' {
        return Err(invalid("invalid date separator, expected `-`"));
    }
    if !b[5..7].iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid character in month"));
    }
    let month = raw[5..7].parse::<u32>().unwrap_or(0);
    if !(1..=12).contains(&month) {
        return Err(invalid("month value is outside expected range of 1-12"));
    }
    if b[7] != b'-' {
        return Err(invalid("invalid date separator, expected `-`"));
    }
    if !b[8..10].iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid character in day"));
    }
    let date = NaiveDate::parse_from_str(&raw[..10], "%Y-%m-%d")
        .map_err(|_| invalid("day value is outside expected range"))?;
    if date.year() == 0 {
        return Err((
            "date_parsing",
            "Input should be a valid date in the format YYYY-MM-DD, year 0 is out of range".into(),
            Some(json!({"error":"year 0 is out of range"})),
        ));
    }
    if raw.len() == 10 {
        return Ok(date);
    }
    if !b"Tt _".contains(&b[10]) {
        return Err(invalid(
            "invalid datetime separator, expected `T`, `t`, `_` or space",
        ));
    }
    if raw.len() < 16 {
        return Err(invalid("input is too short"));
    }
    if !b[11..13].iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid character in hour"));
    }
    let hour = raw[11..13].parse::<u32>().unwrap_or(99);
    if hour > 23 {
        return Err(invalid("hour value is outside expected range of 0-23"));
    }
    if b[13] != b':' {
        return Err(invalid("invalid time separator, expected `:`"));
    }
    if !b[14..16].iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid character in minute"));
    }
    let minute = raw[14..16].parse::<u32>().unwrap_or(99);
    if minute > 59 {
        return Err(invalid("minute value is outside expected range of 0-59"));
    }
    let mut cursor = 16;
    let mut second = 0;
    let mut micros = 0;
    if b.get(cursor) == Some(&b':') {
        if raw.len() < 19 {
            return Err(invalid("input is too short"));
        }
        if !b[17..19].iter().all(u8::is_ascii_digit) {
            return Err(invalid("invalid character in second"));
        }
        second = raw[17..19].parse::<u32>().unwrap_or(99);
        if second > 59 {
            return Err(invalid("second value is outside expected range of 0-59"));
        }
        cursor = 19;
        if matches!(b.get(cursor), Some(b'.' | b',')) {
            cursor += 1;
            let begin = cursor;
            while b.get(cursor).is_some_and(u8::is_ascii_digit) {
                if cursor - begin < 6 {
                    micros = micros * 10 + u32::from(b[cursor] - b'0');
                }
                cursor += 1;
            }
            if cursor == begin {
                return Err(invalid("second fraction digits missing after `.`"));
            }
        }
    }
    if let Some(sign) = b.get(cursor) {
        if matches!(sign, b'Z' | b'z') {
            cursor += 1;
        } else if matches!(sign, b'+' | b'-') {
            cursor += 1;
            if b.get(cursor..cursor + 2)
                .is_none_or(|v| !v.iter().all(u8::is_ascii_digit))
            {
                return Err(invalid("invalid timezone hour"));
            }
            let zone_hour = u32::from(b[cursor] - b'0') * 10 + u32::from(b[cursor + 1] - b'0');
            cursor += 2;
            if b.get(cursor) == Some(&b':') {
                cursor += 1;
            }
            if b.get(cursor..cursor + 2)
                .is_none_or(|v| !v.iter().all(u8::is_ascii_digit))
            {
                return Err(invalid("invalid timezone minute"));
            }
            let zone_minute = u32::from(b[cursor] - b'0') * 10 + u32::from(b[cursor + 1] - b'0');
            cursor += 2;
            if zone_minute > 59 {
                return Err(invalid(
                    "timezone minute value is outside expected range of 0-59",
                ));
            }
            if zone_hour >= 24 {
                return Err(invalid("timezone offset must be less than 24 hours"));
            }
        }
    }
    if cursor != raw.len() {
        return Err(invalid(
            "unexpected extra characters at the end of the input",
        ));
    }
    if hour != 0 || minute != 0 || second != 0 || micros != 0 {
        return Err(inexact());
    }
    Ok(date)
}
fn truthy(v: &Value) -> bool {
    crate::service_registry::python_truthy(v)
}
fn copy_filters(base: &mut Value, q: &HashMap<String, String>, fields: &[&str]) {
    for field in fields {
        base[*field] = json!(q.get(*field));
    }
}
fn merge(base: &mut Value, extra: Value) -> Result<(), UsageAdminError> {
    let base = base.as_object_mut().ok_or(UsageAdminError::Internal)?;
    let extra = extra.as_object().ok_or(UsageAdminError::Internal)?;
    if extra.keys().any(|key| base.contains_key(key)) {
        return Err(UsageAdminError::Internal);
    }
    base.extend(extra.clone());
    Ok(())
}
fn round(n: f64, places: usize) -> f64 {
    format!("{n:.places$}").parse().unwrap_or(n)
}
pub(crate) fn integer(v: &Value, coerce_fraction: bool) -> Result<i64, UsageAdminError> {
    if let Some(n) = v.as_i64() {
        return Ok(n);
    }
    if let Some(n) = v.as_bool() {
        return Ok(i64::from(n));
    }
    if let Some(text) = v.as_str() {
        if !coerce_fraction {
            return parse_integer_text(text)
                .and_then(|v| i64::try_from(v).ok())
                .ok_or(UsageAdminError::Internal);
        }
        let d = Decimal::from_str_exact(text.trim_matches(python_whitespace))
            .map_err(|_| UsageAdminError::Internal)?;
        if !coerce_fraction && !d.fract().is_zero() {
            return Err(UsageAdminError::Internal);
        }
        return d.trunc().to_i64().ok_or(UsageAdminError::Internal);
    }
    if let Some(n) = v.as_f64() {
        if !n.is_finite()
            || n < i64::MIN as f64
            || n >= 9_223_372_036_854_775_808.0
            || (!coerce_fraction && n.fract() != 0.0)
        {
            return Err(UsageAdminError::Internal);
        }
        return Ok(n as i64);
    }
    Err(UsageAdminError::Internal)
}
pub(crate) fn number(v: &Value) -> Result<f64, UsageAdminError> {
    let n = if let Some(v) = v.as_f64() {
        v
    } else if let Some(v) = v.as_bool() {
        f64::from(u8::from(v))
    } else if let Some(v) = v.as_str() {
        v.trim_matches(python_whitespace)
            .parse()
            .map_err(|_| UsageAdminError::Internal)?
    } else {
        return Err(UsageAdminError::Internal);
    };
    if n.is_finite() {
        Ok(n)
    } else {
        Err(UsageAdminError::Internal)
    }
}
fn sum_int(rows: &Value, key: &str) -> Result<i64, UsageAdminError> {
    rows.as_array()
        .ok_or(UsageAdminError::Internal)?
        .iter()
        .try_fold(0_i64, |sum, row| {
            sum.checked_add(integer(row.get(key).unwrap_or(&json!(0)), false)?)
                .ok_or(UsageAdminError::Internal)
        })
}
fn sum_float(rows: &Value, key: &str) -> Result<f64, UsageAdminError> {
    let values = rows
        .as_array()
        .ok_or(UsageAdminError::Internal)?
        .iter()
        .map(|row| {
            if !row.is_object() {
                return Err(UsageAdminError::Internal);
            }
            number(row.get(key).unwrap_or(&json!(0)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(python_float_sum(&values))
}

fn parse_timestamp(text: &str) -> Result<DateTime<chrono::FixedOffset>, UsageAdminError> {
    match ProtocolTimestamp::parse_isoformat(text).map_err(|_| UsageAdminError::Internal)? {
        ProtocolTimestamp::Offset(v) => Ok(v),
        ProtocolTimestamp::Naive(v) => Ok(v.and_utc().fixed_offset()),
    }
}
fn naive_iso(v: NaiveDateTime) -> String {
    ProtocolTimestamp::from(v).to_isoformat()
}
#[derive(Clone, Copy)]
enum Schema {
    Summary,
    Series,
    Latency,
    Failure,
    Trace,
}
fn project_list(value: &Value, schema: Schema) -> Result<Value, UsageAdminError> {
    Ok(Value::Array(
        value
            .as_array()
            .ok_or(UsageAdminError::Internal)?
            .iter()
            .map(|v| project(v, schema))
            .collect::<Result<_, _>>()?,
    ))
}
fn object_list(value: &Value) -> Result<Value, UsageAdminError> {
    if value
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_object))
    {
        Ok(value.clone())
    } else {
        Err(UsageAdminError::Internal)
    }
}
fn object_list_for(value: &Value, field: &str) -> Result<Value, UsageAdminError> {
    let Some(rows) = value.as_array() else {
        return Err(UsageAdminError::ResponseValidation(vec![response_error(
            field,
            "list_type",
            "Input should be a valid list",
        )]));
    };
    let errors: Vec<Value> = rows
        .iter()
        .enumerate()
        .filter(|(_, v)| !v.is_object())
        .map(|(i, _)| {
            response_error(
                &format!("{field}.{i}"),
                "dict_type",
                "Input should be a valid dictionary",
            )
        })
        .collect();
    if errors.is_empty() {
        Ok(value.clone())
    } else {
        Err(UsageAdminError::ResponseValidation(errors))
    }
}
fn response_error(field: &str, kind: &str, message: &str) -> Value {
    json!({"field":field,"message":message,"type":kind})
}
fn project(value: &Value, schema: Schema) -> Result<Value, UsageAdminError> {
    // Endpoint **row expansion itself fails generically for non-mappings.
    let map = value.as_object().ok_or(UsageAdminError::Internal)?;
    let fields: &[(&str, char)] = match schema {
        Schema::Summary => &[
            ("total_requests", 'i'),
            ("success_rate", 'F'),
            ("total_input_tokens", 'i'),
            ("total_output_tokens", 'i'),
            ("total_tokens", 'i'),
            ("total_cost_usd", 'f'),
            ("avg_latency_ms", 'i'),
            ("start_date", 's'),
            ("end_date", 's'),
            ("data_status", 's'),
            ("data_freshness_minutes", 'i'),
            ("last_ingested_at", 'S'),
        ],
        Schema::Series => &[
            ("date", 's'),
            ("requests", 'i'),
            ("input_tokens", 'i'),
            ("output_tokens", 'i'),
            ("total_tokens", 'i'),
            ("cost_usd", 'f'),
            ("avg_latency_ms", 'i'),
        ],
        Schema::Latency => &[
            ("date", 's'),
            ("request_count", 'i'),
            ("avg_total_ms", 'i'),
            ("p50_total_ms", 'i'),
            ("p95_total_ms", 'i'),
            ("p99_total_ms", 'i'),
            ("avg_first_token_ms", 'i'),
            ("avg_llm_inference_ms", 'i'),
            ("avg_retrieval_ms", 'i'),
            ("avg_tool_call_ms", 'i'),
            ("avg_overhead_ms", 'i'),
        ],
        Schema::Failure => &[
            ("dimension", 's'),
            ("dimension_value", 's'),
            ("error_type", 's'),
            ("failure_count", 'i'),
            ("success_count", 'i'),
            ("total_count", 'i'),
            ("success_rate", 'f'),
        ],
        Schema::Trace => &[
            ("trace_id", 's'),
            ("request_id", 's'),
            ("tenant_id", 's'),
            ("user_id", 's'),
            ("service_id", 's'),
            ("assistant_id", 's'),
            ("provider", 's'),
            ("model", 's'),
            ("status", 's'),
            ("error_type", 'S'),
            ("request_total_duration_ms", 'i'),
            ("first_token_latency_ms", 'i'),
            ("llm_inference_duration_ms", 'i'),
            ("retrieval_duration_ms", 'i'),
            ("tool_call_duration_ms", 'i'),
            ("agent_or_graph_overhead_ms", 'i'),
            ("input_tokens", 'i'),
            ("output_tokens", 'i'),
            ("total_cost_usd", 'f'),
            ("sample_reason", 's'),
            ("trace_steps", 'l'),
            ("metadata", 'd'),
            ("timestamp", 'S'),
        ],
    };
    let mut out = Map::new();
    let mut errors = vec![];
    for &(field, kind) in fields {
        let Some(v) = map.get(field) else {
            if kind == 'S' {
                out.insert(field.into(), Value::Null);
            } else {
                errors.push(response_error(field, "missing", "Field required"));
            }
            continue;
        };
        if v.is_null() && matches!(kind, 'F' | 'S') {
            out.insert(field.into(), Value::Null);
            continue;
        }
        let parsed = match kind {
            's' | 'S' => v
                .as_str()
                .map(|v| json!(v))
                .ok_or(("string_type", "Input should be a valid string")),
            'i' => integer(v, false).map(|v| json!(v)).map_err(|_| {
                if v.is_string() {
                    (
                        "int_parsing",
                        "Input should be a valid integer, unable to parse string as an integer",
                    )
                } else if v.is_number() {
                    (
                        "int_from_float",
                        "Input should be a valid integer, got a number with a fractional part",
                    )
                } else {
                    ("int_type", "Input should be a valid integer")
                }
            }),
            'f' | 'F' => number(v).map(|v| json!(v)).map_err(|_| {
                if v.is_string() {
                    (
                        "float_parsing",
                        "Input should be a valid number, unable to parse string as a number",
                    )
                } else {
                    ("float_type", "Input should be a valid number")
                }
            }),
            'd' => {
                if v.is_object() {
                    Ok(v.clone())
                } else {
                    Err(("dict_type", "Input should be a valid dictionary"))
                }
            }
            'l' => {
                if let Some(rows) = v.as_array() {
                    for (i, row) in rows.iter().enumerate() {
                        if !row.is_object() {
                            errors.push(response_error(
                                &format!("{field}.{i}"),
                                "dict_type",
                                "Input should be a valid dictionary",
                            ));
                        }
                    }
                    Ok(v.clone())
                } else {
                    Err(("list_type", "Input should be a valid list"))
                }
            }
            _ => unreachable!(),
        };
        match parsed {
            Ok(value) => {
                out.insert(field.into(), value);
            }
            Err((kind, message)) => errors.push(response_error(field, kind, message)),
        };
    }
    if errors.is_empty() {
        if let Some(metering) = map.get("metering").filter(|v| v.is_object()) {
            out.insert("metering".into(), metering.clone());
        }
        Ok(Value::Object(out))
    } else {
        Err(UsageAdminError::ResponseValidation(errors))
    }
}
/// Canonical gateway handler.py public error envelopes. No backend error text is
/// retained. Trace state is supplied by trusted host extensions, never query data.
pub fn exception_response(error: &UsageAdminError, trace: &Value) -> Option<(u16, Value)> {
    let (status, mut body) = match error {
        UsageAdminError::Http { status, detail } => {
            return Some((*status, json!({"detail":detail})));
        }
        UsageAdminError::Internal => (
            500,
            json!({"error":{"code":2000,"message":"Internal server error"}}),
        ),
        UsageAdminError::ResponseValidation(errors) => (
            400,
            json!({"error":{"code":1001,"message":"Request validation failed","details":{"errors":errors}}}),
        ),
        UsageAdminError::Cancelled => return None,
    };
    if truthy(trace) {
        body["error"]["trace_id"] = trace.clone();
    }
    Some((status, body))
}

fn in_unicode_ranges(c: char, ranges: &[(u32, u32)]) -> bool {
    let c = c as u32;
    ranges
        .binary_search_by(|&(start, end)| {
            if c < start {
                std::cmp::Ordering::Greater
            } else if c > end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}
fn title(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut previous_cased = false;
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let cased = in_unicode_ranges(c, PYTHON_CASED_RANGES);
        if cased {
            if !previous_cased {
                python_title_character(c, &mut out);
            } else if c == '\u{3a3}' {
                // Title decides whether to lowercase from the immediately prior
                // character's Cased property. Final_Sigma is a separate Unicode
                // context: scan BOTH directions while skipping Case_Ignorable.
                // E.g. modifier ʰ is Cased AND Case_Ignorable, so ʰΣ -> ʰσ.
                let prior_cased = chars[..i]
                    .iter()
                    .rev()
                    .copied()
                    .find(|&prior| !in_unicode_ranges(prior, PYTHON_CASE_IGNORABLE_RANGES))
                    .is_some_and(|prior| in_unicode_ranges(prior, PYTHON_CASED_RANGES));
                let next_cased = chars[i + 1..]
                    .iter()
                    .copied()
                    .find(|&next| !in_unicode_ranges(next, PYTHON_CASE_IGNORABLE_RANGES))
                    .is_some_and(|next| in_unicode_ranges(next, PYTHON_CASED_RANGES));
                out.push(if prior_cased && !next_cased {
                    '\u{3c2}'
                } else {
                    '\u{3c3}'
                });
            } else {
                out.extend(c.to_lowercase());
            }
        } else {
            out.push(c);
        }
        previous_cased = cased;
    }
    out
}

fn csv_row(out: &mut String, cells: &[Value]) {
    for (i, v) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let text = if v.is_null() {
            String::new()
        } else {
            python_value_to_string(v)
        };
        if text.contains([',', '"', '\r', '\n']) || (cells.len() == 1 && text.is_empty()) {
            out.push('"');
            out.push_str(&text.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(&text);
        }
    }
    out.push_str("\r\n");
}
fn export_csv(rows: &Value, breakdown: &Value, dimension: &str) -> Result<String, UsageAdminError> {
    let mut out = String::new();
    csv_row(&mut out, &[json!("=== Daily Usage ===")]);
    csv_row(
        &mut out,
        &[
            "Date",
            "Requests",
            "Input Tokens",
            "Output Tokens",
            "Total Tokens",
            "Cost (USD)",
            "Avg Latency (ms)",
        ]
        .map(Value::from),
    );
    for row in rows.as_array().ok_or(UsageAdminError::Internal)? {
        let cells = [
            "date",
            "requests",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cost_usd",
            "avg_latency_ms",
        ]
        .iter()
        .map(|k| row.get(*k).cloned().ok_or(UsageAdminError::Internal))
        .collect::<Result<Vec<_>, _>>()?;
        csv_row(&mut out, &cells);
    }
    csv_row(&mut out, &[]);
    csv_row(
        &mut out,
        &[json!(format!("=== Breakdown by {} ===", title(dimension)))],
    );
    csv_row(
        &mut out,
        &[
            title(dimension),
            "Requests".into(),
            "Input Tokens".into(),
            "Output Tokens".into(),
            "Total Tokens".into(),
            "Cost (USD)".into(),
            "Percentage".into(),
        ]
        .map(Value::from),
    );
    for row in breakdown.as_array().ok_or(UsageAdminError::Internal)? {
        let mut cells = vec![row.get(dimension).cloned().unwrap_or(json!("Unknown"))];
        for key in [
            "requests",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cost_usd",
            "percentage",
        ] {
            cells.push(row.get(key).cloned().unwrap_or(json!(0)));
        }
        csv_row(&mut out, &cells);
    }
    Ok(out)
}

pub type UsageCancellationFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;
pub trait UsageAdminCancellation: Send + Sync {
    fn cancelled(&self) -> UsageCancellationFuture<'_>;
}
#[derive(Clone)]
struct HttpState {
    api: Arc<UsageAdminApi>,
    cancellation: Arc<dyn UsageAdminCancellation>,
}
pub fn router(api: Arc<UsageAdminApi>, cancellation: Arc<dyn UsageAdminCancellation>) -> Router {
    Router::new()
        .route("/api/v1/usage/summary", get(http_summary))
        .route("/api/v1/usage/breakdown", get(http_breakdown))
        .route("/api/v1/usage/timeseries", get(http_timeseries))
        .route("/api/v1/usage/performance-breakdown", get(http_performance))
        .route("/api/v1/usage/failure-breakdown", get(http_failure))
        .route("/api/v1/usage/traces", get(http_traces))
        // `any` suppresses Axum's automatic Allow union on encoded-slash 404s.
        // Explicit HEAD rejects before the GET extractors or any collaborators.
        .route(
            "/api/v1/usage/traces/{request_id}",
            any(method_not_allowed)
                .get(http_trace)
                .head(method_not_allowed),
        )
        .route(
            "/api/v1/usage/user/{user_id}",
            any(method_not_allowed)
                .get(http_user)
                .head(method_not_allowed),
        )
        .route("/api/v1/usage/export", get(http_export))
        .route("/api/v1/usage/models", get(http_models))
        .fallback(http_fallback)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(HttpState { api, cancellation })
}
async fn respond(
    state: HttpState,
    operation: UsageAdminOperation,
    query: HashMap<String, String>,
    request: Request,
) -> Response {
    if matches!(&operation,UsageAdminOperation::Trace(id)|UsageAdminOperation::User(id) if id.contains('/'))
    {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    if request.method() == axum::http::Method::HEAD {
        return method_not_allowed(request).await;
    }
    let context = request
        .extensions()
        .get::<UsageRequestContext>()
        .cloned()
        .unwrap_or_default();
    match state
        .api
        .handle(operation, request.headers(), &query, &context)
        .await
    {
        Ok(UsageAdminOutput::Json(value)) => Json(value).into_response(),
        Ok(UsageAdminOutput::Csv { content, filename }) => {
            let mut response = content.into_response();
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("text/csv; charset=utf-8"),
            );
            if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename={filename}")) {
                response.headers_mut().insert("content-disposition", value);
            }
            response
        }
        Err(UsageAdminError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let trace = context.exception_trace(request.headers());
            let (status, body) =
                exception_response(&error, &trace).expect("non-cancellation error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
async fn method_not_allowed(request: Request) -> Response {
    if request
        .uri()
        .path()
        .strip_prefix("/api/v1/usage/")
        .and_then(|p| {
            p.strip_prefix("traces/")
                .or_else(|| p.strip_prefix("user/"))
        })
        .is_some_and(|id| id.to_ascii_lowercase().contains("%2f"))
    {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let mut response = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"detail":"Method Not Allowed"})),
    )
        .into_response();
    response
        .headers_mut()
        .insert("allow", HeaderValue::from_static("GET"));
    response
}
// Source Starlette trailing-slash 307 redirects require host assembly with a
// verified effective origin. The bare factory never constructs an origin from
// Host, forwarding headers, an attacker-supplied absolute URI, or localhost.
async fn http_fallback() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
}

macro_rules! endpoint {
    ($name:ident,$op:ident) => {
        async fn $name(
            State(state): State<HttpState>,
            Query(query): Query<HashMap<String, String>>,
            request: Request,
        ) -> Response {
            respond(state, UsageAdminOperation::$op, query, request).await
        }
    };
}
endpoint!(http_summary, Summary);
endpoint!(http_breakdown, Breakdown);
endpoint!(http_timeseries, Timeseries);
endpoint!(http_performance, Performance);
endpoint!(http_failure, Failure);
endpoint!(http_traces, Traces);
endpoint!(http_export, Export);
endpoint!(http_models, Models);
async fn http_trace(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    respond(state, UsageAdminOperation::Trace(id), query, request).await
}
async fn http_user(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    respond(state, UsageAdminOperation::User(id), query, request).await
}

/// Read-only SQL command. All values are positional parameters; dimensions and
/// granularity select only the canonical fixed expressions below.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageSqlPlan {
    pub sql: String,
    pub parameters: Vec<UsageSqlParameter>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum UsageSqlParameter {
    Text(String),
    Date(NaiveDate),
    Integer(i64),
}
pub trait UsageAnalyticsDatabase: Send + Sync {
    fn fetch<'a>(&'a self, plan: UsageSqlPlan) -> UsageAdminFuture<'a, Vec<Value>>;
}
pub struct PgUsageAnalyticsDatabase {
    pool: PgPool,
}
impl PgUsageAnalyticsDatabase {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
impl UsageAnalyticsDatabase for PgUsageAnalyticsDatabase {
    fn fetch<'a>(&'a self, plan: UsageSqlPlan) -> UsageAdminFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let mut connection = self
                .pool
                .acquire()
                .await
                .map_err(|_| UsageAdminError::Internal)?;
            let sql = if plan.sql.contains("usage_records") {
                ai_platform_service_primitives::data_connection::append_store::pruning_sql(
                    &mut connection,
                    "gateway.usage_records",
                    &plan.sql,
                )
                .await
                .map_err(|_| UsageAdminError::Internal)?
            } else {
                plan.sql.clone()
            };
            let mut query = sqlx::query(&sql);
            for p in plan.parameters {
                query = match p {
                    UsageSqlParameter::Text(v) => query.bind(v),
                    UsageSqlParameter::Date(v) => query.bind(v),
                    UsageSqlParameter::Integer(v) => query.bind(v),
                };
            }
            let rows = query
                .fetch_all(&mut *connection)
                .await
                .map_err(|_| UsageAdminError::Internal)?;
            rows.iter()
                .map(|row| {
                    let mut value = Map::new();
                    for column in row.columns() {
                        let index = column.ordinal();
                        let cell = row
                            .try_get_raw(index)
                            .map_err(|_| UsageAdminError::Internal)?;
                        let v = if cell.is_null() {
                            Value::Null
                        } else {
                            match column.type_info().name() {
                                "INT8" => json!(
                                    row.try_get::<i64, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                                "INT4" => json!(
                                    row.try_get::<i32, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                                "INT2" => json!(
                                    row.try_get::<i16, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                                "NUMERIC" => json!(
                                    row.try_get::<Decimal, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                        .to_string()
                                ),
                                "FLOAT8" => json!(
                                    row.try_get::<f64, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                                "FLOAT4" => json!(
                                    row.try_get::<f32, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                                "TIMESTAMPTZ" => json!(
                                    ProtocolTimestamp::from(
                                        row.try_get::<DateTime<Utc>, _>(index)
                                            .map_err(|_| UsageAdminError::Internal)?
                                            .fixed_offset()
                                    )
                                    .to_isoformat()
                                ),
                                "TIMESTAMP" => json!(naive_iso(
                                    row.try_get::<NaiveDateTime, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                )),
                                "JSON" | "JSONB" => row
                                    .try_get::<Value, _>(index)
                                    .map_err(|_| UsageAdminError::Internal)?,
                                "BOOL" => json!(
                                    row.try_get::<bool, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                                "UUID" => json!(
                                    uuid::Uuid::from_slice(
                                        cell.as_bytes().map_err(|_| UsageAdminError::Internal)?
                                    )
                                    .map_err(|_| UsageAdminError::Internal)?
                                    .to_string()
                                ),
                                // Other canonical text columns decode without lossy casts.
                                _ => json!(
                                    row.try_get::<String, _>(index)
                                        .map_err(|_| UsageAdminError::Internal)?
                                ),
                            }
                        };
                        value.insert(column.name().into(), v);
                    }
                    Ok(Value::Object(value))
                })
                .collect()
        })
    }
}
fn required_text<'a>(args: &'a Value, key: &str) -> Result<&'a str, UsageAdminError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or(UsageAdminError::Internal)
}
fn filter(args: &Value, today: NaiveDate) -> Result<UsageFilter, UsageAdminError> {
    let date = |name: &str| -> Result<NaiveDate, UsageAdminError> {
        match args.get(name).filter(|v| !v.is_null()) {
            None => Ok(today),
            Some(v) => {
                NaiveDate::parse_from_str(v.as_str().ok_or(UsageAdminError::Internal)?, "%Y-%m-%d")
                    .map_err(|_| UsageAdminError::Internal)
            }
        }
    };
    let mut f = UsageFilter::new(
        required_text(args, "tenant_id")?,
        date("start_date")?,
        date("end_date")?,
    );
    f.user_id = args["user_id"].as_str().map(str::to_owned);
    f.model = args["model"].as_str().map(str::to_owned);
    f.service_id = args["service_id"].as_str().map(str::to_owned);
    f.assistant_id = args["assistant_id"].as_str().map(str::to_owned);
    f.provider = args["provider"].as_str().map(str::to_owned);
    Ok(f)
}
/// Existing pools only. None models the source recorder's absent database/pool.
/// PgBillingStore remains the sole implementation of existing billing readers.
pub struct NativeUsageAdminRecorder {
    store: Option<PgBillingStore>,
    analytics: Option<Arc<dyn UsageAnalyticsDatabase>>,
    clock: Arc<dyn UsageAdminClock>,
}
impl NativeUsageAdminRecorder {
    pub fn new(store: PgBillingStore, clock: Arc<dyn UsageAdminClock>) -> Self {
        let analytics = Arc::new(PgUsageAnalyticsDatabase::new(store.pool().clone()));
        Self {
            store: Some(store),
            analytics: Some(analytics),
            clock,
        }
    }
    pub fn unavailable(clock: Arc<dyn UsageAdminClock>) -> Self {
        Self {
            store: None,
            analytics: None,
            clock,
        }
    }
}
impl UsageAdminRecorder for NativeUsageAdminRecorder {
    fn call<'a>(&'a self, op: UsageReadOperation, args: Value) -> UsageAdminFuture<'a, Value> {
        Box::pin(async move {
            let Some(store) = &self.store else {
                return Ok(match op {
                    UsageReadOperation::Summary => {
                        billing_store::empty_summary(self.clock.today(), "unavailable")
                    }
                    UsageReadOperation::Freshness | UsageReadOperation::Trace => Value::Null,
                    _ => json!([]),
                });
            };
            let f = filter(&args, self.clock.today())?;
            let result = match op {
                UsageReadOperation::Summary => {
                    store.try_usage_summary(&f).await.unwrap_or_else(|_| {
                        billing_store::empty_summary(self.clock.today(), "collection_error")
                    })
                }
                UsageReadOperation::Breakdown => json!(
                    store
                        .get_usage_breakdown(
                            &f,
                            args["dimension"].as_str().unwrap_or("model"),
                            args["limit"].as_i64().unwrap_or(20)
                        )
                        .await
                ),
                UsageReadOperation::Timeseries => json!(
                    store
                        .get_usage_timeseries(&f, args["granularity"].as_str().unwrap_or("day"))
                        .await
                ),
                UsageReadOperation::Freshness => store
                    .get_last_ingested_at(
                        &f.tenant_id,
                        args.get("start_date")
                            .filter(|v| !v.is_null())
                            .map(|_| f.start_date),
                        args.get("end_date")
                            .filter(|v| !v.is_null())
                            .map(|_| f.end_date),
                        Granularity::from_python(args["granularity"].as_str().unwrap_or("day")),
                    )
                    .await
                    .map_err(|_| UsageAdminError::Internal)?
                    .map(|v| json!(ProtocolTimestamp::from(v.fixed_offset()).to_isoformat()))
                    .unwrap_or(Value::Null),
                _ => {
                    return read_analytics(
                        self.analytics.as_deref().ok_or(UsageAdminError::Internal)?,
                        op,
                        &args,
                        self.clock.today(),
                    )
                    .await;
                }
            };
            Ok(result)
        })
    }
}
fn conditions(
    args: &Value,
    today: NaiveDate,
    keys: &[&str],
) -> Result<(Vec<String>, Vec<UsageSqlParameter>), UsageAdminError> {
    let f = filter(args, today)?;
    let mut clauses = vec![
        "tenant_id = $1".into(),
        "created_at::date >= $2".into(),
        "created_at::date <= $3".into(),
    ];
    let mut params = vec![
        UsageSqlParameter::Text(f.tenant_id),
        UsageSqlParameter::Date(f.start_date),
        UsageSqlParameter::Date(f.end_date),
    ];
    for key in keys {
        if let Some(text) = args[*key].as_str().filter(|v| !v.is_empty()) {
            clauses.push(format!("{key} = ${}", params.len() + 1));
            params.push(UsageSqlParameter::Text(text.into()));
        }
    }
    Ok((clauses, params))
}
/// Exact SQL structure/bind ordering from the missing canonical recorder methods.
/// Whitespace alone is normalized by the offline AST oracle.
pub fn analytics_plan(
    op: UsageReadOperation,
    args: &Value,
    today: NaiveDate,
) -> Result<Option<UsageSqlPlan>, UsageAdminError> {
    if op == UsageReadOperation::Trace {
        return Ok(Some(UsageSqlPlan{sql:"SELECT * FROM request_traces WHERE tenant_id = $1 AND request_id = $2 ORDER BY created_at DESC LIMIT 1".into(),parameters:vec![UsageSqlParameter::Text(required_text(args,"tenant_id")?.into()),UsageSqlParameter::Text(required_text(args,"request_id")?.into())]}));
    }
    let keys: &[&str] = if op == UsageReadOperation::Traces {
        &[
            "service_id",
            "user_id",
            "provider",
            "model",
            "status",
            "error_type",
        ]
    } else {
        &["user_id", "model", "service_id", "provider"]
    };
    let (clauses, mut parameters) = conditions(args, today, keys)?;
    let where_clause = clauses.join(" AND ");
    let sql = match op {
        UsageReadOperation::Latency => {
            let unit = if args["granularity"] == "hour" {
                "hour"
            } else {
                "day"
            };
            format!(
                "SELECT date_trunc('{unit}', created_at) AS bucket, COUNT(*) AS request_count, COALESCE( AVG(COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)), 0 ) AS avg_total_ms, COALESCE( PERCENTILE_CONT(0.50) WITHIN GROUP ( ORDER BY COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0) ), 0 ) AS p50_total_ms, COALESCE( PERCENTILE_CONT(0.95) WITHIN GROUP ( ORDER BY COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0) ), 0 ) AS p95_total_ms, COALESCE( PERCENTILE_CONT(0.99) WITHIN GROUP ( ORDER BY COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0) ), 0 ) AS p99_total_ms, COALESCE(AVG(first_token_ms), 0) AS avg_first_token_ms, COALESCE(AVG(llm_inference_duration_ms), 0) AS avg_llm_inference_ms, COALESCE(AVG(retrieval_duration_ms), 0) AS avg_retrieval_ms, COALESCE(AVG(tool_call_duration_ms), 0) AS avg_tool_call_ms, COALESCE(AVG(agent_or_graph_overhead_ms), 0) AS avg_overhead_ms FROM usage_records WHERE {where_clause} GROUP BY bucket ORDER BY bucket"
            )
        }
        UsageReadOperation::Failure => {
            let expr = match args["dimension"].as_str().unwrap_or("service") {
                "service" => "COALESCE(NULLIF(service_id, ''), 'unattributed_service')",
                "model" => "COALESCE(NULLIF(model, ''), 'unattributed_model')",
                "provider" => "COALESCE(NULLIF(provider, ''), 'unattributed')",
                "user" => "COALESCE(NULLIF(user_id, ''), 'anonymous')",
                _ => return Ok(None),
            };
            let position = parameters.len() + 1;
            parameters.push(UsageSqlParameter::Integer(
                args["limit"].as_i64().unwrap_or(50),
            ));
            format!(
                "SELECT {expr} AS dimension_value, COALESCE(NULLIF(error_type, ''), 'unknown') AS error_type, COUNT(*) FILTER (WHERE status != 'success') AS failure_count, COUNT(*) FILTER (WHERE status = 'success') AS success_count, COUNT(*) AS total_count FROM usage_records WHERE {where_clause} GROUP BY dimension_value, error_type HAVING COUNT(*) FILTER (WHERE status != 'success') > 0 ORDER BY failure_count DESC, total_count DESC LIMIT ${position}"
            )
        }
        UsageReadOperation::Traces => {
            let position = parameters.len() + 1;
            parameters.push(UsageSqlParameter::Integer(
                args["limit"].as_i64().unwrap_or(100),
            ));
            format!(
                "SELECT * FROM request_traces WHERE {where_clause} ORDER BY created_at DESC LIMIT ${position}"
            )
        }
        _ => return Err(UsageAdminError::Internal),
    };
    Ok(Some(UsageSqlPlan { sql, parameters }))
}
/// Source broad Exception fallbacks exclude explicit cancellation (Python's
/// CancelledError is BaseException). Parsing failures use the same fallbacks.
pub async fn read_analytics(
    db: &dyn UsageAnalyticsDatabase,
    op: UsageReadOperation,
    args: &Value,
    today: NaiveDate,
) -> Result<Value, UsageAdminError> {
    let empty = || {
        if op == UsageReadOperation::Trace {
            Value::Null
        } else {
            json!([])
        }
    };
    let Some(plan) = analytics_plan(op, args, today)? else {
        return Ok(empty());
    };
    let rows = match db.fetch(plan).await {
        Ok(rows) => rows,
        Err(UsageAdminError::Cancelled) => return Err(UsageAdminError::Cancelled),
        Err(_) => return Ok(empty()),
    };
    let converted = (|| -> Result<Value, UsageAdminError> {
        if op == UsageReadOperation::Trace {
            return rows
                .first()
                .map(trace_row)
                .transpose()
                .map(|v| v.unwrap_or(Value::Null));
        }
        rows.iter().map(|row|match op {
            UsageReadOperation::Traces=>trace_row(row),
            UsageReadOperation::Latency=>{
                let mut out=json!({"date":required_text(row,"bucket")?});
                for key in ["request_count","avg_total_ms","p50_total_ms","p95_total_ms","p99_total_ms","avg_first_token_ms","avg_llm_inference_ms","avg_retrieval_ms","avg_tool_call_ms","avg_overhead_ms"] {let v=row.get(key).ok_or(UsageAdminError::Internal)?;out[key]=json!(if key!="request_count"&&!truthy(v){0}else{integer(v,true)?});}Ok(out)
            }
            UsageReadOperation::Failure=>{
                let count=integer(row.get("total_count").ok_or(UsageAdminError::Internal)?,true)?;let success=integer(row.get("success_count").ok_or(UsageAdminError::Internal)?,true)?;
                Ok(json!({"dimension":args["dimension"].as_str().unwrap_or("service"),"dimension_value":row.get("dimension_value").ok_or(UsageAdminError::Internal)?,"error_type":row.get("error_type").ok_or(UsageAdminError::Internal)?,"failure_count":integer(row.get("failure_count").ok_or(UsageAdminError::Internal)?,true)?,"success_count":success,"total_count":count,"success_rate":if count>0{round(integer_ratio(success,count)*100.0,2)}else{0.0}}))
            }
            _=>Err(UsageAdminError::Internal),
        }).collect::<Result<Vec<_>,_>>().map(Value::Array)
    })();
    Ok(converted.unwrap_or_else(|_| empty()))
}
fn trace_row(row: &Value) -> Result<Value, UsageAdminError> {
    if !truthy(row) {
        return Ok(json!({}));
    }
    let mut out = Map::new();
    out.insert(
        "trace_id".into(),
        json!(python_value_to_string(
            row.get("trace_id").ok_or(UsageAdminError::Internal)?
        )),
    );
    for key in [
        "request_id",
        "tenant_id",
        "user_id",
        "service_id",
        "assistant_id",
        "provider",
        "model",
        "status",
        "error_type",
        "sample_reason",
    ] {
        out.insert(
            key.into(),
            row.get(key).cloned().ok_or(UsageAdminError::Internal)?,
        );
    }
    for key in [
        "request_total_duration_ms",
        "first_token_latency_ms",
        "llm_inference_duration_ms",
        "retrieval_duration_ms",
        "tool_call_duration_ms",
        "agent_or_graph_overhead_ms",
        "input_tokens",
        "output_tokens",
    ] {
        let v = row.get(key).ok_or(UsageAdminError::Internal)?;
        out.insert(
            key.into(),
            json!(if truthy(v) { integer(v, true)? } else { 0 }),
        );
    }
    let cost = row
        .get("total_cost_cents")
        .ok_or(UsageAdminError::Internal)?;
    out.insert(
        "total_cost_usd".into(),
        json!(round(
            if truthy(cost) {
                integer_ratio(integer(cost, true)?, 1_000_000)
            } else {
                0.0
            },
            6
        )),
    );
    for (key, array) in [("trace_steps", true), ("metadata", false)] {
        let raw = row.get(key).ok_or(UsageAdminError::Internal)?;
        let decoded = if let Some(text) = raw.as_str() {
            serde_json::from_str::<Value>(text).ok()
        } else {
            Some(raw.clone())
        };
        out.insert(
            key.into(),
            decoded
                .filter(|v| if array { v.is_array() } else { v.is_object() })
                .unwrap_or_else(|| if array { json!([]) } else { json!({}) }),
        );
    }
    out.insert(
        "timestamp".into(),
        row.get("created_at")
            .cloned()
            .ok_or(UsageAdminError::Internal)?,
    );
    Ok(Value::Object(out))
}

// BEGIN GENERATED PYTHON CSV TITLE DATA
// Derived from CPython Unicode 15.0.0.
const PYTHON_CASED_RANGES: &[(u32, u32)] = &[
    (0x41, 0x5a),
    (0x61, 0x7a),
    (0xaa, 0xaa),
    (0xb5, 0xb5),
    (0xba, 0xba),
    (0xc0, 0xd6),
    (0xd8, 0xf6),
    (0xf8, 0x1ba),
    (0x1bc, 0x1bf),
    (0x1c4, 0x293),
    (0x295, 0x2b8),
    (0x2c0, 0x2c1),
    (0x2e0, 0x2e4),
    (0x345, 0x345),
    (0x370, 0x373),
    (0x376, 0x377),
    (0x37a, 0x37d),
    (0x37f, 0x37f),
    (0x386, 0x386),
    (0x388, 0x38a),
    (0x38c, 0x38c),
    (0x38e, 0x3a1),
    (0x3a3, 0x3f5),
    (0x3f7, 0x481),
    (0x48a, 0x52f),
    (0x531, 0x556),
    (0x560, 0x588),
    (0x10a0, 0x10c5),
    (0x10c7, 0x10c7),
    (0x10cd, 0x10cd),
    (0x10d0, 0x10fa),
    (0x10fc, 0x10ff),
    (0x13a0, 0x13f5),
    (0x13f8, 0x13fd),
    (0x1c80, 0x1c88),
    (0x1c90, 0x1cba),
    (0x1cbd, 0x1cbf),
    (0x1d00, 0x1dbf),
    (0x1e00, 0x1f15),
    (0x1f18, 0x1f1d),
    (0x1f20, 0x1f45),
    (0x1f48, 0x1f4d),
    (0x1f50, 0x1f57),
    (0x1f59, 0x1f59),
    (0x1f5b, 0x1f5b),
    (0x1f5d, 0x1f5d),
    (0x1f5f, 0x1f7d),
    (0x1f80, 0x1fb4),
    (0x1fb6, 0x1fbc),
    (0x1fbe, 0x1fbe),
    (0x1fc2, 0x1fc4),
    (0x1fc6, 0x1fcc),
    (0x1fd0, 0x1fd3),
    (0x1fd6, 0x1fdb),
    (0x1fe0, 0x1fec),
    (0x1ff2, 0x1ff4),
    (0x1ff6, 0x1ffc),
    (0x2071, 0x2071),
    (0x207f, 0x207f),
    (0x2090, 0x209c),
    (0x2102, 0x2102),
    (0x2107, 0x2107),
    (0x210a, 0x2113),
    (0x2115, 0x2115),
    (0x2119, 0x211d),
    (0x2124, 0x2124),
    (0x2126, 0x2126),
    (0x2128, 0x2128),
    (0x212a, 0x212d),
    (0x212f, 0x2134),
    (0x2139, 0x2139),
    (0x213c, 0x213f),
    (0x2145, 0x2149),
    (0x214e, 0x214e),
    (0x2160, 0x217f),
    (0x2183, 0x2184),
    (0x24b6, 0x24e9),
    (0x2c00, 0x2ce4),
    (0x2ceb, 0x2cee),
    (0x2cf2, 0x2cf3),
    (0x2d00, 0x2d25),
    (0x2d27, 0x2d27),
    (0x2d2d, 0x2d2d),
    (0xa640, 0xa66d),
    (0xa680, 0xa69d),
    (0xa722, 0xa787),
    (0xa78b, 0xa78e),
    (0xa790, 0xa7ca),
    (0xa7d0, 0xa7d1),
    (0xa7d3, 0xa7d3),
    (0xa7d5, 0xa7d9),
    (0xa7f2, 0xa7f6),
    (0xa7f8, 0xa7fa),
    (0xab30, 0xab5a),
    (0xab5c, 0xab69),
    (0xab70, 0xabbf),
    (0xfb00, 0xfb06),
    (0xfb13, 0xfb17),
    (0xff21, 0xff3a),
    (0xff41, 0xff5a),
    (0x10400, 0x1044f),
    (0x104b0, 0x104d3),
    (0x104d8, 0x104fb),
    (0x10570, 0x1057a),
    (0x1057c, 0x1058a),
    (0x1058c, 0x10592),
    (0x10594, 0x10595),
    (0x10597, 0x105a1),
    (0x105a3, 0x105b1),
    (0x105b3, 0x105b9),
    (0x105bb, 0x105bc),
    (0x10780, 0x10780),
    (0x10783, 0x10785),
    (0x10787, 0x107b0),
    (0x107b2, 0x107ba),
    (0x10c80, 0x10cb2),
    (0x10cc0, 0x10cf2),
    (0x118a0, 0x118df),
    (0x16e40, 0x16e7f),
    (0x1d400, 0x1d454),
    (0x1d456, 0x1d49c),
    (0x1d49e, 0x1d49f),
    (0x1d4a2, 0x1d4a2),
    (0x1d4a5, 0x1d4a6),
    (0x1d4a9, 0x1d4ac),
    (0x1d4ae, 0x1d4b9),
    (0x1d4bb, 0x1d4bb),
    (0x1d4bd, 0x1d4c3),
    (0x1d4c5, 0x1d505),
    (0x1d507, 0x1d50a),
    (0x1d50d, 0x1d514),
    (0x1d516, 0x1d51c),
    (0x1d51e, 0x1d539),
    (0x1d53b, 0x1d53e),
    (0x1d540, 0x1d544),
    (0x1d546, 0x1d546),
    (0x1d54a, 0x1d550),
    (0x1d552, 0x1d6a5),
    (0x1d6a8, 0x1d6c0),
    (0x1d6c2, 0x1d6da),
    (0x1d6dc, 0x1d6fa),
    (0x1d6fc, 0x1d714),
    (0x1d716, 0x1d734),
    (0x1d736, 0x1d74e),
    (0x1d750, 0x1d76e),
    (0x1d770, 0x1d788),
    (0x1d78a, 0x1d7a8),
    (0x1d7aa, 0x1d7c2),
    (0x1d7c4, 0x1d7cb),
    (0x1df00, 0x1df09),
    (0x1df0b, 0x1df1e),
    (0x1df25, 0x1df2a),
    (0x1e030, 0x1e06d),
    (0x1e900, 0x1e943),
    (0x1f130, 0x1f149),
    (0x1f150, 0x1f169),
    (0x1f170, 0x1f189),
];
const PYTHON_CASE_IGNORABLE_RANGES: &[(u32, u32)] = &[
    (0x27, 0x27),
    (0x2e, 0x2e),
    (0x3a, 0x3a),
    (0x5e, 0x5e),
    (0x60, 0x60),
    (0xa8, 0xa8),
    (0xad, 0xad),
    (0xaf, 0xaf),
    (0xb4, 0xb4),
    (0xb7, 0xb8),
    (0x2b0, 0x36f),
    (0x374, 0x375),
    (0x37a, 0x37a),
    (0x384, 0x385),
    (0x387, 0x387),
    (0x483, 0x489),
    (0x559, 0x559),
    (0x55f, 0x55f),
    (0x591, 0x5bd),
    (0x5bf, 0x5bf),
    (0x5c1, 0x5c2),
    (0x5c4, 0x5c5),
    (0x5c7, 0x5c7),
    (0x5f4, 0x5f4),
    (0x600, 0x605),
    (0x610, 0x61a),
    (0x61c, 0x61c),
    (0x640, 0x640),
    (0x64b, 0x65f),
    (0x670, 0x670),
    (0x6d6, 0x6dd),
    (0x6df, 0x6e8),
    (0x6ea, 0x6ed),
    (0x70f, 0x70f),
    (0x711, 0x711),
    (0x730, 0x74a),
    (0x7a6, 0x7b0),
    (0x7eb, 0x7f5),
    (0x7fa, 0x7fa),
    (0x7fd, 0x7fd),
    (0x816, 0x82d),
    (0x859, 0x85b),
    (0x888, 0x888),
    (0x890, 0x891),
    (0x898, 0x89f),
    (0x8c9, 0x902),
    (0x93a, 0x93a),
    (0x93c, 0x93c),
    (0x941, 0x948),
    (0x94d, 0x94d),
    (0x951, 0x957),
    (0x962, 0x963),
    (0x971, 0x971),
    (0x981, 0x981),
    (0x9bc, 0x9bc),
    (0x9c1, 0x9c4),
    (0x9cd, 0x9cd),
    (0x9e2, 0x9e3),
    (0x9fe, 0x9fe),
    (0xa01, 0xa02),
    (0xa3c, 0xa3c),
    (0xa41, 0xa42),
    (0xa47, 0xa48),
    (0xa4b, 0xa4d),
    (0xa51, 0xa51),
    (0xa70, 0xa71),
    (0xa75, 0xa75),
    (0xa81, 0xa82),
    (0xabc, 0xabc),
    (0xac1, 0xac5),
    (0xac7, 0xac8),
    (0xacd, 0xacd),
    (0xae2, 0xae3),
    (0xafa, 0xaff),
    (0xb01, 0xb01),
    (0xb3c, 0xb3c),
    (0xb3f, 0xb3f),
    (0xb41, 0xb44),
    (0xb4d, 0xb4d),
    (0xb55, 0xb56),
    (0xb62, 0xb63),
    (0xb82, 0xb82),
    (0xbc0, 0xbc0),
    (0xbcd, 0xbcd),
    (0xc00, 0xc00),
    (0xc04, 0xc04),
    (0xc3c, 0xc3c),
    (0xc3e, 0xc40),
    (0xc46, 0xc48),
    (0xc4a, 0xc4d),
    (0xc55, 0xc56),
    (0xc62, 0xc63),
    (0xc81, 0xc81),
    (0xcbc, 0xcbc),
    (0xcbf, 0xcbf),
    (0xcc6, 0xcc6),
    (0xccc, 0xccd),
    (0xce2, 0xce3),
    (0xd00, 0xd01),
    (0xd3b, 0xd3c),
    (0xd41, 0xd44),
    (0xd4d, 0xd4d),
    (0xd62, 0xd63),
    (0xd81, 0xd81),
    (0xdca, 0xdca),
    (0xdd2, 0xdd4),
    (0xdd6, 0xdd6),
    (0xe31, 0xe31),
    (0xe34, 0xe3a),
    (0xe46, 0xe4e),
    (0xeb1, 0xeb1),
    (0xeb4, 0xebc),
    (0xec6, 0xec6),
    (0xec8, 0xece),
    (0xf18, 0xf19),
    (0xf35, 0xf35),
    (0xf37, 0xf37),
    (0xf39, 0xf39),
    (0xf71, 0xf7e),
    (0xf80, 0xf84),
    (0xf86, 0xf87),
    (0xf8d, 0xf97),
    (0xf99, 0xfbc),
    (0xfc6, 0xfc6),
    (0x102d, 0x1030),
    (0x1032, 0x1037),
    (0x1039, 0x103a),
    (0x103d, 0x103e),
    (0x1058, 0x1059),
    (0x105e, 0x1060),
    (0x1071, 0x1074),
    (0x1082, 0x1082),
    (0x1085, 0x1086),
    (0x108d, 0x108d),
    (0x109d, 0x109d),
    (0x10fc, 0x10fc),
    (0x135d, 0x135f),
    (0x1712, 0x1714),
    (0x1732, 0x1733),
    (0x1752, 0x1753),
    (0x1772, 0x1773),
    (0x17b4, 0x17b5),
    (0x17b7, 0x17bd),
    (0x17c6, 0x17c6),
    (0x17c9, 0x17d3),
    (0x17d7, 0x17d7),
    (0x17dd, 0x17dd),
    (0x180b, 0x180f),
    (0x1843, 0x1843),
    (0x1885, 0x1886),
    (0x18a9, 0x18a9),
    (0x1920, 0x1922),
    (0x1927, 0x1928),
    (0x1932, 0x1932),
    (0x1939, 0x193b),
    (0x1a17, 0x1a18),
    (0x1a1b, 0x1a1b),
    (0x1a56, 0x1a56),
    (0x1a58, 0x1a5e),
    (0x1a60, 0x1a60),
    (0x1a62, 0x1a62),
    (0x1a65, 0x1a6c),
    (0x1a73, 0x1a7c),
    (0x1a7f, 0x1a7f),
    (0x1aa7, 0x1aa7),
    (0x1ab0, 0x1ace),
    (0x1b00, 0x1b03),
    (0x1b34, 0x1b34),
    (0x1b36, 0x1b3a),
    (0x1b3c, 0x1b3c),
    (0x1b42, 0x1b42),
    (0x1b6b, 0x1b73),
    (0x1b80, 0x1b81),
    (0x1ba2, 0x1ba5),
    (0x1ba8, 0x1ba9),
    (0x1bab, 0x1bad),
    (0x1be6, 0x1be6),
    (0x1be8, 0x1be9),
    (0x1bed, 0x1bed),
    (0x1bef, 0x1bf1),
    (0x1c2c, 0x1c33),
    (0x1c36, 0x1c37),
    (0x1c78, 0x1c7d),
    (0x1cd0, 0x1cd2),
    (0x1cd4, 0x1ce0),
    (0x1ce2, 0x1ce8),
    (0x1ced, 0x1ced),
    (0x1cf4, 0x1cf4),
    (0x1cf8, 0x1cf9),
    (0x1d2c, 0x1d6a),
    (0x1d78, 0x1d78),
    (0x1d9b, 0x1dff),
    (0x1fbd, 0x1fbd),
    (0x1fbf, 0x1fc1),
    (0x1fcd, 0x1fcf),
    (0x1fdd, 0x1fdf),
    (0x1fed, 0x1fef),
    (0x1ffd, 0x1ffe),
    (0x200b, 0x200f),
    (0x2018, 0x2019),
    (0x2024, 0x2024),
    (0x2027, 0x2027),
    (0x202a, 0x202e),
    (0x2060, 0x2064),
    (0x2066, 0x206f),
    (0x2071, 0x2071),
    (0x207f, 0x207f),
    (0x2090, 0x209c),
    (0x20d0, 0x20f0),
    (0x2c7c, 0x2c7d),
    (0x2cef, 0x2cf1),
    (0x2d6f, 0x2d6f),
    (0x2d7f, 0x2d7f),
    (0x2de0, 0x2dff),
    (0x2e2f, 0x2e2f),
    (0x3005, 0x3005),
    (0x302a, 0x302d),
    (0x3031, 0x3035),
    (0x303b, 0x303b),
    (0x3099, 0x309e),
    (0x30fc, 0x30fe),
    (0xa015, 0xa015),
    (0xa4f8, 0xa4fd),
    (0xa60c, 0xa60c),
    (0xa66f, 0xa672),
    (0xa674, 0xa67d),
    (0xa67f, 0xa67f),
    (0xa69c, 0xa69f),
    (0xa6f0, 0xa6f1),
    (0xa700, 0xa721),
    (0xa770, 0xa770),
    (0xa788, 0xa78a),
    (0xa7f2, 0xa7f4),
    (0xa7f8, 0xa7f9),
    (0xa802, 0xa802),
    (0xa806, 0xa806),
    (0xa80b, 0xa80b),
    (0xa825, 0xa826),
    (0xa82c, 0xa82c),
    (0xa8c4, 0xa8c5),
    (0xa8e0, 0xa8f1),
    (0xa8ff, 0xa8ff),
    (0xa926, 0xa92d),
    (0xa947, 0xa951),
    (0xa980, 0xa982),
    (0xa9b3, 0xa9b3),
    (0xa9b6, 0xa9b9),
    (0xa9bc, 0xa9bd),
    (0xa9cf, 0xa9cf),
    (0xa9e5, 0xa9e6),
    (0xaa29, 0xaa2e),
    (0xaa31, 0xaa32),
    (0xaa35, 0xaa36),
    (0xaa43, 0xaa43),
    (0xaa4c, 0xaa4c),
    (0xaa70, 0xaa70),
    (0xaa7c, 0xaa7c),
    (0xaab0, 0xaab0),
    (0xaab2, 0xaab4),
    (0xaab7, 0xaab8),
    (0xaabe, 0xaabf),
    (0xaac1, 0xaac1),
    (0xaadd, 0xaadd),
    (0xaaec, 0xaaed),
    (0xaaf3, 0xaaf4),
    (0xaaf6, 0xaaf6),
    (0xab5b, 0xab5f),
    (0xab69, 0xab6b),
    (0xabe5, 0xabe5),
    (0xabe8, 0xabe8),
    (0xabed, 0xabed),
    (0xfb1e, 0xfb1e),
    (0xfbb2, 0xfbc2),
    (0xfe00, 0xfe0f),
    (0xfe13, 0xfe13),
    (0xfe20, 0xfe2f),
    (0xfe52, 0xfe52),
    (0xfe55, 0xfe55),
    (0xfeff, 0xfeff),
    (0xff07, 0xff07),
    (0xff0e, 0xff0e),
    (0xff1a, 0xff1a),
    (0xff3e, 0xff3e),
    (0xff40, 0xff40),
    (0xff70, 0xff70),
    (0xff9e, 0xff9f),
    (0xffe3, 0xffe3),
    (0xfff9, 0xfffb),
    (0x101fd, 0x101fd),
    (0x102e0, 0x102e0),
    (0x10376, 0x1037a),
    (0x10780, 0x10785),
    (0x10787, 0x107b0),
    (0x107b2, 0x107ba),
    (0x10a01, 0x10a03),
    (0x10a05, 0x10a06),
    (0x10a0c, 0x10a0f),
    (0x10a38, 0x10a3a),
    (0x10a3f, 0x10a3f),
    (0x10ae5, 0x10ae6),
    (0x10d24, 0x10d27),
    (0x10eab, 0x10eac),
    (0x10efd, 0x10eff),
    (0x10f46, 0x10f50),
    (0x10f82, 0x10f85),
    (0x11001, 0x11001),
    (0x11038, 0x11046),
    (0x11070, 0x11070),
    (0x11073, 0x11074),
    (0x1107f, 0x11081),
    (0x110b3, 0x110b6),
    (0x110b9, 0x110ba),
    (0x110bd, 0x110bd),
    (0x110c2, 0x110c2),
    (0x110cd, 0x110cd),
    (0x11100, 0x11102),
    (0x11127, 0x1112b),
    (0x1112d, 0x11134),
    (0x11173, 0x11173),
    (0x11180, 0x11181),
    (0x111b6, 0x111be),
    (0x111c9, 0x111cc),
    (0x111cf, 0x111cf),
    (0x1122f, 0x11231),
    (0x11234, 0x11234),
    (0x11236, 0x11237),
    (0x1123e, 0x1123e),
    (0x11241, 0x11241),
    (0x112df, 0x112df),
    (0x112e3, 0x112ea),
    (0x11300, 0x11301),
    (0x1133b, 0x1133c),
    (0x11340, 0x11340),
    (0x11366, 0x1136c),
    (0x11370, 0x11374),
    (0x11438, 0x1143f),
    (0x11442, 0x11444),
    (0x11446, 0x11446),
    (0x1145e, 0x1145e),
    (0x114b3, 0x114b8),
    (0x114ba, 0x114ba),
    (0x114bf, 0x114c0),
    (0x114c2, 0x114c3),
    (0x115b2, 0x115b5),
    (0x115bc, 0x115bd),
    (0x115bf, 0x115c0),
    (0x115dc, 0x115dd),
    (0x11633, 0x1163a),
    (0x1163d, 0x1163d),
    (0x1163f, 0x11640),
    (0x116ab, 0x116ab),
    (0x116ad, 0x116ad),
    (0x116b0, 0x116b5),
    (0x116b7, 0x116b7),
    (0x1171d, 0x1171f),
    (0x11722, 0x11725),
    (0x11727, 0x1172b),
    (0x1182f, 0x11837),
    (0x11839, 0x1183a),
    (0x1193b, 0x1193c),
    (0x1193e, 0x1193e),
    (0x11943, 0x11943),
    (0x119d4, 0x119d7),
    (0x119da, 0x119db),
    (0x119e0, 0x119e0),
    (0x11a01, 0x11a0a),
    (0x11a33, 0x11a38),
    (0x11a3b, 0x11a3e),
    (0x11a47, 0x11a47),
    (0x11a51, 0x11a56),
    (0x11a59, 0x11a5b),
    (0x11a8a, 0x11a96),
    (0x11a98, 0x11a99),
    (0x11c30, 0x11c36),
    (0x11c38, 0x11c3d),
    (0x11c3f, 0x11c3f),
    (0x11c92, 0x11ca7),
    (0x11caa, 0x11cb0),
    (0x11cb2, 0x11cb3),
    (0x11cb5, 0x11cb6),
    (0x11d31, 0x11d36),
    (0x11d3a, 0x11d3a),
    (0x11d3c, 0x11d3d),
    (0x11d3f, 0x11d45),
    (0x11d47, 0x11d47),
    (0x11d90, 0x11d91),
    (0x11d95, 0x11d95),
    (0x11d97, 0x11d97),
    (0x11ef3, 0x11ef4),
    (0x11f00, 0x11f01),
    (0x11f36, 0x11f3a),
    (0x11f40, 0x11f40),
    (0x11f42, 0x11f42),
    (0x13430, 0x13440),
    (0x13447, 0x13455),
    (0x16af0, 0x16af4),
    (0x16b30, 0x16b36),
    (0x16b40, 0x16b43),
    (0x16f4f, 0x16f4f),
    (0x16f8f, 0x16f9f),
    (0x16fe0, 0x16fe1),
    (0x16fe3, 0x16fe4),
    (0x1aff0, 0x1aff3),
    (0x1aff5, 0x1affb),
    (0x1affd, 0x1affe),
    (0x1bc9d, 0x1bc9e),
    (0x1bca0, 0x1bca3),
    (0x1cf00, 0x1cf2d),
    (0x1cf30, 0x1cf46),
    (0x1d167, 0x1d169),
    (0x1d173, 0x1d182),
    (0x1d185, 0x1d18b),
    (0x1d1aa, 0x1d1ad),
    (0x1d242, 0x1d244),
    (0x1da00, 0x1da36),
    (0x1da3b, 0x1da6c),
    (0x1da75, 0x1da75),
    (0x1da84, 0x1da84),
    (0x1da9b, 0x1da9f),
    (0x1daa1, 0x1daaf),
    (0x1e000, 0x1e006),
    (0x1e008, 0x1e018),
    (0x1e01b, 0x1e021),
    (0x1e023, 0x1e024),
    (0x1e026, 0x1e02a),
    (0x1e030, 0x1e06d),
    (0x1e08f, 0x1e08f),
    (0x1e130, 0x1e13d),
    (0x1e2ae, 0x1e2ae),
    (0x1e2ec, 0x1e2ef),
    (0x1e4eb, 0x1e4ef),
    (0x1e8d0, 0x1e8d6),
    (0x1e944, 0x1e94b),
    (0x1f3fb, 0x1f3ff),
    (0xe0001, 0xe0001),
    (0xe0020, 0xe007f),
    (0xe0100, 0xe01ef),
];
fn python_title_character(c: char, out: &mut String) {
    match c {
        '\u{df}' => out.push_str("\u{53}\u{73}"),
        '\u{1c4}' => out.push('\u{1c5}'),
        '\u{1c5}' => out.push('\u{1c5}'),
        '\u{1c6}' => out.push('\u{1c5}'),
        '\u{1c7}' => out.push('\u{1c8}'),
        '\u{1c8}' => out.push('\u{1c8}'),
        '\u{1c9}' => out.push('\u{1c8}'),
        '\u{1ca}' => out.push('\u{1cb}'),
        '\u{1cb}' => out.push('\u{1cb}'),
        '\u{1cc}' => out.push('\u{1cb}'),
        '\u{1f1}' => out.push('\u{1f2}'),
        '\u{1f2}' => out.push('\u{1f2}'),
        '\u{1f3}' => out.push('\u{1f2}'),
        '\u{587}' => out.push_str("\u{535}\u{582}"),
        '\u{10d0}' => out.push('\u{10d0}'),
        '\u{10d1}' => out.push('\u{10d1}'),
        '\u{10d2}' => out.push('\u{10d2}'),
        '\u{10d3}' => out.push('\u{10d3}'),
        '\u{10d4}' => out.push('\u{10d4}'),
        '\u{10d5}' => out.push('\u{10d5}'),
        '\u{10d6}' => out.push('\u{10d6}'),
        '\u{10d7}' => out.push('\u{10d7}'),
        '\u{10d8}' => out.push('\u{10d8}'),
        '\u{10d9}' => out.push('\u{10d9}'),
        '\u{10da}' => out.push('\u{10da}'),
        '\u{10db}' => out.push('\u{10db}'),
        '\u{10dc}' => out.push('\u{10dc}'),
        '\u{10dd}' => out.push('\u{10dd}'),
        '\u{10de}' => out.push('\u{10de}'),
        '\u{10df}' => out.push('\u{10df}'),
        '\u{10e0}' => out.push('\u{10e0}'),
        '\u{10e1}' => out.push('\u{10e1}'),
        '\u{10e2}' => out.push('\u{10e2}'),
        '\u{10e3}' => out.push('\u{10e3}'),
        '\u{10e4}' => out.push('\u{10e4}'),
        '\u{10e5}' => out.push('\u{10e5}'),
        '\u{10e6}' => out.push('\u{10e6}'),
        '\u{10e7}' => out.push('\u{10e7}'),
        '\u{10e8}' => out.push('\u{10e8}'),
        '\u{10e9}' => out.push('\u{10e9}'),
        '\u{10ea}' => out.push('\u{10ea}'),
        '\u{10eb}' => out.push('\u{10eb}'),
        '\u{10ec}' => out.push('\u{10ec}'),
        '\u{10ed}' => out.push('\u{10ed}'),
        '\u{10ee}' => out.push('\u{10ee}'),
        '\u{10ef}' => out.push('\u{10ef}'),
        '\u{10f0}' => out.push('\u{10f0}'),
        '\u{10f1}' => out.push('\u{10f1}'),
        '\u{10f2}' => out.push('\u{10f2}'),
        '\u{10f3}' => out.push('\u{10f3}'),
        '\u{10f4}' => out.push('\u{10f4}'),
        '\u{10f5}' => out.push('\u{10f5}'),
        '\u{10f6}' => out.push('\u{10f6}'),
        '\u{10f7}' => out.push('\u{10f7}'),
        '\u{10f8}' => out.push('\u{10f8}'),
        '\u{10f9}' => out.push('\u{10f9}'),
        '\u{10fa}' => out.push('\u{10fa}'),
        '\u{10fd}' => out.push('\u{10fd}'),
        '\u{10fe}' => out.push('\u{10fe}'),
        '\u{10ff}' => out.push('\u{10ff}'),
        '\u{1f80}' => out.push('\u{1f88}'),
        '\u{1f81}' => out.push('\u{1f89}'),
        '\u{1f82}' => out.push('\u{1f8a}'),
        '\u{1f83}' => out.push('\u{1f8b}'),
        '\u{1f84}' => out.push('\u{1f8c}'),
        '\u{1f85}' => out.push('\u{1f8d}'),
        '\u{1f86}' => out.push('\u{1f8e}'),
        '\u{1f87}' => out.push('\u{1f8f}'),
        '\u{1f88}' => out.push('\u{1f88}'),
        '\u{1f89}' => out.push('\u{1f89}'),
        '\u{1f8a}' => out.push('\u{1f8a}'),
        '\u{1f8b}' => out.push('\u{1f8b}'),
        '\u{1f8c}' => out.push('\u{1f8c}'),
        '\u{1f8d}' => out.push('\u{1f8d}'),
        '\u{1f8e}' => out.push('\u{1f8e}'),
        '\u{1f8f}' => out.push('\u{1f8f}'),
        '\u{1f90}' => out.push('\u{1f98}'),
        '\u{1f91}' => out.push('\u{1f99}'),
        '\u{1f92}' => out.push('\u{1f9a}'),
        '\u{1f93}' => out.push('\u{1f9b}'),
        '\u{1f94}' => out.push('\u{1f9c}'),
        '\u{1f95}' => out.push('\u{1f9d}'),
        '\u{1f96}' => out.push('\u{1f9e}'),
        '\u{1f97}' => out.push('\u{1f9f}'),
        '\u{1f98}' => out.push('\u{1f98}'),
        '\u{1f99}' => out.push('\u{1f99}'),
        '\u{1f9a}' => out.push('\u{1f9a}'),
        '\u{1f9b}' => out.push('\u{1f9b}'),
        '\u{1f9c}' => out.push('\u{1f9c}'),
        '\u{1f9d}' => out.push('\u{1f9d}'),
        '\u{1f9e}' => out.push('\u{1f9e}'),
        '\u{1f9f}' => out.push('\u{1f9f}'),
        '\u{1fa0}' => out.push('\u{1fa8}'),
        '\u{1fa1}' => out.push('\u{1fa9}'),
        '\u{1fa2}' => out.push('\u{1faa}'),
        '\u{1fa3}' => out.push('\u{1fab}'),
        '\u{1fa4}' => out.push('\u{1fac}'),
        '\u{1fa5}' => out.push('\u{1fad}'),
        '\u{1fa6}' => out.push('\u{1fae}'),
        '\u{1fa7}' => out.push('\u{1faf}'),
        '\u{1fa8}' => out.push('\u{1fa8}'),
        '\u{1fa9}' => out.push('\u{1fa9}'),
        '\u{1faa}' => out.push('\u{1faa}'),
        '\u{1fab}' => out.push('\u{1fab}'),
        '\u{1fac}' => out.push('\u{1fac}'),
        '\u{1fad}' => out.push('\u{1fad}'),
        '\u{1fae}' => out.push('\u{1fae}'),
        '\u{1faf}' => out.push('\u{1faf}'),
        '\u{1fb2}' => out.push_str("\u{1fba}\u{345}"),
        '\u{1fb3}' => out.push('\u{1fbc}'),
        '\u{1fb4}' => out.push_str("\u{386}\u{345}"),
        '\u{1fb7}' => out.push_str("\u{391}\u{342}\u{345}"),
        '\u{1fbc}' => out.push('\u{1fbc}'),
        '\u{1fc2}' => out.push_str("\u{1fca}\u{345}"),
        '\u{1fc3}' => out.push('\u{1fcc}'),
        '\u{1fc4}' => out.push_str("\u{389}\u{345}"),
        '\u{1fc7}' => out.push_str("\u{397}\u{342}\u{345}"),
        '\u{1fcc}' => out.push('\u{1fcc}'),
        '\u{1ff2}' => out.push_str("\u{1ffa}\u{345}"),
        '\u{1ff3}' => out.push('\u{1ffc}'),
        '\u{1ff4}' => out.push_str("\u{38f}\u{345}"),
        '\u{1ff7}' => out.push_str("\u{3a9}\u{342}\u{345}"),
        '\u{1ffc}' => out.push('\u{1ffc}'),
        '\u{fb00}' => out.push_str("\u{46}\u{66}"),
        '\u{fb01}' => out.push_str("\u{46}\u{69}"),
        '\u{fb02}' => out.push_str("\u{46}\u{6c}"),
        '\u{fb03}' => out.push_str("\u{46}\u{66}\u{69}"),
        '\u{fb04}' => out.push_str("\u{46}\u{66}\u{6c}"),
        '\u{fb05}' => out.push_str("\u{53}\u{74}"),
        '\u{fb06}' => out.push_str("\u{53}\u{74}"),
        '\u{fb13}' => out.push_str("\u{544}\u{576}"),
        '\u{fb14}' => out.push_str("\u{544}\u{565}"),
        '\u{fb15}' => out.push_str("\u{544}\u{56b}"),
        '\u{fb16}' => out.push_str("\u{54e}\u{576}"),
        '\u{fb17}' => out.push_str("\u{544}\u{56d}"),
        _ => out.extend(c.to_uppercase()),
    }
}
// END GENERATED PYTHON CSV TITLE DATA
