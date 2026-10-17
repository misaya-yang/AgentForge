//! Canonical accounting collaborator wrapper. Intents are saved before invoking
//! the existing quota stage, but only its committed receipt authorizes release.
//! This also survives a crash between accounting commit and callback completion.
use super::reservations::ReservationStore;
use crate::usage_recorder::{
    AccountingStage, PersistedUsage, StageStatus, UsageFuture, UsagePersistence, UsageRecord,
    UsageRecorderError, stage_receipts,
};
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub struct CheckpointSettlingPersistence {
    inner: Arc<dyn UsagePersistence>,
    reservations: ReservationStore,
    pool: PgPool,
    failures: AtomicU64,
}
impl CheckpointSettlingPersistence {
    pub fn new(inner: Arc<dyn UsagePersistence>, pool: PgPool) -> Self {
        Self {
            inner,
            reservations: ReservationStore::new(pool.clone()),
            pool,
            failures: AtomicU64::new(0),
        }
    }
    pub fn settlement_failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
    fn failed(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        eprintln!("dispatch_budget_settlement_deferred");
    }
    async fn checkpoint_intents(&self, records: &[UsageRecord]) -> Result<(), ()> {
        tokio::time::timeout(Duration::from_secs(3),async {
            // Bounded flush input is already owned by the canonical recorder.
            // Save each intended exact stage receipt, not an invented digest or
            // a second token/cost accounting algorithm.
            let mut tx=self.pool.begin().await.map_err(|_|())?;
            sqlx::query("SET LOCAL statement_timeout='2000ms'").execute(&mut *tx).await.map_err(|_|())?;
            for record in records.iter().filter(|record| terminal(record)) {
                let receipt=stage_receipts::receipt_id(AccountingStage::QuotaCounters,record).map_err(|_|())?;
                let observation=serde_json::to_value(record).map_err(|_|())?;
                sqlx::query("UPDATE gateway.dispatch_token_reservations SET candidate_receipt_id=$5,settlement_candidate=$6,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 AND service_id=$4 AND request_type=$7 AND state='awaiting_checkpoint'")
                    .bind(&record.tenant_id).bind(&record.user_id).bind(&record.request_id).bind(&record.service_id)
                    .bind(receipt).bind(observation).bind(&record.request_type).execute(&mut *tx).await.map_err(|_|())?;
            }
            tx.commit().await.map_err(|_|())
        }).await.map_err(|_|())?
    }
}
fn terminal(record: &UsageRecord) -> bool {
    !record.request_id.is_empty()
        && matches!(record.request_type.as_str(), "invoke" | "stream")
        && matches!(record.status.as_str(), "success" | "error")
}
impl UsagePersistence for CheckpointSettlingPersistence {
    fn upsert_raw<'a>(
        &'a self,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, Vec<PersistedUsage>> {
        self.inner.upsert_raw(records)
    }
    fn post_write<'a>(
        &'a self,
        stage: AccountingStage,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, StageStatus> {
        Box::pin(async move {
            if stage == AccountingStage::QuotaCounters
                && records.iter().any(terminal)
                && self.checkpoint_intents(records).await.is_err()
            {
                self.failed();
            }
            // No budget mutation may suppress or replace canonical accounting.
            let result = self.inner.post_write(stage, records).await?;
            if stage == AccountingStage::QuotaCounters && result == StageStatus::Applied {
                let users: std::collections::BTreeSet<_> = records
                    .iter()
                    .filter(|record| terminal(record))
                    .map(|record| (record.tenant_id.clone(), record.user_id.clone()))
                    .collect();
                for (tenant, user) in users {
                    if self
                        .reservations
                        .reconcile_user(&tenant, &user)
                        .await
                        .is_err()
                    {
                        self.failed();
                    }
                }
            }
            Ok(result)
        })
    }
    fn reconcile<'a>(
        &'a self,
        stage: AccountingStage,
        records: &'a [UsageRecord],
        error: &'a UsageRecorderError,
    ) -> UsageFuture<'a, ()> {
        self.inner.reconcile(stage, records, error)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_or_non_dispatch_observations_are_never_terminal_proof() {
        let mut record = UsageRecord::new("t", "u", "m", chrono::Utc::now());
        record.request_id = "r".into();
        record.request_type = "invoke".into();
        for status in ["unknown", "running", "pending", ""] {
            record.status = status.into();
            assert!(!terminal(&record));
        }
        record.status = "success".into();
        assert!(terminal(&record));
        record.request_type = "embedding".into();
        assert!(!terminal(&record));
    }
}
