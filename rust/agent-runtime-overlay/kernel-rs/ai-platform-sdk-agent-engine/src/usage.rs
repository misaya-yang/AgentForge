use serde_json::{Value, json};
fn count(v: &Value) -> Option<u64> {
    v.as_u64().filter(|n| *n <= i64::MAX as u64)
}
fn sum(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0u64, |a, n| {
        a.checked_add(n?).filter(|v| *v <= i64::MAX as u64)
    })
}
pub fn observation(
    input: Option<u64>,
    output: Option<u64>,
    details: Value,
    compatible: bool,
) -> Value {
    let input = input.filter(|n| *n <= i64::MAX as u64);
    let output = output.filter(|n| *n <= i64::MAX as u64);
    let mut v = json!({"sdk_usage":{"state":"unavailable","cost_state":"unavailable","provider_usage":details,"platform_pricing_state":if compatible{"two_rate_compatible"}else{"reconciliation_required"}}});
    if let Some(i) = input {
        v["input_tokens"] = json!(i);
    }
    if let Some(o) = output {
        v["output_tokens"] = json!(o);
    }
    if let (Some(i), Some(o)) = (input, output) {
        if let Some(t) = i.checked_add(o).filter(|n| *n <= i64::MAX as u64) {
            v["total_tokens"] = json!(t);
            v["sdk_usage"]["state"] = json!("reported");
        }
    }
    v
}
pub fn aggregate(raw: &[Value], claude: bool) -> Value {
    let input = sum(raw.iter().map(|v| {
        if claude {
            sum([
                count(&v["input_tokens"]),
                count(&v["cache_read_input_tokens"]),
                count(&v["cache_creation_input_tokens"]),
            ]
            .into_iter())
        } else {
            count(&v["input_tokens"])
        }
    }));
    let output = sum(raw.iter().map(|v| count(&v["output_tokens"])));
    let compatible = !raw.is_empty()
        && raw.iter().all(|v| {
            if claude {
                count(&v["cache_read_input_tokens"]) == Some(0)
                    && count(&v["cache_creation_input_tokens"]) == Some(0)
            } else {
                count(&v["input_tokens_details"]["cached_tokens"]) == Some(0)
                    && count(&v["input_tokens_details"]["cache_write_tokens"]) == Some(0)
            }
        });
    let details: Vec<Value> = raw
        .iter()
        .map(|value| {
            let mut item = serde_json::Map::new();
            for key in if claude {
                &[
                    "input_tokens",
                    "output_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                    "total_tokens",
                ][..]
            } else {
                &["input_tokens", "output_tokens", "total_tokens"][..]
            } {
                if let Some(n) = count(&value[*key]) {
                    item.insert((*key).into(), json!(n));
                }
            }
            if !claude && value.is_object() {
                let mut cache = serde_json::Map::new();
                for key in ["cached_tokens", "cache_write_tokens"] {
                    if let Some(n) = count(&value["input_tokens_details"][key]) {
                        cache.insert(key.into(), json!(n));
                    }
                }
                item.insert("input_tokens_details".into(), Value::Object(cache));
            }
            Value::Object(item)
        })
        .collect();
    observation(
        if raw.is_empty() { None } else { input },
        if raw.is_empty() { None } else { output },
        json!(details),
        compatible,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_is_not_zero() {
        assert_eq!(aggregate(&[], false)["sdk_usage"]["state"], "unavailable");
        assert_eq!(
            aggregate(&[json!({"input_tokens":1})], false)["input_tokens"],
            1
        );
        assert!(
            aggregate(&[json!({"input_tokens":1})], false)
                .get("output_tokens")
                .is_none()
        );
    }
    #[test]
    fn cache_components_required() {
        assert!(
            aggregate(&[json!({"input_tokens":1,"output_tokens":2})], true)
                .get("input_tokens")
                .is_none()
        );
    }
}

#[cfg(test)]
#[path = "sdk_migration_parity_usage.rs"]
mod sdk_migration_parity_usage;

/// A platform cost is asserted only from a complete set of Gateway-settled call
/// receipts. Provider usage fields remain observations, not a second charge.
pub fn with_model_receipts(
    raw: &[Value],
    receipts: &[Value],
    claude: bool,
) -> Result<Value, &'static str> {
    if raw.is_empty() || raw.len() != receipts.len() || receipts.len() > 32 {
        return Err("sdk_model_receipt_missing");
    }
    let mut total = 0u64;
    let mut ids = std::collections::HashSet::new();
    for receipt in receipts {
        let id = receipt["receipt_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("sdk_model_receipt_invalid")?;
        if receipt["settled"] != true || !ids.insert(id) {
            return Err("sdk_model_receipt_invalid");
        }
        total = total
            .checked_add(
                receipt["cost_nanos"]
                    .as_u64()
                    .ok_or("sdk_model_receipt_invalid")?,
            )
            .ok_or("sdk_model_cost_overflow")?;
    }
    let mut usage = aggregate(raw, claude);
    // The existing outer SDK accounting path accepts only two_rate_compatible.
    // A settled inner model call must never be charged again as a service call.
    usage["sdk_usage"]["platform_pricing_state"] = "model_plane_settled".into();
    usage["sdk_usage"]["model_plane"] =
        json!({"state":"settled","cost_nanos":total,"receipts":receipts});
    Ok(usage)
}
#[cfg(test)]
mod model_receipt_tests {
    use super::*;
    #[test]
    fn costs_require_every_unique_settled_gateway_receipt() {
        let usage = json!({"input_tokens":2,"output_tokens":1});
        let receipt = json!({"settled":true,"receipt_id":"one","cost_nanos":23});
        assert_eq!(
            with_model_receipts(&[usage.clone()], &[receipt.clone()], false).unwrap()["sdk_usage"]
                ["model_plane"]["cost_nanos"],
            23
        );
        assert!(with_model_receipts(&[usage.clone()], &[], false).is_err());
        assert!(
            with_model_receipts(
                &[usage.clone(), usage.clone()],
                &[receipt.clone(), receipt.clone()],
                false
            )
            .is_err()
        );
        let mut unsettled = receipt;
        unsettled["settled"] = false.into();
        assert!(with_model_receipts(&[usage], &[unsettled], false).is_err());
    }
}
