//! Source-only positive/negative tests using actual approved024 bytes. No DB,
//! arbitrary SQL, synthetic fingerprints or retired external archive is needed.
use super::*;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("agentforge-025-test-{}", uuid::Uuid::new_v4()));
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
        if epoch.epoch >= CANDIDATE_EPOCH {
            // Restore only reviewed real024 archived bytes, never fabricate a
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
            let lock: Value =
                serde_json::from_slice(&fs::read(source.join(LOCK)).unwrap()).unwrap();
            let expected = lock_files(&lock)
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>();
            let actual = safe::collect_distribution_inputs(&source.join("database")).unwrap();
            for relative in actual.difference(&expected) {
                let path = safe::source_file(&source, &format!("database/{relative}")).unwrap();
                fs::remove_file(path).unwrap();
            }
        }
        assert_eq!(
            manifest::file_sha256(&source.join(LOCK)).unwrap(),
            APPROVED_024_LOCK_SHA256
        );
        assert_eq!(
            manifest::file_sha256(&source.join(MANIFEST)).unwrap(),
            APPROVED_024_MANIFEST_SHA256
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
fn actual24_without_git_or_import_archive_prepares_exact_one_change() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert!(!source.join(".git").exists());
    assert!(
        !source
            .join("reports/dots-source-import-20261015.json")
            .exists()
    );
    let candidate = validate_source(&source).unwrap();
    assert_eq!(candidate.epoch.epoch, 25);
    assert_eq!(candidate.epoch.changes.len(), 25);
    assert_eq!(candidate.package_lock_sha256, APPROVED_024_LOCK_SHA256);
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
        24
    );
    assert!(verify_activated_source(&source).is_err());
}
#[test]
fn unactivated_or_noncontiguous_predecessor_is_never_accepted() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let original = manifest::load_epoch_manifest(&source.join(MANIFEST)).unwrap();
    for epoch in [23, 25] {
        let mut invalid = original.clone();
        invalid.epoch = epoch;
        assert!(require_predecessor(&invalid).is_err());
    }
    let mut invalid = original;
    invalid.changes[0].sequence = 2;
    assert!(require_predecessor(&invalid).is_err());
    let path = epoch_file(&source, "SUCCESSOR_024_ACTIVATION.json");
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
fn fingerprint_guard_rejects_acl_role_extension_and_reference_data_drift() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let predecessor = source_json(
        &source,
        &format!("database/{}", epoch_relative("024_verification.json")),
    )
    .unwrap();
    verify_unchanged_fingerprints(&predecessor, &predecessor).unwrap();

    let mut changed_acl = predecessor.clone();
    changed_acl["acl_compatibility"]["raw_acl_lines"]
        .as_array_mut()
        .unwrap()
        .push(json!("acl:assistant.unexpected:gateway:INSERT:grantopt=0"));
    assert!(verify_unchanged_fingerprints(&predecessor, &changed_acl).is_err());

    let mut changed_role = predecessor.clone();
    let roles = changed_role["acl_compatibility"]["raw_acl_lines"]
        .as_array_mut()
        .unwrap();
    let role = roles
        .iter_mut()
        .find(|line| {
            line.as_str()
                .is_some_and(|line| line.starts_with("role:gateway:"))
        })
        .unwrap();
    *role = json!("role:gateway:LIS:None:memberships=");
    assert!(verify_unchanged_fingerprints(&predecessor, &changed_role).is_err());

    for key in ["extensions", "reference_data"] {
        let mut changed = predecessor.clone();
        let measured_structural = predecessor["raw_fingerprints"]["structural"].clone();
        assert_ne!(measured_structural, predecessor["raw_fingerprints"][key]);
        changed["raw_fingerprints"][key] = measured_structural.clone();
        changed["fingerprints"][key] = measured_structural;
        assert!(verify_unchanged_fingerprints(&predecessor, &changed).is_err());
    }
}

#[test]
fn proposal_alters_only_target_metadata_and_pins_unchanged_body() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let proposal = fs::read_to_string(require_proposal(&source).unwrap()).unwrap();
    assert_eq!(
        proposal
            .matches("ALTER FUNCTION assistant.issue_assistant_runtime_turn(")
            .count(),
        2
    );
    for forbidden in [
        "CREATE FUNCTION",
        "CREATE OR REPLACE",
        "GRANT ",
        "REVOKE ",
        "INSERT INTO",
        "CREATE ROLE",
        "ALTER ROLE",
    ] {
        assert!(
            !proposal.contains(forbidden),
            "proposal contains {forbidden}"
        );
    }
    assert!(proposal.contains("SECURITY DEFINER"));
    assert!(proposal.contains("SET search_path = pg_catalog, assistant, gateway, pg_temp"));
    let precondition = &change().preconditions[0];
    let postcondition = &change().postconditions[0];
    assert!(precondition.contains("NOT p.prosecdef") && precondition.contains("proconfig IS NULL"));
    assert!(
        postcondition.contains("p.prosecdef")
            && postcondition.contains("search_path=pg_catalog, assistant, gateway, pg_temp")
    );
    assert!(precondition.contains(ISSUE_FUNCTION_BODY_MD5));
    assert!(postcondition.contains(ISSUE_FUNCTION_BODY_MD5));
    assert!(
        change()
            .preconditions
            .iter()
            .all(|sql| !sql.contains("INSERT"))
    );
    assert_eq!(change().postconditions.len(), 2);
    assert!(change().postconditions[1].contains("count(*)=2"));

    let init =
        fs::read_to_string(source.join("database/baselines/2026_08_post_kb_v1/init.sql")).unwrap();
    let function_start = init
        .find("CREATE FUNCTION assistant.issue_assistant_runtime_turn(")
        .unwrap();
    let body_start = init[function_start..].find("AS $$").unwrap() + function_start + "AS $$".len();
    let body_end = init[body_start..].find("$$;").unwrap() + body_start;
    assert_eq!(
        safe::sha256_bytes(init[body_start..body_end].as_bytes()),
        ISSUE_FUNCTION_BODY_SHA256
    );
}

#[test]
fn predecessor_reference_cannot_be_relabelled_as_a25_measurement() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let candidate = validate_source(&source).unwrap();
    let mut previous = source_json(
        &source,
        &format!("database/{}", epoch_relative("024_verification.json")),
    )
    .unwrap();
    previous["sequence"] = json!(25);
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
        ("schema_successor_verify_turnissue025", false),
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
