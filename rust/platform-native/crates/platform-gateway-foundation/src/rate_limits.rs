//! Reusable, unmounted Gateway rate-limit and quota-counter contracts.
//!
//! Authorities: `src/core/gateway/lua_scripts.py`,
//! `src/core/gateway/multi_dimension_rate_limiter.py`,
//! `src/core/gateway/rate_policy.py`, `src/core/middleware/rate_limit_http.py`,
//! and `src/services/billing/quota_service.py`.
//!
//! Redis executes the existing sliding-window script, with seconds as scores
//! and a sliding `window + 1` second TTL. Admission is atomic but NOT rollback:
//! dimensions preceding a rejected dimension remain charged. Redis failure
//! falls back to process-local memory, not a fail-open unlimited result. Memory
//! expires scores strictly before the boundary; Redis expires through it.
//! These legacy differences are intentional. Redis Cluster cross-slot support
//! is not added: the Python keys have no shared hash tag either.
//!
//! Quota RPM remains process-local and tenant/user scoped. Persistent token,
//! cost (ordinary cents), and daily-request counters remain PostgreSQL-owned.
//! No connection creation, environment reads, HTTP wiring, quota resets,
//! background cleanup, provider calls, or financial settlement happen here.

use crate::billing::{MinuteQuotaCheck, normalize_limit};
use redis::{Script, aio::ConnectionManager};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Verbatim Lua authority; keep the source-parity test when changing it.
pub const SLIDING_WINDOW_CHECK_LUA: &str = r#"
local min_remaining = nil
local remaining_values = {}
for i = 1, #KEYS do
  redis.call('ZREMRANGEBYSCORE', KEYS[i], 0, ARGV[2])
  local count = redis.call('ZCARD', KEYS[i])
  if count >= tonumber(ARGV[4 + i]) then
    local earliest = redis.call('ZRANGE', KEYS[i], 0, 0, 'WITHSCORES')
    local earliest_score = tonumber(ARGV[2])
    if earliest[2] then
      earliest_score = tonumber(earliest[2])
    end
    local response = {i - 1, earliest_score, 0}
    for j = 1, #KEYS do
      table.insert(response, remaining_values[j] or -1)
    end
    return response
  end
  redis.call('ZADD', KEYS[i], ARGV[1], ARGV[4])
  redis.call('EXPIRE', KEYS[i], ARGV[3])
  local remaining = tonumber(ARGV[4 + i]) - count - 1
  remaining_values[i] = remaining
  if min_remaining == nil or remaining < min_remaining then
    min_remaining = remaining
  end
end
local response = {-1, 0, min_remaining or 0}
for i = 1, #KEYS do
  table.insert(response, remaining_values[i] or 0)
end
return response
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitError {
    InvalidClock,
    InvalidWindow,
    IntegerOverflow,
    MissingAnonymousTier,
    MixedWindows,
    InvalidRedisReply,
    RedisUnavailable,
}

impl fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidClock => "invalid_rate_limit_clock",
            Self::InvalidWindow => "invalid_rate_limit_window",
            Self::IntegerOverflow => "rate_limit_integer_overflow",
            Self::MissingAnonymousTier => "missing_anonymous_rate_limit_tier",
            Self::MixedWindows => "custom rate-limit batch must share one window",
            Self::InvalidRedisReply => "invalid_rate_limit_reply",
            Self::RedisUnavailable => "rate_limit_store_unavailable",
        })
    }
}
impl std::error::Error for RateLimitError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatePolicy {
    pub key: String,
    pub dimension: String,
    pub requests: i64,
    pub window: i64,
}
impl RatePolicy {
    pub fn new(
        key: impl Into<String>,
        dimension: impl Into<String>,
        requests: i64,
        window: i64,
    ) -> Self {
        Self {
            key: key.into(),
            dimension: dimension.into(),
            requests,
            window,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitResult {
    pub allowed: bool,
    pub dimension: String,
    pub limit: i64,
    pub remaining: i64,
    pub reset_at: i64,
    pub retry_after: i64,
}
impl Default for RateLimitResult {
    fn default() -> Self {
        Self {
            allowed: true,
            dimension: String::new(),
            limit: 0,
            remaining: 0,
            reset_at: 0,
            retry_after: 0,
        }
    }
}
impl RateLimitResult {
    pub fn headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::from([
            ("X-RateLimit-Limit".into(), self.limit.to_string()),
            ("X-RateLimit-Remaining".into(), self.remaining.to_string()),
            ("X-RateLimit-Reset".into(), self.reset_at.to_string()),
            ("X-RateLimit-Dimension".into(), self.dimension.clone()),
        ]);
        if self.retry_after > 0 {
            headers.insert("Retry-After".into(), self.retry_after.to_string());
        }
        headers
    }
    pub fn exceeded_response(&self) -> Value {
        json!({"error": {"code": "RATE_LIMIT_EXCEEDED", "message": "Too many requests",
            "dimension": self.dimension, "limit": self.limit, "remaining": 0,
            "reset_at": self.reset_at, "retry_after": self.retry_after}})
    }
}

/// HTTP middleware's index is -1 for an allowed result, as in Python.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitInfo {
    pub result: RateLimitResult,
    pub dimension_index: i64,
}

/// The exact script request. Callers inject a store, not a Redis URL or secret.
#[derive(Debug, Clone)]
pub struct SlidingWindowBatch {
    pub policies: Vec<RatePolicy>,
    pub now_seconds: f64,
    pub window_start_seconds: f64,
    pub expire_seconds: i64,
    pub member: String,
}
pub type SlidingWindowFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<f64>, RateLimitError>> + Send + 'a>>;
pub trait SlidingWindowStore: Send + Sync {
    fn check<'a>(&'a self, batch: &'a SlidingWindowBatch) -> SlidingWindowFuture<'a>;
}

/// EVALSHA on a warm script, with redis-rs SCRIPT LOAD/NOSCRIPT recovery.
/// The caller owns connection creation and its connection/response timeouts.
pub struct RedisSlidingWindowStore {
    connection: ConnectionManager,
    script: Script,
}
impl RedisSlidingWindowStore {
    pub fn new(connection: ConnectionManager) -> Self {
        Self {
            connection,
            script: Script::new(SLIDING_WINDOW_CHECK_LUA),
        }
    }
}
impl SlidingWindowStore for RedisSlidingWindowStore {
    fn check<'a>(&'a self, batch: &'a SlidingWindowBatch) -> SlidingWindowFuture<'a> {
        Box::pin(async move {
            let mut invocation = self.script.prepare_invoke();
            for policy in &batch.policies {
                invocation.key(&policy.key);
            }
            invocation
                .arg(batch.now_seconds)
                .arg(batch.window_start_seconds)
                .arg(batch.expire_seconds)
                .arg(&batch.member);
            for policy in &batch.policies {
                invocation.arg(policy.requests);
            }
            invocation
                .invoke_async(&mut self.connection.clone())
                .await
                .map_err(|_| RateLimitError::RedisUnavailable)
        })
    }
}

fn validate(now: f64, policies: &[RatePolicy]) -> Result<(), RateLimitError> {
    // Keep conversion/TTL arithmetic defined rather than silently saturating.
    if !now.is_finite() || now < 0.0 || now >= i64::MAX as f64 {
        return Err(RateLimitError::InvalidClock);
    }
    for policy in policies {
        if policy.window <= 0
            || policy.window == i64::MAX
            || now + policy.window as f64 >= i64::MAX as f64
        {
            return Err(RateLimitError::InvalidWindow);
        }
    }
    Ok(())
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn wall_time() -> Result<f64, RateLimitError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs_f64())
        .map_err(|_| RateLimitError::InvalidClock)
}

#[derive(Default)]
struct MemoryWindows {
    requests: HashMap<String, VecDeque<f64>>,
}
impl MemoryWindows {
    fn check(&mut self, policy: &RatePolicy, now: f64) -> RateLimitResult {
        let start = now - policy.window as f64;
        let timestamps = self.requests.entry(policy.key.clone()).or_default();
        while timestamps.front().is_some_and(|score| *score < start) {
            timestamps.pop_front();
        }
        let count = timestamps.len() as i64;
        let mut result = RateLimitResult {
            dimension: policy.dimension.clone(),
            limit: policy.requests,
            reset_at: (now + policy.window as f64) as i64,
            ..Default::default()
        };
        if count >= policy.requests {
            result.allowed = false;
            result.retry_after = timestamps
                .front()
                .map_or(policy.window, |score| (*score - start) as i64 + 1);
        } else {
            timestamps.push_back(now);
            result.remaining = policy.requests - count - 1;
        }
        result
    }
    fn check_many(&mut self, policies: &[RatePolicy], now: f64) -> Vec<RateLimitResult> {
        let mut results = Vec::new();
        for policy in policies {
            let result = self.check(policy, now);
            let allowed = result.allowed;
            results.push(result);
            if !allowed {
                break;
            }
        }
        results
    }
}

/// Reuses one local fallback across calls. Sharing an Arc shares that fallback;
/// constructing independent instances intentionally does not share local counts.
/// Process-local logical Redis script count, matching GatewayHotPathMetrics.
/// SCRIPT LOAD/NOSCRIPT retries are deliberately not separate source increments.
pub struct GatewayHotPathMetrics {
    redis_round_trips: AtomicU64,
}
impl GatewayHotPathMetrics {
    pub const fn new() -> Self {
        Self {
            redis_round_trips: AtomicU64::new(0),
        }
    }
    pub fn redis_round_trips(&self) -> u64 {
        self.redis_round_trips.load(Ordering::Relaxed)
    }
    pub fn reset(&self) {
        self.redis_round_trips.store(0, Ordering::Relaxed);
    }
    fn record_success(&self) {
        self.redis_round_trips.fetch_add(1, Ordering::Relaxed);
    }
}
impl Default for GatewayHotPathMetrics {
    fn default() -> Self {
        Self::new()
    }
}
pub static GATEWAY_HOT_PATH_METRICS: GatewayHotPathMetrics = GatewayHotPathMetrics::new();

pub struct SlidingWindowRateLimiter {
    store: Option<Arc<dyn SlidingWindowStore>>,
    memory: Mutex<MemoryWindows>,
    successful_redis_batches: AtomicU64,
    fallback_batches: AtomicU64,
}
impl Default for SlidingWindowRateLimiter {
    fn default() -> Self {
        Self::new(None)
    }
}
impl SlidingWindowRateLimiter {
    pub fn new(store: Option<Arc<dyn SlidingWindowStore>>) -> Self {
        Self {
            store,
            memory: Mutex::new(MemoryWindows::default()),
            successful_redis_batches: AtomicU64::new(0),
            fallback_batches: AtomicU64::new(0),
        }
    }
    /// Logical successful Redis invocations, not transport RTTs (NOSCRIPT can add RTTs).
    pub fn successful_redis_batches(&self) -> u64 {
        self.successful_redis_batches.load(Ordering::Relaxed)
    }
    pub fn fallback_batches(&self) -> u64 {
        self.fallback_batches.load(Ordering::Relaxed)
    }

    async fn redis_batch(
        &self,
        policies: &[RatePolicy],
        now: f64,
        member: &str,
    ) -> Result<Vec<RateLimitResult>, RateLimitError> {
        let window = policies[0].window;
        let batch = SlidingWindowBatch {
            policies: policies.to_vec(),
            now_seconds: now,
            window_start_seconds: now - window as f64,
            expire_seconds: window + 1,
            member: member.into(),
        };
        let wire = self
            .store
            .as_ref()
            .ok_or(RateLimitError::RedisUnavailable)?
            .check(&batch)
            .await?;
        GATEWAY_HOT_PATH_METRICS.record_success();
        self.successful_redis_batches
            .fetch_add(1, Ordering::Relaxed);
        decode_reply(&wire, policies, now)
    }

    /// Custom policies normalize nonpositive limits/windows to one, as Python does.
    /// All policies must have the same normalized window. Results stop at rejection.
    pub async fn check_custom_limits_at(
        &self,
        policies: &[RatePolicy],
        now: f64,
    ) -> Result<Vec<RateLimitResult>, RateLimitError> {
        let normalized: Vec<_> = policies
            .iter()
            .cloned()
            .map(|mut policy| {
                policy.requests = policy.requests.max(1);
                policy.window = policy.window.max(1);
                policy
            })
            .collect();
        validate(now, &normalized)?;
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        if normalized
            .iter()
            .any(|policy| policy.window != normalized[0].window)
        {
            return Err(RateLimitError::MixedWindows);
        }
        Ok(self.batch_with_fallback(&normalized, now).await)
    }
    pub async fn check_custom_limits(
        &self,
        policies: &[RatePolicy],
    ) -> Result<Vec<RateLimitResult>, RateLimitError> {
        self.check_custom_limits_at(policies, wall_time()?).await
    }
    async fn batch_with_fallback(&self, policies: &[RatePolicy], now: f64) -> Vec<RateLimitResult> {
        if self.store.is_some() {
            if let Ok(results) = self
                .redis_batch(policies, now, &uuid::Uuid::new_v4().to_string())
                .await
            {
                return results;
            }
            self.fallback_batches.fetch_add(1, Ordering::Relaxed);
        }
        lock(&self.memory).check_many(policies, now)
    }
    /// The route-level Python limiter checks each dimension separately, in order.
    /// Pass already-counted middleware dimensions to `MultiDimensionConfig::policies`.
    pub async fn check_dimensions_at(
        &self,
        policies: &[RatePolicy],
        now: f64,
    ) -> Result<RateLimitResult, RateLimitError> {
        validate(now, policies)?;
        let mut allowed = RateLimitResult::default();
        for policy in policies {
            let result = self
                .batch_with_fallback(std::slice::from_ref(policy), now)
                .await
                .remove(0);
            if !result.allowed {
                return Ok(result);
            }
            if result.limit > 0 {
                allowed = result;
            }
        }
        Ok(allowed)
    }

    /// One script per distinct window for HTTP checks. Python iterates a set of
    /// windows, whose order is unspecified; Rust uses first occurrence order.
    /// A failed group falls back over the ENTIRE input, matching Python's catch.
    pub async fn check_http_many_at(
        &self,
        policies: &[RatePolicy],
        now: f64,
    ) -> Result<RateLimitInfo, RateLimitError> {
        validate(now, policies)?;
        let allowed = || RateLimitInfo {
            result: RateLimitResult {
                limit: policies.last().map_or(0, |policy| policy.requests),
                reset_at: (now + policies.last().map_or(0, |policy| policy.window) as f64) as i64,
                ..Default::default()
            },
            dimension_index: -1,
        };
        if policies.is_empty() {
            return Ok(allowed());
        }
        if self.store.is_some() {
            let mut seen = BTreeSet::new();
            let mut failed = false;
            let member = uuid::Uuid::new_v4().to_string();
            for policy in policies {
                if !seen.insert(policy.window) {
                    continue;
                }
                let indices: Vec<_> = policies
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.window == policy.window)
                    .map(|(i, _)| i)
                    .collect();
                let group: Vec<_> = indices
                    .iter()
                    .map(|index| policies[*index].clone())
                    .collect();
                match self.redis_batch(&group, now, &member).await {
                    Ok(results) => {
                        if let Some((index, result)) = results
                            .into_iter()
                            .enumerate()
                            .find(|(_, result)| !result.allowed)
                        {
                            return Ok(RateLimitInfo {
                                result: RateLimitResult {
                                    dimension: String::new(),
                                    ..result
                                },
                                dimension_index: indices[index] as i64,
                            });
                        }
                    }
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            if !failed {
                return Ok(allowed());
            }
            self.fallback_batches.fetch_add(1, Ordering::Relaxed);
        }
        for (index, result) in lock(&self.memory)
            .check_many(policies, now)
            .into_iter()
            .enumerate()
        {
            if !result.allowed {
                return Ok(RateLimitInfo {
                    result: RateLimitResult {
                        dimension: String::new(),
                        ..result
                    },
                    dimension_index: index as i64,
                });
            }
        }
        Ok(allowed())
    }
    pub async fn check_http_many(
        &self,
        policies: &[RatePolicy],
    ) -> Result<RateLimitInfo, RateLimitError> {
        self.check_http_many_at(policies, wall_time()?).await
    }
}

fn decode_reply(
    wire: &[f64],
    policies: &[RatePolicy],
    now: f64,
) -> Result<Vec<RateLimitResult>, RateLimitError> {
    if wire.len() < 2
        || wire.iter().any(|value| {
            !value.is_finite() || *value >= i64::MAX as f64 || *value < i64::MIN as f64
        })
    {
        return Err(RateLimitError::InvalidRedisReply);
    }
    let rejected = wire[0] as i64;
    if rejected < -1 || rejected >= policies.len() as i64 {
        return Err(RateLimitError::InvalidRedisReply);
    }
    let mut results = Vec::new();
    for (index, policy) in policies.iter().enumerate() {
        let mut result = RateLimitResult {
            dimension: policy.dimension.clone(),
            limit: policy.requests,
            reset_at: (now + policy.window as f64) as i64,
            ..Default::default()
        };
        if index as i64 == rejected {
            let start = now - policy.window as f64;
            result.allowed = false;
            result.retry_after = if wire[1] > start {
                (wire[1] - start) as i64 + 1
            } else {
                policy.window
            };
            results.push(result);
            break;
        }
        result.remaining = wire
            .get(3 + index)
            .filter(|value| **value >= 0.0)
            .or_else(|| wire.get(2))
            .map_or_else(
                || policy.requests.saturating_sub(1).max(0),
                |value| (*value as i64).max(0),
            );
        results.push(result);
    }
    Ok(results)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierLimit {
    pub requests: i64,
    pub window: i64,
    pub burst: i64,
}
impl TierLimit {
    pub fn new(requests: i64, window: i64) -> Self {
        Self {
            requests,
            window,
            burst: 0,
        }
    }
}
#[derive(Debug, Clone)]
pub struct RateLimitContext {
    pub ip: String,
    pub user_id: Option<String>,
    pub user_tier: String,
    pub tenant_id: Option<String>,
    pub assistant_id: Option<String>,
    pub operation: Option<String>,
}
impl Default for RateLimitContext {
    fn default() -> Self {
        Self {
            ip: String::new(),
            user_id: None,
            user_tier: "anonymous".into(),
            tenant_id: None,
            assistant_id: None,
            operation: None,
        }
    }
}
/// Environment-independent defaults. The caller must apply its runtime settings.
#[derive(Clone)]
pub struct MultiDimensionConfig {
    pub global: Option<TierLimit>,
    pub ip: Option<TierLimit>,
    pub user_tiers: BTreeMap<String, TierLimit>,
    pub tenant: Option<TierLimit>,
    pub tenant_limits: BTreeMap<String, i64>,
    pub assistant: Option<TierLimit>,
    pub assistant_limits: BTreeMap<String, i64>,
    pub operations: BTreeMap<String, TierLimit>,
}
impl Default for MultiDimensionConfig {
    fn default() -> Self {
        Self {
            global: Some(TierLimit::new(10000, 60)),
            ip: Some(TierLimit::new(30, 60)),
            user_tiers: [
                ("anonymous", 10),
                ("normal", 60),
                ("premium", 300),
                ("enterprise", 1000),
                ("admin", 10000),
            ]
            .into_iter()
            .map(|(tier, limit)| (tier.into(), TierLimit::new(limit, 60)))
            .collect(),
            tenant: Some(TierLimit::new(5000, 60)),
            tenant_limits: BTreeMap::new(),
            assistant: Some(TierLimit::new(1000, 60)),
            assistant_limits: BTreeMap::new(),
            operations: [
                ("run_create", 100),
                ("thread_create", 50),
                ("store_write", 200),
                ("assistant_chat", 60),
                ("quiz_generate", 10),
                ("image_generate", 20),
                ("file_upload", 30),
                ("code_execute", 30),
                ("quiz_submit_public", 10),
            ]
            .into_iter()
            .map(|(operation, limit)| (operation.into(), TierLimit::new(limit, 60)))
            .collect(),
        }
    }
}
fn nonempty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}
impl MultiDimensionConfig {
    /// Legacy builtin user/assistant keys are NOT tenant-prefixed. Changing them
    /// would create a second authority alongside the still-running Python code.
    pub fn policies(
        &self,
        context: &RateLimitContext,
        skip: &BTreeSet<String>,
    ) -> Result<Vec<RatePolicy>, RateLimitError> {
        let mut policies = Vec::new();
        let mut add = |dimension: &str,
                       key: String,
                       limit: Option<TierLimit>|
         -> Result<(), RateLimitError> {
            if !skip.contains(dimension)
                && let Some(limit) = limit
            {
                policies.push(RatePolicy::new(
                    key,
                    dimension,
                    limit
                        .requests
                        .checked_add(limit.burst)
                        .ok_or(RateLimitError::IntegerOverflow)?,
                    limit.window,
                ));
            }
            Ok(())
        };
        add(
            "global",
            "ratelimit:global".into(),
            self.global.map(|mut limit| {
                limit.burst = 0;
                limit
            }),
        )?;
        if !context.ip.is_empty() {
            add(
                "ip",
                format!("ratelimit:ip:{}", context.ip),
                self.ip.map(|mut limit| {
                    limit.burst = 0;
                    limit
                }),
            )?;
        }
        if let Some(user) = nonempty(&context.user_id)
            && !skip.contains("user")
        {
            let anonymous = self
                .user_tiers
                .get("anonymous")
                .ok_or(RateLimitError::MissingAnonymousTier)?;
            add(
                "user",
                format!("ratelimit:user:{user}"),
                Some(*self.user_tiers.get(&context.user_tier).unwrap_or(anonymous)),
            )?;
        }
        if let Some(tenant) = nonempty(&context.tenant_id) {
            add(
                "tenant",
                format!("ratelimit:tenant:{tenant}"),
                self.tenant.map(|mut limit| {
                    limit.requests = self
                        .tenant_limits
                        .get(tenant)
                        .copied()
                        .unwrap_or(limit.requests);
                    limit.burst = 0;
                    limit
                }),
            )?;
        }
        if let Some(assistant) = nonempty(&context.assistant_id) {
            add(
                "assistant",
                format!("ratelimit:assistant:{assistant}"),
                self.assistant.map(|mut limit| {
                    limit.requests = self
                        .assistant_limits
                        .get(assistant)
                        .copied()
                        .unwrap_or(limit.requests);
                    limit.burst = 0;
                    limit
                }),
            )?;
        }
        if let Some(operation) = nonempty(&context.operation)
            && !skip.contains("operation")
            && let Some(limit) = self.operations.get(operation)
        {
            policies.push(RatePolicy::new(
                format!(
                    "ratelimit:op:{operation}:{}",
                    nonempty(&context.user_id).unwrap_or(&context.ip)
                ),
                format!("operation:{operation}"),
                limit
                    .requests
                    .checked_add(limit.burst)
                    .ok_or(RateLimitError::IntegerOverflow)?,
                limit.window,
            ));
        }
        Ok(policies)
    }
}

fn safe_segment(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "unknown".into()
    } else {
        value.replace([':', '|'], "_")
    }
}
/// RatePolicyResolver's service/rule key contract. `target` is the already
/// resolved scope_id or dimension. Resolving DB rules/cache precedence is separate.
pub fn scoped_policy_key(scope: &str, target: &str, context: &RateLimitContext) -> String {
    let tenant = safe_segment(nonempty(&context.tenant_id).unwrap_or("public"));
    let subject = safe_segment(nonempty(&context.user_id).unwrap_or_else(|| {
        if context.ip.is_empty() {
            "anonymous"
        } else {
            &context.ip
        }
    }));
    let operation = safe_segment(nonempty(&context.operation).unwrap_or("proxy"));
    format!(
        "ratelimit:{scope}:{}:{tenant}:{subject}:{operation}",
        safe_segment(target)
    )
}

/// QuotaService's independent, single-node RPM counter. No Redis key is invented.
pub struct QuotaRpmLimiter {
    origin: Instant,
    requests: Mutex<HashMap<(String, String), VecDeque<f64>>>,
}
impl Default for QuotaRpmLimiter {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            requests: Mutex::new(HashMap::new()),
        }
    }
}
impl QuotaRpmLimiter {
    pub fn check(&self, tenant: &str, user: &str, limit: Option<i64>) -> MinuteQuotaCheck {
        self.check_valid_at(tenant, user, limit, self.origin.elapsed().as_secs_f64())
    }
    pub fn check_at(
        &self,
        tenant: &str,
        user: &str,
        limit: Option<i64>,
        now: f64,
    ) -> Result<MinuteQuotaCheck, RateLimitError> {
        validate(now, &[])?;
        Ok(self.check_valid_at(tenant, user, limit, now))
    }
    fn check_valid_at(
        &self,
        tenant: &str,
        user: &str,
        limit: Option<i64>,
        now: f64,
    ) -> MinuteQuotaCheck {
        let Some(limit) = normalize_limit(limit) else {
            return MinuteQuotaCheck::default();
        };
        let mut requests = lock(&self.requests);
        let timestamps = requests.entry((tenant.into(), user.into())).or_default();
        while timestamps.front().is_some_and(|score| *score < now - 60.0) {
            timestamps.pop_front();
        }
        let count = timestamps.len() as i64;
        if count >= limit {
            return MinuteQuotaCheck {
                allowed: false,
                used: count,
                retry_after_seconds: timestamps
                    .front()
                    .map_or(60, |score| ((*score + 60.0 - now).max(1.0)) as i64),
            };
        }
        timestamps.push_back(now);
        MinuteQuotaCheck {
            allowed: true,
            used: count + 1,
            retry_after_seconds: 0,
        }
    }
}

/// QuotaService.update_usage uses ordinary cents, never the usage-recorder's
/// legacy microcents. Arithmetic is one UPDATE, scoped by BOTH tenant and user.
pub const UPDATE_QUOTA_USAGE_SQL: &str = "UPDATE user_quotas SET current_daily_tokens = current_daily_tokens + $3, current_monthly_tokens = current_monthly_tokens + $3, current_monthly_cost_cents = current_monthly_cost_cents + $4, current_daily_requests = current_daily_requests + 1, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = $1 AND user_id = $2";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaUsageUpdate {
    Disabled,
    Updated { rows: u64 },
    Unavailable,
}
/// Missing pool is a no-op; SQL errors remain nonfatal, matching Python. The
/// explicit outcome lets a caller log/measure degradation without exposing SQL.
/// This is not idempotent: only invoke once per settled request; do not retry an
/// uncertain write automatically. Negative adjustments follow the Python API.
pub async fn update_quota_usage(
    pool: Option<&PgPool>,
    tenant: &str,
    user: &str,
    tokens: i64,
    cost_cents: i64,
) -> QuotaUsageUpdate {
    let Some(pool) = pool else {
        return QuotaUsageUpdate::Disabled;
    };
    match sqlx::query(UPDATE_QUOTA_USAGE_SQL)
        .bind(tenant)
        .bind(user)
        .bind(tokens)
        .bind(cost_cents)
        .execute(pool)
        .await
    {
        Ok(result) => QuotaUsageUpdate::Updated {
            rows: result.rows_affected(),
        },
        Err(_) => QuotaUsageUpdate::Unavailable,
    }
}
