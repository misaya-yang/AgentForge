use super::*;
fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!("upstream-sync-tests-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&p).unwrap();
    p
}
#[test]
fn actual_git_numstat_and_three_way_conflicts_are_not_declarations() {
    let before = b"unchanged\nold\n";
    let after = b"unchanged\nnew\nextra\n";
    let lines = inventory::numstat(before, after).unwrap();
    assert_eq!(lines, json!({"added":2,"removed":1,"binary":false}));
    let conflict = inventory::conflicts(b"platform\n", b"base\n", b"upstream\n").unwrap();
    assert_eq!(conflict["conflict_hunks"], 1);
    assert_eq!(conflict["unmergeable"], false);
    let clean =
        inventory::conflicts(b"platform\ntail\n", b"base\ntail\n", b"base\ntail\n").unwrap();
    assert_eq!(clean["conflict_hunks"], 0);
    assert_eq!(inventory::numstat(b"a\0", b"b\0").unwrap()["binary"], true);
}
#[test]
fn schema_and_sbom_diff_reports_changed_content_not_only_file_count() {
    let old = BTreeMap::from([
        ("same".into(), json!({"version":"1"})),
        ("removed".into(), json!({})),
    ]);
    let new = BTreeMap::from([
        ("same".into(), json!({"version":"2"})),
        ("new".into(), json!({})),
    ]);
    assert_eq!(
        inventory::changed(&old, &new),
        json!({"added":["new"],"removed":["removed"],"changed":["same"]})
    );
}
#[test]
fn two_real_windows_preserve_fifteen_file_target_and_cannot_count_same_month_twice() {
    let baseline = json!({"upstream_revision":"old","source_unit_sha256":"old-unit"});
    let w = json!({"month":"2026-11","baseline":baseline});
    let candidate = json!({"upstream_revision":"new"});
    let p = json!({"schema":REPORT_SCHEMA,"result":"pass","time_to_green_ms":42,"month":"2026-10","candidate":baseline,"tag":{"commit":"old"}});
    assert_eq!(
        continuity(&w, &candidate, None, Some(10), true).unwrap()["met"],
        false
    );
    assert_eq!(
        continuity(&w, &candidate, Some(&p), Some(16), true).unwrap()["result"],
        "target_not_met"
    );
    assert_eq!(
        continuity(&w, &candidate, Some(&p), Some(15), true).unwrap()["met"],
        true
    );
    assert_eq!(
        continuity(&w, &candidate, Some(&p), Some(15), false).unwrap()["met"],
        false
    );
    let mut same = p.clone();
    same["month"] = json!("2026-11");
    assert!(continuity(&w, &candidate, Some(&same), Some(15), true).is_err());
    let mut stale = p;
    stale["candidate"]["upstream_revision"] = json!("different");
    assert!(continuity(&w, &candidate, Some(&stale), Some(15), true).is_err());
}
#[test]
fn sole_ledger_pins_original_window_clock_and_prohibits_restarting_it() {
    let root = scratch();
    std::fs::create_dir_all(root.join("deploy/runbooks/native-enterprise-upgrade")).unwrap();
    let w = json!({"month":"2026-10","started_at":"2026-10-01T00:00:00Z","tag":{"commit":"fixed"}});
    let ledger = json!({"work_packages":[{"id":"U-RT-07","sync_windows":[{"month":"2026-10","started_at":w["started_at"],"target_commit":"fixed","window_sha256":"original"}]}]});
    std::fs::write(root.join(LEDGER), serde_json::to_vec(&ledger).unwrap()).unwrap();
    let mut args = Args::default();
    args.values
        .insert("--repo-root".into(), root.to_string_lossy().into());
    assert!(registered(&args, &w, "original").is_ok());
    assert!(registered(&args, &w, "new-clock").is_err());
    let mut changed = w;
    changed["started_at"] = json!("2026-10-15T00:00:00Z");
    assert!(registered(&args, &changed, "original").is_err());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn immutable_receipt_cannot_replace_previous_failure_or_follow_symlink() {
    let root = scratch();
    let path = root.join("window.json");
    immutable(&path, &json!({"result":"blocked"})).unwrap();
    assert!(immutable(&path, &json!({"result":"pass"})).is_err());
    assert_eq!(load(&path).unwrap()["result"], "blocked");
    #[cfg(unix)]
    {
        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(immutable(&link, &json!({})).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn empty_or_relabelled_gate_scenarios_cannot_pass() {
    assert!(coverage(&json!([]), &["ordinary_turn"], "scenario", "items").is_err());
    assert!(
        coverage(
            &json!([{"scenario":"ordinary_turn","items":0}]),
            &["ordinary_turn"],
            "scenario",
            "items"
        )
        .is_err()
    );
    assert!(
        coverage(
            &json!([{"scenario":"ordinary_turn","items":1},{"scenario":"ordinary_turn","items":1}]),
            &["ordinary_turn"],
            "scenario",
            "items"
        )
        .is_err()
    );
}
#[test]
fn history_gate_rejects_stale_unit_failed_execution_and_missing_raw_receipt() {
    let w = json!({"started_at":"2026-10-01T00:00:00Z"});
    let candidate = json!({"source_unit_sha256":"unit"});
    let lock = json!({});
    let run = json!({"schema":RUN_SCHEMA,"kind":"history","result":"pass","exit_code":1,"timed_out":false});
    assert!(validate_gate("history", &run, &w, &candidate, &lock, "window", "lock").is_err());
    let mut run = run;
    run["exit_code"] = json!(0);
    run["window_sha256"] = json!("window");
    run["candidate"] = json!({"source_unit_sha256":"stale"});
    run["source_lock_sha256"] = json!("lock");
    assert!(validate_gate("history", &run, &w, &candidate, &lock, "window", "lock").is_err());
    run["candidate"] = candidate.clone();
    run["started_at"] = json!("2026-10-01T00:00:01Z");
    run["finished_at"] = json!("2026-10-01T00:00:02Z");
    run["inputs"] = json!([]);
    assert!(validate_gate("history", &run, &w, &candidate, &lock, "window", "lock").is_err());
}
#[test]
fn duplicate_sbom_refs_and_changed_command_inputs_fail_closed() {
    let root = scratch();
    let sbom = root.join("sbom.json");
    std::fs::write(
        &sbom,
        serde_json::to_vec(
            &json!({"bomFormat":"CycloneDX","components":[{"bom-ref":"same"},{"bom-ref":"same"}]}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(inventory::sbom_components(&sbom).is_err());
    let input = root.join("input");
    std::fs::write(&input, b"original").unwrap();
    let spec = json!({"inputs":[{"path":input,"sha256":digest(&input).unwrap()}]});
    assert!(pinned_inputs(&spec).is_ok());
    std::fs::write(&input, b"changed").unwrap();
    assert!(pinned_inputs(&spec).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inventory_counts_real_upstream_patch_separately_from_platform_additions_and_conflicts() {
    let root = scratch();
    let checkout = root.join("upstream");
    std::fs::create_dir_all(checkout.join("codex-rs")).unwrap();
    let g = |args: &[&str]| inventory::git_text(&checkout, args).unwrap();
    g(&["init", "--quiet"]);
    std::fs::write(checkout.join("codex-rs/shared.rs"), "base\n").unwrap();
    g(&["add", "."]);
    g(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "base",
    ]);
    let old = g(&["rev-parse", "HEAD"]);
    let old_blob = g(&["rev-parse", "HEAD:codex-rs/shared.rs"]);
    std::fs::write(checkout.join("codex-rs/shared.rs"), "upstream\n").unwrap();
    g(&["add", "."]);
    g(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "target",
    ]);
    let target = g(&["rev-parse", "HEAD"]);
    let target_blob = g(&["rev-parse", "HEAD:codex-rs/shared.rs"]);
    let make = |directory: &Path, commit: &str, blob: &str, pristine: &[u8], content: &[u8]| {
        let payload = directory.join("payload/kernel-rs");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("shared.rs"), content).unwrap();
        std::fs::write(payload.join("platform.rs"), "platform added\n").unwrap();
        let records = json!([
            {"path":"shared.rs","proposed_sha256":hash(content),"target_preimage_sha256":hash(pristine),"target_upstream_blob":blob,"file_mode":"100644"},
            {"path":"platform.rs","proposed_sha256":hash(b"platform added\n"),"target_preimage_sha256":null,"target_upstream_blob":null,"file_mode":"100644"}
        ]);
        std::fs::write(directory.join("source-unit.json"),serde_json::to_vec(&json!({"schema":"ai-platform/runtime-source-unit/v1","target":commit,"source_file_count":2,"records":records})).unwrap()).unwrap();
    };
    let baseline = root.join("baseline");
    let current = root.join("candidate");
    make(&baseline, &old, &old_blob, b"base\n", b"platform\n");
    make(
        &current,
        &target,
        &target_blob,
        b"upstream\n",
        b"resolved platform\n",
    );
    let metrics = inventory::metrics(&current, &baseline, &checkout).unwrap();
    assert_eq!(metrics["modified_upstream_files"], 1);
    assert_eq!(metrics["platform_added_files"], 1);
    assert_eq!(metrics["text_lines_added"], 1);
    assert_eq!(metrics["text_lines_removed"], 1);
    assert_eq!(metrics["conflicting_paths"], 1);
    assert_eq!(
        metrics["modified_upstream"][0]["path"],
        "codex-rs/shared.rs"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sbom_dependency_graph_changes_remain_visible_with_identical_components() {
    let root = scratch();
    let path = root.join("sbom.json");
    let mut doc = json!({"bomFormat":"CycloneDX","components":[{"bom-ref":"a"},{"bom-ref":"b"}],"dependencies":[{"ref":"a","dependsOn":[]},{"ref":"b","dependsOn":[]}]});
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let old = inventory::sbom_components(&path).unwrap();
    doc["dependencies"][0]["dependsOn"] = json!(["b"]);
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let new = inventory::sbom_components(&path).unwrap();
    assert_eq!(
        inventory::changed(&old, &new)["changed"],
        json!(["dependency:a"])
    );
    doc["dependencies"][0]["dependsOn"] = json!(["missing"]);
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    assert!(inventory::sbom_components(&path).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn strict_cli_rejects_unknown_operations_flags_extra_positionals_and_duplicate_options() {
    let mut args = Args::default();
    args.positionals = vec!["upstream-sync".into(), "fabricated-window".into()];
    assert!(run(&args).is_err());
    args.positionals = vec!["upstream-sync".into(), "inspect".into(), "extra".into()];
    assert!(run(&args).is_err());
    args.positionals.pop();
    args.flags = vec!["--apply".into()];
    assert!(run(&args).is_err());
    args.flags.clear();
    args.multiple
        .insert("--package".into(), vec!["first".into(), "second".into()]);
    assert!(run(&args).is_err());
    args.multiple.clear();
    args.multiple
        .insert("--unknown".into(), vec!["value".into()]);
    assert!(run(&args).is_err());
}
