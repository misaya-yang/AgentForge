//! Synthetic finance receipts exercise deterministic grading, never real-run acceptance.
use ai_platform_eval::{canonical::sha256, finance, scenario_contract, strict_json};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};
const PACKET: &str = "2ed163986166adf9283ac64e5d6875a51922a2a69eacb0ae29803548118e1899";
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn dir() -> PathBuf {
    root().join("src/services/eval/fixtures/real_finance_salesforce_fy26_q1")
}
fn golden() -> Value {
    strict_json::parse(&std::fs::read(dir().join("golden.v1.json")).unwrap()).unwrap()
}
fn passing(id: &str) -> Value {
    let g = golden();
    let metrics = g["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap().to_owned(),
                json!({"value":m["expected"],"evidence_ids":[m["evidence_ids"][0]]}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let conclusions = g["required_conclusions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_str().unwrap().to_owned(), c["expected"].clone()))
        .collect::<BTreeMap<_, _>>();
    json!({"run_id":id,"status":"completed","metrics":metrics,"conclusions":conclusions,"subagents":(["gaap_filing_analyst","non_gaap_reconciliation_analyst","skeptical_credit_reviewer"].iter().enumerate().map(|(i,role)|json!({"role":role,"batch_call_id":"batch-1","dispatch_index":i,"status":"completed","started_monotonic_ms":1000+i*10,"finished_monotonic_ms":1200+i*10,"evidence_ids":["10Q.CF.7"],"input_artifact_sha256":PACKET,"side_effects":[],"terminal_receipt_id":format!("terminal-{i}")})).collect::<Vec<_>>()),"final_answer_sha256":sha256(id.as_bytes())})
}
fn grade(r: &Value) -> Value {
    finance::evaluate_run(r, &golden(), PACKET).unwrap()
}
#[tokio::test]
async fn fixture_hashes_and_all_twenty_five_formulas_validate_offline() {
    let r = finance::validate_fixture(&dir(), false).await.unwrap();
    assert_eq!(r["fixture_valid"], true);
    assert_eq!(r["metric_count"], 25);
    assert_eq!(r["source_bytes"], json!([]));
}
#[test]
fn scenario_keeps_three_parallel_trials_and_hidden_goldens() {
    let s = scenario_contract::load(&dir().join("scenario.v1.json")).unwrap();
    let s = &s["scenarios"][0];
    assert_eq!(s["repetitions"], 3);
    assert_eq!(s["require_parallel"], true);
    assert_eq!(
        s["required_agent_ids"],
        json!([
            "community-engineering-reviewers:technical-writer",
            "community-engineering-reviewers:system-architecture-reviewer",
            "community-doublecheck:doublecheck"
        ])
    );
    assert_eq!(s["expected_assertions"].as_array().unwrap().len(), 47);
    for value in ["104.549763", "54.103343", "63.336766", "25188", "3374"] {
        assert!(!s["prompt"].as_str().unwrap().contains(value));
    }
    let assertions = s["expected_assertions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| {
            a["kind"] == "json_number" && a["path"].as_str().unwrap().starts_with("/metrics/")
        })
        .map(|a| (a["path"].as_str().unwrap().split('/').nth(2).unwrap(), a))
        .collect::<BTreeMap<_, _>>();
    let g = golden();
    assert_eq!(assertions.len(), g["metrics"].as_array().unwrap().len());
    for m in g["metrics"].as_array().unwrap() {
        let a = assertions[m["id"].as_str().unwrap()];
        assert_eq!(a["expected"], m["expected"]);
        assert_eq!(a["absolute_tolerance"], m["tolerance_abs"]);
    }
}
fn receipt(runs: Vec<Value>) -> std::process::Output {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("receipt.json");
    std::fs::write(&path,serde_json::to_vec(&json!({"schema_version":"real-finance-eval-receipt/v1","task_id":golden()["task_id"],"runs":runs})).unwrap()).unwrap();
    Command::new(env!("CARGO_BIN_EXE_validate_real_finance_eval"))
        .arg(path)
        .current_dir(root())
        .output()
        .unwrap()
}
#[test]
fn three_run_gate_uses_minimum_repeat_and_unique_identity() {
    let runs = (0..3)
        .map(|i| passing(&format!("run-{i}")))
        .collect::<Vec<_>>();
    let out = receipt(runs.clone());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["passed"], true);
    assert_eq!(r["score"], 100.0);
    let mut weak = runs;
    weak[1]["metrics"]["q1_fcf_usd_m"]["value"] = json!(25188);
    let out = receipt(weak);
    assert_eq!(out.status.code(), Some(1));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["passed"], false);
    assert!(r["score"].as_f64().unwrap() <= 75.0);
    let out = receipt(vec![
        passing("duplicate"),
        passing("duplicate"),
        passing("third"),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unique"));
}
#[test]
fn misleading_and_non_gaap_conclusions_enforce_score_caps() {
    let mut r = passing("run-1");
    r["conclusions"]["misleading_statement_verdict"] = json!("accept");
    let v = grade(&r);
    assert_eq!(v["score"], 55.0);
    assert!(v["failures"].as_array().unwrap().contains(&json!(
        "required conclusion failed: misleading_statement_verdict"
    )));
    let mut r = passing("run-1");
    r["conclusions"]["non_gaap_classification"] = json!("gaap");
    assert_eq!(grade(&r)["score"], 60.0);
}
#[test]
fn sequential_or_unbound_children_cannot_claim_parallel_delegation() {
    let mut r = passing("run-1");
    for (i, c) in r["subagents"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        c["started_monotonic_ms"] = json!(1000 + i * 200);
        c["finished_monotonic_ms"] = json!(1100 + i * 200);
    }
    let v = grade(&r);
    assert_eq!(v["score"], 85.0);
    assert!(v["failures"].as_array().unwrap().contains(&json!(
        "three specialist intervals did not overlap concurrently"
    )));
    let mut r = passing("run-1");
    r["subagents"][0]["input_artifact_sha256"] = json!("0".repeat(64));
    let v = grade(&r);
    assert_eq!(v["score"], 85.0);
    assert!(v["failures"].as_array().unwrap().contains(&json!(
        "child gaap_filing_analyst is not bound to the fixed packet"
    )));
}
#[test]
fn source_unit_and_final_hash_failures_enforce_caps() {
    let mut r = passing("run-1");
    r["metrics"]["q1_fcf_usd_m"]["evidence_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("BLOG.SECONDARY"));
    assert_eq!(grade(&r)["score"], 70.0);
    let mut r = passing("run-1");
    r["metrics"]["q1_net_cash_including_marketable_usd_m"]["value"] = json!(8.973);
    assert!(grade(&r)["score"].as_f64().unwrap() <= 75.0);
    let mut r = passing("run-1");
    r["final_answer_sha256"] = json!("not-a-hash");
    let v = grade(&r);
    assert_eq!(v["passed"], false);
    assert_eq!(v["score"], 90.0);
}
#[test]
fn duplicate_json_keys_fail_closed() {
    let e = strict_json::parse(br#"{"a":1,"a":2}"#).unwrap_err();
    assert!(e.to_string().contains("duplicate JSON key"), "{e}");
}
