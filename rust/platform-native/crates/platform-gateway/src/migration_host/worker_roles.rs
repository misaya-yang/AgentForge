//! Process-role filtering and supervised singleton ownership on the real lifecycle.
use super::lifecycle::{HostError, HostOperation, LifecycleStep};
use ai_platform_gateway_foundation::worker_lease::{self, LeaseStore};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessRole {
    Api,
    Worker,
    Eval,
    #[default]
    All,
}
impl ProcessRole {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "api" => Ok(Self::Api),
            "worker" => Ok(Self::Worker),
            "eval" => Ok(Self::Eval),
            "all" => Ok(Self::All),
            _ => Err("gateway_process_role"),
        }
    }
    pub fn validate_delivery(self, mode: &str) -> Result<(), &'static str> {
        if self != Self::All && mode == "memory" {
            Err("split_process_roles_require_durable_tasks")
        } else {
            Ok(())
        }
    }
    pub fn workers(self) -> bool {
        self != Self::Api
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Worker => "worker",
            Self::Eval => "eval",
            Self::All => "all",
        }
    }
}
/// Existing config modes remain required; role may appear before or after them.
pub struct Arguments {
    pub check_only: bool,
    pub environment_mode: bool,
    pub role: Option<ProcessRole>,
}
impl Arguments {
    pub fn parse(args: &[String]) -> Result<Self, &'static str> {
        let mut mode = None;
        let mut role = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config-stdin"
                | "--check-config-stdin"
                | "--config-env"
                | "--check-config-env" => {
                    if mode.replace(arg.as_str()).is_some() {
                        return Err("gateway_config_mode");
                    }
                }
                "--role" => {
                    if role.is_some() {
                        return Err("gateway_process_role");
                    }
                    role = Some(ProcessRole::parse(
                        args.next().ok_or("gateway_process_role")?,
                    )?);
                }
                _ => return Err("gateway_arguments"),
            }
        }
        let mode = mode.ok_or("gateway_config_mode")?;
        Ok(Self {
            check_only: mode.starts_with("--check-"),
            environment_mode: mode.ends_with("-env"),
            role,
        })
    }
}
fn worker_job(name: &str) -> bool {
    worker_lease::valid_job(name) || name == "task_worker"
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Starting,
    Standby,
    Active,
    Draining,
    Failed,
    Stopped,
}
#[derive(Default)]
pub struct WorkerHealth {
    jobs: RwLock<BTreeMap<&'static str, State>>,
}
impl WorkerHealth {
    fn set(&self, name: &'static str, state: State) {
        self.jobs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, state);
    }
    pub fn ready(&self) -> bool {
        self.jobs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .all(|state| matches!(state, State::Standby | State::Active))
    }
    pub fn image_status(&self) -> &'static str {
        match self
            .jobs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get("image_task_worker")
        {
            Some(State::Active) => "healthy",
            Some(State::Standby) => "standby",
            None => "disabled",
            _ => "unavailable",
        }
    }
}
type AuthorityFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;
trait ActiveLease: Send {
    fn revoke(&self);
    fn renew(&mut self) -> AuthorityFuture<'_, ()>;
    fn release(self: Box<Self>) -> AuthorityFuture<'static, ()>;
}
trait Authority: Send + Sync {
    fn verify_schema(&self) -> AuthorityFuture<'_, ()>;
    fn acquire(&self, name: &'static str) -> AuthorityFuture<'_, Option<Box<dyn ActiveLease>>>;
}
impl ActiveLease for worker_lease::HeldLease {
    fn revoke(&self) {
        self.guard.revoke();
    }
    fn renew(&mut self) -> AuthorityFuture<'_, ()> {
        Box::pin(async move {
            worker_lease::HeldLease::renew(self)
                .await
                .map_err(|_| HostError::Database)
        })
    }
    fn release(self: Box<Self>) -> AuthorityFuture<'static, ()> {
        Box::pin(async move {
            (*self)
                .release()
                .await
                .map_err(|_| HostError::Shutdown("worker_lease_release"))
        })
    }
}
impl Authority for LeaseStore {
    fn verify_schema(&self) -> AuthorityFuture<'_, ()> {
        Box::pin(async move {
            LeaseStore::verify_schema(self)
                .await
                .map_err(|_| HostError::Startup("worker_lease_schema"))
        })
    }
    fn acquire(&self, name: &'static str) -> AuthorityFuture<'_, Option<Box<dyn ActiveLease>>> {
        Box::pin(async move {
            LeaseStore::acquire(self, name)
                .await
                .map(|lease| lease.map(|lease| Box::new(lease) as Box<dyn ActiveLease>))
                .map_err(|_| HostError::Database)
        })
    }
}
struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<(), HostError>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
struct Singleton {
    store: Arc<dyn Authority>,
    name: &'static str,
    start: HostOperation,
    stop: HostOperation,
    health: Arc<WorkerHealth>,
    running: Mutex<Option<Running>>,
}
impl Singleton {
    async fn start(self: &Arc<Self>) -> Result<(), HostError> {
        // Missing authority is fatal before any singleton is activated.
        self.store
            .verify_schema()
            .await
            .map_err(|_| HostError::Startup("worker_lease_schema"))?;
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let (stop, signal) = watch::channel(false);
        // Do not capture the handle owner: doing so creates an Arc cycle that
        // keeps a dropped host's workers alive forever. This inert copy owns no
        // task handle; dropping the actual Running owner signals its shutdown.
        let owner = Self {
            store: self.store.clone(),
            name: self.name,
            start: self.start.clone(),
            stop: self.stop.clone(),
            health: self.health.clone(),
            running: Mutex::new(None),
        };
        self.health.set(self.name, State::Standby);
        let task = tokio::spawn(async move {
            let outcome = owner.supervise(signal).await;
            owner.health.set(
                owner.name,
                if outcome.is_ok() {
                    State::Stopped
                } else {
                    State::Failed
                },
            );
            outcome
        });
        *running = Some(Running { stop, task });
        Ok(())
    }
    async fn supervise(&self, mut signal: watch::Receiver<bool>) -> Result<(), HostError> {
        loop {
            if *signal.borrow() {
                return Ok(());
            }
            let held = tokio::select! {
                _ = signal.changed() => return Ok(()),
                result = self.store.acquire(self.name) => result,
            };
            let mut held = match held {
                Ok(Some(held)) => held,
                result => {
                    // Contention is healthy standby. Database failure is not.
                    self.health.set(
                        self.name,
                        if result.is_ok() {
                            State::Standby
                        } else {
                            State::Failed
                        },
                    );
                    tokio::select! { _ = signal.changed() => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
                    continue;
                }
            };
            self.health.set(self.name, State::Starting);
            let started = tokio::time::timeout(Duration::from_secs(10), (self.start)()).await;
            if !matches!(started, Ok(Ok(()))) {
                held.revoke();
                // Partial startup still owns stop and effect drain.
                (self.stop)().await?;
                held.release()
                    .await
                    .map_err(|_| HostError::Shutdown("worker_lease_release"))?;
                return Err(HostError::Startup(self.name));
            }
            self.health.set(self.name, State::Active);
            let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = signal.changed() => break,
                    _ = heartbeat.tick() => {
                        if held.renew().await.is_err() { break; }
                    }
                }
            }
            held.revoke();
            self.health.set(self.name, State::Draining);
            // No abort of detached job owners or forced release on stop errors.
            // If drain fails, readiness stays failed and host shutdown reports it.
            (self.stop)().await?;
            held.release()
                .await
                .map_err(|_| HostError::Shutdown("worker_lease_release"))?;
            if *signal.borrow() {
                return Ok(());
            }
            self.health.set(self.name, State::Standby);
            tokio::select! { _ = signal.changed() => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
        }
    }
    async fn stop(&self) -> Result<(), HostError> {
        let mut running = self.running.lock().await;
        let result = if let Some(running) = running.as_mut() {
            let _ = running.stop.send(true);
            // Keep the handle owned here if the host's cleanup waiter times out.
            (&mut running.task)
                .await
                .map_err(|_| HostError::Shutdown("worker_singleton"))
                .and_then(|result| result)
        } else {
            let failed = self
                .health
                .jobs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(self.name)
                == Some(&State::Failed);
            if failed {
                Err(HostError::Shutdown("worker_singleton"))
            } else {
                Ok(())
            }
        };
        // A completed JoinHandle cannot be polled a second time. Retain failure
        // in health so a repeated cleanup does not report false success.
        running.take();
        if result.is_err() {
            self.health.set(self.name, State::Failed);
        }
        result
    }
}
/// Includes externally supplied usage consumers; local usage recording and
/// publisher flush stay active in EVERY role, including API-only processes.
pub fn apply(
    role: ProcessRole,
    pool: sqlx::PgPool,
    control: sqlx::PgPool,
    starts: &mut Vec<LifecycleStep>,
    stops: &mut Vec<LifecycleStep>,
) -> Result<Arc<WorkerHealth>, HostError> {
    let health = Arc::new(WorkerHealth::default());
    if role == ProcessRole::Eval {
        starts.retain(|s| !worker_job(s.name) || s.name == "eval_outbox_worker");
        stops.retain(|s| !worker_job(s.name) || s.name == "eval_outbox_worker");
    }
    if !role.workers() {
        starts.retain(|s| !worker_job(s.name));
        stops.retain(|s| !worker_job(s.name));
        return Ok(health);
    }
    let stop_map: BTreeMap<_, _> = stops
        .iter()
        .map(|s| (s.name, s.operation.clone()))
        .collect();
    let store: Arc<dyn Authority> = Arc::new(LeaseStore::new(pool, control));
    let admission = store.clone();
    starts.insert(
        0,
        LifecycleStep {
            name: "worker_lease_schema",
            best_effort: false,
            operation: Arc::new(move || {
                let admission = admission.clone();
                Box::pin(async move { admission.verify_schema().await })
            }),
        },
    );
    for step in starts
        .iter_mut()
        .filter(|s| worker_lease::valid_job(s.name))
    {
        let stop = stop_map
            .get(step.name)
            .cloned()
            .ok_or(HostError::Configuration)?;
        health.set(step.name, State::Starting);
        let singleton = Arc::new(Singleton {
            store: store.clone(),
            name: step.name,
            start: step.operation.clone(),
            stop,
            health: health.clone(),
            running: Mutex::new(None),
        });
        let starter = singleton.clone();
        step.operation = Arc::new(move || {
            let owner = starter.clone();
            Box::pin(async move { owner.start().await })
        });
        step.best_effort = false;
        let target = stops
            .iter_mut()
            .find(|s| s.name == step.name)
            .ok_or(HostError::Configuration)?;
        target.operation = Arc::new(move || {
            let owner = singleton.clone();
            Box::pin(async move { owner.stop().await })
        });
        target.best_effort = false;
    }
    Ok(health)
}
/// Worker processes expose health and authenticated metrics, never business HTTP
/// or SSE, even through a configured companion or SDK callback TLS listener.
pub fn restrict_worker(host: &mut super::GatewayHost) {
    host.local_node_https = None;
    host.sdk_callback_https = None;
    host.router = host
        .router
        .clone()
        .layer(axum::middleware::from_fn(worker_only));
}
async fn worker_only(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if matches!(
        request.uri().path(),
        "/health" | "/health/live" | "/health/ready" | "/metrics"
    ) {
        next.run(request).await
    } else {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}
#[cfg(test)]
mod tests;
