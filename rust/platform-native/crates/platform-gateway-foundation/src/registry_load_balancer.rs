//! Port of `src/services/registry/load_balancer.py`.
//!
//! Preserve input order, healthy-only selection with all-unhealthy fallback,
//! per-service round-robin counters, and unknown-strategy round-robin fallback.
//! Returned records borrow the original slice; no instance or metadata is copied.
//! This module does not register, probe, dispatch to, or start any service.
//!
//! Python's string hash is process-randomized and `random` uses CPython's own
//! generator. The native default uses a process-keyed Rust hash and UUID-v4
//! entropy, so hash assignments and random sequences are NOT cross-language
//! compatible, including when Python has a fixed PYTHONHASHSEED/random seed.
//! `LoadBalancerRuntime` can replay Python choice indices, unit samples and signed
//! hash values when exact selections are required. Weighted selection preserves
//! CPython's float sampling and right-bisect boundaries, including large weights.
//! Weights use the native signed-64-bit domain; Python's arbitrary-size integers
//! and annotation-violating objects are outside this typed port's input domain.
use crate::protocol::UnifiedRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, hash_map::RandomState},
    fmt,
    hash::BuildHasher,
    sync::{Arc, Mutex, OnceLock},
};

fn default_weight() -> i64 {
    1
}
fn default_healthy() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ServiceInstance {
    pub instance_id: String,
    pub base_url: String,
    #[serde(default = "default_weight")]
    pub weight: i64,
    #[serde(default = "default_healthy")]
    pub is_healthy: bool,
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
}
impl ServiceInstance {
    pub fn new(instance_id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            base_url: base_url.into(),
            weight: 1,
            is_healthy: true,
            metadata: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadBalancerError {
    EmptyInstances(&'static str),
    InvalidChoiceIndex,
    InvalidUnitSample,
}
impl LoadBalancerError {
    /// The source's empty-input exception category, without panicking in Rust.
    pub fn python_exception_type(&self) -> Option<&'static str> {
        match self {
            Self::EmptyInstances("round_robin" | "consistent_hash") => Some("ZeroDivisionError"),
            Self::EmptyInstances(_) => Some("IndexError"),
            _ => None,
        }
    }
}
impl fmt::Display for LoadBalancerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInstances("round_robin" | "consistent_hash") => {
                f.write_str("integer modulo by zero")
            }
            Self::EmptyInstances("random") => f.write_str("Cannot choose from an empty sequence"),
            Self::EmptyInstances(_) => f.write_str("list index out of range"),
            Self::InvalidChoiceIndex => f.write_str("load_balancer_choice_index_out_of_range"),
            Self::InvalidUnitSample => f.write_str("load_balancer_sample_outside_unit_interval"),
        }
    }
}
impl std::error::Error for LoadBalancerError {}

/// Inject the only platform-dependent operations. `choice_index` is called only
/// with a nonzero count, and must return an index below that count. `random_unit`
/// must return a finite number in [0, 1). `hash_key` is the signed CPython-style
/// hash value, before Python's nonnegative modulo operation.
pub trait LoadBalancerRuntime: Send + Sync {
    fn choice_index(&self, count: usize) -> usize;
    fn random_unit(&self) -> f64;
    fn hash_key(&self, key: &str) -> i64;
}

#[derive(Default)]
pub struct SystemLoadBalancerRuntime;
impl SystemLoadBalancerRuntime {
    fn random_u64() -> u64 {
        let uuid = uuid::Uuid::new_v4();
        let bytes = uuid.as_bytes();
        // Skip bytes 6 and 8: UUID version/variant bits are not random.
        u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[14], bytes[15],
        ])
    }
}
impl LoadBalancerRuntime for SystemLoadBalancerRuntime {
    fn choice_index(&self, count: usize) -> usize {
        let bound = count as u64;
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let draw = Self::random_u64();
            if draw >= threshold {
                return (draw % bound) as usize;
            }
        }
    }
    fn random_unit(&self) -> f64 {
        (Self::random_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }
    fn hash_key(&self, key: &str) -> i64 {
        static PROCESS_HASH: OnceLock<RandomState> = OnceLock::new();
        PROCESS_HASH.get_or_init(RandomState::new).hash_one(key) as i64
    }
}

#[derive(Default)]
pub struct RoundRobinStrategy {
    counters: Mutex<HashMap<String, usize>>,
}
impl RoundRobinStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    fn select_index(&self, service_id: &str, count: usize) -> Result<usize, LoadBalancerError> {
        if count == 0 {
            return Err(LoadBalancerError::EmptyInstances("round_robin"));
        }
        let mut counters = self
            .counters
            .lock()
            .expect("load balancer counters poisoned");
        let index = counters.get(service_id).copied().unwrap_or(0) % count;
        // Source stores idx + 1, not a monotonically increasing counter. This
        // matters when candidate counts change between calls.
        counters.insert(service_id.to_owned(), index + 1);
        Ok(index)
    }
    pub fn select<'a>(
        &self,
        service_id: &str,
        instances: &'a [ServiceInstance],
    ) -> Result<&'a ServiceInstance, LoadBalancerError> {
        Ok(&instances[self.select_index(service_id, instances.len())?])
    }
}

#[derive(Default)]
pub struct RandomStrategy;
impl RandomStrategy {
    fn select_index(
        &self,
        count: usize,
        runtime: &dyn LoadBalancerRuntime,
    ) -> Result<usize, LoadBalancerError> {
        if count == 0 {
            return Err(LoadBalancerError::EmptyInstances("random"));
        }
        let index = runtime.choice_index(count);
        if index >= count {
            return Err(LoadBalancerError::InvalidChoiceIndex);
        }
        Ok(index)
    }
    pub fn select<'a>(
        &self,
        _service_id: &str,
        instances: &'a [ServiceInstance],
        runtime: &dyn LoadBalancerRuntime,
    ) -> Result<&'a ServiceInstance, LoadBalancerError> {
        Ok(&instances[self.select_index(instances.len(), runtime)?])
    }
}

#[derive(Default)]
pub struct WeightedStrategy;
impl WeightedStrategy {
    fn select_index<'a>(
        &self,
        instances: impl Iterator<Item = &'a ServiceInstance>,
        runtime: &dyn LoadBalancerRuntime,
    ) -> Result<usize, LoadBalancerError> {
        let mut total = 0_u128;
        let cumulative: Vec<u128> = instances
            .map(|instance| {
                total += instance.weight.max(1) as u128;
                total
            })
            .collect();
        if cumulative.is_empty() {
            return Err(LoadBalancerError::EmptyInstances("weighted"));
        }
        let unit = runtime.random_unit();
        if !unit.is_finite() || !(0.0..1.0).contains(&unit) {
            return Err(LoadBalancerError::InvalidUnitSample);
        }
        // random.choices adds 0.0 to the integer total, samples in float, then
        // bisects the integer cumulative weights with hi=len(weights)-1.
        // Floor before the integer comparison: converting each boundary to f64
        // would misroute weights above 2**53. The total fits u128 for any native
        // slice of i64 weights, and the sample is finite/nonnegative.
        let sample_floor = (unit * total as f64) as u128;
        let (mut low, mut high) = (0, cumulative.len() - 1);
        while low < high {
            let middle = low + (high - low) / 2;
            if sample_floor < cumulative[middle] {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        Ok(low)
    }
    pub fn select<'a>(
        &self,
        _service_id: &str,
        instances: &'a [ServiceInstance],
        runtime: &dyn LoadBalancerRuntime,
    ) -> Result<&'a ServiceInstance, LoadBalancerError> {
        Ok(&instances[self.select_index(instances.iter(), runtime)?])
    }
}

/// `(session_id or user_id or request_id) if request else service_id`.
/// Empty strings fall through; whitespace is a nonempty key and is not trimmed.
pub fn consistent_hash_key<'a>(
    service_id: &'a str,
    request: Option<&'a UnifiedRequest>,
) -> &'a str {
    match request {
        Some(request) => request
            .session_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| (!request.user_id.is_empty()).then_some(request.user_id.as_str()))
            .unwrap_or(&request.request_id),
        None => service_id,
    }
}

#[derive(Default)]
pub struct ConsistentHashStrategy;
impl ConsistentHashStrategy {
    fn select_index(
        &self,
        service_id: &str,
        count: usize,
        request: Option<&UnifiedRequest>,
        runtime: &dyn LoadBalancerRuntime,
    ) -> Result<usize, LoadBalancerError> {
        let hash = runtime.hash_key(consistent_hash_key(service_id, request));
        if count == 0 {
            return Err(LoadBalancerError::EmptyInstances("consistent_hash"));
        }
        Ok((hash as i128).rem_euclid(count as i128) as usize)
    }
    pub fn select<'a>(
        &self,
        service_id: &str,
        instances: &'a [ServiceInstance],
        request: Option<&UnifiedRequest>,
        runtime: &dyn LoadBalancerRuntime,
    ) -> Result<&'a ServiceInstance, LoadBalancerError> {
        Ok(&instances[self.select_index(service_id, instances.len(), request, runtime)?])
    }
}

pub struct LoadBalancer {
    pub strategy_name: String,
    round_robin: RoundRobinStrategy,
    runtime: Arc<dyn LoadBalancerRuntime>,
}
impl Default for LoadBalancer {
    fn default() -> Self {
        Self::new("round_robin")
    }
}
impl LoadBalancer {
    pub fn new(strategy: impl Into<String>) -> Self {
        Self::with_runtime(strategy, Arc::new(SystemLoadBalancerRuntime))
    }
    pub fn with_runtime(
        strategy: impl Into<String>,
        runtime: Arc<dyn LoadBalancerRuntime>,
    ) -> Self {
        Self {
            strategy_name: strategy.into(),
            round_robin: RoundRobinStrategy::new(),
            runtime,
        }
    }
    pub async fn select_instance<'a>(
        &self,
        service_id: &str,
        instances: &'a [ServiceInstance],
        request: Option<&UnifiedRequest>,
    ) -> Result<&'a ServiceInstance, LoadBalancerError> {
        let mut healthy: Vec<_> = instances
            .iter()
            .filter(|instance| instance.is_healthy)
            .collect();
        if healthy.is_empty() {
            healthy.extend(instances);
        }
        let index = match self.strategy_name.as_str() {
            "random" => RandomStrategy.select_index(healthy.len(), self.runtime.as_ref())?,
            "weighted" => {
                WeightedStrategy.select_index(healthy.iter().copied(), self.runtime.as_ref())?
            }
            "consistent_hash" => ConsistentHashStrategy.select_index(
                service_id,
                healthy.len(),
                request,
                self.runtime.as_ref(),
            )?,
            _ => self.round_robin.select_index(service_id, healthy.len())?,
        };
        Ok(healthy[index])
    }
}
