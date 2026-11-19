//! Source-case ports for topology modes, immutable LOC, and native hygiene gates.
//! Executes the real tool binary; fixture Git repositories never touch the source repository.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("tools-parity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, p: &str, text: impl AsRef<[u8]>) {
        let p = self.0.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn json(&self, p: &str, v: &Value) {
        self.write(p, serde_json::to_vec_pretty(v).unwrap());
    }
    fn tool(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(args)
            .arg("--repo-root")
            .arg(&self.0)
            .env_remove("CI_JOB_RESULTS_JSON")
            .output()
            .unwrap()
    }
    fn git(&self, args: &[&str]) -> String {
        let o = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap().trim().into()
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "fixture"]);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn value(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}
fn expect(o: Output, pass: bool) -> Value {
    assert_eq!(
        o.status.success(),
        pass,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    if o.stdout.is_empty() {
        serde_json::from_slice(&o.stderr).unwrap()
    } else {
        value(&o)
    }
}
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn topology() -> Fixture {
    let f = Fixture::new();
    for name in [
        "src/core/data/service_topology.json",
        "docker-compose.yml",
        "deploy/compose/docker-compose.compact.yml",
        "deploy/compose/docker-compose.full.yml",
        "deploy/compose/docker-compose.scale.yml",
    ] {
        f.write(name, fs::read(repo().join(name)).unwrap());
    }
    f
}
fn change_yaml(f: &Fixture, name: &str, edit: impl FnOnce(&mut Value)) {
    let mut v: Value = serde_yaml::from_slice(&fs::read(f.0.join(name)).unwrap()).unwrap();
    edit(&mut v);
    f.write(name, serde_yaml::to_string(&v).unwrap());
}
#[test]
fn checked_in_topology_modes_resolve() {
    let f = topology();
    for mode in ["compact", "full", "scale"] {
        let v = expect(f.tool(&["topology", "--mode", mode]), true);
        let rows = v["services"].as_array().unwrap();
        let get = |id: &str| rows.iter().find(|v| v["service_id"] == id);
        assert_eq!(v["mode"], mode);
        assert_eq!(get("gateway").unwrap()["replicas"], 1);
        assert_eq!(get("agent-runtime").unwrap()["replicas"], 1);
        for id in ["gateway-init", "migrate"] {
            assert_eq!(get(id).unwrap()["lifecycle"], "one-shot");
        }
        if mode == "compact" {
            assert_eq!(get("knowledge-service").unwrap()["runtime_role"], "all");
            assert!(get("knowledge-worker").is_none());
        }
        if mode == "scale" {
            for id in ["knowledge-worker", "agent-capability-worker"] {
                assert_eq!(get(id).unwrap()["replicas"], 2);
            }
        }
    }
}
#[test]
fn scale_overlay_cannot_claim_gateway_or_runtime_scale() {
    for id in ["gateway", "agent-runtime"] {
        let f = topology();
        let name = "src/core/data/service_topology.json";
        let mut v: Value = serde_json::from_slice(&fs::read(f.0.join(name)).unwrap()).unwrap();
        v["services"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["service_id"] == id)
            .unwrap()["modes"]["scale"]["replicas"] = json!(2);
        f.json(name, &v);
        let result = expect(f.tool(&["topology", "--mode", "scale"]), false);
        assert!(result["error"].as_str().unwrap().contains("singleton"));
    }
}
#[test]
fn compact_overlay_must_disable_dedicated_worker() {
    let f = topology();
    change_yaml(&f, "deploy/compose/docker-compose.compact.yml", |v| {
        v["services"]["knowledge-worker"]["profiles"] = json!([])
    });
    let v = expect(f.tool(&["topology", "--mode", "compact"]), false);
    assert!(v["error"].as_str().unwrap().contains("must be disabled"));
}
#[test]
fn deploy_service_scopes_are_derived_from_topology() {
    let f = topology();
    let o = f.tool(&[
        "topology",
        "--mode",
        "full",
        "--service-ids",
        "infrastructure",
    ]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8(o.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["postgres", "qdrant", "redis"]
    );
    for mode in ["compact", "scale"] {
        let o = f.tool(&["topology", "--mode", mode, "--service-ids", "app"]);
        assert!(o.status.success());
        let text = String::from_utf8(o.stdout).unwrap();
        let ids: Vec<_> = text.split_whitespace().collect();
        if mode == "compact" {
            assert!(!ids.contains(&"knowledge-worker"));
        } else {
            for id in [
                "gateway",
                "migrate",
                "agent-runtime",
                "agent-capability-worker",
            ] {
                assert!(ids.contains(&id));
            }
        }
    }
}
#[test]
fn scale_workers_cannot_use_fixed_container_identity() {
    let f = topology();
    change_yaml(&f, "docker-compose.yml", |v| {
        v["services"]["agent-capability-worker"]["container_name"] = json!("fixed-worker")
    });
    let v = expect(f.tool(&["topology", "--mode", "scale"]), false);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("fixed container_name")
    );
}
fn loc() -> (Fixture, Value) {
    let f = Fixture::new();
    f.git(&["init", "-q"]);
    f.git(&["config", "user.email", "fixture@example.invalid"]);
    f.git(&["config", "user.name", "Fixture"]);
    f.write("rust/big.rs", "// line\n".repeat(801));
    f.write("web/src/big.ts", "// line\n".repeat(501));
    f.commit();
    let sha = f.git(&["rev-parse", "HEAD"]);
    let b = json!({"schema":"ai-gateway/baseline/loc-baseline/v2","base_git_sha":sha,"thresholds":{"rust_new_file_max":800,"typescript_new_file_max":500},"oversized_rust":{"files":[{"file":"rust/big.rs","lines":801}]},"oversized_typescript":{"files":[{"file":"web/src/big.ts","lines":501}]}});
    (f, b)
}
fn loc_run(f: &Fixture, b: &Value, pass: bool) -> Value {
    f.json("baseline.json", b);
    expect(
        f.tool(&[
            "loc-no-growth",
            "--baseline",
            f.0.join("baseline.json").to_str().unwrap(),
        ]),
        pass,
    )
}
#[test]
fn provenance_checks_every_recorded_oversized_blob() {
    let (f, b) = loc();
    let v = loc_run(&f, &b, true);
    assert_eq!(
        v["provenance"],
        json!({"result":"pass","base_git_sha":b["base_git_sha"],"constrained_files_checked":2})
    );
}
#[test]
fn same_change_cannot_raise_recorded_loc_to_self_bless() {
    let (f, mut b) = loc();
    f.write("rust/big.rs", "// grown\n".repeat(805));
    f.commit();
    b["oversized_rust"]["files"][0]["lines"] = json!(805);
    let v = loc_run(&f, &b, false);
    assert_eq!(v["result"], "error");
    assert!(
        v["provenance"]["error"]
            .as_str()
            .unwrap()
            .contains("immutable Git blob")
    );
}
#[test]
fn unparseable_or_missing_base_sha_fails_closed() {
    for sha in ["not-a-sha", "0000000000000000000000000000000000000000"] {
        let (f, mut b) = loc();
        b["base_git_sha"] = json!(sha);
        assert_eq!(loc_run(&f, &b, false)["result"], "error");
    }
}
#[test]
fn non_ancestor_base_sha_fails_closed() {
    let (f, mut b) = loc();
    let parent = b["base_git_sha"].as_str().unwrap().to_owned();
    f.write("rust/branch.rs", "// main\n");
    f.commit();
    let tree = f.git(&["rev-parse", &format!("{parent}^{{tree}}")]);
    let divergent = f.git(&["commit-tree", &tree, "-p", &parent, "-m", "divergent"]);
    b["base_git_sha"] = json!(divergent);
    assert_eq!(loc_run(&f, &b, false)["result"], "error");
}
#[test]
fn missing_recorded_path_fails_closed() {
    let (f, mut b) = loc();
    b["oversized_rust"]["files"][0]["file"] = json!("rust/missing.rs");
    assert_eq!(loc_run(&f, &b, false)["result"], "error");
}
#[test]
fn threshold_cannot_be_raised_in_same_change() {
    let (f, mut b) = loc();
    b["thresholds"]["rust_new_file_max"] = json!(900);
    let v = loc_run(&f, &b, false);
    assert!(
        v["provenance"]["error"]
            .as_str()
            .unwrap()
            .contains("Rust800")
    );
}
#[test]
fn empty_or_single_language_scan_fails_closed() {
    for files in [
        vec!["rust/big.rs"],
        vec!["web/src/big.ts"],
        vec!["rust/big.rs", "web/src/big.ts"],
    ] {
        let (f, b) = loc();
        for p in files {
            fs::remove_file(f.0.join(p)).unwrap();
        }
        let v = loc_run(&f, &b, false);
        assert_eq!(v["result"], "error");
        assert!(
            v["provenance"]["error"]
                .as_str()
                .unwrap()
                .contains("both be nonempty")
        );
    }
}
#[test]
fn loc_source_read_error_fails_closed() {
    let (f, b) = loc();
    f.write("rust/big.rs", [255_u8]);
    assert_eq!(loc_run(&f, &b, false)["result"], "error");
}
#[cfg(unix)]
#[test]
fn symlinked_loc_scan_subtree_fails_closed() {
    let (f, b) = loc();
    let external = Fixture::new();
    external.write("hidden.rs", "// hidden\n");
    std::os::unix::fs::symlink(&external.0, f.0.join("rust/linked")).unwrap();
    assert_eq!(loc_run(&f, &b, false)["result"], "error");
}
#[test]
fn generator_rejects_unknown_source_object() {
    let (f, _) = loc();
    expect(
        f.tool(&[
            "inventory-generate",
            "--kind",
            "loc-baseline",
            "--write",
            "--source-rev",
            "0000000000000000000000000000000000000000",
        ]),
        false,
    );
}
#[test]
fn generator_counts_committed_blobs_and_refuses_dirty_snapshot() {
    let (f, b) = loc();
    let dest = Fixture::new();
    let args = [
        "inventory-generate",
        "--kind",
        "loc-baseline",
        "--write",
        "--source-rev",
        b["base_git_sha"].as_str().unwrap(),
        "--output-dir",
        dest.0.to_str().unwrap(),
    ];
    expect(f.tool(&args), true);
    let baseline: Value =
        serde_json::from_slice(&fs::read(dest.0.join("loc-baseline.json")).unwrap()).unwrap();
    assert_eq!(baseline["base_git_sha"], b["base_git_sha"]);
    assert_eq!(baseline["oversized_rust"]["files"][0]["lines"], 801);
    assert_eq!(baseline["oversized_typescript"]["files"][0]["lines"], 501);
    f.write("rust/big.rs", "// dirty\n".repeat(900));
    f.write("rust/untracked.rs", "// untracked\n".repeat(900));
    expect(f.tool(&args), false);
    let after: Value =
        serde_json::from_slice(&fs::read(dest.0.join("loc-baseline.json")).unwrap()).unwrap();
    assert_eq!(after, baseline);
}
fn hygiene() -> Fixture {
    let f = Fixture::new();
    f.write(
        "rust/tests/shapes.rs",
        "#[test] fn real(){let result=2;assert_eq!(result,2);}\n",
    );
    f.write(
        "web/src/clean.test.ts",
        "test('real',()=>{expect(value).toBe(1);});\n",
    );
    f.json("skip.json",&json!({"schema":"ai-gateway/baseline/skip-baseline/v2","rust":{"marker_count":0,"markers":[]},"typescript":{"marker_count":0,"markers":[]}}));
    f
}
fn hygiene_run(f: &Fixture, pass: bool) -> Value {
    expect(
        f.tool(&[
            "hygiene-check",
            "--skip-baseline",
            f.0.join("skip.json").to_str().unwrap(),
        ]),
        pass,
    )
}
#[test]
fn rust_syntax_error_fails_closed() {
    let f = hygiene();
    f.write("rust/tests/shapes.rs", "#[test] fn broken( {\n");
    let v = hygiene_run(&f, false);
    assert_eq!(v["result"], "error");
    assert!(
        v["scan_error"]
            .as_str()
            .unwrap()
            .contains("Rust syntax error")
    );
}
#[test]
fn rust_ast_flags_empty_and_self_proving_bodies_only() {
    let f = hygiene();
    f.write("rust/tests/shapes.rs",r#"#[test] fn empty(){} #[test] fn truth(){assert!(true);} #[test] fn comparison(){assert!(1 < 2 && 2 < 3);} #[test] fn equality(){assert_eq!(value,value);} #[test] fn real(){assert_eq!(result,2);} #[test] fn helper_only(){helper();}"#);
    let v = hygiene_run(&f, false);
    let names: std::collections::BTreeSet<_> = v["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["test"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        std::collections::BTreeSet::from(["empty", "truth", "comparison", "equality"])
    );
    assert_eq!(
        v["no_assertion_warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["test"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["helper_only"]
    );
}
#[test]
fn typescript_full_file_scan_catches_multiline_and_placeholders() {
    let f = hygiene();
    f.write("web/src/clean.test.ts","test\n .only('focused',()=>{expect(1).toBe(1);});\ndescribe\n .fixme('disabled',()=>{expect(1).toBe(1);});\ntest(\n 'comment placeholder',\n async ({page}) => {\n // TODO\n },\n);\nit('function placeholder',function(){/* empty */});\ntest('real',async()=>{expect(1).toBe(1);});\n");
    let v = hygiene_run(&f, false);
    let failures = v["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 4);
    assert_eq!(
        failures
            .iter()
            .filter(|x| x["issue"] == "empty test body")
            .count(),
        2
    );
    for name in ["comment placeholder", "function placeholder"] {
        assert!(failures.iter().any(|x| x["test"] == name));
    }
}
#[test]
fn empty_and_one_language_hygiene_scans_fail_closed() {
    for names in [
        vec!["rust/tests/shapes.rs"],
        vec!["web/src/clean.test.ts"],
        vec!["rust/tests/shapes.rs", "web/src/clean.test.ts"],
    ] {
        let f = hygiene();
        for p in names {
            fs::remove_file(f.0.join(p)).unwrap();
        }
        let v = hygiene_run(&f, false);
        assert_eq!(v["result"], "error");
        assert!(
            v["scan_error"]
                .as_str()
                .unwrap()
                .contains("both be nonempty")
        );
    }
}
#[test]
fn hygiene_source_read_error_fails_closed() {
    let f = hygiene();
    f.write("rust/tests/shapes.rs", [255_u8]);
    assert_eq!(hygiene_run(&f, false)["result"], "error");
}
#[cfg(unix)]
#[test]
fn symlinked_hygiene_scan_subtree_fails_closed() {
    let f = hygiene();
    let external = Fixture::new();
    external.write("test.rs", "#[test] fn hidden(){}\n");
    std::os::unix::fs::symlink(&external.0, f.0.join("rust/linked")).unwrap();
    assert_eq!(hygiene_run(&f, false)["result"], "error");
}
#[test]
fn dated_allowlist_match_expiry_and_staleness_are_preserved() {
    for (expiry, test, pass, field) in [
        ("2099-01-01", "empty", true, "allowlisted"),
        ("2000-01-01", "empty", false, "expired_entries"),
        ("2099-01-01", "absent", false, "stale_entries"),
    ] {
        let f = hygiene();
        f.write("rust/tests/shapes.rs", "#[test] fn empty(){}\n");
        f.json("scripts/harness/hygiene_allowlist.json",&json!({"entries":[{"file":"rust/tests/shapes.rs","test":test,"owner":"fixture","reason":"bounded source regression","expires":expiry}]}));
        let v = hygiene_run(&f, pass);
        assert_eq!(v[field].as_array().unwrap().len(), 1);
    }
}
#[test]
fn skip_baseline_allows_removal_but_rejects_new_or_reclassified_skip() {
    let f = hygiene();
    f.write(
        "web/src/clean.test.ts",
        "test.skip('known',()=>{expect(value).toBe(1);});\n",
    );
    let v = hygiene_run(&f, false);
    let markers = v["skip_baseline"]["unexpected"].as_array().unwrap();
    let mut baseline = json!({"schema":"ai-gateway/baseline/skip-baseline/v2","rust":{"marker_count":0,"markers":[]},"typescript":{"marker_count":0,"markers":[]}});
    baseline["typescript"]["markers"] = json!(
        markers
            .iter()
            .map(|m| {
                let mut m = m.clone();
                m.as_object_mut().unwrap().remove("language");
                m
            })
            .collect::<Vec<_>>()
    );
    baseline["typescript"]["marker_count"] =
        json!(baseline["typescript"]["markers"].as_array().unwrap().len());
    f.json("skip.json", &baseline); // Known skips are permitted by the immutable ledger; reclassified markers must fail.
    let known = hygiene_run(&f, true);
    assert!(
        known["skip_baseline"]["unexpected"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.write(
        "web/src/clean.test.ts",
        "test('known',()=>{expect(value).toBe(1);});\n",
    );
    let removed = hygiene_run(&f, true);
    assert_eq!(removed["skip_baseline"]["retired"], 1);
    f.write(
        "web/src/clean.test.ts",
        "test.fixme('known',()=>{expect(value).toBe(1);});\n",
    );
    let changed = hygiene_run(&f, false);
    assert_eq!(
        changed["skip_baseline"]["unexpected"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
fn guard() -> Fixture {
    let f = topology();
    fs::create_dir_all(f.0.join("rust/platform-native/crates/platform-knowledge-service/src"))
        .unwrap();
    let values = "gateway:\n  replicaCount: 1\n  autoscaling:\n    enabled: false\n    minReplicas: 1\n    maxReplicas: 1\nfrontend:\n  replicaCount: 4\n  autoscaling:\n    enabled: true\n    minReplicas: 2\n    maxReplicas: 10\n";
    for name in ["values.yaml", "values-production.yaml"] {
        f.write(&format!("deploy/helm/ai-gateway/{name}"), values);
    }
    f.write("deploy/helm/ai-gateway/templates/gateway-deployment.yaml","kind: Deployment\nmetadata:\n  name: release-gateway\nspec:\n  replicas: 1\n  strategy:\n    type: Recreate\n");
    f.write("deploy/helm/ai-gateway/templates/frontend-hpa.yaml","kind: HorizontalPodAutoscaler\nmetadata:\n  name: release-frontend\nspec:\n  maxReplicas: {{ .Values.frontend.autoscaling.maxReplicas }}\n");
    f
}
fn guard_run(f: &Fixture, pass: bool) -> Value {
    expect(
        f.tool(&[
            "single-instance-guard",
            "--source-sha",
            "1111111111111111111111111111111111111111",
        ]),
        pass,
    )
}
#[test]
fn safe_singletons_allow_manifest_scalable_services() {
    let f = guard();
    let v = guard_run(&f, true);
    assert_eq!(v["failures"], json!([]));
    assert_eq!(v["values"]["gateway"]["production"]["replicaCount"], 1);
    assert_eq!(
        v["values"]["agent-runtime"],
        json!({"helm_values_key":"agentRuntime","present":false})
    );
}
#[test]
fn values_scale_attempts_fail_closed() {
    for (name, key, value) in [
        ("values.yaml", "replicaCount", json!(2)),
        ("values.yaml", "enabled", json!(true)),
        ("values-production.yaml", "maxReplicas", json!(3)),
    ] {
        let f = guard();
        change_yaml(&f, &format!("deploy/helm/ai-gateway/{name}"), |v| {
            if key == "replicaCount" {
                v["gateway"][key] = value;
            } else {
                v["gateway"]["autoscaling"][key] = value;
            }
        });
        let v = guard_run(&f, false);
        assert!(!v["failures"].as_array().unwrap().is_empty());
    }
}
#[test]
fn topology_cannot_self_bless_gateway_scale() {
    for key in ["scale_support", "replicas"] {
        let f = guard();
        let path = "src/core/data/service_topology.json";
        let mut v: Value = serde_json::from_slice(&fs::read(f.0.join(path)).unwrap()).unwrap();
        let gateway = v["services"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["service_id"] == "gateway")
            .unwrap();
        if key == "replicas" {
            gateway["modes"]["scale"][key] = json!(2);
        } else {
            gateway[key] = json!("stateless");
        }
        f.json(path, &v);
        guard_run(&f, false);
    }
}
#[test]
fn template_hpa_values_replica_and_rollout_overlap_are_rejected() {
    let f = guard();
    f.write("deploy/helm/ai-gateway/templates/gateway-deployment.yaml","kind: Deployment\nmetadata:\n  name: release-gateway\nspec:\n  replicas: {{ .Values.gateway.replicaCount }}\n  strategy:\n    type: RollingUpdate\n---\nkind: HorizontalPodAutoscaler\nmetadata:\n  name: release-gateway\n");
    let v = guard_run(&f, false);
    let failures = v["failures"].to_string();
    for fragment in [
        "must not honor gateway",
        "must not render HPA",
        "requires Recreate",
    ] {
        assert!(failures.contains(fragment), "{failures}");
    }
}
#[test]
fn yaml_reader_rejects_duplicate_scale_scalar() {
    let f = guard();
    f.write(
        "deploy/helm/ai-gateway/values.yaml",
        "gateway:\n  replicaCount: 1\n  replicaCount: 2\n",
    );
    let v = guard_run(&f, false);
    assert!(v["error"].as_str().unwrap().contains("duplicate"));
}
#[test]
fn generated_topology_has_pinned_singletons() {
    let f = guard();
    let v = expect(
        f.tool(&[
            "topology-inventory",
            "--source-sha",
            "1111111111111111111111111111111111111111",
        ]),
        true,
    );
    for (id, field, expected) in [
        ("gateway", "class", json!("single-instance")),
        ("agent-runtime", "max_replicas", json!(1)),
    ] {
        let row = v["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["service_id"] == id)
            .unwrap();
        assert_eq!(row["scale_contract"][field], expected);
    }
}
#[test]
fn prd_confirmed_dead_and_self_proving_candidates_are_closed() {
    let o = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
        .args(["known-candidates", "--repo-root"])
        .arg(repo())
        .output()
        .unwrap();
    let v = expect(o, true);
    assert_eq!(v["pending_scoped_removal"], json!([]));
    assert_eq!(
        v["removed"],
        json!([
            "file-storage-barrel-only-export",
            "unused-get-langgraph-proxy"
        ])
    );
    assert_eq!(
        v["repaired"],
        json!([
            "streaming-timeout-placeholder",
            "streaming-connection-placeholder"
        ])
    );
}
