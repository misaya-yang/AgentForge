//! Fixed 027 -> 028 contract-only source factory for the two retained price-book checks.
//! The measured 027 identity must be installed from the real activated package before use.
use super::{measure_successors, next_successor as safe, next_successor_027};
use crate::{
    AuthorityError, Result, commands, distribution, manifest, runner::AuthorityPaths,
    sql::reject_embedded_transactions,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[path = "next_successor_028/activation.rs"]
mod activation;
#[path = "next_successor_028/measurement.rs"]
mod measurement;

pub use activation::{activation_bundle, verify_activated_source};
use measurement::verify_measured_files;
pub use measurement::{MeasureOptions, measure};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 27;
pub const CANDIDATE_EPOCH: i32 = 28;

// Filled only from the real, measured and activated 027 package. None is an
// intentional fail-closed state, never a synthetic hash or inferred identity.
pub const APPROVED_027_LOCK_SHA256: Option<&str> =
    Some("4809ae95335a98ff2f50ed486b47af59463643395dbcad0c8aa9bcda5f20b855");
pub const APPROVED_027_MANIFEST_SHA256: Option<&str> =
    Some("363d3224942be10b5cf5fd405d6c37bbb192beda08b252fe7c4e50ee34c9ead7");
pub const APPROVED_027_REFERENCE_SHA256: Option<&str> =
    Some("80051385a13d7ad1eef64ab504ef871f4c80b2a151b03bf81a063ff689ead324");
pub const APPROVED_027_MEASUREMENT_SHA256: Option<&str> =
    Some("7348b7d49ce48ff11231ade90e5feb96569cbd645b9d484fbb21d62b47b82506");
pub const APPROVED_027_ACTIVATION_SHA256: Option<&str> =
    Some("a9fb964735f86cedc41db33166837d3c9f5e4985750c25701e6c255ae3e95f17");
pub const APPROVED_027_SOURCE_PLAN_SHA256: Option<&str> =
    Some("f8ace131c7c90bc3175d7882f09c35bbe83048a8c566b2d6bcd3f03510a50fbf");

const SQL: &str = "028_price_book_v2_contract.sql";
const PROPOSAL: &str = "database/proposals/price-book-v2-contract-028.sql";
const REFERENCE: &str = "028_verification.json";
const MEASUREMENT: &str = "SUCCESSOR_028_MEASUREMENT.json";
const ACTIVATION: &str = "SUCCESSOR_028_ACTIVATION.json";
const MARKER: &str = "PREVIEW_028_NOT_ACTIVATED.json";
const LOCK: &str = "database/authority/package-authority-lock.json";
const MANIFEST: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-successor-028-approved27-package-lock.json";
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-successor-028-approved27-manifest.yml";
const TERMS_OLD_CHECK: &str = "price_book_v2_terms_component_check";
const TERMS_EXPANDED_CHECK: &str = "price_book_v2_terms_component_request_v2_check";
const RECEIPTS_OLD_CHECK: &str = "price_book_v2_usage_receipts_check";
const RECEIPTS_EXPANDED_CHECK: &str = "price_book_v2_usage_receipts_request_v2_check";
const PROVENANCE: &str = "Fixed contract-only 027 -> 028 successor. It drops only the two exact 026 price-book CHECKs retained by the measured 027 v2 expansion; the independently named, validated 027 v2 expansions remain installed. All model, request-metering, dispatch, enterprise-identity, and routing objects remain present. ACL, extension, and reference-data fingerprints must remain byte-identical.";
const TOOL: &str = "rust/platform-native/crates/platform-migration-authority/src/source_tools/next_successor_028.rs";
const SQL_SHA256: &str = "d6b47a4abd6427f2a788f9447750a0201787dc9e64bcb574dca2deaecf2ca3f4";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";

const RETAINED_TABLES: [&str; 17] = [
    "gateway.knowledge_model_calls",
    "gateway.knowledge_model_workload_nonces",
    "gateway.dispatch_token_reservations",
    "gateway.enterprise_tenant_policy",
    "gateway.enterprise_identity_links",
    "gateway.enterprise_scim_credentials",
    "gateway.enterprise_scim_users",
    "gateway.enterprise_scim_groups",
    "gateway.enterprise_scim_members",
    "gateway.enterprise_scim_role_map",
    "gateway.enterprise_security_state",
    "gateway.enterprise_passkeys",
    "gateway.enterprise_recovery_codes",
    "gateway.enterprise_session_assurance",
    "gateway.routing_policy_versions",
    "gateway.routing_policy_active",
    "gateway.routing_decisions",
];
const RETAINED_FUNCTIONS: [&str; 6] = [
    "gateway.reserve_knowledge_model_call",
    "gateway.settle_knowledge_model_call",
    "gateway.enterprise_user_epoch",
    "gateway.enterprise_assignment_epoch",
    "gateway.enterprise_role_policy_epoch",
    "gateway.enterprise_tenant_mfa_epoch",
];

#[derive(Clone, Copy)]
struct Approved027 {
    lock_sha256: &'static str,
    manifest_sha256: &'static str,
    reference_sha256: &'static str,
    measurement_sha256: &'static str,
    activation_sha256: &'static str,
    source_plan_sha256: &'static str,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub plan: Value,
    pub manifest_value: Value,
    pub epoch: manifest::EpochManifest,
    pub baseline: manifest::BaselineManifest,
    pub baseline_sha256: String,
    pub package_lock: Value,
    pub package_lock_sha256: String,
    pub plan_sha256: String,
    pub sql_sha256: String,
    pub predecessor_reference_sha256: String,
    pub predecessor_measurement_sha256: String,
    pub predecessor_activation_sha256: String,
}

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn epoch_relative(name: &str) -> String {
    format!("migrations/{BASELINE}/{name}")
}
fn epoch_file(root: &Path, name: &str) -> PathBuf {
    root.join("database").join(epoch_relative(name))
}
fn source_json(root: &Path, relative: &str) -> Result<Value> {
    serde_json::from_slice(&fs::read(safe::source_file(root, relative)?)?)
        .map_err(|_| refused("028 source JSON is malformed"))
}
fn lock_files(lock: &Value) -> Result<&serde_json::Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("028 predecessor lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("028 predecessor lock has no inventory"))
}
fn digest(value: &Value) -> Result<String> {
    Ok(safe::sha256_bytes(
        &serde_json::to_vec(value).map_err(|_| refused("028 plan serialization failed"))?,
    ))
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn approved_027() -> Result<Approved027> {
    let pins = Approved027 {
        lock_sha256: APPROVED_027_LOCK_SHA256
            .ok_or_else(|| refused("028 is blocked until the real measured 027 lock is pinned"))?,
        manifest_sha256: APPROVED_027_MANIFEST_SHA256.ok_or_else(|| {
            refused("028 is blocked until the real measured 027 manifest is pinned")
        })?,
        reference_sha256: APPROVED_027_REFERENCE_SHA256.ok_or_else(|| {
            refused("028 is blocked until the real 027 verification reference is pinned")
        })?,
        measurement_sha256: APPROVED_027_MEASUREMENT_SHA256.ok_or_else(|| {
            refused("028 is blocked until the real 027 measurement receipt is pinned")
        })?,
        activation_sha256: APPROVED_027_ACTIVATION_SHA256.ok_or_else(|| {
            refused("028 is blocked until the real 027 activation receipt is pinned")
        })?,
        source_plan_sha256: APPROVED_027_SOURCE_PLAN_SHA256
            .ok_or_else(|| refused("028 is blocked until the real measured 027 plan is pinned"))?,
    };
    if [
        pins.lock_sha256,
        pins.manifest_sha256,
        pins.reference_sha256,
        pins.measurement_sha256,
        pins.activation_sha256,
        pins.source_plan_sha256,
    ]
    .iter()
    .any(|hash| !valid_sha256(hash))
    {
        return Err(refused(
            "028 measured 027 identity contains an invalid SHA-256",
        ));
    }
    Ok(pins)
}
fn require_proposal(root: &Path) -> Result<Vec<u8>> {
    let path = safe::source_file(root, PROPOSAL)?;
    let bytes = fs::read(path)?;
    if safe::sha256_bytes(&bytes) != SQL_SHA256 {
        return Err(refused(
            "028 fixed two-check proposal differs from its approved digest",
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| refused("028 proposal is not UTF-8"))?;
    reject_embedded_transactions(text, "platform_upgrade_price_book_contract_028")?;
    Ok(bytes)
}

fn predecessor_owner_precondition() -> &'static str {
    "SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls))"
}
fn expansion_state_precondition() -> &'static str {
    "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_check' AND convalidated AND position('request' in pg_catalog.pg_get_constraintdef(oid))=0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_request_v2_check' AND convalidated AND position('request' in pg_catalog.pg_get_constraintdef(oid))>0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_check' AND convalidated AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))=0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_request_v2_check' AND convalidated AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))>0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.usage_records'::pg_catalog.regclass AND conname='usage_records_meter_v1' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.users'::pg_catalog.regclass AND conname='enterprise_users_auth_epoch_v2_check' AND convalidated)"
}
fn contraction_postcondition() -> &'static str {
    "SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_check') AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_check') AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_request_v2_check' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_request_v2_check' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.usage_records'::pg_catalog.regclass AND conname='usage_records_meter_v1' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.users'::pg_catalog.regclass AND conname='enterprise_users_auth_epoch_v2_check' AND convalidated)"
}
#[cfg(test)]
pub(crate) fn test_contraction_postcondition() -> &'static str {
    contraction_postcondition()
}
fn retained_feature_postcondition() -> String {
    let tables = RETAINED_TABLES
        .iter()
        .map(|table| format!("pg_catalog.to_regclass('{table}') IS NOT NULL"));
    let functions = RETAINED_FUNCTIONS.iter().map(|name| {
        format!("EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname||'.'||p.proname='{name}')")
    });
    format!(
        "SELECT {}",
        tables.chain(functions).collect::<Vec<_>>().join(" AND ")
    )
}
fn target_artifacts_absent(root: &Path) -> Result<()> {
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        match fs::symlink_metadata(epoch_file(root, name)) {
            Ok(_) => {
                return Err(refused(
                    "028 candidate artifact already exists in its source",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    for relative in [ARCHIVED_LOCK, ARCHIVED_MANIFEST] {
        match fs::symlink_metadata(root.join(relative)) {
            Ok(_) => {
                return Err(refused(
                    "028 predecessor archive already exists in its source",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn change() -> manifest::ChangeSpec {
    manifest::ChangeSpec {
        sequence: CANDIDATE_EPOCH,
        name: "platform_upgrade_price_book_contract_028".into(),
        file: SQL.into(),
        sha256: SQL_SHA256.into(),
        owner: "owner".into(),
        transaction_mode: manifest::TransactionMode::Transactional,
        rollback_class: manifest::RollbackClass::ForwardFixOnly,
        preconditions: vec![
            predecessor_owner_precondition().into(),
            expansion_state_precondition().into(),
        ],
        postconditions: vec![
            contraction_postcondition().into(),
            retained_feature_postcondition(),
        ],
        timeout_seconds: 600,
        lock_budget_seconds: 5,
        resume_handler: None,
        repair_handler: None,
        notes: "Contract only the two exact 026 price-book checks after measured 027 v2 has validated independently named expanded replacements. Preserve every 027 model, metering, dispatch, enterprise-identity, and routing object; ACL and nonstructural fingerprints must remain unchanged.".into(),
    }
}

fn require_predecessor(epoch: &manifest::EpochManifest) -> Result<()> {
    if epoch.baseline_id != BASELINE
        || epoch.epoch != PREDECESSOR_EPOCH
        || epoch.changes.len() != PREDECESSOR_EPOCH as usize
        || epoch
            .changes
            .iter()
            .enumerate()
            .any(|(index, change)| change.sequence != index as i32 + 1)
    {
        return Err(refused(
            "028 requires the exact complete activated 027 predecessor",
        ));
    }
    Ok(())
}

pub fn validate_source(root: &Path) -> Result<Candidate> {
    let pins = approved_027()?;
    let root = safe::canonical_directory(root, "028 source must be a nonsymlink directory")?;
    let sql = require_proposal(&root)?;
    let predecessor_activation = next_successor_027::verify_activated_source(&root)?;
    distribution::verify(&root.join("database"), BASELINE)?;
    let mut epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    require_predecessor(&epoch)?;
    let package_lock = source_json(&root, LOCK)?;
    let package_lock_sha256 = manifest::file_sha256(&safe::source_file(&root, LOCK)?)?;
    let predecessor_manifest_sha256 = manifest::file_sha256(&safe::source_file(&root, MANIFEST)?)?;
    let reference_sha256 = manifest::file_sha256(&epoch_file(&root, "027_verification.json"))?;
    let measurement_sha256 =
        manifest::file_sha256(&epoch_file(&root, "SUCCESSOR_027_MEASUREMENT.json"))?;
    let activation_sha256 =
        manifest::file_sha256(&epoch_file(&root, "SUCCESSOR_027_ACTIVATION.json"))?;
    if package_lock_sha256 != pins.lock_sha256
        || predecessor_manifest_sha256 != pins.manifest_sha256
        || reference_sha256 != pins.reference_sha256
        || measurement_sha256 != pins.measurement_sha256
        || activation_sha256 != pins.activation_sha256
        || predecessor_activation["source_plan_sha256"] != pins.source_plan_sha256
        || predecessor_activation["verification_ref_sha256"] != pins.reference_sha256
        || predecessor_activation["measurement_receipt_sha256"] != pins.measurement_sha256
    {
        return Err(refused(
            "028 predecessor differs from the exact measured 027 identity pins",
        ));
    }
    target_artifacts_absent(&root)?;
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &AuthorityPaths {
            database_dir: root.join("database"),
        },
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let mut manifest_value = source_json(&root, MANIFEST)?;
    let spec = change();
    manifest_value["epoch"] = json!(CANDIDATE_EPOCH);
    manifest_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("028 predecessor manifest has no change list"))?
        .push(serde_json::to_value(&spec).map_err(|_| refused("028 change serialization failed"))?);
    let plan = json!({
        "schema_version": "ai-platform/canonical-fixed-successor-policy/v1",
        "phase": "028_contract_only_after_measured_027_v2",
        "baseline_id": BASELINE,
        "predecessor_epoch": PREDECESSOR_EPOCH,
        "candidate_epoch": CANDIDATE_EPOCH,
        "predecessor_package_lock_sha256": package_lock_sha256,
        "predecessor_manifest_sha256": predecessor_manifest_sha256,
        "predecessor_verification_reference_sha256": reference_sha256,
        "predecessor_measurement_receipt_sha256": measurement_sha256,
        "predecessor_activation_receipt_sha256": activation_sha256,
        "predecessor_source_plan_sha256": pins.source_plan_sha256,
        "baseline_manifest_sha256": baseline_sha256,
        "proposal_path": PROPOSAL,
        "proposal_sha256": SQL_SHA256,
        "contractions": [
            {"table":"gateway.price_book_v2_terms","constraint":TERMS_OLD_CHECK},
            {"table":"gateway.price_book_v2_usage_receipts","constraint":RECEIPTS_OLD_CHECK}
        ],
        "required_validated_expansions": [TERMS_EXPANDED_CHECK, RECEIPTS_EXPANDED_CHECK],
        "preserved_functionality": {
            "tables": RETAINED_TABLES,
            "functions": RETAINED_FUNCTIONS,
            "acl_delta": "none",
            "extensions_delta": "none",
            "reference_data_delta": "none"
        },
        "database_actions_authorized": false,
        "fingerprint_status": "not_measured",
        "successor": {"change": spec}
    });
    let plan_sha256 = digest(&plan)?;
    epoch.epoch = CANDIDATE_EPOCH;
    epoch.changes.push(change());
    Ok(Candidate {
        plan,
        manifest_value,
        epoch,
        baseline,
        baseline_sha256,
        package_lock,
        package_lock_sha256,
        plan_sha256,
        sql_sha256: safe::sha256_bytes(&sql),
        predecessor_reference_sha256: reference_sha256,
        predecessor_measurement_sha256: measurement_sha256,
        predecessor_activation_sha256: activation_sha256,
    })
}

fn external_destination(source: &Path, preview: Option<&Path>, output: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(output)?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(refused("028 output must be new")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("028 output parent is missing"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("028 output name is missing"))?,
    );
    if absolute != resolved
        || absolute.starts_with(source)
        || source.starts_with(&absolute)
        || preview.is_some_and(|path| absolute.starts_with(path) || path.starts_with(&absolute))
    {
        return Err(refused("028 output must be external and disjoint"));
    }
    Ok(absolute)
}

fn common_receipt(
    candidate: &Candidate,
    manifest_sha: &str,
    status: &str,
    database: Option<&str>,
    reference: Option<&str>,
    measurement: Option<&str>,
) -> Value {
    json!({
        "schema": "ai-platform/canonical-fixed-successor-receipt/v1",
        "status": status,
        "baseline_id": BASELINE,
        "predecessor_epoch": PREDECESSOR_EPOCH,
        "candidate_epoch": CANDIDATE_EPOCH,
        "database_actions": status == "measured_external_preview",
        "source_activation": false,
        "source_plan_sha256": candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256": candidate.package_lock_sha256,
        "predecessor_manifest_sha256": candidate.plan["predecessor_manifest_sha256"],
        "predecessor_reference_sha256": candidate.predecessor_reference_sha256,
        "predecessor_measurement_sha256": candidate.predecessor_measurement_sha256,
        "predecessor_activation_sha256": candidate.predecessor_activation_sha256,
        "candidate_manifest_sha256": manifest_sha,
        "baseline_manifest_sha256": candidate.baseline_sha256,
        "candidate_sql_sha256": SQL_SHA256,
        "expected_database": database,
        "verification_ref_sha256": reference,
        "measurement_receipt_sha256": measurement,
        "post_migration_noop": true
    })
}

fn inventory(candidate: &Candidate, measured: bool, bundled: bool) -> Result<BTreeSet<String>> {
    let mut files = lock_files(&candidate.package_lock)?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    files.insert(epoch_relative(SQL));
    if measured || bundled {
        files.insert(epoch_relative(REFERENCE));
        files.insert(epoch_relative(MEASUREMENT));
    }
    if bundled {
        files.insert(epoch_relative(ACTIVATION));
    }
    Ok(files)
}

fn verify_inputs(root: &Path, candidate: &Candidate, measured: bool, bundled: bool) -> Result<()> {
    if safe::collect_distribution_inputs(&root.join("database"))?
        != inventory(candidate, measured, bundled)?
    {
        return Err(refused(
            "028 preview has missing or unexpected database inputs",
        ));
    }
    for (relative, hash) in lock_files(&candidate.package_lock)? {
        if relative == &epoch_relative("manifest.yml") {
            continue;
        }
        let path = safe::source_file(root, &format!("database/{relative}"))?;
        if manifest::file_sha256(&path)? != hash.as_str().unwrap_or_default() {
            return Err(refused("028 changed an activated027 source input"));
        }
    }
    if source_json(root, MANIFEST)? != candidate.manifest_value
        || manifest::load_epoch_manifest(&root.join(MANIFEST))?.epoch != CANDIDATE_EPOCH
        || manifest::file_sha256(&safe::source_file(
            root,
            &format!("database/{}", epoch_relative(SQL)),
        )?)? != SQL_SHA256
    {
        return Err(refused(
            "028 preview is not the exact fixed contract extension",
        ));
    }
    Ok(())
}

fn verify_preview_root(preview: &Path) -> Result<()> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(preview)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refused("028 preview name is invalid"))?;
        if kind.is_symlink()
            || !matches!(name.as_str(), "database" | MARKER)
            || (name == "database") != kind.is_dir()
            || (name == MARKER && !kind.is_file())
        {
            return Err(refused(
                "028 preview contains an unexpected or unsafe entry",
            ));
        }
        names.insert(name);
    }
    if names != BTreeSet::from(["database".into(), MARKER.into()]) {
        return Err(refused("028 preview inventory is incomplete"));
    }
    Ok(())
}

pub fn materialize(source: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "028 source must be a nonsymlink directory")?;
    let candidate = validate_source(&source)?;
    let output = external_destination(&source, None, output)?;
    fs::create_dir(&output)?;
    safe::copy_tree(&source.join("database"), &output.join("database"))?;
    safe::write_new_bytes(&epoch_file(&output, SQL), &require_proposal(&source)?)?;
    fs::write(
        output.join(MANIFEST),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&candidate.manifest_value)
                .map_err(|_| refused("028 manifest serialization failed"))?
        ),
    )?;
    let marker = common_receipt(
        &candidate,
        &manifest::file_sha256(&output.join(MANIFEST))?,
        "inactive_external_preview_only",
        None,
        None,
        None,
    );
    safe::write_new_json(&output.join(MARKER), &marker)?;
    verify_preview(&source, &output, false)?;
    Ok(marker)
}

pub fn verify_preview(source: &Path, preview: &Path, measured: bool) -> Result<Candidate> {
    let source = safe::canonical_directory(source, "028 source must be a real directory")?;
    let preview = safe::canonical_directory(preview, "028 preview must be a real directory")?;
    if preview.starts_with(&source) || source.starts_with(&preview) {
        return Err(refused("028 preview must be external to its source"));
    }
    verify_preview_root(&preview)?;
    let candidate = validate_source(&source)?;
    verify_inputs(&preview, &candidate, measured, false)?;
    if manifest::file_sha256(&safe::source_file(&preview, LOCK)?)? != candidate.package_lock_sha256
    {
        return Err(refused(
            "028 preview modified the 027 predecessor package lock",
        ));
    }
    let manifest_sha = manifest::file_sha256(&preview.join(MANIFEST))?;
    if source_json(&preview, MARKER)?
        != common_receipt(
            &candidate,
            &manifest_sha,
            "inactive_external_preview_only",
            None,
            None,
            None,
        )
    {
        return Err(refused(
            "028 preview marker is not bound to the exact source",
        ));
    }
    if measured {
        measurement::verify_measured_files(&source, &preview, &candidate)?;
    }
    Ok(candidate)
}

#[cfg(test)]
#[path = "next_successor_028_tests.rs"]
mod tests;
