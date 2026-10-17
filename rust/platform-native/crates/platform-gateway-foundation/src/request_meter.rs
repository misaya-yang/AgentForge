//! Explicit request pricing and observations. Token counters are not applicable,
//! rather than measured zero, for a registered request-priced model.
use crate::{
    price_book::{self, Component, PriceKey, PriceReceipt, Unit},
    usage_recorder::{UsageRecord, UsageRecorderError},
};
pub use ai_platform_service_primitives::knowledge_model::{MeterKind, TokenCountState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "meter", rename_all = "snake_case", deny_unknown_fields)]
pub enum MeasuredUnits {
    Tokens {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    Requests {
        requests: u64,
        tokens: TokenCountState,
    },
}
pub const METADATA_KEY: &str = "measured_units";
pub fn measured(record: &UsageRecord) -> Result<Option<MeasuredUnits>, UsageRecorderError> {
    record
        .metadata
        .get(METADATA_KEY)
        .map(|v| {
            serde_json::from_value(v.clone())
                .map_err(|_| UsageRecorderError::InvalidAccountingDelta)
        })
        .transpose()
}
pub fn request_count(record: &UsageRecord) -> Result<Option<u64>, UsageRecorderError> {
    match measured(record)? {
        Some(MeasuredUnits::Requests {
            requests,
            tokens: TokenCountState::NotApplicable,
        }) => Ok(Some(requests)),
        Some(MeasuredUnits::Requests { .. }) => Err(UsageRecorderError::InvalidAccountingDelta),
        _ => Ok(None),
    }
}
pub fn set_requests(record: &mut UsageRecord, requests: u64) {
    record.metadata.insert(
        METADATA_KEY.into(),
        json!(MeasuredUnits::Requests {
            requests,
            tokens: TokenCountState::NotApplicable
        }),
    );
}
pub fn snapshot(receipt: &PriceReceipt) -> Result<Value, price_book::PriceBookError> {
    receipt.validate(&receipt.tenant, &receipt.entry.key)?;
    if receipt.entry.key.component != Component::Request
        || receipt.entry.unit != Unit::Requests
        || receipt.entry.key.currency != "USD"
    {
        return Err(price_book::PriceBookError::UnsupportedLegacyBridge);
    }
    let mut value = json!({"price_book_schema":price_book::SCHEMA,"pricing_meter":"requests","tenant_id":receipt.tenant,"provider_id":receipt.entry.key.provider,"model_id":receipt.entry.key.model,"currency":"USD","unit":"per_request","price_book_request":receipt});
    value["request_snapshot_sha256"] = json!(digest(&value)?);
    Ok(value)
}
fn digest(value: &Value) -> Result<String, price_book::PriceBookError> {
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or(price_book::PriceBookError::InvalidReference)?
        .remove("request_snapshot_sha256");
    let mut hash = Sha256::new();
    hash.update(b"agentforge/request-pricing-snapshot/v1\0");
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), sorted(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    hash.update(
        serde_json::to_vec(&sorted(&value)).map_err(|_| price_book::PriceBookError::Decode)?,
    );
    Ok(format!("{:x}", hash.finalize()))
}
pub fn is_snapshot(snapshot: &Value) -> bool {
    snapshot["pricing_meter"] == "requests"
}
pub fn validate_snapshot(
    snapshot: &Value,
    tenant: &str,
    provider: &str,
    model: &str,
) -> Result<PriceReceipt, price_book::PriceBookError> {
    if snapshot["pricing_meter"] != "requests"
        || snapshot["price_book_schema"] != price_book::SCHEMA
        || snapshot["unit"] != "per_request"
        || snapshot["currency"] != "USD"
        || snapshot["tenant_id"] != tenant
        || snapshot["provider_id"] != provider
        || snapshot["model_id"] != model
        || snapshot["request_snapshot_sha256"] != digest(snapshot)?
    {
        return Err(price_book::PriceBookError::IdentityMismatch);
    }
    let receipt: PriceReceipt = serde_json::from_value(snapshot["price_book_request"].clone())
        .map_err(|_| price_book::PriceBookError::Decode)?;
    let key = PriceKey {
        book_version: receipt.entry.key.book_version.clone(),
        provider: provider.into(),
        model: model.into(),
        model_version: receipt.entry.key.model_version.clone(),
        component: Component::Request,
        currency: "USD".into(),
    };
    receipt.validate(tenant, &key)?;
    if receipt.entry.unit != Unit::Requests {
        return Err(price_book::PriceBookError::UnsupportedLegacyBridge);
    }
    Ok(receipt)
}
pub fn prepare(record: &mut UsageRecord, snapshot: &Value) -> Result<(), UsageRecorderError> {
    let receipt = validate_snapshot(snapshot, &record.tenant_id, &record.provider, &record.model)?;
    let requests = request_count(record)?.ok_or(UsageRecorderError::InvalidAccountingDelta)?;
    if requests > 1 {
        return Err(UsageRecorderError::InvalidAccountingDelta);
    }
    // These legacy scalar fields are compatibility projections only. Request raw
    // rows bind NULL token columns, and all receipts expose only request units.
    if record.input_tokens != 0 || record.output_tokens != 0 {
        return Err(UsageRecorderError::InvalidAccountingDelta);
    }
    let cost_nanos = receipt.quote_nanos(requests)?;
    let projected_cost = quote_microcents(&receipt, requests)?;
    record
        .metadata
        .insert("request_cost_nanos".into(), json!(cost_nanos));
    record.input_cost_cents = projected_cost;
    record.output_cost_cents = 0;
    record
        .metadata
        .insert("pricing_snapshot".into(), snapshot.clone());
    Ok(())
}
/// Legacy ledger units = 1e6/USD; price-book nano-USD = 1e9/USD.
/// Preserve the recorder's half-even rounding, using exact integer arithmetic.
pub fn quote_microcents(receipt: &PriceReceipt, requests: u64) -> Result<i64, UsageRecorderError> {
    let numerator = u128::from(receipt.adjusted_rate_nanos()?)
        .checked_mul(u128::from(requests))
        .ok_or(UsageRecorderError::IntegerOverflow)?;
    let denominator = u128::from(receipt.entry.units_per_rate)
        .checked_mul(1000)
        .ok_or(UsageRecorderError::IntegerOverflow)?;
    if denominator == 0 {
        return Err(UsageRecorderError::InvalidAccountingDelta);
    }
    let whole = numerator / denominator;
    let remainder = numerator % denominator;
    i64::try_from(
        whole
            + u128::from(
                remainder * 2 > denominator || (remainder * 2 == denominator && whole % 2 == 1),
            ),
    )
    .map_err(|_| UsageRecorderError::IntegerOverflow)
}
/// A complete successful HTTP response is an exact invocation observation. An
/// interrupted transport or rejected response is never silently counted as free.
pub fn observed_requests(status: Option<u16>, complete_response: bool) -> Option<MeasuredUnits> {
    match (status, complete_response) {
        (Some(200..=299), true) => Some(MeasuredUnits::Requests {
            requests: 1,
            tokens: TokenCountState::NotApplicable,
        }),
        _ => None,
    }
}
/// Additive compatibility metadata: old aggregate token totals remain the sum of
/// applicable token facts. Request quantities/cost stay in the same totals; they
/// cannot be mistaken for individual observations of zero tokens.
pub async fn annotate_summary(
    pool: &sqlx::PgPool,
    filter: &crate::billing_store::UsageFilter,
    summary: &mut Value,
) -> Result<(), sqlx::Error> {
    let (records,requests,cost_nanos):(i64,i64,i64)=sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(CASE WHEN metadata->'measured_units'->>'requests' ~ '^[0-9]+$' THEN (metadata->'measured_units'->>'requests')::bigint ELSE 0 END),0)::bigint,COALESCE(SUM(CASE WHEN metadata->>'request_cost_nanos' ~ '^[0-9]+$' THEN (metadata->>'request_cost_nanos')::bigint ELSE 0 END),0)::bigint FROM gateway.usage_records WHERE tenant_id=$1 AND created_at::date>=$2 AND created_at::date<=$3 AND metadata->'measured_units'->>'meter'='requests' AND ($4::text IS NULL OR user_id=$4) AND ($5::text IS NULL OR model=$5) AND ($6::text IS NULL OR service_id=$6) AND ($7::text IS NULL OR assistant_id=$7) AND ($8::text IS NULL OR provider=$8)")
  .bind(&filter.tenant_id).bind(filter.start_date).bind(filter.end_date).bind(&filter.user_id).bind(&filter.model).bind(&filter.service_id).bind(&filter.assistant_id).bind(&filter.provider).fetch_one(pool).await?;
    if records > 0 {
        summary["metering"] = json!({"schema":"agentforge.usage-metering/v1","token_totals_scope":"reported_token_usage_only","request_priced_invocations":requests,"tokens_not_applicable_records":records,"request_priced_cost_nanos":cost_nanos,"legacy_cost_projection":"USD half-even to 1e-6"});
    }
    Ok(())
}

/// Explicit operator registration for a credential-free local service. This is
/// unavailable to request payloads and does not change any token model routing.
pub fn credentialless_local(model: &Value, provider: &Value) -> bool {
    model["metadata"]["billing_meter"] == "requests"
        && provider["metadata"]["allow_unauthenticated_local_model"] == true
        && matches!(
            provider["api_type"].as_str(),
            Some("local" | "selfhosted" | "bge")
        )
}

#[cfg(test)]
#[path = "request_meter_tests.rs"]
mod tests;
/// The public raw writer must not erase a fabricated token quantity by projecting
/// it to NULL, or accept prepriced request costs that disagree with frozen terms.
pub fn validate_raw_record(record: &UsageRecord) -> Result<(), UsageRecorderError> {
    let snapshot = record
        .metadata
        .get("pricing_snapshot")
        .cloned()
        .unwrap_or(Value::Null);
    if request_count(record)?.is_some() || is_snapshot(&snapshot) {
        let mut verified = record.clone();
        prepare(&mut verified, &snapshot)?;
        if verified.input_cost_cents != record.input_cost_cents
            || verified.output_cost_cents != record.output_cost_cents
            || verified.metadata.get("request_cost_nanos")
                != record.metadata.get("request_cost_nanos")
        {
            return Err(UsageRecorderError::InvalidAccountingDelta);
        }
    }
    Ok(())
}
