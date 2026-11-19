//! Exact experiment comparison business assertions; repository transport is not substituted.
use ai_platform_gateway::migration_compat::eval_comparison::{
    aggregate_live_case_rows, compare_runs,
};
use serde_json::{Value, json};
fn run(id: &str, score: f64, errors: u64) -> Value {
    json!({"run_id":id,"experiment_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","run_mode":"live_candidate","status":"succeeded","repetitions":1,"dataset_manifest_hash":"manifest-a","evaluator_suite_hash":"suite-a","score_summary":{"schema_version":"eval-gate-metrics/v2","case_count":12,"score_sum":score*12.0,"failed_case_count":errors,"overall_score":score,"pass_rate":(12-errors) as f64/12.0,"trajectory_case_count":12,"trajectory_failed_count":errors,"trajectory_pass_rate":(12-errors) as f64/12.0,"critical_case_count":1,"critical_failed_count":errors.min(1),"critical_pass_rate":1.0-errors.min(1) as f64,"stateful_case_count":0,"stateful_failed_count":0,"stateful_pass_rate":null,"behavior_pass_rate":1.0,"flaky_rate":0.0},"metrics":{"latency_p50_ms":100,"latency_p95_ms":100,"input_tokens_per_task":60,"output_tokens_per_task":40,"total_tokens_per_task":100,"cost_per_task_cents":1,"execution_error_rate":errors as f64/12.0,"behavior_failure_rate":0.0,"failed_trials":errors,"mixed_runtime":false,"actual_fingerprint":{"system_prompt_hash":"prompt-a","tool_schema_hash":"tools-a","model_id":"qwen3.7-plus","provider":"dashscope","sampling":{"temperature":0.5,"max_tokens":4096},"runtime_revision":"runtime-a","rag_config_hash":"rag-a","rag_revision_hash":"rag-revision-a","execution_policy":{"execution_profile":"safe"}}}})
}
fn cases(failed: Option<usize>) -> Vec<Value> {
    (0..12).map(|i|{let passed=failed!=Some(i);json!({"run_case_id":format!("run-case-{i}"),"case_id":format!("case-{i:02}"),"example_id":format!("example-{i}"),"trial_index":1,"status":"succeeded","input":{"message":format!("message {i}")},"expected_output":{},"assertions":[],"metadata":{"critical":i==0},"observed_metrics":{"case_id":format!("case-{i:02}"),"trial_index":1,"trace_id":format!("trace-{i}"),"execution_succeeded":true,"behavior_pass":passed,"aggregate_score":if passed{0.9}else{0.0},"latency_ms":100,"input_tokens":60,"output_tokens":40,"total_tokens":100,"cost_cents":1.0,"output_preview":format!("output {i}"),"tool_trajectory":[],"rag_evidence":[],"contract_failures":if passed{json!([])}else{json!(["behavior failed"])}}})}).collect()
}
fn contains(v: &Value, path: &str, expected: &str) {
    assert!(
        v.pointer(path)
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!(expected)),
        "{path} needs {expected}: {v}"
    );
}
#[test]
fn aggregation_does_not_hide_missing_trial_usage() {
    let mut rows = vec![cases(None)[0].clone(); 2];
    rows[0]["run_case_id"] = json!("run-case-a");
    rows[1]["run_case_id"] = json!("run-case-b");
    rows[1]["trial_index"] = json!(2);
    rows[1]["observed_metrics"]["trial_index"] = json!(2);
    rows[1]["observed_metrics"]["cost_cents"] = Value::Null;
    rows[1]["observed_metrics"]["total_tokens"] = Value::Null;
    let m = &aggregate_live_case_rows(&rows)["case-00"]["observed_metrics"];
    assert_eq!(m["latency_ms"], 100.0);
    assert!(m["total_tokens"].is_null());
    assert!(m["cost_cents"].is_null());
}
#[test]
fn judge_failure_keeps_execution_success_and_quality_unknown() {
    let mut row = cases(None).remove(0);
    row["status"] = json!("failed");
    let m = &mut row["observed_metrics"];
    m["execution_outcome"] = json!("judge_failed");
    m["behavior_pass"] = Value::Null;
    m["aggregate_score"] = Value::Null;
    m["error"] = json!("eval_judge_infrastructure_failure");
    let c = &aggregate_live_case_rows(&[row])["case-00"];
    assert_eq!(c["execution_status"], "succeeded");
    assert_eq!(c["status"], "unscored");
    assert!(c["behavior_pass"].is_null());
}
#[test]
fn critical_quality_and_execution_regressions_block() {
    let b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.86, 1);
    c["metrics"]["actual_fingerprint"]["system_prompt_hash"] = json!("prompt-b");
    let v = compare_runs(&b, &c, &cases(None), &cases(Some(0)));
    assert_eq!(v["compatibility"]["compatible"], true);
    assert_eq!(v["attribution"], "isolated_change");
    assert_eq!(v["statistics"]["paired_case_count"], 12);
    assert_eq!(v["case_diffs"][0]["case_id"], "case-00");
    assert_eq!(v["case_diffs"][0]["status"], "regressed");
    assert_eq!(v["gate"]["status"], "fail");
    for reason in [
        "critical_case_regression",
        "quality_regression",
        "execution_error_regression",
    ] {
        contains(&v, "/gate/failures", reason);
    }
}
#[test]
fn execution_fault_is_unscored_rather_than_quality_failure() {
    let b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.9, 1);
    c["status"] = json!("failed");
    let mut rows = cases(None);
    rows[0]["status"] = json!("failed");
    let m = &mut rows[0]["observed_metrics"];
    m["execution_succeeded"] = json!(false);
    m["behavior_pass"] = Value::Null;
    m["aggregate_score"] = Value::Null;
    m["error"] = json!("provider unavailable");
    let v = compare_runs(&b, &c, &cases(None), &rows);
    assert_eq!(v["case_diffs"][0]["status"], "unscored");
    assert_eq!(v["case_diffs"][0]["candidate_execution_status"], "failed");
    assert_eq!(v["statistics"]["paired_case_count"], 11);
    assert_eq!(v["statistics"]["evidence_status"], "unverifiable");
    contains(&v, "/compatibility/reasons", "unscored_case_results");
    assert_eq!(v["gate"]["status"], "fail");
}
fn bind_version(run: &mut Value, version: &str, spec: &str) {
    let identity = json!({"agent_id":"agent-a","agent_version_id":version,"agent_spec_hash":spec,"agent_runtime_snapshot_hash":format!("sha256:{version}")});
    run["target_snapshot"] = identity.clone();
    run["target_snapshot"]["candidate_type"] = json!("agent_version");
    run["metrics"]["actual_fingerprint"]
        .as_object_mut()
        .unwrap()
        .extend(identity.as_object().unwrap().clone());
}
#[test]
fn fixed_agent_versions_warn_about_fixed_sample() {
    let mut b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.9, 0);
    bind_version(&mut b, "version-a", "spec-a");
    bind_version(&mut c, "version-b", "spec-b");
    let v = compare_runs(&b, &c, &cases(None), &cases(None));
    assert_eq!(v["compatibility"]["compatible"], true);
    contains(&v, "/changed_dimensions", "agent_version");
    assert_eq!(v["statistics"]["paired_case_count"], 12);
    assert_eq!(v["gate"]["status"], "warning");
    contains(&v, "/gate/warnings", "fixed_sample_only");
}
#[test]
fn shared_critical_failure_still_blocks_fixed_versions() {
    let mut b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.9, 0);
    for (r, version) in [(&mut b, "version-a"), (&mut c, "version-b")] {
        bind_version(r, version, &format!("spec-{version}"));
        r["metrics"]["gate"] = json!({"profile":"agent_version_task_suite","status":"fail","reason":"known_hard_failure"});
        r["score_summary"].as_object_mut().unwrap().extend(json!({"score_sum":9.9,"overall_score":0.825,"failed_case_count":1,"pass_rate":11.0/12.0,"critical_failed_count":1,"critical_pass_rate":0.0,"behavior_pass_rate":11.0/12.0}).as_object().unwrap().clone());
    }
    let v = compare_runs(&b, &c, &cases(Some(0)), &cases(Some(0)));
    assert_eq!(v["compatibility"]["compatible"], true);
    assert_eq!(v["case_diffs"][0]["status"], "same_failure");
    assert_eq!(v["gate"]["status"], "fail");
    contains(&v, "/gate/failures", "candidate_critical_case_failed");
}
#[test]
fn unconstrained_efficiency_increase_warns_without_blocking() {
    let b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.9, 0);
    for (key, val) in [
        ("latency_p50_ms", 150.0),
        ("latency_p95_ms", 150.0),
        ("input_tokens_per_task", 78.0),
        ("output_tokens_per_task", 52.0),
        ("total_tokens_per_task", 130.0),
        ("cost_per_task_cents", 1.5),
    ] {
        c["metrics"][key] = json!(val);
    }
    let mut rows = cases(None);
    for r in &mut rows {
        r["observed_metrics"]["latency_ms"] = json!(150);
    }
    let v = compare_runs(&b, &c, &cases(None), &rows);
    assert_eq!(v["gate"]["status"], "pass");
    for reason in [
        "latency_ms_increased",
        "total_tokens_per_task_increased",
        "cost_per_task_cents_increased",
    ] {
        contains(&v, "/gate/warnings", reason);
    }
}
#[test]
fn explicit_performance_and_manifest_mismatch_block() {
    let b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.9, 0);
    c["dataset_manifest_hash"] = json!("manifest-b");
    let mut rows = cases(None);
    rows[1]["assertions"] = json!([{"type":"latency_ms_lt","value":90}]);
    let v = compare_runs(&b, &c, &cases(None), &rows);
    assert_eq!(v["compatibility"]["compatible"], false);
    assert_eq!(v["gate"]["status"], "fail");
    for reason in [
        "dataset_manifest_mismatch",
        "explicit_performance_constraint_failed",
    ] {
        contains(&v, "/gate/failures", reason);
    }
}
#[test]
fn forged_versioned_metrics_are_unverifiable() {
    let b = run("baseline", 0.9, 0);
    let mut c = run("candidate", 0.9, 0);
    c["score_summary"]["failed_case_count"] = json!(1);
    let v = compare_runs(&b, &c, &cases(None), &cases(None));
    assert_eq!(v["compatibility"]["compatible"], false);
    contains(
        &v,
        "/compatibility/reasons",
        "candidate_gate_metrics_unverifiable",
    );
    assert_eq!(v["gate"]["status"], "fail");
}
