//! Source retention scheduler. Construction is inert; the host owns start/stop.
use super::*;
use chrono::{NaiveDate, Timelike, Utc};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};
#[derive(Clone)]
pub struct RetentionConfig {
    pub retention_days: i32,
    pub request_trace_retention_days: i32,
    pub cleanup_hour: u32,
    pub cleanup_minute: u32,
    pub batch_size: i64,
}
impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            retention_days: 90,
            request_trace_retention_days: 14,
            cleanup_hour: 2,
            cleanup_minute: 15,
            batch_size: 500,
        }
    }
}
struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
pub struct TraceRetentionScheduler {
    repository: Arc<TraceRepository>,
    config: RetentionConfig,
    running: Mutex<Option<Running>>,
    last_cleanup: Mutex<Option<NaiveDate>>,
}
impl TraceRetentionScheduler {
    pub fn new(repository: Arc<TraceRepository>, mut config: RetentionConfig) -> Arc<Self> {
        config.request_trace_retention_days = config.request_trace_retention_days.clamp(7, 14);
        Arc::new(Self {
            repository,
            config,
            running: Mutex::new(None),
            last_cleanup: Mutex::new(None),
        })
    }
    pub async fn start(self: &Arc<Self>) {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return;
        }
        let (stop, mut signal) = watch::channel(false);
        let service = self.clone();
        let task = tokio::spawn(async move {
            loop {
                if *signal.borrow() {
                    break;
                }
                let now = Utc::now();
                let last = *service.last_cleanup.lock().await;
                let due = last != Some(now.date_naive())
                    && (now.hour() > service.config.cleanup_hour
                        || now.hour() == service.config.cleanup_hour
                            && now.minute() >= service.config.cleanup_minute);
                if due && service.run_cleanup_once().await.is_ok() {
                    *service.last_cleanup.lock().await = Some(now.date_naive());
                }
                tokio::select! {_=tokio::time::sleep(Duration::from_secs(300))=>{},_=signal.changed()=>{if *signal.borrow(){break;}}}
            }
        });
        *running = Some(Running { stop, task });
    }
    pub async fn stop(&self) {
        let running = self.running.lock().await.take();
        if let Some(running) = running {
            let _ = running.stop.send(true);
            running.task.abort();
            let _ = running.task.await;
            let _ = self.repository.drain_cancelled().await;
        }
    }
    pub async fn run_cleanup_once(&self) -> Result<usize, TraceError> {
        let mut total = 0;
        loop {
            let deleted = self
                .repository
                .purge_expired_traces(self.config.retention_days, self.config.batch_size)
                .await?;
            total += deleted;
            if (deleted as i64) < self.config.batch_size {
                break;
            }
        }
        let cutoff =
            Utc::now() - chrono::Duration::days(self.config.request_trace_retention_days as i64);
        total += self
            .repository
            .run(move |c| {
                Box::pin(async move {
                    sqlx::query("DELETE FROM request_traces WHERE created_at < $1")
                        .bind(cutoff)
                        .execute(c)
                        .await
                        .map(|r| r.rows_affected() as usize)
                })
            })
            .await
            .unwrap_or(0);
        Ok(total)
    }
}
