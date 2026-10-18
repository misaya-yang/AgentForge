//! Verified Batch cost, raw usage, stage mutations/checkpoints and hold retirement are atomic.
use crate::{
    price_book::PriceReceipt,
    provider_batch::BatchError,
    task_delivery::DeliveryIdentity,
    usage_recorder::{AccountingStage, UsageRecord, stage_receipts},
};
use ai_platform_service_primitives::data_connection::append_store;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;
pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    id: &DeliveryIdentity,
    claim: Uuid,
    pricing: &PriceReceipt,
    snapshot: &Value,
    receipt: &Value,
    completed: u64,
    nanos: u64,
) -> Result<(), BatchError> {
    let lock = serde_json::to_string(&(&id.tenant_id, &id.user_id))
        .map_err(|_| BatchError::Unavailable)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71107))")
        .bind(lock)
        .execute(&mut **tx)
        .await
        .map_err(|_| BatchError::Unavailable)?;
    let hold=sqlx::query("SELECT request_count,reserved_cost_cents,state,created_at FROM gateway.task_batch_quota_holds WHERE task_id=$1 AND claim_id=$2 AND tenant_id=$3 AND user_id=$4 FOR UPDATE").bind(&id.task_id).bind(claim).bind(&id.tenant_id).bind(&id.user_id).fetch_one(&mut **tx).await.map_err(|_|BatchError::Unavailable)?;
    let state: String = hold.try_get("state").map_err(|_| BatchError::Unavailable)?;
    if !matches!(state.as_str(), "held" | "settled") {
        return Err(BatchError::BindingMismatch);
    }
    let maximum: i64 = hold
        .try_get("request_count")
        .map_err(|_| BatchError::Unavailable)?;
    let reserved: i64 = hold
        .try_get("reserved_cost_cents")
        .map_err(|_| BatchError::Unavailable)?;
    let total = receipt["request_counts"]["total"]
        .as_u64()
        .ok_or(BatchError::BindingMismatch)?;
    if total > maximum as u64
        || completed > total
        || u128::from(nanos) > reserved as u128 * 10_000_000
    {
        return Err(BatchError::BindingMismatch);
    }
    let created: chrono::DateTime<chrono::Utc> = hold
        .try_get("created_at")
        .map_err(|_| BatchError::Unavailable)?;
    let mut record = UsageRecord::new(
        &id.tenant_id,
        &id.user_id,
        &pricing.entry.key.model,
        created,
    );
    record.request_id = id.request_id.clone();
    record.service_id = id.service_id.clone();
    record.provider = pricing.entry.key.provider.clone();
    record.request_type = "provider_batch".into();
    record.status = if receipt["status"] == "completed" {
        "success"
    } else {
        "error"
    }
    .into();
    record.input_cost_cents = crate::request_meter::quote_microcents(pricing, completed)
        .map_err(|_| BatchError::AccountingRequired)?;
    crate::request_meter::set_requests(&mut record, completed);
    record
        .metadata
        .insert("pricing_snapshot".into(), snapshot.clone());
    record
        .metadata
        .insert("request_cost_nanos".into(), json!(nanos));
    record
        .metadata
        .insert("_accounting_request_delta".into(), json!(total));
    record
        .metadata
        .insert("_accounting_success_delta".into(), json!(completed));
    record
        .metadata
        .insert("_accounting_error_delta".into(), json!(total - completed));
    record.metadata.insert("batch_claim".into(), json!(claim));
    record
        .metadata
        .insert("batch_job".into(), receipt["id"].clone());
    let metadata = Value::Object(record.metadata.clone());
    append_store::bind_tenant(&mut **tx, "gateway.usage_records", &id.tenant_id)
        .await
        .map_err(|_| BatchError::Unavailable)?;
    append_store::execute(&mut **tx, "gateway.usage_records", sqlx::query("INSERT INTO gateway.usage_records(tenant_id,user_id,request_id,service_id,model,provider,input_tokens,output_tokens,input_cost_cents,output_cost_cents,status,request_type,metadata,created_at) VALUES($1,$2,$3,$4,$5,$6,NULL,NULL,$7,0,$8,'provider_batch',$9,$10) ON CONFLICT DO NOTHING").bind(&id.tenant_id).bind(&id.user_id).bind(&id.request_id).bind(&id.service_id).bind(&record.model).bind(&record.provider).bind(record.input_cost_cents).bind(&record.status).bind(&metadata).bind(created)).await.map_err(|_|BatchError::Unavailable)?;
    let existing:Value=sqlx::query_scalar("SELECT metadata FROM gateway.usage_records WHERE tenant_id=$1 AND request_id=$2 AND service_id=$3 AND request_type='provider_batch' FOR UPDATE").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).fetch_one(&mut **tx).await.map_err(|_|BatchError::Unavailable)?;
    if existing != metadata {
        return Err(BatchError::BindingMismatch);
    }
    for stage in [
        AccountingStage::QuotaCounters,
        AccountingStage::DailyAggregates,
        AccountingStage::HourlyAggregates,
    ] {
        stage_receipts::apply_in_transaction(tx, stage, std::slice::from_ref(&record))
            .await
            .map_err(|_| BatchError::Unavailable)?;
        let observation = serde_json::to_value(&record).map_err(|_| BatchError::Unavailable)?;
        append_store::execute(&mut **tx, "gateway.price_book_v2_accounting_checkpoints", sqlx::query("INSERT INTO gateway.price_book_v2_accounting_checkpoints(tenant_id,request_id,service_id,request_type,stage,observation) VALUES($1,$2,$3,'provider_batch',$4,$5) ON CONFLICT DO NOTHING").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).bind(stage.name()).bind(&observation)).await.map_err(|_|BatchError::Unavailable)?;
        let saved:Value=sqlx::query_scalar("SELECT observation FROM gateway.price_book_v2_accounting_checkpoints WHERE tenant_id=$1 AND request_id=$2 AND service_id=$3 AND request_type='provider_batch' AND stage=$4 FOR UPDATE").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).bind(stage.name()).fetch_one(&mut **tx).await.map_err(|_|BatchError::Unavailable)?;
        if saved != observation {
            return Err(BatchError::BindingMismatch);
        }
    }
    sqlx::query("UPDATE gateway.task_batch_quota_holds SET state='settled',settled_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2 AND state='held'").bind(&id.task_id).bind(claim).execute(&mut **tx).await.map_err(|_|BatchError::Unavailable)?;
    sqlx::query("UPDATE gateway.dispatch_token_reservations SET state='settled',settlement_observation=$6,updated_at=clock_timestamp() WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 AND service_id=$4 AND request_type='provider_batch' AND logical_id=$5 AND state IN ('dispatched','outcome_unknown','awaiting_checkpoint')").bind(&id.tenant_id).bind(&id.user_id).bind(&id.request_id).bind(&id.service_id).bind(claim).bind(serde_json::to_value(&record).map_err(|_|BatchError::Unavailable)?).execute(&mut **tx).await.map_err(|_|BatchError::Unavailable)?;
    Ok(())
}
