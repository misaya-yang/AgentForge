use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("service-source-tests-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn anchor() -> Scratch {
    let root = repository();
    let scratch = Scratch::new();
    safe::copy_tree(&root.join("database"), &scratch.0.join("database")).unwrap();
    fs::create_dir(scratch.0.join("docs")).unwrap();
    safe::copy_tree(&root.join("docs/design"), &scratch.0.join("docs/design")).unwrap();
    copy_selected_inputs(&root, &scratch.0, 35).unwrap();
    let epoch = manifest::load_epoch_manifest(&scratch.0.join(MANIFEST))
        .unwrap()
        .epoch;
    if epoch > 29 {
        fs::write(
            scratch.0.join(LOCK),
            fs::read(root.join(ARCHIVED_LOCK)).unwrap(),
        )
        .unwrap();
        fs::write(
            scratch.0.join(MANIFEST),
            fs::read(root.join(ARCHIVED_MANIFEST)).unwrap(),
        )
        .unwrap();
        for step in catalog()
            .unwrap()
            .steps
            .iter()
            .filter(|s| s.sequence <= epoch)
        {
            for name in [
                step.change.file.clone(),
                reference_name(step.sequence),
                receipt_name(step.sequence),
            ] {
                fs::remove_file(file(&scratch.0, &name)).unwrap();
            }
        }
        for n in 30..=epoch {
            fs::remove_file(file(&scratch.0, &activation_name(n))).unwrap();
        }
    }
    validate(&scratch.0, 30).unwrap();
    scratch
}
#[test]
fn fixed_catalog_reconstructs_every_original_input_and_separates_local_contract() {
    let root = anchor();
    let source = validate(&root.0, 35).unwrap();
    for n in 30..=35 {
        let step = step(&source.catalog, n).unwrap();
        let sql = String::from_utf8(render(&root.0, &source.catalog, step).unwrap()).unwrap();
        assert!(recognizes_change(&step.change));
        assert_eq!(step.change.sequence, n);
        assert_eq!(step.change.owner, "owner");
        assert_eq!(step.change.lock_budget_seconds, if n >= 34 { 2 } else { 5 });
        assert!(!step.change.postconditions.is_empty());
        if n == 30 {
            assert!(!sql.contains("ALTER TABLE gateway.local_node_request_keys"));
            assert!(sql.contains("knowledge.pipeline_work"));
            assert!(!sql.contains("TO ai_gateway_gateway;"));
            assert!(!sql.contains("\nBEGIN;"));
        }
        if n == 31 {
            assert!(!sql.contains("DROP CONSTRAINT"));
            assert!(!sql.contains("ADD COLUMN algorithm text NOT NULL"));
            assert!(
                sql.contains(
                    "local_node_algorithm_not_null CHECK (algorithm IS NOT NULL) NOT VALID"
                )
            );
        }
        if n == 32 {
            assert!(!sql.contains("CREATE TABLE"));
            assert!(
                sql.find("VALIDATE CONSTRAINT local_node_algorithm_not_null")
                    .unwrap()
                    < sql.find("ALTER COLUMN algorithm SET NOT NULL").unwrap()
            );
        }
    }
}
#[test]
fn preview_is_external_exact_and_never_fabricates_measurement_or_activation() {
    let root = anchor();
    let outputs = Scratch::new();
    let preview = outputs.0.join("preview30");
    let source_hash = manifest::file_sha256(&root.0.join(MANIFEST)).unwrap();
    prepare(&root.0, None, &preview, 30).unwrap();
    verify_preview(&root.0, &preview, 30, false).unwrap();
    assert!(verify_preview(&root.0, &preview, 30, true).is_err());
    assert!(!file(&preview, &reference_name(30)).exists());
    assert!(!file(&preview, &receipt_name(30)).exists());
    assert!(activation_bundle(&root.0, &preview, &outputs.0.join("bundle"), 30).is_err());
    assert!(prepare(&root.0, Some(&preview), &outputs.0.join("preview31"), 31).is_err());
    assert!(prepare(&root.0, None, &root.0.join("inside"), 30).is_err());
    assert!(prepare(&root.0, None, &preview, 30).is_err());
    assert_eq!(
        source_hash,
        manifest::file_sha256(&root.0.join(MANIFEST)).unwrap()
    );
    fs::write(file(&preview, "unreviewed.sql"), b"SELECT 1").unwrap();
    assert!(verify_preview(&root.0, &preview, 30, false).is_err());
}
#[test]
fn proposal_catalog_and_frozen_anchor_tampering_are_refused() {
    let root = anchor();
    let c = catalog().unwrap();
    let mut paths = vec![CATALOG.to_owned(), LOCK.into(), MANIFEST.into()];
    paths.extend(c.proposal_inputs_sha256.keys().cloned());
    paths.extend(c.steps.iter().map(|s| s.sql_source.clone()));
    for relative in paths {
        let path = root.0.join(&relative);
        let bytes = fs::read(&path).unwrap();
        let mut changed = bytes.clone();
        changed.push(b' ');
        fs::write(&path, changed).unwrap();
        assert!(
            validate_source(&root.0, 35).is_err(),
            "accepted drift {relative}"
        );
        fs::write(path, bytes).unwrap();
    }
    for n in [0, 29, 36, 999] {
        assert!(validate_source(&root.0, n).is_err());
    }
}
#[test]
fn acl_is_exact_and_forbids_other_roles_identity_updates_and_duplicate_lines() {
    let c = catalog().unwrap();
    let additions = &step(&c, 30).unwrap().acl_additions;
    assert!(additions.iter().any(
        |l| l == "column_acl:gateway.gateway_a2a_contexts.created_at:gateway:UPDATE:grantopt=0"
    ));
    assert!(!additions.iter().any(|l| l.contains(":capability_worker:")
        || l.contains(":knowledge_api:")
        || l.contains(":public:")
        || l.contains("gateway_a2a_tasks.tenant_id:")));
    let old = json!({"acl_compatibility":{"raw_acl_lines":["retained"],"delta_line":"fixed","revision":"r1","manifest_sha256":"fixed","removed_delta_count":1},"raw_fingerprints":{"structural":"old","acl":"old","extensions":"e","reference_data":"r"},"fingerprints":{"extensions":"e","reference_data":"r"}});
    let mut new = old.clone();
    new["raw_fingerprints"]["structural"] = json!("new");
    new["raw_fingerprints"]["acl"] = json!("new");
    let mut lines = vec!["retained".to_owned()];
    lines.extend(additions.clone());
    new["acl_compatibility"]["raw_acl_lines"] = json!(lines);
    measurement::exact_acl(&old, &new, additions).unwrap();
    for extra in [
        "retained",
        "acl:knowledge.pipeline_work:knowledge_api:UPDATE:grantopt=0",
        "column_acl:gateway.gateway_a2a_tasks.tenant_id:gateway:UPDATE:grantopt=0",
    ] {
        let mut drift = new.clone();
        drift["acl_compatibility"]["raw_acl_lines"]
            .as_array_mut()
            .unwrap()
            .push(json!(extra));
        assert!(measurement::exact_acl(&old, &drift, additions).is_err());
    }
    let mut missing = new.clone();
    missing["acl_compatibility"]["raw_acl_lines"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(measurement::exact_acl(&old, &missing, additions).is_err());
}
#[test]
fn proposed_admission_export_is_the_exact_anchor_prefix_plus_fixed_steps() {
    let root = anchor();
    let s = validate(&root.0, 32).unwrap();
    let expected = admission_snapshot(&s, 30).unwrap();
    assert_eq!(expected["epoch"], 30);
    assert_eq!(expected["changes"].as_array().unwrap().len(), 30);
    let compiled = crate::application_guard::compiled_admission_snapshot().unwrap();
    assert_eq!(expected["baselines"], compiled["baselines"]);
    assert_eq!(
        &expected["changes"].as_array().unwrap()[..29],
        &compiled["changes"].as_array().unwrap()[..29]
    );
    assert!(
        admission_requirements(&s, 30)["requirements"]
            .get("local_node")
            .is_none()
    );
    assert!(
        admission_requirements(&s, 32)["requirements"]
            .get("local_node")
            .is_some()
    );
}
#[tokio::test]
async fn measurement_requires_approval_before_source_or_database_access() {
    assert!(
        measure(MeasureOptions {
            source_root: "/not-accessed".into(),
            preview_root: "/not-accessed".into(),
            sequence: 30,
            expected_database: "postgres".into(),
            run_approved: false
        })
        .await
        .is_err()
    );
}

#[test]
fn future_input_changes_do_not_invalidate_an_already_frozen_core_scope() {
    let c = catalog().unwrap();
    let mut future = c.clone();
    future.steps[1].change.notes.push(' ');
    future.proposal_inputs_sha256.insert(
        "database/proposals/local-node-key-assurance.sql".into(),
        "changed-future-input".into(),
    );
    assert_eq!(scope(&c, 30).unwrap(), scope(&future, 30).unwrap());
    assert_ne!(scope(&c, 31).unwrap(), scope(&future, 31).unwrap());
    future.steps[0].change.notes.push(' ');
    assert_ne!(scope(&c, 30).unwrap(), scope(&future, 30).unwrap());
}

#[test]
fn final_service_bundle_is_closed_without_recreating_cedar_or_activating_partitions() {
    let root = anchor();
    let s = validate(&root.0, 35).unwrap();
    let all = (30..=35)
        .map(|n| {
            String::from_utf8(render(&s.root, &s.catalog, step(&s.catalog, n).unwrap()).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        all.iter()
            .map(|sql| sql
                .matches("CREATE TABLE gateway.authorization_resource_decisions")
                .count())
            .sum::<usize>(),
        1
    );
    assert_eq!(step(&s.catalog, 33).unwrap().acl_additions.len(), 2);
    let expansion = &all[3];
    let contract = &all[4];
    let data = &all[5];
    assert!(expansion.contains("GRANT UPDATE(remote_task_id,cancel_state)"));
    assert!(!expansion.contains("cancel_state text NOT NULL"));
    assert_eq!(expansion.matches("NOT VALID;").count(), 5);
    assert_eq!(contract.matches("VALIDATE CONSTRAINT").count(), 5);
    assert!(
        contract
            .find("VALIDATE CONSTRAINT a2a_outbound_cancel_state_required")
            .unwrap()
            < contract
                .find("ALTER COLUMN cancel_state SET NOT NULL")
                .unwrap()
    );
    let step = step(&s.catalog, 35).unwrap();
    let originals = step
        .pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Source(p) => Some(p.source.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for name in [
        "expand",
        "migrate",
        "write-compatibility",
        "retention",
        "native-writer-policies",
        "legacy-retention-adapters",
    ] {
        assert!(originals.contains(
            format!("docs/design/proposals/data-platform/append-partitions/{name}.sql").as_str()
        ));
    }
    assert_eq!(originals.len(), 6);
    assert!(step.auxiliary_inputs_sha256.contains_key(
        "docs/design/proposals/data-platform/append-partitions/acceptance-catalog.sql"
    ));
    assert!(data.contains("a.attname IN('tenant_id','user_id','session_id')"));
    // UPDATE's directory uniqueness must follow the real payload WITH CHECK.
    // BEFORE UPDATE records only a nonunique movement marker; AFTER UPDATE or
    // row-movement AFTER INSERT finalizes keys and preserves original FK actions.
    let row_guard = data
        .split_once("CREATE FUNCTION partition_ops.row_guard()")
        .unwrap()
        .1
        .split_once("END $$;")
        .unwrap()
        .0;
    let before = row_guard.split_once("RETURN NULL;\n END IF;").unwrap().1;
    assert!(before.contains("SET af_moving_xid=txid_current(),af_move_identity=$1"));
    assert!(!before.contains("SET %s WHERE af_identity"));
    assert!(row_guard.contains("af_move_identity=$4 RETURNING true"));
    assert!(data.contains("(af_moving_xid,af_move_identity) WHERE af_moving_xid IS NOT NULL"));

    assert!(data.contains("existing->'session_id' IS DISTINCT FROM p_row->'session_id'"));
    assert!(data.contains("existing->'user_id' IS DISTINCT FROM p_row->'user_id'"));
    assert!(
        data.contains(
            "partition_ops.owner_family_enabled('knowledge.kb_document_progress_events')"
        )
    );
    assert!(data.contains("partition_ops.require_operator_approval(p_family,'expand'"));
    assert!(data.contains("partition_ops.require_operator_approval(p_family,'cutover'"));
    assert!(!data.contains("SET LOCAL lock_timeout"));
    assert!(!data.contains("SET LOCAL statement_timeout"));
    // No operation call is a migration statement; concurrent commands exist only
    // as installed owner routine source returned for a later native operator stage.
    let top_level = crate::sql::strip_sql_bodies(data).unwrap();
    for statement in top_level.split(';') {
        let text = statement.trim_start().to_ascii_lowercase();
        assert!(!text.starts_with("select partition_ops.expand("));
        assert!(!text.starts_with("select partition_ops.cutover("));
        assert!(!text.starts_with("select partition_ops.retention_begin("));
        assert!(!text.starts_with("create index concurrently"));
        assert!(!text.starts_with("alter table") || !text.contains("detach partition"));
    }
}

#[test]
fn final_schema_requirements_are_epoch_scoped_and_require_operator_positive_negative_acl() {
    let c = catalog().unwrap();
    assert_eq!(admission_checks(32, "gateway").unwrap().len(), 4);
    assert_eq!(admission_checks(33, "gateway").unwrap().len(), 5);
    assert_eq!(admission_checks(35, "gateway").unwrap().len(), 6);
    for principal in [
        "runtime",
        "knowledge_api",
        "knowledge_worker",
        "capability_worker",
    ] {
        assert!(
            !admission_checks(34, principal)
                .unwrap()
                .iter()
                .any(|sql| sql.contains("partition_ops.schema_admitted"))
        );
        assert!(
            admission_checks(35, principal)
                .unwrap()
                .iter()
                .any(|sql| sql == "SELECT partition_ops.schema_admitted()")
        );
    }
    let mut future = c.clone();
    future.steps[5].change.notes.push(' ');
    future
        .admission_requirements
        .insert("partition_gateway".into(), "tampered".into());
    assert_eq!(scope(&c, 32).unwrap(), scope(&future, 32).unwrap());
    assert_ne!(scope(&c, 35).unwrap(), scope(&future, 35).unwrap());
}
