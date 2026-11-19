//! Source-only tests for the fixed 026 -> 027 authoring factory.
use super::*;

const CONTRACT_028: &str =
    include_str!("../../../../../../database/proposals/price-book-v2-contract-028.sql");

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("agentforge-027-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn predecessor_source()
-> crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture {
    crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(
        PREDECESSOR_EPOCH,
    )
}

#[test]
fn fixed_sources_compose_one_exact_027_change_from_the_activated026_pins() {
    let fixture = predecessor_source();
    let root = fixture.root();
    let candidate = validate_source(&root).unwrap();
    assert_eq!(candidate.epoch.epoch, CANDIDATE_EPOCH);
    assert_eq!(candidate.epoch.changes.len(), CANDIDATE_EPOCH as usize);
    assert_eq!(candidate.package_lock_sha256, APPROVED_026_LOCK_SHA256);
    assert_eq!(
        candidate.plan["predecessor_manifest_sha256"],
        APPROVED_026_MANIFEST_SHA256
    );
    assert_eq!(candidate.plan["fingerprint_status"], "not_measured");
    assert_eq!(
        candidate.plan["dependency_order"].as_array().unwrap().len(),
        5
    );
    assert_eq!(
        candidate.plan["existing_table_deltas"]["gateway.usage_records"],
        "make input/output token facts nullable; add four stored generated request-meter columns and add usage_records_meter_v1 NOT VALID then VALIDATE"
    );
    assert_eq!(
        candidate.plan["existing_table_deltas"]["gateway.price_book_v2_terms"],
        "retain the 026 component check and add a separately named request expansion CHECK as NOT VALID then VALIDATE"
    );
    assert_eq!(
        candidate.plan["existing_table_deltas"]["gateway.price_book_v2_usage_receipts"],
        "retain the 026 receipt check and add a separately named per_request expansion CHECK as NOT VALID then VALIDATE"
    );
    assert_eq!(
        candidate.plan["existing_table_deltas"]["gateway.users"],
        "add auth_epoch bigint DEFAULT 0; add and validate a non-null/nonnegative CHECK before SET NOT NULL"
    );
    assert_eq!(
        candidate.plan["migration_policy"]["phase"],
        "027_expansion_only_v2"
    );
    assert_eq!(
        candidate.plan["migration_policy"]["deferred_028_contract"]["proposal"],
        CONTRACT_028_PROPOSAL
    );
    assert_eq!(
        candidate.plan["migration_policy"]["deferred_028_contract"]["proposal_sha256"],
        CONTRACT_028_SHA256
    );
    assert_eq!(
        candidate.plan["migration_policy"]["deferred_028_contract"]["request_metered_model_enablement"],
        "remain gated until the 028 contract completes"
    );
    assert_eq!(
        candidate.plan["proposal_inputs"].as_array().unwrap().len(),
        5
    );
    assert_eq!(candidate.sql_sha256, SQL_SHA256);
    let sql = expected_sql(&root).unwrap();
    assert_eq!(safe::sha256_bytes(&sql), SQL_SHA256);
    reject_embedded_transactions(
        std::str::from_utf8(&sql).unwrap(),
        "platform_upgrade_native_contracts_027",
    )
    .unwrap();
}

#[test]
fn v2_keeps_old_price_book_checks_and_stages_all_new_checks() {
    let fixture = predecessor_source();
    let sql = String::from_utf8(expected_sql(fixture.root()).unwrap()).unwrap();
    assert!(!sql.contains("DROP CONSTRAINT price_book_v2_terms_component_check"));
    assert!(!sql.contains("DROP CONSTRAINT price_book_v2_usage_receipts_check"));

    for (table, constraint) in [
        ("gateway.price_book_v2_terms", TERMS_REQUEST_EXPANSION_CHECK),
        (
            "gateway.price_book_v2_usage_receipts",
            RECEIPTS_REQUEST_EXPANSION_CHECK,
        ),
        ("gateway.usage_records", USAGE_RECORDS_METER_CHECK),
        ("gateway.users", AUTH_EPOCH_EXPANSION_CHECK),
    ] {
        let add = sql
            .find(&format!("ALTER TABLE {table} ADD CONSTRAINT {constraint}"))
            .unwrap();
        let not_valid = sql[add..].find("NOT VALID;").unwrap() + add;
        let validate = sql[not_valid..]
            .find(&format!(
                "ALTER TABLE {table} VALIDATE CONSTRAINT {constraint};"
            ))
            .unwrap()
            + not_valid;
        assert!(
            add < not_valid && not_valid < validate,
            "{table}.{constraint}"
        );
    }

    let add_epoch = sql
        .find("ALTER TABLE gateway.users ADD COLUMN auth_epoch bigint DEFAULT 0;")
        .unwrap();
    let add_epoch_check = sql
        .find("ALTER TABLE gateway.users ADD CONSTRAINT enterprise_users_auth_epoch_v2_check")
        .unwrap();
    let validate_epoch = sql
        .find("ALTER TABLE gateway.users VALIDATE CONSTRAINT enterprise_users_auth_epoch_v2_check;")
        .unwrap();
    let set_not_null = sql
        .find("ALTER TABLE gateway.users ALTER COLUMN auth_epoch SET NOT NULL;")
        .unwrap();
    assert!(add_epoch < add_epoch_check && add_epoch_check < validate_epoch);
    assert!(validate_epoch < set_not_null);
    assert!(!sql.contains("ADD COLUMN auth_epoch bigint NOT NULL DEFAULT 0"));
}

#[test]
fn unnumbered_028_proposal_only_contracts_the_two_retained_checks() {
    assert_eq!(
        safe::sha256_bytes(CONTRACT_028.as_bytes()),
        "d6b47a4abd6427f2a788f9447750a0201787dc9e64bcb574dca2deaecf2ca3f4"
    );
    assert!(
        CONTRACT_028.contains("price_book_v2_terms_component_request_v2_check' AND convalidated")
    );
    assert!(
        CONTRACT_028.contains("price_book_v2_usage_receipts_request_v2_check' AND convalidated")
    );
    let drops = CONTRACT_028
        .lines()
        .filter(|line| line.starts_with("ALTER TABLE "))
        .collect::<Vec<_>>();
    assert_eq!(
        drops,
        [
            "ALTER TABLE gateway.price_book_v2_terms DROP CONSTRAINT price_book_v2_terms_component_check;",
            "ALTER TABLE gateway.price_book_v2_usage_receipts DROP CONSTRAINT price_book_v2_usage_receipts_check;"
        ]
    );
}

#[test]
fn privilege_allowlist_is_gateway_only_and_matches_the_structural_delta() {
    let additions = acl::expected_acl_additions();
    assert_eq!(additions.len(), 75);
    assert!(additions.contains(
        "column_acl:gateway.knowledge_model_calls.observation:gateway:UPDATE:grantopt=0"
    ));
    assert!(additions.contains(
        "function_acl:gateway.reserve_knowledge_model_call(p_tenant text, p_user text, p_call text, p_hash text, p_lease uuid, p_token jsonb, p_body jsonb, p_snapshot jsonb, p_price jsonb, p_input bigint, p_output bigint, p_cost bigint):gateway:EXECUTE:grantopt=0"
    ));
    assert!(additions.contains("acl:gateway.enterprise_scim_role_map:gateway:SELECT:grantopt=0"));
    assert!(
        !additions
            .iter()
            .any(|line| line.contains(":runtime:") || line.contains(":knowledge_worker:"))
    );
}

#[test]
fn materialize_and_verify_keep027_external_and_unactivated() {
    let fixture = predecessor_source();
    let root = fixture.root();
    let source_manifest = manifest::file_sha256(&root.join(MANIFEST)).unwrap();
    let scratch = Scratch::new("preview");
    let preview = scratch.0.join("preview");
    let marker = materialize(&root, &preview).unwrap();
    assert_eq!(marker["candidate_epoch"], CANDIDATE_EPOCH);
    assert_eq!(marker["source_activation"], false);
    assert_eq!(marker["database_actions"], false);
    assert_eq!(
        verify_preview(&root, &preview, false).unwrap().plan_sha256,
        validate_source(&root).unwrap().plan_sha256
    );
    assert_eq!(
        manifest::file_sha256(&root.join(MANIFEST)).unwrap(),
        source_manifest
    );
    assert!(!epoch_file(&root, SQL).exists());
    assert!(verify_preview(&root, &preview, true).is_err());
}

#[test]
fn copied_source_refuses_proposal_drift_without_touching_the_live_source() {
    let live_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let live_manifest_sha = manifest::file_sha256(&live_root.join(MANIFEST)).unwrap();
    let fixture = predecessor_source();
    let root = fixture.root();
    let scratch = Scratch::new("tamper");
    let source = scratch.0.join("source");
    fs::create_dir(&source).unwrap();
    safe::copy_tree(&root.join("database"), &source.join("database")).unwrap();
    fs::create_dir(source.join("docs")).unwrap();
    safe::copy_tree(&root.join("docs/design"), &source.join("docs/design")).unwrap();
    let input = source.join(PROPOSAL_INPUTS[4].path);
    let mut bytes = fs::read(&input).unwrap();
    bytes.push(b' ');
    fs::write(&input, bytes).unwrap();
    assert!(validate_source(&source).is_err());
    assert_eq!(
        manifest::load_epoch_manifest(&root.join(MANIFEST))
            .unwrap()
            .epoch,
        PREDECESSOR_EPOCH
    );
    assert_eq!(
        manifest::file_sha256(&live_root.join(MANIFEST)).unwrap(),
        live_manifest_sha
    );
}

#[test]
fn activated_verifier_never_accepts_the_unactivated026_source() {
    let fixture = predecessor_source();
    assert!(verify_activated_source(fixture.root()).is_err());
}
