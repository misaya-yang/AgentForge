//! Dataset identity pure cases. Enqueue/release transaction integration remains separate.
use ai_platform_gateway::migration_compat::eval_queue::{
    build_eval_dataset_manifest, canonical_hash,
};
use serde_json::{Value, json};
fn fixture() -> (Value, Vec<Value>) {
    (
        json!({"dataset_id":"11111111-1111-4111-8111-111111111111","tenant_id":"tenant-a","name":"Cases","version":"v1","schema":{},"metadata":{"owner":"quality"}}),
        vec![
            json!({"example_id":"22222222-2222-4222-8222-222222222222","split":"regression","input":{"message":"Fact?"},"expected_output":{"contains":"fact"},"source_trace_id":"33333333-3333-4333-8333-333333333333","metadata":{"case_id":"fact","review_status":"approved","assertions":[{"type":"output_contains","value":"fact"}]}}),
        ],
    )
}
fn hash(d: &Value, e: &[Value]) -> String {
    canonical_hash(&build_eval_dataset_manifest(d, e)).unwrap()
}
#[test]
fn manifest_binds_behavior_review_and_provenance() {
    let (d, e) = fixture();
    let original = hash(&d, &e);
    for field in [
        "input",
        "expected_output",
        "split",
        "source_trace_id",
        "metadata",
    ] {
        let mut changed = e.clone();
        changed[0][field] = if matches!(field, "input" | "expected_output" | "metadata") {
            json!({"changed":true})
        } else {
            json!("44444444-4444-4444-8444-444444444444")
        };
        assert_ne!(hash(&d, &changed), original, "{field}");
    }
}
#[test]
fn sql_json_normalizes_but_dataset_revision_changes_identity() {
    let (mut d, e) = fixture();
    let original = hash(&d, &e);
    let mut encoded = e.clone();
    for field in ["input", "expected_output", "metadata"] {
        encoded[0][field] = json!(serde_json::to_string(&encoded[0][field]).unwrap());
    }
    assert_eq!(hash(&d, &encoded), original);
    d["version"] = json!("v2");
    assert_ne!(hash(&d, &e), original);
}
