//! Observations of committed accounting deltas, never pricing estimates.
use super::UsageRecord;
use rust_decimal::prelude::ToPrimitive;

fn observation(record: &UsageRecord) -> Option<(Option<(u64, u64)>, f64)> {
    // Legacy/unpriced records cannot establish an authoritative charged amount.
    record
        .metadata
        .get("pricing_snapshot")?
        .get("price_book_schema")?
        .as_str()?;
    if crate::request_meter::request_count(record).ok()?.is_some() {
        // Request-priced calls have no token counts. Do not turn N/A into zeros.
        let nanos = record.metadata.get("request_cost_nanos")?.as_u64()?;
        return Some((None, nanos as f64 / 1_000_000_000.0));
    }
    let input = u64::try_from(record.input_tokens).ok()?;
    let output = u64::try_from(record.output_tokens).ok()?;
    if record.input_cost_cents < 0 || record.output_cost_cents < 0 {
        return None;
    }
    let cost = record
        .input_cost_cents
        .checked_add(record.output_cost_cents)?;
    // The legacy column names contain microcents; reuse the billing authority.
    let usd = crate::billing::microcents_to_usd(cost).to_f64()?;
    Some((Some((input, output)), usd))
}

/// Call once for a newly claimed delta only after its owning transaction commits.
pub fn record(record: &UsageRecord) -> bool {
    let Some((tokens, usd)) = observation(record) else {
        return false;
    };
    match tokens {
        Some((input, output)) => {
            ai_platform_service_primitives::genai_telemetry::record_settled_usage(
                &record.tenant_id,
                &record.model,
                input,
                output,
                usd,
            )
        }
        None => ai_platform_service_primitives::genai_telemetry::record_settled_cost(
            &record.tenant_id,
            &record.model,
            usd,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn priced() -> UsageRecord {
        let mut value =
            UsageRecord::new("tenant-test", "user-test", "model-test", chrono::Utc::now());
        value.input_tokens = 12;
        value.output_tokens = 3;
        value.input_cost_cents = 250_000;
        value.output_cost_cents = 750_000;
        value.metadata.insert(
            "pricing_snapshot".into(),
            json!({"price_book_schema":"ai-platform/price-book/v2"}),
        );
        value
    }
    #[test]
    fn charged_microcents_use_the_existing_usd_authority() {
        assert_eq!(observation(&priced()), Some((Some((12, 3)), 1.0)));
    }
    #[test]
    fn absent_price_and_invalid_amounts_do_not_manufacture_metrics() {
        assert!(
            observation(&UsageRecord::new(
                "tenant-test",
                "user-test",
                "model-test",
                chrono::Utc::now()
            ))
            .is_none()
        );
        let mut record = priced();
        record.input_tokens = -1;
        assert!(observation(&record).is_none());
        record.input_tokens = 12;
        record.input_cost_cents = -1;
        assert!(observation(&record).is_none());
        record.input_cost_cents = i64::MAX;
        assert!(observation(&record).is_none());
    }
}
