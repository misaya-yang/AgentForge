use ai_platform_eval::{observations, rag};
use serde_json::{Value, json};
fn rows(s: &str) -> Vec<Value> {
    s.lines()
        .filter(|s| !s.trim().is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[test]
fn frozen_rag_fixture_passes_without_claiming_judge_execution() {
    let cases = rows(include_str!(
        "../../../../../tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl"
    ));
    let obs = observations(&rows(include_str!(
        "../../../../../tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl"
    )))
    .unwrap();
    let report = rag::evaluate(&cases, &obs, &rag::GateOptions::default()).unwrap();
    assert_eq!(report["gate"]["status"], "pass", "{report}");
    assert_eq!(report["answer_quality"]["status"], "not_run");
    assert_eq!(report["gate"]["not_run"], json!(["answer_quality"]));
}
#[test]
fn insufficient_samples_fail_regardless_of_perfect_retrieval() {
    let cases = rows(include_str!(
        "../../../../../tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl"
    ));
    let obs = observations(&rows(include_str!(
        "../../../../../tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl"
    )))
    .unwrap();
    let mut o = rag::GateOptions::default();
    o.min_total = 1000;
    assert_eq!(
        rag::evaluate(&cases, &obs, &o).unwrap()["gate"]["status"],
        "fail"
    );
}
