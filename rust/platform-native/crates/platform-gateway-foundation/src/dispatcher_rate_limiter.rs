//! Legacy dispatcher limiter from `src/core/gateway/rate_limiter.py`.
//!
//! This is distinct from the Redis/multi-dimensional HTTP limiter. Checks charge
//! sequentially, without rollback after a later rejection. User, service, and IP
//! keys deliberately lack tenant scope, matching the source rather than silently
//! changing its policy. `strategy` is descriptive: all checks use a sliding window.
//!
//! Source limitations retained: `max_entries` is stored but not enforced; cleanup
//! uses only top-level configured windows (at least 60 seconds), not per-service
//! new/legacy windows; empty rejected buckets survive cleanup. Policy resolution
//! happens in the service adapter: `configured` means the new format is enabled.
//! No Redis, HTTP binding, service lookup, or environment reads happen here.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    pub requests: i64,
    pub window: i64,
    #[serde(default)]
    pub burst: i64,
    #[serde(default = "sliding_window")]
    pub strategy: String,
}
fn sliding_window() -> String {
    "sliding_window".into()
}
impl RateLimit {
    pub fn new(requests: i64, window: i64) -> Self {
        Self {
            requests,
            window,
            burst: 0,
            strategy: sliding_window(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub global_limit: Option<RateLimit>,
    pub tenant_limits: BTreeMap<String, RateLimit>,
    pub user_limits: BTreeMap<String, RateLimit>,
    pub service_limits: BTreeMap<String, RateLimit>,
    pub ip_limits: Option<RateLimit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceRateLimits {
    pub service_id: String,
    /// Populate only when get_service_config().rate_limit.enabled is true.
    pub configured: Option<RateLimit>,
    /// Populate for a truthy legacy service.rate_limit dictionary.
    pub legacy: Option<RateLimit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitExceeded {
    pub key: String,
}
impl fmt::Display for RateLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rate limit exceeded for {}", self.key)
    }
}
impl std::error::Error for RateLimitExceeded {}

struct State {
    requests: BTreeMap<String, VecDeque<f64>>,
    last_cleanup: f64,
}
pub struct RateLimiter {
    pub config: RateLimitConfig,
    deferred: bool,
    /// Compatibility field only; Python does not enforce this bound.
    pub max_entries: usize,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    // Each check and cleanup is atomic, with no awaits inside the critical
    // section. Releasing between checks preserves partial sequential charging.
    state: Mutex<State>,
}
impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(
            config,
            10_000,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|time| time.as_secs_f64())
                    .unwrap_or_else(|error| -error.duration().as_secs_f64())
            }),
        )
    }

    /// Production dispatch consumes authenticated policy with capacity/token
    /// admission. Only the process-local IP shield remains in preflight.
    pub fn deferred(config: RateLimitConfig) -> Self {
        let mut limiter = Self::new(config);
        limiter.deferred = true;
        limiter
    }

    pub fn with_clock(
        config: RateLimitConfig,
        max_entries: usize,
        clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) -> Self {
        let last_cleanup = clock();
        Self {
            config,
            deferred: false,
            max_entries,
            clock,
            state: Mutex::new(State {
                requests: BTreeMap::new(),
                last_cleanup,
            }),
        }
    }

    fn cleanup(&self, state: &mut State, now: f64) {
        if now - state.last_cleanup < 60.0 {
            return;
        }
        let max_window = self
            .config
            .global_limit
            .iter()
            .chain(self.config.ip_limits.iter())
            .chain(self.config.tenant_limits.values())
            .chain(self.config.user_limits.values())
            .chain(self.config.service_limits.values())
            .map(|limit| limit.window)
            .max()
            .unwrap_or(60)
            .max(60);
        let cutoff = now - max_window as f64;
        state
            .requests
            .retain(|_, timestamps| !timestamps.back().is_some_and(|last| *last < cutoff));
        state.last_cleanup = now;
    }

    async fn check_key(&self, key: String, limit: &RateLimit) -> Result<(), RateLimitExceeded> {
        // Read separately for each dimension, just like Python _check_key.
        let now = (self.clock)();
        let mut state = self.state.lock().await;
        self.cleanup(&mut state, now);
        let timestamps = state.requests.entry(key.clone()).or_default();
        let window_start = now - limit.window as f64;
        while timestamps.front().is_some_and(|time| *time < window_start) {
            timestamps.pop_front();
        }
        // A wider sum preserves Python addition for every pair of i64 inputs.
        let allowed = i128::from(limit.requests) + i128::from(limit.burst);
        if timestamps.len() as i128 >= allowed {
            return Err(RateLimitExceeded { key });
        }
        timestamps.push_back(now);
        Ok(())
    }

    pub async fn enforce(
        &self,
        tenant_id: &str,
        user_id: &str,
        service: &ServiceRateLimits,
        client_ip: Option<&str>,
    ) -> Result<(), RateLimitExceeded> {
        if self.deferred {
            if let Some(ip) = client_ip.filter(|ip| !ip.is_empty())
                && let Some(limit) = &self.config.ip_limits
            {
                self.check_key(format!("ip:{ip}"), limit).await?;
            }
            return Ok(());
        }
        let mut checks: Vec<(String, &RateLimit)> = Vec::new();
        if let Some(limit) = &self.config.global_limit {
            checks.push(("global".into(), limit));
        }
        if !tenant_id.is_empty()
            && let Some(limit) = self.config.tenant_limits.get(tenant_id)
        {
            checks.push((format!("tenant:{tenant_id}"), limit));
        }
        if !user_id.is_empty()
            && let Some(limit) = self.config.user_limits.get(user_id)
        {
            checks.push((format!("user:{user_id}"), limit));
        }
        if !service.service_id.is_empty()
            && let Some(limit) = self.config.service_limits.get(&service.service_id)
        {
            checks.push((format!("service:{}", service.service_id), limit));
        }
        if let Some(ip) = client_ip.filter(|ip| !ip.is_empty())
            && let Some(limit) = &self.config.ip_limits
        {
            checks.push((format!("ip:{ip}"), limit));
        }
        if let Some(limit) = &service.configured {
            checks.push((format!("service:{}:config", service.service_id), limit));
        } else if let Some(limit) = &service.legacy {
            checks.push((format!("service:{}:local", service.service_id), limit));
        }
        for (key, limit) in checks {
            self.check_key(key, limit).await?;
        }
        Ok(())
    }

    /// Read-only diagnostics; like Python's stored state, this does not expire it.
    pub async fn snapshot(&self) -> BTreeMap<String, Vec<f64>> {
        self.state
            .lock()
            .await
            .requests
            .iter()
            .map(|(key, timestamps)| (key.clone(), timestamps.iter().copied().collect()))
            .collect()
    }
}
