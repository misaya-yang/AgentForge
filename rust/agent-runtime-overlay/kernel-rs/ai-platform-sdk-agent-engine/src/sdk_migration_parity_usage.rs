//! Source worker usage assertions, adapted to per-turn provider-wire audit arrays.
use super::*;
#[test]
fn missing_not_zero() {
    for raw in [
        vec![],
        vec![Value::Null],
        vec![json!({})],
        vec![json!({"input_tokens":5,"output_tokens":2})],
    ] {
        let r = aggregate(&raw, true);
        assert_eq!(r["sdk_usage"]["state"], "unavailable");
        assert!(r.get("input_tokens").is_none());
    }
}
#[test]
fn explicit_zero_is_reported_without_invented_cost() {
    let r = aggregate(
        &[
            json!({"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}),
        ],
        true,
    );
    assert_eq!(r["total_tokens"], 0);
    assert_eq!(r["sdk_usage"]["state"], "reported");
    assert_eq!(r["sdk_usage"]["cost_state"], "unavailable");
    assert!(r["sdk_usage"].get("provider_cost_usd").is_none());
}
#[test]
fn cache_total_and_reconciliation() {
    let r = aggregate(
        &[
            json!({"input_tokens":4,"output_tokens":1,"cache_read_input_tokens":3,"cache_creation_input_tokens":2}),
        ],
        true,
    );
    assert_eq!(
        (
            r["input_tokens"].as_u64(),
            r["output_tokens"].as_u64(),
            r["total_tokens"].as_u64()
        ),
        (Some(9), Some(1), Some(10))
    );
    assert_eq!(r["sdk_usage"]["provider_usage"][0]["input_tokens"], 4);
    assert_eq!(
        r["sdk_usage"]["platform_pricing_state"],
        "reconciliation_required"
    );
}
#[test]
fn invalid_counts_and_overflow() {
    for n in [
        json!(-1),
        json!(true),
        json!(1.5),
        json!("5"),
        json!(1u64 << 63),
        Value::Null,
    ] {
        let r = aggregate(&[json!({"input_tokens":n,"output_tokens":2})], false);
        assert_eq!(r["sdk_usage"]["state"], "unavailable");
        assert!(r.get("input_tokens").is_none());
    }
    assert!(
        observation(Some(i64::MAX as u64), Some(1), json!([]), false)
            .get("total_tokens")
            .is_none()
    );
    assert!(
        observation(Some(u64::MAX), Some(1), json!([]), false)
            .get("input_tokens")
            .is_none()
    );
}
#[test]
fn partial_evidence_is_preserved() {
    let r = aggregate(&[json!({"input_tokens":5,"output_tokens":2})], true);
    assert_eq!(r["output_tokens"], 2);
    assert!(r.get("input_tokens").is_none());
    assert_eq!(r["sdk_usage"]["provider_usage"][0]["input_tokens"], 5);
}
#[test]
fn openai_initialized_zero_is_not_evidence_and_all_responses_required() {
    let r = aggregate(&[Value::Null], false);
    assert!(r.get("input_tokens").is_none());
    let r = aggregate(
        &[json!({"input_tokens":5,"output_tokens":2}), Value::Null],
        false,
    );
    assert!(r.get("input_tokens").is_none());
    assert_eq!(r["sdk_usage"]["state"], "unavailable");
}
#[test]
fn openai_reported_totals() {
    assert_eq!(
        aggregate(&[json!({"input_tokens":14,"output_tokens":6})], false)["total_tokens"],
        20
    );
}
#[test]
fn no_arbitrary_provider_metadata() {
    for claude in [false, true] {
        let r = aggregate(
            &[
                json!({"secret":"do not preserve","input_tokens":3,"input_tokens_details":{"cached_tokens":0,"secret":"hidden"}}),
            ],
            claude,
        );
        assert!(!r.to_string().contains("secret"));
        assert!(!r.to_string().contains("do not preserve"));
        assert!(!r.to_string().contains("hidden"));
    }
    assert_eq!(
        aggregate(&[json!({"secret":"do not preserve"})], true)["sdk_usage"]["provider_usage"],
        json!([{}])
    );
}
