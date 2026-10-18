//! Pure source/receipt/ACL tests. These do not stand in for actual-role PG acceptance.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("agentforge-029-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
/// Copy real source, never fabricate predecessor receipts or rewrite frozen references.
/// After activation, reconstruct only the exact archived 028 view from its actual bytes.
fn predecessor_source() -> Scratch {
    let fixture =
        crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(
            28,
        );
    let scratch = Scratch::new("source28");
    safe::copy_tree(
        &fixture.root().join("database"),
        &scratch.0.join("database"),
    )
    .unwrap();
    fs::create_dir(scratch.0.join("docs")).unwrap();
    safe::copy_tree(
        &fixture.root().join("docs/design"),
        &scratch.0.join("docs/design"),
    )
    .unwrap();
    scratch
}

#[test]
fn pins_bind_every_actual_028_artifact_and_two_original_proposals() {
    let source = predecessor_source();
    let pins = approved_028().unwrap();
    for (path, expected) in [
        (source.0.join(LOCK), pins.lock_sha256),
        (source.0.join(MANIFEST), pins.manifest_sha256),
        (
            epoch_file(&source.0, "028_verification.json"),
            pins.reference_sha256,
        ),
        (
            epoch_file(&source.0, "SUCCESSOR_028_MEASUREMENT.json"),
            pins.measurement_sha256,
        ),
        (
            epoch_file(&source.0, "SUCCESSOR_028_ACTIVATION.json"),
            pins.activation_sha256,
        ),
    ] {
        assert_eq!(manifest::file_sha256(&path).unwrap(), expected);
    }
    let activation = next_successor_028::verify_activated_source(&source.0).unwrap();
    assert_eq!(activation["source_plan_sha256"], pins.source_plan_sha256);
    let bytes = require_proposal(&source.0).unwrap();
    assert_eq!(safe::sha256_bytes(&bytes), SQL_SHA256);
    assert_eq!(bytes, expected_sql(&source.0).unwrap());
    let candidate = validate_source(&source.0).unwrap();
    assert_eq!(candidate.epoch.epoch, 29);
    assert_eq!(candidate.epoch.changes.len(), 29);
    assert_eq!(candidate.plan["predecessor_epoch"], 28);
    assert_eq!(
        candidate.plan["proposal_inputs"].as_array().unwrap().len(),
        2
    );
}
#[test]
fn worker_grant_transformation_is_exact_terminal_and_role_prefix_safe() {
    let source = predecessor_source();
    let text = fs::read_to_string(source.0.join(PROPOSAL_INPUTS[0].path)).unwrap();
    let transformed = transform_worker_grant(&text).unwrap();
    assert!(transformed.ends_with(WORKER_GRANT_DERIVED));
    assert!(!transformed.contains("TO ai_gateway_gateway"));
    assert!(transformed.contains("principal:=prefix||'gateway'"));
    assert!(transformed.contains("TO %I',principal)"));
    assert_eq!(
        transformed.replace(WORKER_GRANT_DERIVED, WORKER_GRANT_ORIGINAL),
        text
    );
    for drifted in [
        text.replace("TO ai_gateway_gateway;", "TO PUBLIC;"),
        format!("{text}\n"),
        format!("{text}{WORKER_GRANT_ORIGINAL}"),
        text.replace("p_job text", "p_task text")
            .replace(WORKER_GRANT_ORIGINAL, ""),
    ] {
        assert!(transform_worker_grant(&drifted).is_err());
    }
}
#[test]
fn fixed_change_preserves_owner_boundary_and_requires_empty_batch_marker() {
    let source = predecessor_source();
    let spec = change();
    assert_eq!(spec.sequence, 29);
    assert_eq!(
        spec.transaction_mode,
        manifest::TransactionMode::Transactional
    );
    assert_eq!(spec.preconditions.len(), 3);
    assert_eq!(spec.postconditions.len(), 5);
    for flag in [
        "rolcanlogin",
        "rolsuper",
        "rolcreatedb",
        "rolcreaterole",
        "rolreplication",
        "rolbypassrls",
    ] {
        assert!(spec.preconditions[0].contains(flag));
    }
    assert!(
        spec.postconditions[2].contains("NOT EXISTS(SELECT 1 FROM gateway.task_batch_contract)")
    );
    assert!(spec.postconditions[2].contains("count(*)=4"));
    assert!(spec.postconditions[2].contains("count(*)=2"));
    let sql = String::from_utf8(require_proposal(&source.0).unwrap()).unwrap();
    assert!(!sql.lines().any(|line| {
        line.trim_start()
            .starts_with("INSERT INTO gateway.task_batch_contract")
    }));
    for check in [
        "price_book_v2_batch_receipt_check",
        "price_book_v2_batch_stage_check",
        "dispatch_token_reservations_batch_type_check",
        "usage_records_batch_meter_v1",
    ] {
        assert!(sql.contains(&format!("VALIDATE CONSTRAINT {check};")));
    }
    assert!(!sql.contains("ALTER TABLE gateway.price_book_v2_terms"));
    reject_embedded_transactions(&sql, "platform_upgrade_gateway_workers_batch_029").unwrap();
}
#[test]
fn tampered_proposal_predecessor_or_premature_candidate_is_refused() {
    let source = predecessor_source();
    for relative in [
        PROPOSAL,
        PROPOSAL_INPUTS[0].path,
        PROPOSAL_INPUTS[1].path,
        LOCK,
        MANIFEST,
        "database/migrations/2026_08_post_kb_v1/028_verification.json",
        "database/migrations/2026_08_post_kb_v1/SUCCESSOR_028_MEASUREMENT.json",
        "database/migrations/2026_08_post_kb_v1/SUCCESSOR_028_ACTIVATION.json",
    ] {
        let path = source.0.join(relative);
        let before = fs::read(&path).unwrap();
        let mut changed = before.clone();
        changed.push(b' ');
        fs::write(&path, &changed).unwrap();
        assert!(
            validate_source(&source.0).is_err(),
            "accepted tampered {relative}"
        );
        fs::write(path, before).unwrap();
    }
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        let path = epoch_file(&source.0, name);
        fs::write(&path, b"premature").unwrap();
        assert!(validate_source(&source.0).is_err());
        fs::remove_file(path).unwrap();
    }
}
#[test]
fn preview_is_external_exact_and_cannot_bundle_unmeasured_or_forged_evidence() {
    let source = predecessor_source();
    let scratch = Scratch::new("outputs");
    let output = scratch.0.join("preview");
    let before = manifest::file_sha256(&source.0.join(MANIFEST)).unwrap();
    let receipt = materialize(&source.0, &output).unwrap();
    assert_eq!(receipt["candidate_epoch"], 29);
    assert_eq!(receipt["source_activation"], false);
    assert_eq!(receipt["database_actions"], false);
    assert!(receipt["post_migration_noop"].is_null());
    assert_eq!(receipt["batch_submission_activated"], false);
    verify_preview(&source.0, &output, false).unwrap();
    assert_eq!(
        manifest::file_sha256(&source.0.join(MANIFEST)).unwrap(),
        before
    );
    assert!(!epoch_file(&source.0, SQL).exists());
    assert!(materialize(&source.0, &source.0.join("inside-source")).is_err());
    assert!(materialize(&source.0, &output).is_err());
    assert!(activation_bundle(&source.0, &output, &scratch.0.join("unmeasured-bundle")).is_err());
    assert!(!scratch.0.join("unmeasured-bundle").exists());
    assert!(verify_preview(&source.0, &output, true).is_err());
    safe::write_new_json(
        &epoch_file(&output, REFERENCE),
        &json!({"sequence":29,"fingerprints":"fake"}),
    )
    .unwrap();
    safe::write_new_json(
        &epoch_file(&output, MEASUREMENT),
        &json!({"expected_database":"schema_successor_verify_fabricated"}),
    )
    .unwrap();
    assert!(verify_preview(&source.0, &output, true).is_err());
    assert!(verify_activated_source(&source.0).is_err());
}
fn fingerprint_pair() -> (Value, Value) {
    let old = json!({"acl_compatibility":{"raw_acl_lines":["retained"],"delta_line":ACL_DELTA,"revision":"r1","manifest_sha256":"ledger","removed_delta_count":1,"base_acl_sha256":"oldbase","effective_acl_sha256":"oldraw"},"fingerprints":{"acl":"oldbase","extensions":"e","reference_data":"r","structural":"oldstruct"},"raw_fingerprints":{"acl":"oldraw","extensions":"e","reference_data":"r","structural":"oldstruct"}});
    let mut new = old.clone();
    let mut lines = acl::expected_acl_additions();
    lines.insert("retained".into());
    new["acl_compatibility"]["raw_acl_lines"] = json!(lines);
    new["acl_compatibility"]["base_acl_sha256"] = json!("newbase");
    new["acl_compatibility"]["effective_acl_sha256"] = json!("newraw");
    new["fingerprints"]["acl"] = json!("newbase");
    new["raw_fingerprints"]["acl"] = json!("newraw");
    new["fingerprints"]["structural"] = json!("newstruct");
    new["raw_fingerprints"]["structural"] = json!("newstruct");
    (old, new)
}
#[test]
fn acl_allowlist_is_exact_minimum_and_rejects_any_extra_missing_or_duplicate_line() {
    let (old, new) = fingerprint_pair();
    let additions = acl::expected_acl_additions();
    assert_eq!(additions.len(), 38);
    assert!(additions.iter().all(|line| !line.contains(":runtime:")
        && !line.contains(":knowledge_api:")
        && !line.contains(":public:")));
    assert!(
        !additions
            .iter()
            .any(|line| line.starts_with("acl:gateway.singleton_worker_leases:gateway:"))
    );
    acl::verify_exact_fingerprints(&old, &new).unwrap();
    for line in [
        "acl:gateway.task_batch_contract:gateway:INSERT,SELECT:grantopt=0",
        "acl:gateway.singleton_worker_leases:gateway:SELECT:grantopt=0",
        "acl:gateway.task_batch_inputs:runtime:SELECT:grantopt=0",
        "function_acl:gateway.task_batch_binding():public:EXECUTE:grantopt=0",
        "column_acl:gateway.task_provider_batches.tenant_id:gateway:UPDATE:grantopt=0",
    ] {
        let mut drift = new.clone();
        drift["acl_compatibility"]["raw_acl_lines"]
            .as_array_mut()
            .unwrap()
            .push(json!(line));
        assert!(
            acl::verify_exact_fingerprints(&old, &drift).is_err(),
            "accepted {line}"
        );
    }
    let mut missing = new.clone();
    missing["acl_compatibility"]["raw_acl_lines"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(acl::verify_exact_fingerprints(&old, &missing).is_err());
    let mut duplicate = new.clone();
    duplicate["acl_compatibility"]["raw_acl_lines"]
        .as_array_mut()
        .unwrap()
        .push(json!("retained"));
    assert!(acl::verify_exact_fingerprints(&old, &duplicate).is_err());
    for key in ["extensions", "reference_data"] {
        let mut drift = new.clone();
        drift["raw_fingerprints"][key] = json!("changed");
        assert!(acl::verify_exact_fingerprints(&old, &drift).is_err());
    }
    let mut unchanged = new.clone();
    unchanged["raw_fingerprints"]["structural"] = old["raw_fingerprints"]["structural"].clone();
    assert!(acl::verify_exact_fingerprints(&old, &unchanged).is_err());
}
#[test]
fn symlinked_input_cannot_bypass_exact_source_policy() {
    #[cfg(unix)]
    {
        let source = predecessor_source();
        let path = source.0.join(PROPOSAL_INPUTS[0].path);
        let target = source.0.join("worker-copy.sql");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(require_proposal(&source.0).is_err());
    }
}

#[test]
fn activated_029_verifier_binds_the_real_measured_chain() {
    let fixture =
        crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(
            29,
        );
    let root = fixture.root();
    let receipt = verify_activated_source(root).unwrap();
    assert_eq!(receipt["candidate_epoch"], 29);
    assert_eq!(receipt["predecessor_epoch"], 28);
    assert_eq!(receipt["post_migration_noop"], true);
    assert_eq!(receipt["batch_submission_activated"], false);
    assert_eq!(
        manifest::file_sha256(&root.join(LOCK)).unwrap(),
        distribution::LOCK_SHA256
    );
}

#[test]
fn activated_029_rejects_changed_archives_and_receipts() {
    let source = Scratch::new("active-tamper");
    let fixture =
        crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(
            29,
        );
    let repository = fixture.root();
    safe::copy_tree(&repository.join("database"), &source.0.join("database")).unwrap();
    fs::create_dir(source.0.join("docs")).unwrap();
    safe::copy_tree(
        &repository.join("docs/design"),
        &source.0.join("docs/design"),
    )
    .unwrap();
    for relative in [
        ARCHIVED_LOCK,
        ARCHIVED_MANIFEST,
        "database/migrations/2026_08_post_kb_v1/SUCCESSOR_029_MEASUREMENT.json",
        "database/migrations/2026_08_post_kb_v1/SUCCESSOR_029_ACTIVATION.json",
    ] {
        let path = source.0.join(relative);
        let bytes = fs::read(&path).unwrap();
        let mut changed = bytes.clone();
        changed.push(b' ');
        fs::write(&path, changed).unwrap();
        assert!(
            verify_activated_source(&source.0).is_err(),
            "accepted tampered {relative}"
        );
        fs::write(path, bytes).unwrap();
    }
}
