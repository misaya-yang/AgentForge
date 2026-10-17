//! Durable admission intents. These are NOT usage or financial accounting.
//! The per-user lock spans reconciliation, quota snapshot and Redis admission.
//! Only canonical quota-counter checkpoints can retire a dispatched token hold.
use super::{QuotaRow, SELECT_QUOTA};
use crate::billing::{self, MinuteQuotaCheck};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ReservationIdentity {
    pub tenant: String,
    pub user: String,
    pub request: String,
    pub service: String,
    pub request_type: String,
    pub member: String,
    pub logical_id: String,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct TokenEnvelope {
    pub estimate: i64,
    pub daily_used: i64,
    pub monthly_used: i64,
    pub pending: i64,
    pub daily_limit: Option<i64>,
    pub monthly_limit: Option<i64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservationError {
    Unavailable,
    QuotaExceeded,
    Duplicate,
}
impl From<()> for ReservationError {
    fn from(_: ()) -> Self {
        Self::Unavailable
    }
}
#[derive(Clone)]
pub struct ReservationStore {
    pool: PgPool,
}
pub struct PreparedReservation {
    tx: Option<Transaction<'static, Postgres>>,
    pub envelope: TokenEnvelope,
    pub rpm_limit: Option<i64>,
}
impl ReservationStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn prepare(
        &self,
        id: &ReservationIdentity,
        estimate: i64,
        ttl_ms: i64,
    ) -> Result<PreparedReservation, ReservationError> {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.prepare_inner(id, estimate, ttl_ms, false),
        )
        .await
        .map_err(|_| ())?
    }
    pub(crate) async fn prepare_provider_batch(&self,id:&ReservationIdentity,ttl_ms:i64)->Result<PreparedReservation,ReservationError>{tokio::time::timeout(Duration::from_secs(3),self.prepare_inner(id,0,ttl_ms,true)).await.map_err(|_|())?}
    async fn prepare_inner(
        &self,
        id: &ReservationIdentity,
        estimate: i64,
        ttl_ms: i64,
        verified_batch:bool,
    ) -> Result<PreparedReservation, ReservationError> {
        if estimate < 0 || id.tenant.is_empty() || id.user.is_empty() || id.request.is_empty() {
            return Err(ReservationError::Unavailable);
        }
        let mut tx = self.pool.begin().await.map_err(|_| ())?;
        sqlx::query("SET LOCAL statement_timeout='2000ms'")
            .execute(&mut *tx)
            .await
            .map_err(|_| ())?;
        // JSON tuple encoding prevents delimiter collisions; hash collisions only
        // serialize unrelated users and never combine their accounting rows.
        let lock = serde_json::to_string(&(&id.tenant, &id.user)).map_err(|_| ())?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 71107))")
            .bind(lock)
            .execute(&mut *tx)
            .await
            .map_err(|_| ())?;
        reconcile(&mut tx, &id.tenant, &id.user).await?;
        let duplicate:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.dispatch_token_reservations WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 AND service_id=$4 AND request_type=$5 AND logical_id<>$6::text::uuid AND state IN ('prepared','dispatched','awaiting_checkpoint','outcome_unknown'))")
            .bind(&id.tenant).bind(&id.user).bind(&id.request).bind(&id.service).bind(&id.request_type).bind(&id.logical_id)
            .fetch_one(&mut *tx).await.map_err(|_|())?;
        if duplicate {
            tx.commit().await.map_err(|_| ())?;
            return Err(ReservationError::Duplicate);
        }
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT CURRENT_TIMESTAMP")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| ())?;
        let mut loaded = sqlx::query_as::<_, QuotaRow>(SELECT_QUOTA)
            .bind(&id.tenant)
            .bind(&id.user)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ())?
            .map(QuotaRow::into_quota);
        let (mut batch_cost,mut batch_requests)=crate::provider_batch_quota::pending(&mut tx,&id.tenant,&id.user).await.map_err(|_|())?;
        if verified_batch {let own:(i64,i64)=sqlx::query_as("SELECT reserved_cost_cents,request_count FROM gateway.task_batch_quota_holds WHERE tenant_id=$1 AND user_id=$2 AND claim_id=$3::text::uuid AND state='held'").bind(&id.tenant).bind(&id.user).bind(&id.logical_id).fetch_one(&mut *tx).await.map_err(|_|())?;batch_cost=batch_cost.checked_sub(own.0).ok_or(())?;batch_requests=batch_requests.checked_sub(own.1).ok_or(())?;}
        if let Some(loaded)=loaded.as_mut(){loaded.quota.current_monthly_cost_cents=loaded.quota.current_monthly_cost_cents.checked_add(batch_cost).ok_or(())?;loaded.quota.current_daily_requests=loaded.quota.current_daily_requests.checked_add(batch_requests).ok_or(())?;}
        let pending: i64 = sqlx::query_scalar("SELECT COALESCE(sum(estimated_tokens),0)::bigint FROM gateway.dispatch_token_reservations WHERE tenant_id=$1 AND user_id=$2 AND state IN ('prepared','dispatched','awaiting_checkpoint','outcome_unknown')")
            .bind(&id.tenant).bind(&id.user).fetch_one(&mut *tx).await.map_err(|_| ())?;
        let estimate_with_pending = estimate.checked_add(pending).ok_or(())?;
        let check = billing::check_quota(
            loaded.as_ref().map(|row| &row.quota),
            estimate_with_pending,
            now,
            |_| MinuteQuotaCheck::default(),
        )
        .map_err(|_| ())?;
        // Preserve the canonical allow/alert/downgrade decision. Existing model
        // preparation owns routing; admission never invents a downgrade model.
        if !check.can_proceed() {
            // Keep bounded expiry/receipt progress even when the remaining
            // holds still deny this request. Rolling it back would starve a
            // user with more than one reconciliation batch of old intents.
            tx.commit().await.map_err(|_| ())?;
            return Err(ReservationError::QuotaExceeded);
        }
        let mut envelope = TokenEnvelope {
            estimate,
            pending,
            ..TokenEnvelope::default()
        };
        let rpm_limit = loaded
            .as_ref()
            .and_then(|row| billing::normalize_limit(row.quota.requests_per_minute));
        if let Some(loaded) = loaded {
            let q = loaded.quota;
            let limits = billing::effective_quota_limits(
                q.daily_token_limit,
                q.monthly_token_limit,
                q.monthly_cost_limit_cents,
                q.temporary_extra_tokens,
                q.temporary_extra_cost_cents,
                q.temporary_expires_at,
                now,
            )
            .map_err(|_| ())?;
            envelope.daily_used = q.current_daily_tokens;
            envelope.monthly_used = q.current_monthly_tokens;
            if matches!(
                q.overage_strategy,
                billing::OverageStrategy::HardBlock | billing::OverageStrategy::RateLimit
            ) {
                envelope.daily_limit = limits.daily_tokens.filter(|limit| *limit != 0);
                envelope.monthly_limit = limits.monthly_tokens.filter(|limit| *limit != 0);
            }
        }
        sqlx::query("INSERT INTO gateway.dispatch_token_reservations(tenant_id,user_id,request_id,service_id,request_type,member,estimated_tokens,state,prepare_deadline,logical_id) VALUES($1,$2,$3,$4,$5,$6::text::uuid,$7,'prepared',CURRENT_TIMESTAMP + ($8::bigint * interval '1 millisecond'),$9::text::uuid)")
            .bind(&id.tenant).bind(&id.user).bind(&id.request).bind(&id.service).bind(&id.request_type)
            .bind(&id.member).bind(estimate).bind(ttl_ms).bind(&id.logical_id).execute(&mut *tx).await.map_err(|_| ())?;
        Ok(PreparedReservation {
            tx: Some(tx),
            envelope,
            rpm_limit,
        })
    }
    /// Called after the canonical quota stage commits, and lazily on admission
    /// for crash recovery. Serialize retirement with quota snapshots.
    pub async fn reconcile_user(&self, tenant: &str, user: &str) -> Result<(), ()> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut tx = self.pool.begin().await.map_err(|_| ())?;
            sqlx::query("SET LOCAL statement_timeout='2000ms'")
                .execute(&mut *tx)
                .await
                .map_err(|_| ())?;
            let lock = serde_json::to_string(&(tenant, user)).map_err(|_| ())?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71107))")
                .bind(lock)
                .execute(&mut *tx)
                .await
                .map_err(|_| ())?;
            reconcile(&mut tx, tenant, user).await?;
            tx.commit().await.map_err(|_| ())
        })
        .await
        .map_err(|_| ())?
    }
    /// Commit before polling provider I/O. An uncertain commit never authorizes
    /// dispatch; the intent remains conservative until independently reconciled.
    pub async fn mark_dispatched(&self, id: &ReservationIdentity) -> Result<(), ()> {
        let result = tokio::time::timeout(Duration::from_secs(3), sqlx::query("UPDATE gateway.dispatch_token_reservations SET state='dispatched',updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2 AND member=$3::text::uuid AND state='prepared' AND prepare_deadline>CURRENT_TIMESTAMP")
            .bind(&id.tenant).bind(&id.user).bind(&id.member).execute(&self.pool)).await.map_err(|_| ())?.map_err(|_| ())?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(())
        }
    }
    pub async fn mark_outcome(
        &self,
        id: &ReservationIdentity,
        outcome: crate::dispatcher::DispatchSettlement,
    ) -> Result<(), ()> {
        let state = match outcome {
            crate::dispatcher::DispatchSettlement::Completed => "awaiting_checkpoint",
            crate::dispatcher::DispatchSettlement::NotDispatched => "cancelled",
            crate::dispatcher::DispatchSettlement::Unknown => "outcome_unknown",
        };
        tokio::time::timeout(Duration::from_secs(3),sqlx::query("UPDATE gateway.dispatch_token_reservations SET state=$4,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2 AND member=$3::text::uuid AND state='dispatched'")
            .bind(&id.tenant).bind(&id.user).bind(&id.member).bind(state).execute(&self.pool)).await.map_err(|_|())?.map_err(|_|())?;
        Ok(())
    }
    /// Capacity release is independent of token release. Unknown/cancelled work
    /// retains its hold; only a committed canonical checkpoint can refund it.
    pub async fn finish(&self, id: &ReservationIdentity) -> Result<(), ()> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut tx = self.pool.begin().await.map_err(|_| ())?;
            sqlx::query("UPDATE gateway.dispatch_token_reservations SET state=CASE WHEN state='prepared' THEN 'cancelled' ELSE 'outcome_unknown' END,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2 AND member=$3::text::uuid AND state IN ('prepared','dispatched')")
                .bind(&id.tenant).bind(&id.user).bind(&id.member).execute(&mut *tx).await.map_err(|_| ())?;
            // Do not reconcile here: another admission may hold an older quota
            // snapshot. Retirement only happens under that admission's user lock.
            tx.commit().await.map_err(|_| ())
        }).await.map_err(|_| ())?
    }
}
impl PreparedReservation {
    pub async fn commit(mut self) -> Result<(), ()> {
        tokio::time::timeout(Duration::from_secs(3), self.tx.take().ok_or(())?.commit())
            .await
            .map_err(|_| ())?
            .map_err(|_| ())
    }
}
async fn reconcile(tx: &mut Transaction<'_, Postgres>, tenant: &str, user: &str) -> Result<(), ()> {
    // The checkpoint is written in the SAME transaction as quota counters.
    // Unknown observations are deliberately excluded, even after a long crash.
    // NotApplicable remains typed in the receipt, never converted to tokens=0.
    sqlx::query("WITH candidates AS (SELECT r.member,c.observation FROM gateway.dispatch_token_reservations r JOIN gateway.price_book_v2_accounting_checkpoints c ON c.tenant_id=r.tenant_id AND c.request_id=r.request_id AND c.service_id=r.service_id AND c.request_type=r.request_type AND c.stage='quota_counters' WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.state='awaiting_checkpoint' AND c.updated_at>=r.created_at AND c.observation->>'user_id'=r.user_id AND c.observation->>'status' IN ('success','error') ORDER BY r.created_at LIMIT 128) UPDATE gateway.dispatch_token_reservations r SET state='settled',settlement_observation=c.observation,updated_at=CURRENT_TIMESTAMP FROM candidates c WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.member=c.member")
        .bind(tenant).bind(user).execute(&mut **tx).await.map_err(|_| ())?;
    // Legacy canonical stage receipts carry an observation digest rather than
    // the complete v2 checkpoint. The exact receipt ID and trusted observation
    // intent were saved BEFORE calling that existing stage. Neither raw usage
    // alone nor an in-memory callback success can authorize retirement.
    sqlx::query("WITH candidates AS (SELECT r.member,r.settlement_candidate FROM gateway.dispatch_token_reservations r JOIN gateway.billing_events b ON b.event_id=r.candidate_receipt_id AND b.tenant_id=r.tenant_id AND b.user_id=r.user_id AND b.request_id=r.request_id AND b.service_id=r.service_id WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.state='awaiting_checkpoint' AND b.status='accounting_applied' AND b.metadata->>'event'='usage_accounting_stage_receipt_v1' AND b.metadata->>'stage'='quota_counters' AND b.created_at>=r.created_at AND r.settlement_candidate->>'request_type'=r.request_type AND r.settlement_candidate->>'status' IN ('success','error') ORDER BY r.created_at LIMIT 128) UPDATE gateway.dispatch_token_reservations r SET state='settled',settlement_observation=c.settlement_candidate,updated_at=CURRENT_TIMESTAMP FROM candidates c WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.member=c.member")
        .bind(tenant).bind(user).execute(&mut **tx).await.map_err(|_| ())?;
    sqlx::query("WITH candidates AS (SELECT member FROM gateway.dispatch_token_reservations WHERE tenant_id=$1 AND user_id=$2 AND state='prepared' AND prepare_deadline<=CURRENT_TIMESTAMP ORDER BY prepare_deadline LIMIT 128) UPDATE gateway.dispatch_token_reservations r SET state='expired',updated_at=CURRENT_TIMESTAMP FROM candidates c WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.member=c.member AND r.state='prepared'")
        .bind(tenant).bind(user).execute(&mut **tx).await.map_err(|_| ())?;
    Ok(())
}

#[cfg(test)]
#[path = "reservations_tests.rs"]
mod tests;
