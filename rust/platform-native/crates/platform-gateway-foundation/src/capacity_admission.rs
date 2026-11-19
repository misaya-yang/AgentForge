//! Owned admission/lease port of `src/core/gateway/admission.py`.
//!
//! A local bounded queue protects every enforced budget. Shared budgets also
//! atomically reserve the existing Redis shared/tenant pair; local tenant
//! bulkheads apply without Redis and only on explicitly allowed degradation.
//! Lease renewal/release use the original Lua and never resurrect lost members.
//!
//! Rust cancellation is explicit: `acquire_with_cancel` and `CapacityLease::run`
//! wait for owned cleanup before reporting cooperative cancellation or lease
//! loss. Dropping an acquire/release/work future does not cancel its owned
//! cleanup task. Arbitrary Tokio task abort cannot delay the abort acknowledgment
//! until async cleanup finishes; drop cleanup is a backup, not Python shield
//! equivalence. Keep the Tokio runtime alive until explicit release completes.
//! Runtime shutdown can prevent remote cleanup; Redis TTL bounds that failure.
//! No HTTP wiring, environment reads, connection creation, or metrics backend.

pub use crate::load_shedder::CapacityRejected;
use crate::{
    capacity::{CapacityBudget, CapacityError},
    load_shedder::AdaptiveLoadShedder,
};
use redis::{Script, aio::ConnectionManager};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    runtime::Handle,
    sync::{Notify, watch},
    task::JoinHandle,
};

pub const CAPACITY_ACQUIRE_PAIR_LUA: &str = r#"
local function admit(key, limit)
  if limit <= 0 then
    return -1
  end
  redis.call('ZREMRANGEBYSCORE', key, 0, ARGV[1])
  local count = redis.call('ZCARD', key)
  if count < limit then
    redis.call('ZADD', key, ARGV[2], ARGV[3])
    redis.call('PEXPIRE', key, ARGV[4])
  end
  return count
end
local shared_limit = tonumber(ARGV[5])
local tenant_limit = tonumber(ARGV[6])
local shared_count = admit(KEYS[1], shared_limit)
local tenant_count = -1
if shared_count < shared_limit then
  tenant_count = admit(KEYS[2], tenant_limit)
  if tenant_count >= tenant_limit then
    redis.call('ZREM', KEYS[1], ARGV[3])
  end
end
return {shared_count, tenant_count}
"#;
pub const CAPACITY_RENEW_LUA: &str = r#"
for i = 1, #KEYS do
  local score = redis.call('ZSCORE', KEYS[i], ARGV[3])
  if not score or tonumber(score) <= tonumber(ARGV[1]) then return 0 end
end
for i = 1, #KEYS do
  redis.call('ZADD', KEYS[i], 'XX', ARGV[2], ARGV[3])
  redis.call('PEXPIRE', KEYS[i], ARGV[4])
end
return 1
"#;
pub const CAPACITY_RELEASE_LUA: &str = r#"
for i = 1, #KEYS do
  redis.call('ZREM', KEYS[i], ARGV[1])
end
return #KEYS
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityStoreError {
    Unavailable,
}
pub type CapacityStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CapacityStoreError>> + Send + 'a>>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityPair {
    pub shared_key: String,
    pub tenant_key: String,
    pub member: String,
}
#[derive(Debug, Clone)]
pub struct CapacityAcquire {
    pub pair: CapacityPair,
    pub now_ms: i64,
    pub expires_at_ms: i64,
    pub ttl_ms: i64,
    pub shared_limit: i64,
    pub tenant_limit: i64,
}
pub trait CapacityStore: Send + Sync {
    fn acquire<'a>(&'a self, request: &'a CapacityAcquire) -> CapacityStoreFuture<'a, (i64, i64)>;
    fn renew<'a>(
        &'a self,
        pair: &'a CapacityPair,
        now_ms: i64,
        expires_at_ms: i64,
        ttl_ms: i64,
    ) -> CapacityStoreFuture<'a, bool>;
    fn release<'a>(&'a self, pair: &'a CapacityPair) -> CapacityStoreFuture<'a, ()>;
}
pub struct RedisCapacityStore {
    connection: ConnectionManager,
    acquire: Script,
    renew: Script,
    release: Script,
}
impl RedisCapacityStore {
    pub fn new(connection: ConnectionManager) -> Self {
        Self {
            connection,
            acquire: Script::new(
                r#"local clock=redis.call('TIME'); local now=tonumber(clock[1])*1000+math.floor(tonumber(clock[2])/1000); redis.call('ZREMRANGEBYSCORE',KEYS[1],'-inf',now); local n=redis.call('ZCARD',KEYS[1]); if n<tonumber(ARGV[5]) then redis.call('ZADD',KEYS[1],now+tonumber(ARGV[4]),ARGV[3]); redis.call('PEXPIRE',KEYS[1],ARGV[4]); end; return n"#,
            ),
            renew: Script::new(
                r#"local clock=redis.call('TIME'); local now=tonumber(clock[1])*1000+math.floor(tonumber(clock[2])/1000); local score=redis.call('ZSCORE',KEYS[1],ARGV[3]); if not score or tonumber(score)<=now then return 0 end; redis.call('ZADD',KEYS[1],'XX',now+tonumber(ARGV[4]),ARGV[3]); redis.call('PEXPIRE',KEYS[1],ARGV[4]); return 1"#,
            ),
            release: Script::new(CAPACITY_RELEASE_LUA),
        }
    }
}
impl CapacityStore for RedisCapacityStore {
    fn acquire<'a>(&'a self, request: &'a CapacityAcquire) -> CapacityStoreFuture<'a, (i64, i64)> {
        Box::pin(async move {
            let shared: i64 = self
                .acquire
                .key(&request.pair.shared_key)
                .arg(request.now_ms)
                .arg(request.expires_at_ms)
                .arg(&request.pair.member)
                .arg(request.ttl_ms)
                .arg(request.shared_limit)
                .invoke_async(&mut self.connection.clone())
                .await
                .map_err(|_| CapacityStoreError::Unavailable)?;
            if shared >= request.shared_limit {
                return Ok((shared, -1));
            }
            let tenant: i64 = self
                .acquire
                .key(&request.pair.tenant_key)
                .arg(request.now_ms)
                .arg(request.expires_at_ms)
                .arg(&request.pair.member)
                .arg(request.ttl_ms)
                .arg(request.tenant_limit)
                .invoke_async(&mut self.connection.clone())
                .await
                .map_err(|_| CapacityStoreError::Unavailable)?;
            if tenant >= request.tenant_limit {
                let _: i64 = self
                    .release
                    .key(&request.pair.shared_key)
                    .arg(&request.pair.member)
                    .invoke_async(&mut self.connection.clone())
                    .await
                    .map_err(|_| CapacityStoreError::Unavailable)?;
            }
            Ok((shared, tenant))
        })
    }
    fn renew<'a>(
        &'a self,
        pair: &'a CapacityPair,
        now_ms: i64,
        expires_at_ms: i64,
        ttl_ms: i64,
    ) -> CapacityStoreFuture<'a, bool> {
        Box::pin(async move {
            for key in [&pair.shared_key, &pair.tenant_key] {
                let result: i64 = self
                    .renew
                    .key(key)
                    .arg(now_ms)
                    .arg(expires_at_ms)
                    .arg(&pair.member)
                    .arg(ttl_ms)
                    .invoke_async(&mut self.connection.clone())
                    .await
                    .map_err(|_| CapacityStoreError::Unavailable)?;
                if result != 1 {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
    fn release<'a>(&'a self, pair: &'a CapacityPair) -> CapacityStoreFuture<'a, ()> {
        Box::pin(async move {
            for key in [&pair.shared_key, &pair.tenant_key] {
                let _: i64 = self
                    .release
                    .key(key)
                    .arg(&pair.member)
                    .invoke_async(&mut self.connection.clone())
                    .await
                    .map_err(|_| CapacityStoreError::Unavailable)?;
            }
            Ok(())
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdmissionError {
    Rejected(Box<CapacityRejected>),
    Cancelled,
    Configuration(CapacityError),
    TaskFailed,
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(rejected) => formatter.write_str(&rejected.message),
            Self::Cancelled => formatter.write_str("admission_cancelled"),
            Self::Configuration(error) => error.fmt(formatter),
            Self::TaskFailed => formatter.write_str("admission_owned_task_failed"),
        }
    }
}
impl std::error::Error for AdmissionError {}
impl From<CapacityError> for AdmissionError {
    fn from(error: CapacityError) -> Self {
        Self::Configuration(error)
    }
}

/// Cooperative owner/caller cancellation; clones address the same signal.
#[derive(Clone, Debug)]
pub struct AdmissionCancellation(watch::Sender<bool>);
impl Default for AdmissionCancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl AdmissionCancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}
struct CancelOnDrop(Option<AdmissionCancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(signal) = self.0.take() {
            signal.cancel();
        }
    }
}

#[derive(Clone)]
pub struct AdmissionOptions {
    pub gateway_instance_id: String,
    pub cluster_epoch: String,
    pub lease_ttl_ms: i64,
    pub per_tenant_default_share: f64,
    pub per_tenant_limits: BTreeMap<String, i64>,
    pub load_shedder: Option<Arc<Mutex<AdaptiveLoadShedder>>>,
}
impl Default for AdmissionOptions {
    fn default() -> Self {
        Self {
            gateway_instance_id: "gateway-1".into(),
            cluster_epoch: "uat-2026-05".into(),
            lease_ttl_ms: 30000,
            per_tenant_default_share: 0.2,
            per_tenant_limits: BTreeMap::new(),
            load_shedder: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct AdmissionRequest {
    pub tenant_id: String,
    pub user_id: String,
    pub service_id: String,
    pub request_class: String,
    pub request_id: String,
    pub allow_degraded_open: bool,
    pub priority: i64,
}
impl Default for AdmissionRequest {
    fn default() -> Self {
        Self {
            tenant_id: String::new(),
            user_id: String::new(),
            service_id: String::new(),
            request_class: String::new(),
            request_id: String::new(),
            allow_degraded_open: false,
            priority: 2,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CapacitySnapshot {
    pub inflight: i64,
    pub queue_depth: i64,
    pub limit: i64,
}
struct LocalState {
    limit: i64,
    queue_max: i64,
    queue_timeout_ms: i64,
    inflight: i64,
    queue_depth: i64,
}
struct LocalBudget {
    state: Mutex<LocalState>,
    changed: Notify,
}
impl LocalBudget {
    fn new(budget: &CapacityBudget) -> Self {
        Self {
            state: Mutex::new(LocalState {
                limit: budget.limit.max(1),
                queue_max: budget.queue_max.max(0),
                queue_timeout_ms: budget.queue_timeout_ms.max(1),
                inflight: 0,
                queue_depth: 0,
            }),
            changed: Notify::new(),
        }
    }
}
struct LocalPermit(Arc<LocalBudget>);
impl Drop for LocalPermit {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.0.state);
            state.inflight = (state.inflight - 1).max(0);
        }
        self.0.changed.notify_one();
    }
}
struct QueuePermit(Arc<LocalBudget>);
impl Drop for QueuePermit {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.0.state);
            state.queue_depth = (state.queue_depth - 1).max(0);
        }
        // Transfer a possibly consumed wake when a queued owner is cancelled.
        self.0.changed.notify_one();
    }
}
#[derive(Default)]
struct Registry {
    local: HashMap<String, Arc<LocalBudget>>,
    tenants: HashMap<String, Arc<LocalBudget>>,
}
/// Synchronous local telemetry; implementations must not perform I/O or affect admission.
pub trait AdmissionRejectionObserver: Send + Sync {
    fn rejected(&self, service: &str, reason: &str);
}
struct ControllerInner {
    rejection_observer: OnceLock<Arc<dyn AdmissionRejectionObserver>>,
    options: AdmissionOptions,
    store: Option<Arc<dyn CapacityStore>>,
    model_admission: OnceLock<Arc<atomic::model_plane::ModelPlaneAdmission>>,
    states: Mutex<Registry>,
    release_failures: AtomicU64,
    lost_leases: AtomicU64,
}
#[derive(Clone)]
pub struct CapacityAdmissionController {
    inner: Arc<ControllerInner>,
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
fn retry_after(milliseconds: i64) -> i64 {
    ((milliseconds as f64 / 1000.0).ceil() as i64).max(1)
}
fn rejected(
    key: &str,
    code: &str,
    message: Option<String>,
    status: u16,
    wait: f64,
    retry: i64,
) -> CapacityRejected {
    let wait = wait.max(0.0);
    let retry = retry.max(1);
    CapacityRejected {
        budget_key: key.into(),
        code: code.into(),
        status_code: status,
        queue_wait_ms: wait,
        retry_after: retry,
        message: message.unwrap_or_else(|| format!("Gateway capacity exhausted for {key}")),
        headers: BTreeMap::from([
            ("X-Gateway-Capacity-Key".into(), key.into()),
            ("X-Gateway-Queue-Wait-Ms".into(), format!("{wait:.1}")),
            ("Retry-After".into(), retry.to_string()),
        ]),
    }
}
fn tenant_rejected(budget: &CapacityBudget, tenant: &str) -> AdmissionError {
    let mut error = rejected(
        &budget.key,
        "GATEWAY_TENANT_CAPACITY_EXHAUSTED",
        Some(format!("Tenant capacity exhausted for {tenant}")),
        429,
        0.0,
        retry_after(budget.queue_timeout_ms),
    );
    error
        .headers
        .insert("X-RateLimit-Remaining".into(), "0".into());
    error
        .headers
        .insert("X-Gateway-Tenant-Id".into(), tenant.into());
    AdmissionError::Rejected(Box::new(error))
}
fn unix_millis() -> Result<i64, AdmissionError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CapacityError::InvalidNumber)?
        .as_millis();
    i64::try_from(millis).map_err(|_| CapacityError::IntegerOverflow.into())
}
fn ascii_json(value: &str) -> String {
    let json = serde_json::to_string(value).expect("string serialization");
    let mut output = String::new();
    for character in json.chars() {
        if character.is_ascii() {
            output.push(character);
        } else {
            let mut buffer = [0; 2];
            for unit in character.encode_utf16(&mut buffer).iter() {
                output.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    output
}

impl CapacityAdmissionController {
    pub fn new(
        mut options: AdmissionOptions,
        store: Option<Arc<dyn CapacityStore>>,
    ) -> Result<Self, CapacityError> {
        if !options.per_tenant_default_share.is_finite() {
            return Err(CapacityError::InvalidNumber);
        }
        options.per_tenant_default_share = options.per_tenant_default_share.clamp(0.0, 1.0);
        options.lease_ttl_ms = options.lease_ttl_ms.max(1000);
        Ok(Self {
            inner: Arc::new(ControllerInner {
                options,
                store,
                model_admission: OnceLock::new(),
                rejection_observer: OnceLock::new(),
                states: Mutex::new(Registry::default()),
                release_failures: AtomicU64::new(0),
                lost_leases: AtomicU64::new(0),
            }),
        })
    }
    fn install_model_plane(
        &self,
        admission: Arc<atomic::model_plane::ModelPlaneAdmission>,
    ) -> bool {
        self.inner.model_admission.set(admission).is_ok()
    }
    pub fn model_plane_admission(&self) -> Option<Arc<atomic::model_plane::ModelPlaneAdmission>> {
        self.inner.model_admission.get().cloned()
    }
    pub fn set_rejection_observer(&self, observer: Arc<dyn AdmissionRejectionObserver>) -> bool {
        self.inner.rejection_observer.set(observer).is_ok()
    }
    pub fn shared_key(&self, budget_key: &str) -> String {
        format!(
            "gateway:capacity:{{{}}}:global:{}",
            atomic::global_tag(&self.inner.options.cluster_epoch),
            budget_key
        )
    }
    pub fn tenant_key(&self, tenant: &str, budget_key: &str) -> String {
        format!(
            "gateway:tenant-capacity:{{{}}}:{}:{}",
            atomic::encode(tenant),
            atomic::encode(&self.inner.options.cluster_epoch),
            budget_key
        )
    }
    /// Same queues and local tenant fairness, with remote admission performed
    /// by the aggregate Cluster-aware dispatcher rather than once per budget.
    pub async fn acquire_local_only(
        &self,
        budgets: &[CapacityBudget],
        request: &AdmissionRequest,
    ) -> Result<CapacityLease, AdmissionError> {
        let budgets: Vec<_> = budgets
            .iter()
            .cloned()
            .map(|mut budget| {
                budget.shared = false;
                budget
            })
            .collect();
        self.acquire(&budgets, request).await
    }
    pub fn tenant_limit_for(
        &self,
        budget: &CapacityBudget,
        tenant: &str,
    ) -> Result<i64, CapacityError> {
        let limits = &self.inner.options.per_tenant_limits;
        if let Some(limit) = limits
            .get(tenant)
            .or_else(|| limits.get(&format!("tenant:{tenant}:{}", budget.key)))
        {
            return Ok((*limit).max(1));
        }
        let limit = (budget.limit as f64 * self.inner.options.per_tenant_default_share)
            .ceil()
            .max(1.0);
        if limit >= i64::MAX as f64 {
            return Err(CapacityError::IntegerOverflow);
        }
        Ok(limit as i64)
    }
    pub fn gateway_instance_id(&self) -> &str {
        &self.inner.options.gateway_instance_id
    }
    pub fn snapshot(&self) -> BTreeMap<String, CapacitySnapshot> {
        lock(&self.inner.states)
            .local
            .iter()
            .map(|(key, local)| {
                let state = lock(&local.state);
                (
                    key.clone(),
                    CapacitySnapshot {
                        inflight: state.inflight,
                        queue_depth: state.queue_depth,
                        limit: state.limit,
                    },
                )
            })
            .collect()
    }
    pub fn release_failures(&self) -> u64 {
        self.inner.release_failures.load(Ordering::Relaxed)
    }
    pub fn lost_leases(&self) -> u64 {
        self.inner.lost_leases.load(Ordering::Relaxed)
    }
    pub async fn acquire(
        &self,
        budgets: &[CapacityBudget],
        request: &AdmissionRequest,
    ) -> Result<CapacityLease, AdmissionError> {
        self.acquire_with_cancel(budgets, request, AdmissionCancellation::default())
            .await
    }
    /// Owned worker preserves local/remote rollback even if this future is dropped.
    pub async fn acquire_with_cancel(
        &self,
        budgets: &[CapacityBudget],
        request: &AdmissionRequest,
        cancellation: AdmissionCancellation,
    ) -> Result<CapacityLease, AdmissionError> {
        let mut on_drop = CancelOnDrop(Some(cancellation.clone()));
        let controller = self.clone();
        let budgets = budgets.to_vec();
        let request = request.clone();
        let worker_cancel = cancellation.clone();
        let result = tokio::spawn(async move {
            controller
                .acquire_owned(budgets, request, worker_cancel)
                .await
        })
        .await
        .map_err(|_| AdmissionError::TaskFailed)?;
        on_drop.0 = None;
        match result {
            Ok(lease) if cancellation.is_cancelled() => {
                lease.release().await;
                Err(AdmissionError::Cancelled)
            }
            result => result,
        }
    }
    async fn acquire_owned(
        &self,
        budgets: Vec<CapacityBudget>,
        request: AdmissionRequest,
        cancellation: AdmissionCancellation,
    ) -> Result<CapacityLease, AdmissionError> {
        let budgets: Vec<_> = budgets
            .into_iter()
            .filter(|budget| budget.enforced)
            .collect();
        let started = Instant::now();
        let lifecycle = budgets.iter().any(|budget| {
            matches!(
                budget.scope.as_str(),
                "run" | "provider" | "capability" | "job" | "sse"
            ) && budget.key == format!("gateway.{}_inflight", budget.scope)
        });
        let shedder = if lifecycle {
            None
        } else {
            self.inner.options.load_shedder.clone()
        };
        let mut owned = OwnedAcquisition {
            controller: self.clone(),
            resources: Some(Resources::default()),
            runtime: Handle::current(),
        };
        let result = async {
            if cancellation.is_cancelled() {
                return Err(AdmissionError::Cancelled);
            }
            if let Some(shedder) = &shedder
                && let Some(error) = lock(shedder).maybe_reject(
                    &request.service_id,
                    &request.request_class,
                    request.priority,
                )
            {
                return Err(AdmissionError::Rejected(Box::new(error)));
            }
            let minimum_timeout = budgets
                .iter()
                .map(|budget| budget.queue_timeout_ms)
                .min()
                .unwrap_or(1);
            for budget in &budgets {
                let remaining = (i128::from(minimum_timeout)
                    - started.elapsed().as_millis() as i128)
                    .min(i128::from(budget.queue_timeout_ms))
                    .max(1) as i64;
                let local = tokio::select! { biased;
                    _ = cancellation.cancelled() => return Err(AdmissionError::Cancelled),
                    local = self.acquire_local(budget, remaining) => local?,
                };
                owned.resources.as_mut().unwrap().local.push(local);
                let tenant = if request.tenant_id.is_empty() {
                    "public"
                } else {
                    &request.tenant_id
                };
                let tenant_limit = self.tenant_limit_for(budget, tenant)?;
                let shared = if budget.shared && self.inner.store.is_some() {
                    self.acquire_pair(budget, tenant, tenant_limit, &request, &cancellation)
                        .await?
                } else {
                    None
                };
                if let Some(shared) = shared {
                    owned.resources.as_mut().unwrap().shared.push(shared);
                } else {
                    owned
                        .resources
                        .as_mut()
                        .unwrap()
                        .tenants
                        .push(self.acquire_tenant(budget, tenant, tenant_limit)?);
                }
            }
            if cancellation.is_cancelled() {
                return Err(AdmissionError::Cancelled);
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if let AdmissionError::Rejected(rejected) = &error
                && let Some(observer) = self.inner.rejection_observer.get()
            {
                observer.rejected(&request.service_id, &rejected.code);
            }
            owned.release().await;
            return Err(error);
        }
        let resources = owned.resources.take().unwrap();
        let lease = CapacityLease::new(
            self.clone(),
            budgets.iter().map(|budget| budget.key.clone()).collect(),
            elapsed_ms(started),
            started,
            shedder,
            resources,
        );
        lease.start_renew();
        Ok(lease)
    }
    async fn acquire_local(
        &self,
        budget: &CapacityBudget,
        timeout_ms: i64,
    ) -> Result<LocalPermit, AdmissionError> {
        let started = Instant::now();
        let local = lock(&self.inner.states)
            .local
            .entry(budget.key.clone())
            .or_insert_with(|| Arc::new(LocalBudget::new(budget)))
            .clone();
        {
            let mut state = lock(&local.state);
            state.limit = budget.limit.max(1);
            state.queue_max = budget.queue_max.max(0);
            state.queue_timeout_ms = budget.queue_timeout_ms.max(1);
            if state.inflight < state.limit {
                state.inflight += 1;
                return Ok(LocalPermit(local.clone()));
            }
            if state.queue_max <= 0 || state.queue_depth >= state.queue_max {
                return Err(AdmissionError::Rejected(Box::new(rejected(
                    &budget.key,
                    "GATEWAY_CAPACITY_EXHAUSTED",
                    None,
                    503,
                    elapsed_ms(started),
                    retry_after(state.queue_timeout_ms),
                ))));
            }
            state.queue_depth += 1;
        }
        let _queued = QueuePermit(local.clone());
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(timeout_ms.max(1) as u64))
            .ok_or(CapacityError::IntegerOverflow)?;
        loop {
            let notification = local.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            {
                let mut state = lock(&local.state);
                if state.inflight < state.limit {
                    state.inflight += 1;
                    return Ok(LocalPermit(local.clone()));
                }
            }
            if tokio::time::timeout_at(deadline, notification)
                .await
                .is_err()
            {
                return Err(AdmissionError::Rejected(Box::new(rejected(
                    &budget.key,
                    "GATEWAY_CAPACITY_EXHAUSTED",
                    None,
                    503,
                    elapsed_ms(started),
                    retry_after(lock(&local.state).queue_timeout_ms),
                ))));
            }
        }
    }
    fn acquire_tenant(
        &self,
        budget: &CapacityBudget,
        tenant: &str,
        limit: i64,
    ) -> Result<LocalPermit, AdmissionError> {
        let key = format!("tenant:{tenant}:{}", budget.key);
        let local = lock(&self.inner.states)
            .tenants
            .entry(key)
            .or_insert_with(|| Arc::new(LocalBudget::new(budget)))
            .clone();
        {
            let mut state = lock(&local.state);
            state.limit = limit;
            if state.inflight >= state.limit {
                return Err(tenant_rejected(budget, tenant));
            }
            state.inflight += 1;
        }
        Ok(LocalPermit(local))
    }
    async fn acquire_pair(
        &self,
        budget: &CapacityBudget,
        tenant: &str,
        tenant_limit: i64,
        request: &AdmissionRequest,
        cancellation: &AdmissionCancellation,
    ) -> Result<Option<CapacityPair>, AdmissionError> {
        let now = unix_millis()?;
        let ttl = self.inner.options.lease_ttl_ms;
        let expires = now.checked_add(ttl).ok_or(CapacityError::IntegerOverflow)?;
        let member = format!(
            "{{\"gateway_instance_id\": {}, \"lease_expires_at_ms\": {expires}, \"lease_id\": {}, \"request_id\": {}, \"started_at_ms\": {now}}}",
            ascii_json(&self.inner.options.gateway_instance_id),
            ascii_json(&uuid::Uuid::new_v4().to_string()),
            ascii_json(&request.request_id)
        );
        let pair = CapacityPair {
            shared_key: self.shared_key(&budget.key),
            tenant_key: self.tenant_key(tenant, &budget.key),
            member,
        };
        let input = CapacityAcquire {
            pair: pair.clone(),
            now_ms: now,
            expires_at_ms: expires,
            ttl_ms: ttl,
            shared_limit: budget.limit,
            tenant_limit,
        };
        let store = self.inner.store.as_ref().unwrap().clone();
        let mut task = tokio::spawn(async move { store.acquire(&input).await });
        let result = tokio::select! { biased;
            _ = cancellation.cancelled() => {
                let _ = task.await; // shield request completion before removing possible late writes
                self.release_pair(&pair).await;
                return Err(AdmissionError::Cancelled);
            }
            result = &mut task => result,
        };
        match result {
            Ok(Ok((shared, _))) if shared >= budget.limit => {
                Err(AdmissionError::Rejected(Box::new(rejected(
                    &budget.key,
                    "GATEWAY_CAPACITY_EXHAUSTED",
                    None,
                    503,
                    0.0,
                    retry_after(budget.queue_timeout_ms),
                ))))
            }
            Ok(Ok((_, used))) if used >= tenant_limit => {
                self.release_pair(&pair).await;
                Err(tenant_rejected(budget, tenant))
            }
            Ok(Ok(_)) => Ok(Some(pair)),
            // Match Python: uncertain non-cancellation store failures can leave
            // a partial remote write until TTL; do not replay acquisition.
            _ if request.allow_degraded_open => Ok(None),
            _ => Err(AdmissionError::Rejected(Box::new(rejected(
                &budget.key,
                "GATEWAY_CAPACITY_DEGRADED",
                Some(format!("Shared capacity unavailable for {}", budget.key)),
                503,
                0.0,
                1,
            )))),
        }
    }
    async fn release_pair(&self, pair: &CapacityPair) {
        if let Some(store) = &self.inner.store {
            let timeout = Duration::from_millis(self.inner.options.lease_ttl_ms.min(5000) as u64);
            if !matches!(
                tokio::time::timeout(timeout, store.release(pair)).await,
                Ok(Ok(()))
            ) {
                self.inner.release_failures.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    async fn release_many(&self, mut resources: Resources) {
        for permit in resources.tenants.drain(..).rev() {
            drop(permit);
        }
        for pair in resources.shared.iter().rev() {
            self.release_pair(pair).await;
        }
        for permit in resources.local.drain(..).rev() {
            drop(permit);
        }
    }
}

#[derive(Default)]
struct Resources {
    local: Vec<LocalPermit>,
    tenants: Vec<LocalPermit>,
    shared: Vec<CapacityPair>,
}
struct OwnedAcquisition {
    controller: CapacityAdmissionController,
    resources: Option<Resources>,
    runtime: Handle,
}
impl OwnedAcquisition {
    async fn release(mut self) {
        if let Some(resources) = self.resources.take() {
            self.controller.release_many(resources).await;
        }
    }
}
impl Drop for OwnedAcquisition {
    fn drop(&mut self) {
        if let Some(resources) = self.resources.take() {
            let controller = self.controller.clone();
            self.runtime.spawn(async move {
                controller.release_many(resources).await;
            });
        }
    }
}
struct LeaseInner {
    controller: CapacityAdmissionController,
    runtime: Handle,
    budget_keys: Vec<String>,
    queue_wait_ms: f64,
    started: Instant,
    shedder: Option<Arc<Mutex<AdaptiveLoadShedder>>>,
    resources: Mutex<Option<Resources>>,
    renew: Mutex<Option<JoinHandle<()>>>,
    release_state: AtomicU8,
    released: watch::Sender<bool>,
    lost: AtomicBool,
    loss: AdmissionCancellation,
    owner: Mutex<Option<AdmissionCancellation>>,
}
#[must_use = "release the lease explicitly or use run() to await owned cleanup"]
pub struct CapacityLease {
    inner: Arc<LeaseInner>,
}
impl fmt::Debug for CapacityLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapacityLease")
            .field("budget_keys", &self.inner.budget_keys)
            .field("queue_wait_ms", &self.inner.queue_wait_ms)
            .field("lost", &self.lost())
            .finish()
    }
}
impl CapacityLease {
    fn new(
        controller: CapacityAdmissionController,
        budget_keys: Vec<String>,
        queue_wait_ms: f64,
        started: Instant,
        shedder: Option<Arc<Mutex<AdaptiveLoadShedder>>>,
        resources: Resources,
    ) -> Self {
        Self {
            inner: Arc::new(LeaseInner {
                controller,
                runtime: Handle::current(),
                budget_keys,
                queue_wait_ms,
                started,
                shedder,
                resources: Mutex::new(Some(resources)),
                renew: Mutex::new(None),
                release_state: AtomicU8::new(0),
                released: watch::channel(false).0,
                lost: AtomicBool::new(false),
                loss: AdmissionCancellation::default(),
                owner: Mutex::new(None),
            }),
        }
    }
    pub fn budget_keys(&self) -> &[String] {
        &self.inner.budget_keys
    }
    pub fn queue_wait_ms(&self) -> f64 {
        self.inner.queue_wait_ms
    }
    pub fn lost(&self) -> bool {
        self.inner.lost.load(Ordering::Acquire)
    }
    pub fn headers(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "X-Gateway-Capacity-Key".into(),
                self.inner.budget_keys.join(","),
            ),
            (
                "X-Gateway-Queue-Wait-Ms".into(),
                format!("{:.1}", self.inner.queue_wait_ms),
            ),
        ])
    }
    pub fn lost_error(&self) -> AdmissionError {
        AdmissionError::Rejected(Box::new(rejected(
            &self.inner.budget_keys.join(","),
            "GATEWAY_CAPACITY_LEASE_LOST",
            None,
            503,
            0.0,
            1,
        )))
    }
    pub fn bind_owner(&self, owner: AdmissionCancellation) -> Result<(), AdmissionError> {
        *lock(&self.inner.owner) = Some(owner);
        if self.lost() {
            Err(self.lost_error())
        } else {
            Ok(())
        }
    }
    pub async fn wait_lost(&self) {
        self.inner.loss.cancelled().await;
    }
    fn start_renew(&self) {
        let pairs = lock(&self.inner.resources).as_ref().unwrap().shared.clone();
        if pairs.is_empty() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let controller = self.inner.controller.clone();
        let ttl = controller.inner.options.lease_ttl_ms;
        let interval = Duration::from_secs_f64(ttl as f64 / 3000.0);
        let task = self.inner.runtime.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner.release_state.load(Ordering::Acquire) != 0 {
                    return;
                }
                for pair in &pairs {
                    let renewed = match unix_millis()
                        .ok()
                        .and_then(|now| now.checked_add(ttl).map(|expires| (now, expires)))
                    {
                        Some((now, expires)) => matches!(
                            tokio::time::timeout(
                                interval,
                                controller
                                    .inner
                                    .store
                                    .as_ref()
                                    .unwrap()
                                    .renew(pair, now, expires, ttl)
                            )
                            .await,
                            Ok(Ok(true))
                        ),
                        None => false,
                    };
                    if !renewed {
                        inner.lost.store(true, Ordering::Release);
                        controller.inner.lost_leases.fetch_add(1, Ordering::Relaxed);
                        inner.loss.cancel();
                        if let Some(owner) = lock(&inner.owner).as_ref() {
                            owner.cancel();
                        }
                        return;
                    }
                }
            }
        });
        *lock(&self.inner.renew) = Some(task);
    }
    fn start_release(&self) {
        if self
            .inner
            .release_state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let renewal = lock(&inner.renew).take();
            if let Some(task) = renewal {
                task.abort();
                let _ = task.await;
            }
            if let Some(shedder) = &inner.shedder {
                let _ = lock(shedder).record_latency(elapsed_ms(inner.started));
            }
            let resources = lock(&inner.resources).take();
            if let Some(resources) = resources {
                inner.controller.release_many(resources).await;
            }
            inner.release_state.store(2, Ordering::Release);
            inner.released.send_replace(true);
        });
    }
    /// Concurrent/repeated calls await the same cleanup, even if a waiter drops.
    pub async fn release(&self) {
        self.start_release();
        let mut receiver = self.inner.released.subscribe();
        let _ = receiver.wait_for(|released| *released).await;
    }
    /// Stop the owned work future on cancellation/loss, then finish cleanup.
    /// Use cooperative cancellation instead of aborting this wrapper task.
    pub async fn run<F: Future>(
        self,
        work: F,
        cancellation: AdmissionCancellation,
    ) -> Result<F::Output, AdmissionError> {
        if let Err(error) = self.bind_owner(cancellation.clone()) {
            self.release().await;
            return Err(error);
        }
        let result = tokio::select! { biased;
            _ = self.wait_lost() => Err(self.lost_error()),
            _ = cancellation.cancelled() => if self.lost() { Err(self.lost_error()) } else { Err(AdmissionError::Cancelled) },
            result = work => Ok(result),
        };
        self.release().await;
        result
    }
}
impl Drop for CapacityLease {
    fn drop(&mut self) {
        self.start_release();
    }
}

#[path = "capacity_admission/atomic.rs"]
pub mod atomic;
