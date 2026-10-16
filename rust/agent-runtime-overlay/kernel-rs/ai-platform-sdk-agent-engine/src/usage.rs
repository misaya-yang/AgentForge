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
