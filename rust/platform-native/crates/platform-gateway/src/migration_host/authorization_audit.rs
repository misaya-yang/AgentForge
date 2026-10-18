//! Bounded per-process decision log writer. No task or I/O until lifecycle start.
//! Each immutable decision UUID is idempotent; retries never duplicate audit rows.
use super::lifecycle::HostError;
use ai_platform_auth::cedar::{DecisionAudit, DecisionLog, PolicyError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinHandle,
};
struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<(), HostError>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
struct AuditState {
    healthy: AtomicBool,
    accepting: AtomicBool,
    terminal_failure: AtomicBool,
    failed: AtomicU64,
    confirmed: AtomicU64,
}
pub struct DecisionLogWriter {
    pool: sqlx::PgPool,
    sender: mpsc::Sender<DecisionLog>,
    receiver: Mutex<Option<mpsc::Receiver<DecisionLog>>>,
    running: Mutex<Option<Running>>,
    state: Arc<AuditState>,
}
impl DecisionLogWriter {
    pub fn new(pool: sqlx::PgPool) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel(2048);
        Arc::new(Self {
            pool,
            sender,
            receiver: Mutex::new(Some(receiver)),
            running: Mutex::new(None),
            state: Arc::new(AuditState {
                healthy: AtomicBool::new(false),
                accepting: AtomicBool::new(true),
                terminal_failure: AtomicBool::new(false),
                failed: AtomicU64::new(0),
                confirmed: AtomicU64::new(0),
            }),
        })
    }
    pub fn healthy(&self) -> bool {
        self.ready()
    }
    pub fn confirmed(&self) -> u64 {
        self.state.confirmed.load(Ordering::Relaxed)
    }
    pub fn failed(&self) -> u64 {
        self.state.failed.load(Ordering::Relaxed)
    }
    pub async fn start(self: &Arc<Self>) -> Result<(), HostError> {
        let mut running = self.running.lock().await;
        if let Some(handle) = running.as_ref() {
            return if handle.task.is_finished() {
                Err(HostError::Startup("authorization_decision_log_failed"))
            } else {
                Ok(())
            };
        }
        let installed:bool=tokio::time::timeout(Duration::from_secs(3),sqlx::query_scalar("SELECT to_regclass('gateway.authorization_decisions') IS NOT NULL AND COALESCE(has_table_privilege(current_user,to_regclass('gateway.authorization_decisions'),'INSERT'),false) AND to_regclass('gateway.authorization_resource_decisions') IS NOT NULL AND COALESCE(has_table_privilege(current_user,to_regclass('gateway.authorization_resource_decisions'),'INSERT'),false)").fetch_one(&self.pool)).await.map_err(|_|HostError::Database)?.map_err(|_|HostError::Database)?;
        if !installed {
            return Err(HostError::Startup("authorization_decision_log_schema"));
        }
        let receiver = self
            .receiver
            .lock()
            .await
            .take()
            .ok_or(HostError::Startup("authorization_decision_log_reuse"))?;
        self.state.healthy.store(true, Ordering::Release);
        let (stop, signal) = watch::channel(false);
        let pool = self.pool.clone();
        let state = self.state.clone();
        *running = Some(Running {
            stop,
            task: tokio::spawn(async move {
                let result = Self::run(pool, state.clone(), receiver, signal).await;
                state.healthy.store(false, Ordering::Release);
                state
                    .terminal_failure
                    .store(result.is_err(), Ordering::Release);
                result
            }),
        });
        Ok(())
    }
    async fn write(pool: &sqlx::PgPool, event: &DecisionLog) -> Result<(), HostError> {
        if let Some(resource) = &event.resource {
            let value = serde_json::to_value(resource).map_err(|_| HostError::Database)?;
            tokio::time::timeout(Duration::from_secs(3),sqlx::query("INSERT INTO gateway.authorization_resource_decisions(decision_id,capability,policy_sha256,matrix_sha256,principal_fingerprint,mode,legacy_allowed,cedar_allowed,enforced_allowed,evaluation_error,resource_context) VALUES($1::uuid,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(decision_id) DO NOTHING").bind(&event.decision_id).bind(&event.capability).bind(&event.policy_sha256).bind(&event.matrix_sha256).bind(&event.principal_fingerprint).bind(match event.mode{ai_platform_auth::cedar::Mode::Off=>"off",ai_platform_auth::cedar::Mode::Shadow=>"shadow",ai_platform_auth::cedar::Mode::Enforce=>"enforce"}).bind(event.legacy_allowed).bind(event.cedar_allowed).bind(event.enforced_allowed).bind(event.evaluation_error).bind(value).execute(pool)).await.map_err(|_|HostError::Database)?.map_err(|_|HostError::Database)?;
            return Ok(());
        }

        tokio::time::timeout(Duration::from_secs(3),sqlx::query("INSERT INTO gateway.authorization_decisions(decision_id,capability,policy_sha256,matrix_sha256,principal_fingerprint,mode,legacy_allowed,cedar_allowed,enforced_allowed,evaluation_error) VALUES($1::uuid,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(decision_id) DO NOTHING")
            .bind(&event.decision_id).bind(&event.capability).bind(&event.policy_sha256).bind(&event.matrix_sha256).bind(&event.principal_fingerprint)
            .bind(match event.mode{ai_platform_auth::cedar::Mode::Off=>"off",ai_platform_auth::cedar::Mode::Shadow=>"shadow",ai_platform_auth::cedar::Mode::Enforce=>"enforce"})
            .bind(event.legacy_allowed).bind(event.cedar_allowed).bind(event.enforced_allowed).bind(event.evaluation_error).execute(pool)).await
            .map_err(|_|HostError::Database)?.map_err(|_|HostError::Database)?;
        Ok(())
    }
    async fn run(
        pool: sqlx::PgPool,
        state: Arc<AuditState>,
        mut receiver: mpsc::Receiver<DecisionLog>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), HostError> {
        loop {
            let event = if *stop.borrow() {
                receiver.close();
                receiver.recv().await
            } else {
                tokio::select! {_ = stop.changed()=>{receiver.close();receiver.recv().await},event=receiver.recv()=>event}
            };
            let Some(event) = event else {
                return Ok(());
            };
            let mut delivered = false;
            for attempt in 0..3 {
                if Self::write(&pool, &event).await.is_ok() {
                    state.confirmed.fetch_add(1, Ordering::Relaxed);
                    state.healthy.store(true, Ordering::Release);
                    delivered = true;
                    break;
                }
                state.healthy.store(false, Ordering::Release);
                state.failed.fetch_add(1, Ordering::Relaxed);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            if !delivered {
                return Err(HostError::Shutdown(
                    "authorization_decision_log_unconfirmed",
                ));
            }
        }
    }
    pub async fn stop(&self) -> Result<(), HostError> {
        self.state.accepting.store(false, Ordering::Release);
        self.state.healthy.store(false, Ordering::Release);
        let mut running = self.running.lock().await;
        let result = if let Some(handle) = running.as_mut() {
            let _ = handle.stop.send(true);
            (&mut handle.task)
                .await
                .map_err(|_| HostError::Shutdown("authorization_decision_log"))
                .and_then(|result| result)
        } else if self.state.terminal_failure.load(Ordering::Acquire) {
            Err(HostError::Shutdown(
                "authorization_decision_log_unconfirmed",
            ))
        } else {
            Ok(())
        };
        if result.is_err() {
            self.state.terminal_failure.store(true, Ordering::Release);
        }
        running.take();
        result
    }
}
impl DecisionAudit for DecisionLogWriter {
    fn ready(&self) -> bool {
        self.state.healthy.load(Ordering::Acquire) && self.state.accepting.load(Ordering::Acquire)
    }
    fn record(&self, event: DecisionLog) -> Result<(), PolicyError> {
        if !self.state.accepting.load(Ordering::Acquire) {
            return Err(PolicyError::AuditUnavailable);
        }
        self.sender.try_send(event).map_err(|_| {
            self.state.failed.fetch_add(1, Ordering::Relaxed);
            PolicyError::AuditUnavailable
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> DecisionLog {
        DecisionLog {
            decision_id: uuid::Uuid::new_v4().to_string(),
            capability: "AgentInvoke".into(),
            policy_sha256: "a".repeat(64),
            matrix_sha256: "b".repeat(64),
            principal_fingerprint: "c".repeat(64),
            mode: ai_platform_auth::cedar::Mode::Shadow,
            legacy_allowed: true,
            cedar_allowed: true,
            enforced_allowed: true,
            evaluation_error: false,
            resource: None,
        }
    }
    #[tokio::test]
    async fn construction_is_inert_queue_is_bounded_and_shutdown_rejects_new_decisions() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://localhost/unopened").unwrap();
        let writer = DecisionLogWriter::new(pool);
        assert!(!writer.healthy());
        for _ in 0..2048 {
            writer.record(event()).unwrap();
        }
        assert_eq!(writer.record(event()), Err(PolicyError::AuditUnavailable));
        assert_eq!(writer.failed(), 1);
        writer.stop().await.unwrap();
        assert_eq!(writer.record(event()), Err(PolicyError::AuditUnavailable));
        assert!(!writer.healthy());
        writer.stop().await.unwrap();
    }
}
