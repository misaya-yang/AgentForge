//! Offline negative immutable-manifest assertions from the baseline/legacy Python suites.
use ai_platform_migration_authority::{
    DEFAULT_BASELINE_ID, commands, legacy, manifest, runner::AuthorityPaths,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
struct Fixture(PathBuf);
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target)
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("authority-manifests-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn database() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database")
    }
    fn baseline(&self) -> PathBuf {
        let dir = self.0.join("database/baselines").join(DEFAULT_BASELINE_ID);
        copy_tree(
            &Self::database().join("baselines").join(DEFAULT_BASELINE_ID),
            &dir,
        );
        dir.join("manifest.json")
    }
    fn epoch(&self) -> PathBuf {
        let dir = self.0.join("2026_08_probe_v1");
        fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("001_probe.sql");
        fs::write(&sql, "SELECT 1;\n").unwrap();
        let p = dir.join("manifest.yml");
        write(
            &p,
            &json!({"baseline_id":"2026_08_probe_v1","epoch":1,"changes":[{"sequence":1,"name":"probe","file":"001_probe.sql","sha256":manifest::file_sha256(&sql).unwrap(),"owner":"owner","transaction_mode":"transactional","rollback_class":"forward-fix-only","preconditions":["SELECT TRUE"],"postconditions":["SELECT TRUE"],"timeout_seconds":300,"lock_budget_seconds":30,"resume_handler":null,"repair_handler":null}]}),
        );
        p
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
#[test]
fn baseline_metadata_rejects_all_original_ambiguous_shapes() {
    let f = Fixture::new();
    let p = f.baseline();
    let original = read(&p);
    let reference =
        json!({"table":"public.system_values","natural_key":["key"],"immutable_columns":["value"]});
    for (field, value) in [
        ("schema_revision", json!("latest")),
        (
            "reference_data",
            json!([{"table":"public.system_values","natural_key":"key","immutable_columns":["value"]}]),
        ),
        ("reference_data", json!({"table":"public.system_values"})),
        (
            "reference_data",
            json!([{"table":"public.system_values","natural_key":["key"],"immutable_columns":["value"],"where":1}]),
        ),
        ("reference_data", json!([reference.clone(), reference])),
    ] {
        let mut v = original.clone();
        v[field] = value;
        write(&p, &v);
        assert!(
            manifest::load_baseline_manifest(&p).is_err(),
            "accepted malformed {field}"
        );
    }
}
#[test]
fn baseline_readiness_requires_frozen_state_cutover_and_legacy_freeze_agreement() {
    let f = Fixture::new();
    let p = f.baseline();
    let original = read(&p);
    let paths = AuthorityPaths {
        database_dir: f.0.join("database"),
    };
    assert!(commands::baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
    let mut v = original.clone();
    v["state"] = json!("pending-live-freeze");
    write(&p, &v);
    assert!(!commands::baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
    write(&p, &original);
    let cutover = p.parent().unwrap().join("cutover_convergence.sql");
    let bytes = fs::read(&cutover).unwrap();
    fs::remove_file(&cutover).unwrap();
    assert!(!commands::baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
    fs::write(cutover, bytes).unwrap();
    copy_tree(
        &Fixture::database().join("migrations"),
        &paths.database_dir.join("migrations"),
    );
    let mut v = original;
    v["last_legacy_change"] = json!("111_wrong.sql");
    write(&p, &v);
    assert!(
        commands::load_baseline(&paths, DEFAULT_BASELINE_ID)
            .unwrap_err()
            .to_string()
            .contains("freeze point")
    );
}
#[test]
fn epoch_rejects_directory_mismatch_extra_sql_checksum_and_unsafe_execution_contracts() {
    let f = Fixture::new();
    let p = f.epoch();
    let original = read(&p);
    assert_eq!(
        manifest::load_epoch_manifest(&p).unwrap().baseline_id,
        "2026_08_probe_v1"
    );
    let mut v = original.clone();
    v["baseline_id"] = json!("2026_08_other_v1");
    write(&p, &v);
    assert!(manifest::load_epoch_manifest(&p).is_err());
    write(&p, &original);
    let extra = p.parent().unwrap().join("002_extra.sql");
    fs::write(&extra, "SELECT 2;").unwrap();
    assert!(manifest::load_epoch_manifest(&p).is_err());
    fs::remove_file(extra).unwrap();
    let sql = p.parent().unwrap().join("001_probe.sql");
    fs::write(&sql, "SELECT 999;\n").unwrap();
    assert!(
        manifest::load_epoch_manifest(&p)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    fs::write(sql, "SELECT 1;\n").unwrap();
    for patch in [
        json!({"timeout_seconds":0}),
        json!({"timeout_seconds":10,"lock_budget_seconds":11}),
        json!({"transaction_mode":"non_transactional"}),
        json!({"transaction_mode":{"invalid":"shape"}}),
        json!({"rollback_class":["invalid"]}),
        json!({"resume_handler":"unexpected"}),
        json!({"preconditions":[]}),
        json!({"postconditions":["DELETE FROM widgets RETURNING TRUE"]}),
        json!({"typo_field":true}),
    ] {
        let mut v = original.clone();
        for (k, value) in patch.as_object().unwrap() {
            v["changes"][0][k] = value.clone();
        }
        write(&p, &v);
        assert!(
            manifest::load_epoch_manifest(&p).is_err(),
            "accepted {patch}"
        );
    }
}
#[test]
fn legacy_rejects_extra_sql_checksum_and_invalid_transaction_wrappers() {
    let f = Fixture::new();
    let dir = f.0.join("migrations");
    copy_tree(&Fixture::database().join("migrations"), &dir);
    let path = dir.join("legacy-manifest.yml");
    let m = manifest::load_legacy_manifest(&path).unwrap();
    let extra = dir.join("999_unreviewed.sql");
    fs::write(&extra, "SELECT 1;\n").unwrap();
    assert!(manifest::load_legacy_manifest(&path).is_err());
    fs::remove_file(extra).unwrap();
    let sql = dir.join(&m.changes[0].file);
    let old = fs::read(&sql).unwrap();
    let mut changed = old.clone();
    changed.extend_from_slice(b"\n-- drift\n");
    fs::write(&sql, changed).unwrap();
    assert!(
        manifest::load_legacy_manifest(&path)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    fs::write(sql, old).unwrap();
    for content in [
        "BEGIN; SELECT 1;",
        "SELECT 1; COMMIT;",
        "BEGIN; BEGIN; SELECT 1; COMMIT; COMMIT;",
        "BEGIN; SELECT 1; ROLLBACK;",
    ] {
        assert!(
            legacy::strip_outer_transaction(content).is_err(),
            "{content}"
        );
    }
    let outer = m
        .changes
        .iter()
        .filter_map(|spec| {
            legacy::strip_outer_transaction(&fs::read_to_string(dir.join(&spec.file)).unwrap())
                .unwrap()
                .1
                .then_some(spec.file.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(outer.len(), 53);
    assert!(outer.contains(&"031_align_model_prices_20260211.sql"));
    assert!(outer.contains(&"112_kb_document_progress_retention.sql"));
    assert!(!outer.contains(&"049_session_list_performance.sql"));
}
