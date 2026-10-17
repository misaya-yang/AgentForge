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
}
