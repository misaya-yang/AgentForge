use super::*;

fn change(mode: TransactionMode) -> ChangeSpec {
    ChangeSpec {
        sequence: 25,
        name: "fixture_successor".into(),
        file: "025_fixture_successor.sql".into(),
        sha256: "a".repeat(64),
        owner: "owner".into(),
        transaction_mode: mode,
        rollback_class: manifest::RollbackClass::ForwardFixOnly,
        preconditions: vec![],
        postconditions: vec![],
        timeout_seconds: 30,
        lock_budget_seconds: 5,
        resume_handler: (mode == TransactionMode::NonTransactional)
            .then(|| "resume_fixture".into()),
        repair_handler: (mode == TransactionMode::NonTransactional)
            .then(|| "repair_fixture".into()),
        notes: String::new(),
    }
}

fn owner_checked_change(mode: TransactionMode) -> ChangeSpec {
    let mut candidate = change(mode);
    candidate.preconditions = vec![APPROVED_OWNER_PRECONDITION.into()];
    candidate
}

fn blocked(findings: &[Finding], code: &str) -> bool {
    findings
        .iter()
        .any(|finding| finding.severity == "blocked" && finding.code == code)
}

#[test]
fn archived028_lint_keeps_real_n1_compatibility_unrun_until_driver_executes() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    // Restore the complete real028 locked database inventory. This is historical
    // source integrity evidence, not a database compatibility receipt.
    struct LintInputs(PathBuf);
    impl Drop for LintInputs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let inputs = LintInputs(
        std::env::temp_dir().join(format!("migration-lint-028-{}", uuid::Uuid::new_v4())),
    );
    let epoch_dir = inputs
        .0
        .join("database/migrations")
        .join(DEFAULT_BASELINE_ID);
    fs::create_dir_all(&epoch_dir).unwrap();
    let archived =
        repository.join("docs/design/canonical-schema-successor-029-approved28-manifest.yml");
    assert_eq!(manifest::file_sha256(&archived).unwrap(), ai_platform_migration_authority::source_tools::next_successor_029::APPROVED_028_MANIFEST_SHA256.unwrap());
    let lock_path =
        repository.join("docs/design/canonical-schema-successor-029-approved28-package-lock.json");
    assert_eq!(
        manifest::file_sha256(&lock_path).unwrap(),
        ai_platform_migration_authority::source_tools::next_successor_029::APPROVED_028_LOCK_SHA256
            .unwrap()
    );
    let lock: serde_json::Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    for (relative, expected) in lock["files_sha256"].as_object().unwrap() {
        let source = if relative == "migrations/2026_08_post_kb_v1/manifest.yml" {
            archived.clone()
        } else {
            repository.join("database").join(relative)
        };
        assert_eq!(
            manifest::file_sha256(&source).unwrap(),
            expected.as_str().unwrap()
        );
        let destination = inputs.0.join("database").join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
    fs::copy(
        lock_path,
        inputs
            .0
            .join("database/authority/package-authority-lock.json"),
    )
    .unwrap();
    ai_platform_migration_authority::distribution::verify(
        &inputs.0.join("database"),
        DEFAULT_BASELINE_ID,
    )
    .unwrap();
    let manifest = manifest::load_epoch_manifest(&epoch_dir.join("manifest.yml")).unwrap();
    assert_eq!(manifest.epoch, 28);
    let report = lint_source(&inputs.0, 28).unwrap();
    assert_eq!(report.n1_compatibility, "not_run");
    assert!(report.n1_rehearsal_required);
    assert!(!report.release_eligible);
    assert!(!report.has_blockers());
    assert_ne!(
        report.status,
        "n1_application_compatibility_executed_passed"
    );
}

#[test]
fn fixed029_schema_facts_are_recognized_without_claiming_n1_acceptance() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let epoch = manifest::load_epoch_manifest(
        &repository.join("database/migrations/2026_08_post_kb_v1/manifest.yml"),
    )
    .unwrap();
    let change = &epoch.changes[28];
    let sql = fs::read_to_string(
        repository
            .join("database/migrations/2026_08_post_kb_v1")
            .join(&change.file),
    )
    .unwrap();
    let (findings, phase) = lint_sql(&sql, change).unwrap();
    assert_eq!(phase, "expand");
    assert!(
        !findings.iter().any(|f| f.severity == "blocked"),
        "{findings:?}"
    );
    assert!(findings.iter().any(|f| f.severity == "requires_n1"));
    assert!(reviewed_schema::exact029(&sql, change));
    let mut altered = change.clone();
    altered.preconditions[0].push(' ');
    let (findings, _) = lint_sql(&sql, &altered).unwrap();
    assert!(blocked(
        &findings,
        "dynamic_sql_inside_migration_body_is_not_reviewable"
    ));
    assert!(blocked(
        &findings,
        "expand_and_contract_must_be_separate_successors"
    ));
    let drift = sql.replace(" VALIDATE CONSTRAINT ", " DROP CONSTRAINT ");
    assert!(!reviewed_schema::exact029(&drift, change));
    let (findings, _) = lint_sql(&format!("{sql}\n"), change).unwrap();
    assert!(blocked(
        &findings,
        "add_not_null_requires_expand_backfill_contract"
    ));
    assert!(blocked(
        &findings,
        "unique_constraint_requires_concurrent_unique_index"
    ));
}

#[test]
fn source_owned_service_steps_keep_split_contract_and_n1_gates() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let catalog: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/design/proposals/service-successors/catalog.json"
    ))
    .unwrap();
    for step in catalog["steps"].as_array().unwrap() {
        let change: ChangeSpec = serde_json::from_value(step["change"].clone()).unwrap();
        let sql =
            fs::read_to_string(repository.join(step["sql_source"].as_str().unwrap())).unwrap();
        let (findings, phase) = lint_sql(&sql, &change).unwrap();
        assert!(
            !findings.iter().any(|f| f.severity == "blocked"),
            "sequence{}: {findings:?}",
            change.sequence
        );
        assert_eq!(phase, step["compatibility_phase"].as_str().unwrap());
        assert!(findings.iter().any(|f| f.severity == "requires_n1"));
        let mut altered = change.clone();
        altered.notes.push(' ');
        let (findings, _) = lint_sql(&sql, &altered).unwrap();
        if matches!(change.sequence, 32 | 34) {
            assert!(blocked(
                &findings,
                "set_not_null_requires_prevalidated_check"
            ));
        } else {
            assert!(blocked(
                &findings,
                "dynamic_sql_inside_migration_body_is_not_reviewable"
            ));
        }
    }
}

#[test]
fn comments_and_literals_do_not_hide_or_invent_ddl_findings() {
    let sql = r#"
        -- CREATE INDEX silently ignored in comments
        SELECT 'SET lock_timeout = 0';
        /* ALTER TABLE old_table DROP COLUMN old_value; */
    "#;
    let (findings, phase) = lint_sql(sql, &change(TransactionMode::Transactional)).unwrap();
    assert_eq!(phase, "expand");
    assert!(!findings.iter().any(|finding| finding.severity == "blocked"));
}

#[test]
fn rejects_nonconcurrent_indexes_and_transactional_concurrent_builds() {
    let (plain, _) = lint_sql(
        "CREATE INDEX events_created_at_idx ON assistant.events(created_at);",
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert!(blocked(&plain, "create_index_requires_concurrently"));

    let empty_table = "CREATE TABLE assistant.new_events (event_id uuid NOT NULL); CREATE INDEX new_events_idx ON assistant.new_events(event_id);";
    let (empty_table_findings, _) =
        lint_sql(empty_table, &change(TransactionMode::Transactional)).unwrap();
    assert!(!blocked(
        &empty_table_findings,
        "create_index_requires_concurrently"
    ));

    let existing_table = "CREATE TABLE assistant.new_events (event_id uuid NOT NULL); SELECT 1; CREATE INDEX new_events_idx ON assistant.new_events(event_id);";
    let (existing_table_findings, _) =
        lint_sql(existing_table, &change(TransactionMode::Transactional)).unwrap();
    assert!(blocked(
        &existing_table_findings,
        "create_index_requires_concurrently"
    ));

    for write in [
        "INSERT INTO assistant.new_events(event_id) VALUES ('00000000-0000-0000-0000-000000000001');",
        "COPY assistant.new_events(event_id) FROM STDIN;",
    ] {
        let with_write = format!(
            "CREATE TABLE assistant.new_events (event_id uuid NOT NULL); {write} CREATE INDEX new_events_idx ON assistant.new_events(event_id);"
        );
        let (findings, _) = lint_sql(&with_write, &change(TransactionMode::Transactional)).unwrap();
        assert!(blocked(&findings, "create_index_requires_concurrently"));
    }

    let nontransactional_empty_table = "CREATE TABLE assistant.new_events (event_id uuid NOT NULL); CREATE INDEX new_events_idx ON assistant.new_events(event_id);";
    let (nontransactional_findings, _) = lint_sql(
        nontransactional_empty_table,
        &change(TransactionMode::NonTransactional),
    )
    .unwrap();
    assert!(blocked(
        &nontransactional_findings,
        "create_index_requires_concurrently"
    ));

    let (transactional, _) = lint_sql(
        "CREATE INDEX CONCURRENTLY events_created_at_idx ON assistant.events(created_at);",
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert!(blocked(
        &transactional,
        "concurrent_index_requires_nontransactional_recovery_contract"
    ));

    let (recoverable, _) = lint_sql(
        "CREATE INDEX CONCURRENTLY events_created_at_idx ON assistant.events(created_at);",
        &change(TransactionMode::NonTransactional),
    )
    .unwrap();
    assert!(
        !recoverable
            .iter()
            .any(|finding| finding.severity == "blocked")
    );

    let mismatched_quoted_name = "CREATE UNIQUE INDEX CONCURRENTLY \"events_key_idx\" ON assistant.events(event_id); ALTER TABLE assistant.events ADD CONSTRAINT events_key UNIQUE USING INDEX \"EVENTS_KEY_IDX\";";
    let (findings, _) = lint_sql(
        mismatched_quoted_name,
        &change(TransactionMode::NonTransactional),
    )
    .unwrap();
    assert!(blocked(
        &findings,
        "unique_constraint_requires_concurrent_unique_index"
    ));
}

#[test]
fn detects_lock_override_rewrites_unvalidated_constraints_and_opaque_dynamic_ddl() {
    for sql in [
        "SET LOCAL lock_timeout = '0';",
        "SET LOCAL \"lock_timeout\" = '0';",
        "ALTER ROLE ai_gateway_gateway SET statement_timeout = '0';",
        "SELECT set_config('lock_timeout', '0', true);",
        "SELECT set_config('lock_' || 'timeout', '0', true);",
        "SELECT set_config('search_path','pg_catalog',true), set_config('lock_' || 'timeout','0',true);",
        "SELECT set_config('search_path','pg_catalog',true), set_config('statement_' || 'timeout','0',true);",
        "SELECT set_config('search_path','pg_catalog',true);",
        "SELECT set_config('role','ai_gateway_gateway',true);",
        "SELECT set_config('session_authorization','ai_gateway_gateway',true);",
        "SET search_path = pg_catalog, assistant;",
        "ALTER DATABASE agentforge SET search_path = pg_catalog, public;",
        "SELECT set_config(current_setting('app.timeout_name'), '0', true);",
        "ALTER TABLE assistant.events ADD COLUMN new_value text NOT NULL DEFAULT '';",
        "ALTER TABLE assistant.events ADD COLUMN sequence_value bigint DEFAULT nextval('assistant.seq');",
        "ALTER TABLE assistant.events ADD CONSTRAINT events_check CHECK (value > 0);",
        "ALTER TABLE assistant.events ADD CHECK (value > 0) NOT VALID;",
        "ALTER TABLE assistant.events ADD CONSTRAINT CHECK (value > 0) NOT VALID;",
        "ALTER TABLE assistant.events ALTER COLUMN value TYPE bigint;",
        "DO $$ BEGIN EXECUTE 'CREATE INDEX events_idx ON assistant.events(value)'; END $$;",
        "CREATE FUNCTION assistant.migration_helper() RETURNS void LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'DROP TABLE assistant.events'; END $$;",
    ] {
        let (findings, _) = lint_sql(sql, &change(TransactionMode::Transactional)).unwrap();
        assert!(
            findings.iter().any(|finding| finding.severity == "blocked"),
            "unsafe SQL was accepted: {sql}"
        );
    }

    let (safe_multiple_settings, _) = lint_sql(
        "SELECT set_config('application_name','migration',true), set_config('TimeZone','UTC',true);",
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert!(!blocked(
        &safe_multiple_settings,
        "migration_sql_dynamically_controls_authority_timeouts"
    ));
}

#[test]
fn approves_only_exact_role_prefix_grants_after_verified_owner_check() {
    let grant = "DO $grants$ DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); BEGIN IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF; EXECUTE format('GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I',prefix||'gateway'); END $grants$;";
    let (accepted, _) =
        lint_sql(grant, &owner_checked_change(TransactionMode::Transactional)).unwrap();
    assert!(!blocked(
        &accepted,
        "dynamic_sql_inside_migration_body_is_not_reviewable"
    ));

    let no_owner_proof = change(TransactionMode::Transactional);
    let (unproved, _) = lint_sql(grant, &no_owner_proof).unwrap();
    assert!(blocked(
        &unproved,
        "dynamic_sql_inside_migration_body_is_not_reviewable"
    ));

    for unsafe_grant in [
        "EXECUTE format('GRANT SELECT ON gateway.unreviewed_table TO %I',prefix||'gateway');",
        "EXECUTE format('GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I',prefix||'runtime');",
        "EXECUTE format('GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I'||current_user,prefix||'gateway');",
        "EXECUTE format('GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I',current_setting('app.role'));",
    ] {
        let sql = format!(
            "DO $grants$ DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); BEGIN IF prefix !~ '^[a-z][a-z0-9_]{{0,20}}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF; {unsafe_grant} END $grants$;"
        );
        let (findings, _) =
            lint_sql(&sql, &owner_checked_change(TransactionMode::Transactional)).unwrap();
        assert!(
            blocked(
                &findings,
                "dynamic_sql_inside_migration_body_is_not_reviewable"
            ),
            "unsafe dynamic grant was accepted: {unsafe_grant}"
        );
    }

    let unbound_prefix = "DO $grants$ DECLARE prefix text:=current_setting('app.role'); BEGIN IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF; EXECUTE format('GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I',prefix||'gateway'); END $grants$;";
    let (unbound_findings, _) = lint_sql(
        unbound_prefix,
        &owner_checked_change(TransactionMode::Transactional),
    )
    .unwrap();
    assert!(blocked(
        &unbound_findings,
        "dynamic_sql_inside_migration_body_is_not_reviewable"
    ));
}

#[test]
fn authorization_identity_cannot_change_after_owner_precondition() {
    for sql in [
        "SET ROLE ai_gateway_gateway;",
        "SET LOCAL ROLE ai_gateway_gateway;",
        "SET SESSION AUTHORIZATION ai_gateway_gateway;",
        "RESET ROLE;",
        "DO $$ BEGIN SET ROLE ai_gateway_gateway; END $$;",
    ] {
        let (findings, _) =
            lint_sql(sql, &owner_checked_change(TransactionMode::Transactional)).unwrap();
        assert!(
            blocked(
                &findings,
                "migration_sql_cannot_change_authorization_identity"
            ),
            "authorization identity change was accepted: {sql}"
        );
    }
}

#[test]
fn migration_cannot_end_or_replace_the_runner_transaction() {
    for sql in [
        "BEGIN;",
        "COMMIT;",
        "ROLLBACK;",
        "START TRANSACTION;",
        "SAVEPOINT migration_step;",
        "RELEASE SAVEPOINT migration_step;",
        "DO $$ BEGIN COMMIT; END $$;",
    ] {
        let (findings, _) =
            lint_sql(sql, &owner_checked_change(TransactionMode::Transactional)).unwrap();
        assert!(
            blocked(
                &findings,
                "migration_sql_cannot_control_transaction_boundary"
            ),
            "transaction boundary control was accepted: {sql}"
        );
    }
}

#[test]
fn current_026_fixed_bundle_uses_only_new_empty_table_indexes_and_bounded_grants() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let proposal = fs::read_to_string(
        repository.join("docs/design/platform-upgrade-native-contracts-026.sql.proposal"),
    )
    .unwrap();
    let (findings, phase) = lint_sql(
        &proposal,
        &owner_checked_change(TransactionMode::Transactional),
    )
    .unwrap();
    assert_eq!(phase, "expand");
    assert!(
        !findings.iter().any(|finding| finding.severity == "blocked"),
        "026 must satisfy the strict static gate: {findings:?}"
    );
}

#[test]
fn requires_concurrent_unique_index_and_expand_validate_constraint_sequence() {
    let missing_unique =
        "ALTER TABLE assistant.events ADD CONSTRAINT events_key UNIQUE (event_id);";
    let (findings, _) = lint_sql(missing_unique, &change(TransactionMode::Transactional)).unwrap();
    assert!(blocked(
        &findings,
        "unique_constraint_requires_concurrent_unique_index"
    ));

    let quoted_missing_unique =
        "ALTER TABLE assistant.events ADD CONSTRAINT \"events_key\" UNIQUE (event_id);";
    let (findings, _) = lint_sql(
        quoted_missing_unique,
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert!(blocked(
        &findings,
        "unique_constraint_requires_concurrent_unique_index"
    ));

    let expanded = "CREATE UNIQUE INDEX CONCURRENTLY events_key_idx ON assistant.events(event_id); ALTER TABLE assistant.events ADD CONSTRAINT events_key UNIQUE USING INDEX events_key_idx; ALTER TABLE assistant.events ADD CONSTRAINT events_value_check CHECK (value > 0) NOT VALID; ALTER TABLE assistant.events VALIDATE CONSTRAINT events_value_check;";
    let (findings, _) = lint_sql(expanded, &change(TransactionMode::NonTransactional)).unwrap();
    assert!(!findings.iter().any(|finding| finding.severity == "blocked"));
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "successor_requires_executed_n1_rehearsal")
    );
}

#[test]
fn keeps_expand_and_contract_ddl_in_separate_successors() {
    let sql = "ALTER TABLE assistant.events ADD COLUMN new_value text; ALTER TABLE assistant.events DROP COLUMN old_value;";
    let (findings, phase) = lint_sql(sql, &change(TransactionMode::Transactional)).unwrap();
    assert_eq!(phase, "contract");
    assert!(blocked(
        &findings,
        "expand_and_contract_must_be_separate_successors"
    ));
    assert!(
        findings
            .iter()
            .any(|finding| finding.severity == "requires_n1")
    );
}

#[test]
fn classifies_object_drops_as_contract_and_rejects_cascade() {
    let (drop_type, phase) = lint_sql(
        "DROP TYPE assistant.old_status;",
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert_eq!(phase, "contract");
    assert!(drop_type.iter().any(|finding| {
        finding.severity == "requires_n1"
            && finding.code == "drop_object_requires_executed_n1_rehearsal"
    }));

    let (cascade, _) = lint_sql(
        "DROP TYPE assistant.old_status CASCADE;",
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert!(blocked(&cascade, "drop_cascade_is_forbidden_in_successor"));

    let (drop_index, phase) = lint_sql(
        "DROP INDEX CONCURRENTLY assistant.old_status_idx;",
        &change(TransactionMode::NonTransactional),
    )
    .unwrap();
    assert_eq!(phase, "contract");
    assert!(drop_index.iter().any(|finding| {
        finding.severity == "requires_n1"
            && finding.code == "index_removal_requires_executed_n1_rehearsal"
    }));

    let (mixed, phase) = lint_sql(
        "CREATE TYPE assistant.new_status AS ENUM ('queued', 'sent'); DROP TYPE assistant.old_status;",
        &change(TransactionMode::Transactional),
    )
    .unwrap();
    assert_eq!(phase, "contract");
    assert!(blocked(
        &mixed,
        "expand_and_contract_must_be_separate_successors"
    ));
}

#[test]
fn n1_driver_result_must_bind_the_executed_candidate_and_previous_application() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let manifest_path = repository
        .join("database/migrations")
        .join(DEFAULT_BASELINE_ID)
        .join("manifest.yml");
    let manifest = manifest::load_epoch_manifest(&manifest_path).unwrap();
    let source = candidate_source(&repository, manifest.epoch).unwrap();
    let options = N1RunOptions {
        previous_release_id: "gateway-n-minus-one".into(),
        previous_application: "/external/gateway-n-minus-one".into(),
        previous_application_sha256: "b".repeat(64),
        driver: "/external/n1-driver".into(),
        driver_sha256: "c".repeat(64),
        expected_database: "schema_successor_verify_20261017".into(),
        receipt_output: "/external/n1-receipt.json".into(),
    };
    let report = DriverResult {
        schema: DRIVER_SCHEMA.into(),
        status: "passed".into(),
        baseline_id: source.manifest.baseline_id.clone(),
        successor_sequence: source.change.sequence,
        candidate_sql_sha256: source.sql_sha256.clone(),
        candidate_manifest_sha256: source.manifest_sha256.clone(),
        compatibility_phase: "expand".into(),
        expected_database: options.expected_database.clone(),
        previous_release_id: options.previous_release_id.clone(),
        previous_application_sha256: options.previous_application_sha256.clone(),
        authority_applied_candidate: true,
        candidate_epoch_verified: true,
        previous_application_started: true,
        scenarios: vec![DriverScenario {
            id: "turn_create_and_read".into(),
            status: "passed".into(),
        }],
    };
    let bytes = serde_json::to_vec(&report).unwrap();
    assert!(validate_driver_result(&bytes, &source, &options, "expand").is_ok());

    let mut unbound_manifest = report;
    unbound_manifest.candidate_manifest_sha256 = "d".repeat(64);
    let bytes = serde_json::to_vec(&unbound_manifest).unwrap();
    assert!(validate_driver_result(&bytes, &source, &options, "expand").is_err());
}

#[test]
fn fixed027_grants_require_exact_template_and_unchanged_owner_boundary() {
    let template =
        "GRANT SELECT,INSERT ON gateway.routing_policy_versions,gateway.routing_decisions TO %I";
    for guard in [
        "IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid'",
        r"IF current_user NOT LIKE '%\_owner' ESCAPE '\' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid'",
    ] {
        let sql = format!(
            "DO $grants$ DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));principal text; BEGIN {guard}; END IF; principal:=prefix||'gateway'; EXECUTE format('{template}',principal); END $grants$;"
        );
        let owner = owner_checked_change(TransactionMode::Transactional);
        assert!(
            grants::is_approved_role_prefix_grant_body(
                sql.split("$grants$").nth(1).unwrap(),
                &owner
            )
            .unwrap()
        );
        for invalid in [
            sql.replace("gateway.routing_decisions", "gateway.users"),
            sql.replace(
                "principal:=prefix||'gateway'",
                "principal:=prefix||'runtime'",
            ),
            sql.replace("^[a-z][a-z0-9_]{0,20}_$", ".*"),
            sql.replace("THEN RAISE", "OR true THEN RAISE"),
        ] {
            let (findings, _) = lint_sql(&invalid, &owner).unwrap();
            assert!(blocked(
                &findings,
                "dynamic_sql_inside_migration_body_is_not_reviewable"
            ));
        }
        let (findings, _) = lint_sql(&sql, &change(TransactionMode::Transactional)).unwrap();
        assert!(blocked(
            &findings,
            "dynamic_sql_inside_migration_body_is_not_reviewable"
        ));
    }
}

#[test]
fn partition_installer_facts_do_not_allow_edited_maintenance_or_executed_ddl() {
    let catalog: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/design/proposals/service-successors/catalog.json"
    ))
    .unwrap();
    let step = &catalog["steps"][5];
    let spec: ChangeSpec = serde_json::from_value(step["change"].clone()).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let sql = fs::read_to_string(root.join(step["sql_source"].as_str().unwrap())).unwrap();
    assert!(reviewed_schema::partition_installer(&sql, &spec));
    for invalid in [
        format!("{sql}\n"),
        sql.replace("scope_sha256=p_scope_sha256", "scope_sha256 IS NOT NULL"),
        format!("{sql}\nCREATE INDEX CONCURRENTLY unsafe ON gateway.usage_records(created_at);"),
    ] {
        assert!(!reviewed_schema::partition_installer(&invalid, &spec));
        let (findings, _) = lint_sql(&invalid, &spec).unwrap();
        assert!(blocked(
            &findings,
            "dynamic_sql_inside_migration_body_is_not_reviewable"
        ));
    }
    let (findings, _) = lint_sql(&format!("{sql}\nSET lock_timeout=0;"), &spec).unwrap();
    assert!(blocked(&findings, "migration_sql_overrides_lock_timeout"));
}
