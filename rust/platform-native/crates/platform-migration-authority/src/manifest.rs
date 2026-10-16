//! Immutable source manifests, translated from database/authority/manifest.py.
use crate::{AuthorityError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub const BASELINE_SCHEMA: &str = "migration-authority/baseline-manifest/v1";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionMode {
    Transactional,
    NonTransactional,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RollbackClass {
    OldBinaryCompatible,
    ForwardFixOnly,
    RestoreRequired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSpec {
    pub sequence: i32,
    pub name: String,
    pub file: String,
    pub sha256: String,
    pub owner: String,
    pub transaction_mode: TransactionMode,
    pub rollback_class: RollbackClass,
    pub preconditions: Vec<String>,
    pub postconditions: Vec<String>,
    pub timeout_seconds: i64,
    pub lock_budget_seconds: i64,
    pub resume_handler: Option<String>,
    pub repair_handler: Option<String>,
    #[serde(default)]
    pub notes: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochManifest {
    pub baseline_id: String,
    pub epoch: i32,
    pub changes: Vec<ChangeSpec>,
    pub extra: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyChangeSpec {
    pub file: String,
    pub sha256: String,
    pub transaction_mode: TransactionMode,
    pub rollback_class: RollbackClass,
}
impl LegacyChangeSpec {
    pub fn legacy_checksum(&self) -> &str {
        &self.sha256[..16]
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyRollbackSpec {
    pub file: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyManifest {
    pub schema: String,
    pub freeze_point: String,
    pub changes: Vec<LegacyChangeSpec>,
    #[serde(default)]
    pub historical_rollbacks: Vec<LegacyRollbackSpec>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceDataSet {
    pub table: String,
    pub natural_key: Vec<String>,
    pub immutable_columns: Vec<String>,
    #[serde(default, rename = "where")]
    pub where_clause: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineManifest {
    pub baseline_id: String,
    pub schema_revision: String,
    pub source_git_sha: String,
    pub last_legacy_change: String,
    pub structural_sha256: String,
    pub acl_sha256: String,
    pub extensions_sha256: String,
    pub reference_data_sha256: String,
    pub generator: String,
    pub generated_at: String,
    pub postgres_version: String,
    pub schema: String,
    pub state: String,
    pub files_sha256: BTreeMap<String, String>,
    pub policy_files_sha256: BTreeMap<String, String>,
    #[serde(default)]
    pub reference_data: Vec<ReferenceDataSet>,
}
impl BaselineManifest {
    pub fn fingerprints(&self) -> BTreeMap<String, String> {
        [
            ("structural", &self.structural_sha256),
            ("acl", &self.acl_sha256),
            ("extensions", &self.extensions_sha256),
            ("reference_data", &self.reference_data_sha256),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.clone()))
        .collect()
    }
}
fn refused(msg: impl Into<String>) -> AuthorityError {
    AuthorityError::Refused(msg.into())
}
fn matches(pattern: &str, s: &str) -> bool {
    regex::Regex::new(pattern)
        .expect("static regex")
        .is_match(s)
}
pub fn validate_identifier(s: &str) -> Result<()> {
    if matches(r"^[a-z][a-z0-9_]{0,62}$", s) {
        Ok(())
    } else {
        Err(refused("invalid SQL identifier"))
    }
}
pub fn validate_ledger_id(s: &str) -> Result<()> {
    if matches(r"^[a-z0-9][a-z0-9_]{0,62}$", s) {
        Ok(())
    } else {
        Err(refused("invalid baseline ledger id"))
    }
}
fn sha(s: &str) -> Result<()> {
    if matches(r"^[0-9a-f]{64}$", s) {
        Ok(())
    } else {
        Err(refused("invalid full SHA-256"))
    }
}
pub fn file_sha256(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn parent(path: &Path, id: &str) -> Result<()> {
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        == Some(id)
    {
        Ok(())
    } else {
        Err(refused(
            "manifest baseline id differs from parent directory",
        ))
    }
}
fn yaml(path: &Path) -> Result<Value> {
    serde_yaml::from_slice(&fs::read(path)?).map_err(|_| refused("invalid YAML manifest"))
}
fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| refused(format!("invalid manifest fields: {e}")))
}
fn conditions(xs: &[String]) -> Result<()> {
    if xs.is_empty()
        || xs.iter().any(|s| {
            !matches(r"(?i)^\s*SELECT\b", s)
                || [";", "--", "/*", "*/"].iter().any(|m| s.contains(m))
        })
    {
        return Err(refused(
            "conditions must be nonempty single read-only SELECT expressions",
        ));
    }
    Ok(())
}
fn sql_files(dir: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut files = BTreeMap::new();
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.extension().and_then(|s| s.to_str()) == Some("sql") {
            let n = p
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| refused("non UTF-8 SQL filename"))?
                .to_owned();
            files.insert(n, p);
        }
    }
    Ok(files)
}
fn verify_inventory(
    files: &BTreeMap<String, PathBuf>,
    hashes: &BTreeMap<String, String>,
) -> Result<()> {
    if files.keys().collect::<Vec<_>>() != hashes.keys().collect::<Vec<_>>() {
        return Err(refused("manifest does not exactly cover SQL files"));
    }
    for (n, h) in hashes {
        sha(h)?;
        if file_sha256(&files[n])? != *h {
            return Err(refused(format!("checksum drift for {n}")));
        }
    }
    Ok(())
}
pub fn load_epoch_manifest(path: &Path) -> Result<EpochManifest> {
    let raw = yaml(path)?;
    let mut obj = raw
        .as_object()
        .cloned()
        .ok_or_else(|| refused("epoch must be mapping"))?;
    let id = obj
        .remove("baseline_id")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| refused("missing baseline id"))?;
    validate_ledger_id(&id)?;
    parent(path, &id)?;
    let epoch = obj
        .remove("epoch")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0 && *n <= i32::MAX as i64)
        .ok_or_else(|| refused("invalid epoch"))? as i32;
    let raw_changes = obj.remove("changes").unwrap_or(Value::Array(vec![]));
    let entries = if raw_changes.is_null() {
        vec![]
    } else {
        raw_changes
            .as_array()
            .cloned()
            .ok_or_else(|| refused("changes must be array"))?
    };
    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    let mut hashes = BTreeMap::new();
    for entry in entries {
        let o = entry
            .as_object()
            .ok_or_else(|| refused("change must be object"))?;
        for key in [
            "sequence",
            "name",
            "file",
            "sha256",
            "owner",
            "transaction_mode",
            "rollback_class",
            "preconditions",
            "postconditions",
            "timeout_seconds",
            "lock_budget_seconds",
            "resume_handler",
            "repair_handler",
        ] {
            if !o.contains_key(key) {
                return Err(refused(format!("missing change field {key}")));
            }
        }
        let c: ChangeSpec = parse(entry)?;
        if c.sequence < 1 || !seen.insert(c.sequence) {
            return Err(refused("invalid or duplicate sequence"));
        }
        validate_identifier(&c.name)?;
        sha(&c.sha256)?;
        if !matches(r"^\d{3}_[a-z0-9_]+\.sql$", &c.file)
            || c.file[..3].parse::<i32>().ok() != Some(c.sequence)
        {
            return Err(refused("invalid change filename"));
        }
        if ![
            "owner",
            "migrator",
            "gateway",
            "runtime",
            "capability_worker",
            "knowledge_api",
            "knowledge_worker",
        ]
        .contains(&c.owner.as_str())
        {
            return Err(refused("invalid logical principal"));
        }
        if c.timeout_seconds < 1
            || c.lock_budget_seconds < 1
            || c.lock_budget_seconds > c.timeout_seconds
        {
            return Err(refused("invalid timeout or lock budget"));
        }
        for h in [&c.resume_handler, &c.repair_handler].into_iter().flatten() {
            validate_identifier(h)?;
        }
        let recovery = c.resume_handler.is_some() || c.repair_handler.is_some();
        if (c.transaction_mode == TransactionMode::NonTransactional) != recovery {
            return Err(refused("transaction/recovery-handler mismatch"));
        }
        conditions(&c.preconditions)?;
        conditions(&c.postconditions)?;
        hashes.insert(c.file.clone(), c.sha256.clone());
        changes.push(c);
    }
    if seen.iter().copied().ne(1..=seen.len() as i32) {
        return Err(refused("sequences must be contiguous from one"));
    }
    verify_inventory(
        &sql_files(path.parent().ok_or_else(|| refused("missing parent"))?)?,
        &hashes,
    )?;
    changes.sort_by_key(|c| c.sequence);
    Ok(EpochManifest {
        baseline_id: id,
        epoch,
        changes,
        extra: Value::Object(obj),
    })
}
pub fn load_legacy_manifest(path: &Path) -> Result<LegacyManifest> {
    let m: LegacyManifest = parse(yaml(path)?)?;
    if m.schema != "migration-authority/legacy-manifest/v1" || m.changes.is_empty() {
        return Err(refused("invalid legacy schema or empty changes"));
    }
    let mut hashes = BTreeMap::new();
    let mut order = vec![];
    let mut nontransactional = BTreeSet::new();
    for c in &m.changes {
        if !matches(r"^\d{3}_[a-z0-9_]+\.sql$", &c.file)
            || c.file.ends_with("_rollback.sql")
            || hashes.insert(c.file.clone(), c.sha256.clone()).is_some()
        {
            return Err(refused("invalid or duplicate forward migration"));
        }
        order.push(c.file.clone());
        if c.transaction_mode == TransactionMode::NonTransactional {
            nontransactional.insert(c.file.as_str());
        }
    }
    for c in &m.historical_rollbacks {
        if !matches(r"^\d{3}_[a-z0-9_]+_rollback\.sql$", &c.file)
            || hashes.insert(c.file.clone(), c.sha256.clone()).is_some()
        {
            return Err(refused("invalid or duplicate rollback"));
        }
    }
    let mut sorted = order.clone();
    sorted.sort();
    if order != sorted || order.last() != Some(&m.freeze_point) {
        return Err(refused("invalid legacy order or freeze point"));
    }
    if nontransactional != BTreeSet::from(["049_session_list_performance.sql"]) {
        return Err(refused("legacy nontransactional inventory differs"));
    }
    let dir = path
        .parent()
        .ok_or_else(|| refused("missing manifest parent"))?;
    let mut files = sql_files(dir)?;
    if dir.join("legacy").is_dir() {
        for (k, v) in sql_files(&dir.join("legacy"))? {
            if files.insert(k, v).is_some() {
                return Err(refused("duplicate flat and legacy SQL file"));
            }
        }
    }
    verify_inventory(&files, &hashes)?;
    Ok(m)
}
pub fn baseline_artifact_state(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok("missing".into());
    }
    let v: Value =
        serde_json::from_slice(&fs::read(path)?).map_err(|_| refused("invalid baseline JSON"))?;
    if v["schema"] != BASELINE_SCHEMA {
        return Err(refused("invalid baseline schema"));
    }
    let id = v["baseline_id"]
        .as_str()
        .ok_or_else(|| refused("missing baseline id"))?;
    validate_ledger_id(id)?;
    parent(path, id)?;
    match v["state"].as_str() {
        Some(s @ ("frozen" | "pending-live-freeze")) => Ok(s.into()),
        _ => Err(refused("unknown baseline state")),
    }
}
pub fn load_baseline_manifest(path: &Path) -> Result<BaselineManifest> {
    let m: BaselineManifest = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| refused("invalid baseline JSON fields"))?;
    if m.schema != BASELINE_SCHEMA || m.state != "frozen" {
        return Err(refused("baseline is not frozen"));
    }
    validate_ledger_id(&m.baseline_id)?;
    parent(path, &m.baseline_id)?;
    if !matches(r"^[0-9a-f]{40}$", &m.source_git_sha)
        || !matches(r"^\d{3}_[a-z0-9_]+\.sql$", &m.last_legacy_change)
        || !matches(r"^[0-9]+$", &m.schema_revision)
        || !matches(r"^[0-9]+(?:\.[0-9]+)*$", &m.postgres_version)
        || m.generator.is_empty()
    {
        return Err(refused("invalid baseline identity"));
    }
    chrono::DateTime::parse_from_rfc3339(&m.generated_at)
        .map_err(|_| refused("invalid timezone-qualified generated_at"))?;
    for h in m.fingerprints().values() {
        sha(h)?;
    }
    let dir = path.parent().ok_or_else(|| refused("missing parent"))?;
    let required = BTreeSet::from([
        "cutover_convergence.sql",
        "grants.sql",
        "init.sql",
        "reference_data.sql",
        "verify.sql",
    ]);
    if m.files_sha256
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != required
    {
        return Err(refused("baseline SQL coverage mismatch"));
    }
    verify_inventory(&sql_files(dir)?, &m.files_sha256)?;
    if m.policy_files_sha256
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != BTreeSet::from([
            "data-access-inventory.json",
            "ownership-policy.json",
            "grants-policy.json",
        ])
    {
        return Err(refused("policy file coverage mismatch"));
    }
    for (n, h) in &m.policy_files_sha256 {
        sha(h)?;
        if file_sha256(&dir.join(n))? != *h {
            return Err(refused(format!("policy checksum drift for {n}")));
        }
    }
    let mut tables = BTreeSet::new();
    for r in &m.reference_data {
        if !matches(r"^[a-z][a-z0-9_]{0,62}(\.[a-z][a-z0-9_]{0,62})?$", &r.table)
            || !tables.insert(&r.table)
        {
            return Err(refused("invalid or duplicate reference table"));
        }
        for columns in [&r.natural_key, &r.immutable_columns] {
            if columns.is_empty() || columns.iter().collect::<BTreeSet<_>>().len() != columns.len()
            {
                return Err(refused("invalid reference columns"));
            }
            for c in columns {
                validate_identifier(c)?;
            }
        }
        if [";", "--", "/*", "*/"]
            .iter()
            .any(|s| r.where_clause.contains(s))
            || r.where_clause.chars().any(|c| (c as u32) < 32)
        {
            return Err(refused("invalid reference predicate"));
        }
    }
    Ok(m)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_and_conditions_match_contract() {
        assert!(validate_ledger_id("2026_08_post_kb_v1").is_ok());
        assert!(validate_identifier("2026_bad").is_err());
        assert!(conditions(&["SELECT true".into()]).is_ok());
        assert!(conditions(&["SELECT true; DROP TABLE x".into()]).is_err());
        assert!(conditions(&[]).is_err());
    }
    #[test]
    fn current_source_manifests_validate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database");
        load_epoch_manifest(&root.join("migrations/2026_08_post_kb_v1/manifest.yml")).unwrap();
        load_legacy_manifest(&root.join("migrations/legacy-manifest.yml")).unwrap();
        load_baseline_manifest(&root.join("baselines/2026_08_post_kb_v1/manifest.json")).unwrap();
    }
}

/// Verify the same immutable Git provenance as the Python freeze authority.
pub fn verify_baseline_git_provenance(
    path: &Path,
    baseline: &BaselineManifest,
    repo_root: &Path,
) -> Result<()> {
    use std::process::{Command, Output};
    let generator = Path::new(&baseline.generator);
    if generator.is_absolute()
        || baseline.generator.is_empty()
        || generator
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || baseline
            .generator
            .split('/')
            .any(|s| s.is_empty() || s == ".")
    {
        return Err(refused("unsafe baseline generator path"));
    }
    let root = repo_root.canonicalize()?;
    let relative = path
        .canonicalize()?
        .strip_prefix(&root)
        .map_err(|_| refused("baseline outside repository"))?
        .to_string_lossy()
        .replace('\\', "/");
    let cutover = path
        .parent()
        .ok_or_else(|| refused("missing parent"))?
        .join("cutover_convergence.sql")
        .canonicalize()?
        .strip_prefix(&root)
        .map_err(|_| refused("cutover outside repository"))?
        .to_string_lossy()
        .replace('\\', "/");
    let git = |args: &[&str]| -> Result<Output> {
        Ok(Command::new("git").args(args).current_dir(&root).output()?)
    };
    let source = &baseline.source_git_sha;
    let resolved = git(&["rev-parse", "--verify", &format!("{source}^{{commit}}")])?;
    if !resolved.status.success() || String::from_utf8_lossy(&resolved.stdout).trim() != source {
        return Err(refused("source_git_sha is not an exact resolvable commit"));
    }
    if !git(&["merge-base", "--is-ancestor", source, "HEAD"])?
        .status
        .success()
    {
        return Err(refused("freeze source is not an ancestor of HEAD"));
    }
    let initial = git(&["show", &format!("{source}:{relative}")])?;
    let pending: Value = serde_json::from_slice(&initial.stdout).unwrap_or(Value::Null);
    if !initial.status.success()
        || pending["schema"] != BASELINE_SCHEMA
        || pending["state"] != "pending-live-freeze"
        || pending["baseline_id"] != baseline.baseline_id
    {
        return Err(refused(
            "source must contain matching pending freeze manifest",
        ));
    }
    if !git(&[
        "cat-file",
        "-e",
        &format!("{source}:{}", baseline.generator),
    ])?
    .status
    .success()
    {
        return Err(refused("generator absent from freeze source"));
    }
    let retired = crate::producer_provenance::verified_retired_paths(path, baseline, &root)?;
    let first = [
        "diff",
        "--quiet",
        source,
        "--",
        &baseline.generator,
        &cutover,
    ];
    let first: Vec<&str> = first
        .into_iter()
        .filter(|p| !retired.contains(*p))
        .collect();
    if !git(&first)?.status.success() {
        return Err(refused("authority inputs differ from freeze source"));
    }
    let exclude = format!(":(exclude)database/migrations/{}/**", baseline.baseline_id);
    let args = [
        "diff",
        "--quiet",
        source,
        "--",
        "database/schema.sql",
        "database/migrations/legacy-manifest.yml",
        "database/migrations/2026_08_post_kb_v1/manifest.yml",
        "database/migrations/per_service/manifest.yml",
        "database/authority/fingerprint.py",
        "database/authority/fingerprint_catalog.py",
        "database/authority/fingerprint_values.py",
        "database/bootstrap/roles.sql",
        "database/bootstrap/extensions.sql",
        "scripts/inventory/database_policy.py",
        "scripts/inventory/generate_database_grants.py",
        "scripts/database/render_baseline_contract.py",
        "scripts/database/freeze_baseline.py",
        ":(glob)database/migrations/**/*.sql",
        &baseline.generator,
        &cutover,
        &exclude,
    ];
    let args: Vec<&str> = args.into_iter().filter(|p| !retired.contains(*p)).collect();
    if !git(&args)?.status.success() {
        return Err(refused("cannot verify unchanged baseline authority inputs"));
    }
    let epoch_rel = format!("database/migrations/{}/manifest.yml", baseline.baseline_id);
    let initial = git(&["show", &format!("{source}:{epoch_rel}")])?;
    let history = git(&[
        "log",
        "--first-parent",
        "--reverse",
        "--format=%H",
        &format!("{source}..HEAD"),
        "--",
        &epoch_rel,
    ])?;
    if !history.status.success() {
        return Err(refused("cannot verify epoch history"));
    }
    let current = root.join(&epoch_rel);
    if !initial.status.success() {
        let tree = git(&["ls-tree", source, "--", &epoch_rel])?;
        if !tree.status.success() || !tree.stdout.is_empty() {
            return Err(refused("cannot read source epoch manifest"));
        }
        if !current.exists() && history.stdout.is_empty() {
            return Ok(());
        }
    }
    load_epoch_manifest(&current)?;
    let descriptors = |bytes: &[u8]| -> Result<Vec<Value>> {
        let v: Value =
            serde_yaml::from_slice(bytes).map_err(|_| refused("invalid historical epoch YAML"))?;
        if v["baseline_id"] != baseline.baseline_id {
            return Err(refused("historical baseline mismatch"));
        }
        v["changes"]
            .as_array()
            .cloned()
            .ok_or_else(|| refused("invalid historical epoch changes"))
    };
    let mut previous = if initial.status.success() {
        descriptors(&initial.stdout)?
    } else {
        vec![]
    };
    for rev in String::from_utf8_lossy(&history.stdout).lines() {
        let candidate = git(&["show", &format!("{rev}:{epoch_rel}")])?;
        if !candidate.status.success() {
            return Err(refused("integrated epoch removed"));
        }
        let changes = descriptors(&candidate.stdout)?;
        if !changes.starts_with(&previous) {
            return Err(refused("integrated epoch descriptors are immutable"));
        }
        previous = changes;
    }
    if !descriptors(&fs::read(current)?)?.starts_with(&previous) {
        return Err(refused("working epoch changed immutable history"));
    }
    Ok(())
}
