//! Pure model-ledger projection cases from test_runtime_model_evidence.py.
use ai_platform_gateway::migration_compat::eval_observation::runtime_model_evidence;
use serde_json::{Value, json};
const RUN: &str = "11111111-1111-4111-8111-111111111111";
fn fixture() -> (Value, Value) {
    let r = json!({"tenant_id":"tenant-a","provider_id":"dashscope","model_id":"qwen3.7-plus","capability_revision":3,"price_version":"price-v1"});
    let mut e = r.clone();
    e.as_object_mut().unwrap().extend(json!({"session_id":"case-a","run_id":RUN,"pricing_snapshot":{"version":"price-v1"},"runtime_revision":"runtime-v1","parameters":{"temperature":0},"limits":{"max_output_tokens":512},"calls":[{"call_id":"22222222-2222-4222-8222-222222222222","status":"completed","input_tokens":13,"output_tokens":7,"cost_microusd":0,"dispatched_at":"2026-10-15T00:00:00Z","completed_at":"2026-10-15T00:00:01Z"}]}).as_object().unwrap().clone());
    (r, e)
}
fn project(r: &Value, e: &Value) -> (Value, Value, Vec<Value>) {
    runtime_model_evidence(e, "tenant-a", "case-a", RUN, r).unwrap()
}
#[test]
fn durable_ledger_supplies_model_usage_and_frozen_identity() {
    let (r, e) = fixture();
    let (f, u, s) = project(&r, &e);
    assert_eq!(f["model_ref"], r);
    assert_eq!(f["model_ref_verified"], true);
    assert_eq!(
        u,
        json!({"input_tokens":13,"output_tokens":7,"total_tokens":20})
    );
    assert_eq!(s[0]["span_kind"], "model_invocation");
    assert_eq!(s[0]["status"], "succeeded");
    assert_eq!(s[0]["duration_ms"], 1000);
    assert_eq!(s[0]["attributes"]["call_id"], e["calls"][0]["call_id"]);
    assert_eq!(project(&r, &e), (f, u, s));
}
#[test]
fn execution_receipt_proves_effect_without_projecting_arguments() {
    let (r, mut e) = fixture();
    e["tool_executions"] = json!([{"execution_id":"33333333-3333-4333-8333-333333333333","capability_id":"generate_quiz","status":"succeeded","effect":"write","approval_status":"approved","quiz_id":"44444444-4444-4444-8444-444444444444","arguments":{"private":"must-not-project"}}]);
    let (_, _, s) = project(&r, &e);
    let last = s.last().unwrap();
    assert_eq!(last["span_kind"], "tool_execution");
    assert_eq!(last["name"], "generate_quiz");
    assert_eq!(last["status"], "succeeded");
    assert_eq!(
        last["attributes"]["quiz_id"],
        e["tool_executions"][0]["quiz_id"]
    );
    assert!(last["attributes"].get("arguments").is_none());
    assert!(
        !serde_json::to_string(&s)
            .unwrap()
            .contains("must-not-project")
    );
}
#[test]
fn six_scope_or_model_identity_mutations_rejected() {
    for (k, v) in [
        ("tenant_id", json!("foreign")),
        ("session_id", json!("another-case")),
        ("provider_id", json!("foreign")),
        ("model_id", json!("another-model")),
        ("capability_revision", json!(4)),
        ("pricing_snapshot", json!({"version":"changed"})),
    ] {
        let (r, mut e) = fixture();
        e[k] = v;
        let error = runtime_model_evidence(&e, "tenant-a", "case-a", RUN, &r).unwrap_err();
        assert!(
            error.detail.as_str().unwrap().contains("AGENT_EVAL_MODEL_"),
            "{k}: {error:?}"
        );
    }
}
#[test]
fn unobserved_calls_and_usage_are_not_invented() {
    let (r, mut e) = fixture();
    e["calls"][0]["input_tokens"] = Value::Null;
    let mut reserved = e["calls"][0].clone();
    reserved["status"] = json!("reserved");
    reserved["dispatched_at"] = Value::Null;
    reserved["input_tokens"] = json!(9999);
    e["calls"].as_array_mut().unwrap().push(reserved.clone());
    let (_, u, s) = project(&r, &e);
    assert_eq!(u, json!({"output_tokens":7}));
    assert_eq!(s.len(), 1);
    e["calls"] = json!([reserved]);
    let (_, u, s) = project(&r, &e);
    assert_eq!(u, json!({}));
    assert!(s.is_empty());
}
#[test]
fn prompt_and_tools_require_persisted_fingerprints() {
    let (r, mut e) = fixture();
    assert!(project(&r, &e).0.get("system_prompt_hash").is_none());
    e["eval_fingerprint"] =
        json!({"system_prompt_hash":"a".repeat(64),"tool_schema_hash":"b".repeat(64)});
    let f = project(&r, &e).0;
    assert_eq!(f["system_prompt_hash"], "a".repeat(64));
    assert_eq!(f["tool_schema_hash"], "b".repeat(64));
    e["eval_fingerprint"] = json!({"system_prompt_hash":"unverified","tool_schema_hash":""});
    let f = project(&r, &e).0;
    assert!(f.get("system_prompt_hash").is_none());
    assert!(f.get("tool_schema_hash").is_none());
}
