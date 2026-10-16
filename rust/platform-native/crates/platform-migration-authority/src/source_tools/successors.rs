use crate::{
    AuthorityError, Result, commands, manifest, runner::AuthorityPaths,
    sql::reject_embedded_transactions,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};
const BASELINE: &str = "2026_08_post_kb_v1";
const MEASUREMENT_RECEIPT: &str = "SUCCESSOR_MEASUREMENT.json";
const ACTIVATION_RECEIPT: &str = "SUCCESSOR_ACTIVATION.json";
const PREDECESSOR_MANIFEST: &str = "017_predecessor_manifest.yml";
const PREDECESSOR_PACKAGE_LOCK: &str = "017_predecessor_package_lock.json";
const RETIRED_PREIMAGE: &str = "database/authority/ledger_acl.py";
const RETIRED_PREIMAGE_SHA256: &str =
    "9df9e3a5d6f572da0ea325720b9b346dac774055de8e9d900f3f82c2b55857fc";
const EXPECTED: &[(&str, &str)] = &[
    (
        "application_schema_admission",
        "docs/design/application-schema-admission.sql.proposal",
    ),
    (
        "knowledge_progress_trigger_authority",
        "docs/design/knowledge-progress-trigger-authority.sql.proposal",
    ),
    (
        "gateway_task_deliveries",
        "docs/design/generic-task-durable-schema.sql.proposal",
    ),
];
fn refused(text: &str) -> AuthorityError {
    AuthorityError::Refused(text.into())
}
fn safe_relative(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(refused("unsafe source path"));
    }
    Ok(())
}
fn source_path(root: &Path, relative: &str) -> Result<std::path::PathBuf> {
    safe_relative(relative)?;
    let path = Path::new(relative);
    let mut target = root.to_path_buf();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(refused("unsafe source path"));
        };
        target.push(part);
        if fs::symlink_metadata(&target)?.file_type().is_symlink() {
            return Err(refused("authority source path contains a symlink"));
        }
    }
    let target = target.canonicalize()?;
    if !target.starts_with(root.canonicalize()?) || !target.is_file() {
        return Err(refused("missing or escaped source path"));
    }
    Ok(target)
}
fn source_directory(root: &Path, relative: &str) -> Result<std::path::PathBuf> {
    safe_relative(relative)?;
    let path = Path::new(relative);
    let mut target = root.to_path_buf();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(refused("unsafe source directory"));
        };
        target.push(part);
        if fs::symlink_metadata(&target)?.file_type().is_symlink() {
            return Err(refused("authority source path contains a symlink"));
        }
    }
    let target = target.canonicalize()?;
    if !target.starts_with(root.canonicalize()?) || !target.is_dir() {
        return Err(refused("missing or escaped source directory"));
    }
    Ok(target)
}

pub fn activation_receipt_present(root: &Path) -> Result<bool> {
    let root = root.canonicalize()?;
    let directory = source_directory(&root, &format!("database/migrations/{BASELINE}"))?;
    match fs::symlink_metadata(directory.join(ACTIVATION_RECEIPT)) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(refused(
            "successor activation receipt must be a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn verified_preimage_root(
    root: &Path,
    supplied: Option<&Path>,
) -> Result<Option<std::path::PathBuf>> {
    let Some(supplied) = supplied else {
        return Ok(None);
    };
    let metadata = fs::symlink_metadata(supplied)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refused(
            "historical preimage root must be a nonsymlink directory",
        ));
    }
    let absolute = std::path::absolute(supplied)?;
    let supplied = supplied.canonicalize()?;
    if absolute != supplied {
        return Err(refused(
            "historical preimage root must not use a symlink path",
        ));
    }
    let receipt_path = source_path(root, "reports/dots-source-import-20261015.json")?;
    let receipt: Value = serde_json::from_slice(&fs::read(receipt_path)?)
        .map_err(|_| refused("invalid dots source-import receipt"))?;
    let target = receipt["target"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no target"))?;
    let target = Path::new(target)
        .canonicalize()
        .map_err(|_| refused("source-import target is unavailable"))?;
    let previous = receipt["previous_checkout"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no preserved checkout"))?;
    let previous = Path::new(previous)
        .canonicalize()
        .map_err(|_| refused("preserved source checkout is unavailable"))?;
    let archive_sha = receipt["archive_sha256"].as_str().unwrap_or_default();
    if receipt["schema"] != "ai-platform/dots-source-import/v1"
        || target != root
        || supplied != previous
        || receipt["source_files"] != 4_599
        || receipt["rust_files"] != 2_082
        || receipt["verified_source_files"] != 4_599
        || receipt["all_source_files_match_archive"] != true
        || receipt["python_source_files_in_archive"] != 0
        || receipt["git_head_preserved"] != true
        || !is_sha256(archive_sha)
        || !is_sha256(receipt["manifest_sha256"].as_str().unwrap_or_default())
    {
        return Err(refused(
            "historical preimage root does not match the verified source-import receipt",
        ));
    }
    Ok(Some(supplied))
}

fn verify_frozen_preimage(
    root: &Path,
    preimage_root: Option<&Path>,
    relative: &str,
    expected: &str,
) -> Result<()> {
    safe_relative(relative)?;
    match fs::symlink_metadata(root.join(relative)) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(refused("authority source preimage must not be a symlink"));
            }
            if manifest::file_sha256(&source_path(root, relative)?)? != expected {
                return Err(refused(&format!("exact preimage changed: {relative}")));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if relative != RETIRED_PREIMAGE || expected != RETIRED_PREIMAGE_SHA256 {
                return Err(refused(&format!(
                    "required source preimage is missing: {relative}"
                )));
            }
            let preimage_root = preimage_root
                .ok_or_else(|| refused("retired frozen preimage requires --preimage-root"))?;
            source_directory(root, "database/authority")?;
            let preimage = source_path(preimage_root, relative)?;
            if manifest::file_sha256(&preimage)? != expected {
                return Err(refused("preserved ledger ACL preimage digest mismatch"));
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn historical_preimages(plan: &Value) -> Result<Value> {
    let mut verified = serde_json::Map::new();
    for (relative, digest) in plan["frozen_preimages"]
        .as_object()
        .ok_or_else(|| refused("missing frozen preimages"))?
    {
        if relative == RETIRED_PREIMAGE {
            if digest.as_str() != Some(RETIRED_PREIMAGE_SHA256) {
                return Err(refused("retired preimage differs from the source pin"));
            }
            // This historical Python file is hash-checked but never enters the preview.
            verified.insert(relative.clone(), digest.clone());
        }
    }
    if !verified.contains_key(RETIRED_PREIMAGE) {
        return Err(refused("successor plan lost its retired preimage pin"));
    }
    Ok(Value::Object(verified))
}

pub(crate) fn load_source_plan(root: &Path) -> Result<Value> {
    let plan: Value = serde_json::from_slice(&fs::read(source_path(
        root,
        "docs/design/canonical-schema-successors.json",
    )?)?)
    .map_err(|_| refused("invalid successor plan JSON"))?;
    if plan["schema_version"] != "ai-platform/canonical-successor-source-plan/v1"
        || plan["status"] != "inactive_source_candidate"
        || plan["database_actions_authorized"] != false
        || plan["fingerprint_status"] != "not_measured"
        || plan["no_verification_files"] != true
    {
        return Err(refused(
            "historical successor plan is not the reviewed inactive plan",
        ));
    }
    let entries = plan["successors"]
        .as_array()
        .ok_or_else(|| refused("missing successors"))?;
    if plan["baseline_id"] != BASELINE
        || plan["predecessor_epoch"] != 17
        || plan["candidate_epoch"] != 20
        || entries.len() != 3
    {
        return Err(refused("unknown successor baseline or sequence allocation"));
    }
    for (index, entry) in entries.iter().enumerate() {
        let (name, proposal_relative) = EXPECTED[index];
        let sequence = index + 18;
        let change = &entry["change"];
        let proposal = source_path(
            root,
            entry["proposal"]
                .as_str()
                .ok_or_else(|| refused("invalid proposal path"))?,
        )?;
        if entry["proposal"] != proposal_relative
            || change["sequence"] != sequence
            || change["name"] != name
            || change["file"] != format!("{sequence:03}_{name}.sql")
            || change["owner"] != "owner"
            || change["transaction_mode"] != "transactional"
            || change["rollback_class"] != "forward-fix-only"
            || manifest::file_sha256(&proposal)? != change["sha256"].as_str().unwrap_or("")
        {
            return Err(refused(
                "successor plan order, authority or source hash changed",
            ));
        }
        reject_embedded_transactions(
            &fs::read_to_string(proposal)?,
            change["name"].as_str().unwrap_or(""),
        )?;
    }
    if plan["frozen_preimages"].as_object().is_none() {
        return Err(refused("missing frozen preimages"));
    }
    historical_preimages(&plan)?;
    Ok(plan)
}

pub fn validate(root: &Path) -> Result<(Value, Value)> {
    validate_with_preimage_root(root, None)
}

pub fn validate_with_preimage_root(
    root: &Path,
    preimage_root: Option<&Path>,
) -> Result<(Value, Value)> {
    let root = root.canonicalize()?;
    if activation_receipt_present(&root)? {
        return Err(refused(
            "canonical successor chain is already activated; external preview creation is closed",
        ));
    }
    let preimage_root = verified_preimage_root(&root, preimage_root)?;
    let source_paths = AuthorityPaths {
        database_dir: root.join("database"),
    };
    commands::load_baseline_with_mode(
        &source_paths,
        BASELINE,
        crate::distribution::SourceMode::Distribution,
    )?;
    let plan = load_source_plan(&root)?;
    let entries = plan["successors"]
        .as_array()
        .ok_or_else(|| refused("missing successors"))?;
    for (relative, expected) in plan["frozen_preimages"]
        .as_object()
        .ok_or_else(|| refused("missing frozen preimages"))?
    {
        verify_frozen_preimage(
            &root,
            preimage_root.as_deref(),
            relative,
            expected.as_str().unwrap_or(""),
        )?;
    }
    let directory = root.join("database/migrations").join(BASELINE);
    let path = directory.join("manifest.yml");
    let current = manifest::load_epoch_manifest(&path)?;
    if current.epoch != 17
        || manifest::file_sha256(&path)?
            != plan["predecessor_manifest_sha256"].as_str().unwrap_or("")
    {
        if current.epoch == 20 {
            return Err(refused(
                "canonical epoch-20 chain has no verified activation receipt; preview remains closed",
            ));
        }
        return Err(refused("canonical predecessor changed; re-review required"));
    }
    for number in 18..=20 {
        if directory
            .join(format!("{number:03}_verification.json"))
            .exists()
        {
            return Err(refused("unexpected successor reference in inactive source"));
        }
    }
    let mut additions = Vec::new();
    for entry in entries {
        let change = &entry["change"];
        if directory
            .join(change["file"].as_str().unwrap_or(""))
            .exists()
        {
            return Err(refused(
                "inactive successor already entered migration discovery",
            ));
        }
        additions.push(change.clone());
    }
    let mut result: Value = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| refused("canonical manifest must be JSON-compatible"))?;
    result["epoch"] = json!(20);
    result["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("invalid current change array"))?
        .extend(additions);
    Ok((plan, result))
}
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        return Err(refused("authority preview refuses source symlinks"));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(refused("authority preview refuses source symlinks"));
        }
        if kind.is_dir() {
            copy_tree(&path, &target.join(entry.file_name()))?;
        } else if kind.is_file() {
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target.join(entry.file_name()))?;
            output.write_all(&fs::read(path)?)?;
        } else {
            return Err(refused("unsupported authority source file type"));
        }
    }
    Ok(())
}

fn add_file(root: &Path, relative: &str, files: &mut BTreeMap<String, String>) -> Result<()> {
    let source = source_path(root, relative)?;
    files.insert(relative.to_owned(), manifest::file_sha256(&source)?);
    Ok(())
}

fn add_tree(root: &Path, relative: &str, files: &mut BTreeMap<String, String>) -> Result<()> {
    let source = source_directory(root, relative)?;
    fn walk(root: &Path, path: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let child = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(refused("authority preview refuses source symlinks"));
            }
            if kind.is_dir() {
                walk(root, &child, files)?;
            } else if kind.is_file() {
                let relative = child
                    .strip_prefix(root)
                    .map_err(|_| refused("authority support path escaped source"))?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, manifest::file_sha256(&child)?);
            } else {
                return Err(refused("unsupported authority source file type"));
            }
        }
        Ok(())
    }
    walk(root, &source, files)
}

fn legacy_support_files(root: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    let manifest_relative = "database/migrations/legacy-manifest.yml";
    let legacy_manifest = manifest::load_legacy_manifest(&source_path(root, manifest_relative)?)?;
    add_file(root, manifest_relative, files)?;
    for file in legacy_manifest
        .changes
        .iter()
        .map(|entry| entry.file.as_str())
        .chain(
            legacy_manifest
                .historical_rollbacks
                .iter()
                .map(|entry| entry.file.as_str()),
        )
    {
        let direct = format!("database/migrations/{file}");
        let nested = format!("database/migrations/legacy/{file}");
        let relative = if root.join(&direct).exists() {
            direct
        } else {
            nested
        };
        add_file(root, &relative, files)?;
    }
    Ok(())
}

fn support_files(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    add_tree(root, "database/bootstrap", &mut files)?;
    add_file(root, "database/authority/ledger_acl_v1.json", &mut files)?;
    legacy_support_files(root, &mut files)?;
    // The native legacy reconciliation path reads this tree when it is present.
    add_tree(root, "database/migrations/per_service", &mut files)?;
    Ok(files)
}

fn copy_support(root: &Path, preview: &Path, files: &BTreeMap<String, String>) -> Result<()> {
    for relative in files.keys() {
        let source = source_path(root, relative)?;
        let destination = preview.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        output.write_all(&fs::read(source)?)?;
    }
    Ok(())
}

fn compare_epoch(expected: &Value, actual: &manifest::EpochManifest) -> Result<()> {
    let expected_changes: Vec<manifest::ChangeSpec> =
        serde_json::from_value(expected["changes"].clone())
            .map_err(|_| refused("invalid materialized epoch contract"))?;
    if actual.baseline_id != expected["baseline_id"].as_str().unwrap_or_default()
        || actual.epoch != expected["epoch"].as_i64().unwrap_or_default() as i32
        || !actual
            .extra
            .as_object()
            .is_some_and(|extra| extra.is_empty())
        || serde_json::to_value(&actual.changes).map_err(|_| refused("invalid epoch changes"))?
            != serde_json::to_value(expected_changes)
                .map_err(|_| refused("invalid expected changes"))?
    {
        return Err(refused(
            "external preview epoch differs from validated source",
        ));
    }
    Ok(())
}

fn collect_files(root: &Path) -> Result<BTreeSet<String>> {
    fn walk(root: &Path, path: &Path, files: &mut BTreeSet<String>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let child = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(refused("authority preview refuses symlinks"));
            }
            if kind.is_dir() {
                walk(root, &child, files)?;
            } else if kind.is_file() {
                files.insert(
                    child
                        .strip_prefix(root)
                        .map_err(|_| refused("preview file escaped output root"))?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            } else {
                return Err(refused("unsupported authority preview file type"));
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

fn expected_preview_files(
    source_root: &Path,
    plan: &Value,
    expected_manifest: &Value,
    measured: bool,
) -> Result<BTreeSet<String>> {
    let mut files: BTreeSet<String> = support_files(source_root)?.into_keys().collect();
    let baseline_relative = format!("database/baselines/{BASELINE}");
    let baseline = manifest::load_baseline_manifest(&source_path(
        source_root,
        &format!("{baseline_relative}/manifest.json"),
    )?)?;
    files.insert(format!("{baseline_relative}/manifest.json"));
    files.extend(
        baseline
            .files_sha256
            .keys()
            .map(|name| format!("{baseline_relative}/{name}")),
    );
    files.extend(
        baseline
            .policy_files_sha256
            .keys()
            .map(|name| format!("{baseline_relative}/{name}")),
    );
    let epoch_relative = format!("database/migrations/{BASELINE}");
    files.insert(format!("{epoch_relative}/manifest.yml"));
    let changes: Vec<manifest::ChangeSpec> =
        serde_json::from_value(expected_manifest["changes"].clone())
            .map_err(|_| refused("invalid expected preview changes"))?;
    for change in changes {
        files.insert(format!("{epoch_relative}/{}", change.file));
        if change.sequence <= 17 || measured {
            files.insert(format!(
                "{epoch_relative}/{:03}_verification.json",
                change.sequence
            ));
        }
    }
    files.insert("PREVIEW_NOT_ACTIVATED.json".into());
    if measured {
        files.insert(MEASUREMENT_RECEIPT.into());
    }
    if plan["baseline_id"] != BASELINE {
        return Err(refused("preview plan baseline mismatch"));
    }
    Ok(files)
}

fn verify_common(
    source_root: &Path,
    preview_root: &Path,
    measured: bool,
    preimage_root: Option<&Path>,
) -> Result<(
    Value,
    manifest::EpochManifest,
    manifest::BaselineManifest,
    String,
)> {
    if fs::symlink_metadata(preview_root)?.file_type().is_symlink() {
        return Err(refused("authority preview root must not be a symlink"));
    }
    let source_root = source_root.canonicalize()?;
    let preview_root = preview_root.canonicalize()?;
    if source_root == preview_root
        || source_root.starts_with(&preview_root)
        || preview_root.starts_with(&source_root)
    {
        return Err(refused(
            "authority preview and source roots must be external",
        ));
    }
    let (plan, expected_manifest) = validate_with_preimage_root(&source_root, preimage_root)?;
    let source_support = support_files(&source_root)?;
    let preview_support = support_files(&preview_root)?;
    if preview_support != source_support {
        return Err(refused(
            "authority preview support inputs differ from source",
        ));
    }
    let source_paths = AuthorityPaths {
        database_dir: source_root.join("database"),
    };
    let preview_paths = AuthorityPaths {
        database_dir: preview_root.join("database"),
    };
    let (source_baseline, source_baseline_sha) = commands::load_baseline_with_mode(
        &source_paths,
        BASELINE,
        crate::distribution::SourceMode::Distribution,
    )?;
    let (baseline, baseline_sha) = commands::load_baseline_with_mode(
        &preview_paths,
        BASELINE,
        crate::distribution::SourceMode::Development,
    )?;
    if source_baseline_sha != baseline_sha || source_baseline.baseline_id != baseline.baseline_id {
        return Err(refused("preview baseline differs from validated source"));
    }
    let source_epoch =
        manifest::load_epoch_manifest(&source_paths.epoch_dir(BASELINE).join("manifest.yml"))?;
    if source_epoch.epoch != 17 || source_epoch.changes.len() != 17 {
        return Err(refused(
            "source epoch is not the frozen 17-change predecessor",
        ));
    }
    let epoch_path = preview_paths.epoch_dir(BASELINE).join("manifest.yml");
    let epoch = manifest::load_epoch_manifest(&epoch_path)?;
    compare_epoch(&expected_manifest, &epoch)?;
    if epoch.changes.len() != 20
        || serde_json::to_value(&source_epoch.changes)
            .map_err(|_| refused("cannot bind source epoch"))?
            != serde_json::to_value(&epoch.changes[..17])
                .map_err(|_| refused("cannot bind preview predecessor"))?
    {
        return Err(refused(
            "preview is not the exact 20-change source successor set",
        ));
    }
    let receipt_path = preview_root.join("PREVIEW_NOT_ACTIVATED.json");
    let receipt: Value = serde_json::from_slice(&fs::read(receipt_path)?)
        .map_err(|_| refused("invalid authority preview receipt"))?;
    let support_json =
        serde_json::to_value(&source_support).map_err(|_| refused("invalid support inventory"))?;
    let source_plan_sha = manifest::file_sha256(&source_path(
        &source_root,
        "docs/design/canonical-schema-successors.json",
    )?)?;
    let manifest_sha = manifest::file_sha256(&epoch_path)?;
    let baseline_path = preview_paths.baseline_dir(BASELINE).join("manifest.json");
    let baseline_manifest_sha = manifest::file_sha256(&baseline_path)?;
    let historical = historical_preimages(&plan)?;
    let changes = epoch
        .changes
        .iter()
        .map(|change| (change.sequence.to_string(), json!(change.sha256)))
        .collect::<serde_json::Map<_, _>>();
    if receipt["status"] != "inactive_external_preview_only"
        || receipt["epoch"] != 20
        || receipt["database_actions"] != false
        || receipt["missing_measurements"] != json!([18, 19, 20])
        || receipt["source_plan_sha256"] != source_plan_sha
        || receipt["manifest_sha256"] != manifest_sha
        || receipt["baseline_manifest_sha256"] != baseline_manifest_sha
        || receipt["support_files_sha256"] != support_json
        || receipt["historical_preimages_sha256"] != historical
        || receipt["changes"] != Value::Object(changes)
    {
        return Err(refused("inactive preview receipt is not source-bound"));
    }
    let candidate_epoch_manifest = format!("database/migrations/{BASELINE}/manifest.yml");
    for (relative, expected) in plan["frozen_preimages"]
        .as_object()
        .ok_or_else(|| refused("missing frozen preimages"))?
    {
        if relative == RETIRED_PREIMAGE {
            if expected.as_str() != Some(RETIRED_PREIMAGE_SHA256)
                || historical.get(relative).and_then(Value::as_str) != expected.as_str()
            {
                return Err(refused("historical preview preimage digest mismatch"));
            }
            continue;
        }
        // The canonical epoch manifest is a frozen source preimage but is
        // deliberately advanced in the candidate. `validate_with_preimage_root`
        // verifies its source digest, while `compare_epoch` and the receipts
        // bind the generated candidate manifest to the exact successor plan.
        if relative == &candidate_epoch_manifest {
            continue;
        }
        let candidate = source_path(&preview_root, relative)?;
        if manifest::file_sha256(&candidate)? != expected.as_str().unwrap_or("") {
            return Err(refused(&format!(
                "authority preview frozen preimage mismatch: {relative}"
            )));
        }
    }
    let expected_files = expected_preview_files(&source_root, &plan, &expected_manifest, measured)?;
    if collect_files(&preview_root)? != expected_files {
        return Err(refused(
            "authority preview contains missing or unexpected files",
        ));
    }
    if !measured {
        return Ok((plan, epoch, baseline, baseline_sha));
    }
    let preimage_root = verified_preimage_root(&source_root, preimage_root)?
        .ok_or_else(|| refused("measured preview requires --preimage-root"))?;
    verify_measurement_receipt(
        &source_root,
        &preview_root,
        &preimage_root,
        &plan,
        &epoch,
        &baseline,
        &baseline_sha,
        &source_plan_sha,
        &manifest_sha,
    )?;
    let applied_prefix = |sequence: i32| {
        epoch
            .changes
            .iter()
            .filter(|change| change.sequence <= sequence)
            .map(|change| (change.sequence, change.sha256.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    for sequence in 18..=20 {
        commands::expected_epoch_fingerprints(
            &preview_paths,
            &baseline,
            &applied_prefix(sequence),
        )?;
    }
    Ok((plan, epoch, baseline, baseline_sha))
}

fn verify_measurement_receipt(
    source_root: &Path,
    preview_root: &Path,
    preimage_root: &Path,
    plan: &Value,
    epoch: &manifest::EpochManifest,
    baseline: &manifest::BaselineManifest,
    baseline_sha: &str,
    source_plan_sha: &str,
    epoch_sha: &str,
) -> Result<()> {
    let receipt: Value = serde_json::from_slice(&fs::read(preview_root.join(MEASUREMENT_RECEIPT))?)
        .map_err(|_| refused("invalid successor measurement receipt"))?;
    let database = receipt["expected_database"].as_str().unwrap_or_default();
    if receipt["schema"] != "ai-platform/canonical-successor-measurement/v1"
        || receipt["status"] != "measured_external_preview"
        || receipt["database_actions"] != true
        || receipt["database_actions_scope"] != "named_isolated_database_only"
        || receipt["source_activation"] != false
        || !regex::Regex::new(r"^schema_successor_verify_[a-z0-9_]{8,48}$")
            .unwrap()
            .is_match(database)
        || receipt["successor_sequences"] != json!([18, 19, 20])
        || receipt["post_migration_noop"] != true
        || receipt["source_plan_sha256"] != source_plan_sha
        || receipt["baseline_manifest_sha256"] != baseline_sha
        || receipt["epoch_manifest_sha256"] != epoch_sha
    {
        return Err(refused(
            "successor measurement receipt does not bind this preview",
        ));
    }
    let refs = receipt["verification_refs"]
        .as_object()
        .ok_or_else(|| refused("measurement receipt lacks verification references"))?;
    if refs.len() != 3 {
        return Err(refused(
            "measurement receipt must bind exactly three references",
        ));
    }
    for sequence in 18..=20 {
        let name = format!("{sequence:03}_verification.json");
        let path = preview_root
            .join("database/migrations")
            .join(BASELINE)
            .join(&name);
        let digest = manifest::file_sha256(&path)?;
        if refs.get(&name).and_then(Value::as_str) != Some(digest.as_str()) {
            return Err(refused("measurement receipt reference digest mismatch"));
        }
    }
    if plan["baseline_id"] != baseline.baseline_id || epoch.epoch != 20 {
        return Err(refused("measurement receipt baseline or epoch mismatch"));
    }
    verify_source_distribution(
        source_root,
        preimage_root,
        &receipt["source_distribution"],
        baseline,
        baseline_sha,
    )?;
    Ok(())
}

fn verify_source_distribution(
    source_root: &Path,
    preimage_root: &Path,
    evidence: &Value,
    baseline: &manifest::BaselineManifest,
    baseline_sha: &str,
) -> Result<()> {
    let import_path = source_path(source_root, "reports/dots-source-import-20261015.json")?;
    let raw: Value = serde_json::from_slice(&fs::read(&import_path)?)
        .map_err(|_| refused("invalid dots source-import receipt"))?;
    let target = raw["target"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no target"))?;
    let target = Path::new(target)
        .canonicalize()
        .map_err(|_| refused("source-import target is unavailable"))?;
    let archive_sha = raw["archive_sha256"].as_str().unwrap_or_default();
    let manifest_sha = raw["manifest_sha256"].as_str().unwrap_or_default();
    let archive_path = raw["source_archive"]
        .as_str()
        .map(Path::new)
        .ok_or_else(|| refused("source-import receipt has no archive path"))?;
    let archive_metadata = fs::symlink_metadata(archive_path)?;
    let previous = raw["previous_checkout"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no preserved checkout"))?;
    let previous = Path::new(previous)
        .canonicalize()
        .map_err(|_| refused("preserved source checkout is unavailable"))?;
    let (producer_sha, replacements_verified) =
        crate::source_tools::measure_successors::verify_native_producer_descriptor(
            source_root,
            baseline,
            baseline_sha,
        )?;
    let receipt_sha = manifest::file_sha256(&import_path)?;
    if target != source_root
        || raw["schema"] != "ai-platform/dots-source-import/v1"
        || raw["source_files"] != 4_599
        || raw["rust_files"] != 2_082
        || raw["verified_source_files"] != 4_599
        || raw["all_source_files_match_archive"] != true
        || raw["python_source_files_in_archive"] != 0
        || raw["git_head_preserved"] != true
        || !is_sha256(archive_sha)
        || !is_sha256(manifest_sha)
        || !archive_path.is_absolute()
        || archive_metadata.file_type().is_symlink()
        || !archive_metadata.is_file()
        || manifest::file_sha256(archive_path)? != archive_sha
        || previous.as_path() != preimage_root
        || evidence["archive_sha256"] != archive_sha
        || evidence["import_manifest_sha256"] != manifest_sha
        || evidence["import_receipt_sha256"] != receipt_sha
        || evidence["source_files"] != 4_599
        || evidence["rust_files"] != 2_082
        || evidence["verified_source_files"] != 4_599
        || evidence["all_source_files_match_archive"] != true
        || evidence["python_source_files_in_archive"] != 0
        || evidence["producer_descriptor_sha256"] != producer_sha
        || evidence["producer_replacements_verified"] != replacements_verified
        || evidence["historical_preimage_sha256"] != RETIRED_PREIMAGE_SHA256
        || evidence["historical_preimage_verified"] != true
        || evidence["historical_git_blobs_verified"] != false
    {
        return Err(refused(
            "measurement source distribution does not match the imported source receipt",
        ));
    }
    Ok(())
}

pub(crate) fn verify_activated_measurement_provenance(
    root: &Path,
    evidence: &Value,
    baseline: &manifest::BaselineManifest,
    baseline_sha: &str,
) -> Result<()> {
    let import_path = source_path(root, "reports/dots-source-import-20261015.json")?;
    let import: Value = serde_json::from_slice(&fs::read(&import_path)?)
        .map_err(|_| refused("invalid dots source-import receipt"))?;
    let target = import["target"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no target"))?;
    let target = Path::new(target)
        .canonicalize()
        .map_err(|_| refused("source-import receipt target is unavailable"))?;
    let source_archive = import["source_archive"]
        .as_str()
        .ok_or_else(|| refused("source-import receipt has no archive path"))?;
    let archive_sha = import["archive_sha256"].as_str().unwrap_or_default();
    let import_manifest_sha = import["manifest_sha256"].as_str().unwrap_or_default();
    let import_receipt_sha = manifest::file_sha256(&import_path)?;
    let (producer_sha, replacement_count) =
        crate::source_tools::measure_successors::verify_native_producer_descriptor(
            root,
            baseline,
            baseline_sha,
        )?;
    if target != root
        || import["schema"] != "ai-platform/dots-source-import/v1"
        || import["source_files"] != 4_599
        || import["rust_files"] != 2_082
        || import["verified_source_files"] != 4_599
        || import["all_source_files_match_archive"] != true
        || import["python_source_files_in_archive"] != 0
        || import["git_head_preserved"] != true
        || !Path::new(source_archive).is_absolute()
        || !is_sha256(archive_sha)
        || !is_sha256(import_manifest_sha)
        || evidence["archive_sha256"] != archive_sha
        || evidence["import_manifest_sha256"] != import_manifest_sha
        || evidence["import_receipt_sha256"] != import_receipt_sha
        || evidence["source_files"] != 4_599
        || evidence["rust_files"] != 2_082
        || evidence["verified_source_files"] != 4_599
        || evidence["all_source_files_match_archive"] != true
        || evidence["python_source_files_in_archive"] != 0
        || evidence["producer_descriptor_sha256"] != producer_sha
        || evidence["producer_replacements_verified"] != replacement_count
        || evidence["historical_preimage_sha256"] != RETIRED_PREIMAGE_SHA256
        || evidence["historical_preimage_verified"] != true
        || evidence["historical_git_blobs_verified"] != false
    {
        return Err(refused(
            "activated measurement provenance does not match the locked source receipt",
        ));
    }
    Ok(())
}

/// Verifies the canonical epoch-20 chain after the explicit activation receipt is installed.
/// The inactive preview path remains strict and continues to require the exact epoch-17 source.
pub fn verify_activated_source(root: &Path) -> Result<(Value, manifest::EpochManifest)> {
    let root = root.canonicalize()?;
    let current_manifest = manifest::load_epoch_manifest(
        &root
            .join("database/migrations")
            .join(BASELINE)
            .join("manifest.yml"),
    )?;
    if current_manifest.epoch == crate::source_tools::next_successor_022::CANDIDATE_EPOCH {
        return crate::source_tools::next_successor_022::verify_activated_source(&root);
    }
    if current_manifest.epoch == crate::source_tools::next_successor::CANDIDATE_EPOCH {
        return crate::source_tools::next_successor::verify_activated_source(&root);
    }
    if !activation_receipt_present(&root)? {
        return Err(refused("canonical successor activation receipt is absent"));
    }
    let paths = AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        crate::distribution::SourceMode::Distribution,
    )?;
    let plan = load_source_plan(&root)?;
    let plan_sha = manifest::file_sha256(&source_path(
        &root,
        "docs/design/canonical-schema-successors.json",
    )?)?;
    let epoch_dir = paths.epoch_dir(BASELINE);
    let epoch_manifest_path = epoch_dir.join("manifest.yml");
    let predecessor_manifest_path = source_path(
        &root,
        &format!("database/migrations/{BASELINE}/{PREDECESSOR_MANIFEST}"),
    )?;
    let predecessor_manifest_sha = manifest::file_sha256(&predecessor_manifest_path)?;
    if predecessor_manifest_sha
        != plan["predecessor_manifest_sha256"]
            .as_str()
            .unwrap_or_default()
    {
        return Err(refused(
            "retained epoch-17 manifest differs from the reviewed predecessor",
        ));
    }
    // This retained manifest describes the authenticated old prefix. The
    // directory now also contains 018-020; only the active manifest owns that
    // directory's complete SQL inventory. Its normal loader and the exact
    // prefix/hash checks below still validate every admitted SQL file.
    #[derive(serde::Deserialize)]
    struct RetainedEpochPrefix {
        baseline_id: String,
        epoch: i32,
        changes: Vec<manifest::ChangeSpec>,
    }
    let predecessor_epoch: RetainedEpochPrefix =
        serde_json::from_slice(&fs::read(&predecessor_manifest_path)?)
            .map_err(|_| refused("invalid retained epoch-17 manifest"))?;
    if predecessor_epoch.baseline_id != BASELINE
        || predecessor_epoch.epoch != 17
        || predecessor_epoch.changes.len() != 17
    {
        return Err(refused(
            "retained predecessor is not the exact epoch-17 manifest",
        ));
    }

    let predecessor_lock_path = source_path(
        &root,
        &format!("database/migrations/{BASELINE}/{PREDECESSOR_PACKAGE_LOCK}"),
    )?;
    let predecessor_lock_sha = manifest::file_sha256(&predecessor_lock_path)?;
    if predecessor_lock_sha != crate::distribution::PREDECESSOR_LOCK_SHA256 {
        return Err(refused(
            "retained epoch-17 package lock differs from its release pin",
        ));
    }
    let predecessor_lock: Value = serde_json::from_slice(&fs::read(&predecessor_lock_path)?)
        .map_err(|_| refused("invalid retained epoch-17 package lock"))?;
    if predecessor_lock["schema"] != "migration-authority/source-distribution/v1"
        || predecessor_lock["baseline_id"] != BASELINE
    {
        return Err(refused(
            "retained epoch-17 package lock has an unknown identity",
        ));
    }
    let predecessor_files = predecessor_lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("retained epoch-17 package lock has no file inventory"))?;
    let predecessor_epoch_lock_key = format!("migrations/{BASELINE}/manifest.yml");
    let predecessor_baseline_lock_key = format!("baselines/{BASELINE}/manifest.json");
    if predecessor_files
        .get(predecessor_epoch_lock_key.as_str())
        .and_then(Value::as_str)
        != Some(predecessor_manifest_sha.as_str())
        || predecessor_files
            .get(predecessor_baseline_lock_key.as_str())
            .and_then(Value::as_str)
            != Some(baseline_sha.as_str())
    {
        return Err(refused(
            "retained epoch-17 package lock does not bind the predecessor and baseline",
        ));
    }
    let predecessor_manifest_relative = format!("database/migrations/{BASELINE}/manifest.yml");
    for (relative, expected) in plan["frozen_preimages"]
        .as_object()
        .ok_or_else(|| refused("missing frozen preimages"))?
    {
        let digest = expected.as_str().unwrap_or_default();
        if relative == RETIRED_PREIMAGE {
            if digest != RETIRED_PREIMAGE_SHA256 {
                return Err(refused(
                    "retired historical preimage pin changed or entered the active source",
                ));
            }
            match fs::symlink_metadata(root.join(relative)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(refused(
                        "retired historical preimage pin changed or entered the active source",
                    ));
                }
                Err(error) => return Err(error.into()),
            }
            continue;
        }
        let lock_relative = relative
            .strip_prefix("database/")
            .ok_or_else(|| refused("frozen preimage path is outside database artifacts"))?;
        if predecessor_files.get(lock_relative).and_then(Value::as_str) != Some(digest) {
            return Err(refused(&format!(
                "retained epoch-17 package lock does not bind frozen preimage: {relative}"
            )));
        }
        if relative == &predecessor_manifest_relative {
            continue;
        }
        let current = source_path(&root, relative)?;
        if manifest::file_sha256(&current)? != digest {
            return Err(refused(&format!(
                "activated source frozen preimage mismatch: {relative}"
            )));
        }
    }

    let mut expected_manifest: Value =
        serde_json::from_slice(&fs::read(&predecessor_manifest_path)?)
            .map_err(|_| refused("retained epoch-17 manifest must be JSON-compatible"))?;
    expected_manifest["epoch"] = json!(20);
    let additions = plan["successors"]
        .as_array()
        .ok_or_else(|| refused("missing successors"))?;
    for entry in additions {
        let change = &entry["change"];
        let proposal = source_path(
            &root,
            entry["proposal"]
                .as_str()
                .ok_or_else(|| refused("invalid proposal path"))?,
        )?;
        let migration_relative = format!(
            "database/migrations/{BASELINE}/{}",
            change["file"].as_str().unwrap_or_default()
        );
        let migration = source_path(&root, &migration_relative)?;
        if manifest::file_sha256(&proposal)? != change["sha256"].as_str().unwrap_or_default()
            || manifest::file_sha256(&migration)? != change["sha256"].as_str().unwrap_or_default()
            || fs::read(&proposal)? != fs::read(&migration)?
        {
            return Err(refused(&format!(
                "activated successor SQL differs from reviewed proposal: {migration_relative}"
            )));
        }
        expected_manifest["changes"]
            .as_array_mut()
            .ok_or_else(|| refused("invalid predecessor change array"))?
            .push(change.clone());
    }
    let epoch = manifest::load_epoch_manifest(&epoch_manifest_path)?;
    compare_epoch(&expected_manifest, &epoch)?;
    if epoch.baseline_id != BASELINE
        || epoch.epoch != 20
        || epoch.changes.len() != 20
        || serde_json::to_value(&predecessor_epoch.changes)
            .map_err(|_| refused("cannot bind retained predecessor changes"))?
            != serde_json::to_value(&epoch.changes[..17])
                .map_err(|_| refused("cannot bind active predecessor changes"))?
    {
        return Err(refused(
            "active source is not the exact measured epoch-20 chain",
        ));
    }

    let activation_path = source_path(
        &root,
        &format!("database/migrations/{BASELINE}/{ACTIVATION_RECEIPT}"),
    )?;
    let activation: Value = serde_json::from_slice(&fs::read(&activation_path)?)
        .map_err(|_| refused("invalid successor activation receipt"))?;
    let measurement_path = source_path(
        &root,
        &format!("database/migrations/{BASELINE}/{MEASUREMENT_RECEIPT}"),
    )?;
    let measurement_sha = manifest::file_sha256(&measurement_path)?;
    let measurement: Value = serde_json::from_slice(&fs::read(&measurement_path)?)
        .map_err(|_| refused("invalid retained successor measurement receipt"))?;
    let epoch_manifest_sha = manifest::file_sha256(&epoch_manifest_path)?;
    if activation["schema"] != "ai-platform/canonical-successor-activation/v1"
        || activation["status"] != "activated_source_chain"
        || activation["source_activation"] != true
        || activation["epoch"] != 20
        || activation["baseline_id"] != BASELINE
        || activation["source_plan_sha256"] != plan_sha
        || activation["baseline_manifest_sha256"] != baseline_sha
        || activation["epoch_manifest_sha256"] != epoch_manifest_sha
        || activation["predecessor_manifest_sha256"] != predecessor_manifest_sha
        || activation["predecessor_package_lock_sha256"] != predecessor_lock_sha
        || activation["measurement_receipt_sha256"] != measurement_sha
    {
        return Err(refused(
            "successor activation receipt does not bind the active chain",
        ));
    }
    if measurement["schema"] != "ai-platform/canonical-successor-measurement/v1"
        || measurement["status"] != "measured_external_preview"
        || measurement["database_actions"] != true
        || measurement["database_actions_scope"] != "named_isolated_database_only"
        || measurement["source_activation"] != false
        || measurement["source_plan_sha256"] != plan_sha
        || measurement["baseline_manifest_sha256"] != baseline_sha
        || measurement["epoch_manifest_sha256"] != epoch_manifest_sha
        || measurement["successor_sequences"] != json!([18, 19, 20])
        || measurement["post_migration_noop"] != true
        || !regex::Regex::new(r"^schema_successor_verify_[a-z0-9_]{8,48}$")
            .unwrap()
            .is_match(
                measurement["expected_database"]
                    .as_str()
                    .unwrap_or_default(),
            )
    {
        return Err(refused(
            "retained measurement receipt does not bind the active chain",
        ));
    }
    let activation_refs = activation["verification_refs"]
        .as_object()
        .ok_or_else(|| refused("activation receipt lacks verification references"))?;
    let measurement_refs = measurement["verification_refs"]
        .as_object()
        .ok_or_else(|| refused("measurement receipt lacks verification references"))?;
    if activation_refs.len() != 3 || measurement_refs.len() != 3 {
        return Err(refused(
            "activation must bind exactly three measured references",
        ));
    }
    verify_activated_measurement_provenance(
        &root,
        &measurement["source_distribution"],
        &baseline,
        &baseline_sha,
    )?;
    let mut applied = BTreeMap::new();
    for sequence in 1..=20 {
        let spec = epoch
            .changes
            .get((sequence - 1) as usize)
            .filter(|change| change.sequence == sequence)
            .ok_or_else(|| refused("active epoch lost a migration sequence"))?;
        applied.insert(sequence, spec.sha256.clone());
        if !(18..=20).contains(&sequence) {
            continue;
        }
        let name = format!("{sequence:03}_verification.json");
        let reference_path = source_path(&root, &format!("database/migrations/{BASELINE}/{name}"))?;
        let reference_sha = manifest::file_sha256(&reference_path)?;
        if activation_refs.get(&name).and_then(Value::as_str) != Some(reference_sha.as_str())
            || measurement_refs.get(&name).and_then(Value::as_str) != Some(reference_sha.as_str())
        {
            return Err(refused(
                "activation reference digest does not match measured evidence",
            ));
        }
        commands::expected_epoch_fingerprints(&paths, &baseline, &applied)?;
    }
    Ok((plan, epoch))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Strict premeasurement validation. Requires absent 018–020 verification refs.
pub fn verify_preview(
    source_root: &Path,
    preview_root: &Path,
) -> Result<(Value, manifest::EpochManifest)> {
    verify_preview_with_preimage_root(source_root, preview_root, None)
}

pub fn verify_preview_with_preimage_root(
    source_root: &Path,
    preview_root: &Path,
    preimage_root: Option<&Path>,
) -> Result<(Value, manifest::EpochManifest)> {
    let (plan, epoch, _, _) = verify_common(source_root, preview_root, false, preimage_root)?;
    Ok((plan, epoch))
}

/// Validates measured artifacts against the same source and exact external preview.
/// All three verification refs and their final no-op receipt must be present.
pub fn verify_measured_preview(
    source_root: &Path,
    preview_root: &Path,
) -> Result<(Value, manifest::EpochManifest)> {
    verify_measured_preview_with_preimage_root(source_root, preview_root, None)
}

pub fn verify_measured_preview_with_preimage_root(
    source_root: &Path,
    preview_root: &Path,
    preimage_root: Option<&Path>,
) -> Result<(Value, manifest::EpochManifest)> {
    let (plan, epoch, _, _) = verify_common(source_root, preview_root, true, preimage_root)?;
    Ok((plan, epoch))
}
pub fn materialize(root: &Path, output: &Path) -> Result<Value> {
    materialize_with_preimage_root(root, output, None)
}

pub fn materialize_with_preimage_root(
    root: &Path,
    output: &Path,
    preimage_root: Option<&Path>,
) -> Result<Value> {
    let root = root.canonicalize()?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused("preview output must not already exist"));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid preview destination"))?;
    let resolved = parent.canonicalize()?.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid preview destination"))?,
    );
    if absolute != resolved || resolved.starts_with(&root) || root.starts_with(&resolved) {
        return Err(refused(
            "preview must be a nonsymlink external descendant of another directory",
        ));
    }
    let (plan, manifest) = validate_with_preimage_root(&root, preimage_root)?;
    let support_files = support_files(&root)?;
    // Exclusive creation: no overwrite or source-tree mutation mode.
    fs::create_dir(&absolute)?;
    for relative in [
        format!("database/baselines/{BASELINE}"),
        format!("database/migrations/{BASELINE}"),
    ] {
        copy_tree(&root.join(&relative), &absolute.join(&relative))?;
    }
    copy_support(&root, &absolute, &support_files)?;
    let directory = absolute.join("database/migrations").join(BASELINE);
    for entry in plan["successors"].as_array().unwrap() {
        let destination = directory.join(entry["change"]["file"].as_str().unwrap());
        let mut stream = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        stream.write_all(&fs::read(source_path(
            &root,
            entry["proposal"].as_str().unwrap(),
        )?)?)?;
    }
    fs::write(
        directory.join("manifest.yml"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest)
                .map_err(|_| refused("manifest serialization failed"))?
        ),
    )?;
    let valid = manifest::load_epoch_manifest(&directory.join("manifest.yml"))?;
    let changes = valid
        .changes
        .iter()
        .map(|c| (c.sequence.to_string(), json!(c.sha256)))
        .collect::<serde_json::Map<_, _>>();
    let baseline_path = absolute
        .join("database/baselines")
        .join(BASELINE)
        .join("manifest.json");
    let receipt = json!({
        "status":"inactive_external_preview_only",
        "epoch":valid.epoch,
        "manifest_sha256":manifest::file_sha256(&directory.join("manifest.yml"))?,
        "baseline_manifest_sha256":manifest::file_sha256(&baseline_path)?,
        "source_plan_sha256":manifest::file_sha256(&source_path(&root,"docs/design/canonical-schema-successors.json")?)?,
        "support_files_sha256":support_files,
        "historical_preimages_sha256":historical_preimages(&plan)?,
        "changes":changes,
        "missing_measurements":[18,19,20],
        "database_actions":false
    });
    let mut receipt_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(absolute.join("PREVIEW_NOT_ACTIVATED.json"))?;
    receipt_file.write_all(
        format!(
            "{}\n",
            serde_json::to_string_pretty(&receipt)
                .map_err(|_| refused("receipt serialization failed"))?
        )
        .as_bytes(),
    )?;
    verify_preview_with_preimage_root(&root, &absolute, preimage_root)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ai-platform-successors-{label}-{}",
                uuid::Uuid::new_v4().simple()
            ));
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
    fn canonical_successor_source_requires_explicit_activation_state() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let epoch_path = root
            .join("database/migrations")
            .join(BASELINE)
            .join("manifest.yml");
        let epoch = manifest::load_epoch_manifest(&epoch_path).unwrap();
        let activated = activation_receipt_present(&root).unwrap();
        if epoch.epoch == 17 {
            assert!(!activated);
            assert!(verify_activated_source(&root).is_err());
        } else if epoch.epoch == 20 {
            assert!(activated);
            let (_, verified) = verify_activated_source(&root).unwrap();
            assert_eq!(verified.epoch, 20);
            assert_eq!(verified.changes.len(), 20);
            assert!(validate_with_preimage_root(&root, None).is_err());
        } else if epoch.epoch == crate::source_tools::next_successor::CANDIDATE_EPOCH {
            assert!(activated);
            let (_, verified) = verify_activated_source(&root).unwrap();
            assert_eq!(
                verified.epoch,
                crate::source_tools::next_successor::CANDIDATE_EPOCH
            );
            assert_eq!(
                verified.changes.len(),
                crate::source_tools::next_successor::CANDIDATE_EPOCH as usize
            );
            assert!(validate_with_preimage_root(&root, None).is_err());
        } else if epoch.epoch == crate::source_tools::next_successor_022::CANDIDATE_EPOCH {
            assert!(activated);
            let (_, verified) = verify_activated_source(&root).unwrap();
            assert_eq!(
                verified.epoch,
                crate::source_tools::next_successor_022::CANDIDATE_EPOCH
            );
            assert_eq!(
                verified.changes.len(),
                crate::source_tools::next_successor_022::CANDIDATE_EPOCH as usize
            );
            assert!(validate_with_preimage_root(&root, None).is_err());
        } else {
            panic!("unexpected canonical schema epoch");
        }
    }

    #[test]
    fn historical_preimage_fallback_is_narrow_and_digest_checked() {
        let source = Scratch::new("source");
        fs::create_dir_all(source.0.join("database/authority")).unwrap();
        let preserved = Scratch::new("preserved");
        let old_file = preserved.0.join(RETIRED_PREIMAGE);
        fs::create_dir_all(old_file.parent().unwrap()).unwrap();
        fs::write(&old_file, b"wrong historical bytes").unwrap();
        let source = source.0.canonicalize().unwrap();
        let preserved = preserved.0.canonicalize().unwrap();

        assert!(
            verify_frozen_preimage(&source, None, RETIRED_PREIMAGE, RETIRED_PREIMAGE_SHA256)
                .is_err()
        );
        assert!(
            verify_frozen_preimage(
                &source,
                Some(&preserved),
                RETIRED_PREIMAGE,
                RETIRED_PREIMAGE_SHA256
            )
            .is_err()
        );
        assert!(
            verify_frozen_preimage(
                &source,
                Some(&preserved),
                "database/authority/other_retired.py",
                RETIRED_PREIMAGE_SHA256
            )
            .is_err()
        );
    }
}
