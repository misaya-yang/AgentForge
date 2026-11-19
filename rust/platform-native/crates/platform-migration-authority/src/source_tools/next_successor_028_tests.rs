//! Source-only tests for the fixed 027 -> 028 contract factory.
use super::*;

const CONTRACT: &str =
    include_str!("../../../../../../database/proposals/price-book-v2-contract-028.sql");

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

fn predecessor_source()
-> crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture {
    crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(27)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("agentforge-028-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn proposal_is_hash_pinned_and_contains_only_the_two_contract_drops() {
    let fixture = predecessor_source();
    assert_eq!(safe::sha256_bytes(CONTRACT.as_bytes()), SQL_SHA256);
    let bytes = require_proposal(fixture.root()).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let ddl = text
        .lines()
        .filter(|line| line.starts_with("ALTER TABLE "))
        .collect::<Vec<_>>();
    assert_eq!(
        ddl,
        [
            "ALTER TABLE gateway.price_book_v2_terms DROP CONSTRAINT price_book_v2_terms_component_check;",
            "ALTER TABLE gateway.price_book_v2_usage_receipts DROP CONSTRAINT price_book_v2_usage_receipts_check;"
        ]
    );
    assert!(text.contains(TERMS_EXPANDED_CHECK));
    assert!(text.contains(RECEIPTS_EXPANDED_CHECK));
    assert!(text.contains("convalidated"));
    reject_embedded_transactions(text, "platform_upgrade_price_book_contract_028").unwrap();
}

#[test]
fn measured_027_identity_and_every_predecessor_artifact_are_exactly_pinned() {
    let fixture = predecessor_source();
    let root = fixture.root();
    let values = [
        APPROVED_027_LOCK_SHA256,
        APPROVED_027_MANIFEST_SHA256,
        APPROVED_027_REFERENCE_SHA256,
        APPROVED_027_MEASUREMENT_SHA256,
        APPROVED_027_ACTIVATION_SHA256,
        APPROVED_027_SOURCE_PLAN_SHA256,
    ];
    assert!(values.iter().all(Option::is_some));
    let pins = approved_027().unwrap();
    assert!(valid_sha256(pins.lock_sha256));
    assert!(valid_sha256(pins.manifest_sha256));
    assert!(valid_sha256(pins.reference_sha256));
    assert!(valid_sha256(pins.measurement_sha256));
    assert!(valid_sha256(pins.activation_sha256));
    assert!(valid_sha256(pins.source_plan_sha256));
    let candidate = validate_source(root).unwrap();
    assert_eq!(candidate.epoch.epoch, CANDIDATE_EPOCH);
    assert_eq!(candidate.epoch.changes.len(), CANDIDATE_EPOCH as usize);
    assert_eq!(candidate.package_lock_sha256, pins.lock_sha256);
    assert_eq!(
        candidate.plan["predecessor_manifest_sha256"],
        pins.manifest_sha256
    );
    assert_eq!(
        candidate.predecessor_reference_sha256,
        pins.reference_sha256
    );
    assert_eq!(
        candidate.predecessor_measurement_sha256,
        pins.measurement_sha256
    );
    assert_eq!(
        candidate.predecessor_activation_sha256,
        pins.activation_sha256
    );
    assert_eq!(
        candidate.plan["predecessor_source_plan_sha256"],
        pins.source_plan_sha256
    );
}

#[test]
fn fixed_change_is_transactional_and_preserves_the_027_feature_surface() {
    let spec = change();
    assert_eq!(spec.sequence, CANDIDATE_EPOCH);
    assert_eq!(spec.name, "platform_upgrade_price_book_contract_028");
    assert_eq!(spec.file, SQL);
    assert_eq!(spec.sha256, SQL_SHA256);
    assert_eq!(
        spec.transaction_mode,
        manifest::TransactionMode::Transactional
    );
    assert_eq!(spec.preconditions.len(), 2);
    assert!(spec.preconditions[1].contains(TERMS_EXPANDED_CHECK));
    assert!(spec.preconditions[1].contains(RECEIPTS_EXPANDED_CHECK));
    assert_eq!(spec.postconditions.len(), 2);
    assert!(spec.postconditions[0].contains(TERMS_OLD_CHECK));
    assert!(spec.postconditions[0].contains(RECEIPTS_OLD_CHECK));
    for table in RETAINED_TABLES {
        assert!(spec.postconditions[1].contains(table));
    }
    for function in RETAINED_FUNCTIONS {
        assert!(spec.postconditions[1].contains(function));
    }
}

#[test]
fn preview_generation_is_external_and_unactivated_from_the_exact_027_fixture() {
    let fixture = predecessor_source();
    let source = fixture.root();
    let before = manifest::file_sha256(&source.join(MANIFEST)).unwrap();
    let scratch = Scratch::new("preview");
    let preview = scratch.0.join("preview");
    let receipt = materialize(&source, &preview).unwrap();
    assert_eq!(receipt["candidate_epoch"], CANDIDATE_EPOCH);
    assert_eq!(receipt["source_activation"], false);
    assert_eq!(receipt["database_actions"], false);
    assert_eq!(
        verify_preview(&source, &preview, false)
            .unwrap()
            .plan_sha256,
        validate_source(&source).unwrap().plan_sha256
    );
    assert_eq!(
        manifest::file_sha256(&source.join(MANIFEST)).unwrap(),
        before
    );
    assert!(!epoch_file(&source, SQL).exists());
}

#[test]
fn active_028_verifier_accepts_the_real_archived_source() {
    let source =
        crate::source_tools::next_successor::historical_test_fixtures::HistoricalSourceFixture::new(
            28,
        );
    let receipt = verify_activated_source(source.root()).unwrap();
    assert_eq!(receipt["candidate_epoch"], CANDIDATE_EPOCH);
    assert_eq!(receipt["status"], "activated_source_chain");
}

#[test]
fn activated_verifier_rejects_the_exact_027_predecessor_fixture() {
    let fixture = predecessor_source();
    assert!(verify_activated_source(fixture.root()).is_err());
}

#[test]
fn unchanged_metadata_verifier_rejects_acl_and_extension_drift() {
    let baseline = json!({
        "acl_compatibility": {
            "revision": "r1", "manifest_sha256": "a", "delta_line": ACL_DELTA,
            "removed_delta_count": 1, "base_acl_sha256": "b", "effective_acl_sha256": "c",
            "base_acl_lines": ["same"], "raw_acl_lines": ["same"]
        },
        "fingerprints": {"acl":"b", "extensions":"d", "reference_data":"e", "structural":"f"},
        "raw_fingerprints": {"acl":"g", "extensions":"h", "reference_data":"i", "structural":"j"}
    });
    let mut same = baseline.clone();
    same["raw_fingerprints"]["structural"] = json!("changed");
    assert!(measurement::verify_unchanged_acl_and_metadata(&baseline, &same).is_ok());
    same["raw_fingerprints"]["extensions"] = json!("drift");
    assert!(measurement::verify_unchanged_acl_and_metadata(&baseline, &same).is_err());
}
