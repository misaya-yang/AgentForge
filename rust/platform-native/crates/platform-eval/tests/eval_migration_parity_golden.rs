//! Source golden regression business assertions; facade/import/monkeypatch identities are language-only.
use ai_platform_eval::{
    golden,
    golden_gate::{self, METRICS_SCHEMA},
    golden_validation,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn case() -> Value {
    json!({"case_id":"assistant.runtime.test","split":"regression","input":{"input_preview":"test"},"expected_output":{"contains":"ok"},"expected_trajectory":{"required_span_kinds":[],"runtime":{}},"assertions":[{"type":"output_contains","value":"ok"}],"metadata":{"critical":true}})
}
fn replay() -> Value {
    json!({"status":"succeeded","output_preview":"ok","span_kinds":[]})
}
fn metrics() -> Value {
    json!({"schema_version":METRICS_SCHEMA,"case_count":1,"score_sum":1.,"failed_case_count":0,"overall_score":1.,"pass_rate":1.,"trajectory_case_count":1,"trajectory_failed_count":0,"trajectory_pass_rate":1.,"critical_case_count":1,"critical_failed_count":0,"critical_pass_rate":1.,"stateful_case_count":0,"stateful_failed_count":0,"stateful_pass_rate":null})
}
fn gate(m: &Value) -> Value {
    golden_gate::apply_gate(m, None, None, false, false)
}
fn failure(v: &Value, message: &str) {
    assert!(
        v["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains(message)),
        "expected {message}: {v}"
    );
}
#[test]
fn gate_fails_low_quality_with_three_failures() {
    let m = json!({"schema_version":METRICS_SCHEMA,"case_count":2,"score_sum":1.,"failed_case_count":1,"overall_score":0.5,"pass_rate":0.5,"trajectory_case_count":2,"trajectory_failed_count":1,"trajectory_pass_rate":0.5,"critical_case_count":2,"critical_failed_count":1,"critical_pass_rate":0.5,"stateful_case_count":0,"stateful_failed_count":0,"stateful_pass_rate":null});
    let g = gate(&m);
    assert_eq!(g["status"], "fail");
    assert_eq!(g["failures"].as_array().unwrap().len(), 3);
}
#[test]
fn no_critical_cases_skip_only_documented_thresholds() {
    let mut c = case();
    c["metadata"]["critical"] = json!(false);
    let m = golden::evaluate_cases(
        &[c],
        Some(&BTreeMap::from([(
            "assistant.runtime.test".into(),
            replay(),
        )])),
    );
    assert_eq!(m["critical_case_count"], 0);
    assert!(m["critical_pass_rate"].is_null());
    let g = gate(&m);
    assert_eq!(g["status"], "pass");
    assert_eq!(
        g["skipped_thresholds"],
        json!([
            "critical_pass_rate:no_critical_cases",
            "stateful_pass_rate:no_stateful_cases"
        ])
    );
}
#[test]
fn missing_stateful_observation_counts_as_failure() {
    let mut cases = vec![case(), case()];
    for (i, c) in cases.iter_mut().enumerate() {
        c["case_id"] = json!(format!("assistant.stateful.{}", i + 1));
        c["expected_trajectory"]["stateful"] =
            json!({"minimum_turns":2,"tool_pairing":{"required":true}});
    }
    let mut r = replay();
    r["turns"] = json!([{"turn_index":1,"tool_calls":[{"call_id":"call-1","name":"read"}],"tool_results":[]},{"turn_index":2,"tool_calls":[],"tool_results":[{"tool_call_id":"call-1","status":"succeeded"}]}]);
    let m = golden::evaluate_cases(
        &cases,
        Some(&BTreeMap::from([("assistant.stateful.1".into(), r)])),
    );
    assert_eq!(m["stateful_case_count"], 2);
    assert_eq!(m["stateful_pass_rate"], 0.5);
    let g = gate(&m);
    assert_eq!(g["status"], "fail");
    failure(&g, "stateful hard gate requires zero failed stateful cases");
}
#[test]
fn redaction_regression_is_failure() {
    let c = json!({"case_id":"unsafe","input":{},"expected_output":{"contains":"safe"},"expected_trajectory":{"required_span_kinds":["model_invocation"]},"assertions":[],"metadata":{"critical":true,"replay":{"status":"succeeded","output_preview":"safe Authorization: Bearer raw-token","span_kinds":["model_invocation"]}}});
    let m = golden::evaluate_cases(&[c], None);
    assert_eq!(m["critical_pass_rate"], 0.);
    assert_eq!(m["cases"][0]["passed"], false);
    failure(&m["cases"][0], "sensitive replay payload");
}
#[test]
fn runtime_and_latency_mismatches_fail() {
    let mut c = case();
    c["assertions"] = json!([{"type":"latency_ms_lt","value":100}]);
    c["expected_trajectory"]["runtime"] =
        json!({"expected_exit_reason":"approval_denied","memory_sync":"skipped"});
    let mut r = replay();
    r["total_latency_ms"] = json!(101);
    r["exit_reason"] = json!("succeeded");
    r["memory_sync"] = json!("written");
    let result = golden::evaluate_case(&c, Some(&r));
    assert_eq!(result["passed"], false);
    assert_eq!(result["trajectory_pass"], false);
    failure(&result, "latency_ms_lt");
    failure(&result, "expected_exit_reason");
    let m = golden::evaluate_cases(
        &[c],
        Some(&BTreeMap::from([("assistant.runtime.test".into(), r)])),
    );
    assert_eq!(m["trajectory_pass_rate"], 0.);
}
#[test]
fn unknown_assertion_and_missing_observation_fail_closed() {
    let mut c = case();
    c["assertions"] = json!([{"type":"unknown_rule"}]);
    assert_eq!(golden_validation::validate_cases(&[c])["valid"], false);
    let result = golden::evaluate_case(&case(), None);
    assert_eq!(result["passed"], false);
    failure(&result, "missing replay observation");
}
#[test]
fn failure_mode_absent_requires_explicit_evidence() {
    let mut c = case();
    c["assertions"] = json!([{"type":"failure_mode_absent","value":"tool_error"}]);
    for extra in [json!({}), json!({"failure_mode":""})] {
        let mut r = replay();
        r.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let result = golden::evaluate_case(&c, Some(&r));
        assert_eq!(result["passed"], false);
        failure(&result, "failure_mode_absent missing evidence");
    }
    for extra in [
        json!({"failure_modes":[]}),
        json!({"failure_modes":["latency_regression"]}),
        json!({"failure_mode":"latency_regression"}),
    ] {
        let mut r = replay();
        r.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(golden::evaluate_case(&c, Some(&r))["passed"], true);
    }
}
#[test]
fn runtime_expectations_must_be_object() {
    let mut c = case();
    c["expected_trajectory"]["runtime"] = json!("not-an-object");
    let v = golden_validation::validate_cases(&[c]);
    assert_eq!(v["valid"], false);
    assert!(
        v["errors"][0]["errors"]
            .as_array()
            .unwrap()
            .contains(&json!("expected_trajectory.runtime must be an object"))
    );
}
#[test]
fn external_observation_wins_over_inline_replay() {
    let mut c = case();
    c["metadata"]["replay"] = replay();
    let result = golden::evaluate_case(
        &c,
        Some(&json!({"status":"failed","output_preview":"not ok","span_kinds":[]})),
    );
    assert_eq!(result["passed"], false);
    failure(&result, "status=failed");
}
#[test]
fn explicit_zero_baseline_not_replaced_by_average() {
    let mut baseline = metrics();
    for key in [
        "score_sum",
        "overall_score",
        "pass_rate",
        "trajectory_pass_rate",
        "critical_pass_rate",
    ] {
        baseline[key] = json!(0.);
    }
    for key in [
        "failed_case_count",
        "trajectory_failed_count",
        "critical_failed_count",
    ] {
        baseline[key] = json!(1);
    }
    baseline["average_score"] = json!(1.);
    let g = golden_gate::apply_gate(&metrics(), None, Some(&baseline), false, false);
    assert_eq!(g["status"], "pass");
    assert!(!g["failures"].to_string().contains("baseline tolerance"));
}
#[test]
fn unknown_threshold_and_count_rate_mismatches_rejected() {
    let m = metrics();
    let g = golden_gate::apply_gate(&m, Some(&json!({"future_metric":0.})), None, false, false);
    assert_eq!(g["status"], "fail");
    failure(&g, "unsupported gate thresholds");
    let mut m = m.clone();
    m["critical_pass_rate"] = Value::Null;
    let g = gate(&m);
    assert_eq!(g["status"], "fail");
    failure(&g, "critical_pass_rate must be a finite number");
    assert!(
        !g["skipped_thresholds"]
            .as_array()
            .unwrap()
            .contains(&json!("critical_pass_rate:no_critical_cases"))
    );
    m["critical_pass_rate"] = json!(1.);
    m["critical_failed_count"] = json!(1);
    let g = gate(&m);
    assert_eq!(g["status"], "fail");
    failure(&g, "inconsistent with critical case counts");
}
#[test]
fn forged_score_and_trajectory_receipts_rejected() {
    let mut m = metrics();
    m["score_sum"] = json!(0.8);
    failure(
        &gate(&m),
        "overall_score is inconsistent with score_sum and case_count",
    );
    m = metrics();
    m["trajectory_failed_count"] = json!(1);
    failure(
        &gate(&m),
        "trajectory_pass_rate is inconsistent with trajectory case counts",
    );
}
#[test]
fn impossible_subset_counts_rejected() {
    let mut m = metrics();
    m["critical_case_count"] = json!(999);
    m["stateful_case_count"] = json!(999);
    m["stateful_pass_rate"] = json!(1.);
    let g = golden_gate::apply_gate(&m, None, None, true, true);
    assert_eq!(g["status"], "fail");
    failure(&g, "critical_case_count exceeds case_count");
    failure(&g, "stateful_case_count exceeds case_count");
}
#[test]
fn authoritative_rates_accept_four_decimal_display_rounding() {
    let mut m = metrics();
    for key in ["case_count", "trajectory_case_count", "critical_case_count"] {
        m[key] = json!(3);
    }
    m["score_sum"] = json!(3.);
    for key in [
        "failed_case_count",
        "trajectory_failed_count",
        "critical_failed_count",
    ] {
        m[key] = json!(1);
    }
    for key in ["pass_rate", "trajectory_pass_rate", "critical_pass_rate"] {
        m[key] = json!(0.6667);
    }
    let g = golden_gate::apply_gate(
        &m,
        Some(
            &json!({"overall_score":0.,"trajectory_pass_rate":0.,"critical_pass_rate":0.,"stateful_pass_rate":0.,"baseline_tolerance":0.}),
        ),
        None,
        false,
        false,
    );
    assert_eq!(g["status"], "pass");
    for key in ["pass_rate", "trajectory_pass_rate", "critical_pass_rate"] {
        assert_eq!(g["metrics"][key], 2.0 / 3.0);
    }
}
#[test]
fn release_requires_explicit_nonzero_stateful_coverage() {
    let mut m = metrics();
    for key in [
        "stateful_case_count",
        "stateful_failed_count",
        "stateful_pass_rate",
    ] {
        m.as_object_mut().unwrap().remove(key);
    }
    let g = golden_gate::apply_gate(&m, None, None, false, true);
    assert_eq!(g["status"], "fail");
    failure(&g, "release gate requires non-zero stateful case coverage");
}
#[test]
fn one_critical_failure_in_large_suite_cannot_round_up() {
    let mut c = case();
    c["metadata"]["replay"] = replay();
    let mut cases = vec![c.clone(); 20_000];
    c["case_id"] = json!("assistant.runtime.last-failure");
    c["metadata"]["replay"]["status"] = json!("failed");
    cases.push(c);
    let m = golden::evaluate_cases(&cases, None);
    assert_eq!(m["critical_failed_count"], 1);
    assert!(m["critical_pass_rate"].as_f64().unwrap() < 1.);
    let g = gate(&m);
    assert_eq!(g["status"], "fail");
    failure(&g, "critical hard gate requires zero failed critical cases");
}

#[test]
fn behavior_output_tools_and_limits_pass() {
    let mut c = case();
    c["assertions"] = json!([{"type":"output_not_contains","value":"invented"},{"type":"tool_called","value":"get_account_status"},{"type":"tool_not_called","value":"delete_account"},{"type":"latency_ms_lt","value":3000},{"type":"total_tokens_lt","value":2000},{"type":"cost_cents_lt","value":5}]);
    c["expected_output"] = json!({"reference":"Explain that the tool is unavailable","rubric":"Must not invent account status","contains":["ok","fallback"],"not_contains":["invented"]});
    c["expected_trajectory"]["tools"] = json!([{"name":"get_account_status","required":true,"arguments_subset":{"account":{"id":"known"}},"order":1,"max_calls":1,"status":"succeeded"},{"name":"delete_account","forbidden":true}]);
    let r = json!({"status":"succeeded","output_preview":"ok: use the fallback","span_kinds":[],"total_latency_ms":250,"total_tokens":100,"total_cost_cents":4,"tool_calls":[{"name":"get_account_status","arguments":{"account":{"id":"known","region":"au"}},"status":"succeeded"}]});
    assert_eq!(
        golden_validation::validate_cases(&[c.clone()])["valid"],
        true
    );
    let result = golden::evaluate_case(&c, Some(&r));
    assert_eq!(result["passed"], true, "{result}");
    assert_eq!(result["trajectory_pass"], true);
}
#[test]
fn output_tool_and_performance_regressions_report_all_reasons() {
    let mut c = case();
    c["assertions"] = json!([{"type":"output_not_contains","value":"invented"},{"type":"total_tokens_lt","value":100},{"type":"cost_cents_lt","value":5}]);
    c["expected_output"] = json!({"contains":["ok"],"not_contains":["invented"]});
    c["expected_trajectory"]["tools"] = json!([{"name":"search","arguments_subset":{"query":"expected"},"order":1,"max_calls":1,"status":"succeeded"},{"name":"delete_account","forbidden":true}]);
    let r = json!({"status":"succeeded","output_preview":"ok but invented","span_kinds":[],"total_tokens":100,"total_cost_cents":5,"tool_calls":[{"name":"search","arguments":{"query":"wrong"},"status":"failed"},{"name":"search","arguments":{"query":"expected"},"status":"succeeded"},{"name":"delete_account","arguments":{},"status":"succeeded"}]});
    let result = golden::evaluate_case(&c, Some(&r));
    assert_eq!(result["passed"], false);
    assert_eq!(result["trajectory_pass"], false);
    for message in [
        "output_not_contains",
        "found forbidden text",
        "total_tokens_lt",
        "cost_cents_lt",
        "called 2 > 1",
        "did not match expected order, status, or arguments",
        "forbidden tool called",
    ] {
        failure(&result, message);
    }
}
#[test]
fn tool_execution_spans_supply_real_tool_evidence() {
    let mut c = case();
    c["assertions"] = json!([{"type":"tool_called","value":"search"}]);
    c["expected_trajectory"]["tools"] =
        json!([{"name":"search","arguments_subset":{"query":"refund"},"status":"succeeded"}]);
    let mut r = replay();
    r["spans"] = json!([{"span_kind":"tool_execution","name":"tool:search","input_preview":"{\"query\": \"refund\", \"limit\": 3}","status":"succeeded"}]);
    assert_eq!(golden::evaluate_case(&c, Some(&r))["passed"], true);
}
#[test]
fn malformed_rules_and_missing_or_negative_evidence_fail_closed() {
    let mut c = case();
    c["assertions"] = json!([{"type":"tool_not_called","value":"delete_account"},{"type":"total_tokens_lt","value":100}]);
    let result = golden::evaluate_case(&c, Some(&replay()));
    assert_eq!(result["passed"], false);
    failure(&result, "requires valid tool_calls or spans evidence");
    failure(&result, "total_tokens_lt requires numeric evidence");
    c["assertions"] = json!([{"type":"total_tokens_lt","value":100}]);
    let mut r = replay();
    r["total_tokens"] = json!(-1);
    failure(
        &golden::evaluate_case(&c, Some(&r)),
        "total_tokens_lt requires numeric evidence",
    );
    c["assertions"] = json!([{"type":"cost_cents_lt","value":0}]);
    c["expected_output"] = json!({"contains":["ok",1]});
    c["expected_trajectory"]["tools"] = json!([{"name":"search","future_constraint":true}]);
    let v = golden_validation::validate_cases(&[c.clone()]);
    assert_eq!(v["valid"], false);
    let errors = v["errors"][0]["errors"].to_string();
    for message in [
        "expected_output.contains",
        "unsupported fields future_constraint",
        "value must be a positive number",
    ] {
        assert!(errors.contains(message), "{errors}");
    }
    let r = golden::evaluate_case(&c, Some(&replay()));
    assert_eq!(r["passed"], false);
    assert!(
        r["failures"][0]
            .as_str()
            .unwrap()
            .starts_with("invalid behavior contract:")
    );
}
#[test]
fn zero_critical_rate_is_not_replaced_by_pass_rate() {
    let mut m = metrics();
    m["case_count"] = json!(2);
    m["score_sum"] = json!(2.);
    m["failed_case_count"] = json!(1);
    m["pass_rate"] = json!(0.5);
    m["trajectory_case_count"] = json!(2);
    m["critical_failed_count"] = json!(1);
    m["critical_pass_rate"] = json!(0.);
    assert_eq!(gate(&m)["status"], "fail");
}
#[test]
fn invalid_nonfinite_json_boundary_fails_closed() {
    assert!(serde_json::from_str::<Value>("{\"overall_score\":NaN}").is_err());
    assert!(serde_json::from_str::<Value>("{\"overall_score\":Infinity}").is_err());
    let mut m = metrics();
    m["overall_score"] = Value::Null;
    failure(&gate(&m), "overall_score must be a finite number");
    let g = golden_gate::apply_gate(
        &metrics(),
        Some(&json!({"overall_score":null})),
        None,
        false,
        false,
    );
    failure(&g, "threshold overall_score must be a finite number");
    let g = golden_gate::apply_gate(
        &metrics(),
        None,
        Some(&json!({"overall_score":null})),
        false,
        false,
    );
    failure(&g, "baseline overall_score must be a finite number");
    let mut c = case();
    c["assertions"] = json!([{"type":"cost_cents_lt","value":null}]);
    assert_eq!(golden_validation::validate_cases(&[c])["valid"], false);
}

#[test]
fn sensitive_output_check_accepts_persisted_trace_timestamp() {
    let mut c = case();
    c["assertions"] = json!([{"type":"no_sensitive_output"}]);
    let mut r = replay();
    r["spans"] = json!([{"span_kind":"model_invocation","started_at":"2026-10-15T00:00:00+00:00"}]);
    assert_eq!(golden::evaluate_case(&c, Some(&r))["passed"], true);
}
