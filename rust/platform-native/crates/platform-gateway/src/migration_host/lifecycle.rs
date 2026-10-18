//! Ordered lifecycle callbacks preserve independent source startup/shutdown effects.
use super::middleware::DrainState;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::Mutex;
pub type HostFuture = Pin<Box<dyn Future<Output = Result<(), HostError>> + Send + 'static>>;
pub type HostOperation = Arc<dyn Fn() -> HostFuture + Send + Sync>;
#[derive(Debug, Clone, Copy)]
pub enum HostError {
    Configuration,
    Database,
    Redis,
    Listener,
    Startup(&'static str),
    Shutdown(&'static str),
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for HostError {}
#[derive(Clone)]
pub struct LifecycleStep {
    pub name: &'static str,
    pub operation: HostOperation,
    pub best_effort: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum LifecycleState {
    Idle,
    Running,
    Failed,
}
pub struct Lifecycle {
    startup: Vec<LifecycleStep>,
    shutdown: Vec<LifecycleStep>,
    initialized: Mutex<LifecycleState>,
}
impl Lifecycle {
    pub fn new(startup: Vec<LifecycleStep>, shutdown: Vec<LifecycleStep>) -> Self {
        Self {
            startup,
            shutdown,
            initialized: Mutex::new(LifecycleState::Idle),
        }
    }
    pub async fn start(&self) -> Result<(), HostError> {
        let mut initialized = self.initialized.lock().await;
        match *initialized {
            LifecycleState::Running => return Ok(()),
            LifecycleState::Failed => return Err(HostError::Startup("previous_lifecycle_failure")),
            LifecycleState::Idle => {}
        }
        // Mark partial startup owned before the first side effect. If a later
        // required step fails, run the full canonical cleanup, including the
        // partially started step and database operation supervisors.
        *initialized = LifecycleState::Failed;
        for step in &self.startup {
            if (step.operation)().await.is_err() && !step.best_effort {
                let mut failed = false;
                for cleanup in &self.shutdown {
                    if (cleanup.operation)().await.is_err() && !cleanup.best_effort {
                        failed = true;
                    }
                }
                if !failed {
                    *initialized = LifecycleState::Idle;
                }
                return Err(HostError::Startup(step.name));
            }
        }
        *initialized = LifecycleState::Running;
        Ok(())
    }
    pub async fn stop(&self, drain: &DrainState) -> Result<(), HostError> {
        self.stop_with_drain_timeout(drain, Duration::from_secs(30))
            .await
    }
    /// The server may already have spent its request-drain budget.
    pub async fn stop_with_drain_timeout(
        &self,
        drain: &DrainState,
        drain_timeout: Duration,
    ) -> Result<(), HostError> {
        let mut initialized = self.initialized.lock().await;
        if *initialized == LifecycleState::Idle {
            return Ok(());
        }
        let _ = drain.wait_drained(drain_timeout).await;
        let mut failure = None;
        for step in &self.shutdown {
            if (step.operation)().await.is_err() && !step.best_effort && failure.is_none() {
                failure = Some(HostError::Shutdown(step.name));
            }
        }
        *initialized = if failure.is_some() {
            LifecycleState::Failed
        } else {
            LifecycleState::Idle
        };
        failure.map_or(Ok(()), Err)
    }
}
/// This ordering is the canonical shutdown contract, not alphabetical service order.
pub const SHUTDOWN_ORDER: &[&str] = &[
    "sdk_session_cleanup",
    "scheduled_task_dispatcher",
    "usage_event_consumer",
    "usage_event_consumer_task",
    "file_cleanup_service",
    "eval_outbox_worker",
    "trace_retention_scheduler",
    "assistant_trace_reconciler_task",
    "usage_scheduler",
    "billing_interceptor",
    "usage_recorder",
    "usage_event_publisher",
    "image_task_worker",
    "agent_model_plane",
    "mcp_gateway_broker",
    "image_generation_service",
    "agent_runtime_control",
    "agent_capability_catalog_service",
    "agent_runtime_knowledge_resolver",
    "assistant_task_manager",
    "file_storage",
    "task_manager",
    "task_worker",
    "health_monitor",
    "langgraph_proxy",
    "transparent_proxy",
    "container_billing_interceptor",
    "service_adapters",
    "authorization_decision_log",
    "gateway_ha_directory",
    "database",
    "redis",
];
