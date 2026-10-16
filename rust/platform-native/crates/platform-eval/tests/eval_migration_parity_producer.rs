//! Source producer-adapter assertions through canonical native projection and gates.
use ai_platform_eval::{cli::Args, golden, golden_cli, producer};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn artifact(name: &str) -> Value {
    let rows: Vec<Value> =
        serde_json::from_str(include_str!("../src/producer_goldens.json")).unwrap();
    rows.into_iter().find(|r| r["name"] == name).unwrap()["input"].clone()
}
fn adapt(row: &Value) -> Value {
    producer::adapt_producer_case(row).unwrap().1
}
fn case(id: &str) -> Value {
    include_str!("../../../../../tests/fixtures/eval/golden/assistant_regression_v1.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|v| v["case_id"] == id)
        .unwrap()
}
fn status(r: &Value, component: &str, expected: &str) {
    assert_eq!(
        r["adapter_evidence"]["components"][component]["status"], expected,
        "{r}"
    );
}
fn invalid(r: &Value, components: &[&str]) {
    for c in components {
        status(r, c, "unknown");
    }
    assert_eq!(
        r["adapter_evidence"]["integrity"]["status"], "not_verified",
        "{r}"
    );
}
fn gate(row: &Value, r: &Value, expected: bool) -> Value {
    let result = golden::evaluate_case(&case(row["case_id"].as_str().unwrap()), Some(r));
    assert_eq!(result["passed"], expected, "{result}");
    result
}
#[test]
fn canonical_tools_and_durable_hitl_pass_real_golden_gate() {
    for (name, component) in [
        ("_tool_pair_artifact", "tool_pairing"),
        ("_hitl_artifact", "hitl"),
    ] {
        let row = artifact(name);
        let r = adapt(&row);
        status(&r, "binding", "verified");
        status(&r, component, "verified");
        gate(&row, &r, true);
        if component == "hitl" {
            assert_eq!(r["turns"][0]["tool_calls"][0]["approval_required"], true);
        }
    }
}
#[test]
fn checkpoint_identity_is_required_on_each_turn() {
    let mut row = artifact("_hitl_artifact");
    for turn in row["producer"]["turns"].as_array_mut().unwrap() {
        for k in ["run_id", "tenant_id", "session_id"] {
            turn["checkpoint"].as_object_mut().unwrap().remove(k);
        }
    }
    let r = adapt(&row);
    invalid(&r, &["binding", "hitl"]);
    gate(&row, &r, false);
}
#[test]
fn monotonic_budget_projects_exact_exhaustion() {
    let r = adapt(&artifact("_budget_artifact"));
    status(&r, "budget", "verified");
    assert_eq!(
        r["turns"][0]["budget"],
        json!({"iteration":3,"max_iterations":3,"remaining":0})
    );
}
#[test]
fn budget_arithmetic_and_bounds_are_verified() {
    let mut row = artifact("_budget_artifact");
    for p in [
        "/producer/turns/0/context_snapshot/bootstrap/run_budget",
        "/producer/turns/0/events/0/data/budget",
    ] {
        let b = row.pointer_mut(p).unwrap();
        b["limits"]["max_tool_calls"] = json!(-10);
        b["usage"]["tool_calls"] = json!(-20);
        b["remaining"]["tool_calls"] = json!(-30);
    }
    invalid(&adapt(&row), &["budget"]);
    for (section, field, value) in [
        ("limits", "max_parallel_tool_calls", json!(0)),
        ("limits", "max_wall_time_seconds", Value::Null),
        ("usage", "tool_result_bytes", json!(-1)),
        ("remaining", "wall_time_ms", json!(-1)),
        ("remaining", "tool_result_bytes", json!(991)),
    ] {
        let mut row = artifact("_budget_artifact");
        row["producer"]["turns"][0]["context_snapshot"]["bootstrap"]["run_budget"][section]
            [field] = value;
        invalid(&adapt(&row), &["budget"]);
    }
    // Non-finite Python float is rejected at the native JSON boundary before adaptation.
    assert!(serde_json::from_str::<Value>("{\"max_wall_time_seconds\":NaN}").is_err());
    for (field, value) in [
        ("dimension", json!("tool_calls")),
        ("limit", json!(4)),
        ("used", json!(2)),
        ("requested", json!(3)),
    ] {
        let mut row = artifact("_budget_artifact");
        row["producer"]["turns"][0]["events"][0]["data"][field] = value;
        invalid(&adapt(&row), &["budget"]);
    }
}
#[test]
fn unrecorded_plan_and_trace_only_claims_do_not_pass() {
    let mut row = artifact("_tool_pair_artifact");
    row["case_id"] = json!("assistant.stateful.plan_retention");
    let r = adapt(&row);
    status(&r, "plan", "unknown");
    let result = gate(&row, &r, false);
    assert!(
        result["failures"]
            .as_array()
            .unwrap()
            .contains(&json!("canonical producer evidence for plan is unknown"))
    );
    row["case_id"] = json!("assistant.refund_policy.basic");
    row["producer"]["turns"] = json!([]);
    let r = adapt(&row);
    assert_eq!(r["status"], "unknown");
    invalid(&r, &["binding"]);
    let result = gate(&row, &r, false);
    assert!(result["failures"].as_array().unwrap().contains(&json!(
        "canonical producer adapter integrity is not_verified"
    )));
}
#[test]
fn cross_run_join_fails_even_with_paired_tools() {
    let mut row = artifact("_tool_pair_artifact");
    row["producer"]["turns"][1]["events"][0]["data"]["run_id"] = json!("other-run");
    let r = adapt(&row);
    status(&r, "binding", "unknown");
    status(&r, "tool_pairing", "unknown");
    gate(&row, &r, false);
}
#[test]
fn durable_predispatch_checkpoint_and_approval_order_are_required() {
    let mut row = artifact("_hitl_artifact");
    row["producer"]["turns"][0]["checkpoint"]["checkpoint_receipt"]["durability"] =
        json!("process");
    status(&adapt(&row), "hitl", "unknown");
    for (field, value) in [
        ("phase", "tool_call_completed"),
        ("checkpoint_id", ""),
        ("status", "completed"),
    ] {
        let mut row = artifact("_hitl_artifact");
        row["producer"]["turns"][1]["checkpoint"][field] = json!(value);
        invalid(&adapt(&row), &["hitl"]);
    }
    for (key, value) in [
        ("_approval_id", json!("foreign-approval")),
        ("_middleware_approval_required", json!(false)),
    ] {
        let mut row = artifact("_hitl_artifact");
        row["producer"]["turns"][1]["events"][0]["data"]["arguments"][key] = value;
        invalid(&adapt(&row), &["tool_pairing", "hitl"]);
    }
    let mut row = artifact("_hitl_artifact");
    let events = row["producer"]["turns"][1]["events"]
        .as_array_mut()
        .unwrap();
    let result = events.remove(3);
    events.insert(2, result);
    status(&adapt(&row), "hitl", "unknown");
}
#[test]
fn tool_alias_identity_and_success_must_agree() {
    for (event, key, value) in [
        (1, "tool_name", json!("other_tool")),
        (0, "name", json!("other_tool")),
        (1, "success", json!(false)),
    ] {
        let mut row = artifact("_tool_pair_artifact");
        row["producer"]["turns"][0]["events"][event]["data"][key] = value;
        status(&adapt(&row), "tool_pairing", "unknown");
    }
    let mut row = artifact("_tool_pair_artifact");
    let d = row["producer"]["turns"][0]["events"][1]["data"]
        .as_object_mut()
        .unwrap();
    d.remove("run_id");
    d.remove("session_id");
    invalid(&adapt(&row), &["binding"]);
    let mut row = artifact("_tool_pair_artifact");
    let d = row["producer"]["turns"][0]["events"][1]["data"]
        .as_object_mut()
        .unwrap();
    d.remove("tool_name");
    d.remove("success");
    status(&adapt(&row), "tool_pairing", "unknown");
}
#[test]
fn terminal_receipts_must_be_present_versioned_and_consistent() {
    let mut row = artifact("_tool_pair_artifact");
    row["producer"]["trace"]["status"] = json!("failed");
    let r = adapt(&row);
    assert_eq!(r["status"], "unknown");
    status(&r, "binding", "unknown");
    let mut row = artifact("_tool_pair_artifact");
    let t = &mut row["producer"]["turns"][0];
    t["events"].as_array_mut().unwrap().pop();
    t.as_object_mut().unwrap().remove("terminal_envelope");
    let r = adapt(&row);
    assert_eq!(r["status"], "unknown");
    invalid(&r, &["binding", "tool_pairing"]);
    for receipt in ["terminal_envelope", "context_snapshot"] {
        let mut row = artifact("_tool_pair_artifact");
        row["producer"]["turns"][0][receipt]
            .as_object_mut()
            .unwrap()
            .remove("schema_version");
        invalid(&adapt(&row), &["binding"]);
    }
    let mut row = artifact("_tool_pair_artifact");
    terminal_reason(&mut row, 0, "tool_error");
    status(&adapt(&row), "binding", "unknown");
    let mut row = artifact("_tool_pair_artifact");
    terminal_reason(&mut row, 1, "max_iterations");
    let r = adapt(&row);
    assert_eq!(r["status"], "succeeded");
    assert_eq!(r["exit_reason"], "max_iterations");
    status(&r, "binding", "verified");
}
fn terminal_reason(row: &mut Value, turn: usize, reason: &str) {
    let t = &mut row["producer"]["turns"][turn];
    t["terminal_envelope"]["exit_reason"] = json!(reason);
    let envelope = t["terminal_envelope"].clone();
    t["events"].as_array_mut().unwrap().last_mut().unwrap()["data"]["terminal_envelope"] = envelope;
}
#[test]
fn tool_lifecycle_cannot_cross_turn_boundary() {
    let mut row = artifact("_tool_pair_artifact");
    let moved: Vec<_> = row["producer"]["turns"][0]["events"]
        .as_array_mut()
        .unwrap()
        .drain(1..3)
        .collect();
    row["producer"]["turns"][1]["events"]
        .as_array_mut()
        .unwrap()
        .splice(0..0, moved);
    let r = adapt(&row);
    invalid(&r, &["tool_pairing"]);
    gate(&row, &r, false);
}
#[test]
fn paired_failures_are_verified_evidence_but_fail_business_gate() {
    let mut row = artifact("_tool_pair_artifact");
    for turn in row["producer"]["turns"].as_array_mut().unwrap() {
        for event in turn["events"].as_array_mut().unwrap() {
            if matches!(
                event["event_type"].as_str(),
                Some("tool_call_result" | "tool_call_end")
            ) {
                event["data"]["status"] = json!("failed");
                event["data"]["success"] = json!(false);
            }
        }
    }
    let r = adapt(&row);
    status(&r, "tool_pairing", "verified");
    let result = gate(&row, &r, false);
    assert!(
        result["failures"]
            .as_array()
            .unwrap()
            .contains(&json!("tool pairing requires successful terminal results"))
    );
}
#[test]
fn bootstrap_only_regressed_and_nonexhausted_budget_receipts_are_unknown() {
    let mut row = artifact("_budget_artifact");
    row["producer"]["turns"][0]["events"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    row["producer"]["trace"]["status"] = json!("succeeded");
    let t = &mut row["producer"]["turns"][0];
    t["terminal_envelope"]["status"] = json!("succeeded");
    terminal_reason(&mut row, 0, "succeeded");
    status(&adapt(&row), "budget", "unknown");
    let mut row = artifact("_budget_artifact");
    let mut budget =
        row["producer"]["turns"][0]["context_snapshot"]["bootstrap"]["run_budget"].clone();
    budget["usage"]["model_turns"] = json!(1);
    budget["remaining"]["model_turns"] = json!(2);
    row["producer"]["turns"][0]["events"][0]["data"]["budget"] = budget;
    status(&adapt(&row), "budget", "unknown");
    let mut row = artifact("_budget_artifact");
    row["producer"]["turns"][0]["events"][0]["data"]["budget"]["exhausted"] = json!(false);
    invalid(&adapt(&row), &["budget"]);
}
#[test]
fn recorded_candidate_cli_preserves_offline_tiers_and_rejects_bad_receipts() {
    for valid in [true, false] {
        let t = tempfile::tempdir().unwrap();
        let expectations = t.path().join("candidate.jsonl");
        let artifacts = t.path().join("producer.jsonl");
        let output = t.path().join("candidate.json");
        let markdown = t.path().join("candidate.md");
        let mut row = artifact("_tool_pair_artifact");
        if !valid {
            row["producer"]["turns"][0]["events"][1]["data"]["success"] = json!(false);
        }
        std::fs::write(
            &expectations,
            format!("{}\n", case("assistant.stateful.tool_pairing")),
        )
        .unwrap();
        std::fs::write(&artifacts, format!("{row}\n")).unwrap();
        let (report, code) = golden_cli::candidate(Args {
            positional: vec!["candidate".into(), expectations.display().to_string()],
            options: BTreeMap::from([
                ("producer-artifacts".into(), artifacts.display().to_string()),
                ("output".into(), output.display().to_string()),
                ("markdown".into(), markdown.display().to_string()),
            ]),
            repeated: BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(output).unwrap()).unwrap(),
            report
        );
        assert!(markdown.is_file());
        assert_eq!(code, if valid { 0 } else { 1 }, "{report}");
        if valid {
            assert_eq!(report["evidence_scope"], "recorded_runtime_candidate");
            assert_eq!(report["suite_scope"], "partial");
            for (tier, status) in [
                ("fixture_contract", "provided_cases_verified"),
                ("runtime_artifact_adapter", "verified"),
                ("recorded_runtime_artifacts", "provided_cases_verified"),
                ("live_runtime_execution", "not_run"),
                ("real_provider_call", "not_run"),
            ] {
                assert_eq!(report["evidence_tiers"][tier], status, "{report}");
            }
        } else {
            assert_eq!(report["gate"]["status"], "fail");
            assert_eq!(report["observations"]["adapter"]["status"], "not_verified");
            for tier in ["runtime_artifact_adapter", "recorded_runtime_artifacts"] {
                assert_eq!(report["evidence_tiers"][tier], "not_verified");
            }
        }
    }
}
