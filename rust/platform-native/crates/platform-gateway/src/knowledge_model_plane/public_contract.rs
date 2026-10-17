//! Source-owned additive public usage contract. Physical request-meter NULL
//! token facts never change the existing aggregate integer fields to nullable.
use serde_json::{Value, json};
pub fn extend_document(document: &mut Value) {
    let Some(schemas) = document
        .pointer_mut("/components/schemas")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    schemas.insert("UsageMeteringSummary".into(),json!({"type":"object","additionalProperties":false,"required":["schema","token_totals_scope","request_priced_invocations","tokens_not_applicable_records","request_priced_cost_nanos","legacy_cost_projection"],"properties":{"schema":{"type":"string","enum":["agentforge.usage-metering/v1"]},"token_totals_scope":{"type":"string","enum":["reported_token_usage_only"]},"request_priced_invocations":{"type":"integer","minimum":0},"tokens_not_applicable_records":{"type":"integer","minimum":0},"request_priced_cost_nanos":{"type":"integer","minimum":0,"description":"Exact aggregate frozen request price quotes in nano-USD."},"legacy_cost_projection":{"type":"string","enum":["USD half-even to 1e-6"]}}}));
    if let Some(summary) = schemas.get_mut("UsageSummaryResponse") {
        if let Some(properties) = summary.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert("metering".into(),json!({"$ref":"#/components/schemas/UsageMeteringSummary","description":"Present when request-priced invocations exist. Aggregate token integers sum only applicable known token facts, never per-request invented zeros."}));
            for key in ["total_input_tokens", "total_output_tokens", "total_tokens"] {
                if let Some(field) = properties.get_mut(key).and_then(Value::as_object_mut) {
                    field.insert("description".into(),json!("Sum of applicable reported token facts. Request-priced facts are not applicable and are described by metering; this aggregate remains an integer for existing clients."));
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn additive_metering_preserves_existing_integer_sdk_contract() {
        let mut document = json!({"components":{"schemas":{"UsageSummaryResponse":{"type":"object","properties":{"total_input_tokens":{"type":"integer"},"total_output_tokens":{"type":"integer"},"total_tokens":{"type":"integer"}},"required":["total_input_tokens","total_output_tokens","total_tokens"]}}}});
        extend_document(&mut document);
        let summary = &document["components"]["schemas"]["UsageSummaryResponse"];
        for key in ["total_input_tokens", "total_output_tokens", "total_tokens"] {
            assert_eq!(summary["properties"][key]["type"], "integer");
            assert!(summary["properties"][key].get("anyOf").is_none());
        }
        assert!(
            !summary["required"]
                .as_array()
                .unwrap()
                .contains(&json!("metering"))
        );
        assert!(summary["properties"].get("metering").is_some());
    }
}
