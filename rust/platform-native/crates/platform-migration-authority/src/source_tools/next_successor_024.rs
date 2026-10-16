//! The closed 023 -> 024 Local Node column-SELECT extension.
//!
//! This adapter reuses the canonical distribution verifier, safe source/file
//! helpers, reference format, and the shared single-step PostgreSQL runner. It
//! adds only the epoch-24 pins and the exact Local Node column ACL delta.
use crate::{
    AuthorityError, Result, commands, distribution, manifest,
    source_tools::{measure_successors, next_successor, next_successor_023},
    sql::reject_embedded_transactions,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 23;
pub const CANDIDATE_EPOCH: i32 = 24;
const REVIEWED_024_PLAN_SHA256: &str =
    "c116886d421e0be5bb0556b8ddcbff1e8957a07ffec1045a4d99fad7acf650ee";
const PLAN_RELATIVE: &str = "docs/design/canonical-schema-successor-024.json";
const LOCK_ARCHIVE_RELATIVE: &str =
    "docs/design/canonical-schema-successor-024-approved23-package-lock.json";
const MANIFEST_ARCHIVE_RELATIVE: &str =
    "docs/design/canonical-schema-successor-024-approved23-manifest.yml";
const PROPOSAL_RELATIVE: &str = "docs/design/local-node-pairing-channel-select-024.sql.proposal";
const ACTIVE_MANIFEST_RELATIVE: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ACTIVE_LOCK_RELATIVE: &str = "database/authority/package-authority-lock.json";
const EPOCH_RELATIVE: &str = "migrations/2026_08_post_kb_v1";
pub const NEXT_SQL_FILE: &str = "024_local_node_pairing_channel_select_columns.sql";
const NEXT_REFERENCE_FILE: &str = "024_verification.json";
const NEXT_MEASUREMENT_FILE: &str = "SUCCESSOR_024_MEASUREMENT.json";
const NEXT_ACTIVATION_FILE: &str = "SUCCESSOR_024_ACTIVATION.json";
const PREVIEW_RECEIPT_FILE: &str = "PREVIEW_024_NOT_ACTIVATED.json";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";

pub type Candidate = next_successor_023::Candidate;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: String,
    status: String,
    baseline_id: String,
    predecessor_epoch: i32,
    predecessor_manifest_sha256: String,
    approved_predecessor_manifest_archive: String,
    approved_predecessor_manifest_archive_sha256: String,
    approved_predecessor_package_lock: String,
    approved_predecessor_package_lock_sha256: String,
    approved_predecessor_baseline_manifest_sha256: String,
    approved_predecessor_measurement_receipt_sha256: String,
    approved_predecessor_activation_receipt_sha256: String,
    approved_predecessor_sql_sha256: String,
    approved_predecessor_verification_sha256: String,
    candidate_epoch: i32,
    sequence_allocation: String,
    database_actions_authorized: bool,
    fingerprint_status: String,
    successor: PlanSuccessor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanSuccessor {
    proposal: String,
    change: manifest::ChangeSpec,
}

fn refused(message: impl Into<String>) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

fn load_plan(root: &Path) -> Result<(Value, Plan, String)> {
    let path = next_successor::source_file(root, PLAN_RELATIVE)?;
    let plan_sha = manifest::file_sha256(&path)?;
    if plan_sha != REVIEWED_024_PLAN_SHA256 {
        return Err(refused("024 source plan differs from its reviewed digest"));
    }
    let bytes = fs::read(&path)?;
    let plan: Plan = serde_json::from_slice(&bytes)
        .map_err(|_| refused("invalid or unknown 024 successor plan shape"))?;
    if plan.schema_version != "ai-platform/canonical-next-successor-source-plan/v1"
        || plan.status != "inactive_source_candidate"
        || plan.baseline_id != BASELINE
        || plan.predecessor_epoch != PREDECESSOR_EPOCH
        || plan.candidate_epoch != CANDIDATE_EPOCH
        || plan.sequence_allocation != "single_forward_extension_requires_root_review"
        || plan.database_actions_authorized
        || plan.fingerprint_status != "not_measured"
        || plan.predecessor_manifest_sha256
            != "cf6536eb646389142c1d7662eaafac87120d3302ee1a8311d5f1ad8a8c5ca348"
        || plan.approved_predecessor_manifest_archive != MANIFEST_ARCHIVE_RELATIVE
        || plan.approved_predecessor_manifest_archive_sha256 != plan.predecessor_manifest_sha256
        || plan.approved_predecessor_package_lock != LOCK_ARCHIVE_RELATIVE
        || plan.approved_predecessor_package_lock_sha256
            != "97c731f524ae8380cc21c01fb5ba246cb6d1f2862ad805274e825c59066492f6"
        || plan.approved_predecessor_baseline_manifest_sha256
            != "74ffe24788479e72840681d9522838a617d874cd3f170f70c647f9c0072eb2b2"
        || plan.approved_predecessor_measurement_receipt_sha256
            != "60c2bbf3cc270cc032ebee7372b9ed4525548b84591ac6a6f080edc124972d3e"
        || plan.approved_predecessor_activation_receipt_sha256
            != "9be1abfa1934909c43190fa61232dba7ac2137ec3d48314bede62299a8df2dae"
        || plan.approved_predecessor_sql_sha256
            != "80f3b91376dbc773c71359490801718924c93313a6bfbf10153718fe674dd6e2"
        || plan.approved_predecessor_verification_sha256
            != "63496399d60180d02a80cb570a9992171d0074e7781ef403a2dec3df8409e740"
        || plan.successor.proposal != PROPOSAL_RELATIVE
    {
        return Err(refused("024 plan differs from its exact approved-023 pins"));
    }
    let change = &plan.successor.change;
    if change.sequence != CANDIDATE_EPOCH
        || change.name != "local_node_pairing_channel_select_columns"
        || change.file != NEXT_SQL_FILE
        || change.sha256 != "772e6bdd409416444042195d3f320042f83fd29db64807dfabce10987599f2c2"
        || change.owner != "owner"
        || change.transaction_mode != manifest::TransactionMode::Transactional
        || change.rollback_class != manifest::RollbackClass::ForwardFixOnly
        || change.preconditions.len() != 1
        || change.postconditions.len() != 2
        || change.timeout_seconds != 30
        || change.lock_budget_seconds != 5
        || change.resume_handler.is_some()
        || change.repair_handler.is_some()
    {
        return Err(refused("024 change contract is not the reviewed extension"));
    }
    let proposal = next_successor::source_file(root, PROPOSAL_RELATIVE)?;
    if manifest::file_sha256(&proposal)? != change.sha256 {
        return Err(refused("024 proposal differs from its reviewed SQL digest"));
    }
    reject_embedded_transactions(&fs::read_to_string(proposal)?, &change.name)?;
    let value =
        serde_json::from_slice(&bytes).map_err(|_| refused("invalid 024 successor plan JSON"))?;
    Ok((value, plan, plan_sha))
}

fn lock_files(lock: &Value) -> Result<&Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("approved 023 lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("approved 023 lock has no source inventory"))
}

fn lock_digest<'a>(files: &'a Map<String, Value>, relative: &str) -> Option<&'a str> {
    files.get(relative).and_then(Value::as_str)
}

fn approved_023_lock(root: &Path, plan: &Plan) -> Result<(Value, String)> {
    let archived = next_successor::source_file(root, &plan.approved_predecessor_package_lock)?;
    let current = next_successor::source_file(root, ACTIVE_LOCK_RELATIVE)?;
    let archive_sha = manifest::file_sha256(&archived)?;
    if archive_sha != plan.approved_predecessor_package_lock_sha256
        || manifest::file_sha256(&current)? != archive_sha
        || fs::read(&archived)? != fs::read(&current)?
    {
        return Err(refused(
            "active source differs from the exact approved-023 lock",
        ));
    }
    let value: Value = serde_json::from_slice(&fs::read(archived)?)
        .map_err(|_| refused("invalid approved-023 package lock"))?;
    if lock_files(&value)?.len() != 190 {
        return Err(refused(
            "approved-023 distribution inventory has an unexpected size",
        ));
    }
    Ok((value, archive_sha))
}

fn expected_input_paths(lock: &Value, measured: bool, activated: bool) -> Result<BTreeSet<String>> {
    let mut expected: BTreeSet<String> = lock_files(lock)?.keys().cloned().collect();
    let epoch = format!("migrations/{BASELINE}/");
    expected.insert(format!("{epoch}{NEXT_SQL_FILE}"));
    if measured || activated {
        expected.insert(format!("{epoch}{NEXT_REFERENCE_FILE}"));
        expected.insert(format!("{epoch}{NEXT_MEASUREMENT_FILE}"));
    }
    if activated {
        expected.insert(format!("{epoch}{NEXT_ACTIVATION_FILE}"));
    }
    Ok(expected)
}

fn verified_candidate_source(root: &Path) -> Result<Candidate> {
    let root = next_successor::canonical_directory(root, "source root must be a directory")?;
    let (plan_value, plan, plan_sha256) = load_plan(&root)?;
    let (package_lock, package_lock_sha256) = approved_023_lock(&root, &plan)?;
    distribution::verify(&root.join("database"), BASELINE)?;
    let paths = crate::runner::AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let (_, source_epoch) = next_successor_023::verify_activated_source(&root)?;
    let manifest_path = next_successor::source_file(&root, ACTIVE_MANIFEST_RELATIVE)?;
    let manifest_sha = manifest::file_sha256(&manifest_path)?;
    let files = lock_files(&package_lock)?;
    for (path, expected) in [
        (
            format!("migrations/{BASELINE}/manifest.yml"),
            manifest_sha.as_str(),
        ),
        (
            format!("baselines/{BASELINE}/manifest.json"),
            baseline_sha256.as_str(),
        ),
        (
            format!("migrations/{BASELINE}/SUCCESSOR_023_MEASUREMENT.json"),
            plan.approved_predecessor_measurement_receipt_sha256
                .as_str(),
        ),
        (
            format!("migrations/{BASELINE}/SUCCESSOR_023_ACTIVATION.json"),
            plan.approved_predecessor_activation_receipt_sha256.as_str(),
        ),
        (
            format!("migrations/{BASELINE}/023_native_workflow_authority.sql"),
            plan.approved_predecessor_sql_sha256.as_str(),
        ),
        (
            format!("migrations/{BASELINE}/023_verification.json"),
            plan.approved_predecessor_verification_sha256.as_str(),
        ),
    ] {
        if lock_digest(files, &path) != Some(expected) {
            return Err(refused("approved-023 source inputs or receipts changed"));
        }
    }
    if source_epoch.epoch != PREDECESSOR_EPOCH
        || source_epoch.changes.len() != PREDECESSOR_EPOCH as usize
        || manifest_sha != plan.predecessor_manifest_sha256
        || baseline_sha256 != plan.approved_predecessor_baseline_manifest_sha256
    {
        return Err(refused(
            "active source is not the exact verified epoch-23 preimage",
        ));
    }
    let archived_manifest =
        next_successor::source_file(&root, &plan.approved_predecessor_manifest_archive)?;
    if manifest::file_sha256(&archived_manifest)? != manifest_sha
        || fs::read(&archived_manifest)? != fs::read(&manifest_path)?
    {
        return Err(refused("approved-023 manifest archive changed"));
    }
    let epoch_dir = root.join("database").join(EPOCH_RELATIVE);
    for path in [
        epoch_dir.join(NEXT_SQL_FILE),
        epoch_dir.join(NEXT_REFERENCE_FILE),
        epoch_dir.join(NEXT_MEASUREMENT_FILE),
        epoch_dir.join(NEXT_ACTIVATION_FILE),
    ] {
        if fs::symlink_metadata(path).is_ok() {
            return Err(refused("024 already entered the active migration source"));
        }
    }
    let proposal = next_successor::source_file(&root, PROPOSAL_RELATIVE)?;
    let proposal_sha256 = manifest::file_sha256(&proposal)?;
    let mut manifest_value: Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .map_err(|_| refused("active epoch-23 manifest must be JSON-compatible"))?;
    manifest_value["epoch"] = json!(CANDIDATE_EPOCH);
    manifest_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("active epoch-23 manifest has no change list"))?
        .push(
            serde_json::to_value(&plan.successor.change)
                .map_err(|_| refused("024 change serialization failed"))?,
        );
    let mut epoch = source_epoch;
    epoch.epoch = CANDIDATE_EPOCH;
    epoch.changes.push(plan.successor.change.clone());
    Ok(Candidate {
        plan: plan_value,
        manifest_value,
        epoch,
        baseline,
        baseline_sha256,
        package_lock,
        package_lock_sha256,
        plan_sha256,
        proposal_sha256,
        predecessor_measurement_sha256: plan.approved_predecessor_measurement_receipt_sha256,
        predecessor_activation_sha256: plan.approved_predecessor_activation_receipt_sha256,
    })
}

pub fn validate_source(root: &Path) -> Result<Candidate> {
    verified_candidate_source(root)
}

fn require_preview_inventory(
    source_root: &Path,
    preview_root: &Path,
    candidate: &Candidate,
    measured: bool,
) -> Result<()> {
    if next_successor::collect_distribution_inputs(&preview_root.join("database"))?
        != expected_input_paths(&candidate.package_lock, measured, false)?
    {
        return Err(refused(
            "024 preview contains missing or unexpected authority inputs",
        ));
    }
    let files = lock_files(&candidate.package_lock)?;
    let manifest_key = format!("migrations/{BASELINE}/manifest.yml");
    for (relative, expected) in files {
        if relative == &manifest_key {
            continue;
        }
        let path = next_successor::source_file(preview_root, &format!("database/{relative}"))?;
        if manifest::file_sha256(&path)? != expected.as_str().unwrap_or_default() {
            return Err(refused("024 preview changed an approved epoch-23 input"));
        }
    }
    let preview_lock = next_successor::source_file(preview_root, ACTIVE_LOCK_RELATIVE)?;
    if manifest::file_sha256(&preview_lock)? != candidate.package_lock_sha256 {
        return Err(refused(
            "024 preview package lock differs from approved epoch 23",
        ));
    }
    let proposal = next_successor::source_file(source_root, PROPOSAL_RELATIVE)?;
    let preview_sql = next_successor::source_file(
        preview_root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_SQL_FILE}"),
    )?;
    if manifest::file_sha256(&proposal)? != candidate.proposal_sha256
        || manifest::file_sha256(&preview_sql)? != candidate.proposal_sha256
        || fs::read(&proposal)? != fs::read(&preview_sql)?
    {
        return Err(refused(
            "024 preview SQL differs from the reviewed proposal",
        ));
    }
    let manifest_path = next_successor::source_file(
        preview_root,
        &format!("database/{EPOCH_RELATIVE}/manifest.yml"),
    )?;
    let preview_manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .map_err(|_| refused("invalid 024 preview manifest"))?;
    if preview_manifest != candidate.manifest_value {
        return Err(refused(
            "024 preview manifest is not the exact one-change extension",
        ));
    }
    Ok(())
}

fn verify_preview_root(preview_root: &Path) -> Result<()> {
    let mut entries = BTreeSet::new();
    for entry in fs::read_dir(preview_root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !(kind.is_file() || kind.is_dir()) {
            return Err(refused("024 preview contains an unsupported entry"));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refused("024 preview contains an invalid name"))?;
        if !entries.insert(name.clone())
            || (name == "database") != kind.is_dir()
            || (name != "database" && name != PREVIEW_RECEIPT_FILE)
        {
            return Err(refused("024 preview has missing or unexpected entries"));
        }
    }
    if entries != BTreeSet::from(["database".to_owned(), PREVIEW_RECEIPT_FILE.to_owned()]) {
        return Err(refused("024 preview has missing or unexpected entries"));
    }
    Ok(())
}

fn preview_is_external(source_root: &Path, preview_root: &Path) -> Result<()> {
    if preview_root.starts_with(source_root) || source_root.starts_with(preview_root) {
        return Err(refused(
            "024 preview must be external to the active source tree",
        ));
    }
    Ok(())
}

fn verify_marker(preview_root: &Path, candidate: &Candidate) -> Result<Value> {
    let marker: Value = serde_json::from_slice(&fs::read(preview_root.join(PREVIEW_RECEIPT_FILE))?)
        .map_err(|_| refused("invalid 024 preview receipt"))?;
    if !next_successor::exact_keys(
        &marker,
        &[
            "schema",
            "status",
            "database_actions",
            "source_activation",
            "predecessor_epoch",
            "candidate_epoch",
            "source_plan_sha256",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "baseline_manifest_sha256",
            "proposal_sha256",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "candidate_manifest_sha256",
            "candidate_sql_sha256",
        ],
    ) || marker["schema"] != "ai-platform/canonical-next-successor-preview/v1"
        || marker["status"] != "inactive_external_preview_only"
        || marker["database_actions"] != false
        || marker["source_activation"] != false
        || marker["predecessor_epoch"] != PREDECESSOR_EPOCH
        || marker["candidate_epoch"] != CANDIDATE_EPOCH
        || marker["source_plan_sha256"] != candidate.plan_sha256
        || marker["approved_predecessor_package_lock_sha256"] != candidate.package_lock_sha256
        || marker["predecessor_manifest_sha256"] != candidate.plan["predecessor_manifest_sha256"]
        || marker["baseline_manifest_sha256"] != candidate.baseline_sha256
        || marker["proposal_sha256"] != candidate.proposal_sha256
        || marker["candidate_sql_sha256"] != candidate.proposal_sha256
        || marker["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || marker["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
    {
        return Err(refused(
            "024 preview receipt does not bind the exact candidate",
        ));
    }
    let manifest_path = preview_root
        .join("database")
        .join(EPOCH_RELATIVE)
        .join("manifest.yml");
    if marker["candidate_manifest_sha256"] != manifest::file_sha256(&manifest_path)? {
        return Err(refused("024 preview receipt manifest digest differs"));
    }
    Ok(marker)
}

fn verify_preview_state(
    source_root: &Path,
    preview_root: &Path,
    measured: bool,
) -> Result<(Candidate, Value)> {
    let source_root = next_successor::canonical_directory(
        source_root,
        "source root must be a nonsymlink directory",
    )?;
    let preview_root = next_successor::canonical_directory(
        preview_root,
        "024 preview must be a nonsymlink directory",
    )?;
    preview_is_external(&source_root, &preview_root)?;
    verify_preview_root(&preview_root)?;
    let candidate = verified_candidate_source(&source_root)?;
    require_preview_inventory(&source_root, &preview_root, &candidate, measured)?;
    let marker = verify_marker(&preview_root, &candidate)?;
    let epoch_dir = preview_root.join("database").join(EPOCH_RELATIVE);
    if measured {
        verify_reference(&candidate, &source_root, &preview_root)?;
        verify_measurement_receipt(&candidate, &marker, &preview_root)?;
    } else {
        for path in [
            epoch_dir.join(NEXT_REFERENCE_FILE),
            epoch_dir.join(NEXT_MEASUREMENT_FILE),
        ] {
            match fs::symlink_metadata(path) {
                Ok(_) => {
                    return Err(refused(
                        "024 unmeasured preview already has measurement inputs",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok((candidate, marker))
}

pub fn materialize(root: &Path, output: &Path) -> Result<Value> {
    let root = next_successor::canonical_directory(root, "source root must be a directory")?;
    let candidate = verified_candidate_source(&root)?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused("024 preview output must not already exist"));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid 024 preview destination"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid 024 preview destination"))?,
    );
    if resolved != absolute || resolved.starts_with(&root) || root.starts_with(&resolved) {
        return Err(refused(
            "024 preview must be external to the active source tree",
        ));
    }
    fs::create_dir(&absolute)?;
    next_successor::copy_tree(&root.join("database"), &absolute.join("database"))?;
    let epoch_dir = absolute.join("database").join(EPOCH_RELATIVE);
    let mut sql = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(epoch_dir.join(NEXT_SQL_FILE))?;
    sql.write_all(&fs::read(next_successor::source_file(
        &root,
        PROPOSAL_RELATIVE,
    )?)?)?;
    sql.sync_all()?;
    let manifest_path = epoch_dir.join("manifest.yml");
    fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&candidate.manifest_value)
                .map_err(|_| refused("024 candidate manifest serialization failed"))?
        ),
    )?;
    let marker = json!({
        "schema":"ai-platform/canonical-next-successor-preview/v1",
        "status":"inactive_external_preview_only",
        "database_actions":false,
        "source_activation":false,
        "predecessor_epoch":PREDECESSOR_EPOCH,
        "candidate_epoch":CANDIDATE_EPOCH,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "proposal_sha256":candidate.proposal_sha256,
        "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
        "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256,
        "candidate_manifest_sha256":manifest::file_sha256(&manifest_path)?,
        "candidate_sql_sha256":candidate.proposal_sha256
    });
    next_successor::write_new_json(&absolute.join(PREVIEW_RECEIPT_FILE), &marker)?;
    verify_preview(&root, &absolute)?;
    Ok(marker)
}

pub fn verify_preview(source_root: &Path, preview_root: &Path) -> Result<(Candidate, Value)> {
    verify_preview_state(source_root, preview_root, false)
}

pub(crate) fn verification_reference(
    candidate: &Candidate,
    evidence: &measure_successors::SingleStepEvidence,
) -> Value {
    let mut fingerprints = evidence.raw_fingerprints.clone();
    fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
    let changes = candidate
        .epoch
        .changes
        .iter()
        .map(|change| (change.sequence.to_string(), json!(change.sha256)))
        .collect::<Map<_, _>>();
    json!({
        "baseline_id":candidate.baseline.baseline_id,
        "sequence":CANDIDATE_EPOCH,
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "changes":changes,
        "fingerprints":fingerprints,
        "raw_fingerprints":evidence.raw_fingerprints,
        "acl_compatibility":{
            "revision":crate::ledger_acl::REVISION,
            "manifest_sha256":crate::ledger_acl::MANIFEST_SHA256,
            "delta_line":ACL_DELTA,
            "base_acl_sha256":evidence.base_acl_sha256,
            "effective_acl_sha256":evidence.raw_acl_sha256,
            "removed_delta_count":1,
            "raw_acl_lines":evidence.raw_acl_lines,
            "base_acl_lines":evidence.base_acl_lines
        },
        "provenance":"Explicit PostgreSQL 16 measurement of only the 023 -> 024 extension against the exact approved epoch-23 prefix.",
        "provenance_details":{
            "tool":"rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_next_successor_024.rs",
            "predecessor_epoch":PREDECESSOR_EPOCH,
            "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
            "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
            "source_plan_sha256":candidate.plan_sha256,
            "candidate_sql_sha256":candidate.proposal_sha256,
            "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
            "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256,
            "git_baseline_provenance_verified":false
        }
    })
}

fn verify_reference(
    candidate: &Candidate,
    source_root: &Path,
    preview_root: &Path,
) -> Result<String> {
    let path = preview_root
        .join("database")
        .join(EPOCH_RELATIVE)
        .join(NEXT_REFERENCE_FILE);
    let reference: Value = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|_| refused("invalid 024 verification reference"))?;
    let changes = candidate
        .epoch
        .changes
        .iter()
        .map(|change| (change.sequence.to_string(), json!(change.sha256)))
        .collect::<Map<_, _>>();
    let fingerprints = reference["fingerprints"]
        .as_object()
        .ok_or_else(|| refused("024 reference has no canonical fingerprints"))?;
    let raw = reference["raw_fingerprints"]
        .as_object()
        .ok_or_else(|| refused("024 reference has no raw fingerprints"))?;
    let fingerprint_keys: BTreeSet<&str> = ["structural", "acl", "extensions", "reference_data"]
        .into_iter()
        .collect();
    if !next_successor::exact_keys(
        &reference,
        &[
            "baseline_id",
            "sequence",
            "baseline_manifest_sha256",
            "changes",
            "fingerprints",
            "raw_fingerprints",
            "acl_compatibility",
            "provenance",
            "provenance_details",
        ],
    ) || !next_successor::exact_keys(
        &reference["acl_compatibility"],
        &[
            "revision",
            "manifest_sha256",
            "delta_line",
            "base_acl_sha256",
            "effective_acl_sha256",
            "removed_delta_count",
            "raw_acl_lines",
            "base_acl_lines",
        ],
    ) || !next_successor::exact_keys(
        &reference["provenance_details"],
        &[
            "tool",
            "predecessor_epoch",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "source_plan_sha256",
            "candidate_sql_sha256",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "git_baseline_provenance_verified",
        ],
    ) || reference["baseline_id"] != BASELINE
        || reference["sequence"] != CANDIDATE_EPOCH
        || reference["baseline_manifest_sha256"] != candidate.baseline_sha256
        || reference["changes"] != Value::Object(changes)
        || fingerprints
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != fingerprint_keys
        || raw.keys().map(String::as_str).collect::<BTreeSet<_>>() != fingerprint_keys
        || fingerprints.values().chain(raw.values()).any(|v| {
            !v.as_str()
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        || fingerprints["structural"] != raw["structural"]
        || fingerprints["extensions"] != raw["extensions"]
        || fingerprints["reference_data"] != raw["reference_data"]
        || reference["provenance"]
            != "Explicit PostgreSQL 16 measurement of only the 023 -> 024 extension against the exact approved epoch-23 prefix."
        || reference["provenance_details"]["tool"]
            != "rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_next_successor_024.rs"
        || reference["provenance_details"]["predecessor_epoch"] != PREDECESSOR_EPOCH
        || reference["provenance_details"]["approved_predecessor_package_lock_sha256"]
            != candidate.package_lock_sha256
        || reference["provenance_details"]["predecessor_manifest_sha256"]
            != candidate.plan["predecessor_manifest_sha256"]
        || reference["provenance_details"]["source_plan_sha256"] != candidate.plan_sha256
        || reference["provenance_details"]["candidate_sql_sha256"] != candidate.proposal_sha256
        || reference["provenance_details"]["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || reference["provenance_details"]["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || reference["provenance_details"]["git_baseline_provenance_verified"] != false
    {
        return Err(refused(
            "024 reference does not bind the complete 1-24 prefix",
        ));
    }
    let raw_fingerprints: BTreeMap<String, String> =
        serde_json::from_value(reference["raw_fingerprints"].clone())
            .map_err(|_| refused("invalid 024 raw fingerprints"))?;
    let acl_lines: Vec<String> =
        serde_json::from_value(reference["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("invalid 024 raw ACL lines"))?;
    let (base_acl, effective_acl, base_lines) =
        measure_successors::acl_reference(&raw_fingerprints, &acl_lines)?;
    if reference["acl_compatibility"]["revision"] != crate::ledger_acl::REVISION
        || reference["acl_compatibility"]["manifest_sha256"] != crate::ledger_acl::MANIFEST_SHA256
        || reference["acl_compatibility"]["delta_line"] != ACL_DELTA
        || reference["acl_compatibility"]["removed_delta_count"] != 1
        || reference["acl_compatibility"]["base_acl_sha256"] != base_acl
        || reference["acl_compatibility"]["effective_acl_sha256"] != effective_acl
        || fingerprints["acl"] != base_acl
        || reference["acl_compatibility"]["base_acl_lines"] != json!(base_lines)
    {
        return Err(refused(
            "024 ACL compatibility does not preserve ledger ACL v1",
        ));
    }
    let previous_path = next_successor::source_file(
        source_root,
        &format!("database/{EPOCH_RELATIVE}/023_verification.json"),
    )?;
    if manifest::file_sha256(&previous_path)?
        != candidate.plan["approved_predecessor_verification_sha256"]
    {
        return Err(refused("approved 023 verification reference changed"));
    }
    let previous: Value = serde_json::from_slice(&fs::read(previous_path)?)
        .map_err(|_| refused("invalid approved 023 verification reference"))?;
    for key in ["structural", "extensions", "reference_data"] {
        if raw[key] != previous["raw_fingerprints"][key]
            || fingerprints[key] != previous["fingerprints"][key]
        {
            return Err(refused("024 changed a non-ACL canonical fingerprint"));
        }
    }
    let previous_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("approved 023 raw ACL lines are malformed"))?;
    let old: BTreeSet<&str> = previous_lines.iter().map(String::as_str).collect();
    let new: BTreeSet<&str> = acl_lines.iter().map(String::as_str).collect();
    let added = new.difference(&old).copied().collect::<BTreeSet<_>>();
    let removed = old.difference(&new).copied().collect::<BTreeSet<_>>();
    let expected: BTreeSet<&str> = [
        "column_acl:gateway.local_node_pairing_challenges.challenge_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_pairing_challenges.user_code_sha256:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_pairing_challenges.tenant_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_pairing_challenges.user_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_pairing_challenges.consumed_at:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_pairing_challenges.expires_at:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.channel_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.device_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.tenant_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.user_id:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.status:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.receipt_cursor:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.fingerprint:gateway:SELECT:grantopt=0",
        "column_acl:gateway.local_node_channels.created_at:gateway:SELECT:grantopt=0",
    ]
    .into_iter()
    .collect();
    if old.len() != previous_lines.len()
        || new.len() != acl_lines.len()
        || !removed.is_empty()
        || added != expected
    {
        return Err(refused(
            "024 ACL measurement differs from the exact Local Node column grants",
        ));
    }
    Ok(manifest::file_sha256(&path)?)
}

fn verify_measurement_receipt(
    candidate: &Candidate,
    marker: &Value,
    preview_root: &Path,
) -> Result<Value> {
    let path = preview_root
        .join("database")
        .join(EPOCH_RELATIVE)
        .join(NEXT_MEASUREMENT_FILE);
    let receipt: Value = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|_| refused("invalid 024 measurement receipt"))?;
    let reference_sha = manifest::file_sha256(
        &preview_root
            .join("database")
            .join(EPOCH_RELATIVE)
            .join(NEXT_REFERENCE_FILE),
    )?;
    let manifest_sha = manifest::file_sha256(
        &preview_root
            .join("database")
            .join(EPOCH_RELATIVE)
            .join("manifest.yml"),
    )?;
    if !next_successor::exact_keys(
        &receipt,
        &[
            "schema",
            "status",
            "database_actions",
            "database_actions_scope",
            "source_activation",
            "expected_database",
            "predecessor_epoch",
            "candidate_epoch",
            "source_plan_sha256",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "candidate_manifest_sha256",
            "baseline_manifest_sha256",
            "candidate_sql_sha256",
            "successor_sequences",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "verification_ref_sha256",
            "post_migration_noop",
        ],
    ) || receipt["schema"] != "ai-platform/canonical-next-successor-measurement/v1"
        || receipt["status"] != "measured_external_preview"
        || receipt["database_actions"] != true
        || receipt["database_actions_scope"] != "named_isolated_database_only"
        || receipt["source_activation"] != false
        || receipt["expected_database"]
            .as_str()
            .is_none_or(|db| measure_successors::validate_database_name(db).is_err())
        || receipt["predecessor_epoch"] != PREDECESSOR_EPOCH
        || receipt["candidate_epoch"] != CANDIDATE_EPOCH
        || receipt["source_plan_sha256"] != candidate.plan_sha256
        || receipt["approved_predecessor_package_lock_sha256"] != candidate.package_lock_sha256
        || receipt["predecessor_manifest_sha256"] != candidate.plan["predecessor_manifest_sha256"]
        || receipt["candidate_manifest_sha256"] != manifest_sha
        || receipt["baseline_manifest_sha256"] != candidate.baseline_sha256
        || receipt["candidate_sql_sha256"] != candidate.proposal_sha256
        || receipt["successor_sequences"] != json!([CANDIDATE_EPOCH])
        || receipt["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || receipt["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || receipt["verification_ref_sha256"] != reference_sha
        || receipt["post_migration_noop"] != true
        || marker["source_plan_sha256"] != candidate.plan_sha256
    {
        return Err(refused(
            "024 measurement receipt does not bind the reviewed candidate",
        ));
    }
    Ok(receipt)
}

pub fn verify_measured_preview(
    source_root: &Path,
    preview_root: &Path,
) -> Result<(Candidate, Value)> {
    let (candidate, marker) = verify_preview_state(source_root, preview_root, true)?;
    let receipt = verify_measurement_receipt(&candidate, &marker, preview_root)?;
    Ok((candidate, receipt))
}

pub fn activation_bundle(source_root: &Path, preview_root: &Path, output: &Path) -> Result<Value> {
    let source_root =
        next_successor::canonical_directory(source_root, "source root must be a directory")?;
    let preview_root =
        next_successor::canonical_directory(preview_root, "024 preview must be a directory")?;
    let (candidate, _) = verify_measured_preview(&source_root, &preview_root)?;
    let measurement = verify_measurement_receipt(
        &candidate,
        &verify_marker(&preview_root, &candidate)?,
        &preview_root,
    )?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused(
            "024 activation bundle output must not already exist",
        ));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid 024 bundle destination"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid 024 bundle destination"))?,
    );
    if resolved != absolute
        || absolute.starts_with(&source_root)
        || source_root.starts_with(&absolute)
        || absolute.starts_with(&preview_root)
        || preview_root.starts_with(&absolute)
    {
        return Err(refused(
            "024 activation bundle must be external to source and preview",
        ));
    }
    let epoch_dir = preview_root.join("database").join(EPOCH_RELATIVE);
    let manifest_sha = manifest::file_sha256(&epoch_dir.join("manifest.yml"))?;
    let reference_sha = manifest::file_sha256(&epoch_dir.join(NEXT_REFERENCE_FILE))?;
    let measurement_sha = manifest::file_sha256(&epoch_dir.join(NEXT_MEASUREMENT_FILE))?;
    let activation = json!({
        "schema":"ai-platform/canonical-next-successor-activation/v1",
        "status":"activated_source_chain",
        "source_activation":true,
        "predecessor_epoch":PREDECESSOR_EPOCH,
        "epoch":CANDIDATE_EPOCH,
        "baseline_id":BASELINE,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
        "epoch_manifest_sha256":manifest_sha,
        "measurement_receipt_sha256":measurement_sha,
        "verification_ref_sha256":reference_sha,
        "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
        "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256
    });
    let activation_bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&activation)
            .map_err(|_| refused("024 activation receipt serialization failed"))?
    )
    .into_bytes();
    let activation_sha = next_successor::sha256_bytes(&activation_bytes);
    let mut package_lock = candidate.package_lock.clone();
    let files = package_lock["files_sha256"]
        .as_object_mut()
        .ok_or_else(|| refused("approved 023 lock has no inventory"))?;
    for (relative, digest) in [
        (
            format!("migrations/{BASELINE}/manifest.yml"),
            manifest_sha.clone(),
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
            candidate.proposal_sha256.clone(),
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
            reference_sha.clone(),
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
            measurement_sha.clone(),
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
            activation_sha.clone(),
        ),
    ] {
        files.insert(relative, json!(digest));
    }
    let package_lock_bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&package_lock)
            .map_err(|_| refused("024 package lock serialization failed"))?
    )
    .into_bytes();
    let package_lock_sha = next_successor::sha256_bytes(&package_lock_bytes);
    let summary = json!({
        "schema":"ai-platform/canonical-next-successor-activation-bundle/v1",
        "status":"ready_for_owner_review",
        "database_actions":false,
        "source_activation":false,
        "predecessor_epoch":PREDECESSOR_EPOCH,
        "candidate_epoch":CANDIDATE_EPOCH,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "candidate_manifest_sha256":manifest_sha,
        "candidate_sql_sha256":candidate.proposal_sha256,
        "measurement_receipt_sha256":measurement_sha,
        "verification_ref_sha256":reference_sha,
        "activation_receipt_sha256":activation_sha,
        "package_authority_lock_sha256":package_lock_sha,
        "compiled_distribution_lock_sha256_to_review":package_lock_sha,
        "expected_database":measurement["expected_database"],
        "preview_root":preview_root.to_string_lossy()
    });
    fs::create_dir(&absolute)?;
    next_successor::write_new_json(&absolute.join(NEXT_ACTIVATION_FILE), &activation)?;
    next_successor::write_new_bytes(
        &absolute.join(ACTIVE_LOCK_RELATIVE.rsplit('/').next().unwrap()),
        &package_lock_bytes,
    )?;
    next_successor::write_new_json(&absolute.join("ACTIVATION_024_REVIEW.json"), &summary)?;
    Ok(summary)
}

/// Verify the activated epoch-24 distribution against its exact approved-23 prefix.
pub fn verify_activated_source(root: &Path) -> Result<(Value, manifest::EpochManifest)> {
    let root = next_successor::canonical_directory(root, "source root must be a directory")?;
    let (plan_value, plan, plan_sha) = load_plan(&root)?;
    let archived_lock_path = next_successor::source_file(&root, LOCK_ARCHIVE_RELATIVE)?;
    let archived_lock: Value = serde_json::from_slice(&fs::read(&archived_lock_path)?)
        .map_err(|_| refused("invalid approved-23 package lock"))?;
    if manifest::file_sha256(&archived_lock_path)? != plan.approved_predecessor_package_lock_sha256
    {
        return Err(refused("approved-23 package lock archive changed"));
    }
    let old_files = lock_files(&archived_lock)?;
    if old_files.len() != 190 {
        return Err(refused(
            "approved-23 package inventory has an unexpected size",
        ));
    }
    let active_lock_path = next_successor::source_file(&root, ACTIVE_LOCK_RELATIVE)?;
    let active_lock: Value = serde_json::from_slice(&fs::read(&active_lock_path)?)
        .map_err(|_| refused("invalid active epoch-24 package lock"))?;
    let active_files = lock_files(&active_lock)?;
    if active_files.len() != 194 {
        return Err(refused("active epoch-24 inventory has an unexpected size"));
    }
    distribution::verify(&root.join("database"), BASELINE)?;
    let paths = crate::runner::AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let manifest_path = next_successor::source_file(&root, ACTIVE_MANIFEST_RELATIVE)?;
    let manifest_sha = manifest::file_sha256(&manifest_path)?;
    let archived_manifest_path = next_successor::source_file(&root, MANIFEST_ARCHIVE_RELATIVE)?;
    if manifest::file_sha256(&archived_manifest_path)? != plan.predecessor_manifest_sha256
        || lock_digest(old_files, &format!("migrations/{BASELINE}/manifest.yml"))
            != Some(plan.predecessor_manifest_sha256.as_str())
    {
        return Err(refused("approved-23 manifest preimage changed"));
    }
    let mut expected_manifest: Value = serde_json::from_slice(&fs::read(&archived_manifest_path)?)
        .map_err(|_| refused("invalid archived epoch-23 manifest"))?;
    expected_manifest["epoch"] = json!(CANDIDATE_EPOCH);
    expected_manifest["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("archived epoch-23 manifest has no changes"))?
        .push(
            serde_json::to_value(&plan.successor.change)
                .map_err(|_| refused("024 change serialization failed"))?,
        );
    let manifest_value: Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .map_err(|_| refused("active epoch-24 manifest is invalid"))?;
    if manifest_value != expected_manifest
        || baseline_sha256 != plan.approved_predecessor_baseline_manifest_sha256
    {
        return Err(refused(
            "active epoch-24 manifest is not the exact extension of epoch 23",
        ));
    }
    for (relative, digest) in old_files {
        if relative == &format!("migrations/{BASELINE}/manifest.yml") {
            continue;
        }
        let active = next_successor::source_file(&root, &format!("database/{relative}"))?;
        if active_files.get(relative) != Some(digest)
            || manifest::file_sha256(&active)? != digest.as_str().unwrap_or_default()
        {
            return Err(refused(
                "active epoch-24 package changed an approved epoch-23 input",
            ));
        }
    }
    let epoch = manifest::load_epoch_manifest(&manifest_path)?;
    if epoch.epoch != CANDIDATE_EPOCH || epoch.changes.len() != CANDIDATE_EPOCH as usize {
        return Err(refused("active source is not the complete epoch-24 chain"));
    }
    let sql_path =
        next_successor::source_file(&root, &format!("database/{EPOCH_RELATIVE}/{NEXT_SQL_FILE}"))?;
    let reference_path = next_successor::source_file(
        &root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_REFERENCE_FILE}"),
    )?;
    let measurement_path = next_successor::source_file(
        &root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_MEASUREMENT_FILE}"),
    )?;
    let activation_path = next_successor::source_file(
        &root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_ACTIVATION_FILE}"),
    )?;
    let proposal_path = next_successor::source_file(&root, PROPOSAL_RELATIVE)?;
    if manifest::file_sha256(&sql_path)? != plan.successor.change.sha256
        || fs::read(&sql_path)? != fs::read(&proposal_path)?
    {
        return Err(refused(
            "active epoch-24 SQL differs from the reviewed proposal",
        ));
    }
    let measurement: Value = serde_json::from_slice(&fs::read(&measurement_path)?)
        .map_err(|_| refused("invalid activated 024 measurement receipt"))?;
    let reference: Value = serde_json::from_slice(&fs::read(&reference_path)?)
        .map_err(|_| refused("invalid activated 024 verification reference"))?;
    let activation: Value = serde_json::from_slice(&fs::read(&activation_path)?)
        .map_err(|_| refused("invalid activated 024 activation receipt"))?;
    let current_sha = manifest::file_sha256(&manifest_path)?;
    let measurement_sha = manifest::file_sha256(&measurement_path)?;
    let reference_sha = manifest::file_sha256(&reference_path)?;
    if !next_successor::exact_keys(
        &measurement,
        &[
            "schema",
            "status",
            "database_actions",
            "database_actions_scope",
            "source_activation",
            "expected_database",
            "predecessor_epoch",
            "candidate_epoch",
            "source_plan_sha256",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "candidate_manifest_sha256",
            "baseline_manifest_sha256",
            "candidate_sql_sha256",
            "successor_sequences",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "verification_ref_sha256",
            "post_migration_noop",
        ],
    ) || measurement["schema"] != "ai-platform/canonical-next-successor-measurement/v1"
        || measurement["status"] != "measured_external_preview"
        || measurement["database_actions"] != true
        || measurement["database_actions_scope"] != "named_isolated_database_only"
        || measurement["source_activation"] != false
        || measurement["expected_database"]
            .as_str()
            .is_none_or(|db| measure_successors::validate_database_name(db).is_err())
        || measurement["predecessor_epoch"] != PREDECESSOR_EPOCH
        || measurement["candidate_epoch"] != CANDIDATE_EPOCH
        || measurement["source_plan_sha256"] != plan_sha
        || measurement["approved_predecessor_package_lock_sha256"]
            != plan.approved_predecessor_package_lock_sha256
        || measurement["predecessor_manifest_sha256"] != plan.predecessor_manifest_sha256
        || measurement["candidate_manifest_sha256"] != current_sha
        || measurement["baseline_manifest_sha256"] != baseline_sha256
        || measurement["candidate_sql_sha256"] != plan.successor.change.sha256
        || measurement["successor_sequences"] != json!([CANDIDATE_EPOCH])
        || measurement["predecessor_measurement_receipt_sha256"]
            != plan.approved_predecessor_measurement_receipt_sha256
        || measurement["predecessor_activation_receipt_sha256"]
            != plan.approved_predecessor_activation_receipt_sha256
        || measurement["verification_ref_sha256"] != reference_sha
        || measurement["post_migration_noop"] != true
        || !next_successor::exact_keys(
            &activation,
            &[
                "schema",
                "status",
                "source_activation",
                "predecessor_epoch",
                "epoch",
                "baseline_id",
                "source_plan_sha256",
                "approved_predecessor_package_lock_sha256",
                "baseline_manifest_sha256",
                "predecessor_manifest_sha256",
                "epoch_manifest_sha256",
                "measurement_receipt_sha256",
                "verification_ref_sha256",
                "predecessor_measurement_receipt_sha256",
                "predecessor_activation_receipt_sha256",
            ],
        )
        || activation["schema"] != "ai-platform/canonical-next-successor-activation/v1"
        || activation["status"] != "activated_source_chain"
        || activation["source_activation"] != true
        || activation["predecessor_epoch"] != PREDECESSOR_EPOCH
        || activation["epoch"] != CANDIDATE_EPOCH
        || activation["baseline_id"] != BASELINE
        || activation["source_plan_sha256"] != plan_sha
        || activation["approved_predecessor_package_lock_sha256"]
            != plan.approved_predecessor_package_lock_sha256
        || activation["baseline_manifest_sha256"] != baseline_sha256
        || activation["predecessor_manifest_sha256"] != plan.predecessor_manifest_sha256
        || activation["epoch_manifest_sha256"] != current_sha
        || activation["measurement_receipt_sha256"] != measurement_sha
        || activation["verification_ref_sha256"] != reference_sha
        || activation["predecessor_measurement_receipt_sha256"]
            != plan.approved_predecessor_measurement_receipt_sha256
        || activation["predecessor_activation_receipt_sha256"]
            != plan.approved_predecessor_activation_receipt_sha256
    {
        return Err(refused(
            "active 024 receipts do not bind the approved source chain",
        ));
    }
    let mut expected_paths: BTreeSet<String> = old_files.keys().cloned().collect();
    expected_paths.extend([
        format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
        format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
        format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
        format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
    ]);
    if active_files.keys().cloned().collect::<BTreeSet<_>>() != expected_paths {
        return Err(refused(
            "active epoch-24 package lock has missing or unexpected inputs",
        ));
    }
    for (relative, digest) in [
        (format!("migrations/{BASELINE}/manifest.yml"), current_sha),
        (
            format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
            plan.successor.change.sha256.clone(),
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
            reference_sha,
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
            measurement_sha,
        ),
        (
            format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
            manifest::file_sha256(&activation_path)?,
        ),
    ] {
        if active_files.get(&relative).and_then(Value::as_str) != Some(digest.as_str()) {
            return Err(refused("active epoch-24 package lock digest differs"));
        }
    }
    let candidate = Candidate {
        plan: plan_value,
        manifest_value,
        epoch: epoch.clone(),
        baseline,
        baseline_sha256,
        package_lock: active_lock,
        package_lock_sha256: plan.approved_predecessor_package_lock_sha256.clone(),
        plan_sha256: plan_sha,
        proposal_sha256: plan.successor.change.sha256,
        predecessor_measurement_sha256: plan.approved_predecessor_measurement_receipt_sha256,
        predecessor_activation_sha256: plan.approved_predecessor_activation_receipt_sha256,
    };
    let _ = verify_reference(&candidate, &root, &root)?;
    Ok((candidate.plan, epoch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn active_023_is_the_exact_preimage_for_one_reviewed_024_extension() {
        let root = repository_root();
        let active = manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap();
        if active.epoch == PREDECESSOR_EPOCH {
            let candidate = validate_source(&root).unwrap();
            assert_eq!(candidate.epoch.epoch, CANDIDATE_EPOCH);
            assert_eq!(candidate.epoch.changes.len(), CANDIDATE_EPOCH as usize);
            assert_eq!(
                candidate.package_lock_sha256,
                "97c731f524ae8380cc21c01fb5ba246cb6d1f2862ad805274e825c59066492f6"
            );
            assert_eq!(
                candidate.proposal_sha256,
                "772e6bdd409416444042195d3f320042f83fd29db64807dfabce10987599f2c2"
            );
            assert_eq!(candidate.plan["fingerprint_status"], "not_measured");
        } else if active.epoch == CANDIDATE_EPOCH {
            let (_, verified) = verify_activated_source(&root).unwrap();
            assert_eq!(verified.epoch, CANDIDATE_EPOCH);
            assert_eq!(verified.changes.len(), CANDIDATE_EPOCH as usize);
        } else {
            panic!("unexpected canonical schema epoch");
        }
    }

    #[test]
    fn candidate_sql_has_only_the_reviewed_column_select_grants() {
        let root = repository_root();
        let proposal = next_successor::source_file(&root, PROPOSAL_RELATIVE).unwrap();
        let sql = fs::read_to_string(proposal).unwrap();
        assert_eq!(sql.matches("GRANT SELECT (").count(), 2);
        assert!(!sql.contains("GRANT SELECT ON TABLE"));
        assert!(sql.contains(
            "challenge_id, user_code_sha256, tenant_id, user_id, consumed_at, expires_at"
        ));
        assert!(sql.contains("channel_id, device_id, tenant_id, user_id, status, receipt_cursor, fingerprint, created_at"));
    }
}
