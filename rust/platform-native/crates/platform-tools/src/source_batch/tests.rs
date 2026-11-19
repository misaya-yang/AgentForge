use super::*;
#[test]
fn six_packages_and_eight_real_invocations_preserve_old_coverage() {
    let commands = expected_commands();
    assert_eq!(commands.len() + final_commands().len(), 8);
    for stem in [
        "batch-check",
        "batch-list",
        "batch-tests",
        "batch-docs",
        "batch-bins",
    ] {
        let args = &commands[stem];
        for package in PACKAGES {
            assert!(args.windows(2).any(|p| p == ["-p", package]));
        }
        assert!(args.contains(&"--locked".into()) && args.contains(&"--offline".into()));
        assert!(args.contains(&"ai-platform-plugin-quiz/executor".into()));
    }
    assert!(commands["batch-check"].contains(&"--all-targets".into()));
    assert!(commands["batch-bins"].contains(&"--all-targets".into()));
    assert!(commands["batch-list"].ends_with(&["--".into(), "--list".into()]));
    assert!(
        commands["batch-tests"].contains(&"--lib".into())
            && commands["batch-tests"].contains(&"--bins".into())
    );
    assert_ne!(UNIT_SCHEMA, "ai-platform/runtime-source-unit/v1");
}
#[test]
fn complete_lists_include_zero_test_bin_targets_and_reject_duplicates() {
    let groups=listed("{\"reason\":\"compiler-artifact\"}\na::one: test\n\n1 test, 0 benchmarks\n0 tests, 0 benchmarks\n").unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].len(), 1);
    assert!(groups[1].is_empty());
    assert!(listed("a: test\na: test\n2 tests, 0 benchmarks").is_err());
    assert!(listed("a: test\n0 tests, 0 benchmarks").is_err());
    assert!(listed("a: test").is_err());
    assert!(listed("a: test\n1 test, 1 benchmark").is_err());
}
#[test]
fn outcomes_require_full_counts_and_explicit_ignored_results() {
    let valid = "running 2 tests\ntest a ... ok\ntest b - should panic ... ignored, requires database\ntest result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    let result = outcomes(valid).unwrap();
    assert_eq!(result[0].0.len(), 2);
    assert!(!result[0].0["b"]);
    for invalid in [
        valid.replace("0 filtered out", "1 filtered out"),
        valid.replace("0 failed", "1 failed"),
        valid.replace("test a ... ok\n", ""),
        valid.replace(
            "test b - should panic ... ignored, requires database",
            "test a ... ok",
        ),
        valid.replace(" ... ok", " ... FAILED"),
    ] {
        assert!(outcomes(&invalid).is_err());
    }
    assert!(outcomes("running 0 tests").is_err());
}
fn fixture() -> Value {
    let packages=PACKAGES.into_iter().map(|name|json!({"name":name,"id":format!("path+file:///source/{name}#0.0.0"),"targets":[{"name":name.replace('-', "_"),"kind":["lib"],"test":true}]})).collect::<Vec<_>>();
    json!({"workspace_members":packages.iter().map(|p|p["id"].clone()).collect::<Vec<_>>(),"packages":packages})
}
fn artifacts(meta: &Value) -> Vec<Value> {
    meta["packages"].as_array().unwrap().iter().map(|p|json!({"reason":"compiler-artifact","package_id":p["id"],"target":p["targets"][0],"profile":{"test":true},"features":["executor"],"executable":format!("/cargo-target/debug/deps/{}-123",p["targets"][0]["name"].as_str().unwrap())})).collect()
}
fn lines(v: &[Value]) -> String {
    v.iter().map(|v| format!("{v}\n")).collect()
}
#[test]
fn artifact_closure_rejects_missing_duplicate_escape_and_disabled_executor() {
    let metadata = fixture();
    let values = artifacts(&metadata);
    let parsed = artifact_targets(&lines(&values), &metadata).unwrap();
    assert_eq!(parsed.len(), 6);
    assert!(artifact_targets(&lines(&values[1..]), &metadata).is_err());
    let mut duplicate = values.clone();
    duplicate.push(values[0].clone());
    assert!(artifact_targets(&lines(&duplicate), &metadata).is_err());
    let mut escape = values.clone();
    escape[0]["executable"] = json!("/cargo-target/debug/deps/../malicious");
    assert!(artifact_targets(&lines(&escape), &metadata).is_err());
    let mut disabled = values.clone();
    disabled[3]["features"] = json!([]);
    assert!(artifact_targets(&lines(&disabled), &metadata).is_err());
    let mut missing = metadata.clone();
    missing["workspace_members"] = json!([]);
    assert!(targets(&missing).is_err());
    let mut disabled_library = metadata.clone();
    disabled_library["packages"][0]["targets"][0]["test"] = json!(false);
    assert!(targets(&disabled_library).is_err());
    let stderr = parsed
        .keys()
        .map(|path| format!("     Running unittests src/lib.rs ({path})\n"))
        .collect::<String>();
    assert_eq!(running(&stderr, &parsed).unwrap().len(), 6);
    assert!(running("", &parsed).is_err());
    assert!(running(&(stderr.clone() + &stderr), &parsed).is_err());
}
#[test]
fn normal_artifact_closure_requires_all_eleven_distinct_non_test_elfs() {
    let mut meta = fixture();
    for (index, name) in BINS.into_iter().enumerate() {
        let owner = if index < 2 { 2 } else { 1 };
        meta["packages"][owner]["targets"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":name,"kind":["bin"],"test":true}));
    }
    let values=BINS.into_iter().enumerate().map(|(index,name)|json!({"reason":"compiler-artifact","package_id":meta["packages"][if index<2 {2}else{1}]["id"],"profile":{"test":false},"target":{"name":name,"kind":["bin"]},"executable":format!("/cargo-target/debug/{name}")})).collect::<Vec<_>>();
    verify_normal_artifacts(&lines(&values), &meta).unwrap();
    assert!(verify_normal_artifacts(&lines(&values[1..]), &meta).is_err());
    let mut duplicate = values.clone();
    duplicate.push(values[0].clone());
    assert!(verify_normal_artifacts(&lines(&duplicate), &meta).is_err());
    let mut escaped = values.clone();
    escaped[0]["executable"] = json!("/tmp/old-elf");
    assert!(verify_normal_artifacts(&lines(&escaped), &meta).is_err());
    let mut foreign = values.clone();
    foreign[0]["package_id"] = meta["packages"][3]["id"].clone();
    assert!(verify_normal_artifacts(&lines(&foreign), &meta).is_err());
    let mut missing = meta.clone();
    missing["packages"][2]["targets"] = json!([]);
    assert!(verify_normal_artifacts(&lines(&values), &missing).is_err());
}

#[test]
fn required_service_policy_keeps_all_original_twenty_five_leaves() {
    let lines = REQUIRED_SERVICE_TESTS.lines().collect::<BTreeSet<_>>();
    assert_eq!(lines.len(), 25);
    assert!(lines.contains(
        "worker-lib\tactual_runtime_grants_survive_persisted_binding_and_worker_plugin_dispatch"
    ));
    assert!(
        lines.contains("runtime-bin\tactual_fresh_database_admits_the_real_service_startup_guard")
    );
}
#[test]
fn source_batch_rejects_unknown_duplicate_and_extra_arguments() {
    for args in [
        Args {
            positionals: vec!["source-batch".into(), "verify".into(), "extra".into()],
            ..Default::default()
        },
        Args {
            positionals: vec!["source-batch".into(), "verify".into()],
            flags: vec!["--apply".into()],
            ..Default::default()
        },
        Args {
            positionals: vec!["source-batch".into(), "verify".into()],
            multiple: BTreeMap::from([("--package".into(), vec!["a".into(), "b".into()])]),
            ..Default::default()
        },
    ] {
        assert!(run(&args).is_err());
    }
}
#[test]
fn old_source_candidate_does_not_admit_new_v2_schema() {
    let root = std::env::temp_dir().join(format!("batch-v1-refusal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("source-unit.json"),
        json!({"schema":UNIT_SCHEMA}).to_string(),
    )
    .unwrap();
    let args = Args {
        positionals: vec!["source-candidate".into(), "verify".into()],
        values: BTreeMap::from([
            ("--repo-root".into(), root.to_string_lossy().into_owned()),
            ("--upstream".into(), root.to_string_lossy().into_owned()),
            ("--package".into(), root.to_string_lossy().into_owned()),
        ]),
        ..Default::default()
    };
    let error = crate::source::candidate(&args).unwrap_err().to_string();
    assert!(error.contains("source unit schema unsupported"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn contract_full_package_coverage_rejects_unexecuted_new_integration_targets() {
    let mut meta = fixture();
    meta["packages"][0]["targets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"new-integration","kind":["test"],"test":true}));
    assert!(targets(&meta).is_err());
}
