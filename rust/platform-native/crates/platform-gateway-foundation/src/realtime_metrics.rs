//! Source port of `ai_gateway_core.metrics.realtime_metrics` (the gateway path
//! is a compatibility shim). Constructors/singleton initialization are inert.
//! Redis command order, partial snapshots, inclusive windows, tenant/user key
//! quirks, counter fallback and the token single-writer no-op are preserved.
//!
//! Redis and local wall-clock/UUID/sleep collaborators are injected. Explicit
//! start owns one broadcast task; stop cancels and drains it, and owner drop
//! cancels it. Callback failures are isolated, with no replay or detached tasks.
//! The production backend accepts an already-connected Redis manager and an
//! explicit decode-responses mode; it never connects or selects a database.
//!
//! Boundary: signed i64 counters and results. Overflow becomes the source's
//! collection-failure path. Opaque collaborator errors exclude payloads/secrets.
//! Python logging is represented by best-effort outcomes, not copied exception
//! text. Thread maps retain insertion order for the source's stable top-ten cut.
use crate::{
    metrics_recorder::{MetricCommand, MetricValue, MetricsError, MetricsFuture, RecordOutcome},
    usage_parser::{decimal_digit, python_float_string, python_value_to_string, python_whitespace},
};
use chrono::{Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use tokio::task::JoinHandle;

pub const TTL_5M: i64 = 300;
pub const TTL_1H: i64 = 3600;
pub const TTL_24H: i64 = 86400;
pub const WINDOW_MINUTES: i64 = 5;
pub type RealtimeSleep<'a> = Pin<Box<dyn Future<Output = Result<(), MetricsError>> + Send + 'a>>;

pub trait RealtimeRuntime: Send + Sync {
    fn timestamp(&self) -> f64;
    /// Naive process-local wall clock, like datetime.now(), at microsecond precision.
    fn local_now(&self) -> NaiveDateTime;
    fn error_suffix(&self) -> String;
    fn sleep(&self, seconds: f64) -> RealtimeSleep<'_>;
}
pub struct SystemRealtimeRuntime;
impl RealtimeRuntime for SystemRealtimeRuntime {
    fn timestamp(&self) -> f64 {
        chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0
    }
    fn local_now(&self) -> NaiveDateTime {
        let now = Local::now().naive_local();
        now.with_nanosecond(now.nanosecond() / 1000 * 1000).unwrap()
    }
    fn error_suffix(&self) -> String {
        uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
    }
    fn sleep(&self, seconds: f64) -> RealtimeSleep<'_> {
        Box::pin(async move {
            if seconds <= 0.0 {
                tokio::task::yield_now().await;
            } else {
                let duration =
                    std::time::Duration::try_from_secs_f64(seconds).map_err(|_| MetricsError)?;
                tokio::time::sleep(duration).await;
            }
            Ok(())
        })
    }
}
/// Scalar/list replies preserve text versus bytes. withscores only affects the
/// production Redis request: the source uses only the scored list's length.
pub trait RealtimeRedis: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    fn execute<'a>(&'a self, commands: &'a [MetricCommand]) -> MetricsFuture<'a, Vec<MetricValue>>;
    fn get<'a>(&'a self, key: &'a str) -> MetricsFuture<'a, MetricValue>;
    fn zrangebyscore<'a>(
        &'a self,
        key: &'a str,
        minimum: f64,
        maximum: f64,
        withscores: bool,
    ) -> MetricsFuture<'a, Vec<MetricValue>>;
    fn zcount<'a>(&'a self, key: &'a str, minimum: f64, maximum: f64) -> MetricsFuture<'a, i64>;
    fn zcard<'a>(&'a self, key: &'a str) -> MetricsFuture<'a, i64>;
    fn set_queue_depth(&self, depth: i64) -> MetricsFuture<'_, ()>;
}
/// Both synchronous and asynchronous callbacks use this seam. Returning Err is
/// an ordinary Python Exception; cancellation is future drop, never an error.
pub trait RealtimeSubscriber: Send + Sync {
    fn call(&self, snapshot: Value) -> MetricsFuture<'_, ()>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RealtimeSnapshot {
    pub rps: f64,
    pub rps_1m: f64,
    pub rps_5m: f64,
    pub latency_p50: i64,
    pub latency_p95: i64,
    pub latency_p99: i64,
    pub latency_avg: i64,
    pub error_rate: f64,
    pub error_rate_4xx: f64,
    pub error_rate_5xx: f64,
    pub active_users: i64,
    pub total_threads: i64,
    pub threads_by_user: Map<String, Value>,
    pub queue_depth: i64,
    pub concurrent_requests: i64,
    pub max_concurrent: i64,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub token_cost_usd: f64,
    pub tokens_per_minute: f64,
    pub total_runs: i64,
    pub run_success_rate: f64,
    pub avg_run_duration_ms: i64,
    pub timestamp: String,
}
impl Default for RealtimeSnapshot {
    fn default() -> Self {
        Self {
            rps: 0.0,
            rps_1m: 0.0,
            rps_5m: 0.0,
            latency_p50: 0,
            latency_p95: 0,
            latency_p99: 0,
            latency_avg: 0,
            error_rate: 0.0,
            error_rate_4xx: 0.0,
            error_rate_5xx: 0.0,
            active_users: 0,
            total_threads: 0,
            threads_by_user: Map::new(),
            queue_depth: 0,
            concurrent_requests: 0,
            max_concurrent: 100,
            total_tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            token_cost_usd: 0.0,
            tokens_per_minute: 0.0,
            total_runs: 0,
            run_success_rate: 100.0,
            avg_run_duration_ms: 0,
            timestamp: String::new(),
        }
    }
}
impl RealtimeSnapshot {
    pub fn to_dict(&self) -> Value {
        let mut users: Vec<_> = self.threads_by_user.iter().collect();
        users.sort_by_key(|a| std::cmp::Reverse(a.1.as_i64()));
        let users: Map<String, Value> = users
            .into_iter()
            .take(10)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        json!({"rps":round(self.rps,2),"rps_1m":round(self.rps_1m,2),"rps_5m":round(self.rps_5m,2),
            "latency":{"p50":self.latency_p50,"p95":self.latency_p95,"p99":self.latency_p99,"avg":self.latency_avg},
            "errors":{"rate":round(self.error_rate,2),"rate_4xx":round(self.error_rate_4xx,2),"rate_5xx":round(self.error_rate_5xx,2)},
            "users":{"active":self.active_users,"threads_total":self.total_threads,"threads_by_user":users},
            "capacity":{"queue_depth":self.queue_depth,"concurrent":self.concurrent_requests,"max_concurrent":self.max_concurrent,
                "utilization":round(integer_ratio(self.concurrent_requests,self.max_concurrent.max(1))*100.0,1)},
            "tokens":{"total":self.total_tokens,"input":self.input_tokens,"output":self.output_tokens,"cost_usd":round(self.token_cost_usd,4),"per_minute":round(self.tokens_per_minute,1)},
            "runs":{"total":self.total_runs,"success_rate":self.run_success_rate,"avg_duration_ms":self.avg_run_duration_ms},"timestamp":self.timestamp})
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowRequest {
    pub time: f64,
    pub status: i64,
    pub latency: f64,
}
struct Inner {
    redis: RwLock<Option<Arc<dyn RealtimeRedis>>>,
    runtime: Arc<dyn RealtimeRuntime>,
    subscribers: Mutex<Vec<Arc<dyn RealtimeSubscriber>>>,
    running: AtomicBool,
    concurrent: AtomicI64,
    max_concurrent: AtomicI64,
}
#[derive(Default)]
struct BroadcastState {
    task: Option<JoinHandle<Result<(), MetricsError>>>,
    stopped_error: bool,
}
pub struct RealtimeMetricsService {
    inner: Arc<Inner>,
    broadcast: tokio::sync::Mutex<BroadcastState>,
}
impl Default for RealtimeMetricsService {
    fn default() -> Self {
        Self::new(None, Arc::new(SystemRealtimeRuntime))
    }
}
impl RealtimeMetricsService {
    pub fn new(redis: Option<Arc<dyn RealtimeRedis>>, runtime: Arc<dyn RealtimeRuntime>) -> Self {
        Self {
            inner: Arc::new(Inner {
                redis: RwLock::new(redis),
                runtime,
                subscribers: Mutex::new(vec![]),
                running: AtomicBool::new(false),
                concurrent: AtomicI64::new(0),
                max_concurrent: AtomicI64::new(100),
            }),
            broadcast: tokio::sync::Mutex::new(BroadcastState::default()),
        }
    }
    pub fn set_redis(&self, redis: Arc<dyn RealtimeRedis>) {
        *self.inner.redis.write().unwrap() = Some(redis);
    }
    pub fn set_max_concurrent(&self, maximum: i64) {
        self.inner.max_concurrent.store(maximum, Ordering::SeqCst);
    }
    pub fn concurrent_requests(&self) -> i64 {
        self.inner.concurrent.load(Ordering::SeqCst)
    }
    pub fn is_broadcast_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }
    pub fn subscribe(&self, callback: Arc<dyn RealtimeSubscriber>) {
        let mut callbacks = self.inner.subscribers.lock().unwrap();
        if !callbacks.iter().any(|c| Arc::ptr_eq(c, &callback)) {
            callbacks.push(callback);
        }
    }
    pub fn unsubscribe(&self, callback: &Arc<dyn RealtimeSubscriber>) {
        self.inner
            .subscribers
            .lock()
            .unwrap()
            .retain(|c| !Arc::ptr_eq(c, callback));
    }
    pub async fn start_broadcast(&self, interval_seconds: f64) {
        let mut state = self.broadcast.lock().await;
        if self.inner.running.load(Ordering::SeqCst) {
            return;
        }
        // Retain a canceled stop's handle until it has actually terminated.
        if let Some(task) = state.task.as_mut() {
            task.abort();
            let _ = task.await;
        }
        state.task = None;
        state.stopped_error = false;
        self.inner.running.store(true, Ordering::SeqCst);
        let inner = self.inner.clone();
        state.task = Some(tokio::spawn(async move {
            while inner.running.load(Ordering::SeqCst) {
                inner.broadcast_once().await;
                inner.runtime.sleep(interval_seconds).await?;
            }
            Ok(())
        }));
    }
    pub async fn stop_broadcast(&self) -> Result<(), MetricsError> {
        let mut state = self.broadcast.lock().await;
        self.inner.running.store(false, Ordering::SeqCst);
        if let Some(task) = state.task.as_mut() {
            task.abort();
            // Await by reference: canceling this stop future leaves ownership in
            // state, so another stop/start drains it rather than detaching it.
            let result = match task.await {
                Ok(result) => result,
                Err(e) if e.is_cancelled() => Ok(()),
                Err(_) => Err(MetricsError),
            };
            state.task = None;
            state.stopped_error = result.is_err();
            result
        } else if state.stopped_error {
            Err(MetricsError)
        } else {
            Ok(())
        }
    }
    /// Deterministic one-pass seam: same snapshot then captured subscriber list.
    pub async fn broadcast_once(&self) {
        self.inner.broadcast_once().await;
    }
    pub async fn record_request_start(
        &self,
        request_id: &str,
        user_id: &str,
        tenant_id: &str,
    ) -> RecordOutcome {
        if self
            .inner
            .concurrent
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
            .is_err()
        {
            return RecordOutcome::CollectionError;
        }
        let Some(redis) = self.inner.client() else {
            return RecordOutcome::Unavailable;
        };
        let now = self.inner.runtime.timestamp();
        if !now.is_finite() {
            return RecordOutcome::CollectionError;
        }
        let mut commands = vec![
            command(
                "zadd",
                vec![json!("metrics:rt:active_requests"), json!({request_id:now})],
            ),
            command(
                "expire",
                vec![json!("metrics:rt:active_requests"), json!(TTL_5M)],
            ),
        ];
        if !user_id.is_empty() {
            let member = tenant_user_member(tenant_id, user_id);
            let key = thread_key(user_id, tenant_id);
            commands.extend([
                command(
                    "zadd",
                    vec![json!("metrics:rt:active_users"), json!({member:now})],
                ),
                command(
                    "expire",
                    vec![json!("metrics:rt:active_users"), json!(TTL_5M)],
                ),
                command("incr", vec![json!(key)]),
                command("expire", vec![json!(key), json!(TTL_5M)]),
            ]);
        }
        outcome(redis.execute(&commands).await)
    }
    pub async fn record_request_end(
        &self,
        request_id: &str,
        user_id: &str,
        tenant_id: &str,
    ) -> RecordOutcome {
        let _ = self
            .inner
            .concurrent
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some((n - 1).max(0)));
        let Some(redis) = self.inner.client() else {
            return RecordOutcome::Unavailable;
        };
        let mut commands = vec![command(
            "zrem",
            vec![json!("metrics:rt:active_requests"), json!(request_id)],
        )];
        if !user_id.is_empty() {
            commands.push(command("decr", vec![json!(thread_key(user_id, tenant_id))]));
        }
        outcome(redis.execute(&commands).await)
    }
    pub async fn record_request_complete(
        &self,
        status_code: i64,
        duration_ms: f64,
        _user_id: &str,
    ) -> RecordOutcome {
        let Some(redis) = self.inner.client() else {
            return RecordOutcome::Unavailable;
        };
        let now = self.inner.runtime.timestamp();
        if !now.is_finite() {
            return RecordOutcome::CollectionError;
        }
        let member = format!(
            "{}:{status_code}:{}",
            float_text(now),
            float_text(duration_ms)
        );
        let mut commands = vec![
            command(
                "zadd",
                vec![json!("metrics:rt:requests_window"), json!({member:now})],
            ),
            command(
                "zremrangebyscore",
                vec![
                    json!("metrics:rt:requests_window"),
                    json!(0),
                    json!(now - 300.0),
                ],
            ),
        ];
        if status_code >= 400 {
            let key = if status_code < 500 {
                "metrics:rt:errors_4xx"
            } else {
                "metrics:rt:errors_5xx"
            };
            let id = format!("{}:{}", float_text(now), self.inner.runtime.error_suffix());
            commands.extend([
                command("zadd", vec![json!(key), json!({id:now})]),
                command(
                    "zremrangebyscore",
                    vec![json!(key), json!(0), json!(now - 300.0)],
                ),
                command("expire", vec![json!(key), json!(TTL_5M)]),
            ]);
        }
        commands.push(command(
            "expire",
            vec![json!("metrics:rt:requests_window"), json!(TTL_5M)],
        ));
        outcome(redis.execute(&commands).await)
    }
    pub async fn update_queue_depth(&self, depth: i64) -> RecordOutcome {
        let Some(redis) = self.inner.client() else {
            return RecordOutcome::Unavailable;
        };
        outcome(redis.set_queue_depth(depth).await)
    }
    /// Intentionally never touches Redis: MetricsRecorder is the sole writer.
    pub async fn record_token_usage(&self, _input_tokens: i64, _output_tokens: i64) {}
    pub async fn get_realtime_snapshot(&self) -> RealtimeSnapshot {
        self.inner.snapshot().await
    }
    pub async fn get_window_data(&self, now: f64) -> Vec<WindowRequest> {
        self.inner.window(now).await
    }
    pub async fn get_user_metrics(&self, user_id: &str, tenant_id: &str) -> Value {
        let Some(redis) = self.inner.client() else {
            return json!({});
        };
        let read=async {
            let today=self.inner.runtime.local_now().format("%Y-%m-%d").to_string();
            let prefix=if tenant_id.is_empty() {format!("metrics:tokens:user:{user_id}")} else {format!("metrics:tokens:tenant:{tenant_id}:user:{user_id}")};
            let commands=[command("get",vec![json!(format!("{prefix}:input:{today}"))]),command("get",vec![json!(format!("{prefix}:output:{today}"))]),command("get",vec![json!(thread_key(user_id,tenant_id))])];
            let results=redis.execute(&commands).await?;
            let input=integer(results.first().ok_or(MetricsError)?)?; let output=integer(results.get(1).ok_or(MetricsError)?)?;
            let threads=integer(results.get(2).ok_or(MetricsError)?)?.max(0);
            Ok::<_,MetricsError>(json!({"user_id":user_id,"tokens":{"input":input,"output":output,"total":input.checked_add(output).ok_or(MetricsError)?},"active_threads":threads,"timestamp":local_iso(self.inner.runtime.local_now())}))
        }.await;
        read.unwrap_or_else(|_| json!({}))
    }
}
impl Drop for RealtimeMetricsService {
    fn drop(&mut self) {
        self.inner.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.broadcast.get_mut().task.take() {
            task.abort();
        }
    }
}
impl Inner {
    fn client(&self) -> Option<Arc<dyn RealtimeRedis>> {
        self.redis.read().unwrap().clone().filter(|r| r.available())
    }
    async fn broadcast_once(&self) {
        let snapshot = self.snapshot().await;
        let callbacks = self.subscribers.lock().unwrap().clone();
        for callback in callbacks {
            let _ = callback.call(snapshot.to_dict()).await;
        }
    }
    async fn snapshot(&self) -> RealtimeSnapshot {
        let mut snapshot = RealtimeSnapshot {
            timestamp: local_iso(self.runtime.local_now()),
            ..RealtimeSnapshot::default()
        };
        let Some(redis) = self.client() else {
            snapshot.concurrent_requests = self.concurrent.load(Ordering::SeqCst);
            snapshot.max_concurrent = self.max_concurrent.load(Ordering::SeqCst);
            return snapshot;
        };
        // Preserve assignments before an ordinary exception, rather than resetting
        // everything to defaults when a later operation fails.
        let _ = self.populate(&*redis, &mut snapshot).await;
        snapshot
    }
    async fn populate(
        &self,
        redis: &dyn RealtimeRedis,
        snapshot: &mut RealtimeSnapshot,
    ) -> Result<(), MetricsError> {
        let now = self.runtime.timestamp();
        let window = self.window(now).await;
        snapshot.rps = calculate_rps(&window, now, 1);
        snapshot.rps_1m = calculate_rps(&window, now, 60);
        snapshot.rps_5m = calculate_rps(&window, now, 300);
        let mut latencies: Vec<f64> = window
            .iter()
            .filter(|d| now - d.time <= 60.0)
            .map(|d| d.latency)
            .collect();
        if !latencies.is_empty() {
            // Any nonfinite latency makes Python int(sum(...)/n) fail; avoid
            // passing a non-total NaN ordering to the Rust sort implementation.
            if latencies.iter().any(|v| !v.is_finite()) {
                return Err(MetricsError);
            }
            latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = latencies.len();
            snapshot.latency_avg = trunc_i64(python_float_sum(&latencies) / n as f64)?;
            snapshot.latency_p50 = trunc_i64(latencies[(n as f64 * 0.5) as usize])?;
            snapshot.latency_p95 = trunc_i64(latencies[((n as f64 * 0.95) as usize).min(n - 1)])?;
            snapshot.latency_p99 = trunc_i64(latencies[((n as f64 * 0.99) as usize).min(n - 1)])?;
        }
        let total = latencies.len() as i64;
        let errors4 = redis
            .zcount("metrics:rt:errors_4xx", now - 60.0, now)
            .await?;
        let errors5 = redis
            .zcount("metrics:rt:errors_5xx", now - 60.0, now)
            .await?;
        if total > 0 {
            snapshot.error_rate =
                integer_ratio(errors4.checked_add(errors5).ok_or(MetricsError)?, total) * 100.0;
            snapshot.error_rate_4xx = integer_ratio(errors4, total) * 100.0;
            snapshot.error_rate_5xx = integer_ratio(errors5, total) * 100.0;
        }
        let users = redis
            .zrangebyscore("metrics:rt:active_users", now - 300.0, now, false)
            .await?;
        snapshot.active_users = users.len() as i64;
        let mut threads = Map::<String, Value>::new();
        for active in users.iter().take(20) {
            let (tenant, user) = split_tenant_user_member(active);
            if user.is_empty() {
                continue;
            }
            let count = redis.get(&thread_key(&user, &tenant)).await?;
            if truthy(&count) {
                let previous = threads.get(&user).and_then(Value::as_i64).unwrap_or(0);
                threads.insert(
                    user,
                    json!(
                        previous
                            .checked_add(integer(&count)?.max(0))
                            .ok_or(MetricsError)?
                    ),
                );
            }
        }
        snapshot.threads_by_user = threads;
        snapshot.total_threads = snapshot.threads_by_user.values().try_fold(0i64, |sum, v| {
            sum.checked_add(v.as_i64().unwrap()).ok_or(MetricsError)
        })?;
        snapshot.queue_depth = integer(&redis.get("metrics:rt:queue_depth").await?)?;
        let active = redis.zcard("metrics:rt:active_requests").await?;
        snapshot.concurrent_requests = if active == 0 {
            self.concurrent.load(Ordering::SeqCst)
        } else {
            active
        };
        snapshot.max_concurrent = self.max_concurrent.load(Ordering::SeqCst);
        let today = self.runtime.local_now().format("%Y-%m-%d").to_string();
        let commands: Vec<_> = [
            "tokens:input",
            "tokens:output",
            "tokens:cost",
            "tokens:cost_micro",
            "runs:total",
            "runs:success",
            "runs:duration:sum",
            "runs:duration:count",
        ]
        .iter()
        .map(|suffix| command("get", vec![json!(format!("metrics:{suffix}:{today}"))]))
        .collect();
        let results = redis.execute(&commands).await?;
        let nums: Vec<i64> = (0..8)
            .map(|i| integer(results.get(i).ok_or(MetricsError)?))
            .collect::<Result<_, _>>()?;
        snapshot.input_tokens = nums[0];
        snapshot.output_tokens = nums[1];
        snapshot.total_tokens = nums[0].checked_add(nums[1]).ok_or(MetricsError)?;
        snapshot.token_cost_usd = if nums[3] > 0 {
            crate::billing::microcents_to_usd(nums[3])
                .to_string()
                .parse()
                .map_err(|_| MetricsError)?
        } else {
            round(integer_ratio(nums[2], 100), 6)
        };
        let token_now = self.runtime.timestamp();
        match redis
            .zrangebyscore(
                "metrics:rt:requests_window",
                token_now - 300.0,
                token_now,
                true,
            )
            .await
        {
            Ok(recent) if !recent.is_empty() && nums[4] > 0 => {
                snapshot.tokens_per_minute = round(
                    (recent.len() as f64 * integer_ratio(snapshot.total_tokens, nums[4]))
                        / WINDOW_MINUTES as f64,
                    1,
                );
            }
            _ => {
                let now = self.runtime.local_now();
                let elapsed = (now.num_seconds_from_midnight() as f64
                    + now.nanosecond() as f64 / 1_000_000_000.0)
                    / 60.0;
                snapshot.tokens_per_minute =
                    round(snapshot.total_tokens as f64 / elapsed.max(1.0), 1);
            }
        }
        snapshot.total_runs = nums[4];
        snapshot.run_success_rate = round(
            if nums[4] > 0 {
                integer_ratio(nums[5], nums[4]) * 100.0
            } else {
                100.0
            },
            1,
        );
        snapshot.avg_run_duration_ms = if nums[7] > 0 {
            trunc_i64(integer_ratio(nums[6], nums[7]))?
        } else {
            0
        };
        Ok(())
    }
    async fn window(&self, now: f64) -> Vec<WindowRequest> {
        let Some(redis) = self.client() else {
            return vec![];
        };
        let Ok(entries) = redis
            .zrangebyscore("metrics:rt:requests_window", now - 300.0, now, false)
            .await
        else {
            return vec![];
        };
        let mut result = vec![];
        for entry in entries {
            // Python bytes.split(":") raises TypeError outside the per-entry
            // ValueError/IndexError handler, discarding all previously parsed rows.
            let MetricValue::Text(entry) = entry else {
                return vec![];
            };
            let parts: Vec<_> = entry.split(':').collect();
            if parts.len() < 3 {
                continue;
            }
            let (Some(time), Ok(status), Some(latency)) = (
                parse_float(parts[0]),
                integer(&MetricValue::Text(parts[1].to_owned())),
                parse_float(parts[2]),
            ) else {
                continue;
            };
            // int("") must fail here, unlike int(redis_value or 0).
            if parts[1].is_empty() {
                continue;
            }
            result.push(WindowRequest {
                time,
                status,
                latency,
            });
        }
        result
    }
}
pub fn calculate_rps(window: &[WindowRequest], now: f64, window_seconds: i64) -> f64 {
    let cutoff = now - window_seconds as f64;
    let count = window.iter().filter(|d| d.time >= cutoff).count();
    if window_seconds > 0 {
        integer_ratio(count as i64, window_seconds)
    } else {
        0.0
    }
}
pub fn tenant_user_member(tenant_id: &str, user_id: &str) -> String {
    if tenant_id.is_empty() {
        user_id.into()
    } else {
        format!("{tenant_id}:{user_id}")
    }
}
pub fn thread_key(user_id: &str, tenant_id: &str) -> String {
    if tenant_id.is_empty() {
        format!("metrics:rt:threads:{user_id}")
    } else {
        format!("metrics:rt:threads:tenant:{tenant_id}:user:{user_id}")
    }
}
pub fn split_tenant_user_member(member: &MetricValue) -> (String, String) {
    let value = match member {
        MetricValue::Missing => String::new(),
        MetricValue::Text(s) => s.clone(),
        MetricValue::Bytes(b) => utf8_ignore(b),
        MetricValue::Integer(0) => String::new(),
        MetricValue::Integer(n) => n.to_string(),
    };
    match value.split_once(':') {
        Some((a, b)) => (a.into(), b.into()),
        None => (String::new(), value),
    }
}
fn command(op: &str, args: Vec<Value>) -> MetricCommand {
    MetricCommand {
        op: op.into(),
        args,
    }
}
fn outcome<T>(result: Result<T, MetricsError>) -> RecordOutcome {
    if result.is_ok() {
        RecordOutcome::Recorded
    } else {
        RecordOutcome::CollectionError
    }
}
fn round(value: f64, places: usize) -> f64 {
    format!("{value:.places$}")
        .parse()
        .expect("float formatting")
}
fn float_text(value: f64) -> String {
    if value.is_nan() {
        "nan".into()
    } else if value == f64::INFINITY {
        "inf".into()
    } else if value == f64::NEG_INFINITY {
        "-inf".into()
    } else {
        python_value_to_string(&json!(value))
    }
}
fn local_iso(now: NaiveDateTime) -> String {
    if now.nanosecond() == 0 {
        now.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}
fn truthy(value: &MetricValue) -> bool {
    match value {
        MetricValue::Missing => false,
        MetricValue::Integer(v) => *v != 0,
        MetricValue::Text(v) => !v.is_empty(),
        MetricValue::Bytes(v) => !v.is_empty(),
    }
}
fn parse_float(text: &str) -> Option<f64> {
    if text.chars().any(|c| matches!(c, '\u{1c}'..='\u{1f}')) {
        None
    } else {
        python_float_string(text)
    }
}
fn trunc_i64(value: f64) -> Result<i64, MetricsError> {
    let value = value.trunc();
    if value.is_finite()
        && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value)
    {
        Ok(value as i64)
    } else {
        Err(MetricsError)
    }
}
fn integer(value: &MetricValue) -> Result<i64, MetricsError> {
    fn parse(text: &str, bytes: bool) -> Result<i64, MetricsError> {
        if text.chars().any(|c| matches!(c, '\u{1c}'..='\u{1f}')) {
            return Err(MetricsError);
        }
        let text = if bytes {
            text.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}'))
        } else {
            text.trim_matches(python_whitespace)
        };
        let mut normalized = String::new();
        let mut chars = text.chars().peekable();
        if matches!(chars.peek(), Some('+' | '-')) {
            normalized.push(chars.next().unwrap());
        }
        let mut previous = false;
        let mut any = false;
        while let Some(c) = chars.next() {
            if c == '_' {
                if !previous || chars.peek().and_then(|c| decimal_digit(*c)).is_none() {
                    return Err(MetricsError);
                }
                previous = false;
            } else if let Some(d) = decimal_digit(c) {
                normalized.push(char::from(b'0' + d));
                previous = true;
                any = true;
            } else {
                return Err(MetricsError);
            }
        }
        if !any || !previous {
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
fn integer_ratio(numerator: i64, denominator: i64) -> f64 {
    debug_assert_ne!(denominator, 0);
    if numerator == 0 {
        return if denominator < 0 { -0.0 } else { 0.0 };
    }
    let a = u128::from(numerator.unsigned_abs());
    let b = u128::from(denominator.unsigned_abs());
    let mut exponent = (127 - a.leading_zeros()) as i32 - (127 - b.leading_zeros()) as i32;
    if if exponent >= 0 {
        a < (b << exponent)
    } else {
        (a << -exponent) < b
    } {
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
                result.push_str(std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap());
                match e.error_len() {
                    Some(n) => rest = &rest[e.valid_up_to() + n..],
                    None => break,
                }
            }
        }
    }
    result
}
static REALTIME_SERVICE: OnceLock<Arc<RealtimeMetricsService>> = OnceLock::new();
pub fn get_realtime_metrics() -> Arc<RealtimeMetricsService> {
    REALTIME_SERVICE
        .get_or_init(|| Arc::new(RealtimeMetricsService::default()))
        .clone()
}
pub fn init_realtime_metrics(redis: Arc<dyn RealtimeRedis>) -> Arc<RealtimeMetricsService> {
    let service = get_realtime_metrics();
    service.set_redis(redis);
    service
}

/// No connection establishment, background worker, retry or database selection.
pub struct RedisRealtimeBackend {
    connection: redis::aio::ConnectionManager,
    decode_responses: bool,
}
impl RedisRealtimeBackend {
    pub fn new(connection: redis::aio::ConnectionManager, decode_responses: bool) -> Self {
        Self {
            connection,
            decode_responses,
        }
    }
    fn value(&self, value: redis::Value) -> Result<MetricValue, MetricsError> {
        match value {
            redis::Value::Nil => Ok(MetricValue::Missing),
            redis::Value::Int(n) => Ok(MetricValue::Integer(n)),
            redis::Value::BulkString(bytes) => {
                if self.decode_responses {
                    String::from_utf8(bytes)
                        .map(MetricValue::Text)
                        .map_err(|_| MetricsError)
                } else {
                    Ok(MetricValue::Bytes(bytes))
                }
            }
            redis::Value::SimpleString(s) => Ok(MetricValue::Text(s)),
            redis::Value::Okay => Ok(MetricValue::Text("OK".into())),
            _ => Err(MetricsError),
        }
    }
}
impl RealtimeRedis for RedisRealtimeBackend {
    fn execute<'a>(&'a self, commands: &'a [MetricCommand]) -> MetricsFuture<'a, Vec<MetricValue>> {
        Box::pin(async move {
            let mut pipe = redis::pipe();
            pipe.atomic();
            for command in commands {
                match command.op.as_str() {
                    "zadd" => {
                        pipe.cmd("ZADD").arg(
                            command
                                .args
                                .first()
                                .and_then(Value::as_str)
                                .ok_or(MetricsError)?,
                        );
                        for (member, score) in command
                            .args
                            .get(1)
                            .and_then(Value::as_object)
                            .ok_or(MetricsError)?
                        {
                            pipe.arg(score.as_f64().ok_or(MetricsError)?).arg(member);
                        }
                    }
                    "get" | "incr" | "decr" | "expire" | "zrem" | "zremrangebyscore" => {
                        pipe.cmd(&command.op.to_uppercase());
                        for arg in &command.args {
                            match arg {
                                Value::String(s) => {
                                    pipe.arg(s);
                                }
                                Value::Number(n) => {
                                    if let Some(n) = n.as_i64() {
                                        pipe.arg(n);
                                    } else {
                                        pipe.arg(n.as_f64().ok_or(MetricsError)?);
                                    }
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
            replies.into_iter().map(|v| self.value(v)).collect()
        })
    }
    fn get<'a>(&'a self, key: &'a str) -> MetricsFuture<'a, MetricValue> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let reply = redis::cmd("GET")
                .arg(key)
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)?;
            self.value(reply)
        })
    }
    fn zrangebyscore<'a>(
        &'a self,
        key: &'a str,
        minimum: f64,
        maximum: f64,
        withscores: bool,
    ) -> MetricsFuture<'a, Vec<MetricValue>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let mut command = redis::cmd("ZRANGEBYSCORE");
            command.arg(key).arg(minimum).arg(maximum);
            if withscores {
                command.arg("WITHSCORES");
            }
            let values: Vec<redis::Value> = command
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)?;
            if withscores {
                scored_members(values)?
                    .into_iter()
                    .map(|v| self.value(v))
                    .collect()
            } else {
                values.into_iter().map(|v| self.value(v)).collect()
            }
        })
    }
    fn zcount<'a>(&'a self, key: &'a str, minimum: f64, maximum: f64) -> MetricsFuture<'a, i64> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            redis::cmd("ZCOUNT")
                .arg(key)
                .arg(minimum)
                .arg(maximum)
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)
        })
    }
    fn zcard<'a>(&'a self, key: &'a str) -> MetricsFuture<'a, i64> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            redis::cmd("ZCARD")
                .arg(key)
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)
        })
    }
    fn set_queue_depth(&self, depth: i64) -> MetricsFuture<'_, ()> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            redis::cmd("SET")
                .arg("metrics:rt:queue_depth")
                .arg(depth)
                .arg("EX")
                .arg(TTL_5M)
                .query_async(&mut connection)
                .await
                .map_err(|_| MetricsError)
        })
    }
}

// Redis RESP2 flattens scored pairs; RESP3 nests them. Validate score decoding
// even though the service needs only the list length, as redis-py does.
fn scored_members(values: Vec<redis::Value>) -> Result<Vec<redis::Value>, MetricsError> {
    let mut result = vec![];
    let mut values = values.into_iter();
    while let Some(value) = values.next() {
        let (member, score) = match value {
            redis::Value::Array(pair) => {
                if pair.len() != 2 {
                    return Err(MetricsError);
                }
                let mut pair = pair.into_iter();
                (pair.next().unwrap(), pair.next().unwrap())
            }
            member => (member, values.next().ok_or(MetricsError)?),
        };
        let _: f64 = redis::from_redis_value(&score).map_err(|_| MetricsError)?;
        result.push(member);
    }
    Ok(result)
}
// CPython 3.12+ sum(float) uses compensated accumulation. Goldens execute on
// the source harness's Python 3.12; this also avoids changing latency averages
// when small samples appear between large opposite-sign values.
pub(crate) fn python_float_sum(values: &[f64]) -> f64 {
    let mut hi = 0.0f64;
    let mut lo = 0.0f64;
    for &value in values {
        let sum = hi + value;
        lo += if hi.abs() >= value.abs() {
            (hi - sum) + value
        } else {
            (value - sum) + hi
        };
        hi = sum;
    }
    if lo != 0.0 && lo.is_finite() {
        hi + lo
    } else {
        hi
    }
}

