//! Full behavioral cases from test_exact_output_assertion.py and test_search_indexes.py.
use ai_platform_eval::{golden, golden_validation};
use serde_json::{Value, json};
const EXPECTED: &str = "The selected knowledge does not specify that.";
fn case(value: Value) -> Value {
    json!({"case_id":"assistant.runtime.test","split":"regression","input":{"input_preview":"test"},"expected_output":{"contains":EXPECTED},"expected_trajectory":{"required_span_kinds":[],"runtime":{}},"assertions":[{"type":"output_equals","value":value}],"metadata":{"critical":true}})
}
#[test]
fn exact_output_checks_whole_response_six_source_cases() {
    for (output, passed) in [
        (EXPECTED.to_owned(), true),
        (format!("\n{EXPECTED}\n"), true),
        (
            format!("Please ask a question; I would say \"{EXPECTED}\"."),
            false,
        ),
        (format!("The mascot is invented. {EXPECTED}"), false),
        (EXPECTED.to_lowercase(), false),
        (String::new(), false),
    ] {
        let case = case(json!(EXPECTED));
        assert_eq!(
            golden_validation::validate_cases(&[case.clone()])["valid"],
            true
        );
        let result = golden::evaluate_case(
            &case,
            Some(&json!({"status":"succeeded","output_preview":output,"span_kinds":[]})),
        );
        assert_eq!(result["passed"], passed, "{output}: {result}");
        if !passed {
            assert!(
                result["failures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f.as_str().unwrap().contains("output_equals")),
                "{result}"
            );
        }
    }
}
#[test]
fn exact_output_requires_nonempty_string_four_source_cases() {
    for value in [Value::Null, json!(""), json!(" "), json!(12)] {
        assert_eq!(
            golden_validation::validate_cases(&[case(value.clone())])["valid"],
            false,
            "{value}"
        );
    }
}
#[test]
fn trace_search_migration_declares_all_source_indexes() {
    let sql = include_str!("../../../../../database/migrations/062_agent_trace_search_indexes.sql");
    for name in [
        "pg_trgm",
        "idx_agent_traces_input_preview_trgm",
        "idx_agent_traces_output_preview_trgm",
        "idx_agent_trace_spans_trace_kind",
    ] {
        assert!(sql.contains(name), "{name}");
    }
}
#[test]
fn eval_contract_migration_allows_span_evaluators() {
    let sql = include_str!("../../../../../database/migrations/064_eval_platform_contracts.sql");
    assert!(sql.contains("chk_eval_evaluators_type"));
    assert!(sql.contains("'span'"));
}
