//! Cached rate-policy resolution from `src/core/gateway/rate_policy.py`.
//!
//! Enabled positive service configuration replaces all loaded rules. Otherwise
//! rules are stably ordered by scope, then ascending priority, and matched to
//! verified caller context. The cache holds immutable rule snapshots, never
//! tenant-specific resolved policies. Every key retains tenant/subject/operation.
//!
//! Fresh TTL defaults to one second, with five additional seconds of bounded
//! stale reads and a single background refresh. Beyond that budget, failures
//! propagate; DB failures are debounced for one second. Invalidation cancels a
//! background refresh and prevents old in-flight work from repopulating cache.
//! This module creates no connections, reads no environment, mounts no routes,
//! and does not create/update/delete policy configuration.

use crate::rate_limits::{RateLimitContext, RatePolicy, scoped_policy_key};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    fmt,
    future::Future,
    ops::Deref,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatePolicyError {
    Unavailable,
    InvalidRule,
    IntegerOverflow,
}
impl fmt::Display for RatePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "rate_policy_source_unavailable",
            Self::InvalidRule => "invalid_rate_policy_rule",
            Self::IntegerOverflow => "rate_policy_integer_overflow",
        })
    }
}
impl std::error::Error for RatePolicyError {}

pub type RuleSourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Value>, RatePolicyError>> + Send + 'a>>;
pub trait RateRuleSource: Send + Sync {
    fn load_rules(&self) -> RuleSourceFuture<'_>;
}

/// Matches DatabaseStorage.get_rate_limits(enabled=True). Its initial descending
/// priority order is intentionally re-sorted ascending by the resolver, as Python.
pub const SELECT_RATE_RULES_SQL: &str = "SELECT jsonb_build_object('scope',scope,'scope_id',scope_id,'requests',requests,'window_seconds',window_seconds,'burst',burst,'strategy',strategy,'enabled',enabled,'priority',priority) FROM rate_limit_config WHERE enabled = TRUE ORDER BY priority DESC, scope";
pub struct PgRateRuleSource {
    pool: PgPool,
}
impl PgRateRuleSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
impl RateRuleSource for PgRateRuleSource {
    fn load_rules(&self) -> RuleSourceFuture<'_> {
        Box::pin(async move {
            sqlx::query_scalar(SELECT_RATE_RULES_SQL)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| RatePolicyError::Unavailable)
        })
    }
}

/// Select this source when Python's DB is disabled/unavailable as an object.
/// An actual enabled DB query failure must NOT switch to runtime rules.
#[derive(Default)]
pub struct RuntimeRateRules {
    rules: Mutex<Vec<Value>>,
}
impl RuntimeRateRules {
    pub fn new(rules: Vec<Value>) -> Self {
        Self {
            rules: Mutex::new(rules),
        }
    }
    /// Snapshot the same runtime source used by policy resolution.
    pub fn snapshot(&self) -> Vec<Value> {
        lock(&self.rules).clone()
    }
    /// Synchronous mutation under the existing lock avoids read/replace races.
    /// The caller must publish the source and invalidate the resolver afterward.
    pub fn update<T>(&self, update: impl FnOnce(&mut Vec<Value>) -> T) -> T {
        update(&mut lock(&self.rules))
    }
    /// Call resolver.invalidate() after changing runtime rules, as the API does.
    pub fn replace(&self, rules: Vec<Value>) {
        *lock(&self.rules) = rules;
    }
}
impl RateRuleSource for RuntimeRateRules {
    fn load_rules(&self) -> RuleSourceFuture<'_> {
        Box::pin(async { Ok(lock(&self.rules).clone()) })
    }
}

pub trait PolicyClock: Send + Sync {
    fn now(&self) -> Duration;
}
pub struct MonotonicPolicyClock(Instant);
impl Default for MonotonicPolicyClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl PolicyClock for MonotonicPolicyClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}
#[derive(Debug, Clone, Copy)]
pub struct RatePolicyCacheOptions {
    pub ttl: Duration,
    pub max_stale: Duration,
}
impl Default for RatePolicyCacheOptions {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(1),
            max_stale: Duration::from_secs(5),
        }
    }
}

/// Only pass authenticated/resolved identity, not unverified public headers.
#[derive(Debug, Clone, Default)]
pub struct PolicyContext {
    pub actor: RateLimitContext,
    pub service_name: String,
    pub user_service_id: String,
    pub api_key_hash: String,
    /// Authenticated api_key_info's id, key_id, and api_key_id, stringified.
    pub api_key_ids: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct ServiceRatePolicy {
    pub enabled: bool,
    pub requests: i64,
    pub window: i64,
    pub service_id: String,
}
impl Default for ServiceRatePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            requests: 0,
            window: 0,
            service_id: "service".into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRatePolicy {
    pub policy: RatePolicy,
    pub burst: i64,
    pub strategy: String,
}
impl Deref for ResolvedRatePolicy {
    type Target = RatePolicy;
    fn deref(&self) -> &RatePolicy {
        &self.policy
    }
}

struct Snapshot {
    generation: u64,
    expires_at: Duration,
    stale_until: Duration,
    rules: Arc<[Value]>,
}
struct Failure {
    generation: u64,
    retry_at: Duration,
    error: RatePolicyError,
}
struct CacheState {
    source: Arc<dyn RateRuleSource>,
    generation: u64,
    snapshot: Option<Snapshot>,
    failure: Option<Failure>,
    refresh: Option<JoinHandle<()>>,
}
struct ResolverInner {
    state: Mutex<CacheState>,
    refresh_lock: AsyncMutex<()>,
    options: RatePolicyCacheOptions,
    clock: Arc<dyn PolicyClock>,
}
#[derive(Clone)]
pub struct RatePolicyResolver {
    inner: Arc<ResolverInner>,
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl RatePolicyResolver {
    pub fn new(source: Arc<dyn RateRuleSource>) -> Self {
        Self::with_clock(
            source,
            RatePolicyCacheOptions::default(),
            Arc::new(MonotonicPolicyClock::default()),
        )
    }
    pub fn with_clock(
        source: Arc<dyn RateRuleSource>,
        options: RatePolicyCacheOptions,
        clock: Arc<dyn PolicyClock>,
    ) -> Self {
        Self {
            inner: Arc::new(ResolverInner {
                state: Mutex::new(CacheState {
                    source,
                    generation: 0,
                    snapshot: None,
                    failure: None,
                    refresh: None,
                }),
                refresh_lock: AsyncMutex::new(()),
                options,
                clock,
            }),
        }
    }
    fn invalidate_state(state: &mut CacheState) {
        state.generation = state.generation.wrapping_add(1);
        state.snapshot = None;
        state.failure = None;
        if let Some(task) = state.refresh.take() {
            task.abort();
        }
    }
    pub fn invalidate(&self) {
        Self::invalidate_state(&mut lock(&self.inner.state));
    }
    /// Changing database/source identity invalidates snapshots immediately, like
    /// Python's (id(database), epoch) cache key. Unchanged source need not be set.
    pub fn replace_source(&self, source: Arc<dyn RateRuleSource>) {
        let mut state = lock(&self.inner.state);
        state.source = source;
        Self::invalidate_state(&mut state);
    }
    pub async fn resolve(
        &self,
        context: &PolicyContext,
        service: Option<&ServiceRatePolicy>,
    ) -> Result<Vec<ResolvedRatePolicy>, RatePolicyError> {
        if let Some(policy) = service.and_then(|service| service_policy(context, service)) {
            return Ok(vec![policy]);
        }
        let rules = self.load_rules().await?;
        rules
            .iter()
            .filter_map(|rule| resolve_rule(rule, context).transpose())
            .collect()
    }
    async fn load_rules(&self) -> Result<Arc<[Value]>, RatePolicyError> {
        let now = self.inner.clock.now();
        let (generation, stale) = {
            let state = lock(&self.inner.state);
            if let Some(snapshot) = &state.snapshot
                && snapshot.generation == state.generation
            {
                if now < snapshot.expires_at {
                    return Ok(snapshot.rules.clone());
                }
                if now <= snapshot.stale_until {
                    (state.generation, Some(snapshot.rules.clone()))
                } else {
                    (state.generation, None)
                }
            } else {
                (state.generation, None)
            }
        };
        if let Some(rules) = stale {
            self.schedule_refresh(generation);
            return Ok(rules);
        }
        self.refresh(generation).await
    }
    fn schedule_refresh(&self, generation: u64) {
        let mut state = lock(&self.inner.state);
        if state.generation != generation
            || state
                .refresh
                .as_ref()
                .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let resolver = self.clone();
        state.refresh = Some(tokio::spawn(async move {
            let _ = resolver.refresh(generation).await;
            let mut state = lock(&resolver.inner.state);
            if state.generation == generation {
                state.refresh = None;
            }
        }));
    }
    async fn refresh(&self, generation: u64) -> Result<Arc<[Value]>, RatePolicyError> {
        let _singleflight = self.inner.refresh_lock.lock().await;
        let now = self.inner.clock.now();
        let source = {
            let state = lock(&self.inner.state);
            if let Some(snapshot) = &state.snapshot
                && snapshot.generation == generation
                && now < snapshot.expires_at
            {
                return Ok(snapshot.rules.clone());
            }
            if let Some(failure) = &state.failure
                && failure.generation == generation
                && now < failure.retry_at
            {
                return Err(failure.error);
            }
            state.source.clone()
        };
        let raw = match source.load_rules().await {
            Ok(rules) => rules,
            Err(error) => {
                let mut state = lock(&self.inner.state);
                if state.generation == generation {
                    state.failure = Some(Failure {
                        generation,
                        retry_at: self
                            .inner
                            .clock
                            .now()
                            .saturating_add(Duration::from_secs(1)),
                        error,
                    });
                }
                return Err(error);
            }
        };
        // A malformed rule/sort key is not a DB-load failure; do not debounce it.
        let compiled = compile_rules(raw)?;
        let refreshed = self.inner.clock.now();
        let mut state = lock(&self.inner.state);
        if state.generation == generation {
            state.failure = None;
            state.snapshot = Some(Snapshot {
                generation,
                expires_at: refreshed.saturating_add(self.inner.options.ttl),
                stale_until: refreshed
                    .saturating_add(self.inner.options.ttl)
                    .saturating_add(self.inner.options.max_stale),
                rules: compiled.clone(),
            });
        }
        Ok(compiled)
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn string_value(value: Option<&Value>, default: &str) -> String {
    match value.filter(|value| truthy(value)) {
        None => default.into(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(value) => value.to_string(),
    }
}
fn integer(value: Option<&Value>) -> Result<i64, RatePolicyError> {
    match value.filter(|value| truthy(value)) {
        None => Ok(0),
        Some(Value::Bool(value)) => Ok(i64::from(*value)),
        Some(Value::Number(value)) if value.is_i64() => Ok(value.as_i64().unwrap()),
        Some(Value::Number(value)) => {
            let value = value.as_f64().ok_or(RatePolicyError::InvalidRule)?;
            if value < i64::MIN as f64 || value >= i64::MAX as f64 {
                Err(RatePolicyError::IntegerOverflow)
            } else {
                Ok(value as i64)
            }
        }
        Some(Value::String(value)) => value
            .trim()
            .parse()
            .map_err(|_| RatePolicyError::InvalidRule),
        _ => Err(RatePolicyError::InvalidRule),
    }
}
fn normalized_scope(rule: &Value) -> String {
    string_value(rule.get("scope"), "").trim().to_lowercase()
}
fn precedence(scope: &str) -> i64 {
    match scope {
        "service" => 10,
        "api_key" => 20,
        "tenant" => 30,
        "user" => 40,
        "operation" => 50,
        "global" => 60,
        _ => 999,
    }
}
fn compile_rules(rules: Vec<Value>) -> Result<Arc<[Value]>, RatePolicyError> {
    let mut keyed = Vec::with_capacity(rules.len());
    for rule in rules {
        let rule = if rule.is_object() { rule } else { json!({}) };
        keyed.push((
            (
                precedence(&normalized_scope(&rule)),
                integer(rule.get("priority"))?,
            ),
            rule,
        ));
    }
    keyed.sort_by_key(|(key, _)| *key);
    Ok(keyed.into_iter().map(|(_, rule)| rule).collect())
}
fn safe_segment(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "unknown".into()
    } else {
        value.replace([':', '|'], "_")
    }
}
fn service_policy(
    context: &PolicyContext,
    config: &ServiceRatePolicy,
) -> Option<ResolvedRatePolicy> {
    if !config.enabled || config.requests <= 0 || config.window <= 0 {
        return None;
    }
    let service = safe_segment(if context.service_name.is_empty() {
        &config.service_id
    } else {
        &context.service_name
    });
    Some(ResolvedRatePolicy {
        policy: RatePolicy::new(
            scoped_policy_key("service", &service, &context.actor),
            format!("service:{service}"),
            config.requests,
            config.window,
        ),
        burst: 0,
        strategy: "sliding_window".into(),
    })
}
fn resolve_rule(
    rule: &Value,
    context: &PolicyContext,
) -> Result<Option<ResolvedRatePolicy>, RatePolicyError> {
    if rule.get("enabled").is_some_and(|enabled| !truthy(enabled)) {
        return Ok(None);
    }
    let scope = normalized_scope(rule);
    let target = string_value(rule.get("scope_id"), "").trim().to_string();
    let operation = context.actor.operation.as_deref().unwrap_or("");
    let matches = match scope.as_str() {
        "global" => true,
        "service" => target == context.service_name || target == context.user_service_id,
        "api_key" => {
            !target.is_empty()
                && (target == context.api_key_hash || context.api_key_ids.contains(&target))
        }
        "tenant" => {
            !target.is_empty() && Some(target.as_str()) == context.actor.tenant_id.as_deref()
        }
        "user" => !target.is_empty() && Some(target.as_str()) == context.actor.user_id.as_deref(),
        "operation" => !target.is_empty() && target == operation,
        _ => false,
    };
    if !matches {
        return Ok(None);
    }
    let requests = integer(rule.get("requests"))?.max(1);
    let window = integer(
        rule.get("window")
            .filter(|value| truthy(value))
            .or_else(|| rule.get("window_seconds")),
    )?
    .max(1);
    let burst = integer(rule.get("burst"))?.max(0);
    let strategy = string_value(rule.get("strategy"), "sliding_window");
    let strategy = if strategy.trim().is_empty() {
        "sliding_window"
    } else {
        strategy.trim()
    };
    let requests = if matches!(strategy, "token_bucket" | "sliding_window_with_burst") {
        requests
            .checked_add(burst)
            .ok_or(RatePolicyError::IntegerOverflow)?
    } else {
        requests
    };
    let dimension = match scope.as_str() {
        "global" => "global".into(),
        "service" => format!(
            "service:{}",
            if target.is_empty() {
                &context.service_name
            } else {
                &target
            }
        ),
        "tenant" => format!("tenant:{target}"),
        "user" => format!("user:{target}"),
        "operation" => format!(
            "operation:{}",
            if target.is_empty() {
                operation
            } else {
                &target
            }
        ),
        _ => scope.clone(),
    };
    Ok(Some(ResolvedRatePolicy {
        policy: RatePolicy::new(
            scoped_policy_key(
                &scope,
                if target.is_empty() {
                    &dimension
                } else {
                    &target
                },
                &context.actor,
            ),
            dimension,
            requests,
            window,
        ),
        burst,
        strategy: strategy.into(),
    }))
}

