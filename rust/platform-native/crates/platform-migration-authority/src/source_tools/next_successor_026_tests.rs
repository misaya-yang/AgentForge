//! Source-only positive/negative tests using actual approved025 bytes. No DB,
//! arbitrary SQL, or retained import archive is needed. The ACL helper test uses
//! a synthetic line set only to exercise validation; it never writes a receipt.
use super::*;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("agentforge-026-test-{}", uuid::Uuid::new_v4()));
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
            // Restore only reviewed real025 archived bytes, never fabricate a
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
            APPROVED_025_LOCK_SHA256
        );
        assert_eq!(
            manifest::file_sha256(&source.join(MANIFEST)).unwrap(),
            APPROVED_025_MANIFEST_SHA256
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
fn actual25_without_git_or_import_archive_prepares_exact_one_change() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert!(!source.join(".git").exists());
    assert!(
        !source
            .join("reports/dots-source-import-20261015.json")
            .exists()
    );
    let candidate = validate_source(&source).unwrap();
    assert_eq!(candidate.epoch.epoch, 26);
    assert_eq!(candidate.epoch.changes.len(), 26);
    assert_eq!(candidate.package_lock_sha256, APPROVED_025_LOCK_SHA256);
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
        25
    );
    assert!(verify_activated_source(&source).is_err());
}
#[test]
fn unactivated_or_noncontiguous_predecessor_is_never_accepted() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let original = manifest::load_epoch_manifest(&source.join(MANIFEST)).unwrap();
    for epoch in [24, 26] {
        let mut invalid = original.clone();
        invalid.epoch = epoch;
        assert!(require_predecessor(&invalid).is_err());
    }
    let mut invalid = original;
    invalid.changes[0].sequence = 2;
    assert!(require_predecessor(&invalid).is_err());
    let path = epoch_file(&source, "SUCCESSOR_025_ACTIVATION.json");
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
    let extra = epoch_file(&preview, "026_unreviewed.sql");
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
fn exact_acl_allowlist_accepts_only_the_026_object_privilege_delta() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let predecessor = source_json(
        &source,
        &format!("database/{}", epoch_relative("025_verification.json")),
    )
    .unwrap();
    let mut measured = predecessor.clone();
    let mut acl_lines = serde_json::from_value::<Vec<String>>(
        predecessor["acl_compatibility"]["raw_acl_lines"].clone(),
    )
    .unwrap();
    acl_lines.extend(expected_acl_additions());
    measured["acl_compatibility"]["raw_acl_lines"] = json!(acl_lines);
    measured["acl_compatibility"]["base_acl_sha256"] = json!("a".repeat(64));
    measured["acl_compatibility"]["effective_acl_sha256"] = json!("b".repeat(64));
    measured["raw_fingerprints"]["acl"] = json!("c".repeat(64));
    measured["fingerprints"]["acl"] = json!("d".repeat(64));
    verify_exact_fingerprints(&predecessor, &measured).unwrap();

    let mut unexpected_grant = measured.clone();
    unexpected_grant["acl_compatibility"]["raw_acl_lines"]
        .as_array_mut()
        .unwrap()
        .push(json!("acl:assistant.unexpected:runtime:SELECT:grantopt=0"));
    assert!(verify_exact_fingerprints(&predecessor, &unexpected_grant).is_err());

    for forbidden in [
        "acl:assistant.assistant_runtime_capability_witnesses:PUBLIC:SELECT:grantopt=0",
        "acl:assistant.assistant_runtime_capability_witnesses:knowledge_api:SELECT:grantopt=0",
        "acl:assistant.assistant_runtime_capability_witnesses:knowledge_worker:SELECT:grantopt=0",
        "acl:assistant.assistant_runtime_capability_witnesses:gateway:UPDATE:grantopt=0",
        "acl:assistant.assistant_runtime_capability_witnesses:runtime:INSERT,SELECT:grantopt=0",
    ] {
        let mut changed = measured.clone();
        changed["acl_compatibility"]["raw_acl_lines"]
            .as_array_mut()
            .unwrap()
            .push(json!(forbidden));
        assert!(
            verify_exact_fingerprints(&predecessor, &changed).is_err(),
            "forbidden witness privilege was accepted: {forbidden}"
        );
    }

    let mut role_change = measured.clone();
    let roles = role_change["acl_compatibility"]["raw_acl_lines"]
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
    assert!(verify_exact_fingerprints(&predecessor, &role_change).is_err());

    for key in ["extensions", "reference_data"] {
        let mut changed = measured.clone();
        changed["raw_fingerprints"][key] = json!("e".repeat(64));
        changed["fingerprints"][key] = json!("e".repeat(64));
        assert!(verify_exact_fingerprints(&predecessor, &changed).is_err());
    }
}

#[test]
fn proposal_is_fixed_five_source_bundle_with_least_privilege_access() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let proposal = fs::read_to_string(require_proposal(&source).unwrap()).unwrap();
    assert_eq!(PROPOSAL_INPUTS.len(), 5);
    for input in PROPOSAL_INPUTS {
        assert!(proposal.contains(&format!("BEGIN FIXED SOURCE {}:", input.label)));
        assert!(proposal.contains(input.sha256));
    }
    assert!(proposal.contains("Depends on price-book v2"));
    assert!(proposal.contains("local_node_request_nonces"));
    assert!(proposal.contains("runtime-cache-settlement-v2"));
    assert!(proposal.contains("assistant_runtime_capability_witnesses"));
    assert!(!proposal.contains("CREATE ROLE"));
    assert!(!proposal.contains("ALTER ROLE"));
    assert!(!proposal.contains("GRANT ALL"));
    assert!(!proposal.contains("GRANT SELECT,INSERT ON knowledge.vector_mutation_jobs"));
    assert!(!proposal.contains("TO PUBLIC"));
    assert!(!proposal.contains("TO ai_gateway_runtime"));
    assert!(proposal.contains("GRANT INSERT(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision)"));
    assert!(proposal.contains("GRANT UPDATE(state,error_class,updated_at)"));
    assert!(proposal.contains("GRANT UPDATE(state,attempts,result,error_class,updated_at)"));
    assert!(proposal.contains("DO $owner_pre$"));
    assert!(proposal.contains("DO $owner_post$"));
    assert!(proposal.contains("SECURITY DEFINER"));
    assert!(proposal.contains("TO %I',prefix||'gateway')"));
    assert!(proposal.contains("prefix||'runtime'"));
    assert_eq!(TABLES.len(), 14);
    let acl_additions = expected_acl_additions();
    assert_eq!(acl_additions.len(), 84);
    assert!(acl_additions.contains(
        "function_acl:assistant.complete_assistant_runtime_model_call_v2(p_call_id uuid, p_tenant_id character varying, p_input_tokens bigint, p_output_tokens bigint, p_cost_microusd bigint, p_provider_request_id character varying, p_receipt jsonb):gateway:EXECUTE:grantopt=0"
    ));
    assert!(acl_additions.contains(
        "column_acl:knowledge.vector_mutation_jobs.job_id:knowledge_api:INSERT:grantopt=0"
    ));
    assert!(acl_additions.contains(
        "column_acl:knowledge.vector_mutation_jobs.result:knowledge_worker:UPDATE:grantopt=0"
    ));
    assert!(acl_additions.contains(
        "acl:assistant.assistant_runtime_capability_witnesses:gateway:INSERT,SELECT:grantopt=0"
    ));
    assert!(acl_additions.contains(
        "acl:assistant.assistant_runtime_capability_witnesses:runtime:SELECT:grantopt=0"
    ));
    assert!(acl_additions.contains(
        "function_acl:assistant.assert_capability_witness_binding():owner:EXECUTE:grantopt=0"
    ));
    assert!(!acl_additions.iter().any(|line| {
        line.contains("assistant.assistant_runtime_capability_witnesses")
            && (line.contains(":knowledge_api:")
                || line.contains(":knowledge_worker:")
                || line.contains(":PUBLIC:"))
    }));
    assert!(!acl_additions.iter().any(|line| line.contains(":PUBLIC:")));
    assert_eq!(change().sequence, 26);
    assert_eq!(change().owner, "owner");
    assert!(change().preconditions[0].contains("current_user"));
    assert!(change().preconditions[0].contains("pg_namespace"));
    assert!(change().postconditions[0].contains("rolcanlogin"));
    assert!(change().postconditions[2].contains("grantee=0"));
    assert!(change().postconditions[4].contains("count(*)=2"));
    assert!(change().postconditions[5].contains("count(*)=6"));
    assert!(change().postconditions[5].contains("assert_capability_witness_binding"));
    assert!(change().postconditions[5].contains("search_path=pg_catalog, assistant, pg_temp"));
    assert!(change().postconditions[2].contains("assistant_runtime_capability_witnesses"));
    assert!(change().postconditions[2].contains("a.privilege_type='SELECT'"));
}

#[test]
fn proposal_component_or_aggregate_drift_is_rejected() {
    let fixture = Fixture::new();
    let source = fixture.source();
    validate_source(&source).unwrap();

    let witness_proposal =
        source.join("docs/design/proposals/runtime-capability-witness-store.sql");
    let witness_bytes = fs::read(&witness_proposal).unwrap();
    let mut changed_witness = witness_bytes.clone();
    changed_witness.push(b' ');
    fs::write(&witness_proposal, changed_witness).unwrap();
    assert!(validate_source(&source).is_err());
    fs::write(&witness_proposal, witness_bytes).unwrap();

    let vector_proposal = source.join("database/proposals/knowledge-vector-mutation-jobs.sql");
    let vector_bytes = fs::read(&vector_proposal).unwrap();
    let mut changed_vector = vector_bytes.clone();
    changed_vector.push(b' ');
    fs::write(&vector_proposal, changed_vector).unwrap();
    assert!(validate_source(&source).is_err());
    fs::write(&vector_proposal, vector_bytes).unwrap();

    let aggregate = source.join(PROPOSAL);
    let mut changed_aggregate = fs::read(&aggregate).unwrap();
    changed_aggregate.push(b' ');
    fs::write(aggregate, changed_aggregate).unwrap();
    assert!(validate_source(&source).is_err());
}

#[test]
fn predecessor_reference_cannot_be_relabelled_as_a26_measurement() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let candidate = validate_source(&source).unwrap();
    let mut previous = source_json(
        &source,
        &format!("database/{}", epoch_relative("025_verification.json")),
    )
    .unwrap();
    previous["sequence"] = json!(26);
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
        ("schema_successor_verify_turnissue026", false),
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
