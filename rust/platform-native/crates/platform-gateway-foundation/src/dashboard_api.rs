//! HTTP and WebSocket port of `src/api/v1/dashboard.py`.
//!
//! Constructors are inert. The host supplies resolved identity/RBAC, existing
//! realtime and metrics services, optional usage recorder and its database flag,
//! connected Redis, clocks and explicit cancellation policy. No global lookup,
//! connection, credential, environment read, background task or default mount.
//! Native collaborators reuse RealtimeMetricsService, MetricsRecorder,
//! NativeUsageAdminRecorder/PgBillingStore and RedisMetricsAdminBackend.
//!
//! Source quirks are retained: only breakdown/user skip platform-admin scope;
//! user has no ownership check; empty tenant fallback differs between operations;
//! PG truncates input datetimes to dates; Redis fallback is global except user
//! tokens; hourly Redis errors stay zero without a GET; unknown metrics stay zero;
//! unsupported granularity steps hourly while selecting daily counters. Ordinary
//! exceptions retain partial results at their exact source boundaries. Summary
//! freshness failure keeps its already-assigned PG overview, and Redis summary
//! fallback/trend are today-only. Empty rates are null; alerts compare strictly >.
//!
//! Signed i64/finite-f64/Decimal limits match existing native services. ISO query
//! datetimes reuse metrics parsing; extended Python parse diagnostics are not an
//! exact native contract. Bare trailing slashes return 404; canonical absolute
//! 307 redirects require the trusted host URL policy. HEAD/unsupported methods
//! return 405 before identity/backend access. GET bodies are never parsed.
//! Explicit cancellation and dropped futures stop remaining work; the host owns
//! cancellation transport and database cleanup. No invented status or retries.
use crate::{
    billing::microcents_to_usd,
    metrics_admin_api::{
        MetricsAdminApi, MetricsAdminBackendError, MetricsAdminCancellation, MetricsAdminClock,
        MetricsAdminError, MetricsAdminIdentity, MetricsAdminIdentityResolver, MetricsAdminRedis,
        MetricsAdminRequestContext, MetricsPermissionDeniedSink, MetricsSummarySource,
        local_timestamp, parse_query_timestamp, python_strftime_date, query_integer, read_int,
        timestamp_add, timestamp_le,
    },
    metrics_recorder::{MetricValue, compute_data_status, integer_ratio, round},
    protocol::ProtocolTimestamp,
    realtime_metrics::{RealtimeMetricsService, RealtimeSnapshot},
    usage_admin_api::{
        UsageAdminError, UsageAdminFuture, UsageAdminRecorder, UsageReadOperation,
        UsageRequestContext, exception_response, integer, number,
    },
    usage_parser::python_float_string,
};
use ai_platform_auth::rbac::Rbac;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use chrono::{Duration, Timelike};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Arc};

type ApiResult<T> = Result<T, UsageAdminError>;
pub const DASHBOARD_HTTP_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/dashboard/realtime", "get_realtime_dashboard"),
    ("GET", "/dashboard/timeseries/{metric}", "get_timeseries"),
    ("GET", "/dashboard/breakdown", "get_usage_breakdown"),
    ("GET", "/dashboard/alerts", "get_alerts"),
    ("GET", "/dashboard/user/{user_id}", "get_user_dashboard"),
    ("GET", "/dashboard/summary", "get_dashboard_summary"),
];
pub trait DashboardRealtimeSource: Send + Sync {
    fn snapshot(&self) -> UsageAdminFuture<'_, RealtimeSnapshot>;
    fn user_metrics<'a>(&'a self, user: &'a str, tenant: &'a str) -> UsageAdminFuture<'a, Value>;
}
impl DashboardRealtimeSource for RealtimeMetricsService {
    fn snapshot(&self) -> UsageAdminFuture<'_, RealtimeSnapshot> {
        Box::pin(async move { Ok(self.get_realtime_snapshot().await) })
    }
    fn user_metrics<'a>(&'a self, user: &'a str, tenant: &'a str) -> UsageAdminFuture<'a, Value> {
        Box::pin(async move { Ok(self.get_user_metrics(user, tenant).await) })
    }
}
/// Presence and database truthiness are separate source observations. A present
/// recorder with no database still receives breakdown, but no PG summary/series.
#[derive(Default)]
pub struct DashboardUsageState {
    pub recorder: Option<Arc<dyn UsageAdminRecorder>>,
    pub database_enabled: bool,
}
pub struct DashboardDependencies {
    pub realtime: Arc<dyn DashboardRealtimeSource>,
    pub metrics: Arc<dyn MetricsSummarySource>,
    pub usage: DashboardUsageState,
    pub redis: Option<Arc<dyn MetricsAdminRedis>>,
    pub clock: Arc<dyn MetricsAdminClock>,
    pub rbac: Rbac,
    pub denied: Option<Arc<dyn MetricsPermissionDeniedSink>>,
}
#[derive(Clone, Debug)]
pub enum DashboardOperation {
    Realtime,
    Timeseries(String),
    Breakdown,
    Alerts,
    User(String),
    Summary,
}
#[derive(Clone, Debug)]
pub enum DashboardQuery {
    Realtime,
    Timeseries {
        metric: String,
        start: Option<ProtocolTimestamp>,
        end: Option<ProtocolTimestamp>,
        granularity: String,
        user_id: Option<String>,
    },
    Breakdown {
        dimension: String,
        start: Option<ProtocolTimestamp>,
        end: Option<ProtocolTimestamp>,
        limit: i64,
    },
    Alerts,
    User(String),
    Summary(String),
}
impl DashboardQuery {
    fn global(&self) -> bool {
        !matches!(self, Self::Breakdown { .. } | Self::User(_))
    }
}
/// Separate from HTTP admission: the HTTP permit ends at the 101 upgrade.
#[derive(Clone, Debug)]
pub struct DashboardWebSocketConfig {
    max_sessions: usize,
    send_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
}
impl DashboardWebSocketConfig {
    pub fn new(
        max_sessions: usize,
        send_timeout_ms: u64,
        idle_timeout_ms: u64,
    ) -> Result<Self, &'static str> {
        if !(1..=1_000_000).contains(&max_sessions)
            || !(1..=86_400_000).contains(&send_timeout_ms)
            || !(1..=86_400_000).contains(&idle_timeout_ms)
        {
            return Err("invalid dashboard WebSocket limits");
        }
        Ok(Self {
            max_sessions,
            send_timeout: std::time::Duration::from_millis(send_timeout_ms),
            idle_timeout: std::time::Duration::from_millis(idle_timeout_ms),
        })
    }
}
impl Default for DashboardWebSocketConfig {
    fn default() -> Self {
        Self::new(256, 10_000, 60_000).expect("valid dashboard limits")
    }
}
pub struct DashboardApi {
    websocket_config: DashboardWebSocketConfig,
    websocket_sessions: Arc<tokio::sync::Semaphore>,
    gate: MetricsAdminApi,
    realtime: Arc<dyn DashboardRealtimeSource>,
    metrics: Arc<dyn MetricsSummarySource>,
    usage: DashboardUsageState,
    redis: Option<Arc<dyn MetricsAdminRedis>>,
    clock: Arc<dyn MetricsAdminClock>,
}
impl DashboardApi {
    pub fn new(d: DashboardDependencies) -> Self {
        let websocket_config = DashboardWebSocketConfig::default();
        Self {
            websocket_sessions: Arc::new(tokio::sync::Semaphore::new(
                websocket_config.max_sessions,
            )),
            websocket_config,
            gate: MetricsAdminApi::new(
                d.metrics.clone(),
                None,
                None,
                d.clock.clone(),
                d.rbac,
                d.denied,
            ),
            realtime: d.realtime,
            metrics: d.metrics,
            usage: d.usage,
            redis: d.redis,
            clock: d.clock,
        }
    }
    /// Configure before sharing the API with a router; never resize active sessions.
    pub fn with_websocket_config(mut self, config: DashboardWebSocketConfig) -> Self {
        self.websocket_sessions = Arc::new(tokio::sync::Semaphore::new(config.max_sessions));
        self.websocket_config = config;
        self
    }
    pub fn available_websocket_sessions(&self) -> usize {
        self.websocket_sessions.available_permits()
    }
    pub async fn execute(
        &self,
        identity: &MetricsAdminIdentity,
        query: DashboardQuery,
        context: &MetricsAdminRequestContext,
    ) -> ApiResult<Value> {
        self.gate
            .require(identity, query.global(), context)
            .map_err(metrics_error)?;
        match query {
            DashboardQuery::Realtime => realtime_dashboard(&self.realtime.snapshot().await?),
            DashboardQuery::Alerts => {
                let snapshot = self.realtime.snapshot().await?;
                Ok(
                    json!({"alerts":check_alerts(&snapshot, self.clock.as_ref())?,"last_check":self.now()}),
                )
            }
            DashboardQuery::Timeseries {
                metric,
                start,
                end,
                granularity,
                user_id,
            } => {
                let (start, end) = self.range(start, end, 1)?;
                self.timeseries(identity, &metric, start, end, &granularity, user_id)
                    .await
            }
            DashboardQuery::Breakdown {
                dimension,
                start,
                end,
                limit,
            } => {
                let (start, end) = self.range(start, end, 7)?;
                let Some(recorder) = &self.usage.recorder else {
                    return Ok(json!([]));
                };
                let rows = recorder
                    .call(
                        UsageReadOperation::Breakdown,
                        json!({
                            "tenant_id":tenant(identity),"dimension":dimension,
                            "start_date":local_timestamp(start).date().to_string(),
                            "end_date":local_timestamp(end).date().to_string(),"limit":limit
                        }),
                    )
                    .await?;
                let mut output = vec![];
                for row in rows.as_array().ok_or(UsageAdminError::Internal)? {
                    let row = row.as_object().ok_or(UsageAdminError::Internal)?;
                    let mut value = json!({"dimension_value":row.get(&dimension).cloned().unwrap_or(json!("unknown"))});
                    for key in [
                        "requests",
                        "input_tokens",
                        "output_tokens",
                        "total_tokens",
                        "cost_usd",
                        "percentage",
                    ] {
                        value[key] = row.get(key).cloned().unwrap_or(json!(0));
                    }
                    output.push(project(
                        &value,
                        &[
                            ("dimension_value", 's'),
                            ("requests", 'i'),
                            ("input_tokens", 'i'),
                            ("output_tokens", 'i'),
                            ("total_tokens", 'i'),
                            ("cost_usd", 'f'),
                            ("percentage", 'f'),
                        ],
                    )?);
                }
                Ok(json!(output))
            }
            DashboardQuery::User(user) => self.user(identity, &user).await,
            DashboardQuery::Summary(period) => self.summary(identity, &period).await,
        }
    }
    fn now(&self) -> String {
        ProtocolTimestamp::from(self.clock.local_now()).to_isoformat()
    }
    fn range(
        &self,
        start: Option<ProtocolTimestamp>,
        end: Option<ProtocolTimestamp>,
        days: i64,
    ) -> ApiResult<(ProtocolTimestamp, ProtocolTimestamp)> {
        let end = end.unwrap_or_else(|| self.clock.local_now().into());
        let start = match start {
            Some(start) => start,
            None => timestamp_add(end, Duration::days(-days)).map_err(backend_error)?,
        };
        Ok((start, end))
    }
    fn usage_database(&self) -> Option<&dyn UsageAdminRecorder> {
        self.usage
            .database_enabled
            .then_some(self.usage.recorder.as_deref())
            .flatten()
    }
    async fn timeseries(
        &self,
        identity: &MetricsAdminIdentity,
        metric: &str,
        start: ProtocolTimestamp,
        end: ProtocolTimestamp,
        granularity: &str,
        user: Option<String>,
    ) -> ApiResult<Value> {
        let response = |data: Vec<Value>| json!({"metric":metric,"granularity":granularity,"start":start.to_isoformat(),"end":end.to_isoformat(),"data":data});
        if let Some(recorder) = self.usage_database() {
            let result: ApiResult<Vec<Value>> = async {
                let rows = recorder.call(UsageReadOperation::Timeseries, json!({
                    "tenant_id":identity.tenant_id(),"start_date":local_timestamp(start).date().to_string(),
                    "end_date":local_timestamp(end).date().to_string(),"user_id":user,"granularity":granularity
                })).await?;
                let key = match metric { "latency" => "avg_latency_ms", "tokens" => "total_tokens", "cost" => "cost_usd", "runs" => "requests", _ => metric };
                let mut data = vec![];
                for row in rows.as_array().ok_or(UsageAdminError::Internal)? {
                    let row = row.as_object().ok_or(UsageAdminError::Internal)?;
                    let value = row.get(key).filter(|v| !v.is_null()).unwrap_or(&Value::Null);
                    let value = if value.is_null() { 0.0 } else { python_number(value)? };
                    let date = row.get("date").ok_or(UsageAdminError::Internal)?;
                    data.push(project(&json!({"timestamp":date,"value":value,"label":null}), &[("timestamp",'s'),("value",'f'),("label",'S')])?);
                }
                Ok(data)
            }.await;
            match result {
                Ok(data) => return Ok(response(data)),
                Err(UsageAdminError::Cancelled) => return Err(UsageAdminError::Cancelled),
                Err(_) => {}
            }
        }
        let mut data = vec![];
        if let Some(redis) = self.redis.as_deref().filter(|r| r.available()) {
            let result: ApiResult<()> = async {
                let mut current = start;
                let delta = match granularity {
                    "minute" => Duration::minutes(1),
                    "day" => Duration::days(1),
                    _ => Duration::hours(1),
                };
                while timestamp_le(current, end).map_err(backend_error)? {
                    let local = local_timestamp(current);
                    let date = python_strftime_date(local.date());
                    let hour = format!("{:02}", local.hour());
                    let value = match metric {
                        "requests" => {
                            read_float(
                                redis,
                                if granularity == "hour" {
                                    format!("metrics:requests:hour:{date}:{hour}")
                                } else {
                                    format!("metrics:requests:total:{date}")
                                },
                            )
                            .await?
                        }
                        "latency" => {
                            let sum = read_int(redis, format!("metrics:latency:sum:{date}"))
                                .await
                                .map_err(backend_error)?;
                            let count = read_int(redis, format!("metrics:latency:count:{date}"))
                                .await
                                .map_err(backend_error)?;
                            if count > 0 {
                                integer_ratio(sum, count)
                            } else {
                                0.0
                            }
                        }
                        // Canonical hourly branch constructs a key but never reads it.
                        "errors" if granularity == "hour" => 0.0,
                        "errors" => {
                            let mut total = 0_i64;
                            for h in 0..24 {
                                total = total
                                    .checked_add(
                                        read_int(
                                            redis,
                                            format!("metrics:errors:hour:{date}:{h:02}"),
                                        )
                                        .await
                                        .map_err(backend_error)?,
                                    )
                                    .ok_or(UsageAdminError::Internal)?;
                            }
                            total as f64
                        }
                        "tokens" => {
                            let prefix = user
                                .as_deref()
                                .filter(|u| !u.is_empty())
                                .map(|u| {
                                    format!("metrics:tokens:tenant:{}:user:{u}", tenant(identity))
                                })
                                .unwrap_or_else(|| "metrics:tokens".into());
                            let input = read_int(redis, format!("{prefix}:input:{date}"))
                                .await
                                .map_err(backend_error)?;
                            let output = read_int(redis, format!("{prefix}:output:{date}"))
                                .await
                                .map_err(backend_error)?;
                            input.checked_add(output).ok_or(UsageAdminError::Internal)? as f64
                        }
                        "cost" => {
                            let cents = read_int(redis, format!("metrics:tokens:cost:{date}"))
                                .await
                                .map_err(backend_error)?;
                            let micro =
                                read_int(redis, format!("metrics:tokens:cost_micro:{date}"))
                                    .await
                                    .map_err(backend_error)?;
                            if micro > 0 {
                                microcents_to_usd(micro)
                                    .to_string()
                                    .parse::<f64>()
                                    .map_err(|_| UsageAdminError::Internal)?
                            } else {
                                round(integer_ratio(cents, 100), 6)
                            }
                        }
                        "runs" => read_float(redis, format!("metrics:runs:total:{date}")).await?,
                        _ => 0.0,
                    };
                    data.push(
                        json!({"timestamp":current.to_isoformat(),"value":value,"label":null}),
                    );
                    current = timestamp_add(current, delta).map_err(backend_error)?;
                }
                Ok(())
            }
            .await;
            propagate_cancel(result)?;
        }
        Ok(response(data))
    }
    async fn user(&self, identity: &MetricsAdminIdentity, user: &str) -> ApiResult<Value> {
        let user_metrics = self.realtime.user_metrics(user, tenant(identity)).await?;
        let mut requests = json!(0);
        let mut latency = json!(0);
        let mut error_rate = Value::Null;
        let mut cost = json!(0.0);
        if let Some(recorder) = self.usage_database() {
            let result: ApiResult<()> = async {
                let summary = recorder.call(UsageReadOperation::Summary, json!({"tenant_id":identity.tenant_id(),"user_id":user,"start_date":self.clock.today().to_string(),"end_date":self.clock.today().to_string()})).await?;
                let summary = summary.as_object().ok_or(UsageAdminError::Internal)?;
                requests = summary.get("total_requests").cloned().unwrap_or(json!(0));
                latency = summary.get("avg_latency_ms").cloned().unwrap_or(json!(0));
                let rate = summary.get("success_rate").unwrap_or(&Value::Null);
                // Python numeric comparison/subtraction does not coerce strings.
                error_rate = if strict_number(&requests)? > 0.0 && !rate.is_null() { json!(error_percentage(rate)?) } else { Value::Null };
                cost = summary.get("total_cost_usd").cloned().unwrap_or(json!(0.0));
                Ok(())
            }.await;
            propagate_cancel(result)?;
        }
        let user_metrics = user_metrics.as_object().ok_or(UsageAdminError::Internal)?;
        let empty = Map::new();
        let tokens = match user_metrics.get("tokens") {
            None => &empty,
            Some(v) => v.as_object().ok_or(UsageAdminError::Internal)?,
        };
        let tokens = project(
            &json!({"total":tokens.get("total").unwrap_or(&json!(0)),"input_tokens":tokens.get("input").unwrap_or(&json!(0)),"output_tokens":tokens.get("output").unwrap_or(&json!(0)),"cost_usd":cost,"per_minute":0.0}),
            TOKEN_FIELDS,
        )?;
        let mut result = project(
            &json!({"user_id":user,"active_threads":user_metrics.get("active_threads").unwrap_or(&json!(0)),"requests_today":requests,"avg_latency_ms":latency,"error_rate":error_rate,"timestamp":self.now()}),
            &[
                ("user_id", 's'),
                ("active_threads", 'i'),
                ("requests_today", 'i'),
                ("avg_latency_ms", 'i'),
                ("error_rate", 'F'),
                ("timestamp", 's'),
            ],
        )?;
        result["tokens"] = tokens;
        Ok(result)
    }
    async fn summary(&self, identity: &MetricsAdminIdentity, period: &str) -> ApiResult<Value> {
        let mut summary_data = Map::new();
        if let Some(recorder) = self.usage_database() {
            let result: ApiResult<()> = async {
                let days = match period { "week" => 7, "month" => 30, _ => 0 };
                let start = self.clock.today().checked_sub_signed(Duration::days(days)).ok_or(UsageAdminError::Internal)?;
                let args = json!({"tenant_id":tenant(identity),"start_date":start.to_string(),"end_date":self.clock.today().to_string()});
                let summary = recorder.call(UsageReadOperation::Summary, args.clone()).await?;
                let summary = summary.as_object().ok_or(UsageAdminError::Internal)?;
                for (to, from, default) in OVERVIEW_FIELDS {
                    summary_data.insert((*to).into(), summary.get(*from).cloned().unwrap_or_else(|| default_value(*default)));
                }
                summary_data.insert("total_runs".into(), json!(0));
                // Assignment intentionally precedes freshness; partial state survives failure.
                let mut args = args;
                args["granularity"] = json!("day");
                args["end_date"] = json!(self.clock.today().to_string());
                let last = recorder.call(UsageReadOperation::Freshness, args).await?;
                let last = match last {
                    Value::Null => None,
                    Value::String(v) => Some(ProtocolTimestamp::parse_isoformat(&v).map_err(|_| UsageAdminError::Internal)?),
                    _ => return Err(UsageAdminError::Internal),
                };
                let timestamp = last.map(|v| match v { ProtocolTimestamp::Naive(v) => v.and_utc().fixed_offset(), ProtocolTimestamp::Offset(v) => v });
                let requests = summary_data.get("total_requests").unwrap_or(&Value::Null);
                let count = if strict_number(requests).ok() == Some(0.0) { Some(0) } else { Some(1) };
                let (status, age) = compute_data_status(timestamp, self.clock.utc_now(), count, 60);
                // Python set membership hashes its operand; lists/dicts fail
                // before metadata assignment and retain the earlier overview.
                if summary.get("data_status").is_some_and(|v| v.is_array() || v.is_object()) {
                    return Err(UsageAdminError::Internal);
                }
                let status = summary.get("data_status").and_then(Value::as_str).filter(|s| matches!(*s,"collection_error"|"unavailable")).unwrap_or(status);
                summary_data.insert("data_status".into(), json!(status));
                summary_data.insert("data_freshness_minutes".into(), json!(age));
                summary_data.insert("last_ingested_at".into(), last.map(|v| json!(v.to_isoformat())).unwrap_or(Value::Null));
                summary_data.insert("data_source".into(), json!("usage_aggregates"));
                Ok(())
            }.await;
            propagate_cancel(result)?;
        }
        if summary_data.is_empty() && period == "today" {
            let today = self.metrics.today_summary().await.map_err(backend_error)?;
            let today = today.as_object().ok_or(UsageAdminError::Internal)?;
            for (key, default) in [
                ("total_requests", '0'),
                ("success_rate", 'n'),
                ("avg_latency_ms", '0'),
                ("total_tokens", '0'),
                ("estimated_cost_usd", '0'),
                ("total_runs", '0'),
                ("data_status", 'e'),
                ("data_freshness_minutes", '9'),
                ("last_ingested_at", 'n'),
                ("data_source", 's'),
            ] {
                summary_data.insert(
                    key.into(),
                    today
                        .get(key)
                        .cloned()
                        .unwrap_or_else(|| default_value(default)),
                );
            }
        }
        if summary_data.is_empty() {
            summary_data = json!({"total_requests":0,"success_rate":null,"avg_latency_ms":0,"total_tokens":0,"estimated_cost_usd":0,"total_runs":0,"data_status":"empty","data_freshness_minutes":9999,"last_ingested_at":null,"data_source":"none"}).as_object().unwrap().clone();
        }
        if !truthy(summary_data.get("total_requests").unwrap_or(&Value::Null)) {
            summary_data.insert("success_rate".into(), Value::Null);
        }
        let snapshot = self.realtime.snapshot().await?;
        finite_snapshot(&snapshot)?;
        let trend = if period == "today" {
            let today = self.metrics.today_summary().await.map_err(backend_error)?;
            today
                .as_object()
                .ok_or(UsageAdminError::Internal)?
                .get("requests_by_hour")
                .cloned()
                .unwrap_or(json!([]))
        } else {
            json!([])
        };
        let get = |key: &str, default: Value| summary_data.get(key).cloned().unwrap_or(default);
        Ok(
            json!({"period":period,"overview":{"total_requests":get("total_requests",json!(0)),"success_rate":get("success_rate",Value::Null),"avg_latency_ms":get("avg_latency_ms",json!(0)),"total_tokens":get("total_tokens",json!(0)),"estimated_cost_usd":get("estimated_cost_usd",json!(0)),"total_runs":get("total_runs",json!(0))},"realtime":{"rps":snapshot.rps,"active_users":snapshot.active_users,"concurrent_requests":snapshot.concurrent_requests,"queue_depth":snapshot.queue_depth},"latency":{"p50":get("latency_p50",json!(0)),"p95":get("latency_p95",json!(0)),"p99":get("latency_p99",json!(0))},"hourly_trend":trend,"timestamp":self.now(),"data_status":get("data_status",json!("ok")),"data_freshness_minutes":get("data_freshness_minutes",json!(0)),"last_ingested_at":get("last_ingested_at",Value::Null),"data_source":get("data_source",json!("usage_aggregates"))}),
        )
    }
}
const OVERVIEW_FIELDS: &[(&str, &str, char)] = &[
    ("total_requests", "total_requests", '0'),
    ("success_rate", "success_rate", 'n'),
    ("avg_latency_ms", "avg_latency_ms", '0'),
    ("total_tokens", "total_tokens", '0'),
    ("estimated_cost_usd", "total_cost_usd", '0'),
];
const TOKEN_FIELDS: &[(&str, char)] = &[
    ("total", 'i'),
    ("input_tokens", 'i'),
    ("output_tokens", 'i'),
    ("cost_usd", 'f'),
    ("per_minute", 'f'),
];
fn default_value(kind: char) -> Value {
    match kind {
        'n' => Value::Null,
        'e' => json!("empty"),
        's' => json!("none"),
        '9' => json!(9999),
        _ => json!(0),
    }
}
fn tenant(identity: &MetricsAdminIdentity) -> &str {
    if identity.tenant_id().is_empty() {
        "public"
    } else {
        identity.tenant_id()
    }
}
fn backend_error(e: MetricsAdminBackendError) -> UsageAdminError {
    match e {
        MetricsAdminBackendError::Cancelled => UsageAdminError::Cancelled,
        MetricsAdminBackendError::Unavailable => UsageAdminError::Internal,
    }
}
fn metrics_error(e: MetricsAdminError) -> UsageAdminError {
    match e {
        MetricsAdminError::Cancelled => UsageAdminError::Cancelled,
        MetricsAdminError::Internal => UsageAdminError::Internal,
        MetricsAdminError::Http { status, detail } => UsageAdminError::Http { status, detail },
    }
}
fn propagate_cancel(result: ApiResult<()>) -> ApiResult<()> {
    if result == Err(UsageAdminError::Cancelled) {
        Err(UsageAdminError::Cancelled)
    } else {
        Ok(())
    }
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn strict_number(v: &Value) -> ApiResult<f64> {
    if v.is_number() || v.is_boolean() {
        number(v)
    } else {
        Err(UsageAdminError::Internal)
    }
}
fn error_percentage(rate: &Value) -> ApiResult<f64> {
    if let Some(rate) = rate.as_i64() {
        // Python subtracts integers before Pydantic converts the result to float.
        // i128 covers the complete difference for our declared i64 input domain.
        Ok((100_i128 - i128::from(rate)) as f64)
    } else {
        Ok(100.0 - strict_number(rate)?)
    }
}
fn python_number(v: &Value) -> ApiResult<f64> {
    let value = if let Some(s) = v.as_str() {
        // Python float rejects these C0 separators even though str.strip accepts
        // them. Do not reuse Pydantic's separate response-model grammar here.
        if s.chars().any(|c| matches!(c, '\u{1c}'..='\u{1f}')) {
            return Err(UsageAdminError::Internal);
        }
        python_float_string(s).ok_or(UsageAdminError::Internal)?
    } else {
        number(v)?
    };
    value
        .is_finite()
        .then_some(value)
        .ok_or(UsageAdminError::Internal)
}
async fn read_float(redis: &dyn MetricsAdminRedis, key: String) -> ApiResult<f64> {
    let value = redis
        .get(&MetricValue::Text(key))
        .await
        .map_err(backend_error)?;
    match value {
        MetricValue::Missing => Ok(0.0),
        MetricValue::Integer(v) => Ok(v as f64),
        MetricValue::Text(v) => {
            if v.is_empty() {
                Ok(0.0)
            } else {
                python_number(&json!(v))
            }
        }
        MetricValue::Bytes(v) => {
            if v.is_empty() {
                return Ok(0.0);
            }
            if !v.is_ascii() {
                return Err(UsageAdminError::Internal);
            }
            python_number(&json!(
                std::str::from_utf8(&v).map_err(|_| UsageAdminError::Internal)?
            ))
        }
    }
}
/// Pydantic's string-float parser first parses Unicode-trimmed text. On
/// failure it removes non-leading/trailing, nonconsecutive underscores from the
/// ORIGINAL text and parses again without trimming. This differs from float():
/// "1_.0" and "+_1" succeed, while " 1_0 " and Unicode digits fail.
fn model_number(value: &Value) -> ApiResult<f64> {
    let Some(text) = value.as_str() else {
        return number(value);
    };
    let parsed = text
        .trim()
        .parse::<f64>()
        .ok()
        .or_else(|| {
            if text.starts_with('_') || text.ends_with('_') || text.contains("__") {
                return None;
            }
            text.replace('_', "").parse::<f64>().ok()
        })
        .ok_or(UsageAdminError::Internal)?;
    parsed
        .is_finite()
        .then_some(parsed)
        .ok_or(UsageAdminError::Internal)
}
fn model_integer(value: &Value) -> Result<i64, (&'static str, &'static str)> {
    integer(value, false).map_err(|_| {
        if value.is_string() {
            (
                "int_parsing",
                "Input should be a valid integer, unable to parse string as an integer",
            )
        } else if value.is_number() {
            (
                "int_from_float",
                "Input should be a valid integer, got a number with a fractional part",
            )
        } else {
            ("int_type", "Input should be a valid integer")
        }
    })
}
fn project(value: &Value, fields: &[(&str, char)]) -> ApiResult<Value> {
    let mut out = Map::new();
    let mut errors = vec![];
    for &(name, kind) in fields {
        let value = value.get(name).unwrap_or(&Value::Null);
        if value.is_null() && matches!(kind, 'S' | 'F') {
            out.insert(name.into(), Value::Null);
            continue;
        }
        let parsed = match kind {
            's' | 'S' => value
                .as_str()
                .map(|s| json!(s))
                .ok_or(("string_type", "Input should be a valid string")),
            'i' => model_integer(value).map(|v| json!(v)),
            _ => model_number(value).map(|v| json!(v)).map_err(|_| {
                if value.is_string() {
                    (
                        "float_parsing",
                        "Input should be a valid number, unable to parse string as a number",
                    )
                } else {
                    ("float_type", "Input should be a valid number")
                }
            }),
        };
        match parsed {
            Ok(v) => {
                out.insert(name.into(), v);
            }
            Err((kind, message)) => {
                errors.push(json!({"field":name,"type":kind,"message":message}))
            }
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(UsageAdminError::ResponseValidation(errors))
    }
}
fn finite_snapshot(s: &RealtimeSnapshot) -> ApiResult<()> {
    if [
        s.rps,
        s.rps_1m,
        s.rps_5m,
        s.error_rate,
        s.error_rate_4xx,
        s.error_rate_5xx,
        s.token_cost_usd,
        s.tokens_per_minute,
        s.run_success_rate,
    ]
    .iter()
    .all(|v| v.is_finite())
    {
        Ok(())
    } else {
        Err(UsageAdminError::Internal)
    }
}
pub fn realtime_dashboard(s: &RealtimeSnapshot) -> ApiResult<Value> {
    finite_snapshot(s)?;
    let mut threads = Map::new();
    let mut errors = vec![];
    for (key, value) in &s.threads_by_user {
        match model_integer(value) {
            Ok(value) => {
                threads.insert(key.clone(), json!(value));
            }
            Err((kind, message)) => errors.push(
                json!({"field":format!("threads_by_user.{key}"),"type":kind,"message":message}),
            ),
        }
    }
    if !errors.is_empty() {
        return Err(UsageAdminError::ResponseValidation(errors));
    }
    Ok(
        json!({"rps":s.rps,"rps_1m":s.rps_1m,"rps_5m":s.rps_5m,"latency":{"p50":s.latency_p50,"p95":s.latency_p95,"p99":s.latency_p99,"avg":s.latency_avg},"errors":{"rate":s.error_rate,"rate_4xx":s.error_rate_4xx,"rate_5xx":s.error_rate_5xx},"users":{"active":s.active_users,"threads_total":s.total_threads,"threads_by_user":threads},"capacity":{"queue_depth":s.queue_depth,"concurrent":s.concurrent_requests,"max_concurrent":s.max_concurrent,"utilization":round(integer_ratio(s.concurrent_requests,s.max_concurrent.max(1))*100.0,1)},"tokens":{"total":s.total_tokens,"input_tokens":s.input_tokens,"output_tokens":s.output_tokens,"cost_usd":s.token_cost_usd,"per_minute":s.tokens_per_minute},"runs":{"total":s.total_runs,"success_rate":if s.total_runs>0 { json!(s.run_success_rate) } else { Value::Null },"avg_duration_ms":s.avg_run_duration_ms},"timestamp":s.timestamp,"is_live":true}),
    )
}
pub fn check_alerts(s: &RealtimeSnapshot, clock: &dyn MetricsAdminClock) -> ApiResult<Vec<Value>> {
    finite_snapshot(s)?;
    let utilization = integer_ratio(s.concurrent_requests, s.max_concurrent.max(1)) * 100.0;
    let values = [
        (
            "latency_p95",
            s.latency_p95 as f64,
            2000.0,
            5000.0,
            "P95 latency".to_owned(),
            format!("{}ms", s.latency_p95),
        ),
        (
            "error_rate",
            s.error_rate,
            5.0,
            10.0,
            "Error rate".to_owned(),
            format!("{:.1}%", s.error_rate),
        ),
        (
            "queue_depth",
            s.queue_depth as f64,
            50.0,
            100.0,
            "Queue depth".to_owned(),
            s.queue_depth.to_string(),
        ),
        (
            "utilization",
            utilization,
            80.0,
            95.0,
            "System utilization".to_owned(),
            format!("{utilization:.1}%"),
        ),
    ];
    Ok(values.into_iter().map(|(name,value,warning,critical,label,text)| {
        let level = if value > critical { "critical" } else if value > warning { "warning" } else { "ok" };
        json!({"name":name,"level":level,"message":if level=="ok" { format!("{label} is within normal range") } else { format!("{label} ({text}) exceeds {level} threshold") },"threshold":if level=="critical" { critical } else { warning },"current_value":value,"triggered_at":if level=="ok" { Value::Null } else { json!(ProtocolTimestamp::from(clock.local_now()).to_isoformat()) }})
    }).collect())
}
/// Resolve identity first, then validate fields, then execute the capability guard.
pub fn validate_query(
    operation: DashboardOperation,
    values: &HashMap<String, String>,
) -> ApiResult<DashboardQuery> {
    let mut errors = vec![];
    let query = match operation {
        DashboardOperation::Realtime => DashboardQuery::Realtime,
        DashboardOperation::Alerts => DashboardQuery::Alerts,
        DashboardOperation::User(id) => DashboardQuery::User(id),
        DashboardOperation::Summary => DashboardQuery::Summary(
            values
                .get("period")
                .cloned()
                .unwrap_or_else(|| "today".into()),
        ),
        DashboardOperation::Timeseries(metric) => DashboardQuery::Timeseries {
            metric,
            start: optional_timestamp(values, "start", &mut errors),
            end: optional_timestamp(values, "end", &mut errors),
            granularity: values
                .get("granularity")
                .cloned()
                .unwrap_or_else(|| "hour".into()),
            user_id: values.get("user_id").cloned(),
        },
        DashboardOperation::Breakdown => {
            let dimension = values.get("dimension").cloned().unwrap_or_else(|| { errors.push(json!({"type":"missing","loc":["query","dimension"],"msg":"Field required","input":null})); String::new() });
            DashboardQuery::Breakdown {
                dimension,
                start: optional_timestamp(values, "start", &mut errors),
                end: optional_timestamp(values, "end", &mut errors),
                limit: query_integer(values, "limit", 20, false, &mut errors),
            }
        }
    };
    if errors.is_empty() {
        Ok(query)
    } else {
        Err(UsageAdminError::http(422, errors))
    }
}
fn optional_timestamp(
    values: &HashMap<String, String>,
    name: &str,
    errors: &mut Vec<Value>,
) -> Option<ProtocolTimestamp> {
    let value = values.get(name)?;
    let parsed = parse_query_timestamp(value);
    if parsed.is_none() {
        errors.push(json!({"type":"datetime_from_date_parsing","loc":["query",name],"msg":"Input should be a valid datetime or date","input":value}));
    }
    parsed
}
#[derive(Clone)]
struct HttpState {
    identity: Arc<dyn MetricsAdminIdentityResolver>,
    api: Arc<DashboardApi>,
    cancellation: Arc<dyn MetricsAdminCancellation>,
}
pub fn router(
    identity: Arc<dyn MetricsAdminIdentityResolver>,
    api: Arc<DashboardApi>,
    cancellation: Arc<dyn MetricsAdminCancellation>,
) -> Router {
    Router::new()
        .route(
            "/api/v1/dashboard/ws",
            get(http_websocket).head(method_not_allowed),
        )
        .route(
            "/api/v1/dashboard/realtime",
            get(http_realtime)
                .head(method_not_allowed)
                .fallback(method_not_allowed),
        )
        .route(
            "/api/v1/dashboard/timeseries/{metric}",
            any(method_not_allowed)
                .get(http_timeseries)
                .head(method_not_allowed),
        )
        .route(
            "/api/v1/dashboard/breakdown",
            get(http_breakdown)
                .head(method_not_allowed)
                .fallback(method_not_allowed),
        )
        .route(
            "/api/v1/dashboard/alerts",
            get(http_alerts)
                .head(method_not_allowed)
                .fallback(method_not_allowed),
        )
        .route(
            "/api/v1/dashboard/user/{user_id}",
            any(method_not_allowed)
                .get(http_user)
                .head(method_not_allowed),
        )
        .route(
            "/api/v1/dashboard/summary",
            get(http_summary)
                .head(method_not_allowed)
                .fallback(method_not_allowed),
        )
        .fallback(not_found)
        .with_state(HttpState {
            identity,
            api,
            cancellation,
        })
}
fn encoded_slash(path: &str) -> bool {
    ["/api/v1/dashboard/user/", "/api/v1/dashboard/timeseries/"]
        .iter()
        .any(|prefix| {
            path.strip_prefix(prefix)
                .is_some_and(|id| id.to_ascii_lowercase().contains("%2f"))
        })
}
async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
}
async fn method_not_allowed(request: Request) -> Response {
    if encoded_slash(request.uri().path()) {
        return not_found().await;
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
async fn dispatch(state: HttpState, operation: DashboardOperation, request: Request) -> Response {
    if matches!(&operation,DashboardOperation::User(id)|DashboardOperation::Timeseries(id) if id.contains('/'))
    {
        return not_found().await;
    }
    let (parts, body) = request.into_parts();
    drop(body);
    let context = parts
        .extensions
        .get::<MetricsAdminRequestContext>()
        .cloned()
        .unwrap_or_default();
    let error_context = parts
        .extensions
        .get::<UsageRequestContext>()
        .cloned()
        .unwrap_or_default();
    let result = async {
        let identity = state
            .identity
            .resolve(&parts.headers, parts.uri.path())
            .await
            .map_err(metrics_error)?;
        let Query(values) = Query::<HashMap<String, String>>::try_from_uri(&parts.uri)
            .map_err(|_| UsageAdminError::http(422, "Invalid query parameters"))?;
        let query = validate_query(operation, &values)?;
        state.api.execute(&identity, query, &context).await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(UsageAdminError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let (status, body) =
                exception_response(&error, &error_context.exception_trace(&parts.headers))
                    .expect("non-cancellation error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
macro_rules! endpoint {
    ($name:ident,$op:ident) => {
        async fn $name(State(state): State<HttpState>, request: Request) -> Response {
            dispatch(state, DashboardOperation::$op, request).await
        }
    };
}
endpoint!(http_realtime, Realtime);
endpoint!(http_breakdown, Breakdown);
endpoint!(http_alerts, Alerts);
endpoint!(http_summary, Summary);
async fn http_timeseries(
    State(state): State<HttpState>,
    Path(metric): Path<String>,
    request: Request,
) -> Response {
    dispatch(state, DashboardOperation::Timeseries(metric), request).await
}
async fn http_user(
    State(state): State<HttpState>,
    Path(user): Path<String>,
    request: Request,
) -> Response {
    dispatch(state, DashboardOperation::User(user), request).await
}

/// Requested source close code and reason. Before upgrade, ASGI renders these
/// denials as HTTP 403; do not accept an unauthorized connection to send a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardWebSocketRejection {
    pub code: u16,
    pub reason: &'static str,
}
impl DashboardApi {
    pub fn authorize_websocket(
        &self,
        identity: Result<&MetricsAdminIdentity, &MetricsAdminError>,
        context: &MetricsAdminRequestContext,
    ) -> Result<(), DashboardWebSocketRejection> {
        let identity = identity.map_err(|_| DashboardWebSocketRejection {
            code: 4001,
            reason: "Authentication required",
        })?;
        self.gate
            .require(identity, true, context)
            .map_err(|_| DashboardWebSocketRejection {
                code: 4003,
                reason: "Metrics permission required",
            })
    }
    pub async fn websocket_snapshot(&self, tenant_id: &str) -> ApiResult<Value> {
        let snapshot = self.realtime.snapshot().await?;
        let mut data = snapshot.to_dict();
        if snapshot.total_runs == 0 {
            data["runs"]["success_rate"] = Value::Null;
        }
        data["type"] = json!("metrics");
        if !tenant_id.is_empty() {
            data["tenant_id"] = json!(tenant_id);
        }
        data["alerts"] = json!(check_alerts(&snapshot, self.clock.as_ref())?);
        Ok(data)
    }
}
async fn http_websocket(
    State(state): State<HttpState>,
    upgrade: axum::extract::ws::WebSocketUpgrade,
    request: Request,
) -> Response {
    let (parts, _) = request.into_parts();
    let context = parts
        .extensions
        .get::<MetricsAdminRequestContext>()
        .cloned()
        .unwrap_or_default();
    let identity = state
        .identity
        .resolve(&parts.headers, parts.uri.path())
        .await;
    if state
        .api
        .authorize_websocket(identity.as_ref(), &context)
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let identity = identity.expect("authorized identity");
    let Ok(permit) = state.api.websocket_sessions.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "1")]).into_response();
    };
    let config = state.api.websocket_config.clone();
    upgrade
        .on_upgrade(move |mut socket| async move {
            use axum::extract::ws::Message;
            // Captured before upgrade: failed handshake/drop also releases admission.
            let _permit = permit;
            let mut last_received = tokio::time::Instant::now();
            loop {
                let Ok(Ok(data)) = tokio::time::timeout(
                    config.send_timeout,
                    state.api.websocket_snapshot(identity.tenant_id()),
                )
                .await
                else {
                    break;
                };
                if !matches!(
                    tokio::time::timeout(
                        config.send_timeout,
                        socket.send(Message::Text(data.to_string().into()))
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
                let remaining = config.idle_timeout.saturating_sub(last_received.elapsed());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(
                    remaining.min(std::time::Duration::from_secs(5)),
                    socket.recv(),
                )
                .await
                {
                    Err(_) => {
                        if last_received.elapsed() >= config.idle_timeout {
                            break;
                        }
                        // Browser peers answer protocol pings automatically; passive
                        // dashboards stay open while unreachable peers release capacity.
                        if !matches!(
                            tokio::time::timeout(
                                config.send_timeout,
                                socket.send(Message::Ping(Vec::new().into()))
                            )
                            .await,
                            Ok(Ok(()))
                        ) {
                            break;
                        }
                    }
                    Ok(Some(Ok(Message::Text(message)))) => {
                        last_received = tokio::time::Instant::now();
                        if message == "ping"
                            && !matches!(
                                tokio::time::timeout(
                                    config.send_timeout,
                                    socket.send(Message::Text("{\"type\":\"pong\"}".into()))
                                )
                                .await,
                                Ok(Ok(()))
                            )
                        {
                            break;
                        }
                    }
                    Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => {
                        last_received = tokio::time::Instant::now();
                    }
                    _ => break,
                }
            }
        })
        .into_response()
}
