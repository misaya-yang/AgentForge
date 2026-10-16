//! Complete test_assistant_capability_contract.py result-level evidence matrix.
use ai_platform_eval::assistant_capability::{
    CapabilityCasePolicy as Policy, CapabilityTrialReceipt as Receipt, evaluate,
};
use serde_json::{Value, json};
fn policy(id: &str, tier: &str) -> Policy {
    Policy::from_value(json!({"case_id":id,"evidence_tier":tier})).unwrap()
}
fn receipt(id: &str, trial: usize, tier: &str, kind: &str, name: &str, passed: bool) -> Receipt {
    Receipt::from_value(json!({"case_id":id,"evidence_tier":tier,"trial":trial,"checks":[{"name":name,"kind":kind,"passed":passed,"evidence":format!("trial-{trial} bounded receipt")}]})).unwrap()
}
fn failure(v: &Value, text: &str) {
    assert!(
        v["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains(text)),
        "{v}"
    );
}
#[test]
fn quality_passes_two_of_three_result_trials() {
    let p = policy("assistant.multi_turn.exact_marker", "real_provider");
    let r = (1..=3)
        .map(|i| {
            receipt(
                &p.case_id,
                i,
                "real_provider",
                "outcome",
                "exact_answer",
                i < 3,
            )
        })
        .collect::<Vec<_>>();
    let v = evaluate(&[p], &r, 0.9).unwrap();
    assert_eq!(v["passed"], true);
    assert_eq!(v["case_pass_rate"], 1.);
    assert_eq!(v["cases"][0]["observed_passes"], 2);
}
#[test]
fn critical_requires_every_repetition() {
    let mut p = policy("assistant.security.tenant_isolation", "local_live");
    p.critical = true;
    p.minimum_passes = 3;
    let r = (1..=3)
        .map(|i| {
            receipt(
                &p.case_id,
                i,
                "local_live",
                "safety",
                "tenant_isolated",
                i < 3,
            )
        })
        .collect::<Vec<_>>();
    let id = p.case_id.clone();
    let v = evaluate(&[p], &r, 0.9).unwrap();
    assert_eq!(v["passed"], false);
    assert_eq!(v["critical_failures"], json!([id]));
}
#[test]
fn proxy_only_never_proves_capability() {
    for name in ["non_empty_output", "no_run_error"] {
        let p = policy("assistant.code.executes", "local_live");
        let r = (1..=3)
            .map(|i| receipt(&p.case_id, i, "local_live", "proxy", name, true))
            .collect::<Vec<_>>();
        let v = evaluate(&[p], &r, 0.9).unwrap();
        assert_eq!(v["passed"], false);
        assert_eq!(v["cases"][0]["observed_passes"], 0);
        failure(&v, "proxy-only trials cannot prove capability");
    }
}
#[test]
fn protocol_requires_outcome_or_safety() {
    let p = policy("assistant.document.valid_pdf", "local_live");
    let r = (1..=3)
        .map(|i| {
            receipt(
                &p.case_id,
                i,
                "local_live",
                "protocol",
                "terminal_received",
                true,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(evaluate(&[p], &r, 0.9).unwrap()["passed"], false);
}
#[test]
fn mixed_evidence_tiers_fail_closed() {
    let p = policy("assistant.kb.grounded", "real_provider");
    let r = (1..=3)
        .map(|i| {
            receipt(
                &p.case_id,
                i,
                if i == 3 { "mock" } else { "real_provider" },
                "outcome",
                "exact_answer",
                true,
            )
        })
        .collect::<Vec<_>>();
    let v = evaluate(&[p], &r, 0.9).unwrap();
    assert_eq!(v["passed"], false);
    failure(&v, "receipt evidence tier does not match policy");
}
#[test]
fn trial_numbers_and_counts_are_exact() {
    let mut p = policy("assistant.control.cancel", "browser");
    p.critical = true;
    p.minimum_passes = 3;
    let r = [1, 3, 4]
        .into_iter()
        .map(|i| receipt(&p.case_id, i, "browser", "safety", "cancelled", true))
        .collect::<Vec<_>>();
    let v = evaluate(&[p], &r, 0.9).unwrap();
    assert_eq!(v["passed"], false);
    failure(&v, "trial numbers must be exactly [1, 2, 3]");
}
#[test]
fn overall_gate_requires_ninety_percent() {
    let policies=(0..10).map(|i|Policy::from_value(json!({"case_id":format!("case-{i}"),"evidence_tier":"mock","repetitions":1,"minimum_passes":1})).unwrap()).collect::<Vec<_>>();
    let receipts = policies
        .iter()
        .enumerate()
        .map(|(i, p)| receipt(&p.case_id, 1, "mock", "outcome", "exact_answer", i < 8))
        .collect::<Vec<_>>();
    let v = evaluate(&policies, &receipts, 0.9).unwrap();
    assert_eq!(v["case_pass_rate"], 0.8);
    assert_eq!(v["passed"], false);
    failure(&v, "case pass rate 0.8000 < 0.9000");
}
#[test]
fn critical_configuration_cannot_weaken_repetition_requirement() {
    let e=Policy::from_value(json!({"case_id":"assistant.security.policy","evidence_tier":"offline","critical":true,"repetitions":3,"minimum_passes":2})).err().unwrap();
    assert!(e.to_string().contains("must pass every repetition"));
}
