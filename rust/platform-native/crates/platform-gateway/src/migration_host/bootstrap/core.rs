//! Container's registry, durable sessions/tasks, dispatch and worker singletons.
use super::{BootstrapError, DatabasePools};
use crate::{
    capacity::CapacityResolver,
    capacity_admission::{
        AdmissionOptions, CapacityAdmissionController, CapacityStore, RedisCapacityStore,
    },
    dispatch_preflight::{DispatchPreflight, RateLimitEventRecorder},
    dispatcher::DispatchExecutor,
    dispatcher_rate_limiter::{RateLimitConfig, RateLimiter},
    gateway_dispatcher::GatewayDispatcher,
    registry_health_monitor::{HealthMonitor, SystemHealthMonitorRuntime},
    registry_storage::PgRegistryStorage,
    service_registry::ServiceRegistry,
    session_manager::{DatabaseSessionConfig, DatabaseSessionManager},
    session_storage::PgSessionStorage,
    task_actor::PgTaskActorValidator,
    task_delivery::PgTaskDeliveryLedger,
    task_manager::{MemoryTaskQueue, SafeCallbackTransport, TaskManager},
    task_storage::PgTaskStorage,
    task_worker::TaskWorker,
    usage_pricing::{PricedUsageSink, ScopedUsagePricing},
    usage_recorder::UsageRecorder,
};
use ai_platform_auth::rbac::Rbac;
use redis::aio::ConnectionManager;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::Mutex;

pub struct CoreConfiguration {
    pub sessions: DatabaseSessionConfig,
    pub roles: HashMap<String, Vec<String>>,
    pub rate_limits: RateLimitConfig,
    pub capacity: CapacityResolver,
    pub admission: AdmissionOptions,
    pub adapter_bulkhead_limits: String,
    pub health_check_interval: i64,
    pub worker_concurrency: i64,
    pub durable_tasks: bool,
}
pub struct CoreBackends {
    pub callbacks: Arc<dyn SafeCallbackTransport>,
    pub rate_events: Arc<dyn RateLimitEventRecorder>,
    pub langgraph_prepare: Arc<dyn crate::migration_adapters::LangGraphPayloadPreparation>,
    pub callback_allowlist: Vec<String>,
}
pub struct CoreServices {
    pub registry: Arc<ServiceRegistry>,
    pub sessions: Arc<DatabaseSessionManager>,
    pub session_storage: Arc<PgSessionStorage>,
    pub tasks: Arc<TaskManager>,
    pub task_storage: Arc<PgTaskStorage>,
    pub dispatcher: Arc<GatewayDispatcher>,
    pub capacity: Arc<CapacityResolver>,
    pub admission: Arc<CapacityAdmissionController>,
    pub rate_limiter: Arc<RateLimiter>,
    pub rbac: Arc<Rbac>,
    pub health_monitor: Arc<Mutex<HealthMonitor>>,
    pub task_worker: Arc<Mutex<TaskWorker>>,
    pub pricing: Arc<ScopedUsagePricing>,
    pub worker_concurrency: i64,
    pub(crate) sdk_worker_config:
        Option<crate::migration_adapters::sdk_session_cleanup::SdkAgentWorkerConfig>,
}
impl CoreServices {
    pub fn new(
        pools: &DatabasePools,
        redis: Option<ConnectionManager>,
        config: CoreConfiguration,
        backends: CoreBackends,
        usage: &UsageRecorder,
        operator_environment: &BTreeMap<String, String>,
    ) -> Result<Self, BootstrapError> {
        let sdk_worker_config = crate::migration_adapters::sdk_session_cleanup::SdkAgentWorkerConfig::from_operator_environment(
            operator_environment,
        );
        let registry = Arc::new(ServiceRegistry::with_bulkhead_limits(
            Arc::new(PgRegistryStorage::new(pools.data.clone())),
            config.adapter_bulkhead_limits,
        ));
        crate::migration_adapters::register_adapters_with_langgraph(
            &registry,
            crate::migration_adapters::LangGraphAdapterOptions {
                redis: redis.clone(),
                prepare: Some(backends.langgraph_prepare),
                callback_allowlist: backends.callback_allowlist,
            },
        );
        crate::migration_adapters::register_sdk_session_adapters(
            &registry,
            &pools.data,
            sdk_worker_config.clone(),
        );
        let session_storage = Arc::new(
            PgSessionStorage::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("session_storage"))?,
        );
        let sessions = Arc::new(DatabaseSessionManager::new(
            session_storage.clone(),
            config.sessions,
        ));
        let task_storage = Arc::new(
            PgTaskStorage::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("task_storage"))?,
        );
        // Explicit rollout selection. Canonical17 keeps the existing memory
        // path; durable mode never downgrades after a ledger/authority failure.
        let queue = if config.durable_tasks {
            None
        } else {
            Some(Arc::new(MemoryTaskQueue::new()))
        };
        let actors = if config.durable_tasks {
            if config.worker_concurrency <= 0 {
                return Err(BootstrapError::Configuration("task_worker_concurrency"));
            }
            Some(Arc::new(PgTaskActorValidator::new(
                pools.data.clone(),
                redis
                    .clone()
                    .ok_or(BootstrapError::Configuration("task_session_authority"))?,
            )))
        } else {
            None
        };
        let manager = if let Some(queue) = &queue {
            TaskManager::new(task_storage.clone(), queue.clone())
        } else {
            let delivery = Arc::new(
                PgTaskDeliveryLedger::new(pools.data.clone(), pools.control.clone())
                    .map_err(|_| BootstrapError::Construction("task_delivery"))?,
            );
            TaskManager::durable(task_storage.clone(), delivery)
        };
        let tasks = Arc::new(manager.with_safe_callback_transport(backends.callbacks));
        let rbac = Arc::new(Rbac::new(config.roles));
        let rate_limiter = Arc::new(RateLimiter::new(config.rate_limits));
        let preflight = Arc::new(DispatchPreflight::new(
            registry.clone(),
            rbac.clone(),
            rate_limiter.clone(),
            backends.rate_events,
        ));
        let capacity = Arc::new(config.capacity);
        let store = redis.map(|r| Arc::new(RedisCapacityStore::new(r)) as Arc<dyn CapacityStore>);
        let admission = Arc::new(
            CapacityAdmissionController::new(config.admission, store)
                .map_err(|_| BootstrapError::Configuration("capacity_admission"))?,
        );
        admission.set_rejection_observer(Arc::new(
            crate::migration_host::metrics::AdmissionRejectionMetrics::new(
                &crate::migration_host::metrics::global(),
            ),
        ));
        let executor = Arc::new(DispatchExecutor::new(
            admission.clone(),
            (*capacity).clone(),
        ));
        let pricing = Arc::new(ScopedUsagePricing::with_postgres(pools.data.clone()));
        let sink = Arc::new(PricedUsageSink::new(usage.clone(), pricing.clone()));
        let dispatcher = Arc::new(GatewayDispatcher::new(
            preflight,
            executor,
            sessions.clone(),
            tasks.clone(),
            sink,
        ));
        let task_worker = Arc::new(Mutex::new(match (queue, actors) {
            (Some(queue), None) => TaskWorker::new(
                queue,
                task_storage.clone(),
                dispatcher.clone(),
                tasks.clone(),
            ),
            (None, Some(actors)) => TaskWorker::durable(
                task_storage.clone(),
                dispatcher.clone(),
                tasks.clone(),
                actors,
            )
            .map_err(|_| BootstrapError::Construction("task_worker"))?,
            _ => return Err(BootstrapError::Construction("task_delivery_mode")),
        }));
        let health_monitor = Arc::new(Mutex::new(HealthMonitor::with_runtime(
            registry.clone(),
            config.health_check_interval,
            Arc::new(SystemHealthMonitorRuntime),
        )));
        Ok(Self {
            registry,
            sessions,
            session_storage,
            tasks,
            task_storage,
            dispatcher,
            capacity,
            admission,
            rate_limiter,
            rbac,
            health_monitor,
            task_worker,
            pricing,
            worker_concurrency: config.worker_concurrency,
            sdk_worker_config,
        })
    }
    /// Source _load_services_from_database populates caches without rewriting rows.
    pub async fn load_registered_services(&self) -> Result<(), BootstrapError> {
        let services = self
            .registry
            .storage
            .list()
            .await
            .map_err(|_| BootstrapError::Construction("service_registry_load"))?;
        for service in services {
            self.registry.replace_service_cache(service);
        }
        Ok(())
    }
    pub fn lifecycle_steps(
        &self,
    ) -> (
        Vec<super::super::lifecycle::LifecycleStep>,
        Vec<super::super::lifecycle::LifecycleStep>,
    ) {
        use super::super::lifecycle::LifecycleStep;
        let health = self.health_monitor.clone();
        let health_start = LifecycleStep {
            name: "health_monitor",
            best_effort: false,
            operation: Arc::new(move || {
                let health = health.clone();
                Box::pin(async move {
                    health.lock().await.start().await;
                    Ok(())
                })
            }),
        };
        let worker = self.task_worker.clone();
        let concurrency = self.worker_concurrency;
        let worker_start = LifecycleStep {
            name: "task_worker",
            best_effort: false,
            operation: Arc::new(move || {
                let worker = worker.clone();
                Box::pin(async move {
                    let mut worker = worker.lock().await;
                    worker.verify_ready().await.map_err(|_| {
                        super::super::lifecycle::HostError::Startup("task_delivery")
                    })?;
                    worker.start(concurrency).await;
                    Ok(())
                })
            }),
        };
        let worker = self.task_worker.clone();
        let worker_stop = LifecycleStep {
            name: "task_worker",
            best_effort: true,
            operation: Arc::new(move || {
                let worker = worker.clone();
                Box::pin(async move {
                    worker.lock().await.stop().await;
                    Ok(())
                })
            }),
        };
        let health = self.health_monitor.clone();
        let health_stop = LifecycleStep {
            name: "health_monitor",
            best_effort: true,
            operation: Arc::new(move || {
                let health = health.clone();
                Box::pin(async move {
                    health.lock().await.stop().await;
                    Ok(())
                })
            }),
        };
        (
            vec![health_start, worker_start],
            vec![worker_stop, health_stop],
        )
    }
}
