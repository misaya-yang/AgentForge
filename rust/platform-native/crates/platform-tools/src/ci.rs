//! Final CI enforcement: a green unrelated job cannot stand in for a selected gate.
use crate::*;
use std::collections::BTreeSet;
fn scalar(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
fn evaluate(
    selection: &Value,
    gates: &serde_json::Map<String, Value>,
    jobs: &Value,
    current: &str,
) -> Value {
    let mut failures = vec![];
    let mut results = BTreeMap::<String, String>::new();
    if let Some(jobs) = jobs.as_object() {
        for (name, raw) in jobs {
            let status = raw.as_str().or_else(|| raw["result"].as_str());
            if let Some(status) = status.filter(|_| !name.is_empty()) {
                results.insert(name.clone(), status.into());
                if status != "success" {
                    failures.push(format!(
                        "upstream CI job {name} is {status}, expected success"
                    ));
                }
            } else {
                failures.push(format!("CI job {name} has no string result"));
            }
        }
    } else {
        failures.push("CI job results must be a JSON object".into());
    }
    if selection["result"] != "pass" {
        failures.push("affected-gates selector result must be pass".into());
    }
    if !selection["ungated_paths"]
        .as_array()
        .is_some_and(|v| v.is_empty())
    {
        failures.push("affected-gates evidence must contain empty ungated_paths".into());
    }
    let changed = selection["changed_paths"]
        .as_array()
        .filter(|a| a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())))
        .cloned()
        .unwrap_or_else(|| {
            failures.push("invalid changed_paths".into());
            vec![]
        });
    let mut matched = BTreeSet::new();
    let mut covered = BTreeSet::new();
    let mut required = BTreeMap::<String, Vec<String>>::new();
    if let Some(selected) = selection["selected"].as_object() {
        for (name, detail) in selected {
            let Some(contract) = gates.get(name) else {
                failures.push(format!("unknown selected gate {name}"));
                continue;
            };
            let Some(paths) = detail["matched_paths"]
                .as_array()
                .filter(|a| a.iter().all(Value::is_string))
            else {
                failures.push(format!("invalid matched_paths for {name}"));
                continue;
            };
            for path in paths {
                matched.insert(scalar(path).to_owned());
            }
            let job = scalar(&contract["ci_job"]);
            let change = contract["required_on"]
                .as_array()
                .is_some_and(|v| v.contains(&json!("change")));
            if scalar(&contract["evidence"]).is_empty() {
                failures.push(format!("selected gate {name} has no explicit evidence"));
            }
            if job.is_empty() {
                failures.push(format!("selected gate {name} has no ci_job"));
            }
            if job == "manual" && change {
                failures.push(format!(
                    "selected change-required gate {name} cannot be manual"
                ));
            }
            if scalar(&detail["ci_job"]) != job {
                failures.push(format!("selector ci_job drift for {name}"));
            }
            if change && !job.is_empty() && job != "manual" {
                for path in paths {
                    covered.insert(scalar(path).to_owned());
                }
                required.entry(job.into()).or_default().push(name.clone());
            }
        }
    } else {
        failures.push("selected gate map missing".into());
    }
    for path in &changed {
        let path = scalar(path);
        if !matched.contains(path) {
            failures.push(format!(
                "changed path absent from selected gate matches: {path}"
            ));
        }
        if !covered.contains(path) {
            failures.push(format!(
                "changed path has no change-required CI-wired gate: {path}"
            ));
        }
    }
    let mut required_results = BTreeMap::new();
    for (job, mut names) in required {
        names.sort();
        let status = if job == current {
            if names == ["affected_gates"] {
                "success"
            } else {
                failures.push("current final job may satisfy only affected_gates".into());
                "invalid-local-mapping"
            }
        } else {
            let status = results.get(&job).map(String::as_str).unwrap_or("missing");
            if status == "missing" {
                failures.push(format!("required CI job {job} has no result"));
            }
            status
        };
        required_results.insert(job.clone(), json!({"gates":names,"result":status}));
    }
    json!({"gate":"ci-gate-enforcement","tier":"L0","changed_paths":changed,"required_results":required_results,"result":if failures.is_empty(){"pass"}else{"fail"},"failures":failures})
}
fn wiring(root: &Path, gates: &serde_json::Map<String, Value>) -> Result<Vec<String>> {
    let workflow: Value = serde_yaml::from_str(&std::fs::read_to_string(
        root.join(".github/workflows/ci.yml"),
    )?)?;
    let jobs = workflow["jobs"]
        .as_object()
        .ok_or("workflow jobs missing")?;
    let mut failures = vec![];
    let mut wired = BTreeSet::new();
    let scripts = |job: &Value| {
        job["steps"]
            .as_array()
            .map(|s| {
                s.iter()
                    .filter_map(|s| s["run"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    };
    for (name, fields) in gates {
        let job = scalar(&fields["ci_job"]);
        if job.is_empty() {
            failures.push(format!("gate {name} has no explicit ci_job"));
            continue;
        }
        if job == "manual" {
            continue;
        }
        wired.insert(job);
        let Some(body) = jobs.get(job) else {
            failures.push(format!("CI workflow missing gate {name} job {job}"));
            continue;
        };
        let run = scripts(body);
        let executes = if fields.get("make").is_some() {
            harness::runs_make(&run, scalar(&fields["make"]))
        } else {
            let commands: Vec<_> = scalar(&fields["shell"])
                .split("&&")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            !commands.is_empty() && commands.iter().all(|c| harness::runs_shell(&run, c))
        };
        if !executes {
            failures.push(format!(
                "CI job {job} does not execute gate {name} entrypoint"
            ));
        }
    }
    let needs = |job: &str, need: &str| {
        jobs.get(job).is_some_and(|v| {
            v["needs"]
                .as_array()
                .is_some_and(|a| a.contains(&json!(need)))
                || v["needs"] == need
        })
    };
    for job in wired {
        if job != "gate-enforcement" && !needs("gate-enforcement", job) {
            failures.push(format!("gate-enforcement does not require {job}"));
        }
    }
    if !jobs
        .get("architecture-gates")
        .and_then(|j| j["steps"].as_array())
        .is_some_and(|steps| {
            steps.iter().any(|s| {
                scalar(&s["uses"]).starts_with("actions/checkout@")
                    && (s["with"]["fetch-depth"] == 0 || s["with"]["fetch-depth"] == "0")
            })
        })
    {
        failures.push("architecture-gates checkout must fetch-depth 0".into());
    }
    for (job, command) in [
        ("compose-and-harness", "make ci-gate-enforcement-selftest"),
        ("architecture-gates", "exit \"$failed\""),
        ("agent-runtime-contracts", "exit \"$failed\""),
        ("gate-enforcement", "make ci-gate-enforcement"),
    ] {
        if !jobs.get(job).is_some_and(|j| scripts(j).contains(command)) {
            failures.push(format!("CI job {job} missing {command}"));
        }
    }
    if !jobs
        .get("gate-enforcement")
        .is_some_and(|j| scalar(&j["if"]).contains("always()"))
    {
        failures.push("gate-enforcement must run always()".into());
    }
    if !needs("release-ready", "gate-enforcement") {
        failures.push("release-ready must need gate-enforcement".into());
    }
    let setup = |job: &str, hosted: bool| {
        jobs.get(job).is_some_and(|j| {
            j["steps"].as_array().is_some_and(|steps| {
                steps.iter().any(|s| {
                    s["uses"] == "./.github/actions/setup-native"
                        && (!hosted
                            || (s["with"]["capacity-profile"] == "hosted"
                                && s["with"]["nextest"] == "true"))
                })
            })
        })
    };
    if !setup("rust-changed-crate", true)
        || jobs
            .get("rust-changed-crate")
            .is_some_and(|j| j["env"].get("CARGO_BUILD_JOBS").is_some())
    {
        failures.push("rust-changed-crate must use bounded hosted setup-native with nextest and no job-level Cargo job override".into());
    }
    for job in ["gate-enforcement", "release-ready"] {
        if !setup(job, false) {
            failures.push(format!(
                "CI job {job} must install native operational tools"
            ));
        }
    }
    Ok(failures)
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root();
    let outcome=(||->Result<Value>{let harness=args.values.get("--harness").map(PathBuf::from).unwrap_or_else(||root.join("harness.yml"));let config:Value=serde_yaml::from_str(&std::fs::read_to_string(harness)?)?;let gates=config["gates"].as_object().ok_or("gates missing")?;
        let mut value=if args.flag("--selftest"){selftest()?;json!({"gate":"ci-gate-enforcement","tier":"L0","changed_paths":[],"required_results":{},"failures":[],"result":"pass"})}else{
            let selection=object(&args.values.get("--selection").map(PathBuf::from).unwrap_or_else(||root.join("tmp/gate-evidence/affected-gates.json")))?;
            let raw=args.values.get("--job-results-json").cloned().or_else(||std::env::var("CI_JOB_RESULTS_JSON").ok()).ok_or("CI_JOB_RESULTS_JSON required")?;let jobs:Value=serde_json::from_str(&raw)?;
            evaluate(&selection,gates,&jobs,args.values.get("--current-job").map(String::as_str).unwrap_or("gate-enforcement"))};
        value["failures"].as_array_mut().unwrap().extend(wiring(&root,gates)?.into_iter().map(Value::String));value["result"]=json!(if value["failures"].as_array().unwrap().is_empty(){"pass"}else{"fail"});Ok(value)
    })().unwrap_or_else(|e|json!({"gate":"ci-gate-enforcement","tier":"L0","changed_paths":[],"required_results":{},"failures":[format!("gate enforcement input error: {e}")],"result":"fail"}));
    let path = args
        .values
        .get("--evidence-out")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("tmp/gate-evidence/ci-gate-enforcement.json"));
    write_json(&path, &outcome)?;
    Ok(outcome)
}
fn selftest() -> Result<()> {
    let gates = json!({"frontend":{"required_on":["change"],"ci_job":"frontend","evidence":"log"}});
    let selection = json!({"result":"pass","ungated_paths":[],"changed_paths":["web/x"],"selected":{"frontend":{"ci_job":"frontend","matched_paths":["web/x"]}}});
    for (jobs, pass) in [
        (json!({"frontend":"success"}), true),
        (json!({}), false),
        (json!({"frontend":"skipped"}), false),
        (json!({"frontend":"success","unrelated":"cancelled"}), false),
    ] {
        require(
            (evaluate(
                &selection,
                gates.as_object().unwrap(),
                &jobs,
                "gate-enforcement",
            )["result"]
                == "pass")
                == pass,
            "CI enforcement selftest failed",
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_results_fail_closed() {
        selftest().unwrap();
    }
    #[test]
    fn local_result_cannot_impersonate_other_gate() {
        let gates =
            json!({"fake":{"required_on":["change"],"ci_job":"gate-enforcement","evidence":"log"}});
        let selection = json!({"result":"pass","ungated_paths":[],"changed_paths":["x"],"selected":{"fake":{"ci_job":"gate-enforcement","matched_paths":["x"]}}});
        assert_eq!(
            evaluate(
                &selection,
                gates.as_object().unwrap(),
                &json!({}),
                "gate-enforcement"
            )["result"],
            "fail"
        );
    }
}
