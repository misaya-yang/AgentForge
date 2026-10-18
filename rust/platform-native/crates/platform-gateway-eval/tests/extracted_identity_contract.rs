//! Frozen identities cross-checked with Python's original canonical/hash rules.
//! These tests compile against the independent crate, without Gateway/runtime.
use ai_platform_gateway_eval::migration_compat::{
    eval_feedback::build_redacted_dataset_case,
    eval_queue::{build_eval_dataset_manifest, canonical_hash},
};
use serde_json::json;

#[test]
fn canonical_manifest_hash_preserves_python_unicode_exponent_and_negative_zero() {
    let value = json!({"z":1e-5,"a":"α","zero":-0.0});
    assert_eq!(
        canonical_hash(&value).unwrap(),
        "d18667e52df64929167a5df13a0dc7020a1c02846e1bf4394412313744bf4514"
    );
    assert_eq!(
        canonical_hash(&json!({"zero":-0.0,"a":"α","z":1e-5})).unwrap(),
        canonical_hash(&value).unwrap()
    );
    assert_ne!(
        canonical_hash(&json!({"z":1e-5,"a":"α","zero":0.0})).unwrap(),
        canonical_hash(&value).unwrap()
    );
}
#[test]
fn extracted_manifest_still_sorts_cases_and_binds_assertions_review_and_provenance() {
    let dataset =
        json!({"dataset_id":"d","tenant_id":"t","name":"Cases","version":"v1","metadata":{}});
    let a = json!({"example_id":"e-a","split":"regression","input":{"message":"α"},"expected_output":{"contains":"answer"},"source_trace_id":"trace-a","metadata":{"case_id":"a","review_status":"approved","assertions":[{"type":"output_contains","value":"answer"}]}});
    let b = json!({"example_id":"e-b","split":"test","input":{},"expected_output":{},"metadata":{"case_id":"b"}});
    let before = canonical_hash(&build_eval_dataset_manifest(
        &dataset,
        &[a.clone(), b.clone()],
    ))
    .unwrap();
    assert_eq!(
        before,
        canonical_hash(&build_eval_dataset_manifest(
            &dataset,
            &[b.clone(), a.clone()]
        ))
        .unwrap()
    );
    for field in [
        "input",
        "expected_output",
        "source_trace_id",
        "split",
        "metadata",
    ] {
        let mut changed = a.clone();
        changed[field] = if matches!(field, "split" | "source_trace_id") {
            json!("changed")
        } else {
            json!({"changed":true})
        };
        assert_ne!(
            before,
            canonical_hash(&build_eval_dataset_manifest(
                &dataset,
                &[changed, b.clone()]
            ))
            .unwrap(),
            "{field}"
        );
    }
    let mut encoded = a.clone();
    for field in ["input", "expected_output", "metadata"] {
        encoded[field] = json!(a[field].to_string());
    }
    assert_eq!(
        before,
        canonical_hash(&build_eval_dataset_manifest(&dataset, &[encoded, b])).unwrap()
    );
}
#[test]
fn feedback_case_sha1_identity_retains_family_trace_fallback_and_ten_hex_prefix() {
    let id = "44444444-4444-4444-4444-444444444444";
    let detail = json!({"trace":{"trace_id":id,"input_preview":"question","output_preview":"answer","metadata":{}}});
    for (family, prefix) in [("assistant", "0112bfb790"), ("rag", "134314ab22")] {
        let pattern =
            json!({"trace_id":id,"trace_family":family,"failure_mode":"approval_blocked"});
        let case = build_redacted_dataset_case(&detail, &pattern, "test");
        assert_eq!(
            case["case_id"],
            format!("{family}-{prefix}-approval_blocked")
        );
        assert_eq!(case["source_trace_id"], id);
        assert_eq!(case["split"], "test");
        let mut fallback = detail.clone();
        fallback["trace"]["trace_id"] = json!("");
        assert_eq!(
            build_redacted_dataset_case(&fallback, &pattern, "test")["case_id"],
            case["case_id"]
        );
    }
}
