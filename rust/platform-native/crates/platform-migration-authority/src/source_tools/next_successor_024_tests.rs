//! Source-only positive/negative tests using actual approved023 bytes. No DB,
//! arbitrary SQL, synthetic fingerprints or retired external archive is needed.
use super::*;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("agentforge-024-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn source(&self) -> PathBuf {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let source = self.0.join("source");
        fs::create_dir(&source).unwrap();
        safe::copy_tree(&repository.join("database"), &source.join("database")).unwrap();
        fs::create_dir(source.join("docs")).unwrap();
        safe::copy_tree(&repository.join("docs/design"), &source.join("docs/design")).unwrap();
        let epoch = manifest::load_epoch_manifest(&source.join(MANIFEST)).unwrap();
        if epoch.epoch == CANDIDATE_EPOCH {
            // Restore only reviewed real023 archived bytes, never fabricate a
            // predecessor hash/receipt after primary advances canonical source.
            fs::write(
                source.join(LOCK),
                fs::read(source.join(ARCHIVED_LOCK)).unwrap(),
            )
            .unwrap();
            fs::write(
                source.join(MANIFEST),
                fs::read(source.join(ARCHIVED_MANIFEST)).unwrap(),
            )
            .unwrap();
            for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
                fs::remove_file(epoch_file(&source, name)).unwrap();
            }
        }
        assert_eq!(
            manifest::file_sha256(&source.join(LOCK)).unwrap(),
            APPROVED_023_LOCK_SHA256
        );
        assert_eq!(
            manifest::file_sha256(&source.join(MANIFEST)).unwrap(),
            APPROVED_023_MANIFEST_SHA256
        );
        source
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn fixed_proposal_and_change_contract_have_no_arbitrary_sql_or_role_policy() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert_eq!(
        manifest::file_sha256(&require_proposal(&source).unwrap()).unwrap(),
        SQL_SHA256
    );
    let spec = change();
    assert_eq!(spec.sequence, 24);
    assert_eq!(spec.sha256, SQL_SHA256);
    assert_eq!(spec.owner, "owner");
    assert!(spec.preconditions.iter().all(|s| !s.contains("ALTER ROLE")));
    assert!(
        spec.postconditions
            .iter()
            .any(|s| s.contains("count(*)=3") && s.contains("EXECUTE"))
    );
    let path = source.join(PROPOSAL);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"\nGRANT SELECT ON ALL TABLES IN SCHEMA assistant TO PUBLIC;\n");
    fs::write(path, bytes).unwrap();
    assert!(require_proposal(&source).is_err());
}
#[test]
fn actual23_without_git_or_import_archive_prepares_exact_one_change() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert!(!source.join(".git").exists());
    assert!(
        !source
            .join("reports/dots-source-import-20261015.json")
            .exists()
    );
    let candidate = validate_source(&source).unwrap();
    assert_eq!(candidate.epoch.epoch, 24);
    assert_eq!(candidate.epoch.changes.len(), 24);
    assert_eq!(candidate.package_lock_sha256, APPROVED_023_LOCK_SHA256);
    let preview = fixture.0.join("preview");
    let marker = materialize(&source, &preview).unwrap();
    assert_eq!(marker["source_activation"], false);
    assert_eq!(marker["database_actions"], false);
    assert_eq!(
        verify_preview(&source, &preview, false)
            .unwrap()
            .plan_sha256,
        candidate.plan_sha256
    );
    assert_eq!(
        manifest::load_epoch_manifest(&source.join(MANIFEST))
            .unwrap()
            .epoch,
        23
    );
    assert!(verify_activated_source(&source).is_err());
}
#[test]
fn unactivated_or_noncontiguous_predecessor_is_never_accepted() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let original = manifest::load_epoch_manifest(&source.join(MANIFEST)).unwrap();
    for epoch in [22, 24] {
        let mut invalid = original.clone();
        invalid.epoch = epoch;
        assert!(require_predecessor(&invalid).is_err());
    }
    let mut invalid = original;
    invalid.changes[0].sequence = 2;
    assert!(require_predecessor(&invalid).is_err());
    let path = epoch_file(&source, "SUCCESSOR_023_ACTIVATION.json");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b' ');
    fs::write(path, bytes).unwrap();
    assert!(validate_source(&source).is_err());
}
#[test]
fn preview_tampering_extra_inputs_and_forged_stage_are_rejected() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    let path = epoch_file(&preview, SQL);
    let bytes = fs::read(&path).unwrap();
    let mut changed = bytes.clone();
    changed.push(b' ');
    fs::write(&path, changed).unwrap();
    assert!(verify_preview(&source, &preview, false).is_err());
    fs::write(&path, bytes).unwrap();
    let extra = epoch_file(&preview, "025_unreviewed.sql");
    fs::write(&extra, "SELECT 1;").unwrap();
    assert!(verify_preview(&source, &preview, false).is_err());
    fs::remove_file(extra).unwrap();
    let marker = preview.join(MARKER);
    let mut value: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    value["source_activation"] = json!(true);
    fs::write(marker, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(verify_preview(&source, &preview, false).is_err());
}
#[test]
fn exact_metadata_only_acl_delta_rejects_grants_and_role_attribute_changes() {
    let old = vec![
        "role:runtime:LI:None:memberships=".to_owned(),
        "schema_acl:assistant:runtime:USAGE:grantopt=0".to_owned(),
    ];
    let mut new = old.clone();
    new.extend(acl_additions());
    assert!(validate_acl_delta(&old, &new).is_ok());
    let mut extra = new.clone();
    extra.push("acl:assistant.business:runtime:SELECT:grantopt=0".into());
    assert!(validate_acl_delta(&old, &extra).is_err());
    let mut changed = new.clone();
    changed[0] = "role:runtime:LIS:None:memberships=".into();
    assert!(validate_acl_delta(&old, &changed).is_err());
    let mut public = new.clone();
    public.push(
        "function_acl:assistant.platform_schema_admission_snapshot_v1():PUBLIC:EXECUTE:grantopt=0"
            .into(),
    );
    assert!(validate_acl_delta(&old, &public).is_err());
    let mut duplicate = new.clone();
    duplicate.push(new[0].clone());
    assert!(validate_acl_delta(&old, &duplicate).is_err());
}
#[test]
fn predecessor_reference_cannot_be_relabelled_as_a24_measurement() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let candidate = validate_source(&source).unwrap();
    let mut previous = source_json(
        &source,
        &format!("database/{}", epoch_relative("023_verification.json")),
    )
    .unwrap();
    previous["sequence"] = json!(24);
    assert!(verify_reference_value(&source, &candidate, &previous).is_err());
}
#[test]
fn unmeasured_preview_cannot_bundle_or_overwrite_or_write_inside_source() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    let bundle = fixture.0.join("bundle");
    assert!(activation_bundle(&source, &preview, &bundle).is_err());
    assert!(!bundle.exists());
    assert!(verify_preview(&source, &preview, true).is_err());
    assert!(materialize(&source, &preview).is_err());
    let inside = source.join("forbidden-preview");
    assert!(materialize(&source, &inside).is_err());
    assert!(!inside.exists());
}
#[tokio::test]
async fn approval_and_isolated_name_fail_before_source_or_database_access() {
    for (database, approved) in [
        ("schema_successor_verify_reader024", false),
        ("postgres", true),
    ] {
        let result = measure(MeasureOptions {
            source_root: "/unread-source".into(),
            preview_root: "/unread-preview".into(),
            expected_database: database.into(),
            run_approved: approved,
        })
        .await;
        assert!(matches!(result, Err(AuthorityError::Refused(_))));
    }
}
#[cfg(unix)]
#[test]
fn symlinked_source_and_preview_are_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.source();
    let linked = fixture.0.join("source-link");
    symlink(&source, &linked).unwrap();
    assert!(validate_source(&linked).is_err());
    let preview = fixture.0.join("preview");
    materialize(&source, &preview).unwrap();
    let linked = fixture.0.join("preview-link");
    symlink(&preview, &linked).unwrap();
    assert!(verify_preview(&source, &linked, false).is_err());
}
