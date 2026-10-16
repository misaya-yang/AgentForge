//! Offline golden CLI/report/provenance contracts. No model, network, or database calls.
use ai_platform_eval::{
    canonical::{digest, sha256},
    cli::Args,
    golden_cli, golden_validation,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn fixture() -> PathBuf {
    root().join("tests/fixtures/eval/golden/assistant_regression_v1.jsonl")
}
fn observations() -> PathBuf {
    root().join("tests/fixtures/eval/observations/assistant_regression_v1.jsonl")
}
fn args(t: &Path, cases: &Path, obs: &Path) -> Args {
    Args {
        positional: vec!["gate".into(), cases.to_str().unwrap().into()],
        options: BTreeMap::from([
            ("observations".into(), obs.to_str().unwrap().into()),
            (
                "output".into(),
                t.join("latest.json").to_str().unwrap().into(),
            ),
            (
                "markdown".into(),
                t.join("latest.md").to_str().unwrap().into(),
            ),
        ]),
        repeated: BTreeMap::new(),
    }
}
#[test]
fn frozen_fixture_has_all_seed_coverage_and_no_embedded_replay() {
    let (cases, _) = ai_platform_eval::snapshot(fixture()).unwrap();
    let v = golden_validation::validate_cases(&cases);
    assert_eq!(v["valid"], true);
    assert!(v["case_count"].as_u64().unwrap() >= 25);
    assert!(cases.iter().any(|c| c["metadata"]["critical"] == true));
    assert!(
        cases.iter().all(
            |c| c.get("expected_trajectory").is_some() && c["metadata"].get("replay").is_none()
        )
    );
    for id in [
        "assistant.runtime.approval_denial",
        "assistant.runtime.approval_argument_mismatch",
        "assistant.runtime.sandbox_unavailable",
        "assistant.runtime.interrupted_memory_skip",
        "assistant.runtime.stop_resume",
        "assistant.runtime.max_iterations",
        "assistant.runtime.policy_bypass",
        "assistant.runtime.repeated_unknown_side_effect",
        "assistant.tool.failure_recovery",
        "assistant.export.redaction",
        "assistant.stateful.plan_retention",
        "assistant.stateful.tool_pairing",
        "assistant.stateful.budget_termination",
        "assistant.stateful.hitl_pause_resume",
        "assistant.stateful.compaction_retention",
        "assistant.security.prompt_injection",
        "assistant.security.tenant_isolation",
    ] {
        assert!(cases.iter().any(|c| c["case_id"] == id), "{id}");
    }
    let (rows, _) = ai_platform_eval::snapshot(observations()).unwrap();
    let obs = ai_platform_eval::observations(&rows).unwrap();
    let v = golden_validation::validate_observations(&cases, &obs);
    assert_eq!(v["valid"], true);
    assert_eq!(v["joined_count"], 25);
    assert_eq!(obs.len(), 25);
}
#[test]
fn offline_gate_report_preserves_evidence_scope_and_provenance() {
    let t = tempfile::tempdir().unwrap();
    let (payload, code) = golden_cli::run(args(t.path(), &fixture(), &observations())).unwrap();
    assert_eq!(code, 0, "{payload}");
    let persisted: Value =
        serde_json::from_slice(&std::fs::read(t.path().join("latest.json")).unwrap()).unwrap();
    assert_eq!(persisted, payload);
    assert_eq!(payload["gate"]["status"], "pass");
    assert_eq!(payload["suite_scope"], "canonical");
    assert!(payload["metrics"]["overall_score"].as_f64().unwrap() >= 0.85);
    assert_eq!(payload["evidence_scope"], "recorded_offline_observation");
    assert_eq!(payload["observations"]["joined_count"], 25);
    assert_eq!(payload["gate"]["hard_blockers_passed"], true);
    assert_eq!(
        payload["gate"]["required_hard_blockers"],
        json!([
            "assistant.runtime.policy_bypass",
            "assistant.runtime.repeated_unknown_side_effect"
        ])
    );
    assert_eq!(payload["gate"]["stateful_cases_passed"], true);
    assert_eq!(payload["metrics"]["stateful_case_count"], 7);
    assert_eq!(payload["metrics"]["stateful_pass_rate"], 1.);
    let p = &payload["provenance"];
    for key in ["dataset", "observations", "grader"] {
        assert_eq!(p[key]["sha256"].as_str().unwrap().len(), 64);
    }
    assert_eq!(p["grader"]["id"], "assistant_deterministic_contract");
    assert_eq!(p["trial"]["repetitions_per_case"], 1);
    assert_eq!(p["trial"]["seed"], "not_recorded");
    assert_eq!(p["trace"]["receipt"], "not_recorded");
    assert_eq!(
        p["coverage"]["latency"],
        json!([{"case_id":"assistant.latency.short_answer","total_latency_ms":900}])
    );
    assert_eq!(p["coverage"]["tokens"], json!([]));
    assert_eq!(p["coverage"]["cache"], json!([]));
    let recovery = p["coverage"]["recovery"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["case_id"] == "assistant.runtime.repeated_unknown_side_effect")
        .unwrap();
    assert_eq!(recovery["blind_replay"], false);
    assert_eq!(recovery["second_dispatch_count"], 0);
    assert_eq!(p["evidence_tiers"]["real_provider"], "not_run");
    assert!(
        std::fs::read_to_string(t.path().join("latest.md"))
            .unwrap()
            .contains("Eval Regression Gate")
    );
    let components = p["grader"]["components"].as_object().unwrap();
    let paths = [
        "rust/platform-native/crates/platform-eval/src/golden.rs",
        "rust/platform-native/crates/platform-eval/src/golden_gate.rs",
        "rust/platform-native/crates/platform-eval/src/golden_stateful.rs",
        "rust/platform-native/crates/platform-eval/src/golden_validation.rs",
    ];
    assert_eq!(
        components.keys().map(String::as_str).collect::<Vec<_>>(),
        paths
    );
    for path in paths {
        assert_eq!(
            components[path],
            sha256(&std::fs::read(root().join(path)).unwrap())
        );
    }
    assert_eq!(p["grader"]["sha256"], digest(&p["grader"]["components"]));
    assert_eq!(p["grader"]["version"], "v1");
}
#[test]
fn canonical_filename_cannot_grant_hard_blocker_trust() {
    let t = tempfile::tempdir().unwrap();
    let c = t.path().join("assistant_regression_v1.jsonl");
    let o = t.path().join("observations.jsonl");
    let case = json!({"case_id":"assistant.runtime.test","split":"regression","input":{"input_preview":"test"},"expected_output":{"contains":"ok"},"expected_trajectory":{"required_span_kinds":[],"runtime":{}},"assertions":[{"type":"output_contains","value":"ok"}],"metadata":{"critical":true}});
    std::fs::write(&c, format!("{case}\n")).unwrap();
    std::fs::write(&o,format!("{}\n",json!({"case_id":"assistant.runtime.test","replay":{"status":"succeeded","output_preview":"ok","span_kinds":[]}}))).unwrap();
    let (p, code) = golden_cli::run(args(t.path(), &c, &o)).unwrap();
    assert_eq!(code, 0);
    assert_eq!(p["suite_scope"], "custom");
    assert!(p["gate"].get("hard_blockers_passed").is_none());
    assert!(p["gate"].get("stateful_cases_passed").is_none());
}
#[test]
fn out_of_range_baseline_report_rejected() {
    let t = tempfile::tempdir().unwrap();
    let b = t.path().join("baseline.json");
    std::fs::write(&b, r#"{"metrics":{"overall_score":1.03}}"#).unwrap();
    let mut a = args(t.path(), &fixture(), &observations());
    a.options
        .insert("baseline-report".into(), b.to_str().unwrap().into());
    let (p, code) = golden_cli::run(a).unwrap();
    assert_eq!(code, 1);
    assert_eq!(p["baseline"]["source"], b.to_str().unwrap());
    assert!(p["gate"]["failures"].as_array().unwrap().iter().any(|f| {
        f.as_str()
            .unwrap()
            .contains("baseline overall_score must be a finite number in [0, 1]")
    }));
}
