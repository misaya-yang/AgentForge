//! Real CLI CI-selection and harness schema contract matrix. No CI/network execution.
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ci-parity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, p: &str, s: impl AsRef<[u8]>) {
        let p = self.0.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }
    fn json(&self, p: &str, v: &Value) {
        self.write(p, serde_json::to_vec(v).unwrap());
    }
    fn tool(&self, args: &[&str], pass: bool) -> Value {
        let o = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(args)
            .arg("--repo-root")
            .arg(&self.0)
            .output()
            .unwrap();
        assert_eq!(
            o.status.success(),
            pass,
            "stdout={} stderr={}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn workflow() -> Value {
    json!({"jobs":{"frontend":{"steps":[{"run":"make frontend-gate"}]},"architecture-gates":{"steps":[{"uses":"actions/checkout@v4","with":{"fetch-depth":0}},{"run":"exit \"$failed\""}]},"compose-and-harness":{"steps":[{"run":"make ci-gate-enforcement-selftest"}]},"agent-runtime-contracts":{"steps":[{"run":"exit \"$failed\""}]},"gate-enforcement":{"if":"always()","needs":["frontend"],"steps":[{"uses":"./.github/actions/setup-native"},{"run":"make ci-gate-enforcement\nmake affected-gates"}]},"release-ready":{"needs":["gate-enforcement"],"steps":[{"uses":"./.github/actions/setup-native"}]},"rust-changed-crate":{"steps":[{"uses":"./.github/actions/setup-native","with":{"capacity-profile":"hosted","nextest":"true"}}]}}})
}
fn gates() -> Value {
    json!({"affected_gates":{"make":"affected-gates","required_on":["change","release"],"ci_job":"gate-enforcement","evidence":"selector.json"},"frontend":{"make":"frontend-gate","required_on":["change","release"],"ci_job":"frontend","evidence":"frontend.log"},"manual_live":{"required_on":["manual"],"ci_job":"manual","evidence":"live.log"}})
}
fn selection() -> Value {
    json!({"result":"pass","changed_paths":["harness.yml","web/src/App.tsx"],"ungated_paths":[],"selected":{"affected_gates":{"ci_job":"gate-enforcement","matched_paths":["harness.yml"]},"frontend":{"ci_job":"frontend","matched_paths":["web/src/App.tsx"]}}})
}
fn enforce(g: Value, s: Value, j: Value, w: Value, pass: bool) -> Value {
    let f = Fixture::new();
    f.json("harness.yml", &json!({"gates":g}));
    f.json("selection.json", &s);
    f.json(".github/workflows/ci.yml", &w);
    f.tool(
        &[
            "ci-gate-enforcement",
            "--selection",
            f.0.join("selection.json").to_str().unwrap(),
            "--job-results-json",
            &j.to_string(),
        ],
        pass,
    )
}
#[test]
fn success_requires_upstream_result_and_local_selector() {
    let v = enforce(
        gates(),
        selection(),
        json!({"frontend":"success"}),
        workflow(),
        true,
    );
    assert_eq!(
        v["required_results"],
        json!({"frontend":{"gates":["frontend"],"result":"success"},"gate-enforcement":{"gates":["affected_gates"],"result":"success"}})
    );
}
#[test]
fn missing_failed_skipped_cancelled_and_malformed_fail_closed() {
    for jobs in [
        json!({}),
        json!({"frontend":"failure"}),
        json!({"frontend":"skipped"}),
        json!({"frontend":"cancelled"}),
        json!({"frontend":3}),
        json!({"frontend":{}}),
        json!([]),
    ] {
        let v = enforce(gates(), selection(), jobs, workflow(), false);
        assert!(!v["failures"].as_array().unwrap().is_empty());
    }
}
#[test]
fn normalize_accepts_github_needs_and_simple_result_maps() {
    for jobs in [
        json!({"frontend":"success"}),
        json!({"frontend":{"result":"success","outputs":{"ignored":"value"}}}),
    ] {
        enforce(gates(), selection(), jobs, workflow(), true);
    }
}
#[test]
fn manual_only_match_does_not_count_as_ci_coverage() {
    let s = json!({"result":"pass","changed_paths":["rust/new.rs"],"ungated_paths":[],"selected":{"manual_live":{"ci_job":"manual","matched_paths":["rust/new.rs"]}}});
    let v = enforce(gates(), s, json!({}), workflow(), false);
    assert_eq!(
        v["failures"],
        json!(["changed path has no change-required CI-wired gate: rust/new.rs"])
    );
}
#[test]
fn changed_path_must_appear_in_selected_gate_matches() {
    let mut s = selection();
    s["changed_paths"]
        .as_array_mut()
        .unwrap()
        .push(json!("web/src/Missing.tsx"));
    let v = enforce(gates(), s, json!({"frontend":"success"}), workflow(), false);
    assert_eq!(v["failures"].as_array().unwrap().len(), 2);
}
#[test]
fn selection_cannot_change_harness_job_or_invent_gate() {
    for unknown in [false, true] {
        let mut s = selection();
        if unknown {
            s["selected"]["invented"] = json!({"ci_job":"frontend","matched_paths":[]});
        } else {
            s["selected"]["frontend"]["ci_job"] = json!("fake-green-job");
        }
        enforce(
            gates(),
            s,
            json!({"frontend":"success","fake-green-job":"success"}),
            workflow(),
            false,
        );
    }
}
#[test]
fn selected_gate_requires_evidence_and_job() {
    for key in ["evidence", "ci_job"] {
        let mut g = gates();
        g["frontend"].as_object_mut().unwrap().remove(key);
        enforce(
            g,
            selection(),
            json!({"frontend":"success"}),
            workflow(),
            false,
        );
    }
}
#[test]
fn selected_change_gate_cannot_use_manual_or_current_job() {
    for job in ["manual", "gate-enforcement"] {
        let mut g = gates();
        let mut s = selection();
        g["frontend"]["ci_job"] = json!(job);
        s["selected"]["frontend"]["ci_job"] = json!(job);
        enforce(g, s, json!({"frontend":"success"}), workflow(), false);
    }
}
#[test]
fn final_ci_rejects_unselected_upstream_failure() {
    enforce(
        gates(),
        selection(),
        json!({"frontend":"success","other":"failure"}),
        workflow(),
        false,
    );
}
#[test]
fn workflow_rejects_missing_entrypoint_comment_suffix_and_swallowed_command() {
    for command in [
        "make frontend-gate-placeholder",
        "# make frontend-gate",
        "echo make frontend-gate",
        "make frontend-gate || true",
        "make -n frontend-gate",
        "make frontend-gate; true",
    ] {
        let mut w = workflow();
        w["jobs"]["frontend"]["steps"][0]["run"] = json!(command);
        enforce(
            gates(),
            selection(),
            json!({"frontend":"success"}),
            w,
            false,
        );
    }
}
#[test]
fn fetch_depth_belongs_to_architecture_checkout_step() {
    for w in [
        json!({"run":"echo checkout","with":{"fetch-depth":0}}),
        json!({"uses":"actions/checkout@v4","with":{"fetch-depth":1}}),
    ] {
        let mut flow = workflow();
        flow["jobs"]["architecture-gates"]["steps"][0] = w;
        flow["jobs"]["frontend"]["steps"]
            .as_array_mut()
            .unwrap()
            .push(json!({"uses":"actions/checkout@v4","with":{"fetch-depth":0}}));
        enforce(
            gates(),
            selection(),
            json!({"frontend":"success"}),
            flow,
            false,
        );
    }
}
#[test]
fn hosted_capacity_and_operational_tools_are_required_in_the_owning_jobs() {
    for broken in [
        "capacity",
        "nextest",
        "override",
        "final-tools",
        "release-tools",
    ] {
        let mut w = workflow();
        match broken {
            "capacity" => {
                w["jobs"]["rust-changed-crate"]["steps"][0]["with"]["capacity-profile"] =
                    json!("single")
            }
            "nextest" => {
                w["jobs"]["rust-changed-crate"]["steps"][0]["with"]["nextest"] = json!("false")
            }
            "override" => w["jobs"]["rust-changed-crate"]["env"] = json!({"CARGO_BUILD_JOBS":"32"}),
            "final-tools" => {
                w["jobs"]["gate-enforcement"]["steps"][0] = json!({"run":"echo setup-native"})
            }
            "release-tools" => w["jobs"]["release-ready"]["steps"] = json!([]),
            _ => unreachable!(),
        }
        let v = enforce(
            gates(),
            selection(),
            json!({"frontend":"success"}),
            w,
            false,
        );
        assert!(!v["failures"].as_array().unwrap().is_empty(), "{broken}");
    }
}
fn harness(gate: Value, run: &str) -> Fixture {
    let f = Fixture::new();
    f.write(
        "docker-compose.yml",
        fs::read(repository().join("docker-compose.yml")).unwrap(),
    );
    for doc in [
        "AGENTS.md",
        "docs/README.md",
        "docs/harness/README.md",
        "docs/harness/platform-architecture.md",
        "docs/harness/architecture.md",
        "docs/harness/commands.md",
        "docs/harness/workflow.md",
        "docs/harness/work-packages.md",
        "docs/harness/runtime-and-secrets.md",
        "docs/harness/integration-and-rollback.md",
        "docs/harness/repository-quality.md",
    ] {
        f.write(
            doc,
            format!(
                "Fixture\n\n**Last verified:** {}\n",
                chrono::Utc::now().date_naive()
            ),
        );
    }
    for doc in [
        "CONTRIBUTING.md",
        "DEPLOY.md",
        "docs/deployment/operator-guide.md",
        "database/authority/README.md",
        "deploy/agent-runtime-source/README.md",
        "docs/design/enterprise-performance-contract.md",
        "rust/platform-native/crates/platform-migration-authority/README.md",
        "rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/README.md",
    ] {
        f.write(
            doc,
            format!(
                "Fixture\n\n**Last verified:** {}\n",
                chrono::Utc::now().date_naive()
            ),
        );
    }
    f.write("Makefile", "unit-gate:\n\t@echo genuine\n");
    f.json("harness.yml",&json!({"required_docs":["AGENTS.md"],"budgets":{"AGENTS.md":10},"workspace":{"spec_roots":["web"],"spec_globs":["*.spec.ts"],"root_forbidden":["stray.tmp"]},"gates":{"unit":gate}}));
    f.json(
        ".github/workflows/ci.yml",
        &json!({"jobs":{"unit-job":{"steps":[{"run":run}]}}}),
    );
    f
}
fn gate() -> Value {
    json!({"make":"unit-gate","triggers":["rust/**"],"tier":"L1","required_on":["change","release"],"resource":"offline","skip":"never","timeout":10,"evidence":"tmp/unit.log","ci_job":"unit-job"})
}
#[test]
fn complete_gate_with_real_ci_entrypoint_passes() {
    let v = harness(gate(), "make unit-gate").tool(&["harness-check"], true);
    assert!(v["errors"].as_array().unwrap().is_empty());
}
#[test]
fn evidence_and_ci_job_mandatory() {
    for key in ["evidence", "ci_job"] {
        let mut g = gate();
        g.as_object_mut().unwrap().remove(key);
        harness(g, "make unit-gate").tool(&["harness-check"], false);
    }
}
#[test]
fn change_gate_cannot_hide_behind_manual_but_release_gate_can() {
    for resource in ["offline", "hosted-service"] {
        let mut g = gate();
        g["ci_job"] = json!("manual");
        g["resource"] = json!(resource);
        harness(g.clone(), "make unit-gate").tool(&["harness-check"], false);
        g["required_on"] = json!(["release", "manual"]);
        harness(g, "make unit-gate").tool(&["harness-check"], true);
    }
}
#[test]
fn harness_ci_job_must_execute_preserving_outcome() {
    for cmd in [
        "make unit-gate-placeholder",
        "# make unit-gate",
        "echo make unit-gate",
        "make unit-gate || true",
        "make -n unit-gate",
        "make unit-gate; true",
    ] {
        harness(gate(), cmd).tool(&["harness-check"], false);
    }
    let mut g = gate();
    g["ci_job"] = json!("absent");
    harness(g, "make unit-gate").tool(&["harness-check"], false);
}
#[test]
fn shell_gate_requires_every_preserving_command() {
    let mut g = gate();
    g.as_object_mut().unwrap().remove("make");
    g["shell"] = json!("platform-tools first && platform-tools second");
    harness(g.clone(), "platform-tools first\nplatform-tools second")
        .tool(&["harness-check"], true);
    for cmd in [
        "platform-tools first",
        "platform-tools first\n# platform-tools second",
        "platform-tools first\nplatform-tools second-suffix",
        "platform-tools first\nplatform-tools second || true",
    ] {
        harness(g.clone(), cmd).tool(&["harness-check"], false);
    }
}
fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
#[test]
fn checked_in_workflow_jobs_execute_the_gates_they_claim() {
    let output = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
        .args(["harness-check", "--repo-root"])
        .arg(repository())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn agent_runtime_contract_job_provisions_postgres() {
    let workflow: Value = serde_yaml::from_str(
        &fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap(),
    )
    .unwrap();
    let job = &workflow["jobs"]["agent-runtime-contracts"];
    assert_eq!(
        job["services"]["postgres"]["env"]["POSTGRES_DB"],
        "agent_runtime_contract_test"
    );
    let steps = job["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s["name"] == "Configure Agent Runtime contract database")
    );
    assert!(steps.iter().any(|s| s["run"].as_str().is_some_and(|r| {
        r.lines().any(|l| {
            l.trim()
                .starts_with("make agent-runtime-single-kernel-gate ")
                || l.trim() == "make agent-runtime-single-kernel-gate"
        })
    })));
}
#[test]
fn empty_git_diff_still_writes_selector_evidence() {
    let f = Fixture::new();
    f.json("harness.yml",&json!({"gates":{"harness":{"make":"harness-check","triggers":["harness.yml"],"tier":"L0","required_on":["change"],"resource":"offline","skip":"never","timeout":5,"evidence":"tmp/gate-evidence/harness.json","ci_job":"harness"}}}));
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&f.0)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let evidence = f.0.join("affected-gates.json");
    let value = f.tool(
        &[
            "affected-gates",
            "--base",
            "HEAD",
            "--evidence-out",
            evidence.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(value["result"], "pass");
    assert_eq!(value["changed_paths"], json!([]));
    assert_eq!(value["selected"], json!({}));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(evidence).unwrap()).unwrap(),
        value
    );
}
