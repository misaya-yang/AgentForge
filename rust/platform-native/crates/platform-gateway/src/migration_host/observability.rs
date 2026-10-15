//! Concrete trace/usage construction and owned process lifecycle bindings.
//! Construction is inert. The caller decides when the application is started.
use super::lifecycle::{HostError, LifecycleStep};
use crate::{
    migration_trace::{
        TraceRepository,
        retention::{RetentionConfig, TraceRetentionScheduler},
        usage::SampledUsageTraceWriter,
    },
    usage_recorder::{PgUsagePersistence, RecorderConfig, UsageEventPublisher, UsageRecorder},
};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};

struct RunningReconciler {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
pub struct ObservabilityServices {
    pub traces: Arc<TraceRepository>,
    pub usage: Arc<UsageRecorder>,
    pub retention: Arc<TraceRetentionScheduler>,
    retention_days: i32,
    reconciler: Mutex<Option<RunningReconciler>>,
}
impl ObservabilityServices {
    pub fn new(
        pool: PgPool,
        control_pool: PgPool,
        usage_config: RecorderConfig,
        retention_config: RetentionConfig,
        publisher: Option<Arc<dyn UsageEventPublisher>>,
    ) -> Result<Arc<Self>, HostError> {
        let traces = Arc::new(
            TraceRepository::new(pool.clone(), control_pool).map_err(|_| HostError::Database)?,
        );
        let persistence = Arc::new(
            PgUsagePersistence::new(pool)
                .with_trace_writer(Arc::new(SampledUsageTraceWriter::default())),
        );
        let mut usage = UsageRecorder::new(Some(persistence), usage_config);
        if let Some(publisher) = publisher {
            usage = usage.with_publisher(publisher);
        }
        let retention_days = retention_config.retention_days;
        Ok(Arc::new(Self {
            retention: TraceRetentionScheduler::new(traces.clone(), retention_config),
            traces,
            usage: Arc::new(usage),
            retention_days,
            reconciler: Mutex::new(None),
        }))
    }
    pub async fn start_reconciler(self: &Arc<Self>) {
        let mut running = self.reconciler.lock().await;
        if running.is_some() {
            return;
        }
        let (stop, signal) = watch::channel(false);
        let repository = self.traces.clone();
        let retention_days = self.retention_days;
        let task = tokio::spawn(async move {
            repository
                .run_assistant_trace_reconciler(
                    retention_days,
                    100,
                    Duration::from_secs(30),
                    signal,
                )
                .await;
        });
        *running = Some(RunningReconciler { stop, task });
    }
    pub async fn stop_reconciler(&self) {
        let mut running = self.reconciler.lock().await;
        if let Some(worker) = running.take() {
            let _ = worker.stop.send(true);
            worker.task.abort();
            let _ = worker.task.await;
            let _ = self.traces.drain_cancelled().await;
        }
    }
    /// Insert these named operations into the canonical host lifecycle order.
    /// Retention startup is explicit because it performs configured data expiry.
    pub fn lifecycle_steps(self: &Arc<Self>) -> (Vec<LifecycleStep>, Vec<LifecycleStep>) {
        let retention = self.retention.clone();
        let retention_start = LifecycleStep {
            name: "trace_retention_scheduler",
            best_effort: true,
            operation: Arc::new(move || {
                let service = retention.clone();
                Box::pin(async move {
                    service.start().await;
                    Ok(())
                })
            }),
        };
        let reconciler = self.clone();
        let reconcile_start = LifecycleStep {
            name: "assistant_trace_reconciler_task",
            best_effort: true,
            operation: Arc::new(move || {
                let service = reconciler.clone();
                Box::pin(async move {
                    service.start_reconciler().await;
                    Ok(())
                })
            }),
        };
        let usage = self.usage.clone();
        let usage_start = LifecycleStep {
            name: "usage_recorder",
            best_effort: true,
            operation: Arc::new(move || {
                let service = usage.clone();
                Box::pin(async move {
                    service.start().await;
                    Ok(())
                })
            }),
        };
        let retention = self.retention.clone();
        let retention_stop = LifecycleStep {
            name: "trace_retention_scheduler",
            best_effort: true,
            operation: Arc::new(move || {
                let service = retention.clone();
                Box::pin(async move {
                    service.stop().await;
                    Ok(())
                })
            }),
        };
        let reconciler = self.clone();
        let reconcile_stop = LifecycleStep {
            name: "assistant_trace_reconciler_task",
            best_effort: true,
            operation: Arc::new(move || {
                let service = reconciler.clone();
                Box::pin(async move {
                    service.stop_reconciler().await;
                    Ok(())
                })
            }),
        };
        let usage = self.usage.clone();
        let usage_stop = LifecycleStep {
            name: "usage_recorder",
            best_effort: true,
            operation: Arc::new(move || {
                let service = usage.clone();
                Box::pin(async move {
                    let result = service.stop().await;
                    if result.raw_error.is_some()
                        || result.uncertain_records > 0
                        || !result.stage_failures.is_empty()
                    {
                        Err(HostError::Shutdown("usage_recorder"))
                    } else {
                        Ok(())
                    }
                })
            }),
        };
        (
            vec![usage_start, retention_start, reconcile_start],
            vec![retention_stop, reconcile_stop, usage_stop],
        )
    }
}
