//! Native regression orchestration: empty or missing test groups fail closed.
use crate::{Result, cli::Args};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};
struct Group {
    id: &'static str,
    phase: &'static str,
    label: &'static str,
    filter: &'static str,
}
const GROUPS: [Group; 4] = [
    Group {
        id: "runtime-v2-turn-contract",
        phase: "V2",
        label: "Runtime Thread/Turn/Item Contract",
        filter: "runtime_v2_turn_contract",
    },
    Group {
        id: "runtime-control-plane",
        phase: "V2",
        label: "Gateway Runtime Control Plane",
        filter: "runtime_control_plane",
    },
    Group {
        id: "runtime-capability-contract",
        phase: "V2",
        label: "Gateway Capability Boundary",
        filter: "runtime_capability_contract",
    },
    Group {
        id: "runtime-eval-contract",
        phase: "V2",
        label: "Gateway Eval And Observation Contract",
        filter: "runtime_eval_contract",
    },
];
pub fn summary() -> Value {
    let mut groups=GROUPS.iter().map(|g|json!({"id":g.id,"phase":g.phase,"label":g.label,"paths":[format!("rust/platform-native/crates/platform-gateway/tests/{}.rs",g.filter)]})).collect::<Vec<_>>();
    groups.push(json!({"id":"ahr-04-golden-gate","phase":"AHR-04","label":"Eval Golden Regression Gate","paths":["tests/fixtures/eval/golden/assistant_regression_v1.jsonl"]}));
    json!({"schema_version":"assistant-runtime-regression-gate/v1","groups":groups,"total_groups":groups.len()})
}
fn elapsed(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 100.).round() / 100.
}
fn tests_executed(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        line.trim()
            .strip_prefix("running ")
            .and_then(|v| v.split_whitespace().next())
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| n > 0)
    })
}
async fn run_group(group: &Group, root: &Path) -> Result<Value> {
    let start = Instant::now();
    let command = tokio::process::Command::new("cargo")
        .current_dir(root)
        .args([
            "test",
            "--manifest-path",
            "rust/platform-native/Cargo.toml",
            "-p",
            "ai-platform-gateway",
            "--test",
            group.filter,
            "--",
            "--nocapture",
        ])
        .kill_on_drop(true)
        .output();
    let (passed, code, line) = match tokio::time::timeout(Duration::from_secs(600), command).await {
        Err(_) => (false, -1, "TIMEOUT after 600s".to_string()),
        Ok(Err(e)) => (false, -1, format!("native test runner unavailable: {e}")),
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let ran = tests_executed(&stdout);
            let pass = output.status.success() && ran;
            let summary = if !ran {
                "required native regression group did not execute any tests".into()
            } else {
                stdout.trim().lines().last().unwrap_or("").to_string()
            };
            let summary = if !output.status.success() && stderr.contains("no test target named") {
                format!("missing native regression suite: {}", group.filter)
            } else {
                summary
            };
            (pass, output.status.code().unwrap_or(-1), summary)
        }
    };
    Ok(
        json!({"id":group.id,"phase":group.phase,"label":group.label,"passed":passed,"exit_code":code,"elapsed_seconds":elapsed(start),"summary_line":line}),
    )
}
fn golden(root: &Path) -> Result<Value> {
    let start = Instant::now();
    let path = root.join("tests/fixtures/eval/golden/assistant_regression_v1.jsonl");
    let obs_path = root.join("tests/fixtures/eval/observations/assistant_regression_v1.jsonl");
    let result = (|| -> Result<Value> {
        let (cases, hash) = crate::snapshot(path)?;
        let validation = crate::golden_validation::validate_cases(&cases);
        if validation["valid"] != true {
            return Err("golden validate failed".into());
        }
        let (rows, _) = crate::snapshot(obs_path)?;
        let obs = crate::observations(&rows)?;
        let validation = crate::golden_validation::validate_observations(&cases, &obs);
        if validation["valid"] != true {
            return Err("golden observation validation failed".into());
        }
        let metrics = crate::golden::evaluate_cases(&cases, Some(&obs));
        let mut gate = crate::golden_gate::apply_gate(&metrics, None, None, false, false);
        crate::golden_cli::hard_blockers(
            crate::golden_cli::suite_scope(&hash, &cases),
            &mut gate,
            &metrics,
        );
        Ok(gate)
    })();
    let (passed, code, line) = match result {
        Ok(gate) => {
            let pass = gate["status"] == "pass";
            (
                pass,
                if pass { 0 } else { 1 },
                format!(
                    "status={}, pass_rate={}, critical_pass_rate={}, trajectory_pass_rate={}",
                    gate["status"].as_str().unwrap_or("?"),
                    gate["metrics"]["pass_rate"],
                    gate["metrics"]["critical_pass_rate"],
                    gate["metrics"]["trajectory_pass_rate"]
                ),
            )
        }
        Err(e) => (false, 1, e.to_string()),
    };
    Ok(
        json!({"id":"ahr-04-golden-gate","phase":"AHR-04","label":"Eval Golden Regression Gate","passed":passed,"exit_code":code,"elapsed_seconds":elapsed(start),"summary_line":line}),
    )
}
pub async fn gate(root: &Path) -> Result<Value> {
    let mut results = vec![];
    for group in &GROUPS {
        results.push(run_group(group, root).await?);
    }
    results.push(golden(root)?);
    let mut phases = BTreeMap::<String, bool>::new();
    for r in &results {
        let phase = r["phase"].as_str().unwrap();
        *phases.entry(phase.into()).or_insert(true) &= r["passed"] == true;
    }
    let passed = results.iter().filter(|r| r["passed"] == true).count();
    let total = results
        .iter()
        .map(|r| r["elapsed_seconds"].as_f64().unwrap())
        .sum::<f64>();
    Ok(
        json!({"schema_version":"assistant-runtime-regression-gate/v1","timestamp":chrono::Utc::now().to_rfc3339(),"status":if passed==results.len(){"pass"}else{"fail"},"groups":results,"phases":phases,"summary":{"total_groups":results.len(),"passed":passed,"failed":results.len()-passed,"total_elapsed_seconds":(total*100.).round()/100.},"no_go_thresholds":{"all_groups_must_pass":true,"critical_phases":["AHR-01","AHR-02","AHR-03","AHR-04"]}}),
    )
}
pub async fn run(args: Args) -> Result<(Value, i32)> {
    if args.positional.len() != 1 {
        return Err("usage: assistant_runtime_regression {gate,summary}".into());
    }
    args.reject_unknown(&["output", "markdown", "no-write"])?;
    if args.positional[0] == "summary" {
        return Ok((summary(), 0));
    }
    if args.positional[0] != "gate" {
        return Err("unknown command".into());
    }
    let result = gate(&std::env::current_dir()?).await?;
    if !args.options.contains_key("no-write") {
        crate::write_report(
            args.get("output", "reports/assistant-runtime-regression/latest.json"),
            &result,
        )?;
        let mut md = format!(
            "# Assistant Runtime Regression Gate — {}\n\n**Status:** {}\n**Timestamp:** {}\n**Total elapsed:** {}s\n**Groups:** {}/{} passed\n\n## Phase Summary\n\n| Phase | Status |\n| --- | --- |\n",
            if result["status"] == "pass" {
                "PASS"
            } else {
                "FAIL"
            },
            result["status"].as_str().unwrap(),
            result["timestamp"].as_str().unwrap(),
            result["summary"]["total_elapsed_seconds"],
            result["summary"]["passed"],
            result["summary"]["total_groups"]
        );
        for (phase, pass) in result["phases"].as_object().unwrap() {
            md.push_str(&format!(
                "| {phase} | {} |\n",
                if pass == true { "pass" } else { "FAIL" }
            ));
        }
        md.push_str("\n## Group Details\n\n| Group | Phase | Passed | Elapsed | Summary |\n| --- | --- | --- | --- | --- |\n");
        for g in result["groups"].as_array().unwrap() {
            md.push_str(&format!(
                "| {} | {} | {} | {}s | {} |\n",
                g["id"].as_str().unwrap(),
                g["phase"].as_str().unwrap(),
                if g["passed"] == true { "pass" } else { "FAIL" },
                g["elapsed_seconds"],
                g["summary_line"].as_str().unwrap()
            ));
        }
        md.push_str("\n## No-Go Thresholds\n\n- All groups must pass; any failure is a no-go.\n- Critical phases: AHR-01, AHR-02, AHR-03, AHR-04.\n- No production data, secrets, or deployment involved.\n\n## Waiver Policy\n\nA failed group may only be waived when:\n1. The user explicitly waives the specific group.\n2. The failure root cause is documented in this report.\n3. Remaining evidence still proves the affected feature-oracle item.\n");
        let path =
            Path::new(args.get("markdown", "reports/assistant-runtime-regression/latest.md"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?
        }
        std::fs::write(path, md)?;
    }
    let code = if result["status"] == "pass" { 0 } else { 1 };
    Ok((result, code))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_test_success_is_failure() {
        assert!(!tests_executed(
            "running 0 tests\ntest result: ok. 0 passed"
        ));
        assert!(tests_executed("running 4 tests\ntest result: ok. 4 passed"));
    }
    #[test]
    fn unchanged_group_identity_and_threshold_count() {
        assert_eq!(summary()["total_groups"], 5);
    }
}
