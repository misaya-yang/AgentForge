//! Observable outcome contracts, independent of Python repository import mechanics.
use ai_platform_gateway::migration_agent::spec;
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn example() -> Value {
    json!({"schema_version":"agent-spec/v1","identity":{},"outcome":{"goal":"Resolve support requests with a clear owner.","triggers":["A new support request arrives"],"inputs":["Request text","Customer tier"],"human_boundaries":["Ask before issuing a refund"],"success_criteria":["Queue and owner are explicit"]},"instructions":"Use only configured capabilities.","model":{"model_id":"qwen3.7-plus"},"capabilities":[],"knowledge":[],"memory":{}})
}
#[test]
fn outcome_is_closed_redacted_rendered_and_preserved_only_when_present() {
    let original = example();
    assert!(spec::safety_errors(&original).is_empty());
    assert_eq!(
        spec::redact_for_read(&original)["outcome"],
        original["outcome"]
    );
    assert_eq!(
        spec::outcome_contract(&original),
        "[Agent outcome contract]\nGoal: Resolve support requests with a clear owner.\nTriggers:\n- A new support request arrives\nRequired inputs:\n- Request text\n- Customer tier\nHuman boundaries:\n- Ask before issuing a refund\nSuccess criteria:\n- Queue and owner are explicit"
    );
    assert_eq!(spec::copy_spec(&original)["outcome"], original["outcome"]);
    let mut legacy = original.clone();
    legacy.as_object_mut().unwrap().remove("outcome");
    assert!(spec::copy_spec(&legacy).get("outcome").is_none());
    let mut invalid = original;
    invalid["outcome"]["unknown"] = json!("forbidden");
    invalid["outcome"]["success_criteria"] = json!([""]);
    let errors = spec::validate(&invalid);
    let codes = errors
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        codes,
        BTreeSet::from(["AGENT_SPEC_FIELD_FORBIDDEN", "AGENT_OUTCOME_LIST_INVALID"])
    );
}
