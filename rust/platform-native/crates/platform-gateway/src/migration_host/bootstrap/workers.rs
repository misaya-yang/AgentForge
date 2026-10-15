//! Owned source worker tasks. No worker starts until Lifecycle::start is called.
use super::{BootstrapError, DatabasePools, graph::GatewayGraph};
use crate::{
    billing_scheduler::{PgMaintenanceStorage, UsageScheduler, UsageSchedulerConfig},
    migration_content::images::ImageTaskWorker,
    migration_host::lifecycle::{HostError, Lifecycle, LifecycleStep, SHUTDOWN_ORDER},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
struct RunningFiles {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}
pub struct GatewayWorkers {
    pub maintenance: Arc<UsageScheduler>,
    image: Mutex<Option<JoinHandle<()>>>,
    files: Mutex<Option<RunningFiles>>,
    scheduled: Mutex<Option<RunningFiles>>,
    sdk_cleanup: Mutex<Option<RunningFiles>>,
}
impl GatewayWorkers {
    pub fn new(
        pools: &DatabasePools,
        config: UsageSchedulerConfig,
    ) -> Result<Arc<Self>, BootstrapError> {
        let storage = Arc::new(
            PgMaintenanceStorage::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("billing_maintenance"))?,
        );
        Ok(Arc::new(Self {
            maintenance: Arc::new(UsageScheduler::new(storage, config)),
            image: Mutex::new(None),
            files: Mutex::new(None),
            scheduled: Mutex::new(None),
            sdk_cleanup: Mutex::new(None),
        }))
    }
    async fn start_images(
        self: &Arc<Self>,
        worker: Arc<ImageTaskWorker>,
        flag: Arc<std::sync::atomic::AtomicBool>,
    ) {
        let mut running = self.image.lock().await;
        if running.is_none() {
            *running = Some(tokio::spawn(async move {
                struct RunningFlag(Arc<std::sync::atomic::AtomicBool>);
                impl Drop for RunningFlag {
                    fn drop(&mut self) {
                        self.0.store(false, std::sync::atomic::Ordering::Release);
                    }
                }
                flag.store(true, std::sync::atomic::Ordering::Release);
                let _guard = RunningFlag(flag);
                worker.run(Duration::from_secs(1)).await
            }));
        }
    }
    async fn stop_images(&self, worker: Arc<ImageTaskWorker>) -> Result<(), HostError> {
        worker.stop();
        if let Some(task) = self.image.lock().await.take() {
            task.await
                .map_err(|_| HostError::Shutdown("image_task_worker"))?;
        }
        worker.drain_and_wait().await;
        Ok(())
    }
}
impl GatewayGraph {
    /// Build once per host process. External broker/telemetry loops can be added
    /// by name; these steps never use a readiness-success placeholder.
    pub fn lifecycle(
        &self,
        workers: Arc<GatewayWorkers>,
        mut extra_start: Vec<LifecycleStep>,
        mut extra_stop: Vec<LifecycleStep>,
    ) -> Lifecycle {
        let (mut starts, mut stops) = self.core.lifecycle_steps();
        let (evaluation_start, evaluation_stop) = self.evaluation.lifecycle_steps();
        starts.extend(evaluation_start);
        stops.extend(evaluation_stop);
        let owner = workers.clone();
        let agent = self.runtime.agent.clone();
        starts.push(LifecycleStep {
            name: "scheduled_task_dispatcher",
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                let agent = agent.clone();
                Box::pin(async move {
                    if !agent.runtime_enabled { return Ok(()); }
                    let mut running = owner.scheduled.lock().await;
                    if running.is_none() {
                        // Required canonical epoch must be installed before activation.
                        sqlx::query("SELECT t.task_id,e.execution_id FROM gateway.scheduled_tasks t CROSS JOIN gateway.scheduled_task_executions e LIMIT 0")
                            .execute(&agent.repository.pool).await.map_err(|_| HostError::Database)?;
                        let (stop, signal) = watch::channel(false);
                        *running = Some(RunningFiles {
                            stop,
                            task: tokio::spawn(crate::migration_agent::scheduled::run(agent, signal)),
                        });
                    }
                    Ok(())
                })
            }),
        });
        let owner = workers.clone();
        stops.push(LifecycleStep {
            name: "scheduled_task_dispatcher",
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                Box::pin(async move {
                    if let Some(running) = owner.scheduled.lock().await.take() {
                        let _ = running.stop.send(true);
                        running.task.await.map_err(|_| HostError::Shutdown("scheduled_task_dispatcher"))?;
                    }
                    Ok(())
                })
            }),
        });
        let owner = workers.clone();
        let pool = self.pools.data.clone();
        starts.push(LifecycleStep {
            name: "sdk_session_cleanup",
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                let pool = pool.clone();
                Box::pin(async move {
                    let mut running = owner.sdk_cleanup.lock().await;
                    if running.is_none() {
                        sqlx::query("SELECT b.session_id,s.state FROM gateway.sdk_session_barriers b CROSS JOIN gateway.sdk_session_scopes s LIMIT 0")
                            .execute(&pool).await.map_err(|_| HostError::Database)?;
                        let (stop, signal) = watch::channel(false);
                        *running = Some(RunningFiles { stop, task: tokio::spawn(
                            crate::migration_adapters::sdk_session_cleanup::run(pool, signal)) });
                    }
                    Ok(())
                })
            }),
        });
        let owner = workers.clone();
        stops.push(LifecycleStep {
            name: "sdk_session_cleanup",
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                Box::pin(async move {
                    if let Some(running) = owner.sdk_cleanup.lock().await.take() {
                        let _ = running.stop.send(true);
                        running.task.await.map_err(|_| HostError::Shutdown("sdk_session_cleanup"))?;
                    }
                    Ok(())
                })
            }),
        });
        let registry = self.core.registry.clone();
        starts.insert(
            0,
            LifecycleStep {
                name: "service_registry_load",
                best_effort: true,
                operation: Arc::new(move || {
                    let registry = registry.clone();
                    Box::pin(async move {
                        let rows = registry
                            .storage
                            .list()
                            .await
                            .map_err(|_| HostError::Database)?;
                        for row in rows {
                            registry.replace_service_cache(row);
                        }
                        Ok(())
                    })
                }),
            },
        );
        let owner = workers.clone();
        let image = self.content.content.image_worker.clone();
        let image_running = self.image_worker_running.clone();
        starts.push(LifecycleStep {
            name: "image_task_worker",
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                let image = image.clone();
                let image_running = image_running.clone();
                Box::pin(async move {
                    owner.start_images(image, image_running).await;
                    Ok(())
                })
            }),
        });
        let owner = workers.clone();
        let cleanup = self.content.content.public.files.cleanup.clone();
        starts.push(LifecycleStep {
            name: "file_cleanup_service",
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                let cleanup = cleanup.clone();
                Box::pin(async move {
                    let mut running = owner.files.lock().await;
                    if running.is_none() {
                        let (stop, signal) = watch::channel(false);
                        *running = Some(RunningFiles {
                            stop,
                            task: tokio::spawn(async move { cleanup.run_scheduler(signal).await }),
                        });
                    }
                    Ok(())
                })
            }),
        });
        let maintenance = workers.maintenance.clone();
        starts.push(LifecycleStep {
            name: "usage_scheduler",
            best_effort: true,
            operation: Arc::new(move || {
                let maintenance = maintenance.clone();
                Box::pin(async move {
                    maintenance
                        .start()
                        .await
                        .map_err(|_| HostError::Startup("usage_scheduler"))
                })
            }),
        });
        let (observability_start, observability_stop) = self.observability.lifecycle_steps();
        starts.extend(observability_start);
        stops.extend(observability_stop);
        let owner = workers.clone();
        let image = self.content.content.image_worker.clone();
        stops.push(LifecycleStep {
            name: "image_task_worker",
            best_effort: true,
            operation: Arc::new(move || {
                let owner = owner.clone();
                let image = image.clone();
                Box::pin(async move { owner.stop_images(image).await })
            }),
        });
        let owner = workers.clone();
        stops.push(LifecycleStep {
            name: "file_cleanup_service",
            best_effort: true,
            operation: Arc::new(move || {
                let owner = owner.clone();
                Box::pin(async move {
                    if let Some(running) = owner.files.lock().await.take() {
                        let _ = running.stop.send(true);
                        running
                            .task
                            .await
                            .map_err(|_| HostError::Shutdown("file_cleanup_service"))?;
                    }
                    Ok(())
                })
            }),
        });
        let maintenance = workers.maintenance.clone();
        stops.push(LifecycleStep {
            name: "usage_scheduler",
            best_effort: true,
            operation: Arc::new(move || {
                let maintenance = maintenance.clone();
                Box::pin(async move {
                    maintenance
                        .stop()
                        .await
                        .map_err(|_| HostError::Shutdown("usage_scheduler"))
                })
            }),
        });
        let discovery = self.foundation.mcp_broker.clone();
        let invocation = self.compat.mcp.clone();
        stops.push(LifecycleStep {
            name: "mcp_gateway_broker",
            best_effort: true,
            operation: Arc::new(move || {
                let discovery = discovery.clone();
                let invocation = invocation.clone();
                Box::pin(async move {
                    invocation.close().await;
                    discovery.close().await;
                    Ok(())
                })
            }),
        });
        // Drain supervised DB operations after request drain and worker stop.
        let sessions = self.core.session_storage.clone();
        let tasks = self.core.task_storage.clone();
        let deliveries = self.core.tasks.clone();
        let content = self.content.database.clone();
        let memory = self.runtime.memory.clone();
        let mcp = self.runtime.mcp_database.clone();
        let native_foundation = self.native_foundation.clone();
        let pools = self.pools.clone();
        stops.push(LifecycleStep {
            name: "database",
            best_effort: false,
            operation: Arc::new(move || {
                let sessions = sessions.clone();
                let tasks = tasks.clone();
                let deliveries = deliveries.clone();
                let content = content.clone();
                let memory = memory.clone();
                let mcp = mcp.clone();
                let pools = pools.clone();
                let native_foundation = native_foundation.clone();
                Box::pin(async move {
                    let mut failed = native_foundation.drain().await.is_err();
                    failed |= sessions.drain_cancelled().await.is_err();
                    failed |= tasks.drain_cancelled().await.is_err();
                    failed |= deliveries.drain_cancelled_deliveries().await.is_err();
                    failed |= content.drain_cancelled().await.is_err();
                    failed |= memory.drain_cancelled().await.is_err();
                    failed |= mcp.drain_cancelled().await.is_err();
                    if failed {
                        return Err(HostError::Shutdown("database"));
                    }
                    pools.data.close().await;
                    pools.control.close().await;
                    Ok(())
                })
            }),
        });
        starts.append(&mut extra_start);
        stops.append(&mut extra_stop);
        stops.sort_by_key(|step| {
            SHUTDOWN_ORDER
                .iter()
                .position(|name| *name == step.name)
                .unwrap_or(0)
        });
        Lifecycle::new(starts, stops)
    }
}
