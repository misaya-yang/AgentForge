//! Narrow Knowledge settlement adapter over the existing cumulative accounting.
//! Caller owns the transaction containing raw usage, reservation release and final
//! result. A stage failure rolls all of those back, leaving durable observation.
use crate::usage_recorder::{AccountingStage, UsageRecord, UsageRecorderError, stage_receipts};
use serde_json::{Map, json};
use sqlx::{Postgres, Transaction};
pub async fn settle_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    record: &UsageRecord,
) -> Result<(), UsageRecorderError> {
    // The canonical recorder uses this cumulative observation to calculate an exact
    // delta and to suppress replay independently for all three accounting stages.
    let mut observation = record.clone();
    let snapshot = record
        .metadata
        .get("pricing_snapshot")
        .cloned()
        .ok_or(UsageRecorderError::InvalidAccountingDelta)?;
    observation.metadata = Map::from_iter([("pricing_snapshot".into(), snapshot)]);
    if let Some(cache) = record.metadata.get("provider_cache_usage") {
        observation
            .metadata
            .insert("provider_cache_usage".into(), cache.clone());
    }
    if let Some(units) = record.metadata.get(crate::request_meter::METADATA_KEY) {
        observation
            .metadata
            .insert(crate::request_meter::METADATA_KEY.into(), units.clone());
    }
    if let Some(cost)=record.metadata.get("request_cost_nanos"){observation.metadata.insert("request_cost_nanos".into(),cost.clone());}
    observation.trace_steps.clear();
    let mut accounting = record.clone();
    accounting
        .metadata
        .insert("_price_book_v2_cumulative".into(), json!(observation));
    for stage in [
        AccountingStage::QuotaCounters,
        AccountingStage::DailyAggregates,
        AccountingStage::HourlyAggregates,
    ] {
        stage_receipts::apply_in_transaction(transaction, stage, &[accounting.clone()]).await?;
    }
    Ok(())
}
