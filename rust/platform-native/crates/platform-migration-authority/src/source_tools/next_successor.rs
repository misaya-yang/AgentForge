//! The single reviewed 020 -> 021 extension path. Historical 017 -> 020
//! preparation and measurement stay in `successors` and `measure_successors`.
use crate::{
    AuthorityError, Result, commands, distribution, manifest, runner::AuthorityPaths,
    source_tools::successors, sql::reject_embedded_transactions,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 20;
pub const CANDIDATE_EPOCH: i32 = 21;
pub const APPROVED_020_LOCK_SHA256: &str =
    "4f15819d3e0d456cdf012a599435c1b0f8ddfb5a55a7e7347156fd93edcc7910";
const REVIEWED_021_PLAN_SHA256: &str =
    "6cae292ce4ebc08e975018a5d33a5ce6814f70a4ab266fa97084d396aa12896d";
const PLAN_RELATIVE: &str = "docs/design/canonical-schema-successor-021.json";
const LOCK_ARCHIVE_RELATIVE: &str =
    "docs/design/canonical-schema-successor-021-approved20-package-lock.json";
const PROPOSAL_RELATIVE: &str = "docs/design/knowledge-worker-process-rule-021.sql.proposal";
const EPOCH_RELATIVE: &str = "database/migrations/2026_08_post_kb_v1";
const ACTIVE_LOCK_RELATIVE: &str = "database/authority/package-authority-lock.json";
const ACTIVE_MANIFEST_RELATIVE: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const NEXT_SQL_FILE: &str = "021_knowledge_worker_ingestion_writes.sql";
const NEXT_REFERENCE_FILE: &str = "021_verification.json";
const NEXT_MEASUREMENT_FILE: &str = "SUCCESSOR_021_MEASUREMENT.json";
const NEXT_ACTIVATION_FILE: &str = "SUCCESSOR_021_ACTIVATION.json";
const PREVIEW_RECEIPT_FILE: &str = "PREVIEW_021_NOT_ACTIVATED.json";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: String,
    status: String,
    baseline_id: String,
    predecessor_epoch: i32,
    predecessor_manifest_sha256: String,
    approved_predecessor_package_lock: String,
    approved_predecessor_package_lock_sha256: String,
    approved_predecessor_baseline_manifest_sha256: String,
    approved_predecessor_measurement_receipt_sha256: String,
    approved_predecessor_activation_receipt_sha256: String,
    approved_predecessor_020_sql_sha256: String,
    approved_predecessor_020_verification_sha256: String,
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

#[derive(Debug, Clone)]
pub struct Candidate {
    pub plan: Value,
    pub manifest_value: Value,
    pub epoch: manifest::EpochManifest,
    pub baseline: manifest::BaselineManifest,
    pub baseline_sha256: String,
    pub package_lock_sha256: String,
    pub plan_sha256: String,
    pub proposal_sha256: String,
    pub predecessor_measurement_sha256: String,
    pub predecessor_activation_sha256: String,
}

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

pub(crate) fn exact_keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_relative(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(refused("successor source path is unsafe"));
    }
    Ok(())
}

pub(crate) fn source_file(root: &Path, relative: &str) -> Result<PathBuf> {
    safe_relative(relative)?;
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(name) = part else {
            return Err(refused("successor source path is unsafe"));
        };
        path.push(name);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(refused("successor source path contains a symlink"));
        }
    }
    let path = path.canonicalize()?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(refused("successor source file is missing or escaped"));
    }
    Ok(path)
}

pub(crate) fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refused(label));
    }
    let absolute = std::path::absolute(path)?;
    let canonical = path.canonicalize()?;
    if absolute != canonical {
        return Err(refused(label));
    }
    Ok(canonical)
}

pub(crate) fn lock_files(lock: &Value) -> Result<&Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("approved epoch-20 lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("approved epoch-20 lock has no file inventory"))
}

fn load_plan(root: &Path) -> Result<(Value, Plan, String)> {
    let path = source_file(root, PLAN_RELATIVE)?;
    let plan_sha = manifest::file_sha256(&path)?;
    if plan_sha != REVIEWED_021_PLAN_SHA256 {
        return Err(refused("021 source plan differs from its reviewed digest"));
    }
    let bytes = fs::read(&path)?;
    let plan: Plan = serde_json::from_slice(&bytes)
        .map_err(|_| refused("invalid or unknown 021 successor plan shape"))?;
    if plan.schema_version != "ai-platform/canonical-next-successor-source-plan/v1"
        || plan.status != "inactive_source_candidate"
        || plan.baseline_id != BASELINE
        || plan.predecessor_epoch != PREDECESSOR_EPOCH
        || plan.predecessor_manifest_sha256
            != "d526d3c1551c3c4a6f88477b9ac7a5bae59f898e055f4fe85a99ea0dda319d23"
        || plan.approved_predecessor_package_lock != LOCK_ARCHIVE_RELATIVE
        || plan.approved_predecessor_package_lock_sha256 != APPROVED_020_LOCK_SHA256
        || plan.approved_predecessor_baseline_manifest_sha256
            != "74ffe24788479e72840681d9522838a617d874cd3f170f70c647f9c0072eb2b2"
        || plan.approved_predecessor_measurement_receipt_sha256
            != "0cc2f885fdbf430091b834b3e491f2561cd333e18983402f3db240541558dab2"
        || plan.approved_predecessor_activation_receipt_sha256
            != "e15a270db4d2bb6d4b11e98b80421459008f1ce8ff145df2807281e25b102561"
        || plan.approved_predecessor_020_sql_sha256
            != "7bb0b6d555c71e14a88987aea68cfa6250bcbc56acb99bc61075b9ba79008583"
        || plan.approved_predecessor_020_verification_sha256
            != "ec021f78081800e3ee3b20b186fda536fcec499f71b8d39ae6c2b917e6b1b2b2"
        || plan.candidate_epoch != CANDIDATE_EPOCH
        || plan.sequence_allocation != "single_forward_extension_requires_root_review"
        || plan.database_actions_authorized
        || plan.fingerprint_status != "not_measured"
        || plan.successor.proposal != PROPOSAL_RELATIVE
    {
        return Err(refused(
            "021 successor plan differs from its reviewed source pins",
        ));
    }
    let change = &plan.successor.change;
    if change.sequence != CANDIDATE_EPOCH
        || change.name != "knowledge_worker_ingestion_writes"
        || change.file != NEXT_SQL_FILE
        || change.owner != "owner"
        || change.transaction_mode != manifest::TransactionMode::Transactional
        || change.rollback_class != manifest::RollbackClass::ForwardFixOnly
        || change.preconditions.len() != 2
        || change.postconditions.len() != 2
        || change.resume_handler.is_some()
        || change.repair_handler.is_some()
    {
        return Err(refused(
            "021 change contract is not the one reviewed extension",
        ));
    }
    let proposal = source_file(root, PROPOSAL_RELATIVE)?;
    if manifest::file_sha256(&proposal)? != change.sha256 {
        return Err(refused("021 proposal differs from the reviewed SQL digest"));
    }
    reject_embedded_transactions(&fs::read_to_string(proposal)?, &change.name)?;
    let value =
        serde_json::from_slice(&bytes).map_err(|_| refused("invalid 021 successor plan JSON"))?;
    Ok((value, plan, plan_sha))
}

fn approved_lock(root: &Path, plan: &Plan) -> Result<(Value, String)> {
    let archived = source_file(root, &plan.approved_predecessor_package_lock)?;
    let current = source_file(root, ACTIVE_LOCK_RELATIVE)?;
    let archive_sha = manifest::file_sha256(&archived)?;
    let current_sha = manifest::file_sha256(&current)?;
    if archive_sha != APPROVED_020_LOCK_SHA256
        || current_sha != APPROVED_020_LOCK_SHA256
        || fs::read(&archived)? != fs::read(&current)?
    {
        return Err(refused(
            "active source differs from the exact approved epoch-20 package lock",
        ));
    }
    let value: Value = serde_json::from_slice(&fs::read(&archived)?)
        .map_err(|_| refused("invalid approved epoch-20 package lock"))?;
    lock_files(&value)?;
    Ok((value, archive_sha))
}

fn archived_approved_lock(root: &Path, plan: &Plan) -> Result<(Value, String)> {
    let archived = source_file(root, &plan.approved_predecessor_package_lock)?;
    let digest = manifest::file_sha256(&archived)?;
    if digest != APPROVED_020_LOCK_SHA256 {
        return Err(refused("archived approved epoch-20 lock digest changed"));
    }
    let value: Value = serde_json::from_slice(&fs::read(archived)?)
        .map_err(|_| refused("invalid archived approved epoch-20 lock"))?;
    lock_files(&value)?;
    Ok((value, digest))
}

pub(crate) fn lock_digest<'a>(files: &'a Map<String, Value>, relative: &str) -> Option<&'a str> {
    files.get(relative).and_then(Value::as_str)
}

fn verified_candidate_source(root: &Path) -> Result<Candidate> {
    let root = canonical_directory(root, "source root must be a nonsymlink directory")?;
    let (plan_value, plan, plan_sha256) = load_plan(&root)?;
    let (lock, package_lock_sha256) = approved_lock(&root, &plan)?;
    distribution::verify(&root.join("database"), BASELINE)?;
    let paths = AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let (_, source_epoch) = successors::verify_activated_source(&root)?;
    let epoch_manifest = source_file(&root, ACTIVE_MANIFEST_RELATIVE)?;
    let current_manifest_sha = manifest::file_sha256(&epoch_manifest)?;
    let files = lock_files(&lock)?;
    let epoch_lock_key = format!("migrations/{BASELINE}/manifest.yml");
    let baseline_lock_key = format!("baselines/{BASELINE}/manifest.json");
    let measurement_lock_key = format!("migrations/{BASELINE}/SUCCESSOR_MEASUREMENT.json");
    let activation_lock_key = format!("migrations/{BASELINE}/SUCCESSOR_ACTIVATION.json");
    let sql_lock_key = format!("migrations/{BASELINE}/020_gateway_task_deliveries.sql");
    let reference_lock_key = format!("migrations/{BASELINE}/020_verification.json");
    if source_epoch.epoch != PREDECESSOR_EPOCH
        || source_epoch.changes.len() != PREDECESSOR_EPOCH as usize
        || current_manifest_sha != plan.predecessor_manifest_sha256
        || baseline_sha256 != plan.approved_predecessor_baseline_manifest_sha256
        || lock_digest(files, &epoch_lock_key) != Some(current_manifest_sha.as_str())
        || lock_digest(files, &baseline_lock_key) != Some(baseline_sha256.as_str())
        || lock_digest(files, &measurement_lock_key)
            != Some(
                plan.approved_predecessor_measurement_receipt_sha256
                    .as_str(),
            )
        || lock_digest(files, &activation_lock_key)
            != Some(plan.approved_predecessor_activation_receipt_sha256.as_str())
        || lock_digest(files, &sql_lock_key)
            != Some(plan.approved_predecessor_020_sql_sha256.as_str())
        || lock_digest(files, &reference_lock_key)
            != Some(plan.approved_predecessor_020_verification_sha256.as_str())
    {
        return Err(refused(
            "approved epoch-20 source preimage or receipts changed",
        ));
    }
    let epoch_dir = root.join(EPOCH_RELATIVE);
    for path in [
        epoch_dir.join(NEXT_SQL_FILE),
        epoch_dir.join(NEXT_REFERENCE_FILE),
        epoch_dir.join(NEXT_MEASUREMENT_FILE),
        epoch_dir.join(NEXT_ACTIVATION_FILE),
    ] {
        if fs::symlink_metadata(path).is_ok() {
            return Err(refused("021 already entered the active migration source"));
        }
    }
    let proposal_sha256 = manifest::file_sha256(&source_file(&root, PROPOSAL_RELATIVE)?)?;
    let predecessor_measurement_sha256 =
        plan.approved_predecessor_measurement_receipt_sha256.clone();
    let predecessor_activation_sha256 = plan.approved_predecessor_activation_receipt_sha256.clone();
    let mut candidate_value: Value = serde_json::from_slice(&fs::read(&epoch_manifest)?)
        .map_err(|_| refused("active epoch-20 manifest must be JSON-compatible"))?;
    candidate_value["epoch"] = json!(CANDIDATE_EPOCH);
    candidate_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("active epoch-20 manifest has no change list"))?
        .push(
            serde_json::to_value(&plan.successor.change)
                .map_err(|_| refused("021 change could not be serialized"))?,
        );
    // Keep the source loader's runtime-only `extra` map when constructing the
    // typed candidate. The manifest bytes above remain the exact standard
    // manifest projection used in the external preview.
    let mut epoch = source_epoch;
    epoch.epoch = CANDIDATE_EPOCH;
    epoch.changes.push(plan.successor.change.clone());
    if epoch.epoch != CANDIDATE_EPOCH || epoch.changes.len() != CANDIDATE_EPOCH as usize {
        return Err(refused(
            "candidate does not extend the complete 020 prefix exactly once",
        ));
    }
    Ok(Candidate {
        plan: plan_value,
        manifest_value: candidate_value,
        epoch,
        baseline,
        baseline_sha256,
        package_lock_sha256,
        plan_sha256,
        proposal_sha256,
        predecessor_measurement_sha256,
        predecessor_activation_sha256,
    })
}

pub(crate) fn write_new_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn write_new_json(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| refused("successor receipt serialization failed"))?;
    bytes.push(b'\n');
    write_new_bytes(path, &bytes)
}

pub(crate) fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refused(
            "successor preview source must be a nonsymlink directory",
        ));
    }
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(refused("successor preview refuses source symlinks"));
        }
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(refused(
                "successor preview encountered an unsupported file type",
            ));
        }
    }
    Ok(())
}

fn proposal_path(root: &Path) -> Result<PathBuf> {
    source_file(root, PROPOSAL_RELATIVE)
}

fn expected_manifest_value(root: &Path) -> Result<Value> {
    let candidate = verified_candidate_source(root)?;
    Ok(candidate.manifest_value)
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

pub(crate) fn collect_distribution_inputs(database: &Path) -> Result<BTreeSet<String>> {
    fn walk(database: &Path, directory: &Path, result: &mut BTreeSet<String>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(refused("successor preview refuses symlinks"));
            }
            if kind.is_dir() {
                walk(database, &path, result)?;
            } else if kind.is_file()
                && matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("sql" | "json" | "yml" | "yaml")
                )
            {
                result.insert(
                    path.strip_prefix(database)
                        .map_err(|_| refused("successor input escaped database root"))?
                        .to_str()
                        .ok_or_else(|| refused("invalid successor input path"))?
                        .replace('\\', "/"),
                );
            } else if !kind.is_file() && !kind.is_dir() {
                return Err(refused("unsupported successor preview file type"));
            }
        }
        Ok(())
    }
    let mut result = BTreeSet::new();
    for directory in ["baselines", "migrations", "bootstrap"] {
        walk(database, &database.join(directory), &mut result)?;
    }
    result.insert("authority/ledger_acl_v1.json".into());
    Ok(result)
}

fn require_preview_inventory(
    source_root: &Path,
    preview_root: &Path,
    measured: bool,
    activated: bool,
) -> Result<Value> {
    let (_, plan, _) = load_plan(source_root)?;
    let (lock, lock_sha) = approved_lock(source_root, &plan)?;
    if collect_distribution_inputs(&preview_root.join("database"))?
        != expected_input_paths(&lock, measured, activated)?
    {
        return Err(refused(
            "021 preview contains missing or unexpected authority inputs",
        ));
    }
    for (relative, expected) in lock_files(&lock)? {
        let relative = format!("database/{relative}");
        if relative == ACTIVE_MANIFEST_RELATIVE {
            continue;
        }
        let path = source_file(preview_root, &relative)?;
        if manifest::file_sha256(&path)? != expected.as_str().unwrap_or_default() {
            return Err(refused("021 preview changed an approved 020 preimage"));
        }
    }
    let copied_lock = source_file(preview_root, ACTIVE_LOCK_RELATIVE)?;
    if manifest::file_sha256(&copied_lock)? != lock_sha {
        return Err(refused(
            "021 preview package lock differs from approved 020",
        ));
    }
    Ok(lock)
}

fn preview_marker(source_root: &Path, preview_root: &Path, candidate: &Candidate) -> Result<Value> {
    let marker_path = preview_root.join(PREVIEW_RECEIPT_FILE);
    let marker: Value = serde_json::from_slice(&fs::read(marker_path)?)
        .map_err(|_| refused("invalid 021 preview receipt"))?;
    let epoch_path = preview_root.join(ACTIVE_MANIFEST_RELATIVE);
    let proposal = proposal_path(source_root)?;
    if !exact_keys(
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
        || marker["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || marker["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || marker["candidate_manifest_sha256"] != manifest::file_sha256(&epoch_path)?
        || marker["candidate_sql_sha256"]
            != manifest::file_sha256(&preview_root.join(EPOCH_RELATIVE).join(NEXT_SQL_FILE))?
        || marker["candidate_sql_sha256"] != manifest::file_sha256(&proposal)?
    {
        return Err(refused(
            "021 preview receipt does not bind this exact source candidate",
        ));
    }
    Ok(marker)
}

fn verify_candidate_epoch(
    source_root: &Path,
    preview_root: &Path,
    candidate: &Candidate,
) -> Result<()> {
    let manifest_path = preview_root.join(ACTIVE_MANIFEST_RELATIVE);
    let epoch = manifest::load_epoch_manifest(&manifest_path)?;
    let expected = expected_manifest_value(source_root)?;
    let actual: Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .map_err(|_| refused("invalid 021 preview manifest"))?;
    if actual != expected
        || epoch.epoch != CANDIDATE_EPOCH
        || epoch.baseline_id != BASELINE
        || epoch.changes.len() != CANDIDATE_EPOCH as usize
        || serde_json::to_value(&epoch.changes[..PREDECESSOR_EPOCH as usize])
            .map_err(|_| refused("cannot bind 020 prefix"))?
            != serde_json::to_value(&candidate.epoch.changes[..PREDECESSOR_EPOCH as usize])
                .map_err(|_| refused("cannot bind 021 predecessor"))?
        || manifest::file_sha256(&preview_root.join(EPOCH_RELATIVE).join(NEXT_SQL_FILE))?
            != candidate.proposal_sha256
    {
        return Err(refused(
            "021 candidate is not the exact one-change extension of epoch 20",
        ));
    }
    Ok(())
}

/// Verify the exact approved 020 source and return its single reviewed 021 extension.
pub fn validate_source(root: &Path) -> Result<Candidate> {
    verified_candidate_source(root)
}

/// Materialize an external candidate from the complete approved 020 database tree.
/// The canonical database tree remains read-only.
pub fn materialize(root: &Path, output: &Path) -> Result<Value> {
    let root = canonical_directory(root, "source root must be a nonsymlink directory")?;
    let candidate = verified_candidate_source(&root)?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused("021 preview output must not already exist"));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid 021 preview destination"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid 021 preview destination"))?,
    );
    if resolved != absolute || resolved.starts_with(&root) || root.starts_with(&resolved) {
        return Err(refused(
            "021 preview must be external to the active source tree",
        ));
    }
    fs::create_dir(&absolute)?;
    copy_tree(&root.join("database"), &absolute.join("database"))?;
    let epoch_dir = absolute.join(EPOCH_RELATIVE);
    let mut sql = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(epoch_dir.join(NEXT_SQL_FILE))?;
    sql.write_all(&fs::read(proposal_path(&root)?)?)?;
    sql.sync_all()?;
    let epoch_manifest = epoch_dir.join("manifest.yml");
    let expected = expected_manifest_value(&root)?;
    fs::write(
        &epoch_manifest,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&expected)
                .map_err(|_| refused("candidate manifest serialization failed"))?
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
        "candidate_manifest_sha256":manifest::file_sha256(&epoch_manifest)?,
        "candidate_sql_sha256":candidate.proposal_sha256
    });
    write_new_json(&absolute.join(PREVIEW_RECEIPT_FILE), &marker)?;
    verify_preview(&root, &absolute)?;
    Ok(marker)
}

/// Validate the inactive, optionally measured candidate against the immutable 020 source.
fn verify_preview_state(
    source_root: &Path,
    preview_root: &Path,
    measured: bool,
) -> Result<(Candidate, Value)> {
    let source_root =
        canonical_directory(source_root, "source root must be a nonsymlink directory")?;
    let preview_root =
        canonical_directory(preview_root, "preview root must be a nonsymlink directory")?;
    if source_root.starts_with(&preview_root) || preview_root.starts_with(&source_root) {
        return Err(refused("021 preview and source must be external"));
    }
    let candidate = verified_candidate_source(&source_root)?;
    let entries: BTreeSet<String> = fs::read_dir(&preview_root)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    if !entries.contains("database")
        || !entries.contains(PREVIEW_RECEIPT_FILE)
        || entries.len() != 2
    {
        return Err(refused(
            "021 preview root has missing or unexpected entries",
        ));
    }
    require_preview_inventory(&source_root, &preview_root, measured, false)?;
    verify_candidate_epoch(&source_root, &preview_root, &candidate)?;
    let marker = preview_marker(&source_root, &preview_root, &candidate)?;
    if !measured {
        match fs::symlink_metadata(preview_root.join(EPOCH_RELATIVE).join(NEXT_REFERENCE_FILE)) {
            Ok(_) => return Err(refused("021 reference exists before measurement")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match fs::symlink_metadata(
            preview_root
                .join(EPOCH_RELATIVE)
                .join(NEXT_MEASUREMENT_FILE),
        ) {
            Ok(_) => return Err(refused("021 measurement receipt exists before measurement")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok((candidate, marker))
}

pub fn verify_preview(source_root: &Path, preview_root: &Path) -> Result<(Candidate, Value)> {
    verify_preview_state(source_root, preview_root, false)
}

pub(crate) fn write_reference(path: &Path, value: &Value) -> Result<()> {
    write_new_json(path, value)
}

pub(crate) fn validate_database_name(name: &str) -> Result<()> {
    let suffix = name.strip_prefix("schema_successor_verify_").unwrap_or("");
    if !(8..=48).contains(&suffix.len())
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(refused(
            "expected database must be schema_successor_verify_<8-48 lowercase letters, digits or underscores>",
        ));
    }
    Ok(())
}

pub(crate) fn verification_reference(
    candidate: &Candidate,
    applied: &std::collections::BTreeMap<i32, String>,
    raw: &std::collections::BTreeMap<String, String>,
    base_acl: &str,
    raw_acl: &str,
    acl_lines: &[String],
    base_acl_lines: &[String],
) -> Result<Value> {
    let mut fingerprints = raw.clone();
    fingerprints.insert("acl".into(), base_acl.into());
    let changes = applied
        .iter()
        .map(|(sequence, checksum)| (sequence.to_string(), json!(checksum)))
        .collect::<Map<_, _>>();
    Ok(json!({
        "baseline_id": candidate.baseline.baseline_id,
        "sequence": CANDIDATE_EPOCH,
        "baseline_manifest_sha256": candidate.baseline_sha256,
        "changes": changes,
        "fingerprints": fingerprints,
        "raw_fingerprints": raw,
        "acl_compatibility": {
            "revision": crate::ledger_acl::REVISION,
            "manifest_sha256": crate::ledger_acl::MANIFEST_SHA256,
            "delta_line": ACL_DELTA,
            "base_acl_sha256": base_acl,
            "effective_acl_sha256": raw_acl,
            "removed_delta_count": 1,
            "raw_acl_lines": acl_lines,
            "base_acl_lines": base_acl_lines
        },
        "provenance":"Explicit PostgreSQL 16 measurement of only the 020 -> 021 extension against the exact approved epoch-20 prefix.",
        "provenance_details": {
            "tool":"rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_next_successor.rs",
            "predecessor_epoch":PREDECESSOR_EPOCH,
            "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
            "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
            "source_plan_sha256":candidate.plan_sha256,
            "candidate_sql_sha256":candidate.proposal_sha256,
            "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
            "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256,
            "git_baseline_provenance_verified":false
        }
    }))
}

fn verify_reference(candidate: &Candidate, preview_root: &Path) -> Result<String> {
    let path = preview_root.join(EPOCH_RELATIVE).join(NEXT_REFERENCE_FILE);
    let bytes = fs::read(&path)?;
    let reference: Value = serde_json::from_slice(&bytes)
        .map_err(|_| refused("invalid 021 verification reference"))?;
    let epoch = &candidate.epoch;
    let changes = epoch
        .changes
        .iter()
        .map(|change| (change.sequence.to_string(), json!(change.sha256)))
        .collect::<Map<_, _>>();
    let fingerprints = reference["fingerprints"]
        .as_object()
        .ok_or_else(|| refused("021 reference lacks four fingerprints"))?;
    let raw = reference["raw_fingerprints"]
        .as_object()
        .ok_or_else(|| refused("021 reference lacks raw fingerprints"))?;
    let expected_keys: BTreeSet<&str> = ["structural", "acl", "extensions", "reference_data"]
        .into_iter()
        .collect();
    let actual_keys: BTreeSet<&str> = fingerprints.keys().map(String::as_str).collect();
    let raw_keys: BTreeSet<&str> = raw.keys().map(String::as_str).collect();
    if !exact_keys(
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
    ) || !exact_keys(
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
    ) || !exact_keys(
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
        || actual_keys != expected_keys
        || raw_keys != expected_keys
        || fingerprints.values().chain(raw.values()).any(|value| {
            !value.as_str().is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
    {
        return Err(refused(
            "021 reference does not bind the full exact 1-21 prefix",
        ));
    }
    let raw_values: std::collections::BTreeMap<String, String> =
        serde_json::from_value(reference["raw_fingerprints"].clone())
            .map_err(|_| refused("invalid 021 raw fingerprints"))?;
    let acl_lines: Vec<String> =
        serde_json::from_value(reference["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("invalid 021 raw ACL lines"))?;
    let (base_acl, raw_acl, base_lines) =
        crate::source_tools::measure_successors::acl_reference(&raw_values, &acl_lines)?;
    if reference["acl_compatibility"]["revision"] != crate::ledger_acl::REVISION
        || reference["acl_compatibility"]["manifest_sha256"] != crate::ledger_acl::MANIFEST_SHA256
        || reference["acl_compatibility"]["delta_line"] != ACL_DELTA
        || reference["acl_compatibility"]["removed_delta_count"] != 1
        || reference["acl_compatibility"]["base_acl_sha256"] != base_acl
        || reference["acl_compatibility"]["effective_acl_sha256"] != raw_acl
        || fingerprints["acl"] != base_acl
        || reference["acl_compatibility"]["base_acl_lines"] != json!(base_lines)
        || reference["provenance_details"]["tool"]
            != "rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_next_successor.rs"
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
            "021 ACL compatibility does not remove only the exact v1 delta",
        ));
    }
    Ok(manifest::file_sha256(&path)?)
}

/// Verify the measured 021 preview, including the exact 20 preimage and no-op receipt.
pub fn verify_measured_preview(
    source_root: &Path,
    preview_root: &Path,
) -> Result<(Candidate, Value)> {
    let source_root =
        canonical_directory(source_root, "source root must be a nonsymlink directory")?;
    let preview_root =
        canonical_directory(preview_root, "preview root must be a nonsymlink directory")?;
    let (candidate, marker) = verify_preview_state(&source_root, &preview_root, true)?;
    let reference_sha = verify_reference(&candidate, &preview_root)?;
    let receipt_path = preview_root
        .join(EPOCH_RELATIVE)
        .join(NEXT_MEASUREMENT_FILE);
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)
        .map_err(|_| refused("invalid 021 measurement receipt"))?;
    let database = receipt["expected_database"].as_str().unwrap_or_default();
    if !exact_keys(
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
        || receipt["predecessor_epoch"] != PREDECESSOR_EPOCH
        || receipt["candidate_epoch"] != CANDIDATE_EPOCH
        || receipt["successor_sequences"] != json!([CANDIDATE_EPOCH])
        || validate_database_name(database).is_err()
        || receipt["source_plan_sha256"] != candidate.plan_sha256
        || receipt["approved_predecessor_package_lock_sha256"] != candidate.package_lock_sha256
        || receipt["predecessor_manifest_sha256"] != candidate.plan["predecessor_manifest_sha256"]
        || receipt["candidate_manifest_sha256"] != marker["candidate_manifest_sha256"]
        || receipt["baseline_manifest_sha256"] != candidate.baseline_sha256
        || receipt["candidate_sql_sha256"] != candidate.proposal_sha256
        || receipt["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || receipt["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || receipt["verification_ref_sha256"] != reference_sha
        || receipt["post_migration_noop"] != true
    {
        return Err(refused(
            "021 measurement receipt does not bind this measured candidate",
        ));
    }
    Ok((candidate, receipt))
}

/// Prepare a separately reviewable activation bundle without changing the source tree.
/// The owner installs its receipt and package lock only after reviewing the measured preview.
pub fn activation_bundle(source_root: &Path, preview_root: &Path, output: &Path) -> Result<Value> {
    let source_root =
        canonical_directory(source_root, "source root must be a nonsymlink directory")?;
    let preview_root =
        canonical_directory(preview_root, "preview root must be a nonsymlink directory")?;
    let (candidate, measurement) = verify_measured_preview(&source_root, &preview_root)?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused(
            "021 activation bundle output must not already exist",
        ));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid 021 activation bundle destination"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid 021 activation bundle destination"))?,
    );
    if resolved != absolute
        || absolute.starts_with(&source_root)
        || source_root.starts_with(&absolute)
        || absolute.starts_with(&preview_root)
        || preview_root.starts_with(&absolute)
    {
        return Err(refused(
            "021 activation bundle must be external to source and preview",
        ));
    }

    let epoch_dir = preview_root.join(EPOCH_RELATIVE);
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
            .map_err(|_| refused("021 activation receipt serialization failed"))?
    )
    .into_bytes();
    let activation_sha = sha256_bytes(&activation_bytes);

    let (mut package_lock, _) = archived_approved_lock(&source_root, &load_plan(&source_root)?.1)?;
    let files = package_lock["files_sha256"]
        .as_object_mut()
        .ok_or_else(|| refused("approved epoch-20 lock has no file inventory"))?;
    files.insert(
        format!("migrations/{BASELINE}/manifest.yml"),
        json!(manifest_sha),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
        json!(candidate.proposal_sha256),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
        json!(reference_sha),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
        json!(measurement_sha),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
        json!(activation_sha),
    );
    let package_lock_bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&package_lock)
            .map_err(|_| refused("021 package lock serialization failed"))?
    )
    .into_bytes();
    let package_lock_sha = sha256_bytes(&package_lock_bytes);
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
    write_new_json(&absolute.join(NEXT_ACTIVATION_FILE), &activation)?;
    write_new_bytes(
        &absolute.join(ACTIVE_LOCK_RELATIVE.rsplit('/').next().unwrap()),
        &package_lock_bytes,
    )?;
    write_new_json(&absolute.join("ACTIVATION_021_REVIEW.json"), &summary)?;
    Ok(summary)
}

fn verify_old_020_receipts(
    root: &Path,
    old_plan: &Value,
    old_plan_sha: &str,
    baseline: &manifest::BaselineManifest,
    baseline_sha: &str,
    predecessor_lock: &Value,
) -> Result<()> {
    let files = lock_files(predecessor_lock)?;
    let epoch_dir = root.join(EPOCH_RELATIVE);
    let activation_path =
        source_file(root, &format!("{EPOCH_RELATIVE}/SUCCESSOR_ACTIVATION.json"))?;
    let measurement_path = source_file(
        root,
        &format!("{EPOCH_RELATIVE}/SUCCESSOR_MEASUREMENT.json"),
    )?;
    let activation_sha = manifest::file_sha256(&activation_path)?;
    let measurement_sha = manifest::file_sha256(&measurement_path)?;
    let expected_measurement_sha = lock_digest(
        files,
        &format!("migrations/{BASELINE}/SUCCESSOR_MEASUREMENT.json"),
    )
    .ok_or_else(|| refused("approved 020 lock lost its measurement receipt"))?;
    let expected_activation_sha = lock_digest(
        files,
        &format!("migrations/{BASELINE}/SUCCESSOR_ACTIVATION.json"),
    )
    .ok_or_else(|| refused("approved 020 lock lost its activation receipt"))?;
    let approved_predecessor_package_lock_sha = lock_digest(
        files,
        &format!("migrations/{BASELINE}/017_predecessor_package_lock.json"),
    )
    .ok_or_else(|| refused("approved 020 lock lost its epoch-17 package lock"))?;
    if approved_predecessor_package_lock_sha != distribution::PREDECESSOR_LOCK_SHA256 {
        return Err(refused(
            "approved 020 lock has an unexpected epoch-17 package lock",
        ));
    }
    let retained_predecessor_package_lock = source_file(
        root,
        &format!("{EPOCH_RELATIVE}/017_predecessor_package_lock.json"),
    )?;
    if manifest::file_sha256(&retained_predecessor_package_lock)?
        != approved_predecessor_package_lock_sha
    {
        return Err(refused(
            "retained epoch-17 package lock differs from the approved 020 preimage",
        ));
    }
    let activation: Value = serde_json::from_slice(&fs::read(activation_path)?)
        .map_err(|_| refused("invalid historic 020 activation receipt"))?;
    let measurement: Value = serde_json::from_slice(&fs::read(measurement_path)?)
        .map_err(|_| refused("invalid historic 020 measurement receipt"))?;
    let current_manifest_sha =
        lock_digest(files, &format!("migrations/{BASELINE}/manifest.yml"))
            .ok_or_else(|| refused("approved 020 lock lost its epoch manifest"))?;
    if activation_sha != expected_activation_sha
        || measurement_sha != expected_measurement_sha
        || activation["schema"] != "ai-platform/canonical-successor-activation/v1"
        || activation["status"] != "activated_source_chain"
        || activation["source_activation"] != true
        || activation["epoch"] != PREDECESSOR_EPOCH
        || activation["baseline_id"] != BASELINE
        || activation["source_plan_sha256"] != old_plan_sha
        || activation["baseline_manifest_sha256"] != baseline_sha
        || activation["epoch_manifest_sha256"] != current_manifest_sha
        || activation["predecessor_manifest_sha256"] != old_plan["predecessor_manifest_sha256"]
        || activation["predecessor_package_lock_sha256"] != approved_predecessor_package_lock_sha
        || activation["measurement_receipt_sha256"] != measurement_sha
        || measurement["schema"] != "ai-platform/canonical-successor-measurement/v1"
        || measurement["status"] != "measured_external_preview"
        || measurement["database_actions"] != true
        || measurement["database_actions_scope"] != "named_isolated_database_only"
        || measurement["source_activation"] != false
        || measurement["source_plan_sha256"] != old_plan_sha
        || measurement["baseline_manifest_sha256"] != baseline_sha
        || measurement["epoch_manifest_sha256"] != current_manifest_sha
        || measurement["successor_sequences"] != json!([18, 19, 20])
        || measurement["post_migration_noop"] != true
    {
        return Err(refused(
            "historic epoch-20 receipts differ from their approved preimage",
        ));
    }
    let activation_refs = activation["verification_refs"]
        .as_object()
        .ok_or_else(|| refused("historic 020 activation lacks references"))?;
    let measurement_refs = measurement["verification_refs"]
        .as_object()
        .ok_or_else(|| refused("historic 020 measurement lacks references"))?;
    if activation_refs.len() != 3 || measurement_refs.len() != 3 {
        return Err(refused(
            "historic 020 receipts must retain exactly three references",
        ));
    }
    for sequence in 18..=20 {
        let name = format!("{sequence:03}_verification.json");
        let reference = source_file(root, &format!("{EPOCH_RELATIVE}/{name}"))?;
        let digest = manifest::file_sha256(&reference)?;
        if lock_digest(files, &format!("migrations/{BASELINE}/{name}")) != Some(digest.as_str())
            || activation_refs.get(&name).and_then(Value::as_str) != Some(digest.as_str())
            || measurement_refs.get(&name).and_then(Value::as_str) != Some(digest.as_str())
        {
            return Err(refused("historic 020 measurement reference changed"));
        }
    }
    crate::source_tools::successors::verify_activated_measurement_provenance(
        root,
        &measurement["source_distribution"],
        baseline,
        baseline_sha,
    )?;
    let paths = AuthorityPaths {
        database_dir: root.join("database"),
    };
    let manifest = manifest::load_epoch_manifest(&epoch_dir.join("manifest.yml"))?;
    let mut applied = BTreeMap::new();
    for sequence in 1..=PREDECESSOR_EPOCH {
        let spec = manifest
            .changes
            .get(sequence as usize - 1)
            .filter(|spec| spec.sequence == sequence)
            .ok_or_else(|| refused("active epoch lost an approved 020 prefix entry"))?;
        applied.insert(sequence, spec.sha256.clone());
        if (18..=20).contains(&sequence) {
            commands::expected_epoch_fingerprints(&paths, baseline, &applied)?;
        }
    }
    Ok(())
}

/// Verify an activated 021 source after the package lock and compiled lock pin
/// have been reviewed and updated by the owner.
pub fn verify_activated_source(root: &Path) -> Result<(Value, manifest::EpochManifest)> {
    let root = canonical_directory(root, "source root must be a nonsymlink directory")?;
    let (plan_value, next_plan, plan_sha) = load_plan(&root)?;
    let (approved_lock, approved_lock_sha) = archived_approved_lock(&root, &next_plan)?;
    if approved_lock_sha != APPROVED_020_LOCK_SHA256 {
        return Err(refused(
            "021 activation lost its approved 020 preimage lock",
        ));
    }
    distribution::verify(&root.join("database"), BASELINE)?;
    let paths = AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let active_lock_path = source_file(&root, ACTIVE_LOCK_RELATIVE)?;
    let active_lock_sha = manifest::file_sha256(&active_lock_path)?;
    let active_lock: Value = serde_json::from_slice(&fs::read(&active_lock_path)?)
        .map_err(|_| refused("invalid activated epoch-21 package lock"))?;
    let old_files = lock_files(&approved_lock)?;
    let active_files = lock_files(&active_lock)?;
    let epoch_path = source_file(&root, ACTIVE_MANIFEST_RELATIVE)?;
    let epoch_bytes = fs::read(&epoch_path)?;
    let epoch_value: Value = serde_json::from_slice(&epoch_bytes)
        .map_err(|_| refused("activated epoch-21 manifest is malformed"))?;
    let epoch = manifest::load_epoch_manifest(&epoch_path)?;
    let current_manifest_sha = manifest::file_sha256(&epoch_path)?;
    let (old_plan, old_plan_sha) = {
        let old_path = source_file(&root, "docs/design/canonical-schema-successors.json")?;
        (
            successors::load_source_plan(&root)?,
            manifest::file_sha256(&old_path)?,
        )
    };
    let predecessor_manifest_path = source_file(
        &root,
        &format!("{EPOCH_RELATIVE}/017_predecessor_manifest.yml"),
    )?;
    let predecessor_manifest_sha = manifest::file_sha256(&predecessor_manifest_path)?;
    if predecessor_manifest_sha != old_plan["predecessor_manifest_sha256"] {
        return Err(refused(
            "retained epoch-17 manifest differs from the historic plan",
        ));
    }
    let mut expected: Value = serde_json::from_slice(&fs::read(predecessor_manifest_path)?)
        .map_err(|_| refused("retained epoch-17 manifest is malformed"))?;
    expected["epoch"] = json!(CANDIDATE_EPOCH);
    let old_additions = old_plan["successors"]
        .as_array()
        .ok_or_else(|| refused("historic epoch-20 plan has no successor list"))?;
    for entry in old_additions {
        let change = &entry["change"];
        let proposal = source_file(
            &root,
            entry["proposal"]
                .as_str()
                .ok_or_else(|| refused("historic plan has an invalid proposal path"))?,
        )?;
        let active_sql = source_file(
            &root,
            &format!(
                "{EPOCH_RELATIVE}/{}",
                change["file"].as_str().unwrap_or_default()
            ),
        )?;
        if manifest::file_sha256(&proposal)? != change["sha256"]
            || manifest::file_sha256(&active_sql)? != change["sha256"]
            || fs::read(&proposal)? != fs::read(&active_sql)?
        {
            return Err(refused(
                "activated 018-020 SQL differs from its historical plan",
            ));
        }
        expected["changes"]
            .as_array_mut()
            .ok_or_else(|| refused("retained epoch-17 manifest has no changes"))?
            .push(change.clone());
    }
    let archived_lock_path = source_file(&root, LOCK_ARCHIVE_RELATIVE)?;
    if manifest::file_sha256(&archived_lock_path)? != APPROVED_020_LOCK_SHA256
        || lock_digest(old_files, &format!("migrations/{BASELINE}/manifest.yml"))
            != Some(next_plan.predecessor_manifest_sha256.as_str())
        || lock_digest(old_files, &format!("baselines/{BASELINE}/manifest.json"))
            != Some(baseline_sha.as_str())
    {
        return Err(refused(
            "approved epoch-20 lock does not bind its frozen preimage",
        ));
    }
    let prior_manifest_sha = lock_digest(old_files, &format!("migrations/{BASELINE}/manifest.yml"))
        .ok_or_else(|| refused("approved epoch-20 lock has no predecessor manifest"))?;
    if prior_manifest_sha != next_plan.predecessor_manifest_sha256 {
        return Err(refused(
            "approved epoch-20 manifest hash differs from the reviewed plan",
        ));
    }
    expected["epoch"] = json!(PREDECESSOR_EPOCH);
    let reconstructed_020 = format!(
        "{}\n",
        serde_json::to_string_pretty(&expected)
            .map_err(|_| refused("reconstructed epoch-20 manifest serialization failed"))?
    );
    if sha256_bytes(reconstructed_020.as_bytes()) != next_plan.predecessor_manifest_sha256 {
        return Err(refused(
            "reconstructed 020 prefix differs from the approved manifest hash",
        ));
    }
    expected["epoch"] = json!(CANDIDATE_EPOCH);
    expected["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("expected epoch-21 manifest has no changes"))?
        .push(
            serde_json::to_value(&next_plan.successor.change)
                .map_err(|_| refused("invalid reviewed 021 change"))?,
        );
    let next_sql = source_file(&root, &format!("{EPOCH_RELATIVE}/{NEXT_SQL_FILE}"))?;
    let proposal = source_file(&root, &next_plan.successor.proposal)?;
    if manifest::file_sha256(&proposal)? != next_plan.successor.change.sha256
        || manifest::file_sha256(&next_sql)? != next_plan.successor.change.sha256
        || fs::read(&proposal)? != fs::read(&next_sql)?
        || epoch_value != expected
        || epoch.baseline_id != BASELINE
        || epoch.epoch != CANDIDATE_EPOCH
        || epoch.changes.len() != CANDIDATE_EPOCH as usize
    {
        return Err(refused(
            "active source is not the exact 021 extension of its verified 020 prefix",
        ));
    }
    let active_manifest_key = format!("migrations/{BASELINE}/manifest.yml");
    if lock_digest(active_files, &active_manifest_key) != Some(current_manifest_sha.as_str())
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
        ) != Some(next_plan.successor.change.sha256.as_str())
    {
        return Err(refused(
            "activated package lock does not bind the epoch-21 source",
        ));
    }
    for (key, value) in old_files {
        if key == &active_manifest_key {
            continue;
        }
        if active_files.get(key) != Some(value) {
            return Err(refused(
                "activated package lock changed an approved 020 preimage",
            ));
        }
    }
    if baseline_sha != next_plan.approved_predecessor_baseline_manifest_sha256
        || active_lock_sha == approved_lock_sha
        || current_manifest_sha == next_plan.predecessor_manifest_sha256
    {
        return Err(refused(
            "activated epoch-21 distribution did not advance its reviewed preimage",
        ));
    }
    verify_old_020_receipts(
        &root,
        &old_plan,
        &old_plan_sha,
        &baseline,
        &baseline_sha,
        &approved_lock,
    )?;

    let candidate = Candidate {
        plan: plan_value.clone(),
        manifest_value: epoch_value,
        epoch: epoch.clone(),
        baseline,
        baseline_sha256: baseline_sha.clone(),
        package_lock_sha256: approved_lock_sha.to_owned(),
        plan_sha256: plan_sha.clone(),
        proposal_sha256: next_plan.successor.change.sha256.clone(),
        predecessor_measurement_sha256: next_plan
            .approved_predecessor_measurement_receipt_sha256
            .clone(),
        predecessor_activation_sha256: next_plan
            .approved_predecessor_activation_receipt_sha256
            .clone(),
    };
    let measurement_path =
        source_file(&root, &format!("{EPOCH_RELATIVE}/{NEXT_MEASUREMENT_FILE}"))?;
    let measurement_sha = manifest::file_sha256(&measurement_path)?;
    let measurement: Value = serde_json::from_slice(&fs::read(&measurement_path)?)
        .map_err(|_| refused("invalid activated 021 measurement receipt"))?;
    let reference_sha = verify_reference(&candidate, &root)?;
    let measurement_database = measurement["expected_database"]
        .as_str()
        .unwrap_or_default();
    if !exact_keys(
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
    ) || validate_database_name(measurement_database).is_err()
        || measurement["schema"] != "ai-platform/canonical-next-successor-measurement/v1"
        || measurement["status"] != "measured_external_preview"
        || measurement["database_actions"] != true
        || measurement["database_actions_scope"] != "named_isolated_database_only"
        || measurement["source_activation"] != false
        || measurement["source_plan_sha256"] != plan_sha
        || measurement["approved_predecessor_package_lock_sha256"] != approved_lock_sha
        || measurement["predecessor_manifest_sha256"] != next_plan.predecessor_manifest_sha256
        || measurement["candidate_manifest_sha256"] != current_manifest_sha
        || measurement["baseline_manifest_sha256"] != baseline_sha
        || measurement["candidate_sql_sha256"] != next_plan.successor.change.sha256
        || measurement["predecessor_measurement_receipt_sha256"]
            != next_plan.approved_predecessor_measurement_receipt_sha256
        || measurement["predecessor_activation_receipt_sha256"]
            != next_plan.approved_predecessor_activation_receipt_sha256
        || measurement["verification_ref_sha256"] != reference_sha
        || measurement["post_migration_noop"] != true
        || measurement["successor_sequences"] != json!([CANDIDATE_EPOCH])
    {
        return Err(refused(
            "activated 021 measurement does not bind the reviewed candidate",
        ));
    }
    let activation_path = source_file(&root, &format!("{EPOCH_RELATIVE}/{NEXT_ACTIVATION_FILE}"))?;
    let activation_sha = manifest::file_sha256(&activation_path)?;
    let activation: Value = serde_json::from_slice(&fs::read(activation_path)?)
        .map_err(|_| refused("invalid 021 activation receipt"))?;
    if !exact_keys(
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
    ) || activation["schema"] != "ai-platform/canonical-next-successor-activation/v1"
        || activation["status"] != "activated_source_chain"
        || activation["source_activation"] != true
        || activation["predecessor_epoch"] != PREDECESSOR_EPOCH
        || activation["epoch"] != CANDIDATE_EPOCH
        || activation["baseline_id"] != BASELINE
        || activation["source_plan_sha256"] != plan_sha
        || activation["approved_predecessor_package_lock_sha256"] != approved_lock_sha
        || activation["baseline_manifest_sha256"] != baseline_sha
        || activation["predecessor_manifest_sha256"] != next_plan.predecessor_manifest_sha256
        || activation["epoch_manifest_sha256"] != current_manifest_sha
        || activation["measurement_receipt_sha256"] != measurement_sha
        || activation["verification_ref_sha256"] != reference_sha
        || activation["predecessor_measurement_receipt_sha256"]
            != next_plan.approved_predecessor_measurement_receipt_sha256
        || activation["predecessor_activation_receipt_sha256"]
            != next_plan.approved_predecessor_activation_receipt_sha256
    {
        return Err(refused(
            "021 activation receipt does not bind the measured source chain",
        ));
    }
    let expected_active_files = expected_input_paths(&approved_lock, true, true)?;
    if active_files.keys().cloned().collect::<BTreeSet<_>>() != expected_active_files {
        return Err(refused(
            "activated package lock has missing or unexpected inputs",
        ));
    }
    if lock_digest(
        active_files,
        &format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
    ) != Some(reference_sha.as_str())
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
        ) != Some(measurement_sha.as_str())
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
        ) != Some(activation_sha.as_str())
    {
        return Err(refused("activated package lock does not bind 021 evidence"));
    }
    Ok((plan_value, epoch))
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
    fn current_source_is_exact_020_or_its_fully_verified_current_chain() {
        let root = repository_root();
        let path = root.join(ACTIVE_MANIFEST_RELATIVE);
        let active = manifest::load_epoch_manifest(&path).unwrap();
        match active.epoch {
            PREDECESSOR_EPOCH => {
                let candidate = validate_source(&root).unwrap();
                assert_eq!(candidate.epoch.epoch, CANDIDATE_EPOCH);
                assert_eq!(candidate.epoch.changes.len(), CANDIDATE_EPOCH as usize);
                assert_eq!(
                    serde_json::to_value(&candidate.epoch.changes[..PREDECESSOR_EPOCH as usize])
                        .unwrap(),
                    serde_json::to_value(&active.changes).unwrap()
                );
                assert_eq!(
                    candidate.proposal_sha256,
                    "b7dff15371814966c4ff244d3152680680fb16753bb1d88ce80d9502cdb7ffd1"
                );
                assert_eq!(candidate.package_lock_sha256, APPROVED_020_LOCK_SHA256);
                assert_eq!(candidate.plan["fingerprint_status"], "not_measured");
            }
            21..=28 => {
                let verified = super::historical_test_fixtures::verify_current_chain(&root);
                assert_eq!(verified.epoch, active.epoch);
                assert_eq!(verified.changes.len(), active.epoch as usize);
            }
            _ => panic!("unexpected canonical schema epoch"),
        }
    }

    #[test]
    fn historical_021_fixture_preserves_its_exact_measurement_and_reference_contract() {
        let fixture = super::historical_test_fixtures::HistoricalSourceFixture::new(21);
        let root = fixture.root();
        fixture.verify_inventory().unwrap();
        let (_, plan, plan_sha256) = load_plan(root).unwrap();
        let baseline_path = root.join(format!("database/baselines/{BASELINE}/manifest.json"));
        let baseline = manifest::load_baseline_manifest(&baseline_path).unwrap();
        let baseline_sha256 = manifest::file_sha256(&baseline_path).unwrap();
        let epoch = manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap();
        let (lock, package_lock_sha256) = archived_approved_lock(root, &plan).unwrap();
        let candidate = Candidate {
            plan: serde_json::from_slice(&fs::read(root.join(PLAN_RELATIVE)).unwrap()).unwrap(),
            manifest_value: serde_json::from_slice(
                &fs::read(root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap(),
            )
            .unwrap(),
            epoch,
            baseline,
            baseline_sha256,
            package_lock_sha256,
            plan_sha256,
            proposal_sha256: plan.successor.change.sha256.clone(),
            predecessor_measurement_sha256: plan
                .approved_predecessor_measurement_receipt_sha256
                .clone(),
            predecessor_activation_sha256: plan
                .approved_predecessor_activation_receipt_sha256
                .clone(),
        };
        assert_eq!(candidate.epoch.epoch, 21);
        assert_eq!(candidate.epoch.changes.len(), 21);
        assert_eq!(lock_files(&lock).unwrap().len(), 178);
        let reference = verify_reference(&candidate, root).unwrap();
        let measurement: Value = serde_json::from_slice(
            &fs::read(root.join(format!("{EPOCH_RELATIVE}/{NEXT_MEASUREMENT_FILE}"))).unwrap(),
        )
        .unwrap();
        let activation: Value = serde_json::from_slice(
            &fs::read(root.join(format!("{EPOCH_RELATIVE}/{NEXT_ACTIVATION_FILE}"))).unwrap(),
        )
        .unwrap();
        assert_eq!(measurement["verification_ref_sha256"], reference);
        assert_eq!(activation["source_plan_sha256"], candidate.plan_sha256);
        assert_eq!(activation["verification_ref_sha256"], reference);
        let _ = fixture.assert_release_eligibility(verify_activated_source(root));
        let path = root.join(format!("{EPOCH_RELATIVE}/{NEXT_REFERENCE_FILE}"));
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["sequence"] = json!(22);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(fixture.verify_inventory().is_err());
        assert!(verify_reference(&candidate, root).is_err());
    }

    #[test]
    fn measurement_adds_only_the_021_reference_and_receipt() {
        let root = repository_root();
        let (_, plan, _) = load_plan(&root).unwrap();
        let (lock, _) = archived_approved_lock(&root, &plan).unwrap();
        let prepared = expected_input_paths(&lock, false, false).unwrap();
        let measured = expected_input_paths(&lock, true, false).unwrap();
        let additions: BTreeSet<String> = measured.difference(&prepared).cloned().collect();
        assert_eq!(
            additions,
            BTreeSet::from([
                format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
                format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
            ])
        );
        assert!(prepared.contains(&format!("migrations/{BASELINE}/{NEXT_SQL_FILE}")));
    }
}

#[cfg(test)]
#[path = "historical_successor_test_fixtures.rs"]
pub(crate) mod historical_test_fixtures;
