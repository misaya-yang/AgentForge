use ai_platform_eval::{golden, golden_gate, golden_validation, observations};
use serde_json::{Value, json};
fn rows(s: &str) -> Vec<Value> {
    s.lines()
        .filter(|s| !s.trim().is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[test]
fn canonical_assistant_fixture_passes_exact_contract() {
    let cases = rows(include_str!(
        "../../../../../tests/fixtures/eval/golden/assistant_regression_v1.jsonl"
    ));
    let obs = observations(&rows(include_str!(
        "../../../../../tests/fixtures/eval/observations/assistant_regression_v1.jsonl"
    )))
    .unwrap();
    let validation = golden_validation::validate_cases(&cases);
    assert_eq!(validation["valid"], true, "{validation}");
    let metrics = golden::evaluate_cases(&cases, Some(&obs));
    let gate = golden_gate::apply_gate(&metrics, None, None, false, false);
    assert_eq!(gate["status"], "pass", "{metrics}\n{gate}");
}
#[test]
fn missing_observation_cannot_pass() {
    let case = json!({"case_id":"x","input":{},"expected_output":{},"expected_trajectory":{},"assertions":[],"metadata":{}});
    assert_eq!(golden::evaluate_case(&case, None)["passed"], false);
}
#[test]
fn security_cannot_pass_without_explicit_call_evidence() {
    let case = json!({"case_id":"x","input":{},"expected_output":{},"expected_trajectory":{"stateful":{"minimum_turns":2,"security":{"forbidden_tools":["dangerous"]}}},"assertions":[],"metadata":{}});
    let replay =
        json!({"status":"succeeded","turns":[{"turn_index":1},{"turn_index":2}],"security":{}});
    assert_eq!(
        golden::evaluate_case(&case, Some(&replay))["stateful_pass"],
        false
    );
}
#[test]
fn duplicate_results_fail_tool_pairing() {
    let case = json!({"case_id":"x","input":{},"expected_output":{},"expected_trajectory":{"stateful":{"minimum_turns":2,"tool_pairing":{"required":true}}},"assertions":[],"metadata":{}});
    let replay = json!({"status":"succeeded","turns":[{"turn_index":1,"tool_calls":[{"call_id":"1","name":"read"}]},{"turn_index":2,"tool_results":[{"tool_call_id":"1","status":"completed"},{"tool_call_id":"1","status":"completed"}]}]});
    assert_eq!(
        golden::evaluate_case(&case, Some(&replay))["stateful_pass"],
        false
    );
}
