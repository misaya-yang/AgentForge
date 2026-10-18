//! Persistent bounded request/cost holds; shared user lock with normal dispatch.
use crate::{
    price_book::PriceReceipt,
    provider_batch::{BatchError, BatchPlan},
    task_delivery::DeliveryIdentity,
};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;
/// Only absence on an old schema selects zero holds. Permissions and query failures propagate.
pub async fn pending(
    connection: &mut PgConnection,
    tenant: &str,
    user: &str,
) -> Result<(i64, i64), sqlx::Error> {
    let installed: bool = sqlx::query_scalar(
        "SELECT pg_catalog.to_regclass('gateway.task_batch_quota_holds') IS NOT NULL",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !installed {
        return Ok((0, 0));
    }
    sqlx::query_as("SELECT COALESCE(sum(reserved_cost_cents),0)::bigint,COALESCE(sum(request_count),0)::bigint FROM gateway.task_batch_quota_holds WHERE tenant_id=$1 AND user_id=$2 AND state='held'").bind(tenant).bind(user).fetch_one(connection).await
}
pub async fn reserve(
    pool: &PgPool,
    id: &DeliveryIdentity,
    claim: Uuid,
    plan: &BatchPlan,
    pricing: &PriceReceipt,
) -> Result<(), BatchError> {
    let mut tx = pool.begin().await.map_err(|_| BatchError::Unavailable)?;
    let lock = serde_json::to_string(&(&id.tenant_id, &id.user_id))
        .map_err(|_| BatchError::Unavailable)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71107))")
        .bind(lock)
        .execute(&mut *tx)
        .await
        .map_err(|_| BatchError::Unavailable)?;
    let previous: Option<Uuid> = sqlx::query_scalar(
        "SELECT claim_id FROM gateway.task_batch_quota_holds WHERE task_id=$1 FOR UPDATE",
    )
    .bind(&id.task_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| BatchError::Unavailable)?;
    if let Some(previous) = previous {
        if previous != claim {
            return Err(BatchError::BindingMismatch);
        }
        return tx.commit().await.map_err(|_| BatchError::Unavailable);
    }
    let count:i64=sqlx::query_scalar("SELECT i.request_count FROM gateway.task_batch_inputs i JOIN gateway.task_provider_batches b ON b.tenant_id=i.tenant_id AND b.user_id=i.user_id AND b.service_id=i.service_id AND b.provider_id=i.provider_id AND b.model_id=i.model_id AND b.input_file_id=i.input_file_id AND b.input_sha256=i.input_sha256 WHERE b.task_id=$1 AND b.claim_id=$2 AND b.tenant_id=$3 AND b.user_id=$4 AND b.service_id=$5 AND i.expires_at>clock_timestamp()").bind(&id.task_id).bind(claim).bind(&id.tenant_id).bind(&id.user_id).bind(&id.service_id).fetch_one(&mut *tx).await.map_err(|_|BatchError::Unavailable)?;
    if !(1..=50_000).contains(&count)
        || pricing.entry.key.provider != plan.provider_id
        || pricing.entry.key.model != plan.model_id
    {
        return Err(BatchError::BindingMismatch);
    }
    let nanos = pricing
        .quote_nanos(count as u64)
        .map_err(|_| BatchError::AccountingRequired)?;
    let ceiling = i64::try_from((u128::from(nanos) + 9_999_999) / 10_000_000)
        .map_err(|_| BatchError::AccountingRequired)?;
    let row=sqlx::query("SELECT is_blocked,current_monthly_cost_cents,current_daily_requests,monthly_cost_limit_cents,requests_per_day,temporary_extra_cost_cents,temporary_expires_at FROM user_quotas WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE").bind(&id.tenant_id).bind(&id.user_id).fetch_one(&mut *tx).await.map_err(|_|BatchError::Unavailable)?;
    let blocked: bool = row
        .try_get("is_blocked")
        .map_err(|_| BatchError::Unavailable)?;
    if blocked {
        return Err(BatchError::Unavailable);
    }
    let (pending_cost, pending_requests) = pending(&mut tx, &id.tenant_id, &id.user_id)
        .await
        .map_err(|_| BatchError::Unavailable)?;
    let used_cost: i64 = row
        .try_get("current_monthly_cost_cents")
        .map_err(|_| BatchError::Unavailable)?;
    let used_requests: i64 = row
        .try_get("current_daily_requests")
        .map_err(|_| BatchError::Unavailable)?;
    let cost_limit: Option<i64> = row
        .try_get("monthly_cost_limit_cents")
        .map_err(|_| BatchError::Unavailable)?;
    let request_limit: Option<i64> = row
        .try_get("requests_per_day")
        .map_err(|_| BatchError::Unavailable)?;
    let extra: i64 = row
        .try_get("temporary_extra_cost_cents")
        .map_err(|_| BatchError::Unavailable)?;
    let expiry: Option<chrono::DateTime<chrono::Utc>> = row
        .try_get("temporary_expires_at")
        .map_err(|_| BatchError::Unavailable)?;
    let boosted = if expiry.is_some_and(|at| at > chrono::Utc::now()) {
        extra.max(0)
    } else {
        0
    };
    if exceeds(
        used_cost,
        pending_cost,
        ceiling,
        cost_limit.map(|n| n.saturating_add(boosted)),
    ) || exceeds(used_requests, pending_requests, count, request_limit)
    {
        return Err(BatchError::Unavailable);
    }
    sqlx::query("INSERT INTO gateway.task_batch_quota_holds(task_id,claim_id,tenant_id,user_id,request_count,reserved_cost_cents) VALUES($1,$2,$3,$4,$5,$6)").bind(&id.task_id).bind(claim).bind(&id.tenant_id).bind(&id.user_id).bind(count).bind(ceiling).execute(&mut *tx).await.map_err(|_|BatchError::Unavailable)?;
    tx.commit().await.map_err(|_| BatchError::Unavailable)
}
fn exceeds(used: i64, pending: i64, incoming: i64, limit: Option<i64>) -> bool {
    limit.is_some_and(|limit| {
        limit > 0
            && i128::from(used) + i128::from(pending) + i128::from(incoming) > i128::from(limit)
    })
}

#[derive(sqlx::FromRow)]
struct ReleaseBinding {
    hold_state: String,
    delivery_state: String,
    unconfirmed: bool,
}
impl ReleaseBinding {
    fn is_released(&self) -> bool {
        self.unconfirmed
            && self.hold_state == "released"
            && matches!(
                self.delivery_state.as_str(),
                "claimed_unknown" | "failed" | "cancelled"
            )
    }
    fn can_release(&self) -> bool {
        self.unconfirmed
            && self.hold_state == "held"
            && matches!(
                self.delivery_state.as_str(),
                "claimed_unknown" | "cancelled"
            )
    }
}

async fn release_binding(
    connection: &mut PgConnection,
    id: &DeliveryIdentity,
    claim: Uuid,
    nonce: Uuid,
) -> Result<Option<ReleaseBinding>, BatchError> {
    sqlx::query_as(
        "SELECT h.state AS hold_state,d.state AS delivery_state,
                (b.provider_job_id IS NULL AND b.confirmed_at IS NULL) AS unconfirmed
         FROM gateway.task_batch_quota_holds h
         JOIN gateway.task_provider_batches b ON b.task_id=h.task_id AND b.claim_id=h.claim_id
              AND b.tenant_id=h.tenant_id AND b.user_id=h.user_id
         JOIN gateway.task_deliveries d ON d.task_id=b.task_id AND d.claim_id=b.claim_id
              AND d.request_id=b.request_id AND d.service_id=b.service_id
              AND d.tenant_id=b.tenant_id AND d.user_id=b.user_id
         WHERE b.task_id=$1 AND b.request_id=$2 AND b.service_id=$3
              AND b.user_id=$4 AND b.tenant_id=$5 AND b.claim_id=$6 AND b.submission_nonce=$7
         FOR UPDATE OF d,b,h",
    )
    .bind(&id.task_id)
    .bind(&id.request_id)
    .bind(&id.service_id)
    .bind(&id.user_id)
    .bind(&id.tenant_id)
    .bind(claim)
    .bind(nonce)
    .fetch_optional(connection)
    .await
    .map_err(|_| BatchError::Unavailable)
}

pub(crate) async fn was_released(
    pool: &PgPool,
    id: &DeliveryIdentity,
    claim: Uuid,
    nonce: Uuid,
) -> Result<bool, BatchError> {
    let mut connection = pool.acquire().await.map_err(|_| BatchError::Unavailable)?;
    Ok(release_binding(&mut connection, id, claim, nonce)
        .await?
        .is_some_and(|binding| binding.is_released()))
}

/// Only the lane's pre-provider NotDispatched result authorizes this transition.
/// Neither a missing job id nor a failed/expired lease proves no POST occurred.
pub(crate) async fn release_not_dispatched(
    pool: &PgPool,
    id: &DeliveryIdentity,
    claim: Uuid,
    nonce: Uuid,
) -> Result<(), BatchError> {
    let mut tx = pool.begin().await.map_err(|_| BatchError::Unavailable)?;
    let lock = serde_json::to_string(&(&id.tenant_id, &id.user_id))
        .map_err(|_| BatchError::Unavailable)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71107))")
        .bind(lock)
        .execute(&mut *tx)
        .await
        .map_err(|_| BatchError::Unavailable)?;
    let binding = release_binding(&mut tx, id, claim, nonce)
        .await?
        .ok_or(BatchError::BindingMismatch)?;
    if binding.is_released() {
        return tx.commit().await.map_err(|_| BatchError::Unavailable);
    }
    if !binding.can_release() {
        return Err(BatchError::BindingMismatch);
    }
    let changed = sqlx::query(
        "UPDATE gateway.task_batch_quota_holds SET state='released',released_at=clock_timestamp()
         WHERE task_id=$1 AND claim_id=$2 AND tenant_id=$3 AND user_id=$4 AND state='held'",
    )
    .bind(&id.task_id)
    .bind(claim)
    .bind(&id.tenant_id)
    .bind(&id.user_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| BatchError::Unavailable)?;
    if changed.rows_affected() != 1 {
        return Err(BatchError::BindingMismatch);
    }
    tx.commit().await.map_err(|_| BatchError::Unavailable)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn holds_are_counted_without_overflow_or_boundary_gap() {
        assert!(!exceeds(1, 2, 3, Some(6)));
        assert!(exceeds(1, 2, 4, Some(6)));
        assert!(exceeds(i64::MAX, i64::MAX, 1, Some(i64::MAX)));
        assert!(!exceeds(1, 2, 3, None));
    }
    #[test]
    fn released_hold_is_a_durable_failure_before_and_after_delivery_finish() {
        let mut binding = ReleaseBinding {
            hold_state: "released".into(),
            delivery_state: "claimed_unknown".into(),
            unconfirmed: true,
        };
        assert!(binding.is_released());
        assert!(!binding.can_release());
        binding.delivery_state = "failed".into();
        assert!(binding.is_released());
        binding.hold_state = "settled".into();
        assert!(!binding.is_released());
        assert!(!binding.can_release());
    }
    #[test]
    fn confirmed_jobs_and_finished_deliveries_cannot_release_held_quota() {
        let mut binding = ReleaseBinding {
            hold_state: "held".into(),
            delivery_state: "claimed_unknown".into(),
            unconfirmed: false,
        };
        assert!(!binding.can_release());
        binding.unconfirmed = true;
        for terminal in ["failed", "completed"] {
            binding.delivery_state = terminal.into();
            assert!(!binding.can_release());
        }
    }
    #[test]
    fn cancellation_retaining_the_claim_does_not_strand_a_known_undispatched_hold() {
        let mut binding = ReleaseBinding {
            hold_state: "held".into(),
            delivery_state: "cancelled".into(),
            unconfirmed: true,
        };
        assert!(binding.can_release());
        binding.hold_state = "released".into();
        assert!(binding.is_released());
        assert!(!binding.can_release());
        binding.unconfirmed = false;
        assert!(!binding.is_released());
        binding.hold_state = "held".into();
        assert!(!binding.can_release());
    }
}
