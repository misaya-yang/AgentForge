use crate::{
    Result,
    canonical::{digest, sha256},
    cli::Args,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub const CANONICAL_HASH: &str = "cb34ed31d09f0752d18000df6f39eaa28e67f9ae4f9e630421c847a20a8f75ee";
const IDS: [&str; 25] = [
    "assistant.refund_policy.basic",
    "assistant.billing.invoice",
    "assistant.safety.secret_redaction",
    "assistant.tool.failure_recovery",
    "assistant.rag.grounded_answer",
    "assistant.multi_turn.context",
    "assistant.latency.short_answer",
    "assistant.compliance.refusal",
    "assistant.langgraph.proxy_status",
    "assistant.export.redaction",
    "assistant.runtime.approval_denial",
    "assistant.runtime.approval_argument_mismatch",
    "assistant.runtime.sandbox_unavailable",
    "assistant.runtime.interrupted_memory_skip",
    "assistant.runtime.stop_resume",
    "assistant.runtime.max_iterations",
    "assistant.runtime.policy_bypass",
    "assistant.runtime.repeated_unknown_side_effect",
    "assistant.stateful.plan_retention",
    "assistant.stateful.tool_pairing",
    "assistant.stateful.budget_termination",
    "assistant.stateful.hitl_pause_resume",
    "assistant.stateful.compaction_retention",
    "assistant.security.prompt_injection",
    "assistant.security.tenant_isolation",
];
pub fn suite_scope(hash: &str, cases: &[Value]) -> &'static str {
    if hash == CANONICAL_HASH {
        return "canonical";
    }
    let ids = cases
        .iter()
        .filter_map(|c| c["case_id"].as_str())
        .collect::<BTreeSet<_>>();
    if !ids.is_empty() && ids.len() < IDS.len() && ids.iter().all(|id| IDS.contains(id)) {
        "partial"
    } else {
        "custom"
    }
}
pub fn hard_blockers(scope: &str, gate: &mut Value, metrics: &Value) {
    if scope != "canonical" {
        return;
    }
    let results = metrics["cases"].as_array().unwrap();
    for (name, passed_name, ids, stateful, message) in [
        (
            "required_hard_blockers",
            "hard_blockers_passed",
            &IDS[16..18],
            false,
            "required hard blockers missing or failing",
        ),
        (
            "required_stateful_cases",
            "stateful_cases_passed",
            &IDS[18..25],
            true,
            "required stateful cases missing or failing",
        ),
    ] {
        let failed = ids
            .iter()
            .filter(|id| {
                !results.iter().any(|r| {
                    r["case_id"] == **id
                        && r["passed"] == true
                        && (!stateful || r["stateful_pass"] == true)
                })
            })
            .copied()
            .collect::<Vec<_>>();
        gate[name] = json!(ids);
        gate[passed_name] = json!(failed.is_empty());
        if !failed.is_empty() {
            gate["status"] = json!("fail");
            gate["failures"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("{message}: {}", failed.join(", "))));
        }
    }
}
fn measurements(obs: Option<&BTreeMap<String, Value>>, keys: &[&str]) -> Vec<Value> {
    obs.into_iter()
        .flat_map(|m| m.iter())
        .filter(|(_, r)| keys.iter().any(|k| !r[k].is_null()))
        .map(|(id, r)| {
            let mut v = json!({"case_id":id});
            for k in keys {
                if !r[k].is_null() {
                    v[*k] = r[k].clone();
                }
            }
            v
        })
        .collect()
}
pub fn provenance(
    path: &str,
    hash: &str,
    obs_path: Option<&str>,
    obs_hash: Option<&str>,
    cases: &[Value],
    obs: Option<&BTreeMap<String, Value>>,
) -> Value {
    let mut components = json!({"rust/platform-native/crates/platform-eval/src/golden.rs":sha256(include_bytes!("golden.rs")),"rust/platform-native/crates/platform-eval/src/golden_gate.rs":sha256(include_bytes!("golden_gate.rs")),"rust/platform-native/crates/platform-eval/src/golden_validation.rs":sha256(include_bytes!("golden_validation.rs")),"rust/platform-native/crates/platform-eval/src/golden_stateful.rs":sha256(include_bytes!("golden_stateful.rs"))});
    // Preserve the source report contract even when serde_json preserve_order is unified.
    components
        .as_object_mut()
        .expect("grader component object")
        .sort_keys();
    let trace_ids = obs
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|r| r["trace_id"].as_str().filter(|s| !s.is_empty()))
        .collect::<BTreeSet<_>>();
    let thresholds = cases
        .iter()
        .flat_map(|c| {
            c["assertions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| {
                    ["latency_ms_lt", "total_tokens_lt", "cost_cents_lt"]
                        .contains(&a["type"].as_str().unwrap_or(""))
                })
                .map(|a| json!({"case_id":c["case_id"],"type":a["type"],"value":a["value"]}))
        })
        .collect::<Vec<_>>();
    let recoveries = obs
        .map(|m| {
            m.iter()
                .filter(|(_, r)| {
                    !r["recovery_action"].is_null()
                        || [
                            "approval_denied",
                            "policy_denied",
                            "side_effect_unknown",
                            "interrupted",
                            "stopped",
                        ]
                        .contains(&r["exit_reason"].as_str().unwrap_or(""))
                })
                .map(|(id, r)| (id.clone(), r.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    json!({"dataset":{"version":Path::new(path).file_stem().and_then(|s|s.to_str()),"sha256":hash,"source":path},"observations":{"version":obs_path.and_then(|p|Path::new(p).file_stem().and_then(|s|s.to_str())).unwrap_or("inline"),"sha256":obs_hash,"source":obs_path},"grader":{"id":"assistant_deterministic_contract","version":"v1","sha256":digest(&components),"components":components},"trial":{"id":format!("offline-{}",&obs_hash.unwrap_or(hash)[..16]),"repetitions_per_case":1,"seed":"not_recorded","observation_timestamp":"not_recorded","report_generated_at":chrono::Utc::now().to_rfc3339(),"command":"eval_golden gate"},"trace":{"receipt":if trace_ids.is_empty(){"not_recorded"}else{"recorded"},"trace_ids":trace_ids,"span_evidence_cases":obs.into_iter().flat_map(|m|m.values()).filter(|r|r["span_kinds"].as_array().is_some_and(|a|!a.is_empty())).count(),"observation_sha256":obs_hash},"coverage":{"latency":measurements(obs,&["total_latency_ms"]),"tokens":measurements(obs,&["total_tokens","input_tokens","output_tokens"]),"cache":measurements(obs,&["cache_hit","cached_tokens","cache_read_input_tokens"]),"recovery":measurements(Some(&recoveries),&["exit_reason","recovery_action","blind_replay","dispatch_count","second_dispatch_count"]),"numeric_thresholds":thresholds},"evidence_tiers":{"offline":"verified","mock":"not_run","local_live":"not_run","real_provider":"not_run"}})
}
pub fn write_reports(report: &Value, output: &str, markdown: &str) -> Result<()> {
    crate::write_report(output, report)?;
    let mut lines = vec![
        "# Eval Regression Gate".to_string(),
        "".into(),
        format!(
            "Status: `{}`",
            report["gate"]["status"].as_str().unwrap_or("unknown")
        ),
        format!(
            "Evidence scope: `{}`",
            report["evidence_scope"].as_str().unwrap_or("unknown")
        ),
        format!(
            "Joined observations: `{}`",
            report["observations"]["joined_count"]
        ),
        format!("Cases: `{}`", report["summary"]["case_count"]),
        format!("Overall score: `{}`", report["metrics"]["overall_score"]),
        format!(
            "Trajectory pass rate: `{}`",
            report["metrics"]["trajectory_pass_rate"]
        ),
        format!(
            "Critical pass rate: `{}`",
            report["metrics"]["critical_pass_rate"]
        ),
        "".into(),
        "## Failures".into(),
        "".into(),
    ];
    let failures = report["gate"]["failures"].as_array().unwrap();
    if failures.is_empty() {
        lines.push("- None".into());
    } else {
        lines.extend(
            failures
                .iter()
                .map(|f| format!("- {}", f.as_str().unwrap_or(""))),
        );
    }
    let path = Path::new(markdown);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?
    }
    std::fs::write(path, format!("{}\n", lines.join("\n")))?;
    Ok(())
}
pub fn run(args: Args) -> Result<(Value, i32)> {
    let command = args.positional.first().map(String::as_str).unwrap_or("");
    if args.positional.len() != 2 {
        return Err("usage: eval_golden {validate,summarize,gate,import,export} PATH".into());
    }
    let path = &args.positional[1];
    if command == "export" {
        return Ok((
            json!({"status":"ready","mode":"api-required","message":"Use GET /api/v1/eval/datasets/{dataset_id}/examples:export and write the response examples as JSONL.","dataset_id":path}),
            0,
        ));
    }
    args.reject_unknown(&["observations", "baseline-report", "output", "markdown"])?;
    let (cases, hash) = crate::snapshot(path)?;
    let validation = crate::golden_validation::validate_cases(&cases);
    if command == "validate" || validation["valid"] != true {
        let code = if validation["valid"] == true { 0 } else { 1 };
        return Ok((validation, code));
    }
    if command == "summarize" {
        return Ok((crate::golden::summarize(&cases), 0));
    }
    if command == "import" {
        return Ok((
            json!({"status":"ready","mode":"api-required","message":"Use POST /api/v1/eval/datasets/{dataset_id}/examples:import with this JSONL payload.","case_count":cases.len()}),
            0,
        ));
    }
    if command != "gate" {
        return Err("unsupported eval_golden command".into());
    }
    let obs_path = args.options.get("observations");
    let mut obs = None;
    let mut obs_hash = None;
    let mut observation_summary = json!({"source":null,"case_count":cases.len(),"observation_count":0,"joined_count":0,"errors":[]});
    if let Some(path) = obs_path {
        let (rows, hash) = crate::snapshot(path)?;
        let observations = crate::observations(&rows)?;
        observation_summary =
            crate::golden_validation::validate_observations(&cases, &observations);
        observation_summary["source"] = json!(path);
        if observation_summary["valid"] != true {
            return Ok((observation_summary, 1));
        }
        obs = Some(observations);
        obs_hash = Some(hash);
    }
    let baseline = if let Some(path) = args.options.get("baseline-report") {
        let payload: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if !payload.is_object() {
            return Err("Baseline report must be a JSON object".into());
        }
        let metrics = if payload["metrics"].is_object() {
            payload["metrics"].clone()
        } else {
            payload["gate"]["metrics"].clone()
        };
        if !metrics.is_object() {
            return Err("Baseline report must contain metrics".into());
        }
        Some(json!({"source":path,"metrics":metrics}))
    } else {
        None
    };
    let metrics = crate::golden::evaluate_cases(&cases, obs.as_ref());
    let mut gate = crate::golden_gate::apply_gate(
        &metrics,
        None,
        baseline.as_ref().map(|b| &b["metrics"]),
        false,
        false,
    );
    let scope = suite_scope(&hash, &cases);
    hard_blockers(scope, &mut gate, &metrics);
    let mut report = json!({"schema_version":"eval-regression-gate-v1","source":path,"suite_scope":scope,"evidence_scope":if obs.is_some(){"recorded_offline_observation"}else{"legacy_inline_replay"},"observations":observation_summary,"summary":crate::golden::summarize(&cases),"metrics":metrics,"gate":gate,"provenance":provenance(path,&hash,obs_path.map(String::as_str),obs_hash.as_deref(),&cases,obs.as_ref())});
    if let Some(b) = baseline {
        report["baseline"] = b;
    }
    write_reports(
        &report,
        args.get("output", "tmp/eval-regression/latest.json"),
        args.get("markdown", "tmp/eval-regression/latest.md"),
    )?;
    let code = if report["gate"]["status"] == "pass" {
        0
    } else {
        1
    };
    Ok((report, code))
}
pub fn candidate(args: Args) -> Result<(Value, i32)> {
    if args.positional.len() != 2 {
        return Err("candidate requires expectations path".into());
    }
    args.reject_unknown(&[
        "producer-artifacts",
        "baseline-report",
        "output",
        "markdown",
    ])?;
    let path = &args.positional[1];
    let (cases, hash) = crate::snapshot(path)?;
    let validation = crate::golden_validation::validate_cases(&cases);
    if validation["valid"] != true {
        return Ok((validation, 1));
    }
    let producer = args.required("producer-artifacts")?;
    let rows = crate::producer::load_producer_artifacts(producer)?;
    let observations = crate::producer::adapt_producer_artifacts(&rows)?;
    let validation = crate::golden_validation::validate_observations(&cases, &observations);
    let adapter = crate::producer::summarize_adapter_evidence(&observations);
    let mut summary = validation;
    summary["source"] = json!(producer);
    summary["adapter"] = adapter.clone();
    if summary["valid"] != true {
        return Ok((summary, 1));
    }
    let baseline = if let Some(path) = args.options.get("baseline-report") {
        let payload: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if !payload.is_object() {
            return Err("Baseline report must be a JSON object".into());
        }
        let metrics = if payload["metrics"].is_object() {
            payload["metrics"].clone()
        } else {
            payload["gate"]["metrics"].clone()
        };
        if !metrics.is_object() {
            return Err("Baseline report must contain metrics".into());
        }
        Some(json!({"source":path,"metrics":metrics}))
    } else {
        None
    };
    let metrics = crate::golden::evaluate_cases(&cases, Some(&observations));
    let mut gate = crate::golden_gate::apply_gate(
        &metrics,
        None,
        baseline.as_ref().map(|b| &b["metrics"]),
        true,
        true,
    );
    let scope = suite_scope(&hash, &cases);
    hard_blockers(scope, &mut gate, &metrics);
    let adapter_verified = adapter["status"] == "verified";
    let verified = adapter_verified && gate["status"] == "pass";
    let tier = if scope == "canonical" {
        "verified"
    } else {
        "provided_cases_verified"
    };
    let mut report = json!({"schema_version":"eval-recorded-candidate-gate/v1","source":path,"suite_scope":scope,"evidence_scope":"recorded_runtime_candidate","observations":summary,"summary":crate::golden::summarize(&cases),"metrics":metrics,"gate":gate,"provenance":{"dataset_sha256":hash,"canonical_dataset_sha256":CANONICAL_HASH,"suite_scope":scope,"producer_artifacts_sha256":sha256(&std::fs::read(producer)?),"source_adapter":"canonical_assistant_producer","command":"eval_golden candidate"},"evidence_tiers":{"fixture_contract":tier,"runtime_artifact_adapter":if adapter_verified{"verified"}else{"not_verified"},"recorded_runtime_artifacts":if verified{tier}else{"not_verified"},"live_runtime_execution":"not_run","real_provider_call":"not_run"}});
    if let Some(baseline) = baseline {
        report["baseline"] = baseline;
    }
    write_reports(
        &report,
        args.get("output", "tmp/eval-candidate/latest.json"),
        args.get("markdown", "tmp/eval-candidate/latest.md"),
    )?;
    let code = if report["gate"]["status"] == "pass" {
        0
    } else {
        1
    };
    Ok((report, code))
}
