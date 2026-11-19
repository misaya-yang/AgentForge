//! Dashboard Redis metrics, ported from src/services/metrics/metrics_recorder.py
//! and data_status.py. This is distinct from durable usage accounting and the
//! independent realtime_metrics service. No constructor starts a task or connects.
//!
//! Commands remain in source order in one Redis transaction per observation;
//! run tokens are a SECOND transaction only after the run transaction succeeds.
//! Failures are best effort, never retried (a failed transaction may have applied
//! commands). Dropping a future cancels its remaining work, like Python's
//! CancelledError, and cannot undo commands Redis already accepted. There are no
//! detached tasks, invented queues, SQL writes, or process-global ownership.
//!
//! Boundary: i64 counters and finite f64 durations; exact catalog decimal pricing
//! within billing's documented envelope. Redis replies retain bytes/text identity.
//! Nonfinite Redis latency samples fail closed to zero percentiles; CPython
//! timsort ordering of externally injected mixed NaNs is outside this boundary.
//! Last-ingested parsing accepts emitted RFC3339, naive ISO date-times and dates;
//! Python's other fromisoformat extensions are outside this slice. Collaborator
//! errors are opaque, so credentials and payloads cannot enter diagnostics.
use crate::{
    billing::{resolve_pricing, usd_to_microcents},
    usage_parser::python_value_to_string,
};
use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc};

pub const TTL_24H: i64 = 86_400;
pub const TTL_48H: i64 = 172_800;
pub const TTL_7D: i64 = 604_800;
pub const LAST_INGESTED_KEY: &str = "metrics:last_ingested_at";
pub type MetricsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, MetricsError>> + Send + 'a>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsError;
impl std::fmt::Display for MetricsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("metrics_collection_error")
    }
}
impl std::error::Error for MetricsError {}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MetricValue {
    Missing,
    Text(String),
    Bytes(Vec<u8>),
    Integer(i64),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricCommand {
    pub op: String,
    pub args: Vec<Value>,
}
fn command(op: &str, args: Vec<Value>) -> MetricCommand {
    MetricCommand {
        op: op.into(),
        args,
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastIngestedSetter {
    Expiring,
    WithoutExpiry,
    Missing,
}
pub trait MetricsRedis: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    /// Models duck-typed pipeline.set missing / rejecting ex= (TypeError).
    fn last_ingested_setter(&self) -> LastIngestedSetter {
        LastIngestedSetter::Expiring
    }
    fn execute<'a>(&'a self, commands: &'a [MetricCommand]) -> MetricsFuture<'a, Vec<MetricValue>>;
    fn latency_samples(&self) -> MetricsFuture<'_, Vec<MetricValue>>;
}
pub trait MetricsClock: Send + Sync {
    // Separate calls intentionally preserve source's local-date/hour boundary.
    fn local_date(&self) -> String;
    fn local_hour(&self) -> String;
    fn utc_now(&self) -> DateTime<Utc>;
    fn timestamp(&self) -> f64;
    fn sample_suffix(&self) -> String;
}
pub struct SystemMetricsClock;
impl MetricsClock for SystemMetricsClock {
    fn local_date(&self) -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }
    fn local_hour(&self) -> String {
        Local::now().format("%H").to_string()
    }
    fn utc_now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn timestamp(&self) -> f64 {
        Utc::now().timestamp_micros() as f64 / 1_000_000.0
    }
    fn sample_suffix(&self) -> String {
        uuid::Uuid::new_v4().simple().to_string()[..12].to_owned()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    Recorded,
    Unavailable,
    CollectionError,
}
#[derive(Clone)]
pub struct MetricsRecorder {
    redis: Option<Arc<dyn MetricsRedis>>,
    clock: Arc<dyn MetricsClock>,
    latency_sample_cap: i64,
}
impl Default for MetricsRecorder {
    fn default() -> Self {
        Self::new(None, Arc::new(SystemMetricsClock), 10_000)
    }
}
impl MetricsRecorder {
    /// Constructor keeps the source's initial cap verbatim. Reload clamps to 1.
    pub fn new(
        redis: Option<Arc<dyn MetricsRedis>>,
        clock: Arc<dyn MetricsClock>,
        latency_sample_cap: i64,
    ) -> Self {
        Self {
            redis,
            clock,
            latency_sample_cap,
        }
    }
    pub fn set_redis(&mut self, redis: Arc<dyn MetricsRedis>) {
        self.redis = Some(redis);
    }
    pub fn set_latency_sample_cap(&mut self, cap: i64) {
        self.latency_sample_cap = cap.max(1);
    }
    fn client(&self) -> Option<&dyn MetricsRedis> {
        self.redis.as_deref().filter(|r| r.available())
    }
    fn ingestion(
        &self,
        redis: &dyn MetricsRedis,
        commands: &mut Vec<MetricCommand>,
        now: DateTime<Utc>,
    ) {
        let mut args = vec![json!(LAST_INGESTED_KEY), json!(iso(now.fixed_offset()))];
        match redis.last_ingested_setter() {
            LastIngestedSetter::Missing => return,
            LastIngestedSetter::Expiring => args.push(json!(TTL_7D)),
            LastIngestedSetter::WithoutExpiry => (),
        }
        commands.push(command("set", args));
    }
    pub async fn record_request(
        &self,
        _method: &str,
        _path: &str,
        status_code: i32,
        duration_ms: f64,
        _user_id: &str,
        service_id: &str,
    ) -> RecordOutcome {
        let Some(redis) = self.client() else {
            return RecordOutcome::Unavailable;
        };
        let today = self.clock.local_date();
        let hour = self.clock.local_hour();
        let success = (200..400).contains(&status_code);
        let mut commands = vec![];
        self.ingestion(redis, &mut commands, self.clock.utc_now());
        increment(
            &mut commands,
            format!("metrics:requests:total:{today}"),
            None,
            TTL_48H,
        );
        if success {
            increment(
                &mut commands,
                format!("metrics:requests:success:{today}"),
                None,
                TTL_48H,
            );
        }
        let Ok(duration) = trunc_i64(duration_ms) else {
            return RecordOutcome::CollectionError;
        };
        increment(
            &mut commands,
            format!("metrics:latency:sum:{today}"),
            Some(duration),
            TTL_48H,
        );
        increment(
            &mut commands,
            format!("metrics:latency:count:{today}"),
            None,
            TTL_48H,
        );
        increment(
            &mut commands,
            format!("metrics:requests:hour:{today}:{hour}"),
            None,
            TTL_48H,
        );
        if !success {
            increment(
                &mut commands,
                format!("metrics:errors:hour:{today}:{hour}"),
                None,
                TTL_48H,
            );
        }
        if !service_id.is_empty() {
            increment(
                &mut commands,
                format!("metrics:service:{service_id}:{today}"),
                None,
                TTL_48H,
            );
        }
        let timestamp = self.clock.timestamp();
        if !timestamp.is_finite() {
            return RecordOutcome::CollectionError;
        }
        let member = format!(
            "{}:{}:{}",
            python_value_to_string(&json!(timestamp)),
            self.clock.sample_suffix(),
            python_value_to_string(&json!(duration_ms))
        );
        commands.push(command(
            "zadd",
            vec![json!("metrics:latency:samples"), json!({member: timestamp})],
        ));
        let Some(stop) = self
            .latency_sample_cap
            .checked_add(1)
            .and_then(i64::checked_neg)
        else {
            return RecordOutcome::CollectionError;
        };
        commands.push(command(
            "zremrangebyrank",
            vec![json!("metrics:latency:samples"), json!(0), json!(stop)],
        ));
        outcome(redis.execute(&commands).await)
    }
    pub async fn record_tokens(
        &self,
        user_id: &str,
        service_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        model: &str,
        tenant_id: &str,
    ) -> RecordOutcome {
        let Some(redis) = self.client() else {
            return RecordOutcome::Unavailable;
        };
        let today = self.clock.local_date();
        let now = self.clock.utc_now();
        let Ok(cost) = resolve_pricing(model)
            .rates
            .calculate_cost(input_tokens, output_tokens)
            .and_then(|c| usd_to_microcents(c.total_cost_usd))
        else {
            return RecordOutcome::CollectionError;
        };
        // Source explicitly converts the combined microcents through float before
        // half-even legacy-cent rounding; don't substitute component rounding.
        let Ok(cents) = trunc_i64((integer_ratio(cost, 10_000)).round_ties_even()) else {
            return RecordOutcome::CollectionError;
        };
        let mut commands = vec![];
        self.ingestion(redis, &mut commands, now);
        for (key, value) in [
            (format!("metrics:tokens:input:{today}"), input_tokens),
            (format!("metrics:tokens:output:{today}"), output_tokens),
            (format!("metrics:tokens:cost_micro:{today}"), cost),
            (format!("metrics:tokens:cost:{today}"), cents),
        ] {
            increment(&mut commands, key, Some(value), TTL_7D);
        }
        if !user_id.is_empty() {
            let prefix = if tenant_id.is_empty() {
                format!("metrics:tokens:user:{user_id}")
            } else {
                format!("metrics:tokens:tenant:{tenant_id}:user:{user_id}")
            };
            increment(
                &mut commands,
                format!("{prefix}:input:{today}"),
                Some(input_tokens),
                TTL_7D,
            );
            increment(
                &mut commands,
                format!("{prefix}:output:{today}"),
                Some(output_tokens),
                TTL_7D,
            );
        }
        if !service_id.is_empty() {
            increment(
                &mut commands,
                format!("metrics:tokens:service:{service_id}:input:{today}"),
                Some(input_tokens),
                TTL_7D,
            );
            increment(
                &mut commands,
                format!("metrics:tokens:service:{service_id}:output:{today}"),
                Some(output_tokens),
                TTL_7D,
            );
        }
        outcome(redis.execute(&commands).await)
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn record_run(
        &self,
        user_id: &str,
        assistant_id: &str,
        duration_ms: f64,
        status: &str,
        input_tokens: i64,
        output_tokens: i64,
        tenant_id: &str,
    ) -> RecordOutcome {
        let Some(redis) = self.client() else {
            return RecordOutcome::Unavailable;
        };
        let today = self.clock.local_date();
        let mut commands = vec![];
        self.ingestion(redis, &mut commands, self.clock.utc_now());
        increment(
            &mut commands,
            format!("metrics:runs:total:{today}"),
            None,
            TTL_48H,
        );
        if status == "success" {
            increment(
                &mut commands,
                format!("metrics:runs:success:{today}"),
                None,
                TTL_48H,
            );
        }
        let Ok(duration) = trunc_i64(duration_ms) else {
            return RecordOutcome::CollectionError;
        };
        increment(
            &mut commands,
            format!("metrics:runs:duration:sum:{today}"),
            Some(duration),
            TTL_48H,
        );
        increment(
            &mut commands,
            format!("metrics:runs:duration:count:{today}"),
            None,
            TTL_48H,
        );
        if !assistant_id.is_empty() {
            increment(
                &mut commands,
                format!("metrics:runs:assistant:{assistant_id}:{today}"),
                None,
                TTL_48H,
            );
        }
        if redis.execute(&commands).await.is_err() {
            return RecordOutcome::CollectionError;
        }
        if input_tokens > 0 || output_tokens > 0 {
            // Source swallows the inner token failure independently.
            let _ = self
                .record_tokens(
                    user_id,
                    assistant_id,
                    input_tokens,
                    output_tokens,
                    "default",
                    tenant_id,
                )
                .await;
        }
        RecordOutcome::Recorded
    }
    pub async fn get_latency_percentiles(&self) -> Value {
        let empty = || json!({"p50":0,"p95":0,"p99":0});
        let Some(redis) = self.client() else {
            return empty();
        };
        let Ok(samples) = redis.latency_samples().await else {
            return empty();
        };
        let mut values = vec![];
        for sample in samples {
            match sample {
                MetricValue::Text(text) => {
                    if let Some((_, value)) = text.rsplit_once(':')
                        && let Some(value) =
                            (!value.chars().any(|c| matches!(c, '\u{1c}'..='\u{1f}')))
                                .then(|| crate::usage_parser::python_float_string(value))
                                .flatten()
                    {
                        values.push(value);
                    }
                }
                // Python bytes.rsplit(str) raises TypeError, caught by OUTER handler.
                MetricValue::Bytes(_) => return empty(),
                _ => (),
            }
        }
        if values.is_empty() {
            return empty();
        }
        if values.iter().any(|v| !v.is_finite()) {
            return empty();
        }
        values.sort_by(|a, b| a.partial_cmp(b).expect("finite sample"));
        let n = values.len();
        let results: Result<Vec<_>, _> = [50usize, 95, 99]
            .into_iter()
            .map(|p| trunc_i64(values[(n * p / 100).min(n - 1)]))
            .collect();
        let Ok(v) = results else {
            return empty();
        };
        json!({"p50":v[0],"p95":v[1],"p99":v[2]})
    }
    pub async fn get_today_summary(&self) -> Value {
        let Some(redis) = self.client() else {
            return empty_summary("unavailable");
        };
        match self.summary(redis).await {
            Ok(value) => value,
            Err(_) => empty_summary("collection_error"),
        }
    }
    async fn summary(&self, redis: &dyn MetricsRedis) -> Result<Value, MetricsError> {
        let today = self.clock.local_date();
        let mut commands: Vec<_> = [
            "requests:total",
            "requests:success",
            "latency:sum",
            "latency:count",
            "tokens:input",
            "tokens:output",
            "tokens:cost",
            "tokens:cost_micro",
            "runs:total",
            "runs:success",
            "runs:duration:sum",
            "runs:duration:count",
        ]
        .into_iter()
        .map(|name| command("get", vec![json!(format!("metrics:{name}:{today}"))]))
        .collect();
        commands.push(command("get", vec![json!(LAST_INGESTED_KEY)]));
        for hour in 0..24 {
            commands.push(command(
                "get",
                vec![json!(format!("metrics:requests:hour:{today}:{hour:02}"))],
            ));
        }
        let results = redis.execute(&commands).await?;
        if results.len() < 37 {
            return Err(MetricsError);
        }
        let nums: Vec<i64> = results[..12]
            .iter()
            .map(integer)
            .collect::<Result<_, _>>()?;
        let hours: Vec<Value> = (0..24)
            .map(|i| Ok(json!({"hour":format!("{i:02}:00"),"count":integer(&results[13+i])?})))
            .collect::<Result<_, MetricsError>>()?;
        let percentiles = self.get_latency_percentiles().await;
        let ingested = parse_last_ingested_at(&results[12]);
        let (status, age) = if ingested.is_none() && nums[0] == 0 {
            ("empty", 9999)
        } else {
            compute_data_status(ingested, self.clock.utc_now(), Some(nums[0]), 60)
        };
        let ratio = |a: i64, b: i64| {
            if b > 0 {
                Some(round(integer_ratio(a, b) * 100.0, 1))
            } else {
                None
            }
        };
        let average = |a: i64, b: i64| {
            if b > 0 {
                trunc_i64(integer_ratio(a, b))
            } else {
                Ok(0)
            }
        };
        let cost = if nums[7] > 0 {
            crate::billing::microcents_to_usd(nums[7])
                .to_string()
                .parse::<f64>()
                .map_err(|_| MetricsError)?
        } else {
            round(integer_ratio(nums[6], 100), 6)
        };
        Ok(
            json!({"total_requests":nums[0],"success_rate":ratio(nums[1],nums[0]),"avg_latency_ms":average(nums[2],nums[3])?,"latency_p50":percentiles["p50"],"latency_p95":percentiles["p95"],"latency_p99":percentiles["p99"],"total_tokens":nums[4].checked_add(nums[5]).ok_or(MetricsError)?,"prompt_tokens":nums[4],"completion_tokens":nums[5],"estimated_cost_usd":round(cost,6),"total_runs":nums[8],"run_success_rate":ratio(nums[9],nums[8]),"avg_run_duration_ms":average(nums[10],nums[11])?,"requests_by_hour":hours,"last_ingested_at":ingested.map(iso),"data_status":status,"data_freshness_minutes":age,"data_source":"redis"}),
        )
    }
}
fn increment(commands: &mut Vec<MetricCommand>, key: String, value: Option<i64>, ttl: i64) {
    commands.push(match value {
        Some(value) => command("incrby", vec![json!(key), json!(value)]),
        None => command("incr", vec![json!(key)]),
    });
    commands.push(command("expire", vec![json!(key), json!(ttl)]));
}
fn outcome(result: Result<Vec<MetricValue>, MetricsError>) -> RecordOutcome {
    if result.is_ok() {
        RecordOutcome::Recorded
    } else {
        RecordOutcome::CollectionError
    }
}
fn trunc_i64(value: f64) -> Result<i64, MetricsError> {
    if value.is_finite() && value >= i64::MIN as f64 && value < 9_223_372_036_854_775_808.0 {
        Ok(value as i64)
    } else {
        Err(MetricsError)
    }
}
/// Python true division rounds the exact integer ratio once, rather than
/// independently converting both integer operands to f64 first.
pub(crate) fn integer_ratio(numerator: i64, denominator: i64) -> f64 {
    debug_assert_ne!(denominator, 0);
    if numerator == 0 {
        return if denominator < 0 { -0.0 } else { 0.0 };
    }
    let a = u128::from(numerator.unsigned_abs());
    let b = u128::from(denominator.unsigned_abs());
    let mut exponent = (127 - a.leading_zeros()) as i32 - (127 - b.leading_zeros()) as i32;
    let below = if exponent >= 0 {
        a < (b << exponent)
    } else {
        (a << -exponent) < b
    };
    if below {
        exponent -= 1;
    }
    let shift = 52 - exponent;
    let (scaled, divisor) = if shift >= 0 {
        (a << shift, b)
    } else {
        (a, b << -shift)
    };
    let mut quotient = scaled / divisor;
    let remainder = scaled % divisor;
    if remainder * 2 > divisor || (remainder * 2 == divisor && quotient % 2 == 1) {
        quotient += 1;
    }
    let value = quotient as f64 * 2f64.powi(exponent - 52);
    if (numerator < 0) != (denominator < 0) {
        -value
    } else {
        value
    }
}
pub(crate) fn round(value: f64, places: usize) -> f64 {
    format!("{value:.places$}").parse().expect("finite round")
}
pub(crate) fn integer(value: &MetricValue) -> Result<i64, MetricsError> {
    fn parse(text: &str, bytes: bool) -> Result<i64, MetricsError> {
        if text.chars().any(|c| matches!(c, '\u{1c}'..='\u{1f}')) {
            return Err(MetricsError);
        }
        let text = if bytes {
            text.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}'))
        } else {
            text.trim_matches(crate::usage_parser::python_whitespace)
        };
        let mut normalized = String::new();
        let mut chars = text.chars().peekable();
        if matches!(chars.peek(), Some('+' | '-')) {
            normalized.push(chars.next().unwrap());
        }
        let mut previous_digit = false;
        let mut any_digit = false;
        while let Some(c) = chars.next() {
            if c == '_' {
                if !previous_digit
                    || chars
                        .peek()
                        .and_then(|c| crate::usage_parser::decimal_digit(*c))
                        .is_none()
                {
                    return Err(MetricsError);
                }
                previous_digit = false;
            } else if let Some(digit) = crate::usage_parser::decimal_digit(c) {
                normalized.push(char::from(b'0' + digit));
                previous_digit = true;
                any_digit = true;
            } else {
                return Err(MetricsError);
            }
        }
        if !any_digit || !previous_digit {
            return Err(MetricsError);
        }
        normalized.parse().map_err(|_| MetricsError)
    }
    match value {
        MetricValue::Missing => Ok(0),
        MetricValue::Integer(v) => Ok(*v),
        MetricValue::Text(v) if v.is_empty() => Ok(0),
        MetricValue::Bytes(v) if v.is_empty() => Ok(0),
        MetricValue::Text(v) => parse(v, false),
        MetricValue::Bytes(v) => {
            if !v.is_ascii() {
                return Err(MetricsError);
            }
            parse(std::str::from_utf8(v).map_err(|_| MetricsError)?, true)
        }
    }
}
fn iso(value: DateTime<FixedOffset>) -> String {
    value.to_rfc3339_opts(
        if value.timestamp_subsec_micros() == 0 {
            SecondsFormat::Secs
        } else {
            SecondsFormat::Micros
        },
        false,
    )
}
fn utf8_ignore(bytes: &[u8]) -> String {
    let mut result = String::new();
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                result.push_str(s);
                break;
            }
            Err(e) => {
                result
                    .push_str(std::str::from_utf8(&rest[..e.valid_up_to()]).expect("valid prefix"));
                match e.error_len() {
                    Some(n) => rest = &rest[e.valid_up_to() + n..],
                    None => break,
                }
            }
        }
    }
    result
}
pub fn parse_last_ingested_at(value: &MetricValue) -> Option<DateTime<FixedOffset>> {
    let text = match value {
        MetricValue::Missing => return None,
        MetricValue::Text(v) => v.clone(),
        MetricValue::Bytes(v) => utf8_ignore(v),
        MetricValue::Integer(v) => v.to_string(),
    };
    DateTime::parse_from_rfc3339(&text)
        .ok()
        .or_else(|| {
            [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M",
            ]
            .into_iter()
            .find_map(|fmt| {
                NaiveDateTime::parse_from_str(&text, fmt)
                    .ok()
                    .map(|v| v.and_utc().fixed_offset())
            })
        })
        .or_else(|| {
            NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .ok()
                .map(|v| v.and_hms_opt(0, 0, 0).unwrap().and_utc().fixed_offset())
        })
}
pub fn compute_data_status(
    last: Option<DateTime<FixedOffset>>,
    now: DateTime<Utc>,
    total_requests: Option<i64>,
    max_age_minutes: i64,
) -> (&'static str, i64) {
    let Some(last) = last else {
        return (
            if total_requests == Some(0) {
                "empty"
            } else {
                "delayed"
            },
            9999,
        );
    };
    let age = (now
        .signed_duration_since(last)
        .num_microseconds()
        .map(|n| integer_ratio(n, 1_000_000) / 60.0)
        .unwrap_or_else(|| now.signed_duration_since(last).num_seconds() as f64 / 60.0))
        as i64;
    (
        if age > max_age_minutes {
            "delayed"
        } else if total_requests == Some(0) {
            "empty"
        } else {
            "ok"
        },
        age,
    )
}
pub fn empty_summary(status: &str) -> Value {
    json!({"total_requests":0,"success_rate":null,"avg_latency_ms":0,"latency_p50":0,"latency_p95":0,"latency_p99":0,"total_tokens":0,"prompt_tokens":0,"completion_tokens":0,"estimated_cost_usd":0.0,"total_runs":0,"run_success_rate":null,"avg_run_duration_ms":0,"requests_by_hour":(0..24).map(|i|json!({"hour":format!("{i:02}:00"),"count":0})).collect::<Vec<_>>(),"last_ingested_at":null,"data_status":status,"data_freshness_minutes":9999,"data_source":"none"})
}

/// Production adapter over an already-established connection. Creating the
/// recorder or adapter never establishes a connection or selects a Redis DB.
pub struct RedisMetricsBackend {
    connection: redis::aio::ConnectionManager,
}
impl RedisMetricsBackend {
    pub fn new(connection: redis::aio::ConnectionManager) -> Self {
        Self { connection }
    }
}
impl MetricsRedis for RedisMetricsBackend {
    fn execute<'a>(&'a self, commands: &'a [MetricCommand]) -> MetricsFuture<'a, Vec<MetricValue>> {
        Box::pin(async move {
            let mut pipe = redis::pipe();
            pipe.atomic();
            for command in commands {
                match command.op.as_str() {
                    "set" => {
                        pipe.cmd("SET")
                            .arg(
                                command
                                    .args
                                    .first()
                                    .and_then(Value::as_str)
                                    .ok_or(MetricsError)?,
                            )
                            .arg(
                                command
                                    .args
                                    .get(1)
                                    .and_then(Value::as_str)
                                    .ok_or(MetricsError)?,
                            );
                        if let Some(ttl) = command.args.get(2) {
                            pipe.arg("EX").arg(ttl.as_i64().ok_or(MetricsError)?);
                        }
                    }
                    "zadd" => {
                        pipe.cmd("ZADD").arg(
                            command
                                .args
                                .first()
                                .and_then(Value::as_str)
                                .ok_or(MetricsError)?,
                        );
                        let entries = command
                            .args
                            .get(1)
                            .and_then(Value::as_object)
                            .ok_or(MetricsError)?;
                        for (member, score) in entries {
                            pipe.arg(score.as_f64().ok_or(MetricsError)?).arg(member);
                        }
                    }
                    "get" | "incr" | "incrby" | "expire" | "zremrangebyrank" => {
                        pipe.cmd(&command.op.to_uppercase());
                        for arg in &command.args {
                            match arg {
                                Value::String(s) => {
                                    pipe.arg(s);
                                }
                                Value::Number(n) => {
                                    pipe.arg(n.as_i64().ok_or(MetricsError)?);
                                }
                                _ => return Err(MetricsError),
                            }
                        }
                    }
                    _ => return Err(MetricsError),
                }
            }
            let mut connection = self.connection.clone();
            let replies: Vec<redis::Value> = pipe
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)?;
            replies
                .into_iter()
                .map(|value| match value {
                    redis::Value::Nil => Ok(MetricValue::Missing),
                    redis::Value::Int(v) => Ok(MetricValue::Integer(v)),
                    redis::Value::BulkString(v) => String::from_utf8(v)
                        .map(MetricValue::Text)
                        .map_err(|_| MetricsError),
                    redis::Value::SimpleString(v) => Ok(MetricValue::Text(v)),
                    redis::Value::Okay => Ok(MetricValue::Text("OK".into())),
                    _ => Err(MetricsError),
                })
                .collect()
        })
    }
    fn latency_samples(&self) -> MetricsFuture<'_, Vec<MetricValue>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let samples: Vec<String> = redis::cmd("ZRANGE")
                .arg("metrics:latency:samples")
                .arg(0)
                .arg(-1)
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)?;
            Ok(samples.into_iter().map(MetricValue::Text).collect())
        })
    }
}
