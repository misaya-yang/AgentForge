//! Source-only tests. Fixtures preserve real reviewed bytes; no database,
//! invented fingerprints, external import archive or Git ancestry is required.
use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("agentforge-successor-023-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn source(&self) -> PathBuf {
        let historical = crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(22);
        let repository = historical.root();
        let root = self.0.join("source");
        fs::create_dir(&root).unwrap();
        next_successor::copy_tree(&repository.join("database"), &root.join("database")).unwrap();
        fs::create_dir(root.join("docs")).unwrap();
        next_successor::copy_tree(&repository.join("docs/design"), &root.join("docs/design"))
            .unwrap();
        root
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn source_chain_works_without_git_or_retired_external_import_archive() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert!(!source.join(".git").exists());
    assert!(
        !source
            .join("reports/dots-source-import-20261015.json")
            .exists()
    );
    let candidate = validate_source(&source).unwrap();
    assert_eq!(candidate.epoch.epoch, 23);
    assert_eq!(candidate.epoch.changes.len(), 23);
    assert_eq!(candidate.package_lock_sha256, APPROVED_022_LOCK_SHA256);
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    let (rechecked, marker) = verify_preview(&source, &preview).unwrap();
    assert_eq!(rechecked.plan_sha256, candidate.plan_sha256);
    assert_eq!(marker["database_actions"], false);
    assert_eq!(marker["source_activation"], false);
    assert!(!source.join(ACTIVE_MANIFEST_RELATIVE).is_symlink());
    assert_eq!(
        manifest::load_epoch_manifest(&source.join(ACTIVE_MANIFEST_RELATIVE))
            .unwrap()
            .epoch,
        22
    );
}

#[test]
fn changing_reviewed_plan_or_predecessor_receipt_is_rejected() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let plan = source.join(PLAN_RELATIVE);
    let bytes = fs::read(&plan).unwrap();
    let mut modified = bytes.clone();
    modified.push(b' ');
    fs::write(&plan, modified).unwrap();
    assert!(validate_source(&source).is_err());
    fs::write(&plan, bytes).unwrap();
    assert!(validate_source(&source).is_ok());
    let predecessor = source
        .join("database")
        .join(EPOCH_RELATIVE)
        .join("022_verification.json");
    let mut receipt = fs::read(&predecessor).unwrap();
    receipt.push(b' ');
    fs::write(predecessor, receipt).unwrap();
    assert!(validate_source(&source).is_err());
}

#[test]
fn preview_cannot_mutate_predecessor_add_inputs_or_forge_activation() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    let sql = preview
        .join("database")
        .join(EPOCH_RELATIVE)
        .join(NEXT_SQL_FILE);
    let original = fs::read(&sql).unwrap();
    let mut changed = original.clone();
    changed.push(b' ');
    fs::write(&sql, changed).unwrap();
    assert!(verify_preview(&source, &preview).is_err());
    fs::write(&sql, original).unwrap();
    assert!(verify_preview(&source, &preview).is_ok());
    let injected = preview
        .join("database")
        .join(EPOCH_RELATIVE)
        .join("024_unreviewed.sql");
    fs::write(&injected, "SELECT 1;").unwrap();
    assert!(verify_preview(&source, &preview).is_err());
    fs::remove_file(injected).unwrap();
    let marker_path = preview.join(PREVIEW_RECEIPT_FILE);
    let mut marker: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    marker["source_activation"] = json!(true);
    fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    assert!(verify_preview(&source, &preview).is_err());
}

#[test]
fn unmeasured_preview_cannot_produce_bundle_or_pass_measured_verification() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    assert!(verify_measured_preview(&source, &preview).is_err());
    let bundle = fixture.0.join("bundle");
    assert!(activation_bundle(&source, &preview, &bundle).is_err());
    assert!(!bundle.exists());
    assert!(materialize(&source, &preview).is_err());
    let inside = source.join("forbidden-preview");
    assert!(materialize(&source, &inside).is_err());
    assert!(!inside.exists());
}

#[cfg(unix)]
#[test]
fn symlink_source_or_preview_is_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.source();
    let source_link = fixture.0.join("source-link");
    symlink(&source, &source_link).unwrap();
    assert!(validate_source(&source_link).is_err());
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    let preview_link = fixture.0.join("preview-link");
    symlink(&preview, &preview_link).unwrap();
    assert!(verify_preview(&source, &preview_link).is_err());
}
