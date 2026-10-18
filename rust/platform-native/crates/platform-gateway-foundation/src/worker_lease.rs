//! Singleton authority and real effect permits. Construction never enables it.
//!
//! A permit holds KEY SHARE on the authority row through the entire effect;
//! takeover uses explicit FOR UPDATE. Renewal only updates non-key expiry.
//! Supervised PostgreSQL operations revalidate on their OWN transaction and hold
//! that row lock through commit/rollback, including caller cancellation. Blocking
//! work must retain the permit until its real thread finishes. External requests
//! keep their existing durable dispatch/uncertainty markers; this is not a claim
//! of exactly-once execution at an uncooperative remote service.
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const TTL: Duration = Duration::from_secs(30);
// Nine singleton lanes, including two eval pollers and two maintenance loops.
// A separate bounded pool prevents permit locks from occupying every business
// connection while the guarded statements themselves wait for that same pool.
const EFFECT_CONNECTIONS: u32 = 12;
const DATABASE_TIMEOUT: Duration = Duration::from_secs(3);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    Unavailable,
    Lost,
    InvalidJob,
}
static ENFORCED: AtomicBool = AtomicBool::new(false);
static REJECTIONS: AtomicU64 = AtomicU64::new(0);
static GUARDS: OnceLock<Mutex<HashMap<&'static str, Arc<LeaseGuard>>>> = OnceLock::new();
tokio::task_local! { static EFFECT: Arc<EffectPermit>; }
fn guards() -> &'static Mutex<HashMap<&'static str, Arc<LeaseGuard>>> {
    GUARDS.get_or_init(Default::default)
}
/// Only the production composition root calls this, before starting any jobs.
pub fn enforce() {
    ENFORCED.store(true, Ordering::Release);
}
/// A fatal job error also revokes its supervisor's authority. The next heartbeat
/// initiates the real stop/drain path instead of leaving a dead job marked active.
pub fn invalidate(job: &'static str) {
    if let Some(guard) = guards()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(job)
        .cloned()
    {
        guard.revoke();
    }
}
/// Rejected attempts, never successful unauthorized effects. A healthy run is 0.
pub fn fence_rejections() -> u64 {
    REJECTIONS.load(Ordering::Relaxed)
}
fn rejected() -> LeaseError {
    REJECTIONS.fetch_add(1, Ordering::Relaxed);
    LeaseError::Lost
}
#[derive(Clone)]
pub struct LeaseStore {
    effects: PgPool,
    control: PgPool,
    owner: Uuid,
}
pub struct LeaseGuard {
    pool: PgPool,
    job: &'static str,
    owner: Uuid,
    fence: i64,
    live: AtomicBool,
    deadline: Mutex<Instant>,
    permits: AtomicU64,
    drained: tokio::sync::Notify,
    changed: tokio::sync::Notify,
}
impl LeaseGuard {
    pub fn check_local(&self) -> Result<(), LeaseError> {
        if !self.live.load(Ordering::Acquire)
            || Instant::now() >= *self.deadline.lock().unwrap_or_else(|e| e.into_inner())
        {
            Err(rejected())
        } else {
            Ok(())
        }
    }
    pub fn revoke(&self) {
        self.live.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
    async fn invalidated(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let deadline = *self.deadline.lock().unwrap_or_else(|e| e.into_inner());
            if !self.live.load(Ordering::Acquire) || Instant::now() >= deadline {
                return;
            }
            tokio::select! {
                _ = changed => {},
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {},
            }
        }
    }
    pub fn fence(&self) -> i64 {
        self.fence
    }
    /// On the actual effect transaction, before dispatch. Its lock survives
    /// until that transaction commits or rolls back, not just this check.
    pub async fn assert_on(&self, connection: &mut PgConnection) -> Result<(), LeaseError> {
        self.check_local()?;
        let valid: bool = sqlx::query_scalar("SELECT gateway.assert_worker_lease($1,$2::uuid,$3)")
            .bind(self.job)
            .bind(self.owner.to_string())
            .bind(self.fence)
            .fetch_one(connection)
            .await
            .map_err(|_| LeaseError::Unavailable)?;
        self.check_local()?;
        if valid { Ok(()) } else { Err(rejected()) }
    }
    pub async fn drain(&self) {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.permits.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}
pub struct HeldLease {
    store: LeaseStore,
    pub guard: Arc<LeaseGuard>,
}
impl LeaseStore {
    /// Renewal uses the independent control pool so saturation by real effects
    /// cannot consume the heartbeat connection budget.
    pub fn new(data: PgPool, control: PgPool) -> Self {
        let permits = sqlx::postgres::PgPoolOptions::new()
            .max_connections(EFFECT_CONNECTIONS)
            .min_connections(0)
            .acquire_timeout(DATABASE_TIMEOUT)
            .idle_timeout(Duration::from_secs(30))
            .connect_lazy_with((*data.connect_options()).clone());
        Self {
            effects: permits,
            control,
            owner: Uuid::new_v4(),
        }
    }
    pub async fn verify_schema(&self) -> Result<(), LeaseError> {
        let ready: bool = tokio::time::timeout(DATABASE_TIMEOUT, sqlx::query_scalar(
            "SELECT COALESCE(bool_and(to_regprocedure(f) IS NOT NULL AND has_function_privilege(current_user,to_regprocedure(f),'EXECUTE')),false) FROM unnest(ARRAY['gateway.acquire_worker_lease(text,uuid,bigint)','gateway.renew_worker_lease(text,uuid,bigint,bigint)','gateway.assert_worker_lease(text,uuid,bigint)','gateway.release_worker_lease(text,uuid,bigint)']) AS f"
        ).fetch_one(&self.control)).await.map_err(|_| LeaseError::Unavailable)?
            .map_err(|_| LeaseError::Unavailable)?;
        if ready {
            Ok(())
        } else {
            Err(LeaseError::Unavailable)
        }
    }
    pub async fn acquire(&self, job: &'static str) -> Result<Option<HeldLease>, LeaseError> {
        if !valid_job(job) {
            return Err(LeaseError::InvalidJob);
        }
        // Start the conservative monotonic deadline BEFORE the database call.
        let started = Instant::now();
        let fence: Option<i64> = tokio::time::timeout(
            DATABASE_TIMEOUT,
            sqlx::query_scalar("SELECT gateway.acquire_worker_lease($1,$2::uuid,$3)")
                .bind(job)
                .bind(self.owner.to_string())
                .bind(TTL.as_millis() as i64)
                .fetch_one(&self.control),
        )
        .await
        .map_err(|_| LeaseError::Unavailable)?
        .map_err(|_| LeaseError::Unavailable)?;
        let Some(fence) = fence else {
            return Ok(None);
        };
        let guard = Arc::new(LeaseGuard {
            pool: self.effects.clone(),
            job,
            owner: self.owner,
            fence,
            live: AtomicBool::new(true),
            deadline: Mutex::new(started + TTL),
            permits: AtomicU64::new(0),
            drained: tokio::sync::Notify::new(),
            changed: tokio::sync::Notify::new(),
        });
        guards()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job, guard.clone());
        Ok(Some(HeldLease {
            store: self.clone(),
            guard,
        }))
    }
}
impl HeldLease {
    pub async fn renew(&mut self) -> Result<(), LeaseError> {
        self.guard.check_local()?;
        let started = Instant::now();
        let result = tokio::time::timeout(
            DATABASE_TIMEOUT,
            sqlx::query_scalar::<_, bool>("SELECT gateway.renew_worker_lease($1,$2::uuid,$3,$4)")
                .bind(self.guard.job)
                .bind(self.guard.owner.to_string())
                .bind(self.guard.fence)
                .bind(TTL.as_millis() as i64)
                .fetch_one(&self.store.control),
        )
        .await;
        if !matches!(result, Ok(Ok(true))) {
            self.guard.revoke();
            return Err(LeaseError::Lost);
        }
        // Never resurrect a guard whose conservative local TTL elapsed in flight.
        self.guard.check_local()?;
        *self
            .guard
            .deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = started + TTL;
        self.guard.changed.notify_waiters();
        Ok(())
    }
    /// Caller must stop the worker first. This also waits for blocking threads
    /// retaining permits; a timeout must terminate the owning process, not retry.
    pub async fn release(self) -> Result<(), LeaseError> {
        self.guard.revoke();
        self.guard.drain().await;
        let result = tokio::time::timeout(
            DATABASE_TIMEOUT,
            sqlx::query("SELECT gateway.release_worker_lease($1,$2::uuid,$3)")
                .bind(self.guard.job)
                .bind(self.guard.owner.to_string())
                .bind(self.guard.fence)
                .execute(&self.store.control),
        )
        .await;
        let mut registry = guards().lock().unwrap_or_else(|e| e.into_inner());
        if registry
            .get(self.guard.job)
            .is_some_and(|g| Arc::ptr_eq(g, &self.guard))
        {
            registry.remove(self.guard.job);
        }
        result
            .map_err(|_| LeaseError::Unavailable)?
            .map_err(|_| LeaseError::Unavailable)?;
        Ok(())
    }
}
impl Drop for HeldLease {
    fn drop(&mut self) {
        self.guard.revoke();
    }
}
/// Must be retained by blocking closures too, since aborting spawn_blocking's
/// waiter cannot stop filesystem work. Drop queues SQLx rollback on the permit
/// connection; a successor's FOR UPDATE waits until PostgreSQL releases it.
pub struct EffectPermit {
    _transaction: Transaction<'static, Postgres>,
    guard: Arc<LeaseGuard>,
}
impl EffectPermit {
    pub fn check_local(&self) -> Result<(), LeaseError> {
        self.guard.check_local()
    }
    pub fn guard(&self) -> Arc<LeaseGuard> {
        self.guard.clone()
    }
}
impl Drop for EffectPermit {
    fn drop(&mut self) {
        self.guard.permits.fetch_sub(1, Ordering::AcqRel);
        self.guard.drained.notify_waiters();
    }
}
/// Capture BEFORE spawning a task/thread. PostgreSQL supervisors use the guard
/// on their own transaction; blocking file closures retain the complete permit.
pub fn current_effect() -> Option<Arc<EffectPermit>> {
    EFFECT.try_with(Arc::clone).ok()
}
/// Own a single already-authorized effect until its real completion. Use for
/// adapters that hide spawn_blocking internally: dropping their waiter cannot
/// cancel the underlying write. The spawned owner retains both permit and task
/// context; lease release waits for that permit even if this caller disappears.
pub async fn finish_effect<F>(future: F) -> Result<F::Output, LeaseError>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let permit = current_effect();
    tokio::spawn(async move {
        if let Some(permit) = permit {
            permit.check_local()?;
            Ok(EFFECT.scope(permit, future).await)
        } else {
            Ok(future.await)
        }
    })
    .await
    .map_err(|_| LeaseError::Unavailable)?
}
pub async fn effect(job: &'static str) -> Result<Option<Arc<EffectPermit>>, LeaseError> {
    if !ENFORCED.load(Ordering::Acquire) {
        return Ok(None);
    }
    let guard = guards()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(job)
        .cloned()
        .ok_or_else(rejected)?;
    guard.check_local()?;
    let checked = async {
        let mut transaction = tokio::time::timeout(DATABASE_TIMEOUT, guard.pool.begin())
            .await
            .map_err(|_| LeaseError::Unavailable)?
            .map_err(|_| LeaseError::Unavailable)?;
        tokio::time::timeout(DATABASE_TIMEOUT, guard.assert_on(&mut transaction))
            .await
            .map_err(|_| LeaseError::Unavailable)??;
        Ok::<_, LeaseError>(transaction)
    }
    .await;
    let transaction = match checked {
        Ok(transaction) => transaction,
        Err(error) => {
            guard.revoke();
            return Err(error);
        }
    };
    guard.permits.fetch_add(1, Ordering::AcqRel);
    Ok(Some(Arc::new(EffectPermit {
        _transaction: transaction,
        guard,
    })))
}
/// The future is not polled when authority is absent/expired. One scope covers
/// each bounded worker iteration, rather than the entire lifetime of a worker.
pub async fn with_effect<F: Future>(job: &'static str, future: F) -> Result<F::Output, LeaseError> {
    match effect(job).await? {
        Some(permit) => {
            let guard = permit.guard();
            EFFECT
                .scope(permit, until_invalidated(&guard, future))
                .await
        }
        None => Ok(future.await),
    }
}
async fn until_invalidated<F: Future>(
    guard: &LeaseGuard,
    future: F,
) -> Result<F::Output, LeaseError> {
    tokio::select! {
        biased;
        _ = guard.invalidated() => Err(LeaseError::Lost),
        value = future => Ok(value),
    }
}
pub fn valid_job(job: &str) -> bool {
    matches!(
        job,
        "eval_outbox_worker"
            | "usage_scheduler"
            | "scheduled_task_dispatcher"
            | "image_task_worker"
            | "file_cleanup_service"
            | "sdk_session_cleanup"
            | "trace_retention_scheduler"
            | "assistant_trace_reconciler_task"
            | "usage_event_consumer"
    )
}
#[cfg(test)]
mod tests;
