//! Registry health polling from `src/services/registry/health_monitor.py`.
//!
//! Construction is inert. Start retains its handle even after a registry failure
//! terminates the lane; only stop clears it and permits another start. Checks run
//! sequentially, with the configured delay after the entire pass. No retry,
//! timeout, parallel check, status eviction, or extra adapter call is introduced.
//! A one-off check does not update the monitor's stored statuses.
//!
//! Latency uses wall-clock subtraction, including negative clock adjustments.
//! Ordinary registry/adapter errors become `error` statuses using their existing
//! sanitized Display implementations. Cancellation and Rust panics are not
//! ordinary Python Exceptions and are never translated into health statuses.
//! Status maps are copied and health records remain immutable Arc snapshots;
//! mutable aliases to HealthStatus itself are not exposed. Service handles retain
//! the originally selected live service across health-check awaits. Map iteration
//! order is unspecified in the Rust API.
//!
//! Stop aborts without joining, matching the source. Owner drop also cancels
//! instead of detaching the lane; keep the runtime alive for drop cleanup.
//! This module adds no route, listener, connector, or automatic startup.
use crate::{
    capacity_admission::AdmissionError,
    dispatcher::DispatchError,
    protocol::ProtocolTimestamp,
    service_ownership::ServiceHandle,
    service_registry::{AdapterError, RegistryError, ServiceRegistry},
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct HealthStatus {
    pub service_id: String,
    pub status: String,
    pub latency: f64,
    pub last_check: ProtocolTimestamp,
    pub error: Option<String>,
}
impl HealthStatus {
    pub fn new(service_id: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            status: status.into(),
            latency: 0.0,
            last_check: ProtocolTimestamp::now(),
            error: None,
        }
    }
}

pub type HealthSleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Separate wall-time and UTC samples match time.time() and datetime.utcnow().
/// Implementations must cancel pending sleep work when its future is dropped.
pub trait HealthMonitorRuntime: Send + Sync {
    fn wall_time(&self) -> f64;
    fn utcnow(&self) -> NaiveDateTime;
    fn sleep(&self, seconds: i64) -> HealthSleepFuture<'_>;
}
pub struct SystemHealthMonitorRuntime;
impl HealthMonitorRuntime for SystemHealthMonitorRuntime {
    fn wall_time(&self) -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_secs_f64(),
            Err(error) => -error.duration().as_secs_f64(),
        }
    }
    fn utcnow(&self) -> NaiveDateTime {
        Utc::now().naive_utc()
    }
    fn sleep(&self, seconds: i64) -> HealthSleepFuture<'_> {
        Box::pin(async move {
            // asyncio.sleep(0) and negative intervals still yield control.
            if seconds <= 0 {
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(Duration::from_secs(seconds as u64)).await;
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthMonitorError {
    Registry(RegistryError),
    Cancelled,
}
impl fmt::Display for HealthMonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => error.fmt(f),
            Self::Cancelled => f.write_str("health_monitor_cancelled"),
        }
    }
}
impl std::error::Error for HealthMonitorError {}
impl From<RegistryError> for HealthMonitorError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

struct Collaborators {
    registry: Arc<ServiceRegistry>,
    runtime: Arc<dyn HealthMonitorRuntime>,
    check_interval: AtomicI64,
    statuses: Mutex<HashMap<String, Arc<HealthStatus>>>,
}
enum CheckFailure {
    Registry(RegistryError),
    Adapter(AdapterError),
}
impl Collaborators {
    async fn check_service(
        &self,
        service: ServiceHandle,
    ) -> Result<HealthStatus, HealthMonitorError> {
        let start = self.runtime.wall_time();
        let outcome = match self.registry.get_adapter(service.clone()) {
            Ok(adapter) => match adapter.health_check().await {
                Ok(ok) => Ok(ok),
                Err(AdapterError::Dispatch(
                    DispatchError::Cancelled | DispatchError::Admission(AdmissionError::Cancelled),
                )) => return Err(HealthMonitorError::Cancelled),
                Err(error) => Err(CheckFailure::Adapter(error)),
            },
            Err(error) => Err(CheckFailure::Registry(error)),
        };
        let latency = self.runtime.wall_time() - start;
        let last_check = self.runtime.utcnow().into();
        let (status, error) = match outcome {
            Ok(true) => ("healthy", None),
            Ok(false) => ("unhealthy", None),
            Err(CheckFailure::Registry(error)) => ("error", Some(error.to_string())),
            Err(CheckFailure::Adapter(error)) => ("error", Some(error.to_string())),
        };
        Ok(HealthStatus {
            service_id: service.service_id(),
            status: status.into(),
            latency,
            last_check,
            error,
        })
    }

    async fn check_once(&self) -> Result<(), HealthMonitorError> {
        // Do not catch this failure or bypass ServiceRegistry's active filter.
        let services = self.registry.list(None, &[], None).await?;
        for service in services {
            let status = self.check_service(service.clone()).await?;
            self.statuses
                .lock()
                .expect("health statuses poisoned")
                .insert(service.service_id(), Arc::new(status));
        }
        Ok(())
    }
}

pub struct HealthMonitor {
    collaborators: Arc<Collaborators>,
    task: Option<JoinHandle<Result<(), HealthMonitorError>>>,
}
impl HealthMonitor {
    pub fn new(registry: Arc<ServiceRegistry>) -> Self {
        Self::with_runtime(registry, 30, Arc::new(SystemHealthMonitorRuntime))
    }
    pub fn with_runtime(
        registry: Arc<ServiceRegistry>,
        check_interval: i64,
        runtime: Arc<dyn HealthMonitorRuntime>,
    ) -> Self {
        Self {
            collaborators: Arc::new(Collaborators {
                registry,
                runtime,
                check_interval: AtomicI64::new(check_interval),
                statuses: Mutex::new(HashMap::new()),
            }),
            task: None,
        }
    }
    pub fn check_interval(&self) -> i64 {
        self.collaborators.check_interval.load(Ordering::Acquire)
    }
    /// A changed interval is read at the next sleep, just as the source field is.
    pub fn set_check_interval(&self, seconds: i64) {
        self.collaborators
            .check_interval
            .store(seconds, Ordering::Release);
    }
    pub async fn start(&mut self) {
        if self.task.is_none() {
            let collaborators = self.collaborators.clone();
            self.task = Some(tokio::spawn(async move {
                loop {
                    collaborators.check_once().await?;
                    collaborators
                        .runtime
                        .sleep(collaborators.check_interval.load(Ordering::Acquire))
                        .await;
                }
            }));
        }
    }
    pub async fn stop(&mut self) {
        self.abort_task();
    }
    /// Includes a finished/failed handle until stop is called.
    pub fn has_task(&self) -> bool {
        self.task.is_some()
    }
    pub fn task_is_finished(&self) -> bool {
        self.task.as_ref().is_some_and(JoinHandle::is_finished)
    }
    pub async fn check_service(
        &self,
        service: ServiceHandle,
    ) -> Result<HealthStatus, HealthMonitorError> {
        self.collaborators.check_service(service).await
    }
    /// One sequential list-and-check pass, without spawning or sleeping.
    /// Hosts that own scheduling can use this deterministic source-loop seam.
    pub async fn check_once(&self) -> Result<(), HealthMonitorError> {
        self.collaborators.check_once().await
    }
    pub fn get_status(&self, service_id: &str) -> Option<Arc<HealthStatus>> {
        self.collaborators
            .statuses
            .lock()
            .expect("health statuses poisoned")
            .get(service_id)
            .cloned()
    }
    pub fn all_status(&self) -> HashMap<String, Arc<HealthStatus>> {
        self.collaborators
            .statuses
            .lock()
            .expect("health statuses poisoned")
            .clone()
    }
    fn abort_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Drop for HealthMonitor {
    fn drop(&mut self) {
        self.abort_task();
    }
}
